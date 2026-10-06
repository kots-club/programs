//! Network utilities for the Security Governance program
//! 
//! Provides network-aware configuration access for program operations

use anchor_lang::prelude::*;
use kots_common::network_config::{config, NetworkConfig, Network};

/// Get network configuration with optional explicit network parameter
/// 
/// If `network` is None, auto-detects the current network.
/// If `network` is Some(Network), uses the explicit network configuration.
/// 
/// # Arguments
/// * `network` - Optional explicit network to use (None for auto-detection)
/// 
/// # Returns
/// NetworkConfig for the specified or detected network
pub fn get_current_config_with_network(network: Option<Network>) -> NetworkConfig {
    match network {
        Some(net) => NetworkConfig::for_network(net),
        None => config::current(),
    }
}

/// Get current network configuration (auto-detects network)
pub fn get_current_config() -> NetworkConfig {
    get_current_config_with_network(None)
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
    let expected_id = config.program_ids.security_governance;
    
    msg!("Network: {}, Expected Program ID: {}", 
         config.network.as_str(), expected_id);
    
    Ok(())
}

/// Get all program IDs for security validation
pub fn get_all_program_ids() -> Vec<Pubkey> {
    let ids = get_program_ids();
    vec![
        ids.contest_management,
        ids.voting_hieroglyph,
        ids.fee_management,
        ids.prize_distribution,
        ids.progressive_jackpot,
        ids.security_governance,
        ids.treasury_operations,
    ]
}

/// Get security-critical wallets
pub fn get_admin_wallet() -> Pubkey {
    get_system_wallets().admin
}

pub fn get_emergency_wallet() -> Pubkey {
    get_system_wallets().emergency
}

pub fn get_treasury_wallet() -> Pubkey {
    get_system_wallets().treasury
}

/// Check if we're on mainnet (for strictest security)
pub fn is_mainnet() -> bool {
    config::is_mainnet()
}

/// Check if a program ID is valid for the current network
pub fn is_valid_program_id(program_id: &Pubkey) -> bool {
    get_all_program_ids().contains(program_id)
}