// DESTINATION: kots-contracts/programs/kots-amm/src/errors.rs
use anchor_lang::prelude::*;

#[error_code]
pub enum AmmError {
    #[msg("kots-amm mainnet program id is generated at the launch ceremony and does not exist yet")]
    MainnetIdMissing,
    #[msg("pool is paused")]
    Paused,
    #[msg("math overflow")]
    MathOverflow,
    #[msg("output below min_out (user-received)")]
    Slippage,
    #[msg("empty pool or one-sided add after init")]
    InvalidLiquidity,
    #[msg("pool already initialized for this mint pair")]
    AlreadyInitialized,
    #[msg("mint is not the hooked fixture / missing TransferFeeConfig / Stage-1 mint")]
    UnhookedMint,
    #[msg("LP mint must be unhooked classic SPL")]
    HookedLpMint,
    #[msg("hook ExtraAccountMeta list incomplete")]
    IncompleteHookAccounts,
    #[msg("unauthorized")]
    Unauthorized,
    #[msg("quote mint must be native WSOL")]
    WrongQuoteMint,
    #[msg("vault token account is not owned by the pool PDA")]
    VaultOwner,
    #[msg("vault mint mismatch")]
    VaultMint,
    #[msg("Token-2022 program required for KOTS vault / mint")]
    WrongTokenProgram,
    #[msg("classic SPL token program required for LP mint / WSOL vault")]
    WrongLpProgram,
    #[msg("never call register_protocol_pool for AMM vaults")]
    ForbiddenProtocolPool,
    #[msg("first deposit is any both-sides dust via TransferChecked; MintTo seed is retired")]
    BadSeedDeposit,
    #[msg("remove-liquidity on an empty pool")]
    EmptyPool,
    #[msg("captain/bounty WSOL ATA owner is not INCOMING_WALLET / BOUNTY_WALLET")]
    WrongFeeWallet,
}
