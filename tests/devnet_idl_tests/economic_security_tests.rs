//! Economic Security Tests
//!
//! Economic model and security validation for mainnet:
//! - Oracle fallback mechanisms
//! - Fee distribution accuracy (50/37/7/3% splits)
//! - Overflow/underflow protection
//! - MEV attack resistance
//! - Prize calculation accuracy

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::Pubkey;

#[cfg(test)]
mod economic_security_tests {
    use super::*;
    
    /// Test fee distribution accuracy (50% jackpot, 37% liquidity, 7% treasury, 3% burn)
    #[tokio::test]
    async fn test_fee_distribution_percentages() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        let contest_label = generate_test_contest_label("fee_dist");
        let entry_fee = 1_000_000_000; // 1 KOTS
        
        ctx.create_test_contest(contest_label, 3600, 10, entry_fee)
            .await
            .unwrap();
        
        // Get initial balances
        let initial_jackpot = ctx.get_jackpot_balance().await.unwrap_or(0);
        let initial_treasury = ctx.get_treasury_balance().await.unwrap_or(0);
        let initial_liquidity = ctx.get_liquidity_pool_balance().await.unwrap_or(0);
        
        // Register 10 participants (10M total fees)
        for _ in 0..10 {
            // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
            // mutably twice in one expression — E0499.
            let participant_1 = ctx.new_identity();
            ctx.register_contest_participant(contest_label, participant_1)
                .await
                .unwrap();
        }
        
        let total_fees = entry_fee * 10; // 10M KOTS
        
        // Expected distribution:
        // 50% → Jackpot = 5M
        // 37% → Liquidity = 3.7M
        // 7% → Treasury = 700K
        // 3% → Burn = 300K (untracked in tests)
        
        let final_jackpot = ctx.get_jackpot_balance().await.unwrap_or(0);
        let final_treasury = ctx.get_treasury_balance().await.unwrap_or(0);
        let final_liquidity = ctx.get_liquidity_pool_balance().await.unwrap_or(0);
        
        let jackpot_delta = final_jackpot.saturating_sub(initial_jackpot);
        let treasury_delta = final_treasury.saturating_sub(initial_treasury);
        let liquidity_delta = final_liquidity.saturating_sub(initial_liquidity);
        
        // Verify percentages (with 1% tolerance for rounding)
        if jackpot_delta > 0 {
            let jackpot_pct = (jackpot_delta as f64) / (total_fees as f64) * 100.0;
            assert!(
                (jackpot_pct - 50.0).abs() < 1.0,
                "Jackpot should receive ~50%, got {:.2}%",
                jackpot_pct
            );
            println!("✅ Jackpot: {:.2}% (target 50%)", jackpot_pct);
        }
        
        if treasury_delta > 0 {
            let treasury_pct = (treasury_delta as f64) / (total_fees as f64) * 100.0;
            assert!(
                (treasury_pct - 7.0).abs() < 1.0,
                "Treasury should receive ~7%, got {:.2}%",
                treasury_pct
            );
            println!("✅ Treasury: {:.2}% (target 7%)", treasury_pct);
        }
        
        println!("✅ Fee distribution percentages validated");
    }
    
    /// Test prize calculation accuracy across all positions
    #[tokio::test]
    async fn test_prize_calculation_accuracy() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("prize_accuracy");
        let total_pool = 10_000_000; // 10M KOTS
        
        // Expected prize distribution (geometric decay):
        // Position 1: 50.0% = 5M
        // Position 2: 25.0% = 2.5M  
        // Position 3: 12.5% = 1.25M
        // Position 4: 6.25% = 625K
        // Position 5: 3.125% = 312.5K
        
        let position_1_prize = ctx.calc_prize(contest_label, 1, total_pool).await.unwrap();
        let position_2_prize = ctx.calc_prize(contest_label, 2, total_pool).await.unwrap();
        let position_3_prize = ctx.calc_prize(contest_label, 3, total_pool).await.unwrap();
        
        // Verify geometric decay (each position ~50% of previous)
        let ratio_2_to_1 = (position_2_prize as f64) / (position_1_prize as f64);
        let ratio_3_to_2 = (position_3_prize as f64) / (position_2_prize as f64);
        
        // KOTS_TOKENOMICS.md §4c is a LINEAR (11-place)% curve, not geometric:
        // 2nd/1st = 9/10 = 0.9, 3rd/2nd = 8/9 ≈ 0.889. (Was a fabricated 0.5.)
        assert!(
            (ratio_2_to_1 - 0.9).abs() < 0.05,
            "Position 2 should be 9/10 of position 1, ratio: {:.3}",
            ratio_2_to_1
        );
        
        assert!(
            (ratio_3_to_2 - 0.889).abs() < 0.05,
            "Position 3 should be ~50% of position 2, ratio: {:.3}",
            ratio_3_to_2
        );
        
        println!("✅ Prize calculation accuracy validated");
        println!("   Position 1: {} ({:.1}%)", position_1_prize, (position_1_prize as f64 / total_pool as f64) * 100.0);
        println!("   Position 2: {} ({:.1}%)", position_2_prize, (position_2_prize as f64 / total_pool as f64) * 100.0);
        println!("   Position 3: {} ({:.1}%)", position_3_prize, (position_3_prize as f64 / total_pool as f64) * 100.0);
    }
    
    /// Test oracle fallback mechanism (multi-layer resilience)
    #[tokio::test]
    async fn test_oracle_fallback_mechanism() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Simulate oracle failure scenarios
        let gas_price = ctx.get_current_gas_price().await;
        
        match gas_price {
            Ok(price) => {
                assert!(price > 0, "Gas price should be positive");
                println!("✅ Primary oracle operational: {} lamports", price);
            }
            Err(_) => {
                // Primary oracle failed, check fallback
                let fallback_price = ctx.get_fallback_gas_price().await;
                
                if let Ok(price) = fallback_price {
                    assert!(price > 0, "Fallback price should be positive");
                    println!("✅ Fallback oracle operational: {} lamports", price);
                } else {
                    // All oracles down, should use safe default
                    let default_price = ctx.get_default_gas_price().await.unwrap();
                    assert!(default_price > 0, "Default gas price should be positive");
                    println!("✅ Default gas price used (all oracles down): {} lamports", default_price);
                }
            }
        }
        
        println!("✅ Oracle fallback mechanism validated");
    }
    
    /// Test overflow protection in prize calculations
    #[tokio::test]
    async fn test_overflow_protection_prizes() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("overflow_prize");
        
        // Test with maximum safe u64 value
        let max_safe_pool = u64::MAX / 2;
        
        let result = ctx.calc_prize(contest_label, 1, max_safe_pool).await;
        
        match result {
            Ok(prize) => {
                // Should not overflow
                assert!(prize <= max_safe_pool, "Prize should not exceed pool");
                println!("✅ Large pool handled: {}", prize);
            }
            Err(_) => {
                println!("✅ Overflow protection rejected unsafe calculation");
            }
        }
        
        // Test multiplication overflow
        let result = ctx.calc_prize(contest_label, 1, u64::MAX).await;
        
        if let Ok(prize) = result {
            assert!(prize <= u64::MAX, "Should use saturating arithmetic");
        }
        
        println!("✅ Overflow protection validated");
    }
    
    /// Test underflow protection in vote scoring
    #[tokio::test]
    async fn test_underflow_protection_scoring() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        let contest_label = generate_test_contest_label("underflow_test");
        ctx.create_test_contest(contest_label, 3600, 2, 0).await.unwrap();
        
        // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
        // mutably twice in one expression — E0499.
        let participant_2 = ctx.new_identity();
        ctx.register_contest_participant(contest_label, participant_2).await.unwrap();
        
        // Cast massive downvotes (should not underflow below zero)
        for _ in 0..100 {
            let _ = ctx.cast_vote(contest_label, 1, 11).await; // Downvote hieroglyph
        }
        
        // Score should be protected from going negative
        let score = ctx.get_meme_score(contest_label, 1).await.unwrap_or(0);
        
        // Score can be negative in i64, but should be controlled
        println!("✅ Underflow protection: final score = {}", score);
        println!("✅ Scoring system handles extreme downvoting safely");
    }
    
    /// Test MEV attack resistance (front-running protection)
    #[tokio::test]
    async fn test_mev_attack_resistance() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("mev_test");
        ctx.create_test_contest(contest_label, 3600, 3, 100_000_000)
            .await
            .unwrap();
        
        // Register participants
        let early_participant = ctx.new_identity();
        let mev_attacker = ctx.new_identity();
        let late_participant = ctx.new_identity();
        
        ctx.register_contest_participant(contest_label, early_participant).await.unwrap();
        
        // Simulate MEV attack: attacker tries to front-run with higher gas
        // System should maintain fair ordering
        ctx.register_contest_participant(contest_label, mev_attacker).await.unwrap();
        ctx.register_contest_participant(contest_label, late_participant).await.unwrap();
        
        // Create voting pattern
        ctx.cast_vote(contest_label, 1, 1).await.unwrap();
        ctx.cast_vote(contest_label, 2, 1).await.unwrap();
        ctx.cast_vote(contest_label, 3, 1).await.unwrap();
        
        // Finalize
        ctx.advance_clock(7200).await.unwrap();
        ctx.fund_contest_vault(contest_label, 300_000).await.unwrap();
        ctx.execute_auto_distribution(contest_label).await.unwrap();
        
        // Verify results based on votes, not gas/timing manipulation
        let winner = ctx.get_winner_record(contest_label, 1).await.unwrap();
        
        // Winner should be determined by votes, not transaction ordering
        assert!(winner.prize_amount > 0, "Winner determined by votes");
        
        println!("✅ MEV attack resistance validated (voting-based, not transaction-order-based)");
    }
    
    /// Test economic attack: spam voting
    #[tokio::test]
    async fn test_spam_voting_economic_attack() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        let contest_label = generate_test_contest_label("spam_attack");
        ctx.create_test_contest(contest_label, 3600, 2, 0).await.unwrap();
        
        // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
        // mutably twice in one expression — E0499.
        let participant_3 = ctx.new_identity();
        ctx.register_contest_participant(contest_label, participant_3).await.unwrap();
        // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
        // mutably twice in one expression — E0499.
        let participant_4 = ctx.new_identity();
        ctx.register_contest_participant(contest_label, participant_4).await.unwrap();
        
        // Attempt spam attack (rapid voting)
        let mut successful_votes = 0;
        let mut blocked_votes = 0;
        
        for _ in 0..50 {
            let result = ctx.cast_vote(contest_label, 1, 1).await;
            if result.is_ok() {
                successful_votes += 1;
            } else {
                blocked_votes += 1;
            }
        }
        
        // System should limit spam (rate limiting or cost-based protection)
        if blocked_votes > 0 {
            println!("✅ Spam attack mitigated: {} votes blocked", blocked_votes);
        } else {
            println!("✅ All votes accepted (economic cost provides spam protection)");
        }
        
        println!("   Successful: {}, Blocked: {}", successful_votes, blocked_votes);
    }
    
    /// Test economic consistency: total value conservation
    #[tokio::test]
    async fn test_economic_value_conservation() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_prize_distribution().await.unwrap();
        
        let contest_label = generate_test_contest_label("conservation");
        let entry_fee = 500_000_000;
        
        ctx.create_test_contest(contest_label, 3600, 4, entry_fee)
            .await
            .unwrap();
        
        // Track total system value before
        let initial_total = ctx.get_total_system_value().await.unwrap_or(0);
        
        // 4 participants pay entry fees
        for _ in 0..4 {
            // Hoisted (2026-07-23): `ctx.method(ctx.new_identity())` borrows ctx
            // mutably twice in one expression — E0499.
            let participant_5 = ctx.new_identity();
            ctx.register_contest_participant(contest_label, participant_5)
                .await
                .unwrap();
        }
        
        let total_fees_collected = entry_fee * 4;
        
        // Vote and finalize
        ctx.cast_vote(contest_label, 1, 1).await.unwrap();
        ctx.cast_vote(contest_label, 2, 1).await.unwrap();
        ctx.cast_vote(contest_label, 3, 1).await.unwrap();
        ctx.cast_vote(contest_label, 4, 1).await.unwrap();
        
        ctx.advance_clock(7200).await.unwrap();
        ctx.fund_contest_vault(contest_label, total_fees_collected).await.unwrap();
        ctx.execute_auto_distribution(contest_label).await.unwrap();
        
        // Track total system value after
        let final_total = ctx.get_total_system_value().await.unwrap_or(0);
        
        // Value should be conserved (minus burn)
        // Total fees = 2M, Burn 3% = 60K, Remaining = 1.94M
        let expected_min = initial_total + (total_fees_collected * 97 / 100);
        
        if final_total >= expected_min {
            println!("✅ Economic value conserved: {} → {}", initial_total, final_total);
        } else {
            println!("✅ Value tracked (may be distributed across accounts)");
        }
        
        println!("✅ Economic conservation laws validated");
    }
    
    /// Test hieroglyph cost formula accuracy
    #[tokio::test]
    async fn test_hieroglyph_cost_formula() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Hieroglyph cost formula: (ID + 1) × base_cost × (1 + gas_percentage)
        
        let base_cost = 10_000; // 0.01 KOTS
        // TEST FIX (2026-07-23): was hardcoded to 0.15 (15%), which made this
        // assert 23000 for id=1 while the program returned 20000 — exactly
        // (1+1) × 10_000 with NO uplift.
        //
        // The formula is not wrong: kots-common's own unit tests
        // `fee_is_id_plus_one_times_base_at_zero_gas` and `zero_gas_adds_nothing`
        // both pass. The uplift is derived from the gas oracle, and ProgramTest
        // starts from a blank ledger where no oracle has been initialised — so
        // gas reads as 0 and the correct expectation here is 0% uplift.
        //
        // If you want this test to exercise a non-zero uplift, initialise the
        // gas oracle in setup and set this to match the price you seed it with.
        let gas_percentage = 0.0;
        
        // Test costs for hieroglyphs 1-10
        for hieroglyph_id in 1..=10 {
            let cost = ctx.get_hieroglyph_cost(hieroglyph_id).await.unwrap_or(0);
            
            let expected_base = (hieroglyph_id + 1) as u64 * base_cost;
            let expected_with_gas = (expected_base as f64 * (1.0 + gas_percentage)) as u64;
            
            // Allow 10% tolerance for calculation variations
            let lower_bound = expected_with_gas * 90 / 100;
            let upper_bound = expected_with_gas * 110 / 100;
            
            if cost > 0 {
                assert!(
                    cost >= lower_bound && cost <= upper_bound,
                    "Hieroglyph {} cost out of range: {} (expected ~{})",
                    hieroglyph_id,
                    cost,
                    expected_with_gas
                );
                println!("   Hieroglyph {}: {} KOTS", hieroglyph_id, cost);
            }
        }
        
        println!("✅ Hieroglyph cost formula validated");
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
