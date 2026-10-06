// kots-contracts/tests/refund_queue_capacity.rs
//! O32 regression suite — 2026-07-29.
//!
//! Two things are under test here, and they fail for different reasons.
//!
//! **1. The heap budget.** `ExtraAccountMetaList::add_to_cpi_instruction` copies
//! the FULL DATA of every account in the meta list onto the CPI caller's heap.
//! At the original capacity of 1024 the refund queue alone was 49,164 bytes
//! against a 32 KB default BPF heap, so any DEX router swapping KOTS ran out of
//! memory inside the router and reverted the swap. The hook lab never caught it
//! because a DIRECT transfer does not run the CPI resolver at all. There is a
//! `const _: () = assert!(…)` in the program that enforces this at compile time;
//! the tests here restate it independently — recomputing the figure from the
//! account types rather than re-reading the program's own constant — so that
//! deleting the const assert does not silently delete the check.
//!
//! **2. The ring buffer.** Shrinking the capacity meant replacing four inline
//! `% 1024` literals. Miss one and the ring indexes past the end of a shorter
//! array: a panic inside a transfer hook, so every transfer of the mint reverts,
//! and only from the moment the queue first wraps — long after any smoke test.
//! These tests drive `PendingRefundQueue`'s real methods, which is why those
//! methods were extracted from the instruction handlers in the same change. A
//! test that re-implements the arithmetic it is checking agrees with the author
//! and disagrees with the chain.
//!
//! Runs on the host. No validator, no `BPF_OUT_DIR`, no compiled `.so` — unlike
//! every other suite in this crate. `cargo test -p kots-comprehensive-tests --lib
//! refund_queue` is enough.

#![cfg(not(any(feature = "devnet-network", feature = "mainnet-network")))]

use anchor_lang::prelude::Pubkey;
use anchor_lang::Space;
use bytemuck::Zeroable;

use transfer_hook::{
    FeeAuthority, FeeConfig, PendingRefund, PendingRefundQueue, PoolRegistry, WhitelistRegistry,
    META_LIST_COPY_BYTES, META_LIST_COPY_CEILING, REFUND_QUEUE_CAPACITY,
    REFUND_QUEUE_CAPACITY_USIZE, REFUND_QUEUE_SIZE, REFUND_QUEUE_SIZE_USIZE,
};

/// The heap a Solana program gets when nobody asked for more. A DEX router does
/// not request a heap frame for an arbitrary token — only the transaction can,
/// and the router is what builds the transaction. So this, not 256 KB, is the
/// number KOTS has to live inside.
const DEFAULT_BPF_HEAP_BYTES: usize = 32 * 1024;

/// The whole default heap — a NECESSARY condition, not a sufficient one.
///
/// This was heap/2, a fraction chosen because it felt conservative. It was not:
/// the build passed a configuration modelling 13,227 B that OOMs on chain. Then
/// 13,000, fitted to pre-split four-leg measurements, which the split obsoleted.
///
/// Bytes are not the discriminator — capacities 824..6,200 B all failed while
/// removing the queue entirely succeeded. What is certainly true is that a copy
/// larger than the whole heap can never work, so that is what is asserted. The
/// real ceiling is empirical and comes from ops/o32-bisect.sh.
const COPY_CEILING: usize = DEFAULT_BPF_HEAP_BYTES;

/// Everything in `extra_account_metas` that is NOT the refund queue: the
/// Instructions sysvar (transaction-dependent — the largest unknown here), five
/// distribution wallets, three program accounts, and the TLV meta list itself.
/// Measured at ~1,666 bytes on 2026-07-28; charged at ~2.5x that.
const NON_CONST_ALLOWANCE: usize = 4_096;

const PENDING_REFUND_BYTES: usize = 48; // Pubkey + u64 + u64

/// What the resolver would copy onto a caller's heap at a given queue capacity.
/// Deliberately rebuilt from the account types, so this does not merely re-read
/// the constant the program computed.
fn meta_list_copy_bytes_at_capacity(capacity: usize) -> usize {
    let queue_bytes = 8      // Anchor discriminator
        + 32                 // authority
        + 2 + 2 + 2 + 2      // head, tail, count, padding
        + 8                  // last_updated
        + capacity * PENDING_REFUND_BYTES;

    queue_bytes
        + (8 + WhitelistRegistry::INIT_SPACE)
        + (8 + PoolRegistry::INIT_SPACE)
        + (8 + FeeConfig::INIT_SPACE)
        + (8 + FeeAuthority::INIT_SPACE)
        + NON_CONST_ALLOWANCE
}

fn owner(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn empty_queue() -> PendingRefundQueue {
    PendingRefundQueue::zeroed()
}

/// Invariants that must hold after every single operation, at every capacity.
/// Called from inside the loops rather than only at the end, because a ring bug
/// that self-corrects by the final iteration is still a ring bug.
fn assert_pointers_in_range(q: &PendingRefundQueue, context: &str) {
    assert!(
        (q.head as usize) < REFUND_QUEUE_CAPACITY_USIZE,
        "{context}: head {} escaped the array (capacity {})",
        q.head,
        REFUND_QUEUE_CAPACITY_USIZE
    );
    assert!(
        (q.tail as usize) < REFUND_QUEUE_CAPACITY_USIZE,
        "{context}: tail {} escaped the array (capacity {})",
        q.tail,
        REFUND_QUEUE_CAPACITY_USIZE
    );
    assert!(
        (q.count as usize) <= REFUND_QUEUE_CAPACITY_USIZE,
        "{context}: count {} exceeds capacity {}",
        q.count,
        REFUND_QUEUE_CAPACITY_USIZE
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. The O32 invariant
// ═══════════════════════════════════════════════════════════════════════════

/// The exported size must describe the struct that actually exists. This is the
/// one that catches a future field being added to `PendingRefundQueue`: the
/// account is allocated CLIENT-SIDE and `initialize_refund_queue` requires exact
/// equality, so a layout change that nobody propagates does not fail at compile
/// time or in CI — it fails at the ceremony, once, on mainnet.
#[test]
fn refund_queue_size_matches_the_real_struct_layout() {
    assert_eq!(
        REFUND_QUEUE_SIZE_USIZE,
        8 + std::mem::size_of::<PendingRefundQueue>(),
        "REFUND_QUEUE_SIZE_USIZE ({}) no longer matches the real layout ({} + 8 discriminator). \
         A field was added, removed or reordered.",
        REFUND_QUEUE_SIZE_USIZE,
        std::mem::size_of::<PendingRefundQueue>()
    );

    assert_eq!(
        std::mem::size_of::<PendingRefund>(),
        PENDING_REFUND_BYTES,
        "PendingRefund is no longer 48 bytes, so every size calculation in this \
         file and in the four scripts that allocate the queue is now wrong."
    );
}

/// The core of O32. If this fails, KOTS is untradeable on every DEX.
#[test]
fn meta_list_copy_fits_a_routers_default_heap() {
    let copied = meta_list_copy_bytes_at_capacity(REFUND_QUEUE_CAPACITY_USIZE);

    println!(
        "O32: capacity {REFUND_QUEUE_CAPACITY_USIZE} → queue {REFUND_QUEUE_SIZE_USIZE} B, \
         meta-list copy ≈ {copied} B, ceiling {COPY_CEILING} B, \
         default heap {DEFAULT_BPF_HEAP_BYTES} B"
    );

    assert!(
        copied <= COPY_CEILING,
        "O32 REGRESSION: resolving this mint's hook copies ~{copied} bytes onto the \
         CPI caller's heap, over the {COPY_CEILING}-byte ceiling. A DEX router has \
         {DEFAULT_BPF_HEAP_BYTES} bytes and its own allocations to make in them. \
         Every swap of this mint will revert with an out-of-memory panic inside the \
         router — which will read as a Meteora/Jupiter fault, not as ours. \
         Shrink REFUND_QUEUE_CAPACITY_USIZE or remove an account from \
         initialize_extra_account_meta_list_impl. Do not raise the ceiling."
    );
}

/// Documents the defect rather than only its absence. Anyone who later raises the
/// capacity back toward 1024 "for headroom" gets a failing test that explains what
/// the headroom costs.
#[test]
fn the_original_1024_capacity_would_have_exceeded_the_whole_heap() {
    let old = meta_list_copy_bytes_at_capacity(1024);

    assert!(
        old > DEFAULT_BPF_HEAP_BYTES,
        "The pre-O32 configuration is expected to overflow a default heap \
         ({old} B vs {DEFAULT_BPF_HEAP_BYTES} B). If it no longer does, the meta \
         list or the queue struct changed and this test's premise needs revisiting."
    );

    println!(
        "O32 (historical): capacity 1024 → ≈{old} B copied, {:.2}x the default \
         {DEFAULT_BPF_HEAP_BYTES} B heap",
        old as f64 / DEFAULT_BPF_HEAP_BYTES as f64
    );
}

/// Not an assertion about correctness — a printed number for whoever next argues
/// about the capacity, so the argument starts from the budget rather than from
/// taste. Gate A A5 measures the real ceiling; this is the static bound.
#[test]
fn report_the_largest_capacity_that_still_fits() {
    let fixed = meta_list_copy_bytes_at_capacity(0);
    let max = COPY_CEILING.saturating_sub(fixed) / PENDING_REFUND_BYTES;

    println!(
        "O32 budget: fixed cost ≈{fixed} B, so ≤{max} refund slots fit under the \
         {COPY_CEILING} B ceiling. Current capacity is {REFUND_QUEUE_CAPACITY_USIZE}."
    );

    assert!(
        REFUND_QUEUE_CAPACITY_USIZE <= max,
        "capacity {REFUND_QUEUE_CAPACITY_USIZE} exceeds the {max} that fit the budget"
    );
}

/// The queue account is created client-side by TypeScript, and the on-chain check
/// is exact equality. Four scripts currently hardcode THREE different sizes for
/// it — 50176, 49208 and 49152 — so at least two of them could only ever have
/// failed. This test exists to make a capacity change impossible to land without
/// being told which files it breaks.
#[test]
fn queue_size_is_pinned_so_client_drift_is_caught_here_not_at_the_ceremony() {
    // Derived, not pinned to a literal: capacity is selectable for the O32
    // bisection (`queue-cap-*`), so a hard 6,200 would fire on every sweep run
    // and train whoever is bisecting to ignore this test. The property that
    // matters is unchanged — size and capacity must agree, because the account
    // is allocated CLIENT-SIDE and initialize_refund_queue checks exact
    // equality.
    assert_eq!(
        REFUND_QUEUE_SIZE_USIZE,
        56 + REFUND_QUEUE_CAPACITY_USIZE * PENDING_REFUND_BYTES,
        "REFUND_QUEUE_SIZE ({REFUND_QUEUE_SIZE_USIZE}) does not match capacity \
         {REFUND_QUEUE_CAPACITY_USIZE}. Changing the capacity is a breaking change \
         for every client that ALLOCATES this account. Update, in the same commit: \
         scripts/refund-queue-size.ts (which the other four scripts import), then \
         re-run `npx tsc --noEmit`."
    );

    println!(
        "queue geometry: capacity {REFUND_QUEUE_CAPACITY_USIZE}, \
         REFUND_QUEUE_SIZE {REFUND_QUEUE_SIZE_USIZE} B"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. The ring buffer — driving the program's own methods
// ═══════════════════════════════════════════════════════════════════════════

/// The `u32` constants are what the IDL and every TypeScript client see; the
/// `_USIZE` aliases are what the array length and the ring arithmetic use. They
/// are the same number in two types, and nothing but this test and a const
/// assert keeps them that way.
///
/// The `u32` form exists because `#[constant]` cannot export a `usize` — Anchor
/// has no IDL representation for a platform-dependent width, so the macro fails
/// with `no associated function or constant named 'get_full_path'`. That failure
/// surfaces ONLY under `anchor idl build`; `cargo check` and `cargo test` both
/// pass without it. (2026-07-29.)
#[test]
fn exported_u32_constants_agree_with_the_internal_usize_views() {
    assert_eq!(REFUND_QUEUE_CAPACITY as usize, REFUND_QUEUE_CAPACITY_USIZE);
    assert_eq!(REFUND_QUEUE_SIZE as usize, REFUND_QUEUE_SIZE_USIZE);
}

/// This file computes the heap budget from the account types directly, rather
/// than reading the program's own figure — so that deleting the const assert
/// does not silently delete the check. That independence is only worth anything
/// if the two are then compared. If they diverge, one of the two models of the
/// meta list is wrong and neither can be trusted until it is settled.
#[test]
fn independent_budget_agrees_with_the_programs_own_figure() {
    let mine = meta_list_copy_bytes_at_capacity(REFUND_QUEUE_CAPACITY_USIZE);

    assert_eq!(
        mine, META_LIST_COPY_BYTES,
        "This suite computes the meta-list copy as {mine} B; the program computes \
         it as {META_LIST_COPY_BYTES} B. One of the two account lists is out of \
         date — most likely an entry was added to or removed from \
         initialize_extra_account_meta_list_impl and only one side was updated."
    );

    assert_eq!(
        COPY_CEILING, META_LIST_COPY_CEILING,
        "The ceiling this suite enforces has drifted from the one the program \
         enforces at compile time."
    );
}

#[test]
fn push_back_then_pop_front_preserves_fifo_order() {
    let mut q = empty_queue();

    for i in 0..10u8 {
        assert!(q.push_back(owner(i + 1), 1_000 + i as u64, 42), "push {i}");
        assert_pointers_in_range(&q, "during fill");
    }
    assert_eq!(q.count, 10);

    let seen = q.peek_recipients(10);
    let expected: Vec<Pubkey> = (0..10u8).map(|i| owner(i + 1)).collect();
    assert_eq!(seen, expected, "peek must return oldest-first");

    assert_eq!(q.pop_front(4), 4);
    assert_eq!(q.count, 6);
    assert_eq!(
        q.peek_recipients(6),
        (4..10u8).map(|i| owner(i + 1)).collect::<Vec<_>>(),
        "after draining 4, the head must sit on the 5th entry"
    );
}

/// The test the refactor exists for. Four `% 1024` literals became one constant;
/// if any index had been missed the ring would write past the end of a 128-slot
/// array. Drive it through several full wraps rather than one.
#[test]
fn ring_survives_repeated_wraps() {
    let mut q = empty_queue();
    let laps = 3;
    let total = REFUND_QUEUE_CAPACITY_USIZE * laps;

    // Interleave so head and tail chase each other around the array many times,
    // rather than filling and emptying in phase.
    let batch = 7;
    let mut next_pushed = 0usize;
    let mut next_popped = 0usize;

    while next_pushed < total {
        for _ in 0..batch {
            if next_pushed == total {
                break;
            }
            let ok = q.push_back(owner((next_pushed % 250) as u8 + 1), next_pushed as u64, 0);
            assert!(ok, "unexpected full at {next_pushed} (count {})", q.count);
            next_pushed += 1;
            assert_pointers_in_range(&q, "mid-wrap push");
        }

        // Drain a slightly smaller batch, so the ring stays partly occupied and
        // head/tail chase each other instead of resetting to 0 together. Net
        // growth is 1 per round, which cannot reach capacity within `laps`.
        let want = (batch - 1).min(q.count as usize);
        let got = q.peek_recipients(want);
        let expect: Vec<Pubkey> = (next_popped..next_popped + want)
            .map(|i| owner((i % 250) as u8 + 1))
            .collect();
        assert_eq!(got, expect, "FIFO broken around index {next_popped}");

        assert_eq!(q.pop_front(want), want);
        next_popped += want;
        assert_pointers_in_range(&q, "mid-wrap pop");
    }

    assert!(
        next_popped > REFUND_QUEUE_CAPACITY_USIZE * 2,
        "test did not actually wrap the ring (only {next_popped} entries cycled)"
    );
}

/// The precondition for Gate A B8. At capacity 1024 this branch was unreachable
/// in practice; at 128 it is not, and on the chain it performs an `invoke_signed`
/// back into Token-2022 from inside the hook's execute path. Here we only prove
/// the boundary is where we think it is and that a rejected push leaves no trace.
#[test]
fn push_back_refuses_when_full_and_leaves_the_queue_untouched() {
    let mut q = empty_queue();

    for i in 0..REFUND_QUEUE_CAPACITY_USIZE {
        assert!(q.push_back(owner(1), i as u64, 0), "push {i} of capacity");
    }

    assert!(q.is_full());
    assert_eq!(q.count as usize, REFUND_QUEUE_CAPACITY_USIZE);

    let (head, tail, count) = (q.head, q.tail, q.count);
    assert!(
        !q.push_back(owner(99), 999, 0),
        "push_back must refuse at capacity — the caller relies on the false to \
         take the immediate-CPI refund branch"
    );
    assert_eq!(
        (q.head, q.tail, q.count),
        (head, tail, count),
        "a refused push must not mutate the ring"
    );

    // And it recovers: one drain reopens exactly one slot.
    assert_eq!(q.pop_front(1), 1);
    assert!(!q.is_full());
    assert!(q.push_back(owner(99), 999, 0));
    assert!(q.is_full());
}

/// A drained slot must not be re-readable. If a cleared entry could still be
/// returned by `peek_recipients`, the keeper would refund the same wallet twice
/// and `process_refunds_bulk`'s exact-count check on `remaining_accounts` would
/// not catch it — the counts would agree.
#[test]
fn drained_slots_are_cleared_and_never_resurface() {
    let mut q = empty_queue();
    for i in 0..5u8 {
        q.push_back(owner(i + 1), 100, 0);
    }
    q.pop_front(5);

    assert_eq!(q.count, 0);
    assert!(q.is_empty());
    assert!(
        q.peek_recipients(5).is_empty(),
        "peek must be bounded by count, not by what is still sitting in the array"
    );
    for slot in q.refunds.iter().take(5) {
        assert_eq!(slot.recipient, Pubkey::default(), "slot not cleared");
        assert_eq!(slot.amount, 0);
    }
}

/// `process_refunds_bulk` passes a caller-supplied `batch_size` (validated only
/// as 1..=100) straight into the walk. It must not be able to advance the head
/// past the live region and strand entries that were never refunded.
#[test]
fn pop_front_saturates_at_count() {
    let mut q = empty_queue();
    for i in 0..3u8 {
        q.push_back(owner(i + 1), 100, 0);
    }

    assert_eq!(q.pop_front(100), 3, "must drain only what is there");
    assert_eq!(q.count, 0);
    assert_pointers_in_range(&q, "after oversized drain");

    assert_eq!(q.pop_front(50), 0, "draining an empty ring is a no-op");
    assert_eq!(q.head, 3, "head must not run away on an empty drain");
}

#[test]
fn peek_is_bounded_by_count_not_by_the_argument() {
    let mut q = empty_queue();
    q.push_back(owner(1), 100, 0);
    q.push_back(owner(2), 100, 0);

    assert_eq!(q.peek_recipients(100).len(), 2);
    assert_eq!(q.peek_recipients(1).len(), 1);
    assert_eq!(q.peek_recipients(0).len(), 0);
}
