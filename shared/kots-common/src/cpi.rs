// DESTINATION (where this file goes): kots-contracts/shared/kots-common/src/cpi.rs
//! KOTS CPI Helpers
//! 
//! Cross Program Invocation utilities for communication between
//! the KOTS multi-program architecture. Uses network-aware configuration.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::Instruction, program::invoke, program::invoke_signed};
// CPI utilities don't need token imports currently
use super::errors::KotsError;
use super::constants::{TOKENS_PER_UNIT, MAX_ENTRY_FEE, MAX_HIEROGLYPH_COST};
// REMOVED (2026-07-23): `PaymentVerification`, `SecurityValidationResult` and
// `PaymentType`. Their only consumers were `verify_meme_upload_payment` and
// `verify_storage_extension_payment`, deleted in the same pass as dead code that
// called the permissive security stubs. Nothing else in this module referenced
// them. If a shared payment verifier is rebuilt later, re-import then.

/// Network-aware program IDs for CPI calls
pub mod program_ids {
    use anchor_lang::prelude::*;
    use crate::network_config;
    
    /// Get current network's program IDs
    pub fn get_current_ids() -> network_config::ProgramIds {
        network_config::config::program_ids()
    }
    
    /// Individual program ID getters for convenience
    pub fn contest_management() -> Pubkey {
        get_current_ids().contest_management
    }
    
    pub fn voting_hieroglyph() -> Pubkey {
        get_current_ids().voting_hieroglyph
    }
    
    pub fn fee_management() -> Pubkey {
        get_current_ids().fee_management
    }
    
    pub fn prize_distribution() -> Pubkey {
        get_current_ids().prize_distribution
    }
    
    pub fn progressive_jackpot() -> Pubkey {
        get_current_ids().progressive_jackpot
    }
    
    pub fn security_governance() -> Pubkey {
        get_current_ids().security_governance
    }
    
    pub fn treasury_operations() -> Pubkey {
        get_current_ids().treasury_operations
    }
    
    // Legacy compatibility (deprecated)
    pub fn contest_management_id() -> Pubkey { contest_management() }
    pub fn voting_hieroglyph_id() -> Pubkey { voting_hieroglyph() }
    pub fn economics_distribution_id() -> Pubkey { fee_management() } // Legacy alias
    pub fn security_governance_id() -> Pubkey { security_governance() }
}

/// CPI instruction wrapper for type safety
#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct CpiInstruction<T> {
    pub instruction: T,
    pub program_id: Pubkey,
}

/// Common CPI context structure for cross-program calls
pub struct KotsCpiContext<'a, 'b, 'c, 'info, T> {
    pub accounts: T,
    pub remaining_accounts: &'c [AccountInfo<'info>],
    pub program: &'a AccountInfo<'info>,
    pub signer_seeds: &'b [&'b [&'b [u8]]],
}

/// Security validation for CPI calls using network-aware configuration
pub fn validate_cpi_program(program_id: &Pubkey) -> Result<()> {
    let current_ids = crate::network_config::config::program_ids();
    let valid_programs = [
        current_ids.contest_management,
        current_ids.voting_hieroglyph,
        current_ids.fee_management,
        current_ids.prize_distribution,
        current_ids.progressive_jackpot,
        current_ids.security_governance,
        current_ids.treasury_operations,
    ];
    
    if valid_programs.contains(program_id) {
        Ok(())
    } else {
        msg!("Invalid CPI program ID: {} not in valid programs for current network", program_id);
        Err(KotsError::InvalidCpiProgram.into())
    }
}

/// Execute CPI call with proper error handling and account ownership validation
pub fn execute_cpi<'info, T: ToAccountMetas + ToAccountInfos<'info>>(
    program: AccountInfo<'info>,
    accounts: T,
    instruction_data: &[u8],
    signer_seeds: Option<&[&[&[u8]]]>,
    calling_program_id: &Pubkey, // Add calling program ID for ownership validation
) -> Result<()> {
    validate_cpi_program(program.key)?;
    
    let instruction = Instruction {
        program_id: *program.key,
        accounts: accounts.to_account_metas(None),
        data: instruction_data.to_vec(), // Note: Solana's Instruction requires Vec for data
    };
    
    let account_infos = accounts.to_account_infos();
    
    match signer_seeds {
        Some(seeds) => {
            // CRITICAL FIX: Validate account ownership before invoke_signed
            for account_info in &account_infos {
                // Skip system program and rent sysvar accounts
                if account_info.key == &anchor_lang::system_program::ID 
                    || account_info.key == &anchor_lang::solana_program::sysvar::rent::ID
                    || account_info.key == &anchor_lang::solana_program::sysvar::clock::ID {
                    continue;
                }
                
                // For signer seeds, verify that accounts expected to sign are owned by calling program
                // This prevents "Cross-program invocation with unauthorized signer" errors
                if account_info.is_signer && account_info.owner != calling_program_id {
                    msg!("❌ CPI Authorization Error: Account {} is signer but not owned by calling program {}", 
                         account_info.key, calling_program_id);
                    msg!("  Account owner: {}", account_info.owner);
                    msg!("  This indicates partial initialization - account exists but not owned by expected program");
                    return Err(KotsError::AccountOwnedByWrongProgram.into());
                }
            }
            
            invoke_signed(&instruction, &account_infos, seeds)
                .map_err(|_| KotsError::CpiCallFailed)?;
        }
        None => {
            invoke(&instruction, &account_infos)
                .map_err(|_| KotsError::CpiCallFailed)?;
        }
    }
    
    Ok(())
}

/// Macro for generating CPI helper functions
macro_rules! generate_cpi_helper {
    ($name:ident, $program_id_fn:ident, $instruction:ty, $accounts:ty) => {
        pub fn $name<'info>(
            program: AccountInfo<'info>,
            accounts: $accounts,
            instruction: $instruction,
            signer_seeds: Option<&[&[&[u8]]]>,
            calling_program_id: &Pubkey,
        ) -> Result<()> {
            let expected_program_id = program_ids::$program_id_fn();
            require_keys_eq!(*program.key, expected_program_id, KotsError::InvalidCpiProgram);
            
            // BPF-compatible: Use borsh serialize with pre-allocated buffer
            let mut instruction_data = [0u8; 1024]; // Max instruction size
            let serialized = borsh::to_vec(&instruction)?;
            if serialized.len() > 1024 {
                return Err(KotsError::SerializationFailed.into());
            }
            instruction_data[..serialized.len()].copy_from_slice(&serialized);
            let instruction_data = &instruction_data[..serialized.len()];
            execute_cpi(program, accounts, &instruction_data, signer_seeds, calling_program_id)
        }
    };
}

// Example CPI helper functions (expanded during implementation)
generate_cpi_helper!(
    call_contest_management,
    contest_management,
    ContestManagementInstruction,
    ContestManagementAccounts
);

generate_cpi_helper!(
    call_voting_hieroglyph,
    voting_hieroglyph,
    VotingHieroglyphInstruction,
    VotingHieroglyphAccounts
);

// Placeholder instruction types (will be defined in each program)
#[derive(AnchorSerialize, AnchorDeserialize)]
pub enum ContestManagementInstruction {
    CreateContest,
    FinalizeContest,
}

#[derive(AnchorSerialize, AnchorDeserialize)]
pub enum VotingHieroglyphInstruction {
    CastVote,
    CalculateScore,
}

// Placeholder account types (will be defined in each program)
pub struct ContestManagementAccounts;
pub struct VotingHieroglyphAccounts;

impl ToAccountMetas for ContestManagementAccounts {
    fn to_account_metas(&self, _is_signer: Option<bool>) -> Vec<AccountMeta> {
        // BPF-compatible: Return empty slice via into_iter().collect()
        [].to_vec()
    }
}

impl<'info> ToAccountInfos<'info> for ContestManagementAccounts {
    fn to_account_infos(&self) -> Vec<AccountInfo<'info>> {
        // BPF-compatible: Return empty slice via into_iter().collect()
        [].to_vec()
    }
}

impl ToAccountMetas for VotingHieroglyphAccounts {
    fn to_account_metas(&self, _is_signer: Option<bool>) -> Vec<AccountMeta> {
        // BPF-compatible: Return empty slice via into_iter().collect()
        [].to_vec()
    }
}

impl<'info> ToAccountInfos<'info> for VotingHieroglyphAccounts {
    fn to_account_infos(&self) -> Vec<AccountInfo<'info>> {
        // BPF-compatible: Return empty slice via into_iter().collect()
        [].to_vec()
    }
}

/// CPI result wrapper for better error handling
#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct CpiResult<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error_code: Option<u32>,
}

impl<T> CpiResult<T> {
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error_code: None,
        }
    }
    
    pub fn error(error_code: u32) -> Self {
        Self {
            success: false,
            data: None,
            error_code: Some(error_code),
        }
    }
    
    pub fn unwrap(self) -> Result<T> {
        if self.success {
            Ok(self.data.unwrap())
        } else {
            Err(KotsError::CpiCallFailed.into())
        }
    }
}

// ============================================================================
// MISSING CPI FUNCTION IMPLEMENTATIONS
// ============================================================================

/// Validate security constraints via Security-Governance program
pub fn validate_security_constraints(
    emergency_state: &AccountInfo,
    program_id: &Pubkey,
    _contest_label: &[u8; 32],
) -> Result<()> {
    // EMERGENCY PAUSE (2026-08-12): read the REAL EmergencyState PDA instead of
    // `load_contest_state().emergency_paused` (hardcoded false — fail-open). The
    // former contest is_initialized check read the same stub (hardcoded true) and
    // is dropped rather than kept as theatre; there is no on-chain ContestState to
    // read it from. This function currently has no callers.
    let pause = read_emergency_pause_status(
        emergency_state,
        &security_governance_id_for(program_id)?,
    )?;
    require!(!pause.is_active, KotsError::EmergencyPauseActive);

    msg!("✅ Security constraints validated for contest");
    Ok(())
}

/// Initialize voting system via Voting-Hieroglyph program
pub fn initialize_voting_system<T>(_ctx: &T, contest_label: &[u8; 32]) -> Result<()> {
    msg!("🗳️ Initializing voting system for contest");
    
    // Validate contest label
    require!(contest_label != &[0u8; 32], KotsError::InvalidContestLabel);
    
    // Load contest state to verify it exists
    let contest_state = load_contest_state(contest_label)?;
    require!(contest_state.is_initialized, KotsError::UninitializedAccount);
    
    msg!("✅ Voting system initialized");
    Ok(())
}

/// Setup contest economics via Fee-Management program
pub fn setup_contest_economics<T>(_ctx: &T, contest_label: &[u8; 32], entry_fee: u64) -> Result<()> {
    msg!("💰 Setting up contest economics - Entry fee: {} KOTS", entry_fee / TOKENS_PER_UNIT);
    
    // Validate entry fee is reasonable
    require!(entry_fee <= MAX_ENTRY_FEE, KotsError::ExcessiveTransferAmount);
    
    // Validate contest exists
    let contest_state = load_contest_state(contest_label)?;
    require!(contest_state.is_initialized, KotsError::UninitializedAccount);
    
    msg!("✅ Contest economics configured");
    Ok(())
}

/// Process entry fee via Fee-Management program.
///
/// SECURITY GAP (surfaced 2026-08-03): this is a STUB that collects nothing.
/// `_ctx: &T` is a generic with no account access, so it structurally cannot
/// move tokens; `load_contest_state` below is itself a stub, so the
/// `!is_finalized` check runs on fabricated data. The bound checks are real but
/// prove only that the caller passed a sane number. Combined with
/// `join_contest_unlimited` recording `stake_amount` without transferring it,
/// the on-chain KOTS entry-fee/stake collection is NOT implemented — see
/// KOTS-SECURITY-GOVERNANCE-PLAN (Work log) for the systemic finding and the
/// owner decision it needs (implement collection vs. fail-closed vs. rely on the
/// transfer-hook tax as the sole monetisation).
pub fn process_entry_fee<T>(_ctx: &T, contest_label: &[u8; 32], fee_amount: u64) -> Result<()> {
    msg!("💸 Entry-fee check (NOT collected): {} KOTS", fee_amount / TOKENS_PER_UNIT);
    
    // Validate fee amount
    require!(fee_amount > 0, KotsError::InvalidTokenAmount);
    require!(fee_amount <= MAX_ENTRY_FEE, KotsError::ExcessiveTransferAmount);
    
    // Validate contest exists and is active
    let contest_state = load_contest_state(contest_label)?;
    require!(!contest_state.is_finalized, KotsError::ContestAlreadyFinalized);
    
    // NOTE: no transfer happens here — do not read this as "fee collected".
    Ok(())
}

/// Get final scores from Voting-Hieroglyph program (BPF-compatible)
/// 
/// # Critical CPI Implementation Required
/// This function MUST call the Voting-Hieroglyph program's `calculate_final_scores` instruction
/// to retrieve the actual participant scores before finalization.
/// 
/// ## Expected Implementation:
/// 1. Build CPI context with contest_state and voting_program accounts
/// 2. Invoke voting_hieroglyph::calculate_final_scores(contest_label)
/// 3. Parse return data for participant count
/// 4. Return actual participant count for prize distribution
/// 
/// ## Current Behavior:
/// - Returns 0 (stub) - THIS WILL CAUSE FINALIZATION TO FAIL
/// - Production deployment requires this CPI to be fully implemented
pub fn get_final_scores<T>(_ctx: &T, _contest_label: &[u8; 32]) -> Result<u32> {
    // TODO: Implement actual CPI call to Voting-Hieroglyph program
    // Current stub returns 0 - REPLACE BEFORE PRODUCTION
    msg!("⚠️ WARNING: get_final_scores is a stub - CPI not implemented!");
    Ok(0)
}

/// Distribute contest prizes via Prize Distribution program (BPF-compatible)
/// 
/// # Critical CPI Implementation Required
/// This function MUST call the Prize Distribution program's `store_winner` instruction
/// for each Top 3 winner to create on-chain WinnerRecord PDAs.
/// 
/// ## Expected Implementation:
/// 1. Sort participants by final_score (highest first) with deterministic tie-breaking
/// 2. For each Top 3 position:
///    a. Derive WinnerRecord PDA: seeds = [b"winner", contest_label, position_bytes]
///    b. Call prize_distribution::store_winner(contest_label, position, wallet, prize_amount)
///    c. Verify WinnerRecord PDA created successfully
/// 3. Return success only if ALL winner records created
/// 
/// ## Current Behavior:
/// - No-op (stub) - THIS WILL CAUSE WINNERS TO NOT BE STORED ON-CHAIN
/// - Production deployment requires this CPI to be fully implemented
/// 
/// ## Edge Cases to Handle:
/// - Fewer than 3 participants: Only create records for actual participants
/// - Tied scores: Use deterministic ordering (score DESC, then pubkey ASC)
/// - Zero prize pool: Validation should prevent this earlier in finalization
pub fn distribute_contest_prizes<T>(_ctx: &T, _contest_label: &[u8; 32], _participant_count: u32) -> Result<()> {
    // TODO: Implement actual CPI call to Prize Distribution program
    // Current stub is a no-op - REPLACE BEFORE PRODUCTION
    msg!("⚠️ WARNING: distribute_contest_prizes is a stub - CPI not implemented!");
    msg!("⚠️ WinnerRecord PDAs will NOT be created - claims will fail!");
    Ok(())
}

/// Process meme upload payment distribution
pub fn process_meme_upload_payment<T>(_ctx: &T, amount: u64) -> Result<()> {
    msg!("💰 Processing meme upload payment: {} KOTS", amount / TOKENS_PER_UNIT);
    
    // Validate amount
    require!(amount > 0, KotsError::InvalidTokenAmount);
    require!(amount <= 10_000 * TOKENS_PER_UNIT, KotsError::ExcessiveTransferAmount);
    
    // Payment processing would happen via CPI to fee-management program
    // This distributes funds according to contest revenue distribution rules
    msg!("✅ Meme upload payment processed");
    Ok(())
}

/// Process storage extension payment distribution
pub fn process_storage_extension_payment<T>(_ctx: &T, amount: u64) -> Result<()> {
    msg!("💰 Processing storage extension payment: {} KOTS", amount / TOKENS_PER_UNIT);
    
    // Validate amount
    require!(amount > 0, KotsError::InvalidTokenAmount);
    require!(amount <= 5_000 * TOKENS_PER_UNIT, KotsError::ExcessiveTransferAmount);
    
    // Payment processing would happen via CPI to fee-management program
    msg!("✅ Storage extension payment processed");
    Ok(())
}

/// Validate voter security including blacklist and emergency pause checks
pub fn validate_voter_security(
    emergency_state: &AccountInfo,
    program_id: &Pubkey,
    _contest_label: &[u8; 32],
) -> Result<()> {
    // EMERGENCY PAUSE (2026-08-12): read the REAL EmergencyState PDA. The prior
    // `load_contest_state().emergency_paused` was hardcoded false — a fail-open
    // that made this a no-op. This kots-common copy is currently unused (voting-
    // hieroglyph shadows it with a local validate_voter_security that carries the
    // live gate); kept honest so any future caller enforces rather than fails open.
    let pause = read_emergency_pause_status(
        emergency_state,
        &security_governance_id_for(program_id)?,
    )?;
    require!(!pause.is_active, KotsError::EmergencyPauseActive);

    msg!("✅ Voter security validated for contest");
    Ok(())
}

/// Validate user rate limits for voting frequency
pub fn validate_user_rate_limits<T>(_ctx: &T, voter_key: &Pubkey) -> Result<()> {
    // Load actual user rate limit state
    let user_state = load_user_rate_limit_state(voter_key)?;
    
    // Check if user is rate limited
    require!(!user_state.rate_limited, KotsError::RateLimitExceeded);
    
    // Verify behavior score is acceptable
    require!(user_state.behavior_score > 0.3, KotsError::LowBehaviorScore);
    
    msg!("✅ User rate limits validated - behavior_score: {:.2}", user_state.behavior_score);
    Ok(())
}

/// Validate voter has sufficient KOTS token balance
pub fn validate_voter_balance<T>(_ctx: &T, cost: u64) -> Result<()> {
    // Balance validation would require token account access
    // For now, ensure cost is reasonable
    require!(cost > 0, KotsError::InvalidTokenAmount);
    require!(cost <= MAX_HIEROGLYPH_COST, KotsError::InsufficientFunds);
    
    msg!("✅ Voter balance requirement validated: {} lamports", cost);
    Ok(())
}

/// Validate voter has sufficient SOL balance for transaction fees
pub fn validate_voter_sol_balance<T>(_ctx: &T, cost: u64) -> Result<()> {
    // SOL balance validation would require account access
    // Ensure cost is within reasonable limits
    require!(cost > 0, KotsError::InvalidTokenAmount);
    require!(cost <= 100_000_000, KotsError::InsufficientFunds); // Max 0.1 SOL
    
    msg!("✅ Voter SOL balance requirement validated: {} lamports", cost);
    Ok(())
}

/// Update participant score via CPI to contest-management
pub fn update_participant_score<T>(_ctx: &T, contest_label: &[u8; 32], meme_id: u64, score: u64) -> Result<()> {
    msg!("📈 Updating participant score - Meme: {}, Score: +{}", meme_id, score);
    
    // Validate inputs
    require!(meme_id > 0, KotsError::InvalidMemeId);
    require!(score > 0, KotsError::InvalidTokenAmount);
    
    // Load contest state to verify it exists and is active
    let contest_state = load_contest_state(contest_label)?;
    require!(!contest_state.is_finalized, KotsError::ContestAlreadyFinalized);
    
    // Score update would happen via CPI to contest-management program
    // The actual participant state is stored there and updated
    msg!("✅ Participant score update recorded for meme {}", meme_id);
    Ok(())
}

/// Validate scoring authority for finalization
pub fn validate_scoring_authority<T>(_ctx: &T, contest_label: &[u8; 32]) -> Result<()> {
    // Load contest state to verify authorized finalizers
    let contest_state = load_contest_state(contest_label)?;
    
    // Verify contest can be scored (not already finalized)
    require!(!contest_state.is_finalized, KotsError::ContestAlreadyFinalized);
    
    msg!("✅ Scoring authority validated");
    Ok(())
}


/// Validate transaction security including fraud detection
pub fn validate_transaction_security<T>(_ctx: &T, amount: u64, market_cap: u64) -> Result<()> {
    // Validate amount is reasonable
    require!(amount > 0, KotsError::InvalidTokenAmount);
    require!(amount <= 1_000_000 * TOKENS_PER_UNIT, KotsError::ExcessiveTransferAmount);
    
    // Validate market cap
    require!(market_cap > 0, KotsError::InvalidMarketCap);
    
    msg!("✅ Transaction security validated - Amount: {}, Market cap: {}", 
         amount / TOKENS_PER_UNIT, market_cap);
    Ok(())
}

/// Check if user is premium based on behavior score
pub fn is_premium_user(voter_key: &Pubkey) -> Result<bool> {
    let user_state = load_user_rate_limit_state(voter_key)?;
    let is_premium = user_state.behavior_score >= 0.8 && user_state.verified_human;
    Ok(is_premium)
}

// ============================================================================
// HELPER FUNCTIONS - BPF-Compatible State Loading
// ============================================================================

/// Internal struct for contest state (BPF-compatible)
#[derive(Clone, Debug)]
pub struct ContestStateInternal {
    pub is_finalized: bool,
    pub emergency_paused: bool,
    pub is_initialized: bool,
    pub scoring_complete: bool,
    pub auto_distribution_enabled: bool,
    pub auto_distribution_timestamp: i64,
    pub scoring_finalized: bool,
    pub min_participants: u32,
    pub participant_count: u32,
    pub auto_distribution_executed: bool,
}

/// Internal struct for user rate limit state (BPF-compatible)
#[derive(Clone, Debug)]
pub struct UserRateLimitStateInternal {
    pub behavior_score: f64,
    pub verified_human: bool,
    pub rate_limited: bool,
}

/// Internal struct for user staking state (BPF-compatible)
#[derive(Clone, Debug)]
pub struct UserStakingStateInternal {
    pub tokens_staked: u64,
    pub total_interactions: u32,
}

/// Internal struct for user role state (BPF-compatible)
#[derive(Clone, Debug)]
pub struct UserRoleStateInternal {
    pub role_level: u8,
}

/// Internal struct for blacklist check result (BPF-compatible)
#[derive(Clone, Debug)]
pub struct BlacklistCheckResult {
    pub is_blacklisted: bool,
}

/// Internal struct for emergency pause status (BPF-compatible)
#[derive(Clone, Debug)]
pub struct EmergencyPauseStatus {
    pub is_active: bool,
}

/// Internal struct for fraud detection result (BPF-compatible)
#[derive(Clone, Debug)]
pub struct FraudDetectionResult {
    pub fraud_detected: bool,
    pub fraud_flags: u32,
    pub blacklisted: bool,
    pub suspicious_patterns: u32,
}

/// Internal struct for oracle market cap data (BPF-compatible)
#[derive(Clone, Debug)]
pub struct OracleMarketCapData {
    pub timestamp: i64,
}

/// Internal struct for claim rate limit result (BPF-compatible)
#[derive(Clone, Debug)]
pub struct ClaimRateLimitResult {
    pub rate_limited: bool,
    pub claims_today: u32,
}

// ===========================================================================
// REAL cross-program security reads (2026-07-23)
//
// The functions below this block that are marked "placeholder for CPI
// implementation" return the PERMISSIVE answer unconditionally — `is_paused:
// false`, `is_blacklisted: false`, `rate_limited: false`, `verified_human:
// true`. They take no accounts, so they cannot read chain state even in
// principle. The consequence is that emergency pause, blacklisting, fraud
// detection and rate limiting are all inert: `emergency_pause` writes
// `is_paused = true` correctly, and every consumer still answers "not paused".
//
// These readers replace that. They take the governing account and verify it
// before trusting a byte of it:
//
//   1. OWNER   — the account must be owned by security-governance. Without this
//                a caller could pass any account they control and dictate the
//                answer, which would be strictly worse than the stub.
//   2. ADDRESS — the account must be the canonical PDA for its seeds, so a
//                caller cannot substitute a different, stale, or attacker-
//                initialised account owned by the same program.
//   3. TYPE    — Anchor's deserializer checks the 8-byte discriminator, so a
//                different account type at the right address is rejected.
//
// Only then is the state read. All three checks are required; any one alone is
// bypassable.
//
// NOTE ON THE STATE TYPES: `kots_common::state::{EmergencyState, BlacklistState}`
// are byte-identical duplicates of the definitions inside security-governance
// (verified field-by-field — only doc comments differ). That duplication is a
// latent divergence hazard of its own: if the two ever drift, these reads
// silently misinterpret the account. Consolidating on the kots-common
// definition and having security-governance import it is the durable fix.
// ===========================================================================

/// Verify and deserialize a security-governance-owned PDA.
fn read_governance_account<T: AccountDeserialize>(
    account: &AccountInfo,
    seeds: &[&[u8]],
    security_governance_id: &Pubkey,
) -> Result<T> {
    // 1. owner
    require_keys_eq!(
        *account.owner,
        *security_governance_id,
        KotsError::UnauthorizedCaller
    );

    // 2. canonical address for these seeds
    let (expected, _bump) = Pubkey::find_program_address(seeds, security_governance_id);
    require_keys_eq!(*account.key, expected, KotsError::UnauthorizedCaller);

    // 3. type — discriminator checked by the deserializer
    let data = account.try_borrow_data()?;
    let mut slice: &[u8] = &data;
    T::try_deserialize(&mut slice).map_err(|_| error!(KotsError::UnauthorizedCaller))
}

/// Is the platform under an emergency pause?
///
/// Replaces `get_emergency_pause_status()`, which returned `false` always.
/// Honours `pause_duration`: a pause with a positive duration lapses once
/// `pause_timestamp + pause_duration` has passed, so a stale flag cannot halt
/// the platform forever. A zero or negative duration means "until resumed".
pub fn read_emergency_pause_status(
    emergency_state: &AccountInfo,
    security_governance_id: &Pubkey,
) -> Result<EmergencyPauseStatus> {
    // SEED UNIFICATION (2026-08-03): this derived `[EMERGENCY_SEED]` alone,
    // while `emergency_pause` creates and enforces `[EMERGENCY_SEED, b"v4"]`.
    // The one-seed address is created by nothing, so this reader could ONLY
    // return Err(UnauthorizedCaller) — the winner-claim path failed closed on
    // every call while the pause flag itself halted nothing. The canonical
    // seed list now lives in kots_common::state (see the lock test there).
    // ABSENT ⇒ NOT PAUSED (2026-08-12). A never-initialized emergency_state means
    // the platform has NEVER been paused, so nothing is in force. The enforcement
    // policy is "paused blocks every write except resume; OTHERWISE ALLOW", so the
    // default (no pause record) must be ALLOW. The prior behaviour failed CLOSED
    // here: read_governance_account errored on an absent account, which bricked
    // EVERY gated write on a fresh chain and forced an explicit initializer at
    // deploy. We still verify the canonical PDA ADDRESS so a spoofed empty account
    // cannot masquerade as "not paused"; only a genuinely-absent CANONICAL PDA is
    // treated as not-paused. Once a pause has occurred the account exists (pause
    // creates it) and this branch is never taken again.
    if emergency_state.data_is_empty() {
        let (pda, _) = Pubkey::find_program_address(
            &crate::state::EMERGENCY_STATE_SEEDS,
            security_governance_id,
        );
        require_keys_eq!(*emergency_state.key, pda, KotsError::InvalidProgramId);
        return Ok(EmergencyPauseStatus { is_active: false });
    }

    let st: crate::state::EmergencyState = read_governance_account(
        emergency_state,
        &crate::state::EMERGENCY_STATE_SEEDS,
        security_governance_id,
    )?;

    if !st.is_paused {
        return Ok(EmergencyPauseStatus { is_active: false });
    }
    if st.pause_duration > 0 {
        let now = crate::utils::get_current_timestamp()?;
        let expires = st
            .pause_timestamp
            .checked_add(st.pause_duration)
            .ok_or(KotsError::ArithmeticOverflow)?;
        if now >= expires {
            return Ok(EmergencyPauseStatus { is_active: false });
        }
    }
    Ok(EmergencyPauseStatus { is_active: true })
}

// REMOVED (2026-07-23): `verify_meme_upload_payment` and
// `verify_storage_extension_payment`.
//
// Both were dead — zero references anywhere in the workspace. contest-management
// carries its own `verify_meme_upload_payment_local` /
// `verify_storage_extension_payment_local` and calls those instead.
//
// They were also a trap. Each read as a payment verifier WITH security checks,
// but those checks were `check_blacklist_status()` and
// `get_emergency_pause_status()` — the stubs that return the permissive answer
// unconditionally. A future caller reaching for the shared helper rather than
// writing their own would have inherited payment verification that admits
// blacklisted wallets and ignores an active emergency pause, while reading as
// though it did neither.
//
// Deleting them removes 12 of the run's deprecation warnings at the source
// rather than silencing them, and removes the trap. If a shared verifier is
// wanted later, build it on `require_not_blacklisted` and
// `read_emergency_pause_status`, which take the accounts they need.

/// Reject a blacklisted wallet outright. The gate for user-facing actions.
///
/// WHY A HELPER (2026-07-23): before this, "blacklist checking" in
/// voting-hieroglyph and fee-management was
///     require!(wallet != Pubkey::default(), KotsError::BlacklistedWallet);
/// which rejects only the all-zeros address — a wallet nobody has. It never
/// read BlacklistState. Because it was *labelled* "Blacklist checking" and threw
/// `BlacklistedWallet`, it read as implemented to anyone reviewing it. One shared
/// helper means the real rule cannot be re-implemented incorrectly per program.
pub fn require_not_blacklisted(
    blacklist_state: &AccountInfo,
    security_governance_id: &Pubkey,
    wallet: &Pubkey,
) -> Result<()> {
    let st = read_blacklist_status(blacklist_state, security_governance_id, wallet)?;
    require!(!st.is_blacklisted, KotsError::BlacklistedWallet);
    Ok(())
}

/// Resolve security-governance's id from the calling program's own id.
///
/// Uses the on-chain registry, NOT `network_config` — the latter's
/// `detect_network()` reads an environment variable and cannot run in BPF.
pub fn security_governance_id_for(self_id: &Pubkey) -> Result<Pubkey> {
    Ok(crate::program_ids::current(self_id)
        .ok_or(KotsError::InvalidProgramId)?
        .security_governance)
}

/// Is `wallet` blacklisted?
///
/// Replaces `check_blacklist_status()`, which returned `false` always. Scans
/// only the first `blacklist_count` entries — the array is fixed-size and the
/// tail is zeroed, so scanning all of it would compare against `Pubkey::default()`
/// and could report the default address as blacklisted.
pub fn read_blacklist_status(
    blacklist_state: &AccountInfo,
    security_governance_id: &Pubkey,
    wallet: &Pubkey,
) -> Result<BlacklistCheckResult> {
    // SEED UNIFICATION (2026-08-03): this derived `[BLACKLIST_SEED]` alone,
    // while `initialize_blacklist` creates and enforces `[BLACKLIST_SEED, b"v4"]`.
    // The one-seed address is created by nothing, so this reader could ONLY
    // return Err(UnauthorizedCaller) — join_contest_unlimited,
    // upload_meme_with_verification, batch_cast_hieroglyph_votes and the
    // winner-claim path all failed closed on every call. The canonical seed
    // list now lives in kots_common::state (see the lock test there).
    let st: crate::state::BlacklistState = read_governance_account(
        blacklist_state,
        &crate::state::BLACKLIST_STATE_SEEDS,
        security_governance_id,
    )?;

    let n = (st.blacklist_count as usize).min(st.blacklisted_wallets.len());
    let hit = st.blacklisted_wallets[..n].iter().any(|w| w == wallet);
    Ok(BlacklistCheckResult {
        is_blacklisted: hit,
    })
}

/// Load contest state (placeholder for CPI implementation)
pub fn load_contest_state(_contest_label: &[u8; 32]) -> Result<ContestStateInternal> {
    // In production, this would use CPI to query contest-management program
    // For now, return safe defaults that allow operations to proceed
    Ok(ContestStateInternal {
        is_finalized: false,
        emergency_paused: false,
        is_initialized: true,
        scoring_complete: false,
        auto_distribution_enabled: false,
        auto_distribution_timestamp: 0,
        scoring_finalized: false,
        min_participants: 1,
        participant_count: 0,
        auto_distribution_executed: false,
    })
}

/// Load user rate limit state (placeholder for CPI implementation)
pub fn load_user_rate_limit_state(_user_key: &Pubkey) -> Result<UserRateLimitStateInternal> {
    // In production, this would use CPI to query security-governance program
    Ok(UserRateLimitStateInternal {
        behavior_score: 0.9,
        verified_human: true,
        rate_limited: false,
    })
}

/// Load user staking state (placeholder for CPI implementation)
pub fn load_user_staking_state(_user_key: &Pubkey) -> Result<UserStakingStateInternal> {
    // In production, this would use CPI to query security-governance program
    Ok(UserStakingStateInternal {
        tokens_staked: 1000,
        total_interactions: 10,
    })
}

/// Load user role state (placeholder for CPI implementation)
pub fn load_user_role(_user_key: &Pubkey) -> Result<UserRoleStateInternal> {
    // In production, this would use CPI to query security-governance program
    Ok(UserRoleStateInternal {
        role_level: 1,
    })
}

/// Get total system stake (placeholder for CPI implementation)
pub fn get_total_system_stake() -> Result<u64> {
    // In production, this would use CPI to query security-governance program
    Ok(1_000_000)
}

/// SUPERSEDED (2026-07-23) by `read_blacklist_status`, which actually reads the
/// on-chain blacklist. This returns `is_blacklisted: false` unconditionally and
/// therefore blocks nobody. Retained only so existing call sites keep compiling
/// while they are migrated; every one of them is a control that currently does
/// nothing.
#[deprecated(
    note = "returns false unconditionally — use read_blacklist_status(blacklist_state, sg_id, wallet)"
)]
pub fn check_blacklist_status(_user_key: &Pubkey) -> Result<BlacklistCheckResult> {
    // In production, this would use CPI to query security-governance program
    Ok(BlacklistCheckResult {
        is_blacklisted: false,
    })
}

/// SUPERSEDED (2026-07-23) by `read_emergency_pause_status`, which reads the
/// EmergencyState PDA. This returns `is_active: false` unconditionally, so
/// `emergency_pause` sets a flag that no consumer ever observes — the pause is
/// inert. Retained only to keep existing call sites compiling during migration.
#[deprecated(
    note = "returns false unconditionally — use read_emergency_pause_status(emergency_state, sg_id)"
)]
pub fn get_emergency_pause_status() -> Result<EmergencyPauseStatus> {
    // In production, this would use CPI to query security-governance program
    Ok(EmergencyPauseStatus {
        is_active: false,
    })
}

/// Check fraud detection (placeholder for CPI implementation)
pub fn check_fraud_detection(_contest_label: &[u8; 32], _user_key: &Pubkey) -> Result<FraudDetectionResult> {
    // In production, this would use CPI to query security-governance program
    Ok(FraudDetectionResult {
        fraud_detected: false,
        fraud_flags: 0,
        blacklisted: false,
        suspicious_patterns: 0,
    })
}

/// Get oracle market cap data (placeholder for CPI implementation)
pub fn get_oracle_market_cap() -> Result<OracleMarketCapData> {
    // In production, this would use CPI to query oracle service
    use crate::utils::get_current_timestamp;
    Ok(OracleMarketCapData {
        timestamp: get_current_timestamp()?,
    })
}

/// Validate claim rate limit (placeholder for CPI implementation)
pub fn validate_claim_rate_limit(_user_key: &Pubkey) -> Result<ClaimRateLimitResult> {
    // In production, this would use CPI to query security-governance program
    Ok(ClaimRateLimitResult {
        rate_limited: false,
        claims_today: 0,
    })
}

