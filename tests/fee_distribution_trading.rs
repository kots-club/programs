// DESTINATION (where this file goes): kots-contracts/tests/fee_distribution_trading.rs
// kots-contracts/tests/fee_distribution_trading.rs
//! Trading-distribution proof — 2026-08-08.
//!
//! The money_movement suite proves the CONTEST path (proc_contest_revenue_part1..3).
//! This file is its trading-side twin: it proves the four split parts
//! `execute_fee_distribution_part1..4` — the real, pinned, hooked path that
//! replaced the gated monolith `execute_fee_distribution`.
//!
//! Four properties, one test:
//!
//!   1. THE MONEY. Each of the seven legs lands EXACTLY floor(fee * bps / 10_000)
//!      in its destination — jackpot 50%, incoming 7%, admin 3%, treasury 34%,
//!      dev 3%, burn 1%, bounty 2% — read from the TRADING_*_DISTRIBUTION_BPS
//!      constants the parts hardcode (NOT from distribution_state: the trading
//!      split is compile-time, tamper-immune, so the test reads the same source
//!      the bytecode does). The vault is debited exactly the sum; floor dust stays.
//!
//!   2. THE HOOK RAN, once per leg. The mint is a real Token-2022 mint with
//!      TransferFeeConfig(300 bps) + TransferHook — every leg is fee-management →
//!      Token-2022 → transfer-hook. Proven by STATE: the hook enqueues one refund
//!      per exempt leg, so seven queue entries with the right recipients and
//!      amounts is only reachable if the hook executed seven times.
//!
//!   3. THE PINS BITE. `execute_fee_distribution_part1` with a jackpot destination
//!      owned by anything other than JACKPOT_WALLET must REVERT
//!      (OffCanonicalDistributionWallet) before a single lamport moves. Without
//!      this, a caller could redirect trading revenue to an arbitrary account.
//!
//!   4. THE HOOK IS LOAD-BEARING. The same part with the hook's account set
//!      omitted must FAIL (Token-2022 refuses a hooked mint without its resolved
//!      accounts) — a regression that de-hooks the mint can't hide behind a green
//!      happy path.
//!
//! Runs the four parts as SEPARATE transactions: atomic one-tx batching is the
//! caller's job (an Address Lookup Table — see the sidecar's
//! TradingDistributionBatcher), not enforced on-chain, and the PROGRAM behaviour
//! is identical either way. Part 3 books the stats, so distribution_count == 1
//! and total_fees_distributed == fee_amount after all four.
//!
//! Requires the compiled .so files:
//!     export BPF_OUT_DIR=$PWD/target/deploy

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer as SdkSigner},
    system_instruction,
    transaction::Transaction,
};

use fee_management::FeeDistributionState;
use kots_common::constants::{
    BOUNTY_DISTRIBUTION_BPS, TOKENS_PER_UNIT, TRADING_ADMIN_DISTRIBUTION_BPS,
    TRADING_BURN_DISTRIBUTION_BPS, TRADING_DEV_FUND_DISTRIBUTION_BPS,
    TRADING_INCOMING_DISTRIBUTION_BPS, TRADING_JACKPOT_DISTRIBUTION_BPS,
    TRADING_TREASURY_DISTRIBUTION_BPS,
};
use kots_common::network_config::{Network, SystemWallets};
use kots_common::state::CPI_AUTHORITY_SEED;
use transfer_hook as hook;

use spl_token_2022::extension::{
    transfer_fee, transfer_hook as t22_hook_ext, BaseStateWithExtensions, ExtensionType,
    StateWithExtensions,
};

// ---------------------------------------------------------------------------
// Refund-queue layout guards (raw offset reads, same discipline as money_movement)
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
// Small helpers
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

/// One Token-2022 token account of this mint's production shape (TransferFeeAmount
/// + TransferHookAccount), owned by `owner`. Pushes create + init onto `ixs`.
fn make_token_account(
    ixs: &mut Vec<Instruction>,
    payer: &Pubkey,
    acct: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
    acct_space: usize,
    acct_rent: u64,
) {
    ixs.push(system_instruction::create_account(
        payer,
        &acct.pubkey(),
        acct_rent,
        acct_space as u64,
        &spl_token_2022::id(),
    ));
    ixs.push(
        spl_token_2022::instruction::initialize_account3(
            &spl_token_2022::id(),
            &acct.pubkey(),
            mint,
            owner,
        )
        .unwrap(),
    );
}

// ---------------------------------------------------------------------------
// THE TEST
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trading_parts_land_seven_legs_through_the_hook_pins_bite_stats_advance() {
    assert_eq!(
        std::mem::size_of::<hook::PendingRefund>(),
        QUEUE_ENTRY_STRIDE,
        "PendingRefund layout changed — update the queue offsets in this file"
    );

    // ---- Universe: fee-management, transfer-hook, treasury-operations ----
    let mut pt = ProgramTest::default();
    pt.add_program("fee_management", fee_management::ID, None);
    pt.add_program("transfer_hook", hook::ID, None);
    pt.add_program("treasury_operations", treasury_operations::ID, None);

    let (treasury_incoming, _) =
        Pubkey::find_program_address(&[b"treasury_incoming_v4"], &treasury_operations::ID);
    pt.add_account(
        treasury_incoming,
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            data: vec![0u8; 512],
            owner: treasury_operations::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let (mut banks, payer, mut bh) = pt.start().await;

    // ---- Cast: vault authority + the SEVEN canonical destination owners ----
    let vault_authority = Keypair::new();
    let jackpot_owner = kots_common::constants::JACKPOT_WALLET;
    let incoming_owner = kots_common::constants::INCOMING_WALLET;
    let admin_owner = kots_common::constants::ADMIN_WALLET;
    let treasury_owner = kots_common::constants::TREASURY_WALLET;
    let dev_owner = kots_common::constants::DEV_WALLET;
    let burn_owner = kots_common::constants::BURN_ADDRESS;
    let bounty_owner = kots_common::constants::BOUNTY_WALLET;

    // ---- The mint: Token-2022 + TransferFeeConfig(300 bps) + TransferHook ----
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

    // ---- Hook bring-up in dependency order ----
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
                // All seven destination owners + the vault authority: platform
                // wallets are fee-exempt, so the hook refunds each leg (one queue
                // entry per leg — the property asserted below).
                data: hook::instruction::InitializeWhitelist {
                    wallets: vec![
                        vault_authority.pubkey(),
                        jackpot_owner,
                        incoming_owner,
                        admin_owner,
                        treasury_owner,
                        dev_owner,
                        burn_owner,
                        bounty_owner,
                    ],
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

    // ---- Token accounts: source vault + seven destinations, plus a MIS-OWNED
    //      jackpot account for the pin negative (owned by a random key). ----
    let acct_space =
        ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[
            ExtensionType::TransferFeeAmount,
            ExtensionType::TransferHookAccount,
        ])
        .unwrap();
    let acct_rent = rent.minimum_balance(acct_space);

    let source_vault = Keypair::new();
    let jackpot_ata = Keypair::new();
    let incoming_ata = Keypair::new();
    let admin_ata = Keypair::new();
    let treasury_ata = Keypair::new();
    let dev_ata = Keypair::new();
    let burn_ata = Keypair::new();
    let bounty_ata = Keypair::new();
    let bad_jackpot_ata = Keypair::new(); // owned by an off-canonical key
    let rogue_owner = Keypair::new().pubkey();

    let minted: u64 = 2_000_000 * TOKENS_PER_UNIT;
    {
        let mut ixs: Vec<Instruction> = Vec::new();
        make_token_account(&mut ixs, &payer.pubkey(), &source_vault, &mint.pubkey(), &vault_authority.pubkey(), acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &jackpot_ata, &mint.pubkey(), &jackpot_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &incoming_ata, &mint.pubkey(), &incoming_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &admin_ata, &mint.pubkey(), &admin_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &treasury_ata, &mint.pubkey(), &treasury_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &dev_ata, &mint.pubkey(), &dev_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &burn_ata, &mint.pubkey(), &burn_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &bounty_ata, &mint.pubkey(), &bounty_owner, acct_space, acct_rent);
        make_token_account(&mut ixs, &payer.pubkey(), &bad_jackpot_ata, &mint.pubkey(), &rogue_owner, acct_space, acct_rent);
        ixs.push(
            spl_token_2022::instruction::mint_to(
                &spl_token_2022::id(),
                &mint.pubkey(),
                &source_vault.pubkey(),
                &payer.pubkey(),
                &[],
                minted,
            )
            .unwrap(),
        );
        send(
            &mut banks,
            &payer,
            &[
                &source_vault, &jackpot_ata, &incoming_ata, &admin_ata, &treasury_ata,
                &dev_ata, &burn_ata, &bounty_ata, &bad_jackpot_ata,
            ],
            &ixs,
            &mut bh,
        )
        .await
        .expect("token account bring-up failed");
    }

    // ---- fee-management init_fee (creates fee_distribution v4 PDA) ----
    let (distribution_state, _) =
        Pubkey::find_program_address(&[b"fee_distribution", b"v4"], &fee_management::ID);
    let (treasury_main, _) =
        Pubkey::find_program_address(&[b"treasury_main_v4"], &treasury_operations::ID);
    let (treasury_outgoing, _) =
        Pubkey::find_program_address(&[b"treasury_outgoing_v4"], &treasury_operations::ID);
    let (fm_cpi_authority, _) =
        Pubkey::find_program_address(&[CPI_AUTHORITY_SEED], &fee_management::ID);

    send(
        &mut banks,
        &payer,
        &[],
        &[Instruction {
            program_id: fee_management::ID,
            accounts: fee_management::accounts::InitFee {
                distribution_state,
                authority: payer.pubkey(),
                treasury_incoming,
                treasury_main,
                treasury_outgoing,
                system_program: system,
                treasury_operations_program: treasury_operations::ID,
                fee_management_program: fee_management::ID,
                cpi_authority: fm_cpi_authority,
            }
            .to_account_metas(None),
            data: fee_management::instruction::InitFee {}.data(),
        }],
        &mut bh,
    )
    .await
    .expect("init_fee failed");

    // ---- The split, from the SAME constants the parts hardcode. Awkward amount
    //      so every leg exercises floor rounding. ----
    let fee_amount: u64 = 987_654_321_098_765;
    let leg = |bps: u64| (fee_amount as u128 * bps as u128 / 10_000) as u64;
    let jackpot_leg = leg(TRADING_JACKPOT_DISTRIBUTION_BPS);
    let incoming_leg = leg(TRADING_INCOMING_DISTRIBUTION_BPS);
    let admin_leg = leg(TRADING_ADMIN_DISTRIBUTION_BPS);
    let treasury_leg = leg(TRADING_TREASURY_DISTRIBUTION_BPS);
    let dev_leg = leg(TRADING_DEV_FUND_DISTRIBUTION_BPS);
    let burn_leg = leg(TRADING_BURN_DISTRIBUTION_BPS);
    let bounty_leg = leg(BOUNTY_DISTRIBUTION_BPS);
    let legs_sum =
        jackpot_leg + incoming_leg + admin_leg + treasury_leg + dev_leg + burn_leg + bounty_leg;
    assert!(legs_sum <= fee_amount && fee_amount - legs_sum <= 7, "floor-dust model is off");

    // Token-2022 rounds the fee UP; share ONE definition with the code under test.
    let fee_of = |amount: u64| transfer_hook::withheld_fee_for(amount, fee_bps);

    // ---- The hook's 17 forwarded accounts, byte-for-byte as money_movement ----
    let w = SystemWallets::for_network(Network::Devnet);
    let (sell_fee_debt_pda, _) = Pubkey::find_program_address(
        &[b"sell_fee_debt", vault_authority.pubkey().as_ref()],
        &hook::ID,
    );
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

    // Generous per-tx CU: each part is one transaction with <=2 hooked legs.
    let cu = || ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);

    // Part builders (account order MUST match the ExecuteFeeDistributionPartN structs).
    let build_part1 = |jackpot: Pubkey, with_metas: bool| {
        let mut ix = Instruction {
            program_id: fee_management::ID,
            accounts: fee_management::accounts::ExecuteFeeDistributionPart1 {
                source_vault: source_vault.pubkey(),
                jackpot_pool: jackpot,
                incoming_wallet: incoming_ata.pubkey(),
                vault_authority: vault_authority.pubkey(),
                kots_mint: mint.pubkey(),
                token_program: spl_token_2022::id(),
            }
            .to_account_metas(None),
            data: fee_management::instruction::ExecuteFeeDistributionPart1 { fee_amount }.data(),
        };
        if with_metas {
            ix.accounts.extend(hook_metas.clone());
        }
        ix
    };
    let part2 = {
        let mut ix = Instruction {
            program_id: fee_management::ID,
            accounts: fee_management::accounts::ExecuteFeeDistributionPart2 {
                source_vault: source_vault.pubkey(),
                admin_wallet: admin_ata.pubkey(),
                treasury_wallet: treasury_ata.pubkey(),
                vault_authority: vault_authority.pubkey(),
                kots_mint: mint.pubkey(),
                token_program: spl_token_2022::id(),
            }
            .to_account_metas(None),
            data: fee_management::instruction::ExecuteFeeDistributionPart2 { fee_amount }.data(),
        };
        ix.accounts.extend(hook_metas.clone());
        ix
    };
    let part3 = {
        let mut ix = Instruction {
            program_id: fee_management::ID,
            accounts: fee_management::accounts::ExecuteFeeDistributionPart3 {
                distribution_state,
                source_vault: source_vault.pubkey(),
                development_fund: dev_ata.pubkey(),
                burn_address: burn_ata.pubkey(),
                vault_authority: vault_authority.pubkey(),
                kots_mint: mint.pubkey(),
                token_program: spl_token_2022::id(),
            }
            .to_account_metas(None),
            data: fee_management::instruction::ExecuteFeeDistributionPart3 { fee_amount }.data(),
        };
        ix.accounts.extend(hook_metas.clone());
        ix
    };
    let build_part4 = |with_metas: bool| {
        let mut ix = Instruction {
            program_id: fee_management::ID,
            accounts: fee_management::accounts::ExecuteFeeDistributionPart4 {
                source_vault: source_vault.pubkey(),
                bounty_wallet: bounty_ata.pubkey(),
                vault_authority: vault_authority.pubkey(),
                kots_mint: mint.pubkey(),
                token_program: spl_token_2022::id(),
            }
            .to_account_metas(None),
            data: fee_management::instruction::ExecuteFeeDistributionPart4 { fee_amount }.data(),
        };
        if with_metas {
            ix.accounts.extend(hook_metas.clone());
        }
        ix
    };

    // -----------------------------------------------------------------------
    // NEGATIVE 1 — the pin bites. Part 1 with an off-canonical jackpot must
    // revert before any transfer. (Fails => no state change; runs first.)
    // -----------------------------------------------------------------------
    let pin_res = send(
        &mut banks,
        &payer,
        &[&vault_authority],
        &[cu(), build_part1(bad_jackpot_ata.pubkey(), true)],
        &mut bh,
    )
    .await;
    assert!(
        pin_res.is_err(),
        "part 1 accepted a jackpot destination NOT owned by JACKPOT_WALLET — the pin did not fire"
    );

    // -----------------------------------------------------------------------
    // NEGATIVE 2 — the hook is load-bearing. Part 4 with the metas omitted must
    // fail (Token-2022 refuses a hooked transfer without its resolved accounts).
    // -----------------------------------------------------------------------
    let nohook_res = send(
        &mut banks,
        &payer,
        &[&vault_authority],
        &[cu(), build_part4(false)],
        &mut bh,
    )
    .await;
    assert!(
        nohook_res.is_err(),
        "part 4 moved a hooked mint with NO hook accounts — the mint is not actually hooked"
    );

    // -----------------------------------------------------------------------
    // POSITIVE — the four parts, each its own transaction, in order. Nothing
    // above committed, so the vault is still full and the queue empty.
    // -----------------------------------------------------------------------
    let vault_before = token_amount(&mut banks, source_vault.pubkey()).await;
    assert_eq!(vault_before, minted, "negatives must not have moved the vault");

    for (label, ix) in [
        ("part1 (jackpot+incoming)", build_part1(jackpot_ata.pubkey(), true)),
        ("part2 (admin+treasury)", part2),
        ("part3 (dev+burn+stats)", part3),
        ("part4 (bounty)", build_part4(true)),
    ] {
        send(&mut banks, &payer, &[&vault_authority], &[cu(), ix], &mut bh)
            .await
            .unwrap_or_else(|e| panic!("{label} failed: {e:?}"));
    }

    // ---- PROPERTY 1 — the money, exact per wallet ----
    for (name, ata, leg_amount) in [
        ("jackpot", jackpot_ata.pubkey(), jackpot_leg),
        ("incoming", incoming_ata.pubkey(), incoming_leg),
        ("admin", admin_ata.pubkey(), admin_leg),
        ("treasury", treasury_ata.pubkey(), treasury_leg),
        ("development", dev_ata.pubkey(), dev_leg),
        ("burn", burn_ata.pubkey(), burn_leg),
        ("bounty", bounty_ata.pubkey(), bounty_leg),
    ] {
        let avail = token_amount(&mut banks, ata).await;
        let withheld = withheld_amount(&mut banks, ata).await;
        let fee = fee_of(leg_amount);
        assert_eq!(avail, leg_amount - fee, "{name}: available != leg - 3% fee");
        assert_eq!(withheld, fee, "{name}: withheld != 3% of its leg");
        assert_eq!(avail + withheld, leg_amount, "{name}: did not receive floor(fee * bps / 10000)");
    }

    let vault_after = token_amount(&mut banks, source_vault.pubkey()).await;
    assert_eq!(
        vault_after,
        vault_before - legs_sum,
        "source vault not debited by exactly the sum of the seven legs (floor dust stays)"
    );

    // ---- PROPERTY 2 — the hook ran once per leg, in transfer order ----
    let queue_data = banks
        .get_account(refund_queue.pubkey())
        .await
        .unwrap()
        .unwrap()
        .data;
    assert_eq!(
        read_queue_count(&queue_data),
        7,
        "expected one hook execution per leg (7); the forwarding path did not run as expected"
    );
    let expected_refunds = [
        (jackpot_owner, fee_of(jackpot_leg)),
        (incoming_owner, fee_of(incoming_leg)),
        (admin_owner, fee_of(admin_leg)),
        (treasury_owner, fee_of(treasury_leg)),
        (dev_owner, fee_of(dev_leg)),
        (burn_owner, fee_of(burn_leg)),
        (bounty_owner, fee_of(bounty_leg)),
    ];
    for (i, (want_recipient, want_amount)) in expected_refunds.iter().enumerate() {
        let (got_recipient, got_amount) = read_queue_entry(&queue_data, i);
        assert_eq!(&got_recipient, want_recipient, "refund entry {i}: wrong recipient");
        assert_eq!(&got_amount, want_amount, "refund entry {i}: amount != withheld (rate divergence)");
    }

    // ---- Bookkeeping — part 3 booked exactly one distribution for fee_amount ----
    let ds_after_acct = banks.get_account(distribution_state).await.unwrap().unwrap();
    let ds_after =
        FeeDistributionState::try_deserialize(&mut ds_after_acct.data.as_slice()).unwrap();
    assert_eq!(
        ds_after.total_fees_distributed, fee_amount,
        "distribution_state.total_fees_distributed != fee_amount"
    );
    assert_eq!(
        ds_after.distribution_count, 1,
        "expected exactly one recorded distribution — the rejected negatives must not have counted"
    );
}
