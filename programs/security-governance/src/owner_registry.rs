//! On-chain owner registry — the chain-enforced source of truth for "who is an
//! owner" and "what quorum authorizes a governed action".
//!
//! This is piece #1 of moving governance on-chain: it holds the owner set, the
//! M-of-N threshold, and the break-glass key in a program-owned PDA, and it
//! exposes the validation primitives (`is_owner`, `count_owner_signers`,
//! `quorum_met`, `is_break_glass`) that the later pieces — fixed 2-of-3
//! approvals and the treasury payout timelock+veto — will call. Nothing here
//! moves tokens.
//!
//! DELIBERATE DEVIATION from the rest of security-governance: this account is
//! created with Anchor's declarative `init` (the initializer pays the trivial
//! one-time rent), NOT the treasury-reimbursement CPI + manual `try_serialize`
//! pattern the operational accounts use. A one-time governance-setup account is
//! small (no BPF-stack pressure) and should not be coupled to treasury-
//! operations. If you'd rather it match the manual pattern, say so and I'll
//! rewrite the init.
//!
//! Integration (in lib.rs), because Anchor instruction fns must live in the
//! `#[program]` module:
//!
//!   // file scope, near the other `use`s:
//!   pub mod owner_registry;
//!   use owner_registry::*;   // or an explicit import list if a name clashes
//!
//!   // inside `#[program] pub mod security_governance { ... }`:
//!   pub fn initialize_owner_registry(
//!       ctx: Context<InitializeOwnerRegistry>,
//!       owners: [Pubkey; owner_registry::MAX_OWNERS],
//!       owner_count: u8,
//!       threshold: u8,
//!       break_glass: Pubkey,
//!   ) -> Result<()> {
//!       owner_registry::initialize_handler(ctx, owners, owner_count, threshold, break_glass)
//!   }
//!
//!   pub fn rotate_owner<'info>(
//!       ctx: Context<'_, '_, '_, 'info, RotateOwner<'info>>,
//!       old_owner: Pubkey,
//!       new_owner: Pubkey,
//!   ) -> Result<()> {
//!       owner_registry::rotate_handler(ctx, old_owner, new_owner)
//!   }
//!
//! (The explicit `'info` on rotate_owner is required: rotate_handler ties
//! `remaining_accounts` and the accounts struct to one `'info`, so the wrapper
//! must forward that tie rather than the elided, independent lifetimes.)

use anchor_lang::prelude::*;

/// Max slots in the owner array (BPF-safe fixed size). Only the first
/// `owner_count` are active; the rest are the zero key. Sized above the current
/// three partners so an owner can be added later without a migration.
pub const MAX_OWNERS: usize = 5;

/// PDA seed. Versioned like the program's other seeds (`proposal_v4`, etc.) so a
/// future breaking change to the layout can move to a fresh address.
pub const OWNER_REGISTRY_SEED: &[u8] = b"owner_registry_v1";

// 8 discriminator + owners(32*MAX_OWNERS) + owner_count(1) + threshold(1)
// + break_glass(32) + version(2) + bump(1) + initialized(1)
pub const OWNER_REGISTRY_SPACE: usize = 8 + (32 * MAX_OWNERS) + 1 + 1 + 32 + 2 + 1 + 1;

/// PDA seed for the break-glass control account (disabled flag + pending timelocked
/// rotation). Kept in its OWN account so the already-deployed registry (sized with
/// no spare bytes) is never realloc'd.
pub const BREAK_GLASS_CONTROL_SEED: &[u8] = b"break_glass_control_v1";

/// A break-glass-initiated owner rotation cannot execute until this long after it
/// is proposed — the window during which every owner is emailed and any ONE owner
/// can veto it. 24 hours.
pub const BREAK_GLASS_ROTATION_TIMELOCK: i64 = 86_400;

// 8 discriminator + disabled(1) + pending_old(32) + pending_new(32)
// + pending_execute_after(8) + bump(1)
pub const BREAK_GLASS_CONTROL_SPACE: usize = 8 + 1 + 32 + 32 + 8 + 1;

/// The chain-enforced owner set. A single instance per program, at the PDA
/// derived from `OWNER_REGISTRY_SEED`.
#[account]
pub struct OwnerRegistry {
    /// Active owner keys occupy `owners[..owner_count]`; the remainder are zero.
    pub owners: [Pubkey; MAX_OWNERS],
    /// Number of active owners (N in M-of-N).
    pub owner_count: u8,
    /// Signatures required to authorize a governed action (M in M-of-N).
    pub threshold: u8,
    /// The break-glass key — one of the 2-of-4 keys (A.1), never an owner. It may
    /// authorize a governed action only WITH at least one owner co-signer (it
    /// cannot act alone); every use emits `BreakGlassInvoked`, the loudest signal
    /// we have.
    pub break_glass: Pubkey,
    /// Layout version, for forward migrations.
    pub version: u16,
    /// PDA bump.
    pub bump: u8,
    /// Set once at initialization; a consumer can assert it before trusting the
    /// account.
    pub initialized: bool,
}

impl OwnerRegistry {
    /// Is `key` one of the active owners?
    pub fn is_owner(&self, key: &Pubkey) -> bool {
        self.owners[..self.owner_count as usize]
            .iter()
            .any(|k| k == key)
    }

    /// Is `key` the break-glass authority?
    pub fn is_break_glass(&self, key: &Pubkey) -> bool {
        &self.break_glass == key
    }

    /// Count the DISTINCT active owners that actually signed among the provided
    /// account infos. Non-signers, non-owners, and duplicates are ignored, so a
    /// caller cannot inflate the count by passing the same owner twice. This is
    /// the primitive the later governance pieces call to enforce M-of-N.
    pub fn count_owner_signers(&self, infos: &[AccountInfo]) -> u8 {
        let mut seen: [Pubkey; MAX_OWNERS] = [Pubkey::default(); MAX_OWNERS];
        let mut n: usize = 0;
        for ai in infos.iter() {
            if !ai.is_signer {
                continue;
            }
            if !self.is_owner(ai.key) {
                continue;
            }
            if seen[..n].iter().any(|s| s == ai.key) {
                continue;
            }
            if n < MAX_OWNERS {
                seen[n] = *ai.key;
                n += 1;
            }
        }
        n as u8
    }

    /// Did at least `threshold` distinct owners sign?
    pub fn quorum_met(&self, infos: &[AccountInfo]) -> bool {
        self.count_owner_signers(infos) >= self.threshold
    }
}

/// Break-glass control state, in its own PDA. Holds the kill-switch flag and the
/// single in-flight timelocked rotation (if any). Must be initialized once
/// (`initialize_break_glass_control`) after this program is upgraded, so the
/// break-glass recovery path is ready before it is ever needed.
#[account]
pub struct BreakGlassControl {
    /// When true, break-glass cannot propose or execute a rotation. Set by ANY
    /// single owner (`report_unauthorized_break_glass`); cleared only by owner
    /// quorum (`enable_break_glass`) — easy to stop, hard to restart.
    pub disabled: bool,
    /// The pending rotation's target owner slot (the key being replaced). Zero
    /// when no rotation is pending.
    pub pending_old_owner: Pubkey,
    /// The pending rotation's incoming owner key. Zero when none pending.
    pub pending_new_owner: Pubkey,
    /// Unix time at/after which the pending rotation may execute. 0 = NONE pending.
    pub pending_execute_after: i64,
    /// PDA bump.
    pub bump: u8,
}

impl BreakGlassControl {
    /// Is a timelocked rotation currently in flight?
    pub fn has_pending(&self) -> bool {
        self.pending_execute_after != 0
    }

    /// Clear any pending rotation (used on execute and on veto).
    pub fn clear_pending(&mut self) {
        self.pending_old_owner = Pubkey::default();
        self.pending_new_owner = Pubkey::default();
        self.pending_execute_after = 0;
    }
}

// ---------------------------------------------------------------------------
// initialize_owner_registry
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeOwnerRegistry<'info> {
    #[account(
        init,
        payer = initializer,
        space = OWNER_REGISTRY_SPACE,
        seeds = [OWNER_REGISTRY_SEED],
        bump
    )]
    pub owner_registry: Account<'info, OwnerRegistry>,

    /// Pays the one-time rent and, on mainnet, is expected to BE the ceremony
    /// multisig — same first-writer trust model the program's other
    /// initializers use, mitigated by the ceremony controlling the deploy
    /// window. `init` itself makes a second call fail, so re-init is not a
    /// front-running vector after the first.
    #[account(mut)]
    pub initializer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn initialize_handler(
    ctx: Context<InitializeOwnerRegistry>,
    owners: [Pubkey; MAX_OWNERS],
    owner_count: u8,
    threshold: u8,
    break_glass: Pubkey,
) -> Result<()> {
    require!(
        owner_count >= 1 && (owner_count as usize) <= MAX_OWNERS,
        OwnerRegistryError::InvalidOwnerCount
    );
    require!(
        threshold >= 1 && threshold <= owner_count,
        OwnerRegistryError::InvalidThreshold
    );
    require!(
        break_glass != Pubkey::default(),
        OwnerRegistryError::ZeroPubkey
    );

    // Validate the active owners: non-zero, distinct, none equal to break-glass.
    for i in 0..owner_count as usize {
        require!(owners[i] != Pubkey::default(), OwnerRegistryError::ZeroPubkey);
        require!(
            owners[i] != break_glass,
            OwnerRegistryError::BreakGlassCannotBeOwner
        );
        for j in (i + 1)..owner_count as usize {
            require!(owners[i] != owners[j], OwnerRegistryError::DuplicateOwner);
        }
    }

    let registry = &mut ctx.accounts.owner_registry;
    registry.owners = owners; // inactive slots carry the zero key
    registry.owner_count = owner_count;
    registry.threshold = threshold;
    registry.break_glass = break_glass;
    registry.version = 1;
    registry.bump = ctx.bumps.owner_registry;
    registry.initialized = true;

    emit!(OwnerRegistryInitialized {
        owners,
        owner_count,
        threshold,
        break_glass,
        timestamp: Clock::get()?.unix_timestamp,
    });
    msg!(
        "Owner registry initialized: {}-of-{} + break-glass",
        threshold,
        owner_count
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// rotate_owner — replace one owner key, authorized by owner quorum OR break-glass
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct RotateOwner<'info> {
    #[account(mut, seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    /// The primary invoker: either the break-glass key or one of the co-signing
    /// owners. Additional owner co-signers are passed as remaining_accounts, each
    /// a read-only signer. Break-glass requires at least one owner co-signer
    /// (A.1: it is a 2-of-4 key, not a unilateral actor).
    pub authority: Signer<'info>,
}

// LIFETIMES: the elided `Context<RotateOwner>` would give the accounts struct
// and `remaining_accounts` two INDEPENDENT `'info` lifetimes; the quorum branch
// below builds one `Vec<AccountInfo>` from both (`remaining_accounts.to_vec()` +
// `authority.to_account_info()`), and `AccountInfo<'info>` / `Signer<'info>` are
// invariant in `'info`, so the two must be the SAME lifetime. Same pattern as
// `fee_management::proc_contest_revenue` and `progressive_jackpot::award_jackpot`.
pub fn rotate_handler<'info>(
    ctx: Context<'_, '_, '_, 'info, RotateOwner<'info>>,
    old_owner: Pubkey,
    new_owner: Pubkey,
) -> Result<()> {
    // Validate the incoming key before touching authorization.
    require!(new_owner != Pubkey::default(), OwnerRegistryError::ZeroPubkey);

    let registry = &ctx.accounts.owner_registry;
    require!(
        !registry.is_owner(&new_owner),
        OwnerRegistryError::DuplicateOwner
    );
    require!(
        new_owner != registry.break_glass,
        OwnerRegistryError::BreakGlassCannotBeOwner
    );
    let idx = registry.owners[..registry.owner_count as usize]
        .iter()
        .position(|k| k == &old_owner)
        .ok_or(OwnerRegistryError::OwnerNotFound)?;

    let authority_key = ctx.accounts.authority.key();

    // BREAK-GLASS NO LONGER ROTATES INSTANTLY (2026-08-15). A break-glass key may
    // only rotate an owner through the 24h-timelocked, owner-vetoable path
    // (`propose_break_glass_rotation` -> wait -> `execute_break_glass_rotation`),
    // so a compromised break-glass key can, at worst, propose a change that sits
    // visibly for a day while owners are alerted and any one owner can veto it.
    // `rotate_owner` is therefore OWNER-QUORUM ONLY now.
    require!(
        !registry.is_break_glass(&authority_key),
        OwnerRegistryError::BreakGlassMustUseTimelock
    );

    // Quorum: the authority (an owner) plus any owner co-signers passed as
    // remaining_accounts, distinct, must reach the threshold.
    let mut infos: Vec<AccountInfo> = ctx.remaining_accounts.to_vec();
    infos.push(ctx.accounts.authority.to_account_info());
    require!(registry.quorum_met(&infos), OwnerRegistryError::QuorumNotMet);

    let registry = &mut ctx.accounts.owner_registry;
    registry.owners[idx] = new_owner;

    emit!(OwnerRotated {
        old_owner,
        new_owner,
        via_break_glass: false,
        timestamp: Clock::get()?.unix_timestamp,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// break-glass control: init, timelocked rotation (propose -> execute), veto, re-enable
// ---------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeBreakGlassControl<'info> {
    #[account(
        init,
        payer = initializer,
        space = BREAK_GLASS_CONTROL_SPACE,
        seeds = [BREAK_GLASS_CONTROL_SEED],
        bump
    )]
    pub break_glass_control: Account<'info, BreakGlassControl>,

    /// Pays one-time rent. Anyone may create this account; it starts ENABLED with
    /// no pending rotation, so creating it only makes the break-glass recovery
    /// path available (still timelocked + vetoable). Do this right after upgrade.
    #[account(mut)]
    pub initializer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn initialize_break_glass_control_handler(
    ctx: Context<InitializeBreakGlassControl>,
) -> Result<()> {
    let control = &mut ctx.accounts.break_glass_control;
    control.disabled = false;
    control.pending_old_owner = Pubkey::default();
    control.pending_new_owner = Pubkey::default();
    control.pending_execute_after = 0;
    control.bump = ctx.bumps.break_glass_control;
    msg!("Break-glass control initialized (enabled, no pending rotation)");
    Ok(())
}

#[derive(Accounts)]
pub struct ProposeBreakGlassRotation<'info> {
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(mut, seeds = [BREAK_GLASS_CONTROL_SEED], bump = break_glass_control.bump)]
    pub break_glass_control: Account<'info, BreakGlassControl>,

    /// The break-glass key. At least one OWNER co-signer must be passed as a
    /// read-only signer in remaining_accounts (A.1: break-glass never acts alone).
    pub authority: Signer<'info>,
}

pub fn propose_break_glass_rotation_handler(
    ctx: Context<ProposeBreakGlassRotation>,
    old_owner: Pubkey,
    new_owner: Pubkey,
) -> Result<()> {
    let registry = &ctx.accounts.owner_registry;
    let control = &ctx.accounts.break_glass_control;

    require!(!control.disabled, OwnerRegistryError::BreakGlassIsDisabled);
    require!(!control.has_pending(), OwnerRegistryError::PendingRotationExists);

    // Only the break-glass key may propose, and only WITH >= 1 owner co-signer.
    let authority_key = ctx.accounts.authority.key();
    require!(
        registry.is_break_glass(&authority_key),
        OwnerRegistryError::NotBreakGlass
    );
    require!(
        registry.count_owner_signers(ctx.remaining_accounts) >= 1,
        OwnerRegistryError::BreakGlassNeedsOwnerCosigner
    );

    // Validate the rotation targets against the current owner set.
    require!(new_owner != Pubkey::default(), OwnerRegistryError::ZeroPubkey);
    require!(!registry.is_owner(&new_owner), OwnerRegistryError::DuplicateOwner);
    require!(
        new_owner != registry.break_glass,
        OwnerRegistryError::BreakGlassCannotBeOwner
    );
    require!(registry.is_owner(&old_owner), OwnerRegistryError::OwnerNotFound);

    let now = Clock::get()?.unix_timestamp;
    let execute_after = now + BREAK_GLASS_ROTATION_TIMELOCK;

    let control = &mut ctx.accounts.break_glass_control;
    control.pending_old_owner = old_owner;
    control.pending_new_owner = new_owner;
    control.pending_execute_after = execute_after;

    emit!(BreakGlassInvoked {
        actor: authority_key,
        operation: "propose_break_glass_rotation".to_string(),
        timestamp: now,
    });
    emit!(BreakGlassRotationProposed {
        old_owner,
        new_owner,
        execute_after,
        timestamp: now,
    });
    msg!("Break-glass rotation proposed; executable after the timelock unless vetoed");
    Ok(())
}

#[derive(Accounts)]
pub struct ExecuteBreakGlassRotation<'info> {
    #[account(mut, seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(mut, seeds = [BREAK_GLASS_CONTROL_SEED], bump = break_glass_control.bump)]
    pub break_glass_control: Account<'info, BreakGlassControl>,

    /// ANYONE may execute a matured, non-vetoed proposal — the timelock + the
    /// owner veto window are the protection, and the proposal was already
    /// authorized by break-glass + an owner. Just a fee-payer.
    pub executor: Signer<'info>,
}

pub fn execute_break_glass_rotation_handler(
    ctx: Context<ExecuteBreakGlassRotation>,
) -> Result<()> {
    let control = &ctx.accounts.break_glass_control;
    require!(!control.disabled, OwnerRegistryError::BreakGlassIsDisabled);
    require!(control.has_pending(), OwnerRegistryError::NoPendingRotation);

    let now = Clock::get()?.unix_timestamp;
    require!(
        now >= control.pending_execute_after,
        OwnerRegistryError::TimelockNotElapsed
    );

    let old_owner = control.pending_old_owner;
    let new_owner = control.pending_new_owner;

    // Re-validate against the CURRENT owner set — it may have changed during the
    // 24h window (e.g. owners rotated a key, or the target already left).
    let registry = &ctx.accounts.owner_registry;
    require!(new_owner != Pubkey::default(), OwnerRegistryError::ZeroPubkey);
    require!(!registry.is_owner(&new_owner), OwnerRegistryError::DuplicateOwner);
    require!(
        new_owner != registry.break_glass,
        OwnerRegistryError::BreakGlassCannotBeOwner
    );
    let idx = registry.owners[..registry.owner_count as usize]
        .iter()
        .position(|k| k == &old_owner)
        .ok_or(OwnerRegistryError::OwnerNotFound)?;

    let registry = &mut ctx.accounts.owner_registry;
    registry.owners[idx] = new_owner;

    let control = &mut ctx.accounts.break_glass_control;
    control.clear_pending();

    emit!(OwnerRotated {
        old_owner,
        new_owner,
        via_break_glass: true,
        timestamp: now,
    });
    msg!("Break-glass rotation executed after timelock");
    Ok(())
}

#[derive(Accounts)]
pub struct ReportUnauthorizedBreakGlass<'info> {
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(mut, seeds = [BREAK_GLASS_CONTROL_SEED], bump = break_glass_control.bump)]
    pub break_glass_control: Account<'info, BreakGlassControl>,

    /// ANY single owner may disable break-glass ("easy to stop"): this voids a
    /// pending rotation and blocks new ones until owner quorum re-enables it.
    pub authority: Signer<'info>,
}

pub fn report_unauthorized_break_glass_handler(
    ctx: Context<ReportUnauthorizedBreakGlass>,
) -> Result<()> {
    let registry = &ctx.accounts.owner_registry;
    let authority_key = ctx.accounts.authority.key();
    require!(registry.is_owner(&authority_key), OwnerRegistryError::NotAnOwner);

    let control = &mut ctx.accounts.break_glass_control;
    control.disabled = true;
    control.clear_pending();

    emit!(BreakGlassDisabled {
        reporter: authority_key,
        timestamp: Clock::get()?.unix_timestamp,
    });
    msg!("Break-glass DISABLED by owner; any pending rotation voided");
    Ok(())
}

#[derive(Accounts)]
pub struct EnableBreakGlass<'info> {
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(mut, seeds = [BREAK_GLASS_CONTROL_SEED], bump = break_glass_control.bump)]
    pub break_glass_control: Account<'info, BreakGlassControl>,

    /// Re-enabling is QUORUM-gated ("hard to restart"): the authority plus owner
    /// co-signers in remaining_accounts must reach the M-of-N threshold.
    pub authority: Signer<'info>,
}

pub fn enable_break_glass_handler<'info>(
    ctx: Context<'_, '_, '_, 'info, EnableBreakGlass<'info>>,
) -> Result<()> {
    let registry = &ctx.accounts.owner_registry;
    let mut infos: Vec<AccountInfo> = ctx.remaining_accounts.to_vec();
    infos.push(ctx.accounts.authority.to_account_info());
    require!(registry.quorum_met(&infos), OwnerRegistryError::QuorumNotMet);

    let control = &mut ctx.accounts.break_glass_control;
    control.disabled = false;

    emit!(BreakGlassEnabled {
        timestamp: Clock::get()?.unix_timestamp,
    });
    msg!("Break-glass RE-ENABLED by owner quorum");
    Ok(())
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

#[event]
pub struct OwnerRegistryInitialized {
    pub owners: [Pubkey; MAX_OWNERS],
    pub owner_count: u8,
    pub threshold: u8,
    pub break_glass: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct OwnerRotated {
    pub old_owner: Pubkey,
    pub new_owner: Pubkey,
    pub via_break_glass: bool,
    pub timestamp: i64,
}

#[event]
pub struct BreakGlassInvoked {
    pub actor: Pubkey,
    pub operation: String,
    pub timestamp: i64,
}

#[event]
pub struct BreakGlassRotationProposed {
    pub old_owner: Pubkey,
    pub new_owner: Pubkey,
    pub execute_after: i64,
    pub timestamp: i64,
}

#[event]
pub struct BreakGlassDisabled {
    pub reporter: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct BreakGlassEnabled {
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// errors — explicit discriminants above KotsError's range to avoid collision
// ---------------------------------------------------------------------------

#[error_code]
pub enum OwnerRegistryError {
    #[msg("Owner count must be between 1 and MAX_OWNERS")]
    InvalidOwnerCount = 12000,
    #[msg("Threshold must be between 1 and the owner count")]
    InvalidThreshold = 12001,
    #[msg("Duplicate owner key")]
    DuplicateOwner = 12002,
    #[msg("Pubkey must not be the zero key")]
    ZeroPubkey = 12003,
    #[msg("Break-glass key must not also be an owner")]
    BreakGlassCannotBeOwner = 12004,
    #[msg("Old owner not found in the registry")]
    OwnerNotFound = 12005,
    #[msg("Owner quorum not met")]
    QuorumNotMet = 12006,
    #[msg("Break-glass must act with at least one owner co-signer")]
    BreakGlassNeedsOwnerCosigner = 12007,
    #[msg("Signer is not an owner")]
    NotAnOwner = 12008,
    #[msg("Break-glass must rotate via the 24h timelock path, not rotate_owner")]
    BreakGlassMustUseTimelock = 12009,
    #[msg("Break-glass is disabled; owner quorum must re-enable it")]
    BreakGlassIsDisabled = 12010,
    #[msg("A break-glass rotation is already pending")]
    PendingRotationExists = 12011,
    #[msg("No break-glass rotation is pending")]
    NoPendingRotation = 12012,
    #[msg("The break-glass rotation timelock has not elapsed yet")]
    TimelockNotElapsed = 12013,
    #[msg("Signer is not the break-glass authority")]
    NotBreakGlass = 12014,
}
