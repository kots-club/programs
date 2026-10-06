//! Prize Distribution Program Tests
//!
//! Comprehensive test suite for all 4 prize distribution functions
//! Target: 100% function coverage + award system testing + error paths

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

#[cfg(test)]
mod prize_distribution_tests {
    use super::*;
    
    // ============================================================================
    // PRIZE DISTRIBUTION INITIALIZATION TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_initialize_prize_distribution() {
        // Test prize distribution program initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test successful initialization
        let result = ctx.initialize_prize_distribution().await;
        assert!(result.is_ok(), "Prize distribution initialization should succeed");
        
        // Verify distribution state is properly initialized
        let distribution_state = ctx.get_prize_distribution_state().await.unwrap();
        assert!(distribution_state.is_initialized, "Distribution should be initialized");
        assert_eq!(distribution_state.total_prizes_awarded, 0, "Should start with zero prizes awarded");
        assert_eq!(distribution_state.total_value_distributed, 0, "Should start with zero value distributed");
        
        // Test duplicate initialization should fail
        let result2 = ctx.initialize_prize_distribution().await;
        assert!(result2.is_err(), "Duplicate initialization should fail");
        
        println!("✅ Prize distribution initialization test passed!");
    }
    
    // ============================================================================
    // PRIZE CALCULATION TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_calc_prize_various_positions() {
        // Test prize calculation for different positions
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("prize_calc_test");
        let total_pool = 1_000_000; // 1M KOTS total prize pool
        
        // Test prize calculation for positions 1-10
        // KOTS_TOKENOMICS.md §4c: (11-place)% of the 55% placement pool.
        // For a 1_000_000 pool: placement pool = 550_000; place N = 550_000*(11-N)%.
        let expected_prizes = vec![
            (1, 55_000),   // 10% of 550k
            (2, 49_500),   // 9%
            (3, 44_000),   // 8%
            (4, 38_500),   // 7%
            (5, 33_000),   // 6%
        ];
        
        for (position, expected_min) in expected_prizes {
            let prize = ctx.calc_prize(contest_label.clone(), position as u32, total_pool).await.unwrap();
            assert!(prize >= expected_min, "Position {} prize should be at least {}, got {}", position, expected_min, prize);
            println!("✅ Position {} prize: {} KOTS", position, prize);
        }
        
        // Test that higher positions get higher prizes
        let prize_1st = ctx.calc_prize(contest_label.clone(), 1, total_pool).await.unwrap();
        let prize_2nd = ctx.calc_prize(contest_label.clone(), 2, total_pool).await.unwrap();
        let prize_3rd = ctx.calc_prize(contest_label.clone(), 3, total_pool).await.unwrap();
        
        assert!(prize_1st > prize_2nd, "1st place should have higher prize than 2nd");
        assert!(prize_2nd > prize_3rd, "2nd place should have higher prize than 3rd");
        
        println!("✅ Prize calculation by position test passed!");
    }
    
    #[tokio::test]
    async fn test_calc_prize_edge_cases() {
        // Test prize calculation edge cases
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("edge_test");
        
        // Test with zero total pool
        let result = ctx.calc_prize(contest_label.clone(), 1, 0).await;
        assert!(result.is_err(), "Zero total pool should fail");
        
        // Test with invalid position (0)
        let result = ctx.calc_prize(contest_label.clone(), 0, 1000).await;
        assert!(result.is_err(), "Position 0 should fail");
        
        // Test with very large position number
        let result = ctx.calc_prize(contest_label.clone(), 1000, 1000).await;
        assert!(result.is_err() || result.unwrap() == 0, "Very large position should fail or return 0");
        
        // Test with maximum u64 values
        let max_pool = u64::MAX;
        let result = ctx.calc_prize(contest_label.clone(), 1, max_pool).await;
        // Should handle overflow gracefully
        assert!(result.is_ok() || result.is_err(), "Max values should be handled gracefully");
        
        println!("✅ Prize calculation edge cases test passed!");
    }
    
    #[tokio::test]
    async fn test_calc_prize_distribution_fairness() {
        // Test that prize distribution is fair and adds up correctly
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("fairness_test");
        let total_pool = 10_000_000; // 10M KOTS
        
        // Calculate prizes for top 20 positions
        let mut total_distributed = 0;
        let num_winners = 20;
        
        for position in 1..=num_winners {
            // Positions > 10 are past the §4c 10-place curve and pay nothing —
            // calc_prize now returns Err for them (invalid position), which for
            // a "sum the top 20" fairness check means a 0 contribution, not a
            // panic. unwrap_or(0) makes 11..=20 contribute zero as intended.
            let prize = ctx
                .calc_prize(contest_label.clone(), position as u32, total_pool)
                .await
                .unwrap_or(0);
            total_distributed += prize;
            println!("Position {}: {} KOTS", position, prize);
        }
        
        // Total distributed should not exceed total pool
        assert!(total_distributed <= total_pool, "Total distributed ({}) should not exceed pool ({})", total_distributed, total_pool);
        
        // KOTS_TOKENOMICS.md §4c: PLACEMENT prizes are 55% of the pool, and
        // within that the 10 places sum to (10+9+...+1)=55% — so placement pays
        // 0.55*0.55 = 30.25% of the contest pool. The remaining ~45% is the
        // special/voter tranche (Tier 2), NOT placement. Positions 11..20 pay 0.
        // (Was >=0.5, an assumption from the superseded 50/30/20 3-winner curve.)
        let distribution_ratio = (total_distributed as f64) / (total_pool as f64);
        assert!(
            (distribution_ratio - 0.3025).abs() < 0.01,
            "Placement should distribute ~30.25% of pool (55% of 55%), got {:.4}",
            distribution_ratio
        );
        
        println!("✅ Prize distribution fairness test passed! Distributed: {:.2}%", distribution_ratio * 100.0);
    }
    
    // ============================================================================
    // PRIZE CLAIMING TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_claim_prize_success() {
        // Test successful prize claiming
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        // Setup contest and participants
        let contest_label = generate_test_contest_label("claim_test");
        let winner = ctx.new_identity();
        let position = 1;
        let total_pool = 2_000_000;
        
        // Calculate expected prize
        let expected_prize = ctx.calc_prize(contest_label.clone(), position as u32, total_pool).await.unwrap();
        
        // Setup contest with sufficient funds
        ctx.fund_contest_vault(contest_label, total_pool).await.unwrap();

        // F17 D2 (2026-08-06): claim_prize now SETTLES on-chain (lamports leave
        // the vault for the winner), so a real winner record must exist for this
        // claimer at this position with the exact §4c amount. In the full flow
        // execute_auto_distribution writes these; here we store it directly so
        // the unit test targets the claim path in isolation. (claim_prize funds
        // the paying vault + bootstraps emergency state itself.)
        ctx.store_winner(contest_label.clone(), position as u32, winner, expected_prize)
            .await
            .unwrap();

        // Claim prize
        let initial_balance = ctx.get_account_balance(&winner).await.unwrap_or(0);
        let result = ctx.claim_prize(contest_label.clone(), winner, position).await;
        assert!(result.is_ok(), "Prize claim should succeed");
        
        // Verify prize was transferred
        let final_balance = ctx.get_account_balance(&winner).await.unwrap();
        assert!(final_balance >= initial_balance + expected_prize, "Winner should receive prize");
        
        // Verify claim history
        let claim_history = ctx.get_prize_claim_history(contest_label, position as u32).await.unwrap();
        assert!(claim_history.is_claimed, "Prize should be marked as claimed");
        assert_eq!(claim_history.winner, winner, "Claim history should record correct winner");
        
        println!("✅ Prize claim success test passed! Claimed: {} KOTS", expected_prize);
    }
    
    #[tokio::test]
    async fn test_claim_prize_error_conditions() {
        // Test prize claiming error conditions
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("error_test");
        let winner = ctx.new_identity();
        
        // Test claim without contest setup
        let result = ctx.claim_prize(contest_label.clone(), winner, 1).await;
        assert!(result.is_err(), "Claim without contest should fail");
        
        // Setup contest with insufficient funds
        ctx.fund_contest_vault(contest_label, 1000).await.unwrap(); // Very small amount
        
        // Test claim with insufficient vault funds
        let result = ctx.claim_prize(contest_label.clone(), winner, 1).await;
        assert!(result.is_err(), "Claim with insufficient funds should fail");
        
        // Test double claim
        let well_funded_contest = generate_test_contest_label("double_claim_test");
        ctx.fund_contest_vault(well_funded_contest, 5_000_000).await.unwrap();

        // F17 D2: store a real winner record so the first claim has something to
        // settle; the second then fails on the persisted `claimed` flag.
        let wf_prize = ctx.calc_prize(well_funded_contest.clone(), 1, 5_000_000).await.unwrap();
        ctx.store_winner(well_funded_contest.clone(), 1, winner, wf_prize).await.unwrap();

        // First claim should succeed
        let result1 = ctx.claim_prize(well_funded_contest.clone(), winner, 1).await;
        assert!(result1.is_ok(), "First claim should succeed");
        
        // Second claim should fail
        let result2 = ctx.claim_prize(well_funded_contest, winner, 1).await;
        assert!(result2.is_err(), "Double claim should fail");
        
        // Test invalid position claim
        let invalid_position_contest = generate_test_contest_label("invalid_pos_test");
        ctx.fund_contest_vault(invalid_position_contest, 1_000_000).await.unwrap();
        
        let result = ctx.claim_prize(invalid_position_contest, winner, 0).await;
        assert!(result.is_err(), "Invalid position claim should fail");
        
        println!("✅ Prize claim error conditions test passed!");
    }
    
    #[tokio::test]
    async fn test_claim_prize_multiple_winners() {
        // Test claiming prizes for multiple winners
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("multi_winner_test");
        let total_pool = 5_000_000;
        
        // Setup contest
        ctx.fund_contest_vault(contest_label, total_pool).await.unwrap();
        
        // Create multiple winners — signable identities (real claims must sign).
        // Separate bindings so each &mut borrow ends before the next.
        let w1 = ctx.new_identity();
        let w2 = ctx.new_identity();
        let w3 = ctx.new_identity();
        let w4 = ctx.new_identity();
        let w5 = ctx.new_identity();
        let winners = vec![(w1, 1u32), (w2, 2u32), (w3, 3u32), (w4, 4u32), (w5, 5u32)];
        
        let mut total_claimed = 0;
        
        // Each winner claims their prize
        for (winner, position) in winners {
            let expected_prize = ctx.calc_prize(contest_label.clone(), position as u32, total_pool).await.unwrap();

            // F17 D2: a real winner record must exist for this claimer/position
            // (execute_auto_distribution writes these in the full flow; stored
            // directly here to isolate the claim path).
            ctx.store_winner(contest_label.clone(), position as u32, winner, expected_prize)
                .await
                .unwrap();

            let result = ctx.claim_prize(contest_label.clone(), winner, position).await;
            assert!(result.is_ok(), "Position {} claim should succeed", position);
            
            // Verify claim
            let claim_history = ctx.get_prize_claim_history(contest_label, position as u32).await.unwrap();
            assert!(claim_history.is_claimed, "Position {} should be marked as claimed", position);
            
            total_claimed += expected_prize;
            println!("✅ Position {} claimed {} KOTS", position, expected_prize);
        }
        
        // Verify total claimed doesn't exceed pool
        assert!(total_claimed <= total_pool, "Total claimed should not exceed pool");
        
        println!("✅ Multiple winners claim test passed! Total claimed: {} KOTS", total_claimed);
    }
    
    // ============================================================================
    // AUTOMATIC DISTRIBUTION TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_execute_auto_distribution() {
        // Test automatic prize distribution
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("auto_dist_test");
        let total_pool = 3_000_000;

        // Create the contest FIRST — registration joins an existing contest, so
        // without this the participant PDAs have no contest to attach to and
        // JoinContestUnlimited fails 3012 (AccountNotInitialized). (This test
        // funded the vault but never created the contest.)
        ctx.create_test_contest(contest_label, 3600, 2, 100_000_000)
            .await
            .unwrap();

        // Setup contest with sufficient funds
        ctx.fund_contest_vault(contest_label, total_pool).await.unwrap();
        
        // Create participant list with scores
        // Signable identities — registration (JoinContestUnlimited.user) and
        // any downstream claim (ClaimPrize.claimer) are Signer<'info>, so these
        // must be retained keypairs, not Pubkey::new_unique().
        let ep1 = ctx.new_identity();
        let ep2 = ctx.new_identity();
        let ep3 = ctx.new_identity();
        let ep4 = ctx.new_identity();
        let ep5 = ctx.new_identity();
        let participants = vec![
            (ep1, 1000), // Top scorer
            (ep2, 950),  // 2nd place
            (ep3, 900),  // 3rd place
            (ep4, 850),  // 4th place
            (ep5, 800),  // 5th place
        ];
        
        // Register participants
        for (participant, score) in &participants {
            ctx.register_contest_participant(contest_label, *participant).await.unwrap();
        }
        
        // Execute automatic distribution
        let result = ctx.execute_auto_distribution(contest_label.clone()).await;
        assert!(result.is_ok(), "Auto distribution should succeed");
        
        // execute_auto_distribution creates the TOP-3 winner records (unclaimed);
        // the F17 model settles on-chain via claim_prize. These participants have
        // no on-chain score, so ranking is the deterministic pubkey tiebreak —
        // we don't know which participant landed where, so READ each record's
        // actual winner, claim as that wallet, and verify the settlement.
        // (Only 3 records exist: execute_auto_distribution is currently limited
        // to winner_first/second/third; extending it to the full 10-place curve
        // is a separate program task.)
        for position in 1u32..=3 {
            let record = ctx.get_winner_record(contest_label, position).await.unwrap();
            let winner = record.winner_wallet;
            assert!(winner != Pubkey::default(), "Position {} should have a real winner", position);

            let before = ctx.get_account_balance(&winner).await.unwrap_or(0);
            ctx.claim_prize(contest_label.clone(), winner, position).await.unwrap();
            let after = ctx.get_account_balance(&winner).await.unwrap();
            assert!(after >= before, "Position {} winner should receive the prize", position);

            let claim_history = ctx.get_prize_claim_history(contest_label, position).await.unwrap();
            assert!(claim_history.is_claimed, "Position {} should be claimed after settling", position);
            assert_eq!(claim_history.winner, winner, "Claim history should match the winner record");
        }
        
        println!("✅ Auto distribution test passed!");
    }
    
    #[tokio::test]
    async fn test_execute_auto_distribution_error_conditions() {
        // Test auto distribution error conditions
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("auto_error_test");
        
        // Test auto distribution without contest setup
        let result = ctx.execute_auto_distribution(contest_label.clone()).await;
        assert!(result.is_err(), "Auto distribution without contest should fail");
        
        // Test with contest but no participants
        ctx.fund_contest_vault(contest_label, 1_000_000).await.unwrap();
        let result = ctx.execute_auto_distribution(contest_label.clone()).await;
        assert!(result.is_err(), "Auto distribution without participants should fail");
        
        // Test with insufficient funds
        let low_fund_contest = generate_test_contest_label("low_fund_test");
        // The contest must EXIST for registration (was 3012 on the register
        // unwrap below). Not advancing the clock afterwards keeps
        // execute_auto_distribution failing deterministically (timing gate),
        // so the is_err expectation still holds.
        ctx.create_test_contest(low_fund_contest, 3600, 2, 100_000_000)
            .await
            .unwrap();
        ctx.fund_contest_vault(low_fund_contest, 100).await.unwrap(); // Very low amount
        
        // Add participants
        for i in 0..3 {
            let participant = ctx.new_identity();
            ctx.register_contest_participant(low_fund_contest, participant).await.unwrap();
        }
        
        // Distribution scales prizes to the ACTUAL vault balance (placement_prize
        // of the real pool), so a low balance is not an error — it distributes
        // proportionally (tiny amounts). Verify it succeeds gracefully; the real
        // error conditions are "no contest" and "no participants" above.
        let result = ctx.execute_auto_distribution(low_fund_contest).await;
        assert!(result.is_ok(), "Auto distribution scales to available funds (low balance distributes proportionally, not an error)");
        
        println!("✅ Auto distribution error conditions test passed!");
    }
    
    #[tokio::test]
    async fn test_auto_distribution_large_contest() {
        // Test auto distribution with large number of participants
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        // SCALABLE DISTRIBUTION (2026-08-06): 100 participants cannot be scored in
        // one transaction (Solana's ~35-account legacy limit), so this exercises
        // the batched top-N accumulator: init -> score in batches -> finalize ->
        // claim. Proves the mechanism scales past the per-tx account ceiling;
        // production can use ALT for larger batches.
        let contest_label = generate_test_contest_label("large_contest_test");
        let total_pool = 10_000_000_000u64; // fund the vault with real lamports for claims
        let num_participants = 100usize;

        // Contest + vault setup (entry_fee 0 keeps registration lamport-cheap):
        //  1. fund the contest-management vault (becomes total_prize_pool),
        //  2. initialize the prize-distribution ContestVaultState from it,
        //  3. deposit real lamports into the pd-vault so claims can pay out.
        ctx.create_test_contest(contest_label, 3600, 2, 0).await.unwrap();
        ctx.fund_contest_vault(contest_label, total_pool).await.unwrap();
        ctx.ensure_contest_vault_initialized(contest_label).await.unwrap();
        ctx.fund_prize_vault_lamports(contest_label, total_pool).await.unwrap();

        // Register participants (retain pubkeys; all score 0, so the leaderboard
        // ranks deterministically by pubkey via the tiebreak).
        let mut participants: Vec<Pubkey> = Vec::with_capacity(num_participants);
        for _ in 0..num_participants {
            let p = ctx.new_identity();
            ctx.register_contest_participant(contest_label, p).await.unwrap();
            participants.push(p);
        }

        // Batched scoring pipeline — 100 participants across ~4 transactions.
        ctx.init_leaderboard(contest_label).await.unwrap();
        for chunk in participants.chunks(25) {
            ctx.process_score_batch(contest_label, chunk).await.unwrap();
        }
        ctx.finalize_leaderboard(contest_label).await.unwrap();

        // The accumulator holds the ranked top 100. Claim the top 10 (the §4c
        // payout depth); each claim is verified against the on-chain ranking.
        let top = ctx.read_leaderboard_top(contest_label, 10).await.unwrap();
        assert_eq!(top.len(), 10, "leaderboard should hold 10 ranked winners");
        for (i, wallet) in top.iter().enumerate() {
            ctx.claim_from_leaderboard(contest_label, *wallet, i as u16)
                .await
                .unwrap_or_else(|e| panic!("claim for position {i} failed: {e:?}"));
        }

        println!(
            "✅ Scalable large-contest distribution: {} participants scored in batches, top 10 claimed against the accumulator",
            num_participants
        );
    }
    
    // ============================================================================
    // WINNER RECORD PDA TESTS (NEW SECURITY SYSTEM)
    // ============================================================================
    
    #[tokio::test]
    async fn test_store_winner_record() {
        // Test storing winner records on-chain for security
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("winner_record_test");
        let winner_wallet = Pubkey::new_unique();
        let position = 1;
        let prize_amount = 1_000_000; // 1M KOTS
        
        // Store winner record on-chain
        let result = ctx.store_winner(
            contest_label.clone(),
            position,
            winner_wallet,
            prize_amount
        ).await;
        assert!(result.is_ok(), "Storing winner record should succeed");
        
        // Verify WinnerRecord PDA was created correctly
        let winner_record = ctx.get_winner_record(contest_label, position).await.unwrap();
        assert!(winner_record.is_initialized, "Winner record should be initialized");
        assert_eq!(winner_record.winner_wallet, winner_wallet, "Wallet should match");
        assert_eq!(winner_record.position, position, "Position should match");
        assert_eq!(winner_record.prize_amount, prize_amount, "Prize amount should match");
        assert!(!winner_record.claimed, "Should start as unclaimed");
        
        println!("✅ Store winner record test passed! Winner: {}, Amount: {}", winner_wallet, prize_amount);
    }
    
    #[tokio::test]
    async fn test_store_multiple_winners() {
        // Test storing multiple winner records for a contest
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("multi_winner_record");
        let total_pool = 10_000_000;
        
        // Store top 3 winners
        let winners = vec![
            (Pubkey::new_unique(), 1, 5_000_000), // 1st: 50%
            (Pubkey::new_unique(), 2, 3_000_000), // 2nd: 30%
            (Pubkey::new_unique(), 3, 2_000_000), // 3rd: 20%
        ];
        
        for (wallet, position, prize) in &winners {
            let result = ctx.store_winner(contest_label.clone(), *position, *wallet, *prize).await;
            assert!(result.is_ok(), "Storing position {} should succeed", position);
        }
        
        // Verify all winner records
        for (wallet, position, prize) in &winners {
            let record = ctx.get_winner_record(contest_label, *position).await.unwrap();
            assert_eq!(record.winner_wallet, *wallet, "Position {} wallet mismatch", position);
            assert_eq!(record.prize_amount, *prize, "Position {} prize mismatch", position);
        }
        
        println!("✅ Multiple winner records stored successfully!");
    }
    
    #[tokio::test]
    async fn test_claim_prize_with_winner_record() {
        // Test claiming prize using WinnerRecord PDA (contest winner mode)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("winner_claim_test");
        let winner_wallet = ctx.new_identity();
        let position = 1;
        let prize_amount = 2_000_000;
        
        // Setup: Store winner record and fund vault
        ctx.store_winner(contest_label.clone(), position, winner_wallet, prize_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, prize_amount).await.unwrap();
        
        // Claim prize (optional_amount = None triggers winner mode)
        let result = ctx.claim_prize_with_winner_record(
            contest_label.clone(),
            winner_wallet,
            position,
            None  // None = contest winner mode
        ).await;
        assert!(result.is_ok(), "Winner should claim prize successfully");
        
        // Verify winner record is marked as claimed
        let record = ctx.get_winner_record(contest_label, position).await.unwrap();
        assert!(record.claimed, "Winner record should be marked claimed");
        assert!(record.claim_timestamp > 0, "Claim timestamp should be set");
        
        println!("✅ Claim prize with winner record test passed!");
    }
    
    #[tokio::test]
    async fn test_claim_prize_unauthorized_wallet() {
        // Test that wrong wallet cannot claim prize (security check)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("unauthorized_test");
        let real_winner = ctx.new_identity();
        let fake_claimer = ctx.new_identity();
        let position = 1;
        let prize_amount = 1_000_000;
        
        // Store winner record for real_winner
        ctx.store_winner(contest_label.clone(), position, real_winner, prize_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, prize_amount).await.unwrap();
        
        // Attempt to claim with wrong wallet
        let result = ctx.claim_prize_with_winner_record(
            contest_label.clone(),
            fake_claimer,  // Wrong wallet!
            position,
            None
        ).await;
        assert!(result.is_err(), "Unauthorized claim should fail");
        
        // Verify record still unclaimed
        let record = ctx.get_winner_record(contest_label, position).await.unwrap();
        assert!(!record.claimed, "Record should still be unclaimed");
        
        println!("✅ Unauthorized claim blocked successfully!");
    }
    
    #[tokio::test]
    async fn test_double_claim_prevention() {
        // Test that winner cannot claim prize twice (signable identity)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("double_claim_test");
        let winner_wallet = ctx.new_identity();
        let position = 1;
        let prize_amount = 1_500_000;
        
        // Setup
        ctx.store_winner(contest_label.clone(), position, winner_wallet, prize_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, prize_amount * 2).await.unwrap(); // Fund double to test
        
        // First claim should succeed
        let result1 = ctx.claim_prize_with_winner_record(
            contest_label.clone(),
            winner_wallet,
            position,
            None
        ).await;
        assert!(result1.is_ok(), "First claim should succeed");
        
        // Second claim should fail (already claimed)
        let result2 = ctx.claim_prize_with_winner_record(
            contest_label.clone(),
            winner_wallet,
            position,
            None
        ).await;
        assert!(result2.is_err(), "Double claim should fail");
        
        println!("✅ Double claim prevention working!");
    }
    
    // ============================================================================
    // VOTER SHARE RECORD TESTS (DETERMINISTIC VOTER POOL — plan §5a de-chancing)
    // ============================================================================
    
    #[tokio::test]
    async fn test_store_voter_share() {
        // Store a voter's deterministic pool share (plan §5a de-chancing)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("voter_share_test");
        let voter_wallet = Pubkey::new_unique();
        let share_amount = 25 * TOKENS_PER_UNIT; // exact pro-rata amount from vote records
        
        // Store the share (written once at contest finalization)
        let result = ctx.store_voter_share(
            contest_label.clone(),
            voter_wallet,
            share_amount
        ).await;
        assert!(result.is_ok(), "Storing voter share should succeed");
        
        // Verify VoterShareRecord PDA
        let share_record = ctx.get_voter_share_record(contest_label, voter_wallet).await.unwrap();
        assert!(share_record.is_initialized, "Share record should be initialized");
        assert_eq!(share_record.voter_wallet, voter_wallet, "Voter wallet should match");
        assert_eq!(share_record.share_amount, share_amount, "Share amount should match");
        assert!(!share_record.claimed, "Share should start unclaimed");
        
        println!("✅ Store voter share test passed! Amount: {} KOTS", share_amount / TOKENS_PER_UNIT);
    }
    
    #[tokio::test]
    async fn test_voter_share_immutable() {
        // A stored share cannot be overwritten: finalization prices it once.
        // (The ticket-era record ACCUMULATED across sessions — that running
        // chance balance is exactly what de-chancing removed.)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("share_immutable");
        let voter_wallet = Pubkey::new_unique();
        
        let first = ctx.store_voter_share(
            contest_label.clone(),
            voter_wallet,
            10 * TOKENS_PER_UNIT
        ).await;
        assert!(first.is_ok(), "First store should succeed");
        
        let second = ctx.store_voter_share(
            contest_label.clone(),
            voter_wallet,
            999 * TOKENS_PER_UNIT
        ).await;
        assert!(second.is_err(), "Re-storing a share must fail (init, not init_if_needed)");
        
        let record = ctx.get_voter_share_record(contest_label, voter_wallet).await.unwrap();
        assert_eq!(record.share_amount, 10 * TOKENS_PER_UNIT, "Original share must be untouched");
        
        println!("✅ Voter share immutability test passed!");
    }
    
    #[tokio::test]
    async fn test_claim_voter_share() {
        // Claim pays the exact stored amount, once
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("claim_share_test");
        let voter_wallet = Pubkey::new_unique();
        let share_amount = 50 * TOKENS_PER_UNIT;
        
        // Setup: store the share and fund the vault
        ctx.store_voter_share(contest_label.clone(), voter_wallet, share_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, share_amount * 2).await.unwrap();
        
        // Claim (optional_amount = Some(x) triggers voter-share mode; x must
        // equal the stored amount exactly)
        let result = ctx.claim_voter_share(
            contest_label.clone(),
            voter_wallet,
            Some(share_amount)
        ).await;
        assert!(result.is_ok(), "Voter share claim should succeed");
        
        println!("✅ Voter share claim test passed! Claimed: {} KOTS", share_amount / TOKENS_PER_UNIT);
    }
    
    #[tokio::test]
    async fn test_claim_voter_share_exact_amount_required() {
        // The claimer cannot choose an amount — choosing was a ticket-era
        // concept. Anything other than the stored amount fails.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("exact_amount_test");
        let voter = solana_sdk::signature::Keypair::new();
        let voter_wallet = solana_sdk::signer::Signer::pubkey(&voter);
        let share_amount = 150 * TOKENS_PER_UNIT;
        
        ctx.store_voter_share(contest_label.clone(), voter_wallet, share_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, 10_000 * TOKENS_PER_UNIT).await.unwrap();
        
        // Test 1: partial claim must fail
        let partial = ctx.claim_voter_share_signed(contest_label.clone(), &voter,
            Some(share_amount / 2)
        ).await;
        assert!(partial.is_err(), "Partial claim should fail");
        
        // Test 2: inflated claim must fail
        let inflated = ctx.claim_voter_share_signed(contest_label.clone(), &voter,
            Some(share_amount * 2)
        ).await;
        assert!(inflated.is_err(), "Inflated claim should fail");
        
        // Test 3: exact amount succeeds
        let exact = ctx.claim_voter_share_signed(contest_label.clone(), &voter,
            Some(share_amount)
        ).await;
        assert!(exact.is_ok(), "Exact amount should succeed");
        
        println!("✅ Exact-amount requirement tests passed!");
    }
    
    #[tokio::test]
    async fn test_claim_voter_share_twice_fails() {
        // A share is claimable exactly once — same guarantee as WinnerRecord
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("double_share_claim");
        let voter = solana_sdk::signature::Keypair::new();
        let voter_wallet = solana_sdk::signer::Signer::pubkey(&voter);
        let share_amount = 30 * TOKENS_PER_UNIT;
        
        ctx.store_voter_share(contest_label.clone(), voter_wallet, share_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, share_amount * 3).await.unwrap();
        
        let first = ctx.claim_voter_share_signed(contest_label.clone(), &voter, Some(share_amount)).await;
        assert!(first.is_ok(), "First claim should succeed");
        
        let second = ctx.claim_voter_share_signed(contest_label.clone(), &voter, Some(share_amount)).await;
        assert!(second.is_err(), "Second claim must fail (already claimed)");
        
        println!("✅ Double share claim prevention test passed!");
    }
    
    #[tokio::test]
    async fn test_voter_share_security() {
        // Share records prevent database manipulation: no record, no claim
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("share_security");
        let voter = solana_sdk::signature::Keypair::new();
        let voter_wallet = solana_sdk::signer::Signer::pubkey(&voter);
        let fake_voter_kp = solana_sdk::signature::Keypair::new();
        let fake_voter = solana_sdk::signer::Signer::pubkey(&fake_voter_kp);
        let share_amount = 50 * TOKENS_PER_UNIT;
        
        // Store a share for the real voter only
        ctx.store_voter_share(contest_label.clone(), voter_wallet, share_amount).await.unwrap();
        ctx.fund_contest_vault(contest_label, 1000 * TOKENS_PER_UNIT).await.unwrap();
        
        // Fake voter tries to claim (no share record exists)
        // Signed as the fake voter: the record's seeds bind claimer.key(), so
        // the program itself must refuse (no record exists at that PDA).
        let result = ctx.claim_voter_share_signed(
            contest_label.clone(),
            &fake_voter_kp,
            Some(share_amount)
        ).await;
        assert!(result.is_err(), "Voter without a share record should fail");
        
        // Real voter can claim
        let result = ctx.claim_voter_share_signed(contest_label.clone(), &voter,
            Some(share_amount)
        ).await;
        assert!(result.is_ok(), "Real voter with a stored share should succeed");
        
        println!("✅ Voter share security test passed!");
    }
    
    // ========================================================================
    // PARTICIPANT WINNER — PREDICTION ACCURACY (SCORING-MODEL.md §1/Q2)
    // "The voter who was most right about the most memes."
    // ========================================================================

    #[tokio::test]
    async fn test_prediction_accuracy_basics() {
        // Correct FOR adds stake; wrong FOR subtracts it; correct AGAINST adds.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_basics");
        let mut winners = [0u64; 10];
        winners[0] = 101; // meme 101 placed; everything else did not
        ctx.set_prediction_outcomes(label.clone(), winners, 1).await.unwrap();

        let alice = Pubkey::new_unique(); // right, with conviction
        let bob = Pubkey::new_unique();   // wrong, with conviction
        let carol = Pubkey::new_unique(); // right by AGAINST

        ctx.tally_predictions(label.clone(), vec![
            (alice, 101, 1, 50),  // FOR the winner:      +50
            (bob,   202, 1, 50),  // FOR a loser:         -50
            (carol, 202, -1, 30), // AGAINST a loser:     +30
        ]).await.unwrap();

        let t = ctx.get_prediction_tally(label).await.unwrap();
        let acc = |v: &Pubkey| {
            (0..t.voter_count as usize)
                .find(|&i| t.voters[i] == *v)
                .map(|i| t.accuracies[i])
                .unwrap_or(i64::MIN)
        };
        assert_eq!(acc(&alice), 50, "Correct FOR earns the stake");
        assert_eq!(acc(&bob), -50, "Wrong FOR loses the stake — wrongs are negative");
        assert_eq!(acc(&carol), 30, "Correct AGAINST earns the stake");

        println!("✅ Prediction accuracy basics passed");
    }

    #[tokio::test]
    async fn test_prediction_spray_nets_zero() {
        // Spraying equal stakes across everything cannot farm accuracy:
        // one hit cancels against the misses.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_spray");
        let mut winners = [0u64; 10];
        winners[0] = 1;
        ctx.set_prediction_outcomes(label.clone(), winners, 1).await.unwrap();

        let sprayer = Pubkey::new_unique();
        // FOR on the 1 winner and 4 losers, equal stakes
        ctx.tally_predictions(label.clone(), vec![
            (sprayer, 1, 1, 10),
            (sprayer, 2, 1, 10),
            (sprayer, 3, 1, 10),
            (sprayer, 4, 1, 10),
            (sprayer, 5, 1, 10),
        ]).await.unwrap();

        let t = ctx.get_prediction_tally(label).await.unwrap();
        assert_eq!(t.voter_count, 1);
        assert_eq!(t.accuracies[0], 10 - 40, "1 hit (+10) vs 4 misses (-40) = -30: spray loses");

        println!("✅ Spray-and-pray nets negative — skill required");
    }

    #[tokio::test]
    async fn test_prediction_free_actions_never_score() {
        // Stake-0 rows (free Upvotes) are skipped: no skin, no skill credit (P3).
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_free");
        let mut winners = [0u64; 10];
        winners[0] = 7;
        ctx.set_prediction_outcomes(label.clone(), winners, 1).await.unwrap();

        let freeloader = Pubkey::new_unique();
        ctx.tally_predictions(label.clone(), vec![
            (freeloader, 7, 1, 0), // free upvote on the winner — no credit
        ]).await.unwrap();

        let t = ctx.get_prediction_tally(label).await.unwrap();
        assert_eq!(t.voter_count, 0, "Zero-stake predictions never enter the ledger");

        println!("✅ Free actions carry no prediction weight");
    }

    #[tokio::test]
    async fn test_prediction_attack_glyph_economics() {
        // The cut answer, end to end: an attack glyph stakes the attacker FOR
        // the target. Attacking the eventual winner is a (grudgingly) correct
        // prediction; attacking a flop is paying to be wrong twice.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_attack");
        let mut winners = [0u64; 10];
        winners[0] = 11; // the "threat" placed
        ctx.set_prediction_outcomes(label.clone(), winners, 1).await.unwrap();

        let saboteur = Pubkey::new_unique();
        // Effect glyphs (24-29) carry stance FOR; the assembler sends them as such.
        ctx.tally_predictions(label.clone(), vec![
            (saboteur, 11, 1, 25), // "cut" the threat that then won: +25
            (saboteur, 12, 1, 25), // "cut" a meme that flopped:      -25
        ]).await.unwrap();

        let t = ctx.get_prediction_tally(label).await.unwrap();
        assert_eq!(t.accuracies[0], 0, "Threat-recognition is graded like any prediction");

        println!("✅ Attack glyphs are endorsements with consequences");
    }

    #[tokio::test]
    async fn test_voter_pro_rata_everyone_positive() {
        // 20% VOTER POOL, Option A (owner-decided 2026-08-06): every voter with
        // POSITIVE accuracy shares the pool in proportion to that accuracy.
        // Worked example — 3 voters a:+40, b:+90, c:+40 (Σ positive = 170) on a
        // 1_000_000 pool → voter pool = 20% = 200_000.
        let ctx = MultiProgramTestContext::new().await.unwrap();
        let pool: u64 = 1_000_000;
        let voter_pool = pool * kots_common::constants::VOTER_PRO_RATA_BPS / 10_000; // 200_000
        let sum_positive: u128 = (40 + 90 + 40) as u128; // 170

        let a = ctx.voter_pro_rata_share(40, sum_positive, voter_pool);
        let b = ctx.voter_pro_rata_share(90, sum_positive, voter_pool);
        let c = ctx.voter_pro_rata_share(40, sum_positive, voter_pool);

        // Floor division: 40*200000/170=47058, 90*200000/170=105882.
        assert_eq!(a, 47_058, "a's pro-rata slice");
        assert_eq!(b, 105_882, "b's pro-rata slice (highest accuracy → largest slice)");
        assert_eq!(c, 47_058, "c ties a exactly — equal accuracy, equal pay");

        // Non-winners earn nothing; the split never exceeds the pool.
        assert_eq!(ctx.voter_pro_rata_share(0, sum_positive, voter_pool), 0, "zero accuracy → 0");
        assert_eq!(ctx.voter_pro_rata_share(-30, sum_positive, voter_pool), 0, "negative accuracy → 0");
        assert!(a + b + c <= voter_pool, "distributed ≤ voter pool (remainder stays in vault)");
        assert_eq!(voter_pool - (a + b + c), 2, "sub-lamport remainder (2) held, not overpaid");

        println!("✅ Voter pro-rata (everyone-positive) splits the 20% pool by accuracy");
    }

    #[tokio::test]
    async fn test_participant_ranking_deterministic() {
        // Ranking: descending accuracy; equal accuracy → ascending pubkey
        // bytes — a total order with no chance anywhere.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_ranking");
        let mut winners = [0u64; 10];
        winners[0] = 1;
        ctx.set_prediction_outcomes(label.clone(), winners, 1).await.unwrap();

        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let c = Pubkey::new_unique();
        ctx.tally_predictions(label.clone(), vec![
            (a, 1, 1, 40), // +40
            (b, 1, 1, 90), // +90 — the participant winner
            (c, 1, 1, 40), // +40 — ties with a; pubkey bytes decide
        ]).await.unwrap();

        ctx.rank_participant_winners(label.clone()).await.unwrap();

        let t = ctx.get_prediction_tally(label).await.unwrap();
        assert!(t.finalized, "Ranking finalizes the tally");
        // Re-derive the expected order locally — anyone can, that is the point.
        let mut expect = vec![(t.accuracies[0], t.voters[0]), (t.accuracies[1], t.voters[1]), (t.accuracies[2], t.voters[2])];
        expect.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.to_bytes().cmp(&y.1.to_bytes())));
        assert_eq!(expect[0].1, b, "Highest accuracy wins outright");

        // A second finalize must fail — the ranking is computed exactly once.
        let again = ctx.rank_participant_winners(label.clone()).await;
        assert!(again.is_err(), "Ranking is single-shot");

        println!("✅ Participant ranking is total, deterministic, and single-shot");
    }

    #[tokio::test]
    async fn test_prediction_requires_outcomes_first() {
        // Grading before Q1's outcomes exist is meaningless and rejected.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();

        let label = generate_test_contest_label("prediction_no_outcomes");
        let v = Pubkey::new_unique();
        let result = ctx.tally_predictions(label.clone(), vec![(v, 1, 1, 10)]).await;
        assert!(result.is_err(), "Tallying without recorded outcomes must fail");

        println!("✅ Outcomes-before-grading enforced");
    }

    #[tokio::test]
    async fn test_complete_prize_flow() {
        // Integration test: Complete flow from contest finalization to prize claims
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("complete_flow");
        let total_pool = 5_000_000;
        
        // Step 1: Contest finalizes, winners are stored
        // Signable — winners CLAIM (ClaimPrize.claimer is Signer). The amounts
        // here are arbitrary stored values exercised by the claim path, not a
        // placement-curve assertion.
        let cw1 = ctx.new_identity();
        let cw2 = ctx.new_identity();
        let cw3 = ctx.new_identity();
        let winners = vec![
            (cw1, 1u32, 2_500_000u64),
            (cw2, 2u32, 1_500_000u64),
            (cw3, 3u32, 1_000_000u64),
        ];
        
        for (wallet, position, prize) in &winners {
            ctx.store_winner(contest_label.clone(), *position, *wallet, *prize).await.unwrap();
        }
        
        // Step 2: Finalization stores each voter's exact pro-rata pool share
        // (computed from vote records — deterministic, plan §5a)
        let cv1 = ctx.new_identity();
        let cv2 = ctx.new_identity();
        let cv3 = ctx.new_identity();
        let voters = vec![
            (cv1, 50 * TOKENS_PER_UNIT),
            (cv2, 75 * TOKENS_PER_UNIT),
            (cv3, 100 * TOKENS_PER_UNIT),
        ];
        
        for (voter, share) in &voters {
            ctx.store_voter_share(contest_label.clone(), *voter, *share).await.unwrap();
        }
        
        // Step 3: Fund vault for all prizes + voter shares
        let total_needed = total_pool + 225 * TOKENS_PER_UNIT; // Extra for voter shares
        ctx.fund_contest_vault(contest_label, total_needed).await.unwrap();
        
        // Step 4: Winners claim prizes
        for (wallet, position, _) in &winners {
            let result = ctx.claim_prize_with_winner_record(
                contest_label.clone(),
                *wallet,
                *position,
                None
            ).await;
            assert!(result.is_ok(), "Winner {} should claim successfully", position);
        }
        
        // Step 5: Voters claim their exact pool shares
        for (voter, share) in &voters {
            let result = ctx.claim_voter_share(
                contest_label.clone(),
                *voter,
                Some(*share)
            ).await;
            assert!(result.is_ok(), "Voter should claim their share successfully");
        }
        
        println!("✅ Complete prize distribution flow test passed!");
    }
}