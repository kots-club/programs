// DESTINATION (where this file goes): kots-contracts/tests/treasury_movement.rs
// TRACK A — second migrated-rail proof: treasury-operations.
//
// lp_movement proved the PDA-authority hooked transfer on the SIMPLEST rail (one
// bounty leg, no accounting). This proves it on treasury's `distribute_from_treasury`,
// which is the same PDA-authority hooked transfer PLUS the money-path machinery lp's
// leg never touches: a per-bucket conservation ledger, a coarse treasury-level belt,
// and — on the jackpot tag specifically — the "a draw may not exceed the live pool"
// bound (`Σ jackpot distributed ≤ total_jackpot_amount`). We test the JACKPOT tag so
// that bound is exercised, with a negative case proving it actually reverts.
//
// Three cases:
//   1. CONSERVATION NEGATIVE — a draw of `pool_total + 1` (within the bucket's
//      entitlement, over the pool) must REVERT at the pool bound before any transfer.
//   2. HOOK NEGATIVE — a valid draw with NO forwarded hook accounts must REVERT (the
//      hooked mint refuses a bare transfer_checked).
//   3. POSITIVE — a valid draw WITH the hook accounts moves KOTS out of the
//      treasury-outflow-PDA-owned vault, through the hook, into the destination, and
//      advances both the bucket ledger and the coarse belt by exactly the amount.
//
// UNVERIFIED IN-SANDBOX (no cargo/anchor). Gate: operator's `anchor build` +
// `cargo test -p kots-comprehensive-tests treasury_movement`. The hook bring-up is the
// proven money_movement/lp_movement harness; the treasury-specific state (bucket
// ledger, distribution config, and the manually-built jackpot pool) is new and is the
// most likely spot to need a fix-up round.

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    account::Account as SolAccount,
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer as SdkSigner},
    system_instruction,
    transaction::Transaction,
};

use kots_common::constants::TOKENS_PER_UNIT;
use kots_common::network_config::{Network, SystemWallets};
use transfer_hook as hook;
use treasury_operations as tops;

use spl_token_2022::extension::{
    transfer_fee, transfer_hook as t22_hook_ext, BaseStateWithExtensions, ExtensionType,
    StateWithExtensions,
};

// Must match treasury-operations' constants.
const BUCKET_LEDGER_SEED: &[u8] = b"bucket_ledger_v1";
const DISTRIBUTION_CONFIG_SEED: &[u8] = b"distribution_config_v1";
const TREASURY_OUTFLOW_AUTHORITY_SEED: &[u8] = b"treasury_outflow_v4";
const TAG_JACKPOT: u8 = 2; // kots_common::purpose_key::TAG_JACKPOT

// ---------------------------------------------------------------------------
// Refund-queue layout guards (identical to money_movement / lp_movement).
// ---------------------------------------------------------------------------
fn refund_queue_size() -> usize {
    8 + std::mem::size_of::<hook::PendingRefundQueue>()
}
const QUEUE_COUNT_OFFSET: usize = 8 + 32 + 2 + 2;
const QUEUE_ENTRIES_OFFSET: usize = 8 + 32 + 2 + 2 + 2 + 2 + 8;
const QUEUE_ENTRY_STRIDE: usize = 48;

fn read_queue_count(data: &[u8]) -> u16 {
    u16::from_le_bytes(data[QUEUE_COUNT_OFFSET..QUEUE_COUNT_OFFSET + 2].try_into().unwrap())
}
fn read_queue_entry(data: &[u8], i: usize) -> (Pubkey, u64) {
    let off = QUEUE_ENTRIES_OFFSET + i * QUEUE_ENTRY_STRIDE;
    let recipient = Pubkey::new_from_array(data[off..off + 32].try_into().unwrap());
    let amount = u64::from_le_bytes(data[off + 32..off + 40].try_into().unwrap());
    (recipient, amount)
}

// ---------------------------------------------------------------------------
// Small helpers (identical to money_movement / lp_movement).
// ---------------------------------------------------------------------------
async fn send(
    banks: &mut BanksClient,
    payer: &Keypair,
    extra_signers: &[&Keypair],
    ixs: &[Instruction],
    bh: &mut solana_sdk::hash::Hash,
) -> std::result::Result<(), BanksClientError> {
    *bh = banks.get_new_latest_blockhash(bh).await?;
    let mut signers: Vec<&Keypair> = vec![payer];
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, *bh);
    banks.process_transaction(tx).await
}

async fn token_amount(banks: &mut BanksClient, addr: Pubkey) -> u64 {
    let acct = banks.get_account(addr).await.unwrap().unwrap();
    StateWithExtensions::<spl_token_2022::state::Account>::unpack(&acct.data)
        .unwrap()
        .base
        .amount
}

async fn withheld_amount(banks: &mut BanksClient, addr: Pubkey) -> u64 {
    let acct = banks.get_account(addr).await.unwrap().unwrap();
    let state = StateWithExtensions::<spl_token_2022::state::Account>::unpack(&acct.data).unwrap();
    let ext = state
        .get_extension::<transfer_fee::TransferFeeAmount>()
        .unwrap();
    u64::from(ext.withheld_amount)
}

// ---------------------------------------------------------------------------
// THE TEST
// ---------------------------------------------------------------------------
#[tokio::test]
async fn treasury_jackpot_draw_moves_through_hook_and_respects_the_pool_bound() {
    assert_eq!(
        std::mem::size_of::<hook::PendingRefund>(),
        QUEUE_ENTRY_STRIDE,
        "PendingRefund layout changed — update the queue offsets in this file"
    );

    // -----------------------------------------------------------------------
    // Universe: treasury-operations (under test) + transfer-hook. Token-2022 +
    // ATA are program-test builtins. progressive-jackpot is NOT invoked — its
    // pool state is supplied as a data account and read by raw offset.
    // -----------------------------------------------------------------------
    let mut pt = ProgramTest::default();
    pt.add_program("treasury_operations", tops::ID, None);
    pt.add_program("transfer_hook", hook::ID, None);

    // PDAs the instruction derives.
    let (outflow_authority, _outflow_bump) =
        Pubkey::find_program_address(&[TREASURY_OUTFLOW_AUTHORITY_SEED], &tops::ID);
    let (distribution_config_pda, dc_bump) =
        Pubkey::find_program_address(&[DISTRIBUTION_CONFIG_SEED], &tops::ID);
    // The jackpot bucket is tag 2 / bucket 0 (the id-less "jackpot" purpose key).
    let bucket_id: u64 = 0;
    let (bucket_ledger_pda, bl_bump) = Pubkey::find_program_address(
        &[BUCKET_LEDGER_SEED, TAG_JACKPOT.to_le_bytes().as_ref(), bucket_id.to_le_bytes().as_ref()],
        &tops::ID,
    );

    // Cast: the distributor credential (a keypair we control — it must equal
    // distribution_config.authority and sign), the destination owner (a payout
    // recipient), and the pinned jackpot-pool state account (an arbitrary key we
    // control, recorded in the config and built by hand below).
    let distributor = Keypair::new();
    let payout_dest_owner = Pubkey::new_unique();
    let jackpot_pool = Keypair::new();

    // Budgets. entitled/total_entitled are large; the POOL bound is what we're
    // testing, so pool_total sits well below entitlement, and the negative draw
    // is pool_total+1 (inside the bucket, over the pool).
    let entitled: u64 = 10_000_000 * TOKENS_PER_UNIT;
    let total_entitled: u64 = 10_000_000 * TOKENS_PER_UNIT;
    let pool_total: u64 = 500_000 * TOKENS_PER_UNIT;
    let minted: u64 = 2_000_000 * TOKENS_PER_UNIT;
    let draw: u64 = 123_456_789_012; // awkward, << pool_total; exercises floor in the fee
    let over_pool_draw: u64 = pool_total + 1; // within entitlement, over the pool bound

    // Pre-create the three state accounts (before start): their fields depend only
    // on values known up front. distribution_config + bucket_ledger are real Anchor
    // accounts (serialized with their discriminators); the jackpot pool is read by
    // raw offset, so it is hand-built with is_initialized=1 @8 and
    // total_jackpot_amount @18..26 (the handler checks nothing else, not even disc).
    let dc = tops::DistributionConfig {
        authority: distributor.pubkey(),
        total_entitled,
        total_distributed: 0,
        jackpot_pool: jackpot_pool.pubkey(),
        bucket_credit_authority: Pubkey::new_unique(), // unused on this path
        bump: dc_bump,
    };
    let mut dc_data = Vec::new();
    dc.try_serialize(&mut dc_data).expect("serialize DistributionConfig");
    pt.add_account(
        distribution_config_pda,
        SolAccount { lamports: 10_000_000, data: dc_data, owner: tops::ID, executable: false, rent_epoch: 0 },
    );

    let bl = tops::BucketLedger {
        tag: TAG_JACKPOT,
        bucket_id,
        entitled,
        distributed: 0,
        bump: bl_bump,
    };
    let mut bl_data = Vec::new();
    bl.try_serialize(&mut bl_data).expect("serialize BucketLedger");
    pt.add_account(
        bucket_ledger_pda,
        SolAccount { lamports: 10_000_000, data: bl_data, owner: tops::ID, executable: false, rent_epoch: 0 },
    );

    let mut jackpot_data = vec![0u8; 64]; // >= 26; handler reads bytes 8 and 18..26 only
    jackpot_data[8] = 1; // is_initialized
    jackpot_data[18..26].copy_from_slice(&pool_total.to_le_bytes()); // total_jackpot_amount (LE)
    pt.add_account(
        jackpot_pool.pubkey(),
        SolAccount {
            lamports: 10_000_000,
            data: jackpot_data,
            owner: tops::ID, // owner is not checked by the handler; kept in-program for the test
            executable: false,
            rent_epoch: 0,
        },
    );

    let (mut banks, payer, mut bh) = pt.start().await;

    // -----------------------------------------------------------------------
    // Mint + hook bring-up — the proven money_movement/lp_movement harness.
    // Whitelist BOTH the outflow authority (source) and the payout destination
    // owner, so this exempt/refund posture matches the proven tests.
    // -----------------------------------------------------------------------
    let mint = Keypair::new();
    let (fee_authority_pda, _) = Pubkey::find_program_address(&[b"fee_authority"], &hook::ID);
    let decimals: u8 = 9;
    let fee_bps: u16 = 300;

    let mint_space = ExtensionType::try_calculate_account_len::<spl_token_2022::state::Mint>(&[
        ExtensionType::TransferFeeConfig,
        ExtensionType::TransferHook,
    ])
    .unwrap();
    let rent = banks.get_rent().await.unwrap();
    send(
        &mut banks,
        &payer,
        &[&mint],
        &[
            system_instruction::create_account(
                &payer.pubkey(),
                &mint.pubkey(),
                rent.minimum_balance(mint_space),
                mint_space as u64,
                &spl_token_2022::id(),
            ),
            transfer_fee::instruction::initialize_transfer_fee_config(
                &spl_token_2022::id(),
                &mint.pubkey(),
                Some(&payer.pubkey()),
                Some(&fee_authority_pda),
                fee_bps,
                u64::MAX,
            )
            .unwrap(),
            t22_hook_ext::instruction::initialize(
                &spl_token_2022::id(),
                &mint.pubkey(),
                Some(payer.pubkey()),
                Some(hook::ID),
            )
            .unwrap(),
            spl_token_2022::instruction::initialize_mint2(
                &spl_token_2022::id(),
                &mint.pubkey(),
                &payer.pubkey(),
                None,
                decimals,
            )
            .unwrap(),
        ],
        &mut bh,
    )
    .await
    .expect("mint bring-up failed");

    let (fee_config_pda, _) = Pubkey::find_program_address(&[b"fee_config"], &hook::ID);
    let (whitelist_pda, _) = Pubkey::find_program_address(&[b"whitelist"], &hook::ID);
    let (pool_registry_pda, _) = Pubkey::find_program_address(&[b"pool_registry"], &hook::ID);
    let (extra_metas_pda, _) =
        Pubkey::find_program_address(&[b"extra-account-metas", mint.pubkey().as_ref()], &hook::ID);
    let system = solana_sdk::system_program::id();

    send(
        &mut banks,
        &payer,
        &[],
        &[
            Instruction {
                program_id: hook::ID,
                accounts: hook::accounts::InitializeFeeConfig {
                    authority: payer.pubkey(),
                    fee_config: fee_config_pda,
                    system_program: system,
                }
                .to_account_metas(None),
                data: hook::instruction::InitializeFeeConfig { network: Network::Devnet }.data(),
            },
            Instruction {
                program_id: hook::ID,
                accounts: hook::accounts::InitializeFeeAuthority {
                    authority: payer.pubkey(),
                    fee_authority: fee_authority_pda,
                    system_program: system,
                }
                .to_account_metas(None),
                data: hook::instruction::InitializeFeeAuthority {}.data(),
            },
            Instruction {
                program_id: hook::ID,
                accounts: hook::accounts::InitializeWhitelist {
                    authority: payer.pubkey(),
                    whitelist_registry: whitelist_pda,
                    system_program: system,
                }
                .to_account_metas(None),
                data: hook::instruction::InitializeWhitelist {
                    wallets: vec![outflow_authority, payout_dest_owner],
                }
                .data(),
            },
            Instruction {
                program_id: hook::ID,
                accounts: hook::accounts::InitializePoolRegistry {
                    authority: payer.pubkey(),
                    pool_registry: pool_registry_pda,
                    system_program: system,
                }
                .to_account_metas(None),
                data: hook::instruction::InitializePoolRegistry {}.data(),
            },
        ],
        &mut bh,
    )
    .await
    .expect("hook config bring-up failed");

    let refund_queue = Keypair::new();
    let queue_size = refund_queue_size();
    send(
        &mut banks,
        &payer,
        &[&refund_queue],
        &[
            system_instruction::create_account(
                &payer.pubkey(),
                &refund_queue.pubkey(),
                rent.minimum_balance(queue_size),
                queue_size as u64,
                &hook::ID,
            ),
            Instruction {
                program_id: hook::ID,
                accounts: hook::accounts::InitializeRefundQueue {
                    authority: payer.pubkey(),
                    refund_queue: refund_queue.pubkey(),
                    fee_config: fee_config_pda,
                    system_program: system,
                }
                .to_account_metas(None),
                data: hook::instruction::InitializeRefundQueue {}.data(),
            },
        ],
        &mut bh,
    )
    .await
    .expect("refund queue bring-up failed");

    send(
        &mut banks,
        &payer,
        &[],
        &[Instruction {
            program_id: hook::ID,
            accounts: hook::accounts::InitializeExtraAccountMeta {
                payer: payer.pubkey(),
                extra_account_meta_list: extra_metas_pda,
                mint: mint.pubkey(),
                fee_config: fee_config_pda,
                system_program: system,
            }
            .to_account_metas(None),
            data: hook::instruction::InitializeExtraAccountMetaList {}.data(),
        }],
        &mut bh,
    )
    .await
    .expect("extra-account-meta list bring-up failed");

    // -----------------------------------------------------------------------
    // Two token accounts: the treasury source owned by the OUTFLOW PDA (funded)
    // and the payout destination owned by payout_dest_owner.
    // -----------------------------------------------------------------------
    let acct_space = ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[
        ExtensionType::TransferFeeAmount,
        ExtensionType::TransferHookAccount,
    ])
    .unwrap();
    let acct_rent = rent.minimum_balance(acct_space);

    let treasury_token = Keypair::new();
    let dest_token = Keypair::new();

    {
        let mut ixs: Vec<Instruction> = Vec::new();
        for (acct, owner) in [
            (&treasury_token, outflow_authority),
            (&dest_token, payout_dest_owner),
        ] {
            ixs.push(system_instruction::create_account(
                &payer.pubkey(),
                &acct.pubkey(),
                acct_rent,
                acct_space as u64,
                &spl_token_2022::id(),
            ));
            ixs.push(
                spl_token_2022::instruction::initialize_account3(
                    &spl_token_2022::id(),
                    &acct.pubkey(),
                    &mint.pubkey(),
                    &owner,
                )
                .unwrap(),
            );
        }
        ixs.push(
            spl_token_2022::instruction::mint_to(
                &spl_token_2022::id(),
                &mint.pubkey(),
                &treasury_token.pubkey(),
                &payer.pubkey(),
                &[],
                minted,
            )
            .unwrap(),
        );
        send(&mut banks, &payer, &[&treasury_token, &dest_token], &ixs, &mut bh)
            .await
            .expect("token account bring-up failed");
    }

    // The hook's full account set. sell_fee_debt is keyed by the SOURCE authority —
    // here the treasury-outflow PDA.
    let w = SystemWallets::for_network(Network::Devnet);
    let (sell_fee_debt_pda, _) =
        Pubkey::find_program_address(&[b"sell_fee_debt", outflow_authority.as_ref()], &hook::ID);
    let ata_program_id: Pubkey =
        std::str::FromStr::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();

    let hook_metas = vec![
        AccountMeta::new_readonly(hook::ID, false),
        AccountMeta::new_readonly(extra_metas_pda, false),
        AccountMeta::new_readonly(solana_sdk::sysvar::instructions::id(), false),
        AccountMeta::new_readonly(whitelist_pda, false),
        AccountMeta::new_readonly(pool_registry_pda, false),
        AccountMeta::new_readonly(fee_config_pda, false),
        AccountMeta::new_readonly(fee_authority_pda, false),
        AccountMeta::new(w.jackpot, false),
        AccountMeta::new(w.incoming, false),
        AccountMeta::new(w.admin, false),
        AccountMeta::new(w.treasury, false),
        AccountMeta::new(w.dev_fund, false),
        AccountMeta::new_readonly(system, false),
        AccountMeta::new_readonly(spl_token_2022::id(), false),
        AccountMeta::new_readonly(ata_program_id, false),
        AccountMeta::new(refund_queue.pubkey(), false),
        AccountMeta::new(sell_fee_debt_pda, false),
    ];

    // Base account metas for distribute_from_treasury (order = the accounts struct).
    let base_accounts = tops::accounts::DistributeFromTreasury {
        bucket_ledger: bucket_ledger_pda,
        distribution_config: distribution_config_pda,
        treasury_outflow_authority: outflow_authority,
        kots_mint: mint.pubkey(),
        treasury_token_account: treasury_token.pubkey(),
        destination_token_account: dest_token.pubkey(),
        token_program: spl_token_2022::id(),
        distributor: distributor.pubkey(),
        jackpot_pool: jackpot_pool.pubkey(),
        emergency_state: Pubkey::find_program_address(
            &[b"emergency_state_v4", b"v4"],
            &security_governance::ID,
        )
        .0,
    }
    .to_account_metas(None);

    let jackpot_purpose = b"jackpot".to_vec(); // PurposeKey::Jackpot -> tag 2, bucket 0

    // -----------------------------------------------------------------------
    // 1) CONSERVATION NEGATIVE: a draw over the live pool must revert at the
    //    pool bound. over_pool_draw is inside the bucket's entitlement, so the
    //    ONLY failing check is `Σ jackpot distributed ≤ total_jackpot_amount`.
    //    Sent WITH the hook accounts, so nothing but the bound can be at fault.
    // -----------------------------------------------------------------------
    let mut over_ix = Instruction {
        program_id: tops::ID,
        accounts: base_accounts.clone(),
        data: tops::instruction::DistributeFromTreasury {
            tag: TAG_JACKPOT,
            bucket_id,
            amount: over_pool_draw,
            purpose: jackpot_purpose.clone(),
        }
        .data(),
    };
    over_ix.accounts.extend(hook_metas.clone());
    let over = send(&mut banks, &payer, &[&distributor], &[over_ix], &mut bh).await;
    assert!(
        over.is_err(),
        "a jackpot draw exceeding the pool's total_jackpot_amount was allowed — conservation bound is not enforced"
    );

    // -----------------------------------------------------------------------
    // 2) HOOK NEGATIVE: a VALID draw with NO hook accounts must revert (hooked
    //    mint refuses a bare transfer_checked). Any ledger mutation is rolled
    //    back with the failed tx, so state is untouched for case 3.
    // -----------------------------------------------------------------------
    let no_metas_ix = Instruction {
        program_id: tops::ID,
        accounts: base_accounts.clone(),
        data: tops::instruction::DistributeFromTreasury {
            tag: TAG_JACKPOT,
            bucket_id,
            amount: draw,
            purpose: jackpot_purpose.clone(),
        }
        .data(),
    };
    let neg = send(&mut banks, &payer, &[&distributor], &[no_metas_ix], &mut bh).await;
    assert!(
        neg.is_err(),
        "hooked-mint transfer succeeded WITHOUT the hook accounts — mint is not hooked"
    );

    // -----------------------------------------------------------------------
    // 3) POSITIVE: valid draw WITH the hook accounts. The outflow PDA signs the
    //    transfer via seeds (inside the program).
    // -----------------------------------------------------------------------
    let mut ok_ix = Instruction {
        program_id: tops::ID,
        accounts: base_accounts,
        data: tops::instruction::DistributeFromTreasury {
            tag: TAG_JACKPOT,
            bucket_id,
            amount: draw,
            purpose: jackpot_purpose,
        }
        .data(),
    };
    ok_ix.accounts.extend(hook_metas);
    send(&mut banks, &payer, &[&distributor], &[ok_ix], &mut bh)
        .await
        .expect("distribute_from_treasury (jackpot) failed");

    // -----------------------------------------------------------------------
    // Assertions: money moved through the hook, and BOTH ledgers advanced by the
    // exact amount (the accounting lp's bounty leg never exercised).
    // -----------------------------------------------------------------------
    let fee = hook::withheld_fee_for(draw, fee_bps);

    assert_eq!(
        token_amount(&mut banks, treasury_token.pubkey()).await,
        minted - draw,
        "treasury source did not drop by the draw amount"
    );
    let dest_avail = token_amount(&mut banks, dest_token.pubkey()).await;
    let dest_withheld = withheld_amount(&mut banks, dest_token.pubkey()).await;
    assert_eq!(
        dest_avail + dest_withheld,
        draw,
        "destination did not receive the full draw through the hook"
    );
    assert_eq!(dest_withheld, fee, "withheld fee on the destination is wrong");

    // Bucket ledger advanced by exactly the draw (conservation bookkeeping).
    let bl_acct = banks.get_account(bucket_ledger_pda).await.unwrap().unwrap();
    let bl_after = tops::BucketLedger::try_deserialize(&mut bl_acct.data.as_slice()).unwrap();
    assert_eq!(bl_after.distributed, draw, "bucket ledger `distributed` did not advance by the draw");
    assert_eq!(bl_after.entitled, entitled, "bucket `entitled` must not change on a draw");

    // Coarse treasury belt advanced by exactly the draw.
    let dc_acct = banks.get_account(distribution_config_pda).await.unwrap().unwrap();
    let dc_after = tops::DistributionConfig::try_deserialize(&mut dc_acct.data.as_slice()).unwrap();
    assert_eq!(dc_after.total_distributed, draw, "config `total_distributed` did not advance by the draw");

    // Exempt posture: the withheld fee was queued for refund to the destination owner.
    let rq = banks.get_account(refund_queue.pubkey()).await.unwrap().unwrap();
    assert_eq!(read_queue_count(&rq.data), 1, "expected exactly one queued refund");
    let (recipient, amount) = read_queue_entry(&rq.data, 0);
    assert_eq!(recipient, payout_dest_owner, "refund queued to the wrong recipient");
    assert_eq!(amount, fee, "queued refund is not the withheld fee");
}
