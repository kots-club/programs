// Insert next to ADMIN_WALLET in
// kots-contracts/shared/kots-common/src/constants.rs
//
// This is the sidecar ADMIN_WALLET_PRIVATE_KEY pubkey on mainnet — the
// owner of prize ATA 6gvw149Y8ghooY1yzRNJLJS1ngEPmEkJu7BMRP82JsML.
// Distinct from ADMIN_WALLET (2KA5zPr8…), which is the fee-split "admin 3%"
// destination.

/// Prize-distribution owner (mainnet sidecar admin). Hook-exempt so
/// vote→prize funding and prize→winner claims are not skimmed 300 bps.
pub const PRIZE_VAULT_OWNER_MAINNET: Pubkey =
    anchor_lang::solana_program::pubkey!("3s27ATy7XjATn5VNoSaufDHq3arcSRpdJXrwYXHKhWqP");
