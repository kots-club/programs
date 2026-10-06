// kots-contracts/tests/whitelist_quorum_tests.rs
//! ACT-03 (F6) — transfer-hook whitelist edits (`add_to_whitelist` /
//! `remove_from_whitelist`) now require an owner-registry QUORUM, replacing the
//! single stored `authority`. This is the paired positive/negative proof the
//! plan's ACT-20 rule requires before the gate flips to `[x]`:
//!
//!   * `allows_*`  — a 2-of-3 quorum can add and remove.
//!   * `rejects_*` — a lone owner is refused in BOTH directions (the F6 proof),
//!                   and a foreign account passed as `owner_registry` is refused
//!                   by Anchor's cross-program owner + `seeds::program` check.
//!   * `preserves_*` — the pre-existing `AlreadyWhitelisted` guard still bites,
//!                     confirming the gate did not disturb the handler's body.
//!
//! Register this module in `tests/lib.rs`:  pub mod whitelist_quorum_tests;
//!
//! HARNESS: like `money_movement.rs`, and DELIBERATELY NOT
//! `MultiProgramTestContext` (which does not load `transfer_hook`), this stands
//! up a dedicated `ProgramTest` with exactly the two programs the gate spans —
//! `transfer_hook` (the edits) and `security_governance` (the owner registry the
//! edits read across programs). Both `initialize_whitelist` and
//! `initialize_owner_registry` are CPI-free, so no treasury pre-funding is
//! needed here. Requires the compiled `.so` files, same as the other on-chain
//! suites:
//!     export BPF_OUT_DIR=$PWD/target/deploy

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::{BanksClient, BanksClientError, ProgramTest};
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer as SdkSigner},
    transaction::Transaction,
};

use security_governance::owner_registry::{MAX_OWNERS, OWNER_REGISTRY_SEED};
use transfer_hook as hook;

// ---------------------------------------------------------------------------
// PDAs — ground truth from the deployed programs' seeds
// ---------------------------------------------------------------------------

fn whitelist_pda() -> Pubkey {
    Pubkey::find_program_address(&[b"whitelist"], &hook::ID).0
}

/// The owner registry lives under the SECURITY-GOVERNANCE program (the whole
/// point of the cross-program gate), so it derives from that program id.
fn registry_pda() -> Pubkey {
    Pubkey::find_program_address(&[OWNER_REGISTRY_SEED], &security_governance::ID).0
}

fn owners_array(active: &[Pubkey]) -> [Pubkey; MAX_OWNERS] {
    let mut arr = [Pubkey::default(); MAX_OWNERS];
    for (i, k) in active.iter().enumerate() {
        arr[i] = *k;
    }
    arr
}

// ---------------------------------------------------------------------------
// instruction builders
// ---------------------------------------------------------------------------

fn init_owner_registry_ix(
    initializer: Pubkey,
    owners: [Pubkey; MAX_OWNERS],
    owner_count: u8,
    threshold: u8,
    break_glass: Pubkey,
) -> Instruction {
    Instruction {
        program_id: security_governance::ID,
        accounts: security_governance::accounts::InitializeOwnerRegistry {
            owner_registry: registry_pda(),
            initializer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeOwnerRegistry {
            owners,
            owner_count,
            threshold,
            break_glass,
        }
        .data(),
    }
}

fn init_whitelist_ix(authority: Pubkey, wallets: Vec<Pubkey>) -> Instruction {
    Instruction {
        program_id: hook::ID,
        accounts: hook::accounts::InitializeWhitelist {
            authority,
            whitelist_registry: whitelist_pda(),
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: hook::instruction::InitializeWhitelist { wallets }.data(),
    }
}

/// `owner_registry` is a PARAMETER (not hard-wired to `registry_pda()`) so a
/// test can pass a wrong account and prove the cross-program constraint bites.
fn add_whitelist_ix(
    authority: Pubkey,
    owner_registry: Pubkey,
    co_signers: &[Pubkey],
    wallet: Pubkey,
) -> Instruction {
    let mut accounts = hook::accounts::UpdateWhitelist {
        authority,
        whitelist_registry: whitelist_pda(),
        owner_registry,
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id: hook::ID,
        accounts,
        data: hook::instruction::AddToWhitelist { wallet }.data(),
    }
}

fn remove_whitelist_ix(
    authority: Pubkey,
    owner_registry: Pubkey,
    co_signers: &[Pubkey],
    wallet: Pubkey,
) -> Instruction {
    let mut accounts = hook::accounts::UpdateWhitelist {
        authority,
        whitelist_registry: whitelist_pda(),
        owner_registry,
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id: hook::ID,
        accounts,
        data: hook::instruction::RemoveFromWhitelist { wallet }.data(),
    }
}

// ---------------------------------------------------------------------------
// env + process/fetch helpers
// ---------------------------------------------------------------------------

/// The 2-of-3 owner set the whitelist edits now require a quorum from. Returned
/// SEPARATELY from `Env` so a test can hold `&mut env` and `&owners.a` at once.
#[allow(dead_code)]
struct Owners {
    a: Keypair,
    b: Keypair,
    c: Keypair,
    bg: Keypair,
}

/// The harness handle. `bh` is a single recent blockhash reused across the
/// handful of txs per test (same approach as `emergency_and_blacklist.rs`); the
/// one place two txs would otherwise collide — adding the same wallet twice — is
/// handled by signing the second with a different owner, giving it a distinct
/// signature.
struct Env {
    banks: BanksClient,
    bh: Hash,
    payer: Keypair,
}

/// Sign with the payer alone — used for the CPI-free setup instructions.
async fn process(env: &mut Env, ix: Instruction) -> std::result::Result<(), BanksClientError> {
    let payer_pk = env.payer.pubkey();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer_pk), &[&env.payer], env.bh);
    env.banks.process_transaction(tx).await
}

/// Sign with the payer PLUS the given owner keypairs — the owner signatures are
/// what `owner_registry.quorum_met` counts on chain.
async fn process_signed(
    env: &mut Env,
    ix: Instruction,
    owner_signers: &[&Keypair],
) -> std::result::Result<(), BanksClientError> {
    let payer_pk = env.payer.pubkey();
    let mut signers: Vec<&Keypair> = vec![&env.payer];
    signers.extend_from_slice(owner_signers);
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer_pk), &signers, env.bh);
    env.banks.process_transaction(tx).await
}

async fn fetch_whitelist(env: &mut Env) -> hook::WhitelistRegistry {
    let acc = env.banks.get_account(whitelist_pda()).await.unwrap().unwrap();
    hook::WhitelistRegistry::try_deserialize(&mut acc.data.as_ref()).unwrap()
}

fn contains(wl: &hook::WhitelistRegistry, wallet: &Pubkey) -> bool {
    wl.wallets.iter().any(|w| w == wallet)
}

/// Dedicated harness: `transfer_hook` + `security_governance` only. Stands up a
/// 2-of-3 owner registry and an empty whitelist (authority = payer), funds the
/// owners so they can serve as the writable `authority` signer, and returns the
/// ready `Env` and the `Owners`.
async fn setup() -> (Env, Owners) {
    let mut pt = ProgramTest::default();
    pt.add_program("transfer_hook", hook::ID, None);
    pt.add_program("security_governance", security_governance::ID, None);
    let (mut banks, payer, bh) = pt.start().await;

    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    let bg = Keypair::new();
    let owners = owners_array(&[a.pubkey(), b.pubkey(), c.pubkey()]);

    // Owner registry (owned by security-governance) — the quorum source.
    let ix = init_owner_registry_ix(payer.pubkey(), owners, 3, 2, bg.pubkey());
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    banks
        .process_transaction(tx)
        .await
        .expect("initialize_owner_registry should succeed");

    // Whitelist registry (owned by transfer-hook), authority = payer, empty.
    let ix = init_whitelist_ix(payer.pubkey(), vec![]);
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    banks
        .process_transaction(tx)
        .await
        .expect("initialize_whitelist should succeed");

    // Fund the owners so a fresh owner key can be the writable `authority`.
    for owner in [&a, &b, &c] {
        let ix = solana_sdk::system_instruction::transfer(
            &payer.pubkey(),
            &owner.pubkey(),
            1_000_000_000,
        );
        let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
        banks
            .process_transaction(tx)
            .await
            .expect("funding an owner should succeed");
    }

    (Env { banks, bh, payer }, Owners { a, b, c, bg })
}

// ---------------------------------------------------------------------------
// positive: a quorum can add and remove
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_add_and_remove_under_quorum() {
    let (mut env, owners) = setup().await;
    let wallet = Keypair::new().pubkey();

    // Add under a 2-of-3 quorum: owner a (authority) + owner b (co-signer).
    process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), registry_pda(), &[owners.b.pubkey()], wallet),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("add_to_whitelist should succeed under quorum");
    let after_add = fetch_whitelist(&mut env).await;
    assert!(contains(&after_add, &wallet), "wallet must be present after add");
    assert_eq!(after_add.wallets.len(), 1, "exactly one wallet whitelisted");

    // Remove under the same quorum.
    process_signed(
        &mut env,
        remove_whitelist_ix(owners.a.pubkey(), registry_pda(), &[owners.b.pubkey()], wallet),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("remove_from_whitelist should succeed under quorum");
    let after_remove = fetch_whitelist(&mut env).await;
    assert!(!contains(&after_remove, &wallet), "wallet must be gone after remove");
    assert_eq!(after_remove.wallets.len(), 0, "whitelist empty again");
}

// ---------------------------------------------------------------------------
// negative (the F6 proof): a lone owner is refused, in BOTH directions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_add_without_quorum() {
    let (mut env, owners) = setup().await;
    let wallet = Keypair::new().pubkey();

    // Owner a signs alone — below the 2-of-3 threshold. The edit must fail
    // (OwnerRegistryError::QuorumNotMet) and the whitelist must not change.
    let res = process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), registry_pda(), &[], wallet),
        &[&owners.a],
    )
    .await;
    assert!(res.is_err(), "a single owner must not be able to add (F6: quorum required)");
    assert_eq!(fetch_whitelist(&mut env).await.wallets.len(), 0, "no change on a rejected add");
}

#[tokio::test]
async fn rejects_remove_without_quorum() {
    let (mut env, owners) = setup().await;
    let wallet = Keypair::new().pubkey();

    // Seed one entry legitimately (under quorum) so there is something to remove.
    process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), registry_pda(), &[owners.b.pubkey()], wallet),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("seeding add should succeed under quorum");

    // Now a lone owner tries to remove it — must fail; the entry must remain.
    let res = process_signed(
        &mut env,
        remove_whitelist_ix(owners.a.pubkey(), registry_pda(), &[], wallet),
        &[&owners.a],
    )
    .await;
    assert!(res.is_err(), "a single owner must not be able to remove (F6: quorum required)");
    assert!(contains(&fetch_whitelist(&mut env).await, &wallet), "entry must survive a rejected remove");
}

// ---------------------------------------------------------------------------
// cross-program defense: a foreign account passed as `owner_registry` is refused
// by Anchor's owner + seeds::program check, EVEN with a full quorum of signers.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_wrong_owner_registry_account() {
    let (mut env, owners) = setup().await;
    let wallet = Keypair::new().pubkey();

    // The registry seed derived under the WRONG program (transfer-hook instead of
    // security-governance) — a different address that fails the `seeds::program`
    // pin before quorum is ever consulted.
    let wrong_registry = Pubkey::find_program_address(&[OWNER_REGISTRY_SEED], &hook::ID).0;

    let res = process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), wrong_registry, &[owners.b.pubkey()], wallet),
        &[&owners.a, &owners.b],
    )
    .await;
    assert!(res.is_err(), "a foreign owner_registry account must be rejected by the cross-program constraint");
    assert_eq!(fetch_whitelist(&mut env).await.wallets.len(), 0, "no change when the registry account is wrong");
}

// ---------------------------------------------------------------------------
// untouched behavior: the pre-existing duplicate guard still bites under quorum.
// The second add is signed by a DIFFERENT owner (c, not b) so it is a distinct
// transaction — the rejection must come from AlreadyWhitelisted, not tx dedup.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preserves_duplicate_guard_under_quorum() {
    let (mut env, owners) = setup().await;
    let wallet = Keypair::new().pubkey();

    process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), registry_pda(), &[owners.b.pubkey()], wallet),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("first add should succeed under quorum");

    // Same wallet, valid quorum (a + c) — the handler's own AlreadyWhitelisted
    // guard must still reject it, unaffected by the new gate.
    let res = process_signed(
        &mut env,
        add_whitelist_ix(owners.a.pubkey(), registry_pda(), &[owners.c.pubkey()], wallet),
        &[&owners.a, &owners.c],
    )
    .await;
    assert!(res.is_err(), "re-adding a whitelisted wallet must still be rejected (AlreadyWhitelisted)");
    assert_eq!(fetch_whitelist(&mut env).await.wallets.len(), 1, "the duplicate must not have been appended");
}
