//! Cross-Program Upgrade Coordination System
//! 
//! Manages coordinated upgrades across all 4 KOTS programs to ensure
//! consistency and prevent partial upgrade states.

use anchor_lang::prelude::*;
use crate::{
    errors::KotsError,
    upgrade::{ProgramType, UpgradeType},
};

/// Cross-program upgrade session state
#[account]
#[derive(Debug)]
pub struct UpgradeCoordinatorState {
    /// Unique session identifier for this upgrade batch
    pub session_id: u64,
    
    /// Target version for all programs in this upgrade
    pub target_version: u16,
    
    /// Program readiness status [Security, Contest, Voting, Economics, Fee, Prize, Jackpot, Treasury]
    pub program_readiness: [bool; 8],
    
    /// Current upgrade phase
    pub current_phase: UpgradePhase,
    
    /// Session start timestamp
    pub session_start: i64,
    
    /// Session timeout (auto-abort after this time)
    pub session_timeout: i64,
    
    /// Authority that initiated this session
    pub session_authority: Pubkey,
    
    /// Emergency abort capability
    pub emergency_abort_enabled: bool,
    
    /// Cross-program compatibility matrix
    pub compatibility_matrix: [[bool; 8]; 8], // Program x Program compatibility
    
    /// Session completion status
    pub session_complete: bool,
    
    /// Number of programs successfully upgraded
    pub programs_upgraded: u8,
    
    /// Session metadata
    pub upgrade_description: [u8; 256], // BPF-compatible fixed size
    
    /// Breaking changes flag
    pub breaking_changes: bool,
    
    /// Migration required flag
    pub migration_required: bool,
}

impl UpgradeCoordinatorState {
    pub const INIT_SPACE: usize = 8 + 8 + 2 + 8 + 1 + 8 + 8 + 32 + 1 + 64 + 1 + 1 + 256 + 1 + 1;
}

/// Upgrade execution phases for coordinated upgrades
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub enum UpgradePhase {
    /// Initial phase - collecting readiness reports
    Preparation,
    /// Pre-upgrade validation and pause operations
    PreUpgrade,
    /// Actual program upgrades
    Execution,
    /// Post-upgrade validation and resume operations
    PostUpgrade,
    /// Final verification and cleanup
    Completion,
    /// Emergency abort in progress
    Abort,
}

/// Program upgrade readiness report
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ProgramReadinessReport {
    pub program_type: ProgramType,
    pub current_version: u16,
    pub target_version: u16,
    pub ready: bool,
    pub blocking_issues: [[u8; 128]; 10], // Fixed array of issue strings
    pub blocking_issues_count: u8, // Track actual count
    pub estimated_upgrade_time: u32, // seconds
    pub requires_migration: bool,
    pub reporter: Pubkey,
    pub timestamp: i64,
}

/// Cross-program upgrade execution plan
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct UpgradeExecutionPlan {
    pub session_id: u64,
    pub execution_order: [ProgramType; 4], // Order of program upgrades
    pub parallel_execution: bool, // Execute all at once vs sequential
    pub rollback_plan: RollbackPlan,
    pub validation_checks: ValidationChecks,
    pub timeout_per_program: u32, // seconds
    pub max_retry_attempts: u8,
}

/// Rollback plan for failed upgrades
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct RollbackPlan {
    pub auto_rollback_enabled: bool,
    pub rollback_timeout: u32, // seconds
    pub preserve_data: bool,
    pub rollback_to_version: u16,
    pub emergency_contacts: [Option<Pubkey>; 3],
}

/// Validation checks for upgrade safety
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ValidationChecks {
    pub verify_program_data: bool,
    pub verify_account_compatibility: bool,
    pub verify_cpi_compatibility: bool,
    pub verify_data_migration: bool,
    pub verify_network_stability: bool,
    pub custom_validations: [[u8; 128]; 5], // Fixed array of validation strings
    pub custom_validations_count: u8, // Track actual count
}

/// Program upgrade statistics
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct UpgradeStatistics {
    pub total_sessions: u64,
    pub successful_sessions: u64,
    pub failed_sessions: u64,
    pub aborted_sessions: u64,
    pub average_execution_time: u32, // seconds
    pub last_upgrade_timestamp: i64,
    pub longest_upgrade_time: u32,
    pub shortest_upgrade_time: u32,
}

impl Default for UpgradePhase {
    fn default() -> Self {
        UpgradePhase::Preparation
    }
}

impl Default for ProgramReadinessReport {
    fn default() -> Self {
        Self {
            program_type: ProgramType::SecurityGovernance,
            current_version: 1,
            target_version: 1,
            ready: false,
            blocking_issues: [[0u8; 128]; 10],
            blocking_issues_count: 0,
            estimated_upgrade_time: 0,
            requires_migration: false,
            reporter: Pubkey::default(),
            timestamp: 0,
        }
    }
}

impl Default for UpgradeExecutionPlan {
    fn default() -> Self {
        Self {
            session_id: 0,
            execution_order: [
                ProgramType::SecurityGovernance,
                ProgramType::ContestManagement,
                ProgramType::VotingHieroglyph,
                ProgramType::EconomicsDistribution,
            ],
            parallel_execution: false,
            rollback_plan: RollbackPlan::default(),
            validation_checks: ValidationChecks::default(),
            timeout_per_program: 300, // 5 minutes
            max_retry_attempts: 3,
        }
    }
}

impl Default for RollbackPlan {
    fn default() -> Self {
        Self {
            auto_rollback_enabled: true,
            rollback_timeout: 600, // 10 minutes
            preserve_data: true,
            rollback_to_version: 1,
            emergency_contacts: [None; 3],
        }
    }
}

impl Default for ValidationChecks {
    fn default() -> Self {
        Self {
            verify_program_data: true,
            verify_account_compatibility: true,
            verify_cpi_compatibility: true,
            verify_data_migration: true,
            verify_network_stability: true,
            custom_validations: [[0u8; 128]; 5],
            custom_validations_count: 0,
        }
    }
}

impl Default for UpgradeStatistics {
    fn default() -> Self {
        Self {
            total_sessions: 0,
            successful_sessions: 0,
            failed_sessions: 0,
            aborted_sessions: 0,
            average_execution_time: 0,
            last_upgrade_timestamp: 0,
            longest_upgrade_time: 0,
            shortest_upgrade_time: u32::MAX,
        }
    }
}

/// Program ID registry for cross-program calls
pub struct ProgramIdRegistry;

impl ProgramIdRegistry {
    pub const SECURITY_GOVERNANCE: &'static str = "KOTSSecurityGov111111111111111111111111111111";
    pub const CONTEST_MANAGEMENT: &'static str = "KOTSContestMgmt111111111111111111111111111111";
    pub const VOTING_HIEROGLYPH: &'static str = "KOTSVotingHiero11111111111111111111111111111111";
    pub const ECONOMICS_DISTRIBUTION: &'static str = "KOTSEconomics111111111111111111111111111111111";
    pub const FEE_MANAGEMENT: &'static str = "AAc663e8XvawZLH3YHYs1zWtZjKPchiucAJV7iQSZPM9";
    pub const PRIZE_DISTRIBUTION: &'static str = "5c2yG172aKdn4QWn4TPJbEi6voiCx91WBokcavwsETPc";
    pub const PROGRESSIVE_JACKPOT: &'static str = "6z6iPn534dMykYgKv6oRQpeoghohjr8sxiiNpZYgacdk";
    pub const TREASURY_OPERATIONS: &'static str = "WsrwVBpfSpSsvAUVB7v9T5VjHaNfSXoSQng5g6ersEd";
    
    /// Get program ID by type
    pub fn get_program_id(program_type: ProgramType) -> Result<Pubkey> {
        let id_str = match program_type {
            ProgramType::SecurityGovernance => Self::SECURITY_GOVERNANCE,
            ProgramType::ContestManagement => Self::CONTEST_MANAGEMENT,
            ProgramType::VotingHieroglyph => Self::VOTING_HIEROGLYPH,
            ProgramType::EconomicsDistribution => Self::ECONOMICS_DISTRIBUTION,
            ProgramType::FeeManagement => Self::FEE_MANAGEMENT,
            ProgramType::PrizeDistribution => Self::PRIZE_DISTRIBUTION,
            ProgramType::ProgressiveJackpot => Self::PROGRESSIVE_JACKPOT,
            ProgramType::TreasuryOperations => Self::TREASURY_OPERATIONS,
        };
        
        id_str.parse().map_err(|_| KotsError::InvalidProgramId.into())
    }
    
    /// Validate that a pubkey matches expected program type
    pub fn validate_program_id(program_id: &Pubkey, expected_type: ProgramType) -> Result<bool> {
        let expected_id = Self::get_program_id(expected_type)?;
        Ok(*program_id == expected_id)
    }
    
    /// Get all program IDs as array
    pub fn get_all_program_ids() -> Result<[Pubkey; 8]> {
        Ok([
            Self::get_program_id(ProgramType::SecurityGovernance)?,
            Self::get_program_id(ProgramType::ContestManagement)?,
            Self::get_program_id(ProgramType::VotingHieroglyph)?,
            Self::get_program_id(ProgramType::EconomicsDistribution)?,
            Self::get_program_id(ProgramType::FeeManagement)?,
            Self::get_program_id(ProgramType::PrizeDistribution)?,
            Self::get_program_id(ProgramType::ProgressiveJackpot)?,
            Self::get_program_id(ProgramType::TreasuryOperations)?,
        ])
    }
}

/// Cross-program upgrade event
#[event]
pub struct CrossProgramUpgradeEvent {
    pub session_id: u64,
    pub phase: UpgradePhase,
    pub program_type: ProgramType,
    pub upgrade_type: UpgradeType,
    pub success: bool,
    pub error_message: [u8; 256], // BPF-compatible fixed size (empty if no error)
    pub has_error: bool, // Track if error_message is set
    pub timestamp: i64,
    pub executor: Pubkey,
}

/// Upgrade session initialization event
#[event]
pub struct UpgradeSessionInitializedEvent {
    pub session_id: u64,
    pub target_version: u16,
    pub initiator: Pubkey,
    pub estimated_duration: u32,
    pub breaking_changes: bool,
    pub timestamp: i64,
}

/// Upgrade session completion event
#[event]
pub struct UpgradeSessionCompletedEvent {
    pub session_id: u64,
    pub success: bool,
    pub programs_upgraded: u8,
    pub total_duration: u32,
    pub final_phase: UpgradePhase,
    pub timestamp: i64,
}