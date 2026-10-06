// DESTINATION (where this file goes): kots-contracts/programs/security-governance/src/quorum_decision.rs
//
//! On-chain quorum-decision voting — binding M-of-N owner approval recorded as
//! individual on-chain votes, with a fixed voting window and immutable ballots.
//!
//! WHY THIS EXISTS. Today the governed actions (`emergency_resume`, blacklist
//! add/remove, `rotate_owner`) gate on `OwnerRegistry::quorum_met` — the required
//! owners must sign the SAME transaction (co-signed tx). Correct, but hard to
//! operate across owners in different timezones, and the admin UI had no way to
//! collect the second signature. This module moves quorum to an asynchronous,
//! accountable model:
//!   1. an owner OPENS a decision for a specific action (a write) and thereby
//!      casts the first YES,
//!   2. each other owner READS the decision and CASTS an immutable vote (a write),
//!   3. when YES votes reach the registry threshold the decision is ENFORCEABLE
//!      and its action may execute.
//! Every vote is its own on-chain record — non-secret by construction — which is
//! the accountability the co-signed tx never gave.
//!
//! DESIGN (per spec):
//!   * ON-CHAIN, NON-SECRET votes — each ballot is a `DecisionVote` PDA seeded by
//!     (decision, voter); anyone can read who voted for what.
//!   * ONE VOTE PER OWNER, IMMUTABLE — the ballot PDA uses `init`, so a second
//!     vote by the same owner on the same decision fails. You cannot change a vote.
//!   * 1-DAY WINDOW — `VOTE_WINDOW_SECONDS = 86_400`. Comfortably covers owners in
//!     different timezones. Because the window is long, MANY decisions can be open
//!     at once; each is an independent PDA keyed by its `decision_id`, so they do
//!     not interfere. (Enumeration for the UI is a getProgramAccounts scan over
//!     `GovernanceDecision`, or indexing `DecisionOpened` events — a client
//!     concern, not on-chain.)
//!   * VOTING STAYS OPEN UNTIL EXECUTED, NOT UNTIL THRESHOLD. Reaching quorum does
//!     NOT close voting: on a 2-of-4, after the 2nd YES the 3rd and 4th owners can
//!     still cast their ballots (for the record) any time before the decision is
//!     executed and while the window is open. Only execution (or window expiry)
//!     closes voting. Practically: for a routine decision, execute after everyone
//!     has weighed in or the window closes; for an emergency, execute the moment
//!     it is enforceable (later ballots are then simply moot).
//!   * OWNERS ONLY — break-glass is deliberately NOT a voter; it stays the
//!     separate recovery key used only by `rotate_owner`'s break-glass branch.
//!   * BINDING, ONE-OWNER-ONE-VOTE — not the advisory, stake-weighted
//!     `vote_upgrade_proposal`. Threshold is read from `OwnerRegistry`.
//!
//! INTEGRATION (in lib.rs) — Anchor instruction fns must live in `#[program]`:
//!   pub mod quorum_decision;
//!   use quorum_decision::*;
//!   // inside #[program]:
//!   pub fn open_decision(ctx: Context<OpenDecision>, decision_id: u64, action: GovernedAction) -> Result<()> {
//!       quorum_decision::open_decision_handler(ctx, decision_id, action)
//!   }
//!   pub fn cast_vote(ctx: Context<CastVote>, decision_id: u64, approve: bool, expected_action_hash: [u8; 32]) -> Result<()> {
//!       quorum_decision::cast_vote_handler(ctx, decision_id, approve, expected_action_hash)
//!   }
//!
//! ACTION COMMITMENT (2026-08-14). `cast_vote` carries `expected_action_hash`,
//! the sha256 of a domain prefix + the borsh-encoded `GovernedAction`, and the
//! handler REQUIRES it to match the decision's stored action. The signed bytes
//! therefore commit to WHAT is being approved, not merely to a decision id: a
//! UI (or a compromised core/sidecar) that displays "Resume" over a decision
//! that actually carries RotateOwner produces a transaction that FAILS with
//! `ActionHashMismatch` instead of silently recording a mislabeled vote — the
//! Squads vault-transaction pattern, applied to our ballots. The hash shown to
//! the owner is independently checkable in kots-console (decision viewer),
//! which recomputes it from the owner's OWN RPC.
//! Each governed action then swaps its `quorum_met(...)` block for
//! `require_enforceable_and_consume(...)` — see the worked EMERGENCY_RESUME example
//! at the bottom, and the accompanying integration patch.
//!
//! NOT compiled/tested here (no anchor/solana toolchain). Most security-critical
//! code in the program: `anchor build`, run the companion tests, and review before
//! mainnet.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hashv;
use crate::owner_registry::{OwnerRegistry, OWNER_REGISTRY_SEED};

pub const DECISION_SEED: &[u8] = b"gov_decision_v1";
pub const DECISION_VOTE_SEED: &[u8] = b"gov_vote_v1";

/// Voting window — 1 day. Votes after `opened_at + this` are refused; a decision
/// that never reaches threshold in the window simply expires.
pub const VOTE_WINDOW_SECONDS: i64 = 86_400;
/// After the voting window closes, how long an already-enforceable decision may
/// still be executed. Bounds a stale, approved destructive action from lingering.
pub const EXECUTION_GRACE_SECONDS: i64 = 3_600;

// 8 disc + action(<=65) + proposer(32) + decision_id(8) + opened_at(8)
// + expires_at(8) + yes(1) + no(1) + threshold(1) + status(1) + bump(1)
pub const GOVERNANCE_DECISION_SPACE: usize = 8 + 65 + 32 + 8 + 8 + 8 + 1 + 1 + 1 + 1 + 1;
// 8 disc + decision(32) + voter(32) + approve(1) + timestamp(8) + bump(1)
pub const DECISION_VOTE_SPACE: usize = 8 + 32 + 32 + 1 + 8 + 1;

/// The finite set of actions a quorum decision can authorize. Fields bind the
/// decision to its exact params, so a passed `BlacklistAdd{wallet:X}` can only
/// blacklist X. Largest variant (`RotateOwner`) is 1 + 32 + 32 = 65 bytes.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GovernedAction {
    EmergencyResume,
    RotateOwner { old_owner: Pubkey, new_owner: Pubkey },
    BlacklistAdd { wallet: Pubkey },
    BlacklistRemove { wallet: Pubkey },
}

/// Domain prefix for the action-commitment hash (versioned: a future layout
/// change to `GovernedAction` moves to a `:v2` prefix so old hashes can never
/// collide with new encodings). Cheap insurance against cross-protocol
/// preimage reuse — the 32 bytes an owner signs are meaningful ONLY here.
pub const ACTION_HASH_DOMAIN_V1: &[u8] = b"kots:gov_action:v1";

/// The 32-byte commitment a ballot carries: sha256(domain || borsh(action)).
/// Computed identically in three places — here (the enforcer), the sidecar
/// (which embeds it in the built instruction and reports it to the panel), and
/// kots-console codec/security.ts (the owner's INDEPENDENT check) — and pinned
/// by the shared vectors in `action_hash_vectors` below and the console's
/// security-codec test. Borsh of this enum is deterministic (1-byte variant +
/// fixed pubkey fields), so the encoding needs no canonicalization step.
pub fn governed_action_hash(action: &GovernedAction) -> Result<[u8; 32]> {
    let encoded = action
        .try_to_vec()
        .map_err(|_| error!(DecisionError::InvalidAction))?;
    Ok(hashv(&[ACTION_HASH_DOMAIN_V1, &encoded]).to_bytes())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecisionStatus {
    /// Accepting votes (within the window and not yet executed). "Enforceable" is
    /// derived — `yes_votes >= threshold` — and does NOT change this status, so
    /// owners keep voting after quorum is reached.
    Open,
    /// The action has run (consumed). Terminal — no further votes.
    Executed,
}

#[account]
pub struct GovernanceDecision {
    pub action: GovernedAction,
    pub proposer: Pubkey,
    pub decision_id: u64,
    pub opened_at: i64,
    pub expires_at: i64,
    pub yes_votes: u8,
    pub no_votes: u8,
    /// Threshold snapshot at open (M in M-of-N). `is_owner` is still checked live
    /// per vote, so an owner rotated out mid-decision can no longer vote.
    pub threshold: u8,
    pub status: DecisionStatus,
    pub bump: u8,
}

impl GovernanceDecision {
    /// Within the window and still Open → accepting votes.
    pub fn is_open_for_voting(&self, now: i64) -> bool {
        self.status == DecisionStatus::Open && now <= self.expires_at
    }
    /// Window elapsed while still Open (never executed).
    pub fn is_expired(&self, now: i64) -> bool {
        self.status == DecisionStatus::Open && now > self.expires_at
    }
    /// Reached quorum (readable by clients to know it may be executed).
    pub fn has_reached_threshold(&self) -> bool {
        self.yes_votes >= self.threshold
    }
}

#[account]
pub struct DecisionVote {
    pub decision: Pubkey,
    pub voter: Pubkey,
    pub approve: bool,
    pub timestamp: i64,
    pub bump: u8,
}

// ---------------------------------------------------------------------------
// open_decision — an owner opens a decision and casts the first YES
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(decision_id: u64)]
pub struct OpenDecision<'info> {
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(
        init,
        payer = proposer,
        space = GOVERNANCE_DECISION_SPACE,
        seeds = [DECISION_SEED, &decision_id.to_le_bytes()],
        bump
    )]
    pub decision: Account<'info, GovernanceDecision>,

    /// The proposer's ballot — opening IS a YES, and this subjects the proposer to
    /// the one-vote rule (they cannot vote again via `cast_vote`).
    #[account(
        init,
        payer = proposer,
        space = DECISION_VOTE_SPACE,
        seeds = [DECISION_VOTE_SEED, decision.key().as_ref(), proposer.key().as_ref()],
        bump
    )]
    pub proposer_vote: Account<'info, DecisionVote>,

    #[account(mut)]
    pub proposer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn open_decision_handler(
    ctx: Context<OpenDecision>,
    decision_id: u64,
    action: GovernedAction,
) -> Result<()> {
    let registry = &ctx.accounts.owner_registry;
    require!(registry.initialized, DecisionError::RegistryNotInitialized);

    let proposer = ctx.accounts.proposer.key();
    require!(registry.is_owner(&proposer), DecisionError::SignerNotOwner);
    validate_action(registry, &action)?;

    let now = Clock::get()?.unix_timestamp;
    let threshold = registry.threshold;

    let decision = &mut ctx.accounts.decision;
    decision.action = action;
    decision.proposer = proposer;
    decision.decision_id = decision_id;
    decision.opened_at = now;
    decision.expires_at = now + VOTE_WINDOW_SECONDS;
    decision.yes_votes = 1; // the proposer's YES
    decision.no_votes = 0;
    decision.threshold = threshold;
    decision.status = DecisionStatus::Open; // stays Open even if already enforceable
    decision.bump = ctx.bumps.decision;

    let vote = &mut ctx.accounts.proposer_vote;
    vote.decision = decision.key();
    vote.voter = proposer;
    vote.approve = true;
    vote.timestamp = now;
    vote.bump = ctx.bumps.proposer_vote;

    // The commitment the ballots will have to match — emitted so any indexer /
    // listing can show owners the hash to expect without recomputing it.
    let action_hash = governed_action_hash(&action)?;

    emit!(DecisionOpened {
        decision: decision.key(),
        decision_id,
        proposer,
        action,
        action_hash,
        threshold,
        expires_at: decision.expires_at,
        timestamp: now,
    });
    // Threshold can be reached on open (e.g. a 1-of-N registry). Signal it once;
    // do NOT change status — the action executes via `require_enforceable_and_consume`.
    if decision.yes_votes >= threshold {
        emit!(ThresholdReached { decision: decision.key(), decision_id, yes_votes: decision.yes_votes, timestamp: now });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cast_vote — another owner records an immutable YES/NO; open until executed
// ---------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(decision_id: u64)]
pub struct CastVote<'info> {
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    #[account(
        mut,
        seeds = [DECISION_SEED, &decision_id.to_le_bytes()],
        bump = decision.bump
    )]
    pub decision: Account<'info, GovernanceDecision>,

    /// `init` is the one-vote-per-owner enforcer: a second ballot by the same owner
    /// on this decision fails here, so a vote cannot be changed.
    #[account(
        init,
        payer = voter,
        space = DECISION_VOTE_SPACE,
        seeds = [DECISION_VOTE_SEED, decision.key().as_ref(), voter.key().as_ref()],
        bump
    )]
    pub vote: Account<'info, DecisionVote>,

    #[account(mut)]
    pub voter: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn cast_vote_handler(
    ctx: Context<CastVote>,
    _decision_id: u64,
    approve: bool,
    expected_action_hash: [u8; 32],
) -> Result<()> {
    let registry = &ctx.accounts.owner_registry;
    let voter = ctx.accounts.voter.key();
    require!(registry.is_owner(&voter), DecisionError::SignerNotOwner);

    let now = Clock::get()?.unix_timestamp;
    let decision = &mut ctx.accounts.decision;

    // ACTION COMMITMENT: the signed bytes must match the action this decision
    // actually stores. Checked for YES and NO alike — a NO is still a ballot on
    // a specific action. A UI that showed the owner one thing while the account
    // holds another dies HERE, loudly, before any tally moves.
    require!(
        expected_action_hash == governed_action_hash(&decision.action)?,
        DecisionError::ActionHashMismatch
    );

    // Open until EXECUTED or the window elapses. Reaching quorum does not close
    // voting — this is what lets the 3rd/4th owner still register a ballot.
    require!(decision.status == DecisionStatus::Open, DecisionError::VotingClosed);
    require!(now <= decision.expires_at, DecisionError::VotingClosed);

    let threshold = decision.threshold;
    let crossed = if approve {
        let before = decision.yes_votes;
        decision.yes_votes = decision.yes_votes.saturating_add(1);
        before < threshold && decision.yes_votes >= threshold
    } else {
        decision.no_votes = decision.no_votes.saturating_add(1);
        false
    };

    let vote = &mut ctx.accounts.vote;
    vote.decision = decision.key();
    vote.voter = voter;
    vote.approve = approve;
    vote.timestamp = now;
    vote.bump = ctx.bumps.vote;

    emit!(VoteCast {
        decision: decision.key(),
        voter,
        approve,
        yes_votes: decision.yes_votes,
        no_votes: decision.no_votes,
        timestamp: now,
    });
    if crossed {
        emit!(ThresholdReached { decision: decision.key(), decision_id: decision.decision_id, yes_votes: decision.yes_votes, timestamp: now });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// require_enforceable_and_consume — the gate each governed action calls INSTEAD
// of `owner_registry.quorum_met(...)`. Verifies quorum is reached, the action
// matches exactly, and execution is within the grace after the window; marks the
// decision Executed so it authorizes its action exactly once.
// ---------------------------------------------------------------------------

pub fn require_enforceable_and_consume(
    decision: &mut GovernanceDecision,
    decision_key: Pubkey,
    expected: GovernedAction,
    now: i64,
) -> Result<()> {
    require!(decision.status == DecisionStatus::Open, DecisionError::AlreadyExecuted);
    require!(decision.yes_votes >= decision.threshold, DecisionError::NotEnforceable);
    require!(decision.action == expected, DecisionError::ActionMismatch);
    require!(now <= decision.expires_at + EXECUTION_GRACE_SECONDS, DecisionError::ExecutionWindowExpired);
    decision.status = DecisionStatus::Executed;
    emit!(DecisionExecuted { decision: decision_key, decision_id: decision.decision_id, action: decision.action, timestamp: now });
    Ok(())
}

/// Reject structurally-invalid action params before a decision is even opened.
fn validate_action(registry: &OwnerRegistry, action: &GovernedAction) -> Result<()> {
    match action {
        GovernedAction::EmergencyResume => Ok(()),
        GovernedAction::RotateOwner { old_owner, new_owner } => {
            require!(*new_owner != Pubkey::default(), DecisionError::InvalidAction);
            require!(registry.is_owner(old_owner), DecisionError::InvalidAction);
            require!(!registry.is_owner(new_owner), DecisionError::InvalidAction);
            require!(*new_owner != registry.break_glass, DecisionError::InvalidAction);
            Ok(())
        }
        GovernedAction::BlacklistAdd { wallet } | GovernedAction::BlacklistRemove { wallet } => {
            require!(*wallet != Pubkey::default(), DecisionError::InvalidAction);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// WORKED INTEGRATION — emergency_resume, gated by an enforceable decision.
//
// EmergencyResume Accounts struct becomes (drops owner_registry + co-signers,
// adds the decision):
//
//   #[derive(Accounts)]
//   #[instruction(decision_id: u64)]
//   pub struct EmergencyResume<'info> {
//       #[account(mut, seeds = [EMERGENCY_SEED, GOVERNANCE_SUB_SEED], bump)]
//       pub emergency_state: Account<'info, EmergencyState>,
//       pub emergency_authority: Signer<'info>,            // now just invoker/fee-payer
//       #[account(mut, seeds = [DECISION_SEED, &decision_id.to_le_bytes()], bump = decision.bump)]
//       pub decision: Account<'info, GovernanceDecision>,
//   }
//
// Handler (no more remaining_accounts; add decision_id arg):
//
//   pub fn emergency_resume(ctx: Context<EmergencyResume>, decision_id: u64) -> Result<()> {
//       let now = Clock::get()?.unix_timestamp;
//       let decision_key = ctx.accounts.decision.key();
//       quorum_decision::require_enforceable_and_consume(
//           &mut ctx.accounts.decision,
//           decision_key,
//           quorum_decision::GovernedAction::EmergencyResume,
//           now,
//       )?;
//       let emergency = &mut ctx.accounts.emergency_state;
//       require!(emergency.is_paused, KotsError::EmergencyNotActive);
//       emergency.is_paused = false;
//       emergency.pause_reason = [0u8; MAX_STRING_LENGTH];
//       // (audit log)
//       Ok(())
//   }
//
// blacklist add/remove and rotate_owner: identical shape, with
// GovernedAction::BlacklistAdd{wallet}/BlacklistRemove{wallet}/RotateOwner{..}.
// Keep rotate_owner's break-glass branch untouched as the separate recovery path.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

#[event]
pub struct DecisionOpened {
    pub decision: Pubkey,
    pub decision_id: u64,
    pub proposer: Pubkey,
    pub action: GovernedAction,
    /// sha256(ACTION_HASH_DOMAIN_V1 || borsh(action)) — the commitment every
    /// `cast_vote` on this decision must carry.
    pub action_hash: [u8; 32],
    pub threshold: u8,
    pub expires_at: i64,
    pub timestamp: i64,
}

#[event]
pub struct VoteCast {
    pub decision: Pubkey,
    pub voter: Pubkey,
    pub approve: bool,
    pub yes_votes: u8,
    pub no_votes: u8,
    pub timestamp: i64,
}

/// Emitted once, when YES first reaches threshold. The decision is now enforceable
/// (executable); voting nonetheless remains open until executed or the window ends.
#[event]
pub struct ThresholdReached {
    pub decision: Pubkey,
    pub decision_id: u64,
    pub yes_votes: u8,
    pub timestamp: i64,
}

#[event]
pub struct DecisionExecuted {
    pub decision: Pubkey,
    pub decision_id: u64,
    pub action: GovernedAction,
    pub timestamp: i64,
}

// ---------------------------------------------------------------------------
// errors — discriminants above owner_registry's 12000-12008 range
// ---------------------------------------------------------------------------

#[error_code]
pub enum DecisionError {
    #[msg("Owner registry is not initialized")]
    RegistryNotInitialized = 12100,
    #[msg("Signer is not an owner")]
    SignerNotOwner = 12101,
    #[msg("Voting is closed for this decision (executed or window elapsed)")]
    VotingClosed = 12102,
    #[msg("Decision has not reached the quorum threshold")]
    NotEnforceable = 12103,
    #[msg("Decision action does not match the action being executed")]
    ActionMismatch = 12104,
    #[msg("Execution window after the voting window has elapsed")]
    ExecutionWindowExpired = 12105,
    #[msg("Invalid action parameters")]
    InvalidAction = 12106,
    #[msg("Decision has already been executed")]
    AlreadyExecuted = 12107,
    #[msg("Vote's expected_action_hash does not match the decision's stored action")]
    ActionHashMismatch = 12108,
}

// ---------------------------------------------------------------------------
// action-hash vectors — the CODEC TWIN of kots-console/test/security-codec.test.ts
// and kots-core/chain-sidecar/test/security-governance.test.ts. All three pin
// the SAME hex; if one drifts, its build fails, not an owner's vote. Runs with
// plain `cargo test -p security-governance` (hashv works host-side).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod action_hash_vectors {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn domain_is_pinned() {
        assert_eq!(ACTION_HASH_DOMAIN_V1, b"kots:gov_action:v1");
        assert_eq!(ACTION_HASH_DOMAIN_V1.len(), 18);
    }

    #[test]
    fn emergency_resume_vector() {
        assert_eq!(
            governed_action_hash(&GovernedAction::EmergencyResume).unwrap(),
            hex32("9c5bd0eef63714eb0a8e07a7d2760354eef583b11df746c2266063a73c85b96f"),
        );
    }

    #[test]
    fn rotate_owner_vector() {
        let a = GovernedAction::RotateOwner {
            old_owner: Pubkey::new_from_array([1u8; 32]),
            new_owner: Pubkey::new_from_array([2u8; 32]),
        };
        assert_eq!(
            governed_action_hash(&a).unwrap(),
            hex32("f1e0fad79f72f7f9dc084df7e4ac60294b8ef98a9c1d6bbb69160a4765a577fe"),
        );
    }

    #[test]
    fn blacklist_add_vector() {
        let a = GovernedAction::BlacklistAdd { wallet: Pubkey::new_from_array([3u8; 32]) };
        assert_eq!(
            governed_action_hash(&a).unwrap(),
            hex32("f4704ca0e2a7a10f59f01211882bf110754bbca2bab1e6cb93f3aa6607be3539"),
        );
    }

    #[test]
    fn blacklist_remove_vector() {
        let a = GovernedAction::BlacklistRemove { wallet: Pubkey::new_from_array([4u8; 32]) };
        assert_eq!(
            governed_action_hash(&a).unwrap(),
            hex32("54a27b4c553bcfaccf6d2ad812e6106b0501375f81346e13494795edb13d8197"),
        );
    }

    #[test]
    fn params_change_the_hash() {
        // The commitment binds PARAMS, not just the variant: blacklisting X and
        // blacklisting Y must be different signatures.
        let x = GovernedAction::BlacklistAdd { wallet: Pubkey::new_from_array([3u8; 32]) };
        let y = GovernedAction::BlacklistAdd { wallet: Pubkey::new_from_array([9u8; 32]) };
        assert_ne!(governed_action_hash(&x).unwrap(), governed_action_hash(&y).unwrap());
    }
}
