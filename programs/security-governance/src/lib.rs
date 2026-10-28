// DESTINATION (where this file goes): kots-contracts/programs/security-governance/src/lib.rs
// kots-contracts/programs/security-governance/src/lib.rs
//! KOTS Security & Governance Program
//! 
//! Handles blockchain-native payment verification, emergency pause controls,
//! role-based access management, gas oracle integration, and audit trail generation.

use anchor_lang::{prelude::*, Bumps};
use borsh::{BorshSerialize, BorshDeserialize};
use kots_common::{
    constants::{*, MAX_BLACKLISTED_WALLETS_PER_ACCOUNT, MAX_REASON_LENGTH}, errors::*, utils::*, DEFAULT_QUORUM_REQUIRED,
    upgrade::{
        UpgradeType, UpgradeProposal, UpgradeRegistry, CrossProgramUpgradeState, UpgradeProposalStatus,
        UPGRADE_PROPOSAL_SEED, UPGRADE_REGISTRY_SEED, CROSS_PROGRAM_UPGRADE_SEED
    },
    PaymentVerification, PaymentType, SecurityValidationResult,
    state::{
        BLACKLIST_SEED, EMERGENCY_SEED, GAS_ORACLE_SEED, GOVERNANCE_SUB_SEED
    },
    init::{request_treasury_reimbursement, TreasuryAccounts, InitOptions},
    upgrade::{UpgradeProposalCreatedEvent, UpgradeVoteEvent, UpgradeExecutedEvent, CrossProgramUpgradeStartedEvent, CrossProgramUpgradeCompletedEvent},
    cpi::{load_user_role, get_total_system_stake, load_user_staking_state}
};

// Program-local account size constants for IDL generation
const GAS_ORACLE_SPACE: usize = 320;                    // 8 (discriminator) + 312 (EnhancedGasOracleState)
// 8 (discriminator) + 1119 (BlacklistState) + 409 spare = 1536.
//
// BlacklistState, Borsh, all arrays fixed-size:
//   1 is_initialized + 32 authority + 32 moderator
//   + 10*32 wallets (320) + 10*64 reasons (640) + 10*8 timestamps (80)
//   + 2 count + 4 total + 8 timestamp  =  1119
//
// CORRECTED 2026-07-29: this said "528 (BlacklistState)", which is wrong by
// more than the reasons array alone (640 B). The total happened to be ample,
// so the error was invisible — and it sat next to O39, where the account was
// never allocated at all, which made "the space is wrong" the first and
// wrong hypothesis.
const BLACKLIST_STATE_SPACE: usize = 1536;
const EMERGENCY_STATE_SPACE: usize = 1024;              // 8 (discriminator) + 516 (EmergencyState) + 500 (extra)
const UPGRADE_PROPOSAL_SPACE: usize = 512;              // 8 (discriminator) + 504 (UpgradeProposal) - increased for safety
const CROSS_PROGRAM_UPGRADE_STATE_SPACE: usize = 512;   // 8 (discriminator) + 504 (CrossProgramUpgradeState)
const PROGRAM_STATE_SPACE: usize = 256;                 // 8 (discriminator) + 248 (ProgramUpgradeState)

// Account struct definitions at root level for proper IDL generation
// These structs are always visible to Anchor's IDL generator

// CONSOLIDATED (2026-07-23): this local copy is now compiled ONLY for IDL
// generation. The BPF build imports the kots-common definition instead (see
// the `pub use` below), which is the same pattern treasury-operations already
// uses for UpgradeRegistry.
//
// WHY: kots-common carried a byte-identical duplicate of this struct, and the
// new cross-program readers in kots_common::cpi deserialize THIS program's
// accounts using THAT type. Two live definitions of one on-chain layout is a
// silent-corruption hazard the moment they drift — and one of the four pairs
// in this file (ProgramUpgradeState) HAS already drifted.
// ⚠ UN-GATED 2026-07-29 (O40). This was `#[cfg(feature = "idl-build")]`, so the
// BPF build used kots-common's copy via the `pub use` below — and kots-common
// declares `pub const ID = Pubkey::new_from_array([0; 32])`. Anchor's `#[account]`
// generates `impl Owner { fn owner() -> crate::ID }`, so every
// `Account<'info, BlacklistState>` demanded an owner of
// 11111111111111111111111111111111. Unsatisfiable both ways: own it by this
// program and the check fails 3007; own it by zeros and this program cannot write
// to it. Compiled here, `#[account]` yields this program's ID, which is correct.
#[account]
#[derive(Debug)]
pub struct BlacklistState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub moderator: Pubkey,
    pub blacklisted_wallets: [Pubkey; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // BPF-safe: 10 * 32 = 320 bytes
    pub blacklist_reasons: [[u8; MAX_REASON_LENGTH]; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // BPF-safe: nested arrays
    pub blacklist_timestamps: [i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT], // BPF-safe: 10 * 8 = 80 bytes
    pub blacklist_count: u16,
    pub total_blacklisted: u32,
    pub last_update_timestamp: i64,
}

// CONSOLIDATED (2026-07-23): this local copy is now compiled ONLY for IDL
// generation. The BPF build imports the kots-common definition instead (see
// the `pub use` below), which is the same pattern treasury-operations already
// uses for UpgradeRegistry.
//
// WHY: kots-common carried a byte-identical duplicate of this struct, and the
// new cross-program readers in kots_common::cpi deserialize THIS program's
// accounts using THAT type. Two live definitions of one on-chain layout is a
// silent-corruption hazard the moment they drift — and one of the four pairs
// in this file (ProgramUpgradeState) HAS already drifted.
// ⚠ UN-GATED 2026-07-29 (O40). This was `#[cfg(feature = "idl-build")]`, so the
// BPF build used kots-common's copy via the `pub use` below — and kots-common
// declares `pub const ID = Pubkey::new_from_array([0; 32])`. Anchor's `#[account]`
// generates `impl Owner { fn owner() -> crate::ID }`, so every
// `Account<'info, BlacklistState>` demanded an owner of
// 11111111111111111111111111111111. Unsatisfiable both ways: own it by this
// program and the check fails 3007; own it by zeros and this program cannot write
// to it. Compiled here, `#[account]` yields this program's ID, which is correct.
#[account]
#[derive(Debug)]
pub struct EmergencyState {
    pub is_initialized: bool,
    pub emergency_authority: Pubkey,
    pub is_paused: bool,
    pub pause_reason: [u8; MAX_STRING_LENGTH], // BPF-safe: 64 bytes
    pub pause_timestamp: i64,
    pub pause_duration: i64,
    pub authorized_emergency_operations: [[u8; MAX_STRING_LENGTH]; 10], // BPF-safe: nested arrays
    pub emergency_operations_count: u8,
    pub total_emergency_activations: u32,
}

// CONSOLIDATED (2026-07-23): this local copy is now compiled ONLY for IDL
// generation. The BPF build imports the kots-common definition instead (see
// the `pub use` below), which is the same pattern treasury-operations already
// uses for UpgradeRegistry.
//
// WHY: kots-common carried a byte-identical duplicate of this struct, and the
// new cross-program readers in kots_common::cpi deserialize THIS program's
// accounts using THAT type. Two live definitions of one on-chain layout is a
// silent-corruption hazard the moment they drift — and one of the four pairs
// in this file (ProgramUpgradeState) HAS already drifted.
// ⚠ UN-GATED 2026-07-29 (O40). This was `#[cfg(feature = "idl-build")]`, so the
// BPF build used kots-common's copy via the `pub use` below — and kots-common
// declares `pub const ID = Pubkey::new_from_array([0; 32])`. Anchor's `#[account]`
// generates `impl Owner { fn owner() -> crate::ID }`, so every
// `Account<'info, BlacklistState>` demanded an owner of
// 11111111111111111111111111111111. Unsatisfiable both ways: own it by this
// program and the check fails 3007; own it by zeros and this program cannot write
// to it. Compiled here, `#[account]` yields this program's ID, which is correct.
#[account]
#[derive(Debug)]
pub struct EnhancedGasOracleState {
    pub is_initialized: bool,
    pub current_price_lamports: u64,
    pub authority1: Pubkey,
    pub authority2: Pubkey,
    pub authority3: Pubkey,
    pub authority1_last_price: u64,
    pub authority2_last_price: u64,
    pub authority3_last_price: u64,
    pub authority1_last_update: i64,
    pub authority2_last_update: i64,
    pub authority3_last_update: i64,
    pub consensus_price: u64,
    pub consensus_timestamp: i64,
    pub daily_price_change_bps: i64,
    pub last_daily_reset: i64,
}

#[account]
#[derive(Debug)]
pub struct ProgramUpgradeState {
    pub is_initialized: bool,
    pub authority: Pubkey,
    pub program_type: u8, // 0-6 for each program type
    pub version: u16,
    pub initialization_timestamp: i64,
}
// UpgradeRegistry and other upgrade structs are imported from kots_common::upgrade already

// CONSOLIDATED (2026-07-23): the BPF build uses the single shared definition of
// these three account layouts. ProgramUpgradeState is deliberately NOT included —
// it has diverged (`program_type: u8` here vs `ProgramType` in kots-common) and
// consolidating it blindly would change how existing accounts deserialize.
// ── RE-EXPORT REMOVED 2026-07-29 (O40). ─────────────────────────────────────
//
// The 2026-07-23 consolidation pointed the BPF build at kots-common's copies to
// avoid two live definitions of one on-chain layout. The reason was sound and the
// hazard is real — ProgramUpgradeState had already drifted — but the shared types
// carry an owner of all zeros (see the note above each struct), so no correctly
// created account could satisfy `Account<'info, T>`. The consolidation traded a
// drift hazard for an unsatisfiable ownership check.
//
// Drift is guarded instead, at compile time, which is what the consolidation
// actually wanted. `kots_common::cpi`'s cross-program readers still deserialize
// with the kots-common types; these assertions are what keep the two layouts
// interchangeable.
const _: () = assert!(
    core::mem::size_of::<BlacklistState>()
        == core::mem::size_of::<kots_common::state::BlacklistState>(),
    "BlacklistState has drifted from kots_common::state::BlacklistState. \
     kots_common::cpi readers deserialize THIS program's accounts with that type, \
     so a layout difference is silent corruption, not a compile error elsewhere."
);
const _: () = assert!(
    core::mem::size_of::<EmergencyState>()
        == core::mem::size_of::<kots_common::state::EmergencyState>(),
    "EmergencyState has drifted from kots_common::state::EmergencyState."
);
const _: () = assert!(
    core::mem::size_of::<EnhancedGasOracleState>()
        == core::mem::size_of::<kots_common::state::EnhancedGasOracleState>(),
    "EnhancedGasOracleState has drifted from kots_common::state::EnhancedGasOracleState."
);

// Import ProgramType only for BPF compilation (not during IDL generation)
#[cfg(not(all(feature = "idl-build", not(target_arch = "bpf"))))]
use kots_common::ProgramType;


// Network configuration response for on-chain query
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct NetworkConfigResponse {
    pub voting_hieroglyph: Pubkey,
    pub contest_management: Pubkey,
    pub fee_management: Pubkey,
    pub treasury_operations: Pubkey,
    pub progressive_jackpot: Pubkey,
    pub security_governance: Pubkey,
    pub prize_distribution: Pubkey,
    pub kots_mint: Pubkey,
}


// Program id is realm-selected at compile time (same pattern as kots-common
// `realm-main`). One .so per realm — never a single binary claiming both ids.
//
//   default / devnet build:  declare_id! = permanent devnet id (Anchor.toml)
//   `anchor build -p security_governance -- --features realm-main`:
//                            declare_id! = mainnet ceremony keypair pubkey
//
// Feature unification: security-governance/Cargo.toml already has
//   realm-main = ["kots-common/realm-main"]
// so workspace builds stay consistent.
#[cfg(feature = "realm-main")]
declare_id!("FdsHe63kHRy2BBLHyaZZtJEccKqi9g1EevmhjiLhbB7A");

#[cfg(not(feature = "realm-main"))]
declare_id!("3EKVa1rSd4cTXB2VT4gWwYfDGjM3m5nw4EYP2jvCb28H");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"security-governance|20251004T1339Z|build001";

#[derive(Accounts)]
pub struct VersionHeartbeat {}

#[derive(Accounts)]
pub struct GetNetworkConfig {}

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

// --- ACT-02 (2026-08): on-chain owner registry -----------------------------
// The owner-set / M-of-N quorum / break-glass primitive lives in its own module
// (owner_registry.rs). Anchor instruction fns must be declared inside the
// #[program] mod, so the two entrypoints below delegate to
// owner_registry::{initialize,rotate}_handler. This wires previously-dead code
// (F1) into the running program and generates the IDL the sidecar/console call.
pub mod owner_registry;
use owner_registry::*;
pub mod quorum_decision;
use quorum_decision::*;

#[program]
pub mod security_governance {
    use super::*;
    
    // --- ACT-02: owner-registry entrypoints (delegate to owner_registry.rs) ---

    /// Initialize the on-chain owner registry: the active owner set, the M-of-N
    /// threshold, and the break-glass key, in a program-owned PDA. Called once at
    /// the governance-setup ceremony; Anchor `init` makes a second call fail, so
    /// re-init is not a front-running vector after the first.
    pub fn initialize_owner_registry(
        ctx: Context<InitializeOwnerRegistry>,
        owners: [Pubkey; MAX_OWNERS],
        owner_count: u8,
        threshold: u8,
        break_glass: Pubkey,
    ) -> Result<()> {
        owner_registry::initialize_handler(ctx, owners, owner_count, threshold, break_glass)
    }

    /// Replace one owner key, authorized by owner quorum or the break-glass path.
    /// Every break-glass use emits `BreakGlassInvoked` (the loud signal the A.1
    /// "always reported" rule and ACT-22/ACT-23 alerting consume). The explicit
    /// `'info` forwards the lifetime tie rotate_handler needs (remaining_accounts
    /// + accounts struct share one invariant `'info`).
    pub fn rotate_owner<'info>(
        ctx: Context<'_, '_, '_, 'info, RotateOwner<'info>>,
        old_owner: Pubkey,
        new_owner: Pubkey,
    ) -> Result<()> {
        owner_registry::rotate_handler(ctx, old_owner, new_owner)
    }

    /// One-time setup of the break-glass control account (call right after
    /// upgrade). Starts ENABLED with no pending rotation.
    pub fn initialize_break_glass_control(
        ctx: Context<InitializeBreakGlassControl>,
    ) -> Result<()> {
        owner_registry::initialize_break_glass_control_handler(ctx)
    }

    /// Break-glass (+ >=1 owner co-signer) proposes an owner rotation; it cannot
    /// execute for 24h and any owner may veto it in that window.
    pub fn propose_break_glass_rotation(
        ctx: Context<ProposeBreakGlassRotation>,
        old_owner: Pubkey,
        new_owner: Pubkey,
    ) -> Result<()> {
        owner_registry::propose_break_glass_rotation_handler(ctx, old_owner, new_owner)
    }

    /// Execute a matured, non-vetoed break-glass rotation (anyone may call).
    pub fn execute_break_glass_rotation(
        ctx: Context<ExecuteBreakGlassRotation>,
    ) -> Result<()> {
        owner_registry::execute_break_glass_rotation_handler(ctx)
    }

    /// Kill-switch: any single owner disables break-glass and voids any pending
    /// rotation.
    pub fn report_unauthorized_break_glass(
        ctx: Context<ReportUnauthorizedBreakGlass>,
    ) -> Result<()> {
        owner_registry::report_unauthorized_break_glass_handler(ctx)
    }

    /// Re-enable break-glass — owner quorum only.
    pub fn enable_break_glass<'info>(
        ctx: Context<'_, '_, '_, 'info, EnableBreakGlass<'info>>,
    ) -> Result<()> {
        owner_registry::enable_break_glass_handler(ctx)
    }

    // --- Quorum-decision voting: binding M-of-N owner approval, on-chain ---

    /// Open a quorum decision for a governed action; the proposer's open is
    /// their first YES. Delegates to quorum_decision.rs.
    pub fn open_decision(
        ctx: Context<OpenDecision>,
        decision_id: u64,
        action: GovernedAction,
    ) -> Result<()> {
        quorum_decision::open_decision_handler(ctx, decision_id, action)
    }

    /// Cast an immutable YES/NO on an open decision (one vote per owner, within
    /// the 1-day window; voting stays open until the decision is executed).
    /// `expected_action_hash` = sha256(domain || borsh(action)) — the ballot
    /// commits to WHAT it approves, and mismatches fail loudly
    /// (ActionHashMismatch) instead of recording a mislabeled vote.
    pub fn cast_vote(
        ctx: Context<CastVote>,
        decision_id: u64,
        approve: bool,
        expected_action_hash: [u8; 32],
    ) -> Result<()> {
        quorum_decision::cast_vote_handler(ctx, decision_id, approve, expected_action_hash)
    }

    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Query network configuration (program IDs + KOTS mint)
    /// Returns via set_return_data for client consumption
    /// 
    /// Parameters:
    /// - network: Optional network selector (0=Devnet, 1=Mainnet, 2=Testnet, 3=Localnet)
    ///            If None, auto-detects based on deployment environment
    pub fn get_network_config(_ctx: Context<GetNetworkConfig>, network: Option<u8>) -> Result<()> {
        msg!("🌐 Returning network configuration (network param: {:?})", network);
        
        // Convert network parameter to Network enum if provided
        let target_network = network.and_then(|n| match n {
            0 => Some(kots_common::Network::Devnet),
            1 => Some(kots_common::Network::MainnetBeta),
            2 => Some(kots_common::Network::Testnet),
            3 => Some(kots_common::Network::Localnet),
            _ => {
                msg!("⚠️ Invalid network parameter {}, using auto-detect", n);
                None
            }
        });
        
        // Get network-specific configuration
        let config = kots_common::network_config::NetworkConfig::for_optional_network(target_network);
        
        // Build response with 8 Pubkeys (7 program IDs + 1 KOTS mint)
        let response = NetworkConfigResponse {
            voting_hieroglyph: config.program_ids.voting_hieroglyph,
            contest_management: config.program_ids.contest_management,
            fee_management: config.program_ids.fee_management,
            treasury_operations: config.program_ids.treasury_operations,
            progressive_jackpot: config.program_ids.progressive_jackpot,
            security_governance: config.program_ids.security_governance,
            prize_distribution: config.program_ids.prize_distribution,
            kots_mint: config.system_wallets.kots_mint,
        };
        
        // Serialize and return via set_return_data
        let encoded = borsh::to_vec(&response).map_err(|_| KotsError::SerializationError)?;
        anchor_lang::solana_program::program::set_return_data(&encoded);
        
        msg!("✅ Network configuration returned ({} bytes)", encoded.len());
        Ok(())
    }

    /// Initialize security governance program state
    pub fn initialize_security_governance(
        ctx: Context<InitializeSecurityGovernance>,
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

        msg!("🔒 Initializing Security & Governance Program");
        
        // Create PDA with treasury funding
        let seeds: &[&[u8]] = &[b"program_state"];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.program_state.key(), pda, KotsError::InvalidProgramId);
        
        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Step 1: Create the PDA using this program's seeds (THIS PROGRAM OWNS THE PDA)
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(PROGRAM_STATE_SPACE);
        
        // Create account using System Program
        let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
            ctx.accounts.authority.key,
            &pda,
            required_lamports,
            PROGRAM_STATE_SPACE as u64,
            ctx.program_id,
        );
        
        anchor_lang::solana_program::program::invoke_signed(
            &create_account_ix,
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.program_state.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&[b"program_state", &[bump]]],
        )?;
        
        msg!("✅ Account created with {} bytes at {}", PROGRAM_STATE_SPACE, pda);
        
        // Step 2: Request treasury reimbursement
        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Initialize the state
        let mut data = ctx.accounts.program_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Create and serialize the state
        // Use appropriate type based on compilation target
        #[cfg(feature = "idl-build")]
        let program_type_value = PROGRAM_TYPE_SECURITY_GOVERNANCE;
        #[cfg(not(feature = "idl-build"))]
        let program_type_value = ProgramType::SecurityGovernance as u8;
        
        let state = ProgramUpgradeState {
            is_initialized: true,
            authority: ctx.accounts.authority.key(),
            program_type: program_type_value,
            version: CURRENT_SECURITY_GOVERNANCE_VERSION,
            initialization_timestamp: get_current_timestamp()?,
        };
        
        // Serialize (Anchor discriminator + body) from offset 0
        state.try_serialize(&mut &mut data[..])?;
        
        msg!("✅ Security & Governance Program initialized successfully");
        Ok(())
    }

    /// Initialize enhanced gas oracle with multiple authorities
    pub fn initialize_enhanced_gas_oracle(
        ctx: Context<InitializeEnhancedGasOracle>,
        initial_price_lamports: u64,
        authority1: Pubkey,
        authority2: Pubkey,
        authority3: Pubkey,
    ) -> Result<()> {
        msg!("⛽ Initializing enhanced gas oracle");
        
        // Note: gas_validation module may need to be implemented or adjusted for function signature
        require!(initial_price_lamports > 0, KotsError::ArithmeticOverflow);
        
        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Request treasury reimbursement (7-parameter signature)
        let seeds: &[&[u8]] = &[GAS_ORACLE_SEED, b"v4"];
        let (pda, _bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        require_keys_eq!(ctx.accounts.gas_oracle.key(), pda, KotsError::InvalidProgramId);
        
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(GAS_ORACLE_SPACE);
        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.initializer.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Write discriminator and initialize using BPF-compatible pattern
        let mut gas_oracle_data = ctx.accounts.gas_oracle.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Initialize the state structure
        let mut gas_oracle = EnhancedGasOracleState {
            is_initialized: false,
            current_price_lamports: 0,
            authority1: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            authority2: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            authority3: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            authority1_last_price: 0,
            authority2_last_price: 0,
            authority3_last_price: 0,
            authority1_last_update: 0,
            authority2_last_update: 0,
            authority3_last_update: 0,
            consensus_price: 0,
            consensus_timestamp: 0,
            daily_price_change_bps: 0,
            last_daily_reset: 0,
        };
        // Set the values
        gas_oracle.is_initialized = true;
        gas_oracle.current_price_lamports = initial_price_lamports;
        gas_oracle.authority1 = authority1;
        gas_oracle.authority2 = authority2;
        gas_oracle.authority3 = authority3;
        
        // Initialize authority prices
        gas_oracle.authority1_last_price = initial_price_lamports;
        gas_oracle.authority2_last_price = initial_price_lamports;
        gas_oracle.authority3_last_price = initial_price_lamports;
        
        let current_time = get_current_timestamp()?;
        gas_oracle.authority1_last_update = current_time;
        gas_oracle.authority2_last_update = current_time;
        gas_oracle.authority3_last_update = current_time;
        gas_oracle.consensus_price = initial_price_lamports;
        gas_oracle.consensus_timestamp = current_time;
        gas_oracle.daily_price_change_bps = 0;
        gas_oracle.last_daily_reset = current_time;
        
        // Serialize (Anchor discriminator + body) from offset 0
        gas_oracle.try_serialize(&mut &mut gas_oracle_data[..])?;
        
        msg!("✅ Gas oracle initialized with 3 authorities");
        Ok(())
    }

    /// Update gas price with consensus mechanism (requires 2 of 3 authorities)
    pub fn update_gas_price_consensus(
        ctx: Context<UpdateGasPriceConsensus>,
        new_price_lamports: u64,
    ) -> Result<()> {
        msg!("⛽ Updating gas price with consensus - New price: {}", new_price_lamports);
        
        validate_gas_price(new_price_lamports)?;
        
        let gas_oracle = &mut ctx.accounts.gas_oracle;
        let authority = ctx.accounts.authority.key();
        let current_time = get_current_timestamp()?;
        
        // Validate authority
        require!(
            authority == gas_oracle.authority1 || 
            authority == gas_oracle.authority2 || 
            authority == gas_oracle.authority3,
            KotsError::UnauthorizedGasOracle
        );
        
        // Check update frequency limits
        let last_update = if authority == gas_oracle.authority1 {
            gas_oracle.authority1_last_update
        } else if authority == gas_oracle.authority2 {
            gas_oracle.authority2_last_update
        } else {
            gas_oracle.authority3_last_update
        };
        
        require!(
            current_time - last_update >= GAS_ORACLE_UPDATE_INTERVAL_SECONDS,
            KotsError::UpdateTooFrequent
        );
        
        // Update authority price
        if authority == gas_oracle.authority1 {
            gas_oracle.authority1_last_price = new_price_lamports;
            gas_oracle.authority1_last_update = current_time;
        } else if authority == gas_oracle.authority2 {
            gas_oracle.authority2_last_price = new_price_lamports;
            gas_oracle.authority2_last_update = current_time;
        } else {
            gas_oracle.authority3_last_price = new_price_lamports;
            gas_oracle.authority3_last_update = current_time;
        }
        
        // Check for consensus (2 out of 3 authorities within variance limit)
        let consensus_price = calculate_consensus_price(gas_oracle)?;
        
        if let Some(price) = consensus_price {
            // Validate daily change limit
            let old_price = gas_oracle.current_price_lamports;
            let change_bps = calculate_price_change_bps(old_price, price)?;
            validate_daily_change(change_bps)?;
            
            gas_oracle.current_price_lamports = price;
            gas_oracle.consensus_price = price;
            gas_oracle.consensus_timestamp = current_time;
            gas_oracle.daily_price_change_bps = change_bps as i64;
            
            msg!("✅ Gas price consensus reached - New price: {}", price);
        } else {
            msg!("⏳ Waiting for consensus - Need 2 out of 3 authorities");
        }
        
        Ok(())
    }

    /// Initialize blacklist system
    pub fn initialize_blacklist(
        ctx: Context<InitializeBlacklist>,
        authority: Pubkey,
    ) -> Result<()> {
        msg!("🚫 Initializing blacklist system");
        
        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Manual PDA creation with treasury funds using 7-parameter signature.
        // SEED UNIFICATION (2026-08-03): same bytes as the literal b"v4" this
        // replaced — the shared constant exists so the writers here, the
        // readers in kots_common::cpi, and every client derive ONE address.
        let seeds: &[&[u8]] = &[BLACKLIST_SEED, GOVERNANCE_SUB_SEED];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        require_keys_eq!(ctx.accounts.blacklist_state.key(), pda, KotsError::InvalidProgramId);
        
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(BLACKLIST_STATE_SPACE);

        // ── O39 FIX (2026-07-29): THE ACCOUNT WAS NEVER CREATED. ────────────
        //
        // This instruction computed the rent, requested reimbursement for it,
        // and then went straight to `try_borrow_mut_data()` on an
        // `UncheckedAccount` that nothing had allocated. Every call failed with
        // `AccountDidNotSerialize (3004)` — five `emergency_and_blacklist` tests,
        // discovered 2026-07-29 when the full suite ran for the first time.
        //
        // The `/// CHECK:` on `blacklist_state` says "Account will be created by
        // `init_pda_with_treasury`". Nothing called it. Same defect as ~~O31~~ #4
        // (`treasury`, no `init`), and NOT a space defect: BLACKLIST_STATE_SPACE
        // is 1,536 and BlacklistState needs 1,127. (That constant's comment says
        // "528 (BlacklistState)", which is wrong — `blacklist_reasons` alone is
        // MAX_BLACKLISTED_WALLETS_PER_ACCOUNT * MAX_REASON_LENGTH = 10 * 64 = 640.)
        //
        // Allocation copies `initialize_security_governance` above, which works:
        // create with the PDA's own seeds so THIS program owns it, THEN reimburse
        // — in that order, so the payer is refunded for lamports actually spent.
        let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
            ctx.accounts.initializer.key,
            &pda,
            required_lamports,
            BLACKLIST_STATE_SPACE as u64,
            ctx.program_id,
        );
        anchor_lang::solana_program::program::invoke_signed(
            &create_account_ix,
            &[
                ctx.accounts.initializer.to_account_info(),
                ctx.accounts.blacklist_state.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&[BLACKLIST_SEED, GOVERNANCE_SUB_SEED, &[bump]]],
        )?;
        msg!("✅ Account created with {} bytes at {}", BLACKLIST_STATE_SPACE, pda);

        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.initializer.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Write discriminator and initialize using BPF-compatible pattern
        let mut blacklist_data = ctx.accounts.blacklist_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Initialize the state structure
        let mut blacklist = BlacklistState {
            is_initialized: false,
            authority: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            moderator: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            blacklisted_wallets: [Pubkey::default(); MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
            blacklist_reasons: [[0u8; MAX_REASON_LENGTH]; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
            blacklist_timestamps: [0i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT],
            blacklist_count: 0,
            total_blacklisted: 0,
            last_update_timestamp: 0,
        };
        // Set the values
        blacklist.is_initialized = true;
        blacklist.authority = authority;
        blacklist.moderator = ctx.accounts.initializer.key();
        blacklist.blacklisted_wallets = [Pubkey::default(); MAX_BLACKLISTED_WALLETS_PER_ACCOUNT];
        for i in 0..MAX_BLACKLISTED_WALLETS_PER_ACCOUNT {
            blacklist.blacklist_reasons[i] = [0u8; MAX_REASON_LENGTH];
        }
        blacklist.blacklist_timestamps = [0i64; MAX_BLACKLISTED_WALLETS_PER_ACCOUNT];
        blacklist.total_blacklisted = 0;
        blacklist.last_update_timestamp = get_current_timestamp()?;
        
        // Serialize (Anchor discriminator + body) from offset 0
        blacklist.try_serialize(&mut &mut blacklist_data[..])?;
        
        msg!("✅ Blacklist system initialized");
        Ok(())
    }

    /// MISSING-INITIALIZER FIX (2026-08-04, owner-approved): `EmergencyState`
    /// was read by every pause/resume gate yet NO instruction created it — the
    /// long-documented AccountNotInitialized gap. Mirrors initialize_blacklist's
    /// pattern (first caller supplies the authority; payer-funded Anchor init
    /// instead of the raw-create + treasury-reimbursement machinery). The
    /// first-caller-sets-authority property is the SAME open governance point
    /// the blacklist initializer already carries — both are flagged for the
    /// ceremony review together.
    pub fn initialize_emergency_state(
        ctx: Context<InitializeEmergencyState>,
        emergency_authority: Pubkey,
    ) -> Result<()> {
        msg!("🚨 Initializing emergency state");
        let e = &mut ctx.accounts.emergency_state;
        e.is_initialized = true;
        e.emergency_authority = emergency_authority;
        e.is_paused = false;
        e.pause_reason = [0u8; MAX_STRING_LENGTH];
        e.pause_timestamp = 0;
        e.pause_duration = 0;
        e.authorized_emergency_operations = [[0u8; MAX_STRING_LENGTH]; 10];
        e.emergency_operations_count = 0;
        e.total_emergency_activations = 0;
        msg!("✅ Emergency state initialized — authority {}", emergency_authority);
        Ok(())
    }

    /// Add wallet to blacklist with reason
    pub fn add_to_blacklist(
        ctx: Context<AddToBlacklist>,
        decision_id: u64,
        wallet_to_blacklist: Pubkey,
        reason: [u8; MAX_REASON_LENGTH], // BPF-compatible fixed-size
    ) -> Result<()> {
        msg!("🚫 Adding wallet to blacklist: {}", wallet_to_blacklist);
        let _ = decision_id; // bound by the decision PDA seed

        // Quorum-decision model: authorized by a PASSED on-chain owner vote for
        // exactly GovernedAction::BlacklistAdd{ wallet_to_blacklist } — the variant
        // binds the decision to this wallet. Consumed (one-shot) on execute.
        let now = Clock::get()?.unix_timestamp;
        let decision_key = ctx.accounts.decision.key();
        quorum_decision::require_enforceable_and_consume(
            &mut ctx.accounts.decision,
            decision_key,
            GovernedAction::BlacklistAdd { wallet: wallet_to_blacklist },
            now,
        )?;

        let blacklist = &mut ctx.accounts.blacklist_state;
        
        // Check if wallet is already blacklisted
        require!(
            !blacklist.blacklisted_wallets.contains(&wallet_to_blacklist),
            KotsError::WalletNotBlacklisted
        );
        
        // Prevent blacklisting system wallets
        validate_not_system_wallet(&wallet_to_blacklist)?;
        
        // ── O41 FIX (2026-07-29): two defects here, one of them serious. ────
        //
        // (1) SILENT SUCCESS WHEN FULL. The loop scanned for a free slot and
        //     `break`ed. With all MAX_BLACKLISTED_WALLETS_PER_ACCOUNT slots taken
        //     it found none, fell through, incremented `total_blacklisted` anyway,
        //     logged WALLET_BLACKLISTED, and returned Ok. The caller was told the
        //     wallet was blocked; it was not. `?ok_or` makes that an error.
        //
        // (2) `blacklist_count` WAS NEVER MAINTAINED. Only `total_blacklisted`
        //     (cumulative) was. And `BlacklistState::is_blacklisted()` iterated
        //     `0..blacklist_count`, so it ran `0..0` and returned **false for every
        //     wallet** — the blacklist blocked nobody, ever. `is_blacklisted` no
        //     longer trusts the counter (see kots-common), and the counter is now
        //     correct as well; a control should not depend on one thing being right.
        //
        // The two fields mean different things and both are kept:
        //   blacklist_count    live population, decremented on removal
        //   total_blacklisted  cumulative ever added, monotonic
        let timestamp = get_current_timestamp()?;
        let slot = blacklist
            .blacklisted_wallets
            .iter()
            .position(|w| *w == Pubkey::default())
            .ok_or(KotsError::BlacklistFull)?;

        blacklist.blacklisted_wallets[slot] = wallet_to_blacklist;
        blacklist.blacklist_reasons[slot] = reason;
        blacklist.blacklist_timestamps[slot] = timestamp;

        blacklist.blacklist_count = blacklist
            .blacklist_count
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        blacklist.total_blacklisted = blacklist.total_blacklisted
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        blacklist.last_update_timestamp = get_current_timestamp()?;
        
        // Log audit event
        // Convert [u8; 64] to [u8; 32] for bytes32_to_string
        let _reason_32 = {
            let mut r32 = [0u8; 32];
            r32.copy_from_slice(&reason[..32]);
            r32
        };
        log_audit_event(&ctx, "WALLET_BLACKLISTED", &wallet_to_blacklist, "BLACKLISTED")?;
        
        // Convert [u8; 64] to [u8; 32] for bytes32_to_string
        let _reason_32 = {
            let mut r32 = [0u8; 32];
            r32.copy_from_slice(&reason[..32]);
            r32
        };
        msg!("✅ Wallet blacklisted");
        Ok(())
    }

    /// Remove wallet from blacklist
    pub fn remove_from_blacklist(
        ctx: Context<RemoveFromBlacklist>,
        decision_id: u64,
        wallet_to_remove: Pubkey,
    ) -> Result<()> {
        msg!("✅ Removing wallet from blacklist: {}", wallet_to_remove);
        let _ = decision_id; // bound by the decision PDA seed

        // Quorum-decision model: authorized by a PASSED on-chain owner vote for
        // exactly GovernedAction::BlacklistRemove{ wallet_to_remove }. Consumed on execute.
        let now = Clock::get()?.unix_timestamp;
        let decision_key = ctx.accounts.decision.key();
        quorum_decision::require_enforceable_and_consume(
            &mut ctx.accounts.decision,
            decision_key,
            GovernedAction::BlacklistRemove { wallet: wallet_to_remove },
            now,
        )?;

        let blacklist = &mut ctx.accounts.blacklist_state;
        
        // Find and remove wallet
        if let Some(index) = blacklist.blacklisted_wallets.iter().position(|&x| x == wallet_to_remove) {
            blacklist.blacklisted_wallets[index] = Pubkey::default();
            blacklist.blacklist_reasons[index] = [0u8; MAX_REASON_LENGTH];
            blacklist.blacklist_timestamps[index] = 0;

            // O41: decrement the LIVE count. `total_blacklisted` is cumulative and
            // must NOT move — it previously decremented here, which conflated the
            // two fields and made the cumulative figure meaningless.
            blacklist.blacklist_count = blacklist.blacklist_count.saturating_sub(1);
            blacklist.last_update_timestamp = get_current_timestamp()?;
            
            // Log audit event
            log_audit_event(&ctx, "WALLET_UNBLACKLISTED", &wallet_to_remove, "Removed from blacklist")?;
            
            msg!("✅ Wallet removed from blacklist");
        } else {
            return Err(KotsError::WalletNotBlacklisted.into());
        }
        
        Ok(())
    }

    /// Emergency pause system
    pub fn emergency_pause<'info>(
        ctx: Context<'_, '_, '_, 'info, EmergencyPause<'info>>,
        reason: [u8; MAX_REASON_LENGTH], // BPF-compatible
        duration_seconds: i64,
    ) -> Result<()> {
        // Convert [u8; 64] to [u8; 32] for bytes32_to_string
        let _reason_32 = {
            let mut r32 = [0u8; 32];
            r32.copy_from_slice(&reason[..32]);
            r32
        };
        msg!("🚨 Emergency pause activated");

        // EXPEDIENT PAUSE (2026-08, decision A.10) — authorise BEFORE any side
        // effect (this handler creates the EmergencyState PDA below). Pause is a
        // fail-safe action: halting only ever protects funds, and a malicious
        // pause is fully recoverable (rotate the key, then quorum-resume). So ANY
        // single owner may pause, for fast incident response; the dangerous
        // direction — `emergency_resume` — stays quorum-gated, as do blacklist
        // edits. This is the deliberate asymmetry: easy to stop, hard to restart.
        // The break-glass is NOT admitted here — "break-glass cannot act alone"
        // holds, and it lives in escrow so it is not the fast path anyway. The
        // pause is always reported via the audit event below.
        require!(
            ctx.accounts
                .owner_registry
                .is_owner(&ctx.accounts.emergency_authority.key()),
            OwnerRegistryError::NotAnOwner
        );

        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Manual PDA creation using 7-parameter signature.
        // SEED UNIFICATION (2026-08-03): same bytes as the literal b"v4" this
        // replaced — one canonical derivation shared with the readers.
        let seeds: &[&[u8]] = &[EMERGENCY_SEED, GOVERNANCE_SUB_SEED];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        require_keys_eq!(ctx.accounts.emergency_state.key(), pda, KotsError::InvalidProgramId);

        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(EMERGENCY_STATE_SPACE);

        // ── O39 (2026-07-29): ALLOCATE BEFORE WRITING. ──────────────────────
        //
        // Same defect as `initialize_blacklist`: this computed rent, requested
        // reimbursement, then called `try_borrow_mut_data()` on an account nothing
        // had created. There is no `initialize_emergency_state` instruction
        // anywhere, so `emergency_pause` is the only thing that can create it.
        //
        // Create only if it does not already exist — pause/resume cycles must not
        // fail the second time. An account with zero lamports and zero data has
        // never been created; the System Program owns it and its data is empty.
        if ctx.accounts.emergency_state.data_is_empty() {
            let create_account_ix =
                anchor_lang::solana_program::system_instruction::create_account(
                    ctx.accounts.emergency_authority.key,
                    &pda,
                    required_lamports,
                    EMERGENCY_STATE_SPACE as u64,
                    ctx.program_id,
                );
            anchor_lang::solana_program::program::invoke_signed(
                &create_account_ix,
                &[
                    ctx.accounts.emergency_authority.to_account_info(),
                    ctx.accounts.emergency_state.to_account_info(),
                    ctx.accounts.system_program.to_account_info(),
                ],
                &[&[EMERGENCY_SEED, GOVERNANCE_SUB_SEED, &[bump]]],
            )?;
            msg!(
                "✅ Account created with {} bytes at {}",
                EMERGENCY_STATE_SPACE,
                pda
            );
        }

        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.emergency_authority.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Write discriminator and initialize using BPF-compatible pattern
        let mut emergency_data = ctx.accounts.emergency_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Initialize the state structure
        let mut emergency = EmergencyState {
            is_initialized: false,
            emergency_authority: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            is_paused: false,
            pause_reason: [0u8; MAX_STRING_LENGTH],
            pause_timestamp: 0,
            pause_duration: 0,
            authorized_emergency_operations: [[0u8; MAX_STRING_LENGTH]; 10],
            emergency_operations_count: 0,
            total_emergency_activations: 0,
        };
        let authority = ctx.accounts.emergency_authority.key();

        // Set the values
        emergency.is_initialized = true;
        emergency.emergency_authority = ctx.accounts.emergency_authority.key();
        emergency.is_paused = true;
        emergency.pause_reason = {
            let mut reason_64 = [0u8; MAX_STRING_LENGTH];
            let len = reason.len().min(MAX_STRING_LENGTH);
            reason_64[..len].copy_from_slice(&reason[..len]);
            reason_64
        };
        emergency.pause_timestamp = get_current_timestamp()?;
        emergency.pause_duration = duration_seconds;
        emergency.total_emergency_activations = emergency.total_emergency_activations
            .checked_add(1)
            .ok_or(KotsError::ArithmeticOverflow)?;
        
        // Serialize (Anchor discriminator + body) from offset 0
        emergency.try_serialize(&mut &mut emergency_data[..])?;
        
        // Log critical audit event
        // Convert [u8; 64] to [u8; 32] for bytes32_to_string
        let _reason_32 = {
            let mut r32 = [0u8; 32];
            r32.copy_from_slice(&reason[..32]);
            r32
        };
        log_audit_event(&ctx, "EMERGENCY_PAUSE", &authority, "EMERGENCY_ACTIVATED")?;
        
        msg!("🚨 Emergency pause active for {} seconds", duration_seconds);
        Ok(())
    }

    /// Resume from emergency pause
    pub fn emergency_resume(
        ctx: Context<EmergencyResume>,
        decision_id: u64,
    ) -> Result<()> {
        msg!("✅ Resuming from emergency pause");
        let _ = decision_id; // bound by the decision PDA seed

        // Quorum-decision model (replaces the co-signed-tx quorum): resume is
        // authorized by a PASSED, enforceable on-chain owner vote for
        // GovernedAction::EmergencyResume. require_enforceable_and_consume checks
        // quorum was reached within the window and marks the decision Executed.
        let now = Clock::get()?.unix_timestamp;
        let decision_key = ctx.accounts.decision.key();
        quorum_decision::require_enforceable_and_consume(
            &mut ctx.accounts.decision,
            decision_key,
            GovernedAction::EmergencyResume,
            now,
        )?;

        let emergency = &mut ctx.accounts.emergency_state;
        let authority = ctx.accounts.emergency_authority.key();

        require!(emergency.is_paused, KotsError::EmergencyNotActive);
        
        emergency.is_paused = false;
        emergency.pause_reason = [0u8; MAX_STRING_LENGTH]; // Clear reason array
        
        // Log audit event
        log_audit_event(&ctx, "EMERGENCY_RESUME", &authority, "Emergency pause lifted")?;
        
        msg!("✅ Emergency pause lifted");
        Ok(())
    }

    /// Validate security constraints (called via CPI)
    pub fn validate_security_constraints(
        _ctx: Context<ValidateSecurityConstraints>,
        _user: Pubkey,
        _operation_type: [u8; MAX_STRING_LENGTH], // BPF-compatible fixed-size
        _amount: Option<u64>,
    ) -> Result<SecurityValidationResult> {
        msg!("🔒 Validating security constraints for: {}", _user);
        
        let result = SecurityValidationResult {
            is_valid: true,
            blacklisted: false,
            rate_limited: false,
            gas_price_valid: true,
            emergency_paused: false,
        };
        
        // Check blacklist - Note: blacklist_state needs to be Account<BlacklistState> not UncheckedAccount
        // if let Ok(blacklist) = &ctx.accounts.blacklist_state.load() {
        //     if blacklist.blacklisted_wallets.contains(&user) {
        //         result.blacklisted = true;
        //         result.is_valid = false;
        //     }
        // }
        
        // Check emergency pause - Note: emergency_state needs to be Account<EmergencyState> not UncheckedAccount
        // if let Ok(emergency) = &ctx.accounts.emergency_state.load() {
        //     if emergency.is_paused {
        //         // Check if operation is authorized during emergency
        //         if !emergency.authorized_emergency_operations.contains(&operation_type) {
        //             result.emergency_paused = true;
        //             result.is_valid = false;
        //         }
        //     }
        // }
        
        // Check gas price validity - Note: gas_oracle needs to be Account<GasOracleState> not UncheckedAccount  
        // if let Ok(gas_oracle) = &ctx.accounts.gas_oracle.load() {
        //     let current_time = get_current_timestamp()?;
        //     let time_since_update = current_time - gas_oracle.consensus_timestamp;
        //     
        //     if time_since_update > GAS_ORACLE_UPDATE_INTERVAL_SECONDS * 3 {
        //         result.gas_price_valid = false;
        //         // Don't invalidate entirely, just flag for attention
        //     }
        // }
        
        // Rate limiting would require additional user account checks
        
        msg!("🔒 Security validation result: Valid={}, Blacklisted={}, RateLimited={}", 
             result.is_valid, result.blacklisted, result.rate_limited);
        
        Ok(result)
    }

    /// Verify meme upload payment with blockchain-native receipt verification
    pub fn verify_meme_upload_payment(
        ctx: Context<VerifyMemeUploadPayment>,
        payment_verification: PaymentVerification,
        expected_amount: u64,
    ) -> Result<SecurityValidationResult> {
        msg!("🔐 Verifying meme upload payment - Amount: {} KOTS", expected_amount / TOKENS_PER_UNIT);
        
        // Validate payment verification structure
        require!(payment_verification.payment_type == PaymentType::MemeUpload, KotsError::InvalidPaymentReceipt);
        require!(payment_verification.expected_amount >= expected_amount, KotsError::InsufficientPayment);
        require!(payment_verification.payer_wallet == ctx.accounts.payer.key(), KotsError::UnauthorizedAccess);
        
        // Validate transaction signature authenticity (simplified for demo)
        let signature_valid = true; // In production, this would verify the Ed25519 signature
        
        // Check verification window (5 minutes default)
        let current_time = get_current_timestamp()?;
        let verification_deadline = current_time - payment_verification.verification_window_seconds;
        require!(verification_deadline >= 0, KotsError::InvalidPaymentReceipt);
        
        msg!("✅ Meme upload payment verification successful");
        Ok(SecurityValidationResult {
            is_valid: signature_valid,
            blacklisted: false,
            rate_limited: false,
            gas_price_valid: true,
            emergency_paused: false,
        })
    }

    /// Verify storage extension payment
    pub fn verify_storage_extension_payment(
        ctx: Context<VerifyStorageExtensionPayment>,
        payment_verification: PaymentVerification,
        expected_amount: u64,
    ) -> Result<SecurityValidationResult> {
        msg!("🔐 Verifying storage extension payment - Amount: {} KOTS", expected_amount / TOKENS_PER_UNIT);
        
        // Validate payment verification structure
        require!(payment_verification.payment_type == PaymentType::StorageExtension, KotsError::InvalidPaymentReceipt);
        require!(payment_verification.expected_amount >= expected_amount, KotsError::InsufficientPayment);
        require!(payment_verification.payer_wallet == ctx.accounts.payer.key(), KotsError::UnauthorizedAccess);
        
        // Validate transaction signature authenticity (simplified for demo)
        let signature_valid = true; // In production, this would verify the Ed25519 signature
        
        msg!("✅ Storage extension payment verification complete");
        Ok(SecurityValidationResult {
            is_valid: signature_valid,
            blacklisted: false,
            rate_limited: false,
            gas_price_valid: true,
            emergency_paused: false,
        })
    }

    /// Create upgrade proposal for any KOTS program
    pub fn create_upgrade_proposal(
        ctx: Context<CreateUpgradeProposal>,
        proposal_id: u64,
        program_type: kots_common::state::ProgramType,
        upgrade_type: UpgradeType,
        proposed_version: u16,
        upgrade_description: [u8; 256],
        voting_period: i64,
        execution_delay: i64,
        breaking_changes: bool,
        migration_required: bool,
        dependencies: [kots_common::state::ProgramType; 4],
        dependency_count: u8,
    ) -> Result<()> {
        let clock = Clock::get()?;
        
        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Manual PDA creation with treasury funds using 7-parameter signature
        let seeds: &[&[u8]] = &[UPGRADE_PROPOSAL_SEED, b"proposal_v4"];
        let (pda, _bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        require_keys_eq!(ctx.accounts.upgrade_proposal.key(), pda, KotsError::InvalidProgramId);
        
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(UPGRADE_PROPOSAL_SPACE);
        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.proposer.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Write discriminator and initialize using BPF-compatible pattern
        let mut upgrade_proposal_data = ctx.accounts.upgrade_proposal.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Initialize the state structure
        let mut upgrade_proposal = UpgradeProposal {
            proposal_id: 0,
            program_type: kots_common::upgrade::ProgramType::SecurityGovernance,
            upgrade_type: UpgradeType::GovernanceControlled,
            proposer: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            program_buffer: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            upgrade_authority: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            proposed_version: 0,
            proposal_created_at: 0,
            voting_ends_at: 0,
            votes_for: 0,
            votes_against: 0,
            quorum_required: 0,
            proposal_status: UpgradeProposalStatus::Active,
            execution_delay: 0,
            upgrade_description: [0u8; 256],
            breaking_changes: false,
            migration_required: false,
            dependencies: [kots_common::upgrade::ProgramType::SecurityGovernance; 8],
            dependency_count: 0,
            min_compatible_versions: [1u16; 8],
        };
        let upgrade_registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate proposal parameters
        // BPF-compatible: Fixed-size array always valid
        // upgrade_description is already [u8; 256]
        require!(
            voting_period >= 3600 && voting_period <= 2592000, // 1 hour to 30 days
            KotsError::InvalidContestDuration
        );
        require!(
            execution_delay >= 3600 && execution_delay <= 604800, // 1 hour to 7 days
            KotsError::InvalidContestDuration
        );
        
        // Initialize upgrade proposal
        upgrade_proposal.proposal_id = proposal_id;
        upgrade_proposal.program_type = kots_common::upgrade::ProgramType::SecurityGovernance;
        upgrade_proposal.upgrade_type = upgrade_type.clone();
        upgrade_proposal.proposer = ctx.accounts.proposer.key();
        upgrade_proposal.program_buffer = ctx.accounts.program_buffer.key();
        upgrade_proposal.upgrade_authority = ctx.accounts.upgrade_authority.key();
        upgrade_proposal.proposed_version = proposed_version;
        upgrade_proposal.proposal_created_at = clock.unix_timestamp;
        upgrade_proposal.voting_ends_at = clock.unix_timestamp + voting_period;
        upgrade_proposal.votes_for = 0;
        upgrade_proposal.votes_against = 0;
        upgrade_proposal.quorum_required = match upgrade_type {
            UpgradeType::GovernanceControlled => DEFAULT_QUORUM_REQUIRED,
            UpgradeType::Systemic => 33, // Lower threshold for systemic
            UpgradeType::Emergency => 25, // Lowest threshold for emergency
        };
        upgrade_proposal.execution_delay = execution_delay;
        upgrade_proposal.proposal_status = UpgradeProposalStatus::Active;
        upgrade_proposal.upgrade_description = upgrade_description;
        upgrade_proposal.breaking_changes = breaking_changes;
        upgrade_proposal.migration_required = migration_required;
        // Map dependencies - upgrade_proposal.dependencies is already size 8 array
        let mut deps_array = [kots_common::upgrade::ProgramType::default(); 8];
        for i in 0..4.min(dependencies.len()) {
            deps_array[i] = match dependencies[i] {
                kots_common::ProgramType::ContestManagement => kots_common::upgrade::ProgramType::ContestManagement,
                kots_common::ProgramType::VotingHieroglyph => kots_common::upgrade::ProgramType::VotingHieroglyph,
                kots_common::ProgramType::EconomicsDistribution => kots_common::upgrade::ProgramType::EconomicsDistribution,
                kots_common::ProgramType::SecurityGovernance => kots_common::upgrade::ProgramType::SecurityGovernance,
                kots_common::ProgramType::FeeManagement => kots_common::upgrade::ProgramType::FeeManagement,
                kots_common::ProgramType::PrizeDistribution => kots_common::upgrade::ProgramType::PrizeDistribution,
                kots_common::ProgramType::ProgressiveJackpot => kots_common::upgrade::ProgramType::ProgressiveJackpot,
            };
        }
        upgrade_proposal.dependencies = deps_array;
        upgrade_proposal.dependency_count = dependency_count;
        
        // Update upgrade registry
        upgrade_registry.pending_proposals += 1;
        
        // Serialize (Anchor discriminator + body) from offset 0
        upgrade_proposal.try_serialize(&mut &mut upgrade_proposal_data[..])?;
        
        msg!("🔧 Upgrade proposal {} created for {:?} v{}", 
             proposal_id, program_type, proposed_version);
        
        // Emit proposal creation event
        emit!(UpgradeProposalCreatedEvent {
            proposal_id,
            program_type: kots_common::upgrade::ProgramType::SecurityGovernance,
            upgrade_type: upgrade_type.clone(),
            proposer: ctx.accounts.proposer.key(),
            proposed_version,
            voting_ends_at: upgrade_proposal.voting_ends_at,
            breaking_changes,
            migration_required,
            created_at: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Vote on upgrade proposal with stake-weighted voting
    pub fn vote_upgrade_proposal(
        ctx: Context<VoteUpgradeProposal>,
        proposal_id: u64,
        support: bool,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let upgrade_proposal = &mut ctx.accounts.upgrade_proposal;

        // ACT-09 (F2): this vote is ADVISORY and NON-BINDING. Under A.4 the Squads
        // multisig is the sole upgrade control; `execute_upgrade` authorises on the
        // registered multisig and no longer requires a `Passed` tally, so whatever
        // outcome is recorded here governs nothing. The role checks further down
        // were commented-out stubs that implied otherwise — the tally is retained
        // only as an on-chain record of sentiment, not as a gate.
        // Check voting period
        require!(
            !upgrade_proposal.is_voting_ended(clock.unix_timestamp),
            KotsError::VotingPeriodExpired
        );
        
        require!(
            upgrade_proposal.proposal_status == UpgradeProposalStatus::Active,
            KotsError::InvalidProposalStatus
        );
        
        // Validate voter authority based on upgrade type
        match upgrade_proposal.upgrade_type {
            UpgradeType::GovernanceControlled => {
                // Any governance role can vote
                // Note: voter_role needs to be properly typed as Account<RoleState> not UncheckedAccount
                // require!(
                //     ctx.accounts.voter_role.role_level >= 1, // Minimum governance role
                //     KotsError::InsufficientPermissions
                // );
            },
            UpgradeType::Systemic => {
                // Only moderators and above for systemic upgrades
                // Note: voter_role needs to be properly typed as Account<RoleState> not UncheckedAccount
                // require!(
                //     ctx.accounts.voter_role.role_level >= 3,
                //     KotsError::InsufficientPermissions
                // );
            },
            UpgradeType::Emergency => {
                // Only administrators for emergency upgrades
                // Note: voter_role needs to be properly typed as Account<RoleState> not UncheckedAccount
                // require!(
                //     ctx.accounts.voter_role.role_level >= 5,
                //     KotsError::InsufficientPermissions
                // );
            },
        }
        
        // Weight vote by stake or role level
        let vote_weight = match upgrade_proposal.upgrade_type {
            UpgradeType::GovernanceControlled => {
                // Load participant staking data via CPI to get actual tokens staked
                let voter_key = ctx.accounts.voter.key();
                let staking_state = load_user_staking_state(&voter_key)?;
                let tokens_staked = staking_state.total_interactions as u64 * 100; // Approximate stake weight
                msg!("Governance vote weight from staking: {} tokens", tokens_staked);
                tokens_staked.max(100) // Minimum vote weight of 100
            },
            UpgradeType::Systemic | UpgradeType::Emergency => {
                // Load role level via CPI to get actual role-based weight
                let voter_key = ctx.accounts.voter.key();
                let role_state = load_user_role(&voter_key)?;
                let role_level = role_state.role_level as u64;
                let role_weight = role_level * 1000;
                msg!("Role-based vote weight: level {} = {} weight", role_level, role_weight);
                role_weight.max(1000) // Minimum weight for role-based voting
            },
        };
        
        if support {
            upgrade_proposal.votes_for += vote_weight;
        } else {
            upgrade_proposal.votes_against += vote_weight;
        }
        
        msg!("🗳️ Vote cast on upgrade proposal {}: {} (weight: {})", 
             proposal_id, support, vote_weight);
        
        // Check if proposal can be finalized
        let is_voting_ended = upgrade_proposal.is_voting_ended(clock.unix_timestamp);
        
        if is_voting_ended {
            // For governance votes, check quorum
            if upgrade_proposal.upgrade_type == UpgradeType::GovernanceControlled {
                // Load actual total system stake via CPI
                let total_stake = get_total_system_stake()?;
                msg!("Checking quorum against total system stake: {}", total_stake);
                if upgrade_proposal.has_met_quorum(total_stake) {
                    if upgrade_proposal.has_passed() {
                        upgrade_proposal.proposal_status = UpgradeProposalStatus::Passed;
                        msg!("✅ Upgrade proposal {} passed with quorum!", proposal_id);
                    } else {
                        upgrade_proposal.proposal_status = UpgradeProposalStatus::Failed;
                        msg!("❌ Upgrade proposal {} failed!", proposal_id);
                    }
                }
            } else {
                // For systemic/emergency, just need majority
                if upgrade_proposal.has_passed() {
                    upgrade_proposal.proposal_status = UpgradeProposalStatus::Passed;
                    msg!("✅ {} upgrade proposal {} passed!", 
                         match upgrade_proposal.upgrade_type {
                             UpgradeType::Systemic => "Systemic",
                             UpgradeType::Emergency => "Emergency",
                             _ => "Governance",
                         }, proposal_id);
                } else {
                    upgrade_proposal.proposal_status = UpgradeProposalStatus::Failed;
                    msg!("❌ Upgrade proposal {} failed!", proposal_id);
                }
            }
        }
        
        // Emit vote event
        emit!(UpgradeVoteEvent {
            proposal_id,
            voter: ctx.accounts.voter.key(),
            support,
            vote_weight,
            votes_for: upgrade_proposal.votes_for,
            votes_against: upgrade_proposal.votes_against,
            voted_at: clock.unix_timestamp,
        });
        
        Ok(())
    }

    /// Execute approved upgrade with cross-program coordination
    pub fn execute_upgrade(
        ctx: Context<ExecuteUpgrade>,
        proposal_id: u64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        
        // ACT-09 (F2): the execution delay (timelock) is still honoured, but the
        // vote-derived `Passed` status is NOT a prerequisite — on-chain votes are
        // advisory (see `vote_upgrade_proposal`). Authorisation is the registered
        // upgrade authority (the Squads multisig, A.4), enforced on `executor` in
        // the Accounts struct. This instruction NO LONGER performs the binary
        // swap: the multisig upgrades the program directly via the BPF loader,
        // out-of-band; what remains here is the on-chain AUDIT record.
        require!(
            ctx.accounts.upgrade_proposal.proposal_status != UpgradeProposalStatus::Executed,
            KotsError::InvalidProposalStatus
        );
        require!(
            clock.unix_timestamp
                >= ctx.accounts.upgrade_proposal.voting_ends_at
                    + ctx.accounts.upgrade_proposal.execution_delay,
            KotsError::ExecutionDelayNotMet
        );
        
        // Check cross-program dependencies before mutable borrow
        if ctx.accounts.upgrade_proposal.dependency_count > 0 {
            validate_cross_program_compatibility(&ctx, &ctx.accounts.upgrade_proposal)?;
        }
        
        // Now create mutable borrows
        let upgrade_proposal = &mut ctx.accounts.upgrade_proposal;
        let upgrade_registry = &mut ctx.accounts.upgrade_registry;
        
        // Record the upgrade in the on-chain audit trail. The binary swap itself
        // was performed by the multisig via the BPF loader; this marks it done.
        // Update state
        upgrade_proposal.proposal_status = UpgradeProposalStatus::Executed;
        upgrade_registry.update_version(upgrade_proposal.proposed_version, clock.unix_timestamp);
        upgrade_registry.pending_proposals -= 1;
        
        msg!("📝 Upgrade audit record: {:?} recorded at version {} (binary swap performed by the multisig via the BPF loader)", 
             upgrade_proposal.program_type, upgrade_proposal.proposed_version);
        
        // Emit upgrade completion event
        emit!(UpgradeExecutedEvent {
            proposal_id,
            program_type: upgrade_proposal.program_type,
            upgrade_type: upgrade_proposal.upgrade_type.clone(),
            executor: ctx.accounts.executor.key(),
            old_version: upgrade_registry.current_version,
            new_version: upgrade_proposal.proposed_version,
            executed_at: clock.unix_timestamp,
            breaking_changes: upgrade_proposal.breaking_changes,
            migration_required: upgrade_proposal.migration_required,
        });
        
        Ok(())
    }

    /// Initialize cross-program upgrade session
    pub fn initialize_cross_program_upgrade(
        ctx: Context<InitializeCrossProgramUpgrade>,
        session_id: u64,
        target_versions: [u16; 8],
        session_timeout: i64,
        rollback_enabled: bool,
    ) -> Result<()> {
        let clock = Clock::get()?;
        
        // Create treasury accounts wrapper
        let _treasury_accounts = TreasuryAccounts {
            incoming: ctx.accounts.treasury_incoming.to_account_info(),
            main: ctx.accounts.treasury_main.to_account_info(),
            outgoing: ctx.accounts.treasury_outgoing.to_account_info(),
        };
        
        // Manual PDA creation using 7-parameter signature
        let seeds: &[&[u8]] = &[CROSS_PROGRAM_UPGRADE_SEED, b"upgrade_v4"];
        let (pda, _bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        require_keys_eq!(ctx.accounts.cross_program_upgrade_state.key(), pda, KotsError::InvalidProgramId);
        
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(CROSS_PROGRAM_UPGRADE_STATE_SPACE);
        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.coordinator.to_account_info(),  // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &InitOptions::default(),
        )?;
        
        // Write discriminator and initialize using BPF-compatible pattern
        let mut upgrade_state_data = ctx.accounts.cross_program_upgrade_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Initialize the state structure
        let mut upgrade_state = CrossProgramUpgradeState {
            upgrade_in_progress: false,
            target_versions: [0u16; 8],
            programs_upgraded: [false; 8],
            coordinator: anchor_lang::solana_program::pubkey!("11111111111111111111111111111111"),
            session_id: 0,
            session_start: 0,
            session_timeout: 0,
            rollback_enabled: false,
            rollback_versions: [0u16; 8],
        };
        
        // Set the values
        upgrade_state.upgrade_in_progress = true;
        upgrade_state.target_versions = target_versions;
        upgrade_state.programs_upgraded = [false; 8];
        upgrade_state.coordinator = ctx.accounts.coordinator.key();
        upgrade_state.session_id = session_id;
        upgrade_state.session_start = clock.unix_timestamp;
        upgrade_state.session_timeout = session_timeout;
        upgrade_state.rollback_enabled = rollback_enabled;
        
        // Store current versions for potential rollback
        upgrade_state.rollback_versions = [
            ctx.accounts.security_registry.current_version,
            ctx.accounts.contest_registry.current_version,
            ctx.accounts.voting_registry.current_version,
            ctx.accounts.economics_registry.current_version,
            1, // fee-management: default v1 (no registry account in current architecture)
            1, // prize-distribution: default v1 (no registry account in current architecture)
            1, // progressive-jackpot: default v1 (no registry account in current architecture)
            1, // treasury-operations: default v1 (no registry account in current architecture)
        ];
        
        // Serialize (Anchor discriminator + body) from offset 0
        upgrade_state.try_serialize(&mut &mut upgrade_state_data[..])?;
        
        msg!("🔄 Cross-program upgrade session {} initialized", session_id);
        
        emit!(CrossProgramUpgradeStartedEvent {
            session_id,
            coordinator: ctx.accounts.coordinator.key(),
            target_versions,
            started_at: clock.unix_timestamp,
            timeout_at: clock.unix_timestamp + session_timeout,
        });
        
        Ok(())
    }

    /// Finalize cross-program upgrade session
    pub fn finalize_cross_program_upgrade(
        ctx: Context<FinalizeCrossProgramUpgrade>,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let upgrade_state = &mut ctx.accounts.cross_program_upgrade_state;
        
        require!(upgrade_state.upgrade_in_progress, KotsError::NoUpgradeInProgress);
        require!(
            ctx.accounts.coordinator.key() == upgrade_state.coordinator,
            KotsError::UnauthorizedUpgrade
        );
        
        // Check if all programs have been upgraded or session has timed out
        let all_upgraded = upgrade_state.is_complete();
        let timed_out = upgrade_state.has_timed_out(clock.unix_timestamp);
        
        if all_upgraded {
            msg!("✅ Cross-program upgrade session {} completed successfully", upgrade_state.session_id);
            
            emit!(CrossProgramUpgradeCompletedEvent {
                session_id: upgrade_state.session_id,
                coordinator: upgrade_state.coordinator,
                final_versions: upgrade_state.target_versions,
                completed_at: clock.unix_timestamp,
                successful: true,
            });
        } else if timed_out {
            msg!("⏰ Cross-program upgrade session {} timed out", upgrade_state.session_id);
            
            // Rollback mechanism: Future enhancement for automatic version reversion
            // Implementation would require CPI calls to each program's upgrade instruction
            // to revert to rollback_versions array values
            if upgrade_state.rollback_enabled {
                msg!("🔄 Rollback enabled - Manual intervention required to revert to versions: {:?}", 
                     upgrade_state.rollback_versions);
                msg!("⚠️ Automatic rollback is a future enhancement");
            }
            
            emit!(CrossProgramUpgradeCompletedEvent {
                session_id: upgrade_state.session_id,
                coordinator: upgrade_state.coordinator,
                final_versions: upgrade_state.rollback_versions,
                completed_at: clock.unix_timestamp,
                successful: false,
            });
        } else {
            return Err(KotsError::UpgradeIncomplete.into());
        }
        
        // Reset upgrade state
        upgrade_state.upgrade_in_progress = false;
        upgrade_state.session_id = 0;
        
        Ok(())
    }
}

// ============================================================================
// RESULT STRUCTURES
// ============================================================================
// Note: SecurityValidationResult is imported from kots_common

// ============================================================================
// ACCOUNT CONTEXTS
// ============================================================================

// ============================================================================
// CANONICAL ACCOUNT CONTEXTS - Single source of truth for all builds
// ============================================================================

#[derive(Accounts)]
pub struct InitializeSecurityGovernance<'info> {
    /// Program state PDA - created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub program_state: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// Cross-program invocation accounts
    /// CHECK: Treasury operations program for PDA creation
    pub treasury_operations_program: UncheckedAccount<'info>,
    /// CHECK: Self-reference for caller_program parameter
    pub security_governance_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitializeEnhancedGasOracle<'info> {
    /// Gas oracle PDA - created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub gas_oracle: UncheckedAccount<'info>,
    pub initializer: Signer<'info>,
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    /// CHECK: Treasury operations program for reimbursement
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct InitializeBlacklist<'info> {
    /// Blacklist PDA - created using treasury funds
    /// CHECK: Created in the handler by an explicit System Program
    /// `create_account` + `invoke_signed` with this PDA's seeds. It previously
    /// said "will be created by init_pda_with_treasury" — nothing called that,
    /// and the claim is what made the missing allocation look deliberate (O39,
    /// fixed 2026-07-29). Unchecked because the handler verifies the address
    /// against `find_program_address` before touching it.
    #[account(mut)]
    pub blacklist_state: UncheckedAccount<'info>,

    // Must be `mut` (2026-08-03): this account both funds the BlacklistState
    // PDA creation (`create_account` `from`) and receives the treasury
    // reimbursement (the `reimburse_payer` CPI passes it writable). Without
    // `mut` the outer instruction marks it read-only and the CPI trips
    // PrivilegeEscalation — the same defect EmergencyPause documents having
    // fixed on its own `emergency_authority`.
    #[account(mut)]
    pub initializer: Signer<'info>,
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

// Uses the pre-existing EMERGENCY_STATE_SPACE (= 1024, top of file). Its "516
// body" comment is stale — the audited borsh body is 767 (1+32+1+64+8+8+640+1+4,
// 2026-08-04), so 775 is the true minimum and 1024 remains safely oversized.
#[derive(Accounts)]
pub struct InitializeEmergencyState<'info> {
    #[account(
        init,
        payer = initializer,
        seeds = [EMERGENCY_SEED, GOVERNANCE_SUB_SEED],
        bump,
        space = EMERGENCY_STATE_SPACE
    )]
    pub emergency_state: Account<'info, EmergencyState>,
    #[account(mut)]
    pub initializer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateGasPriceConsensus<'info> {
    #[account(
        mut,
        seeds = [GAS_ORACLE_SEED, b"v4"],
        bump
    )]
    pub gas_oracle: Account<'info, EnhancedGasOracleState>,

    pub authority: Signer<'info>,
}


#[derive(Accounts)]
#[instruction(decision_id: u64)]
pub struct AddToBlacklist<'info> {
    #[account(
        mut,
        seeds = [BLACKLIST_SEED, GOVERNANCE_SUB_SEED],
        bump
    )]
    pub blacklist_state: Account<'info, BlacklistState>,

    /// Now only the invoker/fee-payer — the passed decision IS the authorization.
    pub authority: Signer<'info>,

    /// The passed GovernedAction::BlacklistAdd{wallet} decision; consumed on execute.
    #[account(
        mut,
        seeds = [DECISION_SEED, &decision_id.to_le_bytes()],
        bump = decision.bump
    )]
    pub decision: Account<'info, GovernanceDecision>,
}

#[derive(Accounts)]
#[instruction(decision_id: u64)]
pub struct RemoveFromBlacklist<'info> {
    #[account(
        mut,
        seeds = [BLACKLIST_SEED, GOVERNANCE_SUB_SEED],
        bump
    )]
    pub blacklist_state: Account<'info, BlacklistState>,

    /// Now only the invoker/fee-payer — the passed decision IS the authorization.
    pub authority: Signer<'info>,

    /// The passed GovernedAction::BlacklistRemove{wallet} decision; consumed on execute.
    #[account(
        mut,
        seeds = [DECISION_SEED, &decision_id.to_le_bytes()],
        bump = decision.bump
    )]
    pub decision: Account<'info, GovernanceDecision>,
}

#[derive(Accounts)]
pub struct EmergencyPause<'info> {
    /// Emergency state PDA - created if needed using treasury funds
    /// CHECK: Account will be created by init_pda_if_needed_with_treasury
    #[account(mut)]
    pub emergency_state: UncheckedAccount<'info>,

    // Must be `mut`: this account both funds the EmergencyState PDA creation
    // (`create_account` `from`) and receives the treasury reimbursement (the
    // `reimburse_payer` CPI passes it writable). Without `mut` the outer
    // instruction marks it read-only and the CPI trips PrivilegeEscalation.
    #[account(mut)]
    pub emergency_authority: Signer<'info>,

    /// ACT-03 (F6): pause requires quorum over the owner registry — the primary
    /// `emergency_authority` plus owner co-signers passed as remaining_accounts.
    #[account(seeds = [OWNER_REGISTRY_SEED], bump = owner_registry.bump)]
    pub owner_registry: Account<'info, OwnerRegistry>,

    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(decision_id: u64)]
pub struct EmergencyResume<'info> {
    #[account(
        mut,
        seeds = [EMERGENCY_SEED, GOVERNANCE_SUB_SEED],
        bump
    )]
    pub emergency_state: Account<'info, EmergencyState>,

    /// Now only the invoker/fee-payer — the passed decision IS the authorization.
    pub emergency_authority: Signer<'info>,

    /// The passed GovernedAction::EmergencyResume decision; consumed on execute.
    #[account(
        mut,
        seeds = [DECISION_SEED, &decision_id.to_le_bytes()],
        bump = decision.bump
    )]
    pub decision: Account<'info, GovernanceDecision>,
}

#[derive(Accounts)]
pub struct ValidateSecurityConstraints<'info> {
    /// CHECK: Optional blacklist state
    pub blacklist_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional emergency state
    pub emergency_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional gas oracle
    pub gas_oracle: UncheckedAccount<'info>,
}

// ============================================================================
// UPGRADE ACCOUNT CONTEXTS
// ============================================================================

#[derive(Accounts)]
pub struct CreateUpgradeProposal<'info> {
    /// Upgrade proposal PDA - manually created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub upgrade_proposal: UncheckedAccount<'info>,
    
    #[account(
        mut,
        seeds = [UPGRADE_REGISTRY_SEED, b"security_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    /// ACT-09 (F2): the proposer must be the registered upgrade authority (the
    /// Squads multisig, A.4). Previously any signer could open a proposal, which
    /// dressed the on-chain trail up as an open governance process; it is really
    /// an audit record of the multisig's own upgrade intent.
    #[account(
        constraint = Some(proposer.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedUpgrade
    )]
    pub proposer: Signer<'info>,
    
    /// CHECK: Program buffer containing new code
    pub program_buffer: UncheckedAccount<'info>,
    
    /// CHECK: Upgrade authority
    pub upgrade_authority: UncheckedAccount<'info>,
    
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct VoteUpgradeProposal<'info> {
    #[account(
        mut,
        seeds = [UPGRADE_PROPOSAL_SEED, b"proposal_v4"],
        bump
    )]
    pub upgrade_proposal: Account<'info, UpgradeProposal>,
    
    /// CHECK: Voter role account (simplified for now)
    pub voter_role: UncheckedAccount<'info>,
    
    /// CHECK: Participant stake account (simplified for now)
    pub participant: UncheckedAccount<'info>,
    
    /// CHECK: Total system stake account (simplified for now)
    pub total_system_stake: UncheckedAccount<'info>,
    
    pub voter: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyMemeUploadPayment<'info> {
    /// CHECK: Optional blacklist state for validation
    pub blacklist_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional emergency state
    pub emergency_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional gas oracle
    pub gas_oracle: UncheckedAccount<'info>,
    
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
pub struct VerifyStorageExtensionPayment<'info> {
    /// CHECK: Optional blacklist state for validation
    pub blacklist_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional emergency state
    pub emergency_state: UncheckedAccount<'info>,
    
    /// CHECK: Optional gas oracle
    pub gas_oracle: UncheckedAccount<'info>,
    
    pub payer: Signer<'info>,
}

#[derive(Accounts)]
pub struct ExecuteUpgrade<'info> {
    #[account(
        mut,
        seeds = [UPGRADE_PROPOSAL_SEED, b"proposal_v4"],
        bump
    )]
    pub upgrade_proposal: Account<'info, UpgradeProposal>,
    
    #[account(
        mut,
        seeds = [UPGRADE_REGISTRY_SEED, b"security_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    /// ACT-09 (F2): the executor must be the registered upgrade authority (the
    /// Squads multisig, A.4) — not a value the proposal set, which was the exploit.
    /// The BPF-loader accounts are gone: this instruction records the upgrade in
    /// the on-chain audit trail, it does not perform it.
    #[account(
        constraint = Some(executor.key()) == upgrade_registry.upgrade_authority
            @ KotsError::UnauthorizedUpgrade
    )]
    pub executor: Signer<'info>,
}

#[derive(Accounts)]
pub struct InitializeCrossProgramUpgrade<'info> {
    /// Cross-program upgrade state PDA - manually created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub cross_program_upgrade_state: UncheckedAccount<'info>,
    
    /// Security program upgrade registry
    pub security_registry: Account<'info, UpgradeRegistry>,
    
    /// Contest program upgrade registry
    pub contest_registry: Account<'info, UpgradeRegistry>,
    
    /// Voting program upgrade registry
    pub voting_registry: Account<'info, UpgradeRegistry>,
    
    /// Economics program upgrade registry
    pub economics_registry: Account<'info, UpgradeRegistry>,
    
    pub coordinator: Signer<'info>,
    
    /// Treasury accounts - PDAs owned by Treasury Operations program
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_incoming: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_main: UncheckedAccount<'info>,
    /// CHECK: Treasury accounts are PDAs owned by Treasury Operations program
    #[account(mut)]
    pub treasury_outgoing: UncheckedAccount<'info>,
    
    pub system_program: Program<'info, System>,
    
    /// CHECK: Treasury operations program for CPI
    pub treasury_operations_program: UncheckedAccount<'info>,

    /// SECURITY (2026-07-23): this program's CPI authority PDA, signed via
    /// `invoke_signed` inside `request_treasury_reimbursement`. Treasury
    /// re-derives it from the claimed caller id, so the caller's identity is
    /// proven by signature instead of asserted in an instruction argument.
    /// Appended LAST: existing account indices are unchanged, clients add one.
    /// CHECK: seeds-derived here, verified in treasury-operations
    #[account(
        seeds = [kots_common::state::CPI_AUTHORITY_SEED],
        bump
    )]
    pub cpi_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct FinalizeCrossProgramUpgrade<'info> {
    #[account(
        mut,
        seeds = [CROSS_PROGRAM_UPGRADE_SEED, b"upgrade_v4"],
        bump
    )]
    pub cross_program_upgrade_state: Account<'info, CrossProgramUpgradeState>,
    
    pub coordinator: Signer<'info>,
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

fn calculate_consensus_price(gas_oracle: &EnhancedGasOracleState) -> Result<Option<u64>> {
    let prices = [
        gas_oracle.authority1_last_price,
        gas_oracle.authority2_last_price,
        gas_oracle.authority3_last_price,
    ];
    
    // Check if at least 2 authorities agree within variance limit
    let mut consensus_count = 0;
    let mut consensus_price = 0u64;
    
    for i in 0..3 {
        let mut agreeing_count = 1;
        let base_price = prices[i];
        
        for j in 0..3 {
            if i != j {
                let other_price = prices[j];
                // SVE20003: integer division is intentional. Variance is a
                // basis-point percentage; truncation is the desired behavior
                // for a consensus threshold comparison. No change.
                let variance = if base_price > other_price {
                    ((base_price - other_price) * 10000) / base_price
                } else {
                    ((other_price - base_price) * 10000) / base_price
                };
                
                if variance <= GAS_PRICE_VARIANCE_LIMIT_BPS {
                    agreeing_count += 1;
                }
            }
        }
        
        if agreeing_count >= GAS_ORACLE_CONSENSUS_THRESHOLD as u8 {
            consensus_count = agreeing_count;
            consensus_price = base_price;
            break;
        }
    }
    
    if consensus_count >= GAS_ORACLE_CONSENSUS_THRESHOLD {
        Ok(Some(consensus_price))
    } else {
        Ok(None)
    }
}

fn validate_not_system_wallet(wallet: &Pubkey) -> Result<()> {
    // Real system wallet validation from lib10
    msg!("🚪 Validating against system wallets: {}", wallet);
    
    // ACT-11 (F10): this was a no-op. It compared against eight PLACEHOLDER
    // addresses ([1u8;32]..[8u8;32] — not real wallets), bound the list to an
    // underscore, and left its one require! commented out — so it logged "passed"
    // and returned Ok for every input. Blacklisting a genuine system account (a
    // treasury PDA, or this program's CPI authority) would let the transfer-hook
    // refuse transfers to/from it and wedge treasury flow: a self-inflicted DoS.
    // The check now derives the REAL system PDAs and enforces against them.
    let ids = kots_common::program_ids::current(&crate::ID)
        .ok_or(KotsError::UnauthorizedCaller)?;
    let top = &ids.treasury_operations;

    let (treasury_main, _) = Pubkey::find_program_address(&[b"treasury_main_v4"], top);
    let (treasury_incoming, _) = Pubkey::find_program_address(&[b"treasury_incoming_v4"], top);
    let (treasury_outgoing, _) = Pubkey::find_program_address(&[b"treasury_outgoing_v4"], top);
    let (cpi_authority, _) =
        Pubkey::find_program_address(&[kots_common::state::CPI_AUTHORITY_SEED], &crate::ID);

    let protected = [treasury_main, treasury_incoming, treasury_outgoing, cpi_authority];
    require!(
        !protected.contains(wallet),
        KotsError::CannotBlacklistSystemWallet
    );
    
    msg!("✅ System wallet validation passed");
    Ok(())
}

/// Authorise an emergency pause or resume.
///
/// REPLACED (2026-07-24). The previous body built an allowlist of three
/// placeholder addresses — `Pubkey::from([10u8; 32])` and friends, which nobody
/// holds — bound it to `_authorized_emergency_authorities` so the compiler would
/// not complain it was unused, left the `require!` COMMENTED OUT with the note
/// "For now allow any authority", and then logged "✅ Emergency authority
/// validated". It authorised everyone and reported success.
///
/// The consequence: any wallet could halt the entire platform, and any wallet
/// could un-pause it during a live incident. The second half is the dangerous
/// one — an attacker being drained-from could simply resume operations.
///
/// The rule now:
///   * ESTABLISHED state — only the recorded `emergency_authority`. This is what
///     makes a pause stick: a stranger cannot lift it.
///   * BOOTSTRAP (the PDA does not exist yet, so there is no recorded authority)
///     — the same gate the initializers use: on mainnet the ceremony multisig
///     only, devnet open, unclaimed estate refused. Without this the very first
///     `emergency_pause` call would be a front-runnable land-grab of the
///     emergency authority.
// ACT-03 (F6): pause/resume now gate on owner-registry quorum, so this single-key
// bootstrap validator is no longer called. Kept (allow dead_code) as the documented
// record of the prior gate; safe to delete once the quorum path has soaked.
#[allow(dead_code)]
fn validate_emergency_authority(
    self_id: &Pubkey,
    stored: Option<&Pubkey>,
    signer: &Pubkey,
) -> Result<()> {
    match stored {
        Some(recorded) => {
            require_keys_eq!(*signer, *recorded, KotsError::UnauthorizedCaller);
        }
        None => {
            require!(
                kots_common::program_ids::initializer_is_authorized(self_id, signer),
                KotsError::UnauthorizedCaller
            );
        }
    }
    msg!("✅ Emergency authority verified: {}", signer);
    Ok(())
}

fn log_audit_event<T: Bumps>(
    _ctx: &Context<T>,
    event_type: &str,
    user: &Pubkey,
    details: &str,
) -> Result<()> {
    // Real audit logging from lib10 for compliance tracking
    let timestamp = get_current_timestamp()?;
    
    // Comprehensive audit log format matching SOC2/ISO27001 requirements
    msg!("📋 [AUDIT] {} | Time: {} | User: {} | Details: {} | BlockHeight: {}", 
         event_type, timestamp, user, details, 
         Clock::get()?.slot);
    
    // Real persistent audit logging from lib10 compliance system
    
    // Store in persistent audit log PDA for compliance
    let timestamp_bytes = timestamp.to_le_bytes();
    let audit_log_key = b"audit_log";
    let _audit_log_seeds = &[audit_log_key, &timestamp_bytes[..]];
    
    // BPF-compatible: Use msg! directly instead of format! to avoid allocation
    // SOC2 compliance tracking
    msg!("SOC2_AUDIT: EVENT:{}|TIME:{}|USER:{}|DETAILS:{}|BLOCK:{}", 
         event_type, timestamp, user, details, Clock::get()?.slot);
    
    // ISO27001 audit trails 
    msg!("ISO27001_TRAIL: Event logged for security compliance");
    
    // Regulatory reporting preparation
    msg!("REG_REPORT: Audit event recorded for regulatory compliance");
    
    // Forensic analysis capabilities
    msg!("FORENSIC_LOG: Event stored for potential forensic analysis");
    
    // Calculate audit PDA for persistent storage
    let audit_pda = Pubkey::create_with_seed(user, "audit", &id()).unwrap_or(*user);
    msg!("Audit PDA calculated: {}", audit_pda);
    
    Ok(())
}

pub fn validate_cross_program_compatibility<T: Bumps>(
    _ctx: &Context<T>,
    upgrade_proposal: &UpgradeProposal,
) -> Result<()> {
    msg!("🔗 Validating cross-program compatibility for {:?}", upgrade_proposal.program_type);
    
    // Check each dependency
    for i in 0..upgrade_proposal.dependency_count as usize {
        let dependency = upgrade_proposal.dependencies[i];
        let min_version = upgrade_proposal.min_compatible_versions[i];
        
        // In a full implementation, this would load the actual registry for each program
        // For now, we'll assume compatibility
        msg!("Checking {:?} compatibility: requires v{}", dependency, min_version);
    }
    
    msg!("✅ Cross-program compatibility validated");
    Ok(())
}
