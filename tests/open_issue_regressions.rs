//! Public-route regressions for issues reported against percolator-prog.
//!
//! Each test drives the deployed SBF through LiteSVM with public instructions only and pins the
//! behavior the corresponding issue requires. Retained transactions are built before the state
//! they authorized changes; every build carries a distinct compute-unit price, so a rejected
//! retry is rejected by the program, not by duplicate-signature filtering.

#[allow(dead_code)]
mod support;

use percolator_prog::processor;
use support::v16_svm::{MarketConfig, V16Svm};

fn env(seed: u8, config: MarketConfig) -> V16Svm {
    V16Svm::new([seed; 32], config)
}

/// Issue #402: a retained ClosePortfolio signed while the portfolio was empty must not land after
/// the same portfolio incarnation has accepted and returned new capital.
#[test]
fn issue402_retained_close_rejects_after_empty_state_aba() {
    let mut env = env(40, MarketConfig::default());
    let capital = env.primary_portfolio(0).capital.get();
    env.withdraw_primary(0, capital).expect("empty the portfolio");
    let retained_close = env.build_retained_close_primary_portfolio(0);

    // Same incarnation: deposit, then return to an empty state.
    env.deposit_primary(0, 100).expect("deposit");
    env.withdraw_primary(0, 100).expect("withdraw");
    let before = env.primary_portfolio_data(0);

    assert!(
        env.land_retained(retained_close).is_err(),
        "a close signed before the deposit must not land after it"
    );
    assert_eq!(env.primary_portfolio_data(0), before, "rejection is byte-atomic");

    env.close_primary_portfolio(0)
        .expect("a freshly signed close at the current sequence succeeds");
}

/// Issue #391: a retained asset-authority handoff must not restore a revoked backing key after
/// the role has moved on and an independent provider has deposited.
#[test]
fn issue391_retained_authority_handoff_cannot_restore_revoked_backing_key() {
    const ASSET: u16 = 0;
    const DOMAIN: u16 = 0;
    const OLD: usize = 1;
    const NEW: usize = 2;
    let mut env = env(39, MarketConfig::default());
    let first =
        env.build_retained_asset_authority_handoff_from_admin(ASSET, processor::ASSET_AUTH_BACKING_BUCKET, OLD);
    let withheld =
        env.build_retained_asset_authority_handoff_from_admin(ASSET, processor::ASSET_AUTH_BACKING_BUCKET, OLD);
    env.land_retained(first).expect("first handoff lands");
    env.update_asset_authority_from_admin(ASSET, processor::ASSET_AUTH_BACKING_BUCKET, NEW)
        .expect("rotate backing role to the new provider");
    let far_expiry = env.current_slot() + 1_000_000;
    env.top_up_backing_bucket_for_actor(NEW, DOMAIN, 100, far_expiry)
        .expect("new provider deposits backing");

    let market_before = env.market_data(false);
    assert!(
        env.land_retained(withheld).is_err(),
        "a withheld handoff variant must not restore the revoked key"
    );
    assert_eq!(env.market_data(false), market_before);
    assert!(
        env.withdraw_backing_bucket_for_actor(OLD, DOMAIN, 100).is_err(),
        "the revoked key cannot withdraw the new provider's backing"
    );
    env.withdraw_backing_bucket_for_actor(NEW, DOMAIN, 100)
        .expect("the current provider keeps custody of its deposit");
}

fn maintenance_domains_after(syncs: &[u64]) -> (u128, u128) {
    let mut env = env(
        38,
        MarketConfig {
            maintenance_fee_per_slot: 1,
            ..MarketConfig::default()
        },
    );
    let start = env.current_slot();
    for &offset in syncs {
        env.warp_to_slot(start + offset);
        env.sync_maintenance_fee(0, start + offset)
            .expect("permissionless maintenance sync");
    }
    let (_, group) = env.primary_market_state();
    (group.insurance_domain_budget[0], group.insurance_domain_budget[1])
}

/// Issue #386: the long/short allocation of side-neutral maintenance revenue must not depend on
/// the (permissionless, attacker-chosen) sync cadence.
#[test]
fn issue386_maintenance_fee_split_is_cadence_independent() {
    let once = maintenance_domains_after(&[10]);
    let every_slot = maintenance_domains_after(&(1..=10).collect::<Vec<_>>());
    let mixed = maintenance_domains_after(&[3, 10]);
    assert_eq!(once.0 + once.1, 10, "the whole fee reaches asset-0 insurance");
    assert_eq!(once, every_slot, "ten one-atom syncs must split like one ten-atom sync");
    assert_eq!(once, mixed, "odd partitions must split like one sync");
    assert!(once.0.abs_diff(once.1) <= 1);
}

fn market_with_barrier(env: &mut V16Svm, short_side: bool) {
    use percolator_prog::constants::{ASSET_ORACLE_WRAPPER_LEN, MARKET_GROUP_LEN, MARKET_GROUP_OFF};
    let engine = MARKET_GROUP_OFF + MARKET_GROUP_LEN + ASSET_ORACLE_WRAPPER_LEN;
    let field = if short_side {
        core::mem::offset_of!(percolator::EngineAssetSlotV16Account, pending_domain_loss_barrier_short)
    } else {
        core::mem::offset_of!(percolator::EngineAssetSlotV16Account, pending_domain_loss_barrier_long)
    };
    let mut account = env.svm.get_account(&env.market).unwrap();
    account.data[engine + field..engine + field + 8].copy_from_slice(&1u64.to_le_bytes());
    env.svm.set_account(env.market, account).unwrap();
}

/// Issue #438: WithdrawInsuranceAsset debits both of an asset's insurance domains, so a pending
/// loss barrier on either domain must block it (hardening; the barrier is injected because no
/// public trace is known to reach it with all global loss counters clear).
#[test]
fn issue438_insurance_withdrawal_respects_both_domain_barriers() {
    // Control: with no barrier the same two-domain withdrawal succeeds.
    let mut control = env(43, MarketConfig::default());
    control.top_up_insurance_domain(0, 100).expect("fund long domain");
    control.top_up_insurance_domain(1, 100).expect("fund short domain");
    control
        .withdraw_insurance_asset_as_admin(0, 150)
        .expect("barrier-free withdrawal succeeds");
    for short_side in [false, true] {
        let mut env = env(43, MarketConfig::default());
        env.top_up_insurance_domain(0, 100).expect("fund long domain");
        env.top_up_insurance_domain(1, 100).expect("fund short domain");
        market_with_barrier(&mut env, short_side);
        assert!(
            env.withdraw_insurance_asset_as_admin(0, 150).is_err(),
            "short_side={short_side}: a pending barrier must block the two-domain debit"
        );
    }
}

/// Issue #170: matched, value-free spent-backing history left by a round trip must not block
/// restart of an emptied asset forever; tag 70, signed by the asset's backing authority, clears
/// it. The history is installed directly (the engine-level test drives it through public
/// transitions).
#[test]
fn issue170_backing_authority_canonicalizes_history_then_reuse_succeeds() {
    use percolator::{BackingBucketStatusV16, BackingBucketV16, BackingBucketV16Account};
    use percolator::{SourceCreditStateV16, SourceCreditStateV16Account, BOUND_SCALE};
    use percolator_prog::constants::{ASSET_ORACLE_WRAPPER_LEN, MARKET_ASSET_SLOT_LEN};
    use percolator_prog::constants::{MARKET_GROUP_LEN, MARKET_GROUP_OFF};
    const ASSET: u16 = 1;
    const PROVIDER: usize = 1;
    const OTHER: usize = 2;
    let mut env = env(17, MarketConfig::default());
    env.warp_to_slot(1);
    env.retire_asset(ASSET, 1).expect("retire empty asset");
    env.update_asset_authority_from_admin(ASSET, processor::ASSET_AUTH_BACKING_BUCKET, PROVIDER)
        .expect("hand the backing role to the provider");
    let market_id = env.primary_market_state().1.assets[ASSET as usize].market_id;

    let engine = MARKET_GROUP_OFF
        + MARKET_GROUP_LEN
        + ASSET as usize * MARKET_ASSET_SLOT_LEN
        + ASSET_ORACLE_WRAPPER_LEN;
    let history = 25 * BOUND_SCALE;
    let mut account = env.svm.get_account(&env.market).unwrap();
    let src_off = engine + core::mem::offset_of!(percolator::EngineAssetSlotV16Account, source_credit_long);
    let bucket_off = engine + core::mem::offset_of!(percolator::EngineAssetSlotV16Account, backing_long);
    let src = SourceCreditStateV16Account::from_runtime(&SourceCreditStateV16 {
        spent_backing_num: history,
        provider_receivable_num: history,
        credit_rate_num: percolator::CREDIT_RATE_SCALE,
        ..SourceCreditStateV16::EMPTY
    });
    let bucket = BackingBucketV16Account::from_runtime(&BackingBucketV16 {
        market_id,
        consumed_liened_backing_num: history,
        status: BackingBucketStatusV16::Expired,
        ..BackingBucketV16::EMPTY
    });
    account.data[src_off..src_off + core::mem::size_of::<SourceCreditStateV16Account>()]
        .copy_from_slice(bytemuck::bytes_of(&src));
    account.data[bucket_off..bucket_off + core::mem::size_of::<BackingBucketV16Account>()]
        .copy_from_slice(bytemuck::bytes_of(&bucket));
    env.svm.set_account(env.market, account).unwrap();

    // A retired slot returns through lifecycle ACTIVATE (reuse); matched history blocks it.
    let slot = 1_000_000;
    env.warp_to_slot(slot);
    let reuse = env.build_retained_activate_asset(ASSET, slot, support::v16_svm::INITIAL_PRICE);
    assert!(env.land_retained(reuse).is_err(), "matched history blocks reuse");
    let before = env.market_data(false);
    assert!(
        env.canonicalize_spent_backing_history_for_actor(OTHER, ASSET).is_err(),
        "only the asset's backing authority may clear its history"
    );
    assert_eq!(env.market_data(false), before);
    env.canonicalize_spent_backing_history_for_actor(PROVIDER, ASSET)
        .expect("backing authority clears value-free history");
    env.expire_blockhash();
    let reuse = env.build_retained_activate_asset(ASSET, slot, support::v16_svm::INITIAL_PRICE);
    env.land_retained(reuse)
        .expect("reuse succeeds once only history remained");
}


/// Permissionless listing must not require the whole market to be flat: configuring the oracle of
/// a freshly activated, empty asset is target-scoped, so it succeeds while traders on another
/// asset hold open PnL. Asset 0 (mirrored into the base config) keeps the market-wide rule.
#[test]
fn listing_new_asset_oracle_while_other_asset_has_open_pnl() {
    const MARK: u64 = 1_000_000;
    let mut env = env(71, MarketConfig { initial_price: MARK, ..MarketConfig::default() });
    env.set_clock(1, 100);
    let feed0 = [0xa0u8; 32];
    let pyth0 = env.set_pyth_price(&feed0, MARK as i64, -6, 0, 100);
    env.configure_hybrid_oracle(0, 1, 100, 0, [feed0, [0; 32], [0; 32]], &[pyth0], 10, 0)
        .expect("configure asset 0 while the market is flat");
    env.trade_no_cpi(0, 1, 0, percolator::POS_SCALE as i128, MARK, 0)
        .expect("open a position on asset 0");
    env.set_clock(2, 101);
    env.update_pyth_price(pyth0, &feed0, (MARK + MARK / 100) as i64, -6, 0, 101);
    let hint = vec![percolator_prog::ix::CrankObservationHint { asset_index: 0, oracle_accounts: 1 }];
    env.crank_with_oracles(0, 2, hint, std::slice::from_ref(&pyth0))
        .expect("accrue the price move on asset 0");
    let (_, group) = env.primary_market_state();
    assert!(
        group.pnl_pos_tot != 0 || group.negative_pnl_account_count != 0,
        "precondition: a trader holds open PnL on asset 0"
    );

    env.update_market_init_fee_policy(1)
        .expect("enable permissionless activation");
    let asset = env.primary_market_state().1.config.max_market_slots as u16;
    env.activate_permissionless_asset(2, asset, 2, MARK, 1)
        .expect("permissionless activation appends a new asset");
    let feed1 = [0xa1u8; 32];
    let pyth1 = env.set_pyth_price(&feed1, MARK as i64, -6, 0, 101);
    env.configure_hybrid_oracle(asset, 2, 101, 0, [feed1, [0; 32], [0; 32]], &[pyth1], 10, 0)
        .expect("a new, empty asset's oracle is configurable while another asset has open PnL");

    let before = env.market_data(false);
    assert!(
        env.configure_hybrid_oracle(0, 2, 101, 0, [feed0, [0; 32], [0; 32]], &[pyth0], 10, 0)
            .is_err(),
        "asset 0 still requires a quiet market"
    );
    assert_eq!(env.market_data(false), before, "rejection is byte-atomic");
}
