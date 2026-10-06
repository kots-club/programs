//! Compute Unit (CU) Monitoring and Enforcement
//! 
//! Provides mechanisms to ensure all operations stay within the 200k CU limit
//! across the multi-program architecture with cross-program invocation tracking.

use anchor_lang::prelude::*;

/// CU Budget enforcement for individual operations
#[derive(Debug, Clone)]
pub struct CUBudget {
    pub total_limit: u32,
    pub consumed: u32,
    pub reserved_for_cpi: u32,
}

impl Default for CUBudget {
    fn default() -> Self {
        Self {
            total_limit: 200_000,  // Strict 200k limit
            consumed: 0,
            reserved_for_cpi: 50_000, // Reserve CU for cross-program calls
        }
    }
}

impl CUBudget {
    /// Check if operation can proceed without exceeding CU limit
    pub fn can_execute(&self, estimated_cu: u32) -> bool {
        let available = self.total_limit.saturating_sub(self.consumed);
        estimated_cu <= available
    }
    
    /// Record CU consumption for an operation
    pub fn consume(&mut self, cu_used: u32) -> Result<()> {
        let new_total = self.consumed.saturating_add(cu_used);
        
        if new_total > self.total_limit {
            return Err(CUErrorCode::ComputeUnitLimitExceeded.into());
        }
        
        self.consumed = new_total;
        Ok(())
    }
    
    /// Get remaining CU budget
    pub fn remaining(&self) -> u32 {
        self.total_limit.saturating_sub(self.consumed)
    }
    
    /// Check if enough CU remains for CPI calls
    pub fn has_cpi_budget(&self) -> bool {
        self.remaining() >= self.reserved_for_cpi
    }
}

/// CU cost estimation for different operation types
#[derive(Debug, Clone, Copy)]
pub enum OperationType {
    SingleVote,
    BatchVote(u8),  // Number of votes in batch
    ContestCreation,
    ParticipantRegistration,
    AwardDistribution,
    SecurityValidation,
}

impl OperationType {
    /// Get estimated CU cost for operation
    pub fn estimated_cu_cost(&self) -> u32 {
        match self {
            OperationType::SingleVote => 15_000,
            OperationType::BatchVote(count) => 10_000 + (*count as u32 * 3_000),
            OperationType::ContestCreation => 25_000,
            OperationType::ParticipantRegistration => 12_000,
            OperationType::AwardDistribution => 30_000,
            OperationType::SecurityValidation => 8_000,
        }
    }
    
    /// Check if operation exceeds individual operation limit
    pub fn exceeds_limit(&self) -> bool {
        self.estimated_cu_cost() > 150_000 // Leave buffer for CPI
    }
}

/// Cross-program CU tracking for CPI calls
#[derive(Debug, Clone)]
pub struct CPICUTracker {
    pub voting_program_cu: u32,
    pub contest_program_cu: u32,
    pub fee_management_cu: u32,
    pub progressive_jackpot_cu: u32,
    pub treasury_operations_cu: u32,
    pub prize_distribution_cu: u32,
    pub security_program_cu: u32,
    pub total_cpi_cu: u32,
}

impl Default for CPICUTracker {
    fn default() -> Self {
        Self {
            voting_program_cu: 0,
            contest_program_cu: 0,
            fee_management_cu: 0,
            progressive_jackpot_cu: 0,
            treasury_operations_cu: 0,
            prize_distribution_cu: 0,
            security_program_cu: 0,
            total_cpi_cu: 0,
        }
    }
}

impl CPICUTracker {
    /// Record CU consumption for a specific program CPI
    pub fn record_cpi_cu(&mut self, program: &str, cu_used: u32) {
        match program {
            "voting_hieroglyph" => self.voting_program_cu += cu_used,
            "contest_management" => self.contest_program_cu += cu_used,
            "fee_management" => self.fee_management_cu += cu_used,
            "progressive_jackpot" => self.progressive_jackpot_cu += cu_used,
            "treasury_operations" => self.treasury_operations_cu += cu_used,
            "prize_distribution" => self.prize_distribution_cu += cu_used,
            "security_governance" => self.security_program_cu += cu_used,
            _ => {}
        }
        self.total_cpi_cu += cu_used;
    }
    
    /// Check if total CPI CU consumption is reasonable
    pub fn within_cpi_limits(&self) -> bool {
        self.total_cpi_cu <= 100_000 // CPI should not exceed 50% of total budget
    }
}

/// Error codes for CU limit violations
#[error_code]
pub enum CUErrorCode {
    #[msg("Operation would exceed 200k CU limit")]
    ComputeUnitLimitExceeded,
    #[msg("Insufficient CU budget remaining for operation")]
    InsufficientCUBudget,
    #[msg("Cross-program invocation CU budget exceeded")]
    CPICULimitExceeded,
    #[msg("Operation estimated CU cost too high")]
    OperationTooExpensive,
}

/// Macro for CU budget checking before expensive operations
#[macro_export]
macro_rules! check_cu_budget {
    ($budget:expr, $operation:expr) => {
        let estimated_cost = $operation.estimated_cu_cost();
        if !$budget.can_execute(estimated_cost) {
            return Err(CUErrorCode::InsufficientCUBudget.into());
        }
        if $operation.exceeds_limit() {
            return Err(CUErrorCode::OperationTooExpensive.into());
        }
    };
}

/// Helper function to validate CU consumption before CPI
pub fn validate_cpi_cu_budget(remaining_cu: u32, estimated_cpi_cu: u32) -> Result<()> {
    if estimated_cpi_cu > remaining_cu {
        return Err(CUErrorCode::CPICULimitExceeded.into());
    }
    Ok(())
}