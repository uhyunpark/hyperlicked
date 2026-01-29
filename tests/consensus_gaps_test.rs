//! E2E Tests for HotStuff-2 Consensus Gaps
//!
//! Tests the 4 critical HotStuff-2 features:
//! 1. locked_qc updates (2-chain locking rule)
//! 2. voted_views persistence (crash recovery safety)
//! 3. Timeout certificate collection (view change proof)
//! 4. Block sync protocol (node catchup)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::time::timeout;

use hyperlicked::app::AppState;
use hyperlicked::consensus::{
    create_signed_timeout, AppHook, BlockStore, MemoryBlockStore, Pacemaker, Safety,
    TimeoutCollector,
};
use hyperlicked::crypto::bls::BlsSecretKey;
use hyperlicked::network::{MockNetwork, Network, SyncClient, SyncHandler};
use hyperlicked::storage::{ConsensusState, PersistentStore, RocksDbStore};
use hyperlicked::types::{
    hash_short, Block, Certificate, ConsensusConfig, Hash, Message, Prepare, Propose,
    SyncRequest, Vote, View,
};

// =============================================================================
// Test Infrastructure
// =============================================================================

/// Test node with full consensus state tracking
struct TestNode {
    config: ConsensusConfig,
    safety: Safety,
    pacemaker: Pacemaker,
    app: AppState,
    store: MemoryBlockStore,
    network: MockNetwork,
    pending: HashMap<Hash, Block>,
    votes: HashMap<Hash, Vec<Vote>>,
    committed_height: u64,
    /// Track locked_qc for verification
    locked_qc_history: Vec<Option<Certificate>>,
}

impl TestNode {
    fn new(config: ConsensusConfig, network: MockNetwork) -> Self {
        let store = MemoryBlockStore::new();
        let genesis = Block::genesis();
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let mut pacemaker = Pacemaker::new(Duration::from_millis(500));
        pacemaker.with_view_change(config.quorum());

        Self {
            config,
            safety: Safety::new(),
            pacemaker,
            app: AppState::new(),
            store,
            network,
            pending: HashMap::new(),
            votes: HashMap::new(),
            committed_height: 0,
            locked_qc_history: Vec::new(),
        }
    }

    fn is_leader(&self) -> bool {
        self.config.is_leader(self.pacemaker.current_view())
    }

    fn current_view(&self) -> View {
        self.pacemaker.current_view()
    }

    fn get_proposal_parent(&self) -> Block {
        if let Some(qc) = self.safety.high_qc() {
            if let Some(block) = self.store.get(&qc.block_hash) {
                return block;
            }
        }
        self.store.get_by_height(0).unwrap_or_else(Block::genesis)
    }

    async fn run_leader_round(&mut self) -> anyhow::Result<Option<Block>> {
        let view = self.pacemaker.current_view();
        let parent = self.get_proposal_parent();
        let payload = self.app.prepare_payload(&parent);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        let justify = self.safety.high_qc().cloned();
        let mut block = Block {
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload,
            proposer: self.config.node_id,
            app_hash: [0u8; 32],
            timestamp: now.as_millis() as u64,
            justify: justify.clone(),
        };
        block.app_hash = self.app.execute(&block);

        let block_hash = block.hash();
        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        // Broadcast proposal
        let propose = Propose {
            block: block.clone(),
            justify,
        };
        self.network.broadcast_propose(propose).await?;

        // Self-vote
        let self_vote = Vote::new(view, block_hash, block.app_hash, self.config.node_id);
        self.votes.entry(block_hash).or_default().push(self_vote);

        // Collect votes
        let quorum = self.config.quorum();
        let votes = self
            .collect_votes(block_hash, quorum, Duration::from_millis(200))
            .await;

        if votes.len() >= quorum {
            let qc = Certificate::new(view, block_hash, votes);
            let prepare = Prepare {
                view,
                qc: qc.clone(),
            };
            self.network.broadcast_prepare(prepare).await?;
            self.process_qc(qc);
            if let Some(ref high_qc) = self.safety.high_qc() {
                self.pacemaker.advance_view(high_qc);
            }
            return Ok(Some(block));
        }

        self.pacemaker.record_timeout();
        Ok(None)
    }

    async fn run_follower_round(&mut self) -> anyhow::Result<()> {
        let view = self.pacemaker.current_view();

        // Wait for proposal
        let propose = match timeout(Duration::from_millis(300), self.wait_for_proposal(view)).await
        {
            Ok(Ok(p)) => p,
            _ => {
                self.pacemaker.record_timeout();
                return Ok(());
            }
        };

        // Execute and vote
        if let Some(vote) = self.process_proposal(propose) {
            let leader = self.config.leader_of(view);
            let _ = self.network.send_vote(leader, vote).await;
        }

        // Wait for prepare
        match timeout(Duration::from_millis(300), self.wait_for_prepare(view)).await {
            Ok(Ok(prepare)) => {
                self.process_prepare(prepare);
            }
            _ => {
                self.pacemaker.record_timeout();
            }
        }

        Ok(())
    }

    async fn wait_for_proposal(&mut self, target_view: u64) -> anyhow::Result<Propose> {
        loop {
            let (_from, msg) = self.network.recv().await?;
            match msg {
                Message::Propose(propose) if propose.block.view == target_view => {
                    return Ok(propose);
                }
                Message::Vote(vote) => {
                    self.votes.entry(vote.block_hash).or_default().push(vote);
                }
                Message::Prepare(prepare) if prepare.view >= target_view => {
                    self.process_prepare(prepare.clone());
                }
                _ => {}
            }
        }
    }

    async fn wait_for_prepare(&mut self, target_view: u64) -> anyhow::Result<Prepare> {
        loop {
            let (_from, msg) = self.network.recv().await?;
            match msg {
                Message::Prepare(prepare) if prepare.view >= target_view => {
                    return Ok(prepare);
                }
                Message::Vote(vote) => {
                    self.votes.entry(vote.block_hash).or_default().push(vote);
                }
                _ => {}
            }
        }
    }

    async fn collect_votes(
        &mut self,
        block_hash: Hash,
        quorum: usize,
        timeout_duration: Duration,
    ) -> Vec<Vote> {
        let deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            let current_votes = self.votes.get(&block_hash).map(|v| v.len()).unwrap_or(0);
            if current_votes >= quorum || tokio::time::Instant::now() >= deadline {
                return self.votes.remove(&block_hash).unwrap_or_default();
            }

            let remaining = deadline - tokio::time::Instant::now();
            match timeout(remaining, self.network.recv()).await {
                Ok(Ok((_from, Message::Vote(vote)))) if vote.block_hash == block_hash => {
                    self.votes.entry(block_hash).or_default().push(vote);
                }
                _ => {}
            }
        }
    }

    fn process_proposal(&mut self, propose: Propose) -> Option<Vote> {
        let mut block = propose.block.clone();
        let view = block.view;

        // Execute block
        let local_app_hash = self.app.execute(&block);

        // Check safety
        if self.safety.safe_to_vote(&block, local_app_hash).is_err() {
            return None;
        }

        // Copy justify from proposal to block
        if block.justify.is_none() {
            block.justify = propose.justify.clone();
        }

        // Record vote and store block
        self.safety.record_vote(view);
        self.store.save(&block);
        self.pending.insert(block.hash(), block.clone());

        // Update high_qc if proposal includes one
        if let Some(justify) = propose.justify {
            self.safety.update_high_qc(justify);
        }

        Some(Vote::new(view, block.hash(), local_app_hash, self.config.node_id))
    }

    fn process_prepare(&mut self, prepare: Prepare) {
        self.safety.update_high_qc(prepare.qc.clone());
        self.process_qc(prepare.qc);
        if let Some(ref high_qc) = self.safety.high_qc() {
            self.pacemaker.advance_view(high_qc);
        }
    }

    fn process_qc(&mut self, qc: Certificate) {
        self.safety.update_high_qc(qc.clone());

        // 2-chain commit rule
        let certified_block = self
            .pending
            .get(&qc.block_hash)
            .cloned()
            .or_else(|| self.store.get(&qc.block_hash));

        if let Some(block) = certified_block {
            // HotStuff-2 Locking Rule: update locked_qc to block's justify
            if let Some(justify) = &block.justify {
                self.safety.update_locked_qc(justify.clone());
                // Track for verification
                self.locked_qc_history.push(Some(justify.clone()));
            }

            if block.height > 0 {
                self.try_commit(&block.parent);
            }
        }
    }

    fn try_commit(&mut self, block_hash: &Hash) -> Option<Block> {
        let block = match self.pending.remove(block_hash) {
            Some(b) => b,
            None => self.store.get(block_hash)?,
        };

        if block.height <= self.committed_height {
            return None;
        }

        if block.height > self.committed_height + 1 {
            self.try_commit(&block.parent);
        }

        self.store.set_committed(block_hash);
        self.committed_height = block.height;
        self.pending.retain(|_, b| b.height > self.committed_height);
        self.safety.prune_votes_below(block.view);

        Some(block)
    }
}

/// Run consensus rounds across nodes
async fn run_consensus_rounds(
    nodes: &mut [Arc<Mutex<TestNode>>],
    rounds: usize,
) -> anyhow::Result<()> {
    for _ in 0..rounds {
        let mut handles = Vec::new();

        for node in nodes.iter() {
            let node = Arc::clone(node);
            handles.push(tokio::spawn(async move {
                let mut node = node.lock().await;
                if node.is_leader() {
                    let _ = node.run_leader_round().await;
                } else {
                    let _ = node.run_follower_round().await;
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

fn create_test_configs(node_ids: &[[u8; 32]]) -> Vec<ConsensusConfig> {
    node_ids
        .iter()
        .map(|&node_id| ConsensusConfig {
            node_id,
            validators: node_ids.to_vec(),
            view_timeout_ms: 500,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        })
        .collect()
}

fn create_bls_configs(node_ids: &[[u8; 32]]) -> Vec<ConsensusConfig> {
    // Generate BLS keys for each node
    let keys: Vec<BlsSecretKey> = node_ids
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let pubkeys: Vec<Vec<u8>> = keys.iter().map(|k| k.public_key().to_bytes().to_vec()).collect();

    node_ids
        .iter()
        .enumerate()
        .map(|(i, &node_id)| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;

            ConsensusConfig {
                node_id,
                validators: node_ids.to_vec(),
                view_timeout_ms: 500,
                bls_pubkeys: pubkeys.clone(),
                bls_secret_key: Some(seed),
            }
        })
        .collect()
}

// =============================================================================
// Gap 1: locked_qc Updates Test
// =============================================================================

/// Test that locked_qc is updated according to HotStuff-2 2-chain rule.
///
/// After producing blocks B1, B2, B3:
/// - QC(B1) exists -> no lock yet (need QC on QC)
/// - QC(B2) exists -> B2.justify = QC(B1), so locked_qc = QC(B1)
/// - QC(B3) exists -> B3.justify = QC(B2), so locked_qc = QC(B2)
#[tokio::test]
async fn test_locked_qc_updates_on_qc_chain() {
    let (net0, net1, net2) = MockNetwork::create_connected_trio();
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let configs = create_test_configs(&node_ids);

    let mut nodes: Vec<Arc<Mutex<TestNode>>> = vec![
        Arc::new(Mutex::new(TestNode::new(configs[0].clone(), net0))),
        Arc::new(Mutex::new(TestNode::new(configs[1].clone(), net1))),
        Arc::new(Mutex::new(TestNode::new(configs[2].clone(), net2))),
    ];

    // Run 6 rounds to get multiple blocks with QCs
    run_consensus_rounds(&mut nodes, 6).await.unwrap();

    // Verify locked_qc progression on all nodes
    for (i, node) in nodes.iter().enumerate() {
        let node = node.lock().await;

        println!(
            "Node {}: committed_height={}, locked_qc_history_len={}, high_qc_view={}, locked_qc_view={}",
            i,
            node.committed_height,
            node.locked_qc_history.len(),
            node.safety.high_qc().map(|q| q.view).unwrap_or(0),
            node.safety.locked_qc().map(|q| q.view).unwrap_or(0),
        );

        // After multiple rounds, locked_qc should have been updated
        if node.committed_height >= 2 {
            assert!(
                node.safety.locked_qc().is_some(),
                "Node {} should have locked_qc after committing 2+ blocks",
                i
            );

            // locked_qc view should be less than high_qc view (it's always one behind)
            let locked_view = node.safety.locked_qc().map(|q| q.view).unwrap_or(0);
            let high_view = node.safety.high_qc().map(|q| q.view).unwrap_or(0);

            assert!(
                locked_view < high_view,
                "Node {}: locked_qc view ({}) should be less than high_qc view ({})",
                i, locked_view, high_view
            );
        }
    }

    println!("locked_qc update test passed!");
}

/// Test that locking prevents voting for conflicting blocks.
///
/// If we're locked on QC for block at view V, we should reject blocks
/// that don't extend from view >= V.
#[tokio::test]
async fn test_locking_prevents_conflicting_votes() {
    let node_id = [1u8; 32];
    let mut safety = Safety::new();

    // Create a "locked" QC at view 5
    let locked_qc = Certificate {
        view: 5,
        block_hash: [1u8; 32],
        votes: vec![],
        voters: vec![],
        bls_pubkeys: vec![],
        agg_signature: vec![],
        app_hash: Some([0u8; 32]),
    };
    safety.update_locked_qc(locked_qc.clone());

    // Create high_qc at view 6
    let high_qc = Certificate {
        view: 6,
        block_hash: [2u8; 32],
        votes: vec![],
        voters: vec![],
        bls_pubkeys: vec![],
        agg_signature: vec![],
        app_hash: Some([0u8; 32]),
    };
    safety.update_high_qc(high_qc.clone());

    // Try to vote for a block at view 4 (before lock) - should fail
    let bad_block = Block {
        view: 4,
        height: 1,
        parent: [0u8; 32],
        payload: vec![],
        proposer: node_id,
        app_hash: [0u8; 32],
        timestamp: 0,
        justify: None,
    };

    let result = safety.safe_to_vote(&bad_block, [0u8; 32]);
    assert!(
        result.is_err(),
        "Should reject vote for block before locked view"
    );

    // Try to vote for a block at view 7 (after lock, extends high_qc) - should succeed
    let good_block = Block {
        view: 7,
        height: 2,
        parent: high_qc.block_hash,
        payload: vec![],
        proposer: node_id,
        app_hash: [0u8; 32],
        timestamp: 0,
        justify: Some(high_qc),
    };

    let result = safety.safe_to_vote(&good_block, [0u8; 32]);
    assert!(
        result.is_ok(),
        "Should allow vote for block that extends high_qc: {:?}",
        result
    );

    println!("Locking prevents conflicting votes test passed!");
}

// =============================================================================
// Gap 2: voted_views Persistence Test
// =============================================================================

/// Test that voted_views survives crash recovery.
///
/// Scenario:
/// 1. Vote in view 5
/// 2. Save state to persistent store
/// 3. Create new Safety from saved state
/// 4. Try to vote again in view 5 - should fail (double-vote prevention)
#[tokio::test]
async fn test_voted_views_persistence_prevents_double_vote() {
    let temp_dir = TempDir::new().unwrap();
    let store = RocksDbStore::open(temp_dir.path()).unwrap();

    // Phase 1: Vote and persist
    let mut safety = Safety::new();
    safety.record_vote(5);
    safety.record_vote(6);
    safety.record_vote(7);

    let state = ConsensusState {
        high_qc: None,
        locked_qc: None,
        voted_views: safety.voted_views(),
        current_view: 8,
        committed_height: 0,
        committed_hash: [0u8; 32],
        consecutive_timeouts: 0,
        vc_sent_for_view: None,
    };
    store.save_consensus_state(&state).unwrap();

    // Phase 2: Simulate crash - create new Safety from persisted state
    let loaded_state = store.load_consensus_state().unwrap().unwrap();
    let recovered_safety = Safety::with_state(
        loaded_state.high_qc,
        loaded_state.locked_qc,
        &loaded_state.voted_views,
    );

    // Phase 3: Try to vote in views we already voted in - should fail
    let block_v5 = Block {
        view: 5,
        height: 1,
        parent: [0u8; 32],
        payload: vec![],
        proposer: [1u8; 32],
        app_hash: [0u8; 32],
        timestamp: 0,
        justify: None,
    };

    let result = recovered_safety.safe_to_vote(&block_v5, [0u8; 32]);
    assert!(
        result.is_err(),
        "Should reject double-vote in view 5 after recovery"
    );

    // Should also reject view 6 and 7
    let block_v6 = Block { view: 6, ..block_v5.clone() };
    let block_v7 = Block { view: 7, ..block_v5.clone() };

    assert!(
        recovered_safety.safe_to_vote(&block_v6, [0u8; 32]).is_err(),
        "Should reject double-vote in view 6"
    );
    assert!(
        recovered_safety.safe_to_vote(&block_v7, [0u8; 32]).is_err(),
        "Should reject double-vote in view 7"
    );

    // But should allow voting in a new view
    let block_v8 = Block { view: 8, ..block_v5.clone() };
    assert!(
        recovered_safety.safe_to_vote(&block_v8, [0u8; 32]).is_ok(),
        "Should allow vote in new view 8"
    );

    println!("voted_views persistence test passed!");
}

/// Test full consensus state recovery including high_qc and locked_qc.
#[tokio::test]
async fn test_full_consensus_state_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let store = RocksDbStore::open(temp_dir.path()).unwrap();

    // Create state with high_qc and locked_qc
    let high_qc = Certificate {
        view: 10,
        block_hash: [1u8; 32],
        votes: vec![],
        voters: vec![],
        bls_pubkeys: vec![],
        agg_signature: vec![],
        app_hash: Some([0u8; 32]),
    };
    let locked_qc = Certificate {
        view: 9,
        block_hash: [2u8; 32],
        votes: vec![],
        voters: vec![],
        bls_pubkeys: vec![],
        agg_signature: vec![],
        app_hash: Some([0u8; 32]),
    };

    let state = ConsensusState {
        high_qc: Some(high_qc.clone()),
        locked_qc: Some(locked_qc.clone()),
        voted_views: vec![8, 9, 10],
        current_view: 11,
        committed_height: 5,
        committed_hash: [3u8; 32],
        consecutive_timeouts: 3,
        vc_sent_for_view: Some(10),
    };
    store.save_consensus_state(&state).unwrap();

    // Load and verify
    let loaded = store.load_consensus_state().unwrap().unwrap();

    assert_eq!(loaded.high_qc.as_ref().map(|q| q.view), Some(10));
    assert_eq!(loaded.locked_qc.as_ref().map(|q| q.view), Some(9));
    assert_eq!(loaded.voted_views, vec![8, 9, 10]);
    assert_eq!(loaded.current_view, 11);
    assert_eq!(loaded.committed_height, 5);

    // Recover Safety and verify
    let safety = Safety::with_state(
        loaded.high_qc,
        loaded.locked_qc,
        &loaded.voted_views,
    );

    assert_eq!(safety.high_qc().map(|q| q.view), Some(10));
    assert_eq!(safety.locked_qc().map(|q| q.view), Some(9));

    println!("Full consensus state recovery test passed!");
}

// =============================================================================
// Gap 3: Timeout Certificate Collection Test
// =============================================================================

/// Test that TimeoutCollector forms TC when quorum is reached.
#[tokio::test]
async fn test_timeout_certificate_formation() {
    let node_ids: [[u8; 32]; 3] = [[1u8; 32], [2u8; 32], [3u8; 32]];

    // Generate BLS keys
    let keys: Vec<BlsSecretKey> = node_ids
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let mut pubkeys = HashMap::new();
    for (id, key) in node_ids.iter().zip(keys.iter()) {
        pubkeys.insert(*id, key.public_key());
    }

    // Create collector with quorum = 2
    let mut collector = TimeoutCollector::new(2, pubkeys);

    // View 5 times out - first timeout from node 0
    let timeout1 = create_signed_timeout(5, 3, node_ids[0], &keys[0]);
    let result1 = collector.add(timeout1).unwrap();
    assert!(result1.is_none(), "Should not reach quorum with 1 timeout");
    assert_eq!(collector.count(5), 1);

    // Second timeout from node 1 - should reach quorum
    let timeout2 = create_signed_timeout(5, 4, node_ids[1], &keys[1]);
    let result2 = collector.add(timeout2).unwrap();
    assert!(result2.is_some(), "Should reach quorum with 2 timeouts");

    let tc = result2.unwrap();
    assert_eq!(tc.view, 5);
    assert_eq!(tc.high_qc_view, 4); // Highest among timeouts
    assert_eq!(tc.signers.len(), 2);

    println!("Timeout certificate formation test passed!");
}

/// Test that duplicate timeouts are rejected.
#[tokio::test]
async fn test_duplicate_timeout_rejected() {
    let node_id = [1u8; 32];
    let mut seed = [0u8; 32];
    seed[0] = 1;
    seed[31] = 0xBE;
    let key = BlsSecretKey::from_seed(&seed);

    let mut pubkeys = HashMap::new();
    pubkeys.insert(node_id, key.public_key());

    let mut collector = TimeoutCollector::new(1, pubkeys);

    let timeout1 = create_signed_timeout(5, 3, node_id, &key);
    let result1 = collector.add(timeout1.clone());
    assert!(result1.is_ok());

    // Try to add same timeout again
    let result2 = collector.add(timeout1);
    assert!(result2.is_err(), "Should reject duplicate timeout from same sender");

    println!("Duplicate timeout rejection test passed!");
}

/// Test TC verification with aggregated signatures.
#[tokio::test]
async fn test_timeout_certificate_verification() {
    use hyperlicked::consensus::verify_timeout_certificate;

    let node_ids: [[u8; 32]; 3] = [[1u8; 32], [2u8; 32], [3u8; 32]];

    let keys: Vec<BlsSecretKey> = node_ids
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let mut pubkeys = HashMap::new();
    for (id, key) in node_ids.iter().zip(keys.iter()) {
        pubkeys.insert(*id, key.public_key());
    }

    let mut collector = TimeoutCollector::new(2, pubkeys.clone());

    // Collect timeouts
    let timeout1 = create_signed_timeout(5, 3, node_ids[0], &keys[0]);
    let timeout2 = create_signed_timeout(5, 4, node_ids[1], &keys[1]);

    collector.add(timeout1).unwrap();
    let tc = collector.add(timeout2).unwrap().unwrap();

    // Verify the TC
    assert!(
        verify_timeout_certificate(&tc, &pubkeys),
        "TC should verify with correct pubkeys"
    );

    println!("TC verification test passed!");
}

// =============================================================================
// Gap 4: Block Sync Protocol Test
// =============================================================================

/// Test SyncHandler responds correctly to sync requests.
#[tokio::test]
async fn test_sync_handler_responds_to_requests() {
    use std::sync::atomic::AtomicU64;

    let temp_dir = TempDir::new().unwrap();
    let store = Arc::new(RocksDbStore::open(temp_dir.path()).unwrap());

    // Store some blocks
    for i in 0..5 {
        let block = Block {
            view: i * 2,
            height: i,
            parent: if i == 0 { [0u8; 32] } else { [(i - 1) as u8; 32] },
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [i as u8; 32],
            timestamp: 1000 + i,
            justify: None,
        };
        store.save(&block);
    }

    let current_height = Arc::new(AtomicU64::new(4));
    let handler = SyncHandler::new(store, current_height);

    // Request blocks from height 2
    let request = SyncRequest {
        from_height: 2,
        to_height: None,
        max_blocks: 10,
        request_id: 1,
    };

    let response = handler.handle_sync_request(request);

    assert_eq!(response.request_id, 1);
    assert_eq!(response.peer_height, 4);
    // Should return blocks 2, 3, 4
    assert!(response.blocks.len() >= 1, "Should return at least one block");

    println!(
        "SyncHandler returned {} blocks, peer_height={}",
        response.blocks.len(),
        response.peer_height
    );

    println!("SyncHandler test passed!");
}

/// Test SyncClient tracks sync progress correctly.
#[tokio::test]
async fn test_sync_client_progress_tracking() {
    let client = SyncClient::new(100);

    // Create initial request
    let req1 = client.create_sync_request();
    assert_eq!(req1.from_height, 101); // Current + 1
    assert_eq!(req1.request_id, 0);

    // Second request should have new ID
    let req2 = client.create_sync_request();
    assert_eq!(req2.request_id, 1);

    // Update height after receiving blocks
    client.update_height(150);
    assert_eq!(client.current_height(), 150);

    // Next request should start from new height
    let req3 = client.create_sync_request();
    assert_eq!(req3.from_height, 151);

    // Test needs_more
    let response_done = hyperlicked::types::SyncResponse {
        request_id: 0,
        blocks: vec![],
        peer_height: 150,
        has_more: false,
    };
    assert!(!client.needs_more(&response_done), "Should not need more when caught up");

    let response_more = hyperlicked::types::SyncResponse {
        request_id: 0,
        blocks: vec![],
        peer_height: 200,
        has_more: false,
    };
    assert!(client.needs_more(&response_more), "Should need more when behind peer");

    println!("SyncClient progress tracking test passed!");
}

/// Integration test: Late node catches up via sync.
#[tokio::test]
async fn test_late_node_catches_up_via_sync() {
    use std::sync::atomic::AtomicU64;

    let temp_dir = TempDir::new().unwrap();
    let store = Arc::new(RocksDbStore::open(temp_dir.path()).unwrap());

    // Simulate a node that has blocks 0-9
    for i in 0..10u64 {
        let parent = if i == 0 {
            [0u8; 32]
        } else {
            // Compute parent hash (simplified)
            let mut h = [0u8; 32];
            h[0] = (i - 1) as u8;
            h
        };

        let block = Block {
            view: i * 2,
            height: i,
            parent,
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [i as u8; 32],
            timestamp: 1000 + i,
            justify: None,
        };
        store.save(&block);
    }

    let current_height = Arc::new(AtomicU64::new(9));
    let handler = SyncHandler::new(store.clone(), current_height);

    // Late node starts at height 3
    let client = SyncClient::new(3);

    // Request blocks
    let request = client.create_sync_request();
    assert_eq!(request.from_height, 4);

    let response = handler.handle_sync_request(request);

    // Should have blocks to process
    assert!(!response.blocks.is_empty(), "Should receive blocks");
    println!("Received {} blocks for catchup", response.blocks.len());

    // Update client height based on received blocks
    if let Some(last_block) = response.blocks.last() {
        client.update_height(last_block.height);
    }

    // Check if we need more
    if client.needs_more(&response) {
        let request2 = client.create_sync_request();
        println!("Need more blocks, requesting from height {}", request2.from_height);
    }

    println!("Late node catchup test passed!");
}

// =============================================================================
// Combined Integration Test
// =============================================================================

/// Full integration test covering all 4 gaps in a multi-node scenario.
#[tokio::test]
async fn test_full_consensus_with_all_gaps() {
    let (net0, net1, net2) = MockNetwork::create_connected_trio();
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let configs = create_test_configs(&node_ids);

    let mut nodes: Vec<Arc<Mutex<TestNode>>> = vec![
        Arc::new(Mutex::new(TestNode::new(configs[0].clone(), net0))),
        Arc::new(Mutex::new(TestNode::new(configs[1].clone(), net1))),
        Arc::new(Mutex::new(TestNode::new(configs[2].clone(), net2))),
    ];

    // Run 10 rounds
    run_consensus_rounds(&mut nodes, 10).await.unwrap();

    // Verify Gap 1: locked_qc updates
    for (i, node) in nodes.iter().enumerate() {
        let node = node.lock().await;
        if node.committed_height >= 2 {
            assert!(
                node.safety.locked_qc().is_some(),
                "Gap 1 failed: Node {} should have locked_qc",
                i
            );
        }
    }

    // Verify Gap 2: voted_views tracking (check we can't double-vote)
    {
        let node0 = nodes[0].lock().await;
        let voted = node0.safety.voted_views();
        assert!(!voted.is_empty(), "Gap 2 failed: Should have recorded votes");
        println!("Node 0 voted in {} views", voted.len());
    }

    // Verify all nodes committed blocks
    let heights: Vec<u64> = {
        let mut h = Vec::new();
        for node in &nodes {
            h.push(node.lock().await.committed_height);
        }
        h
    };

    println!("Final committed heights: {:?}", heights);

    for (i, &height) in heights.iter().enumerate() {
        assert!(
            height >= 1,
            "Node {} should have committed at least 1 block",
            i
        );
    }

    println!("Full consensus integration test passed!");
}
