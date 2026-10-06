//! Direct Fee-Management Function Tests
//!
//! Real instrumented tests that directly invoke each of the 12 public functions
//! with proper state assertions and error code validation

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;

#[cfg(test)]
mod direct_fee_tests {
    use super::*;
    
    // Direct tests for each of the 12 fee-management public functions
    
    #[tokio::test]
    async fn test_direct_initialize_fee_management() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Direct function invocation with state validation
        let result = fee_management::initialize_fee_management(
            ctx.as_context(),
            ctx.get_fee_management_accounts()
        ).await;
        
        assert!(result.is_ok(), "initialize_fee_management must succeed");
        
        // Validate state changes
        let state = ctx.get_fee_management_state().await.unwrap();
        assert!(state.is_initialized, "State must show initialized");
        assert_eq!(state.total_fees_collected, 0, "Must start with 0 fees collected");
        assert_eq!(state.total_fees_distributed, 0, "Must start with 0 fees distributed");
        
        // Test duplicate initialization
        let result = fee_management::initialize_fee_management(
            ctx.as_context(),
            ctx.get_fee_management_accounts()
        ).await;
        
        assert!(result.is_err(), "initialize_fee_management must fail for duplicate");
    }
    
    #[tokio::test]
    async fn test_direct_collect_transaction_fees() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let payer = Pubkey::new_unique();
        let transaction_type = "vote_cast";
        let fee_amount = 10_000u64; // 0.01 KOTS
        
        // Fund payer
        ctx.fund_account(&payer, fee_amount * 2).await.unwrap();
        
        // Direct function invocation
        let result = fee_management::collect_transaction_fees(
            ctx.as_context(),
            ctx.get_fee_collection_accounts(),
            payer,
            transaction_type,
            fee_amount
        ).await;
        
        assert!(result.is_ok(), "collect_transaction_fees must succeed");
        
        // Validate fee collection
        let fee_state = ctx.get_fee_management_state().await.unwrap();
        assert!(fee_state.total_fees_collected >= fee_amount, "Fees must be collected");
        
        // Validate transaction record
        let transaction_record = ctx.get_transaction_fee_record(payer, transaction_type).await.unwrap();
        assert_eq!(transaction_record.fee_amount, fee_amount, "Fee amount must match");
        assert_eq!(transaction_record.transaction_type, transaction_type, "Transaction type must match");
        
        // Test insufficient balance
        let unfunded_payer = Pubkey::new_unique();
        let result = fee_management::collect_transaction_fees(
            ctx.as_context(),
            ctx.get_fee_collection_accounts(),
            unfunded_payer,
            transaction_type,
            fee_amount
        ).await;
        
        assert!(result.is_err(), "collect_transaction_fees must fail for insufficient balance");
    }
    
    #[tokio::test]
    async fn test_direct_distribute_fees() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        // Collect fees first
        let payer = Pubkey::new_unique();
        ctx.fund_account(&payer, 1_000_000).await.unwrap();
        ctx.collect_transaction_fees(payer, "vote_cast", 100_000).await.unwrap();
        ctx.collect_transaction_fees(payer, "meme_upload", 200_000).await.unwrap();
        
        let distribution_type = "standard";
        
        // Direct function invocation
        let result = fee_management::distribute_fees(
            ctx.as_context(),
            ctx.get_fee_distribution_accounts(),
            distribution_type
        ).await;
        
        assert!(result.is_ok(), "distribute_fees must succeed");
        
        // Validate fee distribution
        let fee_state = ctx.get_fee_management_state().await.unwrap();
        assert!(fee_state.total_fees_distributed > 0, "Fees must be distributed");
        
        let distribution_record = ctx.get_fee_distribution_record(distribution_type).await.unwrap();
        assert!(distribution_record.amount_distributed > 0, "Distribution amount must be positive");
        assert!(distribution_record.timestamp > 0, "Must have timestamp");
        
        // Test distribution with no collected fees
        ctx.distribute_fees(distribution_type).await.unwrap(); // Clear fees
        
        let result = fee_management::distribute_fees(
            ctx.as_context(),
            ctx.get_fee_distribution_accounts(),
            distribution_type
        ).await;
        
        assert!(result.is_err(), "distribute_fees must fail with no collected fees");
    }
    
    #[tokio::test]
    async fn test_direct_calculate_dynamic_pricing() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let base_price = 50_000u64; // 0.05 KOTS
        let demand_factor = 150u32; // 1.5x demand
        let complexity_level = 3u8; // Medium complexity
        
        // Direct function invocation
        let calculated_price = fee_management::calculate_dynamic_pricing(
            ctx.as_context(),
            ctx.get_pricing_calculation_accounts(),
            base_price,
            demand_factor,
            complexity_level
        ).await.unwrap();
        
        // Validate pricing calculation
        assert!(calculated_price >= base_price, "Calculated price must be >= base price");
        assert!(calculated_price <= base_price * 5, "Calculated price must have reasonable upper bound");
        
        // Test different complexity levels
        for complexity in 1..=5 {
            let price = fee_management::calculate_dynamic_pricing(
                ctx.as_context(),
                ctx.get_pricing_calculation_accounts(),
                base_price,
                100, // Normal demand
                complexity
            ).await.unwrap();
            
            if complexity > 1 {
                let prev_price = fee_management::calculate_dynamic_pricing(
                    ctx.as_context(),
                    ctx.get_pricing_calculation_accounts(),
                    base_price,
                    100,
                    complexity - 1
                ).await.unwrap();
                
                assert!(price >= prev_price, "Higher complexity should cost more");
            }
        }
        
        // Test invalid parameters
        let result = fee_management::calculate_dynamic_pricing(
            ctx.as_context(),
            ctx.get_pricing_calculation_accounts(),
            0, // Invalid base price
            demand_factor,
            complexity_level
        ).await;
        
        assert!(result.is_err(), "calculate_dynamic_pricing must fail for zero base price");
    }
    
    #[tokio::test]
    async fn test_direct_process_hieroglyph_payment() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let user = Pubkey::new_unique();
        let hieroglyph_id = 5u8; // Love hieroglyph
        let payment_amount = 35_000u64; // Love costs 35 points worth
        
        // Fund user
        ctx.fund_account(&user, payment_amount * 2).await.unwrap();
        
        // Direct function invocation
        let result = fee_management::process_hieroglyph_payment(
            ctx.as_context(),
            ctx.get_hieroglyph_payment_accounts(),
            user,
            hieroglyph_id,
            payment_amount
        ).await;
        
        assert!(result.is_ok(), "process_hieroglyph_payment must succeed");
        
        // Validate payment processing
        let payment_record = ctx.get_hieroglyph_payment_record(user, hieroglyph_id).await.unwrap();
        assert_eq!(payment_record.hieroglyph_id, hieroglyph_id, "Hieroglyph ID must match");
        assert_eq!(payment_record.amount_paid, payment_amount, "Payment amount must match");
        assert!(payment_record.timestamp > 0, "Must have timestamp");
        
        // Test invalid hieroglyph
        let result = fee_management::process_hieroglyph_payment(
            ctx.as_context(),
            ctx.get_hieroglyph_payment_accounts(),
            user,
            32, // Invalid hieroglyph ID
            payment_amount
        ).await;
        
        assert!(result.is_err(), "process_hieroglyph_payment must fail for invalid hieroglyph");
        
        // Test insufficient payment
        let result = fee_management::process_hieroglyph_payment(
            ctx.as_context(),
            ctx.get_hieroglyph_payment_accounts(),
            user,
            hieroglyph_id,
            1_000 // Too low
        ).await;
        
        assert!(result.is_err(), "process_hieroglyph_payment must fail for insufficient payment");
    }
    
    #[tokio::test]
    async fn test_direct_update_fee_structure() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let fee_type = "vote_cast";
        let new_fee_amount = 15_000u64; // Updated fee
        let effective_timestamp = ctx.get_current_timestamp().await + 3600; // 1 hour from now
        
        // Direct function invocation
        let result = fee_management::update_fee_structure(
            ctx.as_context(),
            ctx.get_fee_structure_accounts(),
            fee_type,
            new_fee_amount,
            effective_timestamp
        ).await;
        
        assert!(result.is_ok(), "update_fee_structure must succeed");
        
        // Validate fee structure update
        let fee_structure = ctx.get_fee_structure(fee_type).await.unwrap();
        assert_eq!(fee_structure.fee_amount, new_fee_amount, "Fee amount must be updated");
        assert_eq!(fee_structure.effective_timestamp, effective_timestamp, "Effective timestamp must match");
        
        // Test invalid fee type
        let result = fee_management::update_fee_structure(
            ctx.as_context(),
            ctx.get_fee_structure_accounts(),
            "invalid_type",
            new_fee_amount,
            effective_timestamp
        ).await;
        
        assert!(result.is_err(), "update_fee_structure must fail for invalid fee type");
        
        // Test past effective timestamp
        let past_timestamp = ctx.get_current_timestamp().await - 3600; // 1 hour ago
        let result = fee_management::update_fee_structure(
            ctx.as_context(),
            ctx.get_fee_structure_accounts(),
            fee_type,
            new_fee_amount,
            past_timestamp
        ).await;
        
        assert!(result.is_err(), "update_fee_structure must fail for past timestamp");
    }
    
    #[tokio::test]
    async fn test_direct_get_fee_statistics() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        // Generate some fee activity
        let payer = Pubkey::new_unique();
        ctx.fund_account(&payer, 1_000_000).await.unwrap();
        
        for i in 0..5 {
            ctx.collect_transaction_fees(payer, "vote_cast", 10_000 + i * 1_000).await.unwrap();
        }
        
        let period_start = ctx.get_current_timestamp().await - 86400; // 24 hours ago
        let period_end = ctx.get_current_timestamp().await;
        
        // Direct function invocation
        let fee_stats = fee_management::get_fee_statistics(
            ctx.as_context(),
            ctx.get_fee_statistics_accounts(),
            period_start,
            period_end
        ).await.unwrap();
        
        // Validate fee statistics
        assert!(fee_stats.total_collected > 0, "Total collected must be positive");
        assert!(fee_stats.transaction_count >= 5, "Transaction count must be >= 5");
        assert!(fee_stats.average_fee > 0, "Average fee must be positive");
        assert_eq!(fee_stats.period_start, period_start, "Period start must match");
        assert_eq!(fee_stats.period_end, period_end, "Period end must match");
        
        // Test invalid time range
        let result = fee_management::get_fee_statistics(
            ctx.as_context(),
            ctx.get_fee_statistics_accounts(),
            period_end, // Start after end
            period_start
        ).await;
        
        assert!(result.is_err(), "get_fee_statistics must fail for invalid time range");
    }
    
    #[tokio::test]
    async fn test_direct_validate_fee_payment() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let payer = Pubkey::new_unique();
        let fee_type = "meme_upload";
        let required_amount = 290_000u64; // 0.290 KOTS
        
        // Fund payer sufficiently
        ctx.fund_account(&payer, required_amount * 2).await.unwrap();
        
        // Direct function invocation
        let result = fee_management::validate_fee_payment(
            ctx.as_context(),
            ctx.get_fee_validation_accounts(),
            payer,
            fee_type,
            required_amount
        ).await;
        
        assert!(result.is_ok(), "validate_fee_payment must succeed for sufficient funds");
        
        // Validate validation details
        let validation_result = result.unwrap();
        assert!(validation_result.is_valid, "Validation must be successful");
        assert!(validation_result.balance_sufficient, "Balance must be sufficient");
        assert_eq!(validation_result.required_amount, required_amount, "Required amount must match");
        
        // Test insufficient balance
        let unfunded_payer = Pubkey::new_unique();
        let result = fee_management::validate_fee_payment(
            ctx.as_context(),
            ctx.get_fee_validation_accounts(),
            unfunded_payer,
            fee_type,
            required_amount
        ).await;
        
        let validation_result = result.unwrap();
        assert!(!validation_result.is_valid, "Validation must fail");
        assert!(!validation_result.balance_sufficient, "Balance must be insufficient");
        
        // Test invalid fee type
        let result = fee_management::validate_fee_payment(
            ctx.as_context(),
            ctx.get_fee_validation_accounts(),
            payer,
            "invalid_fee_type",
            required_amount
        ).await;
        
        assert!(result.is_err(), "validate_fee_payment must fail for invalid fee type");
    }
    
    #[tokio::test]
    async fn test_direct_emergency_fee_adjustment() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        let adjustment_type = "global_reduction";
        let adjustment_factor = 50u32; // 50% reduction
        let reason = "Network congestion mitigation";
        
        // Direct function invocation
        let result = fee_management::emergency_fee_adjustment(
            ctx.as_context(),
            ctx.get_emergency_adjustment_accounts(),
            adjustment_type,
            adjustment_factor,
            reason.to_string()
        ).await;
        
        assert!(result.is_ok(), "emergency_fee_adjustment must succeed");
        
        // Validate adjustment
        let adjustment_record = ctx.get_emergency_adjustment_record().await.unwrap();
        assert_eq!(adjustment_record.adjustment_type, adjustment_type, "Adjustment type must match");
        assert_eq!(adjustment_record.adjustment_factor, adjustment_factor, "Adjustment factor must match");
        assert_eq!(adjustment_record.reason, reason, "Reason must match");
        assert!(adjustment_record.timestamp > 0, "Must have timestamp");
        assert!(adjustment_record.is_active, "Adjustment must be active");
        
        // Test invalid adjustment factor
        let result = fee_management::emergency_fee_adjustment(
            ctx.as_context(),
            ctx.get_emergency_adjustment_accounts(),
            adjustment_type,
            0, // Invalid factor
            reason.to_string()
        ).await;
        
        assert!(result.is_err(), "emergency_fee_adjustment must fail for invalid factor");
        
        // Test excessive adjustment factor
        let result = fee_management::emergency_fee_adjustment(
            ctx.as_context(),
            ctx.get_emergency_adjustment_accounts(),
            adjustment_type,
            1000, // 1000% - excessive
            reason.to_string()
        ).await;
        
        assert!(result.is_err(), "emergency_fee_adjustment must fail for excessive factor");
    }
    
    #[tokio::test]
    async fn test_direct_initialize_upgrade_registry() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Direct function invocation
        let result = fee_management::initialize_upgrade_registry(
            ctx.as_context(),
            ctx.get_fee_upgrade_registry_accounts()
        ).await;
        
        assert!(result.is_ok(), "initialize_upgrade_registry must succeed");
        
        // Validate registry state
        let registry_state = ctx.get_fee_upgrade_registry_state().await.unwrap();
        assert!(registry_state.is_initialized, "Registry must be initialized");
        assert_eq!(registry_state.pending_upgrades, 0, "Must start with 0 pending upgrades");
        
        // Test duplicate initialization
        let result = fee_management::initialize_upgrade_registry(
            ctx.as_context(),
            ctx.get_fee_upgrade_registry_accounts()
        ).await;
        
        assert!(result.is_err(), "initialize_upgrade_registry must fail for duplicate");
    }
    
    #[tokio::test]
    async fn test_direct_execute_systemic_upgrade() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_upgrade_registry().await.unwrap();
        
        let upgrade_id = 1u32;
        
        // Direct function invocation
        let result = fee_management::execute_systemic_upgrade(
            ctx.as_context(),
            ctx.get_fee_upgrade_accounts(),
            upgrade_id
        ).await;
        
        assert!(result.is_ok(), "execute_systemic_upgrade must succeed");
        
        // Validate upgrade execution
        let registry_state = ctx.get_fee_upgrade_registry_state().await.unwrap();
        assert!(registry_state.last_upgrade_id >= upgrade_id, "Upgrade ID must be recorded");
        
        // Test invalid upgrade ID
        let result = fee_management::execute_systemic_upgrade(
            ctx.as_context(),
            ctx.get_fee_upgrade_accounts(),
            999 // Invalid
        ).await;
        
        assert!(result.is_err(), "execute_systemic_upgrade must fail for invalid ID");
    }
    
    #[tokio::test]
    async fn test_direct_report_fee_readiness() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_fee_management().await.unwrap();
        
        // Direct function invocation
        let result = fee_management::report_fee_readiness(
            ctx.as_context(),
            ctx.get_fee_readiness_accounts()
        ).await;
        
        assert!(result.is_ok(), "report_fee_readiness must succeed");
        
        // Validate readiness report
        let readiness_report = result.unwrap();
        assert!(readiness_report.is_ready, "Fee management must be ready after initialization");
        assert!(readiness_report.fee_structures_loaded, "Fee structures must be loaded");
        assert!(readiness_report.collection_enabled, "Fee collection must be enabled");
        assert!(readiness_report.distribution_enabled, "Fee distribution must be enabled");
        
        // Test readiness without initialization
        let ctx2 = MultiProgramTestContext::new().await.unwrap();
        let result = fee_management::report_fee_readiness(
            ctx2.as_context(),
            ctx2.get_fee_readiness_accounts()
        ).await;
        
        assert!(result.is_err(), "report_fee_readiness must fail without initialization");
    }
}