//! Network utilities for the Fee Management program
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
    let expected_id = config.program_ids.fee_management;
    
    msg!("Network: {}, Expected Program ID: {}", 
         config.network.as_str(), expected_id);
    
    Ok(())
}

/// Get treasury operations program ID for fee distribution
pub fn get_treasury_operations_program_id() -> Pubkey {
    get_program_ids().treasury_operations
}

/// Get treasury wallet for fee collection
pub fn get_treasury_wallet() -> Pubkey {
    get_system_wallets().treasury
}

/// Get incoming wallet for immediate fee collection
pub fn get_incoming_wallet() -> Pubkey {
    get_system_wallets().incoming
}

/// Get burn wallet for token burning
pub fn get_burn_wallet() -> Pubkey {
    get_system_wallets().burn
}

/// Check if we're on mainnet (for different fee rates)
pub fn is_mainnet() -> bool {
    config::is_mainnet()
}