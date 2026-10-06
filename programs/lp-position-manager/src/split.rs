//! Pure LP fee-split arithmetic.
//!
//! Extracted so it can be unit-tested without a validator. lp-position-manager is
//! one of only two programs with **no coverage anywhere** — no unit tests, no
//! integration tests, and it is not even a dependency of the integration test
//! crate — despite splitting real fee revenue.
//!
//! # 2026-08-21 — u128 intermediate arithmetic
//!
//! `apply_bps` previously used pure `u64` `checked_mul`. That is the same
//! overflow class that produced `MathOverflow (6030)` in the transfer-hook at
//! mainnet supply scale. All BPS math now uses a `u128` intermediate then
//! narrows with `try_from`.
//!
//! # The tables here MATCH the transfer hook (resolved 2026-07-22)
//!
//! This program previously carried the pre-correction figures — incoming 34%,
//! treasury 7%, burn 3% on the KOTS side, and incoming 34% / treasury 10% on the
//! SOL side. When the transfer hook's split was corrected on 2026-07-22,
//! lp-position-manager was not updated in that pass, so the two programs disagreed
//! about where LP revenue goes: the Captain's incoming share was receiving 34%
//! while treasury took 7% — the inverse of the ruling.
//!
//! Both tables summed to exactly 10_000, so each looked correct in isolation and
//! nothing compared them. That is how the divergence survived, in the one program
//! with no coverage of any kind.
//!
//! Both are now aligned with the hook:
//!
//! | leg | SOL | KOTS |
//! |---|---:|---:|
//! | jackpot | 5000 | 5000 |
//! | incoming (Captain's) | 700 | 700 |
//! | admin | 300 | 300 |
//! | treasury | 3700 | 3600 |
//! | dev | 300 | 300 |
//! | burn | — (SOL is not burned) | 100 |
//!
//! The SOL treasury share is 3700 rather than 3600 because it absorbs the notional
//! burn-redirect: there is no SOL burn leg.
//!
//! `kots_split_matches_the_transfer_hook_table` below now guards this. It was
//! written to fail while the divergence existed; keep it so a future edit to one
//! program cannot silently desynchronise the other.

/// Basis-point denominator.
pub const BPS_DENOMINATOR: u64 = 10_000;

// ── SOL-side split, as currently compiled into lp-position-manager ───────────
pub const JACKPOT_BPS_SOL: u64 = 5_000;
pub const INCOMING_BPS_SOL: u64 = 700; // Captain's share
pub const ADMIN_BPS_SOL: u64 = 300;
pub const TREASURY_BPS_SOL: u64 = 3_700; // includes the burn-redirect; SOL is not burned
pub const DEV_BPS_SOL: u64 = 300;

// ── KOTS-side split, as currently compiled into lp-position-manager ──────────
pub const JACKPOT_BPS_KOTS: u64 = 5_000;
pub const INCOMING_BPS_KOTS: u64 = 700; // Captain's share
pub const ADMIN_BPS_KOTS: u64 = 300;
pub const TREASURY_BPS_KOTS: u64 = 3_500; // 35% — LP-1 fix: the 1% bounty is carved from here (was 3_600). LP treasury (3_500) + bounty (100) = 3_600 = the hook's treasury (remainder) share.
pub const DEV_BPS_KOTS: u64 = 300;
pub const BURN_BPS_KOTS: u64 = 100;
pub const BOUNTY_BPS_KOTS: u64 = 100; // 1% owner bounty, carved from treasury above. Recipient = kots_common::constants::BOUNTY_WALLET (fee-exempt in the hook). Moved here from lib.rs (LP-1 fix).

/// A SOL-side split. No burn leg — SOL is never burned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SolSplit {
    pub jackpot: u64,
    pub incoming: u64,
    pub admin: u64,
    pub treasury: u64,
    pub dev: u64,
}

impl SolSplit {
    pub fn total(&self) -> u64 {
        self.jackpot
            .saturating_add(self.incoming)
            .saturating_add(self.admin)
            .saturating_add(self.treasury)
            .saturating_add(self.dev)
    }
}

/// A KOTS-side split, including the burn leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct KotsSplit {
    pub jackpot: u64,
    pub incoming: u64,
    pub admin: u64,
    pub treasury: u64,
    pub dev: u64,
    pub burn: u64,
    pub bounty: u64,
}

impl KotsSplit {
    pub fn total(&self) -> u64 {
        self.jackpot
            .saturating_add(self.incoming)
            .saturating_add(self.admin)
            .saturating_add(self.treasury)
            .saturating_add(self.dev)
            .saturating_add(self.burn)
            .saturating_add(self.bounty)
    }
}

/// Apply a basis-point rate, rounding down.
///
/// # 2026-08-21 — u128 intermediate arithmetic
///
/// Mainnet-scale amounts (or any value near `u64::MAX / bps`) overflow a pure
/// `u64` `checked_mul` before the divide. The same class of bug that produced
/// `MathOverflow (6030)` in the transfer-hook anti-whale path. Use a `u128`
/// intermediate, then narrow with `try_from`. Returns `None` only when the
/// final result does not fit in `u64`.
pub fn apply_bps(amount: u64, bps: u64) -> Option<u64> {
    let v = (amount as u128)
        .checked_mul(bps as u128)?
        .checked_div(BPS_DENOMINATOR as u128)?;
    u64::try_from(v).ok()
}

/// Split SOL-denominated LP revenue.
///
/// Every leg is a share of the total; integer division rounds each down, so the
/// sum can fall a few units short of the input. That residue is currently
/// **unassigned** — see `sol_split_can_leave_dust_unassigned`.
pub fn split_sol(total: u64) -> Option<SolSplit> {
    Some(SolSplit {
        jackpot: apply_bps(total, JACKPOT_BPS_SOL)?,
        incoming: apply_bps(total, INCOMING_BPS_SOL)?,
        admin: apply_bps(total, ADMIN_BPS_SOL)?,
        treasury: apply_bps(total, TREASURY_BPS_SOL)?,
        dev: apply_bps(total, DEV_BPS_SOL)?,
    })
}

/// Split KOTS-denominated LP revenue.
pub fn split_kots(total: u64) -> Option<KotsSplit> {
    Some(KotsSplit {
        jackpot: apply_bps(total, JACKPOT_BPS_KOTS)?,
        incoming: apply_bps(total, INCOMING_BPS_KOTS)?,
        admin: apply_bps(total, ADMIN_BPS_KOTS)?,
        treasury: apply_bps(total, TREASURY_BPS_KOTS)?,
        dev: apply_bps(total, DEV_BPS_KOTS)?,
        burn: apply_bps(total, BURN_BPS_KOTS)?,
        bounty: apply_bps(total, BOUNTY_BPS_KOTS)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the tables ───────────────────────────────────────────────────────────

    #[test]
    fn sol_bps_constants_sum_to_one_hundred_percent() {
        let sum = JACKPOT_BPS_SOL + INCOMING_BPS_SOL + ADMIN_BPS_SOL + TREASURY_BPS_SOL + DEV_BPS_SOL;
        assert_eq!(sum, BPS_DENOMINATOR, "SOL split must total 100%");
    }

    #[test]
    fn kots_bps_constants_sum_to_one_hundred_percent() {
        let sum = JACKPOT_BPS_KOTS
            + INCOMING_BPS_KOTS
            + ADMIN_BPS_KOTS
            + TREASURY_BPS_KOTS
            + DEV_BPS_KOTS
            + BURN_BPS_KOTS
            + BOUNTY_BPS_KOTS; // LP-1 fix: bounty included so an over-100% total fails here
        assert_eq!(sum, BPS_DENOMINATOR, "KOTS split (incl. bounty) must total 100%");
    }

    #[test]
    fn kots_split_matches_the_transfer_hook_table() {
        // RESOLVED 2026-07-22 — this now PASSES and stands as the guard.
        //
        // It was written to fail while lp-position-manager still carried the
        // pre-correction table (incoming 34% / treasury 7% / burn 3%) against the
        // hook's corrected 7% / 36% / 1%. Both summed to 10_000, so neither looked
        // wrong alone and nothing compared them.
        //
        // Keep this test. It is the only thing tying the two programs' tables
        // together; without it a future edit to one silently desynchronises the
        // other, which is exactly what happened the first time.
        const HOOK_INCOMING_BPS: u64 = 700;
        const HOOK_TREASURY_BPS: u64 = 3_600;
        const HOOK_BURN_BPS: u64 = 100;

        assert_eq!(
            INCOMING_BPS_KOTS, HOOK_INCOMING_BPS,
            "incoming diverges from the corrected Captain's share"
        );
        // LP-1 fix: lp carves the 1% bounty out of treasury, while the hook has NO
        // bounty leg (bounty wallet is merely fee-exempt) and pays treasury as the
        // REMAINDER. The reconciling identity is lp treasury + bounty == hook treasury.
        assert_eq!(
            TREASURY_BPS_KOTS + BOUNTY_BPS_KOTS, HOOK_TREASURY_BPS,
            "lp treasury + bounty must equal the hook's treasury (remainder) share"
        );
        assert_eq!(
            BURN_BPS_KOTS, HOOK_BURN_BPS,
            "burn diverges from the corrected 1%"
        );
    }

    #[test]
    fn the_captains_share_is_not_the_largest_non_jackpot_leg() {
        // A shape check independent of the exact numbers, kept as a second signal
        // alongside the table comparison. Under the ruling treasury takes 36% and
        // the Captain's incoming share 7%, so incoming must be the SMALLER of the
        // two. An inverted pair is precisely the bug corrected on 2026-07-22.
        assert!(
            INCOMING_BPS_KOTS < TREASURY_BPS_KOTS,
            "incoming ({INCOMING_BPS_KOTS}) should be well below treasury \
             ({TREASURY_BPS_KOTS}); an inverted pair is the exact bug corrected \
             in the transfer hook on 2026-07-22"
        );
    }

    // ── split behaviour: conservation and limits ────────────────────────────

    #[test]
    fn sol_split_of_a_round_amount_is_exact() {
        let s = split_sol(1_000_000).unwrap();
        assert_eq!(s.jackpot, 500_000); // 50%
        assert_eq!(s.incoming, 70_000); //  7% — Captain's share
        assert_eq!(s.admin, 30_000); //  3%
        assert_eq!(s.treasury, 370_000); // 37%
        assert_eq!(s.dev, 30_000); //  3%
        assert_eq!(s.total(), 1_000_000, "a round amount must divide exactly");
    }

    #[test]
    fn kots_split_of_a_round_amount_is_exact() {
        let s = split_kots(1_000_000).unwrap();
        assert_eq!(s.jackpot, 500_000); // 50%
        assert_eq!(s.incoming, 70_000); //  7% — Captain's share
        assert_eq!(s.admin, 30_000); //  3%
        assert_eq!(s.treasury, 350_000); // 35% (LP-1 fix: was 36%; bounty carved out)
        assert_eq!(s.dev, 30_000); //  3%
        assert_eq!(s.burn, 10_000); //  1%
        assert_eq!(s.bounty, 10_000); //  1% owner bounty
        assert_eq!(s.total(), 1_000_000);
    }

    #[test]
    fn sol_split_can_leave_dust_unassigned() {
        // Unlike the transfer hook — where treasury takes the REMAINDER so nothing
        // is stranded — every leg here floors independently. The shortfall is
        // small but it is never credited to anyone, so it accumulates in whatever
        // account the total was drawn from.
        //
        // Pinned as known behaviour; whether to add a remainder leg is a product
        // decision.
        let s = split_sol(9_999).unwrap();
        assert!(s.total() <= 9_999, "the split must never exceed the input");
        assert!(s.total() < 9_999, "and here it falls short — dust is unassigned");
    }

    #[test]
    fn kots_split_can_leave_dust_unassigned() {
        let s = split_kots(9_999).unwrap();
        assert!(s.total() <= 9_999);
        assert!(s.total() < 9_999);
    }

    #[test]
    fn no_split_ever_exceeds_its_input() {
        // The invariant that must hold at every scale: paying out more than came
        // in would drain an adjacent account.
        for total in [
            0u64, 1, 2, 3, 7, 99, 100, 9_999, 10_000, 10_001, 123_456_789,
            1_000_000_000, 1_000_000_000_000_000,
        ] {
            assert!(split_sol(total).unwrap().total() <= total, "SOL at {total}");
            assert!(split_kots(total).unwrap().total() <= total, "KOTS at {total}");
        }
    }

    #[test]
    fn dust_shortfall_is_bounded_by_the_leg_count() {
        // Each leg loses at most one unit to flooring, so the shortfall can never
        // exceed the number of legs. If it ever does, a leg is miscomputed.
        for total in 0u64..3_000 {
            let sol = split_sol(total).unwrap();
            assert!(total - sol.total() < 5, "SOL dust too large at {total}");
            let kots = split_kots(total).unwrap();
            assert!(total - kots.total() < 7, "KOTS dust too large at {total}"); // LP-1 fix: 7 legs now (bounty added)
        }
    }

    #[test]
    fn a_zero_total_splits_to_all_zeros() {
        assert_eq!(split_sol(0).unwrap(), SolSplit::default());
        assert_eq!(split_kots(0).unwrap(), KotsSplit::default());
    }

    #[test]
    fn dust_totals_floor_every_leg_to_zero() {
        // Below 1/5000th of the denominator even the largest leg floors away.
        let s = split_kots(1).unwrap();
        assert_eq!(s.total(), 0, "a single unit cannot be split at all");
    }

    #[test]
    fn u64_max_no_longer_overflows_with_u128_intermediate() {
        // Pre-fix: pure u64 checked_mul(u64::MAX, 5000) returned None.
        // Post-fix: u128 intermediate succeeds; result is u64::MAX / 2 for 50% leg.
        let sol = split_sol(u64::MAX).expect("u128 intermediate must accept u64::MAX");
        let kots = split_kots(u64::MAX).expect("u128 intermediate must accept u64::MAX");
        assert_eq!(sol.jackpot, u64::MAX / 2);
        assert_eq!(kots.jackpot, u64::MAX / 2);
        assert!(sol.total() <= u64::MAX);
        assert!(kots.total() <= u64::MAX);
    }

    #[test]
    fn mainnet_scale_lp_fees_no_overflow() {
        // Even a pathological LP fee equal to the full mainnet supply is safe.
        const MAINNET_SUPPLY: u64 = 1_111_111_111_000_000_000; // 1.111B * 1e9
        assert!(split_kots(MAINNET_SUPPLY).is_some());
        assert!(split_sol(MAINNET_SUPPLY).is_some());
        let s = split_kots(MAINNET_SUPPLY).unwrap();
        assert_eq!(s.jackpot, MAINNET_SUPPLY / 2);
    }

    #[test]
    fn the_largest_total_still_splits() {
        // With u128 intermediate every u64 amount is safe for bps ≤ 10_000.
        assert!(split_kots(u64::MAX).is_some(), "u64::MAX must work");
        assert!(split_sol(u64::MAX).is_some());
    }

    #[test]
    fn jackpot_is_always_the_largest_leg() {
        // At 50% it must dominate every other leg at any non-trivial scale.
        for total in [10_000u64, 1_000_000, 1_000_000_000] {
            let s = split_kots(total).unwrap();
            for (name, leg) in [
                ("incoming", s.incoming),
                ("admin", s.admin),
                ("treasury", s.treasury),
                ("dev", s.dev),
                ("burn", s.burn),
            ] {
                assert!(s.jackpot >= leg, "jackpot must dominate {name} at {total}");
            }
        }
    }

    #[test]
    fn splits_scale_linearly() {
        // Doubling the input doubles every leg, for any amount that divides evenly.
        let a = split_kots(1_000_000).unwrap();
        let b = split_kots(2_000_000).unwrap();
        assert_eq!(b.jackpot, a.jackpot * 2);
        assert_eq!(b.incoming, a.incoming * 2);
        assert_eq!(b.burn, a.burn * 2);
    }
}
