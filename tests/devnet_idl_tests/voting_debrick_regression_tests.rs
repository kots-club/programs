//! Voting de-brick regression guard (2026-08-03)
//!
//! `validate_voter_security` used to run `require!(Pubkey::default() != Pubkey::default(), …)`
//! — an always-false check — so `batch_cast_hieroglyph_votes` reverted for EVERY caller with
//! `BlacklistedWallet`, regardless of who voted. The dedicated `voting_hieroglyph_tests`
//! module that would normally cover this is PARKED (it references ~40 harness methods that do
//! not exist yet — see `mod.rs`), so the fix shipped with no active coverage.
//!
//! This module is the minimal replacement guard. It deliberately uses ONLY harness methods
//! that exist today — `create_test_contest`, `fund_lamports`, `batch_cast_votes` — and asserts
//! that a funded, non-blacklisted voter's batch SUCCEEDS. If the brick is ever reintroduced (or
//! the upstream blacklist bootstrap in `batch_cast_votes` breaks), this test fails.
//!
//! IMPORTANT — authored WITHOUT a Rust toolchain in the authoring environment. Every symbol
//! used here was verified to exist in `test_utils`, but this file has NOT been compiled or run.
//! Build it (`cargo test -p kots-comprehensive-tests --features legacy-suite -- \
//! voting_debrick_regression`) before relying on it; if it does not compile, the fix in
//! `programs/voting-hieroglyph/src/lib.rs` is unaffected — this is test scaffolding only.
//!
//! Harness note surfaced while writing this: the harness pays AND signs `batch_cast_votes` with
//! `test_user`, a fresh `Keypair::new()` that `new()` never funds, and no active test has ever
//! exercised `batch_cast_votes`. That is why the funding step below is required. Reviving the
//! full parked `voting_hieroglyph_tests` will likely want `test_user` funded once in `new()`.

use super::test_utils::*;
use solana_sdk::signer::Signer; // for `.pubkey()` on the test_user keypair

#[cfg(test)]
mod voting_debrick_regression {
    use super::*;

    /// Build a self-contained 32-byte contest label without depending on a
    /// helper that lives in another (possibly parked) module.
    fn debrick_contest_label() -> [u8; 32] {
        let mut label = [0u8; 32];
        let tag = b"voting_debrick_v1";
        label[..tag.len()].copy_from_slice(tag);
        label
    }

    #[tokio::test]
    async fn batch_voting_succeeds_for_clean_voter_debrick_guard() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();

        let contest_label = debrick_contest_label();

        // 24h contest, min 2 participants, no entry fee — same shape the parked
        // `test_batch_voting_functionality` used.
        ctx.create_test_contest(contest_label, 86_400, 2, 0)
            .await
            .expect("create_test_contest failed (harness setup, not the path under test)");

        // The harness pays AND signs `batch_cast_votes` with `test_user`, a fresh
        // zero-lamport keypair. Fund it so the batch cannot fail on fees/rent
        // instead of on the logic this test guards. `Pubkey` is `Copy`, so bind
        // it first to avoid borrowing `ctx` immutably and mutably at once.
        let voter = ctx.test_user.pubkey();
        ctx.fund_lamports(&voter, 100_000_000)
            .await
            .expect("funding test_user failed (harness setup, not the path under test)");

        // A small mixed batch of (meme_id, interaction_type).
        let votes: Vec<(u64, u32)> = vec![
            (generate_test_meme_id(1), 0), // upvote
            (generate_test_meme_id(1), 1), // comment
            (generate_test_meme_id(2), 5), // love
        ];

        // 2026-08-05: the vote gate reads blacklist_state; bootstrap it so the
        // guard tests the de-brick, not a missing account (the old 3012).
        ctx.ensure_blacklist_initialized().await.unwrap();

        // 2026-08-05: BatchCastHieroglyphVotes binds interaction_batch as `mut`
        // (NOT init) — the batch PDA must exist first. Batch 0, voter-funded,
        // per InitializeInteractionBatch (voting-hieroglyph:617).
        {
            let (batch_pda, _) = solana_sdk::pubkey::Pubkey::find_program_address(
                &[b"interaction_batch" as &[u8], &contest_label, &0u32.to_le_bytes()],
                &ctx.network_config.voting_hieroglyph_id,
            );
            if ctx.banks_client.get_account(batch_pda).await.unwrap().is_none() {
                // Plain latest blockhash: this instruction is unique per test,
                // so the duplicate-message trap the ext-trait refresh guards
                // against cannot occur (and the trait isn't imported here).
                ctx.recent_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
                let mut data = Vec::new();
                data.extend_from_slice(
                    &solana_sdk::hash::hash(b"global:initialize_interaction_batch")
                        .to_bytes()[..8],
                );
                data.extend_from_slice(&contest_label);
                data.extend_from_slice(&0u32.to_le_bytes());
                let ix = solana_sdk::instruction::Instruction {
                    program_id: ctx.network_config.voting_hieroglyph_id,
                    accounts: vec![
                        solana_sdk::instruction::AccountMeta::new(batch_pda, false),
                        solana_sdk::instruction::AccountMeta::new(ctx.test_user.pubkey(), true),
                        solana_sdk::instruction::AccountMeta::new_readonly(solana_sdk::system_program::id(), false),
                    ],
                    data,
                };
                let tx = solana_sdk::transaction::Transaction::new_signed_with_payer(
                    &[ix],
                    Some(&ctx.test_user.pubkey()),
                    &[&ctx.test_user],
                    ctx.recent_blockhash,
                );
                ctx.banks_client.process_transaction(tx).await.unwrap();
            }
        }
        let result = ctx.batch_cast_votes(contest_label, votes).await;
        assert!(
            result.is_ok(),
            "REGRESSION: batch_cast_hieroglyph_votes must succeed for a funded, \
             non-blacklisted voter. A failure here means EITHER the \
             validate_voter_security brick (require!(default != default)) was \
             reintroduced, OR its upstream blacklist bootstrap broke. Error: {:?}",
            result.err()
        );
    }
}
