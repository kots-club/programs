//! Direct Security-Governance Function Tests
//!
//! Real instrumented tests that directly invoke each of the 16 public functions
//! with proper state assertions and error code validation

use super::test_utils::*;
use kots_common::{constants::*, state::*, errors::*};
use anchor_lang::prelude::*;

#[cfg(test)]
mod direct_security_tests {
    use super::*;
    
    // Direct tests for each of the 16 security-governance public functions
    
    #[tokio::test]
    async fn test_direct_initialize_security_governance() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        
        // Direct function invocation with state validation
        let result = security_governance::initialize_security_governance(
            ctx.as_context(),
            ctx.get_security_governance_accounts()
        ).await;
        
        assert!(result.is_ok(), "initialize_security_governance must succeed");
        
        // Validate state changes
        let state = ctx.get_security_governance_state().await.unwrap();
        assert!(state.is_initialized, "State must show initialized");
        assert_eq!(state.emergency_level, 0, "Emergency level must be 0");
    }
    
    #[tokio::test]
    async fn test_direct_initialize_enhanced_gas_oracle() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        // Direct function invocation
        let result = security_governance::initialize_enhanced_gas_oracle(
            ctx.as_context(),
            ctx.get_gas_oracle_accounts()
        ).await;
        
        assert!(result.is_ok(), "initialize_enhanced_gas_oracle must succeed");
        
        // Validate oracle state
        let oracle_state = ctx.get_gas_oracle_state().await.unwrap();
        assert!(oracle_state.is_initialized, "Oracle must be initialized");
        assert!(oracle_state.sources_count > 0, "Must have oracle sources");
    }
    
    #[tokio::test]
    async fn test_direct_update_gas_price_consensus() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        ctx.initialize_enhanced_gas_oracle().await.unwrap();
        
        let gas_prices = vec![100_000u64, 105_000, 95_000, 102_000, 98_000];
        
        // Direct function invocation
        let result = security_governance::update_gas_price_consensus(
            ctx.as_context(),
            ctx.get_gas_oracle_accounts(),
            gas_prices
        ).await;
        
        assert!(result.is_ok(), "update_gas_price_consensus must succeed");
        
        // Validate consensus calculation
        let oracle_state = ctx.get_gas_oracle_state().await.unwrap();
        assert!(oracle_state.current_consensus > 0, "Must have consensus price");
    }
    
    #[tokio::test] 
    async fn test_direct_initialize_blacklist() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        // Direct function invocation
        let result = security_governance::initialize_blacklist(
            ctx.as_context(),
            ctx.get_blacklist_accounts()
        ).await;
        
        assert!(result.is_ok(), "initialize_blacklist must succeed");
        
        // Validate blacklist state
        let blacklist_state = ctx.get_blacklist_state().await.unwrap();
        assert!(blacklist_state.is_initialized, "Blacklist must be initialized");
        assert_eq!(blacklist_state.total_entries, 0, "Must start with 0 entries");
    }
    
    #[tokio::test]
    async fn test_direct_add_to_blacklist() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        ctx.initialize_blacklist().await.unwrap();
        
        let malicious_account = Pubkey::new_unique();
        let reason = "Bot activity detected";
        
        // Direct function invocation
        let result = security_governance::add_to_blacklist(
            ctx.as_context(),
            ctx.get_blacklist_accounts(),
            malicious_account,
            reason.to_string()
        ).await;
        
        assert!(result.is_ok(), "add_to_blacklist must succeed");
        
        // Validate account is blacklisted
        let is_blacklisted = ctx.is_account_blacklisted(malicious_account).await.unwrap();
        assert!(is_blacklisted, "Account must be blacklisted");
        
        // Validate state update
        let blacklist_state = ctx.get_blacklist_state().await.unwrap();
        assert_eq!(blacklist_state.total_entries, 1, "Must have 1 entry");
    }
    
    #[tokio::test]
    async fn test_direct_remove_from_blacklist() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        ctx.initialize_blacklist().await.unwrap();
        
        let test_account = Pubkey::new_unique();
        
        // Add first, then remove
        ctx.add_to_blacklist(test_account, "Test".to_string()).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::remove_from_blacklist(
            ctx.as_context(),
            ctx.get_blacklist_accounts(),
            test_account
        ).await;
        
        assert!(result.is_ok(), "remove_from_blacklist must succeed");
        
        // Validate removal
        let is_blacklisted = ctx.is_account_blacklisted(test_account).await.unwrap();
        assert!(!is_blacklisted, "Account must not be blacklisted");
    }
    
    #[tokio::test]
    async fn test_direct_emergency_pause() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let emergency_level = 3u8;
        
        // Direct function invocation
        let result = security_governance::emergency_pause(
            ctx.as_context(),
            ctx.get_security_governance_accounts(),
            emergency_level
        ).await;
        
        assert!(result.is_ok(), "emergency_pause must succeed");
        
        // Validate emergency state
        let security_state = ctx.get_security_governance_state().await.unwrap();
        assert_eq!(security_state.emergency_level, emergency_level, "Emergency level must match");
        assert!(security_state.is_emergency_active, "Emergency must be active");
    }
    
    #[tokio::test]
    async fn test_direct_emergency_resume() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        // Activate emergency first
        ctx.emergency_pause(4).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::emergency_resume(
            ctx.as_context(),
            ctx.get_security_governance_accounts()
        ).await;
        
        assert!(result.is_ok(), "emergency_resume must succeed");
        
        // Validate resume
        let security_state = ctx.get_security_governance_state().await.unwrap();
        assert_eq!(security_state.emergency_level, 0, "Emergency level must be reset");
        assert!(!security_state.is_emergency_active, "Emergency must be inactive");
    }
    
    #[tokio::test]
    async fn test_direct_validate_security_constraints() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        ctx.initialize_blacklist().await.unwrap();
        
        let test_account = Pubkey::new_unique();
        let action_type = "vote_cast";
        
        // Direct function invocation
        let result = security_governance::validate_security_constraints(
            ctx.as_context(),
            ctx.get_security_governance_accounts(),
            test_account,
            action_type
        ).await;
        
        assert!(result.is_ok(), "validate_security_constraints must succeed for clean account");
        
        // Test with blacklisted account
        ctx.add_to_blacklist(test_account, "Test blacklist".to_string()).await.unwrap();
        
        let result = security_governance::validate_security_constraints(
            ctx.as_context(),
            ctx.get_security_governance_accounts(),
            test_account,
            action_type
        ).await;
        
        assert!(result.is_err(), "validate_security_constraints must fail for blacklisted account");
    }
    
    #[tokio::test]
    async fn test_direct_verify_meme_upload_payment() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let uploader = Pubkey::new_unique();
        let payment_amount = 290_000u64; // 0.290 KOTS
        let storage_duration = 90u32; // 3 months
        
        // Fund uploader
        ctx.fund_account(&uploader, payment_amount * 2).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::verify_meme_upload_payment(
            ctx.as_context(),
            ctx.get_payment_verification_accounts(),
            uploader,
            payment_amount,
            storage_duration
        ).await;
        
        assert!(result.is_ok(), "verify_meme_upload_payment must succeed");
        
        // Test insufficient payment
        let result = security_governance::verify_meme_upload_payment(
            ctx.as_context(),
            ctx.get_payment_verification_accounts(),
            uploader,
            100_000, // Too low
            storage_duration
        ).await;
        
        assert!(result.is_err(), "verify_meme_upload_payment must fail for insufficient payment");
    }
    
    #[tokio::test]
    async fn test_direct_verify_storage_extension_payment() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let user = Pubkey::new_unique();
        let meme_id = generate_test_meme_id(1);
        let extension_days = 30u32;
        let payment_amount = 50_000u64;
        
        // Fund user
        ctx.fund_account(&user, payment_amount * 2).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::verify_storage_extension_payment(
            ctx.as_context(),
            ctx.get_storage_verification_accounts(),
            user,
            meme_id,
            extension_days,
            payment_amount
        ).await;
        
        assert!(result.is_ok(), "verify_storage_extension_payment must succeed");
        
        // Test zero extension
        let result = security_governance::verify_storage_extension_payment(
            ctx.as_context(),
            ctx.get_storage_verification_accounts(),
            user,
            meme_id,
            0, // Invalid
            payment_amount
        ).await;
        
        assert!(result.is_err(), "verify_storage_extension_payment must fail for zero extension");
    }
    
    #[tokio::test]
    async fn test_direct_create_upgrade_proposal() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let proposer = Pubkey::new_unique();
        let title = "Upgrade Scoring System";
        let description = "Implement new scoring algorithm";
        let upgrade_type = "scoring";
        
        // Direct function invocation
        let proposal_id = security_governance::create_upgrade_proposal(
            ctx.as_context(),
            ctx.get_governance_accounts(),
            proposer,
            title.to_string(),
            description.to_string(),
            upgrade_type.to_string()
        ).await.unwrap();
        
        // Validate proposal creation
        let proposal = ctx.get_upgrade_proposal(proposal_id).await.unwrap();
        assert_eq!(proposal.proposer, proposer, "Proposer must match");
        assert_eq!(proposal.title, title, "Title must match");
        assert_eq!(proposal.votes_for, 0, "Must start with 0 votes");
        assert!(!proposal.is_executed, "Must not be executed initially");
    }
    
    #[tokio::test]
    async fn test_direct_vote_upgrade_proposal() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let proposer = Pubkey::new_unique();
        let voter = Pubkey::new_unique();
        
        // Create proposal first
        let proposal_id = ctx.create_upgrade_proposal(
            proposer,
            "Test Proposal".to_string(),
            "Test Description".to_string(),
            "test".to_string()
        ).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::vote_upgrade_proposal(
            ctx.as_context(),
            ctx.get_governance_accounts(),
            proposal_id,
            voter,
            true // vote for
        ).await;
        
        assert!(result.is_ok(), "vote_upgrade_proposal must succeed");
        
        // Validate vote recorded
        let proposal = ctx.get_upgrade_proposal(proposal_id).await.unwrap();
        assert_eq!(proposal.votes_for, 1, "Must have 1 vote for");
        
        // Test double voting
        let result = security_governance::vote_upgrade_proposal(
            ctx.as_context(),
            ctx.get_governance_accounts(),
            proposal_id,
            voter,
            false
        ).await;
        
        assert!(result.is_err(), "vote_upgrade_proposal must fail for double voting");
    }
    
    #[tokio::test]
    async fn test_direct_execute_upgrade() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let proposer = Pubkey::new_unique();
        
        // Create proposal
        let proposal_id = ctx.create_upgrade_proposal(
            proposer,
            "Approved Upgrade".to_string(),
            "Approved Description".to_string(),
            "approved".to_string()
        ).await.unwrap();
        
        // Add sufficient votes
        for i in 0..10 {
            let voter = Pubkey::new_unique();
            ctx.vote_upgrade_proposal(proposal_id, voter, true).await.unwrap();
        }
        
        // Direct function invocation
        let result = security_governance::execute_upgrade(
            ctx.as_context(),
            ctx.get_governance_accounts(),
            proposal_id
        ).await;
        
        assert!(result.is_ok(), "execute_upgrade must succeed");
        
        // Validate execution
        let proposal = ctx.get_upgrade_proposal(proposal_id).await.unwrap();
        assert!(proposal.is_executed, "Proposal must be executed");
    }
    
    #[tokio::test]
    async fn test_direct_initialize_cross_program_upgrade() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let coordinator = Pubkey::new_unique();
        let version = "v2.1.0";
        let programs = vec!["voting-hieroglyph".to_string(), "contest-management".to_string()];
        
        // Direct function invocation
        let upgrade_id = security_governance::initialize_cross_program_upgrade(
            ctx.as_context(),
            ctx.get_cross_program_upgrade_accounts(),
            coordinator,
            version.to_string(),
            programs.clone()
        ).await.unwrap();
        
        // Validate initialization
        let upgrade_state = ctx.get_cross_program_upgrade_state(upgrade_id).await.unwrap();
        assert_eq!(upgrade_state.coordinator, coordinator, "Coordinator must match");
        assert_eq!(upgrade_state.version, version, "Version must match");
        assert_eq!(upgrade_state.programs_count, programs.len() as u32, "Program count must match");
        assert!(!upgrade_state.is_finalized, "Must not be finalized initially");
    }
    
    #[tokio::test]
    async fn test_direct_finalize_cross_program_upgrade() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        ctx.initialize_security_governance().await.unwrap();
        
        let coordinator = Pubkey::new_unique();
        let programs = vec!["treasury-operations".to_string()];
        
        // Initialize upgrade first
        let upgrade_id = ctx.initialize_cross_program_upgrade(
            coordinator,
            "v2.2.0".to_string(),
            programs
        ).await.unwrap();
        
        // Direct function invocation
        let result = security_governance::finalize_cross_program_upgrade(
            ctx.as_context(),
            ctx.get_cross_program_upgrade_accounts(),
            upgrade_id,
            coordinator
        ).await;
        
        assert!(result.is_ok(), "finalize_cross_program_upgrade must succeed");
        
        // Validate finalization
        let upgrade_state = ctx.get_cross_program_upgrade_state(upgrade_id).await.unwrap();
        assert!(upgrade_state.is_finalized, "Upgrade must be finalized");
        assert!(upgrade_state.finalization_timestamp > 0, "Must have timestamp");
        
        // Test finalization by non-coordinator
        let non_coordinator = Pubkey::new_unique();
        let new_upgrade_id = ctx.initialize_cross_program_upgrade(
            coordinator,
            "v2.3.0".to_string(),
            vec!["fee-management".to_string()]
        ).await.unwrap();
        
        let result = security_governance::finalize_cross_program_upgrade(
            ctx.as_context(),
            ctx.get_cross_program_upgrade_accounts(),
            new_upgrade_id,
            non_coordinator
        ).await;
        
        assert!(result.is_err(), "finalize_cross_program_upgrade must fail for non-coordinator");
    }
}