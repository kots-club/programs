// DESTINATION: kots-contracts/programs/kots-amm/src/lib.rs
//! KOTS AMM — constant-product KOTS/WSOL pool (program #10).
//!
//! Spec: `docs/AMM-SPEC.md`. Plan: `KOTS-AMM-PROJECT-PLAN.md`.
//! First add_liquidity is a hooked TransferChecked from the user ATA.
//! Mint authority on mainnet 5iKK is unset — never MintTo KOTS into the vault.

use anchor_lang::prelude::*;

pub mod errors;
pub mod hook;
pub mod initialize;
pub mod math;
pub mod liquidity;
pub mod state;
pub mod swap;

use errors::AmmError;
use initialize::{
    assert_classic_mint, assert_token_2022_mint, assert_token_account_owned_by,
    log_registry_target, populate_pool, Populate, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use state::{Pool, SwapSide, POOL_SEED};

include!("declared_id.rs");

#[program]
pub mod kots_amm {
    use super::*;

    pub fn initialize_pool(ctx: Context<InitializePool>) -> Result<()> {
        assert_token_2022_mint(&ctx.accounts.mint_kots.to_account_info())?;
        assert_classic_mint(&ctx.accounts.lp_mint.to_account_info())?;
        require_keys_eq!(
            *ctx.accounts.mint_wsol.owner,
            TOKEN_PROGRAM_ID,
            AmmError::WrongQuoteMint
        );

        let pool_key = ctx.accounts.pool.key();
        assert_token_account_owned_by(
            &ctx.accounts.vault_kots.to_account_info(),
            &pool_key,
            ctx.accounts.mint_kots.key,
            &TOKEN_2022_PROGRAM_ID,
        )?;
        assert_token_account_owned_by(
            &ctx.accounts.vault_wsol.to_account_info(),
            &pool_key,
            ctx.accounts.mint_wsol.key,
            &TOKEN_PROGRAM_ID,
        )?;

        populate_pool(
            &mut ctx.accounts.pool,
            ctx.accounts.authority.key(),
            Populate {
                mint_kots: ctx.accounts.mint_kots.key(),
                mint_wsol: ctx.accounts.mint_wsol.key(),
                vault_kots: ctx.accounts.vault_kots.key(),
                vault_wsol: ctx.accounts.vault_wsol.key(),
                lp_mint: ctx.accounts.lp_mint.key(),
                bump: ctx.bumps.pool,
            },
        )?;
        log_registry_target(ctx.accounts.vault_kots.key);
        Ok(())
    }

    pub fn swap<'info>(
        ctx: Context<'_, '_, '_, 'info, Swap<'info>>,
        side: SwapSide,
        amount_in: u64,
        min_out: u64,
    ) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::Paused);
        hook::require_hook_metas(ctx.remaining_accounts)?;
        let decimals = {
            let data = ctx.accounts.mint_kots.try_borrow_data()?;
            require!(data.len() >= 45, AmmError::UnhookedMint);
            data[44]
        };
        crate::swap::execute_swap(
            &ctx.accounts.pool,
            ctx.accounts.pool.to_account_info(),
            ctx.accounts.user.to_account_info(),
            ctx.accounts.mint_kots.to_account_info(),
            ctx.accounts.vault_kots.to_account_info(),
            ctx.accounts.vault_wsol.to_account_info(),
            ctx.accounts.user_kots.to_account_info(),
            ctx.accounts.user_wsol.to_account_info(),
            ctx.accounts.captain_wsol.to_account_info(),
            ctx.accounts.bounty_wsol.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.remaining_accounts,
            side,
            amount_in,
            min_out,
            decimals,
        )
    }

    pub fn add_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, AddLiquidity<'info>>,
        amount_kots: u64,
        amount_wsol: u64,
    ) -> Result<()> {
        hook::require_hook_metas(ctx.remaining_accounts)?;
        let decimals = {
            let data = ctx.accounts.mint_kots.try_borrow_data()?;
            require!(data.len() >= 45, AmmError::UnhookedMint);
            data[44]
        };
        let now_seeded = crate::liquidity::execute_add(
            &ctx.accounts.pool,
            ctx.accounts.pool.to_account_info(),
            ctx.accounts.user.to_account_info(),
            ctx.accounts.mint_kots.to_account_info(),
            ctx.accounts.vault_kots.to_account_info(),
            ctx.accounts.vault_wsol.to_account_info(),
            ctx.accounts.user_kots.to_account_info(),
            ctx.accounts.user_wsol.to_account_info(),
            ctx.accounts.lp_mint.to_account_info(),
            ctx.accounts.user_lp.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.remaining_accounts,
            amount_kots,
            amount_wsol,
            decimals,
        )?;
        if now_seeded {
            ctx.accounts.pool.seeded = true;
        }
        Ok(())
    }

    pub fn remove_liquidity<'info>(
        ctx: Context<'_, '_, '_, 'info, RemoveLiquidity<'info>>,
        lp_amount: u64,
    ) -> Result<()> {
        hook::require_hook_metas(ctx.remaining_accounts)?;
        let decimals = {
            let data = ctx.accounts.mint_kots.try_borrow_data()?;
            require!(data.len() >= 45, AmmError::UnhookedMint);
            data[44]
        };
        crate::liquidity::execute_remove(
            &ctx.accounts.pool,
            ctx.accounts.pool.to_account_info(),
            ctx.accounts.user.to_account_info(),
            ctx.accounts.mint_kots.to_account_info(),
            ctx.accounts.vault_kots.to_account_info(),
            ctx.accounts.vault_wsol.to_account_info(),
            ctx.accounts.user_kots.to_account_info(),
            ctx.accounts.user_wsol.to_account_info(),
            ctx.accounts.lp_mint.to_account_info(),
            ctx.accounts.user_lp.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.token_2022_program.to_account_info(),
            ctx.remaining_accounts,
            lp_amount,
            decimals,
        )
    }

    pub fn pause(ctx: Context<Admin>) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.authority.key(),
            ctx.accounts.pool.authority,
            AmmError::Unauthorized
        );
        ctx.accounts.pool.paused = true;
        Ok(())
    }

    pub fn unpause(ctx: Context<Admin>) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.authority.key(),
            ctx.accounts.pool.authority,
            AmmError::Unauthorized
        );
        ctx.accounts.pool.paused = false;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = Pool::SPACE,
        seeds = [POOL_SEED, mint_kots.key().as_ref(), mint_wsol.key().as_ref()],
        bump
    )]
    pub pool: Account<'info, Pool>,
    /// CHECK: Token-2022 hooked fixture mint. Handler asserts owner + allow-list.
    pub mint_kots: UncheckedAccount<'info>,
    /// CHECK: WSOL mint. Handler asserts So111… + classic SPL owner.
    pub mint_wsol: UncheckedAccount<'info>,
    /// CHECK: ATA of pool PDA (Token-2022). Created by prepare. Register this
    /// pubkey on the hook AFTER this ix (`register_pool`, never protocol_pool).
    pub vault_kots: UncheckedAccount<'info>,
    /// CHECK: ATA of pool PDA (classic SPL / WSOL).
    pub vault_wsol: UncheckedAccount<'info>,
    /// CHECK: unhooked classic SPL mint. Handler asserts Tokenkeg owner.
    pub lp_mint: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Swap<'info> {
    pub user: Signer<'info>,
    #[account(
        seeds = [POOL_SEED, pool.mint_kots.as_ref(), pool.mint_wsol.as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    /// CHECK: Token-2022 hooked fixture mint.
    pub mint_kots: UncheckedAccount<'info>,
    /// CHECK: Token-2022 KOTS vault. Remaining accounts = hook extras.
    #[account(mut)]
    pub vault_kots: UncheckedAccount<'info>,
    /// CHECK: WSOL vault.
    #[account(mut)]
    pub vault_wsol: UncheckedAccount<'info>,
    /// CHECK: user KOTS ATA (Token-2022).
    #[account(mut)]
    pub user_kots: UncheckedAccount<'info>,
    /// CHECK: user WSOL ATA.
    #[account(mut)]
    pub user_wsol: UncheckedAccount<'info>,
    /// CHECK: captain WSOL ATA. Owner must be INCOMING_WALLET.
    #[account(mut)]
    pub captain_wsol: UncheckedAccount<'info>,
    /// CHECK: bounty WSOL ATA. Owner must be BOUNTY_WALLET.
    #[account(mut)]
    pub bounty_wsol: UncheckedAccount<'info>,
    /// CHECK: classic SPL Token program (WSOL).
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: Token-2022 program (KOTS).
    pub token_2022_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    pub user: Signer<'info>,
    #[account(
        mut,
        seeds = [POOL_SEED, pool.mint_kots.as_ref(), pool.mint_wsol.as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    /// CHECK: hooked KOTS mint.
    pub mint_kots: UncheckedAccount<'info>,
    /// CHECK: KOTS vault.
    #[account(mut)]
    pub vault_kots: UncheckedAccount<'info>,
    /// CHECK: WSOL vault.
    #[account(mut)]
    pub vault_wsol: UncheckedAccount<'info>,
    /// CHECK: user KOTS ATA.
    #[account(mut)]
    pub user_kots: UncheckedAccount<'info>,
    /// CHECK: user WSOL ATA.
    #[account(mut)]
    pub user_wsol: UncheckedAccount<'info>,
    /// CHECK: unhooked LP mint.
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,
    /// CHECK: user LP ATA.
    #[account(mut)]
    pub user_lp: UncheckedAccount<'info>,
    /// CHECK: classic SPL.
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: Token-2022.
    pub token_2022_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    pub user: Signer<'info>,
    #[account(
        seeds = [POOL_SEED, pool.mint_kots.as_ref(), pool.mint_wsol.as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
    /// CHECK: hooked KOTS mint.
    pub mint_kots: UncheckedAccount<'info>,
    /// CHECK: KOTS vault.
    #[account(mut)]
    pub vault_kots: UncheckedAccount<'info>,
    /// CHECK: WSOL vault.
    #[account(mut)]
    pub vault_wsol: UncheckedAccount<'info>,
    /// CHECK: user KOTS ATA.
    #[account(mut)]
    pub user_kots: UncheckedAccount<'info>,
    /// CHECK: user WSOL ATA.
    #[account(mut)]
    pub user_wsol: UncheckedAccount<'info>,
    /// CHECK: unhooked LP mint.
    #[account(mut)]
    pub lp_mint: UncheckedAccount<'info>,
    /// CHECK: user LP ATA.
    #[account(mut)]
    pub user_lp: UncheckedAccount<'info>,
    /// CHECK: classic SPL.
    pub token_program: UncheckedAccount<'info>,
    /// CHECK: Token-2022.
    pub token_2022_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Admin<'info> {
    pub authority: Signer<'info>,
    #[account(
        mut,
        seeds = [POOL_SEED, pool.mint_kots.as_ref(), pool.mint_wsol.as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,
}
