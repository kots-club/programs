//! Network Configuration Module
//!
//! Provides network-aware configuration for program IDs, wallet addresses,
//! and other network-specific constants. Automatically detects the current
//! network and provides the appropriate configuration.

use anchor_lang::prelude::*;
// C1: only `detect_network()` uses this, and that is now gated to off-chain
// builds. Without the same gate here, every BPF build warns `unused import`.
#[cfg(not(target_os = "solana"))]
use std::env;

/// Network types supported by KOTS
///
/// Derives AnchorSerialize/AnchorDeserialize so it can be passed directly as an
/// instruction argument: on-chain programs cannot auto-detect their network
/// (detect_network() always defaults to Mainnet on-chain), so the deploy ceremony
/// passes the correct Network to fee_config initialization and the program looks
/// wallets up via SystemWallets::for_network — one table, resolved at deploy time.
#[derive(Clone, Debug, PartialEq, Copy, AnchorSerialize, AnchorDeserialize)]
pub enum Network {
    Devnet,
    MainnetBeta,
    Testnet,
    Localnet,
}

impl Network {
    pub fn as_str(&self) -> &str {
        match self {
            Network::Devnet => "devnet",
            Network::MainnetBeta => "mainnet-beta",
            Network::Testnet => "testnet",
            Network::Localnet => "localnet",
        }
    }
}

/// Re-export, NOT a redeclaration. The program-ID table lives in ONE place
/// (`program_ids.rs`) and this module borrows it. The struct that used to be
/// declared here owned four hand-maintained copies of the estate — devnet,
/// mainnet, testnet, localnet — and every one of them was wrong in a different
/// way. A type alias cannot drift.
pub use crate::program_ids::ProgramIds;

/// **C1 / D11 — the realm is decided at COMPILE time, and defaults to Dev.**
///
/// `detect_network()` below reads env vars, which do not exist in BPF. On-chain
/// its default branch was always taken, so every deployed program resolved
/// MainnetBeta regardless of where it was running. This constant replaces that
/// guess with a build flag.
///
/// The default is deliberately inverted from the old behaviour: a build WITHOUT
/// `realm-main` resolves Dev and can never resolve Main. Producing a mainnet
/// artifact requires opting in explicitly:
///
/// ```text
/// anchor build -- --features realm-main
/// ```
#[cfg(feature = "realm-main")]
pub const REALM: crate::program_ids::Realm = crate::program_ids::Realm::Main;
#[cfg(not(feature = "realm-main"))]
pub const REALM: crate::program_ids::Realm = crate::program_ids::Realm::Dev;

/// The realm expressed as a `Network`, for the accessors that still take one.
pub const fn realm_network() -> Network {
    match REALM {
        crate::program_ids::Realm::Main => Network::MainnetBeta,
        crate::program_ids::Realm::Dev => Network::Devnet,
    }
}

/// System wallet configuration for a network
#[derive(Clone, Debug)]
pub struct SystemWallets {
    pub treasury: Pubkey,
    pub incoming: Pubkey,
    pub outgoing: Pubkey,
    pub burn: Pubkey,
    pub admin: Pubkey,
    pub emergency: Pubkey,
    pub jackpot: Pubkey,
    pub dev_fund: Pubkey,
    pub kots_mint: Pubkey,
    /// 1% owner bounty recipient (fee-exempt in the transfer hook).
    pub bounty: Pubkey,
}

impl SystemWallets {
    /// Resolve the wallet set for an EXPLICITLY-passed network, WITHOUT building
    /// the full NetworkConfig (which allocates String rpc_url/explorer_base).
    ///
    /// This is the accessor on-chain programs must use. `detect_network()` cannot
    /// work inside a deployed program (no env vars, no RPC url, no shell — it
    /// always falls through to its Mainnet default), so the network is resolved at
    /// DEPLOY time: the ceremony passes a Network flag as an instruction argument
    /// and the program looks the wallets up here. The addresses live in exactly
    /// ONE place — this table — for every program and both networks.
    ///
    /// Kept deliberately allocation-free (returns Copy-able Pubkeys, no Strings)
    /// so pulling kots-common into the size-constrained transfer-hook adds as
    /// little to the .so as possible.
    pub fn for_network(network: Network) -> Self {
        match network {
            Network::Devnet => Self {
                treasury: pubkey!("F17Eo7JZDZrJZqyevXWuJ7tQUxYrh4ttCVrrGzVrAWBu"),
                incoming: pubkey!("67xZhPqDhbpMet621zqYEwmUVL4funczGif1nKAQPWL"),
                outgoing: pubkey!("3bSDece3Zif68Y6ZtStUN611J5TuMTo9HmZX3kNWNj9s"),
                burn: pubkey!("11111111111111111111111111111112"),
                admin: pubkey!("F17Eo7JZDZrJZqyevXWuJ7tQUxYrh4ttCVrrGzVrAWBu"),
                emergency: pubkey!("67xZhPqDhbpMet621zqYEwmUVL4funczGif1nKAQPWL"),
                jackpot: pubkey!("3bSDece3Zif68Y6ZtStUN611J5TuMTo9HmZX3kNWNj9s"),
                dev_fund: pubkey!("F17Eo7JZDZrJZqyevXWuJ7tQUxYrh4ttCVrrGzVrAWBu"),
                kots_mint: pubkey!("BiPAn9bMY7Xo5TSwkXbPrXbmsWsXhi4GFbQ7DaVteHwZ") /* C12 (2026-10-02): was 6v4zr7… (pre-2026-07-15 mint); the realm dev mint is BiPAn9… per ops/dev/dev-mint.json */,
                bounty: pubkey!("6ZuFby6oKQ3bS7surHU9scKVj9dcvnUuMbAt5mT7zB3J"),
            },
            // ─────────────────────────── C8 / D12 ───────────────────────────
            // Mainnet fee-distribution set (must stay aligned with constants.rs
            // realm-main pins and transfer-hook INCOMING_BPS / TREASURY remainder).
            //
            // 2026-08-21 economic mapping (handlers in transfer-hook):
            //   incoming  = 700 BPS (7%)  — Captain's share
            //   treasury  = remainder (~36%) — platform residual
            //   jackpot   = 5000 BPS (50%)
            //   admin     = 300 BPS (3%)
            //   dev_fund  = 300 BPS (3%)
            //
            // Captain (7%) owner confirmation: 6wQtCc5…
            // Platform residual:               HW1G2x…
            // outgoing mirrors residual treasury.
            // kots_mint placeholder until Phase 3 re-mint.
            // emergency = Owner 1 (76Jo6Qzc…).
            Network::MainnetBeta => Self {
                // Residual ~36% (NOT captain)
                treasury: pubkey!("HW1G2xVnAkudRGT6buZEY8QL6UaPu291pyKwHALj3AkD"),
                // Captain 7%
                incoming: pubkey!("6wQtCc5NzdUGuEaP4PhVcxh8kEA1PZiMbt312pD3Qa4"),
                // Mirrors residual treasury
                outgoing: pubkey!("HW1G2xVnAkudRGT6buZEY8QL6UaPu291pyKwHALj3AkD"),
                burn: pubkey!("11111111111111111111111111111112"),
                admin: pubkey!("2KA5zPr8oHv5dLr67NN2s9fiwajRCEuYtQiQFJP1W6ME"),
                emergency: pubkey!("76Jo6QzcAskwghcn7wmcjTos8SMsjTDBpNNMeV9nk3XX"),
                jackpot: pubkey!("5wEeX8zYxSGiwZpqpUWmzTyRFjcN8hDFpXF6BWUKgUsP"),
                dev_fund: pubkey!("Drbz3GJNr2Qcpe76hsL2wEGa4qMnSEFRbMcNBjk9ouvA"),
                kots_mint: pubkey!("5iKKRjEdYbdQLCmSvUqUu81pvZicy1Xi9CBxhZDgEW8u") /* C2 (2026-10-02): was the 1111…1112 placeholder; the mainnet mint, minted 2026-08-21 */,
                bounty: pubkey!("BbvBeQy9oEmF9D5eVtj8hp1tibvMhtFFgVdiD4EqtT9V"),
            },
            // Testnet reuses its placeholder set; the full NetworkConfig block
            // below remains the documented home. On-chain only Devnet and
            // MainnetBeta are ever passed by the ceremony.
            Network::Testnet => Self {
                treasury: pubkey!("1111111111111111111111111111111H"),
                incoming: pubkey!("1111111111111111111111111111111J"),
                outgoing: pubkey!("1111111111111111111111111111111K"),
                burn: pubkey!("11111111111111111111111111111112"),
                admin: pubkey!("1111111111111111111111111111111L"),
                emergency: pubkey!("1111111111111111111111111111111M"),
                jackpot: pubkey!("1111111111111111111111111111111N"),
                dev_fund: pubkey!("1111111111111111111111111111111P"),
                kots_mint: pubkey!("1111111111111111111111111111111Q"),
                bounty: pubkey!("1111111111111111111111111111111Y"),
            },
            Network::Localnet => Self {
                treasury: pubkey!("1111111111111111111111111111111Q"),
                incoming: pubkey!("1111111111111111111111111111111R"),
                outgoing: pubkey!("1111111111111111111111111111111S"),
                burn: pubkey!("11111111111111111111111111111112"),
                admin: pubkey!("1111111111111111111111111111111T"),
                emergency: pubkey!("1111111111111111111111111111111U"),
                jackpot: pubkey!("1111111111111111111111111111111V"),
                dev_fund: pubkey!("1111111111111111111111111111111W"),
                kots_mint: pubkey!("1111111111111111111111111111111X"),
                bounty: pubkey!("1111111111111111111111111111111Z"),
            },
        }
    }
}

/// Complete network configuration
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    pub network: Network,
    pub program_ids: ProgramIds,
    pub system_wallets: SystemWallets,
    pub rpc_url: String,
    pub explorer_base: String,
}

impl NetworkConfig {
    /// Resolve a config for an optionally-supplied network, falling back to
    /// runtime detection when none is given.
    ///
    /// Lifted here 2026-07-22 from seven near-duplicate `network_utils.rs` copies,
    /// one per program. Those files were thin pass-throughs to this module; only
    /// this single function was ever called (by security-governance and
    /// voting-hieroglyph), while ~86 dead-code warnings came from the rest. The
    /// copies had already drifted — every one had a different checksum — which is
    /// the same failure mode that let the wallet tables diverge.
    ///
    /// NOTE: the `None` branch calls `detect_network()`, which cannot work inside a
    /// deployed program (no env vars, no RPC url, no shell) and always falls
    /// through to Mainnet. Callers on-chain should pass `Some(network)` explicitly.
    pub fn for_optional_network(network: Option<Network>) -> Self {
        match network {
            Some(net) => Self::for_network(net),
            None => Self::current(),
        }
    }

    /// Get configuration for a specific network
    pub fn for_network(network: Network) -> Self {
        match network {
            Network::Devnet => Self::devnet(),
            Network::MainnetBeta => Self::mainnet(),
            Network::Testnet => Self::testnet(),
            Network::Localnet => Self::localnet(),
        }
    }

    /// The config for the realm this binary was BUILT for.
    ///
    /// C1: was `Self::for_network(detect_network())`. That call cannot work
    /// on-chain and always fell through to MainnetBeta, so `current()` was the
    /// single largest source of the "every deployed program thinks it is on
    /// mainnet" defect. It now resolves from the compile-time `REALM`, which is
    /// Dev unless the crate was built with `--features realm-main`.
    ///
    /// Off-chain callers that genuinely want environment sniffing should call
    /// `detect_network()` directly and pass the result to `for_network`.
    pub fn current() -> Self {
        Self::for_network(realm_network())
    }

    /// Devnet configuration (current live deployment)
    fn devnet() -> Self {
        Self {
            network: Network::Devnet,
            program_ids: crate::program_ids::DEV, // realm dev — one table (program_ids.rs)
            system_wallets: SystemWallets::for_network(Network::Devnet),
            rpc_url: "https://api.devnet.solana.com".to_string(),
            explorer_base: "https://explorer.solana.com/?cluster=devnet".to_string(),
        }
    }

    /// Mainnet configuration
    fn mainnet() -> Self {
        Self {
            network: Network::MainnetBeta,
            // Prefer MAIN when populated; otherwise DEV keeps off-chain tooling
            // resolvable until program_ids::MAIN is sealed post-ceremony.
            program_ids: crate::program_ids::MAIN.unwrap_or(crate::program_ids::DEV),
            system_wallets: SystemWallets::for_network(Network::MainnetBeta),
            rpc_url: "https://api.mainnet-beta.solana.com".to_string(),
            explorer_base: "https://explorer.solana.com".to_string(),
        }
    }

    /// Testnet configuration
    fn testnet() -> Self {
        Self {
            network: Network::Testnet,
            program_ids: crate::program_ids::DEV, // no testnet estate; dev ids keep this resolvable
            system_wallets: SystemWallets::for_network(Network::Testnet),
            rpc_url: "https://api.testnet.solana.com".to_string(),
            explorer_base: "https://explorer.solana.com/?cluster=testnet".to_string(),
        }
    }

    /// Localnet configuration for development
    fn localnet() -> Self {
        Self {
            network: Network::Localnet,
            program_ids: crate::program_ids::DEV, // localnet mirrors the dev estate
            system_wallets: SystemWallets::for_network(Network::Localnet),
            rpc_url: "http://127.0.0.1:8899".to_string(),
            explorer_base: "http://localhost:3000".to_string(),
        }
    }

    /// Get program ID by name for this network
    pub fn get_program_id(&self, program_name: &str) -> Option<Pubkey> {
        match program_name {
            "voting_hieroglyph" => Some(self.program_ids.voting_hieroglyph),
            "contest_management" => Some(self.program_ids.contest_management),
            "fee_management" => Some(self.program_ids.fee_management),
            "prize_distribution" => Some(self.program_ids.prize_distribution),
            "progressive_jackpot" => Some(self.program_ids.progressive_jackpot),
            "security_governance" => Some(self.program_ids.security_governance),
            "treasury_operations" => Some(self.program_ids.treasury_operations),
            _ => None,
        }
    }

    /// Get system wallet by name for this network
    pub fn get_system_wallet(&self, wallet_name: &str) -> Option<Pubkey> {
        match wallet_name {
            "treasury" => Some(self.system_wallets.treasury),
            "incoming" => Some(self.system_wallets.incoming),
            "outgoing" => Some(self.system_wallets.outgoing),
            "burn" => Some(self.system_wallets.burn),
            "admin" => Some(self.system_wallets.admin),
            "emergency" => Some(self.system_wallets.emergency),
            "jackpot" => Some(self.system_wallets.jackpot),
            "dev_fund" => Some(self.system_wallets.dev_fund),
            "kots_mint" => Some(self.system_wallets.kots_mint),
            "bounty" => Some(self.system_wallets.bounty),
            _ => None,
        }
    }
}

/// Auto-detect the current network from env vars and RPC URL.
///
/// **C1: OFF-CHAIN ONLY.** Gated on `not(target_os = "solana")` because none of
/// what it reads — env vars, a shell, the `solana` binary — exists inside a
/// deployed program. On-chain this function silently fell through to its final
/// branch, which returned MainnetBeta, so every program believed it was on
/// mainnet no matter where it ran. On-chain code must use `REALM` /
/// `realm_network()` instead; this remains for CLI tooling and tests.
#[cfg(not(target_os = "solana"))]
pub fn detect_network() -> Network {
    // Check explicit network override
    if let Ok(network) = env::var("KOTS_NETWORK") {
        return match network.as_str() {
            "devnet" => Network::Devnet,
            "mainnet-beta" | "mainnet" => Network::MainnetBeta,
            "testnet" => Network::Testnet,
            "localnet" => Network::Localnet,
            _ => Network::Devnet,
        };
    }

    // Check Anchor provider URL
    if let Ok(url) = env::var("ANCHOR_PROVIDER_URL") {
        if url.contains("devnet") {
            return Network::Devnet;
        } else if url.contains("mainnet") {
            return Network::MainnetBeta;
        } else if url.contains("testnet") {
            return Network::Testnet;
        } else if url.contains("localhost") || url.contains("127.0.0.1") {
            return Network::Localnet;
        }
    }

    // Check Solana config (if available)
    if let Ok(output) = std::process::Command::new("solana")
        .args(&["config", "get"])
        .output()
    {
        if let Ok(config) = String::from_utf8(output.stdout) {
            if config.contains("devnet") {
                return Network::Devnet;
            } else if config.contains("mainnet") {
                return Network::MainnetBeta;
            } else if config.contains("testnet") {
                return Network::Testnet;
            } else if config.contains("localhost") {
                return Network::Localnet;
            }
        }
    }

    // C1: was `Network::MainnetBeta` — "default to mainnet for production".
    // Inverted deliberately. An unidentifiable environment is not evidence of
    // production; defaulting to the network where mistakes are irreversible is
    // backwards. Mainnet is now reached only by building with `realm-main`.
    Network::Devnet
}

/// Convenience functions for common network configurations
pub mod config {
    use super::*;

    /// Get current network configuration
    pub fn current() -> NetworkConfig {
        NetworkConfig::current()
    }

    /// Get devnet configuration
    pub fn devnet() -> NetworkConfig {
        NetworkConfig::for_network(Network::Devnet)
    }

    /// Get mainnet configuration
    pub fn mainnet() -> NetworkConfig {
        NetworkConfig::for_network(Network::MainnetBeta)
    }

    /// Get current program IDs
    pub fn program_ids() -> ProgramIds {
        current().program_ids
    }

    /// Get current system wallets
    pub fn system_wallets() -> SystemWallets {
        current().system_wallets
    }

    /// Get current network name
    pub fn network_name() -> String {
        current().network.as_str().to_string()
    }

    /// Check if we're on mainnet
    pub fn is_mainnet() -> bool {
        matches!(current().network, Network::MainnetBeta)
    }

    /// Check if we're on devnet
    pub fn is_devnet() -> bool {
        matches!(current().network, Network::Devnet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "solana"))]
    #[test]
    fn test_network_detection() {
        env::set_var("KOTS_NETWORK", "devnet");
        assert_eq!(detect_network(), Network::Devnet);

        env::set_var("KOTS_NETWORK", "mainnet");
        assert_eq!(detect_network(), Network::MainnetBeta);
    }

    #[test]
    fn test_devnet_config() {
        let config = NetworkConfig::for_network(Network::Devnet);
        assert_eq!(config.network, Network::Devnet);
        assert_eq!(
            config.program_ids.voting_hieroglyph,
            crate::program_ids::DEV.voting_hieroglyph,
            "devnet config must resolve to the DEV estate in the program-ID registry"
        );
        assert_eq!(
            crate::program_ids::realm_of(&config.program_ids.voting_hieroglyph),
            Some(crate::program_ids::Realm::Dev),
            "devnet's voting_hieroglyph must be registered as a Dev-realm id"
        );
    }

    /// Captain 7% is `incoming`; residual is `treasury` (2026-08-21 swap).
    #[test]
    fn test_mainnet_captain_on_incoming_leg() {
        let w = SystemWallets::for_network(Network::MainnetBeta);
        // Captain 7%
        assert_eq!(
            w.incoming.to_string(),
            "6wQtCc5NzdUGuEaP4PhVcxh8kEA1PZiMbt312pD3Qa4"
        );
        // Platform residual
        assert_eq!(
            w.treasury.to_string(),
            "HW1G2xVnAkudRGT6buZEY8QL6UaPu291pyKwHALj3AkD"
        );
        // outgoing mirrors residual treasury
        assert_eq!(w.outgoing, w.treasury);
        assert_eq!(
            w.jackpot.to_string(),
            "5wEeX8zYxSGiwZpqpUWmzTyRFjcN8hDFpXF6BWUKgUsP"
        );
        assert_eq!(
            w.dev_fund.to_string(),
            "Drbz3GJNr2Qcpe76hsL2wEGa4qMnSEFRbMcNBjk9ouvA"
        );
        assert_eq!(
            w.admin.to_string(),
            "2KA5zPr8oHv5dLr67NN2s9fiwajRCEuYtQiQFJP1W6ME"
        );
        assert_eq!(
            w.bounty.to_string(),
            "BbvBeQy9oEmF9D5eVtj8hp1tibvMhtFFgVdiD4EqtT9V"
        );
    }
}