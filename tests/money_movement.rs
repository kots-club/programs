// DESTINATION (where this file goes): tests/money_movement.rs
// kots-contracts/tests/money_movement.rs
//! Money-movement suite — 2026-07-24.
//!
//! The security_regressions suite asserts that the BAD thing is rejected. This
//! file asserts the opposite half of the contract: that the GOOD thing actually
//! happens, to the exact raw unit. Until now, no test anywhere in the workspace
//! moved a single token through the on-chain programs — 355 green TS tests and
//! six green authz tests later, "fee distribution works" was still an untested
//! claim.
//!
//! One test, three properties:
//!
//!   1. `proc_contest_revenue` lands EXACTLY floor(revenue * bps / 10_000) in
//!      each of the four destination wallets (treasury 50%, development 30%,
//!      jackpot 10%, burn 10% — read back from the FeeDistributionState the
//!      program itself initialized, not re-hardcoded here), debits the source
//!      vault by exactly the sum of the five legs, and leaves the floor dust
//!      behind.
//!
//!   2. The transfer-hook forwarding path EXECUTES. The mint is a real
//!      Token-2022 mint with the TransferFeeConfig + TransferHook extensions —
//!      the production shape — so every leg is fee-management → Token-2022 →
//!      transfer-hook, three BPF programs deep. That is the exact CPI chain
//!      `award_jackpot` and `execute_fee_distribution` also depend on and which
//!      had never run before this test. Execution is proven by STATE, not logs:
//!      the hook enqueues one refund per exempt leg into the zero-copy refund
//!      queue, so `queue.count == 4` with the right recipients and amounts is
//!      only reachable if the hook ran four times.
//!
//!   3. The hook is LOAD-BEARING, not decorative: the same instruction with the
//!      hook's account set omitted must FAIL (Token-2022 refuses to transfer a
//!      hooked mint without its resolved accounts). Without this negative
//!      control, a regression that silently de-hooks the mint would leave the
//!      happy path green and the fee model dead.
//!
//! Deliberately NOT built on `MultiProgramTestContext`: that harness constructs
//! a classic SPL-Token universe (its `kots_mint` is a Tokenkeg mint), and the
//! whole point here is the Token-2022 + hook shape. A dedicated ProgramTest
//! with exactly three programs keeps the two universes from contaminating each
//! other.
//!
//! Requires the compiled .so files, same as security_regressions:
//!     export BPF_OUT_DIR=$PWD/target/deploy
//! (transfer_hook.so is now required in addition to the original seven.)

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
use kots_common::constants::TOKENS_PER_UNIT;
use kots_common::network_config::{Network, SystemWallets};
use kots_common::state::CPI_AUTHORITY_SEED;
// The crate under test on the hook side. Aliased because spl_token_2022 has a
// module of the same name (the mint extension) and both appear below.
use transfer_hook as hook;

use spl_token_2022::extension::{
    transfer_fee, transfer_hook as t22_hook_ext, BaseStateWithExtensions, ExtensionType,
    StateWithExtensions,
};

// ---------------------------------------------------------------------------
// Layout guards for the raw refund-queue reads below.
//
// The queue is #[account(zero_copy)] #[repr(C)], read here by offset rather
// than through bytemuck so the test does not depend on which bytemuck instance
// derived Pod. If someone reorders the struct, these fire at test time with a
// message instead of the offsets silently reading garbage.
// ---------------------------------------------------------------------------

/// 8 (anchor discriminator) + PendingRefundQueue.
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
// Small helpers
// ---------------------------------------------------------------------------

/// Send one transaction with a FRESH blockhash. Same rule the harness learned
/// the hard way (2026-07-23): a reused blockhash makes byte-identical retries
/// return the cached Ok without reaching the program.
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
async fn contest_revenue_lands_exact_amounts_in_all_four_wallets_via_the_hook() {
    // Sanity-pin the layout the raw reads above assume.
    assert_eq!(
        std::mem::size_of::<hook::PendingRefund>(),
        QUEUE_ENTRY_STRIDE,
        "PendingRefund layout changed — update the queue offsets in this file"
    );

    // -----------------------------------------------------------------------
    // Universe: fee-management (the program under test), transfer-hook (the
    // program every leg must route through), treasury-operations (init_fee
    // CPIs into it for rent reimbursement). Token-2022 + ATA are program-test
    // builtins — real BPF binaries, so the whole chain runs as deployed
    // bytecode.
    // -----------------------------------------------------------------------
    let mut pt = ProgramTest::default();
    pt.add_program("fee_management", fee_management::ID, None);
    pt.add_program("transfer_hook", hook::ID, None);
    pt.add_program("treasury_operations", treasury_operations::ID, None);

    // Pre-fund treasury_incoming, same as the shared harness and for the same
    // reason: init_fee reimburses its rent from it, and an empty treasury
    // correctly refuses to pay.
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

    // -----------------------------------------------------------------------
    // Cast: the vault authority (owner of the distribution source vault — a
    // plain Signer in ProcContestRevenue, so ownership of the vault IS the
    // authorization) and four destination owners. All five are whitelisted,
    // which is the production posture: platform wallets are exempt, so the
    // hook refunds the withheld fee and the anti-whale cap does not apply to
    // them ("treasury, dev, jackpot, etc. hold >>3% by design").
    // -----------------------------------------------------------------------
    let vault_authority = Keypair::new();
    // The five destinations are token accounts OWNED BY the canonical fee wallets
    // — the single source of truth in kots-common, and exactly what the
    // fee-management destination pins now require. A token account can be owned
    // by any address without holding its private key, and this test only RECEIVES
    // into these accounts (none of them signs), so no keypair is needed for them.
    let treasury_owner = kots_common::constants::TREASURY_WALLET;
    let dev_owner = kots_common::constants::DEV_WALLET;
    let jackpot_owner = kots_common::constants::JACKPOT_WALLET;
    let burn_owner = kots_common::constants::BURN_ADDRESS;
    let bounty_owner = kots_common::constants::BOUNTY_WALLET;

    // -----------------------------------------------------------------------
    // The mint: Token-2022 with TransferFeeConfig(300 bps) + TransferHook —
    // the ceremony's shape. 300 bps matches what initialize_fee_config writes
    // into fee_config.transfer_fee_bps; the hook's refund math REQUIRES the
    // two to agree ("MUST equal the mint's actual extension rate or
    // refunds/rebates will be wrong"), and this test would catch a divergence:
    // the queued refund amounts below are asserted equal to the actually
    // withheld amounts.
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
                // Production note: the ceremony sets the hook's fee_authority
                // PDA as the withheld-withdraw authority. Mirrored here.
                Some(&fee_authority_pda),
                fee_bps,
                u64::MAX, // no maximum_fee cap — keeps the 3% math exact
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
    // Hook bring-up, in dependency order: fee_config first (Devnet wallet
    // table — the currently deployed reality), then fee_authority, whitelist
    // (seeded with all five platform identities in ONE call), pool registry;
    // then the 49KB refund queue (client-side create_account because it is
    // over the 10KB CPI limit, then initialize_refund_queue which also writes
    // its address into fee_config); LAST the extra-account-meta list, which
    // snapshots fee_config.refund_queue and must therefore come after it.
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
                    wallets: vec![
                        vault_authority.pubkey(),
                        treasury_owner,
                        dev_owner,
                        jackpot_owner,
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

    // -----------------------------------------------------------------------
    // Five token accounts of the production account shape for this mint:
    // TransferFeeAmount (required by TransferFeeConfig) + TransferHookAccount
    // (required by TransferHook). Then 2M KOTS into the source vault.
    // -----------------------------------------------------------------------
    let acct_space =
        ExtensionType::try_calculate_account_len::<spl_token_2022::state::Account>(&[
            ExtensionType::TransferFeeAmount,
            ExtensionType::TransferHookAccount,
        ])
        .unwrap();
    let acct_rent = rent.minimum_balance(acct_space);

    let source_vault = Keypair::new();
    let treasury_ata = Keypair::new();
    let dev_ata = Keypair::new();
    let jackpot_ata = Keypair::new();
    let burn_ata = Keypair::new();
    let bounty_ata = Keypair::new();

    let minted: u64 = 2_000_000 * TOKENS_PER_UNIT;

    {
        let mut ixs: Vec<Instruction> = Vec::new();
        for (acct, owner) in [
            (&source_vault, vault_authority.pubkey()),
            (&treasury_ata, treasury_owner),
            (&dev_ata, dev_owner),
            (&jackpot_ata, jackpot_owner),
            (&burn_ata, burn_owner),
            (&bounty_ata, bounty_owner),
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
            &[&source_vault, &treasury_ata, &dev_ata, &jackpot_ata, &burn_ata, &bounty_ata],
            &ixs,
            &mut bh,
        )
        .await
        .expect("token account bring-up failed");
    }

    // -----------------------------------------------------------------------
    // fee-management bring-up: init_fee creates the fee_distribution v4 PDA
    // (via the treasury-reimbursement CPI, signed by fee-management's own
    // cpi_authority PDA — the post-C-2 shape) and writes the contest split.
    // -----------------------------------------------------------------------
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
            // init_fee is args-less now (destinations are pinned per-instruction,
            // not stored in distribution_state).
            data: fee_management::instruction::InitFee {}.data(),
        }],
        &mut bh,
    )
    .await
    .expect("init_fee failed");

    // Read the split back from chain: the expectations below derive from what
    // the program wrote, so a bps change in kots-common moves the assertions
    // with it instead of silently diverging.
    let ds_account = banks.get_account(distribution_state).await.unwrap().unwrap();
    let ds = FeeDistributionState::try_deserialize(&mut ds_account.data.as_slice()).unwrap();
    assert_eq!(
        ds.contest_treasury_bps
            + ds.contest_development_bps
            + ds.contest_jackpot_bps
            + ds.contest_burn_bps
            + kots_common::constants::BOUNTY_DISTRIBUTION_BPS,
        10_000,
        "contest split no longer sums to 100% — the dust assertions below assume it does"
    );

    // -----------------------------------------------------------------------
    // The instruction under test. Deliberately awkward amount so every leg
    // exercises floor rounding.
    // -----------------------------------------------------------------------
    let revenue: u64 = 1_234_567_890_123_457;
    let leg = |bps: u64| (revenue as u128 * bps as u128 / 10_000) as u64;
    let treasury_leg = leg(ds.contest_treasury_bps);
    let dev_leg = leg(ds.contest_development_bps);
    let jackpot_leg = leg(ds.contest_jackpot_bps);
    let burn_leg = leg(ds.contest_burn_bps);
    let bounty_leg = leg(kots_common::constants::BOUNTY_DISTRIBUTION_BPS);
    let legs_sum = treasury_leg + dev_leg + jackpot_leg + burn_leg + bounty_leg;
    assert!(legs_sum <= revenue && revenue - legs_sum <= 3, "floor-dust model is off");

    // CALLS THE PROGRAM'S FUNCTION, does not restate the model.
    //
    // This was `amount * fee_bps / 10_000` — floor. Token-2022 rounds the fee UP,
    // so every assertion below was off by one on any leg with a remainder, and
    // the suite could not have gone green even against a correct program. The
    // hook had the same floor bug in its refund path; both are fixed, and this
    // now shares ONE definition with the code under test so they cannot drift
    // apart again.
    let fee_of = |amount: u64| transfer_hook::withheld_fee_for(amount, fee_bps);

    // The Devnet wallet table, because fee_config was initialized with
    // Network::Devnet: the extra-account-meta list pins these five pubkeys as
    // the writable SOL-fee wallets, and Token-2022's resolver matches by key.
    let w = SystemWallets::for_network(Network::Devnet);
    let (sell_fee_debt_pda, _) = Pubkey::find_program_address(
        &[b"sell_fee_debt", vault_authority.pubkey().as_ref()],
        &hook::ID,
    );
    // Hardcoded in the hook's meta list (entry #13); must match byte-for-byte.
    let ata_program_id: Pubkey =
        std::str::FromStr::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL").unwrap();

    // (2026-08-04) The monolithic `proc_contest_revenue` is GATED on-chain
    // (KotsError::MonolithicDistributionHeapBound) and slated for removal at
    // the mainnet-ceremony ABI cleanup; this test no longer constructs it.
    // The hook load-bearing control below moved to Part 1.

    // The hook's full account set, forwarded as remaining_accounts exactly as
    // a production caller must: the hook program itself, the validation PDA,
    // then the fifteen entries of the meta list in order. Token-2022 resolves
    // by pubkey, so what matters is that every one of these is PRESENT and at
    // least as writable as the list demands.
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

    // -----------------------------------------------------------------------
    // NEGATIVE CONTROL FIRST: without the hook's accounts, the transfer of a
    // hooked mint must FAIL. If this ever starts passing, the mint has been
    // silently de-hooked and every fee assumption in the tokenomics is void.
    // -----------------------------------------------------------------------
    // ═══════════════════════════════════════════════════════════════════════
    // GATE A PROBE — ONE hooked CPI transfer, default heap, default CU.
    // ═══════════════════════════════════════════════════════════════════════
    //
    // WHY THIS EXISTS, AND WHY THE LADDER BELOW DOES NOT ANSWER IT.
    //
    // The ladder's first rung is named "router condition" because it omits
    // ComputeBudget. That is necessary and nowhere near sufficient:
    // `proc_contest_revenue` performs FOUR hooked transfers in one instruction
    // and NO DEX router does that — a router does one. Reasoning from that rung
    // to "KOTS is untradeable" was wrong, and it was reasoned from for several
    // runs on 2026-07-29.
    //
    // This probe is the real thing:
    //
    //   * ONE hooked CPI transfer. Revenue is tuned from the bps the program
    //     itself wrote so that exactly one leg rounds above zero —
    //     `transfer_one_leg` returns early on `amount == 0`, so the other three
    //     never reach `invoke_transfer_checked`.
    //   * NO ComputeBudget instructions. Default 32 KB heap, default 200K CU,
    //     which is precisely what a router gives an arbitrary token.
    //   * NON-EXEMPT destination. The throwaway accounts below are owned by
    //     fresh keypairs that are not in the whitelist, so the hook takes the
    //     ordinary trader path rather than the platform-wallet refund path.
    //     That is the shape a Meteora or Jupiter swap actually has.
    //
    // It runs BEFORE the four-leg call and writes into a THROWAWAY, non-exempt
    // destination, so it disturbs nothing the assertions below read (a non-exempt
    // transfer takes the trader path and enqueues no refund).
    //
    // It is issued as a RAW hooked `transfer_checked`, NOT through a distribution
    // instruction. It used to ride Part 1, which was legal because
    // `ProcContestRevenue` constrained none of its destinations. The destination
    // pins (kots-common single source) changed that: every callable distribution
    // instruction now requires each destination to be OWNED BY a canonical wallet,
    // and every canonical wallet is whitelisted/exempt — so any pin-passing dest
    // would take the platform-refund path and disturb the refund assertions below.
    // A raw transfer still forwards all 17 hook accounts, so the resolver — the
    // dominant 32 KB-heap cost, and the thing that actually OOMs — runs exactly as
    // in production. What it no longer wraps around that is an intermediate
    // program's frame, so it is a slightly weaker router proxy than the Part-1
    // form; the heap piece it proves is the load-bearing one.
    // The probe must be GENUINELY non-exempt on BOTH sides. The hook exempts a
    // transfer when EITHER the source OR the destination owner is whitelisted
    // (is_sender_exempt || is_receiver_exempt), and the distribution `source_vault`
    // is authority-owned by `vault_authority` — which IS whitelisted — so a
    // transfer OUT of it takes the platform-refund path no matter what the
    // destination is. That is the DEX shape's opposite. So the probe funds its
    // OWN source: a fresh keypair `probe_source_auth` that is NOT in the whitelist,
    // owning a freshly minted vault. Source non-exempt + destination non-exempt =
    // the ordinary trader path, and NO refund is ever enqueued at ANY amount, so
    // this cannot disturb the refund-queue assertions below. `source_vault` is now
    // untouched by the probe, so vault_before_legs below equals the minted total.
    let probe_source_auth = Keypair::new(); // NOT whitelisted
    let probe_source = Keypair::new();
    let probe_dests: Vec<Keypair> = (0..1).map(|_| Keypair::new()).collect();
    {
        let mut ixs: Vec<Instruction> = Vec::new();
        // Fresh non-exempt SOURCE (minted directly — mint_to is not a hooked
        // transfer, so it adds no hook/refund noise), plus the throwaway dest.
        ixs.push(system_instruction::create_account(
            &payer.pubkey(),
            &probe_source.pubkey(),
            acct_rent,
            acct_space as u64,
            &spl_token_2022::id(),
        ));
        ixs.push(
            spl_token_2022::instruction::initialize_account3(
                &spl_token_2022::id(),
                &probe_source.pubkey(),
                &mint.pubkey(),
                &probe_source_auth.pubkey(), // owner: fresh, and NOT whitelisted
            )
            .unwrap(),
        );
        for acct in &probe_dests {
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
                    &Keypair::new().pubkey(), // owner: nobody, and NOT whitelisted
                )
                .unwrap(),
            );
        }
        // Fund the non-exempt source (payer is the mint authority).
        ixs.push(
            spl_token_2022::instruction::mint_to(
                &spl_token_2022::id(),
                &mint.pubkey(),
                &probe_source.pubkey(),
                &payer.pubkey(),
                &[],
                TOKENS_PER_UNIT, // 1 KOTS; comfortably covers probe_amount + fee
            )
            .unwrap(),
        );
        let mut signers: Vec<&Keypair> = vec![&probe_source];
        signers.extend(probe_dests.iter());
        send(&mut banks, &payer, &signers, &ixs, &mut bh)
            .await
            .expect("probe source/destination accounts could not be created");
    }

    // A single RAW hooked transfer_checked FROM the non-exempt source TO the
    // non-exempt throwaway — the ordinary trader path. No leg-tuning is needed
    // (a raw transfer is one transfer by construction). The full 17-account hook
    // set is appended, so Token-2022 resolves the ExtraAccountMetaList and invokes
    // the hook exactly as a production trader transfer of this mint does.
    let probe_amount: u64 = 1_000_000; // 0.001 KOTS; any non-zero amount suffices
    let mut probe_ix = spl_token_2022::instruction::transfer_checked(
        &spl_token_2022::id(),
        &probe_source.pubkey(),
        &mint.pubkey(),
        &probe_dests[0].pubkey(),
        &probe_source_auth.pubkey(),
        &[],
        probe_amount,
        decimals,
    )
    .expect("probe transfer_checked build failed");
    probe_ix.accounts.extend(hook_metas.clone());

    println!(
        "\n=== GATE A PROBE — 1 raw hooked transfer, no ComputeBudget, non-exempt src+dest ==="
    );
    let probe_res = send(&mut banks, &payer, &[&probe_source_auth], &[probe_ix], &mut bh).await;
    match &probe_res {
        Ok(()) => println!(
            "  PASS — a single raw hooked transfer of this mint COMPLETES in a \
             default 32 KB heap and 200K CU. This is the Gate A A1-A5 \
             precondition, and it is the first time it has held."
        ),
        Err(e) => println!("  FAIL — {e:?}"),
    }
    println!("=== end probe ===\n");

    // AVAILABLE + WITHHELD, not available alone.
    //
    // The hook applies the 300 bps fee on this non-exempt transfer, so the amount
    // arrives split: `available` holds amount - fee and the TransferFeeAmount
    // extension withholds `fee`. `token_amount()` reads only the available half,
    // so "did the transfer land" is available + withheld — the full amount either
    // way. (Summed over probe_dests, which is now a single account.)
    let mut probe_landed = 0u64;
    for d in &probe_dests {
        // Two statements, not one expression. `f(&mut banks).await +
        // g(&mut banks).await` holds the first mutable borrow until the end of
        // the STATEMENT, so the second call overlaps it — E0499. The assertion
        // loop further down already does it this way; I should have copied it.
        let avail = token_amount(&mut banks, d.pubkey()).await;
        let withheld = withheld_amount(&mut banks, d.pubkey()).await;
        probe_landed += avail + withheld;
    }
    probe_res.expect(
        "GATE A PROBE FAILED — a SINGLE raw hooked transfer could not complete with \
         a default heap and default CU. This is the router case, so on this evidence \
         the mint cannot be swapped on any DEX and Gate A A1-A5 cannot pass. Read the \
         cause from the runtime log: 'memory allocation failed' is the heap, \
         'Access violation' is the stack, 'Computational budget exceeded' is CU — \
         they need different fixes and the transaction error alone cannot tell them \
         apart.",
    );
    assert!(
        probe_landed > 0,
        "the probe transaction succeeded but nothing arrived in any destination \
         (available + withheld are both zero) — the leg was skipped rather than \
         transferred, so this proved nothing about the hook"
    );

    // The probe now spends from its OWN source (see above), so the distribution
    // vault is untouched here and its balance still equals `minted`. Read the live
    // balance anyway as the baseline for the four-leg assertions below — reading
    // rather than assuming is the robust habit, and it stays correct if the setup
    // ever changes.
    let vault_before_legs = token_amount(&mut banks, source_vault.pubkey()).await;
    assert_eq!(
        vault_before_legs, minted,
        "the distribution vault must be intact before the four legs — the probe \
         uses its own source and must not debit this vault"
    );

    // HOOK LOAD-BEARING CONTROL (2026-08-04). Previously this exercised the
    // OLD monolithic `proc_contest_revenue` without the hook account set; that
    // instruction is now GATED and refuses before the hook question is even
    // asked. The control therefore moves to Part 1 WITHOUT the hook metas —
    // same claim, live entry point.
    let bare_no_hook = Instruction {
        program_id: fee_management::ID,
        accounts: fee_management::accounts::ProcContestRevenuePart1 {
            distribution_state,
            source_vault: source_vault.pubkey(),
            treasury_wallet: treasury_ata.pubkey(),
            development_fund: dev_ata.pubkey(),
            vault_authority: vault_authority.pubkey(),
            kots_mint: mint.pubkey(),
            token_program: spl_token_2022::id(),
        }
        .to_account_metas(None),
        data: fee_management::instruction::ProcContestRevenuePart1 {
            revenue_amount: revenue,
        }
        .data(),
    };
    let res = send(&mut banks, &payer, &[&vault_authority], &[bare_no_hook], &mut bh).await;
    assert!(
        res.is_err(),
        "Part 1 moved a hooked mint WITHOUT the transfer-hook account set — \
         the hook is not load-bearing and the fee model is bypassable"
    );

    // -----------------------------------------------------------------------
    // THE REAL CALL — run as an O32 HEAP LADDER (2026-07-29).
    //
    // WHY THIS IS NO LONGER A SINGLE HARD-CODED SEND
    //
    // On 2026-07-29 the refund queue was shrunk from 1024 entries (49,164 B) to
    // 128 (6,200 B), which cut the TLV resolver's copy by ~43 KB. This call
    // still died with "memory allocation failed, out of memory", at 63,656 CU
    // versus 60,784 before — i.e. the queue shrank by 43 KB and the failure
    // barely moved. Shrinking the queue was necessary and is not sufficient.
    //
    // ONE LEG, NOT FOUR. `transfer_one_leg` returns early when `amount == 0` and
    // its "O32: forwarding N" log sits AFTER that return, so the single
    // `forwarding 17` line in the failing run proves the OOM happens on the
    // FIRST non-zero leg — one `invoke_transfer_checked`. The earlier "the
    // fourth leg exhausts the heap" reading is wrong and is corrected here.
    //
    // WHAT WAS UNRESOLVED. Whether solana-program-test's BPF VM APPLIES
    // `request_heap_frame` at all. `ops/o32-run.sh` greps the trace for
    // ComputeBudget and reports the request as processed when it appears — but
    // that only proves the instruction ran. The CU limit demonstrably applies
    // (1,399,700 rather than 200,000); the heap field is a different code path,
    // and a green line there is not evidence about it. Another check that reads
    // as a pass and measures something adjacent.
    //
    // WHAT THIS LADDER MEASURES. The same instruction under escalating heap
    // requests, cheapest first, stopping at the first configuration that works.
    // The outcomes are printed as a table, so the run yields a NUMBER rather
    // than a verdict:
    //
    //   * The first rung is the DEX-ROUTER CONDITION — no ComputeBudget
    //     instructions at all, default 32 KB heap, default 200K CU. A router
    //     builds its own transaction and will never request a frame for an
    //     arbitrary token. If that rung fails, KOTS is untradeable no matter
    //     what the later rungs do, and Gate A A1-A5 cannot pass.
    //   * If EVERY rung fails identically, the frame is not being applied and
    //     this harness cannot answer the question — the next measurement has to
    //     move to a validator.
    //   * If a later rung succeeds, the frame IS applied, the failure is a real
    //     heap requirement, and the ladder has bracketed it.
    //
    // Ordering matters: a successful transaction COMMITS, so the ladder must
    // stop at the first success or the assertions below would see the transfer
    // applied more than once. Failed transactions revert and leave no trace,
    // which is why the earlier rungs are safe to attempt.
    // -----------------------------------------------------------------------
    // THREE INSTRUCTIONS, ONE TRANSACTION (2026-07-29 split; 2026-08-04
    // part 3 added for the owner-bounty leg — max two hooked legs per
    // top-level instruction, learned the hard way twice).
    //
    // The four-leg `proc_contest_revenue` cannot complete: leg 3 dies on CU at
    // the 200K default and on the heap at 1.4M. The heap is allocated per
    // top-level instruction, so splitting gives each part a fresh 32 KB — while
    // staying in ONE transaction, which keeps the distribution atomic. If part 2
    // fails the whole thing reverts and part 1's transfers roll back with it.
    //
    // CU is charged per TRANSACTION, so splitting does NOT reduce it. The ladder
    // rungs below still vary the CU limit, and that is the point: the split is
    // expected to fix the heap and leave CU as an ordinary client obligation.
    let mut part1 = Instruction {
        program_id: fee_management::ID,
        accounts: fee_management::accounts::ProcContestRevenuePart1 {
            distribution_state,
            source_vault: source_vault.pubkey(),
            treasury_wallet: treasury_ata.pubkey(),
            development_fund: dev_ata.pubkey(),
            vault_authority: vault_authority.pubkey(),
            kots_mint: mint.pubkey(),
            token_program: spl_token_2022::id(),
        }
        .to_account_metas(None),
        data: fee_management::instruction::ProcContestRevenuePart1 {
            revenue_amount: revenue,
        }
        .data(),
    };
    part1.accounts.extend(hook_metas.clone());

    let mut part2 = Instruction {
        program_id: fee_management::ID,
        accounts: fee_management::accounts::ProcContestRevenuePart2 {
            distribution_state,
            source_vault: source_vault.pubkey(),
            jackpot_pool: jackpot_ata.pubkey(),
            burn_address: burn_ata.pubkey(),
            vault_authority: vault_authority.pubkey(),
            kots_mint: mint.pubkey(),
            token_program: spl_token_2022::id(),
        }
        .to_account_metas(None),
        data: fee_management::instruction::ProcContestRevenuePart2 {
            revenue_amount: revenue,
        }
        .data(),
    };
    part2.accounts.extend(hook_metas.clone());

    // O32 PART 3 (2026-08-04): the owner-bounty leg alone — the 3-leg Part 1
    // proved heap-bound in this very ladder; the split invariant is now max
    // TWO hooked legs per top-level instruction. Same transaction, so the
    // whole distribution stays atomic.
    let mut part3 = Instruction {
        program_id: fee_management::ID,
        accounts: fee_management::accounts::ProcContestRevenuePart3 {
            source_vault: source_vault.pubkey(),
            bounty_wallet: bounty_ata.pubkey(),
            vault_authority: vault_authority.pubkey(),
            kots_mint: mint.pubkey(),
            token_program: spl_token_2022::id(),
        }
        .to_account_metas(None),
        data: fee_management::instruction::ProcContestRevenuePart3 {
            revenue_amount: revenue,
        }
        .data(),
    };
    part3.accounts.extend(hook_metas);


    const CU: u32 = 1_400_000;
    // (label, requests_a_heap_frame, prefix). The flag is DATA, not something to
    // recover from the label later: `label.contains("heap frame")` matched
    // "no heap frame" on 2026-07-29 and printed the exact opposite of the run.
    let ladder: Vec<(&str, bool, Vec<Instruction>)> = vec![
        ("router condition (no ComputeBudget)", false, vec![]),
        (
            "CU limit only, no heap frame",
            false,
            vec![ComputeBudgetInstruction::set_compute_unit_limit(CU)],
        ),
        (
            "CU limit + 64 KB heap frame",
            true,
            vec![
                ComputeBudgetInstruction::set_compute_unit_limit(CU),
                ComputeBudgetInstruction::request_heap_frame(64 * 1024),
            ],
        ),
        (
            "CU limit + 128 KB heap frame",
            true,
            vec![
                ComputeBudgetInstruction::set_compute_unit_limit(CU),
                ComputeBudgetInstruction::request_heap_frame(128 * 1024),
            ],
        ),
        (
            "CU limit + 256 KB heap frame",
            true,
            vec![
                ComputeBudgetInstruction::set_compute_unit_limit(CU),
                ComputeBudgetInstruction::request_heap_frame(256 * 1024),
            ],
        ),
    ];

    println!("\n=== O32 HEAP LADDER — one hooked CPI transfer, escalating heap ===");
    let mut succeeded_with: Option<(String, bool)> = None;
    let mut outcomes: Vec<String> = Vec::new();

    for (label, requests_heap_frame, prefix) in ladder {
        let mut ixs = prefix;
        ixs.push(part1.clone());
        ixs.push(part2.clone());
        ixs.push(part3.clone());

        let res = send(&mut banks, &payer, &[&vault_authority], &ixs, &mut bh).await;

        // CLASSIFY, do not just record. On 2026-07-29 this loop printed
        // "FAILED" for both `ProgramFailedToComplete` and `Custom(3005)`, which
        // are opposite results: the first is the resolver dying of heap
        // exhaustion before Token-2022 is even reached; the second is the
        // resolver SUCCEEDING, Token-2022 invoking the hook, and the hook's
        // accounts struct wanting one more key than the meta list declares.
        //
        // Note 3005 is doubly ambiguous — it was ALSO the 2026-07-28 symptom,
        // where it meant no resolution happened at all (hook received 4
        // accounts). The disambiguator is the "O32: hook received N accounts"
        // line: 4 means resolution never ran, 18+ means it ran and the struct
        // is short. Read the log, not just the code.
        let verdict = match &res {
            Ok(()) => "OK",
            Err(e) => {
                let text = format!("{e:?}");
                if text.contains("ProgramFailedToComplete") {
                    // NOT "OOM". The harness sees only a TransactionError; it
                    // cannot see the runtime log, so it cannot know WHY the
                    // program aborted. Calling this OOM was an inference printed
                    // as a measurement, and capacity 1 disproved it — same
                    // error code, no "memory allocation failed" anywhere in the
                    // log. Report the code, and say where the cause lives.
                    "aborted (ProgramFailedToComplete) — cause is in the runtime log, not here"
                } else if text.contains("Custom(3005)") {
                    "resolution OK, hook reached, accounts struct SHORT (3005)"
                } else if text.contains("ComputationalBudgetExceeded")
                    || text.contains("ExceededMaxInstructions")
                {
                    // Transaction-level CU exhaustion. Expected on rung 1 now
                    // that proc_contest_revenue is split: CU is charged per
                    // TRANSACTION, so four legs exceed the 200K default however
                    // they are divided. Four of six rows in the 2026-07-29 sweep
                    // read "other" for this, which the classifier could have
                    // named.
                    "CU exhausted (expected on rung 1 — 4 legs exceed 200K)"
                } else if text.contains("Custom(2110272652)") {
                    "hook accounts missing entirely (negative-control shape)"
                } else {
                    "other"
                }
            }
        };

        match res {
            Ok(()) => {
                println!("  {label:<38} -> OK");
                outcomes.push(format!("{label}: OK"));
                succeeded_with = Some((label.to_string(), requests_heap_frame));
                // MUST stop: this transaction committed. Continuing would apply
                // the distribution twice and every assertion below would be
                // measuring two transfers while claiming to measure one.
                break;
            }
            Err(e) => {
                println!("  {label:<38} -> {verdict}");
                println!("  {:<38}    {e:?}", "");
                outcomes.push(format!("{label}: {verdict} [{e:?}]"));
            }
        }
    }
    println!("=== end ladder ===\n");

    let (required_heap, needed_heap_frame) = succeeded_with.unwrap_or_else(|| {
        let all = outcomes.join("\n");

        // Branch on WHAT was seen. The previous version of this message asserted
        // that "one hooked CPI transfer completed under NO heap configuration",
        // which is false whenever the failure is 3005 — in that case the
        // transfer's RESOLUTION completed, Token-2022 ran, and the hook
        // executed. A panic message that misstates the result is worse than a
        // terse one, because it is what the next reader will quote.
        if all.contains("accounts struct SHORT") {
            panic!(
                "RESOLUTION SUCCEEDED — and this is the EXPECTED result of an \
                 `--features o32-probe` build.\n\n{all}\n\n\
                 The resolver survived, Token-2022 invoked the hook, and the hook \
                 executed. It then failed because `refund_queue` is still declared \
                 in the TransferHook accounts struct while the probe omits it from \
                 the meta list. That is the POSITIVE branch of the experiment: the \
                 refund queue was the binding constraint on the heap.\n\n\
                 If you did NOT build with o32-probe, this is a real defect: the \
                 meta list and the accounts struct disagree, and every transfer of \
                 this mint will revert. Check the \"extra_account_metas: N entries\" \
                 line against the struct.\n\n\
                 Next: bisect a capacity that fits WITH the queue present — \
                 `bash ops/o32-bisect.sh`."
            );
        }

        panic!(
            "O32 HEAP-BOUND — the resolver never reached Token-2022 under any \
             configuration, including a 256 KB frame.\n\n{all}\n\n\
             If the rungs failed IDENTICALLY (same CU consumed), \
             solana-program-test is not applying `request_heap_frame` — measured \
             2026-07-29, where all five rungs died at exactly 59,156 CU while the \
             CU limit itself tracked correctly. That is NOT a reason to dismiss \
             the result: a harness that supplies 32 KB and ignores frames IS a \
             DEX router, so the router rung is faithful and the mint is \
             untradeable in this configuration.\n\n\
             Next: reduce the meta-list copy. `ops/o32-bisect.sh` sweeps the \
             refund-queue capacity; below ~16 entries the queue is no longer the \
             dominant term and MAX_WHITELISTED_WALLETS (2,100 B) becomes the \
             thing to shrink."
        )
    });

    // Report what the winning rung ACTUALLY required. The previous version of
    // this block treated "not rung 1" as "needed a heap frame" and printed that
    // a router "still runs out of memory and reverts" — on a run whose winning
    // rung was literally named "no heap frame" and in which no OOM occurred.
    // A message that contradicts the evidence directly above it is worse than
    // silence, because it is what gets quoted.
    let needed_cu_bump = required_heap != "router condition (no ComputeBudget)";

    if needed_heap_frame {
        println!(
            "⚠ O32 / GATE A — the cheapest rung that completed was \"{required_heap}\", \
             which REQUESTS A HEAP FRAME. A DEX router supplies none, and this \
             harness does not apply the request anyway (measured 2026-07-29), so a \
             router-initiated swap would still fail. The meta list has to shrink."
        );
    } else if needed_cu_bump {
        println!(
            "✓ NO HEAP FRAME REQUIRED. The cheapest rung that completed was \
             \"{required_heap}\" — a CU limit and nothing more.\n  \
             CU is charged per TRANSACTION, so four legs exceed the 200K default \
             however they are split; that is an ordinary client obligation and \
             `set_compute_unit_limit` IS honoured. It is NOT a tradeability \
             problem: a swap is ONE hooked transfer, and the Gate A probe above \
             completed that with no ComputeBudget instructions at all."
        );
    } else {
        println!(
            "✓ Four legs completed with NO ComputeBudget instructions at all. \
             Nothing is required of the caller."
        );
    }

    // -----------------------------------------------------------------------
    // PROPERTY 1 — the money. Token-2022 withholds 3% of each leg INSIDE the
    // destination account (available balance + withheld == the leg), and the
    // vault is debited the full leg. Exact, per wallet, to the raw unit.
    // -----------------------------------------------------------------------
    for (name, ata, leg_amount) in [
        ("treasury", treasury_ata.pubkey(), treasury_leg),
        ("development", dev_ata.pubkey(), dev_leg),
        ("jackpot", jackpot_ata.pubkey(), jackpot_leg),
        ("burn", burn_ata.pubkey(), burn_leg),
        ("bounty", bounty_ata.pubkey(), bounty_leg),
    ] {
        let avail = token_amount(&mut banks, ata).await;
        let withheld = withheld_amount(&mut banks, ata).await;
        let fee = fee_of(leg_amount);
        assert_eq!(
            avail,
            leg_amount - fee,
            "{name} wallet available balance is not leg - 3% fee"
        );
        assert_eq!(withheld, fee, "{name} wallet withheld fee is not exactly 3% of its leg");
        assert_eq!(
            avail + withheld,
            leg_amount,
            "{name} wallet did not receive exactly floor(revenue * bps / 10000)"
        );
    }

    let vault_after = token_amount(&mut banks, source_vault.pubkey()).await;
    assert_eq!(
        vault_after,
        vault_before_legs - legs_sum,
        "source vault was not debited by exactly the sum of the five legs \
         (floor dust must remain in the vault). Baseline is vault_before_legs, \
         read live right before the legs — it equals `minted`, since the probe \
         spends from its own source, not this vault."
    );

    // -----------------------------------------------------------------------
    // PROPERTY 2 — the hook ran, once per leg, on the right data. The refund
    // queue is written ONLY by the hook's execute path, so four entries whose
    // recipients are the four destination OWNERS (in transfer order: treasury,
    // development, jackpot, burn) and whose amounts equal the actually-
    // withheld fees are proof of four hook executions with correct inputs —
    // and proof the refund math agrees with the extension rate.
    // -----------------------------------------------------------------------
    let queue_data = banks
        .get_account(refund_queue.pubkey())
        .await
        .unwrap()
        .unwrap()
        .data;
    assert_eq!(
        read_queue_count(&queue_data),
        5,
        "expected exactly one hook execution per leg (5); the transfer-hook \
         forwarding path did not run as expected"
    );
    let expected_refunds = [
        (treasury_owner, fee_of(treasury_leg)),
        (dev_owner, fee_of(dev_leg)),
        (jackpot_owner, fee_of(jackpot_leg)),
        (burn_owner, fee_of(burn_leg)),
        (bounty_owner, fee_of(bounty_leg)),
    ];
    for (i, (want_recipient, want_amount)) in expected_refunds.iter().enumerate() {
        let (got_recipient, got_amount) = read_queue_entry(&queue_data, i);
        assert_eq!(
            &got_recipient, want_recipient,
            "refund queue entry {i}: wrong recipient — hook saw a different \
             destination owner than the leg transferred to"
        );
        assert_eq!(
            &got_amount, want_amount,
            "refund queue entry {i}: refund amount != withheld amount — \
             fee_config.transfer_fee_bps has diverged from the mint's \
             TransferFeeConfig rate"
        );
    }

    // -----------------------------------------------------------------------
    // Bookkeeping: exactly ONE distribution recorded (the failed bare call
    // must not have counted), for exactly the requested revenue.
    // -----------------------------------------------------------------------
    let ds_after_acct = banks.get_account(distribution_state).await.unwrap().unwrap();
    let ds_after =
        FeeDistributionState::try_deserialize(&mut ds_after_acct.data.as_slice()).unwrap();
    assert_eq!(
        ds_after.total_contest_revenue_distributed, revenue,
        "distribution_state.total_contest_revenue_distributed != revenue"
    );
    assert_eq!(
        ds_after.distribution_count, 1,
        "expected exactly one recorded distribution — either the rejected \
         bare call counted, or the real one did not"
    );
}
