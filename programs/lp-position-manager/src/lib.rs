// DESTINATION (where this file goes): kots-contracts/programs/lp-position-manager/src/lib.rs
//! # 2026-08-21 — u128 intermediate arithmetic (MathOverflow class)
//!
//! `lp_bps` and the SOL distribute path previously used pure `u64` `checked_mul`.
//! That is the same overflow class that produced `MathOverflow (6030)` in the
//! transfer-hook at mainnet supply scale. Both sites (and `split::apply_bps`)
//! now multiply in `u128` then narrow. Rebuild with `--features realm-main`
//! and Squad-upgrade before any large LP fee distribution.
use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};

// Pure LP fee-split arithmetic, extracted for validator-free unit testing
// (`cargo test -p lp-position-manager split`). This program had NO coverage of any
// kind — no unit tests, no integration tests, and it is not a dependency of the
// integration test crate — despite splitting real fee revenue.
//
// The KOTS and SOL tables below were RESYNCHRONISED with the transfer hook on
// 2026-07-22. This program had been left on the pre-correction figures (incoming
// 34% / treasury 7% / burn 3%) after the hook was corrected to 7% / 36% / 1%, so LP
// revenue routed the Captain's share inverted. Both tables summed to 10_000, so
// neither looked wrong alone and nothing compared them.
// split.rs::kots_split_matches_the_transfer_hook_table now guards the pairing.
pub mod split;

declare_id!("CaxiD5ViyLyFsX2ogRaStgFi2R3FdTM5t1BiRqeSQRGP");

// ============================================================================
// CONSTANTS
// ============================================================================

// SOL fee distribution percentages (basis points out of 10000)
const JACKPOT_BPS_SOL: u64 = 5000; // 50%
const INCOMING_BPS_SOL: u64 = 700; // 7% — Captain's share, uniform with KOTS. 2026-07-22: CORRECTED from an inverted 34%.
const ADMIN_BPS_SOL: u64 = 300; // 3%
const TREASURY_BPS_SOL: u64 = 3700; // 37% (includes the 3% burn-redirect — SOL is not burned). 2026-07-22: was 10%; absorbed the corrected incoming share.
const DEV_BPS_SOL: u64 = 300; // 3%

// KOTS fee distribution percentages (basis points out of 10000)
const JACKPOT_BPS_KOTS: u64 = 5000; // 50%
const INCOMING_BPS_KOTS: u64 = 700; // 7% — Captain's share (main investor profit). 2026-07-22: CORRECTED from an inverted 34%; this and treasury were swapped.
const ADMIN_BPS_KOTS: u64 = 300; // 3%
const TREASURY_BPS_KOTS: u64 = 3500; // 35%. 2026-08-07: 3600 -> 3500, carving the 1% owner bounty from treasury (fee-management precedent).
const DEV_BPS_KOTS: u64 = 300; // 3%
const BURN_BPS_KOTS: u64 = 100; // 1% (2026-07-22: reduced from 3%, per the operations ruling)
const BOUNTY_BPS_KOTS: u64 = 100; // 1% owner bounty (2026-08-07). Recipient = kots_common BOUNTY_WALLET, fee-exempt in the hook. The seven KOTS legs sum to 10000.
// Owner bounty recipient = kots_common::constants::BOUNTY_WALLET (single source of truth,
// imported directly below now that this crate depends on kots-common).

// PDA seeds
const MANAGER_SEED: &[u8] = b"lp_manager";

// EXPECTED WARNING — do not "fix" this by adding #![allow(deprecated)].
//
// Anchor 0.31.1's #[program] macro expands to a call to the deprecated
// AccountInfo::realloc. The call is in GENERATED code emitted at crate root, not
// inside this module, so a module-level #[allow(deprecated)] does not reach it —
// tried 2026-07-28, the warning was unchanged.
//
// The only thing that silences it is a crate-level #![allow(deprecated)], and
// that is a bad trade here: `solana_program::system_instruction` is being
// deprecated in favour of the `solana-system-interface` crate, six of these
// programs use it, and the test crate already warns about it. Blanket-silencing
// would hide that migration the moment it lands on the program crates.
//
// One unactionable warning per program is the correct cost. It goes away on the
// next Anchor upgrade, not before.
#[program]
pub mod lp_position_manager {
    use super::*;

    /// Initialize the LP Position Manager
    /// 
    /// **PURPOSE:** Set up the manager PDA that will own the Raydium CLMM position.
    /// This PDA has authority to collect fees and distribute them.
    pub fn initialize_manager(ctx: Context<InitializeManager>) -> Result<()> {
        let manager = &mut ctx.accounts.manager;
        
        manager.authority = ctx.accounts.authority.key();
        manager.bump = ctx.bumps.manager;
        
        // Hardcoded distribution wallets (same as transfer hook)
        // Single source of truth: kots_common::constants (shared with fee-management + the
        // hook whitelist). lp no longer hardcodes its own copy.
        manager.jackpot_wallet = kots_common::constants::JACKPOT_WALLET;
        manager.incoming_wallet = kots_common::constants::INCOMING_WALLET;
        manager.admin_wallet = kots_common::constants::ADMIN_WALLET;
        manager.treasury_wallet = kots_common::constants::TREASURY_WALLET;
        manager.dev_wallet = kots_common::constants::DEV_WALLET;
        manager.burn_address = kots_common::constants::BURN_ADDRESS;
        
        manager.total_fees_collected_sol = 0;
        manager.total_fees_collected_kots = 0;
        manager.last_distribution = Clock::get()?.unix_timestamp;
        
        msg!("✓ LP Position Manager initialized");
        msg!("  Manager PDA: {}", manager.key());
        msg!("  Authority: {}", manager.authority);
        msg!("  → This PDA will own the Raydium CLMM position");
        
        Ok(())
    }

    /// Distribute accumulated SOL LP fees to 5 wallets
    /// Split into separate instruction to reduce BPF stack usage
    pub fn distribute_sol_lp_fees(ctx: Context<DistributeSOLFees>) -> Result<()> {
        let manager = &mut ctx.accounts.manager;
        let sol_balance = ctx.accounts.manager_sol_account.lamports();
        
        require!(sol_balance > 0, ErrorCode::NoFeesToDistribute);
        msg!("Distributing {} lamports SOL fees", sol_balance);
        
        distribute_sol_fees(
            &ctx.accounts.manager_sol_account,
            &ctx.accounts.jackpot_sol,
            &ctx.accounts.incoming_sol,
            &ctx.accounts.admin_sol,
            &ctx.accounts.treasury_sol,
            &ctx.accounts.dev_sol,
            &ctx.accounts.system_program,
            sol_balance,
            manager.bump,
        )?;
        
        manager.total_fees_collected_sol = manager
            .total_fees_collected_sol
            .checked_add(sol_balance)
            .ok_or(ErrorCode::MathOverflow)?;
        manager.last_distribution = Clock::get()?.unix_timestamp;
        
        Ok(())
    }

    /// Distribute accumulated KOTS LP fees to 6 wallets
    /// Split into separate instruction to reduce BPF stack usage
    /// GATED (2026-08-07). The monolithic 6-leg distribution cannot execute against
    /// the hooked KOTS mint: Token-2022 refuses more than ~2 hooked CPI legs per
    /// instruction (measured in fee-management — four legs die on leg 3, CU + the
    /// 32 KB Anchor heap). Call `distribute_kots_lp_fees_part1..4` in ONE transaction
    /// instead. Discriminator retained for any client still holding it.
    pub fn distribute_kots_lp_fees(_ctx: Context<DistributeKOTSFees>) -> Result<()> {
        Err(ErrorCode::MonolithicDistributionGated.into())
    }

    /// KOTS LP-fee distribution, PART 1 of 4 — jackpot 50% + incoming 7%. Two hooked legs (the
    /// Token-2022 ceiling). Shares from `total_amount` (caller snapshots the balance
    /// once, passes it to all four parts in ONE tx; balance drains between parts) and
    /// the caller forwards the hook account set as remaining_accounts + a compute bump.
    pub fn distribute_kots_lp_fees_part1<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributeKOTSFeesPart1<'info>>,
        total_amount: u64,
    ) -> Result<()> {
        require!(total_amount > 0, ErrorCode::NoFeesToDistribute);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin destinations to the manager's canonical wallets (a mirror of
        // kots_common::constants). A destination owned by anything else is rejected.
        require_keys_eq!(ctx.accounts.jackpot_kots.owner, ctx.accounts.manager.jackpot_wallet, ErrorCode::WrongDistributionWallet);
        require_keys_eq!(ctx.accounts.incoming_kots.owner, ctx.accounts.manager.incoming_wallet, ErrorCode::WrongDistributionWallet);
        let signer_seeds: &[&[&[u8]]] = &[&[MANAGER_SEED, &[ctx.accounts.manager.bump]]];
        let hook_metas = ctx.remaining_accounts;
        for (to, bps, label) in [
            (ctx.accounts.jackpot_kots.to_account_info(), JACKPOT_BPS_KOTS, "jackpot"),
            (ctx.accounts.incoming_kots.to_account_info(), INCOMING_BPS_KOTS, "incoming"),
        ] {
            let amount = lp_bps(total_amount, bps)?;
            lp_transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.manager_kots_account.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                to,
                ctx.accounts.manager.to_account_info(),
                hook_metas,
                amount,
                decimals,
                signer_seeds,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        msg!("✅ LP KOTS fees part 1/4 complete");
        Ok(())
    }

    /// KOTS LP-fee distribution, PART 2 of 4 — admin 3% + treasury 35%. Two hooked legs (the
    /// Token-2022 ceiling). Shares from `total_amount` (caller snapshots the balance
    /// once, passes it to all four parts in ONE tx; balance drains between parts) and
    /// the caller forwards the hook account set as remaining_accounts + a compute bump.
    pub fn distribute_kots_lp_fees_part2<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributeKOTSFeesPart2<'info>>,
        total_amount: u64,
    ) -> Result<()> {
        require!(total_amount > 0, ErrorCode::NoFeesToDistribute);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin destinations to the manager's canonical wallets (a mirror of
        // kots_common::constants). A destination owned by anything else is rejected.
        require_keys_eq!(ctx.accounts.admin_kots.owner, ctx.accounts.manager.admin_wallet, ErrorCode::WrongDistributionWallet);
        require_keys_eq!(ctx.accounts.treasury_kots.owner, ctx.accounts.manager.treasury_wallet, ErrorCode::WrongDistributionWallet);
        let signer_seeds: &[&[&[u8]]] = &[&[MANAGER_SEED, &[ctx.accounts.manager.bump]]];
        let hook_metas = ctx.remaining_accounts;
        for (to, bps, label) in [
            (ctx.accounts.admin_kots.to_account_info(), ADMIN_BPS_KOTS, "admin"),
            (ctx.accounts.treasury_kots.to_account_info(), TREASURY_BPS_KOTS, "treasury"),
        ] {
            let amount = lp_bps(total_amount, bps)?;
            lp_transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.manager_kots_account.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                to,
                ctx.accounts.manager.to_account_info(),
                hook_metas,
                amount,
                decimals,
                signer_seeds,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        msg!("✅ LP KOTS fees part 2/4 complete");
        Ok(())
    }

    /// KOTS LP-fee distribution, PART 3 of 4 — dev 3% + burn 1%. Two hooked legs (the
    /// Token-2022 ceiling). Shares from `total_amount` (caller snapshots the balance
    /// once, passes it to all four parts in ONE tx; balance drains between parts) and
    /// the caller forwards the hook account set as remaining_accounts + a compute bump.
    pub fn distribute_kots_lp_fees_part3<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributeKOTSFeesPart3<'info>>,
        total_amount: u64,
    ) -> Result<()> {
        require!(total_amount > 0, ErrorCode::NoFeesToDistribute);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin destinations to the manager's canonical wallets (a mirror of
        // kots_common::constants). A destination owned by anything else is rejected.
        require_keys_eq!(ctx.accounts.dev_kots.owner, ctx.accounts.manager.dev_wallet, ErrorCode::WrongDistributionWallet);
        require_keys_eq!(ctx.accounts.burn_kots.owner, ctx.accounts.manager.burn_address, ErrorCode::WrongDistributionWallet);
        let signer_seeds: &[&[&[u8]]] = &[&[MANAGER_SEED, &[ctx.accounts.manager.bump]]];
        let hook_metas = ctx.remaining_accounts;
        for (to, bps, label) in [
            (ctx.accounts.dev_kots.to_account_info(), DEV_BPS_KOTS, "dev"),
            (ctx.accounts.burn_kots.to_account_info(), BURN_BPS_KOTS, "burn"),
        ] {
            let amount = lp_bps(total_amount, bps)?;
            lp_transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.manager_kots_account.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                to,
                ctx.accounts.manager.to_account_info(),
                hook_metas,
                amount,
                decimals,
                signer_seeds,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        msg!("✅ LP KOTS fees part 3/4 complete");
        Ok(())
    }

    /// KOTS LP-fee distribution, PART 4 of 4 — owner bounty (1%), then books the
    /// statistics. Run parts 1..4 in ONE transaction so the booking reverts with any
    /// same-tx failure.
    pub fn distribute_kots_lp_fees_part4<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributeKOTSFeesPart4<'info>>,
        total_amount: u64,
    ) -> Result<()> {
        require!(total_amount > 0, ErrorCode::NoFeesToDistribute);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin the bounty destination to the canonical BOUNTY_WALLET.
        require_keys_eq!(ctx.accounts.bounty_kots.owner, kots_common::constants::BOUNTY_WALLET, ErrorCode::WrongDistributionWallet);
        let signer_seeds: &[&[&[u8]]] = &[&[MANAGER_SEED, &[ctx.accounts.manager.bump]]];
        let bounty_amount = lp_bps(total_amount, BOUNTY_BPS_KOTS)?;
        lp_transfer_one_leg(
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.manager_kots_account.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.bounty_kots.to_account_info(),
            ctx.accounts.manager.to_account_info(),
            ctx.remaining_accounts,
            bounty_amount,
            decimals,
            signer_seeds,
        )?;
        msg!("  -> bounty {} raw", bounty_amount);
        let manager = &mut ctx.accounts.manager;
        manager.total_fees_collected_kots = manager
            .total_fees_collected_kots
            .checked_add(total_amount)
            .ok_or(ErrorCode::MathOverflow)?;
        manager.last_distribution = Clock::get()?.unix_timestamp;
        msg!("✅ LP KOTS fees part 4/4 (owner bounty) complete — total {}", total_amount);
        Ok(())
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// Distribute SOL fees to 5 wallets
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn distribute_sol_fees<'info>(
    manager_account: &AccountInfo<'info>,
    jackpot: &AccountInfo<'info>,
    incoming: &AccountInfo<'info>,
    admin: &AccountInfo<'info>,
    treasury: &AccountInfo<'info>,
    dev: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    total_amount: u64,
    bump: u8,
) -> Result<()> {
    msg!("Distributing SOL fees...");
    
    // Calculate amounts — u128 intermediate (same class of bug as transfer-hook
    // MathOverflow 6030: u64 checked_mul overflows before the divide at scale).
    let jackpot_amount = (total_amount as u128)
        .checked_mul(JACKPOT_BPS_SOL as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    
    let incoming_amount = (total_amount as u128)
        .checked_mul(INCOMING_BPS_SOL as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    
    let admin_amount = (total_amount as u128)
        .checked_mul(ADMIN_BPS_SOL as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    
    let treasury_amount = (total_amount as u128)
        .checked_mul(TREASURY_BPS_SOL as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    
    let dev_amount = (total_amount as u128)
        .checked_mul(DEV_BPS_SOL as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    
    // Prepare signer seeds
    let signer_seeds: &[&[&[u8]]] = &[&[MANAGER_SEED, &[bump]]];
    
    // Transfer to each wallet
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program.clone(),
            system_program::Transfer {
                from: manager_account.clone(),
                to: jackpot.clone(),
            },
            signer_seeds,
        ),
        jackpot_amount,
    )?;
    msg!("  ✓ Jackpot: {} SOL (50%)", jackpot_amount as f64 / 1e9);
    
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program.clone(),
            system_program::Transfer {
                from: manager_account.clone(),
                to: incoming.clone(),
            },
            signer_seeds,
        ),
        incoming_amount,
    )?;
    msg!("  ✓ Incoming: {} SOL (34%)", incoming_amount as f64 / 1e9);
    
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program.clone(),
            system_program::Transfer {
                from: manager_account.clone(),
                to: admin.clone(),
            },
            signer_seeds,
        ),
        admin_amount,
    )?;
    msg!("  ✓ Admin: {} SOL (3%)", admin_amount as f64 / 1e9);
    
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program.clone(),
            system_program::Transfer {
                from: manager_account.clone(),
                to: treasury.clone(),
            },
            signer_seeds,
        ),
        treasury_amount,
    )?;
    msg!("  ✓ Treasury: {} SOL (10%)", treasury_amount as f64 / 1e9);
    
    system_program::transfer(
        CpiContext::new_with_signer(
            system_program.clone(),
            system_program::Transfer {
                from: manager_account.clone(),
                to: dev.clone(),
            },
            signer_seeds,
        ),
        dev_amount,
    )?;
    msg!("  ✓ Dev: {} SOL (3%)", dev_amount as f64 / 1e9);
    
    Ok(())
}

/// Distribute KOTS fees to 6 wallets
#[inline(never)]
#[allow(clippy::too_many_arguments)]
/// One hooked KOTS leg via the Token-2022 hook-aware helper. Borrows `hook_metas`
/// (never clones — a per-leg clone is what exhausts the 32 KB heap). Split
/// invariant: at most TWO of these per top-level instruction.
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn lp_transfer_one_leg<'info>(
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

/// floor(total * bps / 10_000), checked.
///
/// u128 intermediate — identical fix class as transfer-hook MathOverflow 6030
/// and the SOL distribute path above. A pure `u64` `checked_mul` overflows for
/// any total ≳ `u64::MAX / bps` (mainnet supply scale is already past that for
/// modest bps).
fn lp_bps(total: u64, bps: u64) -> Result<u64> {
    let v = (total as u128)
        .checked_mul(bps as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(ErrorCode::MathOverflow)?;
    Ok(v)
}

// ============================================================================
// ACCOUNT STRUCTURES
// ============================================================================

#[derive(Accounts)]
pub struct InitializeManager<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + LPManager::INIT_SPACE,
        seeds = [MANAGER_SEED],
        bump
    )]
    pub manager: Account<'info, LPManager>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DistributeSOLFees<'info> {
    #[account(
        mut,
        seeds = [MANAGER_SEED],
        bump = manager.bump,
        has_one = authority
    )]
    pub manager: Account<'info, LPManager>,
    
    pub authority: Signer<'info>,
    
    /// CHECK: Manager PDA account holds SOL
    #[account(mut, seeds = [MANAGER_SEED], bump = manager.bump)]
    pub manager_sol_account: AccountInfo<'info>,
    
    /// CHECK: Jackpot wallet
    #[account(mut)]
    pub jackpot_sol: AccountInfo<'info>,
    
    /// CHECK: Incoming wallet
    #[account(mut)]
    pub incoming_sol: AccountInfo<'info>,
    
    /// CHECK: Admin wallet
    #[account(mut)]
    pub admin_sol: AccountInfo<'info>,
    
    /// CHECK: Treasury wallet
    #[account(mut)]
    pub treasury_sol: AccountInfo<'info>,
    
    /// CHECK: Dev wallet
    #[account(mut)]
    pub dev_sol: AccountInfo<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DistributeKOTSFees<'info> {
    #[account(
        mut,
        seeds = [MANAGER_SEED],
        bump = manager.bump,
        has_one = authority
    )]
    pub manager: Account<'info, LPManager>,
    
    pub authority: Signer<'info>,
    
    #[account(mut)]
    pub manager_kots_account: InterfaceAccount<'info, TokenAccount>,
    
    pub kots_mint: InterfaceAccount<'info, Mint>,
    
    #[account(mut)]
    pub jackpot_kots: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub incoming_kots: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub admin_kots: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub treasury_kots: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub dev_kots: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub burn_kots: InterfaceAccount<'info, TokenAccount>,
    
    pub token_program: Interface<'info, TokenInterface>,
}

#[account]
#[derive(InitSpace)]
pub struct LPManager {
    pub authority: Pubkey,
    pub bump: u8,
    
    // Distribution wallets (hardcoded, same as transfer hook)
    pub jackpot_wallet: Pubkey,
    pub incoming_wallet: Pubkey,
    pub admin_wallet: Pubkey,
    pub treasury_wallet: Pubkey,
    pub dev_wallet: Pubkey,
    pub burn_address: Pubkey,
    
    // Tracking
    pub total_fees_collected_sol: u64,
    pub total_fees_collected_kots: u64,
    pub last_distribution: i64,
}

// ============================================================================
// ERRORS
// ============================================================================

#[error_code]
pub enum ErrorCode {
    #[msg("No fees available to distribute")]
    NoFeesToDistribute,
    
    #[msg("Math overflow in fee calculation")]
    MathOverflow,

    #[msg("Monolithic KOTS fee distribution is gated; call distribute_kots_lp_fees_part1..4 in one transaction")]
    MonolithicDistributionGated,

    #[msg("Fee-distribution destination is not the canonical wallet for this leg")]
    WrongDistributionWallet,
}

// ============================================================================
// SPLIT KOTS FEE-DISTRIBUTION ACCOUNT STRUCTS (2026-08-07) — minimal accounts per
// part keeps the hooked-transfer heap in budget; all share the manager PDA.
// ============================================================================

#[derive(Accounts)]
pub struct DistributeKOTSFeesPart1<'info> {
    #[account(mut, seeds = [MANAGER_SEED], bump = manager.bump, has_one = authority)]
    pub manager: Account<'info, LPManager>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub manager_kots_account: InterfaceAccount<'info, TokenAccount>,
    pub kots_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub jackpot_kots: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub incoming_kots: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct DistributeKOTSFeesPart2<'info> {
    #[account(mut, seeds = [MANAGER_SEED], bump = manager.bump, has_one = authority)]
    pub manager: Account<'info, LPManager>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub manager_kots_account: InterfaceAccount<'info, TokenAccount>,
    pub kots_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub admin_kots: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub treasury_kots: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct DistributeKOTSFeesPart3<'info> {
    #[account(mut, seeds = [MANAGER_SEED], bump = manager.bump, has_one = authority)]
    pub manager: Account<'info, LPManager>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub manager_kots_account: InterfaceAccount<'info, TokenAccount>,
    pub kots_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub dev_kots: InterfaceAccount<'info, TokenAccount>,
    #[account(mut)]
    pub burn_kots: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct DistributeKOTSFeesPart4<'info> {
    #[account(mut, seeds = [MANAGER_SEED], bump = manager.bump, has_one = authority)]
    pub manager: Account<'info, LPManager>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub manager_kots_account: InterfaceAccount<'info, TokenAccount>,
    pub kots_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub bounty_kots: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}
