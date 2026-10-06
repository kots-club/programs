// kots-contracts/tests/operational_tier_tests.rs
//! ACT-25 graduated operational-transfer tests — the governed escape hatch
//! (F16): the Routine tier's immediate-but-fenced path (quorum-set whitelist,
//! per-transfer cap, rolling-day aggregate) and the Exceptional tier's
//! size-graduated queue delay. Same discipline as the sibling suites:
//! `allows_*` proves the paths, `rejects_*` proves every fence, and the
//! pre-existing timelock suite silently serves as the config-ABSENT leg
//! (base delay when the tiers are unarmed — deploy-safe).
//!
//! Register this module in `tests/lib.rs`:  pub mod operational_tier_tests;

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer as SdkSigner},
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use crate::treasury_timelock_tests::{
    init_window_ix, queue_ix, queued_transfer_pda, rejected, send, settle_ix, setup_kots_account,
    setup_registry, tier_config_pda, token_balance, treasury_id, treasury_outflow_authority_pda,
    upgrade_registry_pda,
};
use treasury_operations::{
    OperationalTierConfig, QueuedTransfer, ROUTINE_DAY_SECS, TREASURY_TIMELOCK_DELAY_SECS,
};

const PER_TRANSFER: u64 = 1_000;
const DAILY: u64 = 1_000;
const EXCEPTIONAL_AT: u64 = 50_000;
const EXCEPTIONAL_DELAY: i64 = 72 * 60 * 60;

// ---------------------------------------------------------------------------
// builders
// ---------------------------------------------------------------------------

fn init_tiers_ix(program_id: Pubkey, authority: Pubkey, routine_authority: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeOperationalTierConfig {
            tier_config: tier_config_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeOperationalTierConfig {
            routine_authority,
            per_transfer_cap: PER_TRANSFER,
            daily_cap: DAILY,
            exceptional_threshold: EXCEPTIONAL_AT,
            exceptional_delay_secs: EXCEPTIONAL_DELAY,
        }
        .data(),
    }
}

fn mutate_accounts(
    program_id: Pubkey,
    authority: Pubkey,
) -> treasury_operations::accounts::MutateOperationalTierConfig {
    treasury_operations::accounts::MutateOperationalTierConfig {
        tier_config: tier_config_pda(&program_id),
        upgrade_registry: upgrade_registry_pda(&program_id),
        authority,
    }
}

fn add_dest_ix(program_id: Pubkey, authority: Pubkey, destination: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: mutate_accounts(program_id, authority).to_account_metas(None),
        data: treasury_operations::instruction::AddRoutineDestination { destination }.data(),
    }
}

fn remove_dest_ix(program_id: Pubkey, authority: Pubkey, destination: Pubkey) -> Instruction {
    Instruction {
        program_id,
        accounts: mutate_accounts(program_id, authority).to_account_metas(None),
        data: treasury_operations::instruction::RemoveRoutineDestination { destination }.data(),
    }
}

fn routine_ix(
    program_id: Pubkey,
    operator: Pubkey,
    amount: u64,
    mint: Pubkey,
    treasury_ata: Pubkey,
    dest_ata: Pubkey,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::RoutineOperationalTransfer {
            tier_config: tier_config_pda(&program_id),
            treasury_outflow_authority: treasury_outflow_authority_pda(&program_id),
            kots_mint: mint,
            treasury_token_account: treasury_ata,
            destination_token_account: dest_ata,
            token_program: spl_token::id(),
            operator,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::RoutineOperationalTransfer { amount }.data(),
    }
}

async fn read_tiers(ctx: &mut MultiProgramTestContext, program_id: &Pubkey) -> OperationalTierConfig {
    let acct = ctx
        .banks_client
        .get_account(tier_config_pda(program_id))
        .await
        .unwrap()
        .expect("tier config should exist");
    OperationalTierConfig::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

/// Shared scaffold: registry, tiers armed with `operator` as routine
/// authority, PDA-owned treasury holding 200_000, whitelisted dest ATA.
async fn scaffold(
    ctx: &mut MultiProgramTestContext,
    owner: &Keypair,
    operator: &Keypair,
) -> (Pubkey, Pubkey, Pubkey) {
    let program_id = treasury_id(ctx);
    setup_registry(ctx, owner).await;
    send(ctx, &[init_tiers_ix(program_id, owner.pubkey(), operator.pubkey())], &[owner])
        .await
        .unwrap();
    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let mint = ctx.kots_mint.pubkey();
    let treasury_ata = setup_kots_account(ctx, &outflow_pda, 200_000).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(ctx, &recipient, 0).await;
    send(ctx, &[add_dest_ix(program_id, owner.pubkey(), dest_ata)], &[owner])
        .await
        .unwrap();
    (mint, treasury_ata, dest_ata)
}

// ---------------------------------------------------------------------------
// the Routine tier — the immediate-but-fenced path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_routine_within_caps_and_resets_on_a_new_day() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let operator = Keypair::new();
    let (mint, treasury_ata, dest_ata) = scaffold(&mut ctx, &owner, &operator).await;

    // 600 then 400: exactly the daily cap, tokens move, window tallies.
    for (amount, expect_dest, expect_day) in [(600u64, 600u64, 600u64), (400, 1_000, 1_000)] {
        send(
            &mut ctx,
            &[routine_ix(program_id, operator.pubkey(), amount, mint, treasury_ata, dest_ata)],
            &[&operator],
        )
        .await
        .unwrap();
        assert_eq!(token_balance(&mut ctx, dest_ata).await, expect_dest);
        assert_eq!(read_tiers(&mut ctx, &program_id).await.day_total, expect_day);
    }

    // The day is spent: one more nano refuses.
    let over_day = send(
        &mut ctx,
        &[routine_ix(program_id, operator.pubkey(), 1, mint, treasury_ata, dest_ata)],
        &[&operator],
    )
    .await;
    assert!(rejected(&over_day), "the daily aggregate is a hard fence");

    // A new rolling day: the window resets and routine flows again.
    ctx.advance_clock(ROUTINE_DAY_SECS + 1).await.unwrap();
    send(
        &mut ctx,
        &[routine_ix(program_id, operator.pubkey(), 250, mint, treasury_ata, dest_ata)],
        &[&operator],
    )
    .await
    .unwrap();
    let tiers = read_tiers(&mut ctx, &program_id).await;
    assert_eq!(tiers.day_total, 250, "a fresh day starts a fresh tally");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 1_250);
}

#[tokio::test]
async fn rejects_routine_over_per_transfer_cap_even_on_a_fresh_day() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let operator = Keypair::new();
    let (mint, treasury_ata, dest_ata) = scaffold(&mut ctx, &owner, &operator).await;

    let res = send(
        &mut ctx,
        &[routine_ix(program_id, operator.pubkey(), PER_TRANSFER + 1, mint, treasury_ata, dest_ata)],
        &[&operator],
    )
    .await;
    assert!(rejected(&res), "per-transfer cap binds before the day is even touched");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0);
}

#[tokio::test]
async fn rejects_routine_to_unlisted_destination_and_by_non_operator() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let operator = Keypair::new();
    let (mint, treasury_ata, dest_ata) = scaffold(&mut ctx, &owner, &operator).await;

    // A perfectly valid amount to an UNLISTED account refuses.
    let stranger_ata = setup_kots_account(&mut ctx, &Keypair::new().pubkey(), 0).await;
    let unlisted = send(
        &mut ctx,
        &[routine_ix(program_id, operator.pubkey(), 100, mint, treasury_ata, stranger_ata)],
        &[&operator],
    )
    .await;
    assert!(rejected(&unlisted), "routine reaches ONLY the quorum-set whitelist");

    // The right destination, the wrong signer.
    let mallory = Keypair::new();
    let wrong_signer = send(
        &mut ctx,
        &[routine_ix(program_id, mallory.pubkey(), 100, mint, treasury_ata, dest_ata)],
        &[&mallory],
    )
    .await;
    assert!(rejected(&wrong_signer), "only the configured routine operator may call");

    // And after a quorum REMOVAL, yesterday's destination refuses too.
    send(&mut ctx, &[remove_dest_ix(program_id, owner.pubkey(), dest_ata)], &[&owner])
        .await
        .unwrap();
    let removed = send(
        &mut ctx,
        &[routine_ix(program_id, operator.pubkey(), 100, mint, treasury_ata, dest_ata)],
        &[&operator],
    )
    .await;
    assert!(rejected(&removed), "removal takes effect immediately");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0);
}

#[tokio::test]
async fn rejects_tier_admin_by_non_authority_and_duplicate_listing() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let operator = Keypair::new();
    let (_mint, _treasury_ata, dest_ata) = scaffold(&mut ctx, &owner, &operator).await;

    let mallory = Keypair::new();
    let bad_add = send(
        &mut ctx,
        &[add_dest_ix(program_id, mallory.pubkey(), Keypair::new().pubkey())],
        &[&mallory],
    )
    .await;
    assert!(rejected(&bad_add), "the whitelist is a quorum surface");

    let dup = send(&mut ctx, &[add_dest_ix(program_id, owner.pubkey(), dest_ata)], &[&owner]).await;
    assert!(rejected(&dup), "a destination lists once");
}

// ---------------------------------------------------------------------------
// the Exceptional tier — size-graduated delay on the queue
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queues_graduate_the_delay_at_the_exceptional_threshold() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let operator = Keypair::new();
    let (mint, treasury_ata, _dest) = scaffold(&mut ctx, &owner, &operator).await;
    send(&mut ctx, &[init_window_ix(program_id, owner.pubkey())], &[&owner])
        .await
        .unwrap();
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(&mut ctx, &recipient, 0).await;
    let payer = ctx.payer.pubkey();

    // Below the threshold: the base 24h delay, unchanged behavior.
    send(
        &mut ctx,
        &[queue_ix(program_id, payer, 900, dest_ata, EXCEPTIONAL_AT - 1, b"ops".to_vec())],
        &[],
    )
    .await
    .unwrap();
    // At the threshold: the LONGER window.
    send(
        &mut ctx,
        &[queue_ix(program_id, payer, 901, dest_ata, EXCEPTIONAL_AT, b"ops big".to_vec())],
        &[],
    )
    .await
    .unwrap();

    let read_delay = |data: Vec<u8>| {
        QueuedTransfer::try_deserialize(&mut data.as_slice()).unwrap().execution_delay
    };
    let small = ctx.banks_client.get_account(queued_transfer_pda(&program_id, 900)).await.unwrap().unwrap();
    let big = ctx.banks_client.get_account(queued_transfer_pda(&program_id, 901)).await.unwrap().unwrap();
    assert_eq!(read_delay(small.data), TREASURY_TIMELOCK_DELAY_SECS);
    assert_eq!(read_delay(big.data), EXCEPTIONAL_DELAY, "size graduates the window");

    // The graduated window is ENFORCED at settle: 24h+1 is not enough...
    ctx.advance_clock(TREASURY_TIMELOCK_DELAY_SECS + 1).await.unwrap();
    let early = send(
        &mut ctx,
        &[settle_ix(program_id, payer, 901, mint, treasury_ata, dest_ata)],
        &[],
    )
    .await;
    assert!(rejected(&early), "an exceptional transfer waits its full window");

    // ...72h is.
    ctx.advance_clock(EXCEPTIONAL_DELAY - TREASURY_TIMELOCK_DELAY_SECS).await.unwrap();
    send(
        &mut ctx,
        &[settle_ix(program_id, payer, 901, mint, treasury_ata, dest_ata)],
        &[],
    )
    .await
    .unwrap();
    assert_eq!(token_balance(&mut ctx, dest_ata).await, EXCEPTIONAL_AT);
}
