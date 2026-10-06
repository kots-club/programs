// DESTINATION: split as noted below.
//   * `mod pure_logic_tests`  → append to quorum_decision.rs (runs with plain `cargo test`,
//     no validator — it only tests the pure predicates).
//   * `mod integration_tests` → kots-contracts/tests/devnet_idl_tests/ alongside
//     direct_security_governance_tests.rs (needs the MultiProgramTestContext harness +
//     a test validator; the harness needs the small helpers noted at the top of that mod).
//
// These are NOT run in the authoring environment (no anchor/solana toolchain). They
// encode the behavior the module must satisfy — run them after `anchor build`.

// ===========================================================================
// PURE-LOGIC UNIT TESTS — directly runnable, no on-chain harness.
// Exercises the window/threshold predicates on `GovernanceDecision`. (The
// handlers and `require_enforceable_and_consume` emit events and so need the
// runtime — those are covered in integration_tests below.)
// ===========================================================================
#[cfg(test)]
mod pure_logic_tests {
    use super::*; // when appended into quorum_decision.rs

    fn decision(opened_at: i64, yes: u8, threshold: u8, status: DecisionStatus) -> GovernanceDecision {
        GovernanceDecision {
            action: GovernedAction::EmergencyResume,
            proposer: Pubkey::default(),
            decision_id: 1,
            opened_at,
            expires_at: opened_at + VOTE_WINDOW_SECONDS,
            yes_votes: yes,
            no_votes: 0,
            threshold,
            status,
            bump: 0,
        }
    }

    #[test]
    fn window_is_one_day() {
        assert_eq!(VOTE_WINDOW_SECONDS, 86_400);
    }

    #[test]
    fn open_for_voting_inside_window() {
        let d = decision(1_000, 1, 2, DecisionStatus::Open);
        assert!(d.is_open_for_voting(1_000));                    // at open
        assert!(d.is_open_for_voting(1_000 + VOTE_WINDOW_SECONDS)); // at the boundary
        assert!(!d.is_open_for_voting(1_001 + VOTE_WINDOW_SECONDS)); // one past
    }

    #[test]
    fn expired_only_after_window_and_while_open() {
        let d = decision(1_000, 1, 2, DecisionStatus::Open);
        assert!(!d.is_expired(1_000 + VOTE_WINDOW_SECONDS));     // still in window
        assert!(d.is_expired(1_001 + VOTE_WINDOW_SECONDS));      // past window
        let done = decision(1_000, 2, 2, DecisionStatus::Executed);
        assert!(!done.is_expired(1_001 + VOTE_WINDOW_SECONDS));  // executed is never "expired"
    }

    #[test]
    fn threshold_reached_is_yes_ge_threshold() {
        assert!(!decision(0, 1, 2, DecisionStatus::Open).has_reached_threshold());
        assert!(decision(0, 2, 2, DecisionStatus::Open).has_reached_threshold());
        assert!(decision(0, 3, 2, DecisionStatus::Open).has_reached_threshold()); // over-threshold still true
    }

    #[test]
    fn executed_decision_is_not_open_for_voting() {
        let d = decision(1_000, 2, 2, DecisionStatus::Executed);
        assert!(!d.is_open_for_voting(1_000)); // even well inside the window
    }
}

// ===========================================================================
// INTEGRATION TESTS — MultiProgramTestContext + test validator.
//
// Harness helpers to add to test_utils (as for any new instruction). Suggested
// shape (names are illustrative — match your harness):
//   ctx.init_owner_registry(owners: &[Pubkey], threshold: u8, break_glass: Pubkey)
//   ctx.open_decision(id: u64, action: GovernedAction, proposer: &Keypair) -> Result<...>
//   ctx.cast_vote(id: u64, approve: bool, voter: &Keypair)               -> Result<...>
//     — ACTION COMMITMENT (2026-08-14): cast_vote now carries
//       expected_action_hash: [u8; 32]. THIS helper stays hash-free at the call
//       site by fetching the decision and computing the HONEST hash itself
//       (quorum_decision::governed_action_hash(&decision.action)), so every
//       spec below remains valid unchanged. The doctored path gets its own raw
//       helper:
//   ctx.cast_vote_with_hash(id: u64, approve: bool, voter: &Keypair, hash: [u8; 32]) -> Result<...>
//   ctx.emergency_resume(id: u64, invoker: &Keypair)                     -> Result<...>
//   ctx.get_decision(id: u64) -> GovernanceDecision
//   ctx.warp_seconds(secs: i64)   // advance the validator clock
// Assertions below follow the existing suite's style (result.is_ok() / error code).
// Hash VECTOR pins (the codec twins shared with kots-console and the sidecar)
// live in quorum_decision.rs `mod action_hash_vectors` — not duplicated here.
// ===========================================================================
#[cfg(test)]
mod integration_tests {
    use super::*;

    // Convenience: 3 owners, 2-of-3 (the live config), plus a break-glass key.
    // `owners[0..3]` are the signing keypairs; a 4th is used for the 2-of-4 case.

    #[tokio::test]
    async fn happy_path_open_vote_execute() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let (o, bg) = ctx.owners_2of3();          // 3 keypairs + break-glass
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.pause_via_setup().await.unwrap();     // put emergency_state in is_paused=true

        // o[0] opens a resume decision (this is their YES)
        ctx.open_decision(1, GovernedAction::EmergencyResume, &o[0]).await.unwrap();
        let d = ctx.get_decision(1).await.unwrap();
        assert_eq!(d.yes_votes, 1);
        assert!(!d.has_reached_threshold(), "1 of 2 must not be enforceable");
        assert_eq!(d.status, DecisionStatus::Open);

        // o[1] votes YES → threshold (2) reached
        ctx.cast_vote(1, true, &o[1]).await.unwrap();
        let d = ctx.get_decision(1).await.unwrap();
        assert_eq!(d.yes_votes, 2);
        assert!(d.has_reached_threshold());
        assert_eq!(d.status, DecisionStatus::Open, "reaching quorum must NOT close voting");

        // any owner executes → resume happens, decision consumed
        ctx.emergency_resume(1, &o[2]).await.unwrap();
        let d = ctx.get_decision(1).await.unwrap();
        assert_eq!(d.status, DecisionStatus::Executed);
        assert!(!ctx.emergency_is_paused().await.unwrap(), "system must be resumed");

        let _ = bg;
    }

    #[tokio::test]
    async fn third_owner_can_vote_after_quorum_is_reached() {
        // The requested behavior: 2-of-4; after the 2nd YES (enforceable), the
        // 3rd (and 4th) owners can STILL record their ballots, until execution.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_4().await;            // 4 keypairs
        ctx.init_owner_registry(&ctx.pubkeys(&o), 2, ctx.break_glass()).await.unwrap();

        ctx.open_decision(7, GovernedAction::BlacklistAdd { wallet: ctx.some_wallet() }, &o[0]).await.unwrap(); // YES #1
        ctx.cast_vote(7, true, &o[1]).await.unwrap();                                                          // YES #2 → threshold
        let d = ctx.get_decision(7).await.unwrap();
        assert!(d.has_reached_threshold());
        assert_eq!(d.status, DecisionStatus::Open);

        // 3rd owner still votes — MUST succeed and be recorded
        let r = ctx.cast_vote(7, true, &o[2]).await;
        assert!(r.is_ok(), "3rd owner must be able to register a vote after quorum");
        let d = ctx.get_decision(7).await.unwrap();
        assert_eq!(d.yes_votes, 3, "the 3rd ballot is recorded");

        // 4th owner votes NO — also recorded, doesn't undo enforceability
        ctx.cast_vote(7, false, &o[3]).await.unwrap();
        let d = ctx.get_decision(7).await.unwrap();
        assert_eq!(d.no_votes, 1);
        assert!(d.has_reached_threshold(), "a late NO cannot revoke a reached quorum");
    }

    #[tokio::test]
    async fn one_vote_per_owner_immutable() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.open_decision(2, GovernedAction::EmergencyResume, &o[0]).await.unwrap();

        ctx.cast_vote(2, true, &o[1]).await.unwrap();
        // same owner votes again on the same decision → the ballot PDA already
        // exists, so `init` fails (Anchor: account already in use / 0x0).
        let err = ctx.cast_vote(2, false, &o[1]).await.unwrap_err();
        assert!(ctx.is_account_already_in_use(&err), "a second ballot must be rejected");
        // and the proposer cannot double-vote either (open was their ballot)
        let err2 = ctx.cast_vote(2, true, &o[0]).await.unwrap_err();
        assert!(ctx.is_account_already_in_use(&err2));
    }

    #[tokio::test]
    async fn vote_with_wrong_action_hash_is_rejected() {
        // ACTION COMMITMENT: the ballot's expected_action_hash must equal
        // sha256(domain || borsh(decision.action)). A doctored hash — the exact
        // signature a lying UI would produce over a mislabeled decision — dies
        // with ActionHashMismatch, for NO votes as much as YES votes, and the
        // tally does not move.
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.open_decision(13, GovernedAction::EmergencyResume, &o[0]).await.unwrap();

        let e = ctx.cast_vote_with_hash(13, true, &o[1], [0xAB; 32]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::ActionHashMismatch as u32);
        let e2 = ctx.cast_vote_with_hash(13, false, &o[1], [0xCD; 32]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e2), DecisionError::ActionHashMismatch as u32);

        let d = ctx.get_decision(13).await.unwrap();
        assert_eq!(d.yes_votes, 1, "only the proposer's open counts");
        assert_eq!(d.no_votes, 0, "the doctored NO must not be recorded");

        // The honest path (helper computes the matching hash) still lands.
        ctx.cast_vote(13, true, &o[1]).await.unwrap();
        assert_eq!(ctx.get_decision(13).await.unwrap().yes_votes, 2);
    }

    #[tokio::test]
    async fn non_owner_cannot_open_or_vote() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        let stranger = ctx.random_keypair();
        ctx.init_owner_registry_2of3().await.unwrap();

        let e1 = ctx.open_decision_as(3, GovernedAction::EmergencyResume, &stranger).await.unwrap_err();
        assert_eq!(ctx.error_code(&e1), DecisionError::SignerNotOwner as u32);

        ctx.open_decision(3, GovernedAction::EmergencyResume, &o[0]).await.unwrap();
        let e2 = ctx.cast_vote_as(3, true, &stranger).await.unwrap_err();
        assert_eq!(ctx.error_code(&e2), DecisionError::SignerNotOwner as u32);
    }

    #[tokio::test]
    async fn votes_after_window_are_rejected_and_never_reach_threshold() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.open_decision(4, GovernedAction::EmergencyResume, &o[0]).await.unwrap();

        ctx.warp_seconds(VOTE_WINDOW_SECONDS + 1).await; // past the 1-day window
        let e = ctx.cast_vote(4, true, &o[1]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::VotingClosed as u32);
        let d = ctx.get_decision(4).await.unwrap();
        assert!(!d.has_reached_threshold(), "stuck at 1 YES");
    }

    #[tokio::test]
    async fn below_threshold_cannot_execute() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.pause_via_setup().await.unwrap();
        ctx.open_decision(5, GovernedAction::EmergencyResume, &o[0]).await.unwrap(); // only 1 YES

        let e = ctx.emergency_resume(5, &o[0]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::NotEnforceable as u32);
    }

    #[tokio::test]
    async fn no_votes_do_not_count_toward_quorum() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.open_decision(6, GovernedAction::EmergencyResume, &o[0]).await.unwrap(); // YES=1
        ctx.cast_vote(6, false, &o[1]).await.unwrap();                              // NO=1
        let d = ctx.get_decision(6).await.unwrap();
        assert_eq!(d.yes_votes, 1);
        assert_eq!(d.no_votes, 1);
        assert!(!d.has_reached_threshold(), "a NO does not advance quorum");
    }

    #[tokio::test]
    async fn action_and_params_are_bound() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        let x = ctx.some_wallet();

        // A passed BlacklistAdd{X} decision...
        ctx.open_decision(8, GovernedAction::BlacklistAdd { wallet: x }, &o[0]).await.unwrap();
        ctx.cast_vote(8, true, &o[1]).await.unwrap(); // enforceable

        // ...cannot be spent as an emergency_resume
        let e1 = ctx.emergency_resume(8, &o[0]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e1), DecisionError::ActionMismatch as u32);

        // ...and cannot blacklist a DIFFERENT wallet Y (handler passes BlacklistAdd{Y})
        let y = ctx.other_wallet();
        let e2 = ctx.add_to_blacklist(8, y, &o[0]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e2), DecisionError::ActionMismatch as u32);
    }

    #[tokio::test]
    async fn execution_grace_bounds_a_stale_passed_decision() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.pause_via_setup().await.unwrap();
        ctx.open_decision(9, GovernedAction::EmergencyResume, &o[0]).await.unwrap();
        ctx.cast_vote(9, true, &o[1]).await.unwrap(); // enforceable, within window

        // let the window close AND the grace elapse, then try to execute
        ctx.warp_seconds(VOTE_WINDOW_SECONDS + EXECUTION_GRACE_SECONDS + 1).await;
        let e = ctx.emergency_resume(9, &o[0]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::ExecutionWindowExpired as u32);
    }

    #[tokio::test]
    async fn a_decision_executes_at_most_once() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.pause_via_setup().await.unwrap();
        ctx.open_decision(10, GovernedAction::EmergencyResume, &o[0]).await.unwrap();
        ctx.cast_vote(10, true, &o[1]).await.unwrap();

        ctx.emergency_resume(10, &o[0]).await.unwrap();                 // 1st: ok
        let e = ctx.emergency_resume(10, &o[0]).await.unwrap_err();     // 2nd: consumed
        assert_eq!(ctx.error_code(&e), DecisionError::AlreadyExecuted as u32);
    }

    #[tokio::test]
    async fn no_votes_on_an_executed_decision() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.pause_via_setup().await.unwrap();
        ctx.open_decision(11, GovernedAction::EmergencyResume, &o[0]).await.unwrap();
        ctx.cast_vote(11, true, &o[1]).await.unwrap();
        ctx.emergency_resume(11, &o[0]).await.unwrap();

        let e = ctx.cast_vote(11, true, &o[2]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::VotingClosed as u32);
    }

    #[tokio::test]
    async fn owner_rotated_out_mid_decision_can_no_longer_vote() {
        let mut ctx = MultiProgramTestContext::new().await.unwrap();
        let o = ctx.owners_3().await;
        ctx.init_owner_registry_2of3().await.unwrap();
        ctx.open_decision(12, GovernedAction::EmergencyResume, &o[0]).await.unwrap();

        // rotate o[2] out (replace with a fresh key), via a separate quorum action
        let fresh = ctx.random_keypair();
        ctx.rotate_owner_via_quorum(o[2].pubkey(), fresh.pubkey(), &[&o[0], &o[1]]).await.unwrap();

        // o[2] is no longer an owner → its vote is refused
        let e = ctx.cast_vote(12, true, &o[2]).await.unwrap_err();
        assert_eq!(ctx.error_code(&e), DecisionError::SignerNotOwner as u32);
        // the fresh owner CAN vote
        ctx.cast_vote_as(12, true, &fresh).await.unwrap();
    }
}
