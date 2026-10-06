// kots-contracts/tests/voter_share_credit_tests.rs
//! ACT-24 auto-crediting tests (decision: CPI-coupled, voter-share leg) —
//! the cross-program act: `store_voter_share` writes the obligation AND
//! raises its treasury ceiling in one transaction, signed by the one PDA
//! only prize-distribution can produce. Paired discipline as everywhere:
//! `allows_*` proves the act, `rejects_*` proves both gates and the replay
//! bar.
//!
//! Register this module in `tests/lib.rs`:  pub mod voter_share_credit_tests;

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    account::{Account as SdkAccount, AccountSharedData},
    instruction::Instruction,
    signature::{Keypair, Signer as SdkSigner},
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use crate::treasury_timelock_tests::{rejected, send, setup_registry, treasury_id, upgrade_registry_pda};
use kots_common::purpose_key::TAG_CONTEST_VOTER_SHARE;
use prize_distribution::BUCKET_CREDIT_AUTHORITY_SEED;
use treasury_operations::{BucketLedger, DistributionConfig, BUCKET_LEDGER_SEED, DISTRIBUTION_CONFIG_SEED};

// ---------------------------------------------------------------------------
// PDAs + fabrication + builders
// ---------------------------------------------------------------------------

fn prize_id(ctx: &MultiProgramTestContext) -> Pubkey {
    ctx.network_config.prize_distribution_id
}

fn credit_authority_pda(prize: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[BUCKET_CREDIT_AUTHORITY_SEED], prize).0
}

fn program_state_pda(prize: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"program_state" as &[u8]], prize).0
}

fn voter_share_record_pda(prize: &Pubkey, label: &[u8; 32], voter: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"voter_share" as &[u8], label, voter.as_ref()], prize).0
}

fn bucket_pda(treasury: &Pubkey, contest_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            BUCKET_LEDGER_SEED,
            &[TAG_CONTEST_VOTER_SHARE],
            &contest_id.to_le_bytes(),
        ],
        treasury,
    )
    .0
}

fn config_pda(treasury: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[DISTRIBUTION_CONFIG_SEED], treasury).0
}

/// Fabricate prize-distribution's `program_state` with the given finalization
/// authority — the raw layout initialize writes: disc[8] | is_initialized u8 |
/// authority Pubkey @9..41 | ... (the gate reads exactly these bytes).
fn set_program_state(ctx: &mut MultiProgramTestContext, authority: &Pubkey) {
    let prize = prize_id(ctx);
    let mut data = vec![0u8; 64];
    data[8] = 1;
    data[9..41].copy_from_slice(authority.as_ref());
    let account = SdkAccount {
        lamports: 10_000_000,
        data,
        owner: prize,
        executable: false,
        rent_epoch: 0,
    };
    ctx.context
        .set_account(&program_state_pda(&prize), &AccountSharedData::from(account));
}

fn init_config_ix(
    treasury: Pubkey,
    authority: Pubkey,
    credit_authority: Pubkey,
) -> Instruction {
    Instruction {
        program_id: treasury,
        accounts: treasury_operations::accounts::InitializeDistributionConfig {
            distribution_config: config_pda(&treasury),
            upgrade_registry: upgrade_registry_pda(&treasury),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeDistributionConfig {
            distribution_authority: Keypair::new().pubkey(), // distributor unused here
            jackpot_pool: Keypair::new().pubkey(),           // pool unused here
            bucket_credit_authority: credit_authority,
        }
        .data(),
    }
}

#[allow(clippy::too_many_arguments)]
fn store_ix(
    prize: Pubkey,
    treasury: Pubkey,
    finalizer: Pubkey,
    label: [u8; 32],
    voter: Pubkey,
    amount: u64,
    contest_id: u64,
) -> Instruction {
    Instruction {
        program_id: prize,
        accounts: prize_distribution::accounts::StoreVoterShare {
            voter_share_record: voter_share_record_pda(&prize, &label, &voter),
            program_state: program_state_pda(&prize),
            authority: finalizer,
            system_program: solana_sdk::system_program::ID,
            treasury_program: treasury,
            voter_share_bucket: bucket_pda(&treasury, contest_id),
            treasury_distribution_config: config_pda(&treasury),
            bucket_credit_authority: credit_authority_pda(&prize),
        }
        .to_account_metas(None),
        data: prize_distribution::instruction::StoreVoterShare {
            contest_label: label,
            voter_wallet: voter,
            share_amount: amount,
            contest_id,
        }
        .data(),
    }
}

async fn read_bucket(ctx: &mut MultiProgramTestContext, treasury: &Pubkey, contest_id: u64) -> BucketLedger {
    let acct = ctx
        .banks_client
        .get_account(bucket_pda(treasury, contest_id))
        .await
        .unwrap()
        .expect("voter-share bucket should exist");
    BucketLedger::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

async fn read_config(ctx: &mut MultiProgramTestContext, treasury: &Pubkey) -> DistributionConfig {
    let acct = ctx
        .banks_client
        .get_account(config_pda(treasury))
        .await
        .unwrap()
        .expect("distribution config should exist");
    DistributionConfig::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

/// Shared scaffold: treasury registry + config (credit authority = the REAL
/// prize-distribution PDA), program_state under `finalizer`, finalizer funded
/// (it pays record + fresh-ledger rent through the CPI).
async fn scaffold(ctx: &mut MultiProgramTestContext, finalizer: &Keypair) -> (Pubkey, Pubkey) {
    let treasury = treasury_id(ctx);
    let prize = prize_id(ctx);
    let owner = Keypair::new();
    setup_registry(ctx, &owner).await;
    send(
        ctx,
        &[init_config_ix(treasury, owner.pubkey(), credit_authority_pda(&prize))],
        &[&owner],
    )
    .await
    .unwrap();
    set_program_state(ctx, &finalizer.pubkey());
    let fund = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &finalizer.pubkey(),
        2_000_000_000,
    );
    send(ctx, &[fund], &[]).await.unwrap();
    (treasury, prize)
}

fn label(name: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..name.len()].copy_from_slice(name.as_bytes());
    out
}

// ---------------------------------------------------------------------------
// the act, its accumulation, and the replay bar
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_store_to_credit_the_bucket_and_accumulate_per_record() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let finalizer = Keypair::new();
    let (treasury, prize) = scaffold(&mut ctx, &finalizer).await;
    let lbl = label("weekly-42");

    let voter_a = Keypair::new().pubkey();
    send(
        &mut ctx,
        &[store_ix(prize, treasury, finalizer.pubkey(), lbl, voter_a, 1_000, 42)],
        &[&finalizer],
    )
    .await
    .unwrap();

    let bucket = read_bucket(&mut ctx, &treasury, 42).await;
    assert_eq!(bucket.tag, TAG_CONTEST_VOTER_SHARE, "fresh ledger stamped by the credit");
    assert_eq!(bucket.bucket_id, 42);
    assert_eq!(bucket.entitled, 1_000, "record and ceiling are ONE act");
    assert_eq!(bucket.distributed, 0);

    // A second voter's record ACCUMULATES into the same contest bucket.
    let voter_b = Keypair::new().pubkey();
    send(
        &mut ctx,
        &[store_ix(prize, treasury, finalizer.pubkey(), lbl, voter_b, 500, 42)],
        &[&finalizer],
    )
    .await
    .unwrap();
    assert_eq!(read_bucket(&mut ctx, &treasury, 42).await.entitled, 1_500);
    assert_eq!(read_config(&mut ctx, &treasury).await.total_entitled, 1_500);
}

#[tokio::test]
async fn rejects_double_store_so_double_credit_is_impossible() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let finalizer = Keypair::new();
    let (treasury, prize) = scaffold(&mut ctx, &finalizer).await;
    let lbl = label("weekly-7");
    let voter = Keypair::new().pubkey();

    send(
        &mut ctx,
        &[store_ix(prize, treasury, finalizer.pubkey(), lbl, voter, 800, 7)],
        &[&finalizer],
    )
    .await
    .unwrap();
    let replay = send(
        &mut ctx,
        &[store_ix(prize, treasury, finalizer.pubkey(), lbl, voter, 800, 7)],
        &[&finalizer],
    )
    .await;
    assert!(rejected(&replay), "the record's init-once is the replay bar");
    assert_eq!(
        read_bucket(&mut ctx, &treasury, 7).await.entitled,
        800,
        "no record, no credit — atomically"
    );
}

// ---------------------------------------------------------------------------
// both gates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_store_by_anyone_but_the_finalization_authority() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let finalizer = Keypair::new();
    let (treasury, prize) = scaffold(&mut ctx, &finalizer).await;

    let mallory = Keypair::new();
    let fund = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &mallory.pubkey(),
        2_000_000_000,
    );
    send(&mut ctx, &[fund], &[]).await.unwrap();

    let res = send(
        &mut ctx,
        &[store_ix(prize, treasury, mallory.pubkey(), label("weekly-9"), Keypair::new().pubkey(), 999, 9)],
        &[&mallory],
    )
    .await;
    assert!(
        rejected(&res),
        "an open store would be an open entitlement keyhole — and a record-griefing vector besides"
    );
    assert!(
        ctx.banks_client
            .get_account(bucket_pda(&treasury, 9))
            .await
            .unwrap()
            .is_none(),
        "no gate pass, no bucket"
    );
}

#[tokio::test]
async fn rejects_direct_credit_without_the_program_pda_signature() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let finalizer = Keypair::new();
    let (treasury, _prize) = scaffold(&mut ctx, &finalizer).await;

    // Mallory calls the treasury credit instruction DIRECTLY, signing with her
    // own key as the "credit authority": the pinned-PDA constraint refuses —
    // only prize-distribution's invoke_signed can ever produce that signer.
    let mallory = Keypair::new();
    let fund = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &mallory.pubkey(),
        2_000_000_000,
    );
    send(&mut ctx, &[fund], &[]).await.unwrap();

    let ix = Instruction {
        program_id: treasury,
        accounts: treasury_operations::accounts::CreditVoterShareBucket {
            bucket_ledger: bucket_pda(&treasury, 11),
            distribution_config: config_pda(&treasury),
            bucket_credit_authority: mallory.pubkey(),
            payer: mallory.pubkey(),
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::CreditVoterShareBucket {
            contest_id: 11,
            amount: 1_000_000,
        }
        .data(),
    };
    let res = send(&mut ctx, &[ix], &[&mallory]).await;
    assert!(rejected(&res), "spenders and strangers never raise ceilings (F15)");
    assert!(
        ctx.banks_client
            .get_account(bucket_pda(&treasury, 11))
            .await
            .unwrap()
            .is_none()
    );
}
