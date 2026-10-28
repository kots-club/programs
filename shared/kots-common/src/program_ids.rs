//! # THE program-ID registry — the single source of truth for program identity
//!
//! The companion to `hieroglyphs.rs`. That file killed five disagreeing tables of
//! what a glyph IS; this one kills five disagreeing tables of what a PROGRAM is.
//! They were, on 2026-07-16:
//!
//! | table | voting_hieroglyph said | reality |
//! |---|---|---|
//! | the deployed estate | `FzGbgX…` | ← this one |
//! | `Anchor.toml` + `declare_id!()` | `3f64J…` (v3.9.2) | dead estate |
//! | `network_config::devnet()` | `HtPcg8…` | never deployed, anywhere |
//! | `treasury::validate_kots_program_caller` | `3f64J…` | gated 5 CPI paths |
//! | `guide.tsx` | `4roVLp…` | printed under "chain-validated" |
//!
//! Five answers, no two alike, and the one that gated CPI authorisation was
//! wrong — so the security control rejected the real programs. One row per
//! program per realm makes that unrepresentable.
//!
//! ## Why this can be a compiled const and still be network-aware
//!
//! A program cannot read an env var (there is no environment inside BPF —
//! `network_config::detect_network()` tried, which is why it silently resolved
//! to MainnetBeta *on devnet*). It cannot read the genesis hash. There is no
//! syscall for "which cluster am I".
//!
//! But it knows ONE thing about itself with certainty: **its own program id**.
//! Anchor's entrypoint refuses to execute when the runtime `program_id` is not
//! the compiled-in `declare_id!()` — `DeclaredProgramIdMismatch` (4100). That
//! guarantee is not decorative; this platform spent a day inert because of it.
//! So `crate::ID` is a fact the runtime itself enforces, and because **the
//! realms have different addresses**, that fact names the realm:
//!
//! ```ignore
//! let ids = kots_common::program_ids::current(&crate::ID)?;   // ← self-identifying
//! ```
//!
//! No env, no feature flag, no config account to initialise, no clock. A pure
//! function of a constant the loader has already verified. A devnet binary
//! *cannot* claim to be mainnet, because it would have to be deployed at a
//! mainnet address, and then Anchor would refuse to run it at all.
//!
//! ## The one id that must live off-chain (the bootstrap)
//!
//! Off-chain components cannot ask the chain "what are your programs?" without
//! already knowing where to ask. That is irreducible: exactly ONE id must be
//! configured out-of-band. It is `KOTS_PROGRAM_VOTING_HIEROGLYPH` in the sealed
//! `kots-chain-secret`, and it buys all the others via `get_program_ids`.
//!
//! It is also self-checking: the set the chain returns CONTAINS the bootstrap
//! id. If the sealed secret is stale, the sidecar's own id is absent from the
//! estate's answer and the read fails loudly instead of quietly talking to a
//! stranger. One id to trust, and it proves itself.

use anchor_lang::prelude::*;

/// The realms of the platform. Mirrors `ops/<realm>/`, `kots-infra/realms/<realm>`
/// and `KOTS_REALM` in core. There is no `testnet` and no `localnet`: a realm is
/// an estate with an authority model, not a cluster you can point a URL at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Realm {
    /// devnet. Throwaway keys, deployer-held authority.
    Dev,
    /// mainnet-beta. Hardware wallets, Squads 2-of-3, offline keypairs.
    Main,
}

impl Realm {
    pub const fn as_str(self) -> &'static str {
        match self {
            Realm::Dev => "dev",
            Realm::Main => "main",
        }
    }
}

/// The seven programs kots-core actually talks to. `transfer_hook` (Stage 2) and
/// `lp_position_manager` (DEX-only) are deliberately absent: they are not part of
/// the platform's CPI graph, and listing an id nobody calls invites the drift
/// this file exists to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, AnchorSerialize, AnchorDeserialize)]
pub struct ProgramIds {
    pub voting_hieroglyph: Pubkey,
    pub contest_management: Pubkey,
    pub fee_management: Pubkey,
    pub prize_distribution: Pubkey,
    pub progressive_jackpot: Pubkey,
    pub security_governance: Pubkey,
    pub treasury_operations: Pubkey,
}

impl ProgramIds {
    /// All seven, in a fixed order. The order is the wire order of
    /// `get_program_ids`'s return data — do not reorder without the sidecar.
    pub const fn all(&self) -> [Pubkey; 7] {
        [
            self.voting_hieroglyph,
            self.contest_management,
            self.fee_management,
            self.prize_distribution,
            self.progressive_jackpot,
            self.security_governance,
            self.treasury_operations,
        ]
    }

    /// Is this pubkey one of ours? THE authorisation primitive — the answer to
    /// "may this program CPI into me". `treasury_operations` asks this; it used
    /// to ask a hand-copied list that had drifted onto the dead estate.
    pub fn contains(&self, id: &Pubkey) -> bool {
        self.all().iter().any(|k| k == id)
    }
}

/// **Realm dev — devnet.** Deployed 2026-07-15, upgraded in place 2026-07-16.
/// These addresses do not change ("Program IDs are permanent"). The
/// custody that keeps them is `target/deploy/*-keypair.json`, backed up outside
/// the repo; losing one abandons that address forever.
pub const DEV: ProgramIds = ProgramIds {
    voting_hieroglyph: pubkey!("FzGbgXetN5jqZAktTr8KJ2D1BGmYx5kw2Lrp9cJNUArN"),
    contest_management: pubkey!("FMKwB7AnhwHJLq5fphVYJcxZnRf72ECaHdkYApw3yn91"),
    fee_management: pubkey!("8rLAaxGjPpBQJXDT73XnsbHtFSgYkksfEgJ76zvcspHt"),
    prize_distribution: pubkey!("GDrCv9tAh9scvxCTe3XD1N2Xdj2pQYhBtYo68hfDXJqk"),
    progressive_jackpot: pubkey!("7xSq4Fk84ib2195fMnmiwCN9ZyMj6jEp1pimsZicJH6y"),
    security_governance: pubkey!("3EKVa1rSd4cTXB2VT4gWwYfDGjM3m5nw4EYP2jvCb28H"),
    treasury_operations: pubkey!("33LbYv6dGKegxAVNAFWx1An4HtGCacXvEMUy28ZXJvvs"),
};

/// **Realm main — mainnet-beta.** Recorded 2026-09-11 after M10
/// (`solana program show`: Authority = Squads `J3hS5w9FYsdHJ8R9SgM3quNCP4UbXQ1DsxZK7WXqAGiA`
/// on every id below). AMM `A3Rq…` and hook `QGYMH…` stay out of this struct
/// (not on the platform CPI graph). On-chain binaries still carry the old
/// `None` until those seven are rebuilt and Squads Add-Upgrade'd — a later sitting.
pub const MAIN: Option<ProgramIds> = Some(ProgramIds {
    voting_hieroglyph: pubkey!("HZavpBFJgdoCSzBsiaZqHqB68VoushL1i98zPfr3BiAe"),
    contest_management: pubkey!("CdtbPRx3ghk8vjjuXShsKMPeoLXCpqTYDDZHQBsamrXe"),
    fee_management: pubkey!("9UrtrL9uCJVKDGNRTvTMEz2pVD9ogLRuiKBmGheRX2PY"),
    prize_distribution: pubkey!("gQPejCrRxr6y824vTMZ7ad33uwp95L1rRxXMGftBAxa"),
    progressive_jackpot: pubkey!("CAccwwWNqJxCRxuvqQTwXnokVEbUmPCoJX5hibWWnHKg"),
    security_governance: pubkey!("FdsHe63kHRy2BBLHyaZZtJEccKqi9g1EevmhjiLhbB7A"),
    treasury_operations: pubkey!("2A9B5KLWVPbHU36d6eSehxu68Bz5eyPJ5xJz1riKzb98"),
});

/// The realm whose estate contains `self_id`, or `None` if no realm claims it.
///
/// `None` is meaningful and must not be swallowed: it means this binary is
/// running at an address that is in no estate — a stray deployment, a fork, or
/// a mainnet launch that has not been recorded in `MAIN` yet. Callers fail
/// closed. (`kots-core` treats an unreachable registry the same way, plan §2.)
pub fn realm_of(self_id: &Pubkey) -> Option<Realm> {
    if DEV.contains(self_id) {
        return Some(Realm::Dev);
    }
    if let Some(main) = MAIN {
        if main.contains(self_id) {
            return Some(Realm::Main);
        }
    }
    None
}

/// Who is permitted to call a program's `initialize_*` instructions.
///
/// WHY THIS EXISTS (2026-07-23). The initializers are first-caller-wins: each one
/// writes `authority = whoever signed`. Between `solana program deploy` and the
/// moment the ceremony gets round to initializing, ANY wallet watching mainnet
/// can call `initialize_*` and become the authority for the life of the program.
/// Bots do exactly this. It is the only irreversible-by-surprise risk in the
/// launch, and it cannot be closed by a readiness flag — a readiness check
/// confirms the accounts EXIST, not that they are yours.
///
/// Deploying atomically is not available: `solana program deploy` is inherently
/// several transactions, so there is always a window. The fix is therefore to
/// make the instruction refuse anyone else, rather than to try to win the race.
///
/// The multisig address is known BEFORE the programs are built — ceremony step 2
/// creates it, step 4 deploys — so it can be a compile-time constant.
/// Set 2026-10-03: the Squads multisig VAULT (the signer of every executed
/// Squads transaction, and the upgrade authority of all eight mainnet
/// programs per `solana program show`). It was the all-zero placeholder until
/// then, which no one can sign as: initialization was locked, not open.
pub const MAINNET_INITIALIZER: Pubkey = pubkey!("J3hS5w9FYsdHJ8R9SgM3quNCP4UbXQ1DsxZK7WXqAGiA");

/// The rule, expressed over a realm rather than a program id.
///
/// Split out deliberately so the MAINNET branch is unit-testable TODAY, while
/// `MAIN` is still `None` and no mainnet program id exists. A mainnet-only code
/// path that first executes during the launch is precisely the failure mode this
/// codebase has already been bitten by (a 155-test suite that never validated
/// anything; build config keys Anchor silently ignored; fee helpers that log
/// instead of transfer). This one gets exercised on every `cargo test`.
///
/// - `Main`: only the ceremony multisig.
/// - `Dev`:  open. Throwaway keys, deployer-held authority — nothing to front-run,
///           and gating it would break the in-process test harness, whose payer is
///           randomly generated while the .so still carries the DEV declare_id.
/// - `None`: FAIL CLOSED. Per `realm_of`, `None` means a stray deployment, a fork,
///           or an unrecorded mainnet launch. An unrecorded mainnet launch is
///           exactly the case we must not wave through, so the only safe answer
///           is to refuse — with the localnet carve-out below.
pub fn initializer_is_authorized_in(realm: Option<Realm>, signer: &Pubkey) -> bool {
    match realm {
        Some(Realm::Main) => *signer == MAINNET_INITIALIZER,
        Some(Realm::Dev) => true,
        None => false,
    }
}

/// The rule, applied to a program's own id.
///
/// NOTE the localnet carve-out: under `solana-program-test` the .so carries the
/// DEV `declare_id!`, so `realm_of` answers `Some(Realm::Dev)` and this is open —
/// which is what lets the security-regression suite run. A program deployed at an
/// address in no estate answers `None` and is refused.
pub fn initializer_is_authorized(self_id: &Pubkey, signer: &Pubkey) -> bool {
    initializer_is_authorized_in(realm_of(self_id), signer)
}

/// The estate for a realm. `None` for a realm that has not launched.
pub const fn for_realm(realm: Realm) -> Option<ProgramIds> {
    match realm {
        Realm::Dev => Some(DEV),
        Realm::Main => MAIN,
    }
}

/// **THE accessor.** Pass your own `crate::ID`; get your estate back.
///
/// ```ignore
/// let ids = kots_common::program_ids::current(&crate::ID)
///     .ok_or(KotsError::UnknownRealm)?;
/// let fee_program = ids.fee_management;
/// ```
pub fn current(self_id: &Pubkey) -> Option<ProgramIds> {
    realm_of(self_id).and_then(for_realm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the five-table era violated: every id is distinct, and the
    /// table describes exactly one estate.
    #[test]
    fn dev_ids_are_unique() {
        let all = DEV.all();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "duplicate id at {i}/{j}");
            }
        }
    }

    #[test]
    fn dev_ids_are_real_addresses() {
        for id in DEV.all() {
            assert_ne!(id, Pubkey::default(), "placeholder id in DEV");
            assert_ne!(id, anchor_lang::system_program::ID, "system program in DEV");
        }
    }

    // ---- initializer authorization (2026-07-23) -------------------------------
    //
    // The MAINNET branch is covered here, today, while MAIN is still None. That
    // is the whole reason `initializer_is_authorized_in` takes a realm rather
    // than a program id.

    #[test]
    fn mainnet_admits_only_the_ceremony_multisig() {
        assert!(initializer_is_authorized_in(
            Some(Realm::Main),
            &MAINNET_INITIALIZER
        ));
    }

    #[test]
    fn mainnet_refuses_every_other_signer() {
        let bot = pubkey!("9D6PxgLQ1FhFPQZBDLbCJqXKUgpvVU5KcJ3T8qz1nQ7Y");
        assert!(!initializer_is_authorized_in(Some(Realm::Main), &bot));

        // NOTE (2026-07-23, kept for history; resolved 2026-10-03 when the
        // placeholder was replaced): this deliberately does NOT assert that
        // `Pubkey::default()` is refused. MAINNET_INITIALIZER is currently the
        // placeholder `11111111111111111111111111111111`, which IS the all-zero
        // default — so while the placeholder stands, default is the one address
        // that IS authorised, and asserting otherwise contradicts
        // `mainnet_admits_only_the_ceremony_multisig` directly above.
        //
        // That contradiction is the point: it is exactly why the placeholder
        // must not ship. `the_mainnet_initializer_has_been_filled_in` below is
        // the guard for it, and the default case moves there.
    }

    #[test]
    fn devnet_is_open_because_its_keys_are_throwaway() {
        let anyone = pubkey!("9D6PxgLQ1FhFPQZBDLbCJqXKUgpvVU5KcJ3T8qz1nQ7Y");
        assert!(initializer_is_authorized_in(Some(Realm::Dev), &anyone));
    }

    /// `realm_of` documents that `None` means a stray deployment, a fork, or an
    /// UNRECORDED MAINNET LAUNCH. The last of those is the one that must not be
    /// waved through, so the rule fails closed.
    #[test]
    fn an_unclaimed_estate_fails_closed() {
        let anyone = pubkey!("9D6PxgLQ1FhFPQZBDLbCJqXKUgpvVU5KcJ3T8qz1nQ7Y");
        assert!(!initializer_is_authorized_in(None, &anyone));
    }

    /// The in-process harness runs the DEV declare_id, so the guard is open there
    /// and the security-regression suite can execute. If this ever flips, those
    /// tests will start failing on the guard rather than on the property under
    /// test — a confusing failure worth naming in advance.
    #[test]
    fn the_test_harness_realm_is_open() {
        assert!(initializer_is_authorized(
            &DEV.progressive_jackpot,
            &Pubkey::default()
        ));
    }

    /// DELIBERATELY RED until ceremony step 2 fills in the multisig.
    ///
    /// A placeholder that ships to mainnet would admit whoever holds the system
    /// program address — i.e. nobody, locking initialization permanently. Failing
    /// loudly here is cheaper than discovering it at the ceremony.
    /// Was #[ignore] until 2026-10-03, when MAINNET_INITIALIZER was set to the
    /// Squads vault. Now always on: the placeholder can never ship again.
    #[test]
    fn the_mainnet_initializer_has_been_filled_in() {
        assert_ne!(
            MAINNET_INITIALIZER,
            pubkey!("11111111111111111111111111111111"),
            "MAINNET_INITIALIZER is still the placeholder — set it to the Squads \
             multisig before deploying to mainnet"
        );
        // Once a real multisig is set, the all-zero default must be refused like
        // any other stranger. This cannot be asserted while the placeholder IS
        // that default, which is why it lives here rather than in
        // `mainnet_refuses_every_other_signer`.
        assert!(!initializer_is_authorized_in(
            Some(Realm::Main),
            &Pubkey::default()
        ));
    }

    /// A program identifies its own realm from the one fact the loader enforces.
    #[test]
    fn realm_is_self_identifying() {
        assert_eq!(realm_of(&DEV.voting_hieroglyph), Some(Realm::Dev));
        assert_eq!(realm_of(&DEV.treasury_operations), Some(Realm::Dev));
        assert_eq!(current(&DEV.voting_hieroglyph), Some(DEV));
    }

    /// The dead v3.9.2 estate and the never-deployed fourth generation are in
    /// NO realm. If one of these ever answers `Some`, a stale table came back.
    #[test]
    fn abandoned_estates_are_in_no_realm() {
        let dead = [
            pubkey!("3f64JLCsuKBZiRGWhxWtwgc3Vy1ibtA7B9LMThzg1GVF"), // v3.9.2 voting
            pubkey!("6PYMzCL3XtP8XUo3Vwz7hTv85hqovMMVcmRVTzvdQpPs"), // v3.9.2 contest
            pubkey!("HtPcg8aay57oVPJUVMPkok1S3rhwy6t8AKzgNQ6KQnX9"), // never deployed
            pubkey!("4roVLpDfGNnvZUyXLsxU3fdqF25via5XyDqb81iHexyZ"), // the frontend's fiction
        ];
        for id in dead {
            assert_eq!(realm_of(&id), None, "an abandoned id claimed a realm: {id}");
            assert!(!DEV.contains(&id));
        }
    }

    /// Mainnet has not launched. Until it does, it must fail closed rather than
    /// resolve to something plausible.
    #[test]
    // Was `mainnet_does_not_exist_yet`, a tripwire that fired once MAIN was
    // recorded (M10 sitting, 2026-09-11). Replaced 2026-10-03 by its successor:
    // mainnet IS recorded, and a mainnet program identifies itself as Main.
    fn mainnet_is_recorded() {
        assert!(MAIN.is_some(), "MAIN is the mainnet record (M10 sitting, 2026-09-11)");
        assert_eq!(for_realm(Realm::Main), MAIN);
        let main = MAIN.unwrap();
        assert_eq!(realm_of(&main.voting_hieroglyph), Some(Realm::Main));
        assert_eq!(realm_of(&main.fee_management), Some(Realm::Main));
        // Mainnet programs admit only the vault to their initializers.
        assert!(initializer_is_authorized(&main.contest_management, &MAINNET_INITIALIZER));
        assert!(!initializer_is_authorized(&main.contest_management, &Pubkey::default()));
        // The old fiction: mainnet resolving to the v3.9.2 estate.
        assert_eq!(realm_of(&pubkey!("6PYMzCL3XtP8XUo3Vwz7hTv85hqovMMVcmRVTzvdQpPs")), None);
    }

    /// Wire order is a contract with the sidecar's decoder.
    #[test]
    fn wire_order_is_stable() {
        let all = DEV.all();
        assert_eq!(all[0], DEV.voting_hieroglyph);
        assert_eq!(all[6], DEV.treasury_operations);
        assert_eq!(all.len(), 7);
    }
}
