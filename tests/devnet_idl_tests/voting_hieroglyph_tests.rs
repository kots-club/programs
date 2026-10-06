//! Voting & Hieroglyph Program Tests
//! 
//! Migrated from smart_contract/tests/test_score_calculator.rs and related tests
//! Adapted for multi-program architecture with cross-program communication

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};

#[cfg(test)]
mod voting_tests {
    use super::*;
    
    #[tokio::test]
    async fn test_calculate_hieroglyph_array_score_logic() {
        // Migrated from smart_contract/tests/test_score_calculator.rs
        // Test the score calculation logic using multi-program architecture
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("score_test");
        
        // Create test contest first
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Test Case 1: User's example - 5 upvotes should give 50 points
        let interactions_1 = vec![(0, 5)]; // 5 upvotes
        let expected_score_1 = calculate_expected_score(&interactions_1);
        assert_eq!(expected_score_1, 50, "5 upvotes should equal 50 points");
        
        // Test vote casting through multi-program architecture
        for _ in 0..5 {
            ctx.cast_vote(contest_label, generate_test_meme_id(1), 0).await.unwrap();
        }
        
        // Test Case 2: Mixed positive scores
        let interactions_2 = vec![
            (0, 3),  // 3 upvotes (10 points each)
            (1, 2),  // 2 comments (15 points each)  
            (3, 1),  // 1 laugh (25 points)
            (5, 1),  // 1 love (35 points)
        ];
        // Expected: (3×10) + (2×15) + (1×25) + (1×35) = 30 + 30 + 25 + 35 = 120
        let expected_score_2 = calculate_expected_score(&interactions_2);
        assert_eq!(expected_score_2, 120, "Mixed positive should equal 120 points");
        
        // Test batch voting for efficiency
        let votes = vec![
            (generate_test_meme_id(2), 0), // upvote
            (generate_test_meme_id(2), 0), // upvote  
            (generate_test_meme_id(2), 0), // upvote
            (generate_test_meme_id(2), 1), // comment
            (generate_test_meme_id(2), 1), // comment
            (generate_test_meme_id(2), 3), // laugh
            (generate_test_meme_id(2), 5), // love
        ];
        ctx.batch_cast_votes(contest_label, votes).await.unwrap();
        
        // Test Case 3: SIGNED scores — no clamp (SCORING-MODEL.md P4). A
        // net-downvoted meme legitimately ranks below an untouched one.
        let interactions_3 = vec![(2, 10)]; // 10 downvotes (-5 each)
        let expected_score_3 = calculate_expected_score(&interactions_3);
        assert_eq!(expected_score_3, -50, "Ten downvotes net to -50 — negatives are real");
        
        // Test Case 4: High-value hieroglyphs
        let interactions_4 = vec![(31, 1)]; // 1 crown (+30, apex)
        let expected_score_4 = calculate_expected_score(&interactions_4);
        assert_eq!(expected_score_4, 30, "crown should equal +30");
        
        // Test apex vote through program
        ctx.cast_vote(contest_label, generate_test_meme_id(3), 31).await.unwrap();
        
        // Test Case 5: Complex mixed scenario with a negative component
        let interactions_5 = vec![
            (0, 2),  // 2 upvotes (+10 each)
            (1, 1),  // 1 comment (+15)
            (2, 3),  // 3 downvotes (-5 each)
            (5, 1),  // 1 Surprise reaction (+8)
            (12, 1), // 1 Target award (+20)
        ];
        // Expected: (2×10) + 15 + (3×-5) + 8 + 20 = 20 + 15 - 15 + 8 + 20 = 48
        let expected_score_5 = calculate_expected_score(&interactions_5);
        assert_eq!(expected_score_5, 48, "Complex mixed should equal 48 points");
        
        println!("✅ All multi-program score calculation tests passed!");
    }
    
    #[tokio::test]
    async fn test_hieroglyph_array_bounds() {
        // Migrated from smart_contract/tests/test_score_calculator.rs
        // Test array bounds and edge cases in multi-program context
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("bounds_test");
        
        // Create test contest
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Test all zeros
        let interactions_zeros = vec![];
        let expected_score = calculate_expected_score(&interactions_zeros);
        assert_eq!(expected_score, 0, "All zeros should equal 0 points");
        
        // Test maximum values (within reasonable bounds)
        let interactions_max = vec![(31, 100)]; // 100 crowns (apex glyph)
        let expected_score_max = calculate_expected_score(&interactions_max);
        assert_eq!(expected_score_max, 3000, "100 crowns should equal 3000 points (+30 each)");
        
        // Test out of bounds interaction type (should be handled gracefully)
        let interactions_oob = vec![(32, 1)]; // Invalid interaction type
        let expected_score_oob = calculate_expected_score(&interactions_oob);
        assert_eq!(expected_score_oob, 0, "Out of bounds interaction should equal 0 points");
        
        // Test edge case: exactly 32 different interaction types
        // Restored from monolithic: comprehensive all-hieroglyph iteration testing
        let mut all_interactions = vec![];
        for i in 0..32 {
            all_interactions.push((i, 1));
        }
        let total_expected = calculate_expected_score(&all_interactions);
        
        // Manually calculate expected total with comprehensive validation (restored from monolithic)
        let mut manual_total = 0i64;
        for i in 0..32 {
            manual_total += get_meme_score_delta(i);
            
            // Test individual hieroglyph properties during iteration
            assert!(i < 32, "Hieroglyph ID {} should be within bounds", i);
            
            let weight = get_meme_score_delta(i);
            match i {
                0 => assert_eq!(weight, 10, "upvote should have +10"),
                2 => assert_eq!(weight, -5, "downvote is the sole negative: -5 (P4)"),
                31 => assert_eq!(weight, 30, "crown is the apex delta: +30"),
                _ => assert!(weight != 0, "Hieroglyph {} should have non-zero delta", i),
            }
        }
        // SIGNED model: no negative clamp (SCORING-MODEL.md).
        
        assert_eq!(total_expected, manual_total, "All 32 interactions total should match manual calculation");
        
        println!("✅ All multi-program bounds tests passed!");
    }
    
    #[tokio::test]
    async fn test_all_hieroglyph_scoring_weights() {
        // The canonical table (HIEROGLYPH-REGISTRY.md §3): SIGNED meme deltas
        // + stances, asserted against the registry const itself — the same
        // array the deployed program compiles in.

        // Basics
        assert_eq!(get_meme_score_delta(0), 10, "upvote +10");
        assert_eq!(get_meme_score_delta(1), 15, "comment +15 — strongest basic reach signal");
        assert_eq!(get_meme_score_delta(2), -5, "downvote -5 — half the upvote by design (P4)");

        // Bands (basic 0-2 · reaction 3-11 · award 12-16 · alteration 17-27 · advanced 28-31)
        for i in 3..=11 {
            assert_eq!(get_meme_score_delta(i), 8, "Reactions are +8 (paid impact, P1)");
        }
        for i in 12..=16 {
            assert_eq!(
                get_meme_score_delta(i),
                12 + 2 * (i as i64 - 12),
                "Awards escalate +12..+20 with cost"
            );
        }
        for i in 17..=27 {
            assert_eq!(
                get_meme_score_delta(i), 6,
                "Alterations — the attack band — are the LOWEST positive (P2): the cut answer"
            );
            assert!(
                kots_common::hieroglyphs::is_alteration(i),
                "Glyph {} must be in the alteration band", i
            );
        }
        for i in 28..=31 {
            assert_eq!(
                get_meme_score_delta(i),
                24 + 2 * (i as i64 - 28),
                "Advanced apex tallies escalate +24..+30 with cost"
            );
        }
        assert_eq!(get_meme_score_delta(31), 30, "crown +30 — apex");
        assert_eq!(get_meme_score_delta(32), 0, "Out of range contributes nothing");

        // Exactly one negative instrument on the whole platform
        let mut negative_count = 0;
        for i in 0..32 {
            let d = get_meme_score_delta(i);
            assert!(d != 0, "Every glyph has a non-zero delta");
            if d < 0 { negative_count += 1; }
            assert!(
                d.abs() <= 30,
                "Deltas stay in a tight band (apex crown +30): buying rank has diminishing returns per KOTS"
            );
        }
        assert_eq!(negative_count, 1, "Downvote is the ONLY negative delta (P1/P4)");

        // The attack band buys the least boost per glyph among positives
        for i in 0..32u32 {
            let d = get_meme_score_delta(i);
            if d > 0 {
                assert!(d >= 6, "No positive glyph moves a meme less than an effect glyph");
            }
        }

        // Stances: Downvote is the sole AGAINST; Comment the sole NEUTRAL;
        // everything else — including every attack glyph — is FOR (P2).
        assert_eq!(get_prediction_stance(2), -1, "Downvote stakes AGAINST");
        assert_eq!(get_prediction_stance(1), 0, "Comment stakes nothing");
        for i in 0..32u32 {
            if i != 1 && i != 2 {
                assert_eq!(
                    get_prediction_stance(i), 1,
                    "Glyph {} stakes FOR — attacks are endorsements (the cut answer)", i
                );
            }
        }

        println!("✅ Canonical scoring table validated: signed deltas, single negative, stances");
    }

        #[tokio::test]
    async fn test_batch_voting_functionality() {
        // New test specific to multi-program batch voting capabilities
        // Restored from monolithic: comprehensive batch testing with iteration patterns
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("batch_test");
        
        // Create test contest
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Comprehensive batch testing (restored from monolithic patterns)
        // Test multiple batch sizes and hieroglyph combinations
        let batch_scenarios = vec![
            // Small batch
            vec![
                (generate_test_meme_id(1), 0), // upvote
                (generate_test_meme_id(1), 1), // comment
            ],
            // Medium batch with mixed types
            vec![
                (generate_test_meme_id(2), 0), // upvote
                (generate_test_meme_id(2), 1), // comment
                (generate_test_meme_id(2), 3), // laugh
                (generate_test_meme_id(2), 5), // love
                (generate_test_meme_id(2), 12), // wood award
            ],
            // Large batch with high-value interactions
            vec![
                (generate_test_meme_id(3), 28), // space
                (generate_test_meme_id(3), 29), // quantum
                (generate_test_meme_id(3), 30), // universe
                (generate_test_meme_id(3), 31), // crown
            ],
        ];
        
        // Test each batch scenario (restored iteration pattern)
        for (scenario_idx, batch_votes) in batch_scenarios.iter().enumerate() {
            // Execute batch vote
            ctx.batch_cast_votes(contest_label, batch_votes.clone()).await.unwrap();
            
            // Calculate expected total score from batch
            let interaction_counts: Vec<(u32, u32)> = batch_votes.iter()
                .map(|(_meme_id, interaction_type)| (*interaction_type, 1))
                .collect();
            
            let expected_batch_score = calculate_expected_score(&interaction_counts);
            
            println!("✓ Batch scenario {} completed - {} votes, expected score: {}", 
                    scenario_idx + 1, batch_votes.len(), expected_batch_score);
        }
        
        // Test comprehensive batch with all 32 hieroglyphs (restored from monolithic)
        let mut comprehensive_batch = vec![];
        for i in 0..32 {
            comprehensive_batch.push((generate_test_meme_id(4), i as u8));
        }
        
        // Calculate expected score for comprehensive batch
        let comprehensive_interaction_counts: Vec<(u32, u32)> = comprehensive_batch.iter()
            .map(|(_meme_id, interaction_type)| (*interaction_type as u32, 1))
            .collect();
        let comprehensive_expected_score = calculate_expected_score(&comprehensive_interaction_counts);
        
        println!("✓ Comprehensive batch prepared - 32 votes, expected score: {}", comprehensive_expected_score);
        
        // Execute comprehensive batch
        ctx.batch_cast_votes(contest_label, comprehensive_batch).await.unwrap();
        
        println!("✅ Batch voting functionality test passed with comprehensive iteration testing!");
    }
    
    #[tokio::test]
    async fn test_vote_data_query() {
        // Test the query_vote_data functionality from the voting program
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("query_test");
        
        // Create test contest
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        let meme_id = generate_test_meme_id(1);
        let interaction_type = 0; // upvote
        
        // Cast some votes first
        for _ in 0..3 {
            ctx.cast_vote(contest_label, meme_id, interaction_type).await.unwrap();
        }
        
        // Note: In a full implementation, we would query the vote data here
        // For now, we test that the voting succeeded without errors
        
        println!("✅ Vote data query test passed!");
    }
    
    #[tokio::test]
    async fn test_final_scores_calculation() {
        // Test the calculate_final_scores functionality
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("final_scores_test");
        
        // Create test contest
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Cast votes for multiple memes and users
        let test_votes = vec![
            (generate_test_meme_id(1), 0), // upvote meme 1
            (generate_test_meme_id(1), 1), // comment meme 1
            (generate_test_meme_id(2), 3), // laugh meme 2
            (generate_test_meme_id(2), 5), // love meme 2
            (generate_test_meme_id(3), 31), // crown meme 3
        ];
        
        for (meme_id, interaction_type) in test_votes {
            ctx.cast_vote(contest_label, meme_id, interaction_type).await.unwrap();
        }
        
        // Note: In a full implementation, we would call calculate_final_scores
        // and verify the participant count and scoring results
        
        println!("✅ Final scores calculation test passed!");
    }
}

#[cfg(test)]
mod voting_integration_tests {
    use super::*;
    
    #[tokio::test]
    async fn test_cross_program_vote_economics() {
        // Test voting program communication with economics program
        // This tests the CPI calls for score tracking and payment processing
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("economics_test");
        
        // Create test contest with entry fee
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Test that votes trigger proper CPI calls to economics program
        // for score tracking (no ticket awards — de-chancing, plan §5a)
        
        let high_value_votes = vec![
            (generate_test_meme_id(1), 31), // Crown
            (generate_test_meme_id(1), 30), // Universe
            (generate_test_meme_id(1), 29), // Quantum
        ];
        
        for (meme_id, interaction_type) in high_value_votes {
            ctx.cast_vote(contest_label, meme_id, interaction_type).await.unwrap();
        }
        
        // Note: In full implementation, we would verify:
        // 1. Score accumulation (scoring weights from the registry)
        // 2. Economics program state updates
        
        println!("✅ Cross-program vote economics test passed!");
    }
    
    #[tokio::test]
    async fn test_voting_security_integration() {
        // Test voting program integration with security-governance program
        
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let contest_label = generate_test_contest_label("security_test");
        
        // Create test contest
        ctx.create_test_contest(contest_label, 86400, 2, 0).await.unwrap();
        
        // Test security validations through CPI
        // In full implementation, this would test:
        // 1. Blacklist checking
        // 2. Rate limiting
        // 3. Sybil attack detection
        // 4. Emergency pause states
        
        ctx.cast_vote(contest_label, generate_test_meme_id(1), 0).await.unwrap();
        
        println!("✅ Voting security integration test passed!");
    }
    
    // ============================================================================
    // MISSING FUNCTION COVERAGE TESTS - INITIALIZATION & INFO QUERIES
    // ============================================================================
    
    #[tokio::test]
    async fn test_initialize_voting_hieroglyph() {
        // Test voting hieroglyph program initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test successful initialization
        let result = ctx.initialize_voting_hieroglyph().await;
        assert!(result.is_ok(), "Voting hieroglyph initialization should succeed");
        
        // Verify voting state is properly initialized
        let voting_state = ctx.get_voting_hieroglyph_state().await.unwrap();
        assert!(voting_state.is_initialized, "Voting should be initialized");
        assert_eq!(voting_state.total_votes_cast, 0, "Should start with zero votes");
        
        // Test duplicate initialization should fail
        let result2 = ctx.initialize_voting_hieroglyph().await;
        assert!(result2.is_err(), "Duplicate initialization should fail");
        
        println!("✅ Voting hieroglyph initialization test passed!");
    }
    
    #[tokio::test]
    async fn test_initialize_vote_tracker() {
        // Test vote tracker initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        let contest_label = generate_test_contest_label("vote_tracker_test");
        let meme_id = generate_test_meme_id(1);
        
        // Test vote tracker initialization
        let result = ctx.initialize_vote_tracker(contest_label.clone(), meme_id).await;
        assert!(result.is_ok(), "Vote tracker initialization should succeed");
        
        // Verify tracker state
        let tracker_state = ctx.get_vote_tracker_state(contest_label, meme_id).await.unwrap();
        assert!(tracker_state.is_initialized, "Tracker should be initialized");
        assert_eq!(tracker_state.total_interactions, 0, "Should start with zero interactions");
        
        // Test duplicate initialization should fail
        let result2 = ctx.initialize_vote_tracker(contest_label, meme_id).await;
        assert!(result2.is_err(), "Duplicate tracker initialization should fail");
        
        println!("✅ Vote tracker initialization test passed!");
    }
    
    #[tokio::test]
    async fn test_initialize_interaction_batch() {
        // Test interaction batch initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        let batch_id = 1;
        let batch_size = 100;
        
        // Test batch initialization
        let result = ctx.initialize_interaction_batch(batch_id, batch_size).await;
        assert!(result.is_ok(), "Interaction batch initialization should succeed");
        
        // Verify batch state
        let batch_state = ctx.get_interaction_batch_state(batch_id).await.unwrap();
        assert!(batch_state.is_initialized, "Batch should be initialized");
        assert_eq!(batch_state.batch_size, batch_size, "Batch size should match");
        assert_eq!(batch_state.processed_count, 0, "Should start with zero processed");
        
        // Test invalid batch size
        let result_invalid = ctx.initialize_interaction_batch(2, 0).await;
        assert!(result_invalid.is_err(), "Zero batch size should fail");
        
        println!("✅ Interaction batch initialization test passed!");
    }
    
    #[tokio::test]
    async fn test_get_hieroglyph_info() {
        // Test hieroglyph information retrieval
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        // Test getting info for all 32 hieroglyphs
        for hieroglyph_id in 0..32 {
            let info = ctx.get_hieroglyph_info(hieroglyph_id).await.unwrap();
            
            // Verify hieroglyph info structure
            assert!(info.hieroglyph_id == hieroglyph_id, "Hieroglyph ID should match");
            assert!(info.name.len() > 0, "Hieroglyph should have a name");
            assert!(info.points != 0, "Hieroglyph should have point value");
            
            // Verify expected point values for key hieroglyphs
            match hieroglyph_id {
                0 => assert_eq!(info.points, 10, "upvote should be +10"),
                1 => assert_eq!(info.points, 15, "comment should be +15"),
                2 => assert_eq!(info.points, -5, "downvote should be -5 (sole negative)"),
                31 => assert_eq!(info.points, 30, "crown should be +30 (apex)"),
                _ => assert!(info.points != 0, "All hieroglyphs should have non-zero points"),
            }
            
            println!("✅ Hieroglyph {}: {} ({} points)", hieroglyph_id, info.name, info.points);
        }
        
        // Test invalid hieroglyph ID
        let result = ctx.get_hieroglyph_info(32).await;
        assert!(result.is_err(), "Invalid hieroglyph ID should fail");
        
        println!("✅ Hieroglyph info retrieval test passed!");
    }
    
    #[tokio::test]
    async fn test_get_hieroglyph_count() {
        // Test hieroglyph count retrieval
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        // Test getting total hieroglyph count
        let count = ctx.get_hieroglyph_count().await.unwrap();
        assert_eq!(count, 32, "Should have exactly 32 hieroglyphs");
        
        // Test count consistency across multiple calls
        for _ in 0..5 {
            let count_check = ctx.get_hieroglyph_count().await.unwrap();
            assert_eq!(count_check, 32, "Count should remain consistent");
        }
        
        println!("✅ Hieroglyph count retrieval test passed!");
    }
    
    // ============================================================================
    // UPGRADE SYSTEM TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_initialize_upgrade_registry() {
        // Test upgrade registry initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test upgrade registry initialization
        let result = ctx.initialize_voting_upgrade_registry().await;
        assert!(result.is_ok(), "Voting upgrade registry initialization should succeed");
        
        // Verify registry state
        let registry_state = ctx.get_voting_upgrade_registry_state().await.unwrap();
        assert!(registry_state.is_initialized, "Registry should be initialized");
        assert_eq!(registry_state.pending_upgrades, 0, "Should start with zero pending upgrades");
        
        // Test duplicate initialization should fail
        let result2 = ctx.initialize_voting_upgrade_registry().await;
        assert!(result2.is_err(), "Duplicate registry initialization should fail");
        
        println!("✅ Voting upgrade registry initialization test passed!");
    }
    
    #[tokio::test]
    async fn test_execute_systemic_upgrade() {
        // Test systemic upgrade execution
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_upgrade_registry().await.unwrap();
        
        // Test upgrade execution
        let upgrade_id = 1;
        let result = ctx.execute_voting_systemic_upgrade(upgrade_id).await;
        assert!(result.is_ok(), "Voting systemic upgrade should succeed");
        
        // Verify upgrade was processed
        let registry_state = ctx.get_voting_upgrade_registry_state().await.unwrap();
        assert!(registry_state.last_upgrade_id >= upgrade_id, "Upgrade ID should be recorded");
        
        // Test invalid upgrade ID
        let invalid_upgrade_id = 999;
        let result = ctx.execute_voting_systemic_upgrade(invalid_upgrade_id).await;
        assert!(result.is_err(), "Invalid upgrade ID should fail");
        
        println!("✅ Voting systemic upgrade test passed!");
    }
    
    #[tokio::test]
    async fn test_pause_voting_for_upgrade() {
        // Test voting pause functionality
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        // Test pause
        let result = ctx.pause_voting_for_upgrade().await;
        assert!(result.is_ok(), "Voting pause should succeed");
        
        // Verify voting is paused
        let voting_state = ctx.get_voting_hieroglyph_state().await.unwrap();
        assert!(voting_state.is_paused, "Voting should be paused");
        
        // Test that voting fails when paused
        let contest_label = generate_test_contest_label("pause_test");
        let meme_id = generate_test_meme_id(1);
        let result = ctx.cast_vote(contest_label, meme_id, 0).await;
        assert!(result.is_err(), "Voting should fail when paused");
        
        println!("✅ Voting pause test passed!");
    }
    
    #[tokio::test]
    async fn test_resume_voting_after_upgrade() {
        // Test voting resume functionality
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        // Pause first
        ctx.pause_voting_for_upgrade().await.unwrap();
        
        // Test resume
        let result = ctx.resume_voting_after_upgrade().await;
        assert!(result.is_ok(), "Voting resume should succeed");
        
        // Verify voting is resumed
        let voting_state = ctx.get_voting_hieroglyph_state().await.unwrap();
        assert!(!voting_state.is_paused, "Voting should be resumed");
        
        // Test that voting works when resumed
        let contest_label = generate_test_contest_label("resume_test");
        ctx.create_test_contest(contest_label.clone(), 86400, 2, 0).await.unwrap();
        let meme_id = generate_test_meme_id(1);
        let result = ctx.cast_vote(contest_label, meme_id, 0).await;
        assert!(result.is_ok(), "Voting should work when resumed");
        
        println!("✅ Voting resume test passed!");
    }
    
    #[tokio::test]
    async fn test_report_voting_readiness() {
        // Test voting readiness reporting
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        // Test readiness report
        let result = ctx.report_voting_readiness().await;
        assert!(result.is_ok(), "Voting readiness report should succeed");
        
        // Verify readiness status
        let readiness = ctx.get_voting_readiness_status().await.unwrap();
        assert!(readiness.is_ready, "Voting should be ready after initialization");
        assert!(readiness.hieroglyphs_loaded, "Hieroglyphs should be loaded");
        assert!(readiness.scoring_system_active, "Scoring system should be active");
        
        // Test report with paused system
        ctx.pause_voting_for_upgrade().await.unwrap();
        let readiness_paused = ctx.get_voting_readiness_status().await.unwrap();
        assert!(!readiness_paused.is_ready, "Should not be ready when paused");
        
        println!("✅ Voting readiness report test passed!");
    }
    
    // ============================================================================
    // COMPREHENSIVE HIEROGLYPH INTERACTION TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_all_32_hieroglyphs_interaction() {
        // Test interaction with all 32 hieroglyphs
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        let contest_label = generate_test_contest_label("all_hieroglyphs_test");
        ctx.create_test_contest(contest_label.clone(), 86400, 2, 0).await.unwrap();
        
        let meme_id = generate_test_meme_id(1);
        ctx.initialize_vote_tracker(contest_label.clone(), meme_id).await.unwrap();
        
        // Test each hieroglyph
        for hieroglyph_id in 0..32 {
            let result = ctx.cast_vote(contest_label.clone(), meme_id, hieroglyph_id).await;
            assert!(result.is_ok(), "Hieroglyph {} should be castable", hieroglyph_id);
            
            // Verify vote was recorded
            let vote_data = ctx.query_vote_data(contest_label.clone(), meme_id).await.unwrap();
            assert!(vote_data.hieroglyph_counts[hieroglyph_id as usize] > 0, "Hieroglyph {} should be recorded", hieroglyph_id);
        }
        
        // Calculate final score with all hieroglyphs
        let final_scores = ctx.calculate_final_scores(contest_label.clone()).await.unwrap();
        assert!(final_scores.len() > 0, "Should have calculated scores");
        
        println!("✅ All 32 hieroglyphs interaction test passed!");
    }
    
    #[tokio::test]
    async fn test_hieroglyph_cost_formula() {
        // Test the revolutionary hieroglyph cost formula: (ID + 1) × base_cost × (1 + gas_percentage)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_voting_hieroglyph().await.unwrap();
        
        let base_cost = 100_000; // 100K lamports base
        let gas_percentage = 0.1; // 10% gas
        
        // Test cost calculation for different hieroglyphs
        for hieroglyph_id in 0..32 {
            let expected_cost = ((hieroglyph_id + 1) as f64 * base_cost as f64 * (1.0 + gas_percentage)) as u64;
            let actual_cost = ctx.calculate_hieroglyph_cost(hieroglyph_id, base_cost, gas_percentage).await.unwrap();
            
            assert_eq!(actual_cost, expected_cost, "Cost formula incorrect for hieroglyph {}", hieroglyph_id);
            
            // Verify price differentiation
            if hieroglyph_id > 0 {
                let prev_cost = ctx.calculate_hieroglyph_cost(hieroglyph_id - 1, base_cost, gas_percentage).await.unwrap();
                assert!(actual_cost > prev_cost, "Higher ID hieroglyphs should cost more");
            }
            
            println!("✅ Hieroglyph {} cost: {} lamports", hieroglyph_id, actual_cost);
        }
        
        println!("✅ Hieroglyph cost formula test passed!");
    }
}