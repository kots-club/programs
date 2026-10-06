// kots-contracts/shared/kots-common/src/state.rs
//! KOTS State Structures
//! 
//! All account state structures shared across programs.
//! These structures are preserved exactly from the original program
//! to maintain complete compatibility.

use anchor_lang::prelude::*;
use super::constants::*;

/// Program type for cross-program calls
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub enum ProgramType {
    // ⚠ TWO ENUMS NAMED ProgramType EXIST IN THIS CRATE (2026-07-23).
    //
    // This one, and `kots_common::upgrade::ProgramType`, WITH DIFFERENT ORDERINGS:
    //     variant              state   upgrade
    //     ContestManagement      0        1
    //     FeeManagement          1        4
    //     SecurityGovernance     4        0
    // (the other four happen to agree)
    //
    // Both reach the crate root: lib.rs does `pub use upgrade::*` (glob) and
    // `pub use state::{.. ProgramType ..}` (explicit). Rust gives the explicit
    // re-export precedence, so a bare `kots_common::ProgramType` resolves HERE —
    // by a shadowing rule most readers will not have in mind. Delete the explicit
    // re-export and every `as u8` in the workspace silently changes value.
    //
    // Today the two are used by disjoint struct families — this one by
    // ProgramUpgradeState, the other by UpgradeProposal / UpgradeRegistry and
    // their events — so no byte written under one is read under the other. That
    // separation is an accident of current usage, not a guarantee. The pinning
    // tests at the bottom of this file exist to make a reorder loud.

    ContestManagement,
    FeeManagement,
    VotingHieroglyph, 
    EconomicsDistribution,
    SecurityGovernance,
    PrizeDistribution,
    ProgressiveJackpot,
}

impl Default for ProgramType {
    fn default() -> Self {
        ProgramType::SecurityGovernance
    }
}

// ============================================================================  
// INTERACTION TYPES AND HELPER STRUCTURES
// ============================================================================

/// Interaction types for hieroglyph voting
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub struct InteractionType {
    pub id: u32,
    pub name: [u8; 32], // Fixed-size for BPF compatibility
    // SIGNED since de-chancing (SCORING-MODEL.md): Downvote is -5. Borsh
    // layout width is unchanged (8 bytes) but the sign interpretation is a
    // client-visible change; regenerate IDL clients.
    pub scoring_weight: i64,
}

impl InteractionType {
    pub fn from_u32(id: u32) -> Result<Self> {
        // The `names` array that used to live here is GONE. It disagreed with
        // voting-hieroglyph's emoji table (name "Love" beside symbol "😂") and
        // with everything off-chain. One table now: hieroglyphs.rs.
        let def = crate::hieroglyphs::get(id as u8)
            .ok_or(super::errors::KotsError::InvalidMemeId)?;

        let mut name_bytes = [0u8; 32];
        let bytes = def.name.as_bytes();
        let len = bytes.len().min(32);
        name_bytes[..len].copy_from_slice(&bytes[..len]);

        Ok(InteractionType {
            id,
            name: name_bytes,
            scoring_weight: def.meme_delta,
        })
    }

    /// Get hieroglyph name as byte slice for BPF compatibility
    pub fn name_bytes(&self) -> &[u8] {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        &self.name[..end]
    }
    
    pub fn get_scoring_weight(&self) -> i64 {
        self.scoring_weight
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Copy, PartialEq)]
pub struct InteractionRecord {
    pub interaction_type: u32,
    pub timestamp: i64,
    pub voter_wallet: Pubkey,
    pub cost_paid: u64,
    pub is_premium_user: bool,
}

impl Default for InteractionRecord {
    fn default() -> Self {
        Self {
            interaction_type: 0,
            timestamp: 0,
            voter_wallet: Pubkey::default(),
            cost_paid: 0,
            is_premium_user: false,
        }
    }
}

// ============================================================================
// ACCOUNT STORAGE FOR LARGE DATA (BPF STACK-SAFE)
// ============================================================================

/// IDL-compatible structure for participant scores
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct ParticipantScore {
    pub participant: Pubkey,
    pub score: u64,
}

impl Default for ParticipantScore {
    fn default() -> Self {
        Self {
            participant: Pubkey::default(),
            score: 0,
        }
    }
}

/// Participant score storage account - one account per 100 participants
#[account]
#[derive(Debug)]
pub struct ParticipantScoreAccount {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub account_number: u32, // Which account in the series (0, 1, 2...)
    pub participant_scores: [ParticipantScore; MAX_PARTICIPANT_SCORES_PER_ACCOUNT],
    pub score_count: u16, // Actual number of scores in this account
    pub last_updated: i64,
}

/// Blacklist storage account - one account per 250 wallets  
#[account]
#[derive(Debug)]
pub struct BlacklistAccountStorage {
    pub is_initialized: bool,
    pub account_number: u32, // Which blacklist account (0, 1, 2...)
    pub blacklisted_wallets: [Pubkey; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
    pub blacklist_reasons: [[u8; MAX_REASON_LENGTH]; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
    pub blacklist_timestamps: [i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
    pub blacklist_count: u16, // Actual entries in this account
    pub last_updated: i64,
}

/// Account storage for large data structures (BPF stack-safe)

// ============================================================================
// CONTEST MANAGEMENT STATE STRUCTURES
// ============================================================================

/// Core contest state - only metadata, no participant Vec for unlimited scale
#[account]
#[derive(Debug)]
pub struct ContestCoreState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub creator: Pubkey,
    pub creation_timestamp: i64,
    pub duration_seconds: i64,
    pub min_participants: u16,
    pub entry_fee: u64,
    pub total_prize_pool: u64,
    pub participant_count: u32, // Counter only, not Vec
    pub is_finalized: bool,
    pub finalization_timestamp: i64,
    pub finalized_by: Pubkey,
    pub total_interactions: u64,
    pub scoring_complete: bool,
    pub is_paused: bool,
    pub is_active: bool,
    pub pause_timestamp: i64,
    pub pause_reason: [u8; MAX_STRING_LENGTH],
    pub vault_authority: Pubkey,
    pub vault_bump: u8,
    pub auto_distribution_enabled: bool,
    pub auto_distribution_timestamp: i64,
    pub peak_interaction_rate: f64,
    pub most_popular_interaction: u32,
    pub total_unique_voters: u32,
    pub authorized_finalizers: [Pubkey; MAX_AUTHORIZED_FINALIZERS], // Fixed-size array for BPF
    pub authorized_finalizers_count: u8, // Track actual count
}

impl Default for ContestCoreState {
    fn default() -> Self {
        Self {
            is_initialized: false,
            contest_label: [0u8; MAX_CONTEST_LABEL_LENGTH],
            creator: Pubkey::default(),
            creation_timestamp: 0,
            duration_seconds: 0,
            min_participants: 0,
            entry_fee: 0,
            total_prize_pool: 0,
            participant_count: 0,
            is_finalized: false,
            finalization_timestamp: 0,
            finalized_by: Pubkey::default(),
            total_interactions: 0,
            scoring_complete: false,
            is_paused: false,
            is_active: true,
            pause_timestamp: 0,
            pause_reason: [0; MAX_STRING_LENGTH],
            vault_authority: Pubkey::default(),
            vault_bump: 0,
            auto_distribution_enabled: false,
            auto_distribution_timestamp: 0,
            peak_interaction_rate: 0.0,
            most_popular_interaction: 0,
            total_unique_voters: 0,
            authorized_finalizers: [Pubkey::default(); MAX_AUTHORIZED_FINALIZERS],
            authorized_finalizers_count: 0,
        }
    }
}

/// Individual participant account - one PDA per participant
#[account]
#[derive(Debug)]
pub struct ParticipantState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub user_wallet: Pubkey,
    pub meme_id: u64,
    pub ipfs_hash: [u8; MAX_IPFS_HASH_LENGTH],
    pub entry_timestamp: i64,
    pub join_timestamp: i64,
    pub current_score: i32,
    pub total_votes_received: u64,
    pub is_disqualified: bool,
    pub stake_amount: u64, // Sybil protection stake
    pub total_interactions_received: u64,
    pub unique_voters: u32,
    pub final_score: u64,
    pub prize_position: u32,
    pub prize_amount: u64,
    pub prize_claimed: bool,
    pub claim_timestamp: i64,
    // Scoring components
    pub base_score: u64,
    pub time_bonus: u64,
    pub diversity_bonus: u64,
    pub consistency_bonus: u64,
    pub premium_bonus: u64,
    // Analytics
    pub interaction_velocity: f64,
    pub dominant_interaction_type: u32,
    pub interaction_breakdown: [u32; 32], // Count per interaction type
}

// ============================================================================
// VOTING & HIEROGLYPH STATE STRUCTURES
// ============================================================================

/// Vote tracking per meme per hieroglyph - COST-EFFECTIVE SCALING
#[account]
#[derive(Debug)]
pub struct VoteTrackerState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub meme_id: u64,
    pub interaction_type: u32, // 0-31 for 32 hieroglyphs
    pub hieroglyph_type: u32, // Alias for interaction_type (backward compatibility)
    pub voters: [Pubkey; MAX_VOTERS_PER_TRACKER], // 125 voters max (BPF limit)
    pub voter_count: u16, // Actual individual voters tracked
    pub total_votes: u32, // TOTAL votes including those beyond individual tracking
    pub total_value: u64, // Total monetary value of all votes
    pub votes_beyond_limit: u32, // Votes after 125 individual voters (count-only)
    pub individual_tracking_full: bool, // True when 125 individual slots used
    pub creation_timestamp: i64,
    pub last_vote_timestamp: i64,
    pub score_weight: u64, // Current scoring weight for this interaction
}

/// Master tracker for managing unlimited vote sequences
#[account]
#[derive(Debug)]
pub struct VoteSequenceMaster {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub meme_id: u64,
    pub interaction_type: u32,
    pub total_sequences: u32, // Number of sequence accounts created
    pub total_unique_voters: u32, // Total across all sequences
    pub total_votes: u32, // Global vote count
    pub current_active_sequence: u32, // Which sequence to add next voter to
    pub sequence_accounts: [Pubkey; MAX_VOTE_TRACKER_SEQUENCES], // Track all sequence accounts
    pub last_sequence_created: i64,
}

/// Batch of interactions for efficient storage
#[account]
#[derive(Debug)]
pub struct InteractionBatchState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub meme_id: u64, // Target meme for this batch
    pub batch_number: u32,
    pub interactions: [InteractionRecord; MAX_INTERACTIONS_PER_BATCH], // Fixed-size for BPF
    pub interaction_count: u32, // Track actual interactions
    pub batch_timestamp: i64,
    pub total_score_awarded: u64,
}


/// User rate limiting state
#[account]
#[derive(Debug)]
pub struct UserRateLimitState {
    pub is_initialized: bool,
    pub user_wallet: Pubkey,
    pub daily_contest_count: u8,
    pub last_contest_date: i64,
    pub minute_interaction_count: u16,
    pub last_interaction_minute: i64,
    pub last_interaction_timestamp: i64,
    pub total_interactions_today: u32,
    pub reputation_score: u64,
    pub is_suspended: bool,
    pub suspension_end: i64,
}

// ============================================================================
// ECONOMICS & DISTRIBUTION STATE STRUCTURES
// ============================================================================

/// Fee distribution configuration and state - Enhanced dual distribution system
#[account]
#[derive(Debug)]
pub struct FeeDistributionState {
    pub is_initialized: bool,
    // Wallet addresses
    pub jackpot_pool: Pubkey,
    pub liquidity_pool: Pubkey,
    pub treasury_wallet: Pubkey,
    pub burn_address: Pubkey,
    pub development_fund: Pubkey,
    
    // Trading tax distribution percentages (in basis points)
    pub trading_jackpot_bps: u64,    // 50% (5000 BPS)
    pub trading_liquidity_bps: u64,  // 37% (3700 BPS)
    pub trading_treasury_bps: u64,   // 7% (700 BPS)
    pub trading_burn_bps: u64,       // 3% (300 BPS)
    
    // Contest revenue distribution percentages (in basis points)
    pub contest_treasury_bps: u64,     // 50% (5000 BPS)
    pub contest_development_bps: u64,  // 30% (3000 BPS)
    pub contest_jackpot_bps: u64,      // 10% (1000 BPS)
    pub contest_burn_bps: u64,         // 10% (1000 BPS)
    
    // Legacy distribution fields for backward compatibility
    pub jackpot_distribution_bps: u64,
    pub specified_wallet_distribution_bps: u64,
    pub burn_distribution_bps: u64,
    pub dev_distribution_bps: u64,
    
    // Statistics
    pub total_fees_distributed: u64,
    pub total_contest_revenue_distributed: u64,
    pub last_distribution_timestamp: i64,
    pub distribution_count: u64,
}

/// Contest vault for prize pool management
#[account]
#[derive(Debug)]
pub struct ContestVaultState {
    pub is_initialized: bool,
    pub contest_label: [u8; MAX_CONTEST_LABEL_LENGTH],
    pub vault_authority: Pubkey,
    pub vault_bump: u8,
    pub total_prize_pool: u64,
    pub reserved_amounts: u64, // Reserved for guaranteed prizes
    pub available_for_distribution: u64,
    pub distribution_complete: bool,
    pub auto_distribution_executed: bool,
    pub distribution_timestamp: i64,
    // Rate limiting fields for treasure hunt distributions
    pub daily_transfer_count: u32,
    pub last_daily_reset: i64,
    pub lifetime_distributed: u64,
    pub last_transfer_timestamp: i64,
}

/// Progressive jackpot state (52-week cycle)
#[account]
#[derive(Debug)]
pub struct ProgressiveJackpotState {
    pub is_initialized: bool,
    pub current_cycle_week: u8, // 1-52
    pub cycle_start_timestamp: i64,
    pub total_jackpot_amount: u64,
    pub weekly_contributions: [u64; 52],
    pub last_winner: Option<Pubkey>,
    pub last_win_timestamp: i64,
    pub total_cycles_completed: u32,
}

// ============================================================================
// SECURITY & GOVERNANCE STATE STRUCTURES  
// ============================================================================

/// Enhanced gas oracle with multiple authorities
#[account]
#[derive(Debug)]
pub struct EnhancedGasOracleState {
    pub is_initialized: bool,
    pub current_price_lamports: u64,
    pub authority1: Pubkey,
    pub authority2: Pubkey,
    pub authority3: Pubkey,
    // Consensus tracking
    pub authority1_last_price: u64,
    pub authority2_last_price: u64,
    pub authority3_last_price: u64,
    pub authority1_last_update: i64,
    pub authority2_last_update: i64,
    pub authority3_last_update: i64,
    pub consensus_price: u64,
    pub consensus_timestamp: i64,
    pub daily_price_change_bps: i64, // Can be negative
    pub last_daily_reset: i64,
}

/// Blacklist management state
#[account]
#[derive(Debug)]
pub struct BlacklistState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub moderator: Pubkey,
    pub blacklisted_wallets: [Pubkey; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // BPF fixed-size
    pub blacklist_reasons: [[u8; MAX_REASON_LENGTH]; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // BPF byte arrays
    pub blacklist_timestamps: [i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // Fixed-size
    pub blacklist_count: u16, // Track actual entries
    pub total_blacklisted: u32,
    pub last_update_timestamp: i64,
}

impl BlacklistState {
    /// Check if a wallet is blacklisted.
    ///
    /// ⚠ REWRITTEN 2026-07-29 (O41). This used to iterate `0..blacklist_count`.
    /// `add_to_blacklist` never incremented `blacklist_count` — it maintained only
    /// `total_blacklisted` — so the loop ran `0..0` and **this function always
    /// returned false**. A security control that reported "not blacklisted" for
    /// every wallet, including ones it had just been told to block, and did so
    /// silently because the add path returned `Ok`.
    ///
    /// The counter is fixed too, but this no longer depends on it. Scanning the
    /// array is authoritative, costs at most MAX_BLACKLISTED_WALLETS_PER_ACCOUNT
    /// comparisons, and cannot be defeated by bookkeeping drift. A control should
    /// not fail open because a counter is wrong.
    ///
    /// Empty slots hold `Pubkey::default()`, so that address is excluded
    /// explicitly — otherwise querying it would match every unused slot.
    pub fn is_blacklisted(&self, wallet: &Pubkey) -> bool {
        if *wallet == Pubkey::default() {
            return false;
        }
        self.blacklisted_wallets.iter().any(|w| w == wallet)
    }
}

/// Emergency pause state
#[account]
#[derive(Debug)]
pub struct EmergencyState {
    pub is_initialized: bool,
    pub emergency_authority: Pubkey,
    pub is_paused: bool,
    pub pause_reason: [u8; MAX_STRING_LENGTH], // BPF-compatible byte array
    pub pause_timestamp: i64,
    pub pause_duration: i64,
    pub authorized_emergency_operations: [[u8; MAX_STRING_LENGTH]; 10], // Fixed emergency ops
    pub emergency_operations_count: u8, // Track actual operations
    pub total_emergency_activations: u32,
}

/// Audit trail for compliance
#[account]
#[derive(Debug)]
pub struct AuditState {
    pub is_initialized: bool,
    pub audit_authority: Pubkey,
    pub events: [AuditEvent; 5],  // BPF-safe: ~1KB audit trail
    pub event_count: u32, // Track actual events
    pub total_events: u64,
    pub last_event_timestamp: i64,
}

/// Role and permission management state
#[account]
#[derive(Debug)]
pub struct RoleState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub role_level: u8, // 0=User, 1=Moderator, 2=Admin, 3=SuperAdmin, 4=Gov
    pub permissions: u64, // Bitfield for permissions
    pub assigned_by: Pubkey,
    pub assigned_timestamp: i64,
    pub last_action_timestamp: i64,
}

/// Staking account for governance participation
#[account]
#[derive(Debug)]
pub struct StakingState {
    pub is_initialized: bool,
    pub staker: Pubkey,
    pub tokens_staked: u64,
    pub staking_timestamp: i64,
    pub last_reward_claim: i64,
    pub total_rewards_earned: u64,
    pub vote_weight: u64,
}

/// Global staking registry for tracking total stakes
#[account]
#[derive(Debug)]
pub struct StakingRegistry {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub total_staked: u64,
    pub total_stakers: u32,
    pub minimum_stake: u64,
    pub last_update_timestamp: i64,
}

/// Individual audit event
#[derive(AnchorSerialize, AnchorDeserialize, Debug, Clone)]
pub struct AuditEvent {
    pub event_type: [u8; MAX_STRING_LENGTH], // BPF byte array
    pub user: Pubkey,
    pub program: Pubkey,
    pub details: [u8; MAX_STRING_LENGTH], // BPF byte array
    pub timestamp: i64,
    pub block_height: u64,
}

/// Program upgrade state for versioning
/// Per-program bootstrap record: who owns the program and what version it is.
///
/// DIVERGENCE FIXED (2026-07-23): `program_type` was declared `ProgramType` here
/// while ALL SIX programs that actually write this account declare it `u8` and
/// write `ProgramType::X as u8`. Borsh gives a fieldless enum the same one byte
/// as a u8, so the layout matched — but reading a byte outside the enum's range
/// as `ProgramType` is a deserialization ERROR, not a wrong value. This copy is
/// currently shadowed by each program's own definition, so nothing was broken;
/// the first caller to use the shared type would have found out the hard way.
///
/// Declared `u8` to match what is actually written. Validate the range at the
/// point of use, where a bad value can be reported, rather than at deserialize
/// time, where it just fails the whole instruction.
#[account]
#[derive(Debug)]
pub struct ProgramUpgradeState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    /// `ProgramType as u8`. See the note above on why this is not the enum.
    pub program_type: u8,
    pub version: u16,
    pub initialization_timestamp: i64,
}

// ============================================================================
// PDA SEED CONSTANTS (UPDATED FOR CLIENT COMPATIBILITY)
// ============================================================================

pub const CONTEST_SEED: &[u8] = b"contest_v4";
pub const PARTICIPANT_SEED: &[u8] = b"participant_v4";
pub const VAULT_SEED: &[u8] = b"vault";
/// SECURITY (2026-07-23): seed for the per-program CPI authority PDA. Every KOTS
/// program that calls a privileged treasury instruction signs with
/// `find_program_address([CPI_AUTHORITY_SEED], &own_program_id)` via
/// `invoke_signed`. Treasury re-derives it from the claimed caller id, so the
/// caller's identity is proven by signature instead of asserted in an argument.
pub const CPI_AUTHORITY_SEED: &[u8] = b"kots_cpi_authority";
pub const VOTE_TRACKER_SEED: &[u8] = b"vote_tracker";
pub const VOTE_SEQUENCE_MASTER_SEED: &[u8] = b"vote_master";
pub const VOTE_SEQUENCE_SEED: &[u8] = b"vote_seq";
pub const INTERACTION_BATCH_SEED: &[u8] = b"interaction_batch";
pub const DISTRIBUTION_SEED: &[u8] = b"distribution";
pub const PRIZE_POOL_SEED: &[u8] = b"prize_pool";
pub const GAS_ORACLE_SEED: &[u8] = b"gas_oracle_v4";
pub const BLACKLIST_SEED: &[u8] = b"blacklist_v4";
pub const EMERGENCY_SEED: &[u8] = b"emergency_state_v4";

// ── CANONICAL GOVERNANCE PDA DERIVATION (seed unification, 2026-08-03) ──────
//
// security-governance CREATES BlacklistState and EmergencyState with a trailing
// b"v4" sub-seed on top of the *_v4 base constants — [BLACKLIST_SEED, b"v4"] and
// [EMERGENCY_SEED, b"v4"] — and its handlers ENFORCE those addresses
// (require_keys_eq! in initialize_blacklist / emergency_pause; Anchor
// `seeds = [...]` on AddToBlacklist / RemoveFromBlacklist / EmergencyResume).
// Every external client
// (chain-sidecar TS, kots-console codec, tests/bootstrap.rs,
// tests/emergency_and_blacklist.rs, tests/security_regressions.rs) derives the
// same two-seed address. The shared readers in cpi.rs and the devnet harness
// were the only two call sites deriving with the base seed alone — an address
// NOTHING ever creates — so every read failed closed with UnauthorizedCaller
// and every gated instruction (join, batch vote, winner claim) was permanently
// bricked while the emergency pause halted nothing.
//
// These arrays/helpers are now the ONLY sanctioned derivation. The doubled
// "v4" ("..._v4" + "v4") is cosmetic debt, not a bug: PDA derivation
// concatenates seed bytes, so changing EITHER side changes the on-chain
// address. Do not "clean it up" without a deliberate address migration — the
// lock test below will fail loudly if the bytes move.
pub const GOVERNANCE_SUB_SEED: &[u8] = b"v4";
pub const BLACKLIST_STATE_SEEDS: [&[u8]; 2] = [BLACKLIST_SEED, GOVERNANCE_SUB_SEED];
pub const EMERGENCY_STATE_SEEDS: [&[u8]; 2] = [EMERGENCY_SEED, GOVERNANCE_SUB_SEED];

/// Canonical address of security-governance's BlacklistState PDA.
pub fn blacklist_state_address(security_governance_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&BLACKLIST_STATE_SEEDS, security_governance_id)
}

/// Canonical address of security-governance's EmergencyState PDA.
pub fn emergency_state_address(security_governance_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&EMERGENCY_STATE_SEEDS, security_governance_id)
}

#[cfg(test)]
mod governance_seed_lock {
    use super::*;

    /// Address tripwire: PDA derivation concatenates seed bytes, so these exact
    /// byte strings ARE the on-chain addresses. Any edit that moves them is an
    /// address migration and must be done deliberately, not as cleanup.
    #[test]
    fn governance_pda_seed_bytes_are_locked() {
        assert_eq!(
            BLACKLIST_STATE_SEEDS.concat(),
            b"blacklist_v4v4".to_vec(),
            "BlacklistState PDA seed bytes moved — this changes the on-chain address"
        );
        assert_eq!(
            EMERGENCY_STATE_SEEDS.concat(),
            b"emergency_state_v4v4".to_vec(),
            "EmergencyState PDA seed bytes moved — this changes the on-chain address"
        );
    }
}

pub const AUDIT_SEED: &[u8] = b"audit";
pub const RATE_LIMIT_SEED: &[u8] = b"rate_limit";
pub const PROGRESSIVE_JACKPOT_SEED: &[u8] = b"progressive_jackpot";
pub const MEME_STORAGE_SEED: &[u8] = b"meme_storage";

// ============================================================================
// MEME UPLOAD & STORAGE MANAGEMENT STRUCTURES
// ============================================================================

/// Storage tier enumeration for different storage quality levels
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Copy)]
pub enum StorageTier {
    Standard,  // Base 3-month storage
    Premium,   // Extended storage with faster access
    Archive,   // Long-term archive storage
}

impl Default for StorageTier {
    fn default() -> Self {
        StorageTier::Standard
    }
}

impl From<u8> for StorageTier {
    fn from(value: u8) -> Self {
        match value {
            0 => StorageTier::Standard,
            1 => StorageTier::Premium,
            2 => StorageTier::Archive,
            _ => StorageTier::Standard, // Default to Standard for unknown values
        }
    }
}

/// Cost breakdown metadata for transparency
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CostMetadata {
    pub bandwidth_cost: u64,      // Cost for initial upload bandwidth
    pub storage_cost: u64,        // Cost for 3-month storage
    pub server_cost: u64,         // Server operational overhead
    pub ipfs_provider_cost: u64,  // Estimated IPFS provider costs
}

impl Default for CostMetadata {
    fn default() -> Self {
        Self {
            bandwidth_cost: 0,
            storage_cost: 0,
            server_cost: 0,
            ipfs_provider_cost: 0,
        }
    }
}

/// Meme account for tracking individual meme storage and expiration
#[account]
#[derive(Debug)]
pub struct MemeAccount {
    pub is_initialized: bool,
    pub meme_id: [u8; 8],                        // Unique meme identifier
    pub owner: Pubkey,                           // Meme uploader/owner
    pub contest_labels: [[u8; MAX_CONTEST_LABEL_LENGTH]; 10], // Array capacity for 10 (BPF limits to 5)
    pub contest_count: u8,                       // Number of active contest associations (0-5)
    pub ipfs_hash: [u8; MAX_IPFS_HASH_LENGTH],   // IPFS hash for content
    pub upload_timestamp: i64,                   // When meme was uploaded
    pub expiry_timestamp: i64,                   // When storage expires (upload + 3 months)
    pub storage_tier: StorageTier,               // Storage quality tier
    pub cost_breakdown: CostMetadata,            // Cost breakdown for transparency
    pub total_paid: u64,                         // Total amount paid for this meme
    pub extension_count: u8,                     // Number of storage extensions
    pub bump: u8,                                // PDA bump seed
}

impl Default for MemeAccount {
    fn default() -> Self {
        Self {
            is_initialized: false,
            meme_id: [0u8; 8],
            owner: Pubkey::default(),
            contest_labels: [[0u8; MAX_CONTEST_LABEL_LENGTH]; 10],
            contest_count: 0,
            ipfs_hash: [0u8; MAX_IPFS_HASH_LENGTH],
            upload_timestamp: 0,
            expiry_timestamp: 0,
            storage_tier: StorageTier::default(),
            cost_breakdown: CostMetadata::default(),
            total_paid: 0,
            extension_count: 0,
            bump: 0,
        }
    }
}

/// Meme expiration query result for client IPFS management
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct MemeExpirationData {
    pub meme_id: [u8; 8],
    pub upload_timestamp: i64,
    pub expiry_timestamp: i64,
    pub storage_tier: StorageTier,
    pub days_remaining: u32,                     // Days until expiration
    pub total_paid: u64,                         // Total amount paid for this meme
    pub extension_count: u8,                     // Number of extensions purchased
    pub is_expired: bool,                        // Whether meme has expired
}

/// Single hieroglyph metadata for batch queries - ENHANCED as single source of truth
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Copy)]
pub struct HieroglyphMetadata {
    pub id: u8,                          // Hieroglyph ID (0-31)
    pub name: [u8; 32],                  // Hieroglyph name (e.g., "Crown")
    pub symbol: [u8; 8],                 // Unicode symbol (e.g., "👑")
    pub category: [u8; 24],              // Category (e.g., "AdvancedOperation")
    pub scoring_weight: i64,             // SIGNED meme-score delta (SCORING-MODEL.md)
    pub stance: i8,                      // +1 FOR, -1 AGAINST, 0 NEUTRAL — the payer's prediction
    pub base_cost: u64,                  // Cost in KOTS tokens (with gas adjustment)
    pub cost_formula: [u8; 64],          // Human-readable cost formula
    pub total_cost_lamports: u64,        // Total cost including real-time gas
    pub total_cost_kots_scaled: u64,     // Total cost in KOTS scaled by 100000 (e.g., 3200016 = 32.00016 KOTS)
    // NOTE: ticket_award was removed by de-chancing (plan §5a). This changes
    // the Borsh layout of the returned metadata; clients from the ticket era
    // must be regenerated against the new IDL.
    pub gas_multiplier: u32,             // Gas multiplier (ID + 1)
}

impl Default for HieroglyphMetadata {
    fn default() -> Self {
        Self {
            id: 0,
            name: [0u8; 32],
            symbol: [0u8; 8],
            category: [0u8; 24],
            scoring_weight: 0,
            stance: 0,
            base_cost: 0,
            cost_formula: [0u8; 64],
            total_cost_lamports: 0,
            total_cost_kots_scaled: 0,
            gas_multiplier: 0,
        }
    }
}

/// All 32 hieroglyphs information returned at once
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct AllHieroglyphInfo {
    pub hieroglyphs: [HieroglyphMetadata; 32],    // All 32 hieroglyphs with complete info
}

/// Enhanced meme upload result with HYBRID MODEL support
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct MemeUploadResult {
    pub upload_fee_paid: u64,                    // Actual amount paid (can exceed minimum)
    pub minimum_required_cost: u64,              // Minimum cost that was required (0.29 KOTS)
    pub excess_payment: u64,                     // Excess payment routed to treasury
    pub estimated_monthly_cost_coverage: u32,    // Days of hosting coverage
    pub meme_id: [u8; 8],
    pub upload_timestamp: i64,
    pub expiry_timestamp: i64,
    pub storage_tier: StorageTier,
    pub cost_breakdown: CostMetadata,
}

/// Payment verification structure for blockchain-native receipt verification
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct PaymentVerification {
    pub transaction_signature: [u8; 64],        // Ed25519 transaction signature
    pub payer_wallet: Pubkey,                   // Who paid
    pub expected_amount: u64,                   // Expected payment amount
    pub verification_window_seconds: i64,       // Time window for verification
    pub payment_type: PaymentType,              // What the payment is for
}

/// Payment type enumeration
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub enum PaymentType {
    MemeUpload,
    ContestEntry,
    StorageExtension,
    HieroglyphInteraction,
}

/// Contest entry breakdown for HYBRID MODEL
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ContestEntryBreakdown {
    pub temporary_on_chain_state: u64,   // 0.3 KOTS - vote tracking during contest
    pub final_results_storage: u64,      // 0.2 KOTS - permanent winner records
}

/// Contest entry result with HYBRID MODEL support
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ContestEntryResult {
    pub contest_id: u64,
    pub meme_id: [u8; 8],
    pub entry_fee_paid: u64,
    pub on_chain_voting_enabled: bool,
    pub contest_entry_timestamp: i64,
    pub voting_period_end: i64,
    pub entry_breakdown: ContestEntryBreakdown,
}

/// Security validation result for cross-program security checks
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct SecurityValidationResult {
    pub is_valid: bool,
    pub blacklisted: bool,
    pub rate_limited: bool,
    pub gas_price_valid: bool,
    pub emergency_paused: bool,
}

// ============================================================================
// HELPER STRUCTURES FOR CROSS-PROGRAM COMMUNICATION
// ============================================================================

/// Contest state information for CPI calls
#[derive(AnchorSerialize, AnchorDeserialize, Debug, Clone)]
pub struct ContestStateInfo {
    pub is_active: bool,
    pub is_finalized: bool,
    pub participant_count: u64,
    pub total_prize_pool: u64,
    pub end_timestamp: i64,
    pub creation_timestamp: i64,
    pub duration_seconds: i64,
    pub creator: Pubkey,
    pub contest_type: u8,
    pub min_participants: u32,
    pub entry_fee: u64,
    pub total_collected_fees: u64,
    pub total_prize_distributed: u64,
    pub winner_count: u32,
    pub finalization_timestamp: i64,
    pub auto_distribution_enabled: bool,
    pub auto_distribution_timestamp: i64,
    pub peak_interaction_rate: f64,
    pub most_popular_interaction: u32,
    pub total_unique_voters: u32,
    pub authorized_finalizers: [Pubkey; MAX_AUTHORIZED_FINALIZERS],
    pub authorized_finalizers_count: u8,
}

/// Fee distribution breakdown structure
#[derive(AnchorSerialize, AnchorDeserialize, Debug, Clone)]
pub struct FeeDistributionBreakdown {
    pub jackpot_amount: u64,
    pub specified_wallet_amount: u64,
    pub burn_amount: u64,
    pub dev_amount: u64,
}

#[cfg(test)]
mod program_type_pinning {
    //! These pin the WIRE VALUES of both ProgramType enums (2026-07-23).
    //!
    //! Programs persist `ProgramType::X as u8` into account data. Reordering a
    //! variant therefore rewrites the meaning of bytes already on chain — a
    //! silent, unrecoverable relabelling of every existing account. A failure
    //! here means someone has changed an ordering; the fix is almost never to
    //! update the expected value.
    use super::ProgramType as StateProgramType;
    use crate::upgrade::ProgramType as UpgradeProgramType;

    #[test]
    fn state_program_type_wire_values_are_pinned() {
        assert_eq!(StateProgramType::ContestManagement as u8, 0);
        assert_eq!(StateProgramType::FeeManagement as u8, 1);
        assert_eq!(StateProgramType::VotingHieroglyph as u8, 2);
        assert_eq!(StateProgramType::EconomicsDistribution as u8, 3);
        assert_eq!(StateProgramType::SecurityGovernance as u8, 4);
        assert_eq!(StateProgramType::PrizeDistribution as u8, 5);
        assert_eq!(StateProgramType::ProgressiveJackpot as u8, 6);
    }

    #[test]
    fn upgrade_program_type_wire_values_are_pinned() {
        assert_eq!(UpgradeProgramType::SecurityGovernance as u8, 0);
        assert_eq!(UpgradeProgramType::ContestManagement as u8, 1);
        assert_eq!(UpgradeProgramType::VotingHieroglyph as u8, 2);
        assert_eq!(UpgradeProgramType::EconomicsDistribution as u8, 3);
        assert_eq!(UpgradeProgramType::FeeManagement as u8, 4);
        assert_eq!(UpgradeProgramType::PrizeDistribution as u8, 5);
        assert_eq!(UpgradeProgramType::ProgressiveJackpot as u8, 6);
        assert_eq!(UpgradeProgramType::TreasuryOperations as u8, 7);
    }

    /// The disagreement itself, asserted — so nobody "fixes" one ordering to
    /// match the other without realising that renumbers live account data.
    #[test]
    fn the_two_orderings_disagree_and_that_is_load_bearing() {
        assert_ne!(
            StateProgramType::ContestManagement as u8,
            UpgradeProgramType::ContestManagement as u8
        );
        assert_ne!(
            StateProgramType::SecurityGovernance as u8,
            UpgradeProgramType::SecurityGovernance as u8
        );
    }

    /// A bare `kots_common::ProgramType` must keep resolving to the state enum.
    /// If the explicit re-export in lib.rs is ever dropped, the glob from
    /// `upgrade` takes over and every `as u8` in the workspace changes meaning.
    #[test]
    fn the_crate_root_alias_still_points_at_the_state_enum() {
        assert_eq!(
            crate::ProgramType::ContestManagement as u8,
            StateProgramType::ContestManagement as u8
        );
    }
}

#[cfg(test)]
mod blacklist_predicate {
    //! O41 regression suite (2026-07-29).
    //!
    //! `is_blacklisted()` used to iterate `0..blacklist_count`, and
    //! `add_to_blacklist` never incremented `blacklist_count` — it maintained only
    //! `total_blacklisted`. The loop therefore ran `0..0` and the function returned
    //! **false for every wallet**, including ones it had just been told to block.
    //! A security control that failed open, silently, because the add path
    //! returned `Ok`.
    //!
    //! The counter is fixed too, but these tests deliberately construct states in
    //! which the counter is WRONG, because that is the state the defect produced
    //! and a control must not depend on bookkeeping being right.
    //!
    //! TWO of these are regression tests — they fail against the old
    //! implementation and would have caught O41:
    //! `blocks_a_wallet_even_when_the_counter_says_zero` and
    //! `blocks_an_entry_in_the_final_slot`. The rest pass under both
    //! implementations and guard the NEW one's own risks: an over-reporting
    //! counter, `Pubkey::default()` matching empty slots, and a cleared slot still
    //! blocking. Worth stating precisely, because "these tests would have caught
    //! it" is the kind of claim this file exists to stop people making loosely.
    use super::BlacklistState;
    use crate::constants::{MAX_BLACKLISTED_WALLETS_PER_ACCOUNT, MAX_REASON_LENGTH};
    use anchor_lang::prelude::Pubkey;

    /// A state with `count` entries recorded in the array, and `stated_count`
    /// written into the counter — which is allowed to disagree, on purpose.
    fn state_with(entries: &[Pubkey], stated_count: u16) -> BlacklistState {
        let mut wallets = [Pubkey::default(); MAX_BLACKLISTED_WALLETS_PER_ACCOUNT];
        for (slot, w) in entries.iter().enumerate() {
            wallets[slot] = *w;
        }
        BlacklistState {
            is_initialized: true,
            authority: Pubkey::new_unique(),
            moderator: Pubkey::new_unique(),
            blacklisted_wallets: wallets,
            blacklist_reasons: [[0u8; MAX_REASON_LENGTH]; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
            blacklist_timestamps: [0i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
            blacklist_count: stated_count,
            total_blacklisted: entries.len() as u32,
            last_update_timestamp: 0,
        }
    }

    /// THE REGRESSION. This is exactly the state the defective `add_to_blacklist`
    /// produced: the wallet is in the array, and the counter says zero.
    #[test]
    fn blocks_a_wallet_even_when_the_counter_says_zero() {
        let target = Pubkey::new_unique();
        let s = state_with(&[target], 0);
        assert!(
            s.is_blacklisted(&target),
            "a wallet present in blacklisted_wallets must be blocked regardless of \
             blacklist_count. This is the O41 defect: the counter was never \
             incremented, so the old implementation returned false for every wallet \
             and the blacklist blocked nobody."
        );
    }

    /// The mirror: a counter that over-reports must not invent entries.
    #[test]
    fn does_not_block_on_a_counter_that_over_reports() {
        let target = Pubkey::new_unique();
        let s = state_with(&[], MAX_BLACKLISTED_WALLETS_PER_ACCOUNT as u16);
        assert!(
            !s.is_blacklisted(&target),
            "an empty array with a non-zero counter must block nobody"
        );
    }

    /// Empty slots hold `Pubkey::default()`. Querying it must not match them —
    /// scanning the array rather than a counted prefix makes this reachable.
    #[test]
    fn the_default_pubkey_is_never_blacklisted() {
        let s = state_with(&[Pubkey::new_unique()], 1);
        assert!(
            !s.is_blacklisted(&Pubkey::default()),
            "Pubkey::default() fills every unused slot; matching it would blacklist \
             the zero address and, worse, report a hit on any state with a free slot"
        );
        let empty = state_with(&[], 0);
        assert!(!empty.is_blacklisted(&Pubkey::default()));
    }

    /// The last slot is the one a counted-prefix scan is most likely to miss.
    #[test]
    fn blocks_an_entry_in_the_final_slot() {
        let mut entries: Vec<Pubkey> = (0..MAX_BLACKLISTED_WALLETS_PER_ACCOUNT)
            .map(|_| Pubkey::new_unique())
            .collect();
        let last = *entries.last().unwrap();
        // Counter deliberately one short — the old off-by-one shape.
        let stated = (entries.len() - 1) as u16;
        entries.truncate(MAX_BLACKLISTED_WALLETS_PER_ACCOUNT);
        let s = state_with(&entries, stated);
        assert!(
            s.is_blacklisted(&last),
            "the final slot must be checked; a counted-prefix scan with a counter \
             one short would silently exempt the most recently added wallet"
        );
    }

    #[test]
    fn absent_wallet_is_not_blacklisted() {
        let s = state_with(&[Pubkey::new_unique(), Pubkey::new_unique()], 2);
        assert!(!s.is_blacklisted(&Pubkey::new_unique()));
    }

    /// A removed entry is cleared to `Pubkey::default()`, so it must stop matching
    /// even while the counter still claims it is there.
    #[test]
    fn a_cleared_slot_stops_blocking() {
        let target = Pubkey::new_unique();
        let mut s = state_with(&[target, Pubkey::new_unique()], 2);
        assert!(s.is_blacklisted(&target));
        s.blacklisted_wallets[0] = Pubkey::default(); // what remove_from_blacklist does
        assert!(
            !s.is_blacklisted(&target),
            "clearing the slot must un-blacklist, independently of the counter"
        );
    }
}
