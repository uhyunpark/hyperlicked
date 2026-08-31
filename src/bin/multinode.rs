//! Multi-Node Consensus Runner
//!
//! Runs a validator node in a multi-node network.
//!
//! Usage:
//!   cargo run --bin multinode -- --node 0
//!   cargo run --bin multinode -- --node 1
//!   cargo run --bin multinode -- --node 2
//!
//! The demo always uses BLS signatures and authenticated TCP. Its deterministic
//! keys and loopback addresses are development-only and must never be reused
//! for a production validator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use hyperlicked::api::{CanonicalAppHook, SharedState};
use hyperlicked::app::{AppState, StaticValidatorBootstrap};
use hyperlicked::consensus::{verify_certificate, AppHook, BlockStore, ConsensusRunner};
use hyperlicked::crypto::bls::{BlsPublicKey, BlsSecretKey};
use hyperlicked::network::{GossipConfig, NetworkConfig, TcpNetwork};
use hyperlicked::storage::{PersistentStore, RocksDbStore};
use hyperlicked::types::{
    genesis_domain_hash, hash_short, Block, Committee, ConsensusConfig, ConsensusContext,
};

/// Generate a deterministic BLS keypair and retain its original seed for this
/// development-only demo.
///
/// Production validators must load unique secret keys from secure storage.
fn generate_bls_keypair(node_index: usize) -> ([u8; 32], BlsSecretKey, BlsPublicKey) {
    // Deterministic seed based on node index (development-only).
    let mut seed = [0u8; 32];
    seed[0] = (node_index + 1) as u8;
    seed[31] = 0xBE; // BLS marker

    let sk = BlsSecretKey::from_seed(&seed);
    let pk = sk.public_key();

    (seed, sk, pk)
}

/// Replay finalized blocks into the canonical hook before a persistent runner
/// resumes. The regular `hl-node` binary performs the same recovery boundary;
/// keeping it here preserves the historical `multinode` fixture while making
/// its new RocksDB-backed restart path safe for the canonical application.
fn replay_committed_application(
    store: &RocksDbStore,
    app: &mut CanonicalAppHook,
    context: ConsensusContext,
    committee: &Committee,
) -> Result<()> {
    let genesis = Block::genesis(context);
    validate_recovery_epoch(app, &genesis, context)?;

    let Some(state) = store.load_consensus_state()? else {
        let blocks = store.blocks_from_height(1)?;
        if !blocks.is_empty() || store.get_committed_head().is_some() {
            anyhow::bail!(
                "persistent multinode store contains committed data without consensus state"
            );
        }
        return Ok(());
    };
    if state.context() != context {
        anyhow::bail!("persistent multinode consensus context does not match configuration");
    }

    let committed_head = store
        .get_committed_head()
        .ok_or_else(|| anyhow::anyhow!("persistent multinode store has no committed head"))?;
    let committed_height_meta = store
        .load_committed_height()?
        .ok_or_else(|| anyhow::anyhow!("persistent multinode committed height metadata missing"))?;
    if committed_height_meta != state.committed_height {
        anyhow::bail!(
            "persistent multinode committed height metadata does not match consensus state"
        );
    }
    if committed_head.height != state.committed_height
        || committed_head.hash() != state.committed_hash
    {
        anyhow::bail!("persistent multinode committed metadata does not match its block head");
    }

    let blocks = store.blocks_from_height(1)?;
    let expected = usize::try_from(state.committed_height).unwrap_or(usize::MAX);
    let committed = blocks
        .into_iter()
        .filter(|block| block.height <= state.committed_height)
        .collect::<Vec<_>>();
    if committed.len() != expected {
        anyhow::bail!(
            "persistent multinode replay is incomplete: expected {expected} blocks, found {}",
            committed.len()
        );
    }

    let mut parent = genesis.hash();
    let mut parent_block = genesis;
    for (index, block) in committed.iter().enumerate() {
        validate_recovery_epoch(app, block, context)?;
        if block.height != index as u64 + 1 {
            anyhow::bail!(
                "persistent multinode replay has a non-sequential block height {}",
                block.height
            );
        }
        block
            .validate_context(context)
            .map_err(|error| anyhow::anyhow!("persistent replay context mismatch: {error}"))?;
        block
            .validate()
            .map_err(|error| anyhow::anyhow!("persistent replay block is invalid: {error}"))?;
        if block.parent != parent {
            anyhow::bail!(
                "persistent multinode replay has a broken chain at height {}",
                block.height
            );
        }

        if block.height == 1 {
            if block.justify.is_some() {
                anyhow::bail!("persistent multinode height-one block must not carry a QC");
            }
        } else {
            let justify = block.justify.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "persistent multinode committed block {} is missing its parent QC",
                    block.height
                )
            })?;
            verify_certificate(
                committee,
                justify,
                context,
                parent_block.view,
                &block.parent,
                Some(&parent_block.app_hash),
                true,
            )
            .map_err(|error| {
                anyhow::anyhow!(
                    "persistent multinode committed block {} has an invalid parent QC: {error}",
                    block.height
                )
            })?;
        }

        let stored = store.load_commitment(&block.hash())?.ok_or_else(|| {
            anyhow::anyhow!(
                "persistent multinode finalized block {} is missing its Commitment v2 artifact",
                block.height
            )
        })?;
        let regenerated = app.preflight_commitment(block).map_err(|error| {
            anyhow::anyhow!(
                "persistent multinode replay commitment generation failed at height {}: {error}",
                block.height
            )
        })?;
        let generated = regenerated.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "persistent multinode commitment exists at height {} but application produced none",
                block.height
            )
        })?;
        if generated != &stored {
            anyhow::bail!(
                "persistent multinode replay commitment mismatch at height {}",
                block.height
            );
        }
        let generated_commitment_root = generated.root().map_err(|error| {
            anyhow::anyhow!(
                "persistent multinode replay commitment root failed at height {}: {error}",
                block.height
            )
        })?;
        if generated_commitment_root != block.commitment_root {
            anyhow::bail!(
                "persistent multinode authenticated commitment-root mismatch at height {}",
                block.height
            );
        }

        let stored_state_root = store.load_state_root(&block.hash())?.ok_or_else(|| {
            anyhow::anyhow!(
                "persistent multinode finalized block {} is missing its full-state root",
                block.height
            )
        })?;
        let generated_state_root = app
            .preflight_state_root(block)
            .map_err(|error| {
                anyhow::anyhow!(
                    "persistent multinode replay state-root generation failed at height {}: {error}",
                    block.height
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "persistent multinode state root exists at height {} but application produced none",
                    block.height
                )
            })?;
        if generated_state_root != stored_state_root {
            anyhow::bail!(
                "persistent multinode replay state-root mismatch at height {}",
                block.height
            );
        }
        if generated_state_root != block.app_hash {
            anyhow::bail!(
                "persistent multinode authenticated state-root mismatch at height {}",
                block.height
            );
        }

        let app_hash = app.commit(block).map_err(|error| {
            anyhow::anyhow!("persistent multinode application replay failed: {error}")
        })?;
        if app_hash != block.app_hash {
            anyhow::bail!(
                "persistent multinode replay produced a mismatched application root at height {}",
                block.height
            );
        }
        validate_recovery_epoch(app, block, context)?;
        parent = block.hash();
        parent_block = block.clone();
    }

    if parent != state.committed_hash {
        anyhow::bail!("persistent multinode replay head does not match consensus state");
    }
    let observed_height = app
        .shared_state()
        .app
        .read()
        .map_err(|_| {
            anyhow::anyhow!("persistent multinode application lock poisoned during replay")
        })?
        .committed_height();
    if observed_height != state.committed_height {
        anyhow::bail!(
            "persistent multinode application replay committed height {} does not match persisted height {}",
            observed_height,
            state.committed_height
        );
    }
    Ok(())
}

fn validate_recovery_epoch(
    app: &CanonicalAppHook,
    block: &Block,
    expected_context: ConsensusContext,
) -> Result<()> {
    if block.epoch != block.context().epoch || block.context().epoch != expected_context.epoch {
        anyhow::bail!(
            "recovery block/context epoch mismatch: block {}, block context {}, configured {}",
            block.epoch,
            block.context().epoch,
            expected_context.epoch
        );
    }

    let shared_state = app.shared_state();
    let canonical = shared_state.app.read().map_err(|_| {
        anyhow::anyhow!("application state lock poisoned during recovery epoch validation")
    })?;
    if canonical.current_epoch() != block.epoch {
        anyhow::bail!(
            "recovery application epoch {} does not match block epoch {}",
            canonical.current_epoch(),
            block.epoch
        );
    }
    if canonical.pending_validator_update().is_some() {
        anyhow::bail!(
            "recovery application contains a pending validator update in static committee mode"
        );
    }
    Ok(())
}

fn multinode_data_dir(
    args: &[String],
    context: ConsensusContext,
    node_id: [u8; 32],
) -> Result<PathBuf> {
    if let Some(index) = args.iter().position(|arg| arg == "--data-dir") {
        let value = args
            .get(index + 1)
            .ok_or_else(|| anyhow::anyhow!("--data-dir requires a path"))?;
        return Ok(PathBuf::from(value));
    }
    Ok(Path::new(".hyperlicked")
        .join("data")
        .join(hex::encode(context.genesis_hash))
        .join(hex::encode(node_id)))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let node_index: usize = args
        .iter()
        .position(|a| a == "--node")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .expect("Usage: multinode --node <0|1|2>");

    if node_index > 2 {
        panic!("Node index must be 0, 1, or 2");
    }

    println!("╔════════════════════════════════════════╗");
    println!("║   Hyperliquid-RS Multi-Node v0.1.0     ║");
    println!("║   HotStuff-2 Consensus Engine          ║");
    println!("╚════════════════════════════════════════╝");
    println!();

    // Create consensus config
    let node_ids = [[1u8; 32], [2u8; 32], [3u8; 32]];

    // Generate the complete deterministic committee and local key for this
    // development-only demo. The same ID -> key map is used by consensus and
    // the authenticated TCP transport on every node.
    let mut bls_pubkeys = Vec::with_capacity(node_ids.len());
    let mut validator_pubkeys = HashMap::with_capacity(node_ids.len());
    let mut local_bls_secret_key = None;
    let mut local_bls_secret_seed = None;
    for (index, node_id) in node_ids.iter().enumerate() {
        let (secret_seed, secret_key, public_key) = generate_bls_keypair(index);
        bls_pubkeys.push(public_key.to_bytes().to_vec());
        validator_pubkeys.insert(*node_id, public_key);
        if index == node_index {
            local_bls_secret_key = Some(secret_key);
            local_bls_secret_seed = Some(secret_seed);
        }
    }

    let local_bls_secret_key = local_bls_secret_key.expect("local deterministic BLS key missing");
    let local_bls_secret_seed =
        local_bls_secret_seed.expect("local deterministic BLS seed missing");
    let net_config = NetworkConfig::local_three_nodes(node_index)
        .with_authentication(local_bls_secret_key, validator_pubkeys);

    println!("Node Configuration (development-only):");
    println!("  Index: {}", node_index);
    println!("  Node ID: {}", hash_short(&net_config.node_id));
    println!(
        "  Listen: {} (loopback development address)",
        net_config.listen_addr
    );
    println!(
        "  Peers: {:?}",
        net_config
            .peers
            .iter()
            .map(|(_, addr)| addr)
            .collect::<Vec<_>>()
    );
    println!("  TCP authentication: BLS (always enabled)");
    println!("  Key material: deterministic development-only fixtures");
    println!();

    let mut consensus_config = ConsensusConfig {
        epoch: 0,
        genesis_hash: [0u8; 32],
        node_id: net_config.node_id,
        validators: node_ids.to_vec(),
        voting_powers: vec![1, 1, 1],
        view_timeout_ms: 3000,
        bls_pubkeys,
        bls_secret_key: Some(local_bls_secret_seed),
    };
    consensus_config.genesis_hash = genesis_domain_hash(
        "hyperlicked-multinode-local",
        consensus_config.epoch,
        consensus_config.view_timeout_ms,
        consensus_config
            .committee()
            .expect("multinode committee must be valid")
            .hash(),
    );

    let context = consensus_config
        .context()
        .map_err(|error| anyhow::anyhow!("invalid consensus context: {error}"))?;
    let committee = consensus_config
        .committee()
        .map_err(|error| anyhow::anyhow!("invalid consensus committee: {error}"))?;
    let net_config = net_config
        .with_gossip_validation(context, committee.clone())
        .with_dev_envelopes(true);
    let staking_bootstrap = node_ids
        .iter()
        .enumerate()
        .map(|(index, node_id)| {
            let (_, secret_key, public_key) = generate_bls_keypair(index);
            let proof = secret_key
                .create_proof_of_possession(&context.genesis_hash, node_id)
                .to_bytes()
                .to_vec();
            StaticValidatorBootstrap {
                operator: format!("system:genesis:{}", hex::encode(node_id)),
                node_id: *node_id,
                voting_power: 1,
                bls_pubkey: public_key.to_bytes().to_vec(),
                bls_proof_of_possession: proof,
                self_stake: hyperlicked::app::staking::MIN_SELF_STAKE,
                commission_bps: 0,
            }
        })
        .collect::<Vec<_>>();
    let data_dir = multinode_data_dir(&args, context, net_config.node_id)?;
    let persistent_store = std::sync::Arc::new(RocksDbStore::open(&data_dir).map_err(|error| {
        anyhow::anyhow!(
            "failed to open multinode RocksDB {}: {error}",
            data_dir.display()
        )
    })?);
    println!("  Data directory: {}", data_dir.display());

    println!("Consensus Configuration:");
    println!("  Validators: {}", consensus_config.n());
    println!("  Quorum rule: >2/3 voting power (3/3 in this equal-power fixture)");
    println!("  Byzantine fault tolerance: {}", consensus_config.f());
    println!(
        "  BLS Signatures: {}",
        if consensus_config.bls_enabled() {
            "ENABLED (development-only deterministic keys)"
        } else {
            "ERROR"
        }
    );
    println!();

    // Load gossip config from environment
    let gossip_config = GossipConfig::from_env();
    println!("Gossip Configuration:");
    println!(
        "  Status: {}",
        if gossip_config.enabled {
            "ENABLED"
        } else {
            "disabled"
        }
    );
    if gossip_config.enabled {
        println!("  Fanout: {}", gossip_config.fanout);
        println!("  TTL: {} hops", gossip_config.ttl);
        println!("  Cache size: {}", gossip_config.cache_size);
    }
    println!();

    // Create and start network
    let network = TcpNetwork::new(net_config).await?;
    network.start().await?;

    // Wait for connections to establish
    println!("Waiting for peer connections...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Use the same canonical speculative/commit boundary as `hl-node`.
    // Running the demo directly on AppState would skip Commitment v2 and the
    // schema-v3 full-state-root preflight exercised by production-shaped
    // validators.
    let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
    app_state.set_consensus_context(context);
    app_state
        .bootstrap_static_committee(&committee, &staking_bootstrap)
        .map_err(anyhow::Error::msg)?;
    app_state
        .bind_authoritative_committee(committee.clone())
        .map_err(anyhow::Error::msg)?;
    let shared = SharedState::new(app_state);
    shared.set_user_transaction_publisher(std::sync::Arc::new(network.transaction_broadcaster()));
    let mut app_hook = CanonicalAppHook::new(shared);
    replay_committed_application(&persistent_store, &mut app_hook, context, &committee)?;
    let mut runner =
        ConsensusRunner::new_with_recovery(consensus_config, network, persistent_store)
            .await?
            .with_app(app_hook);

    println!("Starting consensus with orderbook...");
    println!("────────────────────────────────────────");

    runner.run().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlicked::storage::ConsensusState;
    use hyperlicked::types::{Block, Certificate};
    use tempfile::TempDir;

    #[test]
    fn consensus_secret_seed_reconstructs_configured_public_key() {
        let (seed, secret_key, public_key) = generate_bls_keypair(0);
        let node_id = [1u8; 32];
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![public_key.to_bytes().to_vec()],
            bls_secret_key: Some(seed),
        };

        assert_eq!(secret_key.public_key().to_bytes(), public_key.to_bytes());
        assert_eq!(
            config.bls_secret_key().unwrap().public_key().to_bytes(),
            config.bls_pubkeys[0].as_slice()
        );
    }

    #[test]
    fn demo_application_uses_the_canonical_state_root_boundary() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let genesis = Block::genesis(context);
        let mut app_state = AppState::new_with_chain_domain(context.genesis_hash);
        app_state.set_consensus_context(context);
        let mut app = CanonicalAppHook::new(SharedState::new(app_state));
        let mut block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };

        block.app_hash = app.execute(&block);
        let commitment = app.derive_execution_commitment(&block).unwrap().unwrap();
        block.commitment_root = commitment.root().unwrap();
        app.seal_execution_commitment(&block).unwrap();
        assert!(app.preflight_commitment(&block).unwrap().is_some());
        assert!(app.preflight_state_root(&block).unwrap().is_some());
    }

    #[test]
    fn recovery_rejects_corrupted_height_one_metadata_before_application_commit() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let directory = TempDir::new().unwrap();
        let store = RocksDbStore::open(directory.path()).unwrap();
        let genesis_state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &genesis_state).unwrap();

        let mut source = CanonicalAppHook::new(SharedState::new(
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true),
        ));
        source
            .shared_state()
            .app
            .write()
            .unwrap()
            .set_consensus_context(context);
        let mut block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        block.app_hash = source.execute(&block);
        let commitment = source.derive_execution_commitment(&block).unwrap().unwrap();
        block.commitment_root = commitment.root().unwrap();
        source.seal_execution_commitment(&block).unwrap();
        let state_root = source.preflight_state_root(&block).unwrap().unwrap();
        let committed_state = ConsensusState {
            committed_height: 1,
            committed_hash: block.hash(),
            ..genesis_state
        };
        store
            .commit_block_with_commitment_and_state_root(
                &block,
                &committed_state,
                Some(&commitment),
                Some(&state_root),
            )
            .unwrap();

        // `justify` is outside Block::hash(). A crash-corrupted row can keep
        // the authenticated block hash while violating the height-one rule.
        let mut corrupted = block;
        corrupted.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 0,
            block_hash: [0u8; 32],
            app_hash: None,
            votes: Vec::new(),
            voters: Vec::new(),
            bls_pubkeys: Vec::new(),
            agg_signature: Vec::new(),
        });
        BlockStore::save(&store, &corrupted);

        let shared = SharedState::new(AppState::new_with_chain_domain_and_dev(
            context.genesis_hash,
            true,
        ));
        shared.app.write().unwrap().set_consensus_context(context);
        let mut replay = CanonicalAppHook::new(shared.clone());
        let error = replay_committed_application(&store, &mut replay, context, &committee)
            .expect_err("corrupted persisted height-one metadata must fail closed");
        assert!(error
            .to_string()
            .contains("height-one block must not carry"));
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }
}
