// kots-contracts/shared/kots-common/src/lib.rs
//! KOTS Common Library
//! 
//! Shared types, constants, and utilities for the KOTS multi-program architecture.
//! This library ensures consistency across all 4 KOTS programs while minimizing
//! code duplication and maintaining strict compatibility.

use anchor_lang::prelude::*;
use borsh;
// Token imports removed - not used in shared common library

pub mod constants;
pub mod hieroglyphs;  // THE glyph registry  — single source of truth (HIEROGLYPH-REGISTRY.md)
pub mod program_ids;  // THE program registry — single source of truth (the ID law: program IDs are permanent)
pub mod state;
pub mod errors;
pub mod purpose_key; // ACT-24 — the structured accounting key (F15); dependency-free on purpose
pub mod cpi;
pub mod utils;
pub mod upgrade;
pub mod upgrade_coordinator;
pub mod cu_monitor;  // Add CU monitoring module
pub mod network_config; // Network-aware configuration
pub mod init; // Centralized init replacement module
pub mod version; // Deployment verification system

// Re-export commonly used items
pub use constants::*;
pub use errors::*;
pub use cpi::*;
pub use utils::*;
pub use upgrade::*;
pub use upgrade_coordinator::*;
pub use cu_monitor::*;  // Export CU monitoring utilities
pub use network_config::*; // Export network configuration (includes Network enum)
// program_ids is NOT glob-exported on purpose: `kots_common::program_ids::current(&crate::ID)`
// reads as what it is — ask the registry, identifying yourself. A bare `current()` in scope
// is exactly the ambiguity that let five ID tables coexist.
pub use init::*; // Export init replacement utilities
pub use version::*; // Export version heartbeat system
// Specific state re-exports to avoid ambiguity
pub use state::{
    ContestStateInfo, FeeDistributionBreakdown, ProgramType,
    // Meme upload types
    StorageTier, PaymentVerification, PaymentType, MemeAccount,
    MemeExpirationData, CostMetadata, MemeUploadResult, SecurityValidationResult,
    // Account structures for IDL generation
    ParticipantScoreAccount, BlacklistAccountStorage, ContestCoreState,
    ParticipantState, VoteTrackerState, VoteSequenceMaster, InteractionBatchState,
    UserRateLimitState, FeeDistributionState, ContestVaultState,
    ProgressiveJackpotState, EnhancedGasOracleState, BlacklistState,
    EmergencyState, AuditState, RoleState, StakingState, StakingRegistry,
    ProgramUpgradeState
};

// Program ID declarations for CPI - removed declare_id! to fix account ownership conflicts
// Provide ID constant for #[account] macros without declaring a conflicting program ID
pub const ID: anchor_lang::prelude::Pubkey = anchor_lang::prelude::Pubkey::new_from_array([0; 32]);

/// Contest identifier type used across all programs
pub type ContestLabel = [u8; 32];

/// Network types supported by KOTS
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq)]
pub enum NetworkType {
    Localnet,
    Devnet,
    Testnet,
    Mainnet,
}

/// Program identifiers for CPI calls
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ProgramIds {
    pub contest_management: Pubkey,
    pub voting_hieroglyph: Pubkey,
    pub fee_management: Pubkey,
    pub progressive_jackpot: Pubkey,
    pub treasury_operations: Pubkey,
    pub prize_distribution: Pubkey,
    pub security_governance: Pubkey,
}

/// Global configuration shared across programs
#[account]
pub struct GlobalConfig {
    pub network: NetworkType,
    pub program_ids: ProgramIds,
    pub admin_authority: Pubkey,
    pub emergency_authority: Pubkey,
    pub is_initialized: bool,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            network: NetworkType::Localnet,
            program_ids: ProgramIds {
                contest_management: Pubkey::default(),
                voting_hieroglyph: Pubkey::default(),
                fee_management: Pubkey::default(),
                progressive_jackpot: Pubkey::default(),
                treasury_operations: Pubkey::default(),
                prize_distribution: Pubkey::default(),
                security_governance: Pubkey::default(),
            },
            admin_authority: Pubkey::default(),
            emergency_authority: Pubkey::default(),
            is_initialized: false,
        }
    }
}

/// Version information for compatibility checks
pub const KOTS_VERSION: &str = "3.1.0";
pub const PROTOCOL_VERSION: u32 = 1;

// Program functionality moved to individual program crates
// This shared library only contains types, constants, and utilities

// Account structures moved to individual program crates
// This shared library contains only types, constants, and utilities