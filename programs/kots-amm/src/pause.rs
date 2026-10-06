// DESTINATION: kots-contracts/programs/kots-amm/src/pause.rs
//! Dual pause gate.
//!
//! Two flags, both must be off before swap / add:
//!   1. `Pool.paused` — AMM admin ix (`pause` / `unpause`), one key.
//!   2. security-governance `EmergencyState.is_paused` — admin panel pause;
//!      resume is the governance quorum path (`emergency_resume` + decision).
//!
//! Remove-liquidity stays ungated (lock #9). Do not invent a third flag.

use anchor_lang::prelude::*;

use crate::errors::AmmError;

/// Devnet security-governance program id (IDL / program-id-registry).
/// Must equal `KOTS_PROGRAM_SECURITY_GOVERNANCE` on the sidecar of this realm.
/// Confirm before deploy:
///   `solana program show 3EKVa1rSd4cTXB2VT4gWwYfDGjM3m5nw4EYP2jvCb28H --url devnet`
/// If that program is not security-governance on your cluster, stop and replace
/// this const. Do not invent a mainnet id (`realm-main` still compile_error).
pub const SECURITY_GOVERNANCE_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("3EKVa1rSd4cTXB2VT4gWwYfDGjM3m5nw4EYP2jvCb28H");

pub const EMERGENCY_SEED: &[u8] = b"emergency_state_v4";
pub const GOVERNANCE_SUB_SEED: &[u8] = b"v4";

/// Anchor discriminator (8) + is_initialized (1) + emergency_authority (32).
pub const IS_PAUSED_OFFSET: usize = 41;

pub fn require_platform_unpaused(emergency_state: &AccountInfo) -> Result<()> {
    require_keys_eq!(
        *emergency_state.owner,
        SECURITY_GOVERNANCE_ID,
        AmmError::Unauthorized
    );
    let (expected, _) = Pubkey::find_program_address(
        &[EMERGENCY_SEED, GOVERNANCE_SUB_SEED],
        &SECURITY_GOVERNANCE_ID,
    );
    require_keys_eq!(*emergency_state.key, expected, AmmError::Unauthorized);
    let data = emergency_state.try_borrow_data()?;
    require!(data.len() > IS_PAUSED_OFFSET, AmmError::Paused);
    require!(data[IS_PAUSED_OFFSET] == 0, AmmError::Paused);
    Ok(())
}
