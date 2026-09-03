//! Observable clone-cost benchmark for speculative `AppState` children.
//!
//! Run with:
//!
//!     cargo bench --locked --bench state_cow
//!
//! This is intentionally a small text benchmark.  It reports allocator
//! deltas for retained sibling states instead of asserting an absolute
//! performance target, so results remain useful across machines and COW
//! implementation stages.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use hyperlicked::app::{AppState, OrderType, Side, Transaction};

const CHILD_COUNTS: &[usize] = &[1, 4, 8, 16];
const ACCOUNT_COUNT: usize = 2_000;
const ORDER_COUNT: usize = 512;
const MEMPOOL_COUNT: usize = 1_024;
const ACCOUNT_BALANCE: i64 = 1_000_000_000_000;
const ORDER_SIZE: i64 = 1_000_000;
const MARK_PRICE: i64 = 5_000_000;

/// Allocator counters are process-local to this benchmark binary.  They
/// intentionally measure all allocations made while the retained children
/// are built, including map/vector bookkeeping.
struct CountingAllocator;

static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_LIVE_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct AllocationSnapshot {
    allocated_bytes: u64,
    allocation_count: u64,
    live_bytes: u64,
    peak_live_bytes: u64,
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() {
            record_alloc(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc_zeroed(layout);
        if !pointer.is_null() {
            record_alloc(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = System.realloc(pointer, layout, new_size);
        if !new_pointer.is_null() {
            record_realloc(layout.size(), new_size);
        }
        new_pointer
    }
}

fn record_alloc(bytes: usize) {
    ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
    let live = LIVE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
    update_peak(live);
}

fn record_realloc(old_bytes: usize, new_bytes: usize) {
    let old_bytes = old_bytes as u64;
    let new_bytes = new_bytes as u64;
    if new_bytes > old_bytes {
        ALLOCATED_BYTES.fetch_add(new_bytes - old_bytes, Ordering::Relaxed);
        let live = LIVE_BYTES.fetch_add(new_bytes - old_bytes, Ordering::Relaxed)
            + (new_bytes - old_bytes);
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        update_peak(live);
    } else {
        LIVE_BYTES.fetch_sub(old_bytes - new_bytes, Ordering::Relaxed);
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

fn update_peak(live: u64) {
    let mut peak = PEAK_LIVE_BYTES.load(Ordering::Relaxed);
    while live > peak {
        match PEAK_LIVE_BYTES.compare_exchange_weak(
            peak,
            live,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }
}

fn allocation_snapshot() -> AllocationSnapshot {
    AllocationSnapshot {
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        allocation_count: ALLOCATION_COUNT.load(Ordering::Relaxed),
        live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        peak_live_bytes: PEAK_LIVE_BYTES.load(Ordering::Relaxed),
    }
}

fn reset_peak_to_current_live() {
    PEAK_LIVE_BYTES.store(LIVE_BYTES.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn seeded_state() -> AppState {
    let mut state = AppState::new_with_chain_domain_and_dev([7u8; 32], true);

    // Accounts are the primary workload: many independent account records
    // make full-map cloning visible while retaining deterministic setup.
    for index in 0..ACCOUNT_COUNT {
        let address = format!("bench-account-{index:05}");
        state
            .accounts_mut()
            .deposit(&address, ACCOUNT_BALANCE)
            .expect("benchmark account deposit is valid");
    }

    // Resting bids populate BTreeMap price levels and the order indexes.  A
    // distinct trader per order stays below the per-trader open-order limit.
    for index in 0..ORDER_COUNT {
        let trader = format!("bench-order-{index:05}");
        state
            .accounts_mut()
            .deposit(&trader, ACCOUNT_BALANCE)
            .expect("benchmark order account deposit is valid");
        state
            .execute_tx(Transaction::PlaceOrder {
                trader,
                symbol: "BTC-USDT".to_string(),
                side: Side::Bid,
                price: MARK_PRICE + index as i64,
                size: ORDER_SIZE,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .expect("benchmark resting order is valid");
    }

    // Keep all three mempool buckets represented.  The unique traders avoid
    // duplicate hashes and per-address anti-spam limits.
    for index in 0..MEMPOOL_COUNT {
        let trader = format!("bench-pending-{index:05}");
        state
            .submit_tx(Transaction::Deposit { trader, amount: 1 })
            .expect("benchmark mempool entry is valid");
    }

    state
}

fn child_consensus_mutation(child: &mut AppState, index: usize) {
    let account = format!("bench-account-{index:05}");
    child
        .accounts_mut()
        .deposit(&account, 1)
        .expect("benchmark child account mutation is valid");

    child
        .execute_tx(Transaction::PlaceOrder {
            trader: account,
            symbol: "BTC-USDT".to_string(),
            side: Side::Bid,
            price: MARK_PRICE - 1 - index as i64,
            size: ORDER_SIZE,
            order_type: OrderType::Gtc,
            reduce_only: false,
        })
        .expect("benchmark child order mutation is valid");
}

fn child_mixed_mutation(child: &mut AppState, index: usize) {
    child_consensus_mutation(child, index);
    child
        .submit_tx(Transaction::Deposit {
            trader: format!("bench-child-pending-{index:05}"),
            amount: 1,
        })
        .expect("benchmark child mempool mutation is valid");
}

fn measure_clone_only(state: &AppState, child_count: usize) -> AllocationSnapshot {
    reset_peak_to_current_live();
    let before = allocation_snapshot();
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        children.push(black_box(state.clone()));
    }
    black_box(&children);
    let after = allocation_snapshot();
    drop(children);

    AllocationSnapshot {
        allocated_bytes: after.allocated_bytes.saturating_sub(before.allocated_bytes),
        allocation_count: after
            .allocation_count
            .saturating_sub(before.allocation_count),
        live_bytes: after.live_bytes.saturating_sub(before.live_bytes),
        peak_live_bytes: after.peak_live_bytes.saturating_sub(before.live_bytes),
    }
}

fn measure_mutated_children(
    state: &AppState,
    child_count: usize,
    mutate: fn(&mut AppState, usize),
) -> (AllocationSnapshot, f64) {
    reset_peak_to_current_live();
    let before = allocation_snapshot();
    let start = Instant::now();
    let mut children = Vec::with_capacity(child_count);
    for index in 0..child_count {
        let mut child = black_box(state.clone());
        mutate(&mut child, index);
        children.push(child);
    }
    black_box(&children);
    let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
    let after = allocation_snapshot();
    drop(children);

    (
        AllocationSnapshot {
            allocated_bytes: after.allocated_bytes.saturating_sub(before.allocated_bytes),
            allocation_count: after
                .allocation_count
                .saturating_sub(before.allocation_count),
            live_bytes: after.live_bytes.saturating_sub(before.live_bytes),
            peak_live_bytes: after.peak_live_bytes.saturating_sub(before.live_bytes),
        },
        elapsed_ms,
    )
}

fn main() {
    let state = seeded_state();
    let setup = allocation_snapshot();

    println!("state_cow: accounts={ACCOUNT_COUNT} orders={ORDER_COUNT} mempool={MEMPOOL_COUNT}");
    println!(
        "state_cow: setup_live_bytes={} (excluded from child deltas)",
        setup.live_bytes
    );
    println!(
        "state_cow: children clone_allocated_bytes clone_live_delta peak_live_delta clone_allocations candidate_allocated_bytes candidate_live_delta candidate_peak_delta candidate_allocations candidate_ms mixed_allocated_bytes mixed_live_delta mixed_peak_delta mixed_allocations mixed_ms"
    );

    for &child_count in CHILD_COUNTS {
        let clone_only = measure_clone_only(&state, child_count);
        let (candidate, candidate_ms) =
            measure_mutated_children(&state, child_count, child_consensus_mutation);
        let (mixed, mixed_ms) = measure_mutated_children(&state, child_count, child_mixed_mutation);
        println!(
            "state_cow: {child_count} {} {} {} {} {} {} {} {} {candidate_ms:.3} {} {} {} {} {mixed_ms:.3}",
            clone_only.allocated_bytes,
            clone_only.live_bytes,
            clone_only.peak_live_bytes,
            clone_only.allocation_count,
            candidate.allocated_bytes,
            candidate.live_bytes,
            candidate.peak_live_bytes,
            candidate.allocation_count,
            mixed.allocated_bytes,
            mixed.live_bytes,
            mixed.peak_live_bytes,
            mixed.allocation_count,
        );
    }
}
