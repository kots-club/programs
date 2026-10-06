//! KOTS Multiprogram Test Suite
//!
//! Comprehensive integration tests for all 7 smart contract programs
//! using the Anchor testing framework with Solana Program Test.
//!
//! Test Organization:
//! - Program-specific integration tests (existing)
//! - End-to-end contest lifecycle tests (mainnet certification)
//! - Edge case and boundary condition tests (mainnet certification)
//! - CPI security and authorization tests (mainnet certification)
//! - Security controls validation (mainnet certification)
//! - Economic security and attack resistance (mainnet certification)

// Test utilities and shared infrastructure

// QUARANTINED (2026-07-23) behind the `legacy-suite` feature.
//
// These 155 tests have never validated the programs. Bringing them up surfaced,
// in sequence: hand-typed placeholder bytes where Anchor discriminators belong;
// account lists with 4 entries where the instruction declares 10, in a different
// order; `Pubkey::new_unique()` in Signer positions, which cannot sign; a
// blockhash captured once so every repeat call was a deduplicated no-op; and fee
// arithmetic written for a 6-decimal token against a 9-decimal mint.
//
// That is not drift from a working baseline — the crate header says as much:
// "written against earlier crate versions and has never been type-checked".
// Fixing them incrementally moved the pass count 10 -> 28 and then stalled, with
// each repaired layer exposing the next.
//
// They are retained, not deleted: individual cases encode real intent worth
// mining (the prediction-scoring assertions in particular). Run them with
//     cargo test -p kots-comprehensive-tests --features legacy-suite
// The default suite is `security_regressions`, which targets the audit findings
// directly and is written against the current programs.

// test_utils stays ungated: the new suite builds on it.
pub mod test_utils;

// ── TRIAGE (2026-07-22) ──────────────────────────────────────────────────────
//
// The first ever `cargo test` on this crate produced 532 errors, of which 360
// were calls to harness methods that DO NOT EXIST: the tests were written
// against a MultiProgramTestContext roughly twice the size of the one in
// test_utils.rs (94 missing methods against 52 present).
//
// Five files carry 455 of those errors. They are parked below rather than
// deleted — the tests themselves may be perfectly good, they simply need
// harness that was never written. Re-enable each as its dependencies land.
//
// The seven files left enabled account for ~77 errors and are the cheapest path
// to integration tests that actually RUN, which is where the value is: a test
// that compiles but has never executed has proven nothing. That was the lesson
// of the hook-lab matrix, which found a dozen real bugs on first execution.
//
// PARKED — 455 errors, needs ~94 harness methods:
//   direct_security_governance_tests   147
//   direct_fee_management_tests        127
//   treasury_operations_tests           81
//   progressive_jackpot_tests           56
//   voting_hieroglyph_tests             44
//
// pub mod direct_security_governance_tests;
// pub mod direct_fee_management_tests;
// pub mod treasury_operations_tests;
// pub mod progressive_jackpot_tests;
// pub mod voting_hieroglyph_tests;

// ── ENABLED ─────────────────────────────────────────────────────────────────
#[cfg(feature = "legacy-suite")]
pub mod prize_distribution_tests;              // Prize distribution tests
#[cfg(feature = "legacy-suite")]
pub mod test_upload_meme;                      // Meme upload tests

// NEW: Mainnet certification test suites (October 14, 2025)
#[cfg(feature = "legacy-suite")]
pub mod end_to_end_contest_lifecycle_tests;    // Complete contest flows (6 tests)
#[cfg(feature = "legacy-suite")]
pub mod edge_case_tests;                       // Edge cases & boundaries (9 tests)
#[cfg(feature = "legacy-suite")]
pub mod cpi_security_tests;                    // CPI authorization & treasury (8 tests)
#[cfg(feature = "legacy-suite")]
pub mod security_controls_tests;               // Blacklist, pause, RBAC (9 tests)
#[cfg(feature = "legacy-suite")]
pub mod economic_security_tests;               // Oracle, fees, MEV resistance (9 tests)

// REGRESSION (2026-08-03): minimal guard for the voting de-brick
// (validate_voter_security always-false `require!`). The dedicated
// voting_hieroglyph_tests module is PARKED (see the block above); this covers the
// one path the fix touches, using only harness methods that exist today.
#[cfg(feature = "legacy-suite")]
pub mod voting_debrick_regression_tests;       // Voting de-brick guard (1 test)

#[cfg(feature = "devnet-network")]
pub mod paid_vote_devnet_test;

// Re-export test utilities for easy access
pub use test_utils::*;