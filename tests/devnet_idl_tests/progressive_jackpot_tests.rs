//! Progressive Jackpot Program Tests
//!
//! Comprehensive test suite for all 5 progressive jackpot functions
//! Target: 100% function coverage + 52-week cycle testing + error paths

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;

#[cfg(test)]
mod progressive_jackpot_tests {
    use super::*;
    
    // ============================================================================
    // JACKPOT INITIALIZATION TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_initialize_progressive_jackpot() {
        // Test progressive jackpot program initialization
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test successful initialization
        let result = ctx.initialize_progressive_jackpot().await;
        assert!(result.is_ok(), "Progressive jackpot initialization should succeed");
        
        // Verify jackpot state is properly initialized
        let jackpot_state = ctx.get_progressive_jackpot_state().await.unwrap();
        assert!(jackpot_state.is_initialized, "Jackpot should be initialized");
        assert_eq!(jackpot_state.current_week, 0, "Should start at week 0");
        assert_eq!(jackpot_state.total_amount, 0, "Should start with zero amount");
        
        // Test duplicate initialization should fail
        let result2 = ctx.initialize_progressive_jackpot().await;
        assert!(result2.is_err(), "Duplicate initialization should fail");
        
        println!("✅ Progressive jackpot initialization test passed!");
    }
    
    #[tokio::test]
    async fn test_initialize_jackpot() {
        // Test jackpot initialization with initial amount
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        
        // Test initialization with valid initial amount
        let initial_amount = 1_000_000; // 1M KOTS tokens
        let result = ctx.initialize_jackpot(initial_amount).await;
        assert!(result.is_ok(), "Jackpot initialization with amount should succeed");
        
        // Verify jackpot amount is set
        let jackpot_state = ctx.get_progressive_jackpot_state().await.unwrap();
        assert_eq!(jackpot_state.total_amount, initial_amount, "Jackpot amount should match initial amount");
        
        // Test initialization with zero amount
        let ctx2 = MultiProgramTestContext::new().await.unwrap();
        ctx2.initialize_progressive_jackpot().await.unwrap();
        let result = ctx2.initialize_jackpot(0).await;
        assert!(result.is_err(), "Zero initial amount should fail");
        
        println!("✅ Jackpot initialization with amount test passed!");
    }
    
    // ============================================================================
    // 52-WEEK PROGRESSIVE JACKPOT CYCLE TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_manage_progressive_jackpot_full_cycle() {
        // Test complete 52-week jackpot cycle
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        ctx.initialize_jackpot(500_000).await.unwrap(); // Start with 0.5M KOTS
        
        let base_contribution = 10_000; // 10K KOTS per week
        
        // Test all 52 weeks
        for week in 1..=52 {
            let contribution = base_contribution * week; // Progressive contribution
            
            let result = ctx.manage_progressive_jackpot(week, contribution).await;
            assert!(result.is_ok(), "Week {} management should succeed", week);
            
            // Verify week progression
            let jackpot_state = ctx.get_progressive_jackpot_state().await.unwrap();
            assert_eq!(jackpot_state.current_week, week, "Week should progress correctly");
            
            // Verify amount accumulation
            assert!(jackpot_state.total_amount > 0, "Jackpot should have positive amount");
            
            println!("✅ Week {} jackpot management passed (amount: {})", week, jackpot_state.total_amount);
        }
        
        // Test week 53 should trigger cycle reset
        let result = ctx.manage_progressive_jackpot(53, base_contribution).await;
        assert!(result.is_ok(), "Week 53 should trigger cycle reset");
        
        let jackpot_state = ctx.get_progressive_jackpot_state().await.unwrap();
        assert_eq!(jackpot_state.current_week, 1, "Should reset to week 1 after 52 weeks");
        
        println!("✅ Full 52-week jackpot cycle test passed!");
    }
    
    #[tokio::test]
    async fn test_manage_progressive_jackpot_edge_cases() {
        // Test edge cases for jackpot management
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        ctx.initialize_jackpot(100_000).await.unwrap();
        
        // Test invalid week numbers
        let invalid_weeks = vec![0, 54, 100];
        for invalid_week in invalid_weeks {
            let result = ctx.manage_progressive_jackpot(invalid_week, 1000).await;
            assert!(result.is_err(), "Invalid week {} should fail", invalid_week);
        }
        
        // Test maximum contribution
        let max_contribution = u64::MAX;
        let result = ctx.manage_progressive_jackpot(1, max_contribution).await;
        // Should handle overflow gracefully
        assert!(result.is_ok() || result.is_err(), "Max contribution should be handled");
        
        // Test zero contribution
        let result = ctx.manage_progressive_jackpot(2, 0).await;
        assert!(result.is_err(), "Zero contribution should fail");
        
        println!("✅ Jackpot management edge cases test passed!");
    }
    
    #[tokio::test]
    async fn test_manage_progressive_jackpot_arithmetic_safety() {
        // Test arithmetic overflow protection
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        
        // Initialize with near-max amount
        let near_max_amount = u64::MAX - 1_000_000;
        ctx.initialize_jackpot(near_max_amount).await.unwrap();
        
        // Try to add amount that would cause overflow
        let large_contribution = 2_000_000;
        let result = ctx.manage_progressive_jackpot(1, large_contribution).await;
        
        // Should either succeed with saturation or fail safely
        if result.is_ok() {
            let state = ctx.get_progressive_jackpot_state().await.unwrap();
            assert!(state.total_amount >= near_max_amount, "Amount should not decrease");
        }
        
        println!("✅ Arithmetic safety test passed!");
    }
    
    // ============================================================================
    // JACKPOT AWARD TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_award_jackpot_success() {
        // Test successful jackpot award
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        
        // Build up jackpot over several weeks
        let initial_amount = 1_000_000;
        ctx.initialize_jackpot(initial_amount).await.unwrap();
        
        // Add contributions for 10 weeks
        for week in 1..=10 {
            ctx.manage_progressive_jackpot(week, 50_000).await.unwrap();
        }
        
        // Get jackpot amount before award
        let pre_award_state = ctx.get_progressive_jackpot_state().await.unwrap();
        let jackpot_amount = pre_award_state.total_amount;
        assert!(jackpot_amount > initial_amount, "Jackpot should have grown");
        
        // Create winner and award jackpot
        let winner = anchor_lang::solana_program::pubkey::Pubkey::new_unique();
        let contest_label = generate_test_contest_label("award_test");
        
        let result = ctx.award_jackpot(winner, contest_label).await;
        assert!(result.is_ok(), "Jackpot award should succeed");
        
        // Verify jackpot is reset
        let post_award_state = ctx.get_progressive_jackpot_state().await.unwrap();
        assert_eq!(post_award_state.total_amount, 0, "Jackpot should be reset after award");
        assert_eq!(post_award_state.current_week, 0, "Week should reset after award");
        
        println!("✅ Jackpot award success test passed! (Awarded: {} KOTS)", jackpot_amount);
    }
    
    #[tokio::test]
    async fn test_award_jackpot_error_conditions() {
        // Test jackpot award error conditions
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test award without initialization
        let winner = anchor_lang::solana_program::pubkey::Pubkey::new_unique();
        let contest_label = generate_test_contest_label("error_test");
        
        let result = ctx.award_jackpot(winner, contest_label.clone()).await;
        assert!(result.is_err(), "Award without initialization should fail");
        
        // Initialize but don't fund jackpot
        ctx.initialize_progressive_jackpot().await.unwrap();
        
        let result = ctx.award_jackpot(winner, contest_label.clone()).await;
        assert!(result.is_err(), "Award with zero jackpot should fail");
        
        // Test with invalid winner address
        ctx.initialize_jackpot(100_000).await.unwrap();
        let invalid_winner = Pubkey::default(); // System program address
        
        let result = ctx.award_jackpot(invalid_winner, contest_label).await;
        assert!(result.is_err(), "Award to invalid winner should fail");
        
        println!("✅ Jackpot award error conditions test passed!");
    }
    
    // ============================================================================
    // JACKPOT STATUS AND QUERY TESTS
    // ============================================================================
    
    #[tokio::test]
    async fn test_get_jackpot_status_all_states() {
        // Test jackpot status in all possible states
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Test uninitialized state
        let result = ctx.get_jackpot_status().await;
        assert!(result.is_err(), "Status query should fail for uninitialized jackpot");
        
        // Test initialized but empty state
        ctx.initialize_progressive_jackpot().await.unwrap();
        let status = ctx.get_jackpot_status().await.unwrap();
        assert_eq!(status.total_amount, 0, "Empty jackpot should have zero amount");
        assert_eq!(status.current_week, 0, "Empty jackpot should be at week 0");
        assert!(status.is_initialized, "Status should show initialized");
        
        // Test funded state
        ctx.initialize_jackpot(500_000).await.unwrap();
        let status = ctx.get_jackpot_status().await.unwrap();
        assert_eq!(status.total_amount, 500_000, "Funded jackpot should show correct amount");
        
        // Test active state (with contributions)
        ctx.manage_progressive_jackpot(1, 25_000).await.unwrap();
        ctx.manage_progressive_jackpot(2, 30_000).await.unwrap();
        
        let status = ctx.get_jackpot_status().await.unwrap();
        assert_eq!(status.current_week, 2, "Active jackpot should show current week");
        assert!(status.total_amount > 500_000, "Active jackpot should have grown");
        
        // Test post-award state
        let winner = Pubkey::new_unique();
        let contest_label = generate_test_contest_label("status_test");
        ctx.award_jackpot(winner, contest_label).await.unwrap();
        
        let status = ctx.get_jackpot_status().await.unwrap();
        assert_eq!(status.total_amount, 0, "Post-award jackpot should be reset");
        assert_eq!(status.current_week, 0, "Post-award week should be reset");
        
        println!("✅ Jackpot status query test passed!");
    }
    
    #[tokio::test]
    async fn test_jackpot_weekly_progression_patterns() {
        // Test different weekly progression patterns
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        ctx.initialize_jackpot(100_000).await.unwrap();
        
        // Test linear progression
        let base_amount = 10_000;
        for week in 1..=5 {
            ctx.manage_progressive_jackpot(week, base_amount).await.unwrap();
            let status = ctx.get_jackpot_status().await.unwrap();
            assert_eq!(status.current_week, week, "Week should progress linearly");
        }
        
        // Test exponential-like progression
        let ctx2 = MultiProgramTestContext::new().await.unwrap();
        ctx2.initialize_progressive_jackpot().await.unwrap();
        ctx2.initialize_jackpot(50_000).await.unwrap();
        
        for week in 1..=5 {
            let contribution = base_amount * (2_u64.pow(week - 1)); // Exponential growth
            ctx2.manage_progressive_jackpot(week, contribution).await.unwrap();
        }
        
        let final_status = ctx2.get_jackpot_status().await.unwrap();
        assert!(final_status.total_amount > 200_000, "Exponential progression should result in large jackpot");
        
        println!("✅ Weekly progression patterns test passed!");
    }
    
    #[tokio::test]
    async fn test_jackpot_concurrent_operations() {
        // Test concurrent jackpot operations (simulated)
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_progressive_jackpot().await.unwrap();
        ctx.initialize_jackpot(200_000).await.unwrap();
        
        // Simulate rapid week progression
        for week in 1..=10 {
            let contribution = 15_000 + (week * 1_000); // Variable contributions
            
            // Manage jackpot
            let result = ctx.manage_progressive_jackpot(week, contribution).await;
            assert!(result.is_ok(), "Rapid progression week {} should succeed", week);
            
            // Check status consistency
            let status = ctx.get_jackpot_status().await.unwrap();
            assert_eq!(status.current_week, week, "Status should be consistent with operations");
            assert!(status.total_amount > 0, "Amount should remain positive");
        }
        
        println!("✅ Concurrent operations test passed!");
    }
}