//! Version Heartbeat System
//! 
//! Provides deployment verification through queryable version identifiers.
//! Each program exposes a version_heartbeat instruction that returns a hard-coded
//! version string, enabling clients to verify which build is deployed on-chain.


/// Macro to generate version_heartbeat instruction and context
///
/// Usage in each program:
///
/// ```ignore
/// const VERSION_IDENT: &[u8] = b"program-name|20251004T1630Z|build001";
///
/// #[program]
/// pub mod my_program {
///     kots_common::impl_version_heartbeat!(VERSION_IDENT);
/// }
/// ```
///
/// Marked `ignore` (2026-07-22) to match the convention already used in
/// `program_ids.rs`. This snippet is illustrative, not standalone: it only compiles
/// inside a real Anchor program crate, where `#[program]`, `Context`, `msg!` and a
/// `VERSION_IDENT` const are all in scope. Without the marker `cargo test` compiles
/// it as a doctest in an empty scope and it fails with five cascading
/// "cannot find ..." errors — which is exactly what happened the first time these
/// tests were ever run.
#[macro_export]
macro_rules! impl_version_heartbeat {
    ($version_const:expr) => {
        /// Context for version_heartbeat instruction
        /// Zero accounts required for maximum efficiency
        #[derive(anchor_lang::Accounts)]
        pub struct VersionHeartbeat {}
        
        /// Query deployed program version
        /// 
        /// This instruction returns a hard-coded version identifier that changes
        /// with each build. Clients can query this to verify which version is
        /// deployed on-chain without complex testing.
        /// 
        /// Format: "program-name|timestamp|build-hash"
        /// Example: "contest-management|20251004T1630Z|a3f9c2d"
        pub fn version_heartbeat(_ctx: Context<VersionHeartbeat>) -> Result<()> {
            // Use Solana's set_return_data to return version bytes
            // This is BPF-safe and retrievable via RPC simulation
            anchor_lang::solana_program::program::set_return_data($version_const);
            
            // Also log for convenience
            msg!("📡 Version Heartbeat: {}", 
                core::str::from_utf8($version_const).unwrap_or("invalid-utf8"));
            
            Ok(())
        }
    };
}

/// Helper to format version identifier
/// 
/// Creates a version string in the format: "program-name|timestamp|hash"
/// This should be used when updating VERSION_IDENT constants.
pub fn format_version_ident(program_name: &str, timestamp: &str, hash: &str) -> String {
    format!("{}|{}|{}", program_name, timestamp, hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_format_version_ident() {
        let version = format_version_ident(
            "contest-management",
            "20251004T1630Z",
            "a3f9c2d"
        );
        assert_eq!(version, "contest-management|20251004T1630Z|a3f9c2d");
    }
}
