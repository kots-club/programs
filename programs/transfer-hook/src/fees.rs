//! Pure fee, split and cap arithmetic.
//!
//! # Why this module exists
//!
//! Every one of these functions used to be written inline inside an instruction
//! handler, which made it unreachable from a unit test: to exercise
//! `(pot * JACKPOT_BPS) / 10_000` you needed a validator, a mint, a funded wallet
//! and a signed transaction. That is the reason all nine programs carried zero
//! unit tests — not neglect, but shape.
//!
//! It also let four real money bugs ship, each of which is pure integer arithmetic
//! and would have been caught in milliseconds by the tests below:
//!
//! 1. **The split applied each leg's bps to a shrinking remainder** instead of to
//!    the pot, so only the first leg got its nominal share. The Captain's 7%
//!    incoming share paid **3.5%**, admin 3%→1.4%, dev 3%→1.35%, burn 1%→0.44%,
//!    and treasury silently absorbed the difference at **43.3%** instead of 36%.
//! 2. **The exempt refund was hardcoded `amount / 100`** (1%) rather than derived
//!    from the configured rate, so at a 3% extension a whitelisted wallet would be
//!    refunded a third of what was taken and net 2% instead of 0%.
//! 3. **The over-cap threshold ignored the fee haircut**, so a transfer sized at
//!    `cap + margin` landed the recipient *under* the cap and the anti-whale guard
//!    never fired.
//! 4. **Burn-leg sizing** silently changed meaning when the burn moved from a
//!    transfer-to-sink to a real supply reduction.
//!
//! # 2026-08-21 — u128 intermediate arithmetic
//!
//! Mainnet supply is `1_111_111_111 * 10^9 ≈ 1.11e18`. Multiplying that by even
//! modest BPS rates (300, 5000, …) **overflows u64 before the divide**. That is
//! exactly the `MathOverflow` that blocked the first mainnet transfer test.
//!
//! All BPS math in this module now uses a `u128` intermediate, then narrows
//! back to `u64` with `try_from`. The only `None` paths left are:
//! * true arithmetic failure (result does not fit in `u64`), or
//! * unsolvable inputs (`fee_bps >= DENOM` for gross-up / min-amount).
//!
//! This module is deliberately **dependency-free** — no `anchor_lang`, no Solana
//! types, nothing that needs a runtime. It compiles and tests in milliseconds with
//! plain `cargo test -p transfer-hook`, so there is no excuse not to run it.
//!
//! # Invariants worth stating
//!
//! * The KOTS bps constants sum to exactly `10_000`. That is what makes treasury's
//!   remainder land on its nominal 34% — and it is the proof that bug (1) was a
//!   bug rather than an intentional cascade.
//! * Treasury is always paid as the **remainder**, never from its own constant, so
//!   integer-division dust is never stranded. `TREASURY_BPS_KOTS` exists only to
//!   document the intent.
//! * All arithmetic is checked. Nothing here may panic or silently wrap on
//!   adversarial input.

/// Basis points denominator. 10_000 bps = 100%.
pub const BPS_DENOMINATOR: u64 = 10_000;

// ── The KOTS distribution table ──────────────────────────────────────────────
// Guide law (kingoftheshill.com/guide): 50 / 7 / 34 / 3 / 3 / 1 / 2.
// DUAL-PATH (2026-08-27): this module is the NEXT compile of the hook. The
// currently deployed hook still treats treasury as a 36% remainder with no
// bounty leg. Live trading-tax payouts until the upgrade are the sidecar
// keeper (transferChecked + burnChecked), not this binary.
pub const JACKPOT_BPS_KOTS: u64 = 5_000; // 50%
pub const INCOMING_BPS_KOTS: u64 = 700; // 7%  — Captain's share
pub const TREASURY_BPS_KOTS: u64 = 3_400; // 34% — documentation; paid as remainder
pub const ADMIN_BPS_KOTS: u64 = 300; // 3%
pub const DEV_BPS_KOTS: u64 = 300; // 3%
pub const BURN_BPS_KOTS: u64 = 100; // 1%  — a real burn; supply falls
pub const BOUNTY_BPS_KOTS: u64 = 200; // 2% — owner bounty

/// Anti-whale: no non-exempt account may hold more than this share of supply.
pub const MAX_HOLD_BPS: u64 = 300; // 3%

/// Mainnet total supply in raw units (1.111_111_111e9 whole tokens × 1e9 decimals).
/// Used only by regression tests that pin full-supply arithmetic.
#[cfg(test)]
const MAINNET_SUPPLY_RAW: u64 = 1_111_111_111u64 * 1_000_000_000;

/// How a transfer is classified for fee purposes. The extension withholds the
/// SELL rate on every transfer; the rebate owed depends on the class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeeClass {
    /// Sender or receiver whitelisted — the whole withheld amount comes back.
    Exempt,
    /// Destination is a registered pool — nothing comes back.
    Sell,
    /// Everything else, including wallet-to-wallet — rebated to the buy rate.
    Buy,
}

impl FeeClass {
    /// Classify from the two facts the hook can establish on-chain.
    ///
    /// Exemption wins over pool-destination: a whitelisted party trading into a
    /// pool is still fee-free.
    pub fn classify(is_exempt: bool, is_pool_destination: bool) -> Self {
        if is_exempt {
            FeeClass::Exempt
        } else if is_pool_destination {
            FeeClass::Sell
        } else {
            FeeClass::Buy
        }
    }

    /// The NET rate this class should end up paying, in bps.
    pub fn net_bps(self, buy_bps: u64, sell_bps: u64) -> u64 {
        match self {
            FeeClass::Exempt => 0,
            FeeClass::Sell => sell_bps,
            FeeClass::Buy => buy_bps,
        }
    }
}

/// The seven legs of a distribution, in payout order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Split {
    pub jackpot: u64,
    pub incoming: u64,
    pub admin: u64,
    pub dev: u64,
    pub burn: u64,
    pub bounty: u64,
    /// Paid as the remainder, so dust is never stranded.
    pub treasury: u64,
}

impl Split {
    /// Sum of every leg. Must always equal the pot it was derived from.
    pub fn total(&self) -> u64 {
        self.jackpot
            .saturating_add(self.incoming)
            .saturating_add(self.admin)
            .saturating_add(self.dev)
            .saturating_add(self.burn)
            .saturating_add(self.bounty)
            .saturating_add(self.treasury)
    }
}

/// Apply a basis-point rate to an amount, rounding down.
///
/// Uses a `u128` intermediate so mainnet-scale supply × bps never overflows
/// before the divide. Returns `None` only when the final result does not fit
/// in `u64` (e.g. `bps > BPS_DENOMINATOR` on `u64::MAX`).
pub fn apply_bps(amount: u64, bps: u64) -> Option<u64> {
    let v = (amount as u128)
        .checked_mul(bps as u128)?
        .checked_div(BPS_DENOMINATOR as u128)?;
    u64::try_from(v).ok()
}

/// The fee the Transfer Fee Extension withholds at a given rate.
pub fn withheld_fee(amount: u64, fee_bps: u64) -> Option<u64> {
    apply_bps(amount, fee_bps)
}

/// What the recipient actually receives after the extension takes its cut.
pub fn net_received(amount: u64, fee_bps: u64) -> Option<u64> {
    amount.checked_sub(withheld_fee(amount, fee_bps)?)
}

/// The rebate owed to bring a class from the withheld rate down to its net rate.
///
/// Returns `None` if `net_bps > withheld_bps`, which would mean owing the payer
/// more than was taken — always a configuration error, never a valid state.
pub fn rebate_owed(amount: u64, withheld_bps: u64, net_bps: u64) -> Option<u64> {
    if net_bps > withheld_bps {
        return None;
    }
    let withheld = withheld_fee(amount, withheld_bps)?;
    let keep = withheld_fee(amount, net_bps)?;
    withheld.checked_sub(keep)
}

/// Gross up a rebate so the recipient NETS the intended amount despite the
/// extension taxing the rebate transfer itself.
///
/// Paying a bare rebate under-delivers: a 200bps rebate taxed at 3% nets ~194bps.
/// Solving `gross * (1 - fee) = target` gives
/// `gross = target * DENOM / (DENOM - fee_bps)`, rounded UP so the recipient is
/// never short by a dust unit. The extra withheld on the rebate flows back into
/// the fee pool, so the platform recaptures it.
///
/// Returns `None` if `fee_bps >= DENOM` (a 100% fee makes it unsolvable).
pub fn gross_up(target: u64, fee_bps: u64) -> Option<u64> {
    if fee_bps >= BPS_DENOMINATOR {
        return None;
    }
    let denom = (BPS_DENOMINATOR - fee_bps) as u128;
    let numer = (target as u128).checked_mul(BPS_DENOMINATOR as u128)?;
    // Ceiling division: (a + b - 1) / b
    let v = numer.checked_add(denom - 1)?.checked_div(denom)?;
    u64::try_from(v).ok()
}

/// Split a pot across the seven legs.
///
/// Each named leg takes its share **of the pot**, and treasury takes whatever is
/// left. Because the constants sum to exactly 10_000, treasury's remainder lands
/// on its nominal 34% plus any rounding dust.
///
/// This is the function whose absence allowed the successive-subtraction bug.
pub fn split_pot(pot: u64) -> Option<Split> {
    let jackpot = apply_bps(pot, JACKPOT_BPS_KOTS)?;
    let incoming = apply_bps(pot, INCOMING_BPS_KOTS)?;
    let admin = apply_bps(pot, ADMIN_BPS_KOTS)?;
    let dev = apply_bps(pot, DEV_BPS_KOTS)?;
    let burn = apply_bps(pot, BURN_BPS_KOTS)?;
    let bounty = apply_bps(pot, BOUNTY_BPS_KOTS)?;

    let assigned = jackpot
        .checked_add(incoming)?
        .checked_add(admin)?
        .checked_add(dev)?
        .checked_add(burn)?
        .checked_add(bounty)?;
    let treasury = pot.checked_sub(assigned)?;

    Some(Split {
        jackpot,
        incoming,
        admin,
        dev,
        burn,
        bounty,
        treasury,
    })
}

/// The anti-whale cap: the most a non-exempt account may hold.
///
/// Derived from **live** supply, so a real burn tightens it automatically.
/// Safe at mainnet supply (u128 intermediate via `apply_bps`).
pub fn whale_cap(supply: u64) -> Option<u64> {
    apply_bps(supply, MAX_HOLD_BPS)
}

/// Whether a post-transfer balance breaches the cap.
///
/// The check is `>` not `>=`: holding exactly the cap is permitted.
pub fn breaches_cap(post_transfer_balance: u64, cap: u64) -> bool {
    post_transfer_balance > cap
}

/// The smallest transfer amount that leaves the recipient strictly ABOVE `cap`
/// once the fee has been taken.
///
/// The recipient receives `amount - fee`, so clearing the cap requires
/// `amount * (DENOM - fee_bps) / DENOM > cap`. Sizing a test transfer at
/// `cap + small_margin` and forgetting the haircut is how the anti-whale guard
/// silently stopped firing when the cap grew.
pub fn min_amount_to_exceed_cap(cap: u64, fee_bps: u64) -> Option<u64> {
    if fee_bps >= BPS_DENOMINATOR {
        return None;
    }
    let denom = (BPS_DENOMINATOR - fee_bps) as u128;
    // Smallest x with x*denom/DENOM > cap  =>  x > cap*DENOM/denom
    let base = (cap as u128)
        .checked_mul(BPS_DENOMINATOR as u128)?
        .checked_div(denom)?;
    let base_u64 = u64::try_from(base).ok()?;
    base_u64.checked_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the table itself ─────────────────────────────────────────────────────

    #[test]
    fn kots_bps_constants_sum_to_exactly_one_hundred_percent() {
        // THE load-bearing invariant. If this ever fails, treasury's remainder
        // stops equalling its nominal share and the split silently misroutes —
        // which is exactly what the successive-subtraction bug did.
        let sum = JACKPOT_BPS_KOTS
            + INCOMING_BPS_KOTS
            + TREASURY_BPS_KOTS
            + ADMIN_BPS_KOTS
            + DEV_BPS_KOTS
            + BURN_BPS_KOTS
            + BOUNTY_BPS_KOTS;
        assert_eq!(sum, BPS_DENOMINATOR, "KOTS distribution must total 100%");
    }

    #[test]
    fn whale_cap_bps_is_three_percent() {
        assert_eq!(MAX_HOLD_BPS, 300);
    }

    // ── apply_bps ────────────────────────────────────────────────────────────

    #[test]
    fn apply_bps_basic_rates() {
        assert_eq!(apply_bps(10_000, 10_000), Some(10_000)); // 100%
        assert_eq!(apply_bps(10_000, 5_000), Some(5_000)); // 50%
        assert_eq!(apply_bps(10_000, 300), Some(300)); // 3%
        assert_eq!(apply_bps(10_000, 100), Some(100)); // 1%
        assert_eq!(apply_bps(10_000, 0), Some(0)); // 0%
    }

    #[test]
    fn apply_bps_zero_amount_is_zero_at_every_rate() {
        for bps in [0, 1, 100, 300, 5_000, 10_000] {
            assert_eq!(apply_bps(0, bps), Some(0));
        }
    }

    #[test]
    fn apply_bps_rounds_down() {
        // 1% of 99 is 0.99 -> 0. Dust is never invented.
        assert_eq!(apply_bps(99, 100), Some(0));
        assert_eq!(apply_bps(100, 100), Some(1));
        assert_eq!(apply_bps(199, 100), Some(1));
        assert_eq!(apply_bps(1, 5_000), Some(0)); // half of 1 unit
    }

    #[test]
    fn apply_bps_u128_handles_u64_max_at_denom() {
        // With u128 intermediate, MAX * DENOM / DENOM = MAX (fits in u64).
        assert_eq!(apply_bps(u64::MAX, BPS_DENOMINATOR), Some(u64::MAX));
        assert_eq!(apply_bps(u64::MAX, 0), Some(0));
        // Result exceeds u64 when rate > DENOM on max amount:
        assert_eq!(apply_bps(u64::MAX, BPS_DENOMINATOR + 1), None);
    }

    #[test]
    fn apply_bps_mainnet_supply_no_overflow() {
        // THE regression for the live MathOverflow (Error 6030).
        let supply = MAINNET_SUPPLY_RAW;
        let cap = apply_bps(supply, MAX_HOLD_BPS).expect("must not overflow at mainnet supply");
        // 3% of 1.111111111e18 = 3.333333333e16
        assert_eq!(cap, supply / 10_000 * 300);
        assert!(cap > 0);
    }

    #[test]
    fn apply_bps_largest_safe_amount() {
        let safe = u64::MAX / BPS_DENOMINATOR;
        assert_eq!(apply_bps(safe, BPS_DENOMINATOR), Some(safe));
    }

    // ── withheld / net ───────────────────────────────────────────────────────

    #[test]
    fn withheld_and_net_are_complementary() {
        for amount in [0u64, 1, 99, 100, 1_000, 1_000_000, 1_000_000_000] {
            for bps in [0u64, 100, 300, 5_000, 10_000] {
                let fee = withheld_fee(amount, bps).unwrap();
                let net = net_received(amount, bps).unwrap();
                assert_eq!(fee + net, amount, "fee+net must reconstruct the amount");
                assert!(fee <= amount, "fee can never exceed the amount");
            }
        }
    }

    #[test]
    fn three_percent_of_a_thousand_kots() {
        // 1_000 KOTS at 9 decimals, 3% -> 30 KOTS. The figure the matrix reports.
        let amount = 1_000u64 * 1_000_000_000;
        assert_eq!(withheld_fee(amount, 300), Some(30 * 1_000_000_000));
    }

    #[test]
    fn a_full_rate_fee_leaves_nothing() {
        assert_eq!(net_received(1_000, BPS_DENOMINATOR), Some(0));
    }

    // ── rebate ───────────────────────────────────────────────────────────────

    #[test]
    fn rebate_for_each_class_at_the_live_rates() {
        let amount = 1_000_000u64;
        // Withheld 3%; buys net 1%; sells net 3%; exempt nets 0%.
        assert_eq!(rebate_owed(amount, 300, 100), Some(20_000)); // 2% back
        assert_eq!(rebate_owed(amount, 300, 300), Some(0)); // sells keep it
        assert_eq!(rebate_owed(amount, 300, 0), Some(30_000)); // exempt: all back
    }

    #[test]
    fn rebate_refuses_to_owe_more_than_was_taken() {
        // net > withheld would mean paying the user to transact.
        assert_eq!(rebate_owed(1_000_000, 100, 300), None);
    }

    #[test]
    fn rebate_of_zero_amount_is_zero() {
        assert_eq!(rebate_owed(0, 300, 100), Some(0));
    }

    #[test]
    fn rebate_never_exceeds_the_withheld_fee() {
        for amount in [0u64, 1, 1_000, 1_000_000_000] {
            for (w, n) in [(300u64, 100u64), (300, 0), (100, 0), (10_000, 0)] {
                let reb = rebate_owed(amount, w, n).unwrap();
                let withheld = withheld_fee(amount, w).unwrap();
                assert!(reb <= withheld, "rebate must come out of what was taken");
            }
        }
    }

    // ── gross_up ─────────────────────────────────────────────────────────────

    #[test]
    fn gross_up_delivers_at_least_the_target_after_tax() {
        // The property that matters: after the rebate transfer is itself taxed,
        // the recipient must still net >= the target. Never short.
        for target in [1u64, 7, 100, 12_345, 1_000_000, 20_000_000_000] {
            for fee_bps in [0u64, 100, 300, 1_000] {
                let gross = gross_up(target, fee_bps).unwrap();
                let delivered = net_received(gross, fee_bps).unwrap();
                assert!(
                    delivered >= target,
                    "gross_up({target}, {fee_bps}) delivered {delivered}, short of target"
                );
            }
        }
    }

    #[test]
    fn gross_up_is_identity_when_there_is_no_fee() {
        assert_eq!(gross_up(1_000, 0), Some(1_000));
    }

    #[test]
    fn gross_up_overshoots_by_at_most_a_hair() {
        // Ceiling rounding may over-deliver, but only by a trivial margin —
        // otherwise the platform is quietly overpaying every rebate.
        for target in [1_000u64, 1_000_000, 1_000_000_000] {
            let gross = gross_up(target, 300).unwrap();
            let delivered = net_received(gross, 300).unwrap();
            assert!(delivered <= target + 2, "over-delivered {delivered} vs {target}");
        }
    }

    #[test]
    fn gross_up_is_unsolvable_at_a_total_fee() {
        assert_eq!(gross_up(1_000, BPS_DENOMINATOR), None);
        assert_eq!(gross_up(1_000, BPS_DENOMINATOR + 1), None);
    }

    #[test]
    fn gross_up_mainnet_scale_cap_no_overflow() {
        let cap = whale_cap(MAINNET_SUPPLY_RAW).unwrap();
        // gross_up of a whale-scale target at 3% must not overflow.
        let g = gross_up(cap, 300).expect("gross_up at mainnet cap must succeed");
        assert!(g >= cap);
    }

    // ── split_pot ────────────────────────────────────────────────────────────

    #[test]
    fn split_matches_the_published_table() {
        // 30 KOTS at 9 decimals — the pot the matrix collects.
        let pot = 30u64 * 1_000_000_000;
        let s = split_pot(pot).unwrap();
        assert_eq!(s.jackpot, 15_000_000_000); // 50%
        assert_eq!(s.incoming, 2_100_000_000); //  7%
        assert_eq!(s.admin, 900_000_000); //  3%
        assert_eq!(s.dev, 900_000_000); //  3%
        assert_eq!(s.burn, 300_000_000); //  1%
        assert_eq!(s.bounty, 600_000_000); //  2%
        assert_eq!(s.treasury, 10_200_000_000); // 34%
    }

    #[test]
    fn split_is_conservative_at_every_scale() {
        // The legs must always reconstruct the pot exactly — no leakage, no
        // invention. Treasury absorbs all rounding.
        for pot in [
            0u64,
            1,
            2,
            3,
            7,
            99,
            100,
            101,
            9_999,
            10_000,
            10_001,
            123_456_789,
            1_000_000_000,
            1_000_000_000_000_000,
            MAINNET_SUPPLY_RAW / 100, // large but realistic fee pot
        ] {
            let s = split_pot(pot).unwrap();
            assert_eq!(s.total(), pot, "split of {pot} did not reconstruct the pot");
        }
    }

    #[test]
    fn split_leg_shares_are_correct_not_cascading() {
        // The regression guard for the successive-subtraction bug. Under the old
        // code incoming received 7% of what REMAINED after jackpot (i.e. 3.5% of
        // the pot) and treasury swelled to ~43.3%. Pin the true proportions.
        let pot = 1_000_000_000u64;
        let s = split_pot(pot).unwrap();
        assert_eq!(s.jackpot, pot / 2, "jackpot must be 50% of the POT");
        assert_eq!(
            s.incoming,
            pot * 7 / 100,
            "incoming must be 7% of the POT, not 3.5%"
        );
        assert_eq!(s.admin, pot * 3 / 100);
        assert_eq!(s.dev, pot * 3 / 100);
        assert_eq!(s.burn, pot / 100);
        assert_eq!(s.bounty, pot * 2 / 100);
        assert_eq!(s.treasury, pot * 34 / 100, "treasury must be 34%, not 35% or 43.3%");
    }

    #[test]
    fn split_treasury_is_never_starved_by_rounding() {
        // Treasury takes the remainder, so it can only ever gain from dust.
        for pot in 0u64..2_000 {
            let s = split_pot(pot).unwrap();
            let nominal = apply_bps(pot, TREASURY_BPS_KOTS).unwrap();
            assert!(
                s.treasury >= nominal,
                "pot {pot}: treasury {} fell below its nominal {nominal}",
                s.treasury
            );
        }
    }

    #[test]
    fn split_of_dust_pots_goes_entirely_to_treasury() {
        // Below 2 units every percentage leg floors to zero, so treasury takes all.
        for pot in 0u64..2 {
            let s = split_pot(pot).unwrap();
            assert_eq!(s.jackpot, 0);
            assert_eq!(s.incoming, 0);
            assert_eq!(s.burn, 0);
            assert_eq!(s.treasury, pot);
        }
    }

    #[test]
    fn split_mainnet_scale_pot_no_overflow() {
        // Full supply as a pot is pathological but must not MathOverflow.
        let s = split_pot(MAINNET_SUPPLY_RAW).expect("split at mainnet supply");
        assert_eq!(s.total(), MAINNET_SUPPLY_RAW);
    }

    #[test]
    fn split_never_lets_a_leg_exceed_the_pot() {
        for pot in [
            1u64,
            1_000,
            1_000_000_000,
            u64::MAX / BPS_DENOMINATOR,
            MAINNET_SUPPLY_RAW,
        ] {
            let s = split_pot(pot).unwrap();
            for leg in [s.jackpot, s.incoming, s.admin, s.dev, s.burn, s.bounty, s.treasury] {
                assert!(leg <= pot);
            }
        }
    }

    // ── whale cap ────────────────────────────────────────────────────────────

    #[test]
    fn cap_is_three_percent_of_supply() {
        assert_eq!(whale_cap(1_000_000).unwrap(), 30_000);
        // The lab's supply: 1,000,000 KOTS at 9 decimals -> 30,000 KOTS.
        let supply = 1_000_000u64 * 1_000_000_000;
        assert_eq!(whale_cap(supply).unwrap(), 30_000 * 1_000_000_000);
    }

    #[test]
    fn cap_at_mainnet_supply_no_overflow() {
        // Regression: this is the exact failure mode of the first mainnet transfer.
        let cap = whale_cap(MAINNET_SUPPLY_RAW).expect("whale_cap at mainnet supply");
        assert_eq!(cap, MAINNET_SUPPLY_RAW / 10_000 * 300);
        // 3% of 1.111111111e9 whole tokens ≈ 33_333_333.33 whole tokens
        assert_eq!(cap / 1_000_000_000, 33_333_333);
    }

    #[test]
    fn cap_tightens_as_supply_burns() {
        // The property the published copy claims: every burn tightens the cap.
        let before = whale_cap(1_000_000_000_000_000).unwrap();
        let after = whale_cap(1_000_000_000_000_000 - 300_000_000).unwrap();
        assert!(after < before, "a burn must tighten the cap");
    }

    #[test]
    fn cap_boundary_is_inclusive() {
        let cap = whale_cap(1_000_000).unwrap();
        assert!(!breaches_cap(cap - 1, cap), "below the cap is fine");
        assert!(!breaches_cap(cap, cap), "exactly at the cap is ALLOWED");
        assert!(breaches_cap(cap + 1, cap), "one unit over must breach");
    }

    #[test]
    fn cap_of_zero_supply_is_zero() {
        assert_eq!(whale_cap(0), Some(0));
        // With no supply, any positive balance breaches — vacuous but must not panic.
        assert!(breaches_cap(1, 0));
    }

    // ── min_amount_to_exceed_cap ─────────────────────────────────────────────

    #[test]
    fn over_cap_amount_actually_clears_the_cap_after_the_fee() {
        // The regression guard for bug (3): a fixed margin over the cap is NOT
        // enough once the fee haircut is applied.
        for cap in [60u64, 30_000, 29_999, 43_949, 1_000_000] {
            for fee_bps in [0u64, 100, 300] {
                let amount = min_amount_to_exceed_cap(cap, fee_bps).unwrap();
                let received = net_received(amount, fee_bps).unwrap();
                assert!(
                    received > cap,
                    "cap {cap} @ {fee_bps}bps: amount {amount} delivered {received}, did not clear"
                );
            }
        }
    }

    #[test]
    fn a_naive_fixed_margin_fails_at_a_large_cap() {
        // Documents WHY the formula exists. At a 30,000 cap and 3% fee, the old
        // `cap + 10` sizing lands the recipient well under the cap.
        let cap = 30_000u64;
        let naive = cap + 10;
        let received = net_received(naive, 300).unwrap();
        assert!(received < cap, "the naive margin was the bug: {received} < {cap}");
    }

    #[test]
    fn over_cap_amount_is_minimal() {
        // One unit less must NOT clear the cap, or we are over-sizing.
        for cap in [100u64, 30_000, 1_000_000] {
            let amount = min_amount_to_exceed_cap(cap, 300).unwrap();
            let just_under = net_received(amount - 1, 300).unwrap();
            assert!(just_under <= cap, "amount was not minimal for cap {cap}");
        }
    }

    #[test]
    fn over_cap_is_unsolvable_at_a_total_fee() {
        assert_eq!(min_amount_to_exceed_cap(1_000, BPS_DENOMINATOR), None);
    }

    #[test]
    fn min_amount_at_mainnet_cap_no_overflow() {
        let cap = whale_cap(MAINNET_SUPPLY_RAW).unwrap();
        let amount = min_amount_to_exceed_cap(cap, 300).expect("must work at mainnet cap");
        let received = net_received(amount, 300).unwrap();
        assert!(received > cap);
    }

    // ── classification ───────────────────────────────────────────────────────

    #[test]
    fn classification_covers_every_combination() {
        assert_eq!(FeeClass::classify(false, false), FeeClass::Buy);
        assert_eq!(FeeClass::classify(false, true), FeeClass::Sell);
        // Exemption wins: a whitelisted party selling into a pool is still exempt.
        assert_eq!(FeeClass::classify(true, false), FeeClass::Exempt);
        assert_eq!(FeeClass::classify(true, true), FeeClass::Exempt);
    }

    #[test]
    fn class_net_rates_match_the_published_model() {
        let (buy, sell) = (100u64, 300u64);
        assert_eq!(FeeClass::Exempt.net_bps(buy, sell), 0);
        assert_eq!(FeeClass::Buy.net_bps(buy, sell), 100);
        assert_eq!(FeeClass::Sell.net_bps(buy, sell), 300);
    }

    #[test]
    fn every_class_nets_what_the_model_promises() {
        // End-to-end on the arithmetic: withhold the sell rate, rebate per class,
        // and confirm the payer's net cost equals the advertised rate.
        let amount = 1_000_000u64;
        let (withheld_bps, buy_bps, sell_bps) = (300u64, 100u64, 300u64);
        for class in [FeeClass::Exempt, FeeClass::Buy, FeeClass::Sell] {
            let net_bps = class.net_bps(buy_bps, sell_bps);
            let withheld = withheld_fee(amount, withheld_bps).unwrap();
            let rebate = rebate_owed(amount, withheld_bps, net_bps).unwrap();
            let net_cost = withheld - rebate;
            assert_eq!(
                net_cost,
                withheld_fee(amount, net_bps).unwrap(),
                "{class:?} did not net its advertised rate"
            );
        }
    }
}
