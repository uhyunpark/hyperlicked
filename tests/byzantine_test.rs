//! Byzantine Integration Tests
//!
//! Tests Byzantine fault detection and consensus safety:
//! 1. Fabricated QC rejection — follower rejects proposals with fake BLS signatures
//! 2. Corrupted BLS aggregate rejection — tampered aggregate signatures detected
//! 3. Multi-validator determinism — all honest nodes converge to identical state
//! 4. State hash divergence — divergent node excluded, honest majority progresses

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::timeout;

use hyperlicked::app::AppState;
use hyperlicked::consensus::{AppHook, BlockStore, MemoryBlockStore, Pacemaker, Safety};
use hyperlicked::crypto::bls::{aggregate_signatures, BlsSecretKey};
use hyperlicked::network::{MockNetwork, Network};
use hyperlicked::types::{
    hash_short, Block, Certificate, ConsensusConfig, ConsensusContext, Hash, Message, Prepare,
    Propose, Vote,
};

// =============================================================================
// Test Infrastructure: BLS-enabled TestNode
// =============================================================================

/// BLS-enabled test node for Byzantine fault testing
struct BlsTestNode {
    config: ConsensusConfig,
    safety: Safety,
    pacemaker: Pacemaker,
    app: AppState,
    store: MemoryBlockStore,
    network: MockNetwork,
    pending: HashMap<Hash, Block>,
    votes: HashMap<Hash, Vec<Vote>>,
    committed_height: u64,
    bls_key: BlsSecretKey,
}

impl BlsTestNode {
    fn new(config: ConsensusConfig, network: MockNetwork, bls_key: BlsSecretKey) -> Self {
        let store = MemoryBlockStore::new();
        let context = config
            .context()
            .expect("BLS test config must have a canonical context");
        let genesis = Block::genesis(context);
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
            bls_key,
        }
    }

    fn is_leader(&self) -> bool {
        self.config.is_leader(self.pacemaker.current_view())
    }

    fn get_proposal_parent(&self) -> Block {
        if let Some(qc) = self.safety.high_qc() {
            if let Some(block) = self.store.get(&qc.block_hash) {
                return block;
            }
        }
        self.store
            .get_by_height(0)
            .unwrap_or_else(|| Block::genesis(self.config.context().expect("valid context")))
    }

    /// Run a leader round with BLS signatures
    async fn run_leader_round(&mut self) -> anyhow::Result<Option<Block>> {
        let view = self.pacemaker.current_view();
        let parent = self.get_proposal_parent();
        let payload = self.app.prepare_payload(&parent);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        let justify = self.safety.high_qc().cloned();
        let mut block = Block {
            epoch: self.config.epoch,
            committee_hash: self.config.context().expect("valid context").committee_hash,
            genesis_hash: self.config.context().expect("valid context").genesis_hash,
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload,
            proposer: self.config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: now.as_millis() as u64,
            justify: justify.clone(),
        };
        block.app_hash = self.app.execute(&block);

        let block_hash = block.hash();
        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        let propose = Propose {
            epoch: block.epoch,
            committee_hash: block.committee_hash,
            genesis_hash: block.genesis_hash,
            block: block.clone(),
            justify,
            proposer_signature: vec![],
        };
        self.network.broadcast_propose(propose).await?;

        // BLS self-vote
        let self_vote = Vote::new_bls(
            self.config.context().expect("valid context"),
            view,
            block_hash,
            block.app_hash,
            self.config.node_id,
            &self.bls_key,
        );
        self.votes.entry(block_hash).or_default().push(self_vote);

        // Collect votes
        let quorum = self.config.quorum();
        let votes = self
            .collect_votes(block_hash, quorum, Duration::from_millis(300))
            .await;

        if votes.len() >= quorum {
            // Form BLS-aggregated QC
            let sigs: Vec<_> = votes
                .iter()
                .filter_map(|v| {
                    hyperlicked::crypto::bls::BlsSignature::from_slice(&v.signature).ok()
                })
                .collect();

            if let Ok(agg_sig) = aggregate_signatures(&sigs) {
                let qc = Certificate::new_bls(
                    self.config.context().expect("valid context"),
                    view,
                    block_hash,
                    votes,
                    agg_sig.to_bytes().to_vec(),
                )
                .map_err(anyhow::Error::msg)?;
                let prepare = Prepare {
                    epoch: qc.epoch,
                    committee_hash: qc.committee_hash,
                    genesis_hash: qc.genesis_hash,
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
        }

        self.pacemaker.record_timeout();
        Ok(None)
    }

    /// Run a follower round with QC verification
    async fn run_follower_round(&mut self) -> anyhow::Result<()> {
        let view = self.pacemaker.current_view();

        let propose = match timeout(Duration::from_millis(300), self.wait_for_proposal(view)).await
        {
            Ok(Ok(p)) => p,
            _ => {
                self.pacemaker.record_timeout();
                return Ok(());
            }
        };

        if let Some(vote) = self.process_proposal(propose) {
            let leader = self.config.leader_of_active(view);
            let _ = self.network.send_vote(leader, vote).await;
        }

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
                    self.process_prepare(prepare);
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

    /// Process proposal with QC verification (mirrors production runner.rs fix)
    fn process_proposal(&mut self, propose: Propose) -> Option<Vote> {
        let block = &propose.block;
        let view = block.view;
        let height = block.height;

        // SECURITY: Verify QC before any state mutation
        if height > 1 {
            let justify = propose.justify.as_ref().or(block.justify.as_ref());

            if let Some(cert) = justify {
                // Verify QC certifies parent
                if cert.block_hash != block.parent {
                    eprintln!("Rejecting proposal: QC block_hash doesn't match parent");
                    return None;
                }

                // Verify BLS signature if present
                if cert.is_bls() {
                    if let Err(e) = cert.verify_bls() {
                        eprintln!("Rejecting proposal at view {}: invalid BLS QC: {}", view, e);
                        return None;
                    }
                }
            } else {
                eprintln!("Rejecting proposal at height {}: missing QC", height);
                return None;
            }
        }

        // Execute block
        let local_app_hash = self.app.execute(block);

        // Check safety
        if self.safety.safe_to_vote(block, local_app_hash).is_err() {
            return None;
        }

        // Store and vote
        let mut block = block.clone();
        if block.justify.is_none() {
            block.justify = propose.justify.clone();
        }
        self.safety.record_vote(view);
        self.store.save(&block);
        self.pending.insert(block.hash(), block.clone());

        if let Some(justify) = propose.justify {
            self.safety.update_high_qc(justify);
        }

        Some(Vote::new_bls(
            self.config.context().expect("valid context"),
            view,
            block.hash(),
            local_app_hash,
            self.config.node_id,
            &self.bls_key,
        ))
    }

    /// Process prepare with QC verification
    fn process_prepare(&mut self, prepare: Prepare) {
        // SECURITY: Verify BLS QC in prepare before accepting
        if prepare.qc.is_bls() {
            if let Err(e) = prepare.qc.verify_bls() {
                eprintln!(
                    "Rejecting prepare at view {}: invalid QC: {}",
                    prepare.view, e
                );
                return;
            }
        }

        self.safety.update_high_qc(prepare.qc.clone());
        self.process_qc(prepare.qc);
        if let Some(ref high_qc) = self.safety.high_qc() {
            self.pacemaker.advance_view(high_qc);
        }
    }

    fn process_qc(&mut self, qc: Certificate) {
        self.safety.update_high_qc(qc.clone());

        let certified_block = self
            .pending
            .get(&qc.block_hash)
            .cloned()
            .or_else(|| self.store.get(&qc.block_hash));

        if let Some(block) = certified_block {
            if let Some(justify) = &block.justify {
                self.safety.update_locked_qc(justify.clone());
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

// =============================================================================
// Helper functions
// =============================================================================

fn create_bls_keys(count: usize) -> Vec<BlsSecretKey> {
    (0..count)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;
            BlsSecretKey::from_seed(&seed)
        })
        .collect()
}

fn create_bls_configs(node_ids: &[[u8; 32]], keys: &[BlsSecretKey]) -> Vec<ConsensusConfig> {
    let pubkeys: Vec<Vec<u8>> = keys
        .iter()
        .map(|k| k.public_key().to_bytes().to_vec())
        .collect();

    node_ids
        .iter()
        .enumerate()
        .map(|(i, &node_id)| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;

            ConsensusConfig {
                epoch: 0,
                genesis_hash: [0u8; 32],
                node_id,
                validators: node_ids.to_vec(),
                voting_powers: vec![1; node_ids.len()],
                view_timeout_ms: 500,
                bls_pubkeys: pubkeys.clone(),
                bls_secret_key: Some(seed),
            }
        })
        .collect()
}

/// Run BLS consensus rounds
async fn run_bls_rounds(
    nodes: &mut [Arc<Mutex<BlsTestNode>>],
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

// =============================================================================
// Test 1: Byzantine leader fabricated QC rejected
// =============================================================================

/// A Byzantine leader crafts a proposal with a fabricated QC (random aggregate
/// signature but valid structure). Honest followers verify BLS and reject it.
#[tokio::test]
async fn test_byzantine_leader_fabricated_qc_rejected() {
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let keys = create_bls_keys(3);
    let configs = create_bls_configs(&node_ids, &keys);

    let (net0, net1, net2) = MockNetwork::create_connected_trio();

    let mut nodes: Vec<Arc<Mutex<BlsTestNode>>> = vec![
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[0].clone(),
            net0,
            keys[0].clone(),
        ))),
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[1].clone(),
            net1,
            keys[1].clone(),
        ))),
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[2].clone(),
            net2,
            keys[2].clone(),
        ))),
    ];

    // Run 3 honest rounds to establish state
    run_bls_rounds(&mut nodes, 3).await.unwrap();

    // Record follower high_qc views before Byzantine attack
    let pre_attack_high_qc_view_1 = {
        let node = nodes[1].lock().await;
        node.safety.high_qc().map(|q| q.view).unwrap_or(0)
    };
    let pre_attack_high_qc_view_2 = {
        let node = nodes[2].lock().await;
        node.safety.high_qc().map(|q| q.view).unwrap_or(0)
    };

    // Now Byzantine leader (node 0) crafts a proposal with fabricated QC
    let fabricated_qc = {
        let node = nodes[0].lock().await;
        let parent = node.get_proposal_parent();
        let parent_hash = parent.hash();
        let context = node.config.context().expect("valid context");

        // Fabricated QC: valid structure, but random 96-byte signature
        Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 99,
            block_hash: parent_hash,
            app_hash: Some([0u8; 32]),
            votes: vec![],
            voters: vec![node_ids[0], node_ids[1]],
            bls_pubkeys: vec![
                keys[0].public_key().to_bytes().to_vec(),
                keys[1].public_key().to_bytes().to_vec(),
            ],
            // Random bytes — not a valid BLS aggregate signature
            agg_signature: vec![0xDE; 96],
        }
    };

    // Verify the fabricated QC fails BLS verification
    let verify_result = fabricated_qc.verify_bls();
    assert!(
        verify_result.is_err(),
        "Fabricated QC should fail BLS verification, got: {:?}",
        verify_result
    );

    // Craft a proposal containing the fabricated QC
    let byzantine_propose = {
        let node = nodes[0].lock().await;
        let parent = node.get_proposal_parent();
        let context = node.config.context().expect("valid context");
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 100,
            height: parent.height + 1,
            parent: parent.hash(),
            payload: vec![],
            proposer: node_ids[0],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 999999,
            justify: Some(fabricated_qc.clone()),
        };

        Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block,
            justify: Some(fabricated_qc),
            proposer_signature: vec![],
        }
    };

    // Followers should reject the proposal due to invalid QC
    {
        let mut follower1 = nodes[1].lock().await;
        let vote = follower1.process_proposal(byzantine_propose.clone());
        assert!(
            vote.is_none(),
            "Follower 1 should reject proposal with fabricated QC"
        );
    }
    {
        let mut follower2 = nodes[2].lock().await;
        let vote = follower2.process_proposal(byzantine_propose);
        assert!(
            vote.is_none(),
            "Follower 2 should reject proposal with fabricated QC"
        );
    }

    // Verify followers' high_qc was NOT updated to the fabricated QC
    let post_attack_high_qc_view_1 = {
        let node = nodes[1].lock().await;
        node.safety.high_qc().map(|q| q.view).unwrap_or(0)
    };
    let post_attack_high_qc_view_2 = {
        let node = nodes[2].lock().await;
        node.safety.high_qc().map(|q| q.view).unwrap_or(0)
    };

    assert_eq!(
        pre_attack_high_qc_view_1, post_attack_high_qc_view_1,
        "Follower 1 high_qc should not be updated by fabricated QC"
    );
    assert_eq!(
        pre_attack_high_qc_view_2, post_attack_high_qc_view_2,
        "Follower 2 high_qc should not be updated by fabricated QC"
    );

    println!("Byzantine fabricated QC rejection test passed!");
}

// =============================================================================
// Test 2: Corrupted BLS aggregate signature rejected
// =============================================================================

/// Get a valid QC from round 1, corrupt 1 byte of agg_signature, verify it fails.
#[tokio::test]
async fn test_byzantine_leader_corrupted_bls_aggregate_rejected() {
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let keys = create_bls_keys(3);

    // Create a valid BLS-aggregated QC
    let block_hash = [0xABu8; 32];
    let app_hash = [0u8; 32];
    let view = 1u64;
    let context = ConsensusContext::new(0, [9u8; 32]);

    let votes: Vec<Vote> = keys
        .iter()
        .zip(node_ids.iter())
        .map(|(key, &node_id)| Vote::new_bls(context, view, block_hash, app_hash, node_id, key))
        .collect();

    let sigs: Vec<_> = votes
        .iter()
        .filter_map(|v| hyperlicked::crypto::bls::BlsSignature::from_slice(&v.signature).ok())
        .collect();

    let agg_sig = aggregate_signatures(&sigs).expect("aggregation should succeed");
    let valid_qc = Certificate::new_bls(
        context,
        view,
        block_hash,
        votes,
        agg_sig.to_bytes().to_vec(),
    )
    .expect("valid BLS QC");

    // Valid QC should verify
    assert!(
        valid_qc.verify_bls().is_ok(),
        "Valid QC should pass BLS verification"
    );

    // Corrupt 1 byte of the aggregate signature
    let mut corrupted_qc = valid_qc.clone();
    corrupted_qc.agg_signature[0] ^= 0xFF; // Flip all bits in first byte

    // Corrupted QC should fail verification
    let result = corrupted_qc.verify_bls();
    assert!(
        result.is_err(),
        "Corrupted QC should fail BLS verification, got: {:?}",
        result
    );

    // Test with proposal containing corrupted QC
    let (_net0, net1, _net2) = MockNetwork::create_connected_trio();
    let configs = create_bls_configs(&node_ids, &keys);

    let mut follower = BlsTestNode::new(configs[1].clone(), net1, keys[1].clone());

    // Craft proposal with corrupted QC
    let parent = Block::genesis(context);
    let block = Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: 2,
        height: 1,
        parent: parent.hash(),
        payload: vec![],
        proposer: node_ids[0],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: Some(corrupted_qc.clone()),
    };

    let _propose = Propose {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        block,
        justify: Some(corrupted_qc),
        proposer_signature: vec![],
    };

    // Height 1 is exempt from QC check in our test node (matching engine.rs behavior)
    // But the prepare path should still reject corrupted QC
    let prepare = Prepare {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: 2,
        qc: {
            let mut bad_qc = valid_qc.clone();
            bad_qc.agg_signature[0] ^= 0xFF;
            bad_qc
        },
    };

    let pre_high_qc = follower.safety.high_qc().map(|q| q.view).unwrap_or(0);
    follower.process_prepare(prepare);
    let post_high_qc = follower.safety.high_qc().map(|q| q.view).unwrap_or(0);

    assert_eq!(
        pre_high_qc, post_high_qc,
        "Corrupted prepare QC should not update high_qc"
    );

    println!("Corrupted BLS aggregate rejection test passed!");
}

// =============================================================================
// Test 3: Multi-validator determinism — identical state
// =============================================================================

/// 3 honest BLS nodes run 10 rounds with transactions. After all rounds,
/// committed block hashes and app state must be identical across all nodes.
#[tokio::test]
async fn test_multivalidator_determinism_identical_state() {
    use hyperlicked::app::Transaction;

    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let keys = create_bls_keys(3);
    let configs = create_bls_configs(&node_ids, &keys);

    let (net0, net1, net2) = MockNetwork::create_connected_trio();

    let mut nodes: Vec<Arc<Mutex<BlsTestNode>>> = vec![
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[0].clone(),
            net0,
            keys[0].clone(),
        ))),
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[1].clone(),
            net1,
            keys[1].clone(),
        ))),
        Arc::new(Mutex::new(BlsTestNode::new(
            configs[2].clone(),
            net2,
            keys[2].clone(),
        ))),
    ];

    // Submit transactions to the leader node
    {
        let mut node0 = nodes[0].lock().await;
        let _ = node0.app.submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000, // $1M in cents
        });
        let _ = node0.app.submit_tx(Transaction::Deposit {
            trader: "bob".into(),
            amount: 50_000_000,
        });
    }

    // Run 10 consensus rounds
    run_bls_rounds(&mut nodes, 10).await.unwrap();

    // Collect committed heights
    let heights: Vec<u64> = {
        let mut h = Vec::new();
        for node in &nodes {
            h.push(node.lock().await.committed_height);
        }
        h
    };

    println!("Committed heights: {:?}", heights);

    // All nodes must have committed at least 1 block
    for (i, &height) in heights.iter().enumerate() {
        assert!(
            height >= 1,
            "Node {} should have committed at least 1 block, got {}",
            i,
            height
        );
    }

    // Find the minimum committed height across all nodes
    let min_height = *heights.iter().min().unwrap();

    // Compare committed block hashes at each height up to min_height
    for h in 1..=min_height {
        let mut hashes_at_height = Vec::new();
        for node in &nodes {
            let node = node.lock().await;
            if let Some(block) = node.store.get_by_height(h) {
                hashes_at_height.push(block.hash());
            }
        }

        // All nodes that have this height should agree on the hash
        if hashes_at_height.len() >= 2 {
            let first = hashes_at_height[0];
            for (i, hash) in hashes_at_height.iter().enumerate().skip(1) {
                assert_eq!(
                    first,
                    *hash,
                    "Block hash mismatch at height {}: node 0 has {}, node {} has {}",
                    h,
                    hash_short(&first),
                    i,
                    hash_short(hash)
                );
            }
        }
    }

    // Compare app state: all nodes should have the same account balances
    let balances: Vec<Option<i64>> = {
        let mut b = Vec::new();
        for node in &nodes {
            let node = node.lock().await;
            b.push(node.app.account("alice").map(|a| a.balance));
        }
        b
    };

    println!("Alice balances across nodes: {:?}", balances);

    // Nodes that have committed the deposit block should agree on balance
    let non_none_balances: Vec<i64> = balances.into_iter().flatten().collect();
    if non_none_balances.len() >= 2 {
        let first = non_none_balances[0];
        for (i, &bal) in non_none_balances.iter().enumerate().skip(1) {
            assert_eq!(
                first,
                bal,
                "Balance mismatch: node 0 has {}, node {} has {}",
                first,
                i + 1,
                bal
            );
        }
    }

    println!("Multi-validator determinism test passed!");
}

// =============================================================================
// Test 4: State hash divergence prevents quorum
// =============================================================================

/// One node has mutated state, producing a different app_hash in its vote.
/// The leader cannot form a valid QC with mismatched app_hashes.
/// Honest majority still progresses if they agree.
#[tokio::test]
async fn test_state_hash_divergence_prevents_quorum() {
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];
    let keys = create_bls_keys(3);

    // Create a block that all nodes will execute
    let context = ConsensusContext::new(0, [9u8; 32]);
    let genesis = Block::genesis(context);
    let block = Block {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        view: 0,
        height: 1,
        parent: genesis.hash(),
        payload: vec![],
        proposer: node_ids[0],
        commitment_root: [0u8; 32],
        app_hash: [0u8; 32],
        timestamp: 1000,
        justify: None,
    };

    let block_hash = block.hash();

    // Honest nodes produce matching app_hash
    let honest_app_hash = [0u8; 32]; // NoOp app returns zeros

    // Divergent node produces different app_hash (simulating state corruption)
    let divergent_app_hash = [0xFFu8; 32];

    // Create votes
    let honest_vote_0 = Vote::new_bls(
        context,
        0,
        block_hash,
        honest_app_hash,
        node_ids[0],
        &keys[0],
    );
    let honest_vote_1 = Vote::new_bls(
        context,
        0,
        block_hash,
        honest_app_hash,
        node_ids[1],
        &keys[1],
    );
    let divergent_vote = Vote::new_bls(
        context,
        0,
        block_hash,
        divergent_app_hash,
        node_ids[2],
        &keys[2],
    );

    // Attempt to form QC with divergent vote: the constructor must reject
    // votes that disagree on the application state hash.
    let all_sigs: Vec<_> = [&honest_vote_0, &honest_vote_1, &divergent_vote]
        .iter()
        .filter_map(|v| hyperlicked::crypto::bls::BlsSignature::from_slice(&v.signature).ok())
        .collect();

    let agg_sig = aggregate_signatures(&all_sigs).expect("aggregation should succeed");

    // Try to form a QC with all 3 votes (including divergent).
    let mixed_qc = Certificate::new_bls(
        context,
        0,
        block_hash,
        vec![
            honest_vote_0.clone(),
            honest_vote_1.clone(),
            divergent_vote.clone(),
        ],
        agg_sig.to_bytes().to_vec(),
    );

    assert!(
        mixed_qc.is_err(),
        "QC construction must reject votes with divergent app hashes"
    );

    // However, honest majority (2 of 3) can still form a valid QC
    let honest_sigs: Vec<_> = [&honest_vote_0, &honest_vote_1]
        .iter()
        .filter_map(|v| hyperlicked::crypto::bls::BlsSignature::from_slice(&v.signature).ok())
        .collect();

    let honest_agg = aggregate_signatures(&honest_sigs).expect("aggregation should succeed");
    let honest_qc = Certificate::new_bls(
        context,
        0,
        block_hash,
        vec![honest_vote_0, honest_vote_1],
        honest_agg.to_bytes().to_vec(),
    );

    let honest_qc = honest_qc.expect("honest BLS QC");

    // Honest QC should verify
    assert!(
        honest_qc.verify_bls().is_ok(),
        "Honest majority QC should pass BLS verification"
    );

    // Verify quorum: with n=3, quorum=2, honest majority is sufficient
    assert_eq!(honest_qc.vote_count(), 2);
    let config = ConsensusConfig {
        epoch: 0,
        genesis_hash: [0u8; 32],
        node_id: node_ids[0],
        validators: node_ids.to_vec(),
        voting_powers: vec![1; node_ids.len()],
        view_timeout_ms: 500,
        bls_pubkeys: vec![],
        bls_secret_key: None,
    };
    assert!(
        honest_qc.vote_count() >= config.quorum(),
        "Honest majority ({}) should meet quorum ({})",
        honest_qc.vote_count(),
        config.quorum()
    );

    println!("State hash divergence test passed!");
}
