// DESTINATION: kots-contracts/programs/kots-amm/src/state.rs
use anchor_lang::prelude::*;

/// Pool PDA seeds. Lock #11: exactly one pool per mint pair.
pub const POOL_SEED: &[u8] = b"kots-amm";

/// Historical devnet #13 fixture only. Mainnet first add does not read these.
pub const SEED_KOTS: u64 = 10_000_000 * 1_000_000_000;
pub const SEED_WSOL: u64 = 10 * 1_000_000_000;
pub const KOTS_DECIMALS: u8 = 9;
pub const WSOL_DECIMALS: u8 = 9;

#[account]
pub struct Pool {
    pub authority: Pubkey,
    pub mint_kots: Pubkey,
    pub mint_wsol: Pubkey,
    pub vault_kots: Pubkey,
    pub vault_wsol: Pubkey,
    pub lp_mint: Pubkey,
    pub bump: u8,
    pub paused: bool,
    /// Set true after the first both-sides deposit lands. Later adds ratio-match.
    pub seeded: bool,
    pub reserved: [u8; 31],
}

impl Pool {
    pub const SPACE: usize = 8 + 32 * 6 + 1 + 1 + 1 + 31;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum SwapSide {
    /// SOL in, KOTS out.
    Buy,
    /// KOTS in, SOL out.
    Sell,
}
