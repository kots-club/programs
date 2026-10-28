// DESTINATION (where this file goes): kots-contracts/programs/progressive-jackpot/src/lib.rs
//! KOTS Progressive Jackpot Program
//! 
//! Handles 52-week progressive jackpot cycle management,
//! weekly contributions, and jackpot distribution.

use anchor_lang::prelude::*;
// TOKEN-2022 PORT (2026-07-23): was `anchor_spl::token::{self, Token, TokenAccount}`.
// KOTS is a Token-2022 mint with a transfer hook, so the legacy token types and
// the legacy `token::transfer` instruction could never have moved KOTS. These are
// the same interface types lp-position-manager already uses.
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use kots_common::{
    constants::*, errors::*, utils::*,
    state::{
        VAULT_SEED, CPI_AUTHORITY_SEED
    }
};

// Import ProgramType only for BPF compilation (not during IDL generation)
#[cfg(not(all(feature = "idl-build", not(target_arch = "bpf"))))]
use kots_common::ProgramType;

// Program-local account size constants.
//
// BUGFIX (2026-07-23): sizes are now derived rather than asserted. The old
// comment claimed "8 + 504" for a struct whose Borsh size is 479.
//
// ProgressiveJackpotState (Borsh):
//   is_initialized          bool          1
//   current_cycle_week      u8            1
//   cycle_start_timestamp   i64           8
//   total_jackpot_amount    u64           8
//   weekly_contributions    [u64; 52]   416
//   last_winner             Option<Pubkey> 1 + 32 = 33
//   last_win_timestamp      i64           8
//   total_cycles_completed  u32           4
//                                       ----
//                                        479  + 8 discriminator = 487
// 512 keeps the original allocation (headroom for future fields).
const JACKPOT_STATE_SPACE: usize = 512;

// ProgramUpgradeState (Borsh):
//   is_initialized            bool   1
//   authority                 Pubkey 32
//   program_type              u8     1
//   version                   u16    2
//   initialization_timestamp  i64    8
//                                   ---
//                                    44  + 8 discriminator = 52
// 256 keeps the original allocation.
const PROGRAM_STATE_SPACE: usize = 256;

// Account struct definitions at root level for proper IDL generation

#[account]
#[derive(Debug)]
pub struct ProgressiveJackpotState {
    pub is_initialized: bool,
    pub current_cycle_week: u8,
    pub cycle_start_timestamp: i64,
    pub total_jackpot_amount: u64,
    pub weekly_contributions: [u64; 52],
    pub last_winner: Option<Pubkey>,
    pub last_win_timestamp: i64,
    pub total_cycles_completed: u32,
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

// Unified program ID for both devnet and mainnet
#[cfg(feature = "realm-main")]
declare_id!("CAccwwWNqJxCRxuvqQTwXnokVEbUmPCoJX5hibWWnHKg");

#[cfg(not(feature = "realm-main"))]
declare_id!("7xSq4Fk84ib2195fMnmiwCN9ZyMj6jEp1pimsZicJH6y");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"progressive-jackpot|20251004T1339Z|build001";

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
#[program]
pub mod progressive_jackpot {
    use super::*;
    
    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Manage progressive jackpot (52-week cycle)
    pub fn manage_progressive_jackpot(
        ctx: Context<ManageProgressiveJackpot>,
        week_number: u8,
        contribution_amount: u64,
    ) -> Result<()> {
        msg!("💎 Managing progressive jackpot - Week: {}, Amount: {}", week_number, contribution_amount);
        
        require!(week_number >= 1 && week_number <= 52, KotsError::InvalidTokenAmount);
        
        let jackpot = &mut ctx.accounts.jackpot_state;
        
        // Initialize if needed
        if !jackpot.is_initialized {
            jackpot.is_initialized = true;
            jackpot.current_cycle_week = 1;
            jackpot.cycle_start_timestamp = get_current_timestamp()?;
            jackpot.total_jackpot_amount = 0;
            jackpot.weekly_contributions = [0; 52];
            jackpot.last_winner = None;
            jackpot.last_win_timestamp = 0;
            jackpot.total_cycles_completed = 0;
        }
        
        // Add contribution to current week
        // SVE1004: week_number is validated to be in 1..=52 before this point.
        // Subtraction cannot underflow; documented for the auditor.
        let week_index = (week_number - 1) as usize;
        jackpot.weekly_contributions[week_index] = jackpot.weekly_contributions[week_index]
            .checked_add(contribution_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        
        jackpot.total_jackpot_amount = jackpot.total_jackpot_amount
            .checked_add(contribution_amount)
            .ok_or(KotsError::ArithmeticOverflow)?;
        
        // Check if cycle is complete
        if week_number == 52 {
            jackpot.total_cycles_completed = jackpot.total_cycles_completed
                .checked_add(1)
                .ok_or(KotsError::ArithmeticOverflow)?;
            
            // Reset for new cycle
            jackpot.current_cycle_week = 1;
            jackpot.cycle_start_timestamp = get_current_timestamp()?;
            jackpot.weekly_contributions = [0; 52];
        } else {
            jackpot.current_cycle_week = week_number + 1;
        }
        
        msg!("✅ Progressive jackpot updated - Total: {}, Week: {}", 
             jackpot.total_jackpot_amount, jackpot.current_cycle_week);
        Ok(())
    }

    /// Initialize the progressive jackpot ACCOUNTING state.
    ///
    /// BUGFIX (2026-07-23): this instruction previously created the PDA at
    /// `[b"program_state"]` — the same address `initialize_progressive_jackpot`
    /// uses — and wrote a `ProgressiveJackpotState` discriminator into it, at a
    /// different size (512 vs 256) than the `ProgramUpgradeState` the other
    /// initializer writes. Two account types, one address. Whichever ran second
    /// failed with "account already in use", so only one of the two could ever
    /// exist.
    ///
    /// Meanwhile `[b"jackpot_state", b"v4"]` — the PDA that
    /// `manage_progressive_jackpot` and `award_jackpot` actually require — was
    /// never created by ANY instruction. Both of those handlers therefore failed
    /// account validation on every call, which means the destination for 50% of
    /// the trading tax and 10% of contest fees had a payout path that had never
    /// once executed.
    ///
    /// The two states now live at their own addresses:
    ///   `[b"program_state"]`          -> ProgramUpgradeState  (authority, version)
    ///   `[b"jackpot_state", b"v4"]`   -> ProgressiveJackpotState (52-week cycle)
    ///
    /// ORDERING: `initialize_progressive_jackpot` MUST run first — it creates the
    /// authority record this instruction checks against.
    pub fn initialize_jackpot(
        ctx: Context<InitializeJackpot>,
        initial_amount: u64,
    ) -> Result<()> {
        msg!("💎 Initializing progressive jackpot system");

        // Anchor's `init` (see InitializeJackpot) has already created the PDA at
        // [b"jackpot_state", b"v4"], written the correct discriminator, and
        // zero-filled the data. The hand-rolled invoke_signed + manual
        // `hash("account:...")` discriminator + raw byte-offset field writes that
        // used to live here are gone: they duplicated what Anchor does correctly,
        // and the offsets were a silent-corruption risk against any future field
        // reordering in ProgressiveJackpotState.
        let jackpot = &mut ctx.accounts.jackpot_state;
        jackpot.is_initialized = true;
        jackpot.current_cycle_week = 1;
        jackpot.cycle_start_timestamp = get_current_timestamp()?;

        // NOTE: `initial_amount` is ACCOUNTING state only — it does not move
        // tokens and is not evidence the vault holds anything. `award_jackpot`
        // caps every payout at the vault's real balance for exactly this reason.
        jackpot.total_jackpot_amount = initial_amount;

        // weekly_contributions / last_winner / last_win_timestamp /
        // total_cycles_completed are already zero / None from init's zero-fill.
        // Deliberately NOT reassigning weekly_contributions: a `[u64; 52]` literal
        // is a 416-byte stack temporary and the BPF frame is 4KB.

        // Reimburse the payer for the rent `init` just charged them — for BOTH
        // accounts created here. The vault's rent is read from the account rather
        // than recomputed, because Token-2022 sizes a token account according to
        // whichever extensions the mint requires; hardcoding 165 bytes would
        // under-reimburse on a hooked mint.
        let rent = anchor_lang::solana_program::sysvar::rent::Rent::get()?;
        let state_rent = rent.minimum_balance(JACKPOT_STATE_SPACE);
        let vault_rent = ctx.accounts.jackpot_vault.to_account_info().lamports();
        let required_lamports = state_rent
            .checked_add(vault_rent)
            .ok_or(KotsError::ArithmeticOverflow)?;
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(), // payer gets reimbursed
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id, // this program ID for validation
            &ctx.accounts.cpi_authority.to_account_info(),
            &kots_common::init::InitOptions::default(),
        )?;

        msg!("✅ Progressive jackpot initialized with {} KOTS", initial_amount / TOKENS_PER_UNIT);
        Ok(())
    }

    /// Award the accumulated pool to a winner.
    ///
    /// §5a de-chancing: the recipient is NOT drawn. The pool pays the season
    /// championship — distributed across the annual leaderboard on a fixed
    /// schedule (e.g. 50/25/15/10 to the top four) from cumulative contest
    /// results. The authority computes recipients deterministically from
    /// leaderboard state; this instruction only executes the transfer.
    /// (Program name kept for the §5a redeploy naming decision.)
    ///
    /// LIFETIMES (2026-07-23): the elided `Context<AwardJackpot>` gives the
    /// accounts struct and `remaining_accounts` two INDEPENDENT `'info`
    /// lifetimes. This handler feeds both into one CPI — `token_program` and the
    /// token accounts from `ctx.accounts`, plus the transfer hook's extra metas
    /// from `ctx.remaining_accounts` — and both `Interface<'info, T>` and
    /// `AccountInfo<'info>` are INVARIANT over `'info`, so neither can be
    /// coerced to the other. Naming a single `'info` across the struct and the
    /// context's 4th parameter is the standard Anchor fix for CPIs that forward
    /// remaining_accounts.
    pub fn award_jackpot<'info>(
        ctx: Context<'_, '_, '_, 'info, AwardJackpot<'info>>,
        winner: Pubkey,
        contest_label: [u8; 32],
    ) -> Result<()> {
        // EMERGENCY PAUSE GATE (2026-08-12): freeze the jackpot payout while the
        // platform is paused. Reads security-governance's EmergencyState PDA
        // (honours pause_duration; fails closed on an absent PDA, which exists on
        // any chain paused at least once). Resume is never gated.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);

        let binding = String::from_utf8_lossy(&contest_label);
        let contest_label_str = binding.trim_end_matches('\0');
        msg!("🎉 Awarding progressive jackpot to winner: {} for contest: {}", winner, contest_label_str);
        
        // Snapshot everything needed BEFORE any mutable borrow of jackpot_state,
        // which is now also the vault's token authority (and therefore a CPI
        // signer below).
        let vault_amount = ctx.accounts.jackpot_vault.amount;
        let decimals = ctx.accounts.kots_mint.decimals;
        let state_bump = ctx.bumps.jackpot_state;

        {
            let jackpot = &ctx.accounts.jackpot_state;
            require!(jackpot.is_initialized, KotsError::JackpotNotInitialized);
            require!(jackpot.total_jackpot_amount > 0, KotsError::NoJackpotAmount);
        }

        // SECURITY (2026-07-23): the `winner` ARGUMENT must own the
        // `winner_account` TOKEN ACCOUNT. Previously the two were unrelated: the
        // instruction logged (and recorded in `last_winner`) one pubkey while
        // paying whatever token account the caller passed.
        require_keys_eq!(
            ctx.accounts.winner_account.owner,
            winner,
            KotsError::UnauthorizedCaller
        );

        // SECURITY (2026-07-23): `total_jackpot_amount` is ACCOUNTING state that
        // `manage_progressive_jackpot` writes. It is not a proof of custody. Pay
        // out no more than the vault actually holds, so a corrupted or inflated
        // accounting figure cannot over-draw the vault.
        let award_amount = core::cmp::min(
            ctx.accounts.jackpot_state.total_jackpot_amount,
            vault_amount,
        );
        require!(award_amount > 0, KotsError::NoJackpotAmount);

        msg!("💰 Transferring {} KOTS to winner", award_amount / TOKENS_PER_UNIT);

        // TOKEN-2022 PORT (2026-07-23). This program previously used the LEGACY
        // SPL Token program: `anchor_spl::token::{Token, TokenAccount}` and the
        // deprecated `token::transfer`. KOTS is Token-2022 WITH A TRANSFER HOOK
        // (KOTS-TOKENOMICS §1 — the entire transfer-hook program exists for it),
        // so that code could never have moved a single token. It failed three
        // ways at once: a legacy `TokenAccount` will not deserialize a Token-2022
        // account, `Program<'info, Token>` pins a program id that does not own
        // the mint, and Token-2022 requires `transfer_checked` rather than
        // `transfer`. This now matches the house pattern in lp-position-manager.
        //
        // THE HOOK: on a hooked mint, Token-2022 CPIs into the transfer hook,
        // which needs its own account set (extra-account-metas, whitelist,
        // pool_registry, fee_config, fee_authority, the five destination wallets,
        // refund_queue, sell_fee_debt...). Those cannot be named in this struct —
        // they are resolved from the mint's ExtraAccountMetaList — so they are
        // forwarded from `remaining_accounts`.
        //
        // CALLER CONTRACT: the client MUST resolve the hook's extra account metas
        // off-chain (`@solana/spl-token`'s resolver, or the sidecar's
        // `AnchorChainGateway`) and append them as remaining_accounts, in the
        // order the ExtraAccountMetaList declares. Passing none will fail inside
        // Token-2022, not here.
        //
        // NOTE: no program in this repo currently forwards hook accounts —
        // lp-position-manager's four `transfer_checked` calls have the same
        // latent failure and need the same treatment.
        //
        // OPS: whitelist the jackpot vault in the transfer hook so the payout is
        // not taxed on the way out (KOTS-WALLET-SECURITY §11).
        let state_seeds: &[&[u8]] = &[b"jackpot_state", b"v4", &[state_bump]];
        // `invoke_transfer_checked` takes &[&[&[u8]]] — a slice of SIGNER SETS,
        // one per PDA signing the CPI, not a single set. Same shape Anchor's
        // CpiContext::new_with_signer wanted. Passing `state_seeds` directly is
        // an E0308 the compiler catches immediately; it is only noted here
        // because the two types read almost identically at a glance.
        let signer_seeds: &[&[&[u8]]] = &[state_seeds];

        // ───────────────────────────── O32 FIX ─────────────────────────────
        // WAS: anchor_spl::token_interface::transfer_checked(..)
        //          .with_remaining_accounts(ctx.remaining_accounts.to_vec())
        //
        // The note above correctly identified that hook accounts must reach the
        // hook — but forwarding them is not enough. Token-2022 does NOT resolve a
        // hooked mint's extra accounts on the CPI path; the caller must. Measured
        // in fee-management on 2026-07-28: 17 accounts forwarded this way, 4
        // received by the hook, AccountNotEnoughKeys (3005).
        //
        // So `award_jackpot` has never been able to pay out a hooked mint, which
        // is a candidate explanation for O10 (the jackpot has never paid).
        //
        // `onchain::invoke_transfer_checked` reads the ExtraAccountMetaList from
        // the accounts it is given, resolves each entry, and appends them in the
        // order the hook declares. The caller must still PASS the hook program,
        // the meta-list PDA and every extra as remaining_accounts — this call
        // resolves them, it does not invent them.
        //
        // The signer seeds carry across unchanged: the vault's authority is the
        // jackpot_state PDA, so this program signs with its own seeds.
        spl_token_2022::onchain::invoke_transfer_checked(
            ctx.accounts.token_program.to_account_info().key,
            ctx.accounts.jackpot_vault.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.winner_account.to_account_info(),
            ctx.accounts.jackpot_state.to_account_info(),
            ctx.remaining_accounts,
            award_amount,
            decimals,
            signer_seeds,
        )?;
        msg!("✅ Transferred {} KOTS tokens to winner successfully", award_amount / TOKENS_PER_UNIT);

        // Update jackpot state (mutable borrow taken only now that the CPI, which
        // needed jackpot_state as a signer AccountInfo, has completed).
        let now = get_current_timestamp()?;
        let jackpot = &mut ctx.accounts.jackpot_state;
        jackpot.last_winner = Some(winner);
        jackpot.last_win_timestamp = now;
        jackpot.total_jackpot_amount = 0; // Reset jackpot
        jackpot.current_cycle_week = 1; // Reset cycle
        jackpot.cycle_start_timestamp = now;
        jackpot.weekly_contributions = [0; 52]; // Reset contributions
        
        msg!("✅ Progressive jackpot awarded and reset");
        Ok(())
    }

    /// Get current jackpot status
    pub fn get_jackpot_status(
        ctx: Context<GetJackpotStatus>,
    ) -> Result<JackpotStatusResult> {
        let jackpot = &ctx.accounts.jackpot_state;
        
        let result = JackpotStatusResult {
            total_amount: jackpot.total_jackpot_amount,
            current_week: jackpot.current_cycle_week,
            cycle_start: jackpot.cycle_start_timestamp,
            last_winner: jackpot.last_winner,
            cycles_completed: jackpot.total_cycles_completed as u64,
            is_initialized: jackpot.is_initialized,
        };
        
        msg!("📊 Jackpot status: {} KOTS, Week {}", 
             result.total_amount / TOKENS_PER_UNIT, result.current_week);
        
        Ok(result)
    }

    /// Initialize the progressive jackpot PROGRAM state (authority of record).
    ///
    /// BUGFIX (2026-07-23): see `initialize_jackpot`. This instruction keeps
    /// `[b"program_state"]` and keeps writing `ProgramUpgradeState` — it is the
    /// one that was already consistent with what `award_jackpot` /
    /// `manage_progressive_jackpot` now read for their authority check. The
    /// accounting state moved out to its own PDA rather than fighting for this
    /// address.
    ///
    /// ORDERING: run this FIRST, then `initialize_jackpot`.
    ///
    /// FRONT-RUNNING (unresolved by design): this is the bootstrap, so it cannot
    /// check an authority that does not exist yet — whoever calls it first
    /// becomes `program_state.authority` for the life of the program. Bundle it
    /// into the deployment transaction at the mainnet ceremony, or add a
    /// hardcoded deployer constraint. Do not leave it callable on a live program
    /// with no authority set.
    pub fn initialize_progressive_jackpot(
        ctx: Context<InitializeProgressiveJackpot>,
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

        msg!("🚀 Initializing Progressive Jackpot Program");

        // Anchor's `init` (see InitializeProgressiveJackpot) created the PDA at
        // [b"program_state"], wrote the discriminator and zero-filled the data.
        // The previous hand-rolled invoke_signed + manual discriminator +
        // `try_serialize` into a raw byte slice is gone.
        #[cfg(feature = "idl-build")]
        let program_type_value = PROGRAM_TYPE_PROGRESSIVE_JACKPOT;
        #[cfg(not(feature = "idl-build"))]
        let program_type_value = ProgramType::ProgressiveJackpot as u8;

        let state = &mut ctx.accounts.program_state;
        state.is_initialized = true;
        state.authority = ctx.accounts.authority.key();
        state.program_type = program_type_value;
        state.version = CURRENT_PROGRESSIVE_JACKPOT_VERSION;
        state.initialization_timestamp = get_current_timestamp()?;

        // Reimburse the payer for the rent `init` just charged them.
        let rent = anchor_lang::solana_program::sysvar::rent::Rent::get()?;
        let required_lamports = rent.minimum_balance(PROGRAM_STATE_SPACE);
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &kots_common::init::InitOptions::default(),
        )?;

        msg!("✅ Progressive Jackpot Program initialized successfully");
        Ok(())
    }
}

// ============================================================================
// ACCOUNT STRUCTS
// ============================================================================

#[derive(Accounts)]
pub struct ManageProgressiveJackpot<'info> {
    #[account(
        mut,
        seeds = [b"jackpot_state", b"v4"],
        bump
    )]
    pub jackpot_state: Account<'info, ProgressiveJackpotState>,

    /// SECURITY (2026-07-23): authority of record (see AwardJackpot).
    #[account(
        seeds = [b"program_state"],
        bump
    )]
    pub program_state: Account<'info, ProgramUpgradeState>,

    /// SECURITY (2026-07-23): was unbound, so ANY signer could add arbitrary
    /// `contribution_amount` to `weekly_contributions` / `total_jackpot_amount`
    /// and force a 52-week cycle rollover at will.
    #[account(
        constraint = authority.key() == program_state.authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

// Removed unused InitializeJackpot struct - using InitializeProgressiveJackpot instead

#[derive(Accounts)]
pub struct AwardJackpot<'info> {
    /// Also the vault's token authority — see `jackpot_vault` below.
    #[account(
        mut,
        seeds = [b"jackpot_state", b"v4"],
        bump
    )]
    pub jackpot_state: Account<'info, ProgressiveJackpotState>,

    /// TOKEN-2022 PORT (2026-07-23): `transfer_checked` requires the mint (it
    /// verifies decimals), and Token-2022 needs it to reach the transfer hook.
    pub kots_mint: InterfaceAccount<'info, Mint>,

    /// TOKEN-2022 PORT (2026-07-23): was `Program<'info, Token>`, which pins the
    /// LEGACY token program id — an id that does not own the KOTS mint.
    /// Declared BEFORE the token accounts so their `token::token_program`
    /// constraints resolve against an already-bound field.
    pub token_program: Interface<'info, TokenInterface>,

    /// Winner's KOTS token account. Bound to the `winner` argument in the
    /// handler via `winner_account.owner == winner`, and to the mint here.
    #[account(
        mut,
        token::mint = kots_mint,
        token::token_program = token_program
    )]
    pub winner_account: InterfaceAccount<'info, TokenAccount>,

    /// BUGFIX (2026-07-23): created by `initialize_jackpot`. Nothing created it
    /// before, so this instruction failed account validation on every call even
    /// after the jackpot_state PDA was fixed.
    #[account(
        mut,
        seeds = [VAULT_SEED, b"progressive_jackpot"],
        bump,
        token::mint = kots_mint,
        token::authority = jackpot_state,
        token::token_program = token_program
    )]
    pub jackpot_vault: InterfaceAccount<'info, TokenAccount>,

    /// SECURITY (2026-07-23): the program's own authority record, written at
    /// `initialize_progressive_jackpot`. Used as the authority of record so that
    /// `authority` below is bound to something persistent rather than to nothing.
    #[account(
        seeds = [b"program_state"],
        bump
    )]
    pub program_state: Account<'info, ProgramUpgradeState>,

    /// SECURITY (2026-07-23): was a bare `Signer` bound to nothing — ANY signer
    /// could trigger a payout of the entire pool to any token account.
    #[account(
        constraint = authority.key() == program_state.authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    /// EMERGENCY PAUSE GATE (2026-08-12): security-governance's EmergencyState PDA.
    /// Threaded so the jackpot payout can be frozen while the platform is paused.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
    // NOTE: the transfer hook's accounts are supplied via `remaining_accounts`.
    // See the handler.
}

#[derive(Accounts)]
pub struct GetJackpotStatus<'info> {
    pub jackpot_state: Account<'info, ProgressiveJackpotState>,
}

// ============================================================================
// CANONICAL ACCOUNT CONTEXTS - Single source of truth for all builds
// ============================================================================

#[derive(Accounts)]
pub struct InitializeJackpot<'info> {
    /// BUGFIX (2026-07-23): the 52-week accounting state, created HERE at the
    /// seeds `manage_progressive_jackpot` and `award_jackpot` actually read.
    /// Before this change nothing created it, so both of those instructions
    /// failed account validation on every call.
    #[account(
        init,
        payer = authority,
        space = JACKPOT_STATE_SPACE,
        // NOTE: `.as_ref()` on both elements is required ONLY here. Anchor's
        // `init` codegen passes this array straight to `find_program_address`,
        // which needs `&[&[u8]]` — but `[b"jackpot_state", b"v4"]` infers
        // `[&[u8; 13]; 2]` from its first element and then rejects the 2-byte
        // second one. The read-only `seeds` checks elsewhere in this file
        // compile unchanged because that codegen path calls `.as_ref()` itself.
        // Same bytes, so the PDA address is identical.
        seeds = [b"jackpot_state".as_ref(), b"v4".as_ref()],
        bump
    )]
    pub jackpot_state: Account<'info, ProgressiveJackpotState>,

    /// Authority of record. Created by `initialize_progressive_jackpot`, which
    /// MUST run before this instruction.
    #[account(
        seeds = [b"program_state"],
        bump
    )]
    pub program_state: Account<'info, ProgramUpgradeState>,

    /// Pays the rent for `init`, then is reimbursed by the treasury below.
    #[account(
        mut,
        constraint = authority.key() == program_state.authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    /// TOKEN-2022 PORT (2026-07-23): the KOTS mint. `InterfaceAccount` because
    /// KOTS is Token-2022, not legacy SPL.
    ///
    /// NOT pinned to a constant here: the canonical mint is per-realm
    /// (`kots_common::network_config::SystemWallets::kots_mint` — devnet
    /// `BiPAn9…`, and mainnet's is generated at the ceremony). The `token::mint`
    /// constraints below at least force the vault and the mint to agree.
    /// Consider adding an explicit `address = <realm kots_mint>` check once the
    /// mainnet mint exists.
    pub kots_mint: InterfaceAccount<'info, Mint>,

    /// TOKEN-2022 PORT (2026-07-23): required by the `jackpot_vault` init
    /// constraints below. Must be the Token-2022 program for the KOTS mint.
    /// Declared BEFORE `jackpot_vault` so the `init` constraint referencing it
    /// resolves against an already-bound field.
    pub token_program: Interface<'info, TokenInterface>,

    /// BUGFIX (2026-07-23): the vault `award_jackpot` pays out of. NOTHING in the
    /// repo created it — not the programs, not `ops/`, not the sidecar — so the
    /// payout instruction failed account validation even once its state PDA
    /// existed. Created here, as a PDA token account owned by `jackpot_state`.
    ///
    /// Authority is `jackpot_state` rather than the vault itself: Anchor cannot
    /// resolve a self-referential `token::authority` during `init`, and this
    /// program already holds jackpot_state's seeds to sign with.
    #[account(
        init,
        payer = authority,
        seeds = [VAULT_SEED, b"progressive_jackpot"],
        bump,
        token::mint = kots_mint,
        token::authority = jackpot_state,
        token::token_program = token_program
    )]
    pub jackpot_vault: InterfaceAccount<'info, TokenAccount>,

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

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement` so Treasury can
    /// prove which program is asking to be paid.
    /// CHECK: seeds-derived here, re-derived and verified in treasury-operations
    #[account(
        seeds = [CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    /// Cross-program invocation accounts
    /// CHECK: Treasury operations program for PDA creation
    pub treasury_operations_program: UncheckedAccount<'info>,
    /// CHECK: Self-reference for caller_program parameter
    pub progressive_jackpot_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitializeProgressiveJackpot<'info> {
    /// BUGFIX (2026-07-23): now typed and `init`-ed as what it actually holds.
    /// It previously shared this struct with `initialize_jackpot`, which wrote a
    /// DIFFERENT account type (ProgressiveJackpotState, 512 bytes) to this same
    /// address — so whichever initializer ran second always failed.
    #[account(
        init,
        payer = authority,
        space = PROGRAM_STATE_SPACE,
        seeds = [b"program_state"],
        bump
    )]
    pub program_state: Account<'info, ProgramUpgradeState>,

    /// Becomes `program_state.authority`. See the front-running note on the
    /// handler: this is the bootstrap and cannot check a prior authority.
    #[account(mut)]
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

    /// SECURITY (2026-07-23): see InitializeJackpot::cpi_authority.
    /// CHECK: seeds-derived here, re-derived and verified in treasury-operations
    #[account(
        seeds = [CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    /// Cross-program invocation accounts
    /// CHECK: Treasury operations program for PDA creation
    pub treasury_operations_program: UncheckedAccount<'info>,
    /// CHECK: Self-reference for caller_program parameter
    pub progressive_jackpot_program: UncheckedAccount<'info>,
}

// ============================================================================
// RESULT STRUCTS
// ============================================================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct JackpotStatusResult {
    pub total_amount: u64,
    pub current_week: u8,
    pub cycle_start: i64,
    pub last_winner: Option<Pubkey>,
    pub cycles_completed: u64,
    pub is_initialized: bool,
}
