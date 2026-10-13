//! A winner's open gain is burned at the domain's credit rate when the price turns, and what
//! is burned beyond the rate reaches no account. Whether it happens depends only on which
//! accounts were cranked at the turning point, which anyone can choose.
//!
//! Public instructions only, through LiteSVM, on the deployed SBF.

#[allow(dead_code)]
mod support;

use percolator_prog::ix::CrankObservationHint;
use support::v16_svm::{MarketConfig, V16Svm};

const P0: u64 = 1_000_000; // 1.00
const PEAK: u64 = 1_140_000; // +14%
const END: u64 = 855_000; // -25% from the peak
const USD: u128 = 1_000_000;
const Q: i128 = percolator::POS_SCALE as i128;
const FEED: [u8; 32] = [0xa0; 32];

/// Actors: 0 long 4,500 on 1,000 of capital; 1 long 2,000; 2 short 1,500; 3 the maker on the
/// other side of all three (net short 5,000); 4 holds nothing and only accrues the asset.
struct World {
    env: V16Svm,
    pyth: solana_sdk::pubkey::Pubkey,
    slot: u64,
    ts: i64,
}

impl World {
    fn new(seed: u8) -> Self {
        let mut env = V16Svm::new(
            [seed; 32],
            MarketConfig {
                initial_price: P0,
                h_max: 6_480_000,
                min_nonzero_mm_req: 500,
                min_nonzero_im_req: 600,
                maintenance_margin_bps: 1_000,
                initial_margin_bps: 1_000,
                max_price_move_bps_per_slot: 49,
                max_accrual_dt_slots: 10,
                min_funding_lifetime_slots: 10_000_000,
                actor_deposits: [1_000 * USD, 1_000 * USD, 1_000 * USD, 20_000 * USD, 1_000 * USD],
                actor_token_balances: [2_000_000_000, 2_000_000_000, 2_000_000_000, 40_000_000_000, 2_000_000_000],
                ..MarketConfig::default()
            },
        );
        env.set_clock(1, 100);
        let pyth = env.set_pyth_price(&FEED, P0 as i64, -6, 0, 100);
        env.configure_hybrid_oracle(0, 1, 100, 0, [FEED, [0; 32], [0; 32]], &[pyth], 10, 0).expect("oracle");
        env.trade_no_cpi(0, 3, 0, 4_500 * Q, P0, 0).expect("actor 0 long 4,500");
        env.trade_no_cpi(1, 3, 0, 2_000 * Q, P0, 0).expect("actor 1 long 2,000");
        env.trade_no_cpi(2, 3, 0, -1_500 * Q, P0, 0).expect("actor 2 short 1,500");
        Self { env, pyth, slot: 1, ts: 100 }
    }

    fn hint() -> Vec<CrankObservationHint> {
        vec![CrankObservationHint { asset_index: 0, oracle_accounts: 1 }]
    }

    /// Moves the oracle to `price` and accrues the asset (through actor 4, who holds nothing)
    /// until the effective price is there. No account with a position is touched.
    fn move_price(&mut self, price: u64) {
        for _ in 0..400 {
            self.slot += 1;
            self.ts += 1;
            self.env.set_clock(self.slot, self.ts);
            self.env.update_pyth_price(self.pyth, &FEED, price as i64, -6, 0, self.ts);
            let _ = self.env.crank_with_oracles(4, self.slot, Self::hint(), std::slice::from_ref(&self.pyth));
            if self.env.primary_market_state().1.assets[0].effective_price == price {
                return;
            }
        }
        panic!("effective price did not reach {price}");
    }

    /// A permissionless crank on one actor's portfolio at the current slot.
    fn crank(&mut self, actor: usize) {
        let _ = self.env.crank_with_oracles(actor, self.slot, Self::hint(), std::slice::from_ref(&self.pyth));
    }

    fn account(&self, actor: usize) -> (i128, i128) {
        let p = self.env.primary_portfolio(actor);
        (p.capital.get() as i128, p.pnl.get())
    }
}

/// Runs 1.00 -> 1.14 -> 0.855, cranking `at_peak` at the peak and everyone at the end.
/// Returns each of actors 0..=3's change in capital + pnl, and actor 0's final (capital, pnl).
fn run(seed: u8, at_peak: &[usize]) -> ([i128; 4], (i128, i128)) {
    let mut w = World::new(seed);
    let start: Vec<i128> = (0..4).map(|a| { let (c, p) = w.account(a); c + p }).collect();
    w.move_price(PEAK);
    for a in at_peak {
        w.crank(*a);
    }
    if !at_peak.is_empty() {
        let (c, p) = w.account(0);
        println!("    at the peak actor 0 holds capital {c}, pnl {p}");
    }
    w.move_price(END);
    for a in [0, 3, 1, 2, 3, 0] {
        w.crank(a);
    }
    let g = w.env.primary_market_state().1;
    println!("    market totals: vault {}  c_tot {}  insurance {}  pnl_pos_tot {}  => vault - c_tot - insurance = {}", g.vault, g.c_tot, g.insurance, g.pnl_pos_tot, g.vault as i128 - g.c_tot as i128 - g.insurance as i128);
    let mut change = [0i128; 4];
    for a in 0..4 {
        let (c, p) = w.account(a);
        change[a] = c + p - start[a];
    }
    (change, w.account(0))
}

#[test]
fn a_winners_gain_is_burned_at_the_credit_rate_when_only_winners_were_cranked_at_the_peak() {
    // What actor 0's position alone loses: 4,500 x (0.855 - 1.00).
    let plain = -652_500_000i128;
    let mut out = vec![];
    for (name, at_peak) in [
        ("nobody cranked at the peak", &[][..]),
        ("everyone cranked at the peak", &[3, 2, 0, 1][..]),
        ("only actor 0 cranked at the peak (no loss booked: credit rate 0)", &[0][..]),
        ("winners and the short trader cranked, the maker not (210 of 910 backed: 23%)", &[0, 1, 2][..]),
    ] {
        println!("{name}:");
        let (change, end) = run(71, at_peak);
        let sum: i128 = change.iter().sum();
        println!("    actor 0 {:>13}  actor 1 {:>13}  actor 2 {:>13}  maker {:>13}  sum {:>13}   actor 0 ends with capital {}, pnl {}", change[0], change[1], change[2], change[3], sum, end.0, end.1);
        out.push((change, sum));
    }
    // Nobody, or everybody: exact and zero-sum.
    assert_eq!(out[0].0[0], plain);
    assert_eq!(out[1].0[0], plain);
    assert_eq!((out[0].1, out[1].1), (0, 0));
    // No backing at all: still exact.
    assert_eq!((out[2].0[0], out[2].1), (plain, 0));
    // Partial backing: actor 0 should lose the same.
    assert_eq!(out[3].0[0], plain, "actor 0 lost {} where its position lost {plain}; {} reached no account", out[3].0[0], -out[3].1);
}
