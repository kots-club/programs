// DESTINATION: kots-contracts/programs/kots-amm/src/swap.rs
//! Sell / buy fee path. Locked: AMM-SPEC §3.3–3.4, V2 / V3.
//!
//! Sell: TransferChecked KOTS in (hooked) → Δ_in = vault post−pre → k →
//!       10+10 of SOL out → user gets remainder. min_out = user_sol.
//! Buy:  10+10 of SOL in → remainder into k → TransferChecked KOTS out →
//!       min_out = user-received (post T22 ceil withhold).
//!
//! Hook extras ride `remaining_accounts` into
//! `spl_token_2022::onchain::invoke_transfer_checked` (same helper as LPM).
//! Fewer than MIN_HOOK_EXTRAS accounts → IncompleteHookAccounts.
//! At most ONE hooked KOTS leg per swap ix (LPM heap rule is two; we use one).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, program::invoke_signed};
use kots_common::constants::{BOUNTY_WALLET, INCOMING_WALLET};

use crate::errors::AmmError;
use crate::math::{quote_buy, quote_sell, MIN_HOOK_EXTRAS};
use crate::state::{Pool, SwapSide, POOL_SEED};

/// SPL Token / Token-2022 account: owner at 32, amount u64 at 64.
fn token_amount(ai: &AccountInfo) -> Result<u64> {
    let data = ai.try_borrow_data()?;
    require!(data.len() >= 72, AmmError::InvalidLiquidity);
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

fn token_owner(ai: &AccountInfo) -> Result<Pubkey> {
    let data = ai.try_borrow_data()?;
    require!(data.len() >= 64, AmmError::InvalidLiquidity);
    Ok(Pubkey::new_from_array(data[32..64].try_into().unwrap()))
}

fn transfer_kots_hooked<'info>(
    token_2022: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    amount: u64,
    decimals: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    spl_token_2022::onchain::invoke_transfer_checked(
        token_2022.key,
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
            d.push(3u8); // TokenInstruction::Transfer
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

/// One exact-in swap. `pool_ai` is the pool PDA AccountInfo (WSOL + KOTS authority).
pub fn execute_swap<'info>(
    pool: &Pool,
    pool_ai: AccountInfo<'info>,
    user: AccountInfo<'info>,
    mint_kots: AccountInfo<'info>,
    vault_kots: AccountInfo<'info>,
    vault_wsol: AccountInfo<'info>,
    user_kots: AccountInfo<'info>,
    user_wsol: AccountInfo<'info>,
    captain_wsol: AccountInfo<'info>,
    bounty_wsol: AccountInfo<'info>,
    token_program: AccountInfo<'info>,
    token_2022_program: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    side: SwapSide,
    amount_in: u64,
    min_out: u64,
    kots_decimals: u8,
) -> Result<()> {
    require!(amount_in > 0, AmmError::InvalidLiquidity);
    require!(
        hook_metas.len() >= MIN_HOOK_EXTRAS,
        AmmError::IncompleteHookAccounts
    );
    require_keys_eq!(vault_kots.key(), pool.vault_kots, AmmError::Unauthorized);
    require_keys_eq!(vault_wsol.key(), pool.vault_wsol, AmmError::Unauthorized);
    require_keys_eq!(
        token_owner(&captain_wsol)?,
        INCOMING_WALLET,
        AmmError::WrongFeeWallet
    );
    require_keys_eq!(
        token_owner(&bounty_wsol)?,
        BOUNTY_WALLET,
        AmmError::WrongFeeWallet
    );

    let seed_slice: &[&[u8]] = &[
        POOL_SEED,
        pool.mint_kots.as_ref(),
        pool.mint_wsol.as_ref(),
        &[pool.bump],
    ];
    let signer: &[&[&[u8]]] = &[seed_slice];

    match side {
        SwapSide::Sell => {
            // Snapshot BEFORE inbound hop (spec §3.4 / K4).
            let x = token_amount(&vault_kots)?;
            let y = token_amount(&vault_wsol)?;
            require!(x > 0 && y > 0, AmmError::InvalidLiquidity);

            transfer_kots_hooked(
                token_2022_program,
                user_kots,
                mint_kots,
                vault_kots.clone(),
                user,
                hook_metas,
                amount_in,
                kots_decimals,
                &[],
            )?;

            let landed = token_amount(&vault_kots)?
                .checked_sub(x)
                .ok_or(AmmError::MathOverflow)?;
            require!(landed > 0, AmmError::InvalidLiquidity);

            let q = quote_sell(x, y, landed).ok_or(AmmError::MathOverflow)?;
            require!(q.user_sol >= min_out, AmmError::Slippage);

            transfer_wsol(
                &token_program,
                vault_wsol.clone(),
                user_wsol,
                pool_ai.clone(),
                q.user_sol,
                signer,
            )?;
            transfer_wsol(
                &token_program,
                vault_wsol.clone(),
                captain_wsol,
                pool_ai.clone(),
                q.captain,
                signer,
            )?;
            transfer_wsol(
                &token_program,
                vault_wsol,
                bounty_wsol,
                pool_ai,
                q.bounty,
                signer,
            )?;
            msg!(
                "sell landed={} sol_out={} cap={} bounty={} user={}",
                q.landed,
                q.sol_out,
                q.captain,
                q.bounty,
                q.user_sol
            );
        }
        SwapSide::Buy => {
            let x = token_amount(&vault_kots)?;
            let y = token_amount(&vault_wsol)?;
            require!(x > 0 && y > 0, AmmError::InvalidLiquidity);

            let q = quote_buy(x, y, amount_in).ok_or(AmmError::MathOverflow)?;
            require!(q.user_kots >= min_out, AmmError::Slippage);

            // User WSOL: full amount_in to vault, then vault pays captain+bounty
            // so the curve keeps into_k and one user approval covers the fee.
            transfer_wsol(
                &token_program,
                user_wsol,
                vault_wsol.clone(),
                user,
                amount_in,
                &[],
            )?;
            transfer_wsol(
                &token_program,
                vault_wsol.clone(),
                captain_wsol,
                pool_ai.clone(),
                q.captain,
                signer,
            )?;
            transfer_wsol(
                &token_program,
                vault_wsol,
                bounty_wsol,
                pool_ai.clone(),
                q.bounty,
                signer,
            )?;

            // Vault → user KOTS (hooked). User credits q.user_kots after T22 ceil.
            transfer_kots_hooked(
                token_2022_program,
                vault_kots,
                mint_kots,
                user_kots,
                pool_ai,
                hook_metas,
                q.kots_out,
                kots_decimals,
                signer,
            )?;
            msg!(
                "buy into_k={} kots_out={} fee={} user={} cap={} bounty={}",
                q.into_k,
                q.kots_out,
                q.t22_fee,
                q.user_kots,
                q.captain,
                q.bounty
            );
        }
    }
    Ok(())
}
