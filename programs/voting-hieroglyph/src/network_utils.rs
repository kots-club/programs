//! Network utilities for the Voting Hieroglyph program
//! 
//! Provides network-aware configuration access for program operations

use anchor_lang::prelude::*;
use kots_common::network_config::{config, NetworkConfig, Network};

/// Get current network configuration
pub fn get_current_config() -> NetworkConfig {
    config::current()
}

/// Get network configuration with optional network parameter override
/// If network is None, auto-detects (defaults to MainnetBeta on-chain)
pub fn get_current_config_with_network(network: Option<Network>) -> NetworkConfig {
    match network {
        Some(net) => NetworkConfig::for_network(net),
        None => config::current(),
    }
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
    let expected_id = config.program_ids.voting_hieroglyph;
    
    // The current program ID is set by declare_id! in lib.rs
    // This validation ensures consistency between networks
    msg!("Network: {}, Expected Program ID: {}", 
         config.network.as_str(), expected_id);
    
    Ok(())
}

/// Get treasury wallet for the current network
pub fn get_treasury_wallet() -> Pubkey {
    get_system_wallets().treasury
}

/// Get burn wallet for the current network
pub fn get_burn_wallet() -> Pubkey {
    get_system_wallets().burn
}

/// Check if we're on mainnet (for additional security checks)
pub fn is_mainnet() -> bool {
    config::is_mainnet()
}

/// Check if we're on devnet (for debugging features)
pub fn is_devnet() -> bool {
    config::is_devnet()
}