//! Replays cranks against a saved copy of a live market's accounts (REPLAY_DIR: accounts.txt and
//! cranks.json, written by percolator-vault/app/scripts/snapshot.ts), on the program binary
//! REPLAY_SO (default: this tree's build). Without REPLAY_DIR the tests do nothing.
//! A copy of X1 testnet taken on 2026-10-13 while an account could not be settled with the
//! engine of upstream PR #226 is in ../issues/x1-underflow-2026-10-13.
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account, clock::Clock, compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction}, pubkey::Pubkey, signature::{Keypair, Signer},
    transaction::Transaction,
};
use std::str::FromStr;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

#[test]
fn replay() {
    let Ok(dir) = std::env::var("REPLAY_DIR") else { return };
    let so = std::env::var("REPLAY_SO").unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_prog.so").into());
    let lags: usize = std::env::var("REPLAY_LAGS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let extra: u64 = std::env::var("REPLAY_SLOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let program = Pubkey::from_str("8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P").unwrap();
    let mut svm = LiteSVM::new();
    svm.add_program(program, &std::fs::read(so).unwrap());
    for line in std::fs::read_to_string(format!("{dir}/accounts.txt")).unwrap().lines() {
        let p: Vec<&str> = line.split(' ').collect();
        svm.set_account(
            Pubkey::from_str(p[0]).unwrap(),
            Account { lamports: p[1].parse().unwrap(), data: hex(p[3]), owner: Pubkey::from_str(p[2]).unwrap(), executable: false, rent_epoch: 0 },
        )
        .unwrap();
    }
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/cranks.json")).unwrap()).unwrap();
    let slot = j["slot"].as_u64().unwrap() + extra;
    let mut clock: Clock = svm.get_sysvar();
    clock.slot = slot;
    clock.unix_timestamp = 1_791_892_000;
    svm.set_sysvar(&clock);
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
    let ix = |v: &serde_json::Value| {
        let keys: Vec<Pubkey> = v["keys"].as_array().unwrap().iter().map(|k| Pubkey::from_str(k.as_str().unwrap()).unwrap()).collect();
        Instruction {
            program_id: program,
            accounts: vec![AccountMeta::new(payer.pubkey(), true), AccountMeta::new(keys[1], false), AccountMeta::new(keys[2], false)],
            data: hex(v["data"].as_str().unwrap()),
        }
    };
    let which = std::env::var("REPLAY_WHICH").unwrap_or("settleAlone".into());
    let mut ixs = vec![ComputeBudgetInstruction::set_compute_unit_limit(1_400_000)];
    for _ in 0..lags { ixs.push(ix(&j["lagCrank"])); }
    for s in j[which.as_str()].as_array().unwrap() { ixs.push(ix(s)); }
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[&payer], svm.latest_blockhash());
    match svm.send_transaction(tx) {
        Ok(m) => println!("RESULT ok, cu {}\n{}", m.compute_units_consumed, m.logs.iter().filter(|l| l.contains("log")).cloned().collect::<Vec<_>>().join("\n")),
        Err(e) => println!("RESULT {:?}\n{}", e.err, e.meta.logs.iter().rev().take(14).rev().cloned().collect::<Vec<_>>().join("\n")),
    }
}

/// Every out-of-date account of a snapshot, one transaction each, after the lag cranks.
/// REPLAY_REVERSE=1 settles them in the opposite order.
#[test]
fn sweep() {
    let Ok(dir) = std::env::var("REPLAY_DIR") else { return };
    let so = std::env::var("REPLAY_SO").unwrap_or(concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/percolator_prog.so").into());
    let program = Pubkey::from_str("8o3uV87X2CvPYfPwM1sxeaYYE7sGWsTUiEy7SskEMM3P").unwrap();
    let mut svm = LiteSVM::new();
    svm.add_program(program, &std::fs::read(so).unwrap());
    for line in std::fs::read_to_string(format!("{dir}/accounts.txt")).unwrap().lines() {
        let p: Vec<&str> = line.split(' ').collect();
        svm.set_account(Pubkey::from_str(p[0]).unwrap(), Account { lamports: p[1].parse().unwrap(), data: hex(p[3]), owner: Pubkey::from_str(p[2]).unwrap(), executable: false, rent_epoch: 0 }).unwrap();
    }
    let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/cranks.json")).unwrap()).unwrap();
    let mut clock: Clock = svm.get_sysvar();
    clock.slot = j["slot"].as_u64().unwrap() + 2;
    clock.unix_timestamp = 1_791_892_000;
    svm.set_sysvar(&clock);
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
    let ix = |v: &serde_json::Value| {
        let keys: Vec<Pubkey> = v["keys"].as_array().unwrap().iter().map(|k| Pubkey::from_str(k.as_str().unwrap()).unwrap()).collect();
        Instruction { program_id: program, accounts: vec![AccountMeta::new(payer.pubkey(), true), AccountMeta::new(keys[1], false), AccountMeta::new(keys[2], false)], data: hex(v["data"].as_str().unwrap()) }
    };
    let mut n = 0u32;
    let mut send = |svm: &mut LiteSVM, i: Instruction| -> Result<u64, String> {
        n += 1;
        // A distinct compute price makes each transaction unique.
        let ixs = [ComputeBudgetInstruction::set_compute_unit_limit(1_400_000), ComputeBudgetInstruction::set_compute_unit_price(n as u64), i];
        let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[&payer], svm.latest_blockhash());
        svm.send_transaction(tx).map(|m| m.compute_units_consumed).map_err(|e| format!("{:?}", e.err))
    };
    let mut lags = 0;
    while lags < 60 && send(&mut svm, ix(&j["lagCrank"])).is_ok() { lags += 1; }
    let mut list: Vec<&serde_json::Value> = j["settleAlone"].as_array().unwrap().iter().collect();
    if std::env::var("REPLAY_REVERSE").is_ok() { list.reverse(); }
    let mut tally: std::collections::BTreeMap<String, u32> = Default::default();
    let mut pending = list.clone();
    for round in 0..3 {
        let mut next = vec![];
        for v in pending {
            match send(&mut svm, ix(v)) {
                Ok(_) => *tally.entry(format!("ok (round {round})")).or_default() += 1,
                Err(e) => { if round == 2 || !e.contains("Custom(25)") { *tally.entry(e).or_default() += 1; } else { next.push(v); } }
            }
        }
        pending = next;
    }
    println!("SWEEP {dir}: lag cranks {lags}, accounts {}: {tally:?}", list.len());
}
