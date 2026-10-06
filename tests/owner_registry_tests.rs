//! Owner-registry tests — piece #1 of on-chain governance.
//!
//! Same discipline as `security_regressions`: `allows_*` proves the happy path
//! works, `rejects_*` proves each guard actually bites, and the pairs matter —
//! a rejection that passes only because the instruction is broken is worthless.
//!
//! Register this module in `tests/lib.rs`:  pub mod owner_registry;
//! It requires the program to expose `pub mod owner_registry;` in its lib.rs so
//! `security_governance::owner_registry::{OwnerRegistry, MAX_OWNERS,
//! OWNER_REGISTRY_SEED}` resolve, plus the two thin `#[program]` wrappers
//! documented at the top of that module.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer as SdkSigner},
    transaction::Transaction,
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use security_governance::owner_registry::{
    BreakGlassControl, OwnerRegistry, BREAK_GLASS_CONTROL_SEED, BREAK_GLASS_ROTATION_TIMELOCK,
    MAX_OWNERS, OWNER_REGISTRY_SEED,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn registry_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[OWNER_REGISTRY_SEED], program_id).0
}

fn bg_control_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[BREAK_GLASS_CONTROL_SEED], program_id).0
}

/// Build the fixed `owners` array Anchor expects: active keys first, zero-padded.
fn owners_array(active: &[Pubkey]) -> [Pubkey; MAX_OWNERS] {
    let mut arr = [Pubkey::default(); MAX_OWNERS];
    for (i, k) in active.iter().enumerate() {
        arr[i] = *k;
    }
    arr
}

fn init_ix(
    program_id: Pubkey,
    initializer: Pubkey,
    owners: [Pubkey; MAX_OWNERS],
    owner_count: u8,
    threshold: u8,
    break_glass: Pubkey,
) -> Instruction {
    Instruction {
        program_id,
        accounts: security_governance::accounts::InitializeOwnerRegistry {
            owner_registry: registry_pda(&program_id),
            initializer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeOwnerRegistry {
            owners,
            owner_count,
            threshold,
            break_glass,
        }
        .data(),
    }
}

/// Build a rotate instruction. `co_signers` are appended as read-only signer
/// remaining_accounts (the owner co-signers for quorum).
fn rotate_ix(
    program_id: Pubkey,
    authority: Pubkey,
    old_owner: Pubkey,
    new_owner: Pubkey,
    co_signers: &[Pubkey],
) -> Instruction {
    let mut accounts = security_governance::accounts::RotateOwner {
        owner_registry: registry_pda(&program_id),
        authority,
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id,
        accounts,
        data: security_governance::instruction::RotateOwner {
            old_owner,
            new_owner,
        }
        .data(),
    }
}

// --- break-glass control instruction builders (timelocked rotation path) ----

/// propose_break_glass_rotation. `co_signers` are appended as read-only signer
/// remaining_accounts (the owner co-signer(s); break-glass never acts alone).
fn propose_ix(
    program_id: Pubkey,
    authority: Pubkey,
    old_owner: Pubkey,
    new_owner: Pubkey,
    co_signers: &[Pubkey],
) -> Instruction {
    let mut accounts = security_governance::accounts::ProposeBreakGlassRotation {
        owner_registry: registry_pda(&program_id),
        break_glass_control: bg_control_pda(&program_id),
        authority,
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id,
        accounts,
        data: security_governance::instruction::ProposeBreakGlassRotation {
            old_owner,
            new_owner,
        }
        .data(),
    }
}

/// execute_break_glass_rotation (anyone may execute a matured, non-vetoed proposal).
fn execute_ix(program_id: Pubkey, executor: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: security_governance::accounts::ExecuteBreakGlassRotation {
            owner_registry: registry_pda(&program_id),
            break_glass_control: bg_control_pda(&program_id),
            executor,
        }
        .to_account_metas(None),
        data: security_governance::instruction::ExecuteBreakGlassRotation {}.data(),
    }
}

/// report_unauthorized_break_glass (any single owner disables break-glass).
fn report_ix(program_id: Pubkey, authority: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: security_governance::accounts::ReportUnauthorizedBreakGlass {
            owner_registry: registry_pda(&program_id),
            break_glass_control: bg_control_pda(&program_id),
            authority,
        }
        .to_account_metas(None),
        data: security_governance::instruction::ReportUnauthorizedBreakGlass {}.data(),
    }
}

/// enable_break_glass (owner quorum re-enables). `co_signers` help reach the threshold.
fn enable_ix(program_id: Pubkey, authority: Pubkey, co_signers: &[Pubkey]) -> Instruction {
    let mut accounts = security_governance::accounts::EnableBreakGlass {
        owner_registry: registry_pda(&program_id),
        break_glass_control: bg_control_pda(&program_id),
        authority,
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id,
        accounts,
        data: security_governance::instruction::EnableBreakGlass {}.data(),
    }
}

async fn fetch_registry(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> OwnerRegistry {
    let acct = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("registry account should exist");
    OwnerRegistry::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

async fn fetch_bg_control(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> BreakGlassControl {
    let acct = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("break-glass control account should exist");
    BreakGlassControl::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

/// Initialize a standard 2-of-3 registry and return (pda, owner_a, owner_b,
/// owner_c, break_glass). Owner keypairs are returned so tests can sign with
/// them; they never need funding (they sign but are not fee payers).
async fn init_2of3(
    ctx: &mut MultiProgramTestContext,
) -> (Pubkey, Keypair, Keypair, Keypair, Keypair) {
    let program_id = ctx.network_config.security_governance_id;
    let pda = registry_pda(&program_id);
    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    let bg = Keypair::new();
    let owners = owners_array(&[a.pubkey(), b.pubkey(), c.pubkey()]);
    let ix = init_ix(program_id, ctx.payer.pubkey(), owners, 3, 2, bg.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    (pda, a, b, c, bg)
}

/// Create the break-glass control PDA (starts ENABLED, no pending). Must precede
/// any propose/execute/report/enable. The initializer is permissionless; the
/// test payer creates it.
async fn init_bg_control(ctx: &mut MultiProgramTestContext) {
    let program_id = ctx.network_config.security_governance_id;
    let ix = Instruction {
        program_id,
        accounts: security_governance::accounts::InitializeBreakGlassControl {
            break_glass_control: bg_control_pda(&program_id),
            initializer: ctx.payer.pubkey(),
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeBreakGlassControl {}.data(),
    };
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

fn rejected<T, E>(res: &std::result::Result<T, E>) -> bool {
    res.is_err()
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_initialize_two_of_three_with_break_glass() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (pda, a, b, c, bg) = init_2of3(&mut ctx).await;

    let reg = fetch_registry(&mut ctx, pda).await;
    assert_eq!(reg.owner_count, 3);
    assert_eq!(reg.threshold, 2);
    assert_eq!(reg.break_glass, bg.pubkey());
    assert!(reg.is_owner(&a.pubkey()));
    assert!(reg.is_owner(&b.pubkey()));
    assert!(reg.is_owner(&c.pubkey()));
    assert!(!reg.is_owner(&bg.pubkey()));
    assert!(reg.initialized);
}

#[tokio::test]
async fn rejects_initialize_with_threshold_above_owner_count() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = ctx.network_config.security_governance_id;

    let owners = owners_array(&[
        Keypair::new().pubkey(),
        Keypair::new().pubkey(),
        Keypair::new().pubkey(),
    ]);
    // threshold 4 > owner_count 3
    let ix = init_ix(program_id, ctx.payer.pubkey(), owners, 3, 4, Keypair::new().pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_initialize_with_duplicate_owners() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = ctx.network_config.security_governance_id;

    let dup = Keypair::new().pubkey();
    let owners = owners_array(&[dup, dup, Keypair::new().pubkey()]);
    let ix = init_ix(program_id, ctx.payer.pubkey(), owners, 3, 2, Keypair::new().pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_initialize_with_break_glass_as_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = ctx.network_config.security_governance_id;

    let a = Keypair::new().pubkey();
    let owners = owners_array(&[a, Keypair::new().pubkey(), Keypair::new().pubkey()]);
    // break_glass == owner a
    let ix = init_ix(program_id, ctx.payer.pubkey(), owners, 3, 2, a);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_double_initialize() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    // First init succeeds.
    let (_pda, _a, _b, _c, _bg) = init_2of3(&mut ctx).await;

    // Second init at the same PDA must fail (Anchor `init` on an existing
    // account). Vary break_glass so this is a genuinely different transaction,
    // not a signature-dedup false positive, and take a fresh blockhash.
    let program_id = ctx.network_config.security_governance_id;
    let owners = owners_array(&[
        Keypair::new().pubkey(),
        Keypair::new().pubkey(),
        Keypair::new().pubkey(),
    ]);
    let ix = init_ix(program_id, ctx.payer.pubkey(), owners, 3, 2, Keypair::new().pubkey());
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&ctx.payer.pubkey()), &[&ctx.payer], bh);
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

// ---------------------------------------------------------------------------
// rotate_owner
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_rotate_owner_with_two_owner_quorum() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (pda, a, b, c, _bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;

    // Replace c with d, authorized by a (authority) + b (co-signer) = 2 owners.
    let d = Keypair::new();
    let ix = rotate_ix(program_id, a.pubkey(), c.pubkey(), d.pubkey(), &[b.pubkey()]);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a, &b],
        ctx.recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let reg = fetch_registry(&mut ctx, pda).await;
    assert!(!reg.is_owner(&c.pubkey()), "old owner should be gone");
    assert!(reg.is_owner(&d.pubkey()), "new owner should be present");
    assert!(reg.is_owner(&a.pubkey()) && reg.is_owner(&b.pubkey()));
    assert_eq!(reg.owner_count, 3);
}

#[tokio::test]
async fn rejects_rotate_owner_with_single_owner_signature() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, _b, c, _bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;

    // Only a signs (no co-signer) — one owner, below the threshold of two.
    let d = Keypair::new();
    let ix = rotate_ix(program_id, a.pubkey(), c.pubkey(), d.pubkey(), &[]);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_rotate_owner_via_break_glass_alone() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, _a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;

    // A.1: break-glass is one of the 2-of-4 keys, not a unilateral actor. A lone
    // break-glass signature (no owner co-signer) must NOT rotate the owner set.
    let d = Keypair::new();
    let ix = rotate_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[]);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_rotate_owner_via_break_glass_even_with_one_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (pda, a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;

    // HARDENED 2026-08-15: rotate_owner is OWNER-QUORUM ONLY. A break-glass key —
    // even WITH an owner co-signer — can no longer rotate instantly here; it must
    // use the 24h-timelocked, owner-vetoable propose -> execute path. This is the
    // exact rejection (OwnerRegistryError::BreakGlassMustUseTimelock, error 18009)
    // that the removed `allows_rotate_owner_via_break_glass_with_one_owner`
    // wrongly expected to SUCCEED. The availability property it guarded — that
    // break-glass + one owner can still effect a rotation — now lives in
    // `allows_break_glass_rotation_via_timelock_path` below.
    let d = Keypair::new();
    let ix = rotate_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));

    // Non-vacuous: prove the refusal actually PREVENTED the rotation.
    let reg = fetch_registry(&mut ctx, pda).await;
    assert!(reg.is_owner(&c.pubkey()), "c must remain — the rotation was refused");
    assert!(!reg.is_owner(&d.pubkey()), "d must not have been added");
}

#[tokio::test]
async fn rejects_rotate_owner_by_non_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, _a, _b, c, _bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;

    // A funded stranger who is neither an owner nor break-glass, signing alone.
    let stranger = Keypair::new();
    let d = Keypair::new();
    let ix = rotate_ix(program_id, stranger.pubkey(), c.pubkey(), d.pubkey(), &[]);
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &stranger],
        ctx.recent_blockhash,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

// ---------------------------------------------------------------------------
// break-glass timelocked rotation  (propose -> 24h -> execute; veto; re-enable)
//
// HARDENED 2026-08-15. rotate_owner no longer accepts a break-glass signer; the
// recovery path is now propose_break_glass_rotation (break-glass + >=1 owner) ->
// wait BREAK_GLASS_ROTATION_TIMELOCK (24h) -> execute_break_glass_rotation
// (anyone). Any single owner can disable it (report_unauthorized_break_glass);
// only owner quorum can re-enable it (enable_break_glass). These prove the new
// recovery lever WORKS and that every guard bites — the prior suite covered none
// of it. Time is moved with MultiProgramTestContext::advance_clock (seconds).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_break_glass_rotation_via_timelock_path() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (pda, a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // Break-glass + one owner (a) propose replacing c with d.
    let d = Keypair::new();
    let ix = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The proposal is recorded but not yet executable.
    let control = fetch_bg_control(&mut ctx, bg_control_pda(&program_id)).await;
    assert!(control.has_pending(), "a rotation should be pending");
    assert_eq!(control.pending_new_owner, d.pubkey());

    // Move past the 24h timelock, then anyone (the payer) executes.
    ctx.advance_clock(BREAK_GLASS_ROTATION_TIMELOCK + 1).await.unwrap();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let exec = execute_ix(program_id, ctx.payer.pubkey());
    let tx =
        Transaction::new_signed_with_payer(&[exec], Some(&ctx.payer.pubkey()), &[&ctx.payer], bh);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The rotation landed and the pending slot was cleared.
    let reg = fetch_registry(&mut ctx, pda).await;
    assert!(!reg.is_owner(&c.pubkey()), "old owner gone");
    assert!(reg.is_owner(&d.pubkey()), "new owner present");
    assert!(reg.is_owner(&a.pubkey()));
    assert_eq!(reg.owner_count, 3);
    let control = fetch_bg_control(&mut ctx, bg_control_pda(&program_id)).await;
    assert!(!control.has_pending(), "pending cleared after execute");
}

#[tokio::test]
async fn rejects_execute_break_glass_rotation_before_timelock_elapses() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (pda, a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    let d = Keypair::new();
    let propose = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[propose],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Execute immediately — the 24h window has NOT elapsed
    // (OwnerRegistryError::TimelockNotElapsed, 18013).
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let exec = execute_ix(program_id, ctx.payer.pubkey());
    let tx =
        Transaction::new_signed_with_payer(&[exec], Some(&ctx.payer.pubkey()), &[&ctx.payer], bh);
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));

    // The owner set is untouched.
    let reg = fetch_registry(&mut ctx, pda).await;
    assert!(reg.is_owner(&c.pubkey()));
    assert!(!reg.is_owner(&d.pubkey()));
}

#[tokio::test]
async fn rejects_propose_break_glass_rotation_by_break_glass_alone() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, _a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // Break-glass with NO owner co-signer — refused
    // (OwnerRegistryError::BreakGlassNeedsOwnerCosigner, 18007).
    let d = Keypair::new();
    let ix = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg],
        bh,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_propose_break_glass_rotation_by_non_break_glass() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, b, c, _bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // An owner (a), even with another owner co-signer (b), is NOT the break-glass
    // key, so it may not use this path (OwnerRegistryError::NotBreakGlass, 18014).
    let d = Keypair::new();
    let ix = propose_ix(program_id, a.pubkey(), c.pubkey(), d.pubkey(), &[b.pubkey()]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a, &b],
        bh,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));
}

#[tokio::test]
async fn rejects_second_break_glass_rotation_while_one_is_pending() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // First proposal: replace c with d — accepted.
    let d = Keypair::new();
    let ix = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Second proposal while one is in flight — refused
    // (OwnerRegistryError::PendingRotationExists, 18011). Different target so this
    // is a genuinely distinct transaction, plus a fresh blockhash.
    let e = Keypair::new();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let ix2 = propose_ix(program_id, bg.pubkey(), c.pubkey(), e.pubkey(), &[a.pubkey()]);
    let tx2 = Transaction::new_signed_with_payer(
        &[ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx2).await));
}

#[tokio::test]
async fn allows_any_owner_to_disable_break_glass_and_void_a_pending_rotation() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, _b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // A rotation is pending…
    let d = Keypair::new();
    let ix = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // …any single owner (a) trips the kill-switch: disables + voids the pending.
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let report = report_ix(program_id, a.pubkey());
    let tx = Transaction::new_signed_with_payer(
        &[report],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let control = fetch_bg_control(&mut ctx, bg_control_pda(&program_id)).await;
    assert!(control.disabled, "break-glass must be disabled");
    assert!(!control.has_pending(), "the pending rotation must be voided");

    // While disabled, a fresh proposal is refused
    // (OwnerRegistryError::BreakGlassIsDisabled, 18010).
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let ix2 = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let tx2 = Transaction::new_signed_with_payer(
        &[ix2],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx2).await));
}

#[tokio::test]
async fn allows_owner_quorum_to_reenable_break_glass() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, b, c, bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // Disable first (any single owner).
    let report = report_ix(program_id, a.pubkey());
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[report],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Re-enable requires QUORUM: a (authority) + b (co-signer) = 2-of-3.
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let enable = enable_ix(program_id, a.pubkey(), &[b.pubkey()]);
    let tx = Transaction::new_signed_with_payer(
        &[enable],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a, &b],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let control = fetch_bg_control(&mut ctx, bg_control_pda(&program_id)).await;
    assert!(!control.disabled, "break-glass must be re-enabled");

    // And the path works again: break-glass + one owner can propose.
    let d = Keypair::new();
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let propose = propose_ix(program_id, bg.pubkey(), c.pubkey(), d.pubkey(), &[a.pubkey()]);
    let tx = Transaction::new_signed_with_payer(
        &[propose],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &bg, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

#[tokio::test]
async fn rejects_reenable_break_glass_with_single_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (_pda, a, _b, _c, _bg) = init_2of3(&mut ctx).await;
    let program_id = ctx.network_config.security_governance_id;
    init_bg_control(&mut ctx).await;

    // Disable first.
    let report = report_ix(program_id, a.pubkey());
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[report],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a],
        bh,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // A single owner cannot re-enable — quorum is required
    // (OwnerRegistryError::QuorumNotMet, 18006). "Easy to stop, hard to restart."
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let enable = enable_ix(program_id, a.pubkey(), &[]);
    let tx = Transaction::new_signed_with_payer(
        &[enable],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &a],
        bh,
    );
    assert!(rejected(&ctx.banks_client.process_transaction(tx).await));

    let control = fetch_bg_control(&mut ctx, bg_control_pda(&program_id)).await;
    assert!(control.disabled, "must remain disabled after a sub-quorum attempt");
}
