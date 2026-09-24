//! Writes real program-produced market and portfolio accounts, plus the program's own decoding
//! of key fields, for percolator-cli's parser cross-check (tests/v16/layout-fixture.ts there).
//!
//!   PERCOLATOR_CLI_FIXTURE_DIR=../percolator-cli/tests/v16/fixtures \
//!     cargo test --test cli_layout_fixture -- --ignored

#[allow(dead_code)]
mod support;

use percolator_prog::state;
use support::v16_svm::{MarketConfig, V16Svm, INITIAL_PRICE};

#[test]
#[ignore]
fn write_cli_layout_fixture() {
    let dir = std::path::PathBuf::from(
        std::env::var("PERCOLATOR_CLI_FIXTURE_DIR").expect("set PERCOLATOR_CLI_FIXTURE_DIR"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let mut env = V16Svm::new([7u8; 32], MarketConfig::default());
    env.trade_no_cpi(0, 1, 1, 3_000_000, INITIAL_PRICE, 0)
        .expect("open a position on asset 1");
    let market = env.market_data(false);
    let portfolio = env.primary_portfolio_data(0);
    let (cfg, group) = state::read_market(&market).unwrap();
    let p = env.primary_portfolio(0);
    let leg = p.legs.iter().find(|l| l.active != 0).expect("open leg");
    let asset = &group.assets[1];
    let expected = format!(
        "{{\"marketauth\":\"{}\",\"maintenanceFeePerSlot\":\"{}\",\"nextPortfolioId\":\"{}\",\
\"vault\":\"{}\",\"cTot\":\"{}\",\"currentSlot\":\"{}\",\"assetSlotCapacity\":{},\
\"asset1MarketId\":\"{}\",\"asset1EffectivePrice\":\"{}\",\"asset1OiLong\":\"{}\",\"asset1Lifecycle\":{},\
\"capital\":\"{}\",\"pnl\":\"{}\",\"legAssetIndex\":{},\"legBasisPosQ\":\"{}\",\"legMarketId\":\"{}\",\
\"portfolioId\":\"{}\",\"matcherSequence\":\"{}\",\"positionEpoch\":\"{}\"}}",
        solana_sdk::pubkey::Pubkey::new_from_array(cfg.marketauth),
        cfg.maintenance_fee_per_slot,
        cfg.next_portfolio_id,
        group.vault,
        group.c_tot,
        group.current_slot,
        state::read_market_config_mode_and_capacity(&market).unwrap().2,
        asset.market_id,
        asset.effective_price,
        asset.oi_eff_long_q,
        asset.lifecycle as u8,
        p.capital.get(),
        p.pnl.get(),
        leg.asset_index.get(),
        leg.basis_pos_q.get(),
        leg.market_id.get(),
        state::read_portfolio_id(&portfolio).unwrap(),
        state::read_portfolio_matcher_sequence(&portfolio).unwrap(),
        state::read_portfolio_position_epoch(&portfolio).unwrap(),
    );
    std::fs::write(dir.join("market.bin"), &market).unwrap();
    std::fs::write(dir.join("portfolio.bin"), &portfolio).unwrap();
    std::fs::write(dir.join("expected.json"), expected).unwrap();
}
