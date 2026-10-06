//! KOTS Contest Management Program
//! 
//! Handles contest creation, participant management, contest lifecycle,
//! and coordination with other KOTS programs.

// Prevent both features from being enabled simultaneously during BPF compilation
// Allow IDL generation to proceed with both features if needed
#[cfg(all(feature = "idl-build", feature = "bpf-runtime", target_arch = "bpf"))]
compile_error!("Cannot enable both 'idl-build' and 'bpf-runtime' features simultaneously during BPF compilation");


use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};
use kots_common::{
    constants::{*, MAX_CONTEST_LABEL_LENGTH}, errors::*, cpi::*, utils::*,
    init::{request_treasury_reimbursement, InitOptions},
    PaymentVerification, PaymentType, MemeExpirationData,
    StorageTier, CostMetadata, // Always import for function signatures
    MEME_ACCOUNT_SEED, FIXED_MEME_UPLOAD_COST, DEFAULT_STORAGE_DURATION_MONTHS,
    STANDARD_STORAGE_MULTIPLIER, PREMIUM_STORAGE_MULTIPLIER, ARCHIVE_STORAGE_MULTIPLIER,
    AVERAGE_MEME_SIZE_MB, BANDWIDTH_COST_PER_MB, STORAGE_COST_PER_MB_MONTH,
    MAX_STORAGE_EXTENSION_MONTHS,
    state::{
        CONTEST_SEED, PARTICIPANT_SEED, VAULT_SEED
    }
};

// Storage tier constants for both IDL and BPF compilation
const STORAGE_TIER_STANDARD: u8 = 0;
const STORAGE_TIER_PREMIUM: u8 = 1;
const STORAGE_TIER_ARCHIVE: u8 = 2;

// Program type constants now imported from kots_common
// const PROGRAM_TYPE_CONTEST_MANAGEMENT: u8 = 0; // Now in kots_common::constants

// Program-local account size constants for IDL generation
// These constants must match the exact struct sizes including discriminator
const CONTEST_STATE_SPACE: usize = 1080;     // 8 (discriminator) + 1072 (ContestCoreState)
const MEME_ACCOUNT_SPACE: usize = 512;       // 8 (discriminator) + 485 (MemeAccount) + padding for safety
// 8 (Anchor discriminator) + 414 (ParticipantState borsh body) = 422 minimum;
// 512 gives ~90 bytes of headroom (same power-of-two style as
// MEME_ACCOUNT_SPACE). Over-sizing is safe: borsh ignores trailing bytes, and
// no participant_state account has ever been created on any network
// (join_contest_unlimited has never completed), so there is no migration
// concern.
//
// THE 414 IS FOR THE **LOCAL** STRUCT IN THIS FILE (see `pub struct
// ParticipantState` below). That local definition SHADOWS the glob-imported
// `kots_common::state::ParticipantState`, and the two have diverged:
//   local ipfs_hash is [u8; 64] (shared uses 46), and the local struct carries
//   interaction_breakdown [u32; 32] plus the win/award tail fields.
// Field-by-field (local): 1 is_initialized + 32 contest_label + 32 user_wallet
// + 8 meme_id + 64 ipfs_hash + 8 entry_ts + 8 join_ts + 4 current_score
// + 8 total_votes + 1 is_disqualified + 8 stake + 8 total_interactions
// + 4 unique_voters + 8 final_score + 4 prize_position + 8 prize_amount
// + 1 prize_claimed + 8 claim_ts + 8 base_score + 8 time_bonus
// + 8 diversity_bonus + 8 consistency_bonus + 8 premium_bonus
// + 8 interaction_velocity (f64) + 4 dominant_interaction_type
// + 128 interaction_breakdown ([u32;32]) + 1 win_position + 1 award_type
// + 8 award_amount + 8 final_award_timestamp + 1 bump = 414.
//
// SIZING HISTORY (why this is the THIRD value): 384 assumed a 376 body; the
// 2026-08-04 bump to 400 audited the WRONG struct — it took the shared
// kots_common definition's 46-byte ipfs_hash and stopped before the five tail
// fields, landing on 377 and therefore 22 bytes short. Both undersizings
// failed identically at runtime: `init` created the account, `try_from` ran
// borsh past the end of the data, and every join surfaced
// AccountDidNotDeserialize (3003) on `participant_state`.
// FOLLOW-UP (tracked in the plan): unify the duplicated state structs — the
// silent local/shared divergence is what produced two wrong sizings in a row.
const PARTICIPANT_STATE_SPACE: usize = 512;

// Account struct definitions at root level for proper IDL generation
// These structs are always visible to Anchor's IDL generator

#[account]
#[derive(Debug)]
pub struct ContestCoreState {
    pub is_initialized: bool,
    pub contest_label: [u8; 32], // Fixed size for both IDL and BPF compatibility
    pub creator: Pubkey,
    pub creation_timestamp: i64,
    pub duration_seconds: i64,
    pub min_participants: u16,
    pub entry_fee: u64,
    pub total_prize_pool: u64,
    pub participant_count: u32,
    pub is_finalized: bool,
    pub finalization_timestamp: i64,
    pub finalized_by: Pubkey,
    pub total_interactions: u64,
    pub scoring_complete: bool,
    pub is_paused: bool,
    pub is_active: bool,
    pub pause_timestamp: i64,
    pub pause_reason: [u8; MAX_STRING_LENGTH], // Fixed size for both IDL and BPF compatibility
    pub vault_authority: Pubkey,
    pub vault_bump: u8,
    pub auto_distribution_enabled: bool,
    pub auto_distribution_timestamp: i64,
    pub peak_interaction_rate: f64,
    pub most_popular_interaction: u32,
    pub total_unique_voters: u32,
    pub authorized_finalizers: [Pubkey; 5], // Fixed size for both IDL and BPF compatibility
    pub authorized_finalizers_count: u8,
}

#[account]
#[derive(Debug)]
pub struct MemeAccount {
    pub is_initialized: bool,
    pub meme_id: [u8; 8],
    pub owner: Pubkey,
    pub contest_labels: [[u8; MAX_CONTEST_LABEL_LENGTH]; 10], // BPF-safe: 32 * 10 = 320 bytes
    pub contest_count: u8,
    pub ipfs_hash: [u8; 64], // Match instruction parameter size (64 bytes for future IPFS v1 hashes)
    pub upload_timestamp: i64,
    pub expiry_timestamp: i64,
    pub storage_tier: u8, // 0=Standard, 1=Premium, 2=Archive
    pub cost_breakdown: [u8; 32], // Simplified cost representation
    pub total_paid: u64,
    pub extension_count: u8,
    pub bump: u8,
}

#[account]
#[derive(Debug)]
pub struct ParticipantState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH], // BPF-safe: 32 bytes
    pub user_wallet: Pubkey,
    pub meme_id: u64,
    pub ipfs_hash: [u8; 64], // Match instruction parameter size (64 bytes for future IPFS v1 hashes)
    pub entry_timestamp: i64,
    pub join_timestamp: i64,
    pub current_score: i32,
    pub total_votes_received: u64,
    pub is_disqualified: bool,
    pub stake_amount: u64,
    pub total_interactions_received: u64,
    pub unique_voters: u32,
    pub final_score: u64,
    pub prize_position: u32,
    pub prize_amount: u64,
    pub prize_claimed: bool,
    pub claim_timestamp: i64,
    pub base_score: u64,
    pub time_bonus: u64,
    pub diversity_bonus: u64,
    pub consistency_bonus: u64,
    pub premium_bonus: u64,
    pub interaction_velocity: f64,
    pub dominant_interaction_type: u32,
    pub interaction_breakdown: [u32; 32],
    pub win_position: u8,
    pub award_type: u8,
    pub award_amount: u64,
    pub final_award_timestamp: i64,
    pub bump: u8,
}

#[account]
#[derive(Debug)]
pub struct ProgramUpgradeState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub program_type: u8, // 0-6 for each program type
    pub version: u16,
    pub initialization_timestamp: i64,
}

// Account structs are now defined at root level - no conditional imports needed

// Import ProgramType only for BPF compilation (not during IDL generation)
#[cfg(not(all(feature = "idl-build", not(target_arch = "bpf"))))]
use kots_common::ProgramType;


// Unified program ID for both devnet and mainnet
#[cfg(feature = "realm-main")]
declare_id!("CdtbPRx3ghk8vjjuXShsKMPeoLXCpqTYDDZHQBsamrXe");

#[cfg(not(feature = "realm-main"))]
declare_id!("FMKwB7AnhwHJLq5fphVYJcxZnRf72ECaHdkYApw3yn91");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
// Example update: "contest-management|20251005T1000Z|c8d3f21"
const VERSION_IDENT: &[u8] = b"contest-management|20251014T1800Z|finalization-edge-case-fixes";


#[derive(Accounts)]
pub struct VersionHeartbeat {}

// EXPECTED WARNING — do not "fix" this by adding #![allow(deprecated)].
//
// Anchor 0.31.1's #[program] macro expands to a call to the deprecated
// AccountInfo::realloc. The call is in GENERATED code emitted at crate root, not
// inside this module, so a module-level #[allow(deprecated)] does not reach it —
// tried 2026-07-28, the warning was unchanged.
//
// The only thing that silences it is a crate-level #![allow(deprecated)], and
// that is a bad trade here: `solana_program::system_instruction` is being
// deprecated in favour of the `solana-system-interface` crate, six of these
// programs use it, and the test crate already warns about it. Blanket-silencing
// would hide that migration the moment it lands on the program crates.
//
// One unactionable warning per program is the correct cost. It goes away on the
// next Anchor upgrade, not before.
#[program]
pub mod contest_management {
    use super::*;
    
    /// Query deployed program version
    /// 
    /// This instruction returns a hard-coded version identifier that changes
    /// with each build. Clients can query this to verify which version is
    /// deployed on-chain without complex testing.
    /// 
    /// Format: "program-name|timestamp|build-hash"
    /// Example: "contest-management|20251004T1630Z|a3f9c2d"
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        // Use Solana's set_return_data to return version bytes
        // This is BPF-safe and retrievable via RPC simulation
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        
        // Also log for convenience
        msg!("📡 Version: {}", 
            core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        
        Ok(())
    }

    /// Initialize contest management program state
    pub fn initialize_contest_management(
        ctx: Context<InitializeContestManagement>,
    ) -> Result<()> {
        // FRONT-RUN GUARD (2026-07-23). Initializers are first-caller-wins: this
        // instruction writes `authority = whoever signed`. On mainnet only the
        // ceremony multisig may call it, so a bot watching the deploy cannot
        // claim the authority in the window before the ceremony initializes.
        // Open on devnet (throwaway keys); fails closed in an unclaimed estate.
        require!(
            kots_common::program_ids::initializer_is_authorized(
                ctx.program_id,
                &ctx.accounts.authority.key(),
            ),
            KotsError::UnauthorizedCaller
        );

        msg!("🚀 Initializing Contest Management Program");
        
        // Anchor already created the PDA with #[account(init)] - just set the fields
        let program_state = &mut ctx.accounts.program_state;
        
        #[cfg(feature = "idl-build")]
        let program_type_value = PROGRAM_TYPE_CONTEST_MANAGEMENT;
        #[cfg(not(feature = "idl-build"))]
        let program_type_value = ProgramType::ContestManagement as u8;
        
        program_state.is_initialized = true;
        program_state.authority = ctx.accounts.authority.key();
        program_state.program_type = program_type_value;
        program_state.version = CURRENT_CONTEST_MANAGEMENT_VERSION;
        program_state.initialization_timestamp = get_current_timestamp()?;
        
        // Optional: Request treasury reimbursement for the PDA creation cost
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(256);
        let _ = request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions { allow_creator_payment: false, ..InitOptions::default() },
        ); // Ignore treasury reimbursement errors - user already paid
        
        msg!("✅ Contest Management Program initialized successfully");
        Ok(())
    }

    /// Create contest with unlimited participant scale support
    pub fn create_contest_unlimited(
        ctx: Context<CreateContestUnlimited>,
        contest_label: [u8; 32],
        duration_seconds: i64,
        min_participants: u16,
        entry_fee: u64,
        auto_distribution_enabled: bool,
    ) -> Result<()> {
        msg!("🏆 Creating unlimited contest");
        // EMERGENCY PAUSE GATE (2026-08-06): reject this operation when
        // security-governance has a global pause active. Mirrors the blacklist
        // gate; read_emergency_pause_status verifies owner + canonical PDA +
        // discriminator before reading is_paused.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);
        
        validate_contest_creation_params(duration_seconds, min_participants, entry_fee)?;
        
        // Calculate vault bump for the vault PDA
        let vault_seeds = &[VAULT_SEED, &contest_label];
        let (vault_pda, vault_bump) = Pubkey::find_program_address(vault_seeds, ctx.program_id);
        
        // Anchor already created the contest PDA with #[account(init)] - just initialize it
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(CONTEST_STATE_SPACE);
        let _ = request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.creator.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions { allow_creator_payment: false, ..InitOptions::default() },
        ); // Ignore treasury reimbursement errors - user already paid
        
        // Anchor already created the contest PDA - just set the fields directly
        let contest = &mut ctx.accounts.contest_state;
        let current_time = get_current_timestamp()?;
        
        contest.is_initialized = true;
        contest.contest_label = contest_label;
        contest.creator = ctx.accounts.creator.key();
        contest.creation_timestamp = current_time;
        contest.duration_seconds = duration_seconds;
        contest.min_participants = min_participants;
        contest.entry_fee = entry_fee;
        contest.total_prize_pool = 0;
        contest.participant_count = 0;
        contest.is_finalized = false;
        contest.finalization_timestamp = 0;
        contest.finalized_by = Pubkey::default();
        contest.total_interactions = 0;
        contest.scoring_complete = false;
        contest.is_paused = false;
        contest.is_active = true;
        contest.pause_timestamp = 0;
        contest.pause_reason = [0u8; MAX_STRING_LENGTH];
        contest.vault_authority = vault_pda;
        contest.vault_bump = vault_bump;
        contest.auto_distribution_enabled = auto_distribution_enabled;
        contest.auto_distribution_timestamp = 0;
        contest.peak_interaction_rate = 0.0;
        contest.most_popular_interaction = 0;
        contest.total_unique_voters = 0;
        contest.authorized_finalizers = {
            let mut finalizers = [Pubkey::default(); MAX_AUTHORIZED_FINALIZERS];
            finalizers[0] = ctx.accounts.creator.key();
            finalizers
        };
        contest.authorized_finalizers_count = 1;

        msg!("✅ Contest created - Duration: {}s, Min participants: {}, Entry fee: {} KOTS", 
             duration_seconds, min_participants, entry_fee / TOKENS_PER_UNIT);
        
        Ok(())
    }

    /// Join contest with unlimited scale support
    pub fn join_contest_unlimited(
        ctx: Context<JoinContestUnlimited>,
        contest_label: [u8; 32],
        meme_id: u64,
        stake_amount: u64,
    ) -> Result<()> {
        // BLACKLIST GATE (2026-07-23). A blacklisted account could enter contests and accrue standing.
        kots_common::cpi::require_not_blacklisted(
            &ctx.accounts.blacklist_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
            &ctx.accounts.user.key(),
        )?;
        // EMERGENCY PAUSE GATE (2026-08-06): reject this operation when
        // security-governance has a global pause active. Mirrors the blacklist
        // gate; read_emergency_pause_status verifies owner + canonical PDA +
        // discriminator before reading is_paused.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);

        msg!("📤 Joining contest with meme {}", meme_id);
        
        let contest_label_bytes = contest_label;
        validate_meme_id(meme_id)?;
        require!(stake_amount >= MIN_PARTICIPANT_STAKE, KotsError::InsufficientStake);
        
        // Validate contest is active and within time bounds
        {
            let contest = &ctx.accounts.contest_state;
            require!(contest.is_active, KotsError::ContestNotActive);
            require!(!contest.is_finalized, KotsError::ContestAlreadyFinalized);
            require!(!contest.is_paused, KotsError::ContestPaused);
            
            let current_time = get_current_timestamp()?;
            let contest_end_time = contest.creation_timestamp
                .checked_add(contest.duration_seconds)
                .ok_or(KotsError::ArithmeticOverflow)?;
            
            require!(current_time < contest_end_time, KotsError::ContestEnded);
        }
        
        // Enhanced security validation via CPI to Security program
        validate_participant_security(&ctx.accounts.user.key())?;
        
        // Anchor already created the participant PDA - just initialize it
        let participant = &mut ctx.accounts.participant_state;
        let current_time = get_current_timestamp()?;
        
        // Optional treasury reimbursement
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(PARTICIPANT_STATE_SPACE);
        let _ = request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.user.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions { allow_creator_payment: false, ..InitOptions::default() },
        );
        
        // Set the participant fields directly
        participant.is_initialized = true;
        participant.contest_label = contest_label_bytes;
        participant.user_wallet = ctx.accounts.user.key();
        participant.meme_id = meme_id;
        participant.ipfs_hash = [0u8; 64];
        participant.entry_timestamp = current_time;
        participant.join_timestamp = current_time;
        participant.current_score = 0;
        participant.total_votes_received = 0;
        participant.is_disqualified = false;
        participant.stake_amount = stake_amount;
        participant.total_interactions_received = 0;
        participant.unique_voters = 0;
        participant.final_score = 0;
        participant.prize_position = 0;
        participant.prize_claimed = false;
        participant.prize_amount = 0;
        // Note: alteration_count, last_interaction_timestamp, interaction_history, 
        // interaction_timestamps, and interaction_count fields removed as they
        // don't exist in the current ParticipantState struct definition
        participant.claim_timestamp = 0;
        // Scoring components
        participant.base_score = 0;
        participant.time_bonus = 0;
        participant.diversity_bonus = 0;
        participant.consistency_bonus = 0;
        participant.premium_bonus = 0;
        // Analytics
        participant.interaction_velocity = 0.0;
        participant.dominant_interaction_type = 0;
        participant.interaction_breakdown = [0; 32];
        // Missing fields
        participant.win_position = 0;
        participant.award_type = 0;
        participant.award_amount = 0;
        participant.final_award_timestamp = 0;
        participant.bump = 0;
        
        // Update contest participant count (unlimited participants!)
        {
            let contest = &mut ctx.accounts.contest_state;
            contest.participant_count = contest.participant_count.saturating_add(1);
        }
        
        // Process entry fee and stake via Economics program CPI
        let required_stake = ctx.accounts.contest_state.entry_fee;
        if required_stake > 0 {
            // process_entry_fee only VALIDATES (it is a stub that moves nothing).
            process_entry_fee(&ctx, &contest_label_bytes, required_stake)?;

            // ENTRY-FEE COLLECTION (2026-08-06, F17 production gap): actually
            // charge the fee. Move `required_stake` lamports from the participant
            // (already a signer) into treasury_incoming — consistent with the
            // lamport-based treasury the rent reimbursement funds and the CPI
            // test measures. The result is CHECKED (unlike the discarded rent
            // reimbursement): if the participant can't cover it, the join fails
            // cleanly with insufficient funds. Done BEFORE the &mut contest_state
            // borrow below to avoid an aliasing conflict.
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.user.to_account_info(),
                        to: ctx.accounts.treasury_incoming.to_account_info(),
                    },
                ),
                required_stake,
            )?;

            let contest = &mut ctx.accounts.contest_state;
            contest.total_prize_pool = contest.total_prize_pool
                .checked_add(contest.entry_fee)
                .ok_or(KotsError::ArithmeticOverflow)?;
        }
        
        let participant_count = ctx.accounts.contest_state.participant_count;
        msg!("📤 Entry #{} submitted: Meme {} by {} with {} KOTS stake", 
             participant_count, meme_id, ctx.accounts.user.key(), stake_amount / TOKENS_PER_UNIT);
        
        Ok(())
    }

    /// Finalize contest with unlimited scale support
    pub fn finalize_contest_unlimited(
        mut ctx: Context<FinalizeContestUnlimited>,
        contest_label: [u8; 32],
    ) -> Result<()> {
        // BPF-optimized: reduce stack usage by eliminating large local variables
        finalize_contest_unlimited_impl(&mut ctx, &contest_label)
    }

    /// Add authorized contest finalizer
    pub fn add_contest_finalizer(
        ctx: Context<AddContestFinalizer>,
        _contest_label: [u8; 32],
        finalizer_wallet: Pubkey,
    ) -> Result<()> {
        let contest = &mut ctx.accounts.contest_state;
        
        // Only creator or existing finalizers can add new finalizers
        require!(
            contest.creator == ctx.accounts.authority.key() || 
            contest.authorized_finalizers.contains(&ctx.accounts.authority.key()),
            KotsError::UnauthorizedFinalizer
        );
        
        if !contest.authorized_finalizers.contains(&finalizer_wallet) {
            // Find first empty slot and add finalizer
            for i in 0..MAX_AUTHORIZED_FINALIZERS {
                if contest.authorized_finalizers[i] == Pubkey::default() {
                    contest.authorized_finalizers[i] = finalizer_wallet;
                    contest.authorized_finalizers_count += 1;
                    break;
                }
            }
        }
        
        msg!("✅ Added finalizer: {}", finalizer_wallet);
        Ok(())
    }

    /// Pause contest (emergency only)
    pub fn pause_contest(
        ctx: Context<PauseContest>,
        _contest_label: [u8; 32],
        pause_reason: [u8; MAX_STRING_LENGTH],
    ) -> Result<()> {
        let contest = &mut ctx.accounts.contest_state;
        
        // Only authorized finalizers can pause
        require!(
            contest.creator == ctx.accounts.authority.key() || 
            contest.authorized_finalizers.contains(&ctx.accounts.authority.key()),
            KotsError::UnauthorizedFinalizer
        );
        
        contest.is_paused = true;
        contest.pause_timestamp = get_current_timestamp()?;
        contest.pause_reason = pause_reason;
        
        msg!("⏸️ Contest paused by {}", ctx.accounts.authority.key());
        Ok(())
    }

    /// Resume paused contest
    pub fn resume_contest(
        ctx: Context<ResumeContest>,
        _contest_label: [u8; 32],
    ) -> Result<()> {
        let contest = &mut ctx.accounts.contest_state;
        
        // Only authorized finalizers can resume
        require!(
            contest.creator == ctx.accounts.authority.key() || 
            contest.authorized_finalizers.contains(&ctx.accounts.authority.key()),
            KotsError::UnauthorizedFinalizer
        );
        
        require!(contest.is_paused, KotsError::ContestNotPaused);
        
        contest.is_paused = false;
        contest.pause_timestamp = 0;
        contest.pause_reason = [0; MAX_STRING_LENGTH];
        
        msg!("▶️ Contest resumed by {}", ctx.accounts.authority.key());
        Ok(())
    }

    /// Upload meme to multiple contests with blockchain-native receipt verification
    /// Supports registration to 1-10 contests simultaneously via contest-agnostic PDA
    ///
    /// # meme_id Format (client-supplied u64)
    /// - High 48 bits: Unix timestamp in seconds (provides temporal ordering)
    /// - Low 16 bits: Client-generated nonce (ensures uniqueness within same second)
    /// - Example: `(timestamp << 16) | nonce`
    /// 
    /// # Multi-Contest Architecture
    /// - PDA seeds: [MEME_ACCOUNT_SEED, uploader, meme_id] (contest-agnostic)
    /// - MemeAccount stores array of contest_labels (up to 10)
    /// - Cost: (0.29 KOTS × tier) + (active_contest_count × 0.01 KOTS)
    /// - Note: Zero-padded labels are NOT counted in cost calculation
    /// 
    /// # Collision Handling
    /// - If PDA already exists (same meme_id), transaction fails deterministically
    /// - Client should retry with different nonce value
    /// - Timestamp must be within ±30 seconds of blockchain time
    ///
    /// # Security
    /// - Timestamp validated against Clock::unix_timestamp (±30s window)
    /// - Authoritative upload_timestamp derived from Clock, not meme_id
    /// - Nonce prevents race conditions for concurrent uploads
    /// - All contest labels validated to ensure contests exist and are active
    pub fn upload_meme_with_verification(
        ctx: Context<UploadMemeWithVerification>,
        contest_labels: Vec<[u8; 32]>,
        ipfs_hash: [u8; 64],
        meme_id: u64,
        payment_verification: PaymentVerification,
        storage_tier: StorageTier,
    ) -> Result<()> {
        // BLACKLIST GATE (2026-07-23). contest-management had NO blacklist check of any kind, so a blacklisted
        // account could keep publishing the content it was blacklisted for.
        kots_common::cpi::require_not_blacklisted(
            &ctx.accounts.blacklist_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
            &ctx.accounts.uploader.key(),
        )?;

        // Count only non-zero (active) contest labels, excluding zero-padding
        let active_count = contest_labels.iter().filter(|label| **label != [0u8; 32]).count() as u8;
        let total_array_size = contest_labels.len() as u8;
        
        msg!("📤 Uploading meme to {} active contest(s) (array size: {}, ID: {})", 
             active_count, total_array_size, meme_id);
        
        // Validate active contest count is within bounds (1-10 contests)
        require!(active_count >= 1 && active_count <= 10, KotsError::InvalidParameter);
        require!(total_array_size == 10, KotsError::InvalidParameter); // Array must always be padded to 10
        
        // Validate IPFS hash format (BPF-compatible)
        kots_common::validate_ipfs_hash_bytes(&ipfs_hash)?;
        
        // Validate timestamp portion of meme_id is within acceptable window
        // Extract high 48 bits as timestamp (or adjust bit layout as needed)
        let current_time = get_current_timestamp()?;
        let meme_timestamp = (meme_id >> 16) as i64; // High 48 bits for timestamp
        let time_diff = (current_time - meme_timestamp).abs();
        
        require!(
            time_diff <= MEME_ID_TIMESTAMP_WINDOW_SECONDS,
            KotsError::InvalidMemeTimestamp
        );
        
        // Validate all contests exist and are active
        // Note: First contest is validated by Anchor constraints in #[account]
        // Additional contests validated via contest_labels iteration in production
        // Multi-contest CPI validation can be added as enhancement for stricter checks
        
        // Calculate upload cost via Economics program CPI (using active count, not padded array size)
        let upload_cost = calculate_meme_upload_cost(&ctx, storage_tier.clone(), active_count)?;
        
        // Verify payment via Security program CPI
        verify_meme_upload_payment_local(&ctx, &payment_verification, upload_cost)?;
        
        // Process payment via Economics program CPI
        process_meme_upload_payment_local(&ctx, upload_cost)?;
        
        // Manual account creation (Anchor 0.29+ pattern)
        // Ensure account doesn't already exist
        require!(
            ctx.accounts.meme_account.data_is_empty(),
            KotsError::MemeAlreadyExists
        );
        
        let rent = Rent::get()?;
        let lamports_required = rent.minimum_balance(MEME_ACCOUNT_SPACE);
        
        // Store uploader key to extend its lifetime
        let uploader_key = ctx.accounts.uploader.key();
        
        // Derive contest-agnostic PDA seeds and bump (no contest_label in seeds)
        let seeds = &[
            MEME_ACCOUNT_SEED,
            uploader_key.as_ref(),
            &meme_id.to_le_bytes(),
        ];
        let (pda_address, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        
        require!(
            pda_address == ctx.accounts.meme_account.key(),
            KotsError::InvalidPDA
        );
        
        // Create account via System Program CPI
        let signer_seeds: &[&[&[u8]]] = &[&[
            MEME_ACCOUNT_SEED,
            uploader_key.as_ref(),
            &meme_id.to_le_bytes(),
            &[bump],
        ]];
        
        anchor_lang::system_program::create_account(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::CreateAccount {
                    from: ctx.accounts.uploader.to_account_info(),
                    to: ctx.accounts.meme_account.to_account_info(),
                },
                signer_seeds,
            ),
            lamports_required,
            MEME_ACCOUNT_SPACE as u64,
            ctx.program_id,
        )?;
        
        // Request treasury reimbursement for PDA creation (optional)
        let _ = request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.uploader.to_account_info(),
            lamports_required,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions { allow_creator_payment: false, ..InitOptions::default() },
        ); // Ignore treasury reimbursement errors - user already paid
        
        // Now manually write MemeAccount data to the account
        let current_time = get_current_timestamp()?;
        
        // ipfs_hash is already [u8; 64], matching the struct field size
        let hash_array = ipfs_hash;
        
        // Calculate cost breakdown
        #[cfg(feature = "idl-build")]
        let cost_breakdown = [0u8; 32]; // Simplified for IDL
        
        #[cfg(not(feature = "idl-build"))]
        let cost_breakdown = {
            let cost_metadata = CostMetadata {
                bandwidth_cost: (AVERAGE_MEME_SIZE_MB * BANDWIDTH_COST_PER_MB as f64) as u64,
                storage_cost: (AVERAGE_MEME_SIZE_MB * STORAGE_COST_PER_MB_MONTH as f64 * DEFAULT_STORAGE_DURATION_MONTHS as f64) as u64,
                server_cost: upload_cost / 10, // ~10% server overhead
                ipfs_provider_cost: upload_cost / 5, // ~20% IPFS costs
            };
            let mut cost_bytes = [0u8; 32];
            cost_bytes[0..8].copy_from_slice(&cost_metadata.bandwidth_cost.to_le_bytes());
            cost_bytes[8..16].copy_from_slice(&cost_metadata.storage_cost.to_le_bytes());
            cost_bytes[16..24].copy_from_slice(&cost_metadata.server_cost.to_le_bytes());
            cost_bytes[24..32].copy_from_slice(&cost_metadata.ipfs_provider_cost.to_le_bytes());
            cost_bytes
        };
        
        #[cfg(feature = "idl-build")]
        let tier = STORAGE_TIER_STANDARD;
        
        #[cfg(not(feature = "idl-build"))]
        let tier = storage_tier as u8;
        
        // Build MemeAccount struct with all contest labels
        let meme_data = MemeAccount {
            is_initialized: true,
            meme_id: meme_id.to_le_bytes(),
            owner: ctx.accounts.uploader.key(),
            contest_labels: {
                let mut labels = [[0u8; MAX_CONTEST_LABEL_LENGTH]; 10];
                // Copy all provided contest labels
                for (i, label) in contest_labels.iter().enumerate().take(10) {
                    labels[i] = *label;
                }
                labels
            },
            contest_count: active_count,
            ipfs_hash: hash_array,
            upload_timestamp: current_time,
            expiry_timestamp: current_time + (DEFAULT_STORAGE_DURATION_MONTHS as i64 * 30 * 24 * 60 * 60),
            storage_tier: tier,
            total_paid: upload_cost,
            extension_count: 0,
            bump,
            cost_breakdown,
        };
        
        // Manually serialize and write to account
        let mut account_data = ctx.accounts.meme_account.try_borrow_mut_data()?;
        let dst: &mut [u8] = &mut account_data;
        let mut writer = dst;
        meme_data.try_serialize(&mut writer)?;
        
        msg!("✅ Meme uploaded successfully - ID: {:?}, Cost: {} KOTS", 
             meme_id, upload_cost / TOKENS_PER_UNIT);
        
        Ok(())
    }


    /// Query meme expiration data for storage lifecycle management
    pub fn query_meme_expiration(
        ctx: Context<QueryMemeExpiration>,
        meme_id: [u8; 8],
    ) -> Result<MemeExpirationData> {
        let meme_account = &ctx.accounts.meme_account;
        
        require!(meme_account.is_initialized, KotsError::InvalidMemeId);
        require!(meme_account.meme_id == meme_id, KotsError::InvalidMemeId);
        
        let current_time = get_current_timestamp()?;
        let days_until_expiry = ((meme_account.expiry_timestamp - current_time) / (24 * 60 * 60)).max(0);
        
        Ok(MemeExpirationData {
            meme_id: meme_account.meme_id,
            upload_timestamp: meme_account.upload_timestamp,
            expiry_timestamp: meme_account.expiry_timestamp,
            storage_tier: StorageTier::from(meme_account.storage_tier), // Always convert u8 to enum
            days_remaining: days_until_expiry as u32,
            total_paid: meme_account.total_paid,
            extension_count: meme_account.extension_count,
            is_expired: current_time > meme_account.expiry_timestamp,
        })
    }

    /// Extend meme storage beyond default 3 months
    pub fn extend_meme_storage(
        ctx: Context<ExtendMemeStorage>,
        meme_id: [u8; 8],
        extension_months: u8,
        payment_verification: PaymentVerification,
    ) -> Result<()> {
        require!(extension_months > 0, KotsError::InvalidTokenAmount);
        require!(extension_months <= MAX_STORAGE_EXTENSION_MONTHS, KotsError::InvalidTokenAmount);
        
        // Validate and calculate costs before borrowing mutably
        let (total_extension_cost, extension_seconds) = {
            let meme_account = &ctx.accounts.meme_account;
            require!(meme_account.is_initialized, KotsError::InvalidMemeId);
            require!(meme_account.meme_id == meme_id, KotsError::InvalidMemeId);
            require!(meme_account.owner == ctx.accounts.owner.key(), KotsError::UnauthorizedAccess);
            
            // Calculate extension cost
            let monthly_cost = (AVERAGE_MEME_SIZE_MB * STORAGE_COST_PER_MB_MONTH as f64) as u64;
            let extension_cost = monthly_cost * extension_months as u64;
            
            // Apply storage tier multiplier - unify match logic for both compilation targets
            let tier_multiplier = match meme_account.storage_tier {
                STORAGE_TIER_STANDARD => STANDARD_STORAGE_MULTIPLIER,
                STORAGE_TIER_PREMIUM => PREMIUM_STORAGE_MULTIPLIER,
                STORAGE_TIER_ARCHIVE => ARCHIVE_STORAGE_MULTIPLIER,
                _ => STANDARD_STORAGE_MULTIPLIER,
            };
            let total_cost = extension_cost * tier_multiplier;
            let ext_seconds = (extension_months as i64) * 30 * 24 * 60 * 60;
            
            (total_cost, ext_seconds)
        };
        
        // Verify and process payments via CPI
        verify_storage_extension_payment_local(&ctx, &payment_verification, total_extension_cost)?;
        process_storage_extension_payment_local(&ctx, total_extension_cost)?;
        
        // Now update the meme account
        let meme_account = &mut ctx.accounts.meme_account;
        meme_account.expiry_timestamp += extension_seconds;
        meme_account.total_paid += total_extension_cost;
        meme_account.extension_count = meme_account.extension_count.saturating_add(1);
        
        msg!("📅 Storage extended by {} months - New expiry: {}", 
             extension_months, meme_account.expiry_timestamp);
        
        Ok(())
    }
}

// BPF-optimized implementation moved outside main function to reduce stack usage
fn finalize_contest_unlimited_impl(
    ctx: &mut Context<FinalizeContestUnlimited>,
    contest_label: &[u8; 32],
) -> Result<()> {
    msg!("🏁 Finalizing contest");
    
    // Validate contest can be finalized (read-only access)
    {
        let contest = &ctx.accounts.contest_state;
        require!(!contest.is_finalized, KotsError::ContestAlreadyFinalized);
        require!(is_contest_ended(contest.creation_timestamp, contest.duration_seconds)?, 
                KotsError::ContestNotEnded);
        require!(contest.participant_count >= contest.min_participants as u32, 
                KotsError::InsufficientParticipants);
        require!(contest.total_prize_pool > 0, KotsError::NoPrizeAmount);
    }
    
    // IDEMPOTENCY PATTERN: Execute all CPIs BEFORE marking as finalized
    // If any CPI fails, the entire transaction reverts and can be retried
    
    // Step 1: Get final scores from Voting program via CPI (no borrows active)
    let final_scores = get_final_scores(ctx, contest_label)?;
    
    // Step 2: Distribute prizes via Prize Distribution program CPI (no borrows active)
    // This creates WinnerRecord PDAs on-chain for each winner
    distribute_contest_prizes(ctx, contest_label, final_scores)?;
    
    // Step 3: ONLY mark contest as finalized AFTER all critical operations succeed
    // This ensures atomicity - either everything succeeds or transaction reverts
    let contest = &mut ctx.accounts.contest_state;
    contest.is_finalized = true;
    contest.finalization_timestamp = get_current_timestamp()?;
    contest.finalized_by = ctx.accounts.finalizer.key();
    contest.scoring_complete = true;
    
    // Setup auto distribution if enabled
    if contest.auto_distribution_enabled {
        contest.auto_distribution_timestamp = contest.finalization_timestamp + AUTO_DISTRIBUTION_DELAY_SECONDS;
    }
    
    msg!("✅ Contest finalized successfully");
    Ok(())
}

// ============================================================================
// ACCOUNT VALIDATION STRUCTURES
// ============================================================================

#[derive(Accounts)]
pub struct InitializeContestManagement<'info> {
    /// Program state PDA - Anchor creates it automatically
    #[account(
        init,
        payer = authority,
        seeds = [b"program_state"],
        bump,
        space = 256
    )]
    pub program_state: Account<'info, ProgramUpgradeState>,
    
    /// Treasury incoming account for reimbursement
    /// CHECK: Validated as treasury PDA in handler
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
    /// Treasury Operations program for optional reimbursement CPI
    /// CHECK: Program ID validated in handler
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct CreateContestUnlimited<'info> {
    /// Contest state PDA - Anchor creates it automatically
    #[account(
        init,
        payer = creator,
        seeds = [CONTEST_SEED, &contest_label],
        bump,
        space = CONTEST_STATE_SPACE
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    
    /// Treasury incoming account for reimbursement
    /// CHECK: Validated as treasury PDA in handler
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
    /// Treasury Operations program for funding CPI
    /// CHECK: Program ID validated in handler
    pub treasury_operations_program: UncheckedAccount<'info>,
    
    /// Contest management program (self)
    /// CHECK: Program ID validated in handler  
    pub contest_management_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
    /// EMERGENCY PAUSE GATE (2026-08-06): security-governance's EmergencyState PDA.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], meme_id: u64)]
pub struct JoinContestUnlimited<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    
    /// Contest state - must exist and be valid
    #[account(
        constraint = contest_state.is_active @ KotsError::ContestNotActive,
        constraint = !contest_state.is_finalized @ KotsError::ContestAlreadyFinalized
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    
    /// Participant state PDA - Anchor creates it automatically with canonical seeds
    /// Seeds: [PARTICIPANT_SEED, contest_label, user_pubkey]
    #[account(
        init,
        payer = user,
        seeds = [
            PARTICIPANT_SEED,
            &contest_label,
            user.key().as_ref()
        ],
        bump,
        space = PARTICIPANT_STATE_SPACE
    )]
    pub participant_state: Account<'info, ParticipantState>,
    
    /// Treasury incoming account that pays for PDA creation
    /// CHECK: Validated as treasury PDA in handler
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    
    /// User's token account for KOTS
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    /// Treasury Operations program for funding CPI
    /// CHECK: Program ID validated in handler
    pub treasury_operations_program: UncheckedAccount<'info>,
    
    /// Contest management program (self)
    /// CHECK: Program ID validated in handler  
    pub contest_management_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    /// BLACKLIST GATE (2026-07-23): security-governance's blacklist PDA.
    /// CHECK: owner + canonical-PDA + discriminator all verified inside
    /// kots_common::cpi::read_blacklist_status before a byte is read.
    pub blacklist_state: UncheckedAccount<'info>,
    /// EMERGENCY PAUSE GATE (2026-08-06): security-governance's EmergencyState PDA.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct FinalizeContestUnlimited<'info> {
    #[account(
        mut,
        seeds = [CONTEST_SEED, &contest_label],
        bump
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    #[account(mut)]
    pub finalizer: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct AddContestFinalizer<'info> {
    #[account(
        mut,
        seeds = [CONTEST_SEED, &contest_label],
        bump
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct PauseContest<'info> {
    #[account(
        mut,
        seeds = [CONTEST_SEED, &contest_label],
        bump
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct ResumeContest<'info> {
    #[account(
        mut,
        seeds = [CONTEST_SEED, &contest_label],
        bump
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_labels: Vec<[u8; 32]>, ipfs_hash: [u8; 64], meme_id: u64, payment_verification: PaymentVerification, storage_tier: StorageTier)]
pub struct UploadMemeWithVerification<'info> {
    #[account(mut)]
    pub uploader: Signer<'info>,
    
    /// First contest state - must exist and be valid (validates at least one contest)
    /// Additional contest validation happens in instruction handler
    #[account(
        constraint = contest_state.contest_label == contest_labels[0] @ KotsError::InvalidContestLabel,
        constraint = contest_state.is_active @ KotsError::ContestNotActive,
        constraint = !contest_state.is_finalized @ KotsError::ContestAlreadyFinalized,
        constraint = !contest_state.is_paused @ KotsError::ContestPaused
    )]
    pub contest_state: Account<'info, ContestCoreState>,
    
    /// Meme account PDA - contest-agnostic for multi-contest support
    /// Seeds: [MEME_ACCOUNT_SEED, uploader_pubkey, meme_id] (NO contest_label!)
    /// CHECK: Manually initialized in instruction handler
    #[account(
        mut,
        seeds = [
            MEME_ACCOUNT_SEED,
            uploader.key().as_ref(),
            &meme_id.to_le_bytes()
        ],
        bump
    )]
    pub meme_account: UncheckedAccount<'info>,
    
    /// Treasury incoming account that pays for PDA creation
    /// CHECK: Validated as treasury PDA in handler
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    
    /// User's token account for KOTS payment
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    /// Treasury Operations program for funding CPI
    /// CHECK: Program ID validated in handler
    pub treasury_operations_program: UncheckedAccount<'info>,
    
    /// Contest management program (self)
    /// CHECK: Program ID validated in handler  
    pub contest_management_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    /// BLACKLIST GATE (2026-07-23): security-governance's blacklist PDA.
    /// CHECK: owner + canonical-PDA + discriminator all verified inside
    /// kots_common::cpi::read_blacklist_status before a byte is read.
    pub blacklist_state: UncheckedAccount<'info>,
}


#[derive(Accounts)]
pub struct QueryMemeExpiration<'info> {
    /// Meme account to query - validated in handler
    pub meme_account: Account<'info, MemeAccount>,
}

#[derive(Accounts)]
pub struct ExtendMemeStorage<'info> {
    /// Meme account to extend - validated in handler
    #[account(
        mut,
        constraint = meme_account.is_initialized @ KotsError::UninitializedAccount
    )]
    pub meme_account: Account<'info, MemeAccount>,
    #[account(mut)]
    pub owner: Signer<'info>,
    /// Owner's token account for KOTS payment
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

fn validate_contest_creation_params(duration_seconds: i64, min_participants: u16, entry_fee: u64) -> Result<()> {
    require!(duration_seconds >= MIN_CONTEST_DURATION_SECONDS, KotsError::ContestDurationTooShort);
    require!(duration_seconds <= MAX_CONTEST_DURATION_SECONDS, KotsError::ContestDurationTooLong);
    require!(min_participants >= MIN_PARTICIPANTS_REQUIRED, KotsError::MinParticipantsTooLow);
    // BUGFIX (2026-07-23): a zero entry fee is LEGAL — contests are free to enter.
    //
    // This line previously read `require!(entry_fee >= MIN_ENTRY_FEE, ...)`, which
    // made every contest require a paid entry of at least 0.1 KOTS. That
    // contradicted three independent sources:
    //   * KOTS_TOKENOMICS §4b — "Users may OPTIONALLY pay an entry fee to
    //     participate competitively (free voting always available)."
    //   * the public FAQ — "Browsing, entering contests, and upvoting: free."
    //   * `join_contest_unlimited` itself, which already guards the charge with
    //     `if required_stake > 0 { process_entry_fee(..) }` — code written for an
    //     optional fee, reachable only for contests this validator refused to
    //     create.
    //
    // It also failed 29 tests in the 2026-07-24 run, 13 of which assert that a
    // free contest can be created. Those tests were right and the program wrong.
    //
    // MIN_ENTRY_FEE is retained as an ANTI-DUST floor: if a creator charges at
    // all, the fee must be meaningful. Free or ≥ 0.1 KOTS; nothing in between.
    require!(
        entry_fee == 0 || entry_fee >= MIN_ENTRY_FEE,
        KotsError::EntryFeeTooLow
    );
    require!(entry_fee <= MAX_ENTRY_FEE, KotsError::EntryFeeTooHigh);
    Ok(())
}

fn is_contest_ended(creation_timestamp: i64, duration_seconds: i64) -> Result<bool> {
    let current_time = get_current_timestamp()?;
    let end_time = creation_timestamp
        .checked_add(duration_seconds)
        .ok_or(KotsError::ArithmeticOverflow)?;
    Ok(current_time >= end_time)
}

// ============================================================================
// MEME UPLOAD HELPER FUNCTIONS
// ============================================================================

/// Validate meme ID is non-zero (basic sanity check)
fn validate_meme_id(meme_id: u64) -> Result<()> {
    require!(meme_id != 0, KotsError::InvalidMemeId);
    Ok(())
}

/// Calculate meme upload cost for multi-contest registration
/// 
/// Cost Structure:
/// - Base storage cost (0.29 KOTS) × storage_tier_multiplier
/// - Plus (contest_count × 0.01 KOTS) for multi-contest registration
/// 
/// Examples:
/// - 1 contest, Standard: 0.30 KOTS
/// - 5 contests, Standard: 0.34 KOTS
/// - 10 contests, Premium: 0.68 KOTS
fn calculate_meme_upload_cost(
    _ctx: &Context<UploadMemeWithVerification>,
    storage_tier: StorageTier,
    contest_count: u8,
) -> Result<u64> {
    // Base infrastructure cost (storage, hosting, bandwidth)
    let base_cost = FIXED_MEME_UPLOAD_COST;
    
    // Apply storage tier multiplier
    let tier_multiplier = match storage_tier {
        StorageTier::Standard => STANDARD_STORAGE_MULTIPLIER,
        StorageTier::Premium => PREMIUM_STORAGE_MULTIPLIER,
        StorageTier::Archive => ARCHIVE_STORAGE_MULTIPLIER,
    };
    
    let storage_cost = base_cost
        .checked_mul(tier_multiplier)
        .ok_or(KotsError::ArithmeticOverflow)?;
    
    // Add per-contest registration fees
    let registration_cost = CONTEST_REGISTRATION_FEE
        .checked_mul(contest_count as u64)
        .ok_or(KotsError::ArithmeticOverflow)?;
    
    let total_cost = storage_cost
        .checked_add(registration_cost)
        .ok_or(KotsError::ArithmeticOverflow)?;
    
    msg!("💰 Cost Calculation:");
    msg!("   Storage: {} KOTS (tier: {:?})", storage_cost / TOKENS_PER_UNIT, storage_tier);
    msg!("   Registration: {} contests × 0.01 = {} KOTS", contest_count, registration_cost / TOKENS_PER_UNIT);
    msg!("   Total: {} KOTS", total_cost / TOKENS_PER_UNIT);
    
    Ok(total_cost)
}

/// Verify meme upload payment via Security program CPI
fn verify_meme_upload_payment_local(
    _ctx: &Context<UploadMemeWithVerification>,
    payment_verification: &PaymentVerification,
    expected_amount: u64,
) -> Result<()> {
    // Validate payment verification structure
    require!(payment_verification.payment_type == PaymentType::MemeUpload, KotsError::InvalidPaymentReceipt);
    require!(payment_verification.expected_amount >= expected_amount, KotsError::InsufficientPayment);
    
    // SECURITY GAP (surfaced 2026-08-03) — verification here is STRUCTURAL ONLY.
    // `payment_verification` is supplied by the CLIENT; the two requires above
    // confirm it merely CLAIMS the right type and a large-enough amount. Nothing
    // proves a payment happened: `transaction_signature` is never verified (the
    // security-governance verifier it was meant to CPI is itself a
    // `signature_valid = true` stub), and `PaymentVerification` carries no
    // timestamp, so the "verification window" cannot be enforced either. Do NOT
    // read this log as "payment confirmed". Closing it needs a real signature
    // proof or an on-chain transfer check — recorded in
    // KOTS-SECURITY-GOVERNANCE-PLAN (Work log) for an owner decision.
    msg!(
        "🔒 Payment receipt structurally valid (type + claimed amount); \
         signature/transfer NOT verified — cost {}",
        expected_amount
    );

    Ok(())
}

/// Process meme upload payment via Economics program CPI
fn process_meme_upload_payment_local(
    _ctx: &Context<UploadMemeWithVerification>,
    amount: u64,
) -> Result<()> {
    // SECURITY GAP (surfaced 2026-08-03) — this COLLECTS NOTHING. `_ctx` is
    // unused and there is no token transfer: `upload_cost` is computed in the
    // caller and then never charged, so meme uploads are effectively free of the
    // KOTS cost (only Solana rent for the meme account is paid). A real
    // implementation must move `amount` from the uploader's token account into
    // the treasury per the distribution model. Left as a no-op rather than
    // implemented blind (fund-routing must be verified on-toolchain); recorded in
    // KOTS-SECURITY-GOVERNANCE-PLAN (Work log).
    msg!(
        "⚠️ Payment collection NOT WIRED — {} not charged (see plan Work log)",
        amount
    );

    Ok(())
}

/// Verify storage extension payment via Security program CPI
fn verify_storage_extension_payment_local(
    _ctx: &Context<ExtendMemeStorage>,
    payment_verification: &PaymentVerification,
    expected_amount: u64,
) -> Result<()> {
    require!(payment_verification.payment_type == PaymentType::StorageExtension, KotsError::InvalidPaymentReceipt);
    require!(payment_verification.expected_amount >= expected_amount, KotsError::InsufficientPayment);
    
    // SECURITY GAP (surfaced 2026-08-03): structural-only, same as
    // `verify_meme_upload_payment_local` — client-supplied receipt, no signature
    // or transfer verified. Not "payment confirmed".
    msg!(
        "🔒 Extension receipt structurally valid; signature/transfer NOT verified — cost {}",
        expected_amount
    );
    Ok(())
}

/// Process storage extension payment via Economics program CPI
fn process_storage_extension_payment_local(
    _ctx: &Context<ExtendMemeStorage>,
    amount: u64,
) -> Result<()> {
    // SECURITY GAP (surfaced 2026-08-03): collects nothing (no transfer); the
    // extension cost is never charged. Same follow-on as
    // `process_meme_upload_payment_local`.
    msg!(
        "⚠️ Extension payment collection NOT WIRED — {} not charged (see plan Work log)",
        amount
    );
    Ok(())
}