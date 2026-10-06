// DESTINATION (where this file goes): kots-contracts/tests/emergency_and_blacklist.rs
// DESTINATION (where this file goes): kots-contracts/tests/emergency_and_blacklist.rs
// DESTINATION (where this file goes): kots-contracts/tests/emergency_and_blacklist.rs
//! Emergency pause/resume + blacklist tests — the on-chain operations behind
//! the security-governance sidecar service. Same discipline as
//! `owner_registry` / `security_regressions`: `allows_*` proves the happy path
//! works, `rejects_*` proves a guard actually bites (here, the 10-wallet cap).
//!
//! Register this module in `tests/lib.rs`:  pub mod emergency_and_blacklist;
//!
//! DEPENDENCY: the init and pause instructions CPI into treasury-operations to
//! reimburse account rent from `treasury_incoming`, so those accounts must be
//! treasury-operations-owned and funded. The shared harness pre-funds
//! `treasury_incoming`; `setup()` also runs `initialize_treasury_accounts`
//! best-effort so the accounts are program-owned. If a run fails on the
//! reimbursement CPI, that setup is the first place to look — it is the one part
//! that could not be exercised without a real ledger.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::*;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::{BanksClientError, *};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer as SdkSigner},
    transaction::Transaction,
};

use crate::devnet_idl_tests::test_utils::MultiProgramTestContext;
use security_governance::owner_registry::{MAX_OWNERS, OWNER_REGISTRY_SEED};
use security_governance::{BlacklistState, EmergencyState};

// ---------------------------------------------------------------------------
// PDA helpers — ground truth from the deployed program's seeds
// ---------------------------------------------------------------------------

fn program_state_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"program_state"], sg).0
}
fn blacklist_state_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"blacklist_v4", b"v4"], sg).0
}
fn emergency_state_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"emergency_state_v4", b"v4"], sg).0
}
fn cpi_authority_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"kots_cpi_authority"], sg).0
}
fn treasury_pdas(top: &Pubkey) -> (Pubkey, Pubkey, Pubkey) {
    (
        Pubkey::find_program_address(&[b"treasury_main_v4"], top).0,
        Pubkey::find_program_address(&[b"treasury_incoming_v4"], top).0,
        Pubkey::find_program_address(&[b"treasury_outgoing_v4"], top).0,
    )
}

/// The reimbursement queue PDA.
///
/// C10 (2026-07-28): this account is now created by initialize_treasury_accounts.
/// Before that nothing created it, so `reimburse_creator` and `emergency_funding`
/// failed AccountNotInitialized on every call. Adding the field to the Accounts
/// struct is a breaking change for every hand-written caller — including this
/// file, which stopped compiling with E0063 until this was added.
fn reimbursement_queue_pda(top: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"reimbursement_queue_v4"], top).0
}

/// A reason string as the on-chain fixed [u8; 64] (UTF-8, truncated + zero-padded).
fn reason(s: &str) -> [u8; 64] {
    let mut r = [0u8; 64];
    let b = s.as_bytes();
    let n = b.len().min(64);
    r[..n].copy_from_slice(&b[..n]);
    r
}

// ---------------------------------------------------------------------------
// owner-registry helpers (ACT-03 / F6) — pause/resume/blacklist now require a
// quorum over this registry, so the tests stand one up and sign with its owners.
// ---------------------------------------------------------------------------

fn registry_pda(sg: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[OWNER_REGISTRY_SEED], sg).0
}

fn owners_array(active: &[Pubkey]) -> [Pubkey; MAX_OWNERS] {
    let mut arr = [Pubkey::default(); MAX_OWNERS];
    for (i, k) in active.iter().enumerate() {
        arr[i] = *k;
    }
    arr
}

fn init_owner_registry_ix(
    sg: Pubkey,
    initializer: Pubkey,
    owners: [Pubkey; MAX_OWNERS],
    owner_count: u8,
    threshold: u8,
    break_glass: Pubkey,
) -> Instruction {
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::InitializeOwnerRegistry {
            owner_registry: registry_pda(&sg),
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

// ---------------------------------------------------------------------------
// instruction builders
// ---------------------------------------------------------------------------

fn init_treasury_ix(top: Pubkey, payer: Pubkey) -> Instruction {
    let (tmain, tin, tout) = treasury_pdas(&top);
    Instruction {
        program_id: top,
        accounts: treasury_operations::accounts::InitializeTreasuryAccounts {
            treasury_main: tmain,
            treasury_incoming: tin,
            treasury_outgoing: tout,
            reimbursement_queue: reimbursement_queue_pda(&top),
            payer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: treasury_operations::instruction::InitializeTreasuryAccounts {}.data(),
    }
}

fn init_sg_ix(sg: Pubkey, top: Pubkey, authority: Pubkey) -> Instruction {
    let (tmain, tin, tout) = treasury_pdas(&top);
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::InitializeSecurityGovernance {
            program_state: program_state_pda(&sg),
            authority,
            treasury_incoming: tin,
            treasury_main: tmain,
            treasury_outgoing: tout,
            system_program: solana_sdk::system_program::ID,
            treasury_operations_program: top,
            security_governance_program: sg,
            cpi_authority: cpi_authority_pda(&sg),
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeSecurityGovernance {}.data(),
    }
}

fn init_blacklist_ix(sg: Pubkey, top: Pubkey, authority: Pubkey) -> Instruction {
    let (tmain, tin, tout) = treasury_pdas(&top);
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::InitializeBlacklist {
            blacklist_state: blacklist_state_pda(&sg),
            initializer: authority,
            treasury_incoming: tin,
            treasury_main: tmain,
            treasury_outgoing: tout,
            system_program: solana_sdk::system_program::ID,
            treasury_operations_program: top,
            cpi_authority: cpi_authority_pda(&sg),
        }
        .to_account_metas(None),
        data: security_governance::instruction::InitializeBlacklist { authority }.data(),
    }
}

fn pause_ix(
    sg: Pubkey,
    top: Pubkey,
    authority: Pubkey,
    co_signers: &[Pubkey],
    why: &str,
    duration: i64,
) -> Instruction {
    let (tmain, tin, tout) = treasury_pdas(&top);
    let mut accounts = security_governance::accounts::EmergencyPause {
        emergency_state: emergency_state_pda(&sg),
        emergency_authority: authority,
        owner_registry: registry_pda(&sg),
        treasury_incoming: tin,
        treasury_main: tmain,
        treasury_outgoing: tout,
        system_program: solana_sdk::system_program::ID,
        treasury_operations_program: top,
        cpi_authority: cpi_authority_pda(&sg),
    }
    .to_account_metas(None);
    for signer in co_signers {
        accounts.push(AccountMeta::new_readonly(*signer, true));
    }
    Instruction {
        program_id: sg,
        accounts,
        data: security_governance::instruction::EmergencyPause {
            reason: reason(why),
            duration_seconds: duration,
        }
        .data(),
    }
}

// GovernedAction is the enum a quorum decision authorizes. The IDL prints it
// unqualified; if this path is wrong it is re-exported at the crate root instead
// — change this one line to `use security_governance::GovernedAction;`.
// `governed_action_hash` is the ballot's action commitment (2026-08-14):
// cast_vote now REQUIRES sha256(domain || borsh(action)), so every vote helper
// below computes it from the very action it opened — the honest path — and one
// rejects_ test proves a wrong hash dies with ActionHashMismatch.
use security_governance::quorum_decision::{governed_action_hash, DecisionError, GovernedAction};

/// Governance-decision PDA for `decision_id` (seed gov_decision_v1 + the id).
fn decision_pda(sg: &Pubkey, decision_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[b"gov_decision_v1", &decision_id.to_le_bytes()], sg).0
}

/// An owner's ballot PDA on a decision (seed gov_vote_v1 + decision + voter).
fn vote_pda(sg: &Pubkey, decision: &Pubkey, voter: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"gov_vote_v1", decision.as_ref(), voter.as_ref()], sg).0
}

fn open_decision_ix(sg: Pubkey, decision_id: u64, action: GovernedAction, proposer: Pubkey) -> Instruction {
    let decision = decision_pda(&sg, decision_id);
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::OpenDecision {
            owner_registry: registry_pda(&sg),
            decision,
            proposer_vote: vote_pda(&sg, &decision, &proposer),
            proposer,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: security_governance::instruction::OpenDecision { decision_id, action }.data(),
    }
}

fn cast_vote_ix(
    sg: Pubkey,
    decision_id: u64,
    approve: bool,
    voter: Pubkey,
    expected_action_hash: [u8; 32],
) -> Instruction {
    let decision = decision_pda(&sg, decision_id);
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::CastVote {
            owner_registry: registry_pda(&sg),
            decision,
            vote: vote_pda(&sg, &decision, &voter),
            voter,
            system_program: solana_sdk::system_program::ID,
        }
        .to_account_metas(None),
        data: security_governance::instruction::CastVote {
            decision_id,
            approve,
            expected_action_hash,
        }
        .data(),
    }
}

// The three governed actions are now EXECUTE-an-approved-decision instructions:
// they take the decision id + the decision PDA, and the authority is only the
// invoker/fee-payer (the decision is the authorization). The open->vote->execute
// sequencing lives in the flow helpers below.
fn resume_ix(sg: Pubkey, authority: Pubkey, decision_id: u64) -> Instruction {
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::EmergencyResume {
            emergency_state: emergency_state_pda(&sg),
            emergency_authority: authority,
            decision: decision_pda(&sg, decision_id),
        }
        .to_account_metas(None),
        data: security_governance::instruction::EmergencyResume { decision_id }.data(),
    }
}

fn add_blacklist_ix(sg: Pubkey, authority: Pubkey, decision_id: u64, wallet: Pubkey, why: &str) -> Instruction {
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::AddToBlacklist {
            blacklist_state: blacklist_state_pda(&sg),
            authority,
            decision: decision_pda(&sg, decision_id),
        }
        .to_account_metas(None),
        data: security_governance::instruction::AddToBlacklist {
            decision_id,
            wallet_to_blacklist: wallet,
            reason: reason(why),
        }
        .data(),
    }
}

fn remove_blacklist_ix(sg: Pubkey, authority: Pubkey, decision_id: u64, wallet: Pubkey) -> Instruction {
    Instruction {
        program_id: sg,
        accounts: security_governance::accounts::RemoveFromBlacklist {
            blacklist_state: blacklist_state_pda(&sg),
            authority,
            decision: decision_pda(&sg, decision_id),
        }
        .to_account_metas(None),
        data: security_governance::instruction::RemoveFromBlacklist {
            decision_id,
            wallet_to_remove: wallet,
        }
        .data(),
    }
}

// ---------------------------------------------------------------------------
// process + fetch helpers
// ---------------------------------------------------------------------------

/// Sign with the payer alone and submit — used for the setup instructions
/// (treasury / program / blacklist / registry init) that are not quorum-gated.
async fn process(
    ctx: &mut MultiProgramTestContext,
    ix: Instruction,
) -> std::result::Result<(), BanksClientError> {
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        ctx.recent_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Sign with the payer plus the given owner keypairs (the quorum) and submit —
/// used for the ACT-03-gated pause/resume/blacklist instructions. The owner
/// signatures are what `owner_registry.quorum_met` counts on chain.
async fn process_signed(
    ctx: &mut MultiProgramTestContext,
    ix: Instruction,
    owner_signers: &[&Keypair],
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    let mut signers: Vec<&Keypair> = vec![&ctx.payer];
    signers.extend_from_slice(owner_signers);
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&payer), &signers, ctx.recent_blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Drive a quorum decision end-to-end: `proposer` opens it (their open counts as
/// the first YES), each keypair in `extra_yes` casts an additional YES, then
/// `exec_ix` (the governed action) is submitted with the payer as invoking
/// authority. Open and the extra votes must succeed; the EXECUTE result is
/// returned so callers assert on it. `extra_yes = &[&owners.b]` reaches the
/// 2-of-3 threshold; `&[]` stays below it, so the execute is rejected — that is
/// how the "single owner cannot act" tests work under the decision model.
async fn open_vote_execute(
    ctx: &mut MultiProgramTestContext,
    sg: Pubkey,
    proposer: &Keypair,
    extra_yes: &[&Keypair],
    decision_id: u64,
    action: GovernedAction,
    exec_ix: Instruction,
) -> std::result::Result<(), BanksClientError> {
    // The honest commitment: hash the SAME action this decision was opened
    // with. Every vote below carries it, satisfying the on-chain require!.
    let action_hash = governed_action_hash(&action).expect("hashable action");
    process_signed(ctx, open_decision_ix(sg, decision_id, action, proposer.pubkey()), &[proposer])
        .await
        .expect("open_decision should succeed");
    for &voter in extra_yes {
        process_signed(
            ctx,
            cast_vote_ix(sg, decision_id, true, voter.pubkey(), action_hash),
            &[voter],
        )
        .await
        .expect("cast_vote should succeed");
    }
    process(ctx, exec_ix).await
}

/// Resume via a 2-of-3 decision. emergency_state must already exist and be paused
/// (emergency_pause creates it).
async fn resume_via_quorum(
    ctx: &mut MultiProgramTestContext,
    sg: Pubkey,
    owners: &Owners,
    decision_id: u64,
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    open_vote_execute(ctx, sg, &owners.a, &[&owners.b], decision_id,
        GovernedAction::EmergencyResume, resume_ix(sg, payer, decision_id)).await
}

async fn blacklist_add_via_quorum(
    ctx: &mut MultiProgramTestContext,
    sg: Pubkey,
    owners: &Owners,
    decision_id: u64,
    wallet: Pubkey,
    why: &str,
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    open_vote_execute(ctx, sg, &owners.a, &[&owners.b], decision_id,
        GovernedAction::BlacklistAdd { wallet }, add_blacklist_ix(sg, payer, decision_id, wallet, why)).await
}

async fn blacklist_remove_via_quorum(
    ctx: &mut MultiProgramTestContext,
    sg: Pubkey,
    owners: &Owners,
    decision_id: u64,
    wallet: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    open_vote_execute(ctx, sg, &owners.a, &[&owners.b], decision_id,
        GovernedAction::BlacklistRemove { wallet }, remove_blacklist_ix(sg, payer, decision_id, wallet)).await
}

/// A single owner opens a blacklist-add and tries to execute WITHOUT a second
/// YES — below the 2-of-3 threshold, so the execute must be rejected.
async fn blacklist_add_below_quorum(
    ctx: &mut MultiProgramTestContext,
    sg: Pubkey,
    owners: &Owners,
    decision_id: u64,
    wallet: Pubkey,
    why: &str,
) -> std::result::Result<(), BanksClientError> {
    let payer = ctx.payer.pubkey();
    open_vote_execute(ctx, sg, &owners.a, &[], decision_id,
        GovernedAction::BlacklistAdd { wallet }, add_blacklist_ix(sg, payer, decision_id, wallet, why)).await
}

async fn fetch_emergency(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> EmergencyState {
    let acc = ctx.banks_client.get_account(pda).await.unwrap().unwrap();
    EmergencyState::try_deserialize(&mut acc.data.as_ref()).unwrap()
}

async fn fetch_blacklist(ctx: &mut MultiProgramTestContext, pda: Pubkey) -> BlacklistState {
    let acc = ctx.banks_client.get_account(pda).await.unwrap().unwrap();
    BlacklistState::try_deserialize(&mut acc.data.as_ref()).unwrap()
}

fn contains(bl: &BlacklistState, wallet: &Pubkey) -> bool {
    bl.blacklisted_wallets.iter().any(|w| w == wallet)
}

/// The 2-of-3 owner set that pause/resume/blacklist now require a quorum from.
/// Kept whole so tests can sign with any pair; not every field is read.
#[allow(dead_code)]
struct Owners {
    a: Keypair,
    b: Keypair,
    c: Keypair,
    bg: Keypair,
}

/// Initialize treasury (best-effort — the harness may already have it),
/// program_state, blacklist_state, and — ACT-03 (F6) — a 2-of-3 owner registry.
/// Returns (security_governance, treasury_ops, owners).
async fn setup(ctx: &mut MultiProgramTestContext) -> (Pubkey, Pubkey, Owners) {
    let sg = ctx.network_config.security_governance_id;
    let top = ctx.network_config.treasury_operations_id;
    let payer = ctx.payer.pubkey();
    // Best-effort: ignore "already initialized" if the harness stood these up.
    let _ = process(ctx, init_treasury_ix(top, payer)).await;
    process(ctx, init_sg_ix(sg, top, payer))
        .await
        .expect("initialize_security_governance should succeed");
    process(ctx, init_blacklist_ix(sg, top, payer))
        .await
        .expect("initialize_blacklist should succeed");

    // ACT-03 (F6): stand up the owner registry the governed actions now gate on.
    let a = Keypair::new();
    let b = Keypair::new();
    let c = Keypair::new();
    let bg = Keypair::new();
    let owners = owners_array(&[a.pubkey(), b.pubkey(), c.pubkey()]);
    process(ctx, init_owner_registry_ix(sg, payer, owners, 3, 2, bg.pubkey()))
        .await
        .expect("initialize_owner_registry should succeed");

    // Fund the owners so they can pay upfront for PDA creation when an owner is
    // the instruction's payer — e.g. `emergency_pause` funds the EmergencyState
    // PDA from `emergency_authority` (reimbursed from the treasury afterward), so
    // the owner needs lamports on hand first. In production an owner holds SOL;
    // here we top them up. The tx fee is still paid by `ctx.payer`.
    for owner in [&a, &b, &c] {
        process(
            ctx,
            solana_sdk::system_instruction::transfer(&payer, &owner.pubkey(), 1_000_000_000),
        )
        .await
        .expect("funding an owner should succeed");
    }

    (sg, top, Owners { a, b, c, bg })
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_initialize_program_state_and_blacklist() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, _owners) = setup(&mut ctx).await;

    let bl = fetch_blacklist(&mut ctx, blacklist_state_pda(&sg)).await;
    assert!(bl.is_initialized, "blacklist_state must be initialized");
    assert_eq!(bl.blacklist_count, 0, "fresh blacklist starts empty");
}

// ---------------------------------------------------------------------------
// emergency pause / resume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_emergency_pause_then_resume() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, top, owners) = setup(&mut ctx).await;

    // Pause under a 2-of-3 quorum: owner a (authority) + owner b (co-signer).
    process_signed(
        &mut ctx,
        pause_ix(sg, top, owners.a.pubkey(), &[owners.b.pubkey()], "incident-42", 0),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("emergency_pause should succeed under quorum");
    let paused = fetch_emergency(&mut ctx, emergency_state_pda(&sg)).await;
    assert!(paused.is_initialized, "pause creates emergency_state");
    assert!(paused.is_paused, "pause must set is_paused = true");

    resume_via_quorum(&mut ctx, sg, &owners, 1)
        .await
        .expect("emergency_resume should succeed under a 2-of-3 decision");
    let resumed = fetch_emergency(&mut ctx, emergency_state_pda(&sg)).await;
    assert!(!resumed.is_paused, "resume must clear is_paused");
}

#[tokio::test]
async fn rejects_resume_before_any_pause() {
    // emergency_state does not exist until the first pause, so resume must fail.
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, owners) = setup(&mut ctx).await;

    // Even with a full quorum, resume must fail when nothing has been paused.
    let res = resume_via_quorum(&mut ctx, sg, &owners, 1).await;
    assert!(res.is_err(), "resume with no emergency_state must be rejected");
}

// ---------------------------------------------------------------------------
// blacklist add / remove
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_blacklist_add_and_remove() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, owners) = setup(&mut ctx).await;
    let victim = Keypair::new().pubkey();

    blacklist_add_via_quorum(&mut ctx, sg, &owners, 1, victim, "spam")
        .await
        .expect("add_to_blacklist should succeed under a 2-of-3 decision");
    let after_add = fetch_blacklist(&mut ctx, blacklist_state_pda(&sg)).await;
    assert!(contains(&after_add, &victim), "wallet must be present after add");
    assert_eq!(after_add.blacklist_count, 1);

    blacklist_remove_via_quorum(&mut ctx, sg, &owners, 2, victim)
        .await
        .expect("remove_from_blacklist should succeed under a 2-of-3 decision");
    let after_remove = fetch_blacklist(&mut ctx, blacklist_state_pda(&sg)).await;
    assert!(!contains(&after_remove, &victim), "wallet must be gone after remove");
}

// ---------------------------------------------------------------------------
// the 10-wallet cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rejects_blacklist_beyond_capacity() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, owners) = setup(&mut ctx).await;

    // Fill all ten slots — each must succeed (under quorum).
    for i in 0..10u8 {
        let w = Keypair::new().pubkey();
        blacklist_add_via_quorum(&mut ctx, sg, &owners, (i as u64) + 1, w, "fill")
            .await
            .unwrap_or_else(|e| panic!("slot {i} should fill, got {e:?}"));
    }
    let full = fetch_blacklist(&mut ctx, blacklist_state_pda(&sg)).await;
    assert_eq!(full.blacklist_count, 10, "all ten slots filled");

    // The eleventh must be rejected by the on-chain cap.
    let overflow = Keypair::new().pubkey();
    let res = blacklist_add_via_quorum(&mut ctx, sg, &owners, 11, overflow, "overflow").await;
    assert!(res.is_err(), "the 11th blacklist entry must be rejected (cap is 10)");
}

// ---------------------------------------------------------------------------
// ACT-03 (F6) + A.10: quorum still gates the DANGEROUS directions — resume and
// blacklist — but pause is deliberately expedient (any single owner). The
// asymmetry: a lone owner may PAUSE, but not RESUME or edit the BLACKLIST.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allows_pause_by_a_single_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, top, owners) = setup(&mut ctx).await;

    // A.10 (expedient pause): one owner, no co-signer, must succeed — fast
    // incident response does not wait to coordinate a quorum.
    process_signed(
        &mut ctx,
        pause_ix(sg, top, owners.a.pubkey(), &[], "solo", 0),
        &[&owners.a],
    )
    .await
    .expect("a single owner must be able to pause (A.10)");
    let paused = fetch_emergency(&mut ctx, emergency_state_pda(&sg)).await;
    assert!(paused.is_paused, "the solo pause must take effect");
}

#[tokio::test]
async fn rejects_pause_by_a_non_owner() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, top, _owners) = setup(&mut ctx).await;

    // A non-owner signer is refused (NotAnOwner) even for the expedient pause —
    // and refused before any side effect, since the check precedes creation.
    let stranger = Keypair::new();
    let res = process_signed(
        &mut ctx,
        pause_ix(sg, top, stranger.pubkey(), &[], "intruder", 0),
        &[&stranger],
    )
    .await;
    assert!(res.is_err(), "a non-owner must not be able to pause");
}

#[tokio::test]
async fn rejects_blacklist_without_quorum() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, owners) = setup(&mut ctx).await;
    let victim = Keypair::new().pubkey();

    // Owner a signs alone — below threshold. The blacklist must not change.
    let res = blacklist_add_below_quorum(&mut ctx, sg, &owners, 1, victim, "solo").await;
    assert!(res.is_err(), "a single owner must not be able to blacklist (F6: quorum required)");
}

// ---------------------------------------------------------------------------
// ACTION COMMITMENT (2026-08-14): a ballot must match the decision's stored
// action. This is the on-chain leg of the what-you-see-is-what-you-sign fix —
// a UI that labels a BlacklistAdd decision "Resume" builds a vote whose hash
// disagrees with the account, and the chain refuses it BEFORE any tally moves.
// ---------------------------------------------------------------------------

/// DecisionError::ActionHashMismatch as the chain reports it: Anchor offsets
/// `#[error_code]` discriminants by 6000, so 12108 surfaces as Custom(18108)
/// — same shift the IDL shows for 12100..=12107 → 18100..=18107.
const ACTION_HASH_MISMATCH: u32 = DecisionError::ActionHashMismatch as u32 + 6000;

fn is_action_hash_mismatch(res: &std::result::Result<(), BanksClientError>) -> bool {
    use solana_sdk::instruction::InstructionError;
    use solana_sdk::transaction::TransactionError;
    matches!(
        res,
        Err(BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(c),
        ))) if *c == ACTION_HASH_MISMATCH
    )
}

#[tokio::test]
async fn rejects_vote_with_mismatched_action_hash() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();
    let (sg, _top, owners) = setup(&mut ctx).await;

    // Owner a opens an EmergencyResume decision (their YES #1)...
    process_signed(
        &mut ctx,
        open_decision_ix(sg, 41, GovernedAction::EmergencyResume, owners.a.pubkey()),
        &[&owners.a],
    )
    .await
    .expect("open_decision should succeed");

    // ...and owner b's wallet is fed a DOCTORED hash (as a lying UI would).
    // The vote must die with ActionHashMismatch, and the tally must not move.
    let res = process_signed(
        &mut ctx,
        cast_vote_ix(sg, 41, true, owners.b.pubkey(), [0xAB; 32]),
        &[&owners.b],
    )
    .await;
    assert!(
        is_action_hash_mismatch(&res),
        "a wrong action hash must be rejected with ActionHashMismatch (18108), got {res:?}"
    );

    // The honest hash for the SAME decision still works: the guard rejects
    // lies, not votes.
    let honest = governed_action_hash(&GovernedAction::EmergencyResume).unwrap();
    process_signed(
        &mut ctx,
        cast_vote_ix(sg, 41, true, owners.b.pubkey(), honest),
        &[&owners.b],
    )
    .await
    .expect("the honest hash must be accepted");
}

// ===========================================================================
// PAUSE ENFORCEMENT — the on-chain counterpart to the off-chain filter
// ===========================================================================
//
// The off-chain PlatformPauseFilter (kots-core) refuses gated writes over HTTP
// while paused. These tests prove the CONTRACTS refuse the value-moving
// instructions too, so a caller who bypasses the API and submits straight to
// the chain is still stopped. The gate sits at the TOP of each gated handler
// and reverts with KotsError::EmergencyPauseActive (11000, errors.rs:120) BEFORE
// any funds move.
//
// `assert_rejected_with(res, 11000)` is deliberately strict: if the op instead
// dies at 3012 (AccountNotInitialized) the setup was incomplete and the test
// FAILS LOUDLY rather than passing for the wrong reason — a real risk here,
// because the gate only runs once Anchor account validation has already passed.
//
// This exercises award_jackpot (progressive-jackpot). distribute_from_treasury
// (treasury-operations) and the vote paths (voting-hieroglyph) carry the SAME
// gate; enforcing them is the identical three-line leg (pause -> attempt ->
// assert_rejected_with(11000)) bolted onto their existing happy-path setups in
// treasury_movement.rs and the vote tests.

/// KotsError::EmergencyPauseActive — the shared code every gated program returns
/// while the platform is paused (kots-common errors.rs:120).
const EMERGENCY_PAUSE_ACTIVE: u32 = 11000;

/// True iff `res` is a rejection carrying the pause error. Used on the un-paused
/// legs, where the op must get PAST the gate: it may still fail downstream (e.g.
/// the hooked transfer), but it must NOT be the pause error.
fn is_pause_error(res: &std::result::Result<(), BanksClientError>) -> bool {
    use solana_sdk::instruction::InstructionError;
    use solana_sdk::transaction::TransactionError;
    matches!(
        res,
        Err(BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(c),
        ))) if *c == EMERGENCY_PAUSE_ACTIVE
    )
}

/// Build `progressive_jackpot::award_jackpot` with the canonical EmergencyState
/// PDA threaded in. Takes plain Copy pubkeys so the caller can rebuild it across
/// `&mut ctx` submissions without holding a borrow of the context.
#[allow(clippy::too_many_arguments)]
fn award_jackpot_ix(
    jackpot_program: Pubkey,
    jackpot_state: Pubkey,
    jackpot_program_state: Pubkey,
    jackpot_vault: Pubkey,
    mint: Pubkey,
    winner_account: Pubkey,
    authority: Pubkey,
    winner: Pubkey,
    emergency_state: Pubkey,
) -> Instruction {
    Instruction {
        program_id: jackpot_program,
        accounts: progressive_jackpot::accounts::AwardJackpot {
            jackpot_state,
            kots_mint: mint,
            token_program: spl_token::ID,
            winner_account,
            jackpot_vault,
            program_state: jackpot_program_state,
            authority,
            emergency_state,
        }
        .to_account_metas(None),
        data: progressive_jackpot::instruction::AwardJackpot {
            winner,
            contest_label: [0u8; 32],
        }
        .data(),
    }
}

/// award_jackpot — a real KOTS outflow — must be REJECTED at the pause gate
/// while paused, and reachable (past the gate) otherwise.
#[tokio::test]
async fn blocks_award_jackpot_while_paused() {
    let mut ctx = MultiProgramTestContext::new().await.unwrap();

    // Emergency machinery: security-governance, treasury, blacklist, and the
    // 2-of-3 owner registry that pause/resume gate on.
    let (sg, top, owners) = setup(&mut ctx).await;

    // An initialised, funded jackpot so award_jackpot reaches its gate instead
    // of dying at 3012 (Anchor validates jackpot_state/program_state before the
    // handler runs). world_with_jackpot re-inits treasury/security best-effort,
    // which is a no-op after setup().
    // Hoist the mint pubkey out first: passing ctx.kots_mint.pubkey() inline while
    // &mut ctx is also an argument is a borrow conflict (E0502).
    let mint = ctx.kots_mint.pubkey();
    let w = crate::bootstrap::world_with_jackpot(&mut ctx, mint, 1_000_000_000).await;

    let winner = Keypair::new();
    let winner_ata = ctx
        .create_token_account_for(winner.pubkey(), 0)
        .await
        .unwrap();

    // Copy pubkeys so the closure captures no borrow of ctx.
    let jackpot_program = w.jackpot;
    let jackpot_state = w.jackpot_state;
    let jackpot_program_state = w.jackpot_program_state;
    let jackpot_vault = w.jackpot_vault;
    let authority = ctx.payer.pubkey(); // world_with_jackpot inits authority = payer
    let winner_pk = winner.pubkey();
    let es = emergency_state_pda(&sg);
    let mk = || {
        award_jackpot_ix(
            jackpot_program,
            jackpot_state,
            jackpot_program_state,
            jackpot_vault,
            mint,
            winner_ata,
            authority,
            winner_pk,
            es,
        )
    };

    // (1) NOT paused: emergency_state is absent, so under the reader's
    //     "absent => not paused" rule the gate is clear and the op gets PAST it.
    let before = process(&mut ctx, mk()).await;
    assert!(
        !is_pause_error(&before),
        "award_jackpot hit the pause gate BEFORE any pause — the reader is still \
         failing closed on an absent emergency_state (got {before:?})"
    );

    // (2) PAUSE under a 2-of-3 quorum; the SAME op must revert AT the gate with
    //     EmergencyPauseActive — not 3012, not a downstream error.
    process_signed(
        &mut ctx,
        pause_ix(sg, top, owners.a.pubkey(), &[owners.b.pubkey()], "pause-enforcement", 0),
        &[&owners.a, &owners.b],
    )
    .await
    .expect("emergency_pause should succeed under quorum");
    let paused = fetch_emergency(&mut ctx, emergency_state_pda(&sg)).await;
    assert!(paused.is_paused, "precondition: the platform is paused");

    // DISTINCT BLOCKHASH (2026-08-13): `mk()` rebuilds a byte-identical
    // award_jackpot ix and `bootstrap::process` reuses `ctx.recent_blockhash`,
    // so without this the paused submission is signature-identical to the
    // pre-pause `before` tx — banks_client returns the CACHED pre-pause result
    // (NoJackpotAmount, 9005) WITHOUT re-executing, so the award never runs
    // under the pause and the gate never fires. Force a fresh blockhash so this
    // is a distinct tx that actually executes while paused. (Same duplicate-
    // signature trap `test_utils.rs` guards in `resume_operations`.)
    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let during = process(&mut ctx, mk()).await;
    crate::bootstrap::assert_rejected_with(during, EMERGENCY_PAUSE_ACTIVE);

    // (3) RESUME; the gate clears again (emergency_state now exists with
    //     is_paused = false), so the op must NOT hit the pause error.
    resume_via_quorum(&mut ctx, sg, &owners, 1)
        .await
        .expect("emergency_resume should succeed under a 2-of-3 decision");
    // Fresh blockhash again — same reason: the post-resume award must be a
    // distinct tx from the `before`/`during` submissions or it returns a cached
    // result instead of re-executing under the now-cleared pause.
    ctx.recent_blockhash = ctx
        .banks_client
        .get_new_latest_blockhash(&ctx.recent_blockhash)
        .await
        .unwrap();
    let after = process(&mut ctx, mk()).await;
    assert!(
        !is_pause_error(&after),
        "award_jackpot still blocked by the pause gate AFTER resume — resume did \
         not clear is_paused (got {after:?})"
    );
}
