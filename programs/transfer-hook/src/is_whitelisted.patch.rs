// Replace is_whitelisted in programs/transfer-hook/src/lib.rs
// (the function that currently only pins the two bounty wallets).
//
// Prize distribution pays FROM 3s27ATy7… (sidecar ADMIN_WALLET on mainnet).
// That owner is NOT the compile-time ADMIN_WALLET (2KA5zPr8…). Without this
// pin, inbound vote→prize and outbound prize→winner keep the 3% Token-2022
// withhold. Same pattern as BOUNTY_WALLET_* — realm-independent.

fn is_whitelisted(registry: &Account<WhitelistRegistry>, wallet: &Pubkey) -> bool {
    // OWNER BOUNTY is permanently fee-exempt on BOTH realms.
    *wallet == kots_common::constants::BOUNTY_WALLET_DEVNET
        || *wallet == kots_common::constants::BOUNTY_WALLET_MAINNET
        // Prize-distribution owner (sidecar ADMIN on mainnet). Claims and
        // inbound 70.5% vote slice land on this owner's Token-2022 ATA.
        || *wallet == kots_common::constants::PRIZE_VAULT_OWNER_MAINNET
        || registry.wallets.contains(wallet)
}
