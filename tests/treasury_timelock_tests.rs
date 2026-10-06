// DESTINATION (where this file goes): kots-contracts/tests/treasury_timelock_tests.rs
// kots-contracts/tests/treasury_timelock_tests.rs
//! ACT-21 treasury timelock + veto tests — the queued-request "note" and the
//! owner veto/snooze/window-init machinery (Increment 1 + the setup half of
//! Increment 2).
//!
//! Same discipline as `owner_registry_tests`: `allows_*` proves the happy path
//! works, `rejects_*` proves each guard actually bites, and the pairs matter —
//! a rejection that passes only because the instruction is broken is worthless.
//!
//! Register this module in `tests/lib.rs`:  pub mod treasury_timelock_tests;
//!
//! SCOPE NOTE — `settle_external_transfer` is deliberately NOT tested here.
//! Anchor validates `SettleExternalTransfer`'s token accounts (the `token::mint`
//! / `token::authority` constraints on the treasury + destination token
//! accounts) BEFORE the handler runs, so even settle's reject paths require a
//! real KOTS mint + a PDA-owned treasury ATA + a destination ATA — the same
//! token custody the ACT-21 custody migration stands up. Settle's tests
//! therefore belong with that increment. Its post-timelock SUCCESS and the
//! aggregate-ceiling path additionally need a real clock warp, which this
//! harness lacks: `MultiProgramTestContext::advance_clock` mutates a local
//! `Clock` copy and never writes it back (it starts with `.start()`, holding no
//! `ProgramTestContext`), so on-chain time never moves. That no-op is used by
//! ~10 existing tests too and is worth fixing on its own.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer as SdkSigner},
    transaction::Transaction,
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use kots_common::upgrade::UPGRADE_REGISTRY_SEED;
use anchor_lang::solana_program::program_pack::Pack;
use treasury_operations::{
    OutflowWindow, QueuedTransfer, HOT_RAIL_CEILING, OUTFLOW_WINDOW_SEED, QUEUED_TRANSFER_SEED,
    OPERATIONAL_TIER_CONFIG_SEED, TREASURY_OUTFLOW_AUTHORITY_SEED, TREASURY_TIMELOCK_DELAY_SECS,
    WATCHER_REGISTRY_SEED,
};

// ---------------------------------------------------------------------------
// PDAs
// ---------------------------------------------------------------------------

pub(crate) fn queued_transfer_pda(program_id: &Pubkey, id: u64) -> Pubkey {
    Pubkey::find_program_address(&[QUEUED_TRANSFER_SEED, &id.to_le_bytes()], program_id).0
}

pub(crate) fn outflow_window_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[OUTFLOW_WINDOW_SEED], program_id).0
}

pub(crate) fn watcher_registry_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[WATCHER_REGISTRY_SEED], program_id).0
}

pub(crate) fn tier_config_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[OPERATIONAL_TIER_CONFIG_SEED], program_id).0
}

pub(crate) fn upgrade_registry_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[UPGRADE_REGISTRY_SEED, b"treasury"], program_id).0
}

// ---------------------------------------------------------------------------
// instruction builders
// ---------------------------------------------------------------------------

/// Initialize the treasury upgrade registry, recording `upgrade_authority` as
/// the owner authority our veto/snooze/init instructions gate on. In local mode
/// the front-run guard is open (realm = Dev), so `payer` may sign.
pub(crate) fn init_registry_ix(program_id: Pubkey, payer: Pubkey, upgrade_authority: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeUpgradeRegistry {
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority: payer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeUpgradeRegistry {
            initial_version: 1,
            upgrade_authority: Some(upgrade_authority),
            governance_approval_required: false,
        }
        .data(),
    }
}

pub(crate) fn queue_ix(
    program_id: Pubkey,
    requester: Pubkey,
    id: u64,
    destination: Pubkey,
    amount: u64,
    purpose: Vec<u8>,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::QueueExternalTransfer {
            queued_transfer: queued_transfer_pda(&program_id, id),
            requester,
            system_program: solana_sdk::system_program::ID,
            tier_config: tier_config_pda(&program_id),
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::QueueExternalTransfer {
            id,
            destination,
            amount,
            purpose,
        }
        .data(),
    }
}

pub(crate) fn veto_ix(program_id: Pubkey, authority: Pubkey, id: u64) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::VetoTransfer {
            queued_transfer: queued_transfer_pda(&program_id, id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::VetoTransfer { id }.data(),
    }
}

pub(crate) fn snooze_ix(
    program_id: Pubkey,
    authority: Pubkey,
    id: u64,
    additional_delay_secs: i64,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::SnoozeTransfer {
            queued_transfer: queued_transfer_pda(&program_id, id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::SnoozeTransfer {
            id,
            additional_delay_secs,
        }
        .data(),
    }
}

pub(crate) fn init_window_ix(program_id: Pubkey, authority: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeOutflowWindow {
            outflow_window: outflow_window_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeOutflowWindow {}.data(),
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

pub(crate) fn treasury_id(ctx: &MultiProgramTestContext) -> Pubkey {
    ctx.network_config.treasury_operations_id
}

pub(crate) fn rejected<T, E>(res: &std::result::Result<T, E>) -> bool {
    res.is_err()
}

/// Sign+send `ixs` with `payer` as fee-payer plus any extra `signers`.
pub(crate) async fn send(
    ctx: &mut MultiProgramTestContext,
    ixs: &[Instruction],
    signers: &[&Keypair],
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Refresh so each of a test's sequential transactions gets a distinct
    // blockhash — several tests chain queue → veto → snooze.
    ctx.refresh_blockhash().await?;
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(
        ixs,
        Some(&ctx.payer.pubkey()),
        &all,
        ctx.recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await?;
    Ok(())
}

/// Initialize the treasury upgrade registry with `authority` as the recorded
/// `upgrade_authority` (the owner authority veto/snooze/init gate on).
pub(crate) async fn setup_registry(ctx: &mut MultiProgramTestContext, authority: &Keypair) {
    let program_id = treasury_id(ctx);
    // Fund the owner so it can pay rent for accounts it authorizes as payer
    // (e.g. the outflow window, whose `InitializeOutflowWindow` uses
    // `payer = authority`). In production the multisig holds SOL; here we top it
    // up. Owner-gated instructions that create nothing (veto/snooze) don't need
    // this, but funding once here is harmless and keeps the helper uniform.
    let fund_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &authority.pubkey(),
        1_000_000_000, // 1 SOL — ample for account rent
    );
    send(ctx, &[fund_ix], &[]).await.expect("funding the owner should succeed");

    let ix = init_registry_ix(program_id, ctx.payer.pubkey(), authority.pubkey());
    send(ctx, &[ix], &[]).await.expect("registry init should succeed in local mode");
}

/// Queue a standard well-formed transfer under `id`, paid + requested by the
/// test payer. Returns the queued-transfer PDA.
pub(crate) async fn queue_standard(ctx: &mut MultiProgramTestContext, id: u64) -> Pubkey {
    let program_id = treasury_id(ctx);
    let destination = Keypair::new().pubkey();
    let ix = queue_ix(program_id, ctx.payer.pubkey(), id, destination, 500_000, b"payout".to_vec());
    send(ctx, &[ix], &[]).await.expect("queue should succeed");
    queued_transfer_pda(&program_id, id)
}

pub(crate) async fn fetch_queued(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> QueuedTransfer {
    let acct = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("queued transfer account should exist");
    QueuedTransfer::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

pub(crate) async fn fetch_window(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> OutflowWindow {
    let acct = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("outflow window account should exist");
    OutflowWindow::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

// ---------------------------------------------------------------------------
// queue
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn allows_queue_records_the_note_with_chain_clock_and_purpose() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);

    let destination = Keypair::new().pubkey();
    let ix = queue_ix(program_id, ctx.payer.pubkey(), 1, destination, 777_000, b"grant".to_vec());
    send(&mut ctx, &[ix], &[]).await.unwrap();

    let q = fetch_queued(&mut ctx, queued_transfer_pda(&program_id, 1)).await;
    assert_eq!(q.id, 1);
    assert_eq!(q.destination, destination);
    assert_eq!(q.amount, 777_000);
    assert_eq!(q.purpose_len, 5); // "grant"
    assert_eq!(&q.purpose[..5], b"grant");
    assert_eq!(q.execution_delay, TREASURY_TIMELOCK_DELAY_SECS);
    assert_eq!(q.requester, ctx.payer.pubkey());
    assert!(!q.vetoed);
    assert!(!q.settled);
    assert!(q.requested_at > 0, "requested_at is stamped from the chain Clock");
}

#[tokio::test]
pub(crate) async fn rejects_queue_with_empty_purpose() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let ix = queue_ix(program_id, ctx.payer.pubkey(), 2, Keypair::new().pubkey(), 1, vec![]);
    assert!(rejected(&send(&mut ctx, &[ix], &[]).await));
}

#[tokio::test]
pub(crate) async fn rejects_queue_with_oversized_purpose() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    // MAX_REASON_LENGTH is 64; 65 bytes must be refused.
    let ix = queue_ix(program_id, ctx.payer.pubkey(), 3, Keypair::new().pubkey(), 1, vec![7u8; 65]);
    assert!(rejected(&send(&mut ctx, &[ix], &[]).await));
}

// ---------------------------------------------------------------------------
// veto
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn allows_owner_veto_marks_the_request_vetoed() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    let pda = queue_standard(&mut ctx, 10).await;

    send(&mut ctx, &[veto_ix(program_id, owner.pubkey(), 10)], &[&owner])
        .await
        .unwrap();

    let q = fetch_queued(&mut ctx, pda).await;
    assert!(q.vetoed);
}

#[tokio::test]
pub(crate) async fn rejects_veto_from_non_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    let _ = queue_standard(&mut ctx, 11).await;

    // A stranger (not the recorded upgrade authority) signs the veto → refused.
    let stranger = Keypair::new();
    let res = send(&mut ctx, &[veto_ix(program_id, stranger.pubkey(), 11)], &[&stranger]).await;
    assert!(rejected(&res));
}

// ---------------------------------------------------------------------------
// snooze
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn allows_owner_snooze_extends_the_window() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    let pda = queue_standard(&mut ctx, 20).await;

    send(&mut ctx, &[snooze_ix(program_id, owner.pubkey(), 20, 3_600)], &[&owner])
        .await
        .unwrap();

    let q = fetch_queued(&mut ctx, pda).await;
    assert_eq!(q.execution_delay, TREASURY_TIMELOCK_DELAY_SECS + 3_600);
}

#[tokio::test]
pub(crate) async fn rejects_snooze_from_non_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    let _ = queue_standard(&mut ctx, 21).await;

    let stranger = Keypair::new();
    let res = send(&mut ctx, &[snooze_ix(program_id, stranger.pubkey(), 21, 3_600)], &[&stranger]).await;
    assert!(rejected(&res));
}

#[tokio::test]
pub(crate) async fn rejects_snooze_of_a_vetoed_request() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    let _ = queue_standard(&mut ctx, 22).await;

    // Veto first, then a snooze of the same request must be refused.
    send(&mut ctx, &[veto_ix(program_id, owner.pubkey(), 22)], &[&owner])
        .await
        .unwrap();
    let res = send(&mut ctx, &[snooze_ix(program_id, owner.pubkey(), 22, 3_600)], &[&owner]).await;
    assert!(rejected(&res));
}

// ---------------------------------------------------------------------------
// initialize_outflow_window
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn allows_owner_to_initialize_the_outflow_window() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    send(&mut ctx, &[init_window_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();

    let w = fetch_window(&mut ctx, outflow_window_pda(&program_id)).await;
    assert_eq!(w.cumulative_out, 0);
    assert!(w.window_start > 0, "window_start is stamped from the chain Clock");
}

#[tokio::test]
pub(crate) async fn rejects_outflow_window_init_from_non_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    let stranger = Keypair::new();
    let res = send(&mut ctx, &[init_window_ix(program_id, stranger.pubkey())], &[&stranger]).await;
    assert!(rejected(&res));
}

// ---------------------------------------------------------------------------
// F18 — advance_clock actually moves the on-chain clock
// ---------------------------------------------------------------------------

#[tokio::test]
pub(crate) async fn advance_clock_moves_the_on_chain_clock() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let payer = ctx.payer.pubkey();

    // `queue` stamps `requested_at` from the chain Clock — a convenient probe
    // that needs no custody setup.
    let d1 = Keypair::new().pubkey();
    send(&mut ctx, &[queue_ix(program_id, payer, 100, d1, 1, b"a".to_vec())], &[])
        .await
        .unwrap();
    let t1 = fetch_queued(&mut ctx, queued_transfer_pda(&program_id, 100)).await.requested_at;

    // Jump 100_000s and probe again. Before the F18 fix this delta was ~0.
    ctx.advance_clock(100_000).await.unwrap();

    let d2 = Keypair::new().pubkey();
    send(&mut ctx, &[queue_ix(program_id, payer, 101, d2, 1, b"b".to_vec())], &[])
        .await
        .unwrap();
    let t2 = fetch_queued(&mut ctx, queued_transfer_pda(&program_id, 101)).await.requested_at;

    assert!(
        t2 - t1 >= 90_000,
        "advance_clock did not move the chain clock (F18): t1={t1} t2={t2} delta={}",
        t2 - t1
    );
}

// ---------------------------------------------------------------------------
// settle_external_transfer — the custody path (ACT-21 / F4 / F17)
//
// The harness already stands up a classic-SPL KOTS mint (`ctx.kots_mint`, 9
// decimals, `ctx.payer` as mint authority). A classic mint has no transfer hook,
// so settle's Token-2022 `transfer_checked` path exercises cleanly here and the
// hook stays a devnet-only question. The treasury source account is the ATA of
// the `treasury_outflow_v4` PDA, so its token-account authority IS the PDA —
// which is exactly the custody the production `set_authority` cutover installs.
// ---------------------------------------------------------------------------

pub(crate) fn treasury_outflow_authority_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[TREASURY_OUTFLOW_AUTHORITY_SEED], program_id).0
}

pub(crate) fn settle_ix(
    program_id: Pubkey,
    caller: Pubkey,
    id: u64,
    mint: Pubkey,
    treasury_ata: Pubkey,
    dest_ata: Pubkey,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::SettleExternalTransfer {
            queued_transfer: queued_transfer_pda(&program_id, id),
            outflow_window: outflow_window_pda(&program_id),
            treasury_outflow_authority: treasury_outflow_authority_pda(&program_id),
            kots_mint: mint,
            treasury_token_account: treasury_ata,
            destination_token_account: dest_ata,
            token_program: spl_token::id(),
            caller,
            watcher_registry: watcher_registry_pda(&program_id),
            emergency_state: Pubkey::find_program_address(
                &[b"emergency_state_v4", b"v4"],
                &security_governance::ID,
            )
            .0,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::SettleExternalTransfer { id }.data(),
    }
}

/// Create the associated KOTS account for `owner` and optionally mint `amount`
/// into it. Returns the ATA address (its token-account authority is `owner`).
pub(crate) async fn setup_kots_account(
    ctx: &mut MultiProgramTestContext,
    owner: &Pubkey,
    amount: u64,
) -> Pubkey {
    let payer = ctx.payer.pubkey();
    let mint = ctx.kots_mint.pubkey();
    let ata = spl_associated_token_account::get_associated_token_address(owner, &mint);
    let create_ix = spl_associated_token_account::instruction::create_associated_token_account(
        &payer,
        owner,
        &mint,
        &spl_token::id(),
    );
    send(ctx, &[create_ix], &[]).await.expect("create ATA");
    if amount > 0 {
        let mint_ix =
            spl_token::instruction::mint_to(&spl_token::id(), &mint, &ata, &payer, &[], amount)
                .unwrap();
        send(ctx, &[mint_ix], &[]).await.expect("mint to ATA");
    }
    ata
}

pub(crate) async fn token_balance(ctx: &mut MultiProgramTestContext, account: Pubkey) -> u64 {
    let acct = ctx
        .banks_client
        .get_account(account)
        .await
        .unwrap()
        .expect("token account should exist");
    spl_token::state::Account::unpack(&acct.data).unwrap().amount
}

#[tokio::test]
pub(crate) async fn allows_settle_after_timelock_moves_tokens() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    send(&mut ctx, &[init_window_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();

    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let mint = ctx.kots_mint.pubkey();
    let treasury_ata = setup_kots_account(&mut ctx, &outflow_pda, 5_000).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(&mut ctx, &recipient, 0).await;
    let payer = ctx.payer.pubkey();

    send(&mut ctx, &[queue_ix(program_id, payer, 200, dest_ata, 2_000, b"payout".to_vec())], &[])
        .await
        .unwrap();

    // Before the timelock elapses, settle must refuse.
    let early = send(&mut ctx, &[settle_ix(program_id, payer, 200, mint, treasury_ata, dest_ata)], &[]).await;
    assert!(rejected(&early), "settle must refuse before the timelock elapses");

    // Warp past the 24h timelock; settle now succeeds and moves the tokens.
    ctx.advance_clock(TREASURY_TIMELOCK_DELAY_SECS + 1).await.unwrap();
    send(&mut ctx, &[settle_ix(program_id, payer, 200, mint, treasury_ata, dest_ata)], &[])
        .await
        .unwrap();

    assert_eq!(token_balance(&mut ctx, dest_ata).await, 2_000);
    assert_eq!(token_balance(&mut ctx, treasury_ata).await, 3_000);

    // The record is settled; a second settle refuses.
    let again = send(&mut ctx, &[settle_ix(program_id, payer, 200, mint, treasury_ata, dest_ata)], &[]).await;
    assert!(rejected(&again), "a settled transfer cannot settle twice");
}

#[tokio::test]
pub(crate) async fn rejects_settle_when_aggregate_ceiling_exceeded() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    send(&mut ctx, &[init_window_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();

    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let mint = ctx.kots_mint.pubkey();
    let treasury_ata = setup_kots_account(&mut ctx, &outflow_pda, HOT_RAIL_CEILING.saturating_mul(2)).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(&mut ctx, &recipient, 0).await;
    let payer = ctx.payer.pubkey();

    // A sits exactly at the cap; B adds just 1 more — the aggregate must trip.
    send(&mut ctx, &[queue_ix(program_id, payer, 300, dest_ata, HOT_RAIL_CEILING, b"A".to_vec())], &[])
        .await
        .unwrap();
    send(&mut ctx, &[queue_ix(program_id, payer, 301, dest_ata, 1, b"B".to_vec())], &[])
        .await
        .unwrap();

    ctx.advance_clock(TREASURY_TIMELOCK_DELAY_SECS + 1).await.unwrap();

    // A settles (cumulative == cap, within the same window); B trips the ceiling.
    send(&mut ctx, &[settle_ix(program_id, payer, 300, mint, treasury_ata, dest_ata)], &[])
        .await
        .unwrap();
    let b = send(&mut ctx, &[settle_ix(program_id, payer, 301, mint, treasury_ata, dest_ata)], &[]).await;
    assert!(rejected(&b), "aggregate ceiling must reject a transfer that pushes cumulative outflow over the cap");

    // A moved the full cap; B moved nothing.
    assert_eq!(token_balance(&mut ctx, dest_ata).await, HOT_RAIL_CEILING);
}

#[tokio::test]
pub(crate) async fn treasury_custody_rejects_a_raw_transfer_not_signed_by_the_pda() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let treasury_ata = setup_kots_account(&mut ctx, &outflow_pda, 10_000).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(&mut ctx, &recipient, 0).await;

    // The treasury account's authority is the outflow PDA, which no external key
    // can sign for. A raw SPL transfer therefore fails — only `settle`, signing
    // as the PDA through the program, can move these funds (closes F4's graph).
    let attacker = Keypair::new();
    let raw = spl_token::instruction::transfer(
        &spl_token::id(),
        &treasury_ata,
        &dest_ata,
        &attacker.pubkey(),
        &[],
        100,
    )
    .unwrap();
    let res = send(&mut ctx, &[raw], &[&attacker]).await;
    assert!(rejected(&res), "a raw transfer from the PDA-owned treasury account must fail");
    assert_eq!(token_balance(&mut ctx, treasury_ata).await, 10_000);
}
