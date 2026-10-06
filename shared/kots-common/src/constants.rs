// DESTINATION (where this file goes): kots-contracts/shared/kots-common/src/constants.rs
//! KOTS Constants
//!
//! All system constants shared across the KOTS multi-program architecture.
//! Maintains exact compatibility with the original single program.
//!
//! For network-specific configuration (program IDs, wallets), see network_config module.

/// Token supply constants for 11 billion KOTS tokens with proper scaling
pub const TOTAL_KOTS_SUPPLY: u64 = 11_111_111_111;
pub const TOKEN_DECIMALS: u8 = 9;
pub const TOKENS_PER_UNIT: u64 = 1_000_000_000; // 10^9 for 9 decimals

/// Base KOTS cost for all interactions (always 1 KOTS for any interaction)
pub const BASE_KOTS_COST: u64 = 1_000_000_000; // 1 KOTS (with 9 decimals)

/// Meme upload costs (includes hosting + vote tracking infrastructure)
pub const HOSTING_COST_COMPONENT: u64 = 290_000_000; // 0.29 KOTS for hosting (bandwidth + storage + server)
pub const VOTE_TRACKING_COST_COMPONENT: u64 = 2_000_000_000; // 2.0 KOTS for vote tracking infrastructure
pub const TOTAL_MEME_UPLOAD_COST: u64 = HOSTING_COST_COMPONENT + VOTE_TRACKING_COST_COMPONENT; // 2.29 KOTS total

/// Multi-contest registration fee (per contest)
pub const CONTEST_REGISTRATION_FEE: u64 = 10_000_000; // 0.01 KOTS per contest registration

/// Sybil attack protection - minimum stake required per participant
pub const MIN_PARTICIPANT_STAKE: u64 = 10 * TOKENS_PER_UNIT; // 10 KOTS minimum stake
pub const STAKE_MULTIPLIER_FOR_REPUTATION: u64 = 2; // 2x stake for established users

/// Program versioning constants
pub const CURRENT_CONTEST_MANAGEMENT_VERSION: u16 = 350;
pub const CURRENT_FEE_MANAGEMENT_VERSION: u16 = 350;
pub const CURRENT_VOTING_HIEROGLYPH_VERSION: u16 = 350;
pub const CURRENT_ECONOMICS_DISTRIBUTION_VERSION: u16 = 350;
pub const CURRENT_SECURITY_GOVERNANCE_VERSION: u16 = 350;
pub const CURRENT_PRIZE_DISTRIBUTION_VERSION: u16 = 350;
pub const CURRENT_PROGRESSIVE_JACKPOT_VERSION: u16 = 350;

/// Treasury operations program ID for cross-program initialization calls
use anchor_lang::prelude::*;

pub const TREASURY_OPERATIONS_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("TreasuryProgram1111111111111111111111111111");

/// Contest bounds with no arbitrary limits
pub const MIN_PARTICIPANTS_REQUIRED: u16 = 2;
pub const MAX_CONTEST_DURATION_SECONDS: i64 = 32 * 24 * 3600; // 32 days max
pub const MIN_CONTEST_DURATION_SECONDS: i64 = 3600; // 1 hour minimum

/// Entry fee bounds for contest participation
pub const MIN_ENTRY_FEE: u64 = 100_000_000; // 0.1 KOTS minimum
pub const MAX_ENTRY_FEE: u64 = 10_000_000_000; // 10 KOTS maximum

/// Hieroglyph interaction cost limit
pub const MAX_HIEROGLYPH_COST: u64 = 10_000 * TOKENS_PER_UNIT; // 10,000 KOTS maximum per hieroglyph

/// Distribution bounds (in basis points) - Updated dual distribution system
pub const TOTAL_DISTRIBUTION_BPS: u64 = 10000;

// TRADING TAX DISTRIBUTION
//
// Guide law (kingoftheshill.com/guide / kots-frontend guide.tsx):
//   Jackpot 50 / Incoming reserve 7 / Treasury 34 / Admin 3 / Dev fund 3 /
//   Burn 1 / Owner bounty 2.
//
// DUAL-PATH (2026-08-27): these BPS are the guide table and are compiled into
// fee-management `execute_fee_distribution_part1..4`. They are NOT live until
// this program is upgraded in place. The currently deployed binary still
// encodes the previous table (incoming 33 / treasury 7 / burn 3 / bounty 1).
// Until that upgrade is confirmed on the target realm, the sidecar keeper
// pays this same guide split with Token-2022 transferChecked + burnChecked.
// Do not treat a call to the live parts 1–4 as this table. Never run both
// paths against the same vault.
pub const TRADING_JACKPOT_DISTRIBUTION_BPS: u64 = 5000; // 50%
pub const TRADING_INCOMING_DISTRIBUTION_BPS: u64 = 700; // 7% — Incoming reserve / Captain
pub const TRADING_ADMIN_DISTRIBUTION_BPS: u64 = 300; // 3%
pub const TRADING_TREASURY_DISTRIBUTION_BPS: u64 = 3400; // 34% — remainder after bounty 2
pub const TRADING_DEV_FUND_DISTRIBUTION_BPS: u64 = 300; // 3%
pub const TRADING_BURN_DISTRIBUTION_BPS: u64 = 100; // 1%
// Legacy: TRADING_LIQUIDITY_DISTRIBUTION_BPS removed - split into incoming + admin

// CONTEST REVENUE DISTRIBUTION (non-prize remainder only; see guide)
pub const CONTEST_TREASURY_DISTRIBUTION_BPS: u64 = 4800; // 48% — was 49; the extra bounty point is taken here
pub const CONTEST_DEVELOPMENT_DISTRIBUTION_BPS: u64 = 3000; // 30%
pub const CONTEST_JACKPOT_DISTRIBUTION_BPS: u64 = 1000; // 10%
pub const CONTEST_BURN_DISTRIBUTION_BPS: u64 = 1000; // 10%

// OWNER BOUNTY — 2% of trading tax AND 2% of contest remainder (was 1%).
// Trading: explicit 200 bps (treasury is 34, not 35). Contest: carved from
// treasury 49 → 48. BOUNTY_WALLET is fee-exempt in the transfer hook.
// The recipient token account is caller-supplied; BOUNTY_WALLET is the
// intended owner and the hook-exemption anchor.
//
// Realm-selected (same pattern as network_config::REALM): default build = Dev;
// `anchor build -- --features realm-main` = Main. One .so per realm so pin checks
// stay compile-time `require_keys_eq!` without a runtime network guess.
pub const BOUNTY_DISTRIBUTION_BPS: u64 = 200; // 2%
/// Devnet bounty pin. Always compiled (hook exemption is realm-independent).
pub const BOUNTY_WALLET_DEVNET: Pubkey =
    anchor_lang::solana_program::pubkey!("6ZuFby6oKQ3bS7surHU9scKVj9dcvnUuMbAt5mT7zB3J");
/// Mainnet bounty — Phantom KOTS-Bounty (fee recipient only; NOT an OwnerRegistry owner).
pub const BOUNTY_WALLET_MAINNET: Pubkey =
    anchor_lang::solana_program::pubkey!("BbvBeQy9oEmF9D5eVtj8hp1tibvMhtFFgVdiD4EqtT9V");

/// Prize-distribution owner (mainnet sidecar admin). Hook-exempt so
/// vote→prize funding and prize→winner claims are not skimmed 300 bps.
pub const PRIZE_VAULT_OWNER_MAINNET: Pubkey =
    anchor_lang::solana_program::pubkey!("3s27ATy7XjATn5VNoSaufDHq3arcSRpdJXrwYXHKhWqP");

/// Realm-selected alias used by AMM / LPM pin checks.
#[cfg(not(feature = "realm-main"))]
pub const BOUNTY_WALLET: Pubkey = BOUNTY_WALLET_DEVNET;
#[cfg(feature = "realm-main")]
pub const BOUNTY_WALLET: Pubkey = BOUNTY_WALLET_MAINNET;

// CANONICAL SYSTEM WALLETS (2026-08-07). Single source of truth for the fee-
// distribution destination OWNERS. fee-management AND lp-position-manager both read +
// pin against these (lp imports them directly in its manager init — single source of
// truth, per owner directive 2026-08-07). These are exactly the six
// wallets seeded into the transfer-hook whitelist, so any distribution destination
// whose owner is not one of these (or BOUNTY_WALLET) is off-canonical and, once
// pinned, rejected. KEEP IN SYNC with network_config::SystemWallets::MainnetBeta
// and scripts/init-transfer-hook-mainnet.ts.
//
// 2026-08-27 — Captain (7%) is the INCOMING leg. Residual is TREASURY 34%
// after jackpot/admin/dev/burn/bounty. Pins below are destination OWNERS.
pub const JACKPOT_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("5wEeX8zYxSGiwZpqpUWmzTyRFjcN8hDFpXF6BWUKgUsP");
/// Captain 7% leg. Realm-selected: default = historical / dev pin; `realm-main` = captain.
#[cfg(not(feature = "realm-main"))]
pub const INCOMING_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("67xZhPqDhbpMet621zqYEwmUVL4funczGif1nKAQPWL");
#[cfg(feature = "realm-main")]
pub const INCOMING_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("6wQtCc5NzdUGuEaP4PhVcxh8kEA1PZiMbt312pD3Qa4");
pub const ADMIN_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("2KA5zPr8oHv5dLr67NN2s9fiwajRCEuYtQiQFJP1W6ME");
/// Platform residual (34% of trading tax). Realm-selected: default = historical pin; `realm-main` = residual.
#[cfg(not(feature = "realm-main"))]
pub const TREASURY_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("2y6U48F987MnmwGMTrc9Z21MXNDxyzQQZmLD4BLdviu8");
#[cfg(feature = "realm-main")]
pub const TREASURY_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("HW1G2xVnAkudRGT6buZEY8QL6UaPu291pyKwHALj3AkD");
/// Realm-selected development fund (3% leg). Default = historical pin; `realm-main` = new recipient.
/// Must remain distinct from OwnerRegistry owners (recipient ≠ owner).
#[cfg(not(feature = "realm-main"))]
pub const DEV_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("5MkcpH6Pv8keYU39Yn8DyS6ou2L1tQZ1QcS3jzSYGUVX");
#[cfg(feature = "realm-main")]
pub const DEV_WALLET: Pubkey =
    anchor_lang::solana_program::pubkey!("Drbz3GJNr2Qcpe76hsL2wEGa4qMnSEFRbMcNBjk9ouvA");
pub const BURN_ADDRESS: Pubkey =
    anchor_lang::solana_program::pubkey!("11111111111111111111111111111112");

// Legacy constants for backward compatibility
pub const DEFAULT_JACKPOT_DISTRIBUTION_BPS: u64 = TRADING_JACKPOT_DISTRIBUTION_BPS;
pub const DEFAULT_SPECIFIED_WALLET_DISTRIBUTION_BPS: u64 = TRADING_TREASURY_DISTRIBUTION_BPS;
pub const DEFAULT_BURN_DISTRIBUTION_BPS: u64 = TRADING_BURN_DISTRIBUTION_BPS;
pub const DEFAULT_DEV_DISTRIBUTION_BPS: u64 = CONTEST_DEVELOPMENT_DISTRIBUTION_BPS;

/// Meme ID timestamp validation (±30 seconds window for client-supplied timestamps)
pub const MEME_ID_TIMESTAMP_WINDOW_SECONDS: i64 = 30;

/// Rate limiting constants (enhanced with Sybil protection)
pub const MAX_CONTESTS_PER_CREATOR_PER_DAY: u8 = 5;
pub const MAX_INTERACTIONS_PER_USER_PER_MINUTE: u16 = 10;
pub const MIN_TIME_BETWEEN_INTERACTIONS_SECONDS: i64 = 6;
pub const MAX_INTERACTION_MULTIPLIER: u8 = 32;

/// Gas oracle enhanced security constants
pub const GAS_ORACLE_CONSENSUS_THRESHOLD: u8 = 2; // Need 2 out of 3 oracles to agree
pub const GAS_ORACLE_UPDATE_INTERVAL_SECONDS: i64 = 300; // 5 minutes
pub const MAX_GAS_PRICE_LAMPORTS: u64 = 50_000;
pub const MIN_GAS_PRICE_LAMPORTS: u64 = 1_000;
pub const DEFAULT_GAS_PRICE_LAMPORTS: u64 = 5_000;
pub const GAS_PRICE_VARIANCE_LIMIT_BPS: u64 = 1500; // 15%
pub const DAILY_CHANGE_LIMIT_BPS: u64 = 3000; // 30%
pub const COMPUTE_UNITS_PER_INTERACTION: u32 = 200_000;

/// Vote tracking constants
pub const MAX_VOTE_TRACKER_SEQUENCES: usize = 32;

/// Individual account sizes (preserved exactly)
pub const CONTEST_CORE_STATE_SIZE: usize = 512;
pub const PARTICIPANT_STATE_SIZE: usize = 256;
pub const INTERACTION_BATCH_SIZE: usize = 32;
pub const VOTE_TRACKER_SIZE: usize = 128;
pub const USER_RATE_LIMIT_STATE_SIZE: usize = 1024;
pub const GAS_ORACLE_SIZE: usize = 2048;
pub const BLACKLIST_STATE_SIZE: usize = 4096;
pub const EMERGENCY_STATE_SIZE: usize = 512;
pub const FEE_DISTRIBUTION_STATE_SIZE: usize = 2048;

/// Security constants
pub const MIN_MEME_ID: u64 = 1;
pub const MAX_MEME_ID: u64 = 1_000_000_000;
pub const MAX_IPFS_HASH_LENGTH: usize = 46;
pub const IPFS_HASH_LENGTH: usize = 46; // Alias for compatibility
pub const MAX_REASON_LENGTH: usize = 64;

// BPF-compatible STACK array size limits (under 4KB)
pub const MAX_AUTHORIZED_FINALIZERS: usize = 5; // 160 bytes - STACK SAFE
pub const MAX_VOTERS_PER_TRACKER: usize = 50; // 1.6KB - BALANCED cost/scale
pub const MAX_INTERACTIONS_PER_BATCH: usize = 3; // BPF-safe: just enough to fit
pub const MAX_CONTEST_LABEL_LENGTH: usize = 32;
pub const MAX_STRING_LENGTH: usize = 64; // BPF-safe: reduce string sizes

// COST-EFFECTIVE SCALING OPTIONS
pub const ENABLE_UNLIMITED_VOTING_SEQUENCES: bool = false; // Toggle expensive unlimited system
pub const VOTE_COUNT_ONLY_AFTER_LIMIT: bool = true; // Count votes beyond individual tracking
pub const ENABLE_UPGRADE_PATH: bool = true; // Allow future scaling without breaking changes

/// Program type constants for IDL compilation (u8 mapping for ProgramType enum)
pub const PROGRAM_TYPE_CONTEST_MANAGEMENT: u8 = 0;
pub const PROGRAM_TYPE_FEE_MANAGEMENT: u8 = 1;
pub const PROGRAM_TYPE_PRIZE_DISTRIBUTION: u8 = 2;
pub const PROGRAM_TYPE_PROGRESSIVE_JACKPOT: u8 = 3;
pub const PROGRAM_TYPE_SECURITY_GOVERNANCE: u8 = 4;
pub const PROGRAM_TYPE_TREASURY_OPERATIONS: u8 = 5;
pub const PROGRAM_TYPE_VOTING_HIEROGLYPH: u8 = 6;

// ACCOUNT STORAGE limits (large data in separate PDA accounts)
pub const MAX_BLACKLISTED_WALLETS_PER_ACCOUNT: usize = 10; // BPF-safe: ~1KB per account
pub const MAX_PARTICIPANT_SCORES_PER_ACCOUNT: usize = 20; // BPF-safe: ~800 bytes per account
pub const MAX_BLACKLIST_ACCOUNTS: usize = 10; // Support up to 2500 blacklisted wallets
pub const MAX_PARTICIPANT_ACCOUNTS: usize = 100; // Support up to 10,000 participants

/// Aliases for backward compatibility with existing code
pub const MAX_BLACKLISTED_WALLETS: usize = MAX_BLACKLISTED_WALLETS_PER_ACCOUNT;
pub const MAX_PARTICIPANT_SCORES: usize = MAX_PARTICIPANT_SCORES_PER_ACCOUNT;

// Meme account seed for PDA generation
pub const MEME_ACCOUNT_SEED: &[u8] = b"meme_account";

/// RESERVED for §5a tier-qualification stakes (deferred at launch, D7).
///
/// Declared now, unused at launch, so the seed namespace cannot be claimed by
/// another PDA in the interim. Reserving a seed costs nothing; discovering the
/// collision after mainnet PDAs exist costs a migration. See KOTS-MAINNET-PLAN
/// §3.7 (D7) and C5.
pub const QUALIFICATION_STAKE_SEED: &[u8] = b"qualification_stake";

// Additional state sizes needed for BPF compliance
pub const PROGRESSIVE_JACKPOT_STATE_SIZE: usize = 1024;

/// Ownership limits - 3% per wallet for acquisitions only
pub const MAX_WALLET_OWNERSHIP_BPS: u64 = 300; // 3% max per wallet
pub const OWNERSHIP_LIMIT_MARKET_CAP_THRESHOLD: u64 = 100_000_000; // $1M in cents

/// Automatic prize distribution settings
pub const AUTO_DISTRIBUTION_DELAY_SECONDS: i64 = 3600; // 1 hour after contest ends
pub const MANUAL_CLAIM_WINDOW_SECONDS: i64 = 7 * 24 * 3600; // 7 days to claim manually

/// Prize distribution percentages
// SUPERSEDED 2026-08-05 by the KOTS_TOKENOMICS.md §4c 10-place curve (below).
// Kept only so unrelated readers still compile; NOT used by prize math anymore.
pub const FIRST_PLACE_PERCENTAGE: u64 = 5000; // 50% (superseded)
pub const SECOND_PLACE_PERCENTAGE: u64 = 3000; // 30% (superseded)
pub const THIRD_PLACE_PERCENTAGE: u64 = 2000; // 20% (superseded)

// PLACEMENT PRIZE CURVE — KOTS_TOKENOMICS.md §4c (definitive). Placement prizes
// are 55% of the contest pool; within that 55%, places 1..10 take a linear
// (11 - position)% share: 1st 10%, 2nd 9%, ... 10th 1% (sums to 55% of the
// placement pool — the remaining 45% of the *placement pool* is unallocated by
// design in the doc; the CONTEST pool's other 45% is the special/voter tranche,
// handled separately in Tier 2). Positions > 10 earn nothing.
pub const PLACEMENT_POOL_BPS: u64 = 5500; // 55% of contest pool
/// Per-place share IN BPS OF THE PLACEMENT POOL, index 0 = 1st place.
pub const PLACE_SHARE_BPS: [u64; 10] = [1000, 900, 800, 700, 600, 500, 400, 300, 200, 100];

// ── SPECIAL-PRIZE POOL (the 45%) — KOTS_TOKENOMICS.md §4c, owner decisions
// 2026-08-06. All BPS are of the FULL contest pool. Placement (above) is 55%;
// these five sum to the remaining 45%:
//   Voter pro-rata     20%  (Option A, everyone-positive — decided 2026-08-06)
//   Unique commenters   7%  (attractor-of-most-distinct-wallets; recipient TBD)
//   Reserved (was 10% "random distribution") — DOC-FORBIDDEN to allocate until
//                       an owner decision replaces the chance mechanic; held,
//                       not distributed. DeterminismRulesTest fails on any RNG.
//   Varied interactions 3%  (widest hieroglyph-type range)
//   Category winners    5%  (2026-08-06: the old 2% discretionary was REMOVED
//                       and folded here — now FIVE categories at 1% each, up
//                       from three. NOTE: no meme-topic-category system exists
//                       on-chain yet; this constant records the split, the
//                       payout logic is a separate unbuilt subsystem.)
// 20 + 7 + 10 + 3 + 5 = 45. With placement's 55, the pool is fully accounted.
pub const VOTER_PRO_RATA_BPS: u64 = 2000; // 20%
pub const UNIQUE_COMMENTER_BPS: u64 = 700; // 7%
pub const RESERVED_UNDECIDED_BPS: u64 = 1000; // 10% — held, NOT distributed
pub const VARIED_INTERACTION_BPS: u64 = 300; // 3%
pub const CATEGORY_WINNER_COUNT: u64 = 5; // was 3; +2 from removed discretionary
pub const CATEGORY_WINNER_BPS_EACH: u64 = 100; // 1% each → 5% total

/// Scoring system constants - all 32 hieroglyph interactions
pub const INTERACTION_SCORING_WEIGHTS: [u64; 32] = [
    10, 15, 5, // Basic: Upvote, Comment, Downvote
    25, 20, 35, 30, 40, 20, 30, 15, 50, // Emotional reactions
    75, 100, 150, 200, 300, // Recognition awards
    80, 90, 70, 120, 200, 250, 100, 110, 130, // Visual effects
    400, 350, 500, 450, 600, 1000, // Advanced operations (Crown = 1000)
];

/// Emergency and security constants
pub const EMERGENCY_PAUSE_DURATION_SECONDS: i64 = 24 * 3600; // 24 hours
pub const MAX_DAILY_VOLUME_LIMIT: u64 = 1_000_000 * TOKENS_PER_UNIT; // 1M KOTS daily limit

// ============================================================================
// NETWORK-AWARE WALLET CONFIGURATIONS (PRESERVED EXACTLY)
// ============================================================================

// ============================================================================
// NETWORK-SPECIFIC CONFIGURATIONS
// ============================================================================
// NOTE: Network-specific program IDs and wallet addresses are now managed
// through the network_config module. These byte array constants are kept
// for BPF compatibility where Pubkey cannot be used.

/// Universal burn wallet (same across all networks)
pub const BURN_WALLET_BYTES: [u8; 32] = [0; 32]; // Null address for burning

// Legacy wallet byte arrays - migrated to network_config module
// for better maintainability and network separation
pub const DEVNET_TREASURY_WALLET_BYTES: [u8; 32] = [
    156, 38, 126, 162, 69, 239, 102, 177, 142, 22, 181, 227, 229, 209, 78, 124, 10, 195, 247, 57,
    107, 118, 46, 195, 115, 103, 58, 232, 167, 70, 254, 94,
];

pub const DEVNET_DEVELOPMENT_FUND_WALLET_BYTES: [u8; 32] = [
    88, 127, 74, 116, 194, 142, 169, 26, 247, 184, 9, 137, 54, 160, 58, 49, 30, 6, 23, 161, 10, 8,
    16, 56, 193, 4, 10, 32, 170, 254, 149, 84,
];

pub const DEVNET_JACKPOT_REWARD_POOL_WALLET_BYTES: [u8; 32] = [
    165, 47, 115, 169, 19, 66, 192, 8, 180, 134, 50, 52, 202, 195, 87, 161, 92, 216, 140, 35, 18,
    194, 95, 223, 251, 24, 134, 25, 80, 195, 222, 110,
];

// Testnet and mainnet byte arrays kept for compatibility
pub const TESTNET_TREASURY_WALLET_BYTES: [u8; 32] = [
    76, 45, 123, 89, 201, 156, 78, 234, 45, 167, 89, 234, 123, 76, 45, 167, 89, 234, 123, 76, 45,
    167, 89, 234, 123, 76, 45, 167, 89, 234, 123, 45,
];

pub const MAINNET_TREASURY_WALLET_BYTES: [u8; 32] = [
    29, 58, 100, 166, 117, 16, 80, 140, 80, 61, 84, 199, 41, 171, 12, 137, 8, 58, 208, 241, 214, 99,
    167, 107, 199, 45, 89, 73, 206, 157, 153, 251,
];

// ============================================================================
// MEME UPLOAD & STORAGE MANAGEMENT CONSTANTS
// ============================================================================

/// Fixed meme upload cost (infrastructure only - HYBRID MODEL)
pub const FIXED_MEME_UPLOAD_COST: u64 = HOSTING_COST_COMPONENT; // 0.29 KOTS

/// Contest entry fee for on-chain voting participation
pub const CONTEST_ENTRY_FEE: u64 = 500_000_000; // 0.5 KOTS

/// Storage tier pricing multipliers
pub const STANDARD_STORAGE_MULTIPLIER: u64 = 1; // 1x base cost
pub const PREMIUM_STORAGE_MULTIPLIER: u64 = 2; // 2x base cost
pub const ARCHIVE_STORAGE_MULTIPLIER: u64 = 3; // 3x base cost

/// Storage duration constants
pub const DEFAULT_STORAGE_DURATION_MONTHS: u8 = 3; // 3 months default
pub const MAX_STORAGE_EXTENSION_MONTHS: u8 = 12; // 12 months max extension
pub const MIN_STORAGE_EXTENSION_MONTHS: u8 = 1; // 1 month min extension

/// Meme upload size limits and costs
pub const AVERAGE_MEME_SIZE_MB: f64 = 5.0; // Average meme size for calculations
pub const MAX_MEME_SIZE_MB: u64 = 10; // Maximum allowed meme size
pub const MIN_MEME_SIZE_MB: u64 = 1; // Minimum meme size

/// Infrastructure cost components (in nanoKOTS)
pub const BANDWIDTH_COST_PER_MB: u64 = 10_000_000; // 0.01 KOTS per MB
pub const STORAGE_COST_PER_MB_MONTH: u64 = 5_000_000; // 0.005 KOTS per MB per month
pub const SERVER_COST_MULTIPLIER_BPS: u16 = 200; // 2% server overhead

/// Payment verification constants
pub const DEFAULT_VERIFICATION_WINDOW_SECONDS: i64 = 300; // 5 minutes
pub const MIN_SIGNATURE_ENTROPY: u32 = 32; // Minimum entropy for Ed25519 signatures
pub const SIGNATURE_HASH_LENGTH: usize = 32; // SHA256 hash length

/// Meme account storage size
pub const MEME_ACCOUNT_SIZE: usize = 8 + 8 + 32 + 32 + 46 + 8 + 8 + 1 + 32 + 8 + 1 + 1; // ~180 bytes