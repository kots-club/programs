//! Shared world bootstrap for the integration suite.
//!
//! Register in `tests/lib.rs`:  `pub mod bootstrap;`
//!
//! # Why this exists
//!
//! On 2026-07-28 the security-regression suite reported `6 passed; 0 failed`
//! while **four of the six verified nothing**. Each asserted only
//! `res.is_err()`, and each transaction died at Anchor's account-existence check
//! — `AccountNotInitialized` (3012) — before any authority logic ran. A
//! correctly authorised caller would have been rejected identically. The
//! assertion message on one of them claimed it would catch "the entire pool is
//! drainable"; it had never been in a position to.
//!
//! The cause was not carelessness. It was that standing up the accounts needed
//! for a **positive leg** took eighty lines of hand-written instruction
//! building per test, so nobody wrote one. This module makes that three lines,
//! which is the only durable fix: a convention that is expensive to follow will
//! not be followed.
//!
//! # The rule this enables
//!
//! **A rejection test without a paired positive control is not a test.**
//!
//! `allows_a_free_contest_and_rejects_a_dust_fee` is the model — it is
//! trustworthy precisely because it proves the instruction works before proving
//! the guard bites. Write both halves:
//!
//! ```ignore
//! let w = bootstrap::world(&mut ctx).await;
//!
//! // POSITIVE: the authorised caller succeeds. Without this, the negative
//! // below passes just as happily against a completely broken instruction.
//! process(&mut ctx, award_ix(w.jackpot, ctx.payer.pubkey(), winner))
//!     .await
//!     .expect("the authority must be able to award");
//!
//! // NEGATIVE: and only then, that the wrong signer is refused —
//! // for the RIGHT reason.
//! let res = process(&mut ctx, award_ix(w.jackpot, attacker.pubkey(), winner)).await;
//! assert_rejected_with(res, 2003);   // ConstraintRaw, not 3012
//! ```
//!
//! # What this cannot fix
//!
//! `initializer_is_authorized()` returns `true` unconditionally under this
//! harness: the `.so` carries the DEV `declare_id!`, so `realm_of` answers
//! `Some(Realm::Dev)` and that branch short-circuits. Any instruction gated on
//! it — `emergency_pause` among them — cannot have a meaningful authority test
//! here regardless of what this module provides. That is a property of the
//! harness, not a gap in the bootstrap, and `rejects_emergency_pause_from_an_
//! arbitrary_wallet` is `#[ignore]`d as though awaiting a fix it will never get.
//! See Part 0 of KOTS-MAINNET-PLAN.md.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]
#![allow(dead_code)] // helpers are used selectively per test module

use anchor_lang::prelude::*;
use anchor_lang::{InstructionData, ToAccountMetas};
use solana_program_test::BanksClientError;
use solana_sdk::{
    instruction::Instruction,
    signature::Signer as SdkSigner,
    transaction::Transaction,
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;

// NOTE: `anchor_lang::prelude::*` re-exports a ONE-parameter `Result<T>` alias
// (`= core::result::Result<T, anchor_lang::error::Error>`), which shadows the
// std two-parameter form. Every signature carrying a BanksClientError must
// therefore spell `std::result::Result<..>` in full — E0107 otherwise, then a
// confusing E0308 pointing at the match arm rather than the signature.
// `emergency_and_blacklist.rs` already does this; it was not copied.

/// Every address a test needs, derived once.
///
/// Tests re-deriving PDAs by hand is how `query_vote_data` and
/// `batch_cast_hieroglyph_votes` ended up binding seeds their creators never
/// produce (O31 #5/#6). One derivation site, checked against the programs.
#[derive(Clone, Copy, Debug)]
pub struct World {
    // program ids
    pub treasury: Pubkey,
    pub security: Pubkey,
    pub jackpot: Pubkey,

    // treasury-operations
    pub treasury_main: Pubkey,
    pub treasury_incoming: Pubkey,
    pub treasury_outgoing: Pubkey,
    /// C10 (2026-07-28): created by `initialize_treasury_accounts` since today.
    /// Before that nothing created it and `reimburse_creator` /
    /// `emergency_funding` failed on every call, on every network.
    pub reimbursement_queue: Pubkey,

    // security-governance
    pub security_program_state: Pubkey,
    pub blacklist_state: Pubkey,
    pub emergency_state: Pubkey,

    // progressive-jackpot
    pub jackpot_program_state: Pubkey,
    pub jackpot_state: Pubkey,
    pub jackpot_vault: Pubkey,
}

// ---------------------------------------------------------------------------
// PDA derivation — ground truth from each program's own seeds
// ---------------------------------------------------------------------------

pub fn program_state_pda(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"program_state"], program).0
}
pub fn cpi_authority_pda(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[kots_common::state::CPI_AUTHORITY_SEED], program).0
}
pub fn treasury_pdas(top: &Pubkey) -> (Pubkey, Pubkey, Pubkey) {
    (
        Pubkey::find_program_address(&[b"treasury_main_v4"], top).0,
        Pubkey::find_program_address(&[b"treasury_incoming_v4"], top).0,
        Pubkey::find_program_address(&[b"treasury_outgoing_v4"], top).0,
    )
}
pub fn reimbursement_queue_pda(top: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"reimbursement_queue_v4"], top).0
}
pub fn blacklist_state_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"blacklist_v4", b"v4"], sg).0
}
pub fn emergency_state_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"emergency_state_v4", b"v4"], sg).0
}
pub fn jackpot_state_pda(pj: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"jackpot_state".as_ref(), b"v4".as_ref()], pj).0
}
/// `VAULT_SEED` is `b"vault"` — NOT `b"jackpot_vault"`, which is the obvious
/// guess and is wrong. Landed with the 2026-07-23 vault fix.
pub fn jackpot_vault_pda(pj: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[kots_common::state::VAULT_SEED, b"progressive_jackpot"],
        pj,
    )
    .0
}

/// Derive every address without sending anything. Useful for negative tests
/// that must reference an account deliberately left uninitialised.
pub fn addresses(ctx: &MultiProgramTestContext) -> World {
    let treasury = ctx.network_config.treasury_operations_id;
    let security = ctx.network_config.security_governance_id;
    let jackpot = ctx.network_config.progressive_jackpot_id;
    let (main, incoming, outgoing) = treasury_pdas(&treasury);
    World {
        treasury,
        security,
        jackpot,
        treasury_main: main,
        treasury_incoming: incoming,
        treasury_outgoing: outgoing,
        reimbursement_queue: reimbursement_queue_pda(&treasury),
        security_program_state: program_state_pda(&security),
        blacklist_state: blacklist_state_pda(&security),
        emergency_state: emergency_state_pda(&security),
        jackpot_program_state: program_state_pda(&jackpot),
        jackpot_state: jackpot_state_pda(&jackpot),
        jackpot_vault: jackpot_vault_pda(&jackpot),
    }
}

// ---------------------------------------------------------------------------
// transaction plumbing
// ---------------------------------------------------------------------------

pub async fn process(
    ctx: &mut MultiProgramTestContext,
    ix: Instruction,
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let bh = ctx.recent_blockhash;
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    ctx.banks_client.process_transaction(tx).await
}

// ---------------------------------------------------------------------------
// instruction builders
// ---------------------------------------------------------------------------

fn init_treasury_ix(w: &World, payer: Pubkey) -> Instruction {
    Instruction {
        program_id: w.treasury,
        accounts: treasury_operations::accounts::InitializeTreasuryAccounts {
            treasury_main: w.treasury_main,
            treasury_incoming: w.treasury_incoming,
            treasury_outgoing: w.treasury_outgoing,
            // C10: a fourth account since 2026-07-28. Anchor's generated
            // struct is exhaustive, so every hand-written caller must supply
            // it — a TypeScript client will NOT be told, and will fail at
            // runtime with an account-count error that names nothing.
            reimbursement_queue: w.reimbursement_queue,
            payer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeTreasuryAccounts {}.data(),
    }
}

fn init_security_ix(w: &World, authority: Pubkey) -> Instruction {
    Instruction {
        program_id: w.security,
        accounts: security_governance::accounts::InitializeSecurityGovernance {
            program_state: w.security_program_state,
            authority,
            treasury_incoming: w.treasury_incoming,
            treasury_main: w.treasury_main,
            treasury_outgoing: w.treasury_outgoing,
            system_program: solana_sdk::system_program::ID,
            treasury_operations_program: w.treasury,
            security_governance_program: w.security,
            cpi_authority: cpi_authority_pda(&w.security),
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeSecurityGovernance {}.data(),
    }
}

fn init_jackpot_program_ix(w: &World, authority: Pubkey) -> Instruction {
    Instruction {
        program_id: w.jackpot,
        accounts: progressive_jackpot::accounts::InitializeProgressiveJackpot {
            program_state: w.jackpot_program_state,
            authority,
            treasury_incoming: w.treasury_incoming,
            treasury_main: w.treasury_main,
            treasury_outgoing: w.treasury_outgoing,
            cpi_authority: cpi_authority_pda(&w.jackpot),
            system_program: solana_sdk::system_program::ID,
            treasury_operations_program: w.treasury,
            progressive_jackpot_program: w.jackpot,
        }
        .to_account_metas(None),
        data: progressive_jackpot::instruction::InitializeProgressiveJackpot {}.data(),
    }
}

fn init_jackpot_ix(w: &World, authority: Pubkey, mint: Pubkey, initial: u64) -> Instruction {
    Instruction {
        program_id: w.jackpot,
        accounts: progressive_jackpot::accounts::InitializeJackpot {
            jackpot_state: w.jackpot_state,
            program_state: w.jackpot_program_state,
            authority,
            kots_mint: mint,
            token_program: spl_token::ID,
            jackpot_vault: w.jackpot_vault,
            treasury_incoming: w.treasury_incoming,
            treasury_main: w.treasury_main,
            treasury_outgoing: w.treasury_outgoing,
            cpi_authority: cpi_authority_pda(&w.jackpot),
            system_program: solana_sdk::system_program::ID,
            treasury_operations_program: w.treasury,
            progressive_jackpot_program: w.jackpot,
        }
        .to_account_metas(None),
        data: progressive_jackpot::instruction::InitializeJackpot {
            initial_amount: initial,
        }
        .data(),
    }
}

// ---------------------------------------------------------------------------
// the bootstrap
// ---------------------------------------------------------------------------

/// Stand up the accounts every value-moving test needs, and return their
/// addresses.
///
/// **Treasury first, always.** The security-governance and progressive-jackpot
/// initializers both CPI into treasury-operations to reimburse rent from
/// `treasury_incoming`, so those accounts must exist and be
/// treasury-operations-owned before either runs. A plain `solana transfer`
/// creates a *system-owned* account and the program cannot debit what it does
/// not own — producing `instruction spent from the balance of an account it does
/// not own`, which reads like a program bug and is a setup error. See §3.11.5.
///
/// Idempotent: an already-initialised account is not an error here. Tests may
/// call this once per context, and a test that needs a *fresh* uninitialised
/// account should use [`addresses`] instead and initialise nothing.
pub async fn world(ctx: &mut MultiProgramTestContext) -> World {
    let w = addresses(ctx);
    let payer = ctx.payer.pubkey();

    // Best-effort: the harness may already have stood these up. A second call
    // fails with "already in use", which is not a problem worth surfacing.
    let _ = process(ctx, init_treasury_ix(&w, payer)).await;
    let _ = process(ctx, init_security_ix(&w, payer)).await;
    let _ = process(ctx, init_jackpot_program_ix(&w, payer)).await;

    w
}

/// [`world`] plus an initialised jackpot state and funded vault.
///
/// Separate because it needs a mint, and because the two jackpot authority
/// tests are the ones this was written for: without `jackpot_state` existing
/// they die at 3012 and prove nothing about authority at all.
pub async fn world_with_jackpot(
    ctx: &mut MultiProgramTestContext,
    mint: Pubkey,
    initial_amount: u64,
) -> World {
    let w = world(ctx).await;
    process(ctx, init_jackpot_ix(&w, ctx.payer.pubkey(), mint, initial_amount))
        .await
        .expect(
            "initialize_jackpot must succeed — if this fails, every downstream \
             jackpot test is measuring account existence rather than authority",
        );
    w
}

// ---------------------------------------------------------------------------
// assertions
// ---------------------------------------------------------------------------

/// Assert a transaction was rejected **for the stated reason**.
///
/// This exists because `fn rejected(res) -> bool { res.is_err() }` is what made
/// four tests vacuous. `is_err()` cannot tell "the guard bit" from "the account
/// does not exist" from "the instruction does not compile into anything
/// reachable" — and on 2026-07-28 all four were the second.
///
/// Codes worth knowing:
///   3012  AccountNotInitialized — **almost never what a security test wants.**
///         It means the transaction died before your guard ran.
///   2003  ConstraintRaw          — a `constraint = …` bit
///   2001  ConstraintHasOne       — a `has_one` bit
///   6000+ program-defined errors
pub fn assert_rejected_with(
    res: std::result::Result<(), BanksClientError>,
    expected_code: u32,
) {
    use solana_sdk::instruction::InstructionError;
    use solana_sdk::transaction::TransactionError;

    let err = res.expect_err("expected this transaction to be REJECTED, but it succeeded");
    let code = match err {
        BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(c),
        )) => c,
        other => panic!("expected a custom program error, got: {other:?}"),
    };

    assert_ne!(
        code, 3012,
        "rejected with AccountNotInitialized (3012) — the transaction died at \
         account validation, BEFORE the guard under test could run. This is the \
         exact shape of the four vacuous passes found on 2026-07-28. Bootstrap \
         the account with bootstrap::world() and assert again."
    );
    assert_eq!(
        code, expected_code,
        "rejected, but for the wrong reason: got {code}, expected {expected_code}"
    );
}
