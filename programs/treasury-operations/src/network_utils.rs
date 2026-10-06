//! Network utilities for the Treasury Operations program
//! 
//! Provides network-aware configuration access for program operations

use anchor_lang::prelude::*;
use kots_common::network_config::{config, NetworkConfig};

/// Get current network configuration
pub fn get_current_config() -> NetworkConfig {
    config::current()
}

/// Get current program IDs for cross-program calls
pub fn get_program_ids() -> kots_common::network_config::ProgramIds {
    config::program_ids()
}

/// Get current system wallets
pub fn get_system_wallets() -> kots_common::network_config::SystemWallets {
    config::system_wallets()
}

/// Validate that we're using the correct program ID for this network
pub fn validate_program_context() -> Result<()> {
    let config = get_current_config();
    let expected_id = config.program_ids.treasury_operations;
    
    msg!("Network: {}, Expected Program ID: {}", 
         config.network.as_str(), expected_id);
    
    Ok(())
}

/// Get all program IDs for treasury operations
pub fn get_fee_management_program_id() -> Pubkey {
    get_program_ids().fee_management
}

pub fn get_prize_distribution_program_id() -> Pubkey {
    get_program_ids().prize_distribution
}

pub fn get_progressive_jackpot_program_id() -> Pubkey {
    get_program_ids().progressive_jackpot
}

pub fn get_security_governance_program_id() -> Pubkey {
    get_program_ids().security_governance
}

/// Get all treasury-related wallets
pub fn get_treasury_wallet() -> Pubkey {
    get_system_wallets().treasury
}

pub fn get_incoming_wallet() -> Pubkey {
    get_system_wallets().incoming
}

pub fn get_outgoing_wallet() -> Pubkey {
    get_system_wallets().outgoing
}

pub fn get_burn_wallet() -> Pubkey {
    get_system_wallets().burn
}

pub fn get_dev_fund_wallet() -> Pubkey {
    get_system_wallets().dev_fund
}

pub fn get_jackpot_wallet() -> Pubkey {
    get_system_wallets().jackpot
}

/// Check if we're on mainnet (for enhanced treasury security)
pub fn is_mainnet() -> bool {
    config::is_mainnet()
}

/// Get all wallet addresses for validation
pub fn get_all_system_wallets() -> Vec<Pubkey> {
    let wallets = get_system_wallets();
    vec![
        wallets.treasury,
        wallets.incoming,
        wallets.outgoing,
        wallets.burn,
        wallets.admin,
        wallets.emergency,
        wallets.jackpot,
        wallets.dev_fund,
    ]
}