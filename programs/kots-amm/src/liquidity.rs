// DESTINATION: kots-contracts/programs/kots-amm/src/liquidity.rs
//! Add / remove liquidity. AMM-SPEC §3.5. Zero AMM fee.
//!
//! First deposit (mainnet dust): TransferChecked KOTS user → vault (hook fires)
//! plus WSOL user → vault. Any both-sides amount > 0. Vaults must be empty.
//! LP minted = isqrt(landed_kots * landed_wsol). Landed KOTS is vault Δ after
//! Token-2022 withhold. No MintTo. Mint authority is unset on 5iKK.
//!
//! Later deposits ratio-match (floor, favour pool). remove allowed while paused.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, program::invoke_signed};

use crate::errors::AmmError;
use crate::hook::{require_hook_metas, transfer_kots_hooked};
use crate::initialize::TOKEN_PROGRAM_ID;
use crate::state::{Pool, POOL_SEED};

fn token_amount(ai: &AccountInfo) -> Result<u64> {
    let data = ai.try_borrow_data()?;
    require!(data.len() >= 72, AmmError::InvalidLiquidity);
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

fn mint_supply(ai: &AccountInfo) -> Result<u64> {
    let data = ai.try_borrow_data()?;
    require!(data.len() >= 44, AmmError::InvalidLiquidity);
    Ok(u64::from_le_bytes(data[36..44].try_into().unwrap()))
}

fn isqrt(n: u128) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.saturating_add(1) / 2;
    while y < x {
        x = y;
        y = x.saturating_add(n / x) / 2;
    }
    u64::try_from(x).unwrap_or(u64::MAX)
}

fn transfer_wsol<'info>(
    token_program: &AccountInfo<'info>,
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: *token_program.key,
        accounts: vec![
            AccountMeta::new(*from.key, false),
            AccountMeta::new(*to.key, false),
            AccountMeta::new_readonly(*authority.key, true),
        ],
        data: {
            let mut d = Vec::with_capacity(9);
            d.push(3u8);
            d.extend_from_slice(&amount.to_le_bytes());
            d
        },
    };
    let accs = [from, to, authority, token_program.clone()];
    if signer_seeds.is_empty() {
        invoke(&ix, &accs)?;
    } else {
        invoke_signed(&ix, &accs, signer_seeds)?;
    }
    Ok(())
}

fn mint_lp<'info>(
    token_program: &AccountInfo<'info>,
    mint: AccountInfo<'info>,
    dest: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    require!(amount > 0, AmmError::InvalidLiquidity);
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*mint.key, false),
            AccountMeta::new(*dest.key, false),
            AccountMeta::new_readonly(*authority.key, true),
        ],
        data: {
            let mut d = Vec::with_capacity(9);
            d.push(7u8); // MintTo LP (unhooked classic SPL)
            d.extend_from_slice(&amount.to_le_bytes());
            d
        },
    };
    let accs = [mint, dest, authority, token_program.clone()];
    invoke_signed(&ix, &accs, signer_seeds)?;
    Ok(())
}

fn burn_lp<'info>(
    token_program: &AccountInfo<'info>,
    mint: AccountInfo<'info>,
    from: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    require!(amount > 0, AmmError::InvalidLiquidity);
    let ix = anchor_lang::solana_program::instruction::Instruction {
        program_id: TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*from.key, false),
            AccountMeta::new(*mint.key, false),
            AccountMeta::new_readonly(*authority.key, true),
        ],
        data: {
            let mut d = Vec::with_capacity(9);
            d.push(8u8); // Burn
            d.extend_from_slice(&amount.to_le_bytes());
            d
        },
    };
    invoke(&ix, &[from, mint, authority, token_program.clone()])?;
    Ok(())
}

pub fn execute_add<'info>(
    pool: &Pool,
    pool_ai: AccountInfo<'info>,
    user: AccountInfo<'info>,
    mint_kots: AccountInfo<'info>,
    vault_kots: AccountInfo<'info>,
    vault_wsol: AccountInfo<'info>,
    user_kots: AccountInfo<'info>,
    user_wsol: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    user_lp: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    token_2022_program: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    amount_kots: u64,
    amount_wsol: u64,
    kots_decimals: u8,
) -> Result<bool> {
    require!(!pool.paused, AmmError::Paused);
    require!(amount_kots > 0, AmmError::InvalidLiquidity);
    require!(amount_wsol > 0, AmmError::InvalidLiquidity);
    require_hook_metas(hook_metas)?;
    require_keys_eq!(vault_kots.key(), pool.vault_kots, AmmError::Unauthorized);
    require_keys_eq!(vault_wsol.key(), pool.vault_wsol, AmmError::Unauthorized);
    require_keys_eq!(lp_mint.key(), pool.lp_mint, AmmError::Unauthorized);

    let seed_slice: &[&[u8]] = &[
        POOL_SEED,
        pool.mint_kots.as_ref(),
        pool.mint_wsol.as_ref(),
        &[pool.bump],
    ];
    let signer: &[&[&[u8]]] = &[seed_slice];

    let x0 = token_amount(&vault_kots)?;
    let y0 = token_amount(&vault_wsol)?;

    if !pool.seeded {
        require!(x0 == 0 && y0 == 0, AmmError::InvalidLiquidity);
        transfer_kots_hooked(
            token_2022_program,
            user_kots,
            mint_kots,
            vault_kots.clone(),
            user.clone(),
            hook_metas,
            amount_kots,
            kots_decimals,
            &[],
        )?;
        let landed = token_amount(&vault_kots)?;
        require!(landed > 0, AmmError::InvalidLiquidity);
        transfer_wsol(
            &token_program,
            user_wsol,
            vault_wsol.clone(),
            user,
            amount_wsol,
            &[],
        )?;
        let y1 = token_amount(&vault_wsol)?;
        require!(y1 == amount_wsol, AmmError::InvalidLiquidity);
        let lp = isqrt((landed as u128).saturating_mul(y1 as u128));
        require!(lp > 0, AmmError::InvalidLiquidity);
        mint_lp(&token_program, lp_mint, user_lp, pool_ai, lp, signer)?;
        msg!("seed add landed={} wsol={} lp={}", landed, y1, lp);
        return Ok(true);
    }

    require!(x0 > 0 && y0 > 0, AmmError::InvalidLiquidity);
    transfer_kots_hooked(
        token_2022_program,
        user_kots,
        mint_kots,
        vault_kots.clone(),
        user.clone(),
        hook_metas,
        amount_kots,
        kots_decimals,
        &[],
    )?;
    let landed = token_amount(&vault_kots)?
        .checked_sub(x0)
        .ok_or(AmmError::MathOverflow)?;
    require!(landed > 0, AmmError::InvalidLiquidity);
    let supply = mint_supply(&lp_mint)?;
    require!(supply > 0, AmmError::InvalidLiquidity);
    let lp_from_k = (landed as u128)
        .checked_mul(supply as u128)
        .and_then(|v| v.checked_div(x0 as u128))
        .ok_or(AmmError::MathOverflow)?;
    let lp_from_s = (amount_wsol as u128)
        .checked_mul(supply as u128)
        .and_then(|v| v.checked_div(y0 as u128))
        .ok_or(AmmError::MathOverflow)?;
    let lp = core::cmp::min(lp_from_k, lp_from_s);
    let lp = u64::try_from(lp).map_err(|_| AmmError::MathOverflow)?;
    require!(lp > 0, AmmError::InvalidLiquidity);

    let wsol_used = (lp as u128)
        .checked_mul(y0 as u128)
        .and_then(|v| v.checked_div(supply as u128))
        .ok_or(AmmError::MathOverflow)?;
    let wsol_used = u64::try_from(wsol_used).map_err(|_| AmmError::MathOverflow)?;
    require!(wsol_used > 0 && wsol_used <= amount_wsol, AmmError::InvalidLiquidity);

    transfer_wsol(
        &token_program,
        user_wsol,
        vault_wsol,
        user,
        wsol_used,
        &[],
    )?;
    mint_lp(&token_program, lp_mint, user_lp, pool_ai, lp, signer)?;
    msg!("add landed={} wsol_used={} lp={}", landed, wsol_used, lp);
    Ok(false)
}

pub fn execute_remove<'info>(
    pool: &Pool,
    pool_ai: AccountInfo<'info>,
    user: AccountInfo<'info>,
    mint_kots: AccountInfo<'info>,
    vault_kots: AccountInfo<'info>,
    vault_wsol: AccountInfo<'info>,
    user_kots: AccountInfo<'info>,
    user_wsol: AccountInfo<'info>,
    lp_mint: AccountInfo<'info>,
    user_lp: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    token_2022_program: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    lp_amount: u64,
    kots_decimals: u8,
) -> Result<()> {
    require!(lp_amount > 0, AmmError::InvalidLiquidity);
    require_hook_metas(hook_metas)?;
    require_keys_eq!(vault_kots.key(), pool.vault_kots, AmmError::Unauthorized);
    require_keys_eq!(vault_wsol.key(), pool.vault_wsol, AmmError::Unauthorized);
    require_keys_eq!(lp_mint.key(), pool.lp_mint, AmmError::Unauthorized);

    let x = token_amount(&vault_kots)?;
    let y = token_amount(&vault_wsol)?;
    let supply = mint_supply(&lp_mint)?;
    require!(x > 0 && y > 0 && supply > 0, AmmError::EmptyPool);
    require!(lp_amount <= supply, AmmError::InvalidLiquidity);

    let kots_out = (lp_amount as u128)
        .checked_mul(x as u128)
        .and_then(|v| v.checked_div(supply as u128))
        .ok_or(AmmError::MathOverflow)?;
    let wsol_out = (lp_amount as u128)
        .checked_mul(y as u128)
        .and_then(|v| v.checked_div(supply as u128))
        .ok_or(AmmError::MathOverflow)?;
    let kots_out = u64::try_from(kots_out).map_err(|_| AmmError::MathOverflow)?;
    let wsol_out = u64::try_from(wsol_out).map_err(|_| AmmError::MathOverflow)?;
    require!(kots_out > 0 && wsol_out > 0, AmmError::InvalidLiquidity);

    let seed_slice: &[&[u8]] = &[
        POOL_SEED,
        pool.mint_kots.as_ref(),
        pool.mint_wsol.as_ref(),
        &[pool.bump],
    ];
    let signer: &[&[&[u8]]] = &[seed_slice];

    burn_lp(&token_program, lp_mint, user_lp, user.clone(), lp_amount)?;
    transfer_kots_hooked(
        token_2022_program,
        vault_kots,
        mint_kots,
        user_kots,
        pool_ai.clone(),
        hook_metas,
        kots_out,
        kots_decimals,
        signer,
    )?;
    transfer_wsol(
        &token_program,
        vault_wsol,
        user_wsol,
        pool_ai,
        wsol_out,
        signer,
    )?;
    msg!(
        "remove lp={} kots_out={} wsol_out={}",
        lp_amount,
        kots_out,
        wsol_out
    );
    Ok(())
}
