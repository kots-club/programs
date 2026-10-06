// DESTINATION (where this file goes): kots-contracts/programs/transfer-hook/src/lib.rs
// kots-contracts/programs/transfer-hook/src/lib.rs
use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token_interface::{self as token_interface, Mint, TokenAccount, TokenInterface};
use bytemuck::{Pod, Zeroable};
use spl_transfer_hook_interface::instruction::{ExecuteInstruction};
use spl_tlv_account_resolution::{account::ExtraAccountMeta, seeds::Seed, state::ExtraAccountMetaList};
use solana_program::sysvar::instructions;
use solana_program::sysvar::rent::Rent;
// Shared network table (single source of truth for wallet addresses). The hook
// resolves distribution wallets from here by a deploy-time Network flag rather
// than hardcoding, so devnet and mainnet share one documented table.
use kots_common::network_config::{Network, SystemWallets};

// Pure fee/split/cap arithmetic, extracted so it can be unit-tested WITHOUT a
// validator (`cargo test -p transfer-hook`). See fees.rs for why: every arithmetic
// bug found on 2026-07-22 was pure integer math that no test could reach because it
// lived inline inside instruction handlers.
//
// STATUS (2026-08-21): pure arithmetic in fees.rs uses u128 intermediates so
// mainnet-scale supply × BPS cannot overflow. The on-path sites below that do
// the same class of math (anti-whale cap, sell-fee-debt split, SOL distribute)
// now also use u128. The handlers have not yet been refactored to *call*
// fees::apply_bps / fees::split_pot; they duplicate the pattern inline so the
// BPF stack stays flat. Keep both in sync — if BPS tables change, update fees.rs
// constants AND the inline legs here.
pub mod fees;

#[cfg(feature = "realm-main")]
declare_id!("QGYMHWiDQBLi9EZZSTMgEoE6sxKt9Y13kDCsARqG8Qt");

#[cfg(not(feature = "realm-main"))]
declare_id!("2hqT83nFSaR8U4KDyFw9557A5W2FNsWHXmqSb5DuSxiH");

// ============================================================================
// CONSTANTS
// ============================================================================

// BPF-safe constants to stay under 4KB stack limit
//
// PERMANENT ONCE INITIALIZED. Both values size their accounts via #[max_len]
// below and there is no realloc instruction, so whatever mainnet is initialized
// with is the ceiling for the life of the deployment. Raising either one later
// means a new account and a migration.
//
// 64 confirmed 2026-07-28 (KOTS-MAINNET-PLAN C4, AC-C4). Baseline need is 9 —
// treasury, six fee destinations, keeper, hot wallet — with migration cancelled
// under D9. The previous value of 20 left 11 free forever, which is enough
// until the first partner integration or second keeper and then never again.
// Cost of 64 is 44 x 32 = 1,408 further bytes, roughly 0.01 SOL of rent.
const MAX_WHITELISTED_WALLETS: usize = 64;
// 10 CONFIRMED 2026-07-28 by the operator, alongside 64 for the whitelist.
//
// PERMANENT. `#[max_len(MAX_POOL_ADDRESSES)]` sizes the pool registry and there
// is no realloc — ten registered pools is the ceiling for the life of the
// deployment, across every venue, forever.
//
// The staged multi-venue plan (O28) spends against this deliberately: Meteora at
// launch, Raydium and others as each becomes viable for a live-hook Token-2022
// mint. Ten is not obviously generous once you count concentrated and standard
// pools on the same DEX as separate registrations, plus any future migration.
// Recorded as a decision taken with that in view, not a default left in place.
const MAX_POOL_ADDRESSES: usize = 10;

// ── O32 (2026-07-29) — the queue's capacity is a HEAP budget, not a storage
// preference. ───────────────────────────────────────────────────────────────
//
// `spl-tlv-account-resolution`'s `ExtraAccountMetaList::add_to_cpi_instruction`
// resolves the meta list by COPYING THE FULL DATA of every listed account onto
// the CALLER's heap:
//
//     .map(|account| account.try_borrow_data().unwrap().to_vec())
//
// Not the keys — the contents. So every CPI-initiated transfer of this mint
// materialises the whole of `extra_account_metas` on whoever made the CPI.
//
// At the original capacity of 1024 the queue alone was 49,164 bytes against a
// 32 KB default BPF heap, so ANY DEX router swapping KOTS died with an
// out-of-memory panic inside the router, which reverted the swap. A router does
// not request a heap frame for an arbitrary token — only the transaction can,
// and the router builds the transaction. This was never reachable from the hook
// lab, because a DIRECT transfer does not run the CPI resolver at all.
//
// 1024 was not a capacity requirement. The original comment gives the reason as
// "1024 = bytemuck supported size" — a serialization convenience. bytemuck 1.23
// implements `Pod` for `[T; N]` over const generics so any N works, and 128 is
// also in the pre-const-generics fixed list, so this is safe on either.
//
// ⚠ PERMANENT ONCE INITIALIZED. `initialize_refund_queue` requires the account's
// data_len to equal REFUND_QUEUE_SIZE exactly, and the array length is baked
// into the on-chain layout. Changing this after the mint exists is a migration
// (close + re-create the queue, close + re-init the meta list), not an edit.
//
// ⚠ TYPE NOTE — these are `u32`, not `usize`, and that is deliberate.
//
// `#[constant]` exports a constant into the IDL, which requires a type Anchor
// can represent. `usize` has no IDL representation — it is platform-dependent —
// so Anchor implements no `IdlBuild` for it and the macro expansion fails with
// `no associated function or constant named 'get_full_path' found for type
// usize`. That error appears ONLY under `anchor idl build`; a plain
// `cargo check` compiles fine and tells you nothing. (Found 2026-07-29.)
//
// So: `u32` is the exported, client-facing form; the `_USIZE` aliases below are
// the internal form used for array lengths and index arithmetic. One number,
// two views, tied together by the assertions further down.
//
// ── BISECTION (2026-07-29) ──────────────────────────────────────────────────
//
// The default is 64, and it is PROVISIONAL. Two endpoints are measured:
//
//   capacity 128 -> 13,227 B copied -> OOM in the resolver          (measured)
//   no queue     ->  7,027 B copied -> resolver survived, hook ran  (measured)
//
// So the true ceiling lies somewhere in (7,027 .. 13,227) and nothing between
// those endpoints has been tested. 64 is the largest power of two whose
// modelled copy (10,155 B) falls under the corrected guard below; it is a
// candidate, not a result. `ops/o32-bisect.sh` sweeps the alternatives.
//
// The `queue-cap-*` features are TEMPORARY bisection instruments. They are
// mutually exclusive BY CONSTRUCTION — each cfg excludes the ones above it —
// because two live definitions of a `#[constant]` emit the same Anchor symbol
// twice and fail with E0428, which is how the duplicate `#[constant]` on this
// very constant was caught earlier today.
// EXPERIMENT A (2026-07-29). Capacity 1 — the smallest queue that can exist:
// 56 B header + one 48 B slot = 104 B. This is the discriminator between the
// two surviving hypotheses, because at 104 bytes the queue is smaller than
// fee_config (270 B) and barely larger than fee_authority (41 B):
//
//   executes at 104 B -> the constraint is MARGINAL BYTES, threshold just under
//                        the 824 B that failed. Find ~2 KB elsewhere and the
//                        queue can stay.
//   OOMs at 104 B     -> the queue's PRESENCE in the meta list is fatal
//                        regardless of size. No capacity ever works, and the
//                        only route is moving refunds off the execute path
//                        (O34 events), because the hook WRITES this account.
//
// It is not a shippable capacity — a one-slot queue is full after one exempt
// transfer, and the queue-full branch is untested. It exists to answer one
// question.
#[cfg(feature = "queue-cap-1")]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 1;
#[cfg(feature = "queue-cap-1")]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 104;

// EXPERIMENT B (2026-07-29). Capacity 8 — 56 B header + 8 x 48 B = 440 B.
//
// Chosen because `proc_contest_revenue` distributes FOUR legs and each exempt
// leg queues one refund, so 8 clears the four with headroom. Capacity 1 proved
// the heap is fine at 104 B but filled after leg 1, which pushed leg 2 into the
// queue-full branch — and that branch is not merely untested, it is impossible:
// it calls withdraw_withheld_tokens_from_accounts via invoke_signed on accounts
// that Token-2022 hands to a hook as READ-ONLY, NON-SIGNER, and the runtime
// answers "Cross-program invocation with unauthorized signer or writable
// account". Same constraint the SOL leg died of, recorded 2026-07-22.
//
// So 8 is the first capacity that can complete all four legs without touching
// the impossible path. It is a DIAGNOSTIC capacity, not a shipping decision:
// the real one depends on the heap ceiling (bisect upward from here) and on
// B9's drain-rate measurement, which does not exist yet.
#[cfg(all(feature = "queue-cap-8", not(feature = "queue-cap-1")))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 8;
#[cfg(all(feature = "queue-cap-8", not(feature = "queue-cap-1")))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 440;

#[cfg(all(feature = "queue-cap-16", not(any(feature = "queue-cap-1", feature = "queue-cap-8"))))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 16;
#[cfg(all(feature = "queue-cap-16", not(any(feature = "queue-cap-1", feature = "queue-cap-8"))))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 824;

#[cfg(all(feature = "queue-cap-32", not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16"))))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 32;
#[cfg(all(feature = "queue-cap-32", not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16"))))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 1592;

#[cfg(all(
    feature = "queue-cap-256",
    not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16", feature = "queue-cap-32", feature = "queue-cap-128"))
))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 256;
#[cfg(all(
    feature = "queue-cap-256",
    not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16", feature = "queue-cap-32", feature = "queue-cap-128"))
))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 12344;

#[cfg(all(
    feature = "queue-cap-128",
    not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16", feature = "queue-cap-32"))
))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 128;
#[cfg(all(
    feature = "queue-cap-128",
    not(any(feature = "queue-cap-1", feature = "queue-cap-8", feature = "queue-cap-16", feature = "queue-cap-32"))
))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 6200;

// ═══════════════════════════════════════════════════════════════════════════
// CAPACITY DECISION — 64. Recorded 2026-07-29 with the reasoning, because this
// number is PERMANENT ONCE INITIALIZED and cannot be revisited after the mint.
// ═══════════════════════════════════════════════════════════════════════════
//
// THE HEAP NO LONGER CONSTRAINS IT. Post-split, `ops/o32-bisect.sh` passed
// money_movement end-to-end at EVERY capacity swept:
//
//     cap    queue B    meta-list copy    of the 32 KB default heap
//       8        440             7,467                         23%
//      16        824             7,851                         24%
//      32      1,592             8,619                         26%
//      64      3,128            10,155                         31%
//     128      6,200            13,227                         40%
//     256     12,344            19,371                         59%
//
// The sweep found no ceiling, so it bounds nothing. The choice is a trade, and
// the trade is NOT symmetric.
//
// WHY NOT BIGGER, EVEN THOUGH 256 PASSED. The Gate A probe runs inside THIS
// program, with THIS context. A Meteora or Jupiter router does its own work and
// then CPIs the transfer, on the same 32 KB heap. At 256 we consume 59% of it
// before any of the router's allocations are counted, and no third-party router
// has ever touched this mint. The probe is pessimistic in one respect (it
// carries the full four-leg Anchor context) and OPTIMISTIC in another (our
// program allocates far less than a router does before reaching the CPI).
// Nothing measured so far separates those.
//
// So:
//   * a LARGER queue makes exempt-transfer liveness safer, and eats headroom
//     for code we can neither see nor fix;
//   * a SMALLER queue protects that headroom, and fills sooner — and a full
//     queue reverts exempt transfers, because the overflow branch cannot work
//     (O35: Token-2022 passes hook accounts read-only and non-signer).
//
// The asymmetry decides it. The liveness problem is FIXABLE IN OUR SOFTWARE —
// repair or remove the overflow branch, implement the drain, run the keeper more
// often. The router-headroom problem is not fixable by us at all. Tradeability
// is what the whole of 2026-07-29 was spent establishing; spending its margin to
// paper over a bug we can fix is the wrong direction.
//
// 64 leaves ~22 KB for the router. 128 is defensible. 256 is not, and the sweep
// passing at 256 is not evidence that a ROUTER would.
//
// ⚠ REVISIT THIS ONLY WITH: (a) Gate A B9's measured refund arrival rate against
// keeper latency, which does not exist, and (b) a real router observed swapping
// this mint on devnet. Not before, and never after the mint is initialized.

// ⚠ DOC COMMENTS HERE ARE PUBLISHED TO EVERY CLIENT.
//
// `#[constant]` copies the `///` block into the IDL's `constants` entry. On
// 2026-07-29 the doc describing REFUND_QUEUE_SIZE sat above REFUND_QUEUE_CAPACITY
// — Rust attaches a doc comment to the NEXT item — so the IDL told clients that
// the constant whose value is 64 was "Account length in bytes … = 3,128". Each
// `///` block below is attached to the constant it actually describes. Use `//`
// for anything that is commentary rather than client-facing documentation.

/// Number of pending refunds the ring holds. DEFAULT = 64.
///
/// This is a HEAP budget as much as a capacity: resolving this mint's transfer
/// hook copies every `extra_account_metas` account onto the CPI caller's 32 KB
/// heap, and a DEX router has no more than that. 64 leaves it ~22 KB. Every
/// capacity from 8 to 256 passed the harness, so this is an operational choice
/// rather than a hard limit; the full sweep and the reasoning are recorded at the
/// constant in `programs/transfer-hook/src/lib.rs`, which an IDL consumer cannot
/// see from here.
///
/// ⚠ PERMANENT once the refund queue account is initialized.
#[cfg(not(any(
    feature = "queue-cap-1",
    feature = "queue-cap-8",
    feature = "queue-cap-16",
    feature = "queue-cap-32",
    feature = "queue-cap-128",
    feature = "queue-cap-256"
)))]
#[constant]
pub const REFUND_QUEUE_CAPACITY: u32 = 64;

/// Byte length of the refund queue account. DEFAULT = 3,128.
///
/// Derivation: 8 discriminator + 32 authority + 4x u16 + 8 last_updated
///             + 64 * 48 refunds = 3,128.
///
/// A LITERAL, not an expression: `#[constant]` renders the token stream into the
/// IDL, so an expression would put `8 + std::mem::size_of::<…>()` in front of
/// every TypeScript client instead of a number. No digit separator either — the
/// IDL carries the value as a STRING and JavaScript's `Number("3_128")` is NaN.
///
/// Two compile-time assertions keep the literal honest: one ties it to the real
/// struct layout, the other ties it to the capacity, so neither can drift.
///
/// ⚠ The account is created CLIENT-SIDE and `initialize_refund_queue` checks
/// `data_len` for EXACT equality. Read this from the IDL rather than restating
/// it; `scripts/refund-queue-size.ts` is the single source for TypeScript.
#[cfg(not(any(
    feature = "queue-cap-1",
    feature = "queue-cap-8",
    feature = "queue-cap-16",
    feature = "queue-cap-32",
    feature = "queue-cap-128",
    feature = "queue-cap-256"
)))]
#[constant]
pub const REFUND_QUEUE_SIZE: u32 = 3128;

/// Internal view of the capacity. Array lengths and `%` need `usize`.
pub const REFUND_QUEUE_CAPACITY_USIZE: usize = REFUND_QUEUE_CAPACITY as usize;

/// Internal view of the size. `data_len()` and `minimum_balance()` take `usize`.
pub const REFUND_QUEUE_SIZE_USIZE: usize = REFUND_QUEUE_SIZE as usize;

const _: () = assert!(
    REFUND_QUEUE_SIZE_USIZE == 8 + std::mem::size_of::<PendingRefundQueue>(),
    "REFUND_QUEUE_SIZE no longer matches PendingRefundQueue's real layout. A field \
     was added, removed or reordered. Update the literal above — and note that \
     the queue account is created CLIENT-SIDE, so every script that allocates it \
     must change with it."
);

const _: () = assert!(
    REFUND_QUEUE_SIZE_USIZE
        == 8 + 32 + 2 + 2 + 2 + 2 + 8 + (REFUND_QUEUE_CAPACITY_USIZE * 48),
    "REFUND_QUEUE_SIZE and REFUND_QUEUE_CAPACITY disagree. The capacity was \
     changed without updating the size literal, which would produce an account \
     of the wrong length that initialize_refund_queue rejects."
);

// ── O32 GUARD — the invariant, enforced by the compiler ─────────────────────
//
// This is the part that has to outlive the fix. O32 was not a bug in a line of
// code; it was an account added to the meta list by someone who had no reason
// to know that adding it charged its full size to a stranger's heap. The next
// person has the same lack of reason, so the build has to be the one that
// knows.
//
// Only the program-owned accounts are const-computable. The remainder — the
// Instructions sysvar (transaction-dependent, and the largest unknown here),
// five distribution wallets, three program accounts and the TLV meta list
// itself — is charged at a flat allowance, roughly 2.5x the ~1,666 bytes
// measured on 2026-07-28.
//
// `pub` is load-bearing, not stylistic. These are referenced only from the
// anonymous `const _: () = assert!(…)` below, and rustc's dead_code lint does
// not count an anonymous const as a use — a private version of each of these
// warned "never used" on 2026-07-29 while simultaneously being the thing
// enforcing the invariant. Exporting them also lets the test suite compare its
// own independent computation against the program's, which is a stronger check
// than either alone.
pub const META_LIST_NON_CONST_ALLOWANCE: usize = 4_096;

pub const META_LIST_COPY_BYTES: usize = REFUND_QUEUE_SIZE_USIZE
    + (8 + WhitelistRegistry::INIT_SPACE)
    + (8 + PoolRegistry::INIT_SPACE)
    + (8 + FeeConfig::INIT_SPACE)
    + (8 + FeeAuthority::INIT_SPACE)
    + META_LIST_NON_CONST_ALLOWANCE;

// The heap a program gets when nobody asked for more. This is what a router has.
pub const DEFAULT_BPF_HEAP_BYTES: usize = 32 * 1024;

// ⚠ CORRECTED 2026-07-29 — this was `DEFAULT_BPF_HEAP_BYTES / 2` (16,384), a
// fraction chosen because it felt conservative. It was not. The build passed a
// configuration modelling 13,227 bytes that OOMs on chain, and reported a
// 3,157-byte margin that did not exist — a guard that reads as a pass while
// measuring something adjacent, which is the failure this codebase keeps
// recording, this time in the guard written to prevent it.
//
// The number below is now derived from MEASUREMENT, not from taste:
//
//   13,227 B  -> OOM in the resolver                      (measured 2026-07-29)
//    7,027 B  -> resolver survived, hook executed          (measured 2026-07-29)
//
// 13,000 sits just under the known-failing point, which is the most this can
// honestly claim: everything between 7,027 and 13,227 is UNTESTED, so passing
// this guard means "not yet known to fail", not "known to work".
//
// RAISE IT ONLY BY MEASUREMENT. `ops/o32-bisect.sh` narrows the window; when it
// does, set this to the largest capacity proven to execute and record the run.
// ⚠ RAISED 2026-07-29 AND DELIBERATELY WEAKENED TO A NECESSARY CONDITION.
//
// This was 13,000, fitted to PRE-SPLIT measurements of a four-leg instruction.
// It would now REFUSE capacity 128 — truncating the very sweep meant to find the
// real number — on a model already falsified twice: capacities 16..128
// (824..6,200 B) ALL failed while removing the queue entirely succeeded, so
// bytes were never the discriminator; and `proc_contest_revenue` is now two
// two-leg instructions, each with a fresh heap, so the accumulation this number
// was fitted to no longer exists.
//
// The honest bound is the only one certainly true: a copy larger than the whole
// default heap can never work. NECESSARY, not sufficient. Passing means "not
// obviously impossible" and nothing more.
//
// The real ceiling is empirical — `ops/o32-bisect.sh` sweeping capacities
// post-split. When it yields a number, put THAT here with the run that measured
// it. Until then this must not be cited as evidence a configuration is safe; it
// has been cited that way twice and was wrong twice.
pub const META_LIST_COPY_CEILING: usize = DEFAULT_BPF_HEAP_BYTES;

// ⚠ THE BYTE CEILING ABOVE IS NOT KNOWN TO BE THE BINDING CONSTRAINT EITHER.
// Capacities 16..128 (824..6,200 B) all failed; removing the queue entirely
// succeeded. Retained only as a cheap sanity bound on an absurd account —
// do NOT cite it as evidence that a configuration is safe.

const _: () = assert!(
    META_LIST_COPY_BYTES <= META_LIST_COPY_CEILING,
    "O32: extra_account_metas exceeds the CPI heap budget. A DEX router will run \
     out of memory resolving this mint's hook and EVERY swap will revert. Shrink \
     REFUND_QUEUE_CAPACITY, or remove an account from \
     initialize_extra_account_meta_list_impl. Do not raise the ceiling."
);

/// The fee the Token-2022 Transfer Fee Extension ACTUALLY withheld on `amount`.
///
/// Mirrors `spl_token_2022::extension::transfer_fee::TransferFee::calculate_fee`,
/// and the part that matters is that the fee **ROUNDS UP**:
///
/// ```text
/// fee = ceil(amount * transfer_fee_bps / 10_000)
/// ```
///
/// ⚠ WHY THIS EXISTS (2026-07-29). The refund path computed `amount * bps /
/// 10_000` with integer division — i.e. FLOOR — so every exempt transfer whose
/// fee had a remainder refunded ONE RAW UNIT LESS than the runtime withheld.
/// Measured: a 123,456,789,012,345 leg had 3,703,703,670,371 withheld and
/// 3,703,703,670,370 queued for refund.
///
/// Per transfer that is dust. It is also permanent and cumulative: the shortfall
/// stays stranded as withheld fee on a wallet that was promised a FULL rebate,
/// and `process_refunds_bulk` withdraws the short figure, so the withheld
/// balance never reaches zero and the reconciliation Gate A B7 asks for can
/// never balance. "Whitelisted wallets pay 0%" was not true.
///
/// ⚠ ASSUMES `maximum_fee == u64::MAX`. Token-2022 also CAPS the fee at the
/// mint's `maximum_fee`, which this cannot see from inside the hook. If the
/// ceremony sets a real cap, large transfers are withheld LESS than this returns
/// and the refund path would over-refund — worse than the bug it replaces,
/// because `process_refunds_bulk` would then try to withdraw more withheld than
/// exists. The ceremony MUST create the mint with `maximum_fee = u64::MAX`.
/// Recorded as an open item; `scripts/check_mint_extensions.ts` already prints
/// the value, so verifying it is a read, not a build.
pub fn withheld_fee_for(amount: u64, transfer_fee_bps: u16) -> u64 {
    if transfer_fee_bps == 0 || amount == 0 {
        return 0;
    }
    let numerator = (amount as u128).saturating_mul(transfer_fee_bps as u128);
    let raw = numerator / 10_000;
    let fee = if numerator % 10_000 > 0 { raw + 1 } else { raw };
    // The fee can never exceed the transfer itself. Reachable only for amounts
    // so small that the round-up consumes all of them — which is not an edge
    // case: at 300 bps every transfer below 34 raw units is 100% fee.
    core::cmp::min(fee, amount as u128) as u64
}

// head/tail/count are u16 and every index below is `% REFUND_QUEUE_CAPACITY`.
const _: () = assert!(REFUND_QUEUE_CAPACITY_USIZE > 0);
const _: () = assert!(REFUND_QUEUE_CAPACITY_USIZE <= u16::MAX as usize);

// Price precision for fixed-point arithmetic
const PRICE_PRECISION: u64 = 1_000_000_000; // 1e9 scaling factor

// SOL fee distribution percentages (in basis points out of 10000)
// For DEX buys (1%) and sells (3%) in SOL
// C3: the SOL split constants. Used only by distribute_sol_fees, so without the
// same gate they become dead code and warn on every default build.
#[cfg(feature = "sol-fees")]
const JACKPOT_BPS_SOL: u64 = 5000; // 50%
#[cfg(feature = "sol-fees")]
const INCOMING_BPS_SOL: u64 = 700; // 7% — Captain's share, uniform with KOTS. 2026-07-22: CORRECTED from an inverted 34%.
#[cfg(feature = "sol-fees")]
const ADMIN_BPS_SOL: u64 = 300; // 3%
#[cfg(feature = "sol-fees")]
const TREASURY_BPS_SOL: u64 = 3700; // 37% (includes the 3% burn-redirect — SOL is not burned). 2026-07-22: was 10%; absorbed the corrected incoming share.
#[cfg(feature = "sol-fees")]
const DEV_BPS_SOL: u64 = 300; // 3%

// KOTS fee distribution percentages (in basis points out of 10000)
// For regular transfers (1%) in KOTS
// C3 follow-up 2026-07-28. These four are DUPLICATES of the authoritative
// constants in fees.rs (which has tests). Their only use in this file is inside
// collect_sell_fee_debt_part1, which C3 gated — so without the same gate they
// warn `never used` on every default build. TREASURY_BPS_KOTS and BURN_BPS_KOTS
// below were already marked #[allow(dead_code)] for the same reason; this is the
// same situation reached by a different route.
#[cfg(feature = "sol-fees")]
const JACKPOT_BPS_KOTS: u64 = 5000; // 50%
#[cfg(feature = "sol-fees")]
const INCOMING_BPS_KOTS: u64 = 700; // 7% — Captain's share (main investor profit). 2026-07-22: CORRECTED from an inverted 36%; this and treasury were swapped.
#[cfg(feature = "sol-fees")]
const ADMIN_BPS_KOTS: u64 = 300; // 3%
// Retained as documentation of the intended share: treasury is paid as the REMAINDER, never from this constant.
// Not referenced in this crate; the live split lives in fees.rs with tests.
#[allow(dead_code)]
const TREASURY_BPS_KOTS: u64 = 3500; // 35% — documentation only; live remainder math is fees.rs. Guide table as of 2026-08-27. NOT live until the next hook upgrade.
#[cfg(feature = "sol-fees")]
const DEV_BPS_KOTS: u64 = 300; // 3%
// Retained as documentation of the intended share: the burn leg is executed keeper-side under Design B.
// Not referenced in this crate; the live split lives in fees.rs with tests.
#[allow(dead_code)]
const BURN_BPS_KOTS: u64 = 100; // 1% (2026-07-22: reduced from 3%; the freed 2% now falls to treasury via the remainder, per the operations ruling)

// ═══ ANTI-WHALE (contractual: the guide's Ownership Protection promise) ═══
// Maximum individual ownership: 3% of CURRENT supply — dynamic on purpose:
// every real burn tightens the cap automatically, so "3%" stays true as
// supply falls. Enforced per destination token account inside the hook (the
// only place a violation can REVERT rather than merely be observed); the
// whitelist and pool registry are the two exemption lists. Added 2026-07-21;
// proven by hook-lab golden cases W1–W5 before any ceremony.
const MAX_HOLD_BPS: u64 = 300;

// PDA seeds
const FEE_AUTHORITY_SEED: &[u8] = b"fee_authority";

// Known DEX Program IDs for swap detection

// EXPECTED WARNING — do not "fix" this by adding #![allow(deprecated)].
//
// Anchor 0.31.1's #[program] macro expands to a call to the deprecated
// AccountInfo::realloc. The call is in GENERATED code emitted at crate root, not
// inside this module, so a module-level #[allow(deprecated)] does not reach it —
// tried 2026-07-28, the warning was unchanged.
//
// The only thing that silences it is a crate-level #![allow(deprecated)], and
// that is a bad trade here: `solana_program::system_instruction` is being
// deprecated in favour of the `solana-system-interface` crate, six of these
// programs use it, and the test crate already warns about it. Blanket-silencing
// would hide that migration the moment it lands on the program crates.
//
// One unactionable warning per program is the correct cost. It goes away on the
// next Anchor upgrade, not before.
#[program]
pub mod transfer_hook {
    use super::*;

    // ========================================================================
    // INITIALIZATION INSTRUCTIONS
    // ========================================================================

    /// Initialize the Fee Authority PDA that will have permission to withdraw fees
    /// This PDA MUST be set as the withdraw authority when creating the Token-2022 mint
    pub fn initialize_fee_authority(ctx: Context<InitializeFeeAuthority>) -> Result<()> {
        let fee_authority = &mut ctx.accounts.fee_authority;
        fee_authority.authority = ctx.accounts.authority.key();
        fee_authority.bump = ctx.bumps.fee_authority;
        
        msg!("Fee Authority PDA initialized");
        msg!("Fee Authority address: {}", fee_authority.key());
        msg!("NOTE: Set this address as the withdraw_withheld_authority when creating the KOTS mint");
        
        Ok(())
    }

    /// Initialize the whitelist registry with platform wallets
    pub fn initialize_whitelist(
        ctx: Context<InitializeWhitelist>,
        wallets: Vec<Pubkey>,
    ) -> Result<()> {
        require!(
            wallets.len() <= MAX_WHITELISTED_WALLETS,
            ErrorCode::TooManyWallets
        );

        let registry = &mut ctx.accounts.whitelist_registry;
        registry.authority = ctx.accounts.authority.key();
        registry.wallets = wallets;
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("Whitelist initialized with {} wallets", registry.wallets.len());
        Ok(())
    }

    /// Initialize the DEX pool registry
    pub fn initialize_pool_registry(
        ctx: Context<InitializePoolRegistry>,
    ) -> Result<()> {
        let registry = &mut ctx.accounts.pool_registry;
        registry.authority = ctx.accounts.authority.key();
        registry.pools = Vec::new();
        registry.protocol_pool_address = None;
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("Pool registry initialized");
        Ok(())
    }

    /// Close the fee_config PDA (authority-only), returning rent to the authority.
    ///
    /// Added 2026-07-22 for the anti-whale flag: adding `anti_whale_active` changed
    /// the FeeConfig layout, so an existing PDA (e.g. on devnet) cannot be read by
    /// the new hook. There is no in-place migration for this on dev — the account
    /// holds no precious state (wallet addresses re-derive from config, prices are
    /// re-set by the keeper on first run), so the clean path is close-then-reinit:
    /// call this, then `initialize_fee_config` fresh with the new struct. Guarded by
    /// `has_one = authority` so only the program authority can close it. On mainnet
    /// this is never needed — the ceremony inits the PDA fresh with the new layout.
    /// Close the fee_config PDA (authority-only), returning rent to the authority.
    ///
    /// Added 2026-07-22 for the anti-whale flag: adding `anti_whale_active` changed
    /// the FeeConfig layout, so an existing PDA (e.g. on devnet) cannot be read by
    /// the new hook. LAYOUT-AGNOSTIC ON PURPOSE: this reads the raw account rather
    /// than deserializing `FeeConfig`, because the stale on-chain account was
    /// written with the OLD (shorter) layout and `Account<FeeConfig>` would fail to
    /// deserialize it — making the account un-closable exactly when we need to close
    /// it. `authority` is the first field (bytes [8..40], right after the 8-byte
    /// discriminator) and that offset is identical in both layouts, so we verify
    /// against it directly, then drain lamports and zero the account. Never needed
    /// on mainnet (the ceremony inits fresh with the new layout).
    pub fn close_fee_config(ctx: Context<CloseFeeConfig>) -> Result<()> {
        let acc = ctx.accounts.fee_config.to_account_info();
        let data = acc.try_borrow_data()?;
        // 8-byte Anchor discriminator, then the authority Pubkey (first field).
        require!(data.len() >= 40, ErrorCode::InvalidInstruction);
        let mut auth_bytes = [0u8; 32];
        auth_bytes.copy_from_slice(&data[8..40]);
        let stored_authority = Pubkey::new_from_array(auth_bytes);
        require!(
            stored_authority == ctx.accounts.authority.key(),
            ErrorCode::Unauthorized
        );
        drop(data);

        // Manual close: move all lamports to the authority and zero the data so
        // the runtime reclaims the (now rent-unfunded, empty) account.
        let dest = ctx.accounts.authority.to_account_info();
        let mut acc_lamports = acc.try_borrow_mut_lamports()?;
        let mut dest_lamports = dest.try_borrow_mut_lamports()?;
        **dest_lamports = dest_lamports
            .checked_add(**acc_lamports)
            .ok_or(ErrorCode::MathOverflow)?;
        **acc_lamports = 0;
        drop(acc_lamports);
        drop(dest_lamports);
        acc.try_borrow_mut_data()?.fill(0);

        msg!("fee_config PDA closed by authority (layout-agnostic); rent returned. Re-init with initialize_fee_config.");
        Ok(())
    }

    /// Close the fee_authority PDA (authority-only). Added 2026-07-22 alongside
    /// close_fee_config so the hook-lab is fully resettable on a single deployment:
    /// the lab's four fixed-seed PDAs (fee_authority, fee_config, whitelist,
    /// pool_registry) live at the same addresses every bringup, so a re-run must
    /// close them first. Never needed on mainnet (the ceremony inits once).
    pub fn close_fee_authority(_ctx: Context<CloseFeeAuthority>) -> Result<()> {
        msg!("fee_authority PDA closed by authority; rent returned.");
        Ok(())
    }

    /// Close the whitelist_registry PDA (authority-only). See close_fee_authority.
    /// Re-run bringup re-seeds the whitelist with freshly-generated role wallets,
    /// so the old registry MUST be cleared or the exemption tests reference wallets
    /// that no longer exist in the new lab universe.
    pub fn close_whitelist(_ctx: Context<CloseWhitelist>) -> Result<()> {
        msg!("whitelist_registry PDA closed by authority; rent returned.");
        Ok(())
    }

    /// Close the pool_registry PDA (authority-only). See close_fee_authority.
    pub fn close_pool_registry(_ctx: Context<ClosePoolRegistry>) -> Result<()> {
        msg!("pool_registry PDA closed by authority; rent returned.");
        Ok(())
    }

    /// Initialize fee configuration with hardcoded distribution wallets
    pub fn initialize_fee_config(ctx: Context<InitializeFeeConfig>, network: Network) -> Result<()> {
        let fee_config = &mut ctx.accounts.fee_config;

        // Distribution wallets resolved from the shared single-source table by the
        // deploy-time network flag (the ceremony passes Devnet or Mainnet). No
        // hardcoded addresses here — devnet and mainnet both come from
        // SystemWallets::for_network in kots-common/network_config.rs.
        let w = SystemWallets::for_network(network);
        fee_config.jackpot_wallet = w.jackpot;
        fee_config.incoming_wallet = w.incoming;
        fee_config.admin_wallet = w.admin;
        fee_config.treasury_wallet = w.treasury;
        fee_config.dev_wallet = w.dev_fund;
        fee_config.burn_address = w.burn;
        
        // Fee basis points
        // A' fee model (2026-07-22): the extension rate is the MAXIMUM anyone pays
        // — the SELL rate. Buys and ordinary transfers are rebated back down to
        // dex_buy_fee_bps by the keeper. MUST equal the mint's actual
        // TransferFeeConfig rate or refunds/rebates will be wrong.
        fee_config.transfer_fee_bps = 300;      // 3% KOTS withheld on every transfer
        fee_config.dex_buy_fee_bps = 100;       // 1% SOL
        fee_config.dex_sell_fee_bps = 300;      // 3% SOL
        
        // Initialize default KOTS/SOL price (1M KOTS per SOL)
        // This should be updated immediately by keeper script reading actual pool reserves
        fee_config.kots_per_sol_price = 1_000_000 * PRICE_PRECISION;
        fee_config.price_last_updated = Clock::get()?.unix_timestamp;
        
        fee_config.authority = ctx.accounts.authority.key();
        // Anti-whale protection ON from genesis: circulating mcap starts far below
        // the $2M relax threshold. The keeper flips this false once mcap crosses $2M.
        fee_config.anti_whale_active = true;
        fee_config.last_updated = Clock::get()?.unix_timestamp;
        
        msg!("Fee config initialized with 6 distribution wallets ({:?})", network);
        msg!("Transfer fee: {}bps, Buy: {}bps, Sell: {}bps", 
            fee_config.transfer_fee_bps,
            fee_config.dex_buy_fee_bps,
            fee_config.dex_sell_fee_bps
        );
        msg!("Default KOTS/SOL price: 1M KOTS per SOL (update via keeper script)");
        msg!("NOTE: SOL fees calculated using DEX pool reserve pricing");
        
        Ok(())
    }

    /// Migrate FeeConfig to include refund_queue field
    /// 
    /// **CRITICAL UPGRADE PATH:** Extends existing fee_config PDA by 32 bytes to add refund_queue field
    /// Must be called BEFORE initializing refund_queue if fee_config already exists
    pub fn migrate_fee_config(ctx: Context<MigrateFeeConfig>, refund_queue: Pubkey) -> Result<()> {
        let fee_config_account = ctx.accounts.fee_config.to_account_info();
        let current_size = fee_config_account.data_len();
        // FeeConfig is stored packed (Borsh): INIT_SPACE bytes, exactly what
        // initialize_fee_config allocates. std::mem::size_of is the IN-MEMORY size,
        // padding included (288 vs 287 on 2026-10-03), and reading that many bytes
        // panicked: "range end index 288 out of range for slice of length 287".
        let new_size = 8 + FeeConfig::INIT_SPACE;
        
        msg!("Migrating fee_config: {} → {} bytes", current_size, new_size);
        
        // Manually deserialize current data (handles old padding)
        let fee_config_data = fee_config_account.try_borrow_mut_data()?;
        
        // Skip discriminator (8 bytes)
        let data_slice = &fee_config_data[8..];
        // Borsh reads exactly the fields it needs and returns an error (never panics)
        // if the account is shorter than the current layout.
        let mut fee_config: FeeConfig = AnchorDeserialize::deserialize(&mut &data_slice[..])?;
        
        // Verify authority
        require!(
            fee_config.authority == ctx.accounts.authority.key(),
            ErrorCode::Unauthorized
        );
        
        msg!("Current authority: {}", fee_config.authority);
        
        // Update refund_queue field
        fee_config.refund_queue = refund_queue;
        
        // Drop mutable borrow before realloc
        drop(fee_config_data);
        
        // Realloc to exact size (removes padding)
        if current_size != new_size {
            let rent = Rent::get()?;
            let new_lamports = rent.minimum_balance(new_size);
            let current_lamports = fee_config_account.lamports();
            
            if new_lamports > current_lamports {
                let additional_rent = new_lamports - current_lamports;
                msg!("Funding additional rent: {} lamports", additional_rent);
                system_program::transfer(
                    CpiContext::new(
                        ctx.accounts.system_program.to_account_info(),
                        system_program::Transfer {
                            from: ctx.accounts.payer.to_account_info(),
                            to: fee_config_account.clone(),
                        },
                    ),
                    additional_rent,
                )?;
            }
            
            fee_config_account.resize(new_size)?;
            msg!("✅ Fee config reallocated: {} → {} bytes", current_size, new_size);
        }
        
        // Reserialize with updated data
        let mut fee_config_data = fee_config_account.try_borrow_mut_data()?;
        let dst = &mut fee_config_data[8..];
        let mut cursor = std::io::Cursor::new(dst);
        fee_config.serialize(&mut cursor)?;
        
        msg!("✅ Fee config migrated with refund_queue: {}", refund_queue);
        Ok(())
    }

    /// Update fee configuration wallet addresses
    /// 
    /// **CRITICAL:** This fixes corrupted or outdated wallet addresses in fee_config
    /// Call this after program upgrade to update wallet addresses to current hardcoded values
    /// 
    /// **NOTE:** Only the program authority can call this
    pub fn update_fee_config(ctx: Context<UpdateFeeConfig>, network: Network) -> Result<()> {
        let fee_config = &mut ctx.accounts.fee_config;

        // Same single-source resolution as initialize_fee_config: wallets come from
        // SystemWallets::for_network by the passed-in network flag, never hardcoded.
        let w = SystemWallets::for_network(network);
        fee_config.jackpot_wallet = w.jackpot;
        fee_config.incoming_wallet = w.incoming;
        fee_config.admin_wallet = w.admin;
        fee_config.treasury_wallet = w.treasury;
        fee_config.dev_wallet = w.dev_fund;
        fee_config.burn_address = w.burn;
        
        fee_config.last_updated = Clock::get()?.unix_timestamp;
        
        msg!("Fee config wallets updated from shared table ({:?})", network);
        msg!("Jackpot: {}", fee_config.jackpot_wallet);
        msg!("Incoming: {}", fee_config.incoming_wallet);
        msg!("Admin: {}", fee_config.admin_wallet);
        msg!("Treasury: {}", fee_config.treasury_wallet);
        msg!("Dev: {}", fee_config.dev_wallet);
        
        Ok(())
    }

    /// Update fee rates (BPS values) for transfer and DEX fees
    /// (Authority-only)
    /// 
    /// **PURPOSE:** Modify fee rates after initialization (governance/adjustment)
    /// **VALIDATION:** Enforces 0 < BPS ≤ 1000 (max 10% fee)
    /// **MAINNET MIGRATION:** Primary method to set production fee rates (100/100/300)
    pub fn update_fee_rates(
        ctx: Context<UpdateFeeRates>,
        transfer_fee_bps: u16,
        dex_buy_fee_bps: u16,
        dex_sell_fee_bps: u16,
    ) -> Result<()> {
        let fee_config = &mut ctx.accounts.fee_config;
        
        require!(
            ctx.accounts.authority.key() == fee_config.authority,
            ErrorCode::Unauthorized
        );
        
        require!(
            transfer_fee_bps > 0 && transfer_fee_bps <= 1000,
            ErrorCode::InvalidFeeRate
        );
        require!(
            dex_buy_fee_bps > 0 && dex_buy_fee_bps <= 1000,
            ErrorCode::InvalidFeeRate
        );
        require!(
            dex_sell_fee_bps > 0 && dex_sell_fee_bps <= 1000,
            ErrorCode::InvalidFeeRate
        );
        
        let unchanged = 
            fee_config.transfer_fee_bps == transfer_fee_bps &&
            fee_config.dex_buy_fee_bps == dex_buy_fee_bps &&
            fee_config.dex_sell_fee_bps == dex_sell_fee_bps;
        
        require!(!unchanged, ErrorCode::NoChangeDetected);
        
        msg!("=== Updating Fee Rates ===");
        msg!("Old rates:");
        msg!("  Transfer: {} BPS ({}%)", fee_config.transfer_fee_bps, fee_config.transfer_fee_bps / 100);
        msg!("  DEX Buy:  {} BPS ({}%)", fee_config.dex_buy_fee_bps, fee_config.dex_buy_fee_bps / 100);
        msg!("  DEX Sell: {} BPS ({}%)", fee_config.dex_sell_fee_bps, fee_config.dex_sell_fee_bps / 100);
        
        fee_config.transfer_fee_bps = transfer_fee_bps;
        fee_config.dex_buy_fee_bps = dex_buy_fee_bps;
        fee_config.dex_sell_fee_bps = dex_sell_fee_bps;
        fee_config.last_updated = Clock::get()?.unix_timestamp;
        
        msg!("New rates:");
        msg!("  Transfer: {} BPS ({}%)", transfer_fee_bps, transfer_fee_bps / 100);
        msg!("  DEX Buy:  {} BPS ({}%)", dex_buy_fee_bps, dex_buy_fee_bps / 100);
        msg!("  DEX Sell: {} BPS ({}%)", dex_sell_fee_bps, dex_sell_fee_bps / 100);
        msg!("Updated by authority: {}", ctx.accounts.authority.key());
        
        Ok(())
    }

    /// Update KOTS/SOL spot price from DEX pool reserves (authority-only)
    /// 
    /// **PURPOSE:** Update the on-chain spot price used for SOL fee calculations
    /// Called by off-chain keeper script that reads actual pool reserves via RPC
    /// 
    /// **PRICE FORMAT:** `kots_per_sol_price` is scaled by 1e9 (PRICE_PRECISION)
    /// Example: 1_000_000 * 1e9 = 1M KOTS per SOL
    /// 
    /// **KEEPER CALCULATION:**
    /// 1. Read SOL vault balance and KOTS vault balance from on-chain pool
    /// 2. Calculate: `kots_per_sol = (KOTS_balance / SOL_balance) * 1e9`
    /// 3. Call this instruction with the calculated price
    /// 
    /// **NOTE:** Only the program authority can call this
    pub fn update_spot_price(
        ctx: Context<UpdateSpotPrice>,
        kots_per_sol_price: u64,
    ) -> Result<()> {
        require!(
            ctx.accounts.authority.key() == ctx.accounts.fee_config.authority,
            ErrorCode::Unauthorized
        );
        
        // Validate price is non-zero
        require!(
            kots_per_sol_price > 0,
            ErrorCode::InvalidSpotPrice
        );
        
        let fee_config = &mut ctx.accounts.fee_config;
        let old_price = fee_config.kots_per_sol_price;
        
        fee_config.kots_per_sol_price = kots_per_sol_price;
        fee_config.price_last_updated = Clock::get()?.unix_timestamp;
        fee_config.last_updated = Clock::get()?.unix_timestamp;
        
        msg!("Spot price updated by authority: {}", ctx.accounts.authority.key());
        msg!("Old price: {} KOTS per SOL", old_price / PRICE_PRECISION);
        msg!("New price: {} KOTS per SOL", kots_per_sol_price / PRICE_PRECISION);
        msg!("Price last updated: {}", fee_config.price_last_updated);
        
        Ok(())
    }

    /// Set the anti-whale activation flag (added 2026-07-22).
    ///
    /// The 3%-of-supply cap is early-stage protection, active below a $2M
    /// circulating market cap and relaxed above it. Market cap is a USD figure no
    /// on-chain feed supplies trustlessly (KOTS/USD is on no public oracle — see
    /// KOTS-TOKENOMICS §4e), so the OFF-CHAIN keeper — the same one that computes
    /// pool-reserve pricing for update_spot_price — watches circulating mcap and
    /// calls this to flip the flag when the threshold is crossed. The hook only
    /// reads the flag; no oracle enters the transfer instruction.
    ///
    /// Trust model: identical to update_spot_price — a keeper-supplied input behind
    /// an authority (the multisig) gate. Set `active = false` at/above $2M to relax
    /// the cap; set `true` to reinstate it (e.g., if mcap falls back below $2M).
    ///
    /// **NOTE:** Only the program authority can call this.
    pub fn set_anti_whale_active(
        ctx: Context<SetAntiWhaleActive>,
        active: bool,
    ) -> Result<()> {
        require!(
            ctx.accounts.authority.key() == ctx.accounts.fee_config.authority,
            ErrorCode::Unauthorized
        );

        let fee_config = &mut ctx.accounts.fee_config;
        let old = fee_config.anti_whale_active;
        fee_config.anti_whale_active = active;
        fee_config.last_updated = Clock::get()?.unix_timestamp;

        msg!(
            "Anti-whale flag set by authority {}: {} -> {}",
            ctx.accounts.authority.key(),
            old,
            active
        );
        msg!(
            "3%-of-supply cap is now {} (relaxes at $2M circulating mcap; keeper-driven)",
            if active { "ACTIVE" } else { "RELAXED" }
        );

        Ok(())
    }

    /// Initialize sell fee debt PDA for a user (permissionless)
    /// 
    /// **PURPOSE:** Create per-user debt tracking account for DEX sell fee collection
    /// Anyone can initialize a debt PDA for any user (permissionless)
    /// 
    /// **ARCHITECTURE:** Per-user PDA keyed by owner address
    /// PDA derivation: ["sell_fee_debt", owner_pubkey]
    #[cfg(feature = "sol-fees")]
    pub fn initialize_sell_fee_debt(
        ctx: Context<InitializeSellFeeDebt>,
        owner: Pubkey,
    ) -> Result<()> {
        let debt = &mut ctx.accounts.sell_fee_debt;
        debt.owner = owner;
        debt.debt_amount = 0;
        debt.last_updated_slot = Clock::get()?.slot;
        debt.total_sells = 0;
        
        msg!("Sell fee debt PDA initialized for owner: {}", owner);
        msg!("Initial debt: 0 KOTS");
        
        Ok(())
    }

    /// Collect sell fee debt - PART 1 of 2 (BPF stack split)
    /// 
    /// **PURPOSE:** Process accumulated 0.5% KOTS debt from DEX sells
    /// Split into 2 instructions to stay under 4096-byte BPF stack limit
    /// 
    /// **PART 1:** Collect debt + pay keeper + distribute to Jackpot & Incoming (3 transfers)
    /// **PART 2:** call collect_sell_fee_debt_part2 to complete remaining distributions
    /// 
    /// **PERMISSIONLESS:** Anyone can call this (keeper gets 1% of debt + PDA rent ~0.002 SOL)
    #[inline(never)]
    #[cfg(feature = "sol-fees")]
    pub fn collect_sell_fee_debt_part1(ctx: Context<CollectSellFeeDebtPart1>) -> Result<()> {
        let debt_amount = ctx.accounts.sell_fee_debt.debt_amount;
        let owner = ctx.accounts.sell_fee_debt.owner;

        require!(debt_amount > 0, ErrorCode::NoDebtToCollect);

        // DESIGN B (2026-07-22): this step no longer TRANSFERS. It used to move the
        // keeper reward + jackpot + incoming out of the PDA-owned fee vault with
        // `transfer_checked`. On this hooked mint that forces Token-2022 to CPI back
        // into the hook while the hook program is already on the call stack, which
        // Solana forbids (indirect reentrancy) — exactly the defect that made the
        // KOTS collector unable to execute. It could never have run.
        //
        // What remains is the authoritative SPLIT PLAN: the program validates the
        // debt and publishes the exact amounts, using the same successive-subtraction
        // arithmetic as before, so the keeper moves precisely what the contract says.
        // The keeper then signs those transfers out of the keeper-owned vault, where
        // the depth is client -> Token-2022 -> hook and nothing re-enters.
        // Part 2 settles (closes) the debt record.
        let keeper_reward = debt_amount / 100;
        let distributable = debt_amount.checked_sub(keeper_reward)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        // Each leg is its share of the DISTRIBUTABLE POT, not of a shrinking
        // remainder.
        //
        // (2026-07-22 BUGFIX: the original code subtracted each leg from a running
        // `r` and then took the NEXT leg's bps of that reduced figure. Only the
        // first leg received its nominal share; every later leg was progressively
        // shortchanged and treasury silently absorbed the difference — the
        // Captain's 7% incoming share paid 3.5%, admin 3%→1.4%, dev 3%→1.35%.)
        //
        // NOTE the leg SET is unchanged: this path has never had a burn leg, so
        // treasury's remainder here is 37% (36% + the 1% burn redirect), matching
        // the documented SOL-side convention. Changing the leg set is a tokenomics
        // decision, not a bug fix, so it is deliberately left alone.
        // u128 intermediate — same class of bug as the anti-whale MathOverflow
        // (supply-scale amounts × BPS overflow u64 before the divide).
        let jackpot = (distributable as u128)
            .checked_mul(JACKPOT_BPS_KOTS as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let incoming = (distributable as u128)
            .checked_mul(INCOMING_BPS_KOTS as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let admin = (distributable as u128)
            .checked_mul(ADMIN_BPS_KOTS as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let dev = (distributable as u128)
            .checked_mul(DEV_BPS_KOTS as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        // Treasury takes the remainder so rounding dust is never stranded.
        let treasury = distributable
            .checked_sub(jackpot)
            .and_then(|x| x.checked_sub(incoming))
            .and_then(|x| x.checked_sub(admin))
            .and_then(|x| x.checked_sub(dev))
            .ok_or(ProgramError::ArithmeticOverflow)?;

        msg!("=== Sell Fee Debt - split plan (keeper executes) ===");
        msg!("Owner: {}", owner);
        msg!("Debt: {} KOTS", debt_amount);
        let fc = &ctx.accounts.fee_config;
        msg!("  keeper   {} -> {}", keeper_reward, ctx.accounts.keeper.key());
        msg!("  jackpot  {} -> {}", jackpot, fc.jackpot_wallet);
        msg!("  incoming {} -> {}", incoming, fc.incoming_wallet);
        msg!("  admin    {} -> {}", admin, fc.admin_wallet);
        msg!("  dev      {} -> {}", dev, fc.dev_wallet);
        msg!("  treasury {} -> {} (remainder)", treasury, fc.treasury_wallet);
        msg!("Keeper: send these from the keeper vault, then call part2 to settle.");

        Ok(())
    }

    /// Collect sell fee debt - PART 2 of 2 (BPF stack split)
    /// 
    /// **PURPOSE:** Complete distribution of collected debt
    /// Must be called after collect_sell_fee_debt_part1
    /// 
    /// **PART 2:** Distribute remaining fees to Admin, Dev, Treasury + Close PDA
    /// 
    /// **DISTRIBUTION LOGIC:** Part1 already sent Keeper+Jackpot+Incoming.
    /// This distributes whatever remains in fee_vault proportionally to Admin(3%)+Dev(3%)+Treasury(remainder)
    /// 
    /// **PERMISSIONLESS:** Anyone can call this
    #[inline(never)]
    #[cfg(feature = "sol-fees")]
    pub fn collect_sell_fee_debt_part2(ctx: Context<CollectSellFeeDebtPart2>) -> Result<()> {
        let debt_amount = ctx.accounts.sell_fee_debt.debt_amount;
        let owner = ctx.accounts.sell_fee_debt.owner;

        require!(debt_amount > 0, ErrorCode::NoDebtToCollect);

        // DESIGN B (2026-07-22): the admin/dev/treasury transfers that used to live
        // here are gone for the same reason as part1 — a `transfer_checked` of this
        // hooked mint initiated from inside the hook program re-enters the hook,
        // which Solana forbids. The keeper performs the split (see part1's plan)
        // from the keeper-owned vault.
        //
        // This instruction's remaining job is the one thing only the program can do:
        // SETTLE the debt by closing the sell_fee_debt PDA (via `close = keeper` on
        // the account struct), returning its rent to the keeper.
        msg!("=== Sell Fee Debt - settle ===");
        msg!("Owner: {}", owner);
        msg!("Debt cleared: {} KOTS (distribution performed keeper-side)", debt_amount);
        msg!("sell_fee_debt PDA closed; rent returned to keeper.");

        Ok(())
    }

    /// Harvest withheld trading fees into the distribution vault.
    ///
    /// A non-whitelisted transfer leaves its 3% fee WITHHELD on the recipient's
    /// token account ("Fees stay withheld for distribution" in the execute path).
    /// This sweeps that withheld KOTS out of a batch of such accounts into the
    /// fee-distribution vault, where fee-management's `execute_fee_distribution`
    /// parts then split it to the seven canonical wallets. It is the UPSTREAM half
    /// of the trading-fee pipeline (harvest -> distribute).
    ///
    /// SAFE FROM THE DESIGN-B REENTRANCY: this is a Transfer-Fee-extension
    /// withdrawal, NOT a `transfer_checked`, so it does NOT re-enter the transfer
    /// hook — exactly like `process_refunds_bulk`, which issues the same CPI from
    /// inside this program and runs. (The reentrancy that DESIGN B works around
    /// applies only to actual KOTS `transfer_checked`; it does not apply here.)
    ///
    /// AUTHORITY-ONLY: only `fee_config.authority` may call, and it names the
    /// destination `vault`, so a caller cannot redirect the swept fees to an
    /// account it controls. (A permissionless keeper variant would instead need
    /// `vault` pinned to a canonical PDA; the authority gate is the stronger,
    /// simpler control and is the default here.)
    ///
    /// The source accounts (the withheld-holding token accounts) are passed as
    /// `remaining_accounts`; an off-chain keeper/ops script discovers and batches
    /// them. Every source is validated to be a Token-2022 account for THIS mint,
    /// and the vault is never swept into itself.
    #[inline(never)]
    pub fn harvest_withheld_to_vault<'info>(
        ctx: Context<'_, '_, '_, 'info, HarvestWithheldToVault<'info>>,
    ) -> Result<()> {
        require!(
            ctx.accounts.authority.key() == ctx.accounts.fee_config.authority,
            ErrorCode::Unauthorized
        );

        let mint_key = ctx.accounts.mint.key();
        let vault_key = ctx.accounts.vault.key();
        let fee_authority_key = ctx.accounts.fee_authority.key();
        let bump = ctx.accounts.fee_authority.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[FEE_AUTHORITY_SEED, &[bump]]];

        let sources = ctx.remaining_accounts;
        require!(!sources.is_empty(), ErrorCode::NoFeesToDistribute);

        // Validate every source is a Token-2022 account for THIS mint (Token-2022
        // enforces the mint on withdraw too, but fail early and clearly), and
        // never sweep the destination vault into itself.
        let mut source_keys: Vec<Pubkey> = Vec::with_capacity(sources.len());
        for acct in sources.iter() {
            require!(acct.key() != vault_key, ErrorCode::InvalidRefundQueue);
            let data = acct.try_borrow_data()?;
            let token_acct = spl_token_2022::extension::StateWithExtensions::<spl_token_2022::state::Account>::unpack(&data)
                .map_err(|_| error!(ErrorCode::InvalidRefundQueue))?;
            require!(token_acct.base.mint == mint_key, ErrorCode::InvalidRefundQueue);
            drop(data);
            source_keys.push(acct.key());
        }
        let source_refs: Vec<&Pubkey> = source_keys.iter().collect();

        let withdraw_ix = spl_token_2022::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_accounts(
            &spl_token_2022::id(),
            &mint_key,
            &vault_key,          // destination = the distribution vault
            &fee_authority_key,
            &[],
            &source_refs,        // sources = the withheld-holding accounts
        )?;

        // invoke_signed infos: mint, vault, fee_authority, token_program, + sources.
        let mut infos: Vec<AccountInfo<'info>> = Vec::with_capacity(4 + sources.len());
        infos.push(ctx.accounts.mint.to_account_info());
        infos.push(ctx.accounts.vault.to_account_info());
        infos.push(ctx.accounts.fee_authority.to_account_info());
        infos.push(ctx.accounts.token_program.to_account_info());
        for acct in sources.iter() {
            infos.push(acct.clone());
        }

        anchor_lang::solana_program::program::invoke_signed(&withdraw_ix, &infos, signer_seeds)?;

        msg!("\u{2713} Harvested withheld KOTS from {} account(s) into distribution vault {}",
             source_keys.len(), vault_key);
        Ok(())
    }

    /// Close Fee Vault
    /// 
    /// **PURPOSE:** Close and reclaim rent from the fee vault
    /// This is needed when the fee vault is in a broken state and needs to be recreated
    pub fn close_fee_vault(ctx: Context<CloseFeeVault>) -> Result<()> {
        msg!("Closing Fee Vault");
        msg!("Fee Vault: {}", ctx.accounts.fee_vault.key());
        
        let lamports = ctx.accounts.fee_vault.to_account_info().lamports();
        msg!("Returning {} lamports to authority", lamports);
        
        // Use Token-2022 CPI to close the account (fee_authority must sign via PDA)
        let fee_authority_bump = ctx.accounts.fee_authority.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[FEE_AUTHORITY_SEED, &[fee_authority_bump]]];
        
        let cpi_accounts = token_interface::CloseAccount {
            account: ctx.accounts.fee_vault.to_account_info(),
            destination: ctx.accounts.authority.to_account_info(),
            authority: ctx.accounts.fee_authority.to_account_info(),
        };
        
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds);
        
        token_interface::close_account(cpi_ctx)?;
        
        msg!("✅ Fee Vault closed successfully");
        Ok(())
    }

    /// Close Extra Account Meta List
    /// 
    /// **PURPOSE:** Close and reclaim rent from the old Extra Account Meta List
    /// This is needed when updating the account structure (e.g., adding Token Program)
    pub fn close_extra_account_meta_list(ctx: Context<CloseExtraAccountMetaList>) -> Result<()> {
        msg!("Closing Extra Account Meta List");
        msg!("PDA: {}", ctx.accounts.extra_account_meta_list.key());
        
        let lamports = ctx.accounts.extra_account_meta_list.lamports();
        msg!("Returning {} lamports to authority", lamports);
        
        // Transfer all lamports to authority (close the account)
        **ctx.accounts.extra_account_meta_list.try_borrow_mut_lamports()? = 0;
        **ctx.accounts.authority.try_borrow_mut_lamports()? += lamports;
        
        msg!("✅ Extra Account Meta List closed successfully");
        Ok(())
    }

    /// Initialize pending refund queue
    pub fn initialize_refund_queue(ctx: Context<InitializeRefundQueue>) -> Result<()> {
        let queue_account = &ctx.accounts.refund_queue;
        
        // SECURITY: Verify account is owned by this program
        require!(
            queue_account.owner == &crate::ID,
            ErrorCode::AccountOwnedByWrongProgram
        );
        
        // Validate size matches expected
        require!(
            queue_account.data_len() == REFUND_QUEUE_SIZE_USIZE,
            ErrorCode::InvalidRefundQueueSize
        );
        
        // CRITICAL: Verify rent exemption
        let rent = Rent::get()?;
        let required_lamports = rent.minimum_balance(REFUND_QUEUE_SIZE_USIZE);
        require!(
            queue_account.lamports() >= required_lamports,
            ErrorCode::InsufficientRentFunding
        );
        
        msg!("Rent check passed: {} >= {} lamports", queue_account.lamports(), required_lamports);
        msg!("Initializing refund queue: {} bytes", REFUND_QUEUE_SIZE);
        
        // Initialize zero-copy account (already owned by program from creation)
        let mut data = queue_account.try_borrow_mut_data()?;
        
        // Zero the entire data buffer
        data.fill(0);
        
        // Write Anchor discriminator (first 8 bytes)
        // For zero-copy accounts, use the DISCRIMINATOR constant
        data[0..8].copy_from_slice(&PendingRefundQueue::DISCRIMINATOR);
        
        // Initialize the struct fields using bytemuck
        let queue_data = &mut data[8..];
        let queue: &mut PendingRefundQueue = bytemuck::from_bytes_mut(queue_data);
        
        queue.authority = ctx.accounts.authority.key();
        queue.head = 0;
        queue.tail = 0;
        queue.count = 0;
        queue.padding = 0;
        queue.last_updated = Clock::get()?.unix_timestamp;
        // refunds array already zeroed by fill(0) above
        
        // Store refund_queue address in fee_config
        ctx.accounts.fee_config.refund_queue = queue_account.key();

        msg!("✅ Pending refund queue initialized (capacity: {} refunds, {} bytes, zero-copy)", REFUND_QUEUE_CAPACITY, REFUND_QUEUE_SIZE);
        msg!("   Refund queue address: {}", queue_account.key());
        Ok(())
    }

    /// Initialize Extra Account Meta List for Transfer Hook
    /// 
    /// **CRITICAL:** This defines ALL extra accounts that Token-2022 will pass to transfer_hook()
    /// These accounts enable direct SOL fee collection during the hook execution.
    /// 
    /// **NOTE:** The owner account is NOT listed here because it's one of the 4 base accounts
    /// automatically passed by Token-2022 (source, mint, destination, owner). We'll use the
    /// owner as the fee payer for SOL fees since they're already a signer!
    /// 
    /// **IMPORTANT:** Manually creates PDA without Anchor discriminator for Token-2022 TLV compatibility
    /// **BPF STACK:** Uses helper function to avoid stack overflow in shared dispatch frame
    pub fn initialize_extra_account_meta_list(
        ctx: Context<InitializeExtraAccountMeta>,
    ) -> Result<()> {
        // Delegate to helper to isolate stack usage
        initialize_extra_account_meta_list_impl(&ctx.accounts)
    }

    // ========================================================================
    // CONFIGURATION INSTRUCTIONS
    // ========================================================================

    /// Add wallet to whitelist (governance function)
    pub fn add_to_whitelist<'info>(
        ctx: Context<'_, '_, '_, 'info, UpdateWhitelist<'info>>,
        wallet: Pubkey,
    ) -> Result<()> {
        // ACT-03 (F6): whitelist edits require owner-registry quorum, not the
        // single stored authority. Checked before the &mut borrow, mirroring
        // security_governance::add_to_blacklist (primary signer + owner
        // co-signers in remaining_accounts must reach the M-of-N threshold).
        let mut infos: Vec<AccountInfo> = ctx.remaining_accounts.to_vec();
        infos.push(ctx.accounts.authority.to_account_info());
        require!(
            ctx.accounts.owner_registry.quorum_met(&infos),
            security_governance::owner_registry::OwnerRegistryError::QuorumNotMet
        );
        let registry = &mut ctx.accounts.whitelist_registry;
        
        require!(
            registry.wallets.len() < MAX_WHITELISTED_WALLETS,
            ErrorCode::TooManyWallets
        );
        
        require!(
            !registry.wallets.contains(&wallet),
            ErrorCode::AlreadyWhitelisted
        );

        registry.wallets.push(wallet);
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("Added wallet to whitelist: {}", wallet);
        Ok(())
    }

    /// Remove wallet from whitelist (governance function)
    pub fn remove_from_whitelist<'info>(
        ctx: Context<'_, '_, '_, 'info, UpdateWhitelist<'info>>,
        wallet: Pubkey,
    ) -> Result<()> {
        // ACT-03 (F6): quorum-gated, as add_to_whitelist.
        let mut infos: Vec<AccountInfo> = ctx.remaining_accounts.to_vec();
        infos.push(ctx.accounts.authority.to_account_info());
        require!(
            ctx.accounts.owner_registry.quorum_met(&infos),
            security_governance::owner_registry::OwnerRegistryError::QuorumNotMet
        );
        let registry = &mut ctx.accounts.whitelist_registry;
        
        registry.wallets.retain(|&w| w != wallet);
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("Removed wallet from whitelist: {}", wallet);
        Ok(())
    }

    /// Register a DEX pool for buy/sell detection
    pub fn register_pool(
        ctx: Context<UpdatePoolRegistry>,
        pool_address: Pubkey,
        pool_type: PoolType,
    ) -> Result<()> {
        let registry = &mut ctx.accounts.pool_registry;
        
        require!(
            registry.pools.len() < MAX_POOL_ADDRESSES,
            ErrorCode::TooManyPools
        );

        let pool_info = PoolInfo {
            address: pool_address,
            pool_type,
            registered_at: Clock::get()?.unix_timestamp,
        };

        registry.pools.push(pool_info);
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("Registered pool: {} ({:?})", pool_address, pool_type);
        Ok(())
    }

    /// Register the protocol-owned liquidity pool
    /// 
    /// **PURPOSE:** Set YOUR Raydium CLMM pool as the protocol pool.
    /// When buys happen in this pool, we skip transfer hook fees (LP fees handle it).
    /// 
    /// **USAGE:** Call once after creating your POL on Raydium CLMM.
    pub fn register_protocol_pool(
        ctx: Context<UpdatePoolRegistry>,
        pool_address: Pubkey,
    ) -> Result<()> {
        let registry = &mut ctx.accounts.pool_registry;
        
        registry.protocol_pool_address = Some(pool_address);
        registry.last_updated = Clock::get()?.unix_timestamp;

        msg!("✓ Protocol pool registered: {}", pool_address);
        msg!("  → Buys in this pool will skip transfer hook fees");
        msg!("  → LP fees will be collected and distributed automatically");
        Ok(())
    }

    // ========================================================================
    // MAIN TRANSFER HOOK (VAULT-BASED FEE COLLECTION)
    // ========================================================================

    /// Main transfer hook - executes on every KOTS token transfer
    /// 
    /// **TRANSFER FEE EXTENSION + SMART REFUND QUEUE:**
    /// - Transfer Fee Extension withholds 1% KOTS automatically from ALL transfers
    /// - Whitelisted transfers: Queue refund in PDA (recipient gets refunded later)
    /// - Non-whitelisted transfers: Fees stay withheld for distribution
    /// - There is NO in-hook drain, and there cannot be one (O35, 2026-07-29): a
    ///   queued refund names a token account from an earlier transfer, which the
    ///   hook is neither passed nor able to write. The queue drains ONLY via
    ///   `process_refunds_bulk`, off-chain.
    /// - A FULL queue REVERTS the transfer (`RefundQueueFull`) rather than
    ///   silently skipping the refund. If the keeper stops, exempt transfers stop
    ///   after REFUND_QUEUE_CAPACITY of them. Keeper liveness is a requirement.
    /// - Manual bulk processing available via `process_refunds_bulk` instruction
    pub fn transfer_hook(ctx: Context<TransferHook>, amount: u64) -> Result<()> {
        msg!("=== KOTS Transfer Hook Triggered ===");
        msg!("Transfer amount: {} KOTS", amount);

        let source_owner = ctx.accounts.source_token.owner;
        let dest_owner = ctx.accounts.destination_token.owner;
        let slot = Clock::get()?.slot;

        // Step 1: Check if whitelisted - if so, queue refund
        let is_sender_exempt = is_whitelisted(&ctx.accounts.whitelist_registry, &source_owner);
        let is_receiver_exempt = is_whitelisted(&ctx.accounts.whitelist_registry, &dest_owner);

        // ═══ Anti-whale gate — BEFORE any fee logic ═══
        // destination_token.amount is POST-transfer here (Token-2022 moves
        // balances, then invokes the hook), so erroring reverts the whole
        // transfer. Exempt: whitelisted platform wallets (treasury, dev,
        // jackpot, etc. hold >>3% by design) and registered DEX pool vaults
        // (a pool concentrates supply legitimately — unexempted, LISTING
        // itself would be the first violation).
        //
        // GATED on fee_config.anti_whale_active (2026-07-22): the cap is
        // early-stage protection, ACTIVE below a $2M circulating mcap and RELAXED
        // above it. The keeper flips the flag off-chain when mcap crosses $2M (a
        // USD figure no on-chain feed supplies trustlessly). When the flag is
        // false this whole block short-circuits — no cap computed, no revert.
        // Computed ONCE (not inline in the gate below) because the fee
        // classification emitted at the end of this instruction needs it too, and
        // the gate's short-circuit would otherwise skip evaluating it.
        let is_pool_dest = is_pool_destination(
            &ctx.accounts.pool_registry,
            &dest_owner,
            &ctx.accounts.destination_token.key(),
        );
        // Same membership test on the source (owner OR token account).
        // P2P = neither side is a registered vault. AMM buy is pool→user
        // (source IS a vault) and must stay class=BUY so V3 97% holds.
        let is_pool_src = is_pool_destination(
            &ctx.accounts.pool_registry,
            &source_owner,
            &ctx.accounts.source_token.key(),
        );
        let is_p2p = !is_pool_src
            && !is_pool_dest
            && !is_sender_exempt
            && !is_receiver_exempt;

        if ctx.accounts.fee_config.anti_whale_active
            && !is_receiver_exempt
            && !is_pool_dest
        {
            let supply = ctx.accounts.mint.supply as u128;
            let cap = supply
                .checked_mul(MAX_HOLD_BPS as u128)
                .and_then(|x| x.checked_div(10_000))
                .ok_or(ErrorCode::MathOverflow)?;
            let cap = u64::try_from(cap).map_err(|_| ErrorCode::MathOverflow)?;
            require!(
                ctx.accounts.destination_token.amount <= cap,
                ErrorCode::WhaleLimitExceeded
            );
        }

        if is_sender_exempt || is_receiver_exempt || is_p2p {
            if is_p2p {
                msg!("✓ P2P transfer detected - refunding the full withheld fee (H18 / V9)");
            } else {
                msg!("✓ Exempt transfer detected - refunding the full withheld fee");
            }

            // The fee the Transfer Fee Extension withheld, at the CONFIGURED rate.
            // (2026-07-22 BUGFIX: this was hardcoded `amount / 100` = 1%. Under the
            // A' model the extension rate is the SELL rate (3%), so a hardcoded 1%
            // would refund only a third of what was taken and leave whitelisted
            // wallets paying 2% instead of 0%. fee_config.transfer_fee_bps MUST be
            // kept equal to the mint's actual extension rate by the ceremony.)
            // ROUNDS UP, matching what Token-2022 actually withheld. This was
            // floor until 2026-07-29 and under-refunded by one raw unit on every
            // transfer with a remainder — see withheld_fee_for.
            let refund_amount =
                withheld_fee_for(amount, ctx.accounts.fee_config.transfer_fee_bps);
            
            // Minimum refund threshold: Only refund if 1% fee >= 10x gas cost
            // Gas cost estimate: ~5,000 lamports per transaction
            // Minimum refund: 50,000 lamports (0.00005 SOL worth of KOTS)
            const MIN_REFUND_THRESHOLD: u64 = 50_000;
            
            if refund_amount >= MIN_REFUND_THRESHOLD {
                let mut queue = ctx.accounts.refund_queue.load_mut()?;
                
                // ── O35 RULING (2026-07-29): a full queue REVERTS. ──────────
                //
                // This branch used to call `withdraw_withheld_tokens_from_accounts`
                // via `invoke_signed` from inside the hook's execute path. That is
                // not merely untested, it is IMPOSSIBLE: Token-2022 hands a hook
                // every account as READ-ONLY and NON-SIGNER, and the runtime answers
                // "Cross-program invocation with unauthorized signer or writable
                // account" (measured 2026-07-29 at capacity 1). The same constraint
                // the SOL leg died of on 2026-07-22 — known in one place, violated
                // here.
                //
                // WHY REVERT RATHER THAN SKIP. Skipping would let the transfer
                // succeed while the wallet keeps paying a fee it was promised back:
                // a silent under-rebate, which Gate A B8 forbids and which needs
                // reconciliation machinery that does not exist. Reverting is a
                // LIVENESS failure — the transfer does not happen and nobody is
                // short-changed — and it says so, with an error naming the cause.
                //
                // WHAT CLEARS IT. Only the off-chain keeper, via
                // `process_refunds_bulk`. There is no in-band drain and there cannot
                // be one: a queued refund names a recipient token account from an
                // EARLIER transfer, and the hook is passed 12 accounts, none of which
                // is that account. Unreachable and unwritable, both.
                //
                // OPERATIONAL CONSEQUENCE, stated plainly: if the keeper stops,
                // exempt transfers stop after REFUND_QUEUE_CAPACITY of them —
                // treasury and LP seeder included. Keeper liveness is a launch
                // requirement, not a nice-to-have, and Gate A B9 must measure the
                // arrival rate against keeper latency before the capacity can be
                // called sized rather than guessed.
                if queue.is_full() {
                    msg!(
                        "  \u{2717} Refund queue FULL ({} entries). Reverting: the refund cannot \
                         be paid in-band and must not be silently skipped. Run \
                         process_refunds_bulk.",
                        REFUND_QUEUE_CAPACITY
                    );
                    return err!(ErrorCode::RefundQueueFull);
                } else {
                    // Enqueue refund in circular buffer (deferred processing).
                    // push_back re-checks is_full() and cannot overrun the array.
                    queue.push_back(dest_owner, refund_amount, slot);
                    queue.last_updated = Clock::get()?.unix_timestamp;
                    
                    msg!("  → Refund queued: {} KOTS to {} (queue size: {}/{})", refund_amount, dest_owner, queue.count, REFUND_QUEUE_CAPACITY);
                }
                
                msg!("  → 0% net fee (refund guaranteed)");
            } else {
                msg!("  ⚠️  Refund amount {} KOTS too small (<0.00005 SOL worth)", refund_amount);
                msg!("  → Skipping refund to save gas fees (would cost more than refund value)");
                msg!("  → 1% fee goes to distribution pool");
            }
            
            msg!("  → 0% SOL fees (platform wallet exemption)");
        } else {
            msg!("✓ Non-exempt transfer - 1% KOTS fee withheld for distribution");
        }

        // ── O35 (2026-07-29): THE OPPORTUNISTIC DRAIN IS GONE. ──────────────
        //
        // What was here: a block that read the queue length, computed a batch size
        // from three invented CU constants (ESTIMATED_CU_USED 100_000,
        // CU_PER_REFUND 10_000, MIN_CU_BUFFER 50_000), logged four lines, and then
        // called msg!("TODO: Implement batch withdraw_withheld_tokens_from_accounts").
        // It processed nothing, on EVERY exempt transfer, inside the instruction
        // that was running out of both CU and heap.
        //
        // Deleted rather than implemented, because it CANNOT be implemented here.
        // Draining a queued refund means writing to the recipient's token account
        // from an earlier transfer. The hook receives 12 accounts — 4 base, the
        // meta list, and 7 extras — and that account is not among them; nor would
        // it be writable if it were, since Token-2022 passes hook accounts
        // read-only and non-signer.
        //
        // All draining is off-chain, via `process_refunds_bulk`. The hook's job is
        // to RECORD the debt, and recording is all it can do.
        //
        // The IDL doc on `transfer_hook` advertised this as a live feature —
        // "processes 2-10 pending refunds when CU budget allows" — until
        // 2026-07-29. It never did.

        // ── DEX hybrid-fee branch REMOVED 2026-07-22 (fee model: option D) ──
        //
        // What used to be here: instruction introspection detected a swap through
        // Raydium / Orca / Meteora / Jupiter and then applied a "hybrid" fee —
        // buys 0.5% KOTS + 0.5% SOL, sells 1.5% KOTS + 1.5% SOL, with the extra
        // KOTS beyond the extension's automatic 1% recorded as per-user "debt".
        //
        // NONE of it could work, for two independent reasons:
        //
        //  1. The SOL leg called `charge_sol_fee`, which ends in a System transfer
        //     with `from = owner`. A System transfer requires `from` to SIGN, and
        //     Token-2022 converts every account it passes to a hook into a
        //     READ-ONLY, NON-SIGNER account. So the CPI fails — and because a hook
        //     error reverts the whole transaction, EVERY swap routed through a
        //     recognised DEX would have failed. KOTS would have been untradeable
        //     on Raydium, Orca, Meteora and Jupiter. The lab never caught it
        //     because no test transaction contains a DEX program instruction.
        //
        //  2. The extra sell fee was written to a per-user `sell_fee_debt` PDA
        //     only IF the user had already created it. A hook cannot create
        //     accounts (no payer, no signer), so it could never force that PDA
        //     into existence, and the code simply logged "Debt NOT recorded".
        //     The sell fee was therefore opt-in from the payer's side — i.e.
        //     uncollectable. (This is what T4 characterised on-chain.)
        //
        // Option D: ONE uniform KOTS fee, no SOL leg, no buy/sell asymmetry. The
        // fee is the Transfer Fee Extension's automatic withholding — the only
        // charging mechanism a hook can actually rely on — collected and split by
        // the Design B collector. Whitelisted parties are made whole through the
        // refund queue. Every one of those pieces is proven green in the matrix.
        //
        // The hook's remaining jobs on the execute path are: enforce the
        // anti-whale cap, and queue refunds for exempt transfers. It charges
        // nothing itself and can no longer revert a legitimate DEX trade.

        // ── Fee classification for the off-chain rebate keeper (A' model) ──
        //
        // The extension withholds the SELL rate (fee_config.transfer_fee_bps) on
        // EVERY transfer; the keeper then rebates the difference down to each
        // class's net rate. Emitting the class here means the indexer never has to
        // re-derive pool membership or whitelist state from chain history — it
        // reads one greppable line and knows exactly what to pay:
        //
        //   EXEMPT : rebate 100% of the withheld fee  -> net 0
        //            (whitelist OR P2P: neither side is a registered vault)
        //   SELL   : rebate nothing                   -> net dex_sell_fee_bps
        //   BUY    : pool → user (AMM / DEX buy)      -> net dex_buy_fee_bps
        //
        // Keep these strings STABLE — the keeper parses them.
        let fee_class = if is_sender_exempt || is_receiver_exempt || is_p2p {
            "EXEMPT"
        } else if is_pool_dest {
            "SELL"
        } else {
            "BUY"
        };
        msg!(
            "KOTS_FEE_CLASS class={} amount={} withheld_bps={} net_bps={} owner={} dest={}",
            fee_class,
            amount,
            ctx.accounts.fee_config.transfer_fee_bps,
            match fee_class {
                "EXEMPT" => 0u16,
                "SELL" => ctx.accounts.fee_config.dex_sell_fee_bps,
                _ => ctx.accounts.fee_config.dex_buy_fee_bps,
            },
            source_owner,
            dest_owner
        );

        msg!("=== Transfer Hook Complete ===");
        Ok(())
    }

    // ========================================================================
    // REFUND PROCESSING INSTRUCTIONS
    // ========================================================================

    /// Process pending refunds in bulk (manual backlog clearing)
    /// 
    /// **PURPOSE:** Withdraw withheld fees from exempt transfer recipients and clear queue
    /// **USAGE:** Call periodically to clear refund backlog (max 100 refunds per call)
    /// **PERMISSIONLESS:** Anyone can call this to process the queue
    /// 
    /// **PROCESS:**
    /// 1. Read up to `batch_size` pending refunds from queue (FIFO order)
    /// 2. Withdraw withheld tokens from recipient accounts
    /// 3. Remove processed refunds from queue
    #[inline(never)] // BPF stack optimization: Force separate stack frame (builds Vec + CPI)
    pub fn process_refunds_bulk<'info>(
        ctx: Context<'_, '_, '_, 'info, ProcessRefundsBulk<'info>>,
        batch_size: u32,
    ) -> Result<()> {
        msg!("=== Processing Refund Queue (Bulk) ===");
        
        let queue = ctx.accounts.refund_queue.load_mut()?;
        
        // Validate batch size
        require!(batch_size > 0 && batch_size <= 100, ErrorCode::InvalidBatchSize);
        
        // Check if queue is empty
        if queue.count == 0 {
            msg!("Refund queue is empty - nothing to process");
            return Ok(());
        }
        
        // Process up to batch_size refunds
        let refunds_to_process = std::cmp::min(batch_size as usize, queue.count as usize);
        msg!("Processing {} refunds from queue (total pending: {})", refunds_to_process, queue.count);
        
        // Collect recipient OWNERS for withdrawal (FIFO order from circular
        // buffer). NOTE: the queue stores the destination OWNER (wallet), but a
        // withheld-fee withdrawal operates on the TOKEN ACCOUNT (ATA). The client
        // must therefore pass each recipient's ATA in ctx.remaining_accounts, in
        // this same FIFO order; we verify below that each passed ATA is actually
        // owned by the expected wallet before refunding to it.
        let recipient_owners: Vec<Pubkey> = queue.peek_recipients(refunds_to_process);

        if recipient_owners.is_empty() {
            msg!("No valid refunds to process");
            return Ok(());
        }

        // The client passes one ATA per refund in remaining_accounts, FIFO-aligned
        // with recipient_owners. Require an exact count so we never silently skip
        // or over-process.
        require!(
            ctx.remaining_accounts.len() == recipient_owners.len(),
            ErrorCode::InvalidRefundQueue
        );

        // Drop the mutable queue borrow before doing CPIs; we re-borrow to dequeue
        // only after all refunds succeed (so a mid-batch failure leaves the queue
        // intact for a retry rather than losing refunds).
        drop(queue);

        let mint_key = ctx.accounts.mint.key();
        let fee_authority_key = ctx.accounts.fee_authority.key();
        let bump = ctx.accounts.fee_authority.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[FEE_AUTHORITY_SEED, &[bump]]];

        // Per-recipient SELF-refund. withdraw_withheld_tokens_from_accounts moves
        // each source account's withheld tokens into ONE destination, so a true
        // per-account refund is destination == source == that recipient's own ATA,
        // one CPI each. (The prior code used a single destination for all
        // recipients AND never passed the ATAs' AccountInfos to invoke_signed —
        // it could never execute; the golden matrix surfaced it 2026-07-22.)
        for (i, ata_info) in ctx.remaining_accounts.iter().enumerate() {
            let expected_owner = recipient_owners[i];

            // Verify the passed account is a Token-2022 token account for THIS
            // mint whose owner is the queued recipient — otherwise a caller could
            // redirect a refund to an account they control. Recipient ATAs carry
            // the TransferFeeAmount extension (that is where withheld tokens sit),
            // so use StateWithExtensions, not the fixed-length Account::unpack.
            let ata_data = ata_info.try_borrow_data()?;
            let token_acct = spl_token_2022::extension::StateWithExtensions::<spl_token_2022::state::Account>::unpack(&ata_data)
                .map_err(|_| error!(ErrorCode::InvalidRefundQueue))?;
            require!(token_acct.base.mint == mint_key, ErrorCode::InvalidRefundQueue);
            require!(token_acct.base.owner == expected_owner, ErrorCode::InvalidRefundQueue);
            drop(ata_data); // release before the CPI reborrows the account

            let ata_key = ata_info.key();
            let withdraw_ix = spl_token_2022::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_accounts(
                &spl_token_2022::id(),
                &mint_key,
                &ata_key,       // destination = this recipient's own ATA
                &fee_authority_key,
                &[],
                &[&ata_key],    // source     = same ATA (self-refund)
            )?;

            anchor_lang::solana_program::program::invoke_signed(
                &withdraw_ix,
                &[
                    ctx.accounts.mint.to_account_info(),
                    ata_info.clone(), // the recipient ATA's AccountInfo — the piece the old code omitted
                    ctx.accounts.fee_authority.to_account_info(),
                    ctx.accounts.token_program.to_account_info(),
                ],
                signer_seeds,
            )?;
        }

        msg!("✓ Refunded {} accounts", recipient_owners.len());

        // Re-borrow to dequeue now that every refund landed.
        let mut queue = ctx.accounts.refund_queue.load_mut()?;

        // Dequeue processed refunds (FIFO: clear slots, advance head, decrement count).
        let removed = queue.pop_front(refunds_to_process);
        debug_assert_eq!(removed, refunds_to_process);
        
        queue.last_updated = Clock::get()?.unix_timestamp;
        
        msg!("✓ Removed {} refunds from queue", refunds_to_process);
        msg!("✓ Remaining queue size: {}", queue.count);
        msg!("=== Refund Processing Complete ===");
        
        Ok(())
    }

    // ========================================================================
    // FEE COLLECTION INSTRUCTIONS
    // ========================================================================

    /// Collect and distribute KOTS fees from the withheld-fee vault
    /// 
    /// **USAGE:** Admin calls this periodically to distribute accumulated KOTS fees.
    /// Withdraws from the Token-2022 withheld-fee vault and distributes to 6 wallets.
    /// 
    /// **DISTRIBUTION:**
    /// - 50% → Jackpot Pool
    /// - 34% → Incoming
    /// - 3% → Admin
    /// - 7% → Treasury
    /// - 3% → Dev Fund
    /// - 3% → Burn Address
    #[inline(never)] // BPF stack optimization: Force separate stack frame (function has CPI + distributions)
    /// Collect and distribute KOTS fees - PART 1 of 2 (BPF stack split)
    /// 
    /// **PURPOSE:** Withdraw accumulated 1% transfer fees and distribute
    /// Split into 2 instructions to stay under 4096-byte BPF stack limit
    /// 
    /// **PART 1:** Withdraw fees + distribute to Jackpot, Incoming, Admin (3 transfers)
    /// **PART 2:** call collect_and_distribute_kots_fees_part2 to complete remaining distributions
    /// 
    /// **PERMISSIONLESS:** Anyone can call with list of accounts with withheld fees
    pub fn collect_and_distribute_kots_fees_part1<'info>(
        ctx: Context<'_, '_, '_, 'info, CollectKotsFeesPart1<'info>>,
        accounts: Vec<Pubkey>,
    ) -> Result<()> {
        msg!("=== KOTS Fee Collection & Distribution - Part 1/2 ===");
        msg!("Withdrawing fees from {} accounts", accounts.len());
        
        let bump = ctx.accounts.fee_authority.bump;
        
        // Withdraw withheld fees to vault
        {
            let mint_key = ctx.accounts.mint.key();
            let destination_key = ctx.accounts.fee_vault.key();
            let fee_authority_key = ctx.accounts.fee_authority.key();
            let signer_seeds: &[&[&[u8]]] = &[&[FEE_AUTHORITY_SEED, &[bump]]];

            // The source accounts (whose withheld fees we harvest) must be passed
            // as remaining_accounts so their AccountInfos reach the CPI — the ix
            // metas reference them but invoke_signed needs the infos too. The
            // client passes exactly the `accounts` Vec here, in the same order.
            // (2026-07-22: this forwarding was missing — same bug that made
            // process_refunds_bulk unable to execute; Part1 had never run.)
            require!(
                ctx.remaining_accounts.len() == accounts.len(),
                ErrorCode::InvalidBatchSize
            );
            // Optional but cheap safety: the passed infos must match the pubkeys.
            for (info, key) in ctx.remaining_accounts.iter().zip(accounts.iter()) {
                require!(info.key() == *key, ErrorCode::InvalidBatchSize);
            }

            let withdraw_ix = spl_token_2022::extension::transfer_fee::instruction::withdraw_withheld_tokens_from_accounts(
                &spl_token_2022::id(),
                &mint_key,
                &destination_key,
                &fee_authority_key,
                &[],
                &accounts.iter().collect::<Vec<_>>(),
            )?;

            // Base infos + every source account info (the piece the old code omitted).
            let mut infos: Vec<AccountInfo<'info>> = Vec::with_capacity(4 + accounts.len());
            infos.push(ctx.accounts.mint.to_account_info());
            infos.push(ctx.accounts.fee_vault.to_account_info());
            infos.push(ctx.accounts.fee_authority.to_account_info());
            infos.push(ctx.accounts.token_program.to_account_info());
            for info in ctx.remaining_accounts.iter() {
                infos.push(info.clone());
            }

            anchor_lang::solana_program::program::invoke_signed(
                &withdraw_ix,
                &infos,
                signer_seeds,
            )?;
        }
        
        msg!("✓ Fees withdrawn to vault");

        // Reload vault to report the collected total.
        ctx.accounts.fee_vault.reload()?;
        let fee_vault_balance = ctx.accounts.fee_vault.amount;
        require!(fee_vault_balance > 0, ErrorCode::NoFeesToDistribute);

        // DESIGN B (2026-07-22): the distribution legs that used to live here have
        // been REMOVED. They performed `transfer_checked` on this hooked mint from
        // inside the hook program, which makes Token-2022 CPI back into the hook —
        // the hook would then appear twice on the call stack, and Solana forbids
        // that reentrancy ("you cannot call Token-2022 to move a token while that
        // same program is suspended waiting for your hook"). Those transfers could
        // never execute; the golden matrix proved it.
        //
        // The hook now performs ONLY fee-extension operations, which are not
        // transfers and therefore never invoke the hook. The 50/7/36/3/3/1 split is
        // performed by the keeper as ordinary client-signed transfers out of this
        // (keeper-owned) vault, where the depth is client → Token-2022 → hook and
        // no reentrancy occurs. Whitelisting the vault owner and the destination
        // wallets makes the 1% re-withheld on each distribution recoverable through
        // the refund queue (process_refunds_bulk).
        msg!("✓ Collected {} KOTS into the keeper vault", fee_vault_balance);
        msg!("→ Keeper now splits 50/7/36/3/3/1 client-side (see DESIGN B note)");

        Ok(())
    }

    /// Collect and distribute KOTS fees - PART 2 of 2 (BPF stack split)
    /// 
    /// **PURPOSE:** Complete distribution of collected fees
    /// Must be called after collect_and_distribute_kots_fees_part1
    /// 
    /// **PART 2:** Distribute remaining fees to Dev, Burn, Treasury (3 transfers)
    /// 
    /// **PERMISSIONLESS:** Anyone can call this
    #[inline(never)]
    pub fn collect_and_distribute_kots_fees_part2(
        _ctx: Context<CollectKotsFeesPart2>
    ) -> Result<()> {
        // DESIGN B (2026-07-22): SUPERSEDED — this instruction used to transfer the
        // vault balance to dev/burn/treasury with `transfer_checked`. On this hooked
        // mint that makes Token-2022 CPI back into the hook while the hook program is
        // already on the call stack, which Solana forbids (indirect reentrancy). It
        // could never execute. The distribution is now performed by the keeper as
        // ordinary client-signed transfers out of the keeper-owned vault filled by
        // collect_and_distribute_kots_fees_part1. Kept as an explicit, loud failure
        // so nothing silently depends on the old path.
        msg!("SUPERSEDED: distribution is keeper-side under DESIGN B; see part1 docs.");
        Err(ErrorCode::NoFeesToDistribute.into())
    }

    // ========================================================================
    // SOL FEE DISTRIBUTION INSTRUCTION
    // ========================================================================

    /// Distribute SOL fees to designated wallets
    /// 
    /// **USAGE:** Permissionless - anyone can call this
    /// Distributes accumulated SOL fees from the program's fee collection account
    /// to predefined wallet addresses. This ensures the program doesn't accumulate
    /// excessive SOL in the fee collection account.
    /// 
    /// **DISTRIBUTION:**
    /// Follows configurable percentages set in FeeConfig (same as KOTS distribution)
    /// - 50% → Jackpot Pool
    /// - 34% → Incoming
    /// - 7% → Treasury
    /// - 3% → Admin
    /// - 3% → Dev Fund
    /// - 3% → Burn Address
    #[cfg(feature = "sol-fees")]
    pub fn distribute_sol_fees(ctx: Context<DistributeSolFees>) -> Result<()> {
        msg!("=== SOL Fee Distribution ===");
        
        let fee_collector_balance = ctx.accounts.fee_collector.lamports();
        let rent_exempt_min = Rent::get()?.minimum_balance(0);
        
        // Calculate distributable amount (keep rent-exempt minimum in account)
        let distributable = fee_collector_balance.saturating_sub(rent_exempt_min);
        require!(distributable > 0, ErrorCode::NoFeesToDistribute);
        
        msg!("Fee collector balance: {} lamports", fee_collector_balance);
        msg!("Rent exempt minimum: {} lamports", rent_exempt_min);
        msg!("Distributable amount: {} lamports", distributable);
        
        // Calculate individual amounts using the same BPS constants
        // u128 intermediate — same overflow class as KOTS BPS math.
        // SOL pots are usually small; keep one consistent pattern.
        let jackpot_amount = (distributable as u128)
            .checked_mul(JACKPOT_BPS_SOL as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let incoming_amount = (distributable as u128)
            .checked_mul(INCOMING_BPS_SOL as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let treasury_amount = (distributable as u128)
            .checked_mul(TREASURY_BPS_SOL as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let admin_amount = (distributable as u128)
            .checked_mul(ADMIN_BPS_SOL as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        let dev_amount = (distributable as u128)
            .checked_mul(DEV_BPS_SOL as u128)
            .and_then(|x| x.checked_div(10_000))
            .and_then(|x| u64::try_from(x).ok())
            .ok_or(ProgramError::ArithmeticOverflow)?;
        
        // Remaining goes to burn (dust + burn percentage)
        let distributed_total = jackpot_amount + incoming_amount + treasury_amount + admin_amount + dev_amount;
        let burn_amount = distributable.saturating_sub(distributed_total);
        
        msg!("Distribution breakdown:");
        msg!("  Jackpot: {} lamports (50%)", jackpot_amount);
        msg!("  Incoming: {} lamports (7%)", incoming_amount);
        msg!("  Treasury: {} lamports (37%)", treasury_amount);
        msg!("  Admin: {} lamports (3%)", admin_amount);
        msg!("  Dev: {} lamports (3%)", dev_amount);
        msg!("  Burn: {} lamports (3% + dust)", burn_amount);
        
        // Transfer SOL using native transfer (deduct from fee_collector)
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= jackpot_amount;
        **ctx.accounts.jackpot_sol_account.try_borrow_mut_lamports()? += jackpot_amount;
        
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= incoming_amount;
        **ctx.accounts.incoming_sol_account.try_borrow_mut_lamports()? += incoming_amount;
        
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= treasury_amount;
        **ctx.accounts.treasury_sol_account.try_borrow_mut_lamports()? += treasury_amount;
        
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= admin_amount;
        **ctx.accounts.admin_sol_account.try_borrow_mut_lamports()? += admin_amount;
        
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= dev_amount;
        **ctx.accounts.dev_sol_account.try_borrow_mut_lamports()? += dev_amount;
        
        **ctx.accounts.fee_collector.try_borrow_mut_lamports()? -= burn_amount;
        **ctx.accounts.burn_sol_account.try_borrow_mut_lamports()? += burn_amount;
        
        msg!("✅ SOL fees distributed successfully");
        msg!("=== Total distributed: {} lamports ===", distributable);
        
        Ok(())
    }
    
    // ========================================================================
    // FALLBACK HANDLER FOR SPL TRANSFER HOOK INTERFACE (ANCHOR 0.31.1)
    // ========================================================================
    
    /// Fallback function to handle SPL Transfer Hook interface calls
    /// 
    /// **Context:** Anchor 0.31.1 doesn't support #[fallback] or #[interface] attributes,
    /// but it DOES invoke this `fallback` function when it receives an instruction 
    /// discriminator that doesn't match any auto-generated Anchor instructions.
    /// 
    /// Token-2022 calls with discriminator [105, 37, 101, 197, 75, 251, 102, 26]
    /// which represents sighash("spl-transfer-hook-interface:execute")
    /// 
    /// **Payload Structure (Borsh-encoded):**
    /// - Bytes 0-7: Discriminator [105, 37, 101, 197, 75, 251, 102, 26]
    /// - Byte 8: Enum variant tag (0 for TransferHook)
    /// - Bytes 9-16: Amount (u64, little-endian)
    /// 
    /// **This is the officially supported pattern for SPL Token-2022 Transfer Hook in Anchor 0.31+**
    pub fn fallback<'info>(
        program_id: &Pubkey,
        accounts: &'info [AccountInfo<'info>],
        data: &[u8],
    ) -> Result<()> {
        // SPL Transfer Hook Execute discriminator
        const TRANSFER_HOOK_DISCRIMINATOR: [u8; 8] = [105, 37, 101, 197, 75, 251, 102, 26];
        
        // Return early if data is too short or discriminator doesn't match
        if data.len() < 8 || data[0..8] != TRANSFER_HOOK_DISCRIMINATOR {
            // Not a transfer hook call - let Anchor handle other instructions normally
            return Ok(());
        }
        
        // Deserialize amount from the SPL transfer-hook-interface Execute payload.
        // The interface encodes Execute as: [8-byte SplDiscriminate][8-byte amount LE]
        // = 16 bytes total, amount at offset 8. (There is NO Borsh enum-variant tag —
        // the interface uses SplDiscriminate, not Borsh enum encoding. An earlier
        // version assumed a 17-byte [disc][1-byte variant][8-byte amount] layout and
        // read the amount at offset 9, which rejected every real 16-byte call with
        // InvalidInstructionData. 2026-07-22: corrected to offset 8 / length 16 after
        // the golden matrix exercised this path for the first time.)
        if data.len() < 16 {
            return Err(ProgramError::InvalidInstructionData.into());
        }

        // Skip the 8-byte discriminator; amount is the next 8 bytes.
        let amount = u64::from_le_bytes(
            data[8..16].try_into()
                .map_err(|_| ProgramError::InvalidInstructionData)?
        );
        
        msg!("🔄 Fallback routing SPL Transfer Hook Execute (amount: {})", amount);

        // ───────────────────────── O32 INSTRUMENTATION ─────────────────────────
        // TEMPORARY. Remove once O32 is closed.
        //
        // try_accounts below fails with AccountNotEnoughKeys (3005) when the
        // transfer is initiated by CPI from another program, while the same
        // transfer sent directly succeeds. The static counts agree — 14 active
        // extra_account_metas entries + 4 base + 1 meta-list = 19, and the
        // TransferHook struct declares exactly 19 active fields — so something is
        // lost in transit rather than mis-declared.
        //
        // This prints what Token-2022 ACTUALLY delivered. Compare against the
        // count logged by fee-management's transfer_one_leg on the way in.
        // COUNT ONLY — see the matching note in fee-management's transfer_one_leg.
        // Per-account logging here is even more expensive: this runs once per leg
        // INSIDE the hook, on top of the caller's own logging.
        msg!("O32: hook received {} accounts", accounts.len());

        // Reconstruct TransferHook accounts using Anchor's try_accounts
        // Note: try_accounts expects &mut &[AccountInfo], not an iterator
        let mut accounts_slice = &accounts[..];
        let mut transfer_hook_accounts = TransferHook::try_accounts(
            program_id,
            &mut accounts_slice,
            &[],
            &mut Default::default(),
            &mut Default::default(),
        )?;
        
        // Remaining accounts after try_accounts consumed the needed ones
        let remaining_accounts: Vec<AccountInfo<'info>> = accounts_slice.iter().cloned().collect();
        
        // Create Anchor context
        let ctx = Context::new(
            program_id,
            &mut transfer_hook_accounts,
            &remaining_accounts,
            Default::default(),
        );
        
        // Delegate to the main transfer_hook function
        transfer_hook(ctx, amount)
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// Helper function for PDA creation and TLV initialization
/// CRITICAL: #[inline(never)] keeps heavy stack usage isolated from shared dispatch frame
/// Uses crate::ID instead of program_id parameter to avoid Anchor fallback detection
#[inline(never)]
fn initialize_extra_account_meta_list_impl<'info>(
    accounts: &InitializeExtraAccountMeta<'info>,
) -> Result<()> {
    let fee_config = &accounts.fee_config;
    
    // Define the extra accounts needed for the transfer hook
    // NOTE: Use Vec::with_capacity + sequential pushes to avoid stack-backed vec![] macro
    // This prevents ~3.5KB stack array from inflating the shared dispatch frame
    let mut account_metas = Vec::with_capacity(8); // 7 + the gated sell_fee_debt
    
    // Build account_metas incrementally to minimize stack usage
    // 1. Instructions sysvar (for DEX detection)
    account_metas.push(ExtraAccountMeta::new_with_pubkey(
        &instructions::ID,
        false, // is_signer
        false, // is_writable
    )?);
    
    // 2. Whitelist registry (PDA, read-only)
    account_metas.push(ExtraAccountMeta::new_with_seeds(
        &[Seed::Literal {
            bytes: b"whitelist".to_vec(),
        }],
        false,
        false,
    )?);
    
    // 3. Pool registry (PDA, read-only)
    account_metas.push(ExtraAccountMeta::new_with_seeds(
        &[Seed::Literal {
            bytes: b"pool_registry".to_vec(),
        }],
        false,
        false,
    )?);
    
    // 4. Fee config (PDA, read-only)
    account_metas.push(ExtraAccountMeta::new_with_seeds(
        &[Seed::Literal {
            bytes: b"fee_config".to_vec(),
        }],
        false,
        false,
    )?);
    
    // 5. Fee Authority PDA (signer for fee withdrawals)
    account_metas.push(ExtraAccountMeta::new_with_seeds(
        &[Seed::Literal {
            bytes: FEE_AUTHORITY_SEED.to_vec(),
        }],
        false, // is_signer (will be signed via invoke_signed)
        false, // is_writable
    )?);
    
    // ── ENTRIES 6-11 AND 13 REMOVED 2026-07-29 (O32). ──────────────────────
    //
    // Gone, together with the matching seven fields on the TransferHook struct
    // (see the block there for the full reasoning):
    //
    //   6-10  jackpot / incoming / admin / treasury / dev wallets
    //   11    System Program
    //   13    Associated Token Program
    //
    // All seven had ZERO references in this program — residue of the DEX
    // hybrid-fee branch removed 2026-07-22 under Option D, the same category as
    // the gated `sell_fee_debt` below.
    //
    // The measurement that forced it: the resolver OOMs on ENTRY COUNT, not on
    // bytes. 13 entries executed; 14 entries OOM'd at refund-queue capacities
    // 16, 32, 64 and 128 alike, i.e. across an 8x range of account size. So the
    // list had to lose entries, and these were the seven that cost nothing.
    //
    // ⚠ ORDER IS THE ABI. Token-2022 passes the extras in this list's order and
    // Anchor matches TransferHook positionally against it. Adding or removing a
    // push here without the matching struct field — or in a different position —
    // reverts every transfer of the mint with AccountNotEnoughKeys (3005).
    //
    // Token-2022 (was #12) survives because the hook CPIs into it for the
    // immediate-refund path; it is now entry #6.

    // 6. Token-2022 Program (required by transfer hook interface)
    account_metas.push(ExtraAccountMeta::new_with_pubkey(
        &spl_token_2022::ID,
        false,
        false,
    )?);
    
    // 7. Pending Refund Queue (non-PDA keypair account, mutable)
    //
    // ⚠⚠ O32 PROBE GATE — TEMPORARY, ADDED 2026-07-29. REMOVE WHEN O32 CLOSES.
    //
    // WHY THIS GATE EXISTS. The heap ladder run on 2026-07-29 proved that ONE
    // hooked CPI transfer of this mint exhausts a default 32 KB BPF heap even
    // with the refund queue already reduced from 49,164 to 6,200 bytes. All five
    // rungs — no ComputeBudget, CU-only, and 64/128/256 KB heap frames — failed
    // at EXACTLY 59,156 CU. Identical to the compute unit, which is what proves
    // the harness does not apply `request_heap_frame` while it does apply the CU
    // limit (200,000 -> 1,399,850 -> 1,399,700, tracking the instruction count).
    //
    // A harness that gives 32 KB and ignores frames IS a DEX router. So the
    // measurement is faithful, not broken: on this evidence KOTS is untradeable.
    //
    // WHAT THIS GATE DISCRIMINATES. The compile-time guard puts the meta-list
    // copy at 13,227 bytes against 32,768, and it OOMs anyway — so ~19,500 bytes
    // are unaccounted for. Two candidate explanations need separating, and the
    // resolver runs BEFORE the hook body, so the failure MODE tells them apart:
    //
    //   Build with `--features o32-probe` and re-run the ladder.
    //
    //   * STILL OOM  -> the queue was never the binding constraint. Stop
    //                   shrinking it. The meta list must lose ACCOUNTS, and
    //                   MAX_WHITELISTED_WALLETS = 64 (2,100 B) is next largest.
    //   * Past the resolver, then a missing-account failure INSIDE the hook
    //                -> the queue IS the constraint. `refund_queue` is still
    //                   declared in the TransferHook accounts struct, so
    //                   try_accounts is EXPECTED to fail here — that failure is
    //                   the positive result, not a regression. The real fix is
    //                   then moving refunds off the execute path entirely (O34
    //                   events), because the hook WRITES this account and so it
    //                   cannot simply be dropped.
    //
    // Safe to gate: with `sol-fees` off this is the LAST entry, so nothing
    // renumbers. Entry #15's `Seed::AccountKey { index: 3 }` addresses a BASE
    // account (owner), not a position in this list, so it is unaffected either
    // way. Same reasoning C3 recorded on 2026-07-28.
    //
    // ⚠ NEVER SHIP ENABLED. This list is written ON-CHAIN. A mainnet mint
    // initialized from a probe build would have a meta list the hook cannot
    // satisfy, and fixing it post-mint is a migration (close + re-init the meta
    // list), not an edit.
    #[cfg(not(feature = "o32-probe"))]
    account_metas.push(ExtraAccountMeta::new_with_pubkey(
        &fee_config.refund_queue,
        false, // is_signer
        true,  // is_writable
    )?);
    
    // 8. Sell Fee Debt PDA — GATED 2026-07-28 (C3). (Was #15 before the
    //    2026-07-29 removal of seven dead entries; still LAST, so the
    //    'safe to gate because it is the last entry' reasoning holds.)
    //
    // This declared a PDA that Token-2022 derives and passes on EVERY transfer,
    // for a mechanism the hook itself documents as abandoned: "Option D: ONE
    // uniform KOTS fee, no SOL leg, no buy/sell asymmetry", and before that,
    // "a hook cannot create accounts... the sell fee was therefore opt-in from
    // the payer's side — i.e. uncollectable."
    //
    // So every transfer was paying account-resolution and CU cost to pass an
    // address that nothing creates and nothing reads.
    //
    // SAFE TO GATE BECAUSE THIS IS THE LAST ENTRY. Seed::AccountKey { index: 3 }
    // refers to a BASE account, not a position in this list, and no later entry
    // exists whose index would shift. Removing any earlier entry would silently
    // renumber the ones after it.
    //
    // ⚠ THIS LIST IS WRITTEN ON-CHAIN by initialize_extra_account_meta_list.
    // Changing it after the mint exists requires closing and re-initializing the
    // meta list (scripts/05_close_and_reinit_extra_meta.ts). Pre-mint this is
    // free; post-launch it is a migration. That is why it is being done now.
    #[cfg(feature = "sol-fees")]
    account_metas.push(ExtraAccountMeta::new_with_seeds(
        &[
            Seed::Literal {
                bytes: b"sell_fee_debt".to_vec(),
            },
            Seed::AccountKey { index: 3 }, // Owner account (base account #3)
        ],
        false, // is_signer
        true,  // is_writable
    )?);

    // ── ENTRY-COUNT GUARD REMOVED, SAME DAY IT WAS ADDED (2026-07-29) ─────
    //
    // It asserted `account_metas.len() <= 13`, from the observation that 13
    // entries executed and 14 did not. The very next run falsified it: SEVEN
    // entries, with the refund queue present, OOM'd. Fewer entries, worse
    // outcome. Entry count does not predict the failure.
    //
    // Removed rather than relabelled. A guard asserting an invariant known to be
    // wrong is worse than no guard: it will be cited as evidence by whoever
    // reads it next. The knowledge is kept here instead.
    //
    // What every run so far agrees on, and the only thing that does:
    //
    //     queue ABSENT  from extra_account_metas -> resolver survived (13 entries)
    //     queue PRESENT in extra_account_metas   -> OOM, at 7 and 14 entries,
    //                                               and at 824..6,200 bytes
    //
    // When the mechanism IS known, re-add a guard here — and note that its error
    // must again be appended LAST in ErrorCode, because Anchor numbers by
    // declaration order.

    // Calculate required size for TLV data (NO Anchor discriminator)
    let required_size = ExtraAccountMetaList::size_of(account_metas.len())?;

    // The artifact, not the source, is what executes. On 2026-07-28 three
    // separate cases were found where a build output disagreed with the code
    // that produced it (target/verifiable vs target/deploy, a stale IDL, and a
    // dist/ bundle). So the list announces its own shape: a run that reports an
    // entry count you did not expect is running a .so you did not build.
    msg!(
        "extra_account_metas: {} entries, {} bytes TLV",
        account_metas.len(),
        required_size
    );
    #[cfg(feature = "o32-probe")]
    msg!(
        "⚠⚠ O32-PROBE BUILD — refund_queue OMITTED from extra_account_metas. \
         NOT A SHIPPABLE ARTIFACT. If you are seeing this on any cluster that \
         matters, stop."
    );
    
    // Get PDA seeds for signing
    let mint_key = accounts.mint.key();
    let seeds = &[
        b"extra-account-metas".as_ref(),
        mint_key.as_ref(),
    ];
    let (pda, bump) = Pubkey::find_program_address(seeds, &crate::ID);
    require_keys_eq!(pda, accounts.extra_account_meta_list.key(), ErrorCode::InvalidPDA);
    
    let signer_seeds = &[
        b"extra-account-metas".as_ref(),
        mint_key.as_ref(),
        &[bump],
    ];

    // Check if account needs to be created or resized
    let account = &accounts.extra_account_meta_list;
    let current_lamports = account.lamports();
    let current_size = account.data_len();
    
    // Verify account is owned by this program (or system program if uninitialized)
    if current_size > 0 && account.owner != &crate::ID {
        return Err(ErrorCode::AccountOwnedByWrongProgram.into());
    }
    
    let rent = Rent::get()?;
    let required_lamports = rent.minimum_balance(required_size);
    
    if current_size == 0 {
        // Account doesn't exist - create it
        msg!("Creating extra account meta list PDA (size: {} bytes, no discriminator)", required_size);
        
        anchor_lang::solana_program::program::invoke_signed(
            &anchor_lang::solana_program::system_instruction::create_account(
                accounts.payer.key,
                account.key,
                required_lamports,
                required_size as u64,
                &crate::ID,
            ),
            &[
                accounts.payer.to_account_info(),
                account.to_account_info(),
                accounts.system_program.to_account_info(),
            ],
            &[signer_seeds],
        )?;
    } else if current_size != required_size {
        // Account exists but wrong size - realloc
        msg!("Resizing extra account meta list from {} to {} bytes", current_size, required_size);
        
        // Top up rent if needed
        if current_lamports < required_lamports {
            let lamports_diff = required_lamports - current_lamports;
            anchor_lang::solana_program::program::invoke(
                &anchor_lang::solana_program::system_instruction::transfer(
                    accounts.payer.key,
                    account.key,
                    lamports_diff,
                ),
                &[
                    accounts.payer.to_account_info(),
                    account.to_account_info(),
                ],
            )?;
        }
        
        account.resize(required_size)?;
    }
    
    // Zero out the data buffer
    let mut data = account.try_borrow_mut_data()?;
    data.fill(0);
    
    // Write TLV data starting at byte 0 (NO Anchor discriminator)
    ExtraAccountMetaList::init::<ExecuteInstruction>(&mut data, &account_metas)?;

    msg!("✅ Extra account meta list initialized with {} accounts (TLV starts at byte 0)", account_metas.len());
    msg!("✓ Instructions sysvar (DEX detection)");
    msg!("✓ Whitelist, Pool, Fee Config PDAs");
    msg!("✓ Fee Authority PDA");
    msg!("✓ Token-2022 Program");
    msg!("✓ Pending Refund Queue (exempt transfer refunds)");
    #[cfg(feature = "sol-fees")]
    msg!("✓ Sell Fee Debt PDA (per-user, dynamic from owner account)");
    msg!("NOTE: Owner account (fee payer) is a BASE account - not listed here!");
    msg!("NOTE: No Anchor discriminator - TLV data compatible with Token-2022!");
    
    Ok(())
}

// ========================================================================
// BPF STACK OPTIMIZATION: RAW invoke_signed (NO Vec allocations)
// ========================================================================
// CRITICAL: Must avoid .to_vec() which materializes heap buffers on stack.
// Use static arrays and slices ONLY to minimize stack footprint.
#[inline(never)]
// transfer_from_vault_raw was REMOVED 2026-07-22 (DESIGN B).
//
// It built a raw TransferChecked and invoke_signed'd it with the fee_authority PDA,
// i.e. it moved this hooked mint FROM INSIDE the hook program. Every caller of it
// (the KOTS collector legs and the sell-fee-debt legs) was therefore unrunnable:
// Token-2022 must CPI back into the hook to service the transfer, which would put
// the hook program on the call stack twice, and Solana forbids that reentrancy.
//
// It is deleted rather than left dead so the footgun cannot be picked up again.
// All token movement the hook performs is now fee-extension operations only
// (WithdrawWithheldTokensFromAccounts), which are not transfers and do not invoke
// the hook. Percentage splits are executed keeper-side.

// ── DEX detection + SOL charging helpers REMOVED 2026-07-22 (fee model: option D) ──
//
// charge_sol_fee, detect_dex_swap, is_buy_direction, is_known_dex_program,
// is_protocol_pool_trade, detect_transaction_type and
// load_current_transaction_instructions all existed to serve the hybrid
// buy/sell fee. That model is gone: a hook cannot move the user's SOL (the owner
// is passed read-only and non-signing) and cannot create the debt account it
// would need, so the whole path was unrunnable and would have reverted every DEX
// trade. They are deleted rather than left dead so nothing can call them back
// into existence. The fee is now solely the Transfer Fee Extension's automatic
// withholding. `is_pool_destination` and `is_whitelisted` remain — the anti-whale
// cap and the exemption logic still use them.

/// Check if a wallet is whitelisted
fn is_whitelisted(registry: &Account<WhitelistRegistry>, wallet: &Pubkey) -> bool {
    // OWNER BOUNTY is permanently fee-exempt on BOTH realms, as compile-time
    // constants — not a registry row and not `#[cfg(feature = "realm-main")]`.
    // Live 2hqT83 (pre-this build) ignored 6ZuFby6 on devnet (H7 2026-09-08).
    // One .so therefore exempts:
    //   devnet  6ZuFby6oKQ3bS7surHU9scKVj9dcvnUuMbAt5mT7zB3J
    //   mainnet BbvBeQy9oEmF9D5eVtj8hp1tibvMhtFFgVdiD4EqtT9V
    *wallet == kots_common::constants::BOUNTY_WALLET_DEVNET
        || *wallet == kots_common::constants::BOUNTY_WALLET_MAINNET
        || *wallet == kots_common::constants::PRIZE_VAULT_OWNER_MAINNET
        || registry.wallets.contains(wallet)
}

/// Anti-whale exemption: a destination is a pool if a registered address
/// matches either its OWNER (detect_transaction_type's convention) or the
/// token account itself (is_buy_direction's convention) — both registration
/// styles exist in this registry, so the cap honors both.
fn is_pool_destination(
    pool_registry: &Account<PoolRegistry>,
    dest_owner: &Pubkey,
    dest_token: &Pubkey,
) -> bool {
    for pool_info in &pool_registry.pools {
        if dest_owner == &pool_info.address || dest_token == &pool_info.address {
            return true;
        }
    }
    false
}









// ============================================================================
// ACCOUNT CONTEXTS
// ============================================================================

#[derive(Accounts)]
pub struct InitializeFeeAuthority<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + FeeAuthority::INIT_SPACE,
        seeds = [FEE_AUTHORITY_SEED],
        bump
    )]
    pub fee_authority: Account<'info, FeeAuthority>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeWhitelist<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + WhitelistRegistry::INIT_SPACE,
        seeds = [b"whitelist"],
        bump
    )]
    pub whitelist_registry: Account<'info, WhitelistRegistry>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateWhitelist<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    // ACT-03 (F6): the single stored-authority gate (`has_one = authority`) is
    // replaced by owner-registry quorum — see add_to_whitelist / remove_from_whitelist.
    // The registry's `authority` field is retained but is no longer the gate.
    #[account(
        mut,
        seeds = [b"whitelist"],
        bump
    )]
    pub whitelist_registry: Account<'info, WhitelistRegistry>,

    /// ACT-03 (F6): owner registry, owned by the security-governance program.
    /// Read-only. `Account<_, OwnerRegistry>` makes Anchor verify the owning
    /// program and the account discriminator; `seeds::program` pins the
    /// cross-program PDA. Fully-qualified to avoid any import-scoping ambiguity.
    #[account(
        seeds = [security_governance::owner_registry::OWNER_REGISTRY_SEED],
        bump = owner_registry.bump,
        seeds::program = security_governance::ID
    )]
    pub owner_registry: Account<'info, security_governance::owner_registry::OwnerRegistry>,
}

#[derive(Accounts)]
pub struct InitializePoolRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + PoolRegistry::INIT_SPACE,
        seeds = [b"pool_registry"],
        bump
    )]
    pub pool_registry: Account<'info, PoolRegistry>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdatePoolRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"pool_registry"],
        bump,
        has_one = authority
    )]
    pub pool_registry: Account<'info, PoolRegistry>,
}

#[derive(Accounts)]
pub struct CloseFeeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: read as a raw account (not Account<FeeConfig>) so the OLD, shorter
    /// layout can still be closed — see close_fee_config. Seeds pin it to the
    /// real fee_config PDA; the authority is verified manually from the raw bytes.
    #[account(
        mut,
        seeds = [b"fee_config"],
        bump
    )]
    pub fee_config: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseFeeAuthority<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"fee_authority"],
        bump,
        has_one = authority,
        close = authority
    )]
    pub fee_authority: Account<'info, FeeAuthority>,
}

#[derive(Accounts)]
pub struct CloseWhitelist<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"whitelist"],
        bump,
        has_one = authority,
        close = authority
    )]
    pub whitelist_registry: Account<'info, WhitelistRegistry>,
}

#[derive(Accounts)]
pub struct ClosePoolRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"pool_registry"],
        bump,
        has_one = authority,
        close = authority
    )]
    pub pool_registry: Account<'info, PoolRegistry>,
}

#[derive(Accounts)]
pub struct InitializeFeeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = 8 + FeeConfig::INIT_SPACE,
        seeds = [b"fee_config"],
        bump
    )]
    pub fee_config: Account<'info, FeeConfig>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateFeeConfig<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    
    /// CHECK: Manual deserialization to handle size mismatch from old padding
    #[account(
        mut,
        seeds = [b"fee_config"],
        bump
    )]
    pub fee_config: UncheckedAccount<'info>,
    
    /// fee_config.authority, checked manually in the instruction (the account is
    /// read without Anchor's layout check to survive old padding). It MUST sign:
    /// as an UncheckedAccount anyone could pass the authority's public key and
    /// repoint refund_queue, making every hooked transfer fail InvalidRefundQueue
    /// (2026-10-03). All callers already pass the authority as the paying signer.
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateFeeConfig<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
}

#[derive(Accounts)]
pub struct UpdateSpotPrice<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
}

#[derive(Accounts)]
pub struct SetAntiWhaleActive<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
}

#[derive(Accounts)]
pub struct UpdateFeeRates<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
}

#[cfg(feature = "sol-fees")]
#[derive(Accounts)]
#[instruction(owner: Pubkey)]
pub struct InitializeSellFeeDebt<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + SellFeeDebt::INIT_SPACE,
        seeds = [b"sell_fee_debt", owner.as_ref()],
        bump
    )]
    pub sell_fee_debt: Account<'info, SellFeeDebt>,

    pub system_program: Program<'info, System>,
}

/// Account struct for collect_sell_fee_debt_part1 (Part 1: Keeper + Jackpot + Incoming)
#[cfg(feature = "sol-fees")]
/// PDA stays OPEN (no close attribute) - Part2 will close it
#[derive(Accounts)]
pub struct CollectSellFeeDebtPart1<'info> {
    #[account(mut)]
    pub keeper: Signer<'info>,

    /// CHECK: Validated via PDA seeds
    pub owner: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [b"sell_fee_debt", owner.key().as_ref()],
        bump
        // NO close attribute - PDA stays open until part2 settles it
    )]
    pub sell_fee_debt: Account<'info, SellFeeDebt>,

    // DESIGN B (2026-07-22): the fee_vault, fee_authority and the five destination
    // token accounts are GONE. This instruction no longer transfers anything (it
    // cannot — see the note in its body), so requiring those accounts would only
    // force callers to supply ATAs that may not exist. fee_config is retained so the
    // published split plan names the exact destination wallet for each leg.
    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,
}

/// Account struct for collect_sell_fee_debt_part2 (Part 2: Admin + Dev + Treasury)
#[cfg(feature = "sol-fees")]
/// PDA is CLOSED (close attribute present) - returns rent to keeper
#[derive(Accounts)]
pub struct CollectSellFeeDebtPart2<'info> {
    /// SECURITY (2026-07-23): `keeper` was a bare `Signer` bound to NOTHING while
    /// this instruction closes the debt record via `close = keeper`. Any wallet —
    /// including the debtor — could call it for any `owner` and delete that
    /// owner's sell-fee debt without a single token being paid, pocketing the
    /// rent. Because the sell_fee_debt PDA is the ONLY record that the 3% sell
    /// fee is owed, this made the sell tax (and therefore the 50% of it that
    /// funds the jackpot) fully evadable by anyone, permissionlessly.
    ///
    /// The keeper is now bound to `fee_config.authority`.
    ///
    /// NOTE (design, not fixed here): even a validated keeper is TRUSTED to have
    /// performed the part1 split before settling — nothing on-chain proves the
    /// destination wallets were actually credited. See the report: settlement
    /// should verify destination balance deltas, or part1/part2 should be one
    /// atomic instruction.
    #[account(mut)]
    pub keeper: Signer<'info>,

    /// CHECK: Validated via PDA seeds
    pub owner: AccountInfo<'info>,

    #[account(
        seeds = [b"fee_config"],
        bump,
        constraint = fee_config.authority == keeper.key() @ ErrorCode::Unauthorized
    )]
    pub fee_config: Account<'info, FeeConfig>,

    // DESIGN B (2026-07-22): transfers removed; this instruction's only remaining
    // job is the one thing solely the program can do — close the debt record.
    #[account(
        mut,
        seeds = [b"sell_fee_debt", owner.key().as_ref()],
        bump,
        close = keeper  // settle: rent returns to the keeper
    )]
    pub sell_fee_debt: Account<'info, SellFeeDebt>,
}

#[derive(Accounts)]
pub struct InitializeRefundQueue<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// Pre-created refund queue account. REFUND_QUEUE_SIZE bytes (6,200 at a
    /// capacity of 128) — created client-side because the 10KB CPI allocation
    /// limit is not the binding constraint; the O32 heap budget is.
    /// Must be created client-side via SystemProgram.createAccount before calling this instruction
    /// CHECK: Must be system-owned, rent-paid by authority, ownership transferred in instruction
    #[account(mut)]
    pub refund_queue: AccountInfo<'info>,
    
    #[account(
        mut,
        seeds = [b"fee_config"],
        bump,
        constraint = fee_config.authority == authority.key() @ ErrorCode::Unauthorized
    )]
    pub fee_config: Account<'info, FeeConfig>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ProcessRefundsBulk<'info> {
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        constraint = refund_queue.key() == fee_config.refund_queue @ ErrorCode::InvalidRefundQueue
    )]
    pub refund_queue: AccountLoader<'info, PendingRefundQueue>,
    
    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct HarvestWithheldToVault<'info> {
    #[account(mut)]
    pub mint: InterfaceAccount<'info, Mint>,

    /// Destination: the fee-distribution vault (a Token-2022 account of this
    /// mint). `execute_fee_distribution`'s parts drain this to the seven wallets.
    #[account(mut, constraint = vault.mint == mint.key() @ ErrorCode::InvalidRefundQueue)]
    pub vault: InterfaceAccount<'info, TokenAccount>,

    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    /// Must equal `fee_config.authority` (checked in the handler).
    pub authority: Signer<'info>,

    pub token_program: Interface<'info, TokenInterface>,
    // remaining_accounts: the source Token-2022 accounts holding withheld KOTS.
}

#[derive(Accounts)]
pub struct CloseFeeVault<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    /// CHECK: Fee vault account - closed via Token-2022 CPI
    #[account(mut)]
    pub fee_vault: AccountInfo<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    #[account(
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
    
    /// Token-2022 program for closing the account
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct CloseExtraAccountMetaList<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    /// CHECK: Extra Account Meta List PDA - manually closed in function
    pub extra_account_meta_list: AccountInfo<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,
}

#[derive(Accounts)]
pub struct InitializeExtraAccountMeta<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: ExtraAccountMetaList Account - manually created without Anchor discriminator
    /// Must store TLV data starting at byte 0 for Token-2022 compatibility
    #[account(
        mut,
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: AccountInfo<'info>,

    pub mint: InterfaceAccount<'info, Mint>,
    
    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,
    
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct TransferHook<'info> {
    // Core transfer accounts (provided by Token-2022).
    // NOT `mut`: during a transfer-hook Execute CPI, Token-2022 passes the source
    // and destination token accounts as READ-ONLY. The token movement has already
    // happened by the time the hook runs (see destination_token.amount being
    // POST-transfer below), and the hook only READS these accounts (owner, key,
    // amount for the anti-whale cap) — it never mutates balances. Declaring them
    // `mut` made Anchor enforce writability the runtime doesn't grant, failing
    // every transfer with ConstraintMut (0x7d0). The token:: constraints remain —
    // they are read-only validations. (2026-07-22: mut removed after the golden
    // matrix first exercised the execute path.)
    #[account(
        token::mint = mint,
        token::authority = owner,
        token::token_program = token_program
    )]
    pub source_token: InterfaceAccount<'info, TokenAccount>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        token::mint = mint,
        token::token_program = token_program
    )]
    pub destination_token: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: Source token account owner
    /// The owner is a signer from the base transfer (READ-ONLY in transfer hooks)
    /// NOTE: We CANNOT charge SOL fees here - Token-2022 doesn't provide owner as writable
    pub owner: UncheckedAccount<'info>,

    /// CHECK: ExtraAccountMetaList account
    #[account(
        seeds = [b"extra-account-metas", mint.key().as_ref()],
        bump
    )]
    pub extra_account_meta_list: UncheckedAccount<'info>,

    // EXTRA ACCOUNTS for fee logic:
    
    /// CHECK: Instructions sysvar for DEX detection
    pub instructions_sysvar: AccountInfo<'info>,
    
    /// Whitelist registry (extra account)
    #[account(seeds = [b"whitelist"], bump)]
    pub whitelist_registry: Account<'info, WhitelistRegistry>,

    /// Pool registry (extra account)
    #[account(seeds = [b"pool_registry"], bump)]
    pub pool_registry: Account<'info, PoolRegistry>,

    /// Fee config (extra account)
    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,

    /// Fee Authority PDA (extra account, signer for fee withdrawals)
    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    // ── SEVEN ACCOUNTS REMOVED 2026-07-29 (O32). ────────────────────────────
    //
    // Removed here AND from initialize_extra_account_meta_list_impl, together,
    // because the two must agree: the meta list determines what Token-2022
    // passes, and this struct determines what the hook demands. Change one and
    // every transfer reverts with AccountNotEnoughKeys (3005).
    //
    //   jackpot_wallet, incoming_wallet, admin_wallet, treasury_wallet,
    //   dev_wallet   — "Distribution wallets (mutable for SOL fees)"
    //   system_program              — needed only by the SOL leg
    //   associated_token_program    — "for ATA derivation"
    //
    // WHY. All seven had ZERO references in this program. They are residue of
    // the DEX hybrid-fee branch removed 2026-07-22 under Option D, which is the
    // same category as `sell_fee_debt` below — and for the same reason: a hook
    // that no longer moves SOL does not need the SOL destinations. Every
    // transfer of this mint was paying account-resolution cost to pass five
    // wallet addresses and two program IDs that nothing read.
    //
    // WHAT FORCED IT. Not tidiness — measurement. The 2026-07-29 bisection
    // showed the resolver OOMs on ENTRY COUNT, not on bytes:
    //
    //     13 entries -> resolver survived, hook executed   (probe build)
    //     14 entries -> OOM, at capacities 16 / 32 / 64 / 128 alike
    //
    // An 8x reduction in the refund queue's size changed nothing. So the
    // earlier "the resolver copies each account's full data, shrink the biggest
    // account" model was wrong; the queue mattered only because it was the
    // FOURTEENTH entry. Removing seven takes the list to 7 and leaves the queue
    // in place at its original size.
    //
    // ORDER IS POSITIONAL. Token-2022 passes 4 base accounts, then the meta-list
    // account, then the resolved extras IN META-LIST ORDER, and Anchor matches
    // this struct against that sequence. The surviving fields are in meta-list
    // order and must stay that way.
    //
    // SAFE WITH RESPECT TO SEEDS. Nothing here is addressed by
    // `Seed::AccountKey`; the only such reference is entry #15's `index: 3`,
    // which points at a BASE account (owner) and is unaffected by extras
    // changing. Same reasoning C3 recorded on 2026-07-28.
    //
    // TO RESTORE, if the SOL leg is ever revived: re-add these seven fields
    // here in this position and the matching seven pushes in
    // initialize_extra_account_meta_list_impl, in the same order — and re-check
    // the entry count against the measured ceiling before assuming it fits.
    //
    // ⚠ The meta list is written ON-CHAIN. Pre-mint this is free; post-launch it
    // is a migration (close + re-init the meta list), not an edit.
    pub token_program: Interface<'info, TokenInterface>,

    // Pending refund queue (for exempt transfer refunds)
    #[account(
        mut,
        constraint = refund_queue.key() == fee_config.refund_queue @ ErrorCode::InvalidRefundQueue
    )]
    pub refund_queue: AccountLoader<'info, PendingRefundQueue>,

    // GATED 2026-07-28 (C3). Vestigial: the execute path never reads this. The only
    // mention of `sell_fee_debt` inside transfer_hook() is the comment recording
    // why the mechanism was abandoned. Kept behind the feature so the gated
    // instructions still compile with `--features sol-fees`.
    //
    // Last field on purpose: Anchor account order is the wire order, so gating a
    // field in the middle would shift every account after it for every client.
    //
    // The `/// CHECK:` below is REQUIRED, not decorative — Anchor's safety lint
    // rejects any UncheckedAccount without one, and it fires during IDL
    // generation rather than the release build, so dropping it still produces a
    // clean `.so` before it fails. Rewriting this comment block without it broke
    // the build on 2026-07-28.
    #[cfg(feature = "sol-fees")]
    /// CHECK: Optional sell fee debt PDA - may not exist yet.
    /// Validated via seeds derivation in transfer hook logic.
    #[account(mut)]
    pub sell_fee_debt: UncheckedAccount<'info>,
}

/// Account struct for collect_and_distribute_kots_fees_part1
///
/// DESIGN B (2026-07-22): this instruction is now **collect-only**. It performs a
/// single fee-extension operation (WithdrawWithheldTokensFromAccounts) and nothing
/// else. It no longer transfers to the distribution wallets, because a
/// `transfer_checked` of this hooked mint initiated from inside the hook program
/// re-enters the hook (hook → Token-2022 → hook) which Solana forbids.
///
/// The vault is therefore **keeper-owned** (`token::authority = authority`), not
/// PDA-owned: the keeper can then sign the six split transfers client-side, where
/// the depth is client → Token-2022 → hook (three distinct programs, no
/// reentrancy). The withdrawal authority is still the fee_authority PDA — a
/// withdrawal's destination ownership is irrelevant to who may authorize it.
#[derive(Accounts)]
pub struct CollectKotsFeesPart1<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = authority,
        token::token_program = token_program
    )]
    pub fee_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    #[account(
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,

    pub token_program: Interface<'info, TokenInterface>,
}

/// Account struct for collect_and_distribute_kots_fees_part2
/// Part 2: Distribute remaining fees to Dev, Burn, Treasury (3 transfers)
#[derive(Accounts)]
pub struct CollectKotsFeesPart2<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = fee_authority,
        token::token_program = token_program
    )]
    pub fee_vault: InterfaceAccount<'info, TokenAccount>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    #[account(
        seeds = [b"fee_config"],
        bump,
        has_one = authority
    )]
    pub fee_config: Account<'info, FeeConfig>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = fee_config.dev_wallet,
        token::token_program = token_program
    )]
    pub dev_token_account: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = fee_config.burn_address,
        token::token_program = token_program
    )]
    pub burn_token_account: InterfaceAccount<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = fee_config.treasury_wallet,
        token::token_program = token_program
    )]
    pub treasury_token_account: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Interface<'info, TokenInterface>,
}
#[cfg(feature = "sol-fees")]
#[derive(Accounts)]
pub struct DistributeSolFees<'info> {
    // SECURITY TODO (2026-07-23): `fee_collector` is the ONLY account in this
    // struct that is not pinned — every destination below is `address =
    // fee_config.X`, but the SOURCE is an unconstrained AccountInfo. Solana will
    // still refuse a debit of any account this program does not own, so this is
    // not an attacker-payout path; the exposure is that ANY lamport-holding PDA
    // owned by the transfer-hook program can be passed here and swept into the
    // fee wallets by anyone — including the rent of live config PDAs, which can
    // push them below rent-exemption and make them reapable.
    //
    // NOT PATCHED HERE because the intended derivation is a project decision:
    // there is no FEE_COLLECTOR_SEED anywhere in the repo. Pin it to whichever
    // is correct, e.g.
    //     #[account(mut, seeds = [FEE_COLLECTOR_SEED], bump)]
    // or, if the collector IS the fee authority PDA,
    //     #[account(mut, address = fee_authority.key())]
    #[account(mut)]
    /// CHECK: Lamport-only vault controlled by fee authority
    pub fee_collector: AccountInfo<'info>,

    #[account(seeds = [FEE_AUTHORITY_SEED], bump = fee_authority.bump)]
    pub fee_authority: Account<'info, FeeAuthority>,

    #[account(seeds = [b"fee_config"], bump)]
    pub fee_config: Account<'info, FeeConfig>,

    #[account(mut, address = fee_config.jackpot_wallet @ ErrorCode::InvalidJackpotWallet)]
    /// CHECK: SOL jackpot wallet
    pub jackpot_sol_account: AccountInfo<'info>,

    #[account(mut, address = fee_config.incoming_wallet @ ErrorCode::InvalidIncomingWallet)]
    /// CHECK: SOL incoming wallet
    pub incoming_sol_account: AccountInfo<'info>,

    #[account(mut, address = fee_config.treasury_wallet @ ErrorCode::InvalidTreasuryWallet)]
    /// CHECK: SOL treasury wallet
    pub treasury_sol_account: AccountInfo<'info>,

    #[account(mut, address = fee_config.admin_wallet @ ErrorCode::InvalidAdminWallet)]
    /// CHECK: SOL admin wallet
    pub admin_sol_account: AccountInfo<'info>,

    #[account(mut, address = fee_config.dev_wallet @ ErrorCode::InvalidDevWallet)]
    /// CHECK: SOL dev wallet
    pub dev_sol_account: AccountInfo<'info>,

    #[account(mut, address = fee_config.burn_address @ ErrorCode::InvalidBurnWallet)]
    /// CHECK: SOL burn wallet (receives redirected burn share)
    pub burn_sol_account: AccountInfo<'info>,
}

// ============================================================================
// STATE ACCOUNTS
// ============================================================================

#[account]
#[derive(InitSpace)]
pub struct FeeAuthority {
    pub authority: Pubkey,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct WhitelistRegistry {
    pub authority: Pubkey,
    #[max_len(MAX_WHITELISTED_WALLETS)]
    pub wallets: Vec<Pubkey>,
    pub last_updated: i64,
}

#[account]
#[derive(InitSpace)]
pub struct PoolRegistry {
    pub authority: Pubkey,
    #[max_len(MAX_POOL_ADDRESSES)]
    pub pools: Vec<PoolInfo>,
    /// The protocol-owned liquidity pool address (YOUR Raydium CLMM pool)
    /// When a buy happens in this pool, we skip all transfer hook fees (LP fees handle it)
    pub protocol_pool_address: Option<Pubkey>,
    pub last_updated: i64,
}

#[account]
#[derive(InitSpace)]
pub struct FeeConfig {
    pub authority: Pubkey,
    
    // Distribution wallets — validated against fee_config (which is populated
    // from the shared SystemWallets table at init by the deploy-time network flag)
    pub jackpot_wallet: Pubkey,      // 50% for both SOL and KOTS
    pub incoming_wallet: Pubkey,     // 7% Captain's share — main investor (both SOL and KOTS)
    pub admin_wallet: Pubkey,        // 3% for both SOL and KOTS
    pub treasury_wallet: Pubkey,     // 36% KOTS (remainder), 37% SOL (includes 3% burn redirect)
    pub dev_wallet: Pubkey,          // 3% for both SOL and KOTS
    pub burn_address: Pubkey,        // 1% KOTS only (no SOL burn)
    
    // Refund queue account (non-PDA to bypass 10KB CPI limit)
    pub refund_queue: Pubkey,        // Pre-created REFUND_QUEUE_SIZE account for KOTS refund queueing
    
    // Fee basis points
    pub transfer_fee_bps: u16,       // 100 = 1%
    pub dex_buy_fee_bps: u16,        // 100 = 1%
    pub dex_sell_fee_bps: u16,       // 300 = 3%
    
    // DEX pool reserve-based pricing (updated by keeper script)
    // Price is scaled by 1e9 (PRICE_PRECISION) for fixed-point arithmetic
    // Represents: how many KOTS per 1 SOL (e.g., 1_000_000 * 1e9 = 1M KOTS per SOL)
    pub kots_per_sol_price: u64,
    pub price_last_updated: i64,
    
    // Anti-whale activation flag (added 2026-07-22).
    // The 3%-of-supply cap (MAX_HOLD_BPS) is EARLY-STAGE protection: active while
    // the market is small, relaxed once it is large enough that a single 3% wallet
    // is a lesser threat. The trigger is a $2M CIRCULATING market cap — a USD figure
    // no on-chain feed can supply trustlessly (see KOTS-TOKENOMICS §4e: KOTS/USD is
    // on no public oracle). So the keeper that already writes kots_per_sol_price also
    // watches circulating mcap off-chain and flips this flag via set_anti_whale_active
    // (authority-only) when mcap crosses $2M. The hook only READS it — no oracle in
    // the instruction. Defaults TRUE so protection is on from genesis (mcap starts
    // far below $2M); the keeper sets it false once the threshold is crossed. Same
    // trust model as the spot price: a keeper-supplied input, multisig-gated setter.
    pub anti_whale_active: bool,
    
    pub last_updated: i64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, InitSpace)]
pub struct PoolInfo {
    pub address: Pubkey,
    pub pool_type: PoolType,
    pub registered_at: i64,
}

// Zero-copy refund queue for large account support (>10KB)
#[account(zero_copy)]
#[repr(C)]
#[derive(Debug)]
pub struct PendingRefundQueue {
    pub authority: Pubkey,
    pub head: u16,  // Index of first pending refund
    pub tail: u16,  // Index of next free slot
    pub count: u16, // Current number of pending refunds
    pub padding: u16, // Alignment padding
    pub last_updated: i64,
    // O32: length is REFUND_QUEUE_CAPACITY, and that number is a heap budget —
    // see the constant's definition before changing it.
    pub refunds: [PendingRefund; REFUND_QUEUE_CAPACITY_USIZE],
}

// Conditional derives: AnchorSerialize/Deserialize for IDL generation (host), Pod/Zeroable for BPF
#[cfg_attr(not(target_os = "solana"), derive(AnchorSerialize, AnchorDeserialize))]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[repr(C)]
pub struct PendingRefund {
    pub recipient: Pubkey,
    pub amount: u64,
    pub slot: u64,
}

impl Default for PendingRefund {
    fn default() -> Self {
        Self {
            recipient: Pubkey::default(),
            amount: 0,
            slot: 0,
        }
    }
}

// ── Ring-buffer operations ──────────────────────────────────────────────────
//
// Extracted from the instruction handlers on 2026-07-29, alongside the O32
// capacity change. The arithmetic was previously inline at four sites, each
// with its own `% 1024` literal, which is precisely the shape that makes a
// capacity change dangerous: miss one modulo and the ring writes past the end
// of a shorter array — a panic inside a transfer hook, so EVERY transfer of
// the mint reverts, and only from the moment the queue first wraps.
//
// The point of extracting them is that `tests/refund_queue_capacity.rs` can
// then drive the REAL code rather than a re-implementation of it. A test that
// models what the author believes the handler does will agree with the author
// and disagree with the chain.
impl PendingRefundQueue {
    /// The ring is full and the caller must take the immediate-refund branch.
    pub fn is_full(&self) -> bool {
        self.count as usize >= REFUND_QUEUE_CAPACITY_USIZE
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Append one refund at the tail. Returns `false` — without mutating — when
    /// full, so the caller can fall through to the immediate-CPI path.
    pub fn push_back(&mut self, recipient: Pubkey, amount: u64, slot: u64) -> bool {
        if self.is_full() {
            return false;
        }
        let tail = self.tail as usize;
        self.refunds[tail] = PendingRefund {
            recipient,
            amount,
            slot,
        };
        self.tail = ((tail + 1) % REFUND_QUEUE_CAPACITY_USIZE) as u16;
        self.count += 1;
        true
    }

    /// The oldest `n` recipients, FIFO, skipping cleared slots. Never reads more
    /// than `count` entries, so it cannot return a slot that was already drained.
    pub fn peek_recipients(&self, n: usize) -> Vec<Pubkey> {
        let n = n.min(self.count as usize);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let index = ((self.head as usize) + i) % REFUND_QUEUE_CAPACITY_USIZE;
            if self.refunds[index].recipient != Pubkey::default() {
                out.push(self.refunds[index].recipient);
            }
        }
        out
    }

    /// Drop the oldest `n`, clearing their slots and advancing the head.
    /// Saturates at `count`; never advances past the live region.
    pub fn pop_front(&mut self, n: usize) -> usize {
        let n = n.min(self.count as usize);
        for i in 0..n {
            let index = ((self.head as usize) + i) % REFUND_QUEUE_CAPACITY_USIZE;
            self.refunds[index] = PendingRefund::default();
        }
        self.head = ((self.head as usize + n) % REFUND_QUEUE_CAPACITY_USIZE) as u16;
        self.count -= n as u16;
        n
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, InitSpace)]
pub enum PoolType {
    RaydiumCLMM,
    RaydiumCPAMM,
    OrcaWhirlpool,
    Meteora,
    JupiterAggregator,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub enum TransactionType {
    DexBuy,
    DexSell,
    RegularTransfer,
}

// Sell Fee Debt Account - tracks outstanding 0.5% KOTS debt from DEX sells
// Per-user PDA for permissionless collection with keeper incentives
#[cfg(feature = "sol-fees")]
#[account]
#[derive(InitSpace)]
pub struct SellFeeDebt {
    pub owner: Pubkey,           // User who owes the debt
    pub debt_amount: u64,        // Outstanding KOTS debt (0.5% of total sell volume)
    pub last_updated_slot: u64,  // Last slot when debt was updated
    pub total_sells: u64,        // Lifetime sell count for analytics
}

// ============================================================================
// ERROR CODES
// ============================================================================

#[error_code]
pub enum ErrorCode {
    #[msg("Too many wallets in whitelist")]
    TooManyWallets,
    
    #[msg("Wallet already whitelisted")]
    AlreadyWhitelisted,
    
    #[msg("Too many pools registered")]
    TooManyPools,
    
    #[msg("Invalid instruction")]
    InvalidInstruction,
    
    #[msg("Insufficient SOL balance to pay fee")]
    InsufficientSolForFee,
    
    #[msg("Invalid spot price (must be > 0)")]
    InvalidSpotPrice,
    
    #[msg("Spot price is stale (update via keeper script)")]
    StaleSpotPrice,
    
    #[msg("No debt to collect (debt amount is zero)")]
    NoDebtToCollect,
    
    #[msg("No fees available to distribute")]
    NoFeesToDistribute,
    
    #[msg("DEX swap detection failed")]
    DexSwapDetectionFailed,
    
    #[msg("Invalid fee rate (must be 0 < BPS ≤ 1000)")]
    InvalidFeeRate,
    
    #[msg("No change detected in configuration")]
    NoChangeDetected,
    
    #[msg("Invalid refund queue account (doesn't match fee_config.refund_queue)")]
    InvalidRefundQueue,
    
    // O32: deliberately does NOT name a byte count. The previous text said
    // "49KB for 1024 entries" and would have gone on saying it after the
    // capacity changed — an error message that misdirects is worse than a
    // vague one. The authoritative figure is REFUND_QUEUE_SIZE, and
    // initialize_refund_queue logs it on the success path.
    #[msg("Invalid refund queue size (must equal REFUND_QUEUE_SIZE for REFUND_QUEUE_CAPACITY entries)")]
    InvalidRefundQueueSize,
    
    #[msg("Insufficient lamports for rent exemption")]
    InsufficientRentFunding,
    
    #[msg("Invalid instructions sysvar")]
    InvalidInstructionsSysvar,
    
    #[msg("Unknown DEX program in transaction")]
    UnknownDexProgram,
    
    #[msg("Invalid transaction structure for fee detection")]
    InvalidTransactionStructure,
    
    #[msg("Fee payer account is not a signer")]
    FeePayerNotSigner,
    
    #[msg("Unauthorized - only program authority can call this")]
    Unauthorized,
    
    #[msg("Invalid batch size (must be 1-100)")]
    InvalidBatchSize,
    
    #[msg("Account is owned by wrong program")]
    AccountOwnedByWrongProgram,
    
    #[msg("Invalid jackpot wallet address")]
    InvalidJackpotWallet,
    
    #[msg("Invalid incoming wallet address")]
    InvalidIncomingWallet,
    
    #[msg("Invalid treasury wallet address")]
    InvalidTreasuryWallet,
    
    #[msg("Invalid admin wallet address")]
    InvalidAdminWallet,
    
    #[msg("Invalid dev wallet address")]
    InvalidDevWallet,
    
    #[msg("Invalid burn wallet address")]
    InvalidBurnWallet,
    
    #[msg("Invalid PDA derivation")]
    InvalidPDA,

    // ═══ Anti-whale (added 2026-07-21 with the MAX_HOLD_BPS gate) ═══
    #[msg("Transfer would exceed the 3%-of-supply ownership cap")]
    WhaleLimitExceeded,

    #[msg("Arithmetic overflow in cap computation")]
    MathOverflow,

    // APPENDED 2026-07-29 (O35), deliberately LAST. Anchor assigns error codes by
    // declaration order from 6000, so inserting anywhere else renumbers every
    // variant after it and silently breaks any client matching on the number.
    // This is 6031.
    #[msg("Refund queue is full — run process_refunds_bulk; the refund cannot be paid in-band")]
    RefundQueueFull,

}
