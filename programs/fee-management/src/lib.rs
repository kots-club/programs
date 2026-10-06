// DESTINATION (where this file goes): kots-contracts/programs/fee-management/src/lib.rs
// kots-contracts/programs/fee-management/src/lib.rs
//! KOTS Fee Management Program
//! 
//! Handles transaction tax collection, fee distribution to 4 wallets,
//! and trading tax processing.
//!
//! # 2026-08-21 — u128 intermediate arithmetic (MathOverflow class)
//!
//! All BPS splits now route through a local `apply_bps` that multiplies in
//! `u128` then narrows. The previous pure-`u64` paths (both the gated monolith's
//! raw `*` and the live `safe_math::calculate_bps` call sites) were the same
//! overflow class that produced `MathOverflow (6030)` on the transfer-hook at
//! mainnet supply scale. Regenerate this file into the contracts tree and
//! rebuild with `--features realm-main` before the next Squad upgrade.

// Prevent both features from being enabled simultaneously during BPF compilation
// Allow IDL generation to proceed with both features if needed
#[cfg(all(feature = "idl-build", feature = "bpf-runtime", target_arch = "bpf"))]
compile_error!("Cannot enable both 'idl-build' and 'bpf-runtime' features simultaneously during BPF compilation");

use anchor_lang::prelude::*;
// TOKEN-2022 PORT (2026-07-24): was `anchor_spl::token::{self, Token, TokenAccount}`.
// KOTS is a Token-2022 mint WITH a transfer hook. The legacy types cannot
// deserialize a Token-2022 account, `Program<'info, Token>` pins a program id
// that does not own the mint, and `token::transfer` is not the instruction
// Token-2022 expects. The six transfers in execute_fee_distribution were
// therefore guaranteed to fail at runtime — the same defect already found and
// fixed in progressive-jackpot's award_jackpot.
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use kots_common::{
    constants::*, errors::*, utils::*,
    ProgramType, // Import ProgramType from root, not upgrade module
    init::{InitOptions},
    // BlacklistState import removed (2026-07-24): its only use was the unused
    // `_blacklist_state: Option<BlacklistState>` argument on
    // execute_fee_distribution, which put ~1120 bytes of a 4KB BPF frame on the
    // stack for nothing. FeeDistributionState is deliberately not imported here —
    // it would conflict with the local definition.
};

// Program-local account size constants for IDL generation
const FEE_DISTRIBUTION_SPACE: usize = 384;  // 8 (discriminator) + 376 (FeeDistributionState)

// Account struct definitions at root level for proper IDL generation

#[account]
#[derive(Debug)]
pub struct FeeDistributionState {
    pub is_initialized: bool,
    // Fee-distribution destinations are NOT stored here. Each distribution
    // instruction receives its destination as a caller-supplied account and pins
    // it to the canonical wallet via
    // require_keys_eq!(dest.owner, kots_common::constants::<WALLET>) — the single
    // source of truth. The former jackpot_pool / incoming_wallet / admin_wallet /
    // treasury_wallet / burn_address / development_fund fields were written by
    // init_fee and never read on any path; removed (later 20).
    //
    // The trading_*_bps fields were removed (later 26): their only reader was
    // proc_tax, which has been removed (the chain is the ledger — no parallel tax
    // accounting). The trading DISTRIBUTION parts hardcode their split via the
    // TRADING_*_DISTRIBUTION_BPS constants; only contest_*_bps remain here, read by
    // the contest parts.
    pub contest_treasury_bps: u64,
    pub contest_development_bps: u64,
    pub contest_jackpot_bps: u64,
    pub contest_burn_bps: u64,
    pub jackpot_distribution_bps: u64,
    pub specified_wallet_distribution_bps: u64,
    pub burn_distribution_bps: u64,
    pub dev_distribution_bps: u64,
    pub total_fees_distributed: u64,
    pub total_contest_revenue_distributed: u64,
    pub last_distribution_timestamp: i64,
    pub distribution_count: u64,
}

#[account]
#[derive(Debug)]
pub struct ProgramUpgradeState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub program_type: u8, // 0-6 for each program type
    pub version: u16,
    pub initialization_timestamp: i64,
}

// ProgramType now imported at root level above - no conditional import needed

// Program id per realm (2026-10-03). This crate had ONE declare_id!() for both
// networks, so a `--features realm-main` build declared the DEVNET id and the
// binary would refuse every instruction at its mainnet address (4100). The
// cfg line must sit directly above its declare_id!() (ops/lib/ids.sh reads it).
#[cfg(feature = "realm-main")]
declare_id!("9UrtrL9uCJVKDGNRTvTMEz2pVD9ogLRuiKBmGheRX2PY");

#[cfg(not(feature = "realm-main"))]
declare_id!("8rLAaxGjPpBQJXDT73XnsbHtFSgYkksfEgJ76zvcspHt");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"fee-management|20251004T1339Z|build001";

#[derive(Accounts)]
pub struct VersionHeartbeat {}

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

/// Local BPS helper with u128 intermediate.
///
/// # 2026-08-21
/// Replaces reliance on `safe_math::calculate_bps` (which historically used
/// pure `u64` `checked_mul` and is therefore subject to the same
/// MathOverflow class that hit the transfer-hook on mainnet supply).
/// Prefer this for every fee-split site in this program until kots-common
/// is itself regenerated with the u128 pattern.
///
/// Must live **outside** the `#[program]` module — Anchor turns every free
/// function inside that module into an instruction handler.
#[inline(always)]
fn apply_bps(amount: u64, bps: u64) -> Result<u64> {
    let v = (amount as u128)
        .checked_mul(bps as u128)
        .and_then(|x| x.checked_div(10_000))
        .and_then(|x| u64::try_from(x).ok())
        .ok_or(KotsError::ArithmeticOverflow)?;
    Ok(v)
}

#[program]
pub mod fee_management {
    use super::*;

    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Initialize fee management program state
    pub fn initialize_fee_management(
        ctx: Context<InitializeFeeManagement>,
    ) -> Result<()> {
        // FRONT-RUN GUARD (2026-07-23). Initializers are first-caller-wins: this
        // instruction writes `authority = whoever signed`. On mainnet only the
        // ceremony multisig may call it, so a bot watching the deploy cannot
        // claim the authority in the window before the ceremony initializes.
        // Open on devnet (throwaway keys); fails closed in an unclaimed estate.
        require!(
            kots_common::program_ids::initializer_is_authorized(
                ctx.program_id,
                &ctx.accounts.authority.key(),
            ),
            KotsError::UnauthorizedCaller
        );

        msg!("💰 Initializing Fee Management Program");
        
        // Create PDA with treasury funding
        let seeds: &[&[u8]] = &[b"program_state"];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.program_state.key(), pda, KotsError::InvalidProgramId);
        
        // Step 1: Create the PDA using this program's seeds (THIS PROGRAM OWNS THE PDA)
        let rent = anchor_lang::solana_program::sysvar::rent::Rent::get()?;
        let required_lamports = rent.minimum_balance(256);
        
        // Create PDA using invoke_signed with this program's seeds
        let create_seeds: &[&[u8]] = &[b"program_state", &[bump]];
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                &ctx.accounts.authority.key(),
                &ctx.accounts.program_state.key(),
                required_lamports,
                256,
                ctx.program_id,
            ),
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.program_state.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[create_seeds],
        )?;
        
        // Step 2: Request treasury reimbursement
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Initialize the state
        let mut data = ctx.accounts.program_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Create and serialize the state
        // Use appropriate type based on compilation target
        #[cfg(feature = "idl-build")]
        let program_type_value = PROGRAM_TYPE_FEE_MANAGEMENT;
        #[cfg(not(feature = "idl-build"))]
        let program_type_value = ProgramType::FeeManagement as u8;
        
        let state = ProgramUpgradeState {
            is_initialized: true,
            authority: ctx.accounts.authority.key(),
            program_type: program_type_value,
            version: CURRENT_FEE_MANAGEMENT_VERSION,
            initialization_timestamp: get_current_timestamp()?,
        };
        
        // Serialize (Anchor discriminator + body) from offset 0
        state.try_serialize(&mut &mut data[..])?;
        
        msg!("✅ Fee Management Program initialized successfully");
        Ok(())
    }

    /// Initialize fee distribution system with dual distribution support
    pub fn init_fee(
        // Destination wallets are no longer parameters: they are not stored, and
        // each distribution instruction pins its caller-supplied destination to
        // the canonical constant. init_fee is now args-less — which is exactly
        // what the sidecar's ProgramInitializer already sends (this removal fixes
        // a latent arg-count mismatch between the two).
        ctx: Context<InitFee>,
    ) -> Result<()> {
        msg!("💰 Initializing fee distribution system with new wallet routing");
        
        // PDA derivation
        let (pda, bump) = Pubkey::find_program_address(&[b"fee_distribution", b"v4"], ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.distribution_state.key(), pda, KotsError::InvalidProgramId);
        
        // Step 1: Create the PDA using this program's seeds (THIS PROGRAM OWNS THE PDA)
        let rent = anchor_lang::solana_program::sysvar::rent::Rent::get()?;
        let required_lamports = rent.minimum_balance(FEE_DISTRIBUTION_SPACE);
        
        // Create PDA using invoke_signed with this program's seeds
        let create_seeds: &[&[u8]] = &[b"fee_distribution", b"v4", &[bump]];
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                &ctx.accounts.authority.key(),
                &ctx.accounts.distribution_state.key(),
                required_lamports,
                FEE_DISTRIBUTION_SPACE as u64,
                ctx.program_id,
            ),
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.distribution_state.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[create_seeds],
        )?;
        
        // Step 2: Request treasury reimbursement
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Initialize the state
        let mut data = ctx.accounts.distribution_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Create and serialize the state
        let distribution_state = FeeDistributionState {
            is_initialized: true,
            // destination wallets intentionally not stored (see struct doc + the
            // per-instruction canonical pins)
            // Trading tax split not stored (later 26): the distribution parts
            // hardcode it via the TRADING_*_DISTRIBUTION_BPS constants.
            // Contest revenue distribution percentages
            contest_treasury_bps: CONTEST_TREASURY_DISTRIBUTION_BPS,
            contest_development_bps: CONTEST_DEVELOPMENT_DISTRIBUTION_BPS,
            contest_jackpot_bps: CONTEST_JACKPOT_DISTRIBUTION_BPS,
            contest_burn_bps: CONTEST_BURN_DISTRIBUTION_BPS,
            // Legacy fields (set to zero for new format)
            jackpot_distribution_bps: 0,
            specified_wallet_distribution_bps: 0,
            burn_distribution_bps: 0,
            dev_distribution_bps: 0,
            // Statistics
            total_fees_distributed: 0,
            total_contest_revenue_distributed: 0,
            last_distribution_timestamp: get_current_timestamp()?,
            distribution_count: 0,
        };
        
        // Serialize (Anchor discriminator + body) from offset 0
        distribution_state.try_serialize(&mut &mut data[..])?;
        
        msg!("✅ Fee distribution initialized: 50% Jackpot, 33% Incoming, 3% Admin, 7% Treasury, 3% Dev, 3% Burn, 1% Bounty");
        Ok(())
    }

    /// Execute comprehensive fee distribution - BPF OPTIMIZED
    /// LIFETIMES (2026-07-24): `Interface<'info, T>` and `AccountInfo<'info>` are
    /// both invariant, so forwarding `remaining_accounts` (the transfer hook's
    /// extra metas) into the same CPI as accounts from `ctx.accounts` requires a
    /// single named `'info`. Same fix as progressive-jackpot's award_jackpot.
    /// BPF STACK FIX v3 — THE ACTUAL CAUSE (2026-07-24).
    ///
    /// This signature carried `_blacklist_state: Option<BlacklistState>` BY VALUE.
    /// BlacklistState is ~1119 bytes — [Pubkey; 10] + [[u8; 64]; 10] +
    /// [i64; 10] + scalars — so Anchor's generated `__global` wrapper
    /// deserialized ~1120 bytes onto the stack, and typically holds more than one
    /// copy across the deserialize-then-move. On a 4KB BPF frame that is most of
    /// the budget before a single line of this function runs.
    ///
    /// It was UNUSED — underscore-prefixed, zero references in the body, no
    /// caller anywhere in the workspace passing it.
    ///
    /// This is why v1 (loop) and v2 (#[inline(never)]) each reported a frame of
    /// EXACTLY 4576 bytes: both reshaped the transfers, and the transfers were
    /// never the problem. Three identical numbers across three different source
    /// versions was the evidence that the thing being changed was not the thing
    /// being measured.
    ///
    /// v2's `transfer_one_leg` is kept regardless: six inline CPIs in one frame
    /// is a real hazard independent of this argument, and the attribute costs
    /// nothing.
    #[allow(unreachable_code)] // body retained un-executed pending ABI-cleanup removal
    pub fn execute_fee_distribution<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteFeeDistribution<'info>>,
        fee_amount: u64,
    ) -> Result<()> {
        // O32 HEAP GATE (2026-08-04, owner-accepted): every hooked-mint leg's
        // resolver copy is charged against ONE 32 KB Anchor heap, and this
        // monolithic instruction carries too many legs to ever complete —
        // proven by the money_movement O32 ladder (OOM at every rung; larger
        // ComputeBudget frames never reach Anchor's hardcoded allocator).
        // Refuse loudly instead of aborting silently. Use the split Part
        // instructions (max two hooked legs per top-level instruction).
        // Slated for REMOVAL at the mainnet-ceremony ABI cleanup.
        return err!(KotsError::MonolithicDistributionHeapBound);
        require!(fee_amount > 0, KotsError::InvalidTokenAmount);
        // `transfer_checked` verifies decimals against the mint.
        let decimals = ctx.accounts.kots_mint.decimals;
        
        // Use hardcoded trading tax percentages for BPF optimization (UPDATED: 6 wallets)
        // u128 intermediate — same MathOverflow class fixed in transfer-hook.
        // (This body is currently gated by MonolithicDistributionHeapBound, but
        // keep the arithmetic correct so a future ungating does not reintroduce
        // the bug.)
        // Unreachable (heap-gated above). Kept in sync with the guide table so a
        // future ungating cannot revive the old 33/7/3 split.
        let jackpot_amount = (fee_amount as u128).checked_mul(5000).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;   // 50%
        let incoming_amount = (fee_amount as u128).checked_mul(700).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;   // 7%
        let admin_amount = (fee_amount as u128).checked_mul(300).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;      // 3%
        let treasury_amount = (fee_amount as u128).checked_mul(3400).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;  // 34%
        let dev_fund_amount = (fee_amount as u128).checked_mul(300).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;   // 3%
        let burn_amount = (fee_amount as u128).checked_mul(100).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;       // 1%
        let bounty_amount = (fee_amount as u128).checked_mul(200).and_then(|x| x.checked_div(10000)).and_then(|x| u64::try_from(x).ok()).ok_or(KotsError::ArithmeticOverflow)?;     // 2% owner bounty
        
        // BPF STACK FIX (2026-07-24). These six transfers were written inline,
        // each constructing its own CpiContext + TransferChecked + a
        // `remaining_accounts.to_vec()`. Six of those live simultaneously in one
        // frame, and the BPF verifier rejected it:
        //     Stack offset of 4560 exceeded max offset of 4096 by 464 bytes
        //     Estimated function frame size: 4576 bytes
        // A loop reuses ONE slot instead of six, which is also why
        // proc_contest_revenue (written as a loop) built without complaint.
        //
        // `to_vec()` is hoisted out of the loop as well: the hook's extra metas
        // are identical for every leg, so cloning them six times was pure waste.
        // O32: borrow, do not clone. `to_vec()` + a `.clone()` per leg allocated
        // five Vecs of 17 AccountInfo on a 32 KB heap that never frees, before
        // invoke_transfer_checked had allocated anything of its own.
        let hook_metas = ctx.remaining_accounts;

        for (dest, amount, label) in [
            (&ctx.accounts.jackpot_pool,     jackpot_amount,   "jackpot 50%"),
            (&ctx.accounts.incoming_wallet,  incoming_amount,  "incoming 7%"),
            (&ctx.accounts.admin_wallet,     admin_amount,     "admin 3%"),
            (&ctx.accounts.treasury_wallet,  treasury_amount,  "treasury 34%"),
            (&ctx.accounts.development_fund, dev_fund_amount,  "development 3%"),
            (&ctx.accounts.burn_address,     burn_amount,      "burn 1%"),
            (&ctx.accounts.bounty_wallet,    bounty_amount,    "bounty 2%"),
        ] {
            // Out-of-line call: the CPI is constructed in the callee's frame,
            // not here. See transfer_one_leg.
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} : {} raw", label, amount);
        }
        
        // Update distribution stats - BPF optimized
        let distribution = &mut ctx.accounts.distribution_state;
        distribution.total_fees_distributed = distribution.total_fees_distributed
            .checked_add(fee_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.distribution_count = distribution.distribution_count
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.last_distribution_timestamp = get_current_timestamp()?;
        
        msg!("✅ Trading tax distribution: {} KOTS to 7 wallets (50% J, 33% I, 3% A, 7% T, 3% D, 3% B, 1% Bounty)", fee_amount / TOKENS_PER_UNIT);
        Ok(())
    }

    /// Process contest revenue distribution 
    /// ⚠⚠ KNOWN-FAILING ON A 32 KB HEAP — use `proc_contest_revenue_part1` +
    /// `proc_contest_revenue_part2` instead (one transaction, two instructions).
    ///
    /// Measured 2026-07-29: four hooked CPI transfers in one instruction die on
    /// leg 3, on CU at the 200K default and on the heap at 1.4M CU. Retained so
    /// the instruction discriminator does not vanish from the IDL under any
    /// client still holding it, and so this warning is published where those
    /// clients will read it. It is not expected to succeed.
    #[allow(unreachable_code)] // body retained un-executed pending ABI-cleanup removal
    pub fn proc_contest_revenue<'info>(
        ctx: Context<'_, '_, '_, 'info, ProcContestRevenue<'info>>,
        revenue_amount: u64,
    ) -> Result<()> {
        // O32 HEAP GATE (2026-08-04, owner-accepted): every hooked-mint leg's
        // resolver copy is charged against ONE 32 KB Anchor heap, and this
        // monolithic instruction carries too many legs to ever complete —
        // proven by the money_movement O32 ladder (OOM at every rung; larger
        // ComputeBudget frames never reach Anchor's hardcoded allocator).
        // Refuse loudly instead of aborting silently. Use the split Part
        // instructions (max two hooked legs per top-level instruction).
        // Slated for REMOVAL at the mainnet-ceremony ABI cleanup.
        return err!(KotsError::MonolithicDistributionHeapBound);
        require!(revenue_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        
        let distribution = &ctx.accounts.distribution_state;
        
        // Calculate contest revenue distribution amounts
        let treasury_amount = apply_bps(revenue_amount, distribution.contest_treasury_bps)?;
        let development_amount = apply_bps(revenue_amount, distribution.contest_development_bps)?;
        let jackpot_amount = apply_bps(revenue_amount, distribution.contest_jackpot_bps)?;
        let burn_amount = apply_bps(revenue_amount, distribution.contest_burn_bps)?;
        let bounty_amount = apply_bps(revenue_amount, BOUNTY_DISTRIBUTION_BPS)?; // 2% owner bounty (from treasury)
        
        // Execute transfers for contest revenue
        // IMPLEMENTED (2026-07-24). This previously called
        // `distribute_contest_revenue_to_wallets`, which computed the split and
        // only `msg!`d it — no tokens moved. The account struct already carried
        // source_vault plus all four destinations and a vault_authority signer,
        // so everything needed was present; only the transfers were missing.
        // Hoisted for the same reason as execute_fee_distribution: the hook's
        // extra metas are identical for every leg.
        // O32: borrow, do not clone. `to_vec()` + a `.clone()` per leg allocated
        // five Vecs of 17 AccountInfo on a 32 KB heap that never frees, before
        // invoke_transfer_checked had allocated anything of its own.
        let hook_metas = ctx.remaining_accounts;

        for (dest, amount, label) in [
            (&ctx.accounts.treasury_wallet, treasury_amount, "treasury"),
            (&ctx.accounts.development_fund, development_amount, "development"),
            (&ctx.accounts.jackpot_pool, jackpot_amount, "jackpot"),
            (&ctx.accounts.burn_address, burn_amount, "burn"),
            (&ctx.accounts.bounty_wallet, bounty_amount, "bounty"),
        ] {
            // Out-of-line call: the CPI is constructed in the callee's frame,
            // not here. See transfer_one_leg.
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        
        // Update statistics
        let distribution = &mut ctx.accounts.distribution_state;
        distribution.total_contest_revenue_distributed = distribution.total_contest_revenue_distributed
            .checked_add(revenue_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.last_distribution_timestamp = get_current_timestamp()?;
        distribution.distribution_count = distribution.distribution_count
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        
        msg!("✅ Contest revenue processed - Total: {}, Distributed: {}", revenue_amount, 
             treasury_amount + development_amount + jackpot_amount + burn_amount + bounty_amount);
        Ok(())
    }

    /// Contest revenue distribution, PART 1 of 2 — treasury + development.
    ///
    /// ⚠ USE THIS PAIR, NOT `proc_contest_revenue`. Measured 2026-07-29 on
    /// solana-program-test: the four-leg instruction cannot complete.
    ///
    ///   * with the default 200K CU it dies `exceeded CUs meter` on leg 3
    ///   * with 1.4M CU it dies `memory allocation failed` on leg 3
    ///
    /// Both limits are per-leg-cumulative. Each `invoke_transfer_checked` on a
    /// hooked mint resolves the ExtraAccountMetaList and materialises the extra
    /// accounts on a 32 KB BPF heap whose bump allocator NEVER FREES, so legs 1
    /// and 2 fit and leg 3 does not — roughly 10-11 KB per leg, measured.
    ///
    /// WHY SPLITTING WORKS, AND WHY IT BEATS A HEAP FRAME. The heap is allocated
    /// per top-level instruction invocation, so two instructions get two fresh
    /// heaps — while remaining in ONE TRANSACTION, which keeps the distribution
    /// atomic. If part 2 fails the whole transaction reverts and part 1's
    /// transfers roll back with it.
    ///
    /// The alternative, `ComputeBudgetInstruction::request_heap_frame(256 * 1024)`,
    /// is what `transfer_one_leg` currently instructs callers to send. It has
    /// never been verified to do anything: solana-program-test ignores the heap
    /// field while honouring the CU field (measured — five configurations,
    /// identical failure at exactly 59,156 CU), and no validator run exists.
    /// Splitting depends on documented per-instruction semantics instead of on
    /// unverified runtime behaviour.
    ///
    /// CU IS STILL THE CALLER'S JOB. Splitting fixes the heap, not the budget:
    /// CU is charged per TRANSACTION, so the total is unchanged and four legs
    /// still exceed the 200K default. Send
    /// `ComputeBudgetInstruction::set_compute_unit_limit(..)` as well. That
    /// instruction IS honoured — same measurement showed the budget tracking
    /// exactly, 200,000 -> 1,399,850 -> 1,399,700.
    ///
    /// ⚠ CALL BOTH PARTS, IN ONE TRANSACTION, IN ORDER. Part 2 carries the
    /// statistics update, so calling it alone books a distribution that only
    /// half happened. Nothing on-chain enforces the pairing today — the two
    /// instructions cannot see each other without an instructions-sysvar check,
    /// which is not built. Recorded as an open item rather than assumed away.
    pub fn proc_contest_revenue_part1<'info>(
        ctx: Context<'_, '_, '_, 'info, ProcContestRevenuePart1<'info>>,
        revenue_amount: u64,
    ) -> Result<()> {
        require!(revenue_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin fee-distribution destinations to the canonical wallets
        // (kots-common single source of truth). A destination owned by anything
        // else is off-canonical and rejected.
        require_keys_eq!(ctx.accounts.treasury_wallet.owner, kots_common::constants::TREASURY_WALLET, KotsError::OffCanonicalDistributionWallet);
        require_keys_eq!(ctx.accounts.development_fund.owner, kots_common::constants::DEV_WALLET, KotsError::OffCanonicalDistributionWallet);
        let distribution = &ctx.accounts.distribution_state;

        let treasury_amount =
            apply_bps(revenue_amount, distribution.contest_treasury_bps)?;
        let development_amount =
            apply_bps(revenue_amount, distribution.contest_development_bps)?;

        // Borrowed, never cloned — see transfer_one_leg. The hook's extra metas
        // are identical for every leg, and a clone per leg was itself part of
        // what exhausted the heap.
        let hook_metas = ctx.remaining_accounts;

        for (dest, amount, label) in [
            (&ctx.accounts.treasury_wallet, treasury_amount, "treasury"),
            (&ctx.accounts.development_fund, development_amount, "development"),
        ] {
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }

        msg!("✅ Contest revenue part 1/3 (treasury + development) complete");
        Ok(())
    }

    /// Contest revenue distribution, PART 2 of 2 — jackpot + burn, and the
    /// statistics update. See `proc_contest_revenue_part1` for why this is split
    /// and for the calling contract; the two are one operation and belong in one
    /// transaction.
    pub fn proc_contest_revenue_part2<'info>(
        ctx: Context<'_, '_, '_, 'info, ProcContestRevenuePart2<'info>>,
        revenue_amount: u64,
    ) -> Result<()> {
        require!(revenue_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin fee-distribution destinations to the canonical wallets
        // (kots-common single source of truth). A destination owned by anything
        // else is off-canonical and rejected.
        require_keys_eq!(ctx.accounts.jackpot_pool.owner, kots_common::constants::JACKPOT_WALLET, KotsError::OffCanonicalDistributionWallet);
        require_keys_eq!(ctx.accounts.burn_address.owner, kots_common::constants::BURN_ADDRESS, KotsError::OffCanonicalDistributionWallet);
        let distribution = &ctx.accounts.distribution_state;

        let jackpot_amount =
            apply_bps(revenue_amount, distribution.contest_jackpot_bps)?;
        let burn_amount =
            apply_bps(revenue_amount, distribution.contest_burn_bps)?;

        let hook_metas = ctx.remaining_accounts;

        for (dest, amount, label) in [
            (&ctx.accounts.jackpot_pool, jackpot_amount, "jackpot"),
            (&ctx.accounts.burn_address, burn_amount, "burn"),
        ] {
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }

        // Statistics live here in part 2 (2026-08-04: part 3 now follows,
        // carrying the owner-bounty leg). Run ALL THREE parts in ONE
        // transaction: same-tx atomicity is what keeps this booking honest —
        // a failing part 3 reverts the statistics with it. Split across
        // transactions, a booked distribution with an unpaid bounty becomes
        // possible; do not do that.
        let distribution = &mut ctx.accounts.distribution_state;
        distribution.total_contest_revenue_distributed = distribution
            .total_contest_revenue_distributed
            .checked_add(revenue_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.last_distribution_timestamp = get_current_timestamp()?;
        distribution.distribution_count = distribution
            .distribution_count
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;

        msg!("✅ Contest revenue part 2/2 (jackpot + burn) complete — total {}", revenue_amount);
        Ok(())
    }

    /// O32 PART 3 (2026-08-04): the owner-bounty leg, alone. The 3-leg Part 1
    /// (bounty riding with treasury + development) proved heap-bound in the
    /// O32 ladder — empirically TWO hooked legs per instruction is the
    /// ceiling, so the split invariant is now: max two hooked legs per
    /// top-level instruction. Run Part 1 + Part 2 + Part 3 in ONE transaction
    /// so the distribution stays atomic (Part 2 books the statistics; a
    /// same-tx failure of any part reverts everything, including that
    /// booking).
    pub fn proc_contest_revenue_part3<'info>(
        ctx: Context<'_, '_, '_, 'info, ProcContestRevenuePart3<'info>>,
        revenue_amount: u64,
    ) -> Result<()> {
        require!(revenue_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        // Pin fee-distribution destinations to the canonical wallets
        // (kots-common single source of truth). A destination owned by anything
        // else is off-canonical and rejected.
        require_keys_eq!(ctx.accounts.bounty_wallet.owner, kots_common::constants::BOUNTY_WALLET, KotsError::OffCanonicalDistributionWallet);
        let bounty_amount =
            apply_bps(revenue_amount, BOUNTY_DISTRIBUTION_BPS)?; // 2% owner bounty (from treasury)
        let hook_metas = ctx.remaining_accounts;
        transfer_one_leg(
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.source_vault.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.bounty_wallet.to_account_info(),
            ctx.accounts.vault_authority.to_account_info(),
            hook_metas,
            bounty_amount,
            decimals,
        )?;
        msg!("  -> bounty {} raw", bounty_amount);
        msg!("✅ Contest revenue part 3/3 (owner bounty) complete");
        Ok(())
    }

    // ========================================================================
    // TRADING-TAX DISTRIBUTION — split + pinned, mirroring the contest parts.
    //
    // The monolithic `execute_fee_distribution` is heap-gated (O32) and, being
    // gated before it could be split, never got destination pins. These four
    // parts are the LIVE trading-revenue path: max two hooked legs each (the O32
    // ceiling), every destination pinned to its canonical wallet, and the split
    // HARDCODED via the TRADING_*_DISTRIBUTION_BPS constants (owner decision,
    // later 26 — the amounts live in the bytecode, immune to a tampered
    // distribution_state; the pins lock the destinations the same way). Run ALL
    // FOUR in ONE transaction (an Address Lookup Table is required: four
    // instructions + 7 destinations + hook metas exceed a legacy tx) — part 3
    // books the stats, so a same-tx failure of any part reverts the booking with
    // it. The gated monolith is retained for IDL-discriminator stability, removed
    // at the ceremony ABI cleanup.
    // ========================================================================
    /// DUAL-PATH (2026-08-27): parts 1–4 read TRADING_*_DISTRIBUTION_BPS from
    /// kots-common (guide table 50/7/35/3/3/1/1). That table is compiled into
    /// THIS source but is NOT live until the next upgrade-in-place. The
    /// currently deployed binary still encodes incoming 33 / treasury 7 /
    /// burn 3. Until that upgrade is confirmed, the sidecar keeper pays the
    /// guide split with Token-2022 transferChecked + burnChecked. Do not call
    /// these parts on a vault the sidecar keeper is also draining.
    pub fn execute_fee_distribution_part1<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteFeeDistributionPart1<'info>>,
        fee_amount: u64,
    ) -> Result<()> {
        require!(fee_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        require_keys_eq!(ctx.accounts.jackpot_pool.owner, kots_common::constants::JACKPOT_WALLET, KotsError::OffCanonicalDistributionWallet);
        require_keys_eq!(ctx.accounts.incoming_wallet.owner, kots_common::constants::INCOMING_WALLET, KotsError::OffCanonicalDistributionWallet);

        let jackpot_amount = apply_bps(fee_amount, TRADING_JACKPOT_DISTRIBUTION_BPS)?;
        let incoming_amount = apply_bps(fee_amount, TRADING_INCOMING_DISTRIBUTION_BPS)?;

        let hook_metas = ctx.remaining_accounts;
        for (dest, amount, label) in [
            (&ctx.accounts.jackpot_pool, jackpot_amount, "jackpot 50%"),
            (&ctx.accounts.incoming_wallet, incoming_amount, "incoming 7%"),
        ] {
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        msg!("\u{2705} Trading distribution part 1/4 (jackpot + incoming) complete");
        Ok(())
    }

    pub fn execute_fee_distribution_part2<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteFeeDistributionPart2<'info>>,
        fee_amount: u64,
    ) -> Result<()> {
        require!(fee_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        require_keys_eq!(ctx.accounts.admin_wallet.owner, kots_common::constants::ADMIN_WALLET, KotsError::OffCanonicalDistributionWallet);
        require_keys_eq!(ctx.accounts.treasury_wallet.owner, kots_common::constants::TREASURY_WALLET, KotsError::OffCanonicalDistributionWallet);

        let admin_amount = apply_bps(fee_amount, TRADING_ADMIN_DISTRIBUTION_BPS)?;
        let treasury_amount = apply_bps(fee_amount, TRADING_TREASURY_DISTRIBUTION_BPS)?;

        let hook_metas = ctx.remaining_accounts;
        for (dest, amount, label) in [
            (&ctx.accounts.admin_wallet, admin_amount, "admin 3%"),
            (&ctx.accounts.treasury_wallet, treasury_amount, "treasury 35%"),
        ] {
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }
        msg!("\u{2705} Trading distribution part 2/4 (admin + treasury) complete");
        Ok(())
    }

    pub fn execute_fee_distribution_part3<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteFeeDistributionPart3<'info>>,
        fee_amount: u64,
    ) -> Result<()> {
        require!(fee_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        require_keys_eq!(ctx.accounts.development_fund.owner, kots_common::constants::DEV_WALLET, KotsError::OffCanonicalDistributionWallet);
        require_keys_eq!(ctx.accounts.burn_address.owner, kots_common::constants::BURN_ADDRESS, KotsError::OffCanonicalDistributionWallet);

        let dev_amount = apply_bps(fee_amount, TRADING_DEV_FUND_DISTRIBUTION_BPS)?;
        let burn_amount = apply_bps(fee_amount, TRADING_BURN_DISTRIBUTION_BPS)?;

        let hook_metas = ctx.remaining_accounts;
        for (dest, amount, label) in [
            (&ctx.accounts.development_fund, dev_amount, "development 3%"),
            (&ctx.accounts.burn_address, burn_amount, "burn 1%"),
        ] {
            transfer_one_leg(
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.source_vault.to_account_info(),
                ctx.accounts.kots_mint.to_account_info(),
                dest.to_account_info(),
                ctx.accounts.vault_authority.to_account_info(),
                hook_metas,
                amount,
                decimals,
            )?;
            msg!("  -> {} {} raw", label, amount);
        }

        // Statistics booked here (part 3): total_fees_distributed is the TRADING
        // counter (the monolith updated the same field). Part 4 (bounty) follows in
        // the SAME transaction, so a failing bounty leg reverts this booking too.
        let distribution = &mut ctx.accounts.distribution_state;
        distribution.total_fees_distributed = distribution.total_fees_distributed
            .checked_add(fee_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.distribution_count = distribution.distribution_count
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        distribution.last_distribution_timestamp = get_current_timestamp()?;

        msg!("\u{2705} Trading distribution part 3/4 (dev + burn, + stats) complete — total {}", fee_amount);
        Ok(())
    }

    pub fn execute_fee_distribution_part4<'info>(
        ctx: Context<'_, '_, '_, 'info, ExecuteFeeDistributionPart4<'info>>,
        fee_amount: u64,
    ) -> Result<()> {
        require!(fee_amount > 0, KotsError::InvalidTokenAmount);
        let decimals = ctx.accounts.kots_mint.decimals;
        require_keys_eq!(ctx.accounts.bounty_wallet.owner, kots_common::constants::BOUNTY_WALLET, KotsError::OffCanonicalDistributionWallet);
        let bounty_amount = apply_bps(fee_amount, BOUNTY_DISTRIBUTION_BPS)?;
        let hook_metas = ctx.remaining_accounts;
        transfer_one_leg(
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.source_vault.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.bounty_wallet.to_account_info(),
            ctx.accounts.vault_authority.to_account_info(),
            hook_metas,
            bounty_amount,
            decimals,
        )?;
        msg!("  -> bounty {} raw", bounty_amount);
        msg!("\u{2705} Trading distribution part 4/4 (owner bounty) complete");
        Ok(())
    }
}

// ============================================================================
// ACCOUNT STRUCTS
// ============================================================================

#[derive(Accounts)]
pub struct InitializeFeeManagement<'info> {
    /// Program state PDA - manually created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub program_state: UncheckedAccount<'info>,
    
    pub authority: Signer<'info>,
    
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,
    
    /// CHECK: Fee management program (self-reference)
    pub fee_management_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitFee<'info> {
    /// Fee distribution PDA - manually created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub distribution_state: UncheckedAccount<'info>,
    
    pub authority: Signer<'info>,
    
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,
    
    /// CHECK: Fee management program (self-reference)
    pub fee_management_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

// BPF STACK FIX v4 — Box the large accounts (2026-07-24).
//
// v3 removed a 1120-byte unused argument and the frame moved only 4576 -> 4544:
// LLVM had already elided most of an unused value, so that was never the bulk.
//
// The bulk is this struct itself. `Account<'info, T>` and
// `InterfaceAccount<'info, T>` hold the DESERIALIZED T by value plus its
// AccountInfo. Nine token accounts at roughly 230 bytes each, plus the mint and
// the ~345-byte FeeDistributionState, is around 2.7KB of a 4KB frame — before
// the handler body executes.
//
// `Box<...>` moves the payload to the heap and leaves a pointer on the stack.
// This is the standard Anchor remedy for BPF stack overflow, and it is why the
// error names the generated `__global` wrapper rather than any line of ours: the
// cost is in materialising the Accounts struct, not in the code we wrote.
//
// `.to_account_info()` still resolves through Deref, so call sites are unchanged.
#[derive(Accounts)]
pub struct ExecuteFeeDistribution<'info> {
    #[account(
        mut,
        seeds = [b"fee_distribution", b"v4"],
        bump
    )]
    pub distribution_state: Box<Account<'info, FeeDistributionState>>,
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub jackpot_pool: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub incoming_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub admin_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub liquidity_pool: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub development_fund: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub burn_address: Box<InterfaceAccount<'info, TokenAccount>>,
    /// OWNER BOUNTY (2026-08-04): recipient of the 1% owner bounty. Caller-supplied
    /// like the other legs; the intended owner is kots_common::constants::BOUNTY_WALLET
    /// (also permanently fee-exempt in the transfer hook).
    #[account(mut)]
    pub bounty_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    /// TOKEN-2022 PORT (2026-07-24): the mint, required by `transfer_checked`
    /// (it verifies decimals) and by Token-2022 to reach the transfer hook.
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

// BPF STACK FIX v4 — Box the large accounts (2026-07-24).
//
// v3 removed a 1120-byte unused argument and the frame moved only 4576 -> 4544:
// LLVM had already elided most of an unused value, so that was never the bulk.
//
// The bulk is this struct itself. `Account<'info, T>` and
// `InterfaceAccount<'info, T>` hold the DESERIALIZED T by value plus its
// AccountInfo. Nine token accounts at roughly 230 bytes each, plus the mint and
// the ~345-byte FeeDistributionState, is around 2.7KB of a 4KB frame — before
// the handler body executes.
//
// `Box<...>` moves the payload to the heap and leaves a pointer on the stack.
// This is the standard Anchor remedy for BPF stack overflow, and it is why the
// error names the generated `__global` wrapper rather than any line of ours: the
// cost is in materialising the Accounts struct, not in the code we wrote.
//
// `.to_account_info()` still resolves through Deref, so call sites are unchanged.
#[derive(Accounts)]
pub struct ProcContestRevenue<'info> {
    #[account(
        mut,
        seeds = [b"fee_distribution", b"v4"],
        bump
    )]
    pub distribution_state: Box<Account<'info, FeeDistributionState>>,
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub development_fund: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub jackpot_pool: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub burn_address: Box<InterfaceAccount<'info, TokenAccount>>,
    /// OWNER BOUNTY (2026-08-04): recipient of the 1% owner bounty. Caller-supplied
    /// like the other legs; the intended owner is kots_common::constants::BOUNTY_WALLET
    /// (also permanently fee-exempt in the transfer hook).
    #[account(mut)]
    pub bounty_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    /// TOKEN-2022 PORT (2026-07-24): the mint, required by `transfer_checked`
    /// (it verifies decimals) and by Token-2022 to reach the transfer hook.
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

// ── SPLIT ACCOUNT STRUCTS (2026-07-29, O32) ─────────────────────────────────
//
// Deliberately NOT a shared struct with cfg'd fields. Each part names only the
// destinations it touches, so the account list is the smallest that can do the
// job — Anchor deserializes every named account before the first leg runs, and
// on a heap this tight that deserialization is not free.
//
// `Box<...>` throughout, matching ProcContestRevenue above: the boxes exist to
// keep the accounts off the 4 KB BPF stack frame, and dropping them here would
// reintroduce a stack problem while solving a heap one.

#[derive(Accounts)]
pub struct ProcContestRevenuePart1<'info> {
    // Read-only in part 1. The statistics update lives in part 2, so nothing
    // here needs write access and asking for less is free.
    #[account(
        seeds = [b"fee_distribution", b"v4"],
        bump
    )]
    pub distribution_state: Box<Account<'info, FeeDistributionState>>,
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub development_fund: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    /// The mint, required by `transfer_checked` (it verifies decimals) and by
    /// Token-2022 to reach the transfer hook.
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ProcContestRevenuePart2<'info> {
    // MUTABLE here: part 2 carries the statistics update.
    #[account(
        mut,
        seeds = [b"fee_distribution", b"v4"],
        bump
    )]
    pub distribution_state: Box<Account<'info, FeeDistributionState>>,
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub jackpot_pool: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub burn_address: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ProcContestRevenuePart3<'info> {
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// OWNER BOUNTY recipient. Caller-supplied like every other leg; the
    /// intended owner is kots_common::constants::BOUNTY_WALLET (also
    /// permanently fee-exempt in the transfer hook).
    #[account(mut)]
    pub bounty_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    /// The mint, required by `transfer_checked` (decimals) and by Token-2022
    /// to reach the transfer hook.
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ExecuteFeeDistributionPart1<'info> {
    // distribution_state not needed here: the split is hardcoded, and part 3
    // carries the (mutable) stats.
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub jackpot_pool: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub incoming_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ExecuteFeeDistributionPart2<'info> {
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub admin_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub treasury_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ExecuteFeeDistributionPart3<'info> {
    // MUTABLE: part 3 carries the trading-stats update.
    #[account(
        mut,
        seeds = [b"fee_distribution", b"v4"],
        bump
    )]
    pub distribution_state: Box<Account<'info, FeeDistributionState>>,
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub development_fund: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub burn_address: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ExecuteFeeDistributionPart4<'info> {
    #[account(mut)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut)]
    pub bounty_wallet: Box<InterfaceAccount<'info, TokenAccount>>,
    pub vault_authority: Signer<'info>,
    pub kots_mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// One transfer leg, deliberately NOT inlined.
///
/// BPF STACK FIX v2 (2026-07-24). v1 collapsed six inline CPIs into a loop and
/// the frame size did not move ONE BYTE — still 4576, still 464 over. With
/// `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, LLVM fully unrolls a
/// six-element array loop and re-inlines every body, reproducing the original
/// code exactly. A loop is a source-level construct; it does not survive to the
/// verifier.
///
/// `#[inline(never)]` does survive. On Solana BPF each call frame is a separate
/// 4KB allocation, so the CpiContext + TransferChecked + forwarded metas are
/// built in THIS function's frame instead of the caller's — and the frames are
/// reused sequentially rather than stacked. The caller keeps only the arguments
/// for one call at a time.
///
/// Do not remove the attribute to "tidy up". It is load-bearing.
#[inline(never)]
fn transfer_one_leg<'info>(
    token_program: AccountInfo<'info>,
    from: AccountInfo<'info>,
    mint: AccountInfo<'info>,
    to: AccountInfo<'info>,
    authority: AccountInfo<'info>,
    hook_metas: &[AccountInfo<'info>],
    amount: u64,
    decimals: u8,
) -> Result<()> {
    if amount == 0 {
        return Ok(()); // a zero-value transfer_checked is a wasted CPI
    }

    // ───────────────────────── O32 INSTRUMENTATION ─────────────────────────
    // TEMPORARY. Remove once O32 is closed. Pairs with the count logged inside
    // the transfer-hook fallback: this is what leaves, that is what arrives.
    // If they differ, the loss is in Token-2022's resolution or in
    // with_remaining_accounts; if they match, the hook's struct expects more
    // than the meta list declares.
    // ⚠ CLIENT OBLIGATION — HEAP FRAME (O32, 2026-07-28)
    //
    // Every caller of proc_contest_revenue or execute_fee_distribution MUST send
    //     ComputeBudgetInstruction::request_heap_frame(256 * 1024)
    // alongside the compute-unit limit.
    //
    // The default BPF heap is 32 KB with a bump allocator that never frees.
    // invoke_transfer_checked below deserializes the ExtraAccountMetaList, builds
    // a 19-entry Vec<AccountMeta> and resolves every seed — per leg. Four legs
    // (proc_contest_revenue) exhausts it; six (execute_fee_distribution) is
    // worse. Measured failure: "memory allocation failed, out of memory" at
    // 60,784 CU with 1.34M unspent — HEAP-bound, not CU-bound, so raising the
    // compute limit alone changes nothing and misleads.
    //
    // A program cannot request its own heap frame; only the transaction can. A
    // DEX router never meets this because it does one hooked transfer per
    // instruction — multi-leg distribution is the unusual case.

    // COUNT ONLY — the per-account loop that was here caused an OOM. The BPF heap
    // is 32 KB with a BUMP allocator that never frees, and every msg! carrying a
    // Pubkey allocates twice (base58 string, then the formatted line). Seventeen
    // per leg times four legs in one instruction exhausted it, and compute went
    // 39K -> 116K. The measurement broke the thing it was measuring.
    msg!("O32: forwarding {} remaining_accounts into transfer_checked", hook_metas.len());

    // ─────────────────────────────── O32 FIX ───────────────────────────────
    // WAS: anchor_spl::token_interface::transfer_checked(..)
    //          .with_remaining_accounts(hook_metas)
    //
    // That attaches the hook's accounts to the CPI, but Token-2022 does NOT
    // resolve a hooked mint's extra accounts on the CPI path — resolution is the
    // CALLER's job. Measured 2026-07-28: 17 accounts forwarded, **4 received** by
    // the hook (source, mint, destination, owner — not even the meta list), and
    // TransferHook::try_accounts failed AccountNotEnoughKeys (3005).
    //
    // `onchain::invoke_transfer_checked` is the helper that exists for exactly
    // this: it reads the ExtraAccountMetaList from `additional_accounts`,
    // resolves each entry, and appends them to the instruction in the order the
    // hook declares. `hook_metas` already carries what it needs — the hook
    // program, the meta-list PDA, and every extra.
    //
    // Direct transfers never hit this because the runtime hands Token-2022 the
    // whole transaction's account list and its own resolution succeeds. Only CPI
    // callers are affected — which is this program, and every DEX router.
    //
    // seeds = &[]: `vault_authority` is a real `Signer<'info>`, not a PDA, so it
    // signs the outer transaction and needs no invoke_signed seeds. If a caller
    // is ever changed to a PDA authority, its seeds go here or the CPI fails
    // with a missing-signature error rather than anything descriptive.
    spl_token_2022::onchain::invoke_transfer_checked(
        token_program.key,
        from,
        mint,
        to,
        authority,
        hook_metas,
        amount,
        decimals,
        &[],
    )
    .map_err(Into::into)
}

/// ACCOUNTING ONLY — and that is correct here, not a stub to be filled in.
///

/// DEAD (2026-07-24): superseded by the inline transfers in
/// `proc_contest_revenue`. Retained only so the signature is greppable while
/// callers are audited; delete once confirmed unreferenced.
#[allow(dead_code)]
fn distribute_contest_revenue_to_wallets(
    treasury_amount: u64,
    development_amount: u64,
    jackpot_amount: u64,
    burn_amount: u64,
) -> Result<()> {
    // Execute token transfers for contest revenue (4 wallets)
    msg!("💸 Distributing contest revenue - T:{}, D:{}, J:{}, B:{}", 
         treasury_amount, development_amount, jackpot_amount, burn_amount);
    Ok(())
}
