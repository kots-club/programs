// kots-contracts/tests/paid-vote-devnet/src/lib.rs
//! Standalone devnet paid-vote money-path proof (triage step 4).
//!
//! This crate intentionally has no library code — the proof is an integration
//! test at `tests/paid_vote_devnet.rs` that runs against real devnet. See that
//! file's header for prerequisites and the run command. It is decoupled from the
//! banks-client harness (see Cargo.toml) so it compiles without the unfinished
//! live-cluster `test_utils.rs`.
