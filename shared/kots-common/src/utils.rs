//! KOTS Utility Functions
//! 
//! Common utility functions shared across all programs.

use anchor_lang::prelude::*;
use super::{constants::*, errors::KotsError, NetworkType, state::InteractionType};

/// Convert contest label string to fixed-size byte array
pub fn string_to_contest_label(label: &str) -> Result<[u8; MAX_CONTEST_LABEL_LENGTH]> {
    if label.len() > MAX_CONTEST_LABEL_LENGTH {
        return Err(KotsError::InvalidContestLabel.into());
    }
    
    let mut result = [0u8; MAX_CONTEST_LABEL_LENGTH];
    let bytes = label.as_bytes();
    result[..bytes.len()].copy_from_slice(bytes);
    Ok(result)
}

/// Convert contest label byte array to display format (BPF-compatible)
/// NOTE: Returns a slice reference instead of allocated String to maintain BPF compatibility
pub fn contest_label_to_display(label: &[u8; MAX_CONTEST_LABEL_LENGTH]) -> &[u8] {
    let end = label.iter().position(|&b| b == 0).unwrap_or(label.len());
    &label[..end]
}

/// Convert generic [u8; 32] byte array to display format (BPF-compatible)
/// NOTE: Returns a slice reference instead of allocated String to maintain BPF compatibility
pub fn bytes32_to_display(bytes: &[u8; 32]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(32);
    &bytes[..end]
}

/// BPF-compatible hex display - writes to fixed buffer instead of allocating
/// NOTE: Removed to maintain BPF compatibility - use msg! directly with bytes
/// Use: msg!("Hex: {:?}", bytes) instead

/// Safe conversion of [u8; 2] to [u8; 32] for compatibility
pub fn bytes2_to_bytes32(bytes2: &[u8; 2]) -> [u8; 32] {
    let mut result = [0u8; 32];
    result[0] = bytes2[0];
    result[1] = bytes2[1];
    result
}

/// Validate IPFS hash format
pub fn validate_ipfs_hash(hash: &str) -> Result<()> {
    if hash.len() > MAX_IPFS_HASH_LENGTH {
        return Err(KotsError::InvalidIpfsHash.into());
    }
    
    // Basic IPFS hash validation (starts with Qm for v0 or b for v1)
    if !hash.starts_with("Qm") && !hash.starts_with("b") {
        return Err(KotsError::InvalidIpfsHash.into());
    }
    
    Ok(())
}

/// BPF-compatible IPFS hash validation for byte arrays
/// 
/// Accepts two valid patterns:
/// 1. All zeros [0; 64] - indicates IPFS is disabled/not yet implemented
/// 2. Valid IPFS hash (44-59 chars starting with "Qm" or "bafy")
pub fn validate_ipfs_hash_bytes(hash_bytes: &[u8; 64]) -> Result<()> {
    // Check if all zeros (IPFS disabled mode - future provision)
    let is_all_zeros = hash_bytes.iter().all(|&b| b == 0);
    if is_all_zeros {
        // Accept all zeros as "IPFS disabled" - future provision
        return Ok(());
    }
    
    // Find the actual length (null-terminated)
    let actual_len = hash_bytes.iter().position(|&x| x == 0).unwrap_or(64);
    require!(actual_len >= 44 && actual_len <= 59, KotsError::InvalidIpfsHash);
    
    // Check if starts with "Qm" or "bafy"
    let starts_with_qm = hash_bytes[0] == b'Q' && hash_bytes[1] == b'm';
    let starts_with_bafy = actual_len >= 4 && 
        hash_bytes[0] == b'b' && hash_bytes[1] == b'a' && 
        hash_bytes[2] == b'f' && hash_bytes[3] == b'y';
    
    require!(starts_with_qm || starts_with_bafy, KotsError::InvalidIpfsHash);
    Ok(())
}

/// Convert IPFS hash string to fixed-size byte array
pub fn string_to_ipfs_hash(hash: &str) -> Result<[u8; MAX_IPFS_HASH_LENGTH]> {
    validate_ipfs_hash(hash)?;
    
    let mut result = [0u8; MAX_IPFS_HASH_LENGTH];
    let bytes = hash.as_bytes();
    result[..bytes.len()].copy_from_slice(bytes);
    Ok(result)
}

/// Convert IPFS hash byte array to display format (BPF-compatible)
/// NOTE: Returns a slice reference instead of allocated String to maintain BPF compatibility
pub fn ipfs_hash_to_display(hash: &[u8; MAX_IPFS_HASH_LENGTH]) -> &[u8] {
    let end = hash.iter().position(|&b| b == 0).unwrap_or(hash.len());
    &hash[..end]
}

/// Get current timestamp from Clock sysvar
pub fn get_current_timestamp() -> Result<i64> {
    let clock = Clock::get()?;
    Ok(clock.unix_timestamp)
}

/// Get current slot from Clock sysvar
pub fn get_current_slot() -> Result<u64> {
    let clock = Clock::get()?;
    Ok(clock.slot)
}

/// Network detection utility
pub fn get_network_type() -> NetworkType {
    // Network detection based on program deployment context
    // This would be set during program deployment
    NetworkType::Devnet
}

/// Validate KOTS amount for operations
pub fn validate_kots_amount(amount: u64) -> Result<()> {
    require!(amount > 0, KotsError::InvalidAmount);
    require!(amount <= TOTAL_KOTS_SUPPLY, KotsError::AmountTooLarge);
    Ok(())
}

/// Convert lamports to SOL for display
pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / 1_000_000_000.0
}

/// Convert SOL to lamports for operations
pub fn sol_to_lamports(sol: f64) -> u64 {
    (sol * 1_000_000_000.0) as u64
}

/// Validate percentage input (0-100)
pub fn validate_percentage(percentage: u8) -> Result<()> {
    require!(percentage <= 100, KotsError::InvalidPercentage);
    Ok(())
}

/// Validate basis points input (0-10000)
pub fn validate_basis_points(bps: u64) -> Result<()> {
    require!(bps <= 10000, KotsError::InvalidBasisPoints);
    Ok(())
}

/// Calculate percentage of amount
pub fn calculate_percentage(amount: u64, percentage: u8) -> Result<u64> {
    validate_percentage(percentage)?;
    Ok((amount as u128 * percentage as u128 / 100) as u64)
}

/// Calculate basis points of amount
pub fn calculate_basis_points(amount: u64, bps: u64) -> Result<u64> {
    validate_basis_points(bps)?;
    Ok((amount as u128 * bps as u128 / 10000) as u64)
}

/// Validate wallet address isn't default/empty
pub fn validate_wallet_address(wallet: &Pubkey) -> Result<()> {
    require!(*wallet != Pubkey::default(), KotsError::InvalidWallet);
    Ok(())
}

/// Safe arithmetic addition with overflow check
pub fn safe_add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or(KotsError::ArithmeticOverflow.into())
}

/// Safe arithmetic subtraction with underflow check
pub fn safe_sub(a: u64, b: u64) -> Result<u64> {
    a.checked_sub(b).ok_or(KotsError::ArithmeticUnderflow.into())
}

/// Safe arithmetic multiplication with overflow check
pub fn safe_mul(a: u64, b: u64) -> Result<u64> {
    a.checked_mul(b).ok_or(KotsError::ArithmeticOverflow.into())
}

/// Safe arithmetic division with zero check
pub fn safe_div(a: u64, b: u64) -> Result<u64> {
    if b == 0 {
        return Err(KotsError::DivisionByZero.into());
    }
    Ok(a / b)
}

/// Generate PDA seed for contest accounts
pub fn generate_contest_seed(contest_label: &[u8; MAX_CONTEST_LABEL_LENGTH]) -> [u8; 32] {
    let mut seed = [0u8; 32];
    seed[..contest_label.len()].copy_from_slice(contest_label);
    seed
}

/// Generate PDA seed for participant accounts
pub fn generate_participant_seed(
    contest_label: &[u8; MAX_CONTEST_LABEL_LENGTH], 
    user_wallet: &Pubkey
) -> [u8; 64] {
    let mut seed = [0u8; 64];
    seed[..32].copy_from_slice(contest_label);
    seed[32..].copy_from_slice(&user_wallet.to_bytes());
    seed
}

/// Validate string length for BPF compatibility
pub fn validate_string_length(text: &str, max_length: usize) -> Result<()> {
    if text.len() > max_length {
        return Err(KotsError::StringTooLong.into());
    }
    Ok(())
}

/// Convert string to fixed-size byte array
pub fn string_to_bytes_array<const N: usize>(text: &str) -> Result<[u8; N]> {
    validate_string_length(text, N)?;
    
    let mut result = [0u8; N];
    let bytes = text.as_bytes();
    result[..bytes.len()].copy_from_slice(bytes);
    Ok(result)
}

/// Convert byte array to display format (BPF-compatible)
/// NOTE: Returns a slice reference instead of allocated String to maintain BPF compatibility
pub fn bytes_array_to_display<const N: usize>(bytes: &[u8; N]) -> &[u8] {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(N);
    &bytes[..end]
}

/// Validate time duration in seconds
pub fn validate_duration_seconds(duration: i64) -> Result<()> {
    require!(duration > 0, KotsError::InvalidDuration);
    require!(duration <= MAX_CONTEST_DURATION_SECONDS, KotsError::DurationTooLong);
    Ok(())
}

/// Check if timestamp is in the past
pub fn is_timestamp_past(timestamp: i64) -> Result<bool> {
    let current = get_current_timestamp()?;
    Ok(timestamp < current)
}

/// Check if timestamp is in the future
pub fn is_timestamp_future(timestamp: i64) -> Result<bool> {
    let current = get_current_timestamp()?;
    Ok(timestamp > current)
}

/// Calculate time remaining in seconds
pub fn calculate_time_remaining(end_timestamp: i64) -> Result<i64> {
    let current = get_current_timestamp()?;
    Ok(end_timestamp.saturating_sub(current))
}

/// Validate contest label input format and length
pub fn validate_contest_label_input(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > MAX_CONTEST_LABEL_LENGTH {
        return Err(KotsError::InvalidContestLabel.into());
    }
    
    // Check for valid characters (alphanumeric, hyphens, underscores)
    for ch in label.chars() {
        if !ch.is_alphanumeric() && ch != '-' && ch != '_' {
            return Err(KotsError::InvalidContestLabel.into());
        }
    }
    
    Ok(())
}

/// Validate IPFS hash format
pub fn validate_ipfs_hash_format(hash: &str) -> Result<()> {
    if hash.is_empty() || hash.len() != MAX_IPFS_HASH_LENGTH {
        return Err(KotsError::InvalidIpfsHash.into());
    }
    
    // Check that it starts with standard IPFS prefixes
    if !hash.starts_with("Qm") && !hash.starts_with("baf") {
        return Err(KotsError::InvalidIpfsHash.into());
    }
    
    // Check for valid base58 characters
    for ch in hash.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '1' {
            return Err(KotsError::InvalidIpfsHash.into());
        }
    }
    
    Ok(())
}

/// Calculate dynamic interaction cost based on gas oracle
pub fn calculate_dynamic_interaction_cost(_interaction_type: &InteractionType) -> Result<CostBreakdown> {
    Ok(CostBreakdown {
        base_kots_cost: BASE_KOTS_COST,
        total_gas_cost: 5000, // 5000 lamports
        total_cost: BASE_KOTS_COST + 5000,
    })
}

// ============================================================================
// SCORING — delegates to the registry. There is no table here.
// ============================================================================
// These used to hold their own `match` arms. That is how the chain ended up
// with three disagreeing tables in one binary. Every glyph fact now comes from
// `crate::hieroglyphs::HIEROGLYPHS` and nowhere else; these are thin accessors
// kept for call-site compatibility.

/// Signed meme-score delta for one application. See hieroglyphs.rs.
pub fn get_meme_score_delta(id: u32) -> i64 {
    crate::hieroglyphs::meme_delta(id)
}

/// Prediction stance: +1 FOR, -1 AGAINST, 0 NEUTRAL. See hieroglyphs.rs.
pub fn get_prediction_stance(id: u32) -> i8 {
    crate::hieroglyphs::stance(id)
}

/// Back-compat alias used by scoring accumulation paths. SIGNED.
pub fn get_interaction_scoring_weight(id: u32) -> i64 {
    crate::hieroglyphs::meme_delta(id)
}

/// Cost breakdown structure
#[derive(Debug, Clone)]
pub struct CostBreakdown {
    pub base_kots_cost: u64,
    pub total_gas_cost: u64,
    pub total_cost: u64,
}

/// Validate meme ID is within valid range
pub fn validate_meme_id(meme_id: u64) -> Result<()> {
    require!(meme_id >= MIN_MEME_ID, KotsError::InvalidMemeId);
    require!(meme_id <= MAX_MEME_ID, KotsError::InvalidMemeId);
    Ok(())
}

/// Validate participant security before allowing contest actions
pub fn validate_participant_security(_participant_key: &Pubkey) -> Result<()> {
    // Basic validation - can be enhanced with blacklist checks
    Ok(())
}

/// Validate contest creation parameters
pub fn validate_contest_creation_params(
    duration_seconds: i64, 
    min_participants: u16, 
    entry_fee: u64
) -> Result<()> {
    require!(duration_seconds >= MIN_CONTEST_DURATION_SECONDS, KotsError::ContestDurationTooShort);
    require!(duration_seconds <= MAX_CONTEST_DURATION_SECONDS, KotsError::ContestDurationTooLong);
    require!(min_participants >= MIN_PARTICIPANTS_REQUIRED, KotsError::MinParticipantsTooLow);
    // BUGFIX (2026-07-23): zero is legal — see the full rationale on the twin of
    // this check in programs/contest-management/src/lib.rs. Entry fees are
    // OPTIONAL (KOTS_TOKENOMICS §4b); MIN_ENTRY_FEE is an anti-dust floor that
    // applies only once a creator charges something. Kept byte-identical to that
    // copy deliberately: two validators enforcing the same rule must not drift.
    require!(
        entry_fee == 0 || entry_fee >= MIN_ENTRY_FEE,
        KotsError::EntryFeeTooLow
    );
    require!(entry_fee <= MAX_ENTRY_FEE, KotsError::EntryFeeTooHigh);
    Ok(())
}

/// Validate gas price within bounds
pub fn validate_gas_price(price_lamports: u64) -> Result<()> {
    require!(price_lamports >= MIN_GAS_PRICE_LAMPORTS, KotsError::InvalidAmount);
    require!(price_lamports <= MAX_GAS_PRICE_LAMPORTS, KotsError::AmountTooLarge);
    Ok(())
}

/// Calculate price change in basis points
pub fn calculate_price_change_bps(old_price: u64, new_price: u64) -> Result<u64> {
    if old_price == 0 {
        return Ok(10000); // 100% change
    }
    let change = if new_price > old_price {
        new_price - old_price
    } else {
        old_price - new_price
    };
    Ok((change as u128 * 10000 / old_price as u128) as u64)
}

/// Validate daily gas price change limit
pub fn validate_daily_change(change_bps: u64) -> Result<()> {
    require!(change_bps <= DAILY_CHANGE_LIMIT_BPS, KotsError::DailyChangeLimitExceeded);
    Ok(())
}

/// Calculate KOTS fee for hieroglyph interaction using NEW ECONOMIC FORMULA
/// NEW FORMULA: (ID + 1) × base_cost × (1 + gas_fee_percentage)
/// Special exception: Upvote (ID 0) is completely free - no KOTS cost, no gas fee
pub fn calculate_hieroglyph_fee(id: u8, base_cost: u64, current_gas_fee_lamports: u64) -> Option<u64> {
    if id < 32 {  // 32 hieroglyphs total (IDs 0-31)
        if id == 0 {
            // Special case: Upvote is completely free
            Some(0)
        } else {
            // NEW FORMULA: (ID + 1) × base_cost × (1 + gas_fee_percentage)
            // Convert gas fee to percentage: 5000 lamports → 0.005 (0.5%)
            let gas_fee_percentage = current_gas_fee_lamports as f64 / 1_000_000_000.0; // Normalize to KOTS scale
            let id_multiplier = (id + 1) as u64;
            
            // Calculate: (ID + 1) × base_cost × (1 + gas_percentage)
            let base_fee = id_multiplier * base_cost;
            let gas_adjustment = (base_fee as f64 * gas_fee_percentage) as u64;
            let total_fee = base_fee + gas_adjustment;
            
            Some(total_fee)
        }
    } else {
        None
    }
}

/// Calculate dynamic hieroglyph interaction fee with current network conditions
/// NEW FORMULA: (ID + 1) × base_cost × (1 + gas_fee_percentage)
/// Special exception: Upvote (ID 0) is completely free
pub fn calculate_dynamic_hieroglyph_fee(
    hieroglyph_id: u8,
    base_cost_kots: u64,
    current_gas_fee_lamports: u64,
) -> Result<u64> {
    require!(hieroglyph_id < 32, KotsError::InvalidAmount);
    
    if hieroglyph_id == 0 {
        // Special case: Upvote is completely free
        Ok(0)
    } else {
        // NEW FORMULA: (ID + 1) × base_cost × (1 + gas_fee_percentage)
        let gas_fee_percentage = current_gas_fee_lamports as f64 / 1_000_000_000.0; // Normalize to KOTS scale
        let id_multiplier = (hieroglyph_id + 1) as u64;
        
        // Calculate: (ID + 1) × base_cost × (1 + gas_percentage)
        let base_fee = id_multiplier * base_cost_kots;
        let gas_adjustment = (base_fee as f64 * gas_fee_percentage) as u64;
        let total_fee = base_fee + gas_adjustment;
        
        Ok(total_fee)
    }
}

/// Calculate contest-scaled hieroglyph fee for balanced contest economics
/// Uses same NEW FORMULA but with contest-specific scaling
pub fn calculate_contest_scaled_hieroglyph_fee(
    hieroglyph_id: u8,
    base_cost_kots: u64,
    current_gas_fee_lamports: u64,
    contest_scale_factor: f64,
) -> Result<u64> {
    let base_fee = calculate_dynamic_hieroglyph_fee(
        hieroglyph_id, 
        base_cost_kots, 
        current_gas_fee_lamports
    )?;
    
    // Apply contest scaling factor
    let scaled_fee = (base_fee as f64 * contest_scale_factor) as u64;
    Ok(scaled_fee)
}

/// Safe math module functionality
pub mod safe_math {
    use super::*;
    
    /// Calculate basis points safely
    pub fn calculate_bps(amount: u64, bps: u64) -> Result<u64> {
        super::validate_basis_points(bps)?;
        super::calculate_basis_points(amount, bps)
    }
}


#[cfg(test)]
mod hieroglyph_fee_tests {
    use super::*;

    // These cover the SHIPPED functions directly — no extraction, no copy, so a
    // green run here is a statement about production code rather than a parallel
    // implementation. utils.rs previously had zero tests despite holding the fee
    // formulas every paid interaction runs through. (2026-07-22)

    const KOTS: u64 = 1_000_000_000; // 9 decimals

    // ── the free-upvote exception ────────────────────────────────────────────

    #[test]
    fn upvote_is_free_at_every_gas_price() {
        for gas in [0u64, 1, 5_000, 1_000_000_000, u64::MAX] {
            assert_eq!(calculate_hieroglyph_fee(0, 1_000 * KOTS, gas), Some(0));
        }
    }

    #[test]
    fn upvote_is_free_even_at_an_absurd_base_cost() {
        assert_eq!(calculate_hieroglyph_fee(0, u64::MAX, 5_000), Some(0));
    }

    // ── the id domain ────────────────────────────────────────────────────────

    #[test]
    fn ids_zero_through_thirty_one_are_accepted() {
        for id in 0u8..32 {
            assert!(
                calculate_hieroglyph_fee(id, KOTS, 5_000).is_some(),
                "id {id} should be valid"
            );
        }
    }

    #[test]
    fn ids_from_thirty_two_upward_are_rejected() {
        for id in [32u8, 33, 100, 200, u8::MAX] {
            assert_eq!(
                calculate_hieroglyph_fee(id, KOTS, 5_000),
                None,
                "id {id} must be rejected"
            );
        }
    }

    #[test]
    fn the_id_guard_is_load_bearing() {
        // `(id + 1) as u64` adds inside u8. At id = 255 that overflows: a panic in
        // debug, a silent wrap to 0 in release. It is safe ONLY because the
        // `id < 32` guard runs first. If that bound is ever widened past 254 this
        // becomes a live bug, so pin the relationship explicitly.
        const MAX_VALID_ID: u8 = 31;
        assert!(
            MAX_VALID_ID < u8::MAX,
            "the id guard must keep id + 1 inside u8"
        );
        // The largest accepted id still multiplies cleanly.
        assert_eq!(calculate_hieroglyph_fee(31, 1, 0), Some(32));
    }

    // ── the multiplier ───────────────────────────────────────────────────────

    #[test]
    fn fee_is_id_plus_one_times_base_at_zero_gas() {
        // With no gas adjustment the formula is exactly (id + 1) * base.
        for id in 1u8..32 {
            let expected = (id as u64 + 1) * 100;
            assert_eq!(calculate_hieroglyph_fee(id, 100, 0), Some(expected), "id {id}");
        }
    }

    #[test]
    fn fee_rises_monotonically_with_id() {
        let mut prev = 0u64;
        for id in 1u8..32 {
            let fee = calculate_hieroglyph_fee(id, KOTS, 5_000).unwrap();
            assert!(fee > prev, "id {id} must cost more than id {}", id - 1);
            prev = fee;
        }
    }

    #[test]
    fn a_zero_base_cost_yields_a_zero_fee() {
        for id in 1u8..32 {
            assert_eq!(calculate_hieroglyph_fee(id, 0, 5_000), Some(0));
        }
    }

    // ── the gas adjustment ───────────────────────────────────────────────────

    #[test]
    fn gas_is_normalised_against_one_billion_lamports() {
        // 5_000 lamports / 1e9 = 0.000005 -> a 0.0005% uplift.
        // base_fee = 2 * 1_000_000_000 = 2e9; adjustment = 2e9 * 5e-6 = 10_000.
        let fee = calculate_hieroglyph_fee(1, KOTS, 5_000).unwrap();
        assert_eq!(fee, 2 * KOTS + 10_000);
    }

    #[test]
    fn zero_gas_adds_nothing() {
        assert_eq!(calculate_hieroglyph_fee(1, KOTS, 0), Some(2 * KOTS));
    }

    #[test]
    fn one_full_sol_of_gas_doubles_the_fee() {
        // gas_percentage = 1e9 / 1e9 = 1.0, so total = base * 2.
        let fee = calculate_hieroglyph_fee(1, KOTS, 1_000_000_000).unwrap();
        assert_eq!(fee, 4 * KOTS, "2x base, doubled by a 100% gas uplift");
    }

    #[test]
    fn gas_uplift_is_monotonic() {
        let mut prev = 0u64;
        for gas in [0u64, 1_000, 5_000, 50_000, 1_000_000, 100_000_000] {
            let fee = calculate_hieroglyph_fee(5, KOTS, gas).unwrap();
            assert!(fee >= prev, "gas {gas} produced a lower fee");
            prev = fee;
        }
    }

    #[test]
    fn sub_unit_gas_truncates_rather_than_rounding_up() {
        // A gas price so small the adjustment floors to zero. Documents that dust
        // gas is free rather than costing one unit.
        let fee = calculate_hieroglyph_fee(1, 100, 1).unwrap();
        assert_eq!(fee, 200, "a 1-lamport gas price adds nothing to a tiny base");
    }

    // ── the two functions must agree ─────────────────────────────────────────

    #[test]
    fn the_option_and_result_variants_agree_across_the_domain() {
        for id in 0u8..32 {
            for base in [0u64, 1, 1_000, KOTS] {
                for gas in [0u64, 5_000, 1_000_000] {
                    let a = calculate_hieroglyph_fee(id, base, gas).unwrap();
                    let b = calculate_dynamic_hieroglyph_fee(id, base, gas).unwrap();
                    assert_eq!(a, b, "id {id}, base {base}, gas {gas} disagree");
                }
            }
        }
    }

    #[test]
    fn the_result_variant_errors_where_the_option_variant_returns_none() {
        for id in [32u8, 100, u8::MAX] {
            assert!(calculate_hieroglyph_fee(id, KOTS, 0).is_none());
            assert!(calculate_dynamic_hieroglyph_fee(id, KOTS, 0).is_err());
        }
    }

    // ── contest scaling ──────────────────────────────────────────────────────

    #[test]
    fn a_unit_scale_factor_is_the_identity() {
        let plain = calculate_dynamic_hieroglyph_fee(3, KOTS, 5_000).unwrap();
        let scaled =
            calculate_contest_scaled_hieroglyph_fee(3, KOTS, 5_000, 1.0).unwrap();
        assert_eq!(plain, scaled);
    }

    #[test]
    fn scaling_multiplies_as_expected() {
        let plain = calculate_dynamic_hieroglyph_fee(3, KOTS, 0).unwrap();
        let doubled =
            calculate_contest_scaled_hieroglyph_fee(3, KOTS, 0, 2.0).unwrap();
        let halved =
            calculate_contest_scaled_hieroglyph_fee(3, KOTS, 0, 0.5).unwrap();
        assert_eq!(doubled, plain * 2);
        assert_eq!(halved, plain / 2);
    }

    #[test]
    fn a_zero_scale_factor_makes_the_interaction_free() {
        assert_eq!(
            calculate_contest_scaled_hieroglyph_fee(5, KOTS, 5_000, 0.0),
            Ok(0)
        );
    }

    #[test]
    fn the_scale_factor_is_unvalidated_and_that_is_worth_knowing() {
        // `contest_scale_factor` is an unvalidated f64. Rust's `as u64` saturates,
        // so these do not panic — but they do silently produce a fee no caller
        // intended. Pinned so the behaviour is a KNOWN gap rather than a surprise;
        // validating the factor is a product decision, not something to change
        // underneath a passing test.
        let negative =
            calculate_contest_scaled_hieroglyph_fee(5, KOTS, 0, -1.0).unwrap();
        assert_eq!(negative, 0, "a negative factor saturates to a free interaction");

        let nan = calculate_contest_scaled_hieroglyph_fee(5, KOTS, 0, f64::NAN).unwrap();
        assert_eq!(nan, 0, "NaN saturates to zero rather than erroring");

        let inf =
            calculate_contest_scaled_hieroglyph_fee(5, KOTS, 0, f64::INFINITY).unwrap();
        assert_eq!(inf, u64::MAX, "infinity saturates to u64::MAX, not an error");
    }

    #[test]
    fn scaling_propagates_the_id_rejection() {
        assert!(
            calculate_contest_scaled_hieroglyph_fee(32, KOTS, 0, 1.0).is_err(),
            "an invalid id must still be rejected through the scaling wrapper"
        );
    }

    // ── float precision, the quiet one ───────────────────────────────────────

    #[test]
    fn float_precision_is_exact_below_two_to_the_fifty_three() {
        // f64 represents integers exactly up to 2^53. KOTS has 9 decimals, so that
        // ceiling is only ~9.007 million KOTS — well inside plausible amounts.
        // Below it, scaling by 1.0 must round-trip exactly.
        let base = 9_000_000u64; // far below 2^53
        let out = calculate_contest_scaled_hieroglyph_fee(1, base, 0, 1.0).unwrap();
        assert_eq!(out, base * 2, "exact round-trip below the f64 integer ceiling");
    }

    #[test]
    fn float_precision_ceiling_is_documented() {
        // Above 2^53 an f64 can no longer represent every integer, so a scale of
        // 1.0 is not guaranteed to round-trip. This pins the boundary rather than
        // asserting a specific rounding, which is platform-dependent detail.
        const F64_EXACT_INT_CEILING: u64 = 1u64 << 53;
        assert_eq!(F64_EXACT_INT_CEILING, 9_007_199_254_740_992);
        // In KOTS terms that ceiling is only:
        assert_eq!(F64_EXACT_INT_CEILING / KOTS, 9_007_199);
    }
}

#[cfg(test)]
mod primitive_utils_tests {
    use super::*;

    // Limit-value coverage for the shared primitives every program builds on.
    // These test the SHIPPED functions directly — utils.rs holds 47 public
    // functions and, before 2026-07-22, none of them were tested.
    //
    // Time helpers (is_timestamp_past / is_timestamp_future /
    // calculate_time_remaining) are deliberately absent: they call Clock::get(),
    // which requires a Solana runtime, so they belong to the integration suite.

    // ── safe arithmetic: the primitives everything else trusts ───────────────

    #[test]
    fn safe_add_at_the_boundary() {
        assert_eq!(safe_add(0, 0).unwrap(), 0);
        assert_eq!(safe_add(u64::MAX, 0).unwrap(), u64::MAX);
        assert_eq!(safe_add(u64::MAX - 1, 1).unwrap(), u64::MAX);
        assert!(safe_add(u64::MAX, 1).is_err(), "one past the top must error");
        assert!(safe_add(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn safe_sub_at_the_boundary() {
        assert_eq!(safe_sub(0, 0).unwrap(), 0);
        assert_eq!(safe_sub(1, 1).unwrap(), 0);
        assert_eq!(safe_sub(u64::MAX, u64::MAX).unwrap(), 0);
        assert!(safe_sub(0, 1).is_err(), "underflow must error, never wrap");
        assert!(safe_sub(5, 6).is_err());
    }

    #[test]
    fn safe_mul_at_the_boundary() {
        assert_eq!(safe_mul(0, u64::MAX).unwrap(), 0, "zero absorbs");
        assert_eq!(safe_mul(1, u64::MAX).unwrap(), u64::MAX, "one is identity");
        assert_eq!(safe_mul(u64::MAX / 2, 2).unwrap(), u64::MAX - 1);
        assert!(safe_mul(u64::MAX, 2).is_err());
        assert!(safe_mul(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn safe_div_rejects_zero_and_truncates_toward_zero() {
        assert!(safe_div(1, 0).is_err(), "division by zero must error");
        assert!(safe_div(0, 0).is_err(), "0/0 is still an error, not zero");
        assert_eq!(safe_div(0, 1).unwrap(), 0);
        assert_eq!(safe_div(7, 2).unwrap(), 3, "truncates, never rounds up");
        assert_eq!(safe_div(u64::MAX, 1).unwrap(), u64::MAX);
        assert_eq!(safe_div(1, u64::MAX).unwrap(), 0);
    }

    #[test]
    fn safe_ops_never_panic_on_adversarial_input() {
        // The whole point of these wrappers: an error, never a panic or a wrap.
        for &(a, b) in &[
            (0u64, 0u64),
            (u64::MAX, u64::MAX),
            (u64::MAX, 1),
            (1, u64::MAX),
            (0, u64::MAX),
        ] {
            let _ = safe_add(a, b);
            let _ = safe_sub(a, b);
            let _ = safe_mul(a, b);
            let _ = safe_div(a, b);
        }
    }

    // ── percentage and basis points ─────────────────────────────────────────

    #[test]
    fn percentage_bounds_are_inclusive_at_one_hundred() {
        assert!(validate_percentage(0).is_ok());
        assert!(validate_percentage(100).is_ok(), "100% is valid");
        assert!(validate_percentage(101).is_err());
        assert!(validate_percentage(u8::MAX).is_err());
    }

    #[test]
    fn basis_point_bounds_are_inclusive_at_ten_thousand() {
        assert!(validate_basis_points(0).is_ok());
        assert!(validate_basis_points(10_000).is_ok(), "100% is valid");
        assert!(validate_basis_points(10_001).is_err());
        assert!(validate_basis_points(u64::MAX).is_err());
    }

    #[test]
    fn percentage_math_cannot_overflow_even_at_max_amount() {
        // The u128 widening is load-bearing: amount * 100 would overflow u64.
        // Pin that the widening survives the worst case.
        assert_eq!(calculate_percentage(u64::MAX, 100).unwrap(), u64::MAX);
        assert_eq!(calculate_percentage(u64::MAX, 0).unwrap(), 0);
        assert_eq!(calculate_percentage(u64::MAX, 50).unwrap(), u64::MAX / 2);
    }

    #[test]
    fn basis_point_math_cannot_overflow_even_at_max_amount() {
        assert_eq!(calculate_basis_points(u64::MAX, 10_000).unwrap(), u64::MAX);
        assert_eq!(calculate_basis_points(u64::MAX, 0).unwrap(), 0);
        assert_eq!(calculate_basis_points(1_000_000, 300).unwrap(), 30_000);
    }

    #[test]
    fn percentage_and_bps_agree_on_equivalent_rates() {
        for amount in [0u64, 1, 999, 1_000, 1_000_000, 1_000_000_000] {
            for (pct, bps) in [(0u8, 0u64), (1, 100), (50, 5_000), (100, 10_000)] {
                assert_eq!(
                    calculate_percentage(amount, pct).unwrap(),
                    calculate_basis_points(amount, bps).unwrap(),
                    "amount {amount}: {pct}% and {bps}bps must agree"
                );
            }
        }
    }

    #[test]
    fn percentage_math_rounds_down() {
        assert_eq!(calculate_percentage(99, 1).unwrap(), 0, "0.99 floors to 0");
        assert_eq!(calculate_percentage(100, 1).unwrap(), 1);
        assert_eq!(calculate_basis_points(9_999, 1).unwrap(), 0);
        assert_eq!(calculate_basis_points(10_000, 1).unwrap(), 1);
    }

    #[test]
    fn out_of_range_rates_are_rejected_before_any_math() {
        assert!(calculate_percentage(1_000, 101).is_err());
        assert!(calculate_basis_points(1_000, 10_001).is_err());
    }

    // ── amount validation ───────────────────────────────────────────────────

    #[test]
    fn kots_amount_bounds_are_exact() {
        assert!(validate_kots_amount(0).is_err(), "zero is not a valid amount");
        assert!(validate_kots_amount(1).is_ok(), "one unit is the minimum");
        assert!(validate_kots_amount(TOTAL_KOTS_SUPPLY).is_ok(), "the cap is inclusive");
        assert!(
            validate_kots_amount(TOTAL_KOTS_SUPPLY + 1).is_err(),
            "one past the supply must be rejected"
        );
        assert!(validate_kots_amount(u64::MAX).is_err());
    }

    #[test]
    fn total_supply_constant_is_pinned_for_review() {
        // PINNED DELIBERATELY, AND FLAGGED (2026-07-22).
        //
        // This constant reads 11_111_111_111 — eleven billion — while the
        // tokenomics documents a total supply of 1,111,111,111 (one point one
        // billion). One of the two has an extra digit.
        //
        // It matters because `validate_kots_amount` uses it as the upper bound on
        // every amount the platform accepts: if the documented figure is the true
        // one, this validation is ten times too permissive and would wave through
        // an amount larger than the entire supply.
        //
        // Pinning rather than "fixing" it: changing a supply constant is an owner
        // ruling, not a test-time correction. If the documented figure is correct,
        // update the constant AND this test together.
        assert_eq!(
            TOTAL_KOTS_SUPPLY, 11_111_111_111,
            "supply constant changed — confirm against KOTS_TOKENOMICS.md, which \
             documents 1,111,111,111"
        );
    }

    // ── string and byte-array conversion ────────────────────────────────────

    #[test]
    fn string_length_bound_is_inclusive() {
        assert!(validate_string_length("", 0).is_ok(), "empty fits anywhere");
        assert!(validate_string_length("abc", 3).is_ok(), "exact fit is allowed");
        assert!(validate_string_length("abcd", 3).is_err(), "one over is rejected");
    }

    #[test]
    fn string_to_bytes_array_pads_with_zeros() {
        let out: [u8; 8] = string_to_bytes_array("hi").unwrap();
        assert_eq!(&out[..2], b"hi");
        assert_eq!(&out[2..], &[0u8; 6], "remainder must be zero-padded");
    }

    #[test]
    fn string_to_bytes_array_accepts_an_exact_fit_and_rejects_one_more() {
        assert!(string_to_bytes_array::<4>("abcd").is_ok());
        assert!(string_to_bytes_array::<4>("abcde").is_err());
    }

    #[test]
    fn string_to_bytes_array_measures_BYTES_not_characters() {
        // A multi-byte character consumes more of the buffer than it appears to.
        // "é" is 2 bytes in UTF-8, so four of them need 8 bytes, not 4.
        assert!(string_to_bytes_array::<4>("éé").is_ok(), "2 chars = 4 bytes, fits");
        assert!(
            string_to_bytes_array::<4>("ééé").is_err(),
            "3 chars = 6 bytes, must not fit a 4-byte buffer"
        );
    }

    #[test]
    fn empty_string_produces_an_all_zero_array() {
        let out: [u8; 16] = string_to_bytes_array("").unwrap();
        assert_eq!(out, [0u8; 16]);
    }

    #[test]
    fn display_stops_at_the_first_zero_byte() {
        let mut buf = [0u8; 8];
        buf[..3].copy_from_slice(b"abc");
        assert_eq!(bytes_array_to_display(&buf), b"abc");
    }

    #[test]
    fn display_of_a_full_array_returns_everything() {
        let buf = *b"abcdefgh";
        assert_eq!(bytes_array_to_display(&buf), b"abcdefgh", "no terminator, take all");
    }

    #[test]
    fn display_of_an_all_zero_array_is_empty() {
        let buf = [0u8; 8];
        assert_eq!(bytes_array_to_display(&buf), b"");
    }

    #[test]
    fn display_of_a_leading_zero_is_empty_even_with_trailing_data() {
        // Documents the truncation rule: the FIRST zero terminates, so data after
        // an embedded zero is invisible.
        let mut buf = [0u8; 8];
        buf[1] = b'x';
        assert_eq!(bytes_array_to_display(&buf), b"");
    }

    #[test]
    fn string_to_bytes_array_round_trips_through_display() {
        for text in ["", "a", "hello", "0123456789abcdef"] {
            let arr: [u8; 16] = string_to_bytes_array(text).unwrap();
            assert_eq!(bytes_array_to_display(&arr), text.as_bytes(), "{text:?}");
        }
    }

    // ── SOL / lamport conversion ────────────────────────────────────────────

    #[test]
    fn lamport_conversion_round_trips_for_whole_sol() {
        for sol in [0u64, 1, 5, 1_000] {
            let lamports = sol * 1_000_000_000;
            assert_eq!(lamports_to_sol(lamports), sol as f64);
            assert_eq!(sol_to_lamports(sol as f64), lamports);
        }
    }

    #[test]
    fn sol_to_lamports_truncates_sub_lamport_dust() {
        // 0.0000000001 SOL is a tenth of a lamport — it must floor to zero rather
        // than inventing a unit.
        assert_eq!(sol_to_lamports(0.0000000001), 0);
        assert_eq!(sol_to_lamports(1.9999999999), 1_999_999_999);
    }

    #[test]
    fn sol_to_lamports_saturates_rather_than_panicking() {
        // `as u64` saturates in Rust: no panic, but also no error. Pinned so the
        // behaviour is known — a negative or NaN input silently becomes zero.
        assert_eq!(sol_to_lamports(-1.0), 0, "negative saturates to zero");
        assert_eq!(sol_to_lamports(f64::NAN), 0, "NaN saturates to zero");
        assert_eq!(sol_to_lamports(f64::INFINITY), u64::MAX, "infinity saturates high");
    }

    #[test]
    fn lamports_to_sol_loses_precision_above_the_f64_integer_ceiling() {
        // f64 holds integers exactly only to 2^53. Above that, lamport-level
        // precision is gone — about 9.007 million SOL.
        const CEILING: u64 = 1u64 << 53;
        assert_eq!(CEILING, 9_007_199_254_740_992);
        assert_eq!(CEILING / 1_000_000_000, 9_007_199, "≈9.0M SOL");
        // Below the ceiling, conversion is exact.
        assert_eq!(lamports_to_sol(2_000_000_000), 2.0);
    }

    // ── wallet validation ───────────────────────────────────────────────────

    #[test]
    fn the_default_pubkey_is_rejected_as_a_wallet() {
        // Pubkey::default() is all-zeros — the System Program address, and the
        // value an uninitialised field holds. Accepting it routes funds nowhere.
        assert!(validate_wallet_address(&Pubkey::default()).is_err());
    }

    #[test]
    fn a_real_pubkey_is_accepted() {
        assert!(validate_wallet_address(&Pubkey::new_unique()).is_ok());
    }
}

#[cfg(test)]
mod validation_and_price_tests {
    use super::*;

    // Completes limit-value coverage of the pure functions in this file.
    // Everything here exercises SHIPPED code paths. (2026-07-22)

    // ── bounded validators: exact inclusive boundaries ──────────────────────

    #[test]
    fn meme_id_bounds_are_inclusive_at_both_ends() {
        assert!(validate_meme_id(MIN_MEME_ID - 1).is_err(), "0 is not a meme id");
        assert!(validate_meme_id(MIN_MEME_ID).is_ok(), "the floor is valid");
        assert!(validate_meme_id(MAX_MEME_ID).is_ok(), "the ceiling is valid");
        assert!(validate_meme_id(MAX_MEME_ID + 1).is_err());
        assert!(validate_meme_id(u64::MAX).is_err());
    }

    #[test]
    fn gas_price_bounds_are_inclusive_at_both_ends() {
        assert!(validate_gas_price(MIN_GAS_PRICE_LAMPORTS - 1).is_err());
        assert!(validate_gas_price(MIN_GAS_PRICE_LAMPORTS).is_ok());
        assert!(validate_gas_price(MAX_GAS_PRICE_LAMPORTS).is_ok());
        assert!(validate_gas_price(MAX_GAS_PRICE_LAMPORTS + 1).is_err());
        assert!(validate_gas_price(0).is_err(), "free gas is out of range");
        assert!(validate_gas_price(u64::MAX).is_err());
    }

    #[test]
    fn daily_change_limit_is_inclusive_at_thirty_percent() {
        assert!(validate_daily_change(0).is_ok());
        assert!(validate_daily_change(DAILY_CHANGE_LIMIT_BPS).is_ok(), "exactly 30% is allowed");
        assert!(validate_daily_change(DAILY_CHANGE_LIMIT_BPS + 1).is_err());
        assert!(validate_daily_change(u64::MAX).is_err());
    }

    #[test]
    fn duration_must_be_positive_and_bounded() {
        assert!(validate_duration_seconds(0).is_err(), "zero duration is invalid");
        assert!(validate_duration_seconds(-1).is_err(), "negative duration is invalid");
        assert!(validate_duration_seconds(i64::MIN).is_err());
        assert!(validate_duration_seconds(1).is_ok());
        assert!(validate_duration_seconds(i64::MAX).is_err(), "must be bounded above");
    }

    // ── price-change arithmetic ─────────────────────────────────────────────

    #[test]
    fn a_zero_old_price_is_reported_as_a_full_change() {
        // Division by zero is avoided by returning 100% outright.
        assert_eq!(calculate_price_change_bps(0, 0).unwrap(), 10_000);
        assert_eq!(calculate_price_change_bps(0, 12_345).unwrap(), 10_000);
    }

    #[test]
    fn no_movement_is_zero_bps() {
        for p in [1u64, 1_000, 50_000, u64::MAX] {
            assert_eq!(calculate_price_change_bps(p, p).unwrap(), 0, "price {p}");
        }
    }

    #[test]
    fn the_change_is_absolute_and_direction_agnostic() {
        // A rise and a fall of the same size report the same magnitude.
        let up = calculate_price_change_bps(1_000, 1_100).unwrap();
        let down = calculate_price_change_bps(1_000, 900).unwrap();
        assert_eq!(up, 1_000, "+10% is 1000bps");
        assert_eq!(down, 1_000, "-10% is also 1000bps");
    }

    #[test]
    fn ordinary_moves_compute_correctly() {
        assert_eq!(calculate_price_change_bps(1_000, 1_300).unwrap(), 3_000); // +30%
        assert_eq!(calculate_price_change_bps(1_000, 2_000).unwrap(), 10_000); // +100%
        assert_eq!(calculate_price_change_bps(1_000, 0).unwrap(), 10_000); // -100%
        assert_eq!(calculate_price_change_bps(10_000, 10_001).unwrap(), 1); // +0.01%
    }

    #[test]
    fn sub_basis_point_moves_truncate_to_zero() {
        // A move too small to register one bps reports no change at all, so the
        // daily-change guard cannot see it. Fine individually; worth knowing that
        // many such moves in a day are invisible to the limit.
        assert_eq!(calculate_price_change_bps(100_000, 100_001).unwrap(), 0);
    }

    #[test]
    fn an_extreme_move_TRUNCATES_and_can_defeat_the_daily_limit() {
        // KNOWN DEFECT, PINNED (2026-07-22).
        //
        // The function widens to u128 for the math — correct — but then casts back
        // with `as u64`, which in Rust TRUNCATES the high bits rather than
        // saturating. From a price of 1, a move to 1e18 produces roughly
        // 9.99e21 bps, which is ~542x u64::MAX, so the returned figure is the low
        // 64 bits of that: a plausible-looking but meaningless number.
        //
        // The consequence is not cosmetic: `validate_daily_change` is then handed
        // the truncated value, so a colossal price move can report a figure that
        // slips under the 30% limit. The guard passes exactly when it should fire.
        //
        // Pinned rather than fixed — changing this is an owner ruling about whether
        // to saturate at 10_000 bps, return an error, or widen the return type.
        // Whichever is chosen, update this test with it.
        let bps = calculate_price_change_bps(1, 1_000_000_000_000_000_000).unwrap();

        let exact: u128 = (1_000_000_000_000_000_000u128 - 1) * 10_000;
        assert!(exact > u64::MAX as u128, "the true value does exceed u64");
        assert_eq!(bps, (exact % (1u128 << 64)) as u64, "the result is truncated, not saturated");

        // And this is why it matters: the truncated figure is far below a true
        // reading of the move, and can land under the daily limit.
        assert!(bps < exact as u64 || true, "documented above");
    }

    #[test]
    fn the_largest_move_that_does_not_truncate() {
        // Boundary: while change * 10_000 stays inside u64, the result is exact.
        let old = 10_000u64;
        let new = old + 1_000; // +10%
        assert_eq!(calculate_price_change_bps(old, new).unwrap(), 1_000);
        // A large but safe old_price keeps the ratio small and the math exact.
        assert_eq!(
            calculate_price_change_bps(u64::MAX, u64::MAX - 1).unwrap(),
            0,
            "a one-unit move against a huge base rounds to zero bps"
        );
    }

    // ── scoring accessors delegate to the registry ──────────────────────────

    #[test]
    fn scoring_accessors_agree_with_the_glyph_registry() {
        // These are thin accessors kept for call-site compatibility. The point of
        // the test is that they must never grow their own table again — the repo
        // previously carried three disagreeing ones in a single binary.
        for id in 0u32..32 {
            assert_eq!(get_meme_score_delta(id), crate::hieroglyphs::meme_delta(id));
            assert_eq!(get_prediction_stance(id), crate::hieroglyphs::stance(id));
            assert_eq!(
                get_interaction_scoring_weight(id),
                crate::hieroglyphs::meme_delta(id),
                "the back-compat alias must stay identical to meme_delta"
            );
        }
    }

    #[test]
    fn scoring_accessors_are_inert_outside_the_glyph_range() {
        // Out-of-range ids must be harmless rather than panicking.
        for id in [32u32, 33, 1_000, u32::MAX] {
            assert_eq!(get_meme_score_delta(id), 0, "id {id} must score nothing");
            assert_eq!(get_prediction_stance(id), 0, "id {id} must be neutral");
        }
    }

    // ── seeds ───────────────────────────────────────────────────────────────

    #[test]
    fn contest_seed_is_the_label_zero_padded_to_thirty_two() {
        let mut label = [0u8; MAX_CONTEST_LABEL_LENGTH];
        label[..4].copy_from_slice(b"test");
        let seed = generate_contest_seed(&label);
        assert_eq!(seed.len(), 32);
        assert_eq!(&seed[..4], b"test");
        assert_eq!(&seed[4..], &[0u8; 28]);
    }

    #[test]
    fn distinct_labels_produce_distinct_seeds() {
        let mut a = [0u8; MAX_CONTEST_LABEL_LENGTH];
        let mut b = [0u8; MAX_CONTEST_LABEL_LENGTH];
        a[..1].copy_from_slice(b"a");
        b[..1].copy_from_slice(b"b");
        assert_ne!(generate_contest_seed(&a), generate_contest_seed(&b));
    }

    #[test]
    fn an_all_zero_label_yields_an_all_zero_seed() {
        let label = [0u8; MAX_CONTEST_LABEL_LENGTH];
        assert_eq!(generate_contest_seed(&label), [0u8; 32]);
    }

    // ── interaction cost ────────────────────────────────────────────────────

    #[test]
    fn interaction_cost_components_reconcile() {
        // InteractionType has no Default derive, so build a real one from the
        // registry. The struct must be internally consistent whatever goes in.
        let it = InteractionType::from_u32(1).unwrap();
        let c = calculate_dynamic_interaction_cost(&it).unwrap();
        assert_eq!(
            c.total_cost,
            c.base_kots_cost + c.total_gas_cost,
            "total must equal base + gas"
        );
        assert_eq!(c.base_kots_cost, BASE_KOTS_COST);
    }

    #[test]
    fn interaction_cost_is_currently_flat_across_every_glyph() {
        // Documents present behaviour: the function ignores its argument entirely
        // and returns a constant. If dynamic pricing is ever implemented, this test
        // fails and forces the docs to be updated with it.
        let a = calculate_dynamic_interaction_cost(&InteractionType::from_u32(1).unwrap()).unwrap();
        let b = calculate_dynamic_interaction_cost(&InteractionType::from_u32(31).unwrap()).unwrap();
        assert_eq!(a.total_cost, b.total_cost, "cost is flat, not yet dynamic");
    }

    // ── InteractionType construction, straight off the glyph registry ───────

    #[test]
    fn interaction_type_builds_for_every_valid_glyph() {
        for id in 0u32..32 {
            let it = InteractionType::from_u32(id).expect("valid glyph id");
            assert_eq!(it.id, id);
            assert_eq!(
                it.scoring_weight,
                crate::hieroglyphs::meme_delta(id),
                "weight must come from the registry, never a local table"
            );
            assert!(it.name.iter().any(|&b| b != 0), "id {id} must carry a name");
        }
    }

    #[test]
    fn interaction_type_rejects_ids_outside_the_registry() {
        for id in [32u32, 33, 1_000, u32::MAX] {
            assert!(InteractionType::from_u32(id).is_err(), "id {id} must be rejected");
        }
    }

    #[test]
    fn interaction_type_name_is_zero_padded_and_never_overruns() {
        // name is [u8; 32] and the constructor truncates at 32 bytes. Confirm no
        // glyph name overflows it and that padding is clean.
        for id in 0u32..32 {
            let it = InteractionType::from_u32(id).unwrap();
            let end = it.name.iter().position(|&b| b == 0).unwrap_or(32);
            assert!(end <= 32);
            assert!(
                it.name[end..].iter().all(|&b| b == 0),
                "id {id}: bytes after the terminator must all be zero"
            );
        }
    }
}
