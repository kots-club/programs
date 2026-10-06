// kots-contracts/tests/devnet_idl_tests/paid_vote_devnet_test.rs
//! Paid hieroglyph (KOTS-SPL) money-path proof — triage **step 4**.
//!
//! WHY THIS IS A DEVNET TEST, NOT A LOCAL ONE (read before "fixing" it into the
//! banks-client harness):
//!
//! `execute_hieroglyph` → `execute_hieroglyph_payment` transfers KOTS SPL from
//! the voter's token account to the treasury's *incoming* KOTS token account,
//! and it PINS both the mint and the incoming owner to the hardcoded
//! `NetworkConfig::system_wallets` (`require_keys_eq!`). That pinning is a real
//! security control — it stops a spoofed mint or a redirected treasury. The
//! in-process harness mints a *random* mint, so the on-chain check correctly
//! rejects it: the paid rail is UNPROVABLE against a random mint by design.
//!
//! Making it pass locally would require either a feature-seam that ships a
//! DIFFERENT `.so` than production (proving the wrong binary — the exact trap
//! this project already hit once), or moving the expected mint into an on-chain
//! config account governed at the ceremony (a real design change, owner's call).
//! Neither is done here. Instead this proves the ACTUAL rail against the ACTUAL
//! config on the ACTUAL deployed binary — the only honest "money moves" proof.
//!
//! PREREQUISITES (operator-side — this is why it's `#[ignore]` + feature-gated):
//!   1. The current programs are deployed to devnet (built from THIS source, so
//!      the deployed `NetworkConfig::devnet()` matches the addresses below).
//!   2. A devnet keypair funded with SOL (fees) AND KOTS — its ATA for the real
//!      devnet KOTS mint holds at least one hieroglyph fee.
//!   3. The treasury incoming KOTS token account exists (its ATA by default).
//!
//! RUN:
//!   export KOTS_DEVNET_VOTER_KEYPAIR=/path/to/funded-voter.json
//!   # optional, if your deployed treasury uses a non-ATA incoming token account:
//!   # export KOTS_DEVNET_INCOMING_TOKEN_ACCOUNT=<addr>
//!   cargo test --features devnet-network test_hieroglyph_payment_devnet \
//!     -- --ignored --nocapture
//!
//! UNVERIFIED IN THE AUTHORING SANDBOX: there is no `cargo`/Anchor here, so this
//! has NOT been compiled. It is grounded in the harness's own call pattern
//! (`accounts.to_account_metas` + `instruction.data()` → `Instruction`), the
//! real `ExecuteHieroglyph` account list, `VOTE_TRACKER_SEED`, and the fee fn —
//! but the solana-sdk 2.1 import paths are the most likely thing to need a
//! nudge. Your `cargo build --features devnet-network` is the arbiter.

#![cfg(feature = "devnet-network")]

use anchor_lang::{InstructionData, ToAccountMetas};
use kots_common::{
    calculate_dynamic_hieroglyph_fee,
    constants::{BASE_KOTS_COST, DEFAULT_GAS_PRICE_LAMPORTS, TOKENS_PER_UNIT},
    network_config::NetworkConfig,
    state::VOTE_TRACKER_SEED,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    pubkey::Pubkey,
    signer::{keypair::read_keypair_file, Signer},
    system_program,
    transaction::Transaction,
};
use std::str::FromStr;
use voting_hieroglyph::ID as VOTING_HIEROGLYPH_ID;

fn env_pubkey(key: &str) -> Option<Pubkey> {
    std::env::var(key).ok().map(|s| Pubkey::from_str(&s).expect("valid pubkey in env"))
}

async fn token_amount(rpc: &RpcClient, account: &Pubkey, what: &str) -> u64 {
    rpc.get_token_account_balance(account)
        .await
        .unwrap_or_else(|e| panic!("{what} ({account}) must exist + be an SPL token account: {e}"))
        .amount
        .parse()
        .expect("token amount parses as u64")
}

#[tokio::test]
#[ignore = "live devnet + a funded KOTS wallet; run with --features devnet-network -- --ignored"]
async fn test_hieroglyph_payment_devnet() {
    // --- config (real devnet addresses come from NetworkConfig; env can override) ---
    let voter_path = std::env::var("KOTS_DEVNET_VOTER_KEYPAIR")
        .expect("set KOTS_DEVNET_VOTER_KEYPAIR to a funded devnet keypair that holds KOTS");
    let voter = read_keypair_file(&voter_path).expect("read the voter keypair file");

    let cfg = NetworkConfig::devnet();
    let kots_mint = env_pubkey("KOTS_DEVNET_KOTS_MINT").unwrap_or(cfg.system_wallets.kots_mint);
    let incoming_wallet =
        env_pubkey("KOTS_DEVNET_INCOMING_WALLET").unwrap_or(cfg.system_wallets.incoming);

    let rpc = RpcClient::new_with_commitment(cfg.rpc_url.to_string(), CommitmentConfig::confirmed());

    // --- token accounts ---
    let voter_ata =
        spl_associated_token_account::get_associated_token_address(&voter.pubkey(), &kots_mint);
    // the treasury incoming KOTS token account: its ATA by default, overridable
    // for a deployment whose incoming account is a non-ATA token account.
    let incoming_token_account = env_pubkey("KOTS_DEVNET_INCOMING_TOKEN_ACCOUNT").unwrap_or_else(
        || spl_associated_token_account::get_associated_token_address(&incoming_wallet, &kots_mint),
    );

    // --- the fee the program will charge (compute it the SAME way the program does) ---
    let hieroglyph_id: u32 = 5; // non-zero → a fee is charged (id 0 is free)
    let expected_fee =
        calculate_dynamic_hieroglyph_fee(hieroglyph_id as u8, BASE_KOTS_COST, DEFAULT_GAS_PRICE_LAMPORTS)
            .expect("fee calc");

    // --- pre-balances ---
    let voter_before = token_amount(&rpc, &voter_ata, "voter KOTS ATA").await;
    assert!(
        voter_before >= expected_fee,
        "voter needs >= {expected_fee} KOTS base units to pay the fee; holds {voter_before}"
    );
    let treasury_before = token_amount(&rpc, &incoming_token_account, "treasury incoming KOTS account").await;

    // --- build execute_hieroglyph(network = Devnet) ---
    let contest_label: [u8; 32] = *b"devnet_paid_vote_proof__________"; // 32 bytes; only keys the vote_tracker
    let target_meme_id: u64 = 1;
    let (vote_tracker, _bump) = Pubkey::find_program_address(
        &[
            VOTE_TRACKER_SEED,
            contest_label.as_ref(),
            &target_meme_id.to_le_bytes(),
            &hieroglyph_id.to_le_bytes(),
        ],
        &VOTING_HIEROGLYPH_ID,
    );

    let accounts = voting_hieroglyph::accounts::ExecuteHieroglyph {
        voter: voter.pubkey(),
        voter_token_account: voter_ata,
        incoming_token_account,
        kots_mint,
        vote_tracker,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    };
    let data = voting_hieroglyph::instruction::ExecuteHieroglyph {
        contest_label,
        target_meme_id,
        hieroglyph_id,
        network: Some(0u8), // 0 = Devnet — pins the config to the real devnet mint + treasury
    };
    let ix = Instruction {
        program_id: VOTING_HIEROGLYPH_ID,
        accounts: accounts.to_account_metas(None),
        data: data.data(),
    };

    let blockhash = rpc.get_latest_blockhash().await.expect("blockhash");
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&voter.pubkey()), &[&voter], blockhash);
    let sig = rpc
        .send_and_confirm_transaction(&tx)
        .await
        .expect("execute_hieroglyph should land on devnet (check the voter holds KOTS + the deployed config matches)");
    println!("execute_hieroglyph landed: {sig}");

    // --- the proof: the treasury received EXACTLY the calculated fee ---
    let treasury_after = token_amount(&rpc, &incoming_token_account, "treasury incoming KOTS account").await;
    let moved = treasury_after - treasury_before;
    assert_eq!(
        moved, expected_fee,
        "paid hieroglyph must move exactly the calculated KOTS fee to the treasury incoming account \
         (expected {expected_fee}, treasury moved {moved})"
    );

    println!(
        "✅ paid KOTS rail PROVEN on devnet: treasury incoming +{} base units ({} KOTS), sig {}",
        expected_fee,
        expected_fee / TOKENS_PER_UNIT,
        sig
    );
}
