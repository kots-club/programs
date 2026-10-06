// kots-contracts/tests/treasury_watcher_tests.rs
//! ACT-23 watcher liveness (dead-man's-switch) tests — the registry arming
//! ceremony, the signed heartbeat, and the settle gate the whole design
//! exists for: "sidecar dark AND watchers dark" must HALT the external rail.
//!
//! Same discipline as `treasury_timelock_tests` (whose pub(crate) harness this
//! reuses): `allows_*` proves the happy path, `rejects_*` proves each guard
//! bites, and the pairs matter. The pre-existing
//! `allows_settle_after_timelock_moves_tokens` over there is ALSO this
//! feature's third leg — it settles with the registry ABSENT, proving the gate
//! stays off until the arming ceremony (deploy-safe).
//!
//! Register this module in `tests/lib.rs`:  pub mod treasury_watcher_tests;

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer as SdkSigner},
};

use crate::treasury_timelock_tests::{
    init_window_ix, outflow_window_pda, queue_ix, rejected, send, settle_ix,
    setup_kots_account, setup_registry, token_balance, treasury_id,
    treasury_outflow_authority_pda, upgrade_registry_pda, watcher_registry_pda,
};
use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use treasury_operations::{
    WatcherRegistry, ATTESTATION_FRESHNESS_SECS, MAX_WATCHERS,
    TREASURY_TIMELOCK_DELAY_SECS,
};

// ---------------------------------------------------------------------------
// instruction builders
// ---------------------------------------------------------------------------

fn init_watcher_registry_ix(program_id: Pubkey, authority: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeWatcherRegistry {
            watcher_registry: watcher_registry_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeWatcherRegistry {}.data(),
    }
}

fn register_watcher_ix(program_id: Pubkey, authority: Pubkey, watcher: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::RegisterWatcher {
            watcher_registry: watcher_registry_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::RegisterWatcher { watcher }.data(),
    }
}

fn attest_ix(program_id: Pubkey, watcher: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::AttestLiveness {
            watcher_registry: watcher_registry_pda(&program_id),
            watcher,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::AttestLiveness {}.data(),
    }
}

async fn read_registry(ctx: &mut MultiProgramTestContext, program_id: &Pubkey) -> WatcherRegistry {
    let acct = ctx
        .banks_client
        .get_account(watcher_registry_pda(program_id))
        .await
        .unwrap()
        .expect("watcher registry should exist");
    WatcherRegistry::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

// ---------------------------------------------------------------------------
// registry ceremony + heartbeat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_init_register_and_attest_updates_timestamp() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();
    let fresh = read_registry(&mut ctx, &program_id).await;
    assert_eq!(fresh.watcher_count, 0, "registry arms empty; registering is a separate act");

    let watcher = Keypair::new();
    send(
        &mut ctx,
        &[register_watcher_ix(program_id, owner.pubkey(), watcher.pubkey())],
        &[&owner],
    )
    .await
    .unwrap();
    let one = read_registry(&mut ctx, &program_id).await;
    assert_eq!(one.watcher_count, 1);
    assert_eq!(one.watchers[0], watcher.pubkey());
    assert_eq!(one.last_attested[0], 0, "a registered watcher starts NEVER-attested");

    send(&mut ctx, &[attest_ix(program_id, watcher.pubkey())], &[&watcher])
        .await
        .unwrap();
    let beat = read_registry(&mut ctx, &program_id).await;
    assert!(beat.last_attested[0] > 0, "attestation must record the chain clock");
}

#[tokio::test]
async fn rejects_init_by_non_authority() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    let mallory = Keypair::new();
    // fund mallory so the refusal is the CONSTRAINT, not an empty fee-payer
    let fund = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &mallory.pubkey(),
        1_000_000_000,
    );
    send(&mut ctx, &[fund], &[]).await.unwrap();
    let res = send(
        &mut ctx,
        &[init_watcher_registry_ix(program_id, mallory.pubkey())],
        &[&mallory],
    )
    .await;
    assert!(rejected(&res), "only the multisig authority may arm the switch");
}

#[tokio::test]
async fn rejects_register_by_non_authority_and_duplicates_and_overflow() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();

    let mallory = Keypair::new();
    let w1 = Keypair::new().pubkey();
    let bad = send(&mut ctx, &[register_watcher_ix(program_id, mallory.pubkey(), w1)], &[&mallory])
        .await;
    assert!(rejected(&bad), "only the multisig authority may register watchers");

    send(&mut ctx, &[register_watcher_ix(program_id, owner.pubkey(), w1)], &[&owner])
        .await
        .unwrap();
    let dup = send(&mut ctx, &[register_watcher_ix(program_id, owner.pubkey(), w1)], &[&owner])
        .await;
    assert!(rejected(&dup), "a watcher key registers once");

    // fill to MAX_WATCHERS, then the next one must refuse
    for _ in 1..MAX_WATCHERS {
        let w = Keypair::new().pubkey();
        send(&mut ctx, &[register_watcher_ix(program_id, owner.pubkey(), w)], &[&owner])
            .await
            .unwrap();
    }
    let overflow = send(
        &mut ctx,
        &[register_watcher_ix(program_id, owner.pubkey(), Keypair::new().pubkey())],
        &[&owner],
    )
    .await;
    assert!(rejected(&overflow), "the registry holds MAX_WATCHERS keys");
}

#[tokio::test]
async fn rejects_attest_by_unregistered_signer() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;
    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();

    let stranger = Keypair::new();
    let res = send(&mut ctx, &[attest_ix(program_id, stranger.pubkey())], &[&stranger]).await;
    assert!(rejected(&res), "an unregistered signer cannot keep the rail alive");
}

// ---------------------------------------------------------------------------
// the gate itself — the reason all of this exists
// ---------------------------------------------------------------------------

/// Shared settle scaffolding: window + funded PDA-owned treasury + one queued
/// external transfer under `id`, timelock ALREADY warped past. Returns
/// (mint, treasury_ata, dest_ata).
async fn armed_settle_scaffold(
    ctx: &mut MultiProgramTestContext,
    owner: &Keypair,
    id: u64,
) -> (Pubkey, Pubkey, Pubkey) {
    let program_id = treasury_id(ctx);
    send(ctx, &[init_window_ix(program_id, owner.pubkey())], &[owner])
        .await
        .unwrap();
    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let mint = ctx.kots_mint.pubkey();
    let treasury_ata = setup_kots_account(ctx, &outflow_pda, 5_000).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(ctx, &recipient, 0).await;
    let payer = ctx.payer.pubkey();
    send(ctx, &[queue_ix(program_id, payer, id, dest_ata, 2_000, b"payout".to_vec())], &[])
        .await
        .unwrap();
    ctx.advance_clock(TREASURY_TIMELOCK_DELAY_SECS + 1).await.unwrap();
    // sanity: the outflow-window PDA exists (the ceiling leg is live)
    assert!(ctx
        .banks_client
        .get_account(outflow_window_pda(&program_id))
        .await
        .unwrap()
        .is_some());
    (mint, treasury_ata, dest_ata)
}

#[tokio::test]
async fn rejects_settle_when_armed_but_no_watcher_ever_attested() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    // Arm the switch and register a watcher that NEVER heartbeats.
    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();
    let silent = Keypair::new();
    send(
        &mut ctx,
        &[register_watcher_ix(program_id, owner.pubkey(), silent.pubkey())],
        &[&owner],
    )
    .await
    .unwrap();

    let (mint, treasury_ata, dest_ata) = armed_settle_scaffold(&mut ctx, &owner, 700).await;
    let payer = ctx.payer.pubkey();

    // Timelock elapsed, ceiling clear — the ONLY closed gate is liveness.
    let res = send(
        &mut ctx,
        &[settle_ix(program_id, payer, 700, mint, treasury_ata, dest_ata)],
        &[],
    )
    .await;
    assert!(
        rejected(&res),
        "armed + zero fresh attestations must HALT the external rail (dead-man's-switch)"
    );
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0, "nothing may move while halted");
}

#[tokio::test]
async fn allows_settle_when_a_watcher_attested_within_the_window() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();
    let watcher = Keypair::new();
    send(
        &mut ctx,
        &[register_watcher_ix(program_id, owner.pubkey(), watcher.pubkey())],
        &[&owner],
    )
    .await
    .unwrap();

    // Warp FIRST (past the timelock), THEN attest — the heartbeat is fresh at
    // settle time and ATTESTATION_FRESHNESS_SECS (12h) < the 24h warp, so
    // attesting before the warp would (correctly) leave the gate closed.
    let (mint, treasury_ata, dest_ata) = armed_settle_scaffold(&mut ctx, &owner, 701).await;
    send(&mut ctx, &[attest_ix(program_id, watcher.pubkey())], &[&watcher])
        .await
        .unwrap();
    let payer = ctx.payer.pubkey();

    send(
        &mut ctx,
        &[settle_ix(program_id, payer, 701, mint, treasury_ata, dest_ata)],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 2_000);
    assert_eq!(token_balance(&mut ctx, treasury_ata).await, 3_000);

    // Sanity on the freshness math the gate consumed.
    let reg = read_registry(&mut ctx, &program_id).await;
    assert!(reg.last_attested[0] > 0);
    assert!(ATTESTATION_FRESHNESS_SECS == 12 * 60 * 60);
}

#[tokio::test]
async fn rejects_settle_when_the_only_attestation_has_gone_stale() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    setup_registry(&mut ctx, &owner).await;

    send(&mut ctx, &[init_watcher_registry_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();
    let watcher = Keypair::new();
    send(
        &mut ctx,
        &[register_watcher_ix(program_id, owner.pubkey(), watcher.pubkey())],
        &[&owner],
    )
    .await
    .unwrap();

    // Heartbeat NOW — then the 24h timelock warp inside the scaffold pushes it
    // past the 12h freshness window: attested once, went dark, rail halts.
    send(&mut ctx, &[attest_ix(program_id, watcher.pubkey())], &[&watcher])
        .await
        .unwrap();
    let (mint, treasury_ata, dest_ata) = armed_settle_scaffold(&mut ctx, &owner, 702).await;
    let payer = ctx.payer.pubkey();

    let res = send(
        &mut ctx,
        &[settle_ix(program_id, payer, 702, mint, treasury_ata, dest_ata)],
        &[],
    )
    .await;
    assert!(rejected(&res), "a stale attestation must not keep the rail open");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0);
}
