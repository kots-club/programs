# KOTS programs

The Solana programs behind [King of the Shill](https://kingoftheshill.com) and the KOTS
token, published so anyone can check that the code running on mainnet is exactly this
code. More on the project: [kots.club](https://kots.club).

## Programs

{{PROGRAMS}}

Every deployed program can be upgraded only by the project's Squads multisig
`J3hS5w9FYsdHJ8R9SgM3quNCP4UbXQ1DsxZK7WXqAGiA`. Check any of them with
`solana program show <address> -u mainnet-beta` (the `Authority` line).

## Building

You need Docker, the [Anchor CLI](https://www.anchor-lang.com/docs/installation) 0.31.1
and the Solana CLI.

```bash
anchor build --verifiable -- --features realm-main
```

- `--verifiable` builds inside Anchor's pinned Docker image (Solana 2.1.0), so every
  machine produces the same bytes.
- `--features realm-main` selects the mainnet program addresses. Without it you build the
  devnet variant, whose bytes differ.

The programs land in `target/verifiable/`.

## Verifying the deployed programs

Each mainnet release is tagged `mainnet-YYYY-MM-DD`; the tag message lists every
program's hash as `solana-verify` prints it, and the same hashes are published at
[kots.club/transparency](https://kots.club/transparency/#programs). Check out the latest
tag, build as above, then compare each program with the chain. For example, the
transfer hook:

```bash
solana-verify get-executable-hash target/verifiable/transfer_hook.so
solana-verify get-program-hash QGYMHWiDQBLi9EZZSTMgEoE6sxKt9Y13kDCsARqG8Qt -u mainnet-beta
```

The two hashes must be identical. (`solana-verify`: `cargo install solana-verify`.)

Without `solana-verify`, compare the bytes directly. The dumped account is longer than the
program, padded with zeros, so compare only the program's length:

```bash
solana program dump QGYMHWiDQBLi9EZZSTMgEoE6sxKt9Y13kDCsARqG8Qt onchain.so -u mainnet-beta
cmp -n "$(stat -c%s target/verifiable/transfer_hook.so)" target/verifiable/transfer_hook.so onchain.so && echo MATCH
```

## Tests

`tests/` holds the program test suites (treasury conservation, timelocks, multisig quorum,
security regressions and more), in the `kots-comprehensive-tests` crate.

## Security

Found a vulnerability? Please report it privately: see [SECURITY.md](SECURITY.md).

## Contributions

This repository is published for transparency and verification and is maintained by the
KOTS team. We don't accept outside pull requests. Questions are welcome on the
[forum](https://kots.club/forum/).
