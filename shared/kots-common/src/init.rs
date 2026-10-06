//! KOTS Init Replacement Module
//!
//! Centralized PDA creation with intelligent treasury funding.
//! This module provides a generic init replacement that all KOTS programs
//! can use to create PDAs funded by the treasury instead of users.

use anchor_lang::prelude::*;

/// Treasury accounts wrapper for initialization operations
#[derive(Debug)]
pub struct TreasuryAccounts<'info> {
    pub incoming: AccountInfo<'info>,
    pub main: AccountInfo<'info>,
    pub outgoing: AccountInfo<'info>,
}
// use anchor_lang::system_program;  // Removed unused import
// use anchor_lang::solana_program::program::invoke_signed;  // Removed unused import

use crate::errors::KotsError;

/// Options for PDA creation
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct InitOptions {
    /// Use emergency funding if primary funding fails
    pub allow_emergency_funding: bool,
    /// Allow creator to pay with reimbursement if treasury fails
    pub allow_creator_payment: bool,
    /// Extra buffer beyond rent exemption
    pub buffer_lamports: u64,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            allow_emergency_funding: true,
            allow_creator_payment: true,
            buffer_lamports: 0,
        }
    }
}

/// Request treasury reimbursement after PDA creation
/// 
/// This function should be called AFTER the owning program has successfully
/// created its PDA using Anchor's #[account(init)] pattern. It requests
/// reimbursement from Treasury Operations for the creation cost.
/// 
/// CRITICAL: This does NOT create PDAs. Each program must create its own PDAs
/// using invoke_signed with its own seeds, then call this for reimbursement.
/// 
/// # Arguments
/// * `treasury_incoming` - Treasury incoming account (will reimburse)
/// * `payer` - The account that funded the PDA creation (receives reimbursement)
/// * `amount` - Amount of lamports to reimburse
/// * `system_program` - System program for transfers
/// * `treasury_ops_program` - Treasury Operations program
/// * `caller_program_id` - ID of the program requesting reimbursement
/// * `options` - Reimbursement options including fallback strategies
/// * `cpi_authority` - PDA of the CALLING program at [CPI_AUTHORITY_SEED]; signed
///   here via `invoke_signed` so Treasury can prove who is really calling.
///
/// SECURITY (2026-07-23): this helper previously used `invoke` and identified the
/// caller ONLY by serializing `caller_program_id` into the instruction data. That
/// is an assertion, not a proof — program ids are public, so any wallet could
/// hand-craft the same instruction and be reimbursed. It now signs with a PDA
/// derived from the caller's own program id, which no other party can produce.
///
/// CALL-SITE CHANGE REQUIRED: every program calling this must add a
/// `cpi_authority` account (seeds = [CPI_AUTHORITY_SEED], bump) to its Accounts
/// struct and pass it here. See the audit report.
pub fn request_treasury_reimbursement<'info>(
    treasury_incoming: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    amount: u64,
    system_program: &AccountInfo<'info>,
    treasury_ops_program: &AccountInfo<'info>,
    caller_program_id: &Pubkey,
    cpi_authority: &AccountInfo<'info>,
    options: &InitOptions,
) -> Result<()> {
    // IDL build stub - return immediately without actual implementation
    #[cfg(feature = "idl-build")]
    {
        return Ok(());
    }
    
    #[cfg(not(feature = "idl-build"))]
    {
    msg!("💰 Requesting treasury reimbursement");
    msg!("  Amount: {} lamports", amount);
    msg!("  Payer: {}", payer.key());
    msg!("  Caller Program: {}", caller_program_id);
    
    // Manual CPI to Treasury Operations reimburse_payer instruction
    let discriminator = anchor_lang::solana_program::hash::hash(b"global:reimburse_payer").to_bytes();
    let mut instruction_data = Vec::new();
    instruction_data.extend_from_slice(&discriminator[..8]);
    
    // Serialize parameters: amount_lamports: u64, caller_program_id: Pubkey
    let reimburse_args = (amount, *caller_program_id);
    let serialized_args = anchor_lang::prelude::borsh::to_vec(&reimburse_args)
        .map_err(|_| anchor_lang::prelude::ErrorCode::AccountDidNotSerialize)?;
    instruction_data.extend_from_slice(&serialized_args);
    
    let reimburse_instruction = anchor_lang::solana_program::instruction::Instruction {
        program_id: treasury_ops_program.key().clone(),
        accounts: vec![
            // treasury_incoming - writable (source of reimbursement)
            anchor_lang::solana_program::instruction::AccountMeta::new(treasury_incoming.key().clone(), false),
            // payer - writable (receives reimbursement)
            anchor_lang::solana_program::instruction::AccountMeta::new(payer.key().clone(), false),
            // cpi_authority - readonly SIGNER (proves which program is calling)
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(cpi_authority.key().clone(), true),
            // system_program - readonly (for transfer)
            anchor_lang::solana_program::instruction::AccountMeta::new_readonly(system_program.key().clone(), false),
        ],
        data: instruction_data,
    };

    // Derive the signing seeds for THIS program's CPI authority PDA. Because this
    // helper is compiled into each calling program, `invoke_signed` here signs
    // with that program's id — which is exactly the identity Treasury verifies.
    let (derived_authority, cpi_bump) = anchor_lang::prelude::Pubkey::find_program_address(
        &[crate::state::CPI_AUTHORITY_SEED],
        caller_program_id,
    );
    require_keys_eq!(derived_authority, cpi_authority.key(), KotsError::UnauthorizedCaller);
    let cpi_seeds: &[&[u8]] = &[crate::state::CPI_AUTHORITY_SEED, &[cpi_bump]];

    // Execute the CPI call with proper error handling
    match anchor_lang::solana_program::program::invoke_signed(
        &reimburse_instruction,
        &[
            treasury_incoming.clone(),
            payer.clone(),
            cpi_authority.clone(),
            system_program.clone(),
        ],
        &[cpi_seeds],
    ) {
        Ok(()) => {
            msg!("✅ Treasury reimbursement successful");
            msg!("  Amount: {} lamports", amount);
        },
        Err(e) => {
            msg!("⚠️ Treasury reimbursement failed: {:?}", e);
            
            // Check if creator payment fallback is allowed
            if options.allow_creator_payment {
                msg!("💡 Fallback: Creator payment allowed, continuing without reimbursement");
            } else {
                msg!("❌ Creator payment not allowed, transaction will fail");
                return Err(KotsError::InsufficientTreasuryFunds.into());
            }
        }
    }
    
    msg!("✅ Treasury reimbursement request completed");
    Ok(())
    }
}


/// Validate that a PDA has proper ownership
pub fn validate_pda_ownership(
    account: &AccountInfo,
    expected_owner: &Pubkey,
) -> Result<()> {
    if account.owner != expected_owner {
        msg!("❌ PDA ownership mismatch!");
        msg!("  Expected: {}", expected_owner);
        msg!("  Actual: {}", account.owner);
        return Err(KotsError::AccountOwnedByWrongProgram.into());
    }
    
    msg!("✅ PDA ownership validated");
    Ok(())
}

/// Helper to check if an account needs initialization
pub fn needs_initialization(account: &AccountInfo) -> bool {
    account.lamports() == 0 || account.data_is_empty()
}

/// Calculate space needed for a PDA including discriminator
pub fn calculate_pda_space<T>() -> usize {
    8 + std::mem::size_of::<T>()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_calculate_space() {
        // Test space calculation
        #[derive(AnchorSerialize, AnchorDeserialize)]
        struct TestAccount {
            value: u64,
            owner: Pubkey,
        }
        
        let space = calculate_pda_space::<TestAccount>();
        assert_eq!(space, 8 + 8 + 32); // discriminator + u64 + Pubkey
    }
    
    #[test]
    fn test_init_options() {
        let options = InitOptions::default();
        assert!(options.allow_emergency_funding);
        assert!(options.allow_creator_payment);
        assert_eq!(options.buffer_lamports, 0);
    }
}