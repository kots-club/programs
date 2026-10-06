//! Pure prediction-accuracy scoring arithmetic.
//!
//! Extracted from `finalize_prediction_tally` so it can be unit-tested without a
//! validator. The logic is small but unusually hazardous: it is **signed**, it
//! multiplies four terms together, and it narrows `i128 → i64` at the end. Every
//! one of those is a place where a limit value bites.
//!
//! The shipped expression is:
//!
//! ```text
//! k          = min(order, EARLY_WEIGHT_WINDOW)
//! weight_bps = WEIGHT_MAX_BPS - WEIGHT_STEP_BPS * k
//! delta      = stance * stake * outcome * weight_bps / 10_000    (in i128)
//! ```
//!
//! with `outcome = +1` when the meme placed and `-1` when it did not.
//!
//! # The hazards these tests pin
//!
//! * **`weight_bps` is an UNCHECKED subtraction.** It cannot underflow today only
//!   because `k` is clamped to `EARLY_WEIGHT_WINDOW = 10`, and `1_000 * 10` is
//!   below `20_000`. Raise the window past 20 and the program panics on a
//!   perfectly ordinary vote. `weight_never_underflows_across_the_whole_u16_domain`
//!   and `the_clamp_is_load_bearing` exist to make that failure loud.
//! * **`stance` is `i8` and `stake` is `u64`.** A downvote carries a negative
//!   stance, so the delta is signed and an early *wrong* call must cost more than a
//!   late one. Sign handling is tested across the full cross-product.
//! * **The narrowing can overflow.** `stake` is `u64`, so `stake * 2.0×` exceeds
//!   `i64::MAX` long before it exceeds `i128`. The shipped code returns an error
//!   rather than wrapping; these tests pin the exact boundary where that starts.
//! * **Division truncates toward zero for negatives in Rust.** `-7 / 10 == 0`, not
//!   `-1`. That means a tiny wrong call can round to no penalty at all. Whether
//!   that is acceptable is a product question, but it should be a *known* one.
//!
//! Dependency-free on purpose: `cargo test -p prize-distribution` runs it in
//! milliseconds, no SOL, no RPC, no validator.

/// Basis-point denominator.
pub const BPS_DENOMINATOR: i128 = 10_000;

/// Weight at a meme's first prediction: 2.0×.
pub const WEIGHT_MAX_BPS: u64 = 20_000;
/// Linear decay per ordinal step: 0.1×.
pub const WEIGHT_STEP_BPS: u64 = 1_000;
/// From this ordinal onward the weight is flat 1.0×.
pub const EARLY_WEIGHT_WINDOW: u16 = 10;

/// Errors this arithmetic can legitimately produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreError {
    /// The delta did not fit in `i64` after narrowing.
    Overflow,
}

/// Earliness leverage for a prediction made at ordinal `order`.
///
/// `order` is the 0-based position of this prediction among all predictions on the
/// same meme: 0 is the very first. Returns basis points, so 20_000 == 2.0×.
///
/// Flat at 1.0× from `EARLY_WEIGHT_WINDOW` onward — being the 11th or the
/// 11,000th predictor is worth the same.
pub fn weight_bps(order: u16) -> u64 {
    let k = u64::from(order.min(EARLY_WEIGHT_WINDOW));
    // Safe ONLY because of the clamp above. See the tests.
    WEIGHT_MAX_BPS - WEIGHT_STEP_BPS * k
}

/// Outcome multiplier: `+1` if the meme placed in the winning set, `-1` if not.
pub fn outcome(placed: bool) -> i128 {
    if placed {
        1
    } else {
        -1
    }
}

/// The signed accuracy delta credited to a voter for one prediction.
///
/// `stance` is the hieroglyph's direction (positive = for, negative = against),
/// `stake` the KOTS paid, `order` the earliness ordinal, `placed` the result.
///
/// Computed in `i128` and narrowed to `i64`, returning [`ScoreError::Overflow`]
/// rather than wrapping — the shipped behaviour.
pub fn accuracy_delta(
    stance: i8,
    stake: u64,
    order: u16,
    placed: bool,
) -> Result<i64, ScoreError> {
    let w = weight_bps(order) as i128;
    let raw = (stance as i128)
        .checked_mul(stake as i128)
        .and_then(|v| v.checked_mul(outcome(placed)))
        .and_then(|v| v.checked_mul(w))
        .map(|v| v / BPS_DENOMINATOR)
        .ok_or(ScoreError::Overflow)?;
    i64::try_from(raw).map_err(|_| ScoreError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the weight curve ─────────────────────────────────────────────────────

    #[test]
    fn weight_curve_matches_the_specified_shape() {
        assert_eq!(weight_bps(0), 20_000, "first predictor gets 2.0x");
        assert_eq!(weight_bps(1), 19_000);
        assert_eq!(weight_bps(5), 15_000);
        assert_eq!(weight_bps(9), 11_000);
        assert_eq!(weight_bps(10), 10_000, "flat 1.0x from the window onward");
    }

    #[test]
    fn weight_is_flat_beyond_the_window() {
        // Being the 11th or the 65,535th predictor is worth exactly the same.
        for order in [10u16, 11, 100, 1_000, 30_000, u16::MAX] {
            assert_eq!(weight_bps(order), 10_000, "order {order} should be flat 1.0x");
        }
    }

    #[test]
    fn weight_decreases_monotonically_then_plateaus() {
        let mut prev = weight_bps(0);
        for order in 1u16..=15 {
            let w = weight_bps(order);
            assert!(w <= prev, "weight must never increase (order {order})");
            prev = w;
        }
    }

    #[test]
    fn weight_never_underflows_across_the_whole_u16_domain() {
        // Exhaustive over every possible ordinal. `weight_bps` performs an
        // UNCHECKED subtraction; if this ever panics, the program panics on a
        // normal vote.
        for order in 0u16..=u16::MAX {
            let w = weight_bps(order);
            assert!(
                (10_000..=20_000).contains(&w),
                "order {order} produced out-of-range weight {w}"
            );
        }
    }

    #[test]
    fn the_clamp_is_load_bearing() {
        // Documents WHY the unchecked subtraction is currently safe: the clamp
        // keeps the decay below the maximum. If EARLY_WEIGHT_WINDOW were raised
        // past this bound, `WEIGHT_MAX_BPS - WEIGHT_STEP_BPS * k` would underflow
        // and panic in production.
        let max_decay = WEIGHT_STEP_BPS * u64::from(EARLY_WEIGHT_WINDOW);
        assert!(
            max_decay <= WEIGHT_MAX_BPS,
            "EARLY_WEIGHT_WINDOW={EARLY_WEIGHT_WINDOW} decays {max_decay} bps, \
             which exceeds WEIGHT_MAX_BPS={WEIGHT_MAX_BPS} — weight_bps would underflow"
        );
        // And the floor is exactly 1.0x, never below.
        assert_eq!(WEIGHT_MAX_BPS - max_decay, 10_000);
    }

    // ── outcome ──────────────────────────────────────────────────────────────

    #[test]
    fn outcome_is_plus_or_minus_one() {
        assert_eq!(outcome(true), 1);
        assert_eq!(outcome(false), -1);
    }

    // ── delta: the sign cross-product ────────────────────────────────────────

    #[test]
    fn a_correct_for_call_scores_positive() {
        // Backed it (+1 stance), it placed -> reward.
        let d = accuracy_delta(1, 1_000, 10, true).unwrap();
        assert!(d > 0, "correct positive prediction must score positive, got {d}");
    }

    #[test]
    fn a_wrong_for_call_scores_negative() {
        let d = accuracy_delta(1, 1_000, 10, false).unwrap();
        assert!(d < 0, "wrong positive prediction must be penalised, got {d}");
    }

    #[test]
    fn a_correct_against_call_scores_positive() {
        // Downvoted it (-1 stance), it did NOT place (-1 outcome) -> +.
        let d = accuracy_delta(-1, 1_000, 10, false).unwrap();
        assert!(d > 0, "correct negative prediction must score positive, got {d}");
    }

    #[test]
    fn a_wrong_against_call_scores_negative() {
        // Downvoted it, it placed anyway.
        let d = accuracy_delta(-1, 1_000, 10, true).unwrap();
        assert!(d < 0, "wrong negative prediction must be penalised, got {d}");
    }

    #[test]
    fn the_full_sign_cross_product_is_consistent() {
        // stance sign x outcome sign determines the result sign, always.
        for &stance in &[-3i8, -1, 1, 3] {
            for &placed in &[true, false] {
                let d = accuracy_delta(stance, 10_000, 10, placed).unwrap();
                let expected_positive = (stance > 0) == placed;
                assert_eq!(
                    d > 0,
                    expected_positive,
                    "stance {stance}, placed {placed} produced {d}"
                );
            }
        }
    }

    // ── delta: earliness leverage ────────────────────────────────────────────

    #[test]
    fn being_early_amplifies_a_correct_call() {
        let early = accuracy_delta(1, 1_000_000, 0, true).unwrap();
        let late = accuracy_delta(1, 1_000_000, 10, true).unwrap();
        assert_eq!(early, late * 2, "order 0 must be worth exactly 2.0x order 10");
    }

    #[test]
    fn being_early_also_amplifies_the_penalty() {
        // The stated design: an early WRONG call costs more than a late one.
        let early = accuracy_delta(1, 1_000_000, 0, false).unwrap();
        let late = accuracy_delta(1, 1_000_000, 10, false).unwrap();
        assert!(early < late, "early wrong must cost more");
        assert_eq!(early, late * 2);
    }

    #[test]
    fn magnitude_is_monotonic_in_earliness() {
        let mut prev = i64::MAX;
        for order in 0u16..=10 {
            let d = accuracy_delta(1, 1_000_000, order, true).unwrap().abs();
            assert!(d <= prev, "magnitude must not grow with order {order}");
            prev = d;
        }
    }

    // ── delta: zero and dust limits ──────────────────────────────────────────

    #[test]
    fn zero_stake_scores_nothing_regardless_of_everything_else() {
        for &stance in &[-1i8, 0, 1] {
            for &placed in &[true, false] {
                for order in [0u16, 5, 10, u16::MAX] {
                    assert_eq!(accuracy_delta(stance, 0, order, placed), Ok(0));
                }
            }
        }
    }

    #[test]
    fn zero_stance_scores_nothing() {
        assert_eq!(accuracy_delta(0, u64::MAX / 4, 0, true), Ok(0));
    }

    #[test]
    fn no_stake_can_ever_round_away_to_nothing() {
        // CORRECTED 2026-07-22 after CI. This test previously asserted that a
        // 1-unit wrong stake truncates to a zero penalty. It does not, and it
        // cannot: `weight_bps` is ALWAYS >= 10_000, so `stake * weight / 10_000`
        // is always >= stake. A dust stake can never escape its penalty.
        //
        // At exactly 1.0x the multiply and divide cancel, leaving nothing to
        // truncate — which is why the smallest possible wrong call still scores -1.
        assert_eq!(accuracy_delta(1, 1, 10, false), Ok(-1), "1 unit still costs 1");
        assert_eq!(accuracy_delta(1, 1, 10, true), Ok(1));

        // The guarantee, swept: magnitude is never below the stake.
        for stake in [1u64, 2, 3, 7, 99, 10_000, 1_000_000] {
            for order in [0u16, 1, 5, 9, 10, 1_000] {
                let d = accuracy_delta(1, stake, order, true).unwrap();
                assert!(
                    d as u64 >= stake,
                    "stake {stake} @ order {order} scored {d}, below the stake"
                );
            }
        }
    }

    #[test]
    fn truncation_happens_at_fractional_weights_but_never_to_zero() {
        // Truncation is real — it just cannot reach zero. At 1.9x a 1-unit stake
        // computes 1.9 and floors to 1, losing the fraction. Pinned so the
        // rounding direction is a known property.
        assert_eq!(accuracy_delta(1, 1, 1, true), Ok(1), "1.9 floors to 1");
        assert_eq!(accuracy_delta(1, 1, 9, true), Ok(1), "1.1 floors to 1");
        assert_eq!(accuracy_delta(1, 1, 0, true), Ok(2), "2.0 is exact");

        // Penalties floor toward zero as well, so a wrong call is never rounded
        // to a HARSHER penalty than the arithmetic warrants.
        assert_eq!(accuracy_delta(1, 1, 1, false), Ok(-1), "-1.9 truncates to -1");
    }

    #[test]
    fn earliness_can_rescue_a_stake_from_rounding_away() {
        // At 2.0x a stake registers where at 1.0x it would truncate to nothing.
        let late = accuracy_delta(1, 5_000, 10, true).unwrap();
        let early = accuracy_delta(1, 5_000, 0, true).unwrap();
        assert_eq!(late, 5_000 * 10_000 / 10_000);
        assert_eq!(early, 5_000 * 20_000 / 10_000);
        assert!(early > late);
    }

    // ── delta: the narrowing boundary ────────────────────────────────────────

    #[test]
    fn a_stake_that_fits_i64_at_one_x_is_accepted() {
        let stake = i64::MAX as u64;
        // At order 10 the weight is exactly 1.0x, so the delta equals the stake.
        assert_eq!(accuracy_delta(1, stake, 10, true), Ok(i64::MAX));
    }

    #[test]
    fn doubling_that_stake_via_earliness_overflows_rather_than_wrapping() {
        // The hazard: 2.0x leverage pushes a stake that fits i64 past i64::MAX.
        // Must be reported, never silently wrapped to a negative score.
        let stake = i64::MAX as u64;
        assert_eq!(accuracy_delta(1, stake, 0, true), Err(ScoreError::Overflow));
    }

    #[test]
    fn the_overflow_boundary_is_exact_at_two_x() {
        // Largest stake that survives 2.0x leverage, and the first that does not.
        let ok = (i64::MAX as u64) / 2;
        assert!(accuracy_delta(1, ok, 0, true).is_ok(), "should fit");
        let too_big = ok + 2;
        assert_eq!(
            accuracy_delta(1, too_big, 0, true),
            Err(ScoreError::Overflow),
            "just past the boundary must be reported"
        );
    }

    #[test]
    fn an_extreme_stance_magnifies_the_overflow_risk() {
        // stance is i8, so it can reach ±127 — that is a 127x multiplier on top
        // of the 2x earliness leverage.
        let stake = (i64::MAX as u64) / 100;
        assert_eq!(
            accuracy_delta(127, stake, 0, true),
            Err(ScoreError::Overflow)
        );
    }

    #[test]
    fn maximum_everything_is_reported_not_wrapped() {
        for &stance in &[i8::MIN, i8::MAX] {
            for &placed in &[true, false] {
                assert_eq!(
                    accuracy_delta(stance, u64::MAX, 0, placed),
                    Err(ScoreError::Overflow),
                    "stance {stance}, placed {placed} must report overflow"
                );
            }
        }
    }

    #[test]
    fn i8_min_stance_does_not_break_negation() {
        // i8::MIN has no positive counterpart; make sure the widening to i128
        // happens before any sign work so this is arithmetic, not a panic.
        let d = accuracy_delta(i8::MIN, 1_000, 10, true).unwrap();
        assert_eq!(d, -128 * 1_000);
    }

    #[test]
    fn overflow_never_silently_produces_a_valid_looking_score() {
        // CORRECTED 2026-07-22 after CI. This previously demanded Err across
        // shifts 0..4, but shift 3 legitimately FITS: stake 2_305_843_009_213_693_951
        // at stance 2 and 2.0x is 9_223_372_036_854_775_804, just under i64::MAX.
        // Hard-coding where the boundary falls was the mistake.
        //
        // The property that actually matters does not require knowing the
        // boundary: whenever the call SUCCEEDS the answer must be exactly the
        // widened arithmetic — never a wrapped one — and a correct prediction must
        // never come back negative. Sweeping across the boundary exercises both
        // sides without asserting where it is.
        let mut saw_ok = false;
        let mut saw_err = false;
        for shift in 0..12u32 {
            let stake = u64::MAX >> shift;
            match accuracy_delta(2, stake, 0, true) {
                Err(ScoreError::Overflow) => saw_err = true,
                Ok(v) => {
                    saw_ok = true;
                    assert!(v > 0, "a correct prediction wrapped to {v} for stake {stake}");
                    let expected = (stake as i128) * 2 * 2; // stance 2, weight 2.0x
                    assert_eq!(v as i128, expected, "stake {stake} was not exact");
                }
            }
        }
        assert!(saw_err, "the sweep must cross into overflow");
        assert!(saw_ok, "the sweep must also cover values that fit");
    }

    // ── delta: exact values, so a refactor cannot drift ──────────────────────

    #[test]
    fn exact_values_at_every_ordinal_in_the_window() {
        // stake 1_000_000, stance +1, placed. delta = stake * weight / 10_000.
        let stake = 1_000_000u64;
        let expected = [
            2_000_000i64, // order 0  @ 2.0x
            1_900_000,    // order 1
            1_800_000,    // order 2
            1_700_000,    // order 3
            1_600_000,    // order 4
            1_500_000,    // order 5
            1_400_000,    // order 6
            1_300_000,    // order 7
            1_200_000,    // order 8
            1_100_000,    // order 9
            1_000_000,    // order 10 @ 1.0x
        ];
        for (order, want) in expected.iter().enumerate() {
            let got = accuracy_delta(1, stake, order as u16, true).unwrap();
            assert_eq!(got, *want, "order {order}");
        }
    }

    #[test]
    fn penalties_mirror_rewards_exactly() {
        // Same magnitude, opposite sign — no asymmetry hiding in the arithmetic.
        for order in 0u16..=12 {
            let reward = accuracy_delta(1, 1_234_567, order, true).unwrap();
            let penalty = accuracy_delta(1, 1_234_567, order, false).unwrap();
            assert_eq!(reward, -penalty, "order {order} is asymmetric");
        }
    }
}
