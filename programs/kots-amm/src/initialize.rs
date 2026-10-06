// DESTINATION: kots-contracts/programs/kots-amm/src/initialize.rs
//! initialize_pool asserts + registry note.
//!
//! Does **not** CPI `transfer_hook::register_pool`. Hook registry authority is
//! a different key (`has_one = authority` on the hook's pool_registry).
//! Sidecar calls hook `register_pool(vault_kots, PoolType)` after this ix
//! (lock #18). Never `register_protocol_pool`.

use anchor_lang::prelude::*;
use crate::errors::AmmError;
use crate::hook::{assert_allowed_kots_mint, assert_wsol};
use crate::state::Pool;

/// SPL Token program (classic). LP mint + WSOL vault live here.
pub const TOKEN_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022. KOTS mint + KOTS vault live here.
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub fn assert_token_account_owned_by(
    account: &AccountInfo,
    expected_owner: &Pubkey,
    expected_mint: &Pubkey,
    expected_token_program: &Pubkey,
) -> Result<()> {
    require_keys_eq!(*account.owner, *expected_token_program, AmmError::WrongTokenProgram);
    require!(account.data_len() >= 72, AmmError::VaultMint);
    let data = account.try_borrow_data()?;
    let mint = Pubkey::try_from(&data[0..32]).map_err(|_| error!(AmmError::VaultMint))?;
    let owner = Pubkey::try_from(&data[32..64]).map_err(|_| error!(AmmError::VaultOwner))?;
    require_keys_eq!(mint, *expected_mint, AmmError::VaultMint);
    require_keys_eq!(owner, *expected_owner, AmmError::VaultOwner);
    Ok(())
}

pub fn assert_classic_mint(account: &AccountInfo) -> Result<()> {
    require_keys_eq!(*account.owner, TOKEN_PROGRAM_ID, AmmError::HookedLpMint);
    Ok(())
}

pub fn assert_token_2022_mint(account: &AccountInfo) -> Result<()> {
    require_keys_eq!(*account.owner, TOKEN_2022_PROGRAM_ID, AmmError::UnhookedMint);
    Ok(())
}

pub struct Populate {
    pub mint_kots: Pubkey,
    pub mint_wsol: Pubkey,
    pub vault_kots: Pubkey,
    pub vault_wsol: Pubkey,
    pub lp_mint: Pubkey,
    pub bump: u8,
}

pub fn populate_pool(pool: &mut Pool, ctx_authority: Pubkey, p: Populate) -> Result<()> {
    assert_allowed_kots_mint(&p.mint_kots)?;
    assert_wsol(&p.mint_wsol)?;
    pool.authority = ctx_authority;
    pool.mint_kots = p.mint_kots;
    pool.mint_wsol = p.mint_wsol;
    pool.vault_kots = p.vault_kots;
    pool.vault_wsol = p.vault_wsol;
    pool.lp_mint = p.lp_mint;
    pool.bump = p.bump;
    pool.paused = false;
    pool.seeded = false;
    pool.reserved = [0u8; 31];
    Ok(())
}

/// Sidecar-facing reminder. The vault pubkey that must go into a hook registry
/// slot. Logged so P1-T4 evidence is an on-chain log, not a chat message.
pub fn log_registry_target(vault_kots: &Pubkey) {
    msg!("kots-amm registry target (KOTS vault) = {}", vault_kots);
    msg!("sidecar: hook.register_pool(vault_kots, PoolType) — NEVER register_protocol_pool");
}
