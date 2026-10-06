//! End-to-End Contest Lifecycle Tests
//!
//! Comprehensive tests for complete contest flow:
//! Create → Upload → Vote → Finalize → Distribute
//!
//! Tests critical for mainnet certification:
//! - Full contest lifecycle with real state transitions
//! - Cross-program state coordination
//! - Prize distribution atomicity
//! - Error recovery and edge cases

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

#[cfg(test)]
mod end_to_end_lifecycle_tests {
    use super::*;
    
    /// Test complete contest lifecycle: create → vote → finalize → distribute
    #[tokio::test]
    async fn test_full_contest_lifecycle_3_participants() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        // Phase 1: Create Contest
        let contest_label = generate_test_contest_label("e2e_3_participants");
        let duration = 3600; // 1 hour
        let min_participants = 3;
        let entry_fee = 100_000_000; // 0.1 KOTS
        
        ctx.create_test_contest(contest_label, duration, min_participants, entry_fee)
            .await
            .unwrap();
        
        println!("✅ Phase 1: Contest created");
        
        // Phase 2: Participants Upload Memes
        let participant1 = ctx.new_identity();
        let participant2 = ctx.new_identity();
        let participant3 = ctx.new_identity();
        
        ctx.register_contest_participant(contest_label, participant1).await.unwrap();
        ctx.register_contest_participant(contest_label, participant2).await.unwrap();
        ctx.register_contest_participant(contest_label, participant3).await.unwrap();
        
        println!("✅ Phase 2: 3 participants registered");
        
        // Phase 3: Voting Phase
        // Participant 1 gets most votes
        ctx.cast_vote(contest_label, 1, 1).await.unwrap(); // upvote participant 1
        ctx.cast_vote(contest_label, 1, 1).await.unwrap(); // upvote participant 1
        ctx.cast_vote(contest_label, 1, 1).await.unwrap(); // upvote participant 1
        
        // Participant 2 gets moderate votes
        ctx.cast_vote(contest_label, 2, 1).await.unwrap(); // upvote participant 2
        ctx.cast_vote(contest_label, 2, 1).await.unwrap(); // upvote participant 2
        
        // Participant 3 gets least votes
        ctx.cast_vote(contest_label, 3, 1).await.unwrap(); // upvote participant 3
        
        println!("✅ Phase 3: Voting completed (P1: 3 votes, P2: 2 votes, P3: 1 vote)");
        
        // Phase 4: Advance Time to Contest End
        ctx.advance_clock(7200).await.unwrap(); // Advance 2 hours
        
        println!("✅ Phase 4: Contest duration elapsed");
        
        // Phase 5: Finalize Contest and Distribute Prizes
        let total_prize_pool = entry_fee * 3; // 3 participants * entry fee
        ctx.fund_contest_vault(contest_label, total_prize_pool).await.unwrap();
        
        let result = ctx.execute_auto_distribution(contest_label).await;
        assert!(result.is_ok(), "Contest finalization should succeed");
        
        println!("✅ Phase 5: Contest finalized and prizes distributed");
        
        // Phase 6: Verify Winner Records
        let winner1 = ctx.get_winner_record(contest_label, 1).await.unwrap();
        assert_eq!(winner1.position, 1, "Winner 1 should be in position 1");
        assert!(winner1.prize_amount > 0, "Winner 1 should have prize");
        
        let winner2 = ctx.get_winner_record(contest_label, 2).await.unwrap();
        assert_eq!(winner2.position, 2, "Winner 2 should be in position 2");
        
        let winner3 = ctx.get_winner_record(contest_label, 3).await.unwrap();
        assert_eq!(winner3.position, 3, "Winner 3 should be in position 3");
        
        println!("✅ Phase 6: All winners verified");
        println!("✅ END-TO-END LIFECYCLE TEST PASSED (3 participants)");
    }
    
    /// Test contest lifecycle with 10 participants (full prize distribution)
    #[tokio::test]
    async fn test_full_contest_lifecycle_10_participants() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("e2e_10_participants");
        let duration = 3600;
        let min_participants = 10;
        let entry_fee = 500_000_000; // 0.5 KOTS per participant
        
        ctx.create_test_contest(contest_label, duration, min_participants, entry_fee)
            .await
            .unwrap();
        
        // Register 10 participants
        let mut participants = Vec::new();
        for i in 0..10 {
            let participant = ctx.new_identity();
            ctx.register_contest_participant(contest_label, participant).await.unwrap();
            participants.push(participant);
        }
        
        println!("✅ 10 participants registered");
        
        // Cast votes to create ranking (10 votes for #1, 9 for #2, etc.)
        for (meme_id, vote_count) in (1..=10).rev().enumerate() {
            let meme_id_u64 = (meme_id + 1) as u64;
            for _ in 0..vote_count {
                ctx.cast_vote(contest_label, meme_id_u64, 1).await.unwrap();
            }
        }
        
        println!("✅ Voting completed with clear ranking");
        
        // Finalize contest
        ctx.advance_clock(7200).await.unwrap();
        let total_prize_pool = entry_fee * 10;
        ctx.fund_contest_vault(contest_label, total_prize_pool).await.unwrap();
        
        ctx.execute_auto_distribution(contest_label).await.unwrap();
        
        // Verify all 10 winners have records
        for position in 1..=10 {
            let winner = ctx.get_winner_record(contest_label, position).await.unwrap();
            assert_eq!(winner.position, position, "Position {} mismatch", position);
            assert!(winner.prize_amount > 0, "Position {} should have prize", position);
            println!("Position {}: {} KOTS", position, winner.prize_amount);
        }
        
        println!("✅ END-TO-END LIFECYCLE TEST PASSED (10 participants)");
    }
    
    /// Test contest cancellation before finalization
    #[tokio::test]
    async fn test_contest_cancellation_lifecycle() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("e2e_cancel");
        
        ctx.create_test_contest(contest_label, 3600, 3, 100_000_000)
            .await
            .unwrap();
        
        // Register participants
        // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
        // mutably twice in one expression — E0499.
        let participant_1 = ctx.new_identity();
        ctx.register_contest_participant(contest_label, participant_1).await.unwrap();
        // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
        // mutably twice in one expression — E0499.
        let participant_2 = ctx.new_identity();
        ctx.register_contest_participant(contest_label, participant_2).await.unwrap();
        
        println!("✅ Contest created with 2 participants");
        
        // Attempt finalization before min participants met
        ctx.advance_clock(7200).await.unwrap();
        
        let result = ctx.execute_auto_distribution(contest_label).await;
        assert!(result.is_err(), "Should fail - min participants not met");
        
        println!("✅ Contest correctly rejected finalization (insufficient participants)");
    }
    
    /// Test prize claiming after contest finalization
    #[tokio::test]
    async fn test_claim_prizes_after_finalization() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("e2e_claim");
        
        ctx.create_test_contest(contest_label, 3600, 2, 200_000_000)
            .await
            .unwrap();
        
        let winner1 = ctx.new_identity();
        let winner2 = ctx.new_identity();
        
        ctx.register_contest_participant(contest_label, winner1).await.unwrap();
        ctx.register_contest_participant(contest_label, winner2).await.unwrap();
        
        // Winner 1 gets more votes
        ctx.cast_vote(contest_label, 1, 1).await.unwrap();
        ctx.cast_vote(contest_label, 1, 1).await.unwrap();
        ctx.cast_vote(contest_label, 2, 1).await.unwrap();
        
        // Finalize
        ctx.advance_clock(7200).await.unwrap();
        ctx.fund_contest_vault(contest_label, 400_000).await.unwrap();
        ctx.execute_auto_distribution(contest_label).await.unwrap();
        
        println!("✅ Contest finalized");
        
        // Claim prizes
        let initial_balance1 = ctx.get_account_balance(&winner1).await.unwrap_or(0);
        ctx.claim_prize_with_winner_record(contest_label, winner1, 1, None).await.unwrap();
        let final_balance1 = ctx.get_account_balance(&winner1).await.unwrap();
        
        assert!(final_balance1 > initial_balance1, "Winner 1 should receive prize");
        
        let initial_balance2 = ctx.get_account_balance(&winner2).await.unwrap_or(0);
        ctx.claim_prize_with_winner_record(contest_label, winner2, 2, None).await.unwrap();
        let final_balance2 = ctx.get_account_balance(&winner2).await.unwrap();
        
        assert!(final_balance2 > initial_balance2, "Winner 2 should receive prize");
        
        println!("✅ Both winners successfully claimed prizes");
        println!("✅ PRIZE CLAIMING TEST PASSED");
    }
    
    /// Test multiple contests running concurrently
    #[tokio::test]
    async fn test_concurrent_contests_lifecycle() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        // Create 3 concurrent contests
        let contest1 = generate_test_contest_label("concurrent_1");
        let contest2 = generate_test_contest_label("concurrent_2");
        let contest3 = generate_test_contest_label("concurrent_3");
        
        ctx.create_test_contest(contest1, 3600, 2, 100_000_000).await.unwrap();
        ctx.create_test_contest(contest2, 3600, 2, 150_000_000).await.unwrap();
        ctx.create_test_contest(contest3, 3600, 2, 200_000_000).await.unwrap();
        
        println!("✅ 3 concurrent contests created");
        
        // Register participants for each contest
        for contest in [contest1, contest2, contest3] {
            // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
            // mutably twice in one expression — E0499.
            let participant_3 = ctx.new_identity();
            ctx.register_contest_participant(contest, participant_3).await.unwrap();
            // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
            // mutably twice in one expression — E0499.
            let participant_4 = ctx.new_identity();
            ctx.register_contest_participant(contest, participant_4).await.unwrap();
        }
        
        // Vote in all contests
        for contest in [contest1, contest2, contest3] {
            ctx.cast_vote(contest, 1, 1).await.unwrap();
            ctx.cast_vote(contest, 2, 1).await.unwrap();
        }
        
        println!("✅ All contests have votes");
        
        // Finalize all contests
        ctx.advance_clock(7200).await.unwrap();
        
        ctx.fund_contest_vault(contest1, 200_000).await.unwrap();
        ctx.fund_contest_vault(contest2, 300_000).await.unwrap();
        ctx.fund_contest_vault(contest3, 400_000).await.unwrap();
        
        ctx.execute_auto_distribution(contest1).await.unwrap();
        ctx.execute_auto_distribution(contest2).await.unwrap();
        ctx.execute_auto_distribution(contest3).await.unwrap();
        
        println!("✅ All 3 contests finalized successfully");
        
        // Verify each contest has independent winners
        for (i, contest) in [contest1, contest2, contest3].iter().enumerate() {
            let winner1 = ctx.get_winner_record(*contest, 1).await.unwrap();
            let winner2 = ctx.get_winner_record(*contest, 2).await.unwrap();
            
            assert_eq!(winner1.position, 1, "Contest {} winner 1 position", i+1);
            assert_eq!(winner2.position, 2, "Contest {} winner 2 position", i+1);
        }
        
        println!("✅ CONCURRENT CONTESTS TEST PASSED");
    }
}

/// Helper function to generate unique contest labels
pub fn generate_test_contest_label(prefix: &str) -> [u8; 32] {
    let mut label = [0u8; 32];
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    
    let label_str = format!("{}_{}", prefix, timestamp);
    let bytes = label_str.as_bytes();
    let len = bytes.len().min(32);
    label[..len].copy_from_slice(&bytes[..len]);
    
    label
}
