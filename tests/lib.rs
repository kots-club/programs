// kots-contracts/tests/lib.rs
//! KOTS Multiprogram Test Library
//!
//! Crate root for the integration suite. The tests themselves live in
//! `devnet_idl_tests/` and run against an in-process validator
//! (`solana-program-test` / `BanksClient`) by default; the `devnet-network` and
//! `mainnet-network` features switch them to a live cluster instead.
//!
//! # Coverage — read this before trusting the suite
//!
//! This file previously claimed the suite achieved "100% coverage". That was
//! false, and the claim was corrected on 2026-07-22. The accurate position:
//!
//! * **7 of 9 programs are exercised here**: contest-management, fee-management,
//!   prize-distribution, progressive-jackpot, security-governance,
//!   treasury-operations, voting-hieroglyph.
//!
//! * **transfer-hook and lp-position-manager have ZERO tests in this suite.**
//!   Neither is referenced by any test file, and neither is a dependency of this
//!   crate. The transfer hook — which routes every fee — is covered instead by
//!   the executable golden matrix under `ops/hook-lab/`, which is what actually
//!   found its defects. Do not read a green run here as saying anything about
//!   the hook.
//!
//! * **No coverage is measured.** No tarpaulin / llvm-cov / grcov is configured
//!   anywhere in the repo, so no percentage figure has ever been produced by a
//!   tool. Any number quoted in docs is an estimate, not a measurement.
//!
//! * **The individual programs contain no Rust unit tests at all** (zero `#[test]`
//!   functions across all nine). The only unit tests in the workspace are in
//!   `shared/kots-common`, and they cover config tables — the hieroglyph table,
//!   the network/program-ID tables, space calculation, version — not instruction
//!   logic.
//!
//! # History worth keeping in mind
//!
//! Until 2026-07-22 this suite could not build, for three independent reasons:
//! cargo found no lib target (this file sits at the package root while cargo
//! defaults to `src/lib.rs`, and no `[lib] path` was declared), the
//! `solana-program-test` dependency that `test_utils.rs` imports was missing, and
//! four `[[test]]` targets pointed at files that do not exist. Nothing in CI ran
//! it either. Expect the first real compile to surface API drift, since this code
//! was written against earlier crate versions and has never been type-checked.

// Import the consolidated test suite from devnet_idl_tests.
// Its individual test modules are gated behind `legacy-suite` (see mod.rs);
// `test_utils` remains available because the new suite builds on it.
pub mod devnet_idl_tests;

/// Security regression suite (2026-07-23) — one test per audit finding, asserting
/// that the bad thing is REJECTED. This is the default suite.
pub mod security_regressions;

/// Money-movement suite (2026-07-24) — the positive counterpart: proves that
/// `proc_contest_revenue` lands EXACT amounts in all four wallets THROUGH the
/// Token-2022 transfer-hook path (the CPI shape award_jackpot and
/// execute_fee_distribution also depend on), and that omitting the hook's
/// account set fails. First test in the workspace to move a token on-chain.
pub mod money_movement;
pub mod fee_distribution_trading;

/// Track-A migrated-rail proof (2026-08-08) — lp-position-manager. Proves
/// `distribute_kots_lp_fees_part4` moves the owner-bounty leg out of a
/// manager-PDA-owned vault, through the Token-2022 hook, into BOUNTY_WALLET, with
/// the PDA signing via seeds. money_movement proves the mechanism under a KEYPAIR
/// vault; this is the first proof under a PROGRAM-PDA authority.
pub mod lp_movement;

/// Track-A second proof (2026-08-08) — treasury-operations. Proves
/// `distribute_from_treasury` (jackpot tag) moves KOTS out of the outflow-PDA vault
/// through the hook AND enforces the pool-conservation bound (a draw over the live
/// `total_jackpot_amount` reverts). Exercises the accounting lp part-4 doesn't touch.
pub mod treasury_movement;

/// Emergency pause/resume + blacklist suite — the on-chain operations behind the
/// security-governance sidecar service. `allows_*` prove pause/resume and
/// add/remove work; `rejects_*` prove the 10-wallet cap and the
/// resume-before-pause guard actually bite.
pub mod emergency_and_blacklist;

/// ACT-03 (F6) whitelist-quorum suite — the paired positive/negative proof that
/// transfer-hook's `add_to_whitelist` / `remove_from_whitelist` now require an
/// owner-registry quorum (cross-program: the registry lives in
/// security-governance). `allows_*` proves a 2-of-3 can edit; `rejects_*` proves
/// a lone owner is refused in both directions and a foreign `owner_registry`
/// account is refused by Anchor's owner + `seeds::program` check; `preserves_*`
/// proves the existing duplicate guard still bites. Like `money_movement`, it
/// builds a dedicated `ProgramTest` (transfer_hook + security_governance) rather
/// than `MultiProgramTestContext`, which does not load the hook.
pub mod whitelist_quorum_tests;

/// Owner-registry suite (ACT-02) — the on-chain owner set / M-of-N quorum /
/// break-glass primitive (F1), previously dead code. `allows_*` prove
/// initialization and the two-owner and break-glass-with-one-owner rotation
/// paths; `rejects_*` prove single-signer, break-glass-alone (Option B), and
/// non-owner rotations are all refused. File is `owner_registry_tests.rs`, so
/// the module name matches the file (the same file==module convention the other
/// suites here use).
pub mod owner_registry_tests;

/// ACT-21 treasury timelock + veto (2026-08) — `allows_*` prove queue records
/// the Clock-stamped note with a mandatory purpose and that owner veto/snooze/
/// window-init work; `rejects_*` prove the purpose guards and the owner-only
/// gates bite. `settle` is not covered here (its token accounts require the
/// custody substrate + a real clock warp — see the file's SCOPE NOTE). File is
/// `treasury_timelock_tests.rs`, module name matches.
pub mod treasury_timelock_tests;
pub mod treasury_watcher_tests;
pub mod treasury_conservation_tests;
pub mod voter_share_credit_tests;
pub mod operational_tier_tests;

/// O32 regression suite (2026-07-29) — the refund queue's capacity is a heap
/// budget, not a storage preference, because the TLV resolver copies every
/// meta-list account's full data onto the CPI caller's heap. Also covers the
/// ring-buffer arithmetic that the capacity change touched.
///
/// The only suite here that runs on the HOST: no validator, no `BPF_OUT_DIR`,
/// no compiled `.so`. It exercises `PendingRefundQueue`'s real methods, which
/// is why they were extracted from the instruction handlers in the same change.
pub mod refund_queue_capacity;

/// Shared world bootstrap. Exists so that a POSITIVE leg costs three lines
/// rather than eighty — the reason four security regressions passed vacuously
/// on 2026-07-28 was that standing up the accounts was expensive, not that
/// anyone thought a bare `is_err()` was sufficient.
///
/// **A rejection test without a paired positive control is not a test.**
pub mod bootstrap;
