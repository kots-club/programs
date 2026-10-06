// DESTINATION (where this file goes): kots-contracts/tests/treasury_conservation_tests.rs
// kots-contracts/tests/treasury_conservation_tests.rs
//! ACT-24 per-bucket value-conservation tests — the purpose-as-accounting-key
//! ledger and the hard-revert F15 asks for: the books cannot be over-drawn
//! even by a fully compromised distributor credential, and a contest-tagged
//! draw can never reach jackpot funds (nor vice versa) because the seeds bind
//! each ledger to its (tag, id).
//!
//! Same discipline as `treasury_timelock_tests` (whose pub(crate) harness this
//! reuses): `allows_*` proves the happy path, `rejects_*` proves each guard
//! bites, and the pairs matter.
//!
//! Register this module in `tests/lib.rs`:  pub mod treasury_conservation_tests;

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
use crate::treasury_timelock_tests::{
    rejected, send, setup_kots_account, setup_registry, token_balance, treasury_id,
    treasury_outflow_authority_pda, upgrade_registry_pda,
};
use kots_common::purpose_key::{TAG_CONTEST_AWARD, TAG_JACKPOT};
use treasury_operations::{
    BucketLedger, BUCKET_LEDGER_SEED, DISTRIBUTION_CONFIG_SEED,
};

// ---------------------------------------------------------------------------
// PDAs + instruction builders
// ---------------------------------------------------------------------------

fn distribution_config_pda(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[DISTRIBUTION_CONFIG_SEED], program_id).0
}

fn bucket_ledger_pda(program_id: &Pubkey, tag: u8, bucket_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[BUCKET_LEDGER_SEED, &tag.to_le_bytes(), &bucket_id.to_le_bytes()],
        program_id,
    )
    .0
}

fn init_config_ix(
    program_id: Pubkey,
    authority: Pubkey,
    distributor: Pubkey,
    jackpot_pool: Pubkey,
    bucket_credit_authority: Pubkey,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeDistributionConfig {
            distribution_config: distribution_config_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeDistributionConfig {
            distribution_authority: distributor,
            jackpot_pool,
            bucket_credit_authority,
        }
        .data(),
    }
}

fn init_bucket_ix(program_id: Pubkey, authority: Pubkey, tag: u8, bucket_id: u64) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::InitializeBucket {
            bucket_ledger: bucket_ledger_pda(&program_id, tag, bucket_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeBucket { tag, bucket_id }.data(),
    }
}

fn fund_ix(
    program_id: Pubkey,
    authority: Pubkey,
    tag: u8,
    bucket_id: u64,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::FundBucket {
            bucket_ledger: bucket_ledger_pda(&program_id, tag, bucket_id),
            distribution_config: distribution_config_pda(&program_id),
            upgrade_registry: upgrade_registry_pda(&program_id),
            authority,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::FundBucket {
            tag,
            bucket_id,
            additional_entitlement: amount,
        }
        .data(),
    }
}

#[allow(clippy::too_many_arguments)]
fn distribute_ix(
    program_id: Pubkey,
    distributor: Pubkey,
    tag: u8,
    bucket_id: u64,
    amount: u64,
    purpose: Vec<u8>,
    mint: Pubkey,
    treasury_ata: Pubkey,
    dest_ata: Pubkey,
    jackpot_pool: Pubkey,
) -> Instruction {
    Instruction {
        program_id,
        accounts: treasury_operations::accounts::DistributeFromTreasury {
            bucket_ledger: bucket_ledger_pda(&program_id, tag, bucket_id),
            distribution_config: distribution_config_pda(&program_id),
            treasury_outflow_authority: treasury_outflow_authority_pda(&program_id),
            kots_mint: mint,
            treasury_token_account: treasury_ata,
            destination_token_account: dest_ata,
            token_program: spl_token::id(),
            distributor,
            jackpot_pool,
            emergency_state: Pubkey::find_program_address(
                &[b"emergency_state_v4", b"v4"],
                &security_governance::ID,
            )
            .0,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::DistributeFromTreasury {
            tag,
            bucket_id,
            amount,
            purpose,
        }
        .data(),
    }
}

async fn read_ledger(
    ctx: &mut MultiProgramTestContext,
    program_id: &Pubkey,
    tag: u8,
    bucket_id: u64,
) -> BucketLedger {
    let acct = ctx
        .banks_client
        .get_account(bucket_ledger_pda(program_id, tag, bucket_id))
        .await
        .unwrap()
        .expect("bucket ledger should exist");
    BucketLedger::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

/// Shared scaffold: owner registry, distribution config with `distributor`,
/// a PDA-owned treasury ATA holding 10_000, and a fresh destination ATA.
/// Fabricate the progressive-jackpot pool STATE at `addr` with the given
/// live total, owned by the real progressive-jackpot program id — exactly
/// what the multisig pins at the ceremony. Layout per ProgressiveJackpotState:
/// disc[8] | is_initialized u8 | current_cycle_week u8 | cycle_start i64 |
/// total_jackpot_amount u64 LE | ...
fn set_jackpot_pool(ctx: &mut MultiProgramTestContext, addr: &Pubkey, total: u64) {
    let mut data = vec![0u8; 64];
    data[8] = 1; // is_initialized
    data[18..26].copy_from_slice(&total.to_le_bytes());
    let account = SdkAccount {
        lamports: 10_000_000,
        data,
        owner: ctx.network_config.progressive_jackpot_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.context.set_account(addr, &AccountSharedData::from(account));
}

async fn scaffold(
    ctx: &mut MultiProgramTestContext,
    owner: &Keypair,
    distributor: &Keypair,
    pool_total: u64,
) -> (Pubkey, Pubkey, Pubkey, Pubkey) {
    let program_id = treasury_id(ctx);
    setup_registry(ctx, owner).await;
    let jackpot_pool = Keypair::new().pubkey();
    set_jackpot_pool(ctx, &jackpot_pool, pool_total);
    send(
        ctx,
        &[init_config_ix(
            program_id,
            owner.pubkey(),
            distributor.pubkey(),
            jackpot_pool,
            Keypair::new().pubkey(), // credit authority unused on these paths
        )],
        &[owner],
    )
    .await
    .unwrap();
    let outflow_pda = treasury_outflow_authority_pda(&program_id);
    let mint = ctx.kots_mint.pubkey();
    let treasury_ata = setup_kots_account(ctx, &outflow_pda, 10_000).await;
    let recipient = Keypair::new().pubkey();
    let dest_ata = setup_kots_account(ctx, &recipient, 0).await;
    (mint, treasury_ata, dest_ata, jackpot_pool)
}

// ---------------------------------------------------------------------------
// the ledger + the hard revert
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_funded_distribution_and_hard_reverts_the_overdraw() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let distributor = Keypair::new();
    let (mint, treasury_ata, dest_ata, jackpot_pool) = scaffold(&mut ctx, &owner, &distributor, 8_000).await;

    send(&mut ctx, &[init_bucket_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7)], &[&owner])
        .await
        .unwrap();
    send(&mut ctx, &[fund_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7, 3_000)], &[&owner])
        .await
        .unwrap();

    // Two draws inside the budget move tokens and tally the ledger.
    for (amount, expect_dest) in [(2_000u64, 2_000u64), (1_000, 3_000)] {
        send(
            &mut ctx,
            &[distribute_ix(
                program_id, distributor.pubkey(), TAG_CONTEST_AWARD, 7, amount,
                b"contest-award:7".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
            )],
            &[&distributor],
        )
        .await
        .unwrap();
        assert_eq!(token_balance(&mut ctx, dest_ata).await, expect_dest);
    }
    let ledger = read_ledger(&mut ctx, &program_id, TAG_CONTEST_AWARD, 7).await;
    assert_eq!(ledger.distributed, 3_000);
    assert_eq!(ledger.entitled, 3_000);

    // One nano past the budget: HARD REVERT, balances untouched.
    let overdraw = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_CONTEST_AWARD, 7, 1,
            b"contest-award:7".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&overdraw), "Σ distributed must never exceed entitled");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 3_000);
    assert_eq!(token_balance(&mut ctx, treasury_ata).await, 7_000);
}

#[tokio::test]
async fn rejects_unstructured_or_mismatched_purpose() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let distributor = Keypair::new();
    let (mint, treasury_ata, dest_ata, jackpot_pool) = scaffold(&mut ctx, &owner, &distributor, 8_000).await;
    send(&mut ctx, &[init_bucket_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7)], &[&owner])
        .await
        .unwrap();
    send(&mut ctx, &[fund_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7, 5_000)], &[&owner])
        .await
        .unwrap();

    // Free text was fine on the external rail; on the DISTRIBUTION rail the
    // key is the accounting — unparseable purpose refuses.
    let freetext = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_CONTEST_AWARD, 7, 100,
            b"ops note, trust me".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&freetext), "unstructured purpose must refuse");

    // A parseable key that names a DIFFERENT bucket than the args refuses —
    // the human-readable string and the debited ledger can never disagree.
    let mismatch = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_CONTEST_AWARD, 7, 100,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&mismatch), "purpose key must match the targeted bucket");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0);
}

#[tokio::test]
async fn rejects_cross_bucket_reach_the_jackpot_canonical_case() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let distributor = Keypair::new();
    let (mint, treasury_ata, dest_ata, jackpot_pool) = scaffold(&mut ctx, &owner, &distributor, 8_000).await;

    // Jackpot bucket: rich. Contest bucket: nearly empty.
    for (tag, id, fund) in [(TAG_JACKPOT, 0u64, 8_000u64), (TAG_CONTEST_AWARD, 7, 100)] {
        send(&mut ctx, &[init_bucket_ix(program_id, owner.pubkey(), tag, id)], &[&owner])
            .await
            .unwrap();
        send(&mut ctx, &[fund_ix(program_id, owner.pubkey(), tag, id, fund)], &[&owner])
            .await
            .unwrap();
    }

    // A contest-tagged draw cannot borrow the jackpot's plenty: the seeds
    // bound it to ITS ledger, and its ledger hard-reverts.
    let reach = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_CONTEST_AWARD, 7, 5_000,
            b"contest-award:7".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&reach), "a contest draw must never reach jackpot funds");

    // The jackpot itself legitimately exceeds any contest's pool.
    send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_JACKPOT, 0, 5_000,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await
    .unwrap();
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 5_000);
}

#[tokio::test]
async fn rejects_jackpot_draw_beyond_the_live_pool_even_when_over_entitled() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let distributor = Keypair::new();
    // The LIVE pool holds 4_000...
    let (mint, treasury_ata, dest_ata, jackpot_pool) =
        scaffold(&mut ctx, &owner, &distributor, 4_000).await;
    // ...while the ledger is MISTAKENLY over-entitled to 8_000.
    send(&mut ctx, &[init_bucket_ix(program_id, owner.pubkey(), TAG_JACKPOT, 0)], &[&owner])
        .await
        .unwrap();
    send(&mut ctx, &[fund_ix(program_id, owner.pubkey(), TAG_JACKPOT, 0, 8_000)], &[&owner])
        .await
        .unwrap();

    // Entitled says yes; the pool says no — the pool wins.
    let beyond_pool = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_JACKPOT, 0, 5_000,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&beyond_pool), "a mistaken over-entitlement must not outrun the pool");

    // Exactly the pool: fine. One nano more, cumulatively: refused.
    send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_JACKPOT, 0, 4_000,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await
    .unwrap();
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 4_000);
    let one_more = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_JACKPOT, 0, 1,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&one_more), "the pool bound is CUMULATIVE, not per-draw");

    // And an unpinned pool account refuses every draw outright.
    let fake_pool = Keypair::new().pubkey();
    set_jackpot_pool(&mut ctx, &fake_pool, 1_000_000);
    let smuggled = send(
        &mut ctx,
        &[distribute_ix(
            program_id, distributor.pubkey(), TAG_JACKPOT, 0, 1,
            b"jackpot".to_vec(), mint, treasury_ata, dest_ata, fake_pool,
        )],
        &[&distributor],
    )
    .await;
    assert!(rejected(&smuggled), "only the multisig-pinned pool account is consulted");
}

#[tokio::test]
async fn rejects_unauthorized_distributor_and_admin_calls() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let program_id = treasury_id(&ctx);
    let owner = Keypair::new();
    let distributor = Keypair::new();
    let (mint, treasury_ata, dest_ata, jackpot_pool) = scaffold(&mut ctx, &owner, &distributor, 8_000).await;
    send(&mut ctx, &[init_bucket_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7)], &[&owner])
        .await
        .unwrap();
    send(&mut ctx, &[fund_ix(program_id, owner.pubkey(), TAG_CONTEST_AWARD, 7, 5_000)], &[&owner])
        .await
        .unwrap();

    let mallory = Keypair::new();

    // The distributor credential is checked...
    let wrong_signer = send(
        &mut ctx,
        &[distribute_ix(
            program_id, mallory.pubkey(), TAG_CONTEST_AWARD, 7, 100,
            b"contest-award:7".to_vec(), mint, treasury_ata, dest_ata, jackpot_pool,
        )],
        &[&mallory],
    )
    .await;
    assert!(rejected(&wrong_signer), "only the configured distributor may distribute");

    // ...and the admin surface stays multisig-only.
    let bucket_by_mallory =
        send(&mut ctx, &[init_bucket_ix(program_id, mallory.pubkey(), TAG_CONTEST_AWARD, 8)], &[&mallory]).await;
    assert!(rejected(&bucket_by_mallory), "only the multisig may create buckets");
    let fund_by_mallory =
        send(&mut ctx, &[fund_ix(program_id, mallory.pubkey(), TAG_CONTEST_AWARD, 7, 1)], &[&mallory]).await;
    assert!(rejected(&fund_by_mallory), "only the multisig may raise entitlements");
    assert_eq!(token_balance(&mut ctx, dest_ata).await, 0);
}
