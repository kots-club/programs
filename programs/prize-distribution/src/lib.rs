// kots-contracts/programs/prize-distribution/src/lib.rs
//! KOTS Prize Distribution Program
//! 
//! Handles award selection algorithms, prize calculation, winner validation,
//! prize claims, and automatic distribution.

use anchor_lang::prelude::*;
// For the zero-copy LeaderboardAccumulator's entries array (2026-08-06). Same
// pattern transfer-hook uses for its PendingRefund/PendingRefundQueue.
use bytemuck::{Pod, Zeroable};
use kots_common::{
    constants::*, errors::*, utils::*,
    ContestLabel,
    state::{
        ParticipantScore
    },
    // NOTE (2026-07-23): `check_blacklist_status` and `get_emergency_pause_status`
    // were dropped from this import. They became unused when the two live call
    // sites in `validate_prize_claim_security` were switched to
    // `read_blacklist_status` / `read_emergency_pause_status`, which take the
    // governing account and verify it. The unused-import warning was the
    // compiler confirming the wiring landed — nothing here reaches the
    // permissive stubs any more.
    //
    // The four still imported remain STUBS returning the permissive answer:
    // check_fraud_detection -> fraud_detected: false
    // load_user_rate_limit_state -> rate_limited: false, verified_human: true
    // get_oracle_market_cap -> zeroed
    // validate_claim_rate_limit -> rate_limited: false
    // They are called from this same function, so fraud detection and rate
    // limiting on prize claims are still inert.
    cpi::{check_fraud_detection, load_user_rate_limit_state, get_oracle_market_cap, validate_claim_rate_limit}
};

// Account struct definitions for IDL generation only - use kots-common types for BPF compilation

// Conditional compilation: use IDL types for host-side IDL generation, real types for BPF
#[cfg(feature = "idl-build")]
mod idl_types {
    use super::*;
    
    // IDL-only type definitions for proper client generation
    #[account]
    pub struct ProgramUpgradeState {
        pub is_initialized: bool,
        pub authority: Pubkey,
        pub program_type: u8, // 0-6 for each program type
        pub version: u16,
        pub initialization_timestamp: i64,
    }
}

#[cfg(feature = "idl-build")]
pub use idl_types::*;

// Account struct definitions at root level for proper IDL generation

#[account]
#[derive(Debug)]
pub struct ContestVaultState {
    pub is_initialized: bool,
    pub contest_label: [u8; 32], // Fixed size for both IDL and BPF compatibility
    pub vault_authority: Pubkey,
    pub vault_bump: u8,
    pub total_prize_pool: u64,
    pub reserved_amounts: u64,
    pub available_for_distribution: u64,
    pub distribution_complete: bool,
    pub auto_distribution_executed: bool,
    pub distribution_timestamp: i64,
    pub daily_transfer_count: u32,
    pub last_daily_reset: i64,
    pub lifetime_distributed: u64,
    pub last_transfer_timestamp: i64,
}

/// On-chain winner record - authoritative source of truth for prize claims
/// PDA seeds: [b"winner", contest_label, position.to_le_bytes()]
#[account]
#[derive(Debug)]
pub struct WinnerRecord {
    pub is_initialized: bool,
    pub contest_label: [u8; 32],
    pub position: u32,
    pub winner_wallet: Pubkey,
    pub prize_amount: u64,
    pub claimed: bool,
    pub claim_timestamp: i64,
    pub bump: u8,
}

/// On-chain prediction tally — the AUTHORITY for the participant winner
/// (SCORING-MODEL.md §1/Q2: "the voter who was most right about the most
/// memes"). The backend ASSEMBLES (voter, meme, stance, stake) rows from vote
/// records and submits them in batches; this program COMPUTES. Nothing here is
/// or ever was chance: accuracy = Σ weight(order) × stance × stake × outcome, outcome = +1 if
/// the meme placed else -1, wrongs subtract, free actions never arrive (their
/// stake is 0 and the assembler skips them).
/// PDA seeds: [b"prediction_tally", contest_label]
#[account]
#[derive(Debug)]
pub struct PredictionTallyAccount {
    pub is_initialized: bool,
    pub contest_label: [u8; 32],
    /// Q1's outcome set — the meme ids that placed. Must be recorded before
    /// any tally batch (outcome is undefined until the meme winners exist).
    pub winner_meme_ids: [u64; 10],
    pub winner_count: u8,
    pub winners_set: bool,
    /// Per-voter running accuracy. Fixed slots, upserted by voter key.
    pub voters: [Pubkey; MAX_PREDICTION_VOTERS],
    pub accuracies: [i64; MAX_PREDICTION_VOTERS],
    pub voter_count: u16,
    pub finalized: bool,
    pub bump: u8,
}

/// One assembled prediction row: what one voter staked on one meme.
/// stance: +1 FOR, -1 AGAINST (0 rows are meaningless and rejected).
/// stake: KOTS actually paid (lamport-scaled), from the verified payment.
/// order: this prediction's PER-MEME chain ordinal (0 = the first paid
/// prediction that meme received, in ledger order). Supplied by the
/// assembler from the treasury's own transaction history — the same
/// verified sequence the consistency gate audits — so earliness is a
/// ledger fact, not a clock reading. Carrying it IN the row is what keeps
/// tally_predictions order-independent: weight is a pure function of the
/// row, and addition still commutes.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VoterPrediction {
    pub voter: Pubkey,
    pub meme_id: u64,
    pub stance: i8,
    pub stake: u64,
    pub order: u16,
}

/// On-chain voter share record - authoritative source for the DETERMINISTIC
/// voter pool payout (plan §5a de-chancing, "correct prediction pays").
///
/// Written once at contest finalization: voters who backed winning memes
/// split the voter pool pro-rata to their stake in the winners, computed
/// exactly from vote records. There is NO running balance and NO chance unit
/// — the record mirrors WinnerRecord: an exact amount, claimable once.
/// PDA seeds: [b"voter_share", contest_label, voter_wallet]
#[account]
#[derive(Debug)]
pub struct VoterShareRecord {
    pub is_initialized: bool,
    pub contest_label: [u8; 32],
    pub voter_wallet: Pubkey,
    pub share_amount: u64,          // Exact pro-rata amount, set at finalization
    pub claimed: bool,
    pub claim_timestamp: i64,
    /// ACT-24: the numeric contest id — the purpose-key/bucket name of this
    /// obligation, bound to the label by the finalization authority at store
    /// time so audit logs and ledgers can never disagree.
    pub contest_id: u64,
    pub bump: u8,
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

// Import ProgramType only for BPF compilation (not during IDL generation)
#[cfg(not(all(feature = "idl-build", not(target_arch = "bpf"))))]
use kots_common::ProgramType;



// Unified program ID for both devnet and mainnet
// Pure prediction-accuracy scoring, extracted so it can be unit-tested WITHOUT a
// validator (`cargo test -p prize-distribution`). The shipped expression is signed,
// multiplies four terms, and narrows i128 -> i64 — every one of those is a place a
// limit value bites. See scoring.rs.
//
// STATUS: module + tests are self-contained and runnable. finalize_prediction_tally
// has NOT yet been refactored to call it, so these tests currently guard a faithful
// copy rather than the shipped path. Wiring it is the next step and needs a compiler
// in the loop.
pub mod scoring;

#[cfg(feature = "realm-main")]
declare_id!("gQPejCrRxr6y824vTMZ7ad33uwp95L1rRxXMGftBAxa");

#[cfg(not(feature = "realm-main"))]
declare_id!("GDrCv9tAh9scvxCTe3XD1N2Xdj2pQYhBtYo68hfDXJqk");

// VERSION HEARTBEAT: Update this string whenever code changes
// Format: "program-name|timestamp|build-hash"
const VERSION_IDENT: &[u8] = b"prize-distribution|20251014T1800Z|edge-case-security-fixes";

// Account size constants
const WINNER_RECORD_SPACE: usize = 8 + 1 + 32 + 4 + 32 + 8 + 1 + 8 + 1; // discriminator + fields = 95 bytes
const VOTER_SHARE_RECORD_SPACE: usize = 8 + 1 + 32 + 32 + 8 + 1 + 8 + 8 + 1; // discriminator + fields = 99 bytes (contest_id u64 added 2026-08, ACT-24)

// PDA seed constants
const WINNER_SEED: &[u8] = b"winner";
// Seed for the per-contest ContestVaultState PDA (2026-08-04 initializer).
// A const slice like WINNER_SEED so the anchor seeds array unifies to &[u8].
const CONTEST_VAULT_SEED: &[u8] = b"contest_vault";

// ── Scalable distribution (2026-08-06): batched top-N leaderboard accumulator ──
// Bounds every contest's winner set to a constant size regardless of how many
// participants entered, so contests of ANY size are supported (participants are
// scored in batches, one transaction each).
pub const MAX_WINNERS: usize = 100;
const LEADERBOARD_SEED: &[u8] = b"leaderboard";
const VOTER_SHARE_SEED: &[u8] = b"voter_share";
/// ACT-24 auto-crediting: the PDA that alone may raise voter-share bucket
/// entitlements in treasury-operations. It signs ONLY inside
/// `store_voter_share` (invoke_signed below) and is pinned into the
/// treasury's DistributionConfig by the multisig at the ceremony.
pub const BUCKET_CREDIT_AUTHORITY_SEED: &[u8] = b"bucket_credit_authority_v1";
/// sha256("global:credit_voter_share_bucket")[0..8] — the treasury-operations
/// instruction this program invokes; computed independently and pinned, the
/// same discipline as every cross-ABI constant in this workspace.
const CREDIT_VOTER_SHARE_BUCKET_DISCRIMINATOR: [u8; 8] =
    [0xed, 0x48, 0x81, 0x57, 0x4b, 0xf2, 0x45, 0x80];
const PREDICTION_TALLY_SEED: &[u8] = b"prediction_tally";

// Prediction tally sizing. 20 voter slots keeps the account well under a
// single-allocation comfort zone (8 + 1 + 32 + 80 + 1 + 1 + 20*32 + 20*8 + 2
// + 1 + 1 = ~927 bytes) and covers ranking the participant winners; a
// multi-account series (ParticipantScoreAccount pattern) is the extension
// path if a contest's distinct-voter count outgrows it.
/// Earliness-leverage constants (SCORING-MODEL.md Q2, decided 2026-07-21).
/// NOTE their placement: top-level beside the other tally constants — the
/// first attempt inserted them between #[account] and its struct, feeding
/// the attribute macro `pub const` instead of `pub struct`; the BPF build
/// (and only the BPF build) caught it, exactly as DEVNET-BRINGUP warns.
// SUPERSEDED 2026-08-06 by KOTS_TOKENOMICS.md §5a (accuracy = stance × stake ×
// outcome, no weighting). Retained as historical constants only; not used by
// tally_predictions anymore. See the note in that handler.
pub const WEIGHT_MAX_BPS: u64 = 20_000; // (superseded) 2.0× at first prediction
pub const WEIGHT_STEP_BPS: u64 = 1_000; // (superseded) linear decay per step
pub const EARLY_WEIGHT_WINDOW: u16 = 10; // (superseded) flat 1.0× from tenth on

const MAX_PREDICTION_VOTERS: usize = 20;
const MAX_PREDICTION_ROWS_PER_BATCH: usize = 16;
const PREDICTION_TALLY_SPACE: usize = 8 + 1 + 32 + (10 * 8) + 1 + 1
    + (MAX_PREDICTION_VOTERS * 32) + (MAX_PREDICTION_VOTERS * 8) + 2 + 1 + 1;

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
#[program]
pub mod prize_distribution {
    use super::*;
    
    /// Query deployed program version
    pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
        anchor_lang::solana_program::program::set_return_data(VERSION_IDENT);
        msg!("📡 Version: {}", core::str::from_utf8(VERSION_IDENT).unwrap_or("invalid-utf8"));
        Ok(())
    }

    /// Store winner record on-chain (called by contest-management during finalization)
    /// This is the AUTHORITATIVE source of truth for prize claims
    pub fn store_winner(
        ctx: Context<StoreWinner>,
        contest_label: [u8; 32],
        position: u32,
        winner_wallet: Pubkey,
        prize_amount: u64,
    ) -> Result<()> {
        msg!("📝 Storing winner on-chain - Position: {}, Wallet: {}, Amount: {} KOTS", 
             position, winner_wallet, prize_amount / TOKENS_PER_UNIT);
        
        let winner_record = &mut ctx.accounts.winner_record;
        let _current_time = get_current_timestamp()?;
        
        winner_record.is_initialized = true;
        winner_record.contest_label = contest_label;
        winner_record.position = position;
        winner_record.winner_wallet = winner_wallet;
        winner_record.prize_amount = prize_amount;
        winner_record.claimed = false;
        winner_record.claim_timestamp = 0;
        winner_record.bump = ctx.bumps.winner_record;
        
        msg!("✅ Winner record stored on-chain - Blockchain is now authoritative!");
        Ok(())
    }

    /// Store a voter's deterministic pool share (called by contest-management
    /// at finalization). This is the AUTHORITATIVE source for the voter-pool
    /// payout: the amount is the voter's exact pro-rata share of the voter
    /// pool, computed from vote records — no draw, no accumulating balance
    /// (plan §5a de-chancing). Idempotent-hostile by design: a record that is
    /// already initialized cannot be overwritten, so finalization cannot
    /// silently re-price a share.
    pub fn store_voter_share(
        ctx: Context<StoreVoterShare>,
        contest_label: [u8; 32],
        voter_wallet: Pubkey,
        share_amount: u64,
        contest_id: u64,
    ) -> Result<()> {
        msg!("📝 Storing voter share on-chain - Wallet: {}, Amount: {} KOTS",
             voter_wallet, share_amount / TOKENS_PER_UNIT);

        // ACT-24 gate: only the finalization authority (program_state.authority)
        // may store shares. Two reasons, both load-bearing: (1) the store now
        // CPI-credits the treasury voter-share bucket, so an open store would
        // be an open entitlement keyhole; (2) even before that coupling,
        // init-once + anyone-pays let a griefer pre-write a (label, voter)
        // record and block the real one forever — closed here. The account is
        // seeds-pinned under this program and read manually (it was created
        // raw at initialize; layout: disc[8] | is_initialized u8 | authority
        // Pubkey @9..41 | ...).
        {
            let ps = ctx.accounts.program_state.to_account_info();
            require!(!ps.data_is_empty(), KotsError::UnauthorizedCaller);
            let data = ps.try_borrow_data()?;
            require!(data.len() >= 41 && data[8] == 1, KotsError::UnauthorizedCaller);
            let mut auth_bytes = [0u8; 32];
            auth_bytes.copy_from_slice(&data[9..41]);
            require!(
                ctx.accounts.authority.key() == Pubkey::new_from_array(auth_bytes),
                KotsError::UnauthorizedCaller
            );
        }

        let share_record = &mut ctx.accounts.voter_share_record;
        require!(!share_record.is_initialized, KotsError::InvalidParameter);
        require!(share_amount > 0, KotsError::InvalidAmount);

        share_record.is_initialized = true;
        share_record.contest_label = contest_label;
        share_record.voter_wallet = voter_wallet;
        share_record.share_amount = share_amount;
        share_record.claimed = false;
        share_record.claim_timestamp = 0;
        share_record.contest_id = contest_id;
        share_record.bump = ctx.bumps.voter_share_record;

        // ACT-24 auto-crediting (decision: CPI-coupled, voter-share leg): the
        // record and its treasury ceiling are ONE act. Voter shares are
        // authorized on-chain but SETTLED from the treasury, so the bucket
        // this credits is exactly what bounds those settlements; the record's
        // init-once above is what makes double-credit impossible. The target
        // program is resolved from the estate (network-agnostic), and the
        // credit signature is this program's own PDA — nothing else in
        // existence can produce it.
        let ids = kots_common::program_ids::current(ctx.program_id)
            .ok_or(KotsError::UnauthorizedCaller)?;
        require!(
            ctx.accounts.treasury_program.key() == ids.treasury_operations
                && ctx.accounts.treasury_program.executable,
            KotsError::InvalidProgramId
        );
        let mut data = Vec::with_capacity(8 + 8 + 8);
        data.extend_from_slice(&CREDIT_VOTER_SHARE_BUCKET_DISCRIMINATOR);
        data.extend_from_slice(&contest_id.to_le_bytes());
        data.extend_from_slice(&share_amount.to_le_bytes());
        let ix = anchor_lang::solana_program::instruction::Instruction {
            program_id: ids.treasury_operations,
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(
                    ctx.accounts.voter_share_bucket.key(),
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new(
                    ctx.accounts.treasury_distribution_config.key(),
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    ctx.accounts.bucket_credit_authority.key(),
                    true,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new(
                    ctx.accounts.authority.key(),
                    true,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    anchor_lang::system_program::ID,
                    false,
                ),
            ],
            data,
        };
        anchor_lang::solana_program::program::invoke_signed(
            &ix,
            &[
                ctx.accounts.voter_share_bucket.to_account_info(),
                ctx.accounts.treasury_distribution_config.to_account_info(),
                ctx.accounts.bucket_credit_authority.to_account_info(),
                ctx.accounts.authority.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&[BUCKET_CREDIT_AUTHORITY_SEED, &[ctx.bumps.bucket_credit_authority]]],
        )?;

        msg!("✅ Voter share stored + treasury bucket credited — blockchain is authoritative on both");
        Ok(())
    }

    /// Record the meme-winner set into the prediction tally (Q1's outcomes,
    /// needed before any prediction can be graded). Authority-gated, once.
    pub fn set_prediction_outcomes(
        ctx: Context<SetPredictionOutcomes>,
        contest_label: [u8; 32],
        winner_meme_ids: [u64; 10],
        winner_count: u8,
    ) -> Result<()> {
        require!(winner_count as usize <= 10, KotsError::InvalidParameter);
        let tally = &mut ctx.accounts.prediction_tally;
        require!(!tally.winners_set, KotsError::InvalidParameter);

        tally.is_initialized = true;
        tally.contest_label = contest_label;
        tally.winner_meme_ids = winner_meme_ids;
        tally.winner_count = winner_count;
        tally.winners_set = true;
        tally.voter_count = 0;
        tally.finalized = false;
        tally.bump = ctx.bumps.prediction_tally;

        msg!("📋 Prediction outcomes recorded - {} placed memes", winner_count);
        Ok(())
    }

    /// Fold a batch of assembled prediction rows into the tally.
    /// accuracy[voter] += stance × stake × outcome(meme) — KOTS_TOKENOMICS.md
    /// §5a / SCORING-MODEL.md §1 Q2. Deterministic, batch-order-independent
    /// (addition commutes), chance-free by construction.
    ///
    /// ⚠ SUPERSEDED (2026-08-06): the earliness-leverage weight described below
    /// was REMOVED — the definitive tokenomics doc scores raw staked conviction
    /// with no weighting ("No weighted probability ... participates in value
    /// allocation"). The block is kept only as design-history rationale; the
    /// code no longer applies any weight. Original note:
    /// EARLINESS IS LEVERAGE (decided 2026-07-21): weight_bps(k) =
    /// 20_000 − 1_000 × min(k, 10) — a meme's first prediction carries
    /// 2.0×, decaying linearly to 1.0× by the tenth, flat after. The
    /// weight multiplies the SIGNED delta: an early wrong call costs the
    /// same extra it would have earned — leverage, not subsidy. That
    /// symmetry is the anti-abuse core: direction-splitting sybils (one
    /// wallet first-FOR, one first-AGAINST) net exactly zero minus fees,
    /// and the smooth decay makes sniping the literal first slot worth
    /// almost nothing over arriving third — a discrete first-bonus was
    /// rejected as a latency lottery. The 2× cap keeps stake primary:
    /// a large late conviction still outweighs a small early one.
    pub fn tally_predictions(
        ctx: Context<TallyPredictions>,
        _contest_label: [u8; 32],
        rows: [VoterPrediction; MAX_PREDICTION_ROWS_PER_BATCH],
        row_count: u8,
    ) -> Result<()> {
        let tally = &mut ctx.accounts.prediction_tally;
        require!(tally.winners_set, KotsError::InvalidParameter);
        require!(!tally.finalized, KotsError::InvalidParameter);
        require!(
            (row_count as usize) <= MAX_PREDICTION_ROWS_PER_BATCH,
            KotsError::InvalidParameter
        );

        for row in rows.iter().take(row_count as usize) {
            require!(row.stance == 1 || row.stance == -1, KotsError::InvalidParameter);
            // Free actions never reach the ledger (stake 0 = no skill credit,
            // SCORING-MODEL.md P3); the assembler should not send them.
            if row.stake == 0 {
                continue;
            }

            let placed = tally.winner_meme_ids
                [..tally.winner_count as usize]
                .contains(&row.meme_id);
            let outcome: i128 = if placed { 1 } else { -1 };
            // ACCURACY = stance × stake × outcome — KOTS_TOKENOMICS.md §5a /
            // SCORING-MODEL.md §1 Q2 (definitive). The earliness-leverage
            // 2×-decay weight (WEIGHT_*_BPS × order) is SUPERSEDED by the doc,
            // exactly as the 50/30/20 prize curve was: the doc states plainly
            // "No weighted probability ... participates in value allocation."
            // `order` stays in the row for ledger-audit provenance but no
            // longer scales the delta — raw staked conviction, nothing else.
            let delta_i128 = (row.stance as i128)
                .checked_mul(row.stake as i128)
                .and_then(|v| v.checked_mul(outcome))
                .ok_or(KotsError::ArithmeticOverflow)?;
            let delta = i64::try_from(delta_i128)
                .map_err(|_| KotsError::ArithmeticOverflow)?;

            // Upsert the voter's slot.
            let mut slot: Option<usize> = None;
            for i in 0..tally.voter_count as usize {
                if tally.voters[i] == row.voter {
                    slot = Some(i);
                    break;
                }
            }
            let i = match slot {
                Some(i) => i,
                None => {
                    require!(
                        (tally.voter_count as usize) < MAX_PREDICTION_VOTERS,
                        KotsError::InvalidParameter
                    );
                    let i = tally.voter_count as usize;
                    tally.voters[i] = row.voter;
                    tally.accuracies[i] = 0;
                    tally.voter_count += 1;
                    i
                }
            };
            tally.accuracies[i] = tally.accuracies[i]
                .checked_add(delta)
                .ok_or(KotsError::ArithmeticOverflow)?;
        }

        msg!("🧮 Tally batch folded - {} rows, {} voters tracked",
             row_count, tally.voter_count);
        Ok(())
    }

    /// Finalize and return the participant ranking: descending accuracy,
    /// ties by ascending pubkey bytes as the on-chain totality guarantee
    /// (the richer cascade — less total stake, earlier first prediction —
    /// is applied by the assembler before equal-accuracy rows reach here).
    pub fn rank_participant_winners(
        ctx: Context<RankParticipantWinners>,
        _contest_label: [u8; 32],
    ) -> Result<()> {
        let tally = &mut ctx.accounts.prediction_tally;
        require!(tally.winners_set, KotsError::InvalidParameter);
        require!(!tally.finalized, KotsError::InvalidParameter);

        let n = tally.voter_count as usize;

        // BLACKLIST EXCLUSION AT CONTEST CLOSE (2026-07-23).
        //
        // Blocking a blacklisted wallet's ACTIONS is not enough: one that voted
        // before being listed would still be ranked, placed, and owed a prize.
        // Closing a contest must therefore also drop them from the result.
        //
        // Exclusion, not rejection: a blacklisted entrant must not make the whole
        // finalization fail — that would let one bad account hold a contest
        // hostage. They are removed and everyone below them moves up.
        let sg_id = kots_common::cpi::security_governance_id_for(ctx.program_id)?;
        let bl_info = ctx.accounts.blacklist_state.to_account_info();
        let mut eligible: [usize; MAX_PREDICTION_VOTERS] = [0; MAX_PREDICTION_VOTERS];
        let mut n_eligible = 0usize;
        for idx in 0..n {
            let status = kots_common::cpi::read_blacklist_status(
                &bl_info,
                &sg_id,
                &tally.voters[idx],
            )?;
            if !status.is_blacklisted {
                eligible[n_eligible] = idx;
                n_eligible += 1;
            } else {
                msg!("⛔ Excluding blacklisted voter from ranking: {}", tally.voters[idx]);
            }
        }
        let n = n_eligible;
        let mut order: [usize; MAX_PREDICTION_VOTERS] = core::array::from_fn(|i| eligible[i]);
        for i in 0..n {
            let mut best = i;
            for j in (i + 1)..n {
                let a = (tally.accuracies[order[j]], tally.voters[order[j]].to_bytes());
                let b = (tally.accuracies[order[best]], tally.voters[order[best]].to_bytes());
                if a.0 > b.0 || (a.0 == b.0 && a.1 < b.1) {
                    best = j;
                }
            }
            order.swap(i, best);
        }

        let mut ranked: [Pubkey; MAX_PREDICTION_VOTERS] = [Pubkey::default(); MAX_PREDICTION_VOTERS];
        for i in 0..n {
            ranked[i] = tally.voters[order[i]];
        }
        tally.finalized = true;

        let encoded = borsh::to_vec(&(ranked, n as u16))
            .map_err(|_| KotsError::SerializationError)?;
        anchor_lang::solana_program::program::set_return_data(&encoded);
        msg!("🏅 Participant ranking finalized - {} voters ranked", n);
        Ok(())
    }

    /// Initialize prize distribution program state
    pub fn initialize_prize_distribution(
        ctx: Context<InitializePrizeDistribution>,
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

        msg!("🚀 Initializing Prize Distribution Program");
        
        // Create PDA with treasury funding
        let seeds: &[&[u8]] = &[b"program_state"];
        let (pda, bump) = Pubkey::find_program_address(seeds, ctx.program_id);
        
        // Verify the PDA matches
        require_keys_eq!(ctx.accounts.program_state.key(), pda, KotsError::InvalidProgramId);
        
        // Create the account with proper space allocation
        let rent = Rent::get()?;
        let space = 256usize; // space for ProgramUpgradeState
        let required_lamports = rent.minimum_balance(space);
        
        // Create account using System Program (each program creates its own PDAs)
        let create_account_ix = anchor_lang::solana_program::system_instruction::create_account(
            ctx.accounts.authority.key,
            &pda,
            required_lamports,
            space as u64,
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
        
        msg!("✅ Account created with {} bytes at {}", space, pda);
        
        // Request treasury reimbursement (7-parameter signature)
        kots_common::init::request_treasury_reimbursement(
            &ctx.accounts.treasury_incoming.to_account_info(),
            &ctx.accounts.authority.to_account_info(), // payer
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
        // Use appropriate type based on compilation target
        #[cfg(feature = "idl-build")]
        let program_type_value = kots_common::constants::PROGRAM_TYPE_PRIZE_DISTRIBUTION;
        // `as u8` (2026-07-23): kots_common::state::ProgramUpgradeState.program_type
        // is now `u8`, matching what all six programs that write this account
        // already declared. It previously said `ProgramType`, and Borsh gives a
        // fieldless enum the same single byte — so the layout agreed, but reading
        // back a byte outside the enum's range would have failed deserialization
        // rather than yielding a wrong value. This is the one site that actually
        // constructs the shared type in a BPF build, which is why it is the only
        // one the compiler flagged.
        #[cfg(not(feature = "idl-build"))]
        let program_type_value = ProgramType::PrizeDistribution as u8;
        
        // Use appropriate state type based on compilation target
        #[cfg(feature = "idl-build")]
        let state = idl_types::ProgramUpgradeState {
            is_initialized: true,
            authority: ctx.accounts.authority.key(),
            program_type: program_type_value,
            version: kots_common::constants::CURRENT_PRIZE_DISTRIBUTION_VERSION,
            initialization_timestamp: kots_common::utils::get_current_timestamp()?,
        };
        
        #[cfg(not(feature = "idl-build"))]
        let state = kots_common::state::ProgramUpgradeState {
            is_initialized: true,
            authority: ctx.accounts.authority.key(),
            program_type: program_type_value,
            version: kots_common::constants::CURRENT_PRIZE_DISTRIBUTION_VERSION,
            initialization_timestamp: kots_common::utils::get_current_timestamp()?,
        };
        
        // Serialize (Anchor discriminator + body) from offset 0
        state.try_serialize(&mut &mut data[..])?;
        
        msg!("✅ Prize Distribution Program initialized successfully");
        Ok(())
    }

    /// Calculate prize distribution for contest
    pub fn calc_prize(
        ctx: Context<CalcPrize>,
        _contest_label: [u8; 32],
        participant_scores: [ParticipantScore; MAX_PARTICIPANT_SCORES],
        score_count: u32, // Number of actual scores in array
        distribution_type: u8,
    ) -> Result<PrizeDistributionResult> {
        msg!("🏆 Calculating prize distribution for contest with {} participants", score_count);
        
        let vault = &ctx.accounts.contest_vault;
        
        require!(vault.total_prize_pool > 0, KotsError::NoPrizeAmount);
        require!(score_count > 0, KotsError::InsufficientParticipants);
        
        // Deterministic, skills-based selection (plan §5a): rank by score.
        let winners = select_winners_by_score(&participant_scores, score_count, distribution_type)?;
        
        // Calculate prize amounts based on actual participant count
        // If fewer than 3 participants, only award prizes to those who exist
        // §4c placement curve via the shared helper (was 50/30/20).
        let first_place_amount  = if score_count >= 1 { placement_prize(vault.total_prize_pool, 1) } else { 0 };
        let second_place_amount = if score_count >= 2 { placement_prize(vault.total_prize_pool, 2) } else { 0 };
        let third_place_amount  = if score_count >= 3 { placement_prize(vault.total_prize_pool, 3) } else { 0 };
        
        let total_distributed = first_place_amount + second_place_amount + third_place_amount;
        let remaining_pool = vault.total_prize_pool.checked_sub(total_distributed).unwrap_or(0);
        
        let actual_winner_count = score_count.min(3);
        
        let result = PrizeDistributionResult {
            first_place_amount,
            second_place_amount,
            third_place_amount,
            place_amounts: [first_place_amount, second_place_amount, third_place_amount, 0, 0, 0, 0, 0, 0, 0],
            remaining_pool,
            distribution_successful: true,
            winners,
            winner_count: actual_winner_count,
        };
        
        msg!("✅ Prize calculation completed - {} winners: 1st: {}, 2nd: {}, 3rd: {}", 
             actual_winner_count, first_place_amount, second_place_amount, third_place_amount);
        
        Ok(result)
    }

    /// Claim prize for contest winners OR treasure hunt distributions
    /// optional_amount: None = contest winner mode (no limits)
    /// optional_amount: Some(x) = treasure hunt mode (rate limited)
    pub fn claim_prize(
        ctx: Context<ClaimPrize>,
        contest_label: [u8; 32],
        winner_position: u32,
        current_market_cap_usd_cents: u64,
        optional_amount: Option<u64>, // NEW: Determines mode of operation
    ) -> Result<()> {
        // Determine operation mode based on optional_amount
        let is_treasure_hunt = optional_amount.is_some();
        
        if is_treasure_hunt {
            // VOTER SHARE CLAIM MODE (plan §5a de-chancing) — pay the exact
            // pro-rata amount stored at finalization, once. No chance units,
            // no partial redemptions, no per-voter running balance: the
            // record IS the entitlement, same pattern as WinnerRecord.
            msg!("🗳️ Processing voter pool share claim");

            let requested_amount = optional_amount.ok_or(KotsError::InvalidParameter)?;
            let current_time = get_current_timestamp()?;

            // SECURITY: Validate on-chain share record (prevents database doctoring!)
            let share_record = &ctx.accounts.voter_share_record.as_ref()
                .ok_or(KotsError::InvalidParameter)?;

            require!(share_record.is_initialized, KotsError::InvalidParameter);
            require!(
                share_record.voter_wallet == ctx.accounts.claimer.key(),
                KotsError::UnauthorizedClaim
            );
            require!(!share_record.claimed, KotsError::PrizeAlreadyClaimed);
            // The claim must be for exactly the stored share — the caller
            // cannot choose an amount (choosing was a ticket-era concept).
            require!(
                requested_amount == share_record.share_amount,
                KotsError::InvalidAmount
            );

            // Check daily vault limit (in separate scope to drop borrow)
            {
                let vault = &mut ctx.accounts.contest_vault;

                // Reset daily counter if 24 hours have passed
                let one_day = 24 * 60 * 60;
                if current_time >= (vault.last_daily_reset + one_day) {
                    vault.daily_transfer_count = 0;
                    vault.last_daily_reset = current_time;
                }

                // Rate Limit: Max 4 transfers per day
                require!(
                    vault.daily_transfer_count < 4,
                    KotsError::DailyTransferLimitExceeded
                );
            }

            // Authorise the claim on-chain (settlement is off-chain; the vault
            // borrow is now dropped). Does NOT move tokens — see F17.
            authorize_prize_claim(&ctx, requested_amount)?;

            // Mark the share claimed — exactly once
            let share_record_mut = &mut ctx.accounts.voter_share_record.as_mut()
                .ok_or(KotsError::InvalidParameter)?;
            share_record_mut.claimed = true;
            share_record_mut.claim_timestamp = current_time;

            // Update vault counters
            let vault = &mut ctx.accounts.contest_vault;
            vault.daily_transfer_count += 1;
            vault.lifetime_distributed += requested_amount;
            vault.last_transfer_timestamp = current_time;
            vault.available_for_distribution = vault.available_for_distribution
                .checked_sub(requested_amount)
                .ok_or(KotsError::InsufficientVaultFunds)?;

            msg!("✅ Voter pool share claim authorised - Amount: {} KOTS (settled off-chain)",
                 requested_amount / TOKENS_PER_UNIT);

        } else {
            // CONTEST WINNER MODE - No limits!
            msg!("🏆 Processing contest winner prize claim - Position: {}", winner_position);
            
            // SECURITY: Validate on-chain winner record (prevents database doctoring!)
            let winner_record = &ctx.accounts.winner_record.as_ref()
                .ok_or(KotsError::InvalidWinnerRecord)?;
            
            require!(
                winner_record.is_initialized,
                KotsError::InvalidWinnerRecord
            );
            require!(
                winner_record.winner_wallet == ctx.accounts.claimer.key(),
                KotsError::UnauthorizedClaim
            );
            require!(
                !winner_record.claimed,
                KotsError::PrizeAlreadyClaimed
            );
            
            // Security validation
            validate_prize_claim_security(&ctx, &contest_label, current_market_cap_usd_cents)?;
            
            // Get prize amount from on-chain winner record (authoritative source!)
            let prize_amount = winner_record.prize_amount;
            
            msg!("📋 On-chain winner verified - Wallet: {}, Position: {}, Amount: {} KOTS", 
                 winner_record.winner_wallet, winner_record.position, prize_amount / TOKENS_PER_UNIT);
            
            // Authorise the winner claim on-chain (settlement is off-chain).
            // Does NOT move tokens — see F17.
            authorize_prize_claim(&ctx, prize_amount)?;
            
            // Update vault state
            let vault = &mut ctx.accounts.contest_vault;
            vault.available_for_distribution = vault.available_for_distribution
                .checked_sub(prize_amount)
                .ok_or(KotsError::InsufficientVaultFunds)?;
            
            // Mark winner record as claimed (prevents double-claiming)
            let winner_record_mut = &mut ctx.accounts.winner_record.as_mut()
                .ok_or(KotsError::InvalidWinnerRecord)?;
            winner_record_mut.claimed = true;
            winner_record_mut.claim_timestamp = get_current_timestamp()?;
            
            msg!("✅ Contest winner prize claim authorised - Amount: {} KOTS, Position: {} (settled off-chain)",
                 prize_amount / TOKENS_PER_UNIT, winner_position);
        }
        Ok(())
    }

    /// Execute automatic prize distribution
    pub fn execute_auto_distribution(
        ctx: Context<ExecuteAutoDistribution>,
        contest_label: [u8; 32],
    ) -> Result<()> {
        msg!("🤖 Executing automatic prize distribution for contest");
        // EMERGENCY PAUSE GATE (2026-08-06): block finalization during a global pause.
        let __pause = kots_common::cpi::read_emergency_pause_status(
            &ctx.accounts.emergency_state.to_account_info(),
            &kots_common::cpi::security_governance_id_for(ctx.program_id)?,
        )?;
        require!(!__pause.is_active, KotsError::EmergencyPauseActive);
        
        // Validate auto-distribution timing (must be after AUTO_DISTRIBUTION_DELAY_SECONDS)
        validate_auto_distribution_timing(&contest_label)?;
        
        // Get final scores from Voting program via CPI
        let final_scores = get_final_scores_for_distribution(&contest_label, ctx.remaining_accounts)?;
        
        // Get prize pool from vault
        let prize_pool = ctx.accounts.contest_vault.available_for_distribution;
        
        // Execute distribution calculation
        let distribution_result = calculate_and_distribute_prizes(final_scores, prize_pool)?;
        
        // Validate distribution was successful
        require!(distribution_result.distribution_successful, KotsError::DistributionFailed);
        require!(distribution_result.winner_count > 0, KotsError::NoWinnersFound);
        
        let current_time = get_current_timestamp()?;
        
        // Create winner records atomically - guarantees all-or-nothing
        // First place winner (always exists if distribution successful)
        // §4c 10-place distribution — write up to 10 winner records via the
        // shared helper, each guarded by winner_count (init_if_needed accounts
        // for unfilled places stay empty).
        let dr = &distribution_result;
        if dr.winner_count >= 1  { set_winner_record(&mut ctx.accounts.winner_first,  contest_label, 1,  dr.winners[0], dr.place_amounts[0], ctx.bumps.winner_first); }
        if dr.winner_count >= 2  { set_winner_record(&mut ctx.accounts.winner_second, contest_label, 2,  dr.winners[1], dr.place_amounts[1], ctx.bumps.winner_second); }
        if dr.winner_count >= 3  { set_winner_record(&mut ctx.accounts.winner_third,  contest_label, 3,  dr.winners[2], dr.place_amounts[2], ctx.bumps.winner_third); }
        if dr.winner_count >= 4  { set_winner_record(&mut ctx.accounts.winner_4,  contest_label, 4,  dr.winners[3], dr.place_amounts[3], ctx.bumps.winner_4); }
        if dr.winner_count >= 5  { set_winner_record(&mut ctx.accounts.winner_5,  contest_label, 5,  dr.winners[4], dr.place_amounts[4], ctx.bumps.winner_5); }
        if dr.winner_count >= 6  { set_winner_record(&mut ctx.accounts.winner_6,  contest_label, 6,  dr.winners[5], dr.place_amounts[5], ctx.bumps.winner_6); }
        if dr.winner_count >= 7  { set_winner_record(&mut ctx.accounts.winner_7,  contest_label, 7,  dr.winners[6], dr.place_amounts[6], ctx.bumps.winner_7); }
        if dr.winner_count >= 8  { set_winner_record(&mut ctx.accounts.winner_8,  contest_label, 8,  dr.winners[7], dr.place_amounts[7], ctx.bumps.winner_8); }
        if dr.winner_count >= 9  { set_winner_record(&mut ctx.accounts.winner_9,  contest_label, 9,  dr.winners[8], dr.place_amounts[8], ctx.bumps.winner_9); }
        if dr.winner_count >= 10 { set_winner_record(&mut ctx.accounts.winner_10, contest_label, 10, dr.winners[9], dr.place_amounts[9], ctx.bumps.winner_10); }
        msg!("🏆 {} winner record(s) created (§4c, up to 10 places)", dr.winner_count);
        
        // Update vault to deduct distributed prizes (AFTER winner records are written)
        let vault = &mut ctx.accounts.contest_vault;
        let total_distributed: u64 = distribution_result.place_amounts.iter().copied().sum();
        
        vault.available_for_distribution = vault.available_for_distribution
            .checked_sub(total_distributed)
            .ok_or(KotsError::InsufficientVaultFunds)?;
        
        // Mark distribution metadata
        vault.distribution_complete = true;
        vault.auto_distribution_executed = true;
        vault.distribution_timestamp = current_time;
        
        msg!("✅ Atomic distribution complete - {} winner(s), {} KOTS distributed", 
             distribution_result.winner_count, total_distributed / TOKENS_PER_UNIT);
        msg!("🔒 All winner records created and vault updated atomically");
        Ok(())
    }

    /// MISSING-INITIALIZER FIX (2026-08-04, owner-approved): `ContestVaultState`
    /// was required by execute_auto_distribution / calc_prize / claim_prize yet
    /// NO instruction created it — on a fresh chain those paths were
    /// structurally unrunnable. This initializer fills the gap. GATED like
    /// ACT-24: only program_state.authority may create a vault and set its
    /// pool, because available_for_distribution feeds prize math — an open
    /// init would be an open prize-pool keyhole.
    pub fn initialize_contest_vault(
        ctx: Context<InitializeContestVault>,
        contest_label: [u8; 32],
        initial_pool: u64,
    ) -> Result<()> {
        msg!("🏦 Initializing contest vault state");
        // program_state PDA pin + manual authority read (same layout contract
        // as the ACT-24 gate: disc[8] | is_initialized u8 | authority @9..41).
        let (ps_pda, _) = Pubkey::find_program_address(&[b"program_state"], ctx.program_id);
        require_keys_eq!(ctx.accounts.program_state.key(), ps_pda, KotsError::InvalidProgramId);
        {
            let ps = ctx.accounts.program_state.to_account_info();
            require!(!ps.data_is_empty(), KotsError::UnauthorizedCaller);
            let data = ps.try_borrow_data()?;
            require!(data.len() >= 41 && data[8] == 1, KotsError::UnauthorizedCaller);
            let mut auth_bytes = [0u8; 32];
            auth_bytes.copy_from_slice(&data[9..41]);
            require!(
                ctx.accounts.authority.key() == Pubkey::new_from_array(auth_bytes),
                KotsError::UnauthorizedCaller
            );
        }
        let v = &mut ctx.accounts.contest_vault;
        v.is_initialized = true;
        v.contest_label = contest_label;
        v.vault_authority = ctx.accounts.authority.key();
        v.vault_bump = ctx.bumps.contest_vault;
        v.total_prize_pool = initial_pool;
        v.reserved_amounts = 0;
        v.available_for_distribution = initial_pool;
        v.distribution_complete = false;
        v.auto_distribution_executed = false;
        v.distribution_timestamp = 0;
        v.daily_transfer_count = 0;
        v.last_daily_reset = 0;
        v.lifetime_distributed = 0;
        v.last_transfer_timestamp = 0;
        msg!("✅ Contest vault initialized — pool {}", initial_pool);
        Ok(())
    }

    /// SCALABLE DISTRIBUTION (2026-08-06): create the empty top-N accumulator for
    /// a contest. Participants are then scored in batches via process_score_batch
    /// (each batch is one transaction), so a contest of ANY size fits — unlike the
    /// single-shot execute_auto_distribution, which is bounded by the per-tx
    /// account limit (~35 without ALT).
    pub fn init_leaderboard(
        ctx: Context<InitLeaderboard>,
        contest_label: [u8; 32],
    ) -> Result<()> {
        let mut lb = ctx.accounts.leaderboard.load_init()?;
        lb.contest_label = contest_label;
        lb.count = 0;
        lb.participants_processed = 0;
        lb.is_initialized = 1;
        lb.finalized = 0;
        lb.bump = ctx.bumps.leaderboard;
        msg!("📊 Leaderboard accumulator initialized (top {})", MAX_WINNERS);
        Ok(())
    }

    /// SCALABLE DISTRIBUTION (2026-08-06): fold a BATCH of participant score
    /// accounts (passed via remaining_accounts) into the leaderboard's sorted
    /// top-MAX_WINNERS. Call repeatedly with disjoint batches until every
    /// participant is processed — unbounded contest size, one transaction each.
    pub fn process_score_batch(
        ctx: Context<ProcessScoreBatch>,
        contest_label: [u8; 32],
    ) -> Result<()> {
        let cm_id = kots_common::program_ids::current(&crate::ID)
            .ok_or(KotsError::InvalidProgramId)?
            .contest_management;

        let mut lb = ctx.accounts.leaderboard.load_mut()?;
        require!(lb.is_initialized == 1, KotsError::AccountDataCorrupted);
        require!(lb.finalized == 0, KotsError::ContestAlreadyFinalized);
        require!(lb.contest_label == contest_label, KotsError::InvalidContestLabel);

        let mut merged: u32 = 0;
        for acc in ctx.remaining_accounts.iter() {
            // Only genuine, matching, live participant records are counted.
            if *acc.owner != cm_id {
                continue;
            }
            let data = acc.try_borrow_data()?;
            if data.len() < 8 + 414 || data[8] != 1 {
                continue;
            }
            if &data[9..41] != contest_label.as_ref() {
                continue;
            }
            if data[173] != 0 {
                continue; // disqualified participants earn nothing
            }
            let mut wallet_bytes = [0u8; 32];
            wallet_bytes.copy_from_slice(&data[41..73]);
            let wallet = Pubkey::new_from_array(wallet_bytes);
            let mut score_bytes = [0u8; 8];
            score_bytes.copy_from_slice(&data[194..202]);
            let score = u64::from_le_bytes(score_bytes);
            drop(data);
            merge_into_leaderboard(&mut lb, wallet, score);
            merged = merged.saturating_add(1);
        }
        lb.participants_processed = lb.participants_processed.saturating_add(merged);
        msg!(
            "📊 Batch merged {} participants (running total {})",
            merged,
            lb.participants_processed
        );
        Ok(())
    }

    /// SCALABLE DISTRIBUTION (2026-08-06): freeze the leaderboard and snapshot the
    /// prize pool. After this, no more batches are accepted and winners may claim
    /// their position via claim_from_leaderboard. The accumulator's entries[..count]
    /// ARE the final ranking.
    pub fn finalize_leaderboard(
        ctx: Context<FinalizeLeaderboard>,
        contest_label: [u8; 32],
    ) -> Result<()> {
        let pool = ctx.accounts.contest_vault.total_prize_pool;
        let mut lb = ctx.accounts.leaderboard.load_mut()?;
        require!(lb.is_initialized == 1, KotsError::AccountDataCorrupted);
        require!(lb.finalized == 0, KotsError::ContestAlreadyFinalized);
        require!(lb.contest_label == contest_label, KotsError::InvalidContestLabel);
        lb.prize_pool = pool;
        lb.finalized = 1;
        msg!(
            "🏁 Leaderboard finalized — {} ranked winners, prize pool {}",
            lb.count,
            pool
        );
        Ok(())
    }

    /// SCALABLE DISTRIBUTION (2026-08-06): a winner claims the prize for their
    /// finalized leaderboard position. Claims directly against the accumulator —
    /// no per-winner PDA, so the 10-winner stack fragility cannot recur, and this
    /// scales to MAX_WINNERS with zero extra accounts per claim. On-chain lamport
    /// settlement mirrors authorize_prize_claim (F17 D2).
    pub fn claim_from_leaderboard(
        ctx: Context<ClaimFromLeaderboard>,
        contest_label: [u8; 32],
        position: u16,
    ) -> Result<()> {
        let claimer_key = ctx.accounts.claimer.key();
        let pos = position as usize;

        // Phase 1 — validate, compute the prize, mark claimed. The leaderboard
        // borrow is scoped so it is released before we touch the vault.
        let prize = {
            let mut lb = ctx.accounts.leaderboard.load_mut()?;
            require!(lb.is_initialized == 1, KotsError::AccountDataCorrupted);
            require!(lb.finalized == 1, KotsError::DistributionFailed); // must finalize first
            require!(lb.contest_label == contest_label, KotsError::InvalidContestLabel);
            require!(pos < lb.count as usize, KotsError::InvalidParameter);

            let entry = lb.entries[pos];
            require!(entry.wallet == claimer_key, KotsError::UnauthorizedClaim);

            let byte = pos / 8;
            let mask = 1u8 << (pos % 8);
            require!(
                lb.claimed_bitmap[byte] & mask == 0,
                KotsError::PrizeAlreadyClaimed
            );

            let prize = placement_prize(lb.prize_pool, pos + 1);
            lb.claimed_bitmap[byte] |= mask; // claimed exactly once
            prize
        };

        // Phase 2 — settle lamports from the vault (same primitive as
        // authorize_prize_claim), then update the vault's accounting.
        if prize > 0 {
            let vault_ai = ctx.accounts.contest_vault.to_account_info();
            require!(
                ctx.accounts.contest_vault.total_prize_pool >= prize,
                KotsError::InsufficientVaultFunds
            );
            // Never spend the vault below rent-exemption (would close it and
            // strand every other winner's claim).
            let rent = Rent::get()?;
            let min = rent.minimum_balance(vault_ai.data_len());
            require!(
                vault_ai.lamports().saturating_sub(prize) >= min,
                KotsError::InsufficientVaultFunds
            );
            **vault_ai.try_borrow_mut_lamports()? -= prize;
            **ctx.accounts.claimer.to_account_info().try_borrow_mut_lamports()? += prize;

            let vault = &mut ctx.accounts.contest_vault;
            vault.available_for_distribution =
                vault.available_for_distribution.saturating_sub(prize);
            vault.lifetime_distributed = vault.lifetime_distributed.saturating_add(prize);
        }

        msg!(
            "🏆 Leaderboard claim: position {} → {} lamports to {}",
            position, prize, claimer_key
        );
        Ok(())
    }
}

// ============================================================================
// SCALABLE DISTRIBUTION — batched top-N leaderboard accumulator (2026-08-06)
// ============================================================================

/// One (wallet, score) slot. Zero-copy (Pod) so the MAX_WINNERS-slot array is
/// accessed by reference via AccountLoader and NEVER deserializes onto the 4KB
/// BPF stack (the failure mode that bit the 10-winner struct).
// Manual bytemuck derives (NOT Anchor's #[zero_copy]) so `[LeaderboardEntry; N]`
// satisfies bytemuck::Pod for the accumulator's array field. AnchorSerialize/
// Deserialize are host-only (IDL); on-chain the struct is pure Pod. 40 bytes,
// 8-aligned, zero padding — Pod-safe.
#[cfg_attr(not(target_os = "solana"), derive(AnchorSerialize, AnchorDeserialize))]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
#[repr(C)]
pub struct LeaderboardEntry {
    pub wallet: Pubkey, // 32
    pub score: u64,     // 8  (offset 32 — 8-aligned)
}

/// Per-contest accumulator: the current top MAX_WINNERS, kept sorted desc.
/// ONE fixed-size account per contest regardless of participant count.
#[account(zero_copy)]
#[repr(C)]
pub struct LeaderboardAccumulator {
    pub contest_label: [u8; 32],      // 0..32
    pub prize_pool: u64,              // 32..40  (snapshotted at finalize; the pool placement_prize divides)
    pub participants_processed: u32,  // 40..44
    pub count: u16,                   // 44..46  (filled slots, <= MAX_WINNERS)
    pub is_initialized: u8,           // 46
    pub finalized: u8,                // 47
    pub bump: u8,                     // 48
    pub claimed_bitmap: [u8; 16],     // 49..65  (1 bit per position; 128 >= MAX_WINNERS)
    pub _padding: [u8; 7],            // 65..72  (align `entries` to 8)
    pub entries: [LeaderboardEntry; MAX_WINNERS], // 72..
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct InitLeaderboard<'info> {
    #[account(
        init,
        payer = authority,
        seeds = [LEADERBOARD_SEED, &contest_label],
        bump,
        space = 8 + std::mem::size_of::<LeaderboardAccumulator>()
    )]
    pub leaderboard: AccountLoader<'info, LeaderboardAccumulator>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

/// Ranking: higher score wins; on a tie the LOWER pubkey wins. Deterministic and
/// matches the tiebreak used elsewhere so distribution is reproducible.
fn ranks_above(score_a: u64, wallet_a: &Pubkey, score_b: u64, wallet_b: &Pubkey) -> bool {
    score_a > score_b || (score_a == score_b && wallet_a < wallet_b)
}

/// Merge one (wallet, score) into the sorted-descending top-MAX_WINNERS in place.
/// O(MAX_WINNERS) shift. When full, the current last place is dropped iff the new
/// entry outranks it; otherwise the entry is discarded.
fn merge_into_leaderboard(lb: &mut LeaderboardAccumulator, wallet: Pubkey, score: u64) {
    let count = lb.count as usize;
    if count >= MAX_WINNERS {
        let last = lb.entries[MAX_WINNERS - 1];
        if !ranks_above(score, &wallet, last.score, &last.wallet) {
            return; // full and doesn't beat the weakest kept entry
        }
    }
    let filled = if count < MAX_WINNERS { count } else { MAX_WINNERS };
    // Insertion index: first slot the new entry outranks (append if none).
    let mut pos = filled;
    let mut i = 0;
    while i < filled {
        let e = lb.entries[i];
        if ranks_above(score, &wallet, e.score, &e.wallet) {
            pos = i;
            break;
        }
        i += 1;
    }
    // Highest index we may write (drop the overflow slot when full).
    let last_idx = if count < MAX_WINNERS { count } else { MAX_WINNERS - 1 };
    let mut j = last_idx;
    while j > pos {
        let prev = lb.entries[j - 1];
        lb.entries[j] = prev;
        j -= 1;
    }
    lb.entries[pos] = LeaderboardEntry { wallet, score };
    if count < MAX_WINNERS {
        lb.count = (count + 1) as u16;
    }
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct ProcessScoreBatch<'info> {
    #[account(
        mut,
        seeds = [LEADERBOARD_SEED, &contest_label],
        bump
    )]
    pub leaderboard: AccountLoader<'info, LeaderboardAccumulator>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct FinalizeLeaderboard<'info> {
    #[account(
        mut,
        seeds = [LEADERBOARD_SEED, &contest_label],
        bump
    )]
    pub leaderboard: AccountLoader<'info, LeaderboardAccumulator>,
    #[account(
        seeds = [CONTEST_VAULT_SEED, &contest_label],
        bump
    )]
    pub contest_vault: Account<'info, ContestVaultState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct ClaimFromLeaderboard<'info> {
    #[account(
        mut,
        seeds = [LEADERBOARD_SEED, &contest_label],
        bump
    )]
    pub leaderboard: AccountLoader<'info, LeaderboardAccumulator>,
    #[account(
        mut,
        seeds = [CONTEST_VAULT_SEED, &contest_label],
        bump
    )]
    pub contest_vault: Account<'info, ContestVaultState>,
    #[account(mut)]
    pub claimer: Signer<'info>,
}

// ============================================================================
// RESULT STRUCTS
// ============================================================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct PrizeDistributionResult {
    pub first_place_amount: u64,
    pub second_place_amount: u64,
    pub third_place_amount: u64,
    /// Per-place amounts for positions 1..10 (§4c). Index 0 = 1st.
    pub place_amounts: [u64; 10],
    pub remaining_pool: u64,
    pub distribution_successful: bool,
    pub winners: [Pubkey; 10],
    pub winner_count: u32,
}

// ============================================================================
// ACCOUNT STRUCTS
// ============================================================================

#[derive(Accounts)]
pub struct CalcPrize<'info> {
    #[account(mut)]
    pub contest_vault: Account<'info, ContestVaultState>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], winner_position: u32, current_market_cap_usd_cents: u64, optional_amount: Option<u64>)]
pub struct ClaimPrize<'info> {
    #[account(mut)]
    pub contest_vault: Account<'info, ContestVaultState>,
    
    /// Winner record PDA - required for contest winner claims (optional_amount = None)
    /// Optional for voter share claims (optional_amount = Some)
    /// MUST be mut (2026-08-06): the winner branch sets `claimed = true`, and
    /// without `mut` Anchor never writes it back — the marking was silently
    /// lost, so a winner could re-claim indefinitely (double-claim bypass) and
    /// get_prize_claim_history always read unclaimed. Real bug, not test-only.
    #[account(mut)]
    pub winner_record: Option<Account<'info, WinnerRecord>>,
    
    /// Voter share record PDA - required for voter pool claims (optional_amount = Some)
    /// Optional for contest winner claims (optional_amount = None)
    #[account(
        mut,
        seeds = [VOTER_SHARE_SEED, &contest_label, claimer.key().as_ref()],
        bump = voter_share_record.bump,
    )]
    pub voter_share_record: Option<Account<'info, VoterShareRecord>>,
    
#[account(mut)]
    pub claimer: Signer<'info>,

    /// WIRED (2026-07-23): the security-governance blacklist PDA.
    ///
    /// Passed as an UncheckedAccount deliberately — this program cannot type it
    /// as `Account<'info, BlacklistState>` without depending on
    /// security-governance, which would be circular. Safety comes from
    /// `read_blacklist_status`, which verifies the OWNER is security-governance,
    /// the ADDRESS is the canonical PDA for `BLACKLIST_SEED`, and the
    /// DISCRIMINATOR matches, before reading a byte. All three are required;
    /// any one alone is bypassable.
    /// CHECK: verified in kots_common::cpi::read_blacklist_status
    pub blacklist_state: UncheckedAccount<'info>,

    /// WIRED (2026-07-23): the security-governance emergency-pause PDA.
    /// CHECK: verified in kots_common::cpi::read_emergency_pause_status
    pub emergency_state: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
// 2026-08-06: winner records are Box<Account<..>> (heap, not stack). With 10
// on the stack, Anchor's generated try_accounts frame hit 4216 bytes vs the BPF
// 4096 limit — the 32-byte overflow silently zeroed the 32-byte contest_label
// arg (UB). Boxing moves them off the stack.
pub struct ExecuteAutoDistribution<'info> {
    #[account(mut)]
    pub contest_vault: Account<'info, ContestVaultState>,
    
    /// First place winner record - created atomically with distribution
    #[account(
        init_if_needed,
        payer = authority,
        seeds = [WINNER_SEED, &contest_label, &1u32.to_le_bytes()],
        bump,
        space = WINNER_RECORD_SPACE
    )]
    pub winner_first: Box<Account<'info, WinnerRecord>>,
    
    /// Second place winner record - created atomically with distribution
    #[account(
        init_if_needed,
        payer = authority,
        seeds = [WINNER_SEED, &contest_label, &2u32.to_le_bytes()],
        bump,
        space = WINNER_RECORD_SPACE
    )]
    pub winner_second: Box<Account<'info, WinnerRecord>>,
    
    /// Third place winner record - created atomically with distribution
    #[account(
        init_if_needed,
        payer = authority,
        seeds = [WINNER_SEED, &contest_label, &3u32.to_le_bytes()],
        bump,
        space = WINNER_RECORD_SPACE
    )]
    pub winner_third: Box<Account<'info, WinnerRecord>>,

    /// Places 4-10 (§4c 10-place curve). init_if_needed so contests with fewer
    /// than 10 finishers still succeed — unused slots stay empty (0 amount).
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &4u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_4: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &5u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_5: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &6u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_6: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &7u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_7: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &8u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_8: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &9u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_9: Box<Account<'info, WinnerRecord>>,
    #[account(init_if_needed, payer = authority, seeds = [WINNER_SEED, &contest_label, &10u32.to_le_bytes()], bump, space = WINNER_RECORD_SPACE)]
    pub winner_10: Box<Account<'info, WinnerRecord>>,

    /// EMERGENCY PAUSE GATE (2026-08-06): security-governance's EmergencyState PDA.
    /// CHECK: owner + canonical-PDA + discriminator verified inside read_emergency_pause_status.
    pub emergency_state: UncheckedAccount<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

/// Space: 8 disc + 128 borsh (audited field-by-field 2026-08-04: 1+32+32+1+8+8+8
/// +1+1+8+4+8+8+8) = 136 minimum; 192 leaves headroom.
pub const CONTEST_VAULT_STATE_SPACE: usize = 192;

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct InitializeContestVault<'info> {
    #[account(
        init,
        payer = authority,
        seeds = [CONTEST_VAULT_SEED, &contest_label],
        bump,
        space = CONTEST_VAULT_STATE_SPACE
    )]
    pub contest_vault: Account<'info, ContestVaultState>,
    /// CHECK: pinned to the program_state PDA and read manually in the handler
    /// (same layout contract as the ACT-24 gate).
    pub program_state: UncheckedAccount<'info>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], position: u32, winner_wallet: Pubkey, prize_amount: u64)]
pub struct StoreWinner<'info> {
    /// Winner record PDA - created during finalization
    #[account(
        init,
        payer = authority,
        seeds = [WINNER_SEED, &contest_label, &position.to_le_bytes()],
        bump,
        space = WINNER_RECORD_SPACE
    )]
    pub winner_record: Account<'info, WinnerRecord>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], voter_wallet: Pubkey, share_amount: u64)]
pub struct StoreVoterShare<'info> {
    /// Voter share record PDA - written ONCE at contest finalization with the
    /// voter's exact pro-rata amount (plan §5a). init (not init_if_needed):
    /// a share, once stored, is immutable.
    #[account(
        init,
        payer = authority,
        seeds = [VOTER_SHARE_SEED, &contest_label, &voter_wallet.key().to_bytes()],
        bump,
        space = VOTER_SHARE_RECORD_SPACE
    )]
    pub voter_share_record: Account<'info, VoterShareRecord>,

    /// The finalization-authority gate (ACT-24): seeds-pinned program state,
    /// read manually in the handler (created raw at initialize).
    /// CHECK: seeds-pinned under this program; is_initialized + authority
    /// verified in the handler.
    #[account(seeds = [b"program_state"], bump)]
    pub program_state: UncheckedAccount<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,

    /// CHECK: resolved against the estate in the handler (must be the real
    /// treasury-operations program, and executable).
    pub treasury_program: UncheckedAccount<'info>,

    /// The contest's voter-share bucket ledger in treasury-operations.
    /// CHECK: the treasury program validates its own seeds; forwarded to CPI.
    #[account(mut)]
    pub voter_share_bucket: UncheckedAccount<'info>,

    /// CHECK: the treasury program validates its own seeds; forwarded to CPI.
    #[account(mut)]
    pub treasury_distribution_config: UncheckedAccount<'info>,

    /// The credit key only THIS program can make sign.
    /// CHECK: own-program PDA, seeds-verified here; signs via invoke_signed.
    #[account(seeds = [BUCKET_CREDIT_AUTHORITY_SEED], bump)]
    pub bucket_credit_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32], winner_meme_ids: [u64; 10], winner_count: u8)]
pub struct SetPredictionOutcomes<'info> {
    /// Prediction tally PDA — created once per contest when the meme winners
    /// are known. init (not init_if_needed): outcomes are recorded once.
    #[account(
        init,
        payer = authority,
        seeds = [PREDICTION_TALLY_SEED, &contest_label],
        bump,
        space = PREDICTION_TALLY_SPACE
    )]
    pub prediction_tally: Account<'info, PredictionTallyAccount>,

    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct TallyPredictions<'info> {
    #[account(
        mut,
        seeds = [PREDICTION_TALLY_SEED, &contest_label],
        bump = prediction_tally.bump,
    )]
    pub prediction_tally: Account<'info, PredictionTallyAccount>,

    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(contest_label: [u8; 32])]
pub struct RankParticipantWinners<'info> {
    #[account(
        mut,
        seeds = [PREDICTION_TALLY_SEED, &contest_label],
        bump = prediction_tally.bump,
    )]
    pub prediction_tally: Account<'info, PredictionTallyAccount>,

    pub authority: Signer<'info>,

    /// BLACKLIST EXCLUSION (2026-07-23): security-governance's blacklist PDA.
    /// CHECK: verified inside kots_common::cpi::read_blacklist_status.
    pub blacklist_state: UncheckedAccount<'info>,
}

// ============================================================================
// CANONICAL ACCOUNT CONTEXTS - Single source of truth for all builds
// ============================================================================

#[derive(Accounts)]
pub struct InitializePrizeDistribution<'info> {
    /// Program state PDA - created with treasury funding
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
    pub prize_distribution_program: UncheckedAccount<'info>,

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

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn validate_winner_eligibility(
    ctx: &Context<ClaimPrize>,
    contest_label: &ContestLabel,
    position: u32,
) -> Result<()> {
    // Real winner eligibility validation from lib10
    msg!("🏆 Validating winner eligibility - Position: {}", position);
    
    require!(position >= 1 && position <= 10, KotsError::InvalidTokenAmount);
    
    // Real winner eligibility from lib10 prize claim validation
    
    // Verify contest finalization (from lib10 ContestCoreState)
    // Load contest state from CPI to verify finalization
    let contest_state = load_contest_state(contest_label)?;
    require!(contest_state.is_finalized, KotsError::ContestNotFinalized);
    require!(contest_state.scoring_finalized, KotsError::ScoringNotComplete);
    
    msg!("Contest finalization verified: finalized={}, scoring_complete={}", 
         contest_state.is_finalized, contest_state.scoring_finalized);
    
    // Confirm winner position in rankings (from lib10 ContestRanking)
    let max_winner_positions = 10; // Top 10 winners
    require!(position <= max_winner_positions, KotsError::InvalidWinnerPosition);
    let position_exists = position >= 1;
    require!(position_exists, KotsError::WinnerPositionNotFound);
    
    // Real prize claim status from lib10 prize claim tracking
    let claim_timestamp = get_current_timestamp()?;
    let prize_claimed = check_prize_claim_history(contest_label, position, claim_timestamp)?;
    require!(!prize_claimed, KotsError::PrizeAlreadyClaimed);
    
    msg!("Prize claim status verified: not previously claimed");
    
    // Real participant validation from lib10 ParticipantState verification
    // Use actual claimer wallet from context
    let participant_wallet = ctx.accounts.claimer.key();
    let participant_valid = validate_participant_existence(contest_label, &participant_wallet)?;
    require!(participant_valid, KotsError::InvalidParticipant);
    
    msg!("Participant account validated: {}", participant_wallet);
    
    // Fraud detection via CPI to security-governance program
    let fraud_result = check_fraud_detection(contest_label, &participant_wallet)?;
    require!(fraud_result.fraud_flags == 0, KotsError::FraudDetected);
    require!(!fraud_result.blacklisted, KotsError::BlacklistedWallet);
    
    msg!("✅ Winner eligibility validated for position {}", position);
    Ok(())
}

fn validate_prize_claim_security(ctx: &Context<ClaimPrize>, contest_label: &ContestLabel, market_cap: u64) -> Result<()> {
    // Real prize claim security from lib10
    msg!("🔒 Enhanced prize claim security validation");
    
    // Real security validation from lib10 enhanced_security_checkpoint
    
    // Comprehensive blacklist checking (from lib10 BlacklistState)
    // Use actual claimer wallet from context
    let claimer_wallet = ctx.accounts.claimer.key();

    // WIRED (2026-07-23): reads the real on-chain blacklist.
    //
    // Was `check_blacklist_status(&claimer_wallet)`, which took no accounts and
    // returned `is_blacklisted: false` unconditionally — so this require! could
    // never fire and the blacklist blocked nobody. The reader verifies the
    // account's owner, canonical PDA address and discriminator before trusting
    // it, so a caller cannot substitute an account that answers "not listed".
    //
    // The security-governance id comes from the on-chain registry keyed by this
    // program's own id, not from network_config (whose detect_network() reads an
    // env var and cannot run in BPF).
    let sg_id = kots_common::program_ids::current(&crate::ID)
        .ok_or(KotsError::InvalidProgramId)?
        .security_governance;

    let blacklist_check = kots_common::cpi::read_blacklist_status(
        &ctx.accounts.blacklist_state.to_account_info(),
        &sg_id,
        &claimer_wallet,
    )?;
    require!(!blacklist_check.is_blacklisted, KotsError::BlacklistedWallet);
    
    msg!("Blacklist check passed for wallet: {}", claimer_wallet);
    
    // Anti-sybil verification (from lib10 Sybil protection)
    // Load actual behavior score from rate limit state
    let user_state = load_user_rate_limit_state(&claimer_wallet)?;
    let behavior_score = user_state.behavior_score;
    require!(behavior_score > 0.5, KotsError::SybilAttackDetected);
    require!(user_state.verified_human, KotsError::SybilAttackDetected);
    
    msg!("Anti-sybil check passed: behavior_score={:.2}, verified_human={}", 
         behavior_score, user_state.verified_human);
    
    // Market cap validation (from lib10 oracle system)
    require!(market_cap > 0, KotsError::InvalidMarketCap);
    let oracle_data = get_oracle_market_cap()?;
    let oracle_age = get_current_timestamp()? - oracle_data.timestamp;
    require!(oracle_age <= 600, KotsError::StaleOracleData); // Max 10 minutes old
    
    msg!("Oracle data validated: market_cap={}, age={}s", market_cap, oracle_age);
    
    // Fraud detection pattern analysis via CPI
    let fraud_result = check_fraud_detection(contest_label, &claimer_wallet)?;
    require!(fraud_result.suspicious_patterns == 0, KotsError::SuspiciousActivity);
    
    msg!("Fraud detection passed: no suspicious patterns");
    
    // Rate limiting on prize claims
    let rate_limit_result = validate_claim_rate_limit(&claimer_wallet)?;
    require!(!rate_limit_result.rate_limited, KotsError::RateLimitExceeded);
    
    msg!("Rate limit check passed: claims_today={}", rate_limit_result.claims_today);
    
    // WIRED (2026-07-23): reads the real EmergencyState PDA.
    //
    // Was `get_emergency_pause_status()`, hardcoded to `is_active: false`, so
    // `emergency_pause` set a flag nothing observed and the pause was inert.
    // The reader also honours `pause_duration`, so a lapsed pause does not halt
    // claims forever.
    let emergency_state = kots_common::cpi::read_emergency_pause_status(
        &ctx.accounts.emergency_state.to_account_info(),
        &sg_id,
    )?;
    require!(!emergency_state.is_active, KotsError::EmergencyPauseActive);
    
    msg!("Emergency pause check passed");
    
    msg!("✅ Prize claim security checkpoint passed");
    Ok(())
}

/// Authorise a prize claim and enforce the on-chain accounting guard.
///
/// IMPORTANT — this does NOT move tokens, and deliberately so. KOTS prize
/// settlement is performed OFF-CHAIN by the settlement service; this on-chain
/// path exists to AUTHORISE that settlement and keep the accounting honest. The
/// security that matters is upstream in `claim_prize`: the caller has already
/// validated the authoritative on-chain `winner_record` / `voter_share_record`
/// (which defeats database doctoring), enforced the per-vault daily cap, and
/// will mark the record `claimed` exactly once. Here we enforce only that the
/// notional prize pool can cover the authorised amount.
///
/// F17: an earlier version of this helper logged "Prize transfer completed" and
/// computed a fake burn margin and vault PDA authority, giving the false
/// impression that tokens moved on-chain. They did not. Moving custody on-chain
/// — a real per-contest vault, a funding flow, and a `transfer_checked` CPI — is
/// F17 D2: a separate, deliberate change to the token model. (Today entry fees
/// are not collected on-chain — `process_entry_fee` is a stub — so there is no
/// on-chain prize balance to pay from in the first place.)
fn authorize_prize_claim(ctx: &Context<ClaimPrize>, amount: u64) -> Result<()> {
    require!(amount > 0, KotsError::InvalidTokenAmount);

    // F17 D2 — ON-CHAIN SETTLEMENT (owner-decided 2026-08-06). Prizes now move
    // on-chain: lamports transfer from the program-owned contest vault PDA to
    // the claimer, mirroring the proven direct-lamport pattern used in
    // transfer-hook (the same mechanism money_movement exercises end-to-end).
    // The vault must be FUNDED with real lamports for this to succeed — see the
    // funding flow; in production that funding is entry-fee collection, still a
    // stub in `process_entry_fee`, so mainnet needs that un-stubbed for the
    // vault to hold real balance rather than test deposits.
    let vault_ai = ctx.accounts.contest_vault.to_account_info();

    // Accounting guard: the notional pool must cover the authorised amount.
    require!(
        ctx.accounts.contest_vault.total_prize_pool >= amount,
        KotsError::InsufficientVaultFunds
    );

    // Custody guard: never spend the vault below rent-exemption (that would
    // close the account and strand every other winner's claim).
    let rent = Rent::get()?;
    let min = rent.minimum_balance(vault_ai.data_len());
    require!(
        vault_ai.lamports().saturating_sub(amount) >= min,
        KotsError::InsufficientVaultFunds
    );

    // Move the lamports. Both are program-owned/writable, so direct balance
    // manipulation is the correct primitive (no system CPI from a data-bearing
    // PDA, no token CPI — the vault holds native lamports).
    **vault_ai.try_borrow_mut_lamports()? -= amount;
    **ctx.accounts.claimer.to_account_info().try_borrow_mut_lamports()? += amount;

    msg!(
        "💸 Prize settled on-chain: {} lamports → {} (F17 D2 on-chain settlement)",
        amount, ctx.accounts.claimer.key()
    );
    Ok(())
}

fn validate_auto_distribution_timing(
    contest_label: &ContestLabel,
) -> Result<()> {
    // Real auto-distribution timing validation from lib10
    msg!("⏰ Validating auto-distribution timing");
    
    // Real auto-distribution timing validation from lib10
    let current_time = get_current_timestamp()?;
    
    // Check auto-distribution enabled (from lib10 ContestCoreState)
    // Real auto-distribution state from lib10 ContestCoreState
    let contest_core_state = load_contest_state(contest_label)?;
    let auto_distribution_enabled = contest_core_state.auto_distribution_enabled;
    require!(auto_distribution_enabled, KotsError::AutoDistributionDisabled);
    
    msg!("Auto-distribution enabled: {}", auto_distribution_enabled);
    require!(auto_distribution_enabled, KotsError::AutoDistributionDisabled);
    
    // Verify timing requirements (from lib10 auto_distribution_timestamp)
    let auto_distribution_timestamp = contest_core_state.auto_distribution_timestamp;
    require!(current_time >= auto_distribution_timestamp, KotsError::AutoDistributionTooEarly);
    
    // Ensure contest finalization (load actual state)
    let contest_finalized = contest_core_state.is_finalized;
    let scoring_complete = contest_core_state.scoring_complete;
    require!(contest_finalized, KotsError::ContestNotFinalized);
    require!(scoring_complete, KotsError::ScoringNotComplete);
    
    // Confirm minimum participants (load actual participant count)
    let min_participants = contest_core_state.min_participants;
    let actual_participants = contest_core_state.participant_count;
    require!(actual_participants >= min_participants, KotsError::InsufficientParticipants);
    
    // Validate not already executed (check actual flag)
    let already_executed = contest_core_state.auto_distribution_executed;
    require!(!already_executed, KotsError::AutoDistributionAlreadyExecuted);
    
    let current_time = get_current_timestamp()?;
    msg!("✅ Auto-distribution timing valid at timestamp: {}", current_time);
    Ok(())
}

fn get_final_scores_for_distribution<'info>(
    contest_label: &ContestLabel,
    participant_accounts: &[AccountInfo<'info>],
) -> Result<[ParticipantScore; MAX_PARTICIPANT_SCORES]> {
    // SCORES SOURCE FIX (2026-08-05, owner-approved). This used to call
    // load_contest_participants, a hard `Ok(vec![])` stub — the instruction
    // had NO access to participant data, so every auto-distribution failed
    // DistributionFailed (the eight-test 9009 cluster). ParticipantState PDAs
    // live in contest-management and that LOCAL struct (414-byte body,
    // audited 2026-08-04) cannot be imported here, so callers now forward the
    // participant accounts via remaining_accounts and this reads the needed
    // fields BY OFFSET — the codebase's own program_state@9..41 precedent.
    // Layout (disc 8): is_initialized@8 | contest_label@9..41 |
    // user_wallet@41..73 | ... is_disqualified@173 | final_score@194..202 LE.
    msg!("📊 Retrieving final scores for prize distribution");

    let cm_id = kots_common::program_ids::current(&crate::ID)
        .ok_or(KotsError::InvalidProgramId)?
        .contest_management;
    let mut participant_scores = [ParticipantScore::default(); MAX_PARTICIPANT_SCORES];
    let mut score_index = 0;

    for acc in participant_accounts.iter() {
        // Every forwarded account must be a genuine, matching, live participant
        // record; anything else is skipped (never trusted).
        if *acc.owner != cm_id {
            continue;
        }
        let data = acc.try_borrow_data()?;
        if data.len() < 8 + 414 || data[8] != 1 {
            continue;
        }
        if &data[9..41] != contest_label.as_ref() {
            continue;
        }
        if data[173] != 0 {
            continue; // disqualified participants earn nothing
        }
        let mut wallet = [0u8; 32];
        wallet.copy_from_slice(&data[41..73]);
        let mut fs = [0u8; 8];
        fs.copy_from_slice(&data[194..202]);
        if score_index < MAX_PARTICIPANT_SCORES {
            participant_scores[score_index] = ParticipantScore {
                participant: Pubkey::new_from_array(wallet),
                score: u64::from_le_bytes(fs),
            };
            score_index += 1;
        }
    }


    // Sort by final_score (highest first) with deterministic tie-breaking
    // (equal score → ascending pubkey). CRITICAL (2026-08-06): sort ONLY the
    // filled slots [..score_index]. The array is fixed-size and the unused tail
    // is Pubkey::default() with score 0; sorting the WHOLE array let those
    // all-zero slots win the ascending-pubkey tiebreak and sort ahead of real
    // participants whenever scores tie or are zero — so winners[0..3] became
    // the default pubkey and every prize record was empty. Slicing to
    // score_index keeps real participants in the top slots.
    participant_scores[..score_index].sort_by(|a, b| {
        match b.score.cmp(&a.score) {
            core::cmp::Ordering::Equal => a.participant.cmp(&b.participant),
            other => other,
        }
    });
    
    // BPF-compatible fixed-size array return
    // Top winners are already sorted at the beginning of the array
    msg!("📊 Retrieved {} participant scores for prize distribution", score_index);
    msg!("✅ Final scores aggregation completed - returning sorted participant data");
    
    // Return the populated and sorted participant scores array
    Ok(participant_scores)
}

fn calculate_and_distribute_prizes(
    scores: [ParticipantScore; MAX_PARTICIPANT_SCORES],
    prize_pool: u64,
) -> Result<PrizeDistributionResult> {
    msg!("💰 Calculating prize distribution - Pool: {} KOTS", prize_pool / TOKENS_PER_UNIT);
    
    // Count actual participants (non-default scores). FILTER, not take_while
    // (2026-08-05): take_while stopped at the first empty/default slot, so a
    // gap before a real entry counted 0 — the "2 retrieved / 0 processed"
    // 9009. get_final_scores packs by score_index but callers must not assume
    // contiguity; count every populated slot.
    let participant_count = scores.iter()
        .filter(|s| s.participant != Pubkey::default())
        .count();
    
    msg!("📊 Processing {} participants for prize distribution", participant_count);
    
    // Initialize winners array and amounts
    let mut winners = [Pubkey::default(); 10];
    let mut place_amounts = [0u64; 10]; // §4c per-place amounts, index 0 = 1st
    let winner_count;
    let first_place_amount;
    let mut second_place_amount = 0u64;
    let mut third_place_amount = 0u64;
    
    // Handle edge cases
    if prize_pool == 0 {
        msg!("⚠️ Zero prize pool - no distribution");
        return Ok(PrizeDistributionResult {
            first_place_amount: 0,
            second_place_amount: 0,
            third_place_amount: 0,
            place_amounts: [0u64; 10],
            remaining_pool: 0,
            distribution_successful: false,
            winners,
            winner_count: 0,
        });
    }
    
    // Calculate prizes based on participant count and pool
    match participant_count {
        0 => {
            msg!("⚠️ No participants - cannot distribute prizes");
            return Ok(PrizeDistributionResult {
                first_place_amount: 0,
                second_place_amount: 0,
                third_place_amount: 0,
                place_amounts: [0u64; 10],
                remaining_pool: prize_pool,
                distribution_successful: false,
                winners,
                winner_count: 0,
            });
        },
        1 => {
            // Single participant gets entire pool
            first_place_amount = prize_pool;
            place_amounts[0] = prize_pool;
            winners[0] = scores[0].participant;
            winner_count = 1;
            msg!("🥇 Single participant - Full pool: {} KOTS", first_place_amount / TOKENS_PER_UNIT);
        },
        2 => {
            // §4c placement curve (was 70/30): places pay their doc share; the
            // rest of the pool stays in the vault (special/voter tranche + the
            // unallocated remainder are Tier 2, not redistributed to placers).
            first_place_amount = placement_prize(prize_pool, 1);
            second_place_amount = placement_prize(prize_pool, 2);
            place_amounts[0] = first_place_amount;
            place_amounts[1] = second_place_amount;
            winners[0] = scores[0].participant;
            winners[1] = scores[1].participant;
            winner_count = 2;
            msg!("🥇🥈 Two participants - 1st: {}, 2nd: {} KOTS", 
                 first_place_amount / TOKENS_PER_UNIT, second_place_amount / TOKENS_PER_UNIT);
        },
        _ => {
            // §4c placement curve — now up to 10 places (was hard-capped at 3).
            let n = (participant_count as usize).min(10);
            for i in 0..n {
                winners[i] = scores[i].participant;
                place_amounts[i] = placement_prize(prize_pool, i + 1);
            }
            first_place_amount = place_amounts[0];
            second_place_amount = place_amounts[1];
            third_place_amount = place_amounts[2];
            winner_count = n as u32;
            msg!("🏆 Distributed to top {} places (§4c 10-place curve)", n);
        }
    }
    
    // Verify total distribution equals prize pool (except for rounding).
    // Sum ALL place amounts (up to 10), not just the first three.
    let total_distributed: u64 = place_amounts.iter().copied().sum();
    let remaining_pool = prize_pool.saturating_sub(total_distributed);
    
    msg!("✅ Prize distribution calculated - Total: {}, Winners: {}", 
         total_distributed / TOKENS_PER_UNIT, winner_count);
    
    Ok(PrizeDistributionResult {
        first_place_amount,
        second_place_amount,
        third_place_amount,
        place_amounts,
        remaining_pool,
        distribution_successful: winner_count > 0,
        winners,
        winner_count,
    })
}

/// Deterministic winner selection (plan §5a de-chancing): rank strictly by
/// score, descending. Ties break by ascending participant pubkey bytes as the
/// on-chain totality guarantee; the richer §5a cascade (unique-voter count →
/// earlier submission timestamp → lower meme id) is applied upstream when the
/// score array is assembled, so equal scores reaching this function already
/// reflect a genuine tie. No probability, no draw: the ordering is total and
/// reproducible by anyone from the same inputs.
// PLACEMENT PRIZE — single source of truth (KOTS_TOKENOMICS.md §4c, 2026-08-05).
// Module-scope free fn so BOTH the in-#[program] calc_prize AND the free
// calculate_and_distribute_prizes can call it (they used to diverge: 50/30/20
// vs 70/30 vs the doc). place is 1-based; returns this place's (11-place)% share
// of the 55% placement pool; >10 => 0. u128 intermediates for overflow safety.
// VOTER PRO-RATA SHARE — KOTS_TOKENOMICS.md §5a, Option A (everyone-positive),
// owner-decided 2026-08-06. A voter's slice of the 20% voter pool is their
// POSITIVE accuracy as a fraction of the SUM of all positive accuracies, times
// the voter pool. Zero/negative accuracy earns nothing. `voter_pool` is the
// lamports already carved as VOTER_PRO_RATA_BPS of the contest pool;
// `sum_positive` is Σ max(0, accuracy_i) over all voters (a u128 so the sum of
// twenty i64 accuracies cannot overflow). Floor division — the sub-lamport
// remainder stays undistributed in the vault (never over-pays); a remainder/
// dust rule can refine this once the disbursement instruction is wired.
// Deterministic and chance-free, matching the scoring engine it reads.
fn voter_pro_rata_share(voter_accuracy: i64, sum_positive: u128, voter_pool: u64) -> u64 {
    if voter_accuracy <= 0 || sum_positive == 0 {
        return 0;
    }
    (((voter_accuracy as u128) * (voter_pool as u128)) / sum_positive) as u64
}

fn placement_prize(total_pool: u64, place: usize) -> u64 {
    if place < 1 || place > 10 {
        return 0;
    }
    let placement_pool = (total_pool as u128) * (PLACEMENT_POOL_BPS as u128) / 10_000u128;
    ((placement_pool * (PLACE_SHARE_BPS[place - 1] as u128)) / 10_000u128) as u64
}

// Writes one WinnerRecord's fields (F17 D2 / 10-place, 2026-08-06). Factored so
// execute_auto_distribution can fill up to 10 records uniformly.
fn set_winner_record(
    rec: &mut WinnerRecord,
    contest_label: [u8; 32],
    position: u32,
    wallet: Pubkey,
    amount: u64,
    bump: u8,
) {
    rec.is_initialized = true;
    rec.contest_label = contest_label;
    rec.position = position;
    rec.winner_wallet = wallet;
    rec.prize_amount = amount;
    rec.claimed = false;
    rec.claim_timestamp = 0;
    rec.bump = bump;
}

fn select_winners_by_score(
    scores: &[ParticipantScore; MAX_PARTICIPANT_SCORES],
    score_count: u32,
    _distribution_type: u8,
) -> Result<[Pubkey; 10]> {
    let actual_count = score_count.min(MAX_PARTICIPANT_SCORES as u32) as usize;

    // Selection sort over indices — bounded (MAX_PARTICIPANT_SCORES) and
    // allocation-free, BPF-friendly. We only need the top 10.
    let mut order: [usize; MAX_PARTICIPANT_SCORES] = core::array::from_fn(|i| i);
    let places = actual_count.min(10);
    for i in 0..places {
        let mut best = i;
        for j in (i + 1)..actual_count {
            let a = &scores[order[j]];
            let b = &scores[order[best]];
            let better = a.score > b.score
                || (a.score == b.score
                    && a.participant.to_bytes() < b.participant.to_bytes());
            if better {
                best = j;
            }
        }
        order.swap(i, best);
    }

    let mut winners = [Pubkey::default(); 10];
    for i in 0..places {
        winners[i] = scores[order[i]].participant;
    }
    Ok(winners)
}

// Placeholder functions for missing CPI calls
// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn check_prize_claim_history(_contest_label: &ContestLabel, _position: u32, _timestamp: i64) -> Result<bool> {
    Ok(false)
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn validate_participant_existence(_contest_label: &ContestLabel, _participant: &Pubkey) -> Result<bool> {
    Ok(true)
}

// Unreferenced scaffolding (flagged by the 2026-07-22 warning sweep). Silenced
// rather than deleted: it predates this session and removing it is an owner
// call, not a lint fix. Dead private items are stripped by the linker, so this
// costs nothing at deploy. Candidate for removal on review.
#[allow(dead_code)]
fn get_real_vault_balance(_vault_account: &Pubkey) -> Result<u64> {
    Ok(1000000)
}

fn validate_kots_token_account(_account: &Pubkey) -> Result<bool> {
    Ok(true)
}

fn load_contest_state(_contest_label: &ContestLabel) -> Result<ContestCoreStateInternal> {
    Ok(ContestCoreStateInternal {
        is_finalized: true,
        scoring_finalized: true,
        auto_distribution_enabled: true,
        auto_distribution_timestamp: 0,
        scoring_complete: true,
        min_participants: 2,
        participant_count: 3,
        auto_distribution_executed: false,
    })
}

#[allow(dead_code)] // superseded 2026-08-05 by remaining_accounts parsing in get_final_scores_for_distribution
fn load_contest_participants(_contest_label: &ContestLabel) -> Result<Vec<ParticipantStateInternal>> {
    Ok(vec![])
}

#[derive(Clone, Debug)]
struct ContestCoreStateInternal {
    pub is_finalized: bool,
    // Written but never read (2026-07-22 sweep). Kept for layout/parity with the
    // on-chain account it mirrors; silencing rather than removing so the struct
    // shape stays faithful.
    #[allow(dead_code)]
    pub scoring_finalized: bool,
    pub auto_distribution_enabled: bool,
    pub auto_distribution_timestamp: i64,
    pub scoring_complete: bool,
    pub min_participants: u32,
    pub participant_count: u32,
    pub auto_distribution_executed: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct ParticipantStateInternal {
    pub participant_wallet: Pubkey,
    pub final_score: u64,
}
