// DESTINATION (where this file goes): tests/lp_movement.rs
// TRACK A — migrated-rail proof for lp-position-manager.
//
// The mechanism (a hooked-mint transfer) is proven for fee-management in
// `money_movement.rs`, but that test authorizes the source with a PLAIN Keypair
// vault. The migrated rails authorize with a PROGRAM PDA that signs via
// `signer_seeds` — the genuinely unproven shape. This test proves the simplest
// such rail end to end: `distribute_kots_lp_fees_part4` moves the owner-bounty
// leg out of a manager-PDA-owned vault, through the Token-2022 transfer hook, and
// into the canonical BOUNTY_WALLET's account — with the manager PDA signing.
//
// Why lp part-4 (not treasury): it reaches the hooked transfer with almost no
// orthogonal scaffolding — one leg, one destination pin, `total_amount > 0` — so
// what the test exercises is the PDA-authority hook transfer itself, not a bucket
// ledger / purpose-key / jackpot-accounting machine. Treasury's
// `distribute_from_treasury` proves the same PDA property and is the natural
// SECOND proof once that machinery is worth exercising.
//
// UNVERIFIED IN-SANDBOX: there is no cargo/anchor here. `anchor build` +
// `cargo test -p <comprehensive-tests-crate> lp_movement` on the operator machine
// are the gate, exactly as for money_movement. The hook bring-up below is copied
// faithfully from the proven money_movement harness; the lp-specific pieces (the
// manager PDA account, the manager-owned source vault, the bounty destination) are
// new and are the most likely spots to need a fix-up round.

use anchor_lang::prelude::*;
use anchor_lang::{AccountSerialize, InstructionData, ToAccountMetas};
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
// The hook crate; aliased because spl_token_2022 has a `transfer_hook` module too.
use transfer_hook as hook;
use lp_position_manager as lp;

use spl_token_2022::extension::{
    transfer_fee, transfer_hook as t22_hook_ext, BaseStateWithExtensions, ExtensionType,
    StateWithExtensions,
};

// The manager PDA seed (must match lp-position-manager's `MANAGER_SEED`).
const MANAGER_SEED: &[u8] = b"lp_manager";
// The owner-bounty share, must match lp-position-manager's `BOUNTY_BPS_KOTS`.
const BOUNTY_BPS_KOTS: u64 = 100;

// ---------------------------------------------------------------------------
// Refund-queue layout guards (identical to money_movement — the queue is
// #[account(zero_copy)] #[repr(C)], read here by offset).
// ---------------------------------------------------------------------------
fn refund_queue_size() -> usize {
    8 + std::mem::size_of::<hook::PendingRefundQueue>()
}
const QUEUE_COUNT_OFFSET: usize = 8 + 32 + 2 + 2; // disc + authority + head + tail
const QUEUE_ENTRIES_OFFSET: usize = 8 + 32 + 2 + 2 + 2 + 2 + 8; // + count + padding + last_updated
const QUEUE_ENTRY_STRIDE: usize = 48; // Pubkey(32) + amount(8) + slot(8)

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
// Small helpers (identical to money_movement).
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
async fn lp_bounty_leg_lands_in_bounty_wallet_via_the_hook_under_pda_authority() {
    // Sanity-pin the queue layout the raw reads assume.
    assert_eq!(
        std::mem::size_of::<hook::PendingRefund>(),
        QUEUE_ENTRY_STRIDE,
        "PendingRefund layout changed — update the queue offsets in this file"
    );

    // -----------------------------------------------------------------------
    // Universe: lp-position-manager (under test) + transfer-hook (every leg
    // routes through it). Token-2022 + ATA are program-test builtins.
    // -----------------------------------------------------------------------
    let mut pt = ProgramTest::default();
    pt.add_program("lp_position_manager", lp::ID, None);
    pt.add_program("transfer_hook", hook::ID, None);

    // The manager PDA + a bump. The LPManager account is authorized by a plain
    // keypair we control (so we can sign for `has_one = authority`), but the
    // TRANSFER is authorized by the PDA itself signing via seeds — the point of
    // the test. Pre-created here (before start) because its authority pubkey is
    // knowable up front; the payer is not.
    let (manager_pda, manager_bump) = Pubkey::find_program_address(&[MANAGER_SEED], &lp::ID);
    let manager_authority = Keypair::new();

    let manager_state = lp::LPManager {
        authority: manager_authority.pubkey(),
        bump: manager_bump,
        jackpot_wallet: kots_common::constants::JACKPOT_WALLET,
        incoming_wallet: kots_common::constants::INCOMING_WALLET,
        admin_wallet: kots_common::constants::ADMIN_WALLET,
        treasury_wallet: kots_common::constants::TREASURY_WALLET,
        dev_wallet: kots_common::constants::DEV_WALLET,
        burn_address: kots_common::constants::BURN_ADDRESS,
        total_fees_collected_sol: 0,
        total_fees_collected_kots: 0,
        last_distribution: 0,
    };
    let mut manager_data = Vec::new();
    manager_state
        .try_serialize(&mut manager_data)
        .expect("serialize LPManager");
    pt.add_account(
        manager_pda,
        SolAccount {
            lamports: 10_000_000,
            data: manager_data,
            owner: lp::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let (mut banks, payer, mut bh) = pt.start().await;

    // -----------------------------------------------------------------------
    // Mint: Token-2022 with TransferFeeConfig(300 bps) + TransferHook — the
    // ceremony shape, identical to money_movement (300 bps matches what
    // initialize_fee_config writes, which the hook's refund math requires).
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

    // -----------------------------------------------------------------------
    // Hook bring-up in dependency order. Whitelist seeds BOTH the manager PDA
    // (the source authority) and BOUNTY_WALLET (the receiver) so this is the
    // production posture: a platform-destined distribution is fee-exempt and the
    // withheld fee is refunded via the queue.
    // -----------------------------------------------------------------------
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
                data: hook::instruction::InitializeFeeConfig {
                    network: Network::Devnet,
                }
                .data(),
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
                    wallets: vec![manager_pda, kots_common::constants::BOUNTY_WALLET],
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
    // Two token accounts of the production shape (TransferFeeAmount +
    // TransferHookAccount): the manager-PDA-owned source vault (funded) and the
    // bounty destination owned by the canonical BOUNTY_WALLET.
    // -----------------------------------------------------------------------
    let acct_space = ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[
        ExtensionType::TransferFeeAmount,
        ExtensionType::TransferHookAccount,
    ])
    .unwrap();
    let acct_rent = rent.minimum_balance(acct_space);

    let manager_kots = Keypair::new();
    let bounty_kots = Keypair::new();
    let minted: u64 = 2_000_000 * TOKENS_PER_UNIT;

    {
        let mut ixs: Vec<Instruction> = Vec::new();
        for (acct, owner) in [
            (&manager_kots, manager_pda),
            (&bounty_kots, kots_common::constants::BOUNTY_WALLET),
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
                &manager_kots.pubkey(),
                &payer.pubkey(),
                &[],
                minted,
            )
            .unwrap(),
        );
        send(&mut banks, &payer, &[&manager_kots, &bounty_kots], &ixs, &mut bh)
            .await
            .expect("token account bring-up failed");
    }

    // -----------------------------------------------------------------------
    // The hook's full account set, forwarded as remaining_accounts exactly as a
    // production caller must. Identical to money_movement EXCEPT sell_fee_debt is
    // keyed by the SOURCE authority — here the manager PDA, not a keypair vault.
    // -----------------------------------------------------------------------
    let w = SystemWallets::for_network(Network::Devnet);
    let (sell_fee_debt_pda, _) =
        Pubkey::find_program_address(&[b"sell_fee_debt", manager_pda.as_ref()], &hook::ID);
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

    // Awkward amount so the bounty leg exercises floor rounding. Well under the
    // minted supply. bounty = total * 100 / 10_000 (== lp_bps in the program).
    let total_amount: u64 = 1_234_567_890_123;
    let bounty_amount: u64 = (total_amount as u128 * BOUNTY_BPS_KOTS as u128 / 10_000) as u64;
    assert!(bounty_amount > 0 && bounty_amount < minted, "test amount misconfigured");

    let part4_accounts = lp::accounts::DistributeKOTSFeesPart4 {
        manager: manager_pda,
        authority: manager_authority.pubkey(),
        manager_kots_account: manager_kots.pubkey(),
        kots_mint: mint.pubkey(),
        bounty_kots: bounty_kots.pubkey(),
        token_program: spl_token_2022::id(),
    }
    .to_account_metas(None);

    // -----------------------------------------------------------------------
    // NEGATIVE CONTROL: without the hook's accounts, the transfer of a hooked
    // mint must FAIL. If this ever passes, the mint has been silently de-hooked.
    // (manager_authority signs; the pin + math pass, the hooked transfer does not.)
    // -----------------------------------------------------------------------
    let no_metas_ix = Instruction {
        program_id: lp::ID,
        accounts: part4_accounts.clone(),
        data: lp::instruction::DistributeKotsLpFeesPart4 { total_amount }.data(),
    };
    let neg = send(&mut banks, &payer, &[&manager_authority], &[no_metas_ix], &mut bh).await;
    assert!(
        neg.is_err(),
        "hooked-mint transfer succeeded WITHOUT the hook accounts — mint is not hooked"
    );

    // -----------------------------------------------------------------------
    // POSITIVE: the same call WITH the forwarded hook accounts. The manager PDA
    // signs the transfer via seeds (inside the program), so no extra signer here.
    // -----------------------------------------------------------------------
    let mut part4_ix = Instruction {
        program_id: lp::ID,
        accounts: part4_accounts,
        data: lp::instruction::DistributeKotsLpFeesPart4 { total_amount }.data(),
    };
    part4_ix.accounts.extend(hook_metas);
    send(&mut banks, &payer, &[&manager_authority], &[part4_ix], &mut bh)
        .await
        .expect("distribute_kots_lp_fees_part4 failed");

    // -----------------------------------------------------------------------
    // Assertions: the money moved through the hook, exactly.
    //  - source (manager vault) dropped by the full bounty amount,
    //  - destination holds the full amount (available + withheld, since Token-2022
    //    withholds the 300 bps fee on the recipient side),
    //  - and because BOUNTY_WALLET is exempt, the hook queued a refund of exactly
    //    the withheld fee to it.
    // -----------------------------------------------------------------------
    let fee = hook::withheld_fee_for(bounty_amount, fee_bps);

    assert_eq!(
        token_amount(&mut banks, manager_kots.pubkey()).await,
        minted - bounty_amount,
        "source vault did not drop by the bounty amount"
    );
    let dest_avail = token_amount(&mut banks, bounty_kots.pubkey()).await;
    let dest_withheld = withheld_amount(&mut banks, bounty_kots.pubkey()).await;
    assert_eq!(
        dest_avail + dest_withheld,
        bounty_amount,
        "bounty destination did not receive the full leg through the hook"
    );
    assert_eq!(dest_withheld, fee, "withheld fee on the destination is wrong");

    let rq = banks.get_account(refund_queue.pubkey()).await.unwrap().unwrap();
    assert_eq!(read_queue_count(&rq.data), 1, "expected exactly one queued refund");
    let (recipient, amount) = read_queue_entry(&rq.data, 0);
    assert_eq!(
        recipient,
        kots_common::constants::BOUNTY_WALLET,
        "refund queued to the wrong recipient"
    );
    assert_eq!(amount, fee, "queued refund is not the withheld fee");
}
