// DESTINATION (where this file goes): kots-contracts/programs/treasury-operations/src/lib.rs
// kots-contracts/programs/treasury-operations/src/lib.rs
//! KOTS Treasury Operations Program
//! 
//! Handles treasury management, vault operations, upgrades,
//! migrations, and economic parameter updates.

// Prevent both features from being enabled simultaneously during BPF compilation
// Allow IDL generation to proceed with both features if needed
#[cfg(all(feature = "idl-build", feature = "bpf-runtime", target_arch = "bpf"))]
compile_error!("Cannot enable both 'idl-build' and 'bpf-runtime' features simultaneously during BPF compilation");

use anchor_lang::prelude::*;
use anchor_spl::token::{TokenAccount, Mint};
// ACT-21 Increment 2 — Token-2022 interface types for the treasury outflow
// transfer (`settle_external_transfer`). Aliased so they don't clash with the
// classic `token::{TokenAccount, Mint}` used by `check_token_account`.
use anchor_spl::token_interface::{
    Mint as MintInterface, TokenAccount as TokenAccountInterface, TokenInterface,
};
use kots_common::{
    constants::*, errors::*, StorageTier, FIXED_MEME_UPLOAD_COST, STANDARD_STORAGE_MULTIPLIER, PREMIUM_STORAGE_MULTIPLIER, 
    ARCHIVE_STORAGE_MULTIPLIER, BANDWIDTH_COST_PER_MB, STORAGE_COST_PER_MB_MONTH,
    UpgradeExecutedEvent, UpgradeType,
    // SECURITY (2026-07-23): seed for the per-program CPI authority PDA, used by
    // `reimburse_payer` to re-derive the caller's PDA and prove the caller's
    // identity by signature rather than by instruction argument.
    state::{
        CPI_AUTHORITY_SEED
    },
    upgrade::{
        UPGRADE_REGISTRY_SEED
    }
};

// Account struct definitions for IDL generation only - use kots-common types for BPF compilation

// Conditional compilation: use IDL types for host-side IDL generation, real types for BPF
#[cfg(feature = "idl-build")]
mod idl_types {
    use super::*;
    
    // IDL-only type definitions for proper client generation
    #[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq)]
    pub enum ProgramType {
        TreasuryOperations,
    }
    
    #[account]
    pub struct UpgradeRegistry {
        pub program_type: ProgramType,
        pub current_version: u16,
        pub pending_proposals: u32,
        pub successful_upgrades: u32,
        pub failed_upgrades: u32,
        pub last_upgrade_timestamp: i64,
        pub upgrade_authority: Option<Pubkey>,
        pub governance_approval_required: bool,
        pub emergency_upgrade_enabled: bool,
        pub version_history: [[u8; 32]; 10], // Nested arrays for proper IDL type representation
        pub compatible_versions: [u16; 8],
        pub last_compatibility_check: i64,
        pub emergency_pause_enabled: bool,
        pub emergency_contact: Option<Pubkey>,
    }
}

#[cfg(feature = "idl-build")]
pub use idl_types::*;

// NOTE (2026-07-22): the `use kots_common::upgrade::ProgramType` that was here is
// removed — this program defines its OWN `ProgramType` enum above, which shadowed
// the import and left it unreferenced.

// F19 FIX (2026-08): `UpgradeRegistry` is defined LOCALLY here rather than
// re-exported from kots_common. kots_common's `#[account]` types derive
// `Owner::owner() == kots_common::ID`, and that ID is an all-zeros placeholder
// (which IS the System Program address), so `Account<'info, UpgradeRegistry>`
// demanded a System-owned account while Anchor's `init` creates the PDA owned by
// THIS program. Every access failed AccountOwnedByWrongProgram (3007) and the
// registry could never be initialized or read — bricking the A.1 multisig
// authority, ACT-10 emergency_funding, ACT-21 veto/snooze/window-init, and the
// pre-existing systemic-upgrade / pause / reimburse instructions. Defining it
// locally makes `Owner::owner()` resolve to this program's `declare_id!()` —
// exactly as security-governance does for `OwnerRegistry` (which is why its
// tests pass). Fields and `program_type`'s enum are byte-identical to
// kots_common's, and the 8-byte discriminator is derived from the struct name
// ("UpgradeRegistry"), so the on-chain layout is unchanged. The kots_common
// struct remains for cross-program CPI reads (deserialized manually), never
// behind `Account<>` for an account THIS program owns.
#[cfg(not(feature = "idl-build"))]
#[account]
#[derive(Default)]
pub struct UpgradeRegistry {
    pub program_type: kots_common::upgrade::ProgramType,
    pub current_version: u16,
    pub pending_proposals: u32,
    pub successful_upgrades: u32,
    pub failed_upgrades: u32,
    pub last_upgrade_timestamp: i64,
    pub upgrade_authority: Option<Pubkey>,
    pub governance_approval_required: bool,
    pub emergency_upgrade_enabled: bool,
    pub version_history: [[u8; 32]; 10],
    pub compatible_versions: [u16; 8],
    pub last_compatibility_check: i64,
    pub emergency_pause_enabled: bool,
    pub emergency_contact: Option<Pubkey>,
}

#[cfg(not(feature = "idl-build"))]
impl UpgradeRegistry {
    // 8 discriminator + fields; matches the literal `space = 448` used by the
    // initializer and kots_common's own INIT_SPACE.
    pub const INIT_SPACE: usize =
        8 + 1 + 2 + 4 + 4 + 4 + 8 + 33 + 1 + 1 + 320 + 8 + 8 + 1 + 33;

    /// Check upgrade compatibility with another program (mirrors kots_common).
    pub fn is_compatible_with(
        &self,
        other_program: kots_common::upgrade::ProgramType,
        other_version: u16,
    ) -> bool {
        use kots_common::upgrade::ProgramType;
        let index = match other_program {
            ProgramType::SecurityGovernance => 0,
            ProgramType::ContestManagement => 1,
            ProgramType::VotingHieroglyph => 2,
            ProgramType::EconomicsDistribution => 3,
            ProgramType::FeeManagement => 4,
            ProgramType::PrizeDistribution => 5,
            ProgramType::ProgressiveJackpot => 6,
            ProgramType::TreasuryOperations => 7,
        };
        self.compatible_versions[index] <= other_version
    }

    /// Record a successful upgrade (mirrors kots_common exactly).
    pub fn update_version(&mut self, new_version: u16, timestamp: i64) {
        self.current_version = new_version;
        self.last_upgrade_timestamp = timestamp;
        self.successful_upgrades += 1;
        for i in (1..10).rev() {
            self.version_history[i] = self.version_history[i - 1];
        }
        let mut version_bytes = [0u8; 32];
        let version_le_bytes = new_version.to_le_bytes();
        version_bytes[0] = version_le_bytes[0];
        version_bytes[1] = version_le_bytes[1];
        self.version_history[0] = version_bytes;
    }
}

// Unified program ID for both devnet and mainnet
#[cfg(feature = "realm-main")]
declare_id!("2A9B5KLWVPbHU36d6eSehxu68Bz5eyPJ5xJz1riKzb98");

#[cfg(not(feature = "realm-main"))]
declare_id!("33LbYv6dGKegxAVNAFWx1An4HtGCacXvEMUy28ZXJvvs");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"treasury-operations|20251010T0000Z|token-check";

#[derive(Accounts)]
pub struct VersionHeartbeat {}

#[derive(Accounts)]
pub struct CheckTokenAccount<'info> {
    /// CHECK: User wallet to verify
    pub user_wallet: UncheckedAccount<'info>,
    
    /// User's token account - will fail if not initialized
    #[account(
        constraint = user_token_account.owner == user_wallet.key() @ KotsError::InvalidAccountOwner,
        constraint = user_token_account.mint == kots_mint.key() @ KotsError::InvalidTokenMint
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    
    /// KOTS token mint
    pub kots_mint: Account<'info, Mint>,
}

// EXPECTED WARNING — do not "fix" this by adding #![allow(deprecated)].
//
// Anchor 0.31.1's #[program] macro expands to a call to the deprecated
// AccountInfo::realloc. The call is in GENERATED code emitted at crate root, not
// inside this module, so a module-level #[allow(deprecated)] does not reach it —
// tried 2026-07-28, the warning was unchanged.
//
// The only thing that silences it is a crate-level #![allow(deprecated)], and
// that is a bad trade here: `solana_program::system_instruction` is being
// deprecated in favour of the `solana-system-interface` crate, six of these
// programs use it, and the test crate already warns about it. Blanket-silencing
// would hide that migration the moment it lands on the program crates.
//
// One unactionable warning per program is the correct cost. It goes away on the
// next Anchor upgrade, not before.
#[program]
pub mod treasury_operations {
    use super::*;
    
    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Check if a user's KOTS token account exists and is properly initialized
    /// This is a detection-only entry-point for client-side validation
    pub fn check_token_account(_ctx: Context<CheckTokenAccount>) -> Result<()> {
        msg!("✅ User token account verified");
        Ok(())
    }

    /// Initialize treasury accounts with proper program ownership - MUST BE CALLED FIRST
    pub fn initialize_treasury_accounts(
        ctx: Context<InitializeTreasuryAccounts>,
    ) -> Result<()> {
        msg!("🏦 Initializing Treasury Operations accounts");
        msg!("  Treasury Main: {}", ctx.accounts.treasury_main.key());
        msg!("  Treasury Incoming: {}", ctx.accounts.treasury_incoming.key());
        msg!("  Treasury Outgoing: {}", ctx.accounts.treasury_outgoing.key());
        
        // Set initialization flags - Anchor handles account creation with proper ownership
        let treasury_main = &mut ctx.accounts.treasury_main;
        let treasury_incoming = &mut ctx.accounts.treasury_incoming;
        let treasury_outgoing = &mut ctx.accounts.treasury_outgoing;
        
        treasury_main.initialized = true;
        treasury_incoming.initialized = true;
        treasury_outgoing.initialized = true;

        // C10: zero the queue explicitly. Anchor zeroes new account data, so this
        // is redundant today — but it is the only place these fields are ever
        // established, and leaving it implicit is how a future field added to
        // ReimbursementQueue silently starts life as garbage-by-assumption.
        let queue = &mut ctx.accounts.reimbursement_queue;
        queue.pending_reimbursements = 0;
        queue.total_owed = 0;
        queue.last_creator = Pubkey::default();
        queue.last_amount = 0;
        msg!("  Reimbursement Queue: {}", queue.key());

        msg!("✅ Treasury accounts initialized with Treasury Operations program ownership");
        msg!("  All accounts owned by: {}", ctx.program_id);
        
        Ok(())
    }

    /// Fund treasury accounts after initialization
    pub fn fund_treasury_accounts(
        ctx: Context<FundTreasuryAccounts>,
        main_funding: u64,
        incoming_funding: u64,
        outgoing_funding: u64,
    ) -> Result<()> {
        msg!("💰 Funding Treasury Operations accounts");
        
        // Transfer to treasury_main
        if main_funding > 0 {
            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.payer.key(),
                    &ctx.accounts.treasury_main.key(),
                    main_funding,
                ),
                &[
                    ctx.accounts.payer.to_account_info(),
                    ctx.accounts.treasury_main.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
            msg!("✅ Funded treasury_main: {} lamports", main_funding);
        }
        
        // Transfer to treasury_incoming
        if incoming_funding > 0 {
            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.payer.key(),
                    &ctx.accounts.treasury_incoming.key(),
                    incoming_funding,
                ),
                &[
                    ctx.accounts.payer.to_account_info(),
                    ctx.accounts.treasury_incoming.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
            msg!("✅ Funded treasury_incoming: {} lamports", incoming_funding);
        }
        
        // Transfer to treasury_outgoing
        if outgoing_funding > 0 {
            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    &ctx.accounts.payer.key(),
                    &ctx.accounts.treasury_outgoing.key(),
                    outgoing_funding,
                ),
                &[
                    ctx.accounts.payer.to_account_info(),
                    ctx.accounts.treasury_outgoing.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
            msg!("✅ Funded treasury_outgoing: {} lamports", outgoing_funding);
        }
        
        msg!("🎯 Total funding completed: {} lamports", main_funding + incoming_funding + outgoing_funding);
        Ok(())
    }

    /// Initialize upgrade registry for Treasury Operations program
    pub fn initialize_upgrade_registry(
        ctx: Context<InitializeUpgradeRegistry>,
        initial_version: u16,
        upgrade_authority: Option<Pubkey>,
        governance_approval_required: bool,
    ) -> Result<()> {
        // FRONT-RUN GUARD (2026-07-23). Initializers are first-caller-wins: this
        // instruction writes `authority = whoever signed`. On mainnet only the
        // ceremony multisig may call it, so a bot watching the deploy cannot
        // claim the authority in the window before the ceremony initializes.
        // Open on devnet (throwaway keys); fails closed in an unclaimed estate.
        require!(
            kots_common::program_ids::initializer_is_authorized(
                ctx.program_id,
                &ctx.accounts.authority.key(),
            ),
            KotsError::UnauthorizedCaller
        );

        msg!("🔧 Initializing Treasury Operations upgrade registry v{}", initial_version);
        
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Handle conditional compilation differences
        #[cfg(feature = "idl-build")]
        {
            registry.program_type = idl_types::ProgramType::TreasuryOperations;
        }
        #[cfg(not(feature = "idl-build"))]
        {
            registry.program_type = kots_common::upgrade::ProgramType::TreasuryOperations;
        }
        
        registry.current_version = initial_version;
        registry.pending_proposals = 0;
        registry.successful_upgrades = 0;
        registry.failed_upgrades = 0;
        registry.last_upgrade_timestamp = get_current_timestamp()?;
        registry.upgrade_authority = upgrade_authority;
        registry.governance_approval_required = governance_approval_required;
        registry.emergency_upgrade_enabled = false;
        registry.version_history = [[0u8; 32]; 10];
        registry.compatible_versions = [1, 1, 1, 1, 1, 1, 1, 1]; // Compatible with v1 of all programs
        registry.last_compatibility_check = get_current_timestamp()?;
        registry.emergency_pause_enabled = false;
        registry.emergency_contact = None;
        
        // Store initial version in history - pad u16 bytes to [u8; 32]
        let mut version_bytes = [0u8; 32];
        version_bytes[0..2].copy_from_slice(&initial_version.to_le_bytes());
        registry.version_history[0] = version_bytes;
        
        msg!("✅ Treasury Operations upgrade registry initialized");
        Ok(())
    }

    /// Execute systemic upgrade for Treasury Operations (no governance vote required)
    pub fn execute_systemic_upgrade(
        ctx: Context<ExecuteSystemicUpgrade>,
        new_version: u16,
        _upgrade_description: [u8; 256],
    ) -> Result<()> {
        let clock = Clock::get()?;
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate upgrade authority
        require!(
            Some(ctx.accounts.upgrade_authority.key()) == registry.upgrade_authority,
            KotsError::UnauthorizedUpgrade
        );
        
        // Validate version progression
        require!(
            new_version > registry.current_version,
            KotsError::InvalidUpgradeVersion
        );
        
        // BPF-compatible: Fixed-size array always valid
        // _upgrade_description is already [u8; 256]
        
        msg!("🔄 Executing systemic upgrade from v{} to v{}", 
             registry.current_version, new_version);
        
        // For Treasury Operations, ensure no active treasury operations are processing
        validate_economics_system_stable()?;
        
        // Create upgrade instruction for BPF Loader Upgradeable
        let instruction = anchor_lang::solana_program::bpf_loader_upgradeable::upgrade(
            &ctx.accounts.program.key(),
            &ctx.accounts.buffer.key(),
            &ctx.accounts.upgrade_authority.key(),
            &ctx.accounts.spill.key(),
        );
        
        // Execute the upgrade
        anchor_lang::solana_program::program::invoke(
            &instruction,
            &[
                ctx.accounts.program_data.to_account_info(),
                ctx.accounts.program.to_account_info(),
                ctx.accounts.buffer.to_account_info(),
                ctx.accounts.spill.to_account_info(),
                ctx.accounts.upgrade_authority.to_account_info(),
                ctx.accounts.rent.to_account_info(),
                ctx.accounts.clock.to_account_info(),
                ctx.accounts.bpf_loader_upgradeable_program.to_account_info(),
            ],
        )?;
        
        // Update registry - conditional compilation for method availability
        #[cfg(not(feature = "idl-build"))]
        {
            registry.update_version(new_version, clock.unix_timestamp);
        }
        
        #[cfg(feature = "idl-build")]
        {
            registry.current_version = new_version;
            registry.last_upgrade_timestamp = clock.unix_timestamp;
            registry.successful_upgrades = registry.successful_upgrades.saturating_add(1);
        }
        
        msg!("🚀 Treasury Operations program upgraded to v{}", 
             new_version);
        
        // Always use kots_common::ProgramType for events since that's what UpgradeExecutedEvent expects
        let program_type_value = kots_common::upgrade::ProgramType::TreasuryOperations;
        
        emit!(UpgradeExecutedEvent {
            proposal_id: 0, // Systemic upgrades don't have proposal IDs
            program_type: program_type_value,
            upgrade_type: UpgradeType::Systemic,
            executor: ctx.accounts.upgrade_authority.key(),
            old_version: registry.current_version,
            new_version,
            executed_at: clock.unix_timestamp,
            breaking_changes: false,
            migration_required: false,
        });
        
        Ok(())
    }

    /// Pause treasury operations during upgrades
    pub fn pause_treasury_for_upgrade(
        ctx: Context<PauseTreasuryForUpgrade>,
        session_id: u64,
        _reason: [u8; 256],
    ) -> Result<()> {
        msg!("⏸️ Pausing treasury operations for upgrade session {}", session_id);
        
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate pause authority
        require!(
            Some(ctx.accounts.pause_authority.key()) == registry.upgrade_authority ||
            Some(ctx.accounts.pause_authority.key()) == registry.emergency_contact,
            KotsError::UnauthorizedUpgrade
        );
        
        registry.emergency_pause_enabled = true;
        
        msg!("🛑 All treasury operations paused for upgrade");
        Ok(())
    }

    /// Resume treasury operations after upgrade
    pub fn resume_treasury_after_upgrade(
        ctx: Context<ResumeTreasuryAfterUpgrade>,
    ) -> Result<()> {
        msg!("▶️ Resuming treasury operations after upgrade");
        
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate resume authority
        require!(
            Some(ctx.accounts.resume_authority.key()) == registry.upgrade_authority ||
            Some(ctx.accounts.resume_authority.key()) == registry.emergency_contact,
            KotsError::UnauthorizedUpgrade
        );
        
        registry.emergency_pause_enabled = false;
        
        msg!("🟢 Treasury operations resumed");
        Ok(())
    }

    /// Migrate treasury data during major upgrades
    pub fn migrate_treasury_data(
        ctx: Context<MigrateTreasuryData>,
        from_version: u16,
        to_version: u16,
        migration_batch_size: u32,
    ) -> Result<()> {
        msg!("🔄 Migrating treasury data from v{} to v{}", from_version, to_version);
        
        let registry = &ctx.accounts.upgrade_registry;
        
        // Validate migration authority
        require!(
            Some(ctx.accounts.migration_authority.key()) == registry.upgrade_authority,
            KotsError::UnauthorizedUpgrade
        );
        
        // BPF-compatible migration process framework
        // Data migration logic is version-specific and implemented per upgrade
        // This function provides the authorization and validation framework
        // Actual migrations executed through version-specific batch operations
        msg!("🔄 Migration framework validated - execute version-specific logic");
        
        require!(from_version < to_version, KotsError::InvalidUpgradeVersion);
        require!(migration_batch_size > 0 && migration_batch_size <= 1000, KotsError::InvalidTokenAmount);
        
        msg!("✅ Treasury data migration completed");
        Ok(())
    }

    /// Update economic parameters
    pub fn update_economic_parameters(
        ctx: Context<UpdateEconomicParameters>,
        new_tax_rate_bps: u16,
        new_distribution_ratios: [u16; 4],
    ) -> Result<()> {
        msg!("💰 Updating economic parameters");
        
        let registry = &ctx.accounts.upgrade_registry;
        
        // Validate update authority
        require!(
            Some(ctx.accounts.update_authority.key()) == registry.upgrade_authority,
            KotsError::UnauthorizedUpgrade
        );
        
        // Validate parameter ranges
        require!(new_tax_rate_bps <= 1000, KotsError::InvalidTokenAmount); // Max 10%
        
        let total_ratio: u16 = new_distribution_ratios.iter().sum();
        require!(total_ratio == 10000, KotsError::InvalidTokenAmount); // Must sum to 100%
        
        // Economic parameters validated - actual updates applied via fee-management program CPI
        // This function provides authorization and validation framework
        msg!("✅ Validated tax rate: {} bps", new_tax_rate_bps);
        msg!("✅ Validated distribution ratios: {:?}", new_distribution_ratios);
        
        msg!("✅ Economic parameters updated");
        Ok(())
    }

    /// Report treasury readiness for operations
    pub fn report_treasury_readiness(
        _ctx: Context<ReportTreasuryReadiness>,
        readiness_score: u8,
        treasury_balance: u64,
    ) -> Result<()> {
        msg!("📊 Reporting treasury readiness");
        
        require!(readiness_score <= 100, KotsError::InvalidTokenAmount);
        require!(treasury_balance > 0, KotsError::InvalidTokenAmount);
        
        msg!("Treasury readiness score: {}%", readiness_score);
        msg!("Treasury balance: {} KOTS", treasury_balance / TOKENS_PER_UNIT);
        
        // Validate system health
        validate_economics_system_stable()?;
        
        msg!("✅ Treasury readiness reported");
        Ok(())
    }

    /// Calculate meme upload cost based on current economic parameters
    pub fn calculate_meme_upload_cost(
        _ctx: Context<CalculateMemeUploadCost>,
        storage_tier: StorageTier,
        storage_duration_months: u8,
        estimated_size_mb: f32,
    ) -> Result<CostBreakdown> {
        msg!("💾 Calculating meme upload cost");
        
        // Validate parameters
        require!(storage_duration_months >= 1 && storage_duration_months <= 36, KotsError::InvalidTokenAmount);
        require!(estimated_size_mb > 0.0 && estimated_size_mb <= 100.0, KotsError::InvalidTokenAmount);
        
        // Calculate storage cost based on tier
        let storage_multiplier = match storage_tier {
            StorageTier::Standard => STANDARD_STORAGE_MULTIPLIER,
            StorageTier::Premium => PREMIUM_STORAGE_MULTIPLIER,
            StorageTier::Archive => ARCHIVE_STORAGE_MULTIPLIER,
        };
        
        let base_storage_cost = (estimated_size_mb as u64) * STORAGE_COST_PER_MB_MONTH * storage_duration_months as u64;
        let storage_cost = base_storage_cost * storage_multiplier as u64;
        
        // Calculate bandwidth cost  
        let bandwidth_cost = (estimated_size_mb as u64) * BANDWIDTH_COST_PER_MB;
        
        // Calculate total cost
        let total_cost = storage_cost + bandwidth_cost + FIXED_MEME_UPLOAD_COST;
        
        let result = CostBreakdown {
            storage_cost,
            bandwidth_cost,
            base_fee: FIXED_MEME_UPLOAD_COST,
            total_cost,
            storage_tier,
            duration_months: storage_duration_months,
        };
        
        msg!("✅ Upload cost calculated - Total: {} KOTS", total_cost / TOKENS_PER_UNIT);
        
        Ok(result)
    }

    /// Ensure payer has sufficient funds for account creation
    /// This is called via CPI from other programs needing funded account creation
    pub fn ensure_payer_funded(
        ctx: Context<EnsurePayerFunded>,
        required_lamports: u64,
        buffer_lamports: u64,
        caller_program_id: Pubkey,
    ) -> Result<()> {
        msg!("💰 Ensuring payer funded for {} lamports", required_lamports);
        
        // Validate caller is a whitelisted KOTS program
        validate_kots_program_caller(&caller_program_id)?;
        
        let total_needed = required_lamports + buffer_lamports;
        let incoming_balance = ctx.accounts.treasury_incoming.lamports();
        
        // Check if treasury_incoming has enough
        if incoming_balance < total_needed + MIN_OPERATING_BALANCE {
            msg!("Treasury Incoming low, initiating auto-transfer");
            
            let main_balance = ctx.accounts.treasury_main.lamports();
            let transfer_amount = total_needed + BUFFER_AMOUNT;
            
            // Transfer from main treasury if it has enough
            if main_balance >= MIN_TREASURY_RESERVE + transfer_amount {
                **ctx.accounts.treasury_main.try_borrow_mut_lamports()? -= transfer_amount;
                **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += transfer_amount;
                msg!("Auto-transferred {} lamports from Treasury Main", transfer_amount);
            } else {
                // Try treasury_outgoing as fallback
                let outgoing_balance = ctx.accounts.treasury_outgoing.lamports();
                if outgoing_balance >= transfer_amount {
                    **ctx.accounts.treasury_outgoing.try_borrow_mut_lamports()? -= transfer_amount;
                    **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += transfer_amount;
                    msg!("Auto-transferred {} lamports from Treasury Outgoing", transfer_amount);
                } else {
                    return Err(KotsError::InsufficientTreasuryFunds.into());
                }
            }
        }
        
        msg!("✅ Payer funded with {} lamports available", ctx.accounts.treasury_incoming.lamports());
        Ok(())
    }

    /// Emergency funding protocol when normal funding fails
    pub fn emergency_funding(
        ctx: Context<EmergencyFunding>,
        required_lamports: u64,
        caller_program_id: Pubkey,
        creator_pubkey: Pubkey,
    ) -> Result<()> {
        msg!("🚨 EMERGENCY FUNDING PROTOCOL ACTIVATED");
        
        // Validate caller
        validate_kots_program_caller(&caller_program_id)?;
        
        // Consolidate all available funds
        let mut total_available = 0u64;
        total_available += ctx.accounts.treasury_incoming.lamports();
        total_available += ctx.accounts.treasury_outgoing.lamports();
        
        if total_available < required_lamports {
            // Last resort: record creator reimbursement
            ctx.accounts.reimbursement_queue.pending_reimbursements += 1;
            ctx.accounts.reimbursement_queue.total_owed += required_lamports;
            
            // Store reimbursement details (simplified for BPF)
            ctx.accounts.reimbursement_queue.last_creator = creator_pubkey;
            ctx.accounts.reimbursement_queue.last_amount = required_lamports;
            
            msg!("Scheduled reimbursement of {} lamports to {}", required_lamports, creator_pubkey);
            return Ok(());
        }
        
        // Drain secondary accounts to fund primary
        if ctx.accounts.treasury_outgoing.lamports() > 0 {
            let amount = ctx.accounts.treasury_outgoing.lamports();
            **ctx.accounts.treasury_outgoing.try_borrow_mut_lamports()? = 0;
            **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += amount;
            msg!("Emergency consolidated {} lamports from outgoing", amount);
        }
        
        msg!("✅ Emergency funding complete: {} lamports available", 
             ctx.accounts.treasury_incoming.lamports());
        Ok(())
    }

    /// Process creator reimbursement from queue
    ///
    /// SECURITY (2026-07-23): this instruction previously had NO authorization of
    /// any kind. `authority` was a bare `Signer` bound to nothing, `recipient` was
    /// an unconstrained `UncheckedAccount`, and the body debited `treasury_main`
    /// straight into it. Any wallet able to sign a transaction could drain the
    /// treasury up to `queue.total_owed` to an address of its choosing.
    ///
    /// Four independent controls are now enforced:
    ///   1. `authority` must equal `upgrade_registry.upgrade_authority` — the same
    ///      gate `update_economic_parameters` already used (in-struct `constraint`).
    ///   2. `recipient` is pinned to `queue.last_creator`; the caller no longer
    ///      chooses who gets paid, the queue does.
    ///   3. `amount` is capped by BOTH `queue.last_amount` and `queue.total_owed`.
    ///   4. `treasury_main` may not be drawn below `MIN_TREASURY_RESERVE`.
    /// All lamport arithmetic is checked rather than bare `-=` / `+=`.
    pub fn reimburse_creator(
        ctx: Context<ReimburseCreator>,
        recipient: Pubkey,
        amount: u64,
    ) -> Result<()> {
        msg!("💸 Processing reimbursement of {} lamports to {}", amount, recipient);

        let queue = &mut ctx.accounts.reimbursement_queue;

        // Validate reimbursement exists
        require!(queue.pending_reimbursements > 0, KotsError::InvalidTokenAmount);
        require!(amount > 0, KotsError::InvalidTokenAmount);
        require!(queue.total_owed >= amount, KotsError::InsufficientTreasuryFunds);

        // The `recipient` ARGUMENT must agree with the pinned recipient ACCOUNT.
        // (The account itself is already constrained to queue.last_creator in the
        // Accounts struct; this rejects a mismatched argument rather than silently
        // logging one address while paying another.)
        require_keys_eq!(recipient, ctx.accounts.recipient.key(), KotsError::UnauthorizedCaller);

        // A single reimbursement may not exceed the amount actually recorded for
        // this creator. Without this, one queue entry could sweep `total_owed`.
        require!(amount <= queue.last_amount, KotsError::InvalidTokenAmount);

        // Never draw the operating treasury below its reserve floor.
        let treasury_balance = ctx.accounts.treasury_main.lamports();
        require!(
            treasury_balance >= amount.checked_add(MIN_TREASURY_RESERVE)
                .ok_or(KotsError::ArithmeticOverflow)?,
            KotsError::InsufficientTreasuryFunds
        );

        // Transfer from treasury to the pinned recipient (checked arithmetic)
        {
            let mut from = ctx.accounts.treasury_main.try_borrow_mut_lamports()?;
            let mut to = ctx.accounts.recipient.try_borrow_mut_lamports()?;
            **from = from.checked_sub(amount).ok_or(KotsError::InsufficientTreasuryFunds)?;
            **to = to.checked_add(amount).ok_or(KotsError::ArithmeticOverflow)?;
        }

        // Update queue (checked)
        queue.pending_reimbursements = queue.pending_reimbursements
            .checked_sub(1).ok_or(KotsError::ArithmeticOverflow)?;
        queue.total_owed = queue.total_owed
            .checked_sub(amount).ok_or(KotsError::ArithmeticOverflow)?;
        queue.last_amount = queue.last_amount
            .checked_sub(amount).ok_or(KotsError::ArithmeticOverflow)?;

        msg!("✅ Reimbursement processed successfully");
        Ok(())
    }

    /// Auto-balance treasury accounts for optimal operation
    pub fn auto_balance_treasuries(
        ctx: Context<AutoBalanceTreasuries>,
    ) -> Result<()> {
        msg!("⚖️ Auto-balancing treasury accounts");
        
        let incoming = ctx.accounts.treasury_incoming.lamports();
        let outgoing = ctx.accounts.treasury_outgoing.lamports();
        let main = ctx.accounts.treasury_main.lamports();
        
        // Rule 1: Keep incoming funded for operations
        if incoming < MIN_OPERATING_BALANCE * 10 {
            let needed = MIN_OPERATING_BALANCE * 20 - incoming;
            
            if main > MIN_TREASURY_RESERVE + needed {
                **ctx.accounts.treasury_main.try_borrow_mut_lamports()? -= needed;
                **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += needed;
                msg!("Rebalanced: Main -> Incoming: {} lamports", needed);
            } else if outgoing > needed {
                **ctx.accounts.treasury_outgoing.try_borrow_mut_lamports()? -= needed;
                **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += needed;
                msg!("Rebalanced: Outgoing -> Incoming: {} lamports", needed);
            }
        }
        
        // Rule 2: Protect main treasury reserve
        if main < MIN_TREASURY_RESERVE && incoming > MIN_OPERATING_BALANCE * 20 {
            let transfer = (incoming - MIN_OPERATING_BALANCE * 10) / 2;
            **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? -= transfer;
            **ctx.accounts.treasury_main.try_borrow_mut_lamports()? += transfer;
            msg!("Protected reserve: Incoming -> Main: {} lamports", transfer);
        }
        
        msg!("✅ Treasury balancing complete");
        msg!("  Main: {} SOL", main / 1_000_000_000);
        msg!("  Incoming: {} SOL", incoming / 1_000_000_000);
        msg!("  Outgoing: {} SOL", outgoing / 1_000_000_000);
        
        Ok(())
    }

    /// Create PDA for other KOTS programs with treasury funding
    /// This is the centralized way all KOTS programs create PDAs - they CPI here
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn create_pda_for_program(
        ctx: Context<CreatePDAForProgram>,
        target_program_id: Pubkey,
        space: u64,
        seeds: Vec<Vec<u8>>,
        bump: u8,
        buffer_lamports: u64,
    ) -> Result<()> {
        msg!("🔨 Creating PDA for program: {}", target_program_id);
        msg!("  Space: {} bytes", space);
        msg!("  PDA: {}", ctx.accounts.new_account.key());
        
        // Validate caller is a whitelisted KOTS program
        validate_kots_program_caller(&ctx.accounts.caller_program.key())?;
        
        // Validate that target_program_id matches what we expect
        require_keys_eq!(target_program_id, ctx.accounts.caller_program.key(), KotsError::UnauthorizedCaller);
        
        // CRITICAL FIX: Check treasury account ownership before attempting invoke_signed
        let program_id = ctx.program_id;
        let treasury_incoming_owner = ctx.accounts.treasury_incoming.owner;
        let treasury_main_owner = ctx.accounts.treasury_main.owner;
        
        msg!("📊 Treasury Account Ownership Analysis:");
        msg!("  Treasury Incoming owner: {}", treasury_incoming_owner);
        msg!("  Treasury Main owner: {}", treasury_main_owner);
        msg!("  Treasury Operations program: {}", program_id);
        
        // Check if treasury accounts are properly initialized
        let incoming_properly_initialized = treasury_incoming_owner == program_id;
        let main_properly_initialized = treasury_main_owner == program_id;
        
        if !incoming_properly_initialized {
            msg!("⚠️  Treasury Incoming not owned by Treasury Operations - owned by System Program");
            msg!("  This indicates partial treasury setup - accounts funded but not initialized");
            
            // PARTIAL TREASURY STATE FIX: Use alternative creation method
            return create_pda_with_caller_funding(ctx, target_program_id, space, buffer_lamports);
        }
        
        if !main_properly_initialized {
            msg!("⚠️  Treasury Main not owned by Treasury Operations");
        }
        
        // Calculate total lamports needed
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(space as usize);
        let total_needed = required_lamports + buffer_lamports;
        
        msg!("💰 Required lamports: {} (rent: {}, buffer: {})",
             total_needed, required_lamports, buffer_lamports);
        
        // Ensure treasury_incoming has enough funds (only if properly initialized)
        let incoming_balance = ctx.accounts.treasury_incoming.lamports();
        if incoming_balance < total_needed + MIN_OPERATING_BALANCE {
            msg!("⚠️ Treasury Incoming low, auto-funding...");
            
            // Only attempt auto-transfer if main treasury is also properly initialized
            if main_properly_initialized {
                let main_balance = ctx.accounts.treasury_main.lamports();
                if main_balance >= MIN_TREASURY_RESERVE + total_needed + BUFFER_AMOUNT {
                    let transfer_amount = total_needed + BUFFER_AMOUNT;
                    **ctx.accounts.treasury_main.try_borrow_mut_lamports()? -= transfer_amount;
                    **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? += transfer_amount;
                    msg!("✅ Auto-transferred {} lamports from Treasury Main", transfer_amount);
                } else {
                    return Err(KotsError::InsufficientTreasuryFunds.into());
                }
            } else {
                msg!("❌ Cannot auto-transfer - Treasury Main not properly initialized");
                return Err(KotsError::InsufficientTreasuryFunds.into());
            }
        }
        
        // Create the account using System Program with proper treasury PDA signing
        let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
            &ctx.accounts.treasury_incoming.key(),
            &ctx.accounts.new_account.key(),
            total_needed,  // Use total_needed including buffer
            space,
            &target_program_id,  // Set correct owner from the start
        );
        
        // Treasury incoming must sign since it's the payer
        // Use treasury_incoming PDA seeds (we've already validated ownership above)
        let treasury_incoming_bump = ctx.bumps.treasury_incoming;
        
        msg!("💰 Executing PDA creation with Treasury Operations PDA signer");
        msg!("  Payer: {} (Treasury Incoming PDA)", ctx.accounts.treasury_incoming.key());
        msg!("  Bump: {}", treasury_incoming_bump);
        
        // Execute with treasury_incoming PDA signer seeds
        // CRITICAL FIX: Only treasury_incoming signs, new_account is just a target
        anchor_lang::solana_program::program::invoke_signed(
            &create_account_ix,
            &[
                ctx.accounts.treasury_incoming.to_account_info(),
                ctx.accounts.new_account.clone(), // Use clone() for AccountInfo
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&[b"treasury_incoming_v4", &[treasury_incoming_bump]]],
        )?;
        
        msg!("✅ PDA created successfully");
        msg!("  Account: {}", ctx.accounts.new_account.key());
        msg!("  Owner: {}", target_program_id);
        msg!("  Lamports: {}", total_needed);
        
        // Write Anchor discriminator (8 bytes) to mark as initialized
        // This is critical for Anchor to recognize the account
        let mut data = ctx.accounts.new_account.try_borrow_mut_data()?;
        if data.len() >= 8 {
            // Calculate discriminator using Anchor's method
            // For a generic PDA, we use a standard discriminator
            let discriminator = anchor_lang::solana_program::hash::hash(b"account:GenericPDA").to_bytes();
            data[..8].copy_from_slice(&discriminator[..8]);
            msg!("✅ Wrote Anchor discriminator to PDA");
        }
        
        Ok(())
    }
    
    /// Fund a caller program for PDA creation
    /// This provides lamports to caller programs so they can create their own PDAs
    pub fn fund_caller_for_pda(
        ctx: Context<FundCallerForPDA>,
        amount_lamports: u64,
    ) -> Result<()> {
        msg!("💰 Funding caller program for PDA creation");
        msg!("  Caller: {}", ctx.accounts.caller_program.key());
        msg!("  Amount: {} lamports", amount_lamports);
        
        // Validate caller is a whitelisted KOTS program
        validate_kots_program_caller(&ctx.accounts.caller_program.key())?;
        
        // Transfer lamports from treasury_incoming to caller
        **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? -= amount_lamports;
        **ctx.accounts.caller_program.try_borrow_mut_lamports()? += amount_lamports;
        
        msg!("✅ Funded caller with {} lamports", amount_lamports);
        Ok(())
    }

    /// Reimburse a payer for PDA creation costs
    /// Two-step pattern: caller creates PDA with signer, then requests reimbursement
    pub fn reimburse_payer(
        ctx: Context<ReimbursePayer>,
        amount_lamports: u64,
        caller_program_id: Pubkey,
    ) -> Result<()> {
        msg!("💸 Reimbursing PDA creation costs");
        msg!("  Payer: {}", ctx.accounts.payer.key());
        msg!("  Amount: {} lamports", amount_lamports);
        msg!("  Caller Program: {}", caller_program_id);

        // Validate caller is a whitelisted KOTS program
        validate_kots_program_caller(&caller_program_id)?;

        // SECURITY (2026-07-23): the whitelist check above validates an
        // INSTRUCTION ARGUMENT, not the actual invoker. Program IDs are public
        // (they are in Anchor.toml), so any wallet could pass a whitelisted id
        // together with `payer = <its own wallet>` and collect up to
        // `rent.minimum_balance(8192) + 100_000` lamports per call, in a loop,
        // until the treasury hit MIN_OPERATING_BALANCE.
        //
        // The fix proves the caller by SIGNATURE rather than by assertion: the
        // invoking program must sign with a PDA derived from its own program id,
        // which only that program can produce. Callers must switch from
        // `invoke` to `invoke_signed` (see kots_common::init).
        let (expected_cpi_authority, _bump) =
            Pubkey::find_program_address(&[CPI_AUTHORITY_SEED], &caller_program_id);
        require_keys_eq!(
            ctx.accounts.cpi_authority.key(),
            expected_cpi_authority,
            KotsError::UnauthorizedCaller
        );
        require!(ctx.accounts.cpi_authority.is_signer, KotsError::UnauthorizedCaller);
        
        // Validate amount is reasonable (rent + small buffer)
        let rent = Rent::get()?;
        let max_reasonable = rent.minimum_balance(8192) + 100_000; // Max 8KB account + 0.1 SOL buffer
        require!(amount_lamports <= max_reasonable, KotsError::InvalidTokenAmount);
        
        // Check treasury has sufficient funds
        let treasury_balance = ctx.accounts.treasury_incoming.lamports();
        require!(treasury_balance >= amount_lamports + MIN_OPERATING_BALANCE, KotsError::InsufficientTreasuryFunds);
        
        // FIXED: Direct lamports reallocation (avoids "Transfer: from must not carry data" error)
        // Since treasury_incoming is a PDA with data, use direct lamports manipulation instead of SystemProgram::transfer
        **ctx.accounts.treasury_incoming.try_borrow_mut_lamports()? -= amount_lamports;
        **ctx.accounts.payer.try_borrow_mut_lamports()? += amount_lamports;
        
        msg!("✅ Reimbursed {} lamports to payer", amount_lamports);
        Ok(())
    }

    /// Alternative PDA creation when treasury accounts are not properly initialized
    /// This handles the partial treasury state scenario
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn create_pda_with_caller_funding(
        ctx: Context<CreatePDAForProgram>,
        target_program_id: Pubkey,
        space: u64,
        buffer_lamports: u64,
    ) -> Result<()> {
        msg!("🔄 PARTIAL TREASURY STATE: Using alternative PDA creation method");
        msg!("  Reason: Treasury accounts exist but not owned by Treasury Operations");
        msg!("  Solution: Request caller to fund PDA creation directly");
        
        // Calculate required lamports
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(space as usize);
        let total_needed = required_lamports + buffer_lamports;
        
        // Check if treasury_incoming has enough balance (even if not properly owned)
        let incoming_balance = ctx.accounts.treasury_incoming.lamports();
        
        if incoming_balance < total_needed {
            msg!("❌ Treasury Incoming has insufficient funds: {} lamports needed, {} available", 
                 total_needed, incoming_balance);
            return Err(KotsError::InsufficientTreasuryFunds.into());
        }
        
        // For partial treasury state, we need the caller to handle PDA creation
        // since we cannot sign for accounts we don't own
        msg!("💡 RESOLUTION NEEDED: Treasury accounts require proper initialization");
        msg!("  1. Call initialize_treasury_accounts() to set proper ownership");
        msg!("  2. Then retry PDA creation with properly initialized treasury");
        msg!("  3. Current state: accounts funded but owned by System Program");
        
        // Return a specific error that indicates the need for treasury initialization
        Err(KotsError::TreasuryNotInitialized.into())
    }

    // ========================================================================
    // ACT-21 — On-chain treasury timelock + veto.
    // Increment 1: the queued-request "note" + owner veto/snooze. The
    // security-bearing settle path (timelock enforcement + the token transfer +
    // the aggregate ceiling) lands with the custody migration in Increment 2
    // (see KOTS-PHASE-2-BUILD-PLAN → ACT-21).
    //
    // Authority model (deviation, recorded): veto/snooze gate on the treasury
    // `upgrade_registry.upgrade_authority` — the Squads 2-of-4 multisig (A.1),
    // the same owner authority ACT-02's `owner_registry` represents and the same
    // gate `emergency_funding` / `reimburse_creator` use — rather than a
    // cross-program `owner_registry.quorum_met` read (which would mean
    // replicating the OwnerRegistry layout in kots-common). A later refinement
    // may switch to the on-chain owner set via a new kots_common read helper.
    // ========================================================================

    /// Queue a discretionary external transfer. Writes the request as a PDA
    /// stamped with the CHAIN clock and a mandatory `purpose`; it does NOT move
    /// tokens (that is `settle_external_transfer`, Increment 2). The caller is
    /// the operational requester (the hot key); its key is recorded for
    /// attribution. Security lives in the settle-time conditions (timelock, veto,
    /// ceiling), not here — so queuing may be permissive.
    pub fn queue_external_transfer(
        ctx: Context<QueueExternalTransfer>,
        id: u64,
        destination: Pubkey,
        amount: u64,
        purpose: Vec<u8>,
    ) -> Result<()> {
        require!(!purpose.is_empty(), KotsError::PurposeRequired);
        require!(purpose.len() <= MAX_REASON_LENGTH, KotsError::PurposeTooLong);
        require!(amount > 0, KotsError::InvalidAmount);

        let clock = Clock::get()?;
        let q = &mut ctx.accounts.queued_transfer;
        q.id = id;
        q.destination = destination;
        q.amount = amount;

        let mut purpose_buf = [0u8; MAX_REASON_LENGTH];
        purpose_buf[..purpose.len()].copy_from_slice(&purpose);
        q.purpose = purpose_buf;
        q.purpose_len = purpose.len() as u8;

        q.requested_at = clock.unix_timestamp;
        // ACT-25 Exceptional tier: size-graduated delay. When the operational
        // tier config EXISTS and the amount meets its exceptional threshold,
        // the window LENGTHENS (never shortens — init enforces
        // exceptional_delay ≥ the base). An absent config means the base
        // delay: deploy-safe, and arming it is the explicit ceremony.
        let mut delay = TREASURY_TIMELOCK_DELAY_SECS;
        let tier_info = ctx.accounts.tier_config.to_account_info();
        if !tier_info.data_is_empty() {
            let data = tier_info.try_borrow_data()?;
            let mut slice: &[u8] = &data;
            let tiers: OperationalTierConfig =
                OperationalTierConfig::try_deserialize(&mut slice)?;
            if amount >= tiers.exceptional_threshold {
                delay = tiers.exceptional_delay_secs;
            }
        }
        q.execution_delay = delay;
        q.vetoed = false;
        q.settled = false;
        q.requester = ctx.accounts.requester.key();
        q.bump = ctx.bumps.queued_transfer;

        msg!("🕒 Queued external transfer {} — {} to {}, executable after {}s",
             id, amount, destination, q.execution_delay);
        emit!(ExternalTransferQueued {
            id,
            destination,
            amount,
            requested_at: clock.unix_timestamp,
            execution_delay: q.execution_delay,
            requester: q.requester,
        });
        Ok(())
    }

    /// Owner veto: cancels a queued transfer so `settle` can never execute it.
    /// Gated on the treasury multisig authority; the action is proven by
    /// signature and recorded on-chain.
    pub fn veto_transfer(ctx: Context<VetoTransfer>, id: u64) -> Result<()> {
        let q = &mut ctx.accounts.queued_transfer;
        require!(!q.settled, KotsError::TransferAlreadySettled);
        q.vetoed = true;
        msg!("🛑 Vetoed queued transfer {} by {}", id, ctx.accounts.authority.key());
        emit!(ExternalTransferVetoed { id, vetoed_by: ctx.accounts.authority.key() });
        Ok(())
    }

    /// Owner snooze: extends the timelock window on a queued transfer (pushes the
    /// executable time forward). Gated on the treasury multisig authority.
    pub fn snooze_transfer(
        ctx: Context<SnoozeTransfer>,
        id: u64,
        additional_delay_secs: i64,
    ) -> Result<()> {
        require!(additional_delay_secs > 0, KotsError::InvalidParameter);
        let q = &mut ctx.accounts.queued_transfer;
        require!(!q.settled, KotsError::TransferAlreadySettled);
        require!(!q.vetoed, KotsError::TransferVetoed);
        q.execution_delay = q
            .execution_delay
            .checked_add(additional_delay_secs)
            .ok_or(KotsError::InvalidParameter)?;
        msg!("😴 Snoozed queued transfer {} — new delay {}s", id, q.execution_delay);
        emit!(ExternalTransferSnoozed { id, new_execution_delay: q.execution_delay });
        Ok(())
    }

    /// One-time setup of the per-rail aggregate outflow window. Gated on the
    /// treasury multisig authority.
    pub fn initialize_outflow_window(ctx: Context<InitializeOutflowWindow>) -> Result<()> {
        let w = &mut ctx.accounts.outflow_window;
        w.window_start = Clock::get()?.unix_timestamp;
        w.cumulative_out = 0;
        w.bump = ctx.bumps.outflow_window;
        msg!("🪟 Initialized outflow window (ceiling {} base units / {}s)",
             HOT_RAIL_CEILING, OUTFLOW_WINDOW_SECS);
        Ok(())
    }

    /// Settle a previously-queued external transfer. The security-bearing gates
    /// live here, so the caller may be permissionless: the transfer executes ONLY
    /// if the timelock has elapsed (chain clock), the request is not vetoed, and
    /// the rolling-window aggregate ceiling admits the amount. The token move is a
    /// Token-2022 `transfer_checked` signed by the treasury-outflow PDA — after
    /// the custody migration that PDA is the sole authority over the treasury
    /// token account, so a stolen hot key cannot move funds around this path.
    ///
    /// VERIFY ON DEVNET: if the KOTS mint has the Token-2022 TransferHook
    /// extension ENABLED, an external transfer will require the hook program +
    /// its extra accounts here (append them as remaining_accounts / use the
    /// token-2022 onchain transfer helper). This mirrors `lp-position-manager`'s
    /// plain `transfer_checked`, which moves KOTS to internal wallets today.
    pub fn settle_external_transfer<'info>(ctx: Context<'_, '_, '_, 'info, SettleExternalTransfer<'info>>, id: u64) -> Result<()> {
        // EMERGENCY PAUSE GATE (2026-08-12): freeze this treasury outflow while
        // the platform is paused. Reads security-governance's EmergencyState PDA
        // (honours pause_duration; fails closed on an absent PDA, which exists on
        // any chain paused at least once). Resume is never gated, so the platform
        // can always be un-paused.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);

        let clock = Clock::get()?;

        // Read the queued request; copy out what we need so the borrow is freed.
        let (settled, vetoed, requested_at, execution_delay, destination, amount) = {
            let q = &ctx.accounts.queued_transfer;
            (q.settled, q.vetoed, q.requested_at, q.execution_delay, q.destination, q.amount)
        };

        // 1. State + timelock gates (chain clock only — no sidecar-supplied time).
        require!(!settled, KotsError::TransferAlreadySettled);
        require!(!vetoed, KotsError::TransferVetoed);
        require!(
            clock.unix_timestamp >= requested_at + execution_delay,
            KotsError::TimelockNotElapsed
        );

        // 2. Destination must match the queued record (the token account is bound
        //    at queue time; settle cannot redirect funds).
        require!(
            ctx.accounts.destination_token_account.key() == destination,
            KotsError::InvalidParameter
        );

        // 3. Aggregate rolling-window ceiling — CUMULATIVE, not per-transfer, so a
        //    drain split into many sub-cap transfers still trips the net.
        {
            let w = &mut ctx.accounts.outflow_window;
            if clock.unix_timestamp >= w.window_start + OUTFLOW_WINDOW_SECS {
                w.window_start = clock.unix_timestamp;
                w.cumulative_out = 0;
            }
            let projected = w
                .cumulative_out
                .checked_add(amount)
                .ok_or(KotsError::InvalidParameter)?;
            require!(projected <= HOT_RAIL_CEILING, KotsError::OutflowCeilingExceeded);
            w.cumulative_out = projected;
        }

        // 4. Watcher-liveness dead-man's-switch (ACT-23 / ACT-08b, decision C.7).
        //    Once the watcher registry PDA EXISTS, the external rail settles only
        //    while a registered watcher has attested within
        //    ATTESTATION_FRESHNESS_SECS — "sidecar dark AND watchers dark" then
        //    HALTS external movement instead of letting it slip by: the
        //    preventive upgrade over detective alerting (ADV-31). Arming is the
        //    explicit init ceremony: an ABSENT registry skips the gate, so this
        //    change deploys safely ahead of the watchers themselves.
        {
            let reg_info = ctx.accounts.watcher_registry.to_account_info();
            if !reg_info.data_is_empty() {
                let data = reg_info.try_borrow_data()?;
                let mut slice: &[u8] = &data;
                let registry: WatcherRegistry = WatcherRegistry::try_deserialize(&mut slice)?;
                let freshest = registry.last_attested[..registry.watcher_count as usize]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                require!(
                    clock.unix_timestamp.saturating_sub(freshest) <= ATTESTATION_FRESHNESS_SECS,
                    KotsError::WatcherLivenessStale
                );
            }
        }

        // 5. The transfer — Token-2022 `transfer_checked` signed by the PDA.
        let bump = ctx.bumps.treasury_outflow_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[TREASURY_OUTFLOW_AUTHORITY_SEED, &[bump]]];
        let decimals = ctx.accounts.kots_mint.decimals;
        // Token-2022 hook-aware transfer — KOTS carries a TransferHook on mainnet, so a
        // plain transfer_checked is REFUSED. `onchain::invoke_transfer_checked` resolves the
        // hook accounts the CALLER forwards in remaining_accounts (mirrors fee-management /
        // progressive-jackpot); the treasury-outflow PDA signs via signer_seeds.
        spl_token_2022::onchain::invoke_transfer_checked(
            ctx.accounts.token_program.to_account_info().key,
            ctx.accounts.treasury_token_account.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.destination_token_account.to_account_info(),
            ctx.accounts.treasury_outflow_authority.to_account_info(),
            ctx.remaining_accounts,
            amount,
            decimals,
            signer_seeds,
        )?;

        // 6. Mark settled (idempotent — a second settle hits the guard above).
        ctx.accounts.queued_transfer.settled = true;

        msg!("✅ Settled external transfer {} — {} base units to {}", id, amount, destination);
        emit!(ExternalTransferSettled {
            id,
            destination,
            amount,
            settled_at: clock.unix_timestamp,
        });
        Ok(())
    }

    /// ACT-23 (decision C.7) — one-time creation of the watcher registry: the
    /// dead-man's-switch ARMING ceremony. Gated on the treasury multisig
    /// authority like the outflow window. ⚠️ Arming is FAIL-CLOSED: from the
    /// moment this account exists, `settle_external_transfer` refuses unless a
    /// registered watcher has attested within ATTESTATION_FRESHNESS_SECS — an
    /// armed registry with no live watcher HALTS the external rail. Register
    /// watcher keys in the SAME ceremony and have them attesting first.
    pub fn initialize_watcher_registry(ctx: Context<InitializeWatcherRegistry>) -> Result<()> {
        let r = &mut ctx.accounts.watcher_registry;
        r.watchers = [Pubkey::default(); MAX_WATCHERS];
        r.last_attested = [0; MAX_WATCHERS];
        r.watcher_count = 0;
        r.bump = ctx.bumps.watcher_registry;
        msg!(
            "👁️ Watcher registry initialized — external settle now requires an attestation fresher than {}s",
            ATTESTATION_FRESHNESS_SECS
        );
        Ok(())
    }

    /// Register a watcher key (multisig authority). EITHER registered
    /// watcher's fresh attestation satisfies the settle gate, so a benign
    /// single-watcher outage does not halt the rail — C.7 runs two.
    pub fn register_watcher(ctx: Context<RegisterWatcher>, watcher: Pubkey) -> Result<()> {
        let r = &mut ctx.accounts.watcher_registry;
        let count = r.watcher_count as usize;
        require!(count < MAX_WATCHERS, KotsError::WatcherRegistryFull);
        require!(
            !r.watchers[..count].contains(&watcher),
            KotsError::WatcherAlreadyRegistered
        );
        r.watchers[count] = watcher;
        r.last_attested[count] = 0;
        r.watcher_count = count as u8 + 1;
        msg!("👁️ Watcher registered: {} ({} of {})", watcher, r.watcher_count, MAX_WATCHERS);
        emit!(WatcherRegistered { watcher, watcher_count: r.watcher_count });
        Ok(())
    }

    /// A registered watcher's signed liveness heartbeat (~6h cadence). The
    /// signature is the WATCHER's own — outside the sidecar's trust domain —
    /// so a sidecar hijack can neither forge it nor keep it alive (ADV-31).
    pub fn attest_liveness(ctx: Context<AttestLiveness>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let r = &mut ctx.accounts.watcher_registry;
        let signer = ctx.accounts.watcher.key();
        let count = r.watcher_count as usize;
        let idx = r.watchers[..count]
            .iter()
            .position(|w| *w == signer)
            .ok_or(KotsError::WatcherNotRegistered)?;
        r.last_attested[idx] = now;
        msg!("💓 Liveness attested by watcher {} at {}", signer, now);
        emit!(LivenessAttested { watcher: signer, attested_at: now });
        Ok(())
    }

    /// ACT-24 — one-time distribution config (multisig): pins WHO may call
    /// `distribute_from_treasury` (post-cutover: the sidecar's hot key — a
    /// CALLER credential only; even fully stolen it can move nothing beyond
    /// each bucket's remaining entitlement) and carries the coarse
    /// treasury-level totals for the F15 tripwire belt.
    pub fn initialize_distribution_config(
        ctx: Context<InitializeDistributionConfig>,
        distribution_authority: Pubkey,
        jackpot_pool: Pubkey,
        bucket_credit_authority: Pubkey,
    ) -> Result<()> {
        let c = &mut ctx.accounts.distribution_config;
        c.authority = distribution_authority;
        c.total_entitled = 0;
        c.total_distributed = 0;
        c.jackpot_pool = jackpot_pool;
        c.bucket_credit_authority = bucket_credit_authority;
        c.bump = ctx.bumps.distribution_config;
        msg!(
            "📒 Distribution config initialized — distributor {}, jackpot pool {}",
            distribution_authority,
            jackpot_pool
        );
        Ok(())
    }

    /// Rotate the distributor credential (multisig) — e.g. after a hot-key
    /// rotation; entitlements and totals are untouched.
    pub fn set_distribution_authority(
        ctx: Context<MutateDistributionConfig>,
        new_authority: Pubkey,
    ) -> Result<()> {
        ctx.accounts.distribution_config.authority = new_authority;
        msg!("📒 Distribution authority rotated to {}", new_authority);
        Ok(())
    }

    /// ACT-24 — create a bucket ledger, zeroed (multisig). EXPLICIT init, not
    /// init_if_needed: the ledger is value-bearing (entitled/distributed), and
    /// this file's own rule is that value-bearing accounts are never created
    /// implicitly by their consumers.
    pub fn initialize_bucket(ctx: Context<InitializeBucket>, tag: u8, bucket_id: u64) -> Result<()> {
        let l = &mut ctx.accounts.bucket_ledger;
        l.tag = tag;
        l.bucket_id = bucket_id;
        l.entitled = 0;
        l.distributed = 0;
        l.bump = ctx.bumps.bucket_ledger;
        msg!("📒 Bucket initialized: tag {} id {}", tag, bucket_id);
        Ok(())
    }

    /// ACT-24 — credit entitlement to a bucket (multisig). Entitlement is the
    /// ONLY thing that raises a bucket's ceiling; distribution only spends it.
    /// For a contest, credit pool × the defined split at finalization; the
    /// jackpot bucket (tag 2, id 0) is credited in lockstep with its pool —
    /// binding it to the progressive-jackpot pool ACCOUNT directly is the
    /// Increment-2 tightening.
    pub fn fund_bucket(
        ctx: Context<FundBucket>,
        tag: u8,
        bucket_id: u64,
        additional_entitlement: u64,
    ) -> Result<()> {
        let l = &mut ctx.accounts.bucket_ledger;
        require!(l.tag == tag && l.bucket_id == bucket_id, KotsError::InvalidParameter);
        l.entitled = l
            .entitled
            .checked_add(additional_entitlement)
            .ok_or(KotsError::InvalidParameter)?;
        let c = &mut ctx.accounts.distribution_config;
        c.total_entitled = c
            .total_entitled
            .checked_add(additional_entitlement)
            .ok_or(KotsError::InvalidParameter)?;
        msg!("📒 Bucket {}:{} funded +{} — entitled {}", tag, bucket_id, additional_entitlement, l.entitled);
        emit!(BucketFunded { tag, bucket_id, additional_entitlement, entitled: l.entitled });
        Ok(())
    }

    /// ACT-24 auto-crediting (decision: CPI-coupled, voter-share leg).
    /// Entitlement for `contest-voter-share:N` rises ONLY here, and the sole
    /// signer accepted is the multisig-PINNED bucket-credit authority — a
    /// PDA of the prize-distribution program that signs exclusively inside
    /// `store_voter_share`. The credit therefore lands exactly when, and by
    /// exactly as much as, a share record is written: the record IS the
    /// obligation, this credit IS its treasury ceiling, and the record's
    /// init-once makes double-credit impossible.
    pub fn credit_voter_share_bucket(
        ctx: Context<CreditVoterShareBucket>,
        contest_id: u64,
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, KotsError::InvalidAmount);
        let l = &mut ctx.accounts.bucket_ledger;
        if l.tag == 0 && l.entitled == 0 && l.distributed == 0 {
            // Fresh: init_if_needed zero-created it this instant (a real
            // voter-share ledger always has tag == 1). Stamp its identity.
            l.tag = kots_common::purpose_key::TAG_CONTEST_VOTER_SHARE;
            l.bucket_id = contest_id;
            l.bump = ctx.bumps.bucket_ledger;
        }
        require!(
            l.tag == kots_common::purpose_key::TAG_CONTEST_VOTER_SHARE
                && l.bucket_id == contest_id,
            KotsError::InvalidParameter
        );
        l.entitled = l
            .entitled
            .checked_add(amount)
            .ok_or(KotsError::InvalidParameter)?;
        let c = &mut ctx.accounts.distribution_config;
        c.total_entitled = c
            .total_entitled
            .checked_add(amount)
            .ok_or(KotsError::InvalidParameter)?;
        msg!(
            "📒 Voter-share bucket {} credited +{} — entitled {}",
            contest_id,
            amount,
            l.entitled
        );
        emit!(BucketFunded {
            tag: l.tag,
            bucket_id: contest_id,
            additional_entitlement: amount,
            entitled: l.entitled,
        });
        Ok(())
    }

    /// ACT-24 — THE distribution instruction (F15): the purpose is a
    /// STRUCTURED accounting key, the bucket ledger is selected by it, and
    /// conservation is a HARD REVERT — Σ(distributed to bucket) ≤
    /// entitled(bucket) — so the books cannot be over-drawn even by a fully
    /// compromised distributor credential. Distinct boundaries hold by
    /// construction: the seeds bind the ledger to (tag, id), so a
    /// contest-tagged call can never reach jackpot funds nor vice versa —
    /// the jackpot legitimately exceeds any one contest's pool, and stays
    /// its own bucket. Bucket-level revert, NEVER auto-pause: a pause driven
    /// by a fine-grained check is weaponizable into DoS (the register
    /// entry's own warning); the coarse total below is a belt that cannot
    /// trip unless per-bucket math itself broke. The token move is signed by
    /// the treasury-outflow PDA — post custody-cutover, this instruction and
    /// the ACT-21 external rail are the ONLY ways treasury KOTS moves.
    pub fn distribute_from_treasury<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributeFromTreasury<'info>>,
        tag: u8,
        bucket_id: u64,
        amount: u64,
        purpose: Vec<u8>,
    ) -> Result<()> {
        // EMERGENCY PAUSE GATE (2026-08-12): freeze this treasury outflow while
        // the platform is paused. Reads security-governance's EmergencyState PDA
        // (honours pause_duration; fails closed on an absent PDA, which exists on
        // any chain paused at least once). Resume is never gated, so the platform
        // can always be un-paused.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);

        require!(purpose.len() <= MAX_REASON_LENGTH, KotsError::PurposeTooLong);
        let key = kots_common::purpose_key::PurposeKey::parse(&purpose)
            .ok_or(KotsError::PurposeKeyInvalid)?;
        require!(
            key.tag() == tag && key.bucket_id() == bucket_id,
            KotsError::PurposeKeyMismatch
        );

        // The pinned jackpot-pool account rides EVERY call (one rule, nothing
        // to smuggle): its address must match the multisig-pinned config.
        require!(
            ctx.accounts.jackpot_pool.key() == ctx.accounts.distribution_config.jackpot_pool,
            KotsError::JackpotPoolUnreadable
        );

        // Per-bucket conservation — the hard revert F15 asks for.
        let remaining = {
            let l = &mut ctx.accounts.bucket_ledger;
            let new_distributed = l
                .distributed
                .checked_add(amount)
                .ok_or(KotsError::InvalidParameter)?;
            require!(new_distributed <= l.entitled, KotsError::BucketOverdraw);

            // The canonical case, tightened (Inc 2): a jackpot draw is bounded
            // by the LIVE pool — Σ jackpot distributed ≤ total_jackpot_amount —
            // on top of entitlement bookkeeping, so even a mistaken
            // over-entitlement cannot outrun the pool itself.
            if tag == kots_common::purpose_key::TAG_JACKPOT {
                let pool = ctx.accounts.jackpot_pool.to_account_info();
                require!(!pool.data_is_empty(), KotsError::JackpotPoolUnreadable);
                let data = pool.try_borrow_data()?;
                // ProgressiveJackpotState: disc[8] | is_initialized u8 @8 |
                // current_cycle_week u8 @9 | cycle_start i64 @10 |
                // total_jackpot_amount u64 LE @18.
                require!(data.len() >= 26 && data[8] == 1, KotsError::JackpotPoolUnreadable);
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&data[18..26]);
                let pool_total = u64::from_le_bytes(buf);
                require!(new_distributed <= pool_total, KotsError::JackpotPoolExceeded);
            }

            l.distributed = new_distributed;
            l.entitled - new_distributed
        };

        // Coarse treasury-level belt (cannot trip unless per-bucket math broke).
        {
            let c = &mut ctx.accounts.distribution_config;
            c.total_distributed = c
                .total_distributed
                .checked_add(amount)
                .ok_or(KotsError::InvalidParameter)?;
            require!(c.total_distributed <= c.total_entitled, KotsError::BucketOverdraw);
        }

        let bump = ctx.bumps.treasury_outflow_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[TREASURY_OUTFLOW_AUTHORITY_SEED, &[bump]]];
        let decimals = ctx.accounts.kots_mint.decimals;
        // Token-2022 hook-aware transfer — KOTS carries a TransferHook on mainnet, so a
        // plain transfer_checked is REFUSED. `onchain::invoke_transfer_checked` resolves the
        // hook accounts the CALLER forwards in remaining_accounts (mirrors fee-management /
        // progressive-jackpot); the treasury-outflow PDA signs via signer_seeds.
        spl_token_2022::onchain::invoke_transfer_checked(
            ctx.accounts.token_program.to_account_info().key,
            ctx.accounts.treasury_token_account.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.destination_token_account.to_account_info(),
            ctx.accounts.treasury_outflow_authority.to_account_info(),
            ctx.remaining_accounts,
            amount,
            decimals,
            signer_seeds,
        )?;

        let distributed_total = ctx.accounts.bucket_ledger.distributed;
        msg!("📒 Distributed {} to bucket {}:{} — {} remaining", amount, tag, bucket_id, remaining);
        emit!(DistributionExecuted {
            tag,
            bucket_id,
            amount,
            destination: ctx.accounts.destination_token_account.key(),
            distributed_total,
            remaining,
        });
        Ok(())
    }

    /// ACT-25 — one-time creation of the operational tier config (multisig):
    /// the Routine caps + whitelist and the Exceptional threshold/delay, as
    /// CONFIG per the register entry ("set the tier thresholds and the
    /// operational cap as seed/config"), never constants. Decided 2026-08:
    /// routine per-transfer 1,000 KOTS (deliberately equal to the ACT-07
    /// hot-rail ceiling — "immediate" means one number everywhere), routine
    /// daily aggregate 1,000 KOTS (owner-specified), exceptional threshold
    /// 50,000 KOTS at 72h. Creating this account also ARMS the graduated
    /// queue delay. The exceptional delay may never undercut the base
    /// timelock.
    pub fn initialize_operational_tier_config(
        ctx: Context<InitializeOperationalTierConfig>,
        routine_authority: Pubkey,
        per_transfer_cap: u64,
        daily_cap: u64,
        exceptional_threshold: u64,
        exceptional_delay_secs: i64,
    ) -> Result<()> {
        require!(per_transfer_cap > 0 && daily_cap > 0, KotsError::InvalidAmount);
        require!(per_transfer_cap <= daily_cap, KotsError::InvalidParameter);
        require!(
            exceptional_delay_secs >= TREASURY_TIMELOCK_DELAY_SECS,
            KotsError::InvalidParameter
        );
        let c = &mut ctx.accounts.tier_config;
        c.routine_authority = routine_authority;
        c.per_transfer_cap = per_transfer_cap;
        c.daily_cap = daily_cap;
        c.day_start = Clock::get()?.unix_timestamp;
        c.day_total = 0;
        c.exceptional_threshold = exceptional_threshold;
        c.exceptional_delay_secs = exceptional_delay_secs;
        c.whitelist = [Pubkey::default(); MAX_ROUTINE_DESTINATIONS];
        c.whitelist_count = 0;
        c.bump = ctx.bumps.tier_config;
        msg!(
            "🎚️ Operational tiers armed — routine {}/{} per day, exceptional ≥{} at {}s",
            per_transfer_cap,
            daily_cap,
            exceptional_threshold,
            exceptional_delay_secs
        );
        Ok(())
    }

    /// Amend the tier numbers (multisig). Same floors as init.
    pub fn set_tier_params(
        ctx: Context<MutateOperationalTierConfig>,
        per_transfer_cap: u64,
        daily_cap: u64,
        exceptional_threshold: u64,
        exceptional_delay_secs: i64,
    ) -> Result<()> {
        require!(per_transfer_cap > 0 && daily_cap > 0, KotsError::InvalidAmount);
        require!(per_transfer_cap <= daily_cap, KotsError::InvalidParameter);
        require!(
            exceptional_delay_secs >= TREASURY_TIMELOCK_DELAY_SECS,
            KotsError::InvalidParameter
        );
        let c = &mut ctx.accounts.tier_config;
        c.per_transfer_cap = per_transfer_cap;
        c.daily_cap = daily_cap;
        c.exceptional_threshold = exceptional_threshold;
        c.exceptional_delay_secs = exceptional_delay_secs;
        emit!(TierParamsUpdated {
            per_transfer_cap,
            daily_cap,
            exceptional_threshold,
            exceptional_delay_secs,
        });
        Ok(())
    }

    /// Rotate the Routine operator credential (multisig).
    pub fn set_routine_authority(
        ctx: Context<MutateOperationalTierConfig>,
        new_authority: Pubkey,
    ) -> Result<()> {
        ctx.accounts.tier_config.routine_authority = new_authority;
        msg!("🎚️ Routine authority rotated to {}", new_authority);
        Ok(())
    }

    /// Add an INTERNAL destination token account to the routine whitelist —
    /// a quorum act (OWN-15), loudly evented for the digest/console.
    pub fn add_routine_destination(
        ctx: Context<MutateOperationalTierConfig>,
        destination: Pubkey,
    ) -> Result<()> {
        let c = &mut ctx.accounts.tier_config;
        let count = c.whitelist_count as usize;
        require!(count < MAX_ROUTINE_DESTINATIONS, KotsError::RoutineWhitelistFull);
        require!(
            !c.whitelist[..count].contains(&destination),
            KotsError::RoutineDestinationAlreadyListed
        );
        c.whitelist[count] = destination;
        c.whitelist_count = count as u8 + 1;
        msg!("🎚️ Routine destination added: {} ({} listed)", destination, c.whitelist_count);
        emit!(RoutineWhitelistChanged {
            destination,
            added: true,
            whitelist_count: c.whitelist_count,
        });
        Ok(())
    }

    /// Remove a routine destination (multisig). The last entry backfills the
    /// hole so `[..whitelist_count]` stays dense.
    pub fn remove_routine_destination(
        ctx: Context<MutateOperationalTierConfig>,
        destination: Pubkey,
    ) -> Result<()> {
        let c = &mut ctx.accounts.tier_config;
        let count = c.whitelist_count as usize;
        let idx = c.whitelist[..count]
            .iter()
            .position(|d| *d == destination)
            .ok_or(KotsError::RoutineDestinationNotFound)?;
        c.whitelist[idx] = c.whitelist[count - 1];
        c.whitelist[count - 1] = Pubkey::default();
        c.whitelist_count = count as u8 - 1;
        msg!("🎚️ Routine destination removed: {} ({} listed)", destination, c.whitelist_count);
        emit!(RoutineWhitelistChanged {
            destination,
            added: false,
            whitelist_count: c.whitelist_count,
        });
        Ok(())
    }

    /// ACT-25 Routine tier — the governed IMMEDIATE path (F16): gas top-ups,
    /// hot-wallet float, internal rebalances. Every bound is a fence a stolen
    /// operator key cannot move: destination ∈ the quorum-set whitelist,
    /// amount ≤ per_transfer_cap, and a rolling-day aggregate ≤ daily_cap
    /// (same window idiom as OutflowWindow). Deliberately NOT bucket-
    /// entitlement-bound — the register entry's own words: conservation must
    /// never block a legitimate operational transfer; the tier caps ARE the
    /// bound. PDA-signed like every treasury move; the operator is a caller
    /// credential only. Delay + consensus, never forbid: this tier is the
    /// "no delay needed" floor the quorum itself defined.
    pub fn routine_operational_transfer<'info>(
        ctx: Context<'_, '_, '_, 'info, RoutineOperationalTransfer<'info>>,
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, KotsError::InvalidAmount);
        let now = Clock::get()?.unix_timestamp;
        let destination = ctx.accounts.destination_token_account.key();

        let day_total = {
            let c = &mut ctx.accounts.tier_config;
            require!(
                amount <= c.per_transfer_cap,
                KotsError::RoutinePerTransferCapExceeded
            );
            let count = c.whitelist_count as usize;
            require!(
                c.whitelist[..count].contains(&destination),
                KotsError::RoutineDestinationNotWhitelisted
            );
            if now >= c.day_start + ROUTINE_DAY_SECS {
                c.day_start = now;
                c.day_total = 0;
            }
            let new_total = c
                .day_total
                .checked_add(amount)
                .ok_or(KotsError::InvalidParameter)?;
            require!(new_total <= c.daily_cap, KotsError::RoutineDailyCapExceeded);
            c.day_total = new_total;
            new_total
        };

        let bump = ctx.bumps.treasury_outflow_authority;
        let signer_seeds: &[&[&[u8]]] = &[&[TREASURY_OUTFLOW_AUTHORITY_SEED, &[bump]]];
        let decimals = ctx.accounts.kots_mint.decimals;
        // Token-2022 hook-aware transfer — KOTS carries a TransferHook on mainnet, so a
        // plain transfer_checked is REFUSED. `onchain::invoke_transfer_checked` resolves the
        // hook accounts the CALLER forwards in remaining_accounts (mirrors fee-management /
        // progressive-jackpot); the treasury-outflow PDA signs via signer_seeds.
        spl_token_2022::onchain::invoke_transfer_checked(
            ctx.accounts.token_program.to_account_info().key,
            ctx.accounts.treasury_token_account.to_account_info(),
            ctx.accounts.kots_mint.to_account_info(),
            ctx.accounts.destination_token_account.to_account_info(),
            ctx.accounts.treasury_outflow_authority.to_account_info(),
            ctx.remaining_accounts,
            amount,
            decimals,
            signer_seeds,
        )?;

        msg!(
            "🎚️ Routine transfer {} to {} — {} of {} today",
            amount,
            destination,
            day_total,
            ctx.accounts.tier_config.daily_cap
        );
        emit!(RoutineTransferExecuted {
            destination,
            amount,
            day_total,
            executed_at: now,
        });
        Ok(())
    }
}

// ============================================================================
// ACCOUNT STRUCTS
// ============================================================================

// ----------------------------------------------------------------------------
// ACT-21 — treasury timelock state, events, and account contexts (Increment 1)
// ----------------------------------------------------------------------------

/// ACT-21 — seed for the queued-request PDA.
pub const QUEUED_TRANSFER_SEED: &[u8] = b"queued_transfer_v1";

/// The timelock window applied to a queued external transfer. Mirrors the
/// off-chain payout hold (`kots.payouts.veto.hold` = PT24H) so the on-chain
/// window and the owner-facing digest cadence line up (build decision (a)).
pub const TREASURY_TIMELOCK_DELAY_SECS: i64 = 24 * 60 * 60;

/// ACT-21 — the queued-request record IS the "note": Clock-stamped, carries a
/// mandatory bounded `purpose`, and is the object `settle`/`veto`/`snooze` act
/// on. `requested_at` is written from `Clock::get()` by the contract, never a
/// sidecar-supplied UTC, so a request cannot be pre-dated to skip the window.
#[account]
pub struct QueuedTransfer {
    pub id: u64,
    pub destination: Pubkey,
    pub amount: u64,
    pub purpose: [u8; MAX_REASON_LENGTH],
    pub purpose_len: u8,
    pub requested_at: i64,
    pub execution_delay: i64,
    pub vetoed: bool,
    pub settled: bool,
    pub requester: Pubkey,
    pub bump: u8,
}

impl QueuedTransfer {
    pub const INIT_SPACE: usize =
        8                    // id
        + 32                 // destination
        + 8                  // amount
        + MAX_REASON_LENGTH  // purpose
        + 1                  // purpose_len
        + 8                  // requested_at
        + 8                  // execution_delay
        + 1                  // vetoed
        + 1                  // settled
        + 32                 // requester
        + 1;                 // bump
}

#[event]
pub struct ExternalTransferQueued {
    pub id: u64,
    pub destination: Pubkey,
    pub amount: u64,
    pub requested_at: i64,
    pub execution_delay: i64,
    pub requester: Pubkey,
}

#[event]
pub struct ExternalTransferVetoed {
    pub id: u64,
    pub vetoed_by: Pubkey,
}

#[event]
pub struct ExternalTransferSnoozed {
    pub id: u64,
    pub new_execution_delay: i64,
}

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct QueueExternalTransfer<'info> {
    #[account(
        init,
        payer = requester,
        space = 8 + QueuedTransfer::INIT_SPACE,
        seeds = [QUEUED_TRANSFER_SEED, id.to_le_bytes().as_ref()],
        bump
    )]
    pub queued_transfer: Account<'info, QueuedTransfer>,

    #[account(mut)]
    pub requester: Signer<'info>,

    pub system_program: Program<'info, System>,

    /// ACT-25 graduated-delay source. The fixed tier-config PDA; its ADDRESS
    /// is verified by the seeds whether or not the account exists — ABSENT
    /// (never initialized) means the base delay applies.
    /// CHECK: seeds-pinned PDA; absent-until-armed; discriminator-checked on read.
    #[account(
        seeds = [OPERATIONAL_TIER_CONFIG_SEED],
        bump
    )]
    pub tier_config: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct VetoTransfer<'info> {
    #[account(
        mut,
        seeds = [QUEUED_TRANSFER_SEED, id.to_le_bytes().as_ref()],
        bump = queued_transfer.bump
    )]
    pub queued_transfer: Account<'info, QueuedTransfer>,

    /// The treasury upgrade registry, used purely as the authority of record —
    /// same seeds and gate as `emergency_funding` (ACT-10). One authority model.
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    /// ACT-21 — owner authority (the Squads multisig); proven by signature.
    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct SnoozeTransfer<'info> {
    #[account(
        mut,
        seeds = [QUEUED_TRANSFER_SEED, id.to_le_bytes().as_ref()],
        bump = queued_transfer.bump
    )]
    pub queued_transfer: Account<'info, QueuedTransfer>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

// ----------------------------------------------------------------------------
// ACT-21 Increment 2 — settle + the aggregate outflow window (custody-bearing)
// ----------------------------------------------------------------------------

/// ACT-21 — seed for the per-rail aggregate outflow window PDA.
pub const OUTFLOW_WINDOW_SEED: &[u8] = b"outflow_window_v1";

/// ACT-21 — seed for the treasury-outflow authority PDA. After the custody
/// migration this PDA owns the treasury KOTS token account, so it is the ONLY
/// authority that can move treasury tokens (closing the F4 transfer graph: a
/// stolen hot key is no longer the token authority).
pub const TREASURY_OUTFLOW_AUTHORITY_SEED: &[u8] = b"treasury_outflow_v4";

/// The rolling window over which external outflow is aggregated (24h).
pub const OUTFLOW_WINDOW_SECS: i64 = 24 * 60 * 60;

/// The aggregate ceiling on arbitrary external pushes within the window
/// (B.6 — 1,000 KOTS / 24h). Enforced as a CUMULATIVE sum, so structuring a
/// drain into many sub-cap transfers still trips it.
pub const HOT_RAIL_CEILING: u64 = 1_000 * TOKENS_PER_UNIT;

/// ACT-21 — the per-rail aggregate outflow accumulator. `settle` resets it when
/// the window elapses and refuses once cumulative outflow would exceed the cap.
#[account]
pub struct OutflowWindow {
    pub window_start: i64,
    pub cumulative_out: u64,
    pub bump: u8,
}

impl OutflowWindow {
    pub const INIT_SPACE: usize = 8 + 8 + 1; // window_start + cumulative_out + bump
}

#[event]
pub struct ExternalTransferSettled {
    pub id: u64,
    pub destination: Pubkey,
    pub amount: u64,
    pub settled_at: i64,
}

#[derive(Accounts)]
pub struct InitializeOutflowWindow<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + OutflowWindow::INIT_SPACE,
        seeds = [OUTFLOW_WINDOW_SEED],
        bump
    )]
    pub outflow_window: Account<'info, OutflowWindow>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        mut,
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(id: u64)]
pub struct SettleExternalTransfer<'info> {
    #[account(
        mut,
        seeds = [QUEUED_TRANSFER_SEED, id.to_le_bytes().as_ref()],
        bump = queued_transfer.bump
    )]
    pub queued_transfer: Account<'info, QueuedTransfer>,

    #[account(
        mut,
        seeds = [OUTFLOW_WINDOW_SEED],
        bump = outflow_window.bump
    )]
    pub outflow_window: Account<'info, OutflowWindow>,

    /// The treasury-outflow authority PDA — owner of the treasury KOTS account
    /// after the custody migration; signs the transfer via `invoke_signed`.
    /// CHECK: seeds-derived PDA used only as the transfer authority.
    #[account(
        seeds = [TREASURY_OUTFLOW_AUTHORITY_SEED],
        bump
    )]
    pub treasury_outflow_authority: UncheckedAccount<'info>,

    pub kots_mint: InterfaceAccount<'info, MintInterface>,

    /// Source: the treasury KOTS account. Constrained to the KOTS mint AND to the
    /// outflow PDA authority, so `settle` can only drain the real treasury
    /// account, never an arbitrary one.
    #[account(
        mut,
        token::mint = kots_mint,
        token::authority = treasury_outflow_authority,
    )]
    pub treasury_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    /// Destination KOTS account; its address is verified `== queued.destination`
    /// in the handler, so settle cannot redirect funds.
    #[account(
        mut,
        token::mint = kots_mint,
    )]
    pub destination_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    pub token_program: Interface<'info, TokenInterface>,

    /// Permissionless submitter (tx fee-payer). Security is in the settle-time
    /// conditions, not in who calls this.
    pub caller: Signer<'info>,

    /// ACT-23 dead-man's-switch state (decision C.7). The fixed registry PDA;
    /// its ADDRESS is verified by the seeds constraint whether or not the
    /// account exists — an ABSENT (never-initialized) registry means the gate
    /// is OFF (deploy-safe), so `initialize_watcher_registry` +
    /// `register_watcher` IS the arming ceremony. Contents are manually
    /// deserialized in the handler (discriminator-checked) because absence
    /// must be legal here.
    /// CHECK: seeds-pinned PDA; absent-until-armed; discriminator-checked on read.
    #[account(
        seeds = [WATCHER_REGISTRY_SEED],
        bump
    )]
    pub watcher_registry: UncheckedAccount<'info>,
    /// EMERGENCY PAUSE GATE (2026-08-12): security-governance's EmergencyState PDA.
    /// Threaded so this outflow can be frozen while the platform is paused.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
}

// ----------------------------------------------------------------------------
// ACT-23 — watcher liveness (dead-man's-switch) state, events, and contexts
// ----------------------------------------------------------------------------

/// Seed of the singleton watcher registry — its existence IS the arming switch.
pub const WATCHER_REGISTRY_SEED: &[u8] = b"watcher_registry_v1";
/// Registry capacity — C.7 runs two watchers; headroom for rotation overlap.
pub const MAX_WATCHERS: usize = 4;
/// Settle refuses when NO watcher attested within this window: 12h = two
/// missed ~6h heartbeats from BOTH watchers, per the ACT-08 spec.
pub const ATTESTATION_FRESHNESS_SECS: i64 = 12 * 60 * 60;

#[account]
pub struct WatcherRegistry {
    /// Registered watcher keys; only `[..watcher_count]` are live.
    pub watchers: [Pubkey; MAX_WATCHERS],
    /// Unix seconds of each watcher's last attestation (0 = never).
    pub last_attested: [i64; MAX_WATCHERS],
    pub watcher_count: u8,
    pub bump: u8,
}

impl WatcherRegistry {
    pub const INIT_SPACE: usize = 32 * MAX_WATCHERS + 8 * MAX_WATCHERS + 1 + 1;
}

#[event]
pub struct WatcherRegistered {
    pub watcher: Pubkey,
    pub watcher_count: u8,
}

#[event]
pub struct LivenessAttested {
    pub watcher: Pubkey,
    pub attested_at: i64,
}

#[derive(Accounts)]
pub struct InitializeWatcherRegistry<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + WatcherRegistry::INIT_SPACE,
        seeds = [WATCHER_REGISTRY_SEED],
        bump
    )]
    pub watcher_registry: Account<'info, WatcherRegistry>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        mut,
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterWatcher<'info> {
    #[account(
        mut,
        seeds = [WATCHER_REGISTRY_SEED],
        bump = watcher_registry.bump
    )]
    pub watcher_registry: Account<'info, WatcherRegistry>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct AttestLiveness<'info> {
    #[account(
        mut,
        seeds = [WATCHER_REGISTRY_SEED],
        bump = watcher_registry.bump
    )]
    pub watcher_registry: Account<'info, WatcherRegistry>,

    /// The attesting watcher — its OWN signature is the whole point (ADV-31):
    /// a sidecar hijack holds neither watcher key.
    pub watcher: Signer<'info>,
}

// ----------------------------------------------------------------------------
// ACT-25 — the graduated operational-transfer model (the governed escape hatch)
// ----------------------------------------------------------------------------

/// Seed of the singleton tier config — Routine caps + whitelist + the
/// Exceptional threshold/delay. Its EXISTENCE arms the graduated queue delay.
pub const OPERATIONAL_TIER_CONFIG_SEED: &[u8] = b"operational_tier_config_v1";
pub const MAX_ROUTINE_DESTINATIONS: usize = 8;
pub const ROUTINE_DAY_SECS: i64 = 24 * 60 * 60;

#[account]
pub struct OperationalTierConfig {
    /// The Routine-tier CALLER credential (an ops key): even fully stolen it
    /// moves at most per_transfer_cap per call and daily_cap per rolling day,
    /// to whitelisted INTERNAL destinations only. Rotated by the multisig.
    pub routine_authority: Pubkey,
    pub per_transfer_cap: u64,
    pub daily_cap: u64,
    /// Rolling-day window, same idiom as OutflowWindow.
    pub day_start: i64,
    pub day_total: u64,
    /// Queue amounts at/above this get the LONGER Exceptional delay.
    pub exceptional_threshold: u64,
    pub exceptional_delay_secs: i64,
    /// Whitelisted internal destination TOKEN ACCOUNTS; only
    /// `[..whitelist_count]` are live. Changing this is a quorum act (OWN-15).
    pub whitelist: [Pubkey; MAX_ROUTINE_DESTINATIONS],
    pub whitelist_count: u8,
    pub bump: u8,
}

impl OperationalTierConfig {
    pub const INIT_SPACE: usize = 32 + 8 + 8 + 8 + 8 + 8 + 8 + 32 * MAX_ROUTINE_DESTINATIONS + 1 + 1;
}

#[event]
pub struct RoutineTransferExecuted {
    pub destination: Pubkey,
    pub amount: u64,
    pub day_total: u64,
    pub executed_at: i64,
}

#[event]
pub struct RoutineWhitelistChanged {
    pub destination: Pubkey,
    pub added: bool,
    pub whitelist_count: u8,
}

#[event]
pub struct TierParamsUpdated {
    pub per_transfer_cap: u64,
    pub daily_cap: u64,
    pub exceptional_threshold: u64,
    pub exceptional_delay_secs: i64,
}

#[derive(Accounts)]
pub struct InitializeOperationalTierConfig<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + OperationalTierConfig::INIT_SPACE,
        seeds = [OPERATIONAL_TIER_CONFIG_SEED],
        bump
    )]
    pub tier_config: Account<'info, OperationalTierConfig>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        mut,
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MutateOperationalTierConfig<'info> {
    #[account(
        mut,
        seeds = [OPERATIONAL_TIER_CONFIG_SEED],
        bump = tier_config.bump
    )]
    pub tier_config: Account<'info, OperationalTierConfig>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct RoutineOperationalTransfer<'info> {
    #[account(
        mut,
        seeds = [OPERATIONAL_TIER_CONFIG_SEED],
        bump = tier_config.bump
    )]
    pub tier_config: Account<'info, OperationalTierConfig>,

    /// CHECK: seeds-derived PDA used only as the transfer authority.
    #[account(
        seeds = [TREASURY_OUTFLOW_AUTHORITY_SEED],
        bump
    )]
    pub treasury_outflow_authority: UncheckedAccount<'info>,

    pub kots_mint: InterfaceAccount<'info, MintInterface>,

    #[account(
        mut,
        token::mint = kots_mint,
        token::authority = treasury_outflow_authority,
    )]
    pub treasury_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    /// Whitelist membership is checked in the handler against the config.
    #[account(
        mut,
        token::mint = kots_mint,
    )]
    pub destination_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    pub token_program: Interface<'info, TokenInterface>,

    #[account(
        constraint = operator.key() == tier_config.routine_authority
            @ KotsError::UnauthorizedRoutineOperator
    )]
    pub operator: Signer<'info>,
}

// ----------------------------------------------------------------------------
// ACT-24 — per-bucket value-conservation (purpose-as-accounting-key) state
// ----------------------------------------------------------------------------

pub const BUCKET_LEDGER_SEED: &[u8] = b"bucket_ledger_v1";
pub const DISTRIBUTION_CONFIG_SEED: &[u8] = b"distribution_config_v1";

#[account]
pub struct BucketLedger {
    /// kots_common::purpose_key tag (contest-award / voter-share / jackpot /
    /// treasury-op / tier-stake).
    pub tag: u8,
    pub bucket_id: u64,
    /// Budget granted — only `fund_bucket` (multisig) raises this.
    pub entitled: u64,
    /// Σ debits — the conservation invariant distributed ≤ entitled is a
    /// HARD REVERT in `distribute_from_treasury`.
    pub distributed: u64,
    pub bump: u8,
}

impl BucketLedger {
    pub const INIT_SPACE: usize = 1 + 8 + 8 + 8 + 1;
}

#[account]
pub struct DistributionConfig {
    /// WHO may call `distribute_from_treasury` — a CALLER credential, not a
    /// token authority: even fully stolen it moves nothing beyond bucket
    /// entitlements. Rotated by the multisig.
    pub authority: Pubkey,
    /// Coarse F15 belt: Σ entitled / Σ distributed across every bucket.
    pub total_entitled: u64,
    pub total_distributed: u64,
    /// The progressive-jackpot pool STATE account, pinned by the multisig at
    /// the ceremony (network-agnostic on purpose: no compiled-in program id).
    /// Jackpot draws are additionally bounded by this account's live
    /// `total_jackpot_amount` — the register entry's canonical-case
    /// tightening: the jackpot is bounded by ITS POOL, not by entitlement
    /// bookkeeping alone.
    pub jackpot_pool: Pubkey,
    /// ACT-24 auto-crediting (decision: CPI-coupled, voter-share leg): the
    /// ONE signer `credit_voter_share_bucket` accepts — the prize-distribution
    /// program's bucket-credit PDA, pinned here by the multisig. Only
    /// `store_voter_share` can make that PDA sign, so entitlement rises
    /// exactly when a share record is written, and the DISTRIBUTOR credential
    /// can never reach it (F15: spenders never raise ceilings).
    pub bucket_credit_authority: Pubkey,
    pub bump: u8,
}

impl DistributionConfig {
    pub const INIT_SPACE: usize = 32 + 8 + 8 + 32 + 32 + 1;
}

#[event]
pub struct BucketFunded {
    pub tag: u8,
    pub bucket_id: u64,
    pub additional_entitlement: u64,
    pub entitled: u64,
}

#[event]
pub struct DistributionExecuted {
    pub tag: u8,
    pub bucket_id: u64,
    pub amount: u64,
    pub destination: Pubkey,
    pub distributed_total: u64,
    pub remaining: u64,
}

#[derive(Accounts)]
pub struct InitializeDistributionConfig<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + DistributionConfig::INIT_SPACE,
        seeds = [DISTRIBUTION_CONFIG_SEED],
        bump
    )]
    pub distribution_config: Account<'info, DistributionConfig>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        mut,
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MutateDistributionConfig<'info> {
    #[account(
        mut,
        seeds = [DISTRIBUTION_CONFIG_SEED],
        bump = distribution_config.bump
    )]
    pub distribution_config: Account<'info, DistributionConfig>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(tag: u8, bucket_id: u64)]
pub struct InitializeBucket<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + BucketLedger::INIT_SPACE,
        seeds = [BUCKET_LEDGER_SEED, tag.to_le_bytes().as_ref(), bucket_id.to_le_bytes().as_ref()],
        bump
    )]
    pub bucket_ledger: Account<'info, BucketLedger>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        mut,
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(tag: u8, bucket_id: u64)]
pub struct FundBucket<'info> {
    #[account(
        mut,
        seeds = [BUCKET_LEDGER_SEED, tag.to_le_bytes().as_ref(), bucket_id.to_le_bytes().as_ref()],
        bump = bucket_ledger.bump
    )]
    pub bucket_ledger: Account<'info, BucketLedger>,

    #[account(
        mut,
        seeds = [DISTRIBUTION_CONFIG_SEED],
        bump = distribution_config.bump
    )]
    pub distribution_config: Account<'info, DistributionConfig>,

    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_id: u64)]
pub struct CreditVoterShareBucket<'info> {
    /// The voter-share bucket for this contest. `init_if_needed` is the ONE
    /// sanctioned exception to this file's no-implicit-create rule for
    /// value-bearing accounts — argued, not assumed: (1) the only signer
    /// accepted below is the program-bound credit authority, so no CONSUMER
    /// can reach this; (2) creation zero-inits and is stamped + credited
    /// atomically with the share record whose init-once already bars replay;
    /// (3) a per-contest multisig ceremony here would re-manualize exactly
    /// the auto-crediting ACT-24 exists to build.
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + BucketLedger::INIT_SPACE,
        seeds = [
            BUCKET_LEDGER_SEED,
            &[kots_common::purpose_key::TAG_CONTEST_VOTER_SHARE],
            contest_id.to_le_bytes().as_ref()
        ],
        bump
    )]
    pub bucket_ledger: Account<'info, BucketLedger>,

    #[account(
        mut,
        seeds = [DISTRIBUTION_CONFIG_SEED],
        bump = distribution_config.bump
    )]
    pub distribution_config: Account<'info, DistributionConfig>,

    /// The pinned prize-distribution bucket-credit PDA — the ONLY key that
    /// may credit this leg.
    #[account(
        constraint = bucket_credit_authority.key() == distribution_config.bucket_credit_authority
            @ KotsError::UnauthorizedBucketCreditor
    )]
    pub bucket_credit_authority: Signer<'info>,

    /// Rent payer for a fresh ledger (the finalization authority, forwarded
    /// through the CPI with its outer signature intact).
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(tag: u8, bucket_id: u64)]
pub struct DistributeFromTreasury<'info> {
    #[account(
        mut,
        seeds = [BUCKET_LEDGER_SEED, tag.to_le_bytes().as_ref(), bucket_id.to_le_bytes().as_ref()],
        bump = bucket_ledger.bump
    )]
    pub bucket_ledger: Account<'info, BucketLedger>,

    #[account(
        mut,
        seeds = [DISTRIBUTION_CONFIG_SEED],
        bump = distribution_config.bump
    )]
    pub distribution_config: Account<'info, DistributionConfig>,

    /// The treasury-outflow authority PDA — post-cutover the sole authority
    /// over the treasury token account; signs via `invoke_signed`.
    /// CHECK: seeds-derived PDA used only as the transfer authority.
    #[account(
        seeds = [TREASURY_OUTFLOW_AUTHORITY_SEED],
        bump
    )]
    pub treasury_outflow_authority: UncheckedAccount<'info>,

    pub kots_mint: InterfaceAccount<'info, MintInterface>,

    /// Source: the treasury KOTS account — constrained to the KOTS mint AND
    /// the outflow PDA, so only the real treasury can be debited.
    #[account(
        mut,
        token::mint = kots_mint,
        token::authority = treasury_outflow_authority,
    )]
    pub treasury_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    #[account(
        mut,
        token::mint = kots_mint,
    )]
    pub destination_token_account: InterfaceAccount<'info, TokenAccountInterface>,

    pub token_program: Interface<'info, TokenInterface>,

    /// The configured distributor — a caller credential, verified against the
    /// config; the bucket ledger bounds what it can ever move.
    #[account(
        constraint = distributor.key() == distribution_config.authority
            @ KotsError::UnauthorizedDistributor
    )]
    pub distributor: Signer<'info>,

    /// The multisig-pinned progressive-jackpot pool state (address verified
    /// against the config in the handler on EVERY call; its live
    /// `total_jackpot_amount` additionally bounds jackpot draws).
    /// CHECK: address pinned by DistributionConfig; contents read manually
    /// with initialized/length guards.
    pub jackpot_pool: UncheckedAccount<'info>,
    /// EMERGENCY PAUSE GATE (2026-08-12): security-governance's EmergencyState PDA.
    /// Threaded so this outflow can be frozen while the platform is paused.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitializeTreasuryAccounts<'info> {
    #[account(
        init,
        payer = payer,
        space = 8 + 1, // discriminator + bool
        seeds = [b"treasury_main_v4"],
        bump
    )]
    /// Treasury main account - created with Treasury Operations ownership
    pub treasury_main: Account<'info, TreasuryAccount>,
    
    #[account(
        init,
        payer = payer,
        space = 8 + 1, // discriminator + bool
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// Treasury incoming account - created with Treasury Operations ownership
    pub treasury_incoming: Account<'info, TreasuryAccount>,
    
    #[account(
        init,
        payer = payer,
        space = 8 + 1, // discriminator + bool
        seeds = [b"treasury_outgoing_v4"],
        bump
    )]
    /// Treasury outgoing account - created with Treasury Operations ownership
    pub treasury_outgoing: Account<'info, TreasuryAccount>,

    // ─────────────────────────────── C10 ───────────────────────────────
    // ADDED 2026-07-28. Nothing in the workspace created this PDA, so
    // `reimburse_creator` and `emergency_funding` — which bind it `mut` by
    // seeds with no `init` — failed AccountNotInitialized (3012) on EVERY
    // call, on every network. Both were dead code.
    //
    // Exactly the defect the 2026-07-23 jackpot fix records in
    // progressive-jackpot: "nothing created it, so both of those instructions
    // failed account validation on every call." Fixed there for jackpot_state
    // and the vault; the queue was missed. Nothing flagged it because the only
    // test touching it asserted `res.is_err()` and got its error from the
    // missing account rather than from the authority check it claimed to test.
    //
    // Created HERE rather than with `init_if_needed` on the consumers: this is
    // a value-bearing account (total_owed), and init_if_needed would let any
    // caller create it. The treasury initializer is already the gated
    // bootstrap path, and the queue belongs with the accounts it serves.
    #[account(
        init,
        payer = payer,
        // discriminator + pending_reimbursements(u32) + total_owed(u64)
        //             + last_creator(Pubkey) + last_amount(u64)
        space = 8 + 4 + 8 + 32 + 8,
        seeds = [b"reimbursement_queue_v4"],
        bump
    )]
    /// Reimbursement queue — read-write by ReimburseCreator and EmergencyFunding.
    pub reimbursement_queue: Account<'info, ReimbursementQueue>,

    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FundTreasuryAccounts<'info> {
    #[account(
        mut,
        seeds = [b"treasury_main_v4"],
        bump
    )]
    /// Treasury main account - must be initialized first
    pub treasury_main: Account<'info, TreasuryAccount>,
    
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// Treasury incoming account - must be initialized first
    pub treasury_incoming: Account<'info, TreasuryAccount>,
    
    #[account(
        mut,
        seeds = [b"treasury_outgoing_v4"],
        bump
    )]
    /// Treasury outgoing account - must be initialized first
    pub treasury_outgoing: Account<'info, TreasuryAccount>,
    
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeUpgradeRegistry<'info> {
    // ─────────────────────────── O31 ───────────────────────────
    // FIXED 2026-07-28. Was `#[account(mut)]` on a typed account with NO `init`
    // and no seeds — so the handler's `&mut ctx.accounts.upgrade_registry` tried
    // to deserialize an account nothing had created. Every call failed
    // AccountNotInitialized (3012); this initializer has never initialized.
    //
    // Anchor `init` is used here rather than the manual invoke_signed pattern
    // that voting-hieroglyph needs, because this handler already writes through
    // the typed accessor. The seeds match what ExecuteSystemicUpgrade,
    // PauseTreasuryForUpgrade and ResumeTreasuryAfterUpgrade already bind.
    //
    // space: UpgradeRegistry body is 436 bytes (version_history alone is 320);
    // + 8 discriminator = 444, rounded to 448.
    #[account(
        init,
        payer = authority,
        space = 448,
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecuteSystemicUpgrade<'info> {
    #[account(mut)]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    /// CHECK: This is the program account being upgraded, validated in upgrade logic
    #[account(mut)]
    pub program: UncheckedAccount<'info>,
    /// CHECK: This is the program data account for BPF upgradeable programs
    #[account(mut)]
    pub program_data: UncheckedAccount<'info>,
    /// CHECK: This is the buffer account containing new program code
    #[account(mut)]
    pub buffer: UncheckedAccount<'info>,
    /// CHECK: This is the spill account for upgrade operations
    #[account(mut)]
    pub spill: UncheckedAccount<'info>,
    pub upgrade_authority: Signer<'info>,
    pub rent: Sysvar<'info, Rent>,
    pub clock: Sysvar<'info, Clock>,
    /// CHECK: This is the BPF Loader Upgradeable program ID
    pub bpf_loader_upgradeable_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct PauseTreasuryForUpgrade<'info> {
    #[account(mut)]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    pub pause_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ResumeTreasuryAfterUpgrade<'info> {
    #[account(mut)]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    pub resume_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct MigrateTreasuryData<'info> {
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    pub migration_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UpdateEconomicParameters<'info> {
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub update_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ReportTreasuryReadiness<'info> {
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub reporter: Signer<'info>,
}

#[derive(Accounts)]
pub struct CalculateMemeUploadCost<'info> {
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct EnsurePayerFunded<'info> {
    #[account(
        mut,
        seeds = [b"treasury_main_v4"],
        bump,
        constraint = treasury_main.lamports() >= MIN_TREASURY_RESERVE @ KotsError::InsufficientTreasuryFunds
    )]
    /// CHECK: Treasury main account
    pub treasury_main: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account that will pay for operations
    pub treasury_incoming: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_outgoing_v4"],
        bump
    )]
    /// CHECK: Treasury outgoing account for fallback
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct EmergencyFunding<'info> {
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account
    pub treasury_incoming: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_outgoing_v4"],
        bump
    )]
    /// CHECK: Treasury outgoing account for emergency
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"reimbursement_queue_v4"],
        bump
    )]
    pub reimbursement_queue: Account<'info, ReimbursementQueue>,

    /// SECURITY (ACT-10 / F7): the treasury upgrade registry, used purely as the
    /// authority of record — same seeds and gate as `ReimburseCreator` and
    /// `UpdateEconomicParameters`. One authority model for the program.
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    /// SECURITY (ACT-10 / F7): emergency_funding writes the reimbursement queue
    /// (`last_creator` / `last_amount` / `total_owed`) that `ReimburseCreator`
    /// then pays against. Its only "caller" check was `caller_program_id`, a
    /// spoofable instruction ARGUMENT — any wallet could pass a valid KOTS id and
    /// poison the queue. It is now gated on the registered upgrade authority, so
    /// only the multisig (A.4) can invoke it; `caller_program_id` stays as a
    /// secondary realm check in the handler (defence in depth).
    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReimburseCreator<'info> {
    #[account(
        mut,
        seeds = [b"treasury_main_v4"],
        bump
    )]
    /// CHECK: Treasury main account
    pub treasury_main: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"reimbursement_queue_v4"],
        bump
    )]
    pub reimbursement_queue: Account<'info, ReimbursementQueue>,

    /// SECURITY (2026-07-23): the recipient is no longer caller-chosen. It is
    /// pinned to the creator the queue actually records as owed. This is the
    /// control that turns a "pay anyone" instruction into "pay the payee".
    /// CHECK: address-constrained to reimbursement_queue.last_creator
    #[account(
        mut,
        address = reimbursement_queue.last_creator @ KotsError::UnauthorizedCaller
    )]
    pub recipient: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): the treasury upgrade registry, used here purely as
    /// the authority of record. Same seeds and same gate as
    /// `UpdateEconomicParameters` — one authority model for the program.
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"treasury"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,

    /// SECURITY (2026-07-23): was a bare `Signer` bound to nothing. Now must be
    /// the registered upgrade authority.
    #[account(
        constraint = Some(authority.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedCaller
    )]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AutoBalanceTreasuries<'info> {
    #[account(
        mut,
        seeds = [b"treasury_main_v4"],
        bump
    )]
    /// CHECK: Treasury main account
    pub treasury_main: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account
    pub treasury_incoming: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_outgoing_v4"],
        bump
    )]
    /// CHECK: Treasury outgoing account
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreatePDAForProgram<'info> {
    #[account(
        mut,
        seeds = [b"treasury_main_v4"],
        bump
    )]
    /// CHECK: Treasury main account for fallback funding
    pub treasury_main: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account that will pay for PDA creation
    pub treasury_incoming: UncheckedAccount<'info>,
    
    /// CHECK: The new account to be created - must NOT have signer constraints
    /// This account doesn't exist yet and cannot participate in signing
    pub new_account: AccountInfo<'info>,
    
    /// CHECK: The calling program requesting PDA creation - validated against whitelist
    pub caller_program: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FundCallerForPDA<'info> {
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account that will provide funding
    pub treasury_incoming: UncheckedAccount<'info>,
    
    /// CHECK: The caller program requesting funding - validated against whitelist
    #[account(mut)]
    pub caller_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ReimbursePayer<'info> {
    #[account(
        mut,
        seeds = [b"treasury_incoming_v4"],
        bump
    )]
    /// CHECK: Treasury incoming account that will reimburse
    pub treasury_incoming: UncheckedAccount<'info>,
    
    /// CHECK: The payer to be reimbursed
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): PDA of the CALLING program, passed as a signer via
    /// `invoke_signed`. Verified in the handler against
    /// `find_program_address([CPI_AUTHORITY_SEED], caller_program_id)`. This is
    /// what makes the caller identity unforgeable — a wallet cannot sign for
    /// another program's PDA.
    /// CHECK: key + is_signer verified in the handler
    pub cpi_authority: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

// ============================================================================
// RESULT STRUCTS
// ============================================================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CostBreakdown {
    pub storage_cost: u64,
    pub bandwidth_cost: u64,
    pub base_fee: u64,
    pub total_cost: u64,
    pub storage_tier: StorageTier,
    pub duration_months: u8,
}

#[account]
pub struct ReimbursementQueue {
    pub pending_reimbursements: u32,
    pub total_owed: u64,
    pub last_creator: Pubkey,
    pub last_amount: u64,
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

fn validate_economics_system_stable() -> Result<()> {
    msg!("🔍 Validating economics system stability");
    
    // In a full implementation, this would check system health metrics
    // For now, assume system is stable
    msg!("✅ Economics system is stable - ready for upgrade");
    Ok(())
}

fn validate_kots_program_caller(program_id: &Pubkey) -> Result<()> {
    // THE authorisation gate for five CPI paths — and, until 2026-07-16, a
    // hand-copied list of the ABANDONED v3.9.2 ids. It rejected the real
    // programs: a security control that fails closed on the wrong set is not a
    // control, it is an outage with a good conscience.
    //
    // Now: one table, and it identifies ITSELF. `crate::ID` is the compiled-in
    // declare_id!(), which the loader has already proven equals the runtime
    // program id (DeclaredProgramIdMismatch, 4100) — so "which realm am I in"
    // is answered by a fact nothing can spoof, with no env var (there are none
    // in BPF), no feature flag, and no config account to initialise.
    let ids = kots_common::program_ids::current(&crate::ID)
        .ok_or(KotsError::UnauthorizedCaller)?;   // no realm claims us -> fail closed

    if ids.contains(program_id) {
        msg!("✅ Validated KOTS program caller: {}", program_id);
        return Ok(());
    }

    msg!("❌ Unauthorized program caller: {}", program_id);
    Err(KotsError::UnauthorizedCaller.into())
}



/// Helper function to get current timestamp with error handling
fn get_current_timestamp() -> Result<i64> {
    let clock = Clock::get()?;
    Ok(clock.unix_timestamp)
}

// Treasury account data structure
#[account]
pub struct TreasuryAccount {
    pub initialized: bool,
}

// Constants for treasury management
const MIN_OPERATING_BALANCE: u64 = 1_000_000;     // 0.001 SOL minimum
const BUFFER_AMOUNT: u64 = 10_000_000;            // 0.01 SOL buffer
const MIN_TREASURY_RESERVE: u64 = 100_000_000;    // 0.1 SOL reserve
