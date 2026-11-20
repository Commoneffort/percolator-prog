//! Fork (Turbopump): tag 73 moves a live market account from the pre-c030aa4 engine layout
//! (asset slot 1813 bytes) to the current one (+64 bytes per slot).
//!
//! The fixtures are the six live testnet markets as they were on 2026-11-20 (X1: 281YHBA8,
//! 5fBaKk5D, 74jd4xa2, EHurKZwo, 7a56g7R3; devnet: 35XfkPtq), read with `solana account`.
//! Their market authority is replaced by a test key so the test can sign.

use litesvm::LiteSVM;
use percolator::{AssetStateV16Account, EngineAssetSlotV16Account, Market, ADL_ONE};
use percolator_prog::constants::{
    ASSET_CONTROL_SEQUENCES_OFF, MARKET_ASSET_SLOT_LEN, MARKET_GROUP_LEN,
    MARKET_GROUP_OFF,
};
use percolator_prog::state::{
    layout_v1_market, market_view_mut, AssetControlSequencesV16, AssetOracleStorageV16,
    LAYOUT_V1_ASSET_SLOT_LEN,
};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    rent::Rent,
    signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::mem::offset_of;
use std::path::PathBuf;

const BASE: usize = MARKET_GROUP_OFF + MARKET_GROUP_LEN;
const MARKETAUTH: std::ops::Range<usize> = 16..48;

fn program_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PERCOLATOR_FUZZ_SBF") {
        return PathBuf::from(p);
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    target.join("deploy").join("percolator_prog.so")
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/layout_v1");
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map_or(false, |x| x == "bin"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        .collect();
    out.sort();
    assert_eq!(out.len(), 6, "six live markets");
    out
}

struct Offsets {
    asset: usize,
    raw: usize,
    principal: usize,
}

fn offsets() -> Offsets {
    let engine = offset_of!(Market<AssetOracleStorageV16>, engine);
    let asset = engine + offset_of!(EngineAssetSlotV16Account, asset);
    Offsets {
        asset,
        raw: asset + offset_of!(AssetStateV16Account, raw_basis_sum_long_q),
        principal: engine + offset_of!(EngineAssetSlotV16Account, provider_principal_long_num),
    }
}

fn u128_at(b: &[u8], o: usize) -> u128 {
    u128::from_le_bytes(b[o..o + 16].try_into().unwrap())
}

struct Env {
    svm: LiteSVM,
    program: Pubkey,
    market: Pubkey,
    auth: Keypair,
}

fn env_with(mut data: Vec<u8>, lamports_for_new_len: bool) -> Env {
    let mut svm = LiteSVM::new();
    let program = Pubkey::new_unique();
    svm.add_program(program, &std::fs::read(program_path()).expect("build the SBF program first"));
    let auth = Keypair::new();
    svm.airdrop(&auth.pubkey(), 1_000_000_000).unwrap();
    data[MARKETAUTH].copy_from_slice(&auth.pubkey().to_bytes());
    let (cap, _) = layout_v1_market(&data).expect("fixture is a v1 market");
    let new_len = BASE + cap * MARKET_ASSET_SLOT_LEN;
    let rent = Rent::default();
    let lamports = rent.minimum_balance(if lamports_for_new_len { new_len } else { data.len() });
    let market = Pubkey::new_unique();
    svm.set_account(market, Account { lamports, data, owner: program, executable: false, rent_epoch: 0 })
        .unwrap();
    Env { svm, program, market, auth }
}

fn migrate(env: &mut Env, signer: &Keypair) -> Result<(), String> {
    let ix = Instruction {
        program_id: env.program,
        accounts: vec![AccountMeta::new_readonly(signer.pubkey(), true), AccountMeta::new(env.market, false)],
        data: vec![73],
    };
    let payer = if signer.pubkey() == env.auth.pubkey() { &env.auth } else { signer };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], env.svm.latest_blockhash());
    env.svm.expire_blockhash();
    env.svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?}", e.err))
}

fn migrate_as_auth(env: &mut Env) -> Result<(), String> {
    let auth = env.auth.insecure_clone();
    migrate(env, &auth)
}

fn market_data(env: &Env) -> Vec<u8> {
    env.svm.get_account(&env.market).unwrap().data
}

#[test]
fn migrates_every_live_market_exactly() {
    let o = offsets();
    assert_eq!(o.principal + 32, MARKET_ASSET_SLOT_LEN);
    assert_eq!(LAYOUT_V1_ASSET_SLOT_LEN, 1813);
    for (name, fixture) in fixtures() {
        let mut env = env_with(fixture, true);
        let old = market_data(&env);
        migrate_as_auth(&mut env).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut new = market_data(&env);
        let after_first = new.clone();
        let cap = (old.len() - BASE) / LAYOUT_V1_ASSET_SLOT_LEN;
        assert_eq!(new.len(), old.len() + 64 * cap, "{name}");
        assert_eq!(new[..BASE], old[..BASE], "{name}: wrapper config and group header unchanged");
        let mut open_sides = 0;
        for i in 0..cap {
            let os = &old[BASE + i * LAYOUT_V1_ASSET_SLOT_LEN..BASE + (i + 1) * LAYOUT_V1_ASSET_SLOT_LEN];
            let ns = &new[BASE + i * MARKET_ASSET_SLOT_LEN..BASE + (i + 1) * MARKET_ASSET_SLOT_LEN];
            assert_eq!(ns[..o.raw], os[..o.raw], "{name} slot {i}: bytes before the raw-basis sums");
            assert_eq!(ns[o.raw + 32..o.principal], os[o.raw..o.principal - 32], "{name} slot {i}: shifted bytes");
            assert!(ns[o.principal..].iter().all(|&b| b == 0), "{name} slot {i}: provider principal 0");
            for (a, oi, raw) in [
                (offset_of!(AssetStateV16Account, a_long), offset_of!(AssetStateV16Account, oi_eff_long_q), o.raw),
                (offset_of!(AssetStateV16Account, a_short), offset_of!(AssetStateV16Account, oi_eff_short_q), o.raw + 16),
            ] {
                assert_eq!(u128_at(ns, o.asset + a), ADL_ONE, "{name} slot {i}: fixture side never haircut");
                assert_eq!(u128_at(ns, raw), u128_at(ns, o.asset + oi), "{name} slot {i}: raw-basis sum = effective OI");
                if u128_at(ns, raw) != 0 {
                    open_sides += 1;
                }
            }
        }
        assert!(open_sides > 0, "{name}: the fixture has open positions");
        let (_, group) = market_view_mut(&mut new).unwrap_or_else(|e| panic!("{name}: current program reads it: {e:?}"));
        group.validate_shape().unwrap_or_else(|e| panic!("{name}: engine shape check: {e:?}"));
        // A second run finds a current account and refuses it.
        assert!(migrate_as_auth(&mut env).is_err(), "{name}: second run refused");
        assert_eq!(market_data(&env), after_first, "{name}: second run changed nothing");
    }
}

#[test]
fn refuses_and_leaves_account_unchanged() {
    let o = offsets();
    let (_, fixture) = fixtures().remove(1);
    let cap = (fixture.len() - BASE) / LAYOUT_V1_ASSET_SLOT_LEN;
    let busy = (0..cap)
        .find(|&i| u128_at(&fixture, BASE + i * LAYOUT_V1_ASSET_SLOT_LEN + o.asset + offset_of!(AssetStateV16Account, oi_eff_long_q)) != 0)
        .expect("a slot with open longs");
    let slot = BASE + busy * LAYOUT_V1_ASSET_SLOT_LEN;

    // Not the market authority.
    let mut env = env_with(fixture.clone(), true);
    let before = market_data(&env);
    let stranger = Keypair::new();
    env.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    assert!(migrate(&mut env, &stranger).is_err(), "stranger refused");
    assert_eq!(market_data(&env), before);

    // Not rent exempt at the new length.
    let mut env = env_with(fixture.clone(), false);
    let before = market_data(&env);
    assert!(migrate_as_auth(&mut env).is_err(), "short of rent refused");
    assert_eq!(market_data(&env), before);

    // A side haircut since its reset: the raw-basis sum is not recoverable.
    let mut cut = fixture.clone();
    let a = slot + o.asset + offset_of!(AssetStateV16Account, a_long);
    cut[a..a + 16].copy_from_slice(&(ADL_ONE - 1).to_le_bytes());
    let mut env = env_with(cut, true);
    let before = market_data(&env);
    assert!(migrate_as_auth(&mut env).is_err(), "haircut side refused");
    assert_eq!(market_data(&env), before);

    // A provider topped up backing once: its principal is not recoverable.
    let mut topped = fixture.clone();
    let seq = slot + ASSET_CONTROL_SEQUENCES_OFF + offset_of!(AssetControlSequencesV16, backing_top_up);
    topped[seq..seq + 8].copy_from_slice(&1u64.to_le_bytes());
    let mut env = env_with(topped, true);
    let before = market_data(&env);
    assert!(migrate_as_auth(&mut env).is_err(), "provider top-up refused");
    assert_eq!(market_data(&env), before);
}
