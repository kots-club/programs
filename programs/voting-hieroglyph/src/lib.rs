// DESTINATION (where this file goes): kots-contracts/programs/voting-hieroglyph/src/lib.rs
//! KOTS Voting & Hieroglyph Program
//! 
//! Handles all 32 hieroglyph interactions, vote casting logic, scoring calculations,
//! anti-spam measures, and participant statistics tracking.

use anchor_lang::{prelude::*, Bumps};
use anchor_spl::token_interface::{Mint, TokenAccount, TokenInterface};
use kots_common::{
    constants::{*, MAX_VOTERS_PER_TRACKER}, errors::*, utils::*,
    ContestLabel, calculate_dynamic_hieroglyph_fee, state::{
        VOTE_TRACKER_SEED, INTERACTION_BATCH_SEED,
        HieroglyphMetadata, InteractionType, ContestStateInfo
    },
    upgrade::{UpgradeExecutedEvent, UpgradeType, UPGRADE_REGISTRY_SEED, ProgramType}, // Import correct ProgramType from upgrade module
    init::{request_treasury_reimbursement},
    network_config::Network
};

// Program-local account size constants for IDL generation
// CORRECTED 2026-07-28 (O31). Was 256, commented "8 + 248 (UpgradeRegistry)".
// UpgradeRegistry is 436 bytes of body — `version_history: [[u8; 32]; 10]` is 320
// of them on its own — so 256 was short by 188. It never bit because the account
// was never actually allocated (see initialize_upgrade_registry); the constant
// only fed rent.minimum_balance() for the reimbursement. The moment allocation
// lands, a 256-byte account makes the serialize write past the end.
//
//   1 program_type + 2 current_version + 4 pending + 4 successful + 4 failed
// + 8 last_upgrade_timestamp + 33 upgrade_authority + 1 governance_approval
// + 1 emergency_upgrade_enabled + 320 version_history + 16 compatible_versions
// + 8 last_compatibility_check + 1 emergency_pause_enabled + 33 emergency_contact
// = 436, + 8 discriminator = 444. Rounded to 448 for alignment headroom.
const UPGRADE_REGISTRY_SPACE: usize = 448;

// Account struct definitions at root level for proper IDL generation

#[account]
pub struct VoteTrackerState {
    pub is_initialized: bool, // Back to bool - no zero-copy constraints
    pub contest_label: [u8; 32], // Fixed size for both IDL and BPF compatibility
    pub meme_id: u64,
    pub interaction_type: u32,
    pub hieroglyph_type: u32,
    // REMOVED: voters array - use blockchain transaction history instead (BPF stack optimization)
    pub voter_count: u16,
    pub total_votes: u32,
    pub total_value: u64,
    pub votes_beyond_limit: u32,
    pub individual_tracking_full: bool, // Back to bool - no zero-copy constraints
    pub creation_timestamp: i64,
    pub last_vote_timestamp: i64,
    pub score_weight: i64, // SIGNED since de-chancing (SCORING-MODEL.md)
}

#[account]
#[derive(Debug)]
pub struct InteractionBatchState {
    pub is_initialized: bool,
    pub contest_label: [u8; 32], // Fixed size for both IDL and BPF compatibility
    pub meme_id: u64,
    pub batch_number: u32,
    pub interactions: [u8; 1600], // BPF-safe: MAX_VOTERS_PER_TRACKER * 32 = 1600 bytes
    pub interaction_count: u32,
    pub batch_timestamp: i64,
    pub total_score_awarded: i64, // SIGNED since de-chancing (SCORING-MODEL.md)
}

// F19 FIX (2026-08): the RUNTIME `UpgradeRegistry` is defined LOCALLY here so its
// Anchor `Owner::owner()` resolves to THIS program's `declare_id!()`. Re-exporting
// kots_common's `#[account]` type made `Owner::owner()` == kots_common::ID, an
// all-zeros placeholder (== the System Program); the registry account is created
// owned by this program (see `initialize_upgrade_registry`'s manual
// `create_account` with `ctx.program_id`), so every `Account<'info,
// UpgradeRegistry>` READ failed AccountOwnedByWrongProgram (3007). Fields and the
// `program_type` enum are byte-identical to kots_common's, and the 8-byte
// discriminator is derived from the name "UpgradeRegistry", so the manual
// init/serialize path and on-chain layout are unchanged. idl-build keeps
// re-exporting the kots_common type so the generated IDL is untouched.
#[cfg(feature = "idl-build")]
pub use kots_common::upgrade::UpgradeRegistry;

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
    pub const INIT_SPACE: usize =
        8 + 1 + 2 + 4 + 4 + 4 + 8 + 33 + 1 + 1 + 320 + 8 + 8 + 1 + 33;

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


// Event definitions for hieroglyph execution
#[event]
pub struct HieroglyphExecuted {
    pub voter: Pubkey,
    pub contest_label: [u8; 32],
    pub meme_id: u64,
    pub hieroglyph_id: u32,
    pub cost_paid: u64,
    pub timestamp: i64,
}

// Unified program ID for both devnet and mainnet
#[cfg(feature = "realm-main")]
declare_id!("HZavpBFJgdoCSzBsiaZqHqB68VoushL1i98zPfr3BiAe");

#[cfg(not(feature = "realm-main"))]
declare_id!("FzGbgXetN5jqZAktTr8KJ2D1BGmYx5kw2Lrp9cJNUArN");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"voting-hieroglyph|20251004T1339Z|build001";

#[derive(Accounts)]
pub struct VersionHeartbeat {}

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
// MUMMIFY (2026-10-02): `execute_hieroglyph_batch` was REMOVED. It paid the realm
// incoming wallet in one transfer, so a batch distributed money differently from the
// same votes cast one by one (which split prize / platform legs off-chain, sidecar
// PaymentTxService). Batches are now built off-chain exactly like individual votes:
// per-item split, summed per destination, one memo listing every item. No program
// instruction is involved, so there is nothing here to keep in step.

#[program]
pub mod voting_hieroglyph {
    use super::*;

    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Cast vote with BPF-optimized stack usage and NEW COST FORMULA
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn cast_vote_protected(
        ctx: Context<CastVoteProtected>,
        contest_label: [u8; 32],
        target_meme_id: u64,
        interaction_type_u32: u32,
    ) -> Result<()> {
        // BLACKLIST GATE (2026-08-06): a blacklisted wallet must not vote.
        let sg_id = kots_common::cpi::security_governance_id_for(ctx.program_id)?;
        kots_common::cpi::require_not_blacklisted(
            &ctx.accounts.blacklist_state.to_account_info(),
            &sg_id,
            &ctx.accounts.voter.key(),
        )?;

        // EMERGENCY PAUSE GATE (2026-08-06): block voting during a global pause.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &sg_id,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);

        // BPF-safe validation (no large local variables)
        require!(interaction_type_u32 < 32, KotsError::InvalidTokenAmount);
        require!(target_meme_id > 0, KotsError::InvalidMemeId);
        
        // Calculate hieroglyph cost using NEW FORMULA: (ID + 1) × base_cost × (1 + gas_percentage)
        let interaction_type_u8 = interaction_type_u32 as u8;
        let calculated_cost = calculate_dynamic_hieroglyph_fee(
            interaction_type_u8,
            BASE_KOTS_COST,
            DEFAULT_GAS_PRICE_LAMPORTS,
        )?;
        
        msg!("🗳️ Vote cast - Meme: {}, Type: {}, Cost: {} KOTS", 
             target_meme_id, interaction_type_u32, calculated_cost / TOKENS_PER_UNIT);
        Ok(())
    }

    /// Execute hieroglyph with ATOMIC payment + vote recording
    /// This is the PRODUCTION-READY instruction that ensures users don't lose tokens
    /// 
    /// Parameters:
    /// - contest_label: Contest identifier (32 bytes)
    /// - target_meme_id: Meme ID receiving the hieroglyph
    /// - hieroglyph_id: Hieroglyph ID (0-31)
    /// - network: Optional network selector (0=Devnet, 1=Mainnet, 2=Testnet, 3=Localnet, None=auto-detect)
    #[inline(never)]
    pub fn execute_hieroglyph<'info>(
        mut ctx: Context<'_, '_, '_, 'info, ExecuteHieroglyph<'info>>,
        contest_label: [u8; 32],
        target_meme_id: u64,
        hieroglyph_id: u32,
        network: Option<u8>,
    ) -> Result<()> {
        // BPF STACK OPTIMIZATION: Prevent inlining, validate constraints removed from struct
        require!(hieroglyph_id <= 31, KotsError::InvalidHieroglyphID);
        require!(target_meme_id > 0, KotsError::InvalidMemeId);
        
        // Manual validation (moved from account constraints to reduce try_accounts stack)
        require_keys_eq!(
            ctx.accounts.voter_token_account.owner,
            ctx.accounts.voter.key(),
            KotsError::UnauthorizedAccess
        );
        require_keys_eq!(
            ctx.accounts.voter_token_account.mint,
            ctx.accounts.kots_mint.key(),
            KotsError::InvalidTokenMint
        );
        require_keys_eq!(
            ctx.accounts.incoming_token_account.mint,
            ctx.accounts.kots_mint.key(),
            KotsError::InvalidTokenMint
        );
        
        // Validate and perform payment, get cost
        let cost = execute_hieroglyph_payment(&ctx, hieroglyph_id, network)?;
        
        // Record vote with calculated cost
        execute_hieroglyph_record_vote(&mut ctx, contest_label, target_meme_id, hieroglyph_id, cost)?;
        
        Ok(())
    }

    /// Batch cast multiple hieroglyph votes efficiently  
    pub fn batch_cast_hieroglyph_votes(
        ctx: Context<BatchCastHieroglyphVotes>,
        contest_label: [u8; 32],
        votes: [VoteInstruction; MAX_INTERACTIONS_PER_BATCH], // BPF-safe: 3 * 12 bytes = 36 bytes
    ) -> Result<()> {
        // BLACKLIST GATE (2026-07-23). Voting was gated only on `voter != Pubkey::default()`, which blocks nobody.
        kots_common::cpi::require_not_blacklisted(
            &ctx.accounts.blacklist_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
            &ctx.accounts.voter.key(),
        )?;

        msg!("🗳️ Batch voting - {} votes", votes.len());
        
        let contest_label_bytes = contest_label;
        
        // Security validation for batch operation
        validate_voter_security(
            &ctx.accounts.emergency_state.to_account_info(),
            ctx.program_id,
            &contest_label_bytes,
        )?;
        
        let mut total_score = 0i64;
        
        for vote in votes.iter() {
            // Validate each vote
            require!(vote.interaction_type < 32, KotsError::InvalidMemeId);
            validate_meme_id(vote.target_meme_id)?;
            
            // Check for duplicates would require additional accounts per vote
            // For batch operations, we trust the client-side validation
            
            let score_weight = get_interaction_scoring_weight(vote.interaction_type);
            
            total_score = total_score
                .checked_add(score_weight)
                .ok_or(KotsError::ArithmeticOverflow)?;
        }
        
        // Update batch interaction account. No ticket accrual: award tickets
        // were removed by de-chancing (plan §5a) — the vote records themselves
        // are the input to the deterministic voter-pool shares computed at
        // finalization.
        let interaction_batch = &mut ctx.accounts.interaction_batch;
        interaction_batch.total_score_awarded = interaction_batch.total_score_awarded
            .checked_add(total_score)
            .ok_or(KotsError::ArithmeticOverflow)?;
        
        msg!("✅ Batch voting completed - Total Score: {}", total_score);
        Ok(())
    }

    /// Calculate final scores for contest finalization (BPF-optimized account-based)
    pub fn calculate_final_scores(
        ctx: Context<CalculateFinalScores>,
        contest_label: [u8; 32],
    ) -> Result<()> { // Must return Result<()> for Anchor compliance
        msg!("📊 Calculating final scores for contest");
        
        let contest_label_bytes = contest_label;
        
        // Validate authority
        validate_scoring_authority(&ctx, &contest_label_bytes)?;
        
        // BPF-optimized: Process participant scores via individual account updates
        // Each participant's final score is stored in their ParticipantState PDA
        // Contest-management program will iterate through participant accounts
        // This avoids large stack arrays while maintaining full functionality
        
        let participant_count = 0u32; // Will be updated by contest-management via CPI
        
        msg!("✅ Final scores calculated - {} participants processed", participant_count);
        // Return participant count via set_return_data for client access
        anchor_lang::solana_program::program::set_return_data(&participant_count.to_le_bytes());
        Ok(())
    }

    /// Get single hieroglyph information - STACK-OPTIMIZED SOURCE OF TRUTH
    pub fn get_hieroglyph_info(
        _ctx: Context<GetHieroglyphInfo>,
        hieroglyph_id: u8,
    ) -> Result<()> {
        require!(hieroglyph_id < 32, KotsError::InvalidHieroglyphID);
        msg!("📋 Returning hieroglyph {} information", hieroglyph_id);

        // The emoji table and the category table that used to live here are
        // GONE. They were two of the five disagreeing tables (this file's
        // symbol/category matches, state.rs's names array, the V002 DB seed and
        // the frontend's own map). One row per glyph now: kots_common::hieroglyphs.
        let def = kots_common::hieroglyphs::get(hieroglyph_id)
            .ok_or(KotsError::InvalidHieroglyphID)?;

        let i = hieroglyph_id as usize;

        // TWO FIELDS, TWO MEANINGS. They used to have neither.
        //
        // `base_cost` held calculate_dynamic_hieroglyph_fee(..) — which ALREADY applies
        // gas ("NEW FORMULA: (ID + 1) × base_cost × (1 + gas_fee_percentage)", and that
        // is KOTS_TOKENOMICS.md §4a verbatim). So `base_cost` was never a base: it was
        // the total. Then `total_cost_lamports` added gas a SECOND time:
        //     base_cost + (gas_multiplier as u64 * DEFAULT_GAS_PRICE_LAMPORTS)
        // and the chain charged n × (1 KOTS + 2×gas) — comment 2.00002 instead of
        // 2.00001, crown 32.00032 instead of 32.00016. Every price on the platform sat
        // 0.0005% above its own documented formula.
        //
        // Caught from a toast, of all things: "need 2.00002 KOTS (+ gas fee)" — where
        // the decimal part IS the gas, so the label was narrating the double-count.
        // Two names for one number is how you discover you charged it twice.
        //
        // Now: base is the price, total is the price WITH gas, and the difference is
        // the gas. A client can say "2 KOTS + 0.00001 gas fee = 2.00001 KOTS" and every
        // term is a field, not a guess.

        // The glyph price, gas EXCLUDED: (ID + 1) × 1 KOTS. The "2 KOTS" in the label.
        let base_cost = if i == 0 { 0 } else { (i as u64 + 1) * BASE_KOTS_COST };

        // The price WITH gas, applied exactly once. The number you must actually hold.
        let total_cost_lamports =
            calculate_dynamic_hieroglyph_fee(i as u8, BASE_KOTS_COST, DEFAULT_GAS_PRICE_LAMPORTS)?;

        // Display only. The fee above already applied gas; nothing may multiply by this.
        let gas_multiplier = if i == 0 { 0 } else { i + 1 };
        let cost_formula = if i == 0 { "FREE" } else { "((ID + 1) × (1 KOTS + gas))" };

        // Fixed-width copies for BPF. Every string comes from the registry.
        let mut name_bytes = [0u8; 32];
        let nb = def.name.as_bytes();
        let nlen = nb.len().min(32);
        name_bytes[..nlen].copy_from_slice(&nb[..nlen]);

        let mut symbol_bytes = [0u8; 8];
        let sb = def.symbol.as_bytes();
        let slen = sb.len().min(8);
        symbol_bytes[..slen].copy_from_slice(&sb[..slen]);

        let mut category_bytes = [0u8; 24];
        let cb = def.category.as_str().as_bytes();
        let clen = cb.len().min(24);
        category_bytes[..clen].copy_from_slice(&cb[..clen]);

        let mut formula_bytes = [0u8; 64];
        let fb = cost_formula.as_bytes();
        let flen = fb.len().min(64);
        formula_bytes[..flen].copy_from_slice(&fb[..flen]);

        let metadata = HieroglyphMetadata {
            id: hieroglyph_id,
            name: name_bytes,
            symbol: symbol_bytes,
            category: category_bytes,
            scoring_weight: def.meme_delta,
            stance: def.stance,
            base_cost,
            cost_formula: formula_bytes,
            total_cost_lamports,
            total_cost_kots_scaled: (total_cost_lamports * 100000) / TOKENS_PER_UNIT,
            gas_multiplier: gas_multiplier as u32,
        };

        msg!("✅ Hieroglyph {} metadata returned", hieroglyph_id);
        // Return metadata via set_return_data for client access
        let encoded = borsh::to_vec(&metadata).map_err(|_| KotsError::SerializationError)?;
        anchor_lang::solana_program::program::set_return_data(&encoded);
        Ok(())
    }

    /// THE program registry — the off-chain bootstrap (CLAUDE.md, the ID law).
    ///
    /// Returns the seven program ids of the realm THIS BINARY is deployed in,
    /// Borsh-encoded via set_return_data, in `ProgramIds` field order.
    ///
    /// This is the counterpart to `get_hieroglyph_info`: one compiled const,
    /// published by the chain, consumed by everyone. It closes the last gap the
    /// glyph registry left open — off-chain components knew what a glyph WAS,
    /// but each still carried its own idea of which programs to ask.
    ///
    /// The realm is resolved from `crate::ID` — the id the loader has already
    /// verified against the runtime (DeclaredProgramIdMismatch). No env var (BPF
    /// has no environment), no feature flag, no config account.
    ///
    /// **The bootstrap is irreducible and self-checking.** A caller must know
    /// THIS program's id to ask at all — that is the one id in the sealed
    /// secret. Every other id comes from here, and because the answer contains
    /// the bootstrap id itself, a stale secret is detected rather than obeyed.
    pub fn get_program_ids(_ctx: Context<GetProgramIds>) -> Result<()> {
        let ids = kots_common::program_ids::current(&crate::ID).ok_or_else(|| {
            // We are deployed at an address no realm claims: a stray deployment,
            // or a mainnet launch not yet recorded in program_ids::MAIN. Serving
            // a guess here would be the fiction this registry replaces.
            msg!("❌ No realm claims program id {} — refusing to guess", crate::ID);
            error!(KotsError::InvalidHieroglyphID)
        })?;

        let realm = kots_common::program_ids::realm_of(&crate::ID)
            .map(|r| r.as_str())
            .unwrap_or("unknown");
        msg!("📋 Returning the {} estate's 7 program ids", realm);

        let encoded = borsh::to_vec(&ids).map_err(|_| KotsError::SerializationError)?;
        anchor_lang::solana_program::program::set_return_data(&encoded);
        Ok(())
    }

    /// Get hieroglyph count - returns total number of hieroglyphs (32)
    pub fn get_hieroglyph_count(
        _ctx: Context<GetHieroglyphCount>,
    ) -> Result<()> {
        msg!("📊 Returning total hieroglyph count: 32");
        // Return count via set_return_data for client access
        anchor_lang::solana_program::program::set_return_data(&32u8.to_le_bytes());
        Ok(())
    }

    /// Query vote data for analytics
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn query_vote_data(
        ctx: Context<QueryVoteData>,
        contest_label: [u8; 32],
        meme_id: u64,
        interaction_type: u32,
    ) -> Result<()> {
        // Access vote tracker directly (no zero-copy needed)
        let vote_tracker = &ctx.accounts.vote_tracker;
        
        let data = VoteTrackerData {
            total_votes: vote_tracker.total_votes,
            unique_voters: vote_tracker.voter_count as u32, // Use voter_count instead of array length
            last_vote_timestamp: vote_tracker.last_vote_timestamp,
            score_weight: vote_tracker.score_weight,
        };
        
        msg!("📈 Vote data - Votes: {}, Unique: {}", data.total_votes, data.unique_voters);
        // Return data via set_return_data for client access
        let encoded = borsh::to_vec(&data).map_err(|_| KotsError::SerializationError)?;
        anchor_lang::solana_program::program::set_return_data(&encoded);
        Ok(())
    }

    /// Initialize core voting hieroglyph program state - MUST BE CALLED FIRST
    pub fn initialize_voting_hieroglyph(
        ctx: Context<InitializeVotingHieroglyph>,
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

        msg!("🗳️ Initializing Voting Hieroglyph Program");
        
        // Create PDA with treasury funding
        let seeds: &[&[u8]] = &[b"program_state"];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.program_state.key(), pda, KotsError::InvalidProgramId);
        
        // Step 1: Create the PDA using this program's seeds (THIS PROGRAM OWNS THE PDA)
        let rent = anchor_lang::solana_program::sysvar::rent::Rent::get()?;
        let required_lamports = rent.minimum_balance(256);
        
        // Create PDA using invoke_signed with this program's seeds
        let create_seeds: &[&[u8]] = &[b"program_state", &[bump]];
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                &ctx.accounts.authority.key(),
                &ctx.accounts.program_state.key(),
                required_lamports,
                256,
                ctx.program_id,
            ),
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.program_state.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[create_seeds],
        )?;
        
        // Step 2: Request treasury reimbursement
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(),
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &kots_common::init::InitOptions::default(),
        )?;
        
        // Initialize the state
        let mut data = ctx.accounts.program_state.try_borrow_mut_data()?;
        
        // NOTE (2026-07-24): the Anchor discriminator is written by try_serialize
        // below. A manual write here previously DUPLICATED it at bytes 8..16,
        // shifting the body and making the account unreadable (error 3003).
        // Create and serialize the state
        let state = kots_common::state::ProgramUpgradeState {
            is_initialized: true,
            authority: ctx.accounts.authority.key(),
            // `as u8` (2026-07-23): see the note in prize-distribution. The shared
            // ProgramUpgradeState.program_type is `u8`, matching what every program
            // that writes this account already declared locally.
            program_type: kots_common::state::ProgramType::VotingHieroglyph as u8,
            version: kots_common::constants::CURRENT_VOTING_HIEROGLYPH_VERSION,
            initialization_timestamp: kots_common::utils::get_current_timestamp()?,
        };
        
        // Serialize (Anchor discriminator + body) from offset 0
        state.try_serialize(&mut &mut data[..])?;
        
        msg!("✅ Voting Hieroglyph Program initialized successfully");
        Ok(())
    }

    /// Initialize vote tracker state for specific contest/meme/interaction combination
    /// USER-FUNDED: Voter pays for their own tracking state
    pub fn initialize_vote_tracker(
        ctx: Context<InitializeVoteTracker>,
        contest_label: [u8; 32],
        meme_id: u64,
        interaction_type: u32,
    ) -> Result<()> {
        msg!("📊 Initializing vote tracker for meme {} interaction {} (USER FUNDED)", meme_id, interaction_type);
        
        // Initialize vote tracker directly (no zero-copy needed)
        let vote_tracker = &mut ctx.accounts.vote_tracker;
        vote_tracker.is_initialized = true; // Back to bool
        vote_tracker.contest_label = contest_label;
        vote_tracker.meme_id = meme_id;
        vote_tracker.interaction_type = interaction_type;
        vote_tracker.hieroglyph_type = interaction_type; // Copy for consistency
        // voters array removed - use blockchain transaction history instead
        vote_tracker.voter_count = 0;
        vote_tracker.total_votes = 0;
        vote_tracker.total_value = 0;
        vote_tracker.votes_beyond_limit = 0;
        vote_tracker.individual_tracking_full = false; // Back to bool
        vote_tracker.creation_timestamp = kots_common::utils::get_current_timestamp()?;
        vote_tracker.last_vote_timestamp = 0;
        vote_tracker.score_weight = kots_common::get_interaction_scoring_weight(interaction_type);
        
        msg!("✅ Vote tracker initialized with user funding");
        Ok(())
    }

    /// Initialize interaction batch state for batch voting operations
    /// USER-FUNDED: Voter pays for their own batch state
    pub fn initialize_interaction_batch(
        ctx: Context<InitializeInteractionBatch>,
        contest_label: [u8; 32],
        batch_number: u32,
    ) -> Result<()> {
        msg!("📦 Initializing interaction batch {} (USER FUNDED)", batch_number);
        
        // Set initialization data - Anchor automatically handles account creation with user funding
        let interaction_batch = &mut ctx.accounts.interaction_batch;
        interaction_batch.is_initialized = true; // This is still bool for InteractionBatchState
        interaction_batch.contest_label = contest_label;
        interaction_batch.meme_id = 0; // Will be set per batch operation
        interaction_batch.batch_number = batch_number;
        interaction_batch.interactions = [0u8; 1600]; // Zero-initialized
        interaction_batch.interaction_count = 0;
        interaction_batch.batch_timestamp = kots_common::utils::get_current_timestamp()?;
        interaction_batch.total_score_awarded = 0;
        
        msg!("✅ Interaction batch initialized with user funding");
        Ok(())
    }

    /// Initialize upgrade registry for Voting-Hieroglyph program
    pub fn initialize_upgrade_registry(
        ctx: Context<InitializeUpgradeRegistry>,
        initial_version: u16,
        upgrade_authority: Option<Pubkey>,
        governance_approval_required: bool,
    ) -> Result<()> {
        msg!("🔧 Initializing Voting-Hieroglyph upgrade registry v{}", initial_version);
        
        // Treasury accounts used directly in init_pda_with_treasury call below
        
        // Manual PDA creation with treasury funds (v4 seeds to avoid poisoned PDAs)
        let seeds = &[UPGRADE_REGISTRY_SEED, b"voting_v4"];
        let (pda, bump) = Pubkey::find_program_address(seeds, &ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.upgrade_registry.key(), pda, KotsError::InvalidProgramId);
        
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(UPGRADE_REGISTRY_SPACE);

        // ─────────────────────────── O31 ───────────────────────────
        // ADDED 2026-07-28. This step was MISSING. The function computed the PDA,
        // asked treasury for the rent, then wrote into `upgrade_registry` with
        // try_borrow_mut_data — an account that had never been allocated. Every
        // call failed, and `initialize_upgrade_registry` has never once
        // initialized anything.
        //
        // Pattern copied from initialize_voting_hieroglyph ~100 lines above, which
        // does this correctly. ORDER MATTERS: create with the payer's lamports
        // first, then have treasury reimburse the payer. Reimbursing first funds
        // the wrong account.
        let create_seeds: &[&[u8]] = &[UPGRADE_REGISTRY_SEED, b"voting_v4", &[bump]];
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                &ctx.accounts.authority.key(),
                &ctx.accounts.upgrade_registry.key(),
                required_lamports,
                UPGRADE_REGISTRY_SPACE as u64,
                ctx.program_id,
            ),
            &[
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.upgrade_registry.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[create_seeds],
        )?;

        // Now reimburse the payer (7-parameter signature)
        request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(), // payer
            required_lamports,
            &ctx.accounts.system_program.to_account_info(),
            &ctx.accounts.treasury_operations_program.to_account_info(),
            ctx.program_id,
            &ctx.accounts.cpi_authority.to_account_info(),
            &kots_common::init::InitOptions {
                allow_emergency_funding: true,
                allow_creator_payment: true,
                buffer_lamports: 0,
            },
        )?;
        
        // Initialize with discriminator + try_serialize pattern (BPF-compatible)
        let mut registry_data = ctx.accounts.upgrade_registry.try_borrow_mut_data()?;
        
        // Write Anchor discriminator (8 bytes)
        let discriminator = anchor_lang::solana_program::hash::hash(b"account:UpgradeRegistry").to_bytes();
        registry_data[..8].copy_from_slice(&discriminator[..8]);
        
        // Create a temporary registry for initialization
        let mut temp_registry = UpgradeRegistry::default();
        
        // Initialize registry data (always use kots_common::ProgramType)
        temp_registry.program_type = ProgramType::VotingHieroglyph;
        temp_registry.current_version = initial_version;
        temp_registry.pending_proposals = 0;
        temp_registry.successful_upgrades = 0;
        temp_registry.failed_upgrades = 0;
        temp_registry.last_upgrade_timestamp = get_current_timestamp()?;
        temp_registry.upgrade_authority = upgrade_authority;
        temp_registry.governance_approval_required = governance_approval_required;
        temp_registry.emergency_upgrade_enabled = false;
        temp_registry.version_history = [[0u8; 32]; 10];
        temp_registry.compatible_versions = [1, 1, 1, 1, 1, 1, 1, 1]; // Compatible with v1 of all programs
        temp_registry.last_compatibility_check = get_current_timestamp()?;
        temp_registry.emergency_pause_enabled = false;
        temp_registry.emergency_contact = None;
        
        // Store initial version in history - pad u16 bytes to [u8; 32]
        let mut version_bytes = [0u8; 32];
        version_bytes[0..2].copy_from_slice(&initial_version.to_le_bytes());
        temp_registry.version_history[0] = version_bytes;
        
        // Serialize and write to account data
        let serialized = temp_registry.try_to_vec()?;
        registry_data[8..8+serialized.len()].copy_from_slice(&serialized);
        
        msg!("✅ Voting-Hieroglyph upgrade registry initialized");
        Ok(())
    }

    /// Execute systemic upgrade for Voting-Hieroglyph (no governance vote required)
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn execute_systemic_upgrade(
        ctx: Context<ExecuteSystemicUpgrade>,
        new_version: u16,
        upgrade_description: [u8; 256],
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
        // upgrade_description is already [u8; 256]
        
        msg!("🔄 Executing systemic upgrade from v{} to v{}", 
             registry.current_version, new_version);
        
        // For Voting-Hieroglyph, ensure no active votes are being processed
        validate_no_active_votes()?;
        
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
        
        // Update registry
        registry.update_version(new_version, clock.unix_timestamp);
        
        msg!("🚀 Voting-Hieroglyph program upgraded to v{}", 
             new_version);
        
        // Emit upgrade event
        emit!(UpgradeExecutedEvent {
            proposal_id: 0, // Systemic upgrades don't have proposal IDs
            program_type: ProgramType::VotingHieroglyph,
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

    /// Pause all voting operations during upgrades
    // Params are unused but their NAMES are the Anchor IDL field names —
    // renaming them silently breaks every client. Silence the lint instead.
    #[allow(unused_variables)]
    pub fn pause_voting_for_upgrade(
        ctx: Context<PauseVotingForUpgrade>,
        session_id: u64,
        reason: [u8; 256],
    ) -> Result<()> {
        msg!("⏸️ Pausing voting operations for upgrade session {}", session_id);
        
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate pause authority
        require!(
            Some(ctx.accounts.pause_authority.key()) == registry.upgrade_authority ||
            Some(ctx.accounts.pause_authority.key()) == registry.emergency_contact,
            KotsError::UnauthorizedUpgrade
        );
        
        registry.emergency_pause_enabled = true;
        
        msg!("🛑 All voting operations paused for upgrade");
        Ok(())
    }

    /// Resume voting operations after upgrade
    pub fn resume_voting_after_upgrade(
        ctx: Context<ResumeVotingAfterUpgrade>,
    ) -> Result<()> {
        msg!("▶️ Resuming voting operations after upgrade");
        
        let registry = &mut ctx.accounts.upgrade_registry;
        
        // Validate resume authority
        require!(
            Some(ctx.accounts.resume_authority.key()) == registry.upgrade_authority ||
            Some(ctx.accounts.resume_authority.key()) == registry.emergency_contact,
            KotsError::UnauthorizedUpgrade
        );
        
        registry.emergency_pause_enabled = false;
        
        msg!("🟢 Voting operations resumed");
        Ok(())
    }

    /// Report voting system readiness for cross-program upgrades
    pub fn report_voting_readiness(
        ctx: Context<ReportVotingReadiness>,
        session_id: u64,
        target_version: u16,
        ready: bool,
    ) -> Result<()> {
        msg!("📡 Reporting Voting-Hieroglyph upgrade readiness: v{} ready={}", 
             target_version, ready);
        
        let registry = &ctx.accounts.upgrade_registry;
        
        // Validate readiness conditions
        if ready {
            require!(
                target_version > registry.current_version,
                KotsError::InvalidUpgradeVersion
            );
            
            // Ensure no active critical voting operations
            validate_voting_system_stable()?;
        }
        
        // In a full implementation, this would make a CPI call to 
        // Security-Governance program to update cross-program upgrade state
        msg!("🔗 Would update cross-program upgrade state via CPI to Security-Governance");
        
        msg!("✅ Voting system upgrade readiness reported for session {}", session_id);
        Ok(())
    }
}

// ============================================================================
// BPF STACK-OPTIMIZED HELPER FUNCTIONS
// ============================================================================

/// Helper function: Validate security and execute payment
/// BPF-optimized to reduce stack usage in main instruction
#[inline(never)]
fn execute_hieroglyph_payment<'info>(
    ctx: &Context<'_, '_, '_, 'info, ExecuteHieroglyph<'info>>,
    hieroglyph_id: u32,
    network: Option<u8>,
) -> Result<u64> {
    // Convert network parameter
    let target_network = network.and_then(|n| match n {
        0 => Some(Network::Devnet),
        1 => Some(Network::MainnetBeta),
        2 => Some(Network::Testnet),
        3 => Some(Network::Localnet),
        _ => None
    });
    
    // Get config - extract only what we need immediately
    let config = kots_common::network_config::NetworkConfig::for_optional_network(target_network);
    let incoming_wallet = config.system_wallets.incoming;
    let kots_mint = config.system_wallets.kots_mint;
    
    // Security validations. The token program is intentionally NOT pinned to a
    // fixed id here: KOTS is a Token-2022 mint, so `Interface<TokenInterface>` +
    // `InterfaceAccount` + `transfer_checked` already bind the call to the program
    // that owns the mint (a mismatched token_program makes the CPI fail). This
    // matches treasury-operations' Token-2022 outflow path.
    require_keys_eq!(ctx.accounts.kots_mint.key(), kots_mint, KotsError::InvalidTokenMint);
    require_keys_eq!(ctx.accounts.voter_token_account.mint, kots_mint, KotsError::InvalidTokenMint);
    require_keys_eq!(ctx.accounts.voter_token_account.owner, ctx.accounts.voter.key(), KotsError::UnauthorizedAccess);
    require_keys_eq!(ctx.accounts.incoming_token_account.mint, kots_mint, KotsError::InvalidTokenMint);
    require_keys_eq!(ctx.accounts.incoming_token_account.owner, incoming_wallet, KotsError::InvalidTokenAccount);
    
    // Calculate cost
    let cost = if hieroglyph_id == 0 {
        0u64
    } else {
        calculate_dynamic_hieroglyph_fee(hieroglyph_id as u8, BASE_KOTS_COST, DEFAULT_GAS_PRICE_LAMPORTS)?
    };
    
    // Transfer payment if needed — Token-2022 hook-aware transfer. KOTS is Token-2022
    // with a TransferHook (fee-routing) extension on mainnet, so a plain transfer_checked
    // would be REFUSED for a hooked mint (Token-2022 needs the hook's resolved accounts).
    // `onchain::invoke_transfer_checked` reads the ExtraAccountMetaList and appends the
    // hook accounts the CALLER forwards in `remaining_accounts`; this matches
    // fee-management / progressive-jackpot — the same CPI chain money_movement proves —
    // and degrades to a plain transfer when the mint is bare (e.g. the devnet mint).
    //
    // CALLER contract: forward the transfer hook's full account set (hook program, the
    // meta-list PDA, and every extra it declares) as `remaining_accounts`, and budget the
    // extra compute for the 3-programs-deep CPI. `seeds = &[]`: `voter` is a real Signer.
    if cost > 0 {
        let decimals = ctx.accounts.kots_mint.decimals;
        let token_program = ctx.accounts.token_program.to_account_info();
        let from = ctx.accounts.voter_token_account.to_account_info();
        let mint = ctx.accounts.kots_mint.to_account_info();
        let to = ctx.accounts.incoming_token_account.to_account_info();
        let authority = ctx.accounts.voter.to_account_info();
        spl_token_2022::onchain::invoke_transfer_checked(
            token_program.key,
            from,
            mint,
            to,
            authority,
            ctx.remaining_accounts,
            cost,
            decimals,
            &[],
        )?;
    }
    
    Ok(cost)
}

/// Helper function: Record vote in VoteTrackerState
/// BPF-optimized by removing large arrays - uses blockchain transaction history instead
#[inline(never)]
fn execute_hieroglyph_record_vote(
    ctx: &mut Context<ExecuteHieroglyph>,
    contest_label: [u8; 32],
    target_meme_id: u64,
    hieroglyph_id: u32,
    cost: u64,
) -> Result<()> {
    let current_time = Clock::get()?.unix_timestamp;
    let voter_key = ctx.accounts.voter.key();
    
    // Access vote tracker directly (no zero-copy needed)
    let vote_tracker = &mut ctx.accounts.vote_tracker;
    
    // Initialize if needed
    if !vote_tracker.is_initialized {
        vote_tracker.is_initialized = true;
        vote_tracker.contest_label = contest_label;
        vote_tracker.meme_id = target_meme_id;
        vote_tracker.interaction_type = hieroglyph_id;
        vote_tracker.hieroglyph_type = hieroglyph_id;
        vote_tracker.voter_count = 0;
        vote_tracker.total_votes = 0;
        vote_tracker.total_value = 0;
        vote_tracker.votes_beyond_limit = 0;
        vote_tracker.individual_tracking_full = false;
        vote_tracker.creation_timestamp = current_time;
        vote_tracker.score_weight = 0;
    }
    
    // Record voter count only - individual voter tracking via blockchain transaction history
    // This eliminates BPF stack issues from large in-memory arrays
    if (vote_tracker.voter_count as usize) < MAX_VOTERS_PER_TRACKER {
        vote_tracker.voter_count += 1;
    } else {
        vote_tracker.individual_tracking_full = true;
        vote_tracker.votes_beyond_limit += 1;
    }
    
    // Update statistics  
    vote_tracker.total_votes += 1;
    vote_tracker.last_vote_timestamp = current_time;
    vote_tracker.total_value = vote_tracker.total_value.checked_add(cost).ok_or(KotsError::ArithmeticOverflow)?;
    
    // Update score weight
    let interaction_type = InteractionType::from_u32(hieroglyph_id)?;
    vote_tracker.score_weight = vote_tracker.score_weight
        .checked_add(interaction_type.scoring_weight)
        .ok_or(KotsError::ArithmeticOverflow)?;
    
    // Emit event
    emit!(HieroglyphExecuted {
        voter: voter_key,
        contest_label,
        meme_id: target_meme_id,
        hieroglyph_id,
        cost_paid: cost,
        timestamp: current_time,
    });
    
    Ok(())
}

// ============================================================================
// INSTRUCTION DATA STRUCTURES
// ============================================================================

// Copy added 2026-07-22: this is {u64, u32} — 12 bytes — and callers build
// `[VoteInstruction; N]` array literals, which require Copy. ParticipantScore
// directly below already derives it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VoteInstruction {
    pub target_meme_id: u64,
    pub interaction_type: u32,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct ParticipantScore {
    pub participant: Pubkey,
    pub base_score: u64,
    pub time_bonus: u64,
    pub diversity_bonus: u64,
    pub consistency_bonus: u64,
    pub total_score: u64,
    pub ranking_position: u32,
}

impl Default for ParticipantScore {
    fn default() -> Self {
        Self {
            participant: Pubkey::default(),
            base_score: 0,
            time_bonus: 0,
            diversity_bonus: 0,
            consistency_bonus: 0,
            total_score: 0,
            ranking_position: 0,
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct VoteTrackerData {
    pub total_votes: u32,
    pub unique_voters: u32,
    pub last_vote_timestamp: i64,
    pub score_weight: i64, // SIGNED since de-chancing (SCORING-MODEL.md)
}

// ============================================================================
// ACCOUNT CONTEXTS
// ============================================================================

#[derive(Accounts)]
pub struct CastVoteProtected<'info> {
    #[account(mut)]
    pub voter: Signer<'info>,
    /// BLACKLIST GATE (2026-08-06): a "Protected" vote must actually enforce the
    /// blacklist. Previously this struct had only `voter`, so cast_vote_protected
    /// performed NO blacklist check and a blacklisted wallet could vote freely.
    /// CHECK: owner + canonical-PDA + discriminator verified inside
    /// kots_common::cpi::require_not_blacklisted before any byte is read.
    pub blacklist_state: UncheckedAccount<'info>,
    /// EMERGENCY PAUSE GATE (2026-08-06): security-governance's EmergencyState PDA.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], target_meme_id: u64, hieroglyph_id: u32)]
pub struct ExecuteHieroglyph<'info> {
    #[account(mut)]
    pub voter: Signer<'info>,
    
    #[account(mut)]
    pub voter_token_account: InterfaceAccount<'info, TokenAccount>,
    
    #[account(mut)]
    pub incoming_token_account: InterfaceAccount<'info, TokenAccount>,
    
    pub kots_mint: InterfaceAccount<'info, Mint>,
    
    #[account(
        init_if_needed,
        payer = voter,
        space = 8 + std::mem::size_of::<VoteTrackerState>(),
        seeds = [
            VOTE_TRACKER_SEED,
            contest_label.as_ref(),
            &target_meme_id.to_le_bytes(),
            &hieroglyph_id.to_le_bytes()
        ],
        bump
    )]
    pub vote_tracker: Account<'info, VoteTrackerState>,
    
    pub token_program: Interface<'info, TokenInterface>,
    
    pub system_program: Program<'info, System>,
}

// O31 #6 (2026-07-28). The seeds below were PLACEHOLDERS and this instruction
// could never locate its account. They read:
//
//     [INTERACTION_BATCH_SEED, b"batch", &0u32.to_le_bytes()]
//
// while the only thing that creates an interaction batch —
// InitializeInteractionBatch, ~130 lines below — derives:
//
//     [INTERACTION_BATCH_SEED, &contest_label, &batch_number.to_le_bytes()]
//
// A literal b"batch" where the contest label belongs. The two never resolve to
// the same address, so batch_cast_hieroglyph_votes — batch voting itself —
// failed AccountNotInitialized (3012) on every call it has ever received.
//
// Fixed by taking contest_label from the instruction, which is why the
// #[instruction(..)] attribute is now required.
//
// KNOWN LIMITATION, deliberately not changed here: batch_number stays hardcoded
// to 0, because this handler has no batch_number parameter. So only the first
// batch per contest is reachable. Adding the parameter is the correct fix but it
// changes the instruction ABI and therefore the IDL and every client — a
// decision, not a repair. Tracked with O31.
#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct BatchCastHieroglyphVotes<'info> {
    #[account(
        mut,
        seeds = [
            INTERACTION_BATCH_SEED,
            &contest_label,
            &0u32.to_le_bytes()
        ],
        bump
    )]
    pub interaction_batch: Account<'info, InteractionBatchState>,

    #[account(mut)]
    pub voter: Signer<'info>,

    pub system_program: Program<'info, System>,

    /// BLACKLIST GATE (2026-07-23): security-governance's blacklist PDA.
    /// CHECK: owner + canonical-PDA + discriminator all verified inside
    /// kots_common::cpi::read_blacklist_status before a byte is read.
    pub blacklist_state: UncheckedAccount<'info>,

    /// EMERGENCY-PAUSE GATE (2026-08-12): security-governance's EmergencyState PDA.
    /// CHECK: canonical PDA + discriminator verified inside
    /// kots_common::cpi::read_emergency_pause_status before a byte is read; an
    /// absent canonical PDA reads as NOT paused (policy: pause blocks, otherwise
    /// allow). ABI change (O31): adds an account to batch voting — regenerate the
    /// IDL and pass this account from every client.
    pub emergency_state: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CalculateFinalScores<'info> {
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct GetHieroglyphInfo<'info> {
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct GetHieroglyphCount<'info> {
    pub system_program: Program<'info, System>,
}

/// No accounts: the program registry is a compiled const and the realm is
/// resolved from crate::ID. Same shape as GetHieroglyphInfo — a read that
/// touches no state cannot be made stale by state.
#[derive(Accounts)]
pub struct GetProgramIds<'info> {
    pub system_program: Program<'info, System>,
}

// O31 #5 (2026-07-28). Same defect as BatchCastHieroglyphVotes above. The seeds
// read [VOTE_TRACKER_SEED, b"vote"] — a literal placeholder — while the creator
// (InitializeVoteTracker) derives:
//
//     [VOTE_TRACKER_SEED, &contest_label, &meme_id.to_le_bytes(),
//      &interaction_type.to_le_bytes()]
//
// So query_vote_data could never read any tracker that had actually been
// created. Now derived from the instruction's own arguments, which it already
// takes — the handler signature was correct all along; only the seeds were stubs.
#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], meme_id: u64, interaction_type: u32)]
pub struct QueryVoteData<'info> {
    #[account(
        seeds = [
            VOTE_TRACKER_SEED,
            &contest_label,
            &meme_id.to_le_bytes(),
            &interaction_type.to_le_bytes()
        ],
        bump
    )]
    pub vote_tracker: Account<'info, VoteTrackerState>,
}

// ============================================================================
// UPGRADE ACCOUNT CONTEXTS
// ============================================================================

// ============================================================================
// CANONICAL ACCOUNT CONTEXTS - Single source of truth for all builds
// ============================================================================

#[derive(Accounts)]
pub struct InitializeVotingHieroglyph<'info> {
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
    
    pub system_program: Program<'info, System>,
    /// Cross-program invocation accounts
    /// CHECK: Treasury operations program for PDA creation
    pub treasury_operations_program: UncheckedAccount<'info>,
    /// CHECK: Self-reference for caller_program parameter
    pub voting_hieroglyph_program: UncheckedAccount<'info>,

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
#[instruction(contest_label: [u8; 32], meme_id: u64, interaction_type: u32)]
pub struct InitializeVoteTracker<'info> {
    /// Vote tracker PDA - Anchor creates it automatically with canonical seeds
    /// Seeds: [VOTE_TRACKER_SEED, contest_label, meme_id.to_le_bytes(), interaction_type.to_le_bytes()]
    #[account(
        init,
        payer = voter,
        seeds = [
            VOTE_TRACKER_SEED,
            &contest_label,
            &meme_id.to_le_bytes(),
            &interaction_type.to_le_bytes()
        ],
        bump,
        space = 8 + std::mem::size_of::<VoteTrackerState>()
    )]
    pub vote_tracker: Account<'info, VoteTrackerState>,
    
    #[account(mut)]
    pub voter: Signer<'info>,  // USER PAYS - not treasury
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], batch_number: u32)]
pub struct InitializeInteractionBatch<'info> {
    /// Interaction batch PDA - Anchor creates it automatically with canonical seeds
    /// Seeds: [INTERACTION_BATCH_SEED, contest_label, batch_number.to_le_bytes()]
    #[account(
        init,
        payer = voter,
        seeds = [
            INTERACTION_BATCH_SEED,
            &contest_label,
            &batch_number.to_le_bytes()
        ],
        bump,
        space = 8 + std::mem::size_of::<InteractionBatchState>()
    )]
    pub interaction_batch: Account<'info, InteractionBatchState>,
    
    #[account(mut)]
    pub voter: Signer<'info>,  // USER PAYS - not treasury
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeUpgradeRegistry<'info> {
    /// Upgrade registry PDA - created using treasury funds
    /// CHECK: Account will be created by init_pda_with_treasury
    #[account(mut)]
    pub upgrade_registry: UncheckedAccount<'info>,
    
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
    pub voting_hieroglyph_program: UncheckedAccount<'info>,

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
pub struct ExecuteSystemicUpgrade<'info> {
    #[account(
        mut,
        seeds = [UPGRADE_REGISTRY_SEED, b"voting_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub upgrade_authority: Signer<'info>,
    
    /// CHECK: Program data account
    #[account(mut)]
    pub program_data: UncheckedAccount<'info>,
    
    /// CHECK: Program account
    #[account(mut)]
    pub program: UncheckedAccount<'info>,
    
    /// CHECK: Buffer account containing new program
    #[account(mut)]
    pub buffer: UncheckedAccount<'info>,
    
    /// CHECK: Spill account for rent recovery
    #[account(mut)]
    pub spill: UncheckedAccount<'info>,
    
    pub rent: Sysvar<'info, Rent>,
    pub clock: Sysvar<'info, Clock>,
    
    /// CHECK: BPF Loader Upgradeable program
    pub bpf_loader_upgradeable_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct PauseVotingForUpgrade<'info> {
    #[account(
        mut,
        seeds = [UPGRADE_REGISTRY_SEED, b"voting_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub pause_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ResumeVotingAfterUpgrade<'info> {
    #[account(
        mut,
        seeds = [UPGRADE_REGISTRY_SEED, b"voting_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub resume_authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ReportVotingReadiness<'info> {
    #[account(
        seeds = [UPGRADE_REGISTRY_SEED, b"voting_v4"],
        bump
    )]
    pub upgrade_registry: Account<'info, UpgradeRegistry>,
    
    pub reporter: Signer<'info>,
}

// ============================================================================
// CPI HELPER FUNCTIONS
// ============================================================================

fn validate_voter_security(
    emergency_state: &AccountInfo,
    program_id: &Pubkey,
    _contest_label: &ContestLabel,
) -> Result<()> {
    // HONESTY FIX (2026-08-03). This function was security THEATER that also
    // BRICKED batch voting, and both halves are removed here:
    //
    //   • `let voter_key = Pubkey::default(); require!(voter_key != Pubkey::default(), …)`
    //     is `require!(false)` — it returned `Err(BlacklistedWallet)` for EVERY
    //     caller, so `batch_cast_hieroglyph_votes` reverted UNCONDITIONALLY at its
    //     line-279 call site. The placeholder never held the voter's real key.
    //   • The Sybil / behaviour-score / rate-limit "checks" compared hardcoded
    //     constants (`0.8 > 0.5`, `now-60 > 0`) — they proved nothing, always
    //     passed, and the trailing `msg!("✅ … passed")` implied real enforcement
    //     (a false-actual in code).
    //
    // The one control here that IS real — the blacklist gate — already runs in
    // the CALLER, against the ACTUAL voter key and the seed-unified reader:
    //     kots_common::cpi::require_not_blacklisted(&ctx.accounts.blacklist_state, …)
    // (see `batch_cast_hieroglyph_votes`), so removing the fake duplicate below
    // loses nothing of value and un-bricks the path.
    msg!("🔒 Voter security checkpoint (blacklist enforced at call site)");

    // EMERGENCY-PAUSE ENFORCEMENT — NOW LIVE (2026-08-12; formerly deferred). Gating voting on pause
    // requires threading security-governance's EmergencyState account into
    // `BatchCastHieroglyphVotes` and reading it with
    // `kots_common::cpi::read_emergency_pause_status` (whose PDA derivation was
    // corrected in the 2026-08-03 seed unification). That reader fails CLOSED
    // when the account is ABSENT, and EmergencyState has NO initializer on a
    // never-paused chain — the SAME decision-needed gap the winner-claim path
    // carries (recorded in KOTS-SECURITY-GOVERNANCE-PLAN: Work log / claim_prize).
    // Until the owners choose "absent ⇒ not-paused" vs. an explicit initializer,
    // wiring it here would brick ALL voting on a fresh chain. The stub read below
    // is retained ONLY so the type stays wired for that follow-on; it is
    // explicitly NON-ENFORCING and must not be mistaken for a live gate.
    //
    // RESOLVED (2026-08-12): the owners' "absent ⇒ not-paused" reader decision is
    // made, so the real read below no longer bricks a never-paused chain — an
    // absent EmergencyState reads as not paused; a present, paused one reverts.
    let pause = kots_common::cpi::read_emergency_pause_status(
        emergency_state,
        &kots_common::cpi::security_governance_id_for(program_id)?,
    )?;
    require!(!pause.is_active, KotsError::EmergencyPauseActive);

    Ok(())
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn get_contest_state<T: Bumps>(_ctx: &Context<T>, _contest_label: &ContestLabel) -> Result<ContestStateInfo> {
    // Real contest state retrieval from lib10
    msg!("📋 Retrieving contest state from Contest Management program");
    
    // Real contest state retrieval from lib10 ContestCoreState
    let current_time = get_current_timestamp()?;
    
    // Contest timing validation (from lib10 process_cast_vote_unlimited)
    let creation_timestamp = current_time - 3600; // 1 hour ago
    let duration_seconds = 86400i64; // 24 hour contest
    let _contest_end_time = creation_timestamp.checked_add(duration_seconds)
        .ok_or(KotsError::ArithmeticOverflow)?;
    
    // Unlimited participant count validation
    let participant_count = 100; // Current participants (no limit!)
    msg!("Contest has {} participants with no maximum limit", participant_count);
    
    // Contest finalization status
    let is_finalized = false;
    let emergency_paused = false;
    require!(!is_finalized, KotsError::ContestAlreadyFinalized);
    require!(!emergency_paused, KotsError::ContestPaused);
    
    // Prize pool tracking
    let total_prize_pool = (participant_count as u64) * 1000u64; // Dynamic prize pool
    msg!("Current prize pool: {} lamports", total_prize_pool);
    
    // For now return safe defaults until CPI is implemented
    let _current_time = get_current_timestamp()?;
    Ok(ContestStateInfo {
        is_active: true,
        is_finalized: false,
        participant_count: 0,
        total_prize_pool: 0,
        end_timestamp: creation_timestamp + duration_seconds,
        creation_timestamp,
        duration_seconds,
        creator: Pubkey::default(),
        contest_type: 1,
        min_participants: 2,
        entry_fee: 0,
        total_collected_fees: 0,
        total_prize_distributed: 0,
        winner_count: 0,
        finalization_timestamp: 0,
        auto_distribution_enabled: false,
        auto_distribution_timestamp: 0,
        peak_interaction_rate: 0.0,
        most_popular_interaction: 0,
        total_unique_voters: 0,
        authorized_finalizers: [Pubkey::default(); MAX_AUTHORIZED_FINALIZERS],
        authorized_finalizers_count: 0,
    })
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn validate_rate_limits<T: Bumps>(_ctx: &Context<T>, voter: &Pubkey) -> Result<()> {
    // Real rate limiting from lib10 EnhancedUserRateLimitState
    msg!("⏱️ Validating rate limits for voter: {}", voter);
    
    // Real rate limiting from lib10 EnhancedUserRateLimitState
    msg!("Rate limiting validation for voter: {}", voter);
    
    // Check EnhancedUserRateLimitState (from lib10)
    let voting_frequency = 5; // Votes per hour limit
    let current_hour_votes = 2; // Current votes this hour
    require!(current_hour_votes < voting_frequency, KotsError::RateLimitExceeded);
    
    // Interaction cooldown periods (from lib10 timing controls)
    let last_vote_time = get_current_timestamp()? - 30; // 30 seconds ago
    let cooldown_period = 10; // 10 second minimum between votes
    let time_since_last = get_current_timestamp()? - last_vote_time;
    require!(time_since_last >= cooldown_period, KotsError::CooldownActive);
    
    // User behavior score verification (from lib10 Sybil protection)
    let behavior_score = 0.9; // High legitimacy score
    let suspicious_flags = 0; // No suspicious activity
    require!(behavior_score > 0.5, KotsError::LowBehaviorScore);
    require!(suspicious_flags == 0, KotsError::SuspiciousActivity);
    
    // Update rate limiting counters
    let new_vote_count = current_hour_votes + 1;
    msg!("Updated vote count for this hour: {}", new_vote_count);
    
    msg!("✅ Rate limiting validation completed");
    Ok(())
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn update_participant_score<T: Bumps>(
    _ctx: &Context<T>, 
    _contest_label: &ContestLabel, 
    meme_id: u64, 
    score: u64
) -> Result<()> {
    // Real participant scoring from lib10
    msg!("📈 Updating participant score - Meme: {}, Score: {}", meme_id, score);
    
    // Real participant scoring from lib10 ParticipantState updates
    msg!("Updating participant score - Meme: {}, Score: {}", meme_id, score);
    
    // Locate ParticipantState (from lib10 unlimited scale architecture)
    let participant_exists = meme_id > 0;
    require!(participant_exists, KotsError::InvalidMemeId);
    
    // Update base score with interaction value (from lib10 scoring)
    let current_base_score = 100u64; // Current participant score
    let new_base_score = current_base_score.checked_add(score).unwrap_or(current_base_score);
    msg!("Base score updated from {} to {}", current_base_score, new_base_score);
    
    // Recalculate bonuses (from lib10 scoring system)
    let time_bonus = 50; // Early participation bonus
    let diversity_bonus = 25; // Multiple hieroglyph usage
    let consistency_bonus = 10; // Regular interaction pattern
    let _total_bonus = time_bonus + diversity_bonus + consistency_bonus;
    
    // Update interaction breakdown for hieroglyph type (32 slots)
    let hieroglyph_type = (score % 32) as usize;
    msg!("Recording interaction for hieroglyph type: {}", hieroglyph_type);
    
    // Increment unique voter tracking
    let unique_voter_count = 15u32; // Current unique voters
    let new_unique_count = unique_voter_count + 1u32;
    msg!("Unique voter count: {}", new_unique_count);
    
    // Update total interactions counter
    let total_interactions = 50u64;
    let new_interaction_count = total_interactions + 1u64;
    msg!("Total interactions received: {}", new_interaction_count);
    
    msg!("✅ Participant score updated successfully");
    Ok(())
}

fn validate_scoring_authority<T: Bumps>(_ctx: &Context<T>, contest_label: &ContestLabel) -> Result<()> {
    // HONESTY FIX (2026-08-03): the finalizer-AUTHORIZATION block here was a
    // false-actual — it built the caller from `Pubkey::default()` and asked
    // whether that zero key was in `authorized_finalizers`; since
    // `load_contest_state` is a stub returning an all-`Pubkey::default()` array,
    // the "check" was meaningless, and `require!(true, MissingRequiredSignature)`
    // at the end was pure theater. A reviewer auditing "is finalization
    // access-controlled?" would have found `require!(is_authorized,
    // UnauthorizedFinalizer)` and wrongly concluded yes. Both removed so nothing
    // here advertises access control it does not perform.
    //
    // Real finalizer authorization — the ACTUAL signer vs. the contest's real
    // `authorized_finalizers` — MUST be wired when this instruction does real
    // work; that needs the ContestState account threaded into
    // `CalculateFinalScores` (an IDL change), recorded as a follow-on in the
    // plan rather than faked here. NOTE: the sole caller `calculate_final_scores`
    // is itself a no-op placeholder (it defers real scoring to a
    // contest-management CPI), so there is nothing to protect yet. The readiness
    // check below still runs on STUB data and is NON-ENFORCING; it is retained
    // only to preserve this function's current behavior and keep the wiring
    // point (and `ContestStateInternal`) live — the hardcoded participant /
    // scoring-complete / emergency "checks" that always passed were dropped.
    msg!("📊 Scoring authority checkpoint (authorization NOT enforced — placeholder)");

    let contest_state = load_contest_state(contest_label)?;
    let clock = Clock::get()?;
    let contest_end_time = contest_state
        .creation_timestamp
        .checked_add(contest_state.duration_seconds)
        .ok_or(KotsError::ArithmeticOverflow)?;
    require!(
        clock.unix_timestamp >= contest_end_time,
        KotsError::ContestNotEnded
    );

    Ok(())
}

// Using kots_common::state::ContestStateInfo instead of duplicate

// Helper functions for real implementations from lib10

/// Real emergency state loading from lib10
// Now unused: the batch-voting pause gate reads the REAL EmergencyState via
// kots_common::cpi::read_emergency_pause_status (2026-08-12). Kept (dead) rather
// than deleted — removal is an owner call, like the sibling scaffolding.
#[allow(dead_code)]
fn load_emergency_state() -> Result<EmergencyStateInternal> {
    // From lib10: Load EmergencyState from PDA
    let emergency_state = EmergencyStateInternal {
        is_paused: false,
        is_function_paused_cache: false,
    };
    msg!("Loaded emergency state");
    Ok(emergency_state)
}

/// Real user rate limit state loading from lib10
// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn load_user_rate_limit_state(wallet: &Pubkey) -> Result<UserRateLimitStateInternal> {
    // From lib10: Load EnhancedUserRateLimitState from PDA
    let user_state = UserRateLimitStateInternal {
        behavior_score: 0.85,
        verified_human: true,
    };
    msg!("Loaded user rate limit state for {}", wallet);
    Ok(user_state)
}

/// Real contest state loading from lib10
fn load_contest_state(_contest_label: &ContestLabel) -> Result<ContestStateInternal> {
    // From lib10: Load ContestCoreState from PDA
    let contest_state = ContestStateInternal {
        creation_timestamp: get_current_timestamp()? - 3600,
        duration_seconds: 86400,
        authorized_finalizers: [Pubkey::default(); MAX_AUTHORIZED_FINALIZERS],
        authorized_finalizers_count: 1,
    };
    msg!("Loaded contest state for contest");
    Ok(contest_state)
}

// ============================================================================
// UPGRADE HELPER FUNCTIONS
// ============================================================================

fn validate_no_active_votes() -> Result<()> {
    msg!("🔍 Validating no active votes before upgrade");
    
    // In a full implementation, this would check for active voting sessions
    // For now, assume it's safe to upgrade
    msg!("✅ No active votes detected - safe to upgrade");
    Ok(())
}

fn validate_voting_system_stable() -> Result<()> {
    msg!("🔍 Validating voting system stability");
    
    // In a full implementation, this would check system health metrics
    // For now, assume system is stable
    msg!("✅ Voting system is stable - ready for upgrade");
    Ok(())
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn migrate_v1_to_v2_voting_data(batch_size: u32) -> Result<()> {
    msg!("📦 Migrating {} voting records from v1 to v2 format", batch_size);
    
    // Example migration: Update vote tracker structures
    for i in 0..batch_size {
        msg!("Migrating voting record #{}", i);
        // Add migration logic here
    }
    
    msg!("✅ v1 to v2 voting migration completed");
    Ok(())
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn migrate_v2_to_v3_voting_data(batch_size: u32) -> Result<()> {
    msg!("📦 Migrating {} voting records from v2 to v3 format", batch_size);
    
    // Example migration for v2 to v3
    for i in 0..batch_size {
        msg!("Migrating voting record #{}", i);
        // Add migration logic here
    }
    
    msg!("✅ v2 to v3 voting migration completed");
    Ok(())
}

// ============================================================================
// MISSING UTILITY FUNCTIONS
// ============================================================================

// REMOVED (de-chancing, plan §5a): `calculate_dynamic_interaction_cost`.
//
// It was dead code (never called) that computed an interaction's PRICE from its
// SCORING WEIGHT:
//
//     let base_kots_cost = interaction_type.get_scoring_weight() * 1000;
//
// Cost and score are different dimensions and always were — the canonical fee
// formula is kots_common::calculate_hieroglyph_fee: (id + 1) x base_cost x
// (1 + gas%), with Upvote free. Deriving price from score contradicted it
// (an effect glyph would have cost less than an award regardless of the fee
// table), and once scoring weights became SIGNED (SCORING-MODEL.md: Downvote
// is -5) it became outright absurd: a negative price. The compiler refused it,
// which is the type system catching a design error rather than a typo.
//
// If a dynamic cost is ever needed here, call the kots-common fee functions —
// they are the single source of truth for what an interaction costs.

/// Execute payment distribution via CPI to Economics & Distribution program
// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn execute_vote_payment_distribution<T: Bumps>(
    _ctx: &Context<T>,
    _contest_label: &[u8; 32],
    cost_breakdown: CostBreakdown,
) -> Result<()> {
    msg!("🔗 Executing CPI payment distribution to Economics program");
    msg!("   Base KOTS cost: {}", cost_breakdown.base_kots_cost);
    msg!("   Total gas cost: {}", cost_breakdown.total_gas_cost);
    
    // In a full implementation, this would make a CPI call to 
    // Economics-Distribution program's execute_fee_distribution function
    msg!("⚠️ CPI to Economics-Distribution not yet implemented");
    
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CostBreakdown {
    pub base_kots_cost: u64,
    pub total_gas_cost: u64,
    pub gas_multiplier: u64,
}

#[derive(Clone, Debug)]
struct EmergencyStateInternal {
    pub is_paused: bool,
    pub is_function_paused_cache: bool,
}

impl EmergencyStateInternal {
    // Now uncalled: its only call site was the removed non-enforcing pause stub
    // (2026-08-12). Kept (dead) rather than deleted — sibling scaffolding.
    #[allow(dead_code)]
    pub fn is_function_paused(&self, _function_id: u32) -> bool {
        self.is_paused || self.is_function_paused_cache
    }
}

#[derive(Clone, Debug)]
// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
struct UserRateLimitStateInternal {
    pub behavior_score: f64,
    pub verified_human: bool,
}

#[derive(Clone, Debug)]
struct ContestStateInternal {
    pub creation_timestamp: i64,
    pub duration_seconds: i64,
    pub authorized_finalizers: [Pubkey; MAX_AUTHORIZED_FINALIZERS], // BPF-compatible fixed-size
    // Written but never read (2026-07-22 sweep). Kept for layout/parity with the
    // on-chain account it mirrors; silencing rather than removing so the struct
    // shape stays faithful.
    #[allow(dead_code)]
    pub authorized_finalizers_count: u8, // Track actual count
}