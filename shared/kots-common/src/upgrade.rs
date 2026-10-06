//! KOTS Upgrade Governance Module
//! 
//! Shared upgrade structures and governance mechanisms for all KOTS programs.
//! Implements both governance-controlled upgrades (requiring voting) and 
//! systemic upgrades (internal, no voting required).

use anchor_lang::prelude::*;

/// Upgrade proposal status enumeration
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Default)]
pub enum UpgradeProposalStatus {
    #[default]
    Active,
    Passed,
    Failed,
    Executed,
    Cancelled,
}

/// Upgrade proposal type - determines governance requirements
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Default)]
pub enum UpgradeType {
    /// Requires full governance voting process
    #[default]
    GovernanceControlled,
    /// Internal system upgrade, no voting required
    Systemic,
    /// Emergency upgrade, bypass governance
    Emergency,
}

/// Program type for upgrade coordination
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Default)]
pub enum ProgramType {
    #[default]
    SecurityGovernance,
    ContestManagement,
    VotingHieroglyph,
    EconomicsDistribution,
    FeeManagement,
    PrizeDistribution,
    ProgressiveJackpot,
    TreasuryOperations,
}

/// Upgrade proposal structure for all programs
#[account]
pub struct UpgradeProposal {
    /// Unique proposal identifier
    pub proposal_id: u64,
    /// Program being upgraded
    pub program_type: ProgramType,
    /// Upgrade type (governance vs systemic)
    pub upgrade_type: UpgradeType,
    /// Proposer wallet
    pub proposer: Pubkey,
    /// Program buffer containing new code
    pub program_buffer: Pubkey,
    /// Authority that can execute the upgrade
    pub upgrade_authority: Pubkey,
    /// Proposed version number
    pub proposed_version: u16,
    /// Proposal creation timestamp
    pub proposal_created_at: i64,
    /// Voting period end timestamp
    pub voting_ends_at: i64,
    /// Votes in favor (weighted by stake)
    pub votes_for: u64,
    /// Votes against (weighted by stake)
    pub votes_against: u64,
    /// Required quorum percentage (basis points)
    pub quorum_required: u8,
    /// Delay before execution after voting passes
    pub execution_delay: i64,
    /// Current status of the proposal
    pub proposal_status: UpgradeProposalStatus,
    /// Description of the upgrade (BPF-compatible fixed size)
    pub upgrade_description: [u8; 256],
    /// Whether this upgrade has breaking changes
    pub breaking_changes: bool,
    /// Whether this upgrade requires data migration
    pub migration_required: bool,
    /// Cross-program dependencies
    pub dependencies: [ProgramType; 8],
    /// Number of actual dependencies
    pub dependency_count: u8,
    /// Minimum compatible version for dependencies
    pub min_compatible_versions: [u16; 8],
}

impl Default for UpgradeProposal {
    fn default() -> Self {
        Self {
            proposal_id: 0,
            program_type: ProgramType::default(),
            upgrade_type: UpgradeType::default(),
            proposer: Pubkey::default(),
            program_buffer: Pubkey::default(),
            upgrade_authority: Pubkey::default(),
            proposed_version: 1,
            proposal_created_at: 0,
            voting_ends_at: 0,
            votes_for: 0,
            votes_against: 0,
            quorum_required: 51, // 51% quorum by default
            execution_delay: 86400, // 24 hours delay by default
            proposal_status: UpgradeProposalStatus::default(),
            upgrade_description: [0u8; 256], // Empty description
            breaking_changes: false,
            migration_required: false,
            dependencies: [ProgramType::default(); 8],
            dependency_count: 0,
            min_compatible_versions: [1u16; 8],
        }
    }
}

/// Upgrade registry for tracking upgrade history across programs
#[account]
#[derive(Default)]
pub struct UpgradeRegistry {
    /// Program type this registry tracks
    pub program_type: ProgramType,
    /// Current deployed version
    pub current_version: u16,
    /// Number of pending proposals
    pub pending_proposals: u32,
    /// Total successful upgrades
    pub successful_upgrades: u32,
    /// Total failed upgrades
    pub failed_upgrades: u32,
    /// Timestamp of last upgrade
    pub last_upgrade_timestamp: i64,
    /// Authority that can approve upgrades
    pub upgrade_authority: Option<Pubkey>,
    /// Whether governance approval is required
    pub governance_approval_required: bool,
    /// Whether emergency upgrades are enabled
    pub emergency_upgrade_enabled: bool,
    /// Version history (hash of last 10 versions)
    pub version_history: [[u8; 32]; 10],
    /// Cross-program compatibility matrix
    pub compatible_versions: [u16; 8], // [security, contest, voting, economics, fee, prize, jackpot, treasury]
    /// Last compatibility check timestamp
    pub last_compatibility_check: i64,
    /// Emergency pause capability
    pub emergency_pause_enabled: bool,
    /// Emergency contact for critical issues
    pub emergency_contact: Option<Pubkey>,
}

/// Cross-program upgrade coordination state
#[account]
#[derive(Default)]
pub struct CrossProgramUpgradeState {
    /// Whether a cross-program upgrade is in progress
    pub upgrade_in_progress: bool,
    /// Target versions for all programs
    pub target_versions: [u16; 8], // [security, contest, voting, economics, fee, prize, jackpot, treasury]
    /// Current upgrade progress
    pub programs_upgraded: [bool; 8],
    /// Coordinator authority
    pub coordinator: Pubkey,
    /// Upgrade session ID
    pub session_id: u64,
    /// Session start timestamp
    pub session_start: i64,
    /// Maximum session duration
    pub session_timeout: i64,
    /// Rollback plan if upgrade fails
    pub rollback_enabled: bool,
    /// Previous versions for rollback
    pub rollback_versions: [u16; 8],
}

/// Governance vote record for upgrade proposals
#[account]
#[derive(Default)]
pub struct UpgradeVoteRecord {
    /// Proposal being voted on
    pub proposal_id: u64,
    /// Voter wallet
    pub voter: Pubkey,
    /// Vote choice (true = support, false = oppose)
    pub support: bool,
    /// Vote weight based on stake
    pub vote_weight: u64,
    /// Timestamp of vote
    pub vote_timestamp: i64,
    /// Voter's role level
    pub voter_role: u8,
}

impl UpgradeProposal {
    pub const INIT_SPACE: usize = 8 + // discriminator
        8 + // proposal_id
        1 + // program_type
        1 + // upgrade_type  
        32 + // proposer
        32 + // program_buffer
        32 + // upgrade_authority
        2 + // proposed_version
        8 + // proposal_created_at
        8 + // voting_ends_at
        8 + // votes_for
        8 + // votes_against
        1 + // quorum_required
        8 + // execution_delay
        1 + // proposal_status
        256 + // upgrade_description (fixed byte array)
        1 + // breaking_changes
        1 + // migration_required
        4 + // dependencies array
        1 + // dependency_count
        8; // min_compatible_versions array

    /// Check if voting period has ended
    pub fn is_voting_ended(&self, current_time: i64) -> bool {
        current_time > self.voting_ends_at
    }

    /// Check if proposal has met quorum requirements
    pub fn has_met_quorum(&self, total_stake: u64) -> bool {
        let total_votes = self.votes_for + self.votes_against;
        let quorum_threshold = (total_stake * self.quorum_required as u64) / 100;
        total_votes >= quorum_threshold
    }

    /// Check if proposal has passed (majority vote)
    pub fn has_passed(&self) -> bool {
        self.votes_for > self.votes_against
    }

    /// Check if execution delay has been met
    pub fn can_execute(&self, current_time: i64) -> bool {
        self.proposal_status == UpgradeProposalStatus::Passed &&
        current_time >= self.voting_ends_at + self.execution_delay
    }
}

impl UpgradeRegistry {
    pub const INIT_SPACE: usize = 8 + // discriminator
        1 + // program_type
        2 + // current_version
        4 + // pending_proposals
        4 + // successful_upgrades
        4 + // failed_upgrades
        8 + // last_upgrade_timestamp
        33 + // upgrade_authority (Option<Pubkey>)
        1 + // governance_approval_required
        1 + // emergency_upgrade_enabled
        320 + // version_history (10 * 32 bytes)
        8 + // compatible_versions array
        8 + // last_compatibility_check
        1 + // emergency_pause_enabled
        33; // emergency_contact (Option<Pubkey>)

    /// Check if upgrade is compatible with other programs
    pub fn is_compatible_with(&self, other_program: ProgramType, other_version: u16) -> bool {
        let index = match other_program {
            ProgramType::SecurityGovernance => 0,
            ProgramType::ContestManagement => 1,
            ProgramType::VotingHieroglyph => 2,
            ProgramType::EconomicsDistribution => 3,
            ProgramType::FeeManagement => 4,
            ProgramType::PrizeDistribution => 5,
            ProgramType::ProgressiveJackpot => 6,
            ProgramType::TreasuryOperations => 7,
        };
        
        self.compatible_versions[index] <= other_version
    }

    /// Update version after successful upgrade
    pub fn update_version(&mut self, new_version: u16, timestamp: i64) {
        self.current_version = new_version;
        self.last_upgrade_timestamp = timestamp;
        self.successful_upgrades += 1;
        
        // Update version history (shift and add new)
        for i in (1..10).rev() {
            self.version_history[i] = self.version_history[i - 1];
        }
        // Store version as bytes in first 2 bytes, rest zeros
        let mut version_bytes = [0u8; 32];
        let version_le_bytes = new_version.to_le_bytes();
        version_bytes[0] = version_le_bytes[0];
        version_bytes[1] = version_le_bytes[1];
        self.version_history[0] = version_bytes;
    }
}

impl CrossProgramUpgradeState {
    pub const INIT_SPACE: usize = 8 + // discriminator
        1 + // upgrade_in_progress
        8 + // target_versions array
        4 + // programs_upgraded array
        32 + // coordinator
        8 + // session_id
        8 + // session_start
        8 + // session_timeout
        1 + // rollback_enabled
        8; // rollback_versions array

    /// Check if all programs have been upgraded
    pub fn is_complete(&self) -> bool {
        self.programs_upgraded.iter().all(|&upgraded| upgraded)
    }

    /// Check if session has timed out
    pub fn has_timed_out(&self, current_time: i64) -> bool {
        current_time > self.session_start + self.session_timeout
    }

    /// Mark program as upgraded
    pub fn mark_program_upgraded(&mut self, program_type: ProgramType) {
        let index = match program_type {
            ProgramType::SecurityGovernance => 0,
            ProgramType::ContestManagement => 1,
            ProgramType::VotingHieroglyph => 2,
            ProgramType::EconomicsDistribution => 3,
            ProgramType::FeeManagement => 4,
            ProgramType::PrizeDistribution => 5,
            ProgramType::ProgressiveJackpot => 6,
            ProgramType::TreasuryOperations => 7,
        };
        self.programs_upgraded[index] = true;
    }
}

impl UpgradeVoteRecord {
    pub const INIT_SPACE: usize = 8 + // discriminator
        8 + // proposal_id
        32 + // voter
        1 + // support
        8 + // vote_weight
        8 + // vote_timestamp
        1; // voter_role
}

/// Upgrade governance constants
pub const UPGRADE_PROPOSAL_SEED: &[u8] = b"upgrade_proposal_v4";
pub const UPGRADE_REGISTRY_SEED: &[u8] = b"upgrade_registry_v4"; 
pub const CROSS_PROGRAM_UPGRADE_SEED: &[u8] = b"cross_program_upgrade_v4";
pub const UPGRADE_VOTE_SEED: &[u8] = b"upgrade_vote";

/// Default governance parameters
pub const DEFAULT_QUORUM_REQUIRED: u8 = 51; // 51% quorum
pub const DEFAULT_EXECUTION_DELAY: i64 = 86400; // 24 hours
pub const DEFAULT_VOTING_PERIOD: i64 = 604800; // 7 days
pub const EMERGENCY_EXECUTION_DELAY: i64 = 3600; // 1 hour for emergency
pub const MAX_CROSS_PROGRAM_SESSION_DURATION: i64 = 7200; // 2 hours

/// Version compatibility constants
pub const MAJOR_VERSION_MULTIPLIER: u16 = 1000;
pub const MINOR_VERSION_MULTIPLIER: u16 = 100;
pub const PATCH_VERSION_MULTIPLIER: u16 = 1;

/// Upgrade events
#[event]
pub struct UpgradeProposalCreatedEvent {
    pub proposal_id: u64,
    pub program_type: ProgramType,
    pub upgrade_type: UpgradeType,
    pub proposer: Pubkey,
    pub proposed_version: u16,
    pub voting_ends_at: i64,
    pub breaking_changes: bool,
    pub migration_required: bool,
    pub created_at: i64,
}

#[event]
pub struct UpgradeVoteEvent {
    pub proposal_id: u64,
    pub voter: Pubkey,
    pub support: bool,
    pub vote_weight: u64,
    pub votes_for: u64,
    pub votes_against: u64,
    pub voted_at: i64,
}

#[event]
pub struct UpgradeExecutedEvent {
    pub proposal_id: u64,
    pub program_type: ProgramType,
    pub upgrade_type: UpgradeType,
    pub executor: Pubkey,
    pub old_version: u16,
    pub new_version: u16,
    pub executed_at: i64,
    pub breaking_changes: bool,
    pub migration_required: bool,
}

#[event]
pub struct CrossProgramUpgradeStartedEvent {
    pub session_id: u64,
    pub coordinator: Pubkey,
    pub target_versions: [u16; 8],
    pub started_at: i64,
    pub timeout_at: i64,
}

#[event]
pub struct CrossProgramUpgradeCompletedEvent {
    pub session_id: u64,
    pub coordinator: Pubkey,
    pub final_versions: [u16; 8],
    pub completed_at: i64,
    pub successful: bool,
}