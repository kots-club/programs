// DESTINATION (where this file goes): kots-contracts/tests/security_regressions.rs
// DESTINATION (where this file goes): kots-contracts/tests/security_regressions.rs
//! Security regression suite — 2026-07-23.
//!
//! One test per finding from the authorization audit. Every test here asserts a
//! SECURITY PROPERTY, not a happy path: the question is always "is the bad thing
//! rejected", because that is the thing no test in this repo was checking when
//! the audit found four unauthenticated fund-movement paths.
//!
//! Written fresh against the current programs, using Anchor's generated
//! `accounts::` / `instruction::` types throughout. That is deliberate — the
//! quarantined `devnet_idl_tests` suite hand-built its account lists, and every
//! defect that hid in them (wrong count, wrong order, invented discriminators)
//! would have been a compile error here instead of five rounds of runtime
//! archaeology.
//!
//! Naming: `rejects_*` for negative controls, `allows_*` for the positive
//! counterpart that proves the control is not simply refusing everything — a
//! rejection test that passes because the instruction is broken is worthless, so
//! the pairs matter.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::{InstructionData, ToAccountMetas};
use anchor_lang::prelude::*;
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer as SdkSigner},
    transaction::Transaction,
};

use kots_common::state::CPI_AUTHORITY_SEED;

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// NOTE: `anchor_lang::prelude::*` re-exports `Result<T>` — a ONE-parameter alias
// that shadows `std::result::Result`. Both helpers below therefore spell out
// `std::result::Result` explicitly; writing plain `Result<T, E>` here is an
// E0107, and it is a trap worth leaving signposted for the next author.

/// Any failure at all — used where the exact code matters less than the fact
/// that the operation did not succeed.
///
/// `BanksClient::process_transaction` returns `Result<(), BanksClientError>`,
/// not `TransportError`; the latter is what the older harness code uses.
fn rejected<T, E>(res: &std::result::Result<T, E>) -> bool {
    res.is_err()
}

// ---------------------------------------------------------------------------
// FINDING #1 — reimburse_creator was an unauthenticated treasury drain.
//
// Before the fix: `authority` was a bare Signer bound to nothing, `recipient`
// was an unconstrained UncheckedAccount, and the body debited treasury_main
// straight into it. Any wallet could drain up to `queue.total_owed`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_reimburse_creator_from_a_wrong_signer() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();

    // An arbitrary funded wallet that is NOT the registered upgrade authority.
    let attacker = Keypair::new();
    let (treasury_main, _) = Pubkey::find_program_address(
        &[b"treasury_main_v4"],
        &ctx.network_config.treasury_operations_id,
    );
    let (queue, _) = Pubkey::find_program_address(
        &[b"reimbursement_queue_v4"],
        &ctx.network_config.treasury_operations_id,
    );
    let (registry, _) = Pubkey::find_program_address(
        &[kots_common::upgrade::UPGRADE_REGISTRY_SEED, b"treasury"],
        &ctx.network_config.treasury_operations_id,
    );

    let accounts = treasury_operations::accounts::ReimburseCreator {
        treasury_main,
        reimbursement_queue: queue,
        recipient: attacker.pubkey(),
        upgrade_registry: registry,
        authority: attacker.pubkey(),
        system_program: solana_sdk::system_program::id(),
    };
    let data = treasury_operations::instruction::ReimburseCreator {
        recipient: attacker.pubkey(),
        amount: 1,
    };
    let ix = Instruction {
        program_id: ctx.network_config.treasury_operations_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &attacker],
        ctx.recent_blockhash,
    );
    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(
        rejected(&res),
        "reimburse_creator accepted a signer that is not the upgrade authority — \
         this is the unauthenticated treasury drain the audit found (finding #1)"
    );
}

// ---------------------------------------------------------------------------
// FINDING C-2 — reimburse_payer identified its caller by an INSTRUCTION
// ARGUMENT. Program ids are public, so any wallet could name a whitelisted
// program and be paid ~0.157 SOL per call, in a loop.
//
// The fix requires a PDA of the calling program, signed via invoke_signed.
// A wallet cannot produce that signature, so the drain closes.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_reimburse_payer_without_the_caller_program_pda_signature() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();

    let attacker = Keypair::new();
    let (treasury_incoming, _) = Pubkey::find_program_address(
        &[b"treasury_incoming_v4"],
        &ctx.network_config.treasury_operations_id,
    );
    // Derive the CPI authority for a *whitelisted* program — the exact spoof the
    // old argument-based check permitted. The attacker can name it but cannot
    // sign for it.
    let (cpi_authority, _) = Pubkey::find_program_address(
        &[CPI_AUTHORITY_SEED],
        &ctx.network_config.contest_management_id,
    );

    let accounts = treasury_operations::accounts::ReimbursePayer {
        treasury_incoming,
        payer: attacker.pubkey(),
        cpi_authority,
        system_program: solana_sdk::system_program::id(),
    };
    let data = treasury_operations::instruction::ReimbursePayer {
        amount_lamports: 100_000,
        caller_program_id: ctx.network_config.contest_management_id,
    };
    let ix = Instruction {
        program_id: ctx.network_config.treasury_operations_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(
        rejected(&res),
        "reimburse_payer paid out without a signature from the calling program's \
         CPI authority PDA — the metered treasury drain (finding C-2) is open"
    );
}

// ---------------------------------------------------------------------------
// FINDING #3 — award_jackpot had no authority binding, and the `winner`
// argument was unrelated to the token account being paid: it logged one pubkey
// and paid whatever account the caller passed.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_award_jackpot_from_a_wrong_authority() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let attacker = Keypair::new();

    let (jackpot_state, _) = Pubkey::find_program_address(
        &[b"jackpot_state".as_ref(), b"v4".as_ref()],
        &ctx.network_config.progressive_jackpot_id,
    );
    let (program_state, _) = Pubkey::find_program_address(
        &[b"program_state"],
        &ctx.network_config.progressive_jackpot_id,
    );
    let (vault, _) = Pubkey::find_program_address(
        &[kots_common::state::VAULT_SEED, b"progressive_jackpot"],
        &ctx.network_config.progressive_jackpot_id,
    );

    let attacker_ata = ctx
        .create_token_account_for(attacker.pubkey(), 0)
        .await
        .unwrap();

    let accounts = progressive_jackpot::accounts::AwardJackpot {
        jackpot_state,
        kots_mint: ctx.kots_mint.pubkey(),
        token_program: spl_token::id(),
        winner_account: attacker_ata,
        jackpot_vault: vault,
        program_state,
        authority: attacker.pubkey(),
        emergency_state: Pubkey::find_program_address(
            &[b"emergency_state_v4", b"v4"],
            &security_governance::ID,
        )
        .0,
    };
    let data = progressive_jackpot::instruction::AwardJackpot {
        winner: attacker.pubkey(),
        contest_label: [0u8; 32],
    };
    let ix = Instruction {
        program_id: ctx.network_config.progressive_jackpot_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &attacker],
        ctx.recent_blockhash,
    );
    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(
        rejected(&res),
        "award_jackpot paid out to a caller who is not program_state.authority — \
         finding #3 is open and the entire pool is drainable"
    );
}

// ---------------------------------------------------------------------------
// FINDING H-1 — manage_progressive_jackpot was unbound, so any signer could
// inflate weekly_contributions / total_jackpot_amount and force a cycle
// rollover.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_jackpot_accounting_writes_from_a_wrong_authority() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let attacker = Keypair::new();

    let (jackpot_state, _) = Pubkey::find_program_address(
        &[b"jackpot_state".as_ref(), b"v4".as_ref()],
        &ctx.network_config.progressive_jackpot_id,
    );
    let (program_state, _) = Pubkey::find_program_address(
        &[b"program_state"],
        &ctx.network_config.progressive_jackpot_id,
    );

    let accounts = progressive_jackpot::accounts::ManageProgressiveJackpot {
        jackpot_state,
        program_state,
        authority: attacker.pubkey(),
    };
    let data = progressive_jackpot::instruction::ManageProgressiveJackpot {
        week_number: 1,
        contribution_amount: u64::MAX / 2,
    };
    let ix = Instruction {
        program_id: ctx.network_config.progressive_jackpot_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &attacker],
        ctx.recent_blockhash,
    );
    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(
        rejected(&res),
        "manage_progressive_jackpot accepted accounting writes from an arbitrary \
         signer — finding H-1 is open"
    );
}

// ---------------------------------------------------------------------------
// PROGRAM BUGFIX — a zero entry fee is legal. KOTS_TOKENOMICS §4b calls the fee
// OPTIONAL and the FAQ states entering contests is free, but
// validate_contest_creation_params required >= MIN_ENTRY_FEE, making every
// contest paid. This is the positive control: free contests must be creatable.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_a_free_contest_and_rejects_a_dust_fee() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();

    // string_to_bytes_array returns Result<[u8; 32], Error> — it validates length.
    let free = kots_common::utils::string_to_bytes_array("free_contest").unwrap();
    let ok = ctx.create_test_contest(free, 3600, 2, 0).await;
    assert!(
        ok.is_ok(),
        "a zero entry fee was rejected — contests are free to enter per \
         KOTS_TOKENOMICS §4b and the public FAQ"
    );

    // The anti-dust floor must still hold for contests that DO charge.
    let dust = kots_common::utils::string_to_bytes_array("dust_contest").unwrap();
    let bad = ctx.create_test_contest(dust, 3600, 2, 1).await;
    assert!(
        rejected(&bad),
        "a 1-lamport entry fee was accepted — MIN_ENTRY_FEE no longer guards \
         against dust fees"
    );
}

// ---------------------------------------------------------------------------
// FINDING H (2026-07-24) — CLOSED. `emergency_pause` / `emergency_resume` used
// to call a `validate_emergency_authority` whose allowlist was bound to an
// underscore-prefixed variable, whose `require!` was commented out with the note
// "For now allow any authority", and which then logged "✅ Emergency authority
// validated". Any wallet could halt the platform, and — worse — any wallet could
// RESUME it during a live incident.
//
// The rule is now asymmetric: bootstrap (no recorded authority yet) is gated
// like an initializer, and once recorded, only that authority may pause or
// resume.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_emergency_resume_from_an_arbitrary_wallet() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let attacker = Keypair::new();

    let (emergency_state, _) = Pubkey::find_program_address(
        &[kots_common::state::EMERGENCY_SEED, b"v4"],
        &ctx.network_config.security_governance_id,
    );

    // Resume is now execute-an-approved-decision. The attacker points at a
    // decision id that was never opened or approved, so nothing authorizes
    // lifting the pause — the resume must be rejected.
    let decision_id = 424_242u64;
    let (decision, _) = Pubkey::find_program_address(
        &[b"gov_decision_v1", &decision_id.to_le_bytes()],
        &ctx.network_config.security_governance_id,
    );
    let accounts = security_governance::accounts::EmergencyResume {
        emergency_state,
        emergency_authority: attacker.pubkey(),
        decision,
    };
    let data = security_governance::instruction::EmergencyResume { decision_id };
    let ix = Instruction {
        program_id: ctx.network_config.security_governance_id,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &attacker],
        ctx.recent_blockhash,
    );
    let res = ctx.banks_client.process_transaction(tx).await;

    assert!(
        rejected(&res),
        "an arbitrary wallet lifted the emergency pause without an approved decision — \
         the control an incident response depends on can be undone by the attacker it is aimed at"
    );
}
