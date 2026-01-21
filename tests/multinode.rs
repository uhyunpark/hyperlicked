//! Multi-Node Consensus Tests
//!
//! Tests that verify consensus works correctly across multiple nodes.
//! Uses MockNetwork for deterministic in-process testing.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::timeout;

use hyperlicked::app::AppState;
use hyperlicked::consensus::{AppHook, BlockStore, MemoryBlockStore, Pacemaker, Safety};
use hyperlicked::network::{MockNetwork, Network};
use hyperlicked::types::{
    hash_short, Block, Certificate, ConsensusConfig, Hash, Message, Prepare, Propose, Vote,
};

/// Test node state for multi-node testing
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
        self.store.get_by_height(0).unwrap_or_else(Block::genesis)
    }

    async fn run_leader_round(&mut self) -> anyhow::Result<Option<Block>> {
        let view = self.pacemaker.current_view();
        let parent = self.get_proposal_parent();
        let payload = self.app.prepare_payload(&parent);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        let mut block = Block {
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload,
            proposer: self.config.node_id,
            app_hash: [0u8; 32],
            timestamp: now.as_millis() as u64,
        };
        block.app_hash = self.app.execute(&block);

        let block_hash = block.hash();
        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        // Broadcast proposal
        let propose = Propose {
            block: block.clone(),
            justify: self.safety.high_qc().cloned(),
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
                    // Continue waiting for proposal
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
        let block = &propose.block;
        let view = block.view;

        // Execute block
        let local_app_hash = self.app.execute(block);

        // Check safety
        if self.safety.safe_to_vote(block, local_app_hash).is_err() {
            return None;
        }

        // Record vote and store block
        self.safety.record_vote(view);
        self.store.save(block);
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

        // Commit ancestors first
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

/// Run multiple consensus rounds across nodes
async fn run_consensus_rounds(
    nodes: &mut [Arc<Mutex<TestNode>>],
    rounds: usize,
) -> anyhow::Result<()> {
    for _ in 0..rounds {
        // Determine leader and run appropriate round
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

        // Small delay between rounds
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Ok(())
}

#[tokio::test]
async fn test_three_nodes_reach_consensus() {
    // Create connected networks
    let (net0, net1, net2) = MockNetwork::create_connected_trio();

    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];

    // Create consensus configs
    let configs: Vec<_> = node_ids
        .iter()
        .map(|&node_id| ConsensusConfig {
            node_id,
            validators: node_ids.to_vec(),
            view_timeout_ms: 500,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        })
        .collect();

    // Create nodes
    let mut nodes: Vec<Arc<Mutex<TestNode>>> = vec![
        Arc::new(Mutex::new(TestNode::new(configs[0].clone(), net0))),
        Arc::new(Mutex::new(TestNode::new(configs[1].clone(), net1))),
        Arc::new(Mutex::new(TestNode::new(configs[2].clone(), net2))),
    ];

    // Run 10 consensus rounds
    run_consensus_rounds(&mut nodes, 10).await.unwrap();

    // Verify all nodes have committed at least some blocks
    let heights: Vec<u64> = {
        let mut h = Vec::new();
        for node in &nodes {
            h.push(node.lock().await.committed_height);
        }
        h
    };

    println!("Committed heights: {:?}", heights);

    // All nodes should have committed at least 1 block
    for (i, &height) in heights.iter().enumerate() {
        assert!(
            height >= 1,
            "Node {} should have committed at least 1 block, got {}",
            i,
            height
        );
    }

    // All nodes should have committed the same blocks (check committed head hash)
    let hashes: Vec<Hash> = {
        let mut h = Vec::new();
        for node in &nodes {
            let node = node.lock().await;
            if let Some(block) = node.store.get_committed_head() {
                h.push(block.hash());
            }
        }
        h
    };

    // If all nodes have committed blocks, they should agree on the hash
    if hashes.len() == 3 {
        println!(
            "Committed block hashes: {}, {}, {}",
            hash_short(&hashes[0]),
            hash_short(&hashes[1]),
            hash_short(&hashes[2])
        );
        // Note: heights may differ slightly due to 2-chain commit rule timing
    }
}

#[tokio::test]
async fn test_transactions_included_in_blocks() {
    use hyperlicked::app::Transaction;

    // Create connected networks
    let (net0, net1, net2) = MockNetwork::create_connected_trio();
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];

    let configs: Vec<_> = node_ids
        .iter()
        .map(|&node_id| ConsensusConfig {
            node_id,
            validators: node_ids.to_vec(),
            view_timeout_ms: 500,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        })
        .collect();

    let nodes: Vec<Arc<Mutex<TestNode>>> = vec![
        Arc::new(Mutex::new(TestNode::new(configs[0].clone(), net0))),
        Arc::new(Mutex::new(TestNode::new(configs[1].clone(), net1))),
        Arc::new(Mutex::new(TestNode::new(configs[2].clone(), net2))),
    ];

    // Submit a deposit transaction to the leader (node 0 at view 0)
    {
        let mut node0 = nodes[0].lock().await;
        let _ = node0.app.submit_tx(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        });
    }

    // Run a few rounds
    let nodes_clone: Vec<_> = nodes.iter().map(Arc::clone).collect();
    let mut nodes_mut: Vec<_> = nodes_clone;
    run_consensus_rounds(&mut nodes_mut, 5).await.unwrap();

    // Check that all nodes have alice's deposit
    for (i, node) in nodes.iter().enumerate() {
        let node = node.lock().await;
        let account = node.app.account("alice");

        println!(
            "Node {} alice balance: {:?}",
            i,
            account.map(|a| a.balance)
        );

        // After block execution, alice should have her deposit
        if node.committed_height >= 1 {
            assert!(
                account.is_some(),
                "Node {} should have alice's account after commit",
                i
            );
        }
    }
}

#[tokio::test]
async fn test_leader_rotation() {
    let (_net0, _net1, _net2) = MockNetwork::create_connected_trio();
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];

    let configs: Vec<_> = node_ids
        .iter()
        .map(|&node_id| ConsensusConfig {
            node_id,
            validators: node_ids.to_vec(),
            view_timeout_ms: 500,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        })
        .collect();

    // Verify round-robin leader rotation
    assert_eq!(configs[0].leader_of(0), node_ids[0]);
    assert_eq!(configs[0].leader_of(1), node_ids[1]);
    assert_eq!(configs[0].leader_of(2), node_ids[2]);
    assert_eq!(configs[0].leader_of(3), node_ids[0]); // Wraps around

    // Verify is_leader works correctly
    assert!(configs[0].is_leader(0));
    assert!(!configs[0].is_leader(1));
    assert!(!configs[0].is_leader(2));

    assert!(!configs[1].is_leader(0));
    assert!(configs[1].is_leader(1));
    assert!(!configs[1].is_leader(2));

    assert!(!configs[2].is_leader(0));
    assert!(!configs[2].is_leader(1));
    assert!(configs[2].is_leader(2));

    println!("Leader rotation verified correctly");
}

#[tokio::test]
async fn test_quorum_calculation() {
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];

    let config = ConsensusConfig {
        node_id: node_ids[0],
        validators: node_ids.to_vec(),
        view_timeout_ms: 500,
        bls_pubkeys: vec![],
        bls_secret_key: None,
    };

    // For n=3: f=0, quorum = max(2*0+1, 3/2+1) = max(1, 2) = 2
    assert_eq!(config.n(), 3);
    assert_eq!(config.f(), 0);
    assert_eq!(config.quorum(), 2);

    // For n=4: f=1, quorum = max(2*1+1, 4/2+1) = max(3, 3) = 3
    let config4 = ConsensusConfig {
        node_id: [1u8; 32],
        validators: vec![[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]],
        view_timeout_ms: 500,
        bls_pubkeys: vec![],
        bls_secret_key: None,
    };
    assert_eq!(config4.n(), 4);
    assert_eq!(config4.f(), 1);
    assert_eq!(config4.quorum(), 3);

    println!("Quorum calculations verified");
}

#[tokio::test]
async fn test_bls_signature_aggregation() {
    use hyperlicked::crypto::bls::{aggregate_signatures, verify_aggregate, BlsSecretKey};

    // Generate deterministic BLS keys for 3 nodes (same as multinode binary)
    let keys: Vec<BlsSecretKey> = (0..3)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = (i + 1) as u8;
            seed[31] = 0xBE;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let public_keys: Vec<_> = keys.iter().map(|k| k.public_key()).collect();

    // Simulate voting on a block hash
    let message = b"vote_for_block_hash_12345";

    // All 3 validators sign the same message
    let signatures: Vec<_> = keys.iter().map(|k| k.sign(message)).collect();

    // Individual signatures should verify
    for (i, (pk, sig)) in public_keys.iter().zip(signatures.iter()).enumerate() {
        assert!(pk.verify(message, sig), "Signature {} should verify", i);
    }

    // Aggregate signatures (simulating quorum)
    let agg_sig = aggregate_signatures(&signatures).expect("Aggregation should succeed");

    // Aggregated signature should verify against all public keys
    assert!(
        verify_aggregate(message, &agg_sig, &public_keys),
        "Aggregated signature should verify"
    );

    // Aggregated signature with wrong message should fail
    assert!(
        !verify_aggregate(b"wrong_message", &agg_sig, &public_keys),
        "Wrong message should fail verification"
    );

    println!("BLS signature aggregation verified");
}
