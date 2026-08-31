//! Reproducible, dependency-free baseline for Commitment v2 artifacts.
//!
//! Run with:
//!
//!     cargo bench --bench commitment_v2
//!
//! The output is intentionally a small text table rather than a statistical
//! benchmark report. It records canonical artifact bytes and elapsed wall
//! time so component-tree/dirty-subtree work has a comparable baseline.

use std::hint::black_box;
use std::time::Instant;

use hyperlicked::api::{CanonicalAppHook, SharedState};
use hyperlicked::app::{AppState, ConsensusTransaction, Transaction};
use hyperlicked::consensus::AppHook;
use hyperlicked::types::{
    Block, CommitmentV2, ConsensusContext, EventRecord, EventType, ResourceUsage,
    TransactionReceipt, TransactionType,
};

const COUNTS: &[usize] = &[100, 1_000, 5_000];
const EVENT_PAYLOAD_BYTES: usize = 64;
const REPEATS: usize = 3;
const BLOCK_TIMESTAMP: u64 = 1_000_000;

fn receipt_id(index: usize) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..8].copy_from_slice(&(index as u64).to_le_bytes());
    id
}

fn commitment_for(count: usize) -> CommitmentV2 {
    let receipts = (0..count)
        .map(|index| {
            let payload = vec![(index % 251) as u8; EVENT_PAYLOAD_BYTES];
            let event = EventRecord::new(0, EventType::FILL, payload)
                .expect("fixed benchmark event payload is valid");
            TransactionReceipt::success(
                index as u32,
                receipt_id(index),
                TransactionType::PLACE_ORDER,
                ResourceUsage::default(),
                vec![event],
            )
            .expect("fixed benchmark receipt is valid")
        })
        .collect();

    CommitmentV2::new(receipts).expect("fixed benchmark commitment is valid")
}

fn app_state_block(count: usize, context: ConsensusContext) -> Block {
    let entries: Vec<_> = (0..count)
        .map(|index| {
            ConsensusTransaction::System(Transaction::Deposit {
                trader: format!("bench-trader-{index:05}"),
                amount: 1_000_000_000,
            })
        })
        .collect();

    Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: count as u64,
        height: count as u64,
        parent: [0u8; 32],
        payload: bincode::serialize(&entries).expect("benchmark payload is serializable"),
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: BLOCK_TIMESTAMP,
        justify: None,
    }
}

fn deposit_payload(start: usize, count: usize) -> Vec<u8> {
    let entries: Vec<_> = (start..start + count)
        .map(|index| {
            ConsensusTransaction::System(Transaction::Deposit {
                trader: format!("bench-trader-{index:05}"),
                amount: 1_000_000_000,
            })
        })
        .collect();

    bincode::serialize(&entries).expect("benchmark payload is serializable")
}

fn canonical_block(
    context: ConsensusContext,
    parent: &Block,
    height: u64,
    view: u64,
    payload: Vec<u8>,
) -> Block {
    Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view,
        height,
        parent: parent.hash(),
        payload,
        proposer: [0u8; 32],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: BLOCK_TIMESTAMP + view,
        justify: None,
    }
}

fn finalize_candidate(hook: &mut CanonicalAppHook, mut block: Block) -> Block {
    block.app_hash = hook.execute(&block);
    block
}

fn commitment_baseline() {
    println!(
        "commitment_v2: one event/receipt, event_payload_bytes={EVENT_PAYLOAD_BYTES}, repeats={REPEATS}"
    );
    println!("commitment_v2: receipts events canonical_bytes encode_ms_avg root_ms_avg checksum");

    for &count in COUNTS {
        let commitment = commitment_for(count);
        let canonical_bytes = commitment
            .canonical_bytes()
            .expect("benchmark commitment encodes");
        let mut checksum = 0u64;

        let encode_start = Instant::now();
        for _ in 0..REPEATS {
            let encoded = black_box(
                commitment
                    .canonical_bytes()
                    .expect("benchmark commitment encodes"),
            );
            checksum = checksum.wrapping_add(encoded.len() as u64);
        }
        let encode_ms = encode_start.elapsed().as_secs_f64() * 1_000.0 / REPEATS as f64;

        let root_start = Instant::now();
        for _ in 0..REPEATS {
            let root = black_box(commitment.root().expect("benchmark root computes"));
            checksum ^= u64::from_le_bytes(root[..8].try_into().expect("hash has 8 bytes"));
        }
        let root_ms = root_start.elapsed().as_secs_f64() * 1_000.0 / REPEATS as f64;

        println!(
            "commitment_v2: {count} {count} {} {encode_ms:.3} {root_ms:.3} {checksum:016x}",
            canonical_bytes.len()
        );
    }
}

fn app_state_baseline() {
    let context = ConsensusContext::new(0, [7u8; 32]);
    println!(
        "app_state: deterministic System::Deposit block; execute_ms includes validation, execution, artifact collection, and final state hash"
    );
    println!(
        "app_state: accounts txs payload_bytes execute_ms legacy_state_hash_ms full_state_root_ms checksum"
    );

    for &count in COUNTS {
        let block = app_state_block(count, context);
        let mut state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);

        let execute_start = Instant::now();
        let app_hash = black_box(state.execute(&block));
        let execute_ms = execute_start.elapsed().as_secs_f64() * 1_000.0;

        let state_hash_start = Instant::now();
        let full_hash = black_box(state.compute_state_hash_full());
        let state_hash_full_ms = state_hash_start.elapsed().as_secs_f64() * 1_000.0;

        let full_state_root_start = Instant::now();
        let full_state_root = black_box(state.compute_full_state_root());
        let full_state_root_ms = full_state_root_start.elapsed().as_secs_f64() * 1_000.0;

        let app_hash_prefix =
            u64::from_le_bytes(app_hash[..8].try_into().expect("hash has 8 bytes"));
        let full_hash_prefix =
            u64::from_le_bytes(full_hash[..8].try_into().expect("hash has 8 bytes"));
        let full_state_root_prefix =
            u64::from_le_bytes(full_state_root[..8].try_into().expect("hash has 8 bytes"));
        let checksum = app_hash_prefix
            .wrapping_add(full_hash_prefix.rotate_left(17))
            .wrapping_add(full_state_root_prefix.rotate_left(29));

        println!(
            "app_state: {count} {count} {} {execute_ms:.3} {state_hash_full_ms:.3} {full_state_root_ms:.3} {checksum:016x}",
            block.payload.len()
        );
    }
}

/// Exercise the only externally visible dirty-tree path: a speculative child
/// executed by CanonicalAppHook derives from its parent's sealed tree. The
/// hook currently keeps this API crate-private, so the timed operation also
/// includes block execution/validation and candidate cloning. preflight is an
/// independent fresh-tree oracle and is deliberately outside the timing loop.
/// The block dirty mask is phase-based and deliberately conservative:
/// - metadata is marked when execute updates timestamp/view/height;
/// - accounts is marked by the deposit and by account-affecting system phases;
/// - funding is marked by premium sampling or funding-rate application when a
///   configured market qualifies;
/// - staking is marked by enabled epoch transitions and block rewards.
/// This fixture includes AppState's default BTC-USDT market. Its mark-price
/// fallback supplies a nonzero index, so process_funding samples premium and
/// marks the funding leaf even when the empty book yields zero premium. The
/// timed path therefore exercises metadata/accounts/funding/staking marks and
/// the candidate's partial component-tree derivation; preflight remains a
/// fresh full-tree oracle outside the timing loop.
fn dirty_component_baseline() {
    let context = ConsensusContext::new(0, [7u8; 32]);
    println!(
        "app_state_dirty: accounts={:?}, repeats={REPEATS}; dirty timing includes canonical execute/validation/clone",
        COUNTS
    );
    println!(
        "app_state_dirty: accounts child_txs fresh_full_root_ms dirty_candidate_execute_ms candidate_hit_ms checksum"
    );

    for &count in COUNTS {
        let genesis = Block::genesis(context);
        let base_block = canonical_block(
            context,
            &genesis,
            1,
            1,
            deposit_payload(0, count.saturating_sub(1)),
        );

        let mut hook = CanonicalAppHook::new(SharedState::new(
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true),
        ));
        let base_final = finalize_candidate(&mut hook, base_block.clone());
        let child_blocks: Vec<_> = (0..REPEATS)
            .map(|repeat| {
                canonical_block(
                    context,
                    &base_final,
                    2,
                    2 + repeat as u64,
                    deposit_payload(count, 1),
                )
            })
            .collect();

        // Prepare same-sized post-child states once, so fresh timing measures
        // only the full root derivation and not block execution.
        let mut fresh_states = Vec::with_capacity(REPEATS);
        for child in &child_blocks {
            let mut state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
            state.execute(&base_block);
            state.execute(child);
            fresh_states.push(state);
        }

        let fresh_start = Instant::now();
        let mut fresh_roots = Vec::with_capacity(REPEATS);
        for state in &fresh_states {
            fresh_roots.push(black_box(state.compute_full_state_root()));
        }
        let fresh_full_root_ms = fresh_start.elapsed().as_secs_f64() * 1_000.0 / REPEATS as f64;

        let mut dirty_checksum = 0u64;
        let mut dirty_times = Vec::with_capacity(REPEATS);
        let mut first_finalized_child = None;
        for (repeat, child) in child_blocks.into_iter().enumerate() {
            let dirty_start = Instant::now();
            let app_hash = black_box(hook.execute(&child));
            dirty_times.push(dirty_start.elapsed().as_secs_f64() * 1_000.0);

            let mut finalized_child = child;
            finalized_child.app_hash = app_hash;
            let dirty_root = hook
                .preflight_state_root(&finalized_child)
                .expect("dirty candidate must pass fresh-tree seal verification")
                .expect("canonical hook must expose a state root");
            let fresh_root = fresh_roots[repeat];
            assert_eq!(dirty_root, fresh_root);
            dirty_checksum = dirty_checksum
                .wrapping_add(u64::from_le_bytes(
                    dirty_root[..8].try_into().expect("hash has 8 bytes"),
                ))
                .wrapping_add(u64::from_le_bytes(
                    app_hash[..8].try_into().expect("hash has 8 bytes"),
                ));
            if first_finalized_child.is_none() {
                first_finalized_child = Some(finalized_child);
            }
        }
        let dirty_candidate_execute_ms = dirty_times.iter().sum::<f64>() / dirty_times.len() as f64;

        // A fully formed duplicate block takes the candidate-hit branch. This
        // is a candidate hit (not a public component-leaf cache hit); retaining
        // it in the table prevents callers from mistaking the two semantics.
        let hit_block = first_finalized_child.expect("dirty benchmark creates a candidate");
        let hit_start = Instant::now();
        let hit_hash = black_box(hook.execute(&hit_block));
        let candidate_hit_ms = hit_start.elapsed().as_secs_f64() * 1_000.0;
        assert_eq!(hit_hash, hit_block.app_hash);

        let checksum =
            fresh_roots
                .iter()
                .enumerate()
                .fold(dirty_checksum, |checksum, (index, root)| {
                    checksum.wrapping_add(
                        u64::from_le_bytes(root[..8].try_into().expect("hash has 8 bytes"))
                            .rotate_left(index as u32),
                    )
                });
        println!(
            "app_state_dirty: {count} 1 {fresh_full_root_ms:.3} {dirty_candidate_execute_ms:.3} {candidate_hit_ms:.3} {checksum:016x}"
        );
    }
}

fn main() {
    commitment_baseline();
    app_state_baseline();
    dirty_component_baseline();
}
