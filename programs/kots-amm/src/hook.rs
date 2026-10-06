// DESTINATION: kots-contracts/programs/kots-amm/src/hook.rs
//! Hook ExtraAccountMetas contract + TransferChecked helper.
//!
//! The extra-account list is an on-chain ABI written by
//! `transfer_hook::initialize_extra_account_meta_list`. ORDER IS THE ABI.
//! Missing one account → Token-2022 / hook fails the transfer. There is no
//! silent fallback (AMM-SPEC §2).
//!
//! Live list (non-`o32-probe`, non-`sol-fees`) as of the 2026-07-29 O32 cut:
//!   0. Instructions sysvar
//!   1. whitelist PDA            ["whitelist"]            (hook program)
//!   2. pool_registry PDA        ["pool_registry"]        (hook program)
//!   3. fee_config PDA           ["fee_config"]           (hook program)
//!   4. fee_authority PDA        [FEE_AUTHORITY_SEED]     (hook program)
//!   5. Token-2022 program
//!   6. refund_queue             fee_config.refund_queue  (writable)
//!
//! Plus the ExtraAccountMetaList PDA itself:
//!   seeds ["extra-account-metas", mint] under the hook program.
//!
//! `spl_token_2022::onchain::invoke_transfer_checked` takes the extra-account
//! slice AFTER the four base accounts (source, mint, dest, authority). Same
//! helper as `lp-position-manager`. Heap law from LPM: **at most two hooked
//! legs per top-level instruction.**
//!
//! Registry (lock #18):
//!   After `initialize_pool`, the sidecar calls hook `register_pool(vault_kots, PoolType)`.
//!   `is_pool_destination` matches dest_owner OR dest_token against `PoolInfo.address`.
//!   Register the **KOTS vault token account** (the slot value), never the pool PDA only.
//!   NEVER `register_protocol_pool`.
//!   AMM does **not** CPI the hook registry.

use anchor_lang::prelude::*;
use crate::errors::AmmError;

pub const WSOL_MINT: Pubkey =
    anchor_lang::solana_program::pubkey!("So11111111111111111111111111111111111111112");

pub const DEVNET_FIXTURE_MINT: Pubkey =
    anchor_lang::solana_program::pubkey!("8s2xcGXRnbwszVyyE8vMAFs6coXPGaymJt3Pbspv1HhV");

pub const MAINNET_FIXTURE_MINT: Pubkey =
    anchor_lang::solana_program::pubkey!("5iKKRjEdYbdQLCmSvUqUu81pvZicy1Xi9CBxhZDgEW8u");

pub const FORBIDDEN_STAGE1_MINT: Pubkey =
    anchor_lang::solana_program::pubkey!("BiPAn9bMY7Xo5TSwkXbPrXbmsWsXhi4GFbQ7DaVteHwZ");

pub const FORBIDDEN_DEAD_HOOK: Pubkey =
    anchor_lang::solana_program::pubkey!("75TnNrE6nQWQ3o4jS9vYRizYzRoK3aWdWRoWScA2vTQq");

pub const HOOK_REMAINING_MIN: usize = 8;

pub fn require_hook_metas(remaining: &[AccountInfo<'_>]) -> Result<()> {
    require!(
        remaining.len() >= HOOK_REMAINING_MIN,
        AmmError::IncompleteHookAccounts
    );
    Ok(())
}

pub fn transfer_kots_hooked<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require_hook_metas(hook_metas)?;
    require_keys_neq!(*mint.key, FORBIDDEN_STAGE1_MINT, AmmError::UnhookedMint);
    spl_token_2022::onchain::invoke_transfer_checked(
        token_program.key,
        from,
        mint,
        to,
        authority,
        hook_metas,
        amount,
        decimals,
        signer_seeds,
    )
    .map_err(Into::into)
}

pub fn assert_allowed_kots_mint(mint: &Pubkey) -> Result<()> {
    require_keys_neq!(*mint, FORBIDDEN_STAGE1_MINT, AmmError::UnhookedMint);
    require_keys_neq!(*mint, FORBIDDEN_DEAD_HOOK, AmmError::UnhookedMint);
    #[cfg(not(feature = "realm-main"))]
    require_keys_eq!(*mint, DEVNET_FIXTURE_MINT, AmmError::UnhookedMint);
    #[cfg(feature = "realm-main")]
    require_keys_eq!(*mint, MAINNET_FIXTURE_MINT, AmmError::UnhookedMint);
    Ok(())
}

pub fn assert_wsol(mint: &Pubkey) -> Result<()> {
    require_keys_eq!(*mint, WSOL_MINT, AmmError::WrongQuoteMint);
    Ok(())
}
