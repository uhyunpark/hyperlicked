//! Canonical Hyperlicked validator runtime.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use hyperlicked::api::{
    create_router_with_store_and_mode, run_user_transaction_rebroadcast, CanonicalAppHook,
    SharedState,
};
use hyperlicked::app::AppState;
use hyperlicked::config::Config;
use hyperlicked::consensus::{verify_certificate, AppHook, BlockStore, ConsensusRunner};
use hyperlicked::network::{ActiveSyncClient, ActiveSyncConfig, TcpNetwork};
use hyperlicked::node_config::load_node_runtime_config;
use hyperlicked::storage::{ConsensusState, PersistentStore, RocksDbStore};
use hyperlicked::types::hash_short;
use hyperlicked::types::{Block, Committee, ConsensusContext, Hash};
use hyperlicked::VerifiedBlockImporter;
use serde::Deserialize;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

/// A sync request is deliberately bounded even when the peer reports a very
/// large height.  The peer's status is only a work hint; every returned batch
/// still carries its own locally verified finality proof.
const MAX_BOOTSTRAP_BATCH_BLOCKS: u64 = 1_000;
const MAX_SYNC_STATUS_RESPONSE_BYTES: usize = 64 * 1024;
const BOOTSTRAP_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
struct SyncPeerStatusHint {
    height: u64,
}

#[derive(Debug, Parser)]
#[command(name = "hl-node", about = "Run a Hyperlicked validator")]
struct Args {
    /// Path to the shared genesis configuration.
    #[arg(long, value_name = "PATH")]
    genesis: PathBuf,

    /// Path to this validator's process-local configuration.
    #[arg(long, value_name = "PATH")]
    config: PathBuf,

    /// Stop after this committed height instead of waiting for Ctrl-C.
    #[arg(long, value_name = "HEIGHT")]
    blocks: Option<u64>,

    /// Maximum time to wait for configured peers to connect.
    #[arg(long, default_value_t = 10_000, value_name = "MILLISECONDS")]
    peer_wait_ms: u64,

    /// Persistent RocksDB directory.  Defaults to a chain-domain and
    /// node-id-specific local directory so validators and chains do not share
    /// state accidentally.
    #[arg(long, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Optional HTTP sync peer used to bootstrap a fresh or lagging node
    /// before the API and consensus services start.  The peer is only a
    /// source of bytes and height hints; the local genesis/committee remain
    /// the sole trust roots.
    #[arg(long, value_name = "URL")]
    sync_peer: Option<String>,
}

fn genesis_consensus_state(context: ConsensusContext) -> ConsensusState {
    let genesis = Block::genesis(context);
    ConsensusState {
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
    }
}

/// Install the canonical genesis boundary before a requested bootstrap.
///
/// `ConsensusRunner::new_with_recovery` normally performs this write, but a
/// sync importer must run before that constructor (and before either listener
/// starts).  The same atomic store boundary is used here, and an existing
/// database is never replaced or adopted from the peer.
fn ensure_genesis_state(store: &RocksDbStore, context: ConsensusContext) -> Result<()> {
    if let Some(state) = store.load_consensus_state()? {
        if state.context() != context {
            anyhow::bail!("persisted consensus context does not match node genesis");
        }
        return Ok(());
    }

    if !store.blocks_from_height(0)?.is_empty() || store.get_committed_head().is_some() {
        anyhow::bail!(
            "persistent store contains blocks but no consensus state; refusing bootstrap"
        );
    }

    let genesis = Block::genesis(context);
    let state = genesis_consensus_state(context);
    store
        .commit_block(&genesis, &state)
        .context("failed to atomically install canonical genesis state")?;
    Ok(())
}

async fn query_sync_peer_height(peer: &str) -> Result<u64> {
    if peer.trim().is_empty() {
        anyhow::bail!("--sync-peer must not be empty");
    }
    let client = reqwest::Client::builder()
        .timeout(BOOTSTRAP_REQUEST_TIMEOUT)
        .build()
        .context("failed to create sync status client")?;
    let url = format!("{}/api/v1/sync/status", peer.trim_end_matches('/'));
    let response = client
        .get(url)
        .send()
        .await
        .context("sync peer status request failed")?;
    if !response.status().is_success() {
        anyhow::bail!(
            "sync peer status request returned HTTP {}",
            response.status()
        );
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SYNC_STATUS_RESPONSE_BYTES as u64)
    {
        anyhow::bail!(
            "sync peer status response exceeds {} bytes",
            MAX_SYNC_STATUS_RESPONSE_BYTES
        );
    }
    let mut response = response;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed reading sync peer status response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_SYNC_STATUS_RESPONSE_BYTES {
            anyhow::bail!(
                "sync peer status response exceeds {} bytes",
                MAX_SYNC_STATUS_RESPONSE_BYTES
            );
        }
        body.extend_from_slice(&chunk);
    }
    let status: SyncPeerStatusHint =
        serde_json::from_slice(&body).context("sync peer returned an invalid status response")?;
    Ok(status.height)
}

async fn bootstrap_from_peer(
    peer: &str,
    store: &RocksDbStore,
    app: &mut CanonicalAppHook,
    context: ConsensusContext,
    committee: &Committee,
) -> Result<()> {
    ensure_genesis_state(store, context)?;
    let peer_height = query_sync_peer_height(peer).await?;
    let config = ActiveSyncConfig::try_new(
        vec![peer.to_string()],
        BOOTSTRAP_REQUEST_TIMEOUT,
        context,
        committee.clone(),
    )
    .map_err(|error| anyhow::anyhow!("invalid active-sync configuration: {error}"))?;
    let client = ActiveSyncClient::try_new(config)
        .map_err(|error| anyhow::anyhow!("failed to create active-sync client: {error}"))?;

    let mut state = store.load_consensus_state()?.ok_or_else(|| {
        anyhow::anyhow!("bootstrap consensus state is missing after genesis setup")
    })?;
    let mut head = store.get_committed_head().ok_or_else(|| {
        anyhow::anyhow!("bootstrap committed head is missing after genesis setup")
    })?;
    if head.height != state.committed_height || head.hash() != state.committed_hash {
        anyhow::bail!("bootstrap committed metadata does not match consensus state");
    }

    while head.height < peer_height {
        let to_height = head
            .height
            .saturating_add(MAX_BOOTSTRAP_BATCH_BLOCKS)
            .min(peer_height);
        let batch = client
            .download_verified_finalized_batch(peer, &head, to_height)
            .await
            .map_err(|error| anyhow::anyhow!("verified bootstrap download failed: {error}"))?;
        VerifiedBlockImporter::import(
            app,
            store,
            context,
            committee,
            &batch.blocks,
            &batch.proof.child,
            &batch.proof.commit_qc,
        )
        .map_err(|error| anyhow::anyhow!("verified block import failed: {error}"))?;
        state = store.load_consensus_state()?.ok_or_else(|| {
            anyhow::anyhow!("bootstrap consensus state disappeared after verified import")
        })?;
        head = store
            .get_committed_head()
            .ok_or_else(|| anyhow::anyhow!("bootstrap committed head disappeared after import"))?;
        if head.height != state.committed_height || head.hash() != state.committed_hash {
            anyhow::bail!("bootstrap commit/state metadata diverged after import");
        }
    }
    Ok(())
}

fn replay_committed_application(
    store: &RocksDbStore,
    app: &mut CanonicalAppHook,
    context: ConsensusContext,
    committee: &Committee,
) -> Result<()> {
    let genesis = Block::genesis(context);
    validate_recovery_epoch(app, &genesis, context)?;

    let consensus_state = store.load_consensus_state()?;
    let Some(state) = consensus_state else {
        let blocks = store.blocks_from_height(1)?;
        if !blocks.is_empty() || store.get_committed_head().is_some() {
            anyhow::bail!("persistent store contains committed data without consensus state");
        }
        return Ok(());
    };

    if state.context() != context {
        anyhow::bail!("persisted consensus context does not match node genesis");
    }
    let committed_head = store
        .get_committed_head()
        .ok_or_else(|| anyhow::anyhow!("persisted committed metadata has no block head"))?;
    let committed_height_meta = store
        .load_committed_height()?
        .ok_or_else(|| anyhow::anyhow!("persisted committed height metadata is missing"))?;
    if committed_height_meta != state.committed_height {
        anyhow::bail!("persisted committed height metadata does not match consensus state");
    }
    if committed_head.height != state.committed_height
        || committed_head.hash() != state.committed_hash
    {
        anyhow::bail!("persisted committed metadata does not match its block head");
    }
    let blocks: Vec<_> = store
        .blocks_from_height(1)?
        .into_iter()
        .filter(|block| block.height <= state.committed_height)
        .collect();
    let expected_count = usize::try_from(state.committed_height).unwrap_or(usize::MAX);
    if blocks.len() != expected_count {
        anyhow::bail!(
            "persisted application replay is incomplete: expected {} blocks after genesis, found {}",
            state.committed_height,
            blocks.len()
        );
    }

    let mut parent = genesis.hash();
    let mut parent_block = genesis;
    for (index, block) in blocks.iter().enumerate() {
        validate_recovery_epoch(app, block, context)?;
        if block.height != index as u64 + 1 {
            anyhow::bail!(
                "persisted replay block has non-sequential height {}",
                block.height
            );
        }
        block
            .validate_context(context)
            .map_err(|error| anyhow::anyhow!("persisted replay context mismatch: {error}"))?;
        block
            .validate()
            .map_err(|error| anyhow::anyhow!("persisted replay block is invalid: {error}"))?;
        if block.parent != parent {
            anyhow::bail!(
                "persisted replay chain has a broken parent at height {}",
                block.height
            );
        }
        if block.height == 1 {
            if block.justify.is_some() {
                anyhow::bail!("persisted height-one block must not carry a QC");
            }
        } else {
            let justify = block.justify.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "persisted committed block {} is missing its parent QC",
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
                    "persisted committed block {} has an invalid parent QC: {error}",
                    block.height
                )
            })?;
        }
        let stored = store.load_commitment(&block.hash())?.ok_or_else(|| {
            anyhow::anyhow!(
                "persisted finalized block {} is missing its Commitment v2 artifact",
                block.height
            )
        })?;
        let regenerated = app.preflight_commitment(block).map_err(|error| {
            anyhow::anyhow!(
                "application replay commitment generation failed at height {}: {error}",
                block.height
            )
        })?;
        let generated = regenerated.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "persisted commitment exists at height {} but application produced none",
                block.height
            )
        })?;
        if generated != &stored {
            anyhow::bail!("persisted commitment mismatch at height {}", block.height);
        }
        let generated_commitment_root = generated.root().map_err(|error| {
            anyhow::anyhow!(
                "application replay commitment root failed at height {}: {error}",
                block.height
            )
        })?;
        if generated_commitment_root != block.commitment_root {
            anyhow::bail!(
                "authenticated commitment-root mismatch at height {}: block {}, replay {}",
                block.height,
                hex::encode(block.commitment_root),
                hex::encode(generated_commitment_root)
            );
        }

        let stored_state_root = store.load_state_root(&block.hash())?.ok_or_else(|| {
            anyhow::anyhow!(
                "persisted finalized block {} is missing its full-state root",
                block.height
            )
        })?;
        let generated_state_root = app
            .preflight_state_root(block)
            .map_err(|error| {
                anyhow::anyhow!(
                    "application replay state-root generation failed at height {}: {error}",
                    block.height
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "persisted state root exists at height {} but application produced none",
                    block.height
                )
            })?;
        if generated_state_root != stored_state_root {
            anyhow::bail!("persisted state-root mismatch at height {}", block.height);
        }
        if generated_state_root != block.app_hash {
            anyhow::bail!(
                "authenticated state-root mismatch at height {}: block {}, replay {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(generated_state_root)
            );
        }

        let committed_root = app.commit(block).map_err(|error| {
            anyhow::anyhow!(
                "application replay failed at height {}: {error}",
                block.height
            )
        })?;
        if committed_root != block.app_hash {
            anyhow::bail!(
                "application replay committed root mismatch at height {}: block {}, replay {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(committed_root)
            );
        }
        validate_recovery_epoch(app, block, context)?;
        parent = block.hash();
        parent_block = block.clone();
    }

    if parent != state.committed_hash {
        anyhow::bail!("persisted replay head does not match committed metadata");
    }
    let observed_height = app
        .shared_state()
        .app
        .read()
        .map_err(|_| anyhow::anyhow!("application state lock poisoned during replay"))?
        .committed_height();
    if observed_height != state.committed_height {
        anyhow::bail!(
            "application replay committed height {} does not match persisted height {}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use hyperlicked::app::staking::{StaticValidatorBootstrap, MIN_SELF_STAKE};
    use hyperlicked::app::AppState;
    use hyperlicked::network::active_sync::{
        PeerBlockExport, PeerBlockRangeResponse, PeerCertificateExport, PeerFinalityProofExport,
    };
    use hyperlicked::storage::ConsensusState;
    use hyperlicked::types::{Certificate, ConsensusConfig, Vote};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    fn finalize_speculative_block(
        app: &mut CanonicalAppHook,
        context: ConsensusContext,
        parent: &Block,
        height: u64,
        view: u64,
        proposer: u8,
        timestamp: u64,
        justify: Option<Certificate>,
    ) -> Block {
        let mut block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent: parent.hash(),
            payload: Vec::new(),
            proposer: [proposer; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp,
            justify,
        };
        block.app_hash = app.execute(&block);
        assert_ne!(block.app_hash, [0u8; 32]);
        let commitment = app
            .derive_execution_commitment(&block)
            .unwrap()
            .expect("speculative execution commitment");
        block.commitment_root = commitment.root().unwrap();
        app.seal_execution_commitment(&block).unwrap();
        block
    }

    fn qc_for(config: &ConsensusConfig, context: ConsensusContext, block: &Block) -> Certificate {
        let vote = Vote::new_bls(
            context,
            block.view,
            block.hash(),
            block.app_hash,
            config.node_id,
            &config.bls_secret_key().expect("single-node BLS key"),
        );
        Certificate::new_bls(
            context,
            block.view,
            block.hash(),
            vec![vote.clone()],
            vote.signature.clone(),
        )
        .unwrap()
    }

    fn make_speculative_branch(
        config: &ConsensusConfig,
        context: ConsensusContext,
        length: u64,
        branch_salt: u8,
    ) -> (Vec<Block>, Certificate) {
        let mut app = CanonicalAppHook::new(SharedState::new(
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true),
        ));
        let mut parent = Block::genesis(context);
        let mut blocks = Vec::with_capacity(length as usize);
        for height in 1..=length {
            let justify = (height > 1).then(|| qc_for(config, context, &parent));
            let block = finalize_speculative_block(
                &mut app,
                context,
                &parent,
                height,
                height,
                config.node_id[0],
                u64::from(branch_salt) * 1_000 + height,
                justify,
            );
            parent = block.clone();
            blocks.push(block);
        }
        let qc = qc_for(config, context, &parent);
        (blocks, qc)
    }

    fn bootstrap_test_app(
        config: &ConsensusConfig,
        context: ConsensusContext,
        committee: &Committee,
    ) -> CanonicalAppHook {
        let mut state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        state.set_consensus_context(context);
        let secret = config.bls_secret_key().expect("fixture BLS key");
        state
            .bootstrap_static_committee(
                committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(config.node_id)),
                    node_id: config.node_id,
                    voting_power: 1,
                    bls_pubkey: secret.public_key().to_bytes().to_vec(),
                    bls_proof_of_possession: secret
                        .create_proof_of_possession(&context.genesis_hash, &config.node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
            )
            .expect("fixture staking bootstrap");
        state
            .bind_authoritative_committee(committee.clone())
            .expect("fixture committee binding");
        CanonicalAppHook::new(SharedState::new(state))
    }

    fn bootstrap_test_fixture() -> (
        ConsensusConfig,
        ConsensusContext,
        Committee,
        Vec<Block>,
        Block,
        Certificate,
    ) {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [7u8; 32];
        let context = config.context().expect("fixture context");
        let committee = config.committee().expect("fixture committee");
        let mut source = bootstrap_test_app(&config, context, &committee);
        let genesis = Block::genesis(context);
        let first = finalize_speculative_block(&mut source, context, &genesis, 1, 1, 1, 1, None);
        let first_qc = qc_for(&config, context, &first);
        let second =
            finalize_speculative_block(&mut source, context, &first, 2, 2, 1, 2, Some(first_qc));
        let second_qc = qc_for(&config, context, &second);
        let child =
            finalize_speculative_block(&mut source, context, &second, 3, 3, 1, 3, Some(second_qc));
        let child_qc = qc_for(&config, context, &child);
        (
            config,
            context,
            committee,
            vec![first, second],
            child,
            child_qc,
        )
    }

    fn export_certificate(certificate: &Certificate) -> PeerCertificateExport {
        PeerCertificateExport {
            epoch: certificate.epoch,
            committee_hash: hex::encode(certificate.committee_hash),
            genesis_hash: hex::encode(certificate.genesis_hash),
            view: certificate.view,
            block_hash: hex::encode(certificate.block_hash),
            app_hash: certificate.app_hash.map(hex::encode),
            voters: certificate.voters.iter().map(hex::encode).collect(),
            bls_pubkeys: certificate.bls_pubkeys.iter().map(hex::encode).collect(),
            agg_signature: hex::encode(&certificate.agg_signature),
        }
    }

    fn export_block(block: &Block) -> PeerBlockExport {
        PeerBlockExport {
            epoch: block.epoch,
            committee_hash: hex::encode(block.committee_hash),
            genesis_hash: hex::encode(block.genesis_hash),
            height: block.height,
            view: block.view,
            hash: hex::encode(block.hash()),
            parent_hash: hex::encode(block.parent),
            app_hash: hex::encode(block.app_hash),
            commitment_root: hex::encode(block.commitment_root),
            proposer: hex::encode(block.proposer),
            timestamp: block.timestamp,
            payload: Some(BASE64.encode(&block.payload)),
            justify: block.justify.as_ref().map(export_certificate),
        }
    }

    async fn spawn_http_responses(
        responses: Vec<(&'static str, Vec<u8>)>,
        content_length: bool,
    ) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock sync listener");
        let address = listener.local_addr().expect("mock sync listener address");
        let task = tokio::spawn(async move {
            for (expected_target, body) in responses {
                let (mut stream, _) = listener.accept().await.expect("mock sync request");
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.expect("mock sync read");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request_line = request
                    .split(|byte| *byte == b'\n')
                    .next()
                    .and_then(|line| std::str::from_utf8(line).ok())
                    .expect("mock sync request line");
                assert_eq!(
                    request_line.trim_end(),
                    format!("GET {expected_target} HTTP/1.1")
                );

                let headers = if content_length {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_string()
                };
                stream
                    .write_all(headers.as_bytes())
                    .await
                    .expect("mock sync headers");
                stream.write_all(&body).await.expect("mock sync body");
                stream.shutdown().await.expect("mock sync shutdown");
            }
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn invalid_speculative_qc_fails_before_application_execution() {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [7u8; 32];
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let target = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 0,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let qc = Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: target.view,
            block_hash: target.hash(),
            app_hash: Some(target.app_hash),
            votes: Vec::new(),
            voters: vec![config.node_id],
            bls_pubkeys: vec![committee.bls_pubkey(&config.node_id).unwrap().to_vec()],
            agg_signature: vec![0u8; 96],
        };

        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(qc),
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &state).unwrap();
        store.save_block(&target).unwrap();

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        let error = replay_speculative_application(&store, &mut app, context, &committee)
            .expect_err("invalid QC must fail before speculative execution");
        assert!(error.to_string().contains("certificate"));
        assert_eq!(app.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }

    #[test]
    fn recovery_rejects_speculative_block_from_non_scheduled_leader() {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [7u8; 32];
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let target = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 0,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [2u8; 32],
            commitment_root: [2u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let qc = qc_for(&config, context, &target);

        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(qc),
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 1,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &state).unwrap();
        store.save_block(&target).unwrap();

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        let error = replay_speculative_application(&store, &mut app, context, &committee)
            .expect_err("non-scheduled proposer must fail before speculative execution");
        assert!(error.to_string().contains("scheduled leader"));
        assert_eq!(app.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }

    #[test]
    fn recovery_validates_distinct_qc_branches_without_filling_live_candidate_cap() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let (high_branch, high_qc) = make_speculative_branch(&config, context, 9, 1);
        let (locked_branch, locked_qc) = make_speculative_branch(&config, context, 9, 2);
        assert_ne!(
            high_branch.last().unwrap().hash(),
            locked_branch.last().unwrap().hash()
        );

        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(high_qc),
            locked_qc: Some(locked_qc),
            voted_views: Vec::new(),
            current_view: 20,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &state).unwrap();
        for block in high_branch.iter().chain(locked_branch.iter()) {
            store.save_block(block).unwrap();
        }

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        replay_speculative_application(&store, &mut app, context, &committee)
            .expect("distinct persisted QC branches must be privately validated");
        assert_eq!(
            app.candidate_count(),
            0,
            "startup must leave branch restoration to durable on-demand recovery"
        );
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }

    #[test]
    fn recovery_rejects_corrupted_speculative_commitment_without_publishing() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let (mut branch, _) = make_speculative_branch(&config, context, 2, 3);
        let mut corrupted = branch.pop().unwrap();
        corrupted.commitment_root[0] ^= 1;
        let corrupted_qc = qc_for(&config, context, &corrupted);

        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(corrupted_qc),
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 3,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store.commit_block(&genesis, &state).unwrap();
        for block in branch {
            store.save_block(&block).unwrap();
        }
        store.save_block(&corrupted).unwrap();

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        let error = replay_speculative_application(&store, &mut app, context, &committee)
            .expect_err("corrupted speculative commitment must fail closed");
        assert!(error.to_string().contains("commitment"));
        assert_eq!(app.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }

    #[test]
    fn persistence_rejects_missing_non_genesis_commitment() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let genesis = Block::genesis(context);
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();

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
        // Genesis is the one allowed artifact-less finalized block.
        store.commit_block(&genesis, &genesis_state).unwrap();

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
        let mut execution_state =
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        execution_state.set_consensus_context(context);
        block.app_hash = <AppState as AppHook>::execute(&mut execution_state, &block);

        let committed_state = ConsensusState {
            committed_height: 1,
            committed_hash: block.hash(),
            ..genesis_state
        };
        let error = store
            .commit_block_with_artifacts(&block, &committed_state, None)
            .expect_err("non-genesis finalized blocks require Commitment v2 artifacts");
        assert!(error
            .to_string()
            .contains("is missing its execution commitment"));
        assert!(store.get(&block.hash()).is_none());
    }

    #[test]
    fn replay_allows_genesis_without_commitment() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let genesis = Block::genesis(context);
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        let state = ConsensusState {
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
        store.commit_block(&genesis, &state).unwrap();

        let app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        replay_committed_application(&store, &mut app, context, &committee)
            .expect("genesis may remain artifact-less");
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
    }

    #[test]
    fn bootstrap_genesis_install_is_atomic_and_idempotent() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let directory = TempDir::new().unwrap();
        let store = RocksDbStore::open(directory.path()).unwrap();

        ensure_genesis_state(&store, context).expect("fresh bootstrap installs genesis");
        let state = store
            .load_consensus_state()
            .unwrap()
            .expect("genesis consensus state is durable");
        let head = store
            .get_committed_head()
            .expect("genesis committed head is durable");
        assert_eq!(state.committed_height, 0);
        assert_eq!(state.committed_hash, Block::genesis(context).hash());
        assert_eq!(head.hash(), state.committed_hash);

        ensure_genesis_state(&store, context).expect("restart bootstrap is idempotent");
        assert_eq!(
            store
                .load_consensus_state()
                .unwrap()
                .expect("state remains durable")
                .committed_hash,
            state.committed_hash
        );
    }

    #[test]
    fn bootstrap_refuses_blocks_without_consensus_state() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let directory = TempDir::new().unwrap();
        let store = RocksDbStore::open(directory.path()).unwrap();
        let genesis = Block::genesis(context);
        store.save(&genesis);

        let error = ensure_genesis_state(&store, context)
            .expect_err("partial durable state must fail closed");
        assert!(error.to_string().contains("no consensus state"));
    }

    #[tokio::test]
    async fn bootstrap_from_peer_imports_http_prefix_and_replays_after_reopen() {
        let (config, context, committee, finalized, child, child_qc) = bootstrap_test_fixture();
        let range_body = serde_json::to_vec(&PeerBlockRangeResponse {
            blocks: finalized.iter().map(export_block).collect(),
            next_height: None,
        })
        .expect("range response");
        let proof_body = serde_json::to_vec(&PeerFinalityProofExport {
            target: export_block(finalized.last().expect("terminal block")),
            child: export_block(&child),
            commit_qc: export_certificate(&child_qc),
        })
        .expect("finality response");
        let (peer, server) = spawn_http_responses(
            vec![
                ("/api/v1/sync/status", br#"{"height":2}"#.to_vec()),
                (
                    "/api/v1/sync/blocks?from=1&to=2&limit=100&includePayload=true",
                    range_body,
                ),
                ("/api/v1/sync/finality/2", proof_body),
            ],
            true,
        )
        .await;

        let directory = TempDir::new().expect("bootstrap tempdir");
        let store = RocksDbStore::open(directory.path()).expect("bootstrap store");
        let mut destination_state =
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        destination_state.set_consensus_context(context);
        destination_state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(config.node_id)),
                    node_id: config.node_id,
                    voting_power: 1,
                    bls_pubkey: config
                        .bls_secret_key()
                        .expect("fixture BLS key")
                        .public_key()
                        .to_bytes()
                        .to_vec(),
                    bls_proof_of_possession: config
                        .bls_secret_key()
                        .expect("fixture BLS key")
                        .create_proof_of_possession(&context.genesis_hash, &config.node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
            )
            .expect("destination staking bootstrap");
        destination_state
            .bind_authoritative_committee(committee.clone())
            .expect("destination committee binding");
        let shared = SharedState::new(destination_state);
        let mut app = CanonicalAppHook::new(shared.clone());

        let result = bootstrap_from_peer(&peer, &store, &mut app, context, &committee).await;
        if result.is_err() {
            server.abort();
        }
        result.expect("HTTP bootstrap must use the verified importer");
        server.await.expect("mock sync server");

        let terminal = finalized.last().expect("terminal block");
        let persisted_state = store
            .load_consensus_state()
            .expect("load imported state")
            .expect("imported state");
        assert_eq!(
            persisted_state.committed_height, terminal.height,
            "finalized prefix must be committed"
        );
        assert_eq!(persisted_state.committed_hash, terminal.hash());
        assert_eq!(
            persisted_state.high_qc.as_ref().map(|qc| qc.block_hash),
            Some(child.hash())
        );
        assert_eq!(
            persisted_state.locked_qc.as_ref().map(|qc| qc.block_hash),
            child.justify.as_ref().map(|qc| qc.block_hash)
        );
        assert!(
            store.get(&child.hash()).is_some(),
            "terminal child must be durably recoverable"
        );
        {
            let app_state = shared.app.read().expect("read imported app");
            assert_eq!(app_state.committed_height(), terminal.height);
            assert_eq!(app_state.compute_full_state_root(), terminal.app_hash);
        }
        assert_eq!(app.candidate_count(), 1);

        drop(app);
        drop(store);

        let reopened = RocksDbStore::open(directory.path()).expect("reopen bootstrap store");
        let mut restarted_state =
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        restarted_state.set_consensus_context(context);
        restarted_state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(config.node_id)),
                    node_id: config.node_id,
                    voting_power: 1,
                    bls_pubkey: config
                        .bls_secret_key()
                        .expect("fixture BLS key")
                        .public_key()
                        .to_bytes()
                        .to_vec(),
                    bls_proof_of_possession: config
                        .bls_secret_key()
                        .expect("fixture BLS key")
                        .create_proof_of_possession(&context.genesis_hash, &config.node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
            )
            .expect("restarted staking bootstrap");
        restarted_state
            .bind_authoritative_committee(committee.clone())
            .expect("restarted committee binding");
        let restarted_shared = SharedState::new(restarted_state);
        let mut restarted = CanonicalAppHook::new(restarted_shared.clone());
        replay_committed_application(&reopened, &mut restarted, context, &committee)
            .expect("replay imported finalized prefix");
        replay_speculative_application(&reopened, &mut restarted, context, &committee)
            .expect("replay imported terminal child");
        {
            let app_state = restarted_shared.app.read().expect("read restarted app");
            assert_eq!(app_state.committed_height(), terminal.height);
            assert_eq!(app_state.compute_full_state_root(), terminal.app_hash);
        }
        assert_eq!(restarted.candidate_count(), 0);
        assert!(reopened.get(&child.hash()).is_some());
        assert_eq!(
            reopened
                .load_consensus_state()
                .expect("reload imported state")
                .expect("reloaded state")
                .committed_hash,
            terminal.hash()
        );

        // Exercise the exact production recovery handshake, not only the
        // standalone replay helpers. The target is already committed, so this
        // performs initialization without entering another consensus round.
        let secret = config.bls_secret_key().expect("fixture BLS key");
        let network = TcpNetwork::new(hyperlicked::network::NetworkConfig {
            node_id: config.node_id,
            listen_addr: "127.0.0.1:0".to_string(),
            peers: Vec::new(),
            require_authenticated_peers: true,
            bls_secret_key: Some(secret.clone()),
            validator_pubkeys: std::collections::HashMap::from([(
                config.node_id,
                secret.public_key(),
            )]),
            gossip_validation: Some(hyperlicked::network::GossipValidationConfig {
                context,
                committee: committee.clone(),
                allow_dev_envelopes: false,
            }),
        })
        .await
        .expect("authenticated recovery network");
        let persisted = Arc::new(reopened);
        let mut runner = ConsensusRunner::new_with_recovery(config, network, persisted)
            .await
            .expect("runner must accept imported recovery state")
            .with_app(restarted);
        runner
            .run_until_committed(terminal.height)
            .await
            .expect("runner initialization must accept imported app/QC state");
    }

    #[tokio::test]
    async fn sync_peer_status_body_is_bounded_without_content_length() {
        let oversized = vec![b'x'; MAX_SYNC_STATUS_RESPONSE_BYTES + 1];
        let (peer, server) =
            spawn_http_responses(vec![("/api/v1/sync/status", oversized)], false).await;
        let error = query_sync_peer_height(&peer)
            .await
            .expect_err("oversized status must fail closed");
        server.await.expect("mock oversized-status server");
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn recovery_rejects_stale_application_epoch_before_genesis_early_return() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        app_state.staking_mut().current_epoch = 1;
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared);

        let error = replay_committed_application(&store, &mut app, context, &committee)
            .expect_err("recovery must reject an application epoch mismatch");
        assert!(error.to_string().contains("application epoch"));
    }

    #[test]
    fn recovery_rejects_pending_validator_update_before_genesis_early_return() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let committee = config.committee().unwrap();
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();

        let mut app_state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        app_state.set_consensus_context(context);
        let mut pending_block = Block::genesis(context);
        pending_block.view = 1;
        <AppState as AppHook>::execute(&mut app_state, &pending_block);
        app_state.staking_mut().current_epoch = 0;
        assert!(app_state.pending_validator_update().is_some());
        let shared = SharedState::new(app_state);
        let mut app = CanonicalAppHook::new(shared);

        let error = replay_committed_application(&store, &mut app, context, &committee)
            .expect_err("recovery must reject pending validator updates");
        assert!(error.to_string().contains("pending validator update"));
    }

    #[test]
    fn persistence_rejects_mismatched_non_genesis_state_root() {
        let config = ConsensusConfig::single_node();
        let context = config.context().unwrap();
        let genesis = Block::genesis(context);
        let dir = TempDir::new().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
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
        let mut execution_state =
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true);
        execution_state.set_consensus_context(context);
        block.app_hash = <AppState as AppHook>::execute(&mut execution_state, &block);

        let source = CanonicalAppHook::new(SharedState::new(
            AppState::new_with_chain_domain_and_dev(context.genesis_hash, true),
        ));
        let commitment = source.derive_execution_commitment(&block).unwrap().unwrap();
        block.commitment_root = commitment.root().unwrap();
        let root = source.preflight_state_root(&block).unwrap().unwrap();
        let bad_root = if root == [0u8; 32] {
            [1u8; 32]
        } else {
            [0u8; 32]
        };
        let committed_state = ConsensusState {
            committed_height: 1,
            committed_hash: block.hash(),
            ..genesis_state
        };
        let error = store
            .commit_block_with_commitment_and_state_root(
                &block,
                &committed_state,
                Some(&commitment),
                Some(&bad_root),
            )
            .expect_err("state-root mismatch must fail closed before persistence");
        assert!(error
            .to_string()
            .contains("state root does not match the authenticated block app hash"));
    }
}

/// Load one persisted QC branch, stopping at the canonical committed head.
/// Every block above that head is returned in parent-to-child order so the
/// application hook can deterministically rebuild its speculative candidates.
fn load_speculative_branch(
    store: &RocksDbStore,
    context: ConsensusContext,
    committee: &Committee,
    committed_head: &Block,
    target_hash: Hash,
) -> Result<Vec<Block>> {
    fn load(
        store: &RocksDbStore,
        context: ConsensusContext,
        committee: &Committee,
        committed_head: &Block,
        target_hash: Hash,
        visited: &mut HashSet<Hash>,
    ) -> Result<Vec<Block>> {
        if !visited.insert(target_hash) {
            anyhow::bail!("persisted speculative branch contains a parent cycle");
        }
        let block = store
            .get(&target_hash)
            .ok_or_else(|| anyhow::anyhow!("persisted QC target block is missing"))?;
        if block.hash() != target_hash {
            anyhow::bail!("persisted QC target hash does not match its block body");
        }
        block
            .validate_context(context)
            .map_err(|error| anyhow::anyhow!("speculative block context mismatch: {error}"))?;
        block
            .validate()
            .map_err(|error| anyhow::anyhow!("speculative block is invalid: {error}"))?;

        if block.height <= committed_head.height {
            let canonical = store.get_by_height(block.height).ok_or_else(|| {
                anyhow::anyhow!(
                    "persisted QC target at committed height {} has no canonical block",
                    block.height
                )
            })?;
            if canonical.hash() != block.hash() {
                anyhow::bail!(
                    "persisted QC target conflicts with the committed chain at height {}",
                    block.height
                );
            }
            return Ok(Vec::new());
        }
        let scheduled_leader = committee.leader(block.view);
        if block.proposer != scheduled_leader {
            anyhow::bail!(
                "persisted speculative block proposer is not the scheduled leader for view {}",
                block.view
            );
        }

        let parent = store
            .get(&block.parent)
            .ok_or_else(|| anyhow::anyhow!("persisted speculative block parent is missing"))?;
        let mut branch = load(
            store,
            context,
            committee,
            committed_head,
            parent.hash(),
            visited,
        )?;
        let expected_parent = branch
            .last()
            .map(Block::hash)
            .unwrap_or_else(|| committed_head.hash());
        if block.parent != expected_parent {
            anyhow::bail!(
                "persisted speculative branch does not connect to committed head at height {}",
                block.height
            );
        }
        let expected_height = committed_head
            .height
            .checked_add(branch.len() as u64 + 1)
            .ok_or_else(|| anyhow::anyhow!("speculative replay height overflows"))?;
        if block.height != expected_height {
            anyhow::bail!(
                "persisted speculative branch skips height {} (expected {})",
                block.height,
                expected_height
            );
        }

        let parent_block = branch.last().cloned().unwrap_or(parent);
        match (block.height, block.justify.as_ref()) {
            (1, None) => {}
            (1, Some(_)) => anyhow::bail!("height-one speculative block carries a QC"),
            (_, Some(justify)) => {
                justify.validate_context(context).map_err(|error| {
                    anyhow::anyhow!("speculative block justification context mismatch: {error}")
                })?;
                if justify.block_hash != block.parent {
                    anyhow::bail!("speculative block justification does not certify its parent");
                }
                if justify.view != parent_block.view {
                    anyhow::bail!("speculative block justification view does not match its parent");
                }
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
                        "speculative block justification certificate is invalid: {error}"
                    )
                })?;
            }
            (_, None) => anyhow::bail!("non-genesis speculative block is missing its QC"),
        }

        branch.push(block);
        Ok(branch)
    }

    load(
        store,
        context,
        committee,
        committed_head,
        target_hash,
        &mut HashSet::new(),
    )
}

/// Validate application branches for blocks referenced by persisted safety
/// QCs. This runs after canonical replay and before consensus starts. Branches
/// are replayed through the hook's private preflight path, but are deliberately
/// not published into the live candidate map: the runner restores the exact
/// branch from its durable journal when it needs that parent for a proposal.
fn replay_speculative_application(
    store: &RocksDbStore,
    app: &mut CanonicalAppHook,
    context: ConsensusContext,
    committee: &Committee,
) -> Result<()> {
    let Some(state) = store.load_consensus_state()? else {
        return Ok(());
    };
    if state.context() != context {
        anyhow::bail!("persisted consensus context does not match speculative replay context");
    }
    let committed_head = store
        .get_committed_head()
        .ok_or_else(|| anyhow::anyhow!("speculative replay has no committed head"))?;
    if committed_head.height != state.committed_height {
        anyhow::bail!("speculative replay committed height metadata does not match state");
    }

    for qc in [state.high_qc.as_ref(), state.locked_qc.as_ref()]
        .into_iter()
        .flatten()
    {
        qc.validate_context(context)
            .map_err(|error| anyhow::anyhow!("speculative QC context mismatch: {error}"))?;
        let target = store
            .get(&qc.block_hash)
            .ok_or_else(|| anyhow::anyhow!("persisted QC target block is missing"))?;
        let qc_app_hash = qc
            .app_hash
            .or_else(|| qc.votes.first().map(|vote| vote.app_hash));
        if qc_app_hash != Some(target.app_hash) {
            anyhow::bail!("persisted QC app hash does not match its target block");
        }
        verify_certificate(
            committee,
            qc,
            context,
            target.view,
            &target.hash(),
            Some(&target.app_hash),
            true,
        )
        .map_err(|error| anyhow::anyhow!("persisted QC certificate is invalid: {error}"))?;
        let branch =
            load_speculative_branch(store, context, committee, &committed_head, qc.block_hash)?;

        // A QC may already point at the committed head, in which case the
        // committed replay above has authenticated the application state and
        // there is no speculative block to preflight. For a branch above the
        // head, preflight the complete ancestor closure and target on a
        // temporary hook. CanonicalAppHook's implementation intentionally
        // stages this replay privately, so malformed application hashes or
        // commitment roots cannot leave partial live candidates behind.
        if let Some((target, ancestors)) = branch.split_last() {
            app.preflight_block_with_speculative_branch(
                context,
                target,
                &committed_head,
                ancestors,
            )
            .map_err(|error| {
                anyhow::anyhow!("failed to validate persisted speculative QC branch: {error}")
            })?;
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let runtime_config = Config::global();
    runtime_config
        .validate_production_safety()
        .map_err(anyhow::Error::msg)
        .context("unsafe runtime configuration")?;
    if !runtime_config.mode.is_dev() {
        anyhow::bail!(
            "hl-node currently requires MODE=dev; production startup remains disabled until the remaining mainnet P0 hardening is complete"
        );
    }

    let args = Args::parse();
    let sync_peer = args.sync_peer.clone();
    let resolved = load_node_runtime_config(&args.genesis, &args.config)
        .context("failed to load node runtime configuration")?;
    let api_addr = resolved
        .api_listen_addr
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid API listen address `{}`", resolved.api_listen_addr))?;
    let hyperlicked::node_config::ResolvedNodeConfig {
        consensus,
        network,
        staking_bootstrap,
        hyck_allocations,
        ..
    } = resolved;
    let network = network.with_dev_envelopes(runtime_config.mode.is_dev());
    let node_id = consensus.node_id;
    let committee = consensus
        .committee()
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to derive trusted active committee")?;
    let context = consensus
        .context()
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to derive consensus context")?;

    let data_dir = args
        .data_dir
        .or_else(|| runtime_config.data_dir.clone().map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(".hyperlicked")
                .join("data")
                .join(hex::encode(context.genesis_hash))
                .join(hex::encode(node_id))
        });
    let persistent_store = Arc::new(RocksDbStore::open(&data_dir).with_context(|| {
        format!(
            "failed to open RocksDB data directory {}",
            data_dir.display()
        )
    })?);

    // A requested bootstrap must establish the canonical genesis boundary
    // before application replay. Without a peer flag, preserve the existing
    // runner-owned fresh-store initialization path unchanged.
    if sync_peer.is_some() {
        ensure_genesis_state(persistent_store.as_ref(), context)?;
    }

    // Canonical recovery starts from a fresh AppState and replays every
    // finalized block through the same AppHook commit path used at runtime.
    // AppSnapshot is intentionally not used: it omits orderbooks.
    let mut app_state =
        AppState::new_with_chain_domain_and_dev(context.genesis_hash, runtime_config.mode.is_dev());
    app_state.set_consensus_context(context);
    app_state
        .bootstrap_static_committee(&committee, &staking_bootstrap)
        .map_err(anyhow::Error::msg)
        .context("failed to bootstrap curated staking records")?;
    let allocation_pairs: Vec<_> = hyck_allocations
        .iter()
        .map(|allocation| (allocation.address.clone(), allocation.amount))
        .collect();
    app_state
        .apply_genesis_hyck_allocations(&allocation_pairs)
        .map_err(anyhow::Error::msg)
        .context("failed to apply genesis HYCK allocations")?;
    app_state
        .bind_authoritative_committee(committee.clone())
        .map_err(anyhow::Error::msg)
        .context("failed to bind authoritative committee to application")?;
    let shared = SharedState::new(app_state);
    let mut app_hook = CanonicalAppHook::new(shared.clone());
    replay_committed_application(
        persistent_store.as_ref(),
        &mut app_hook,
        context,
        &committee,
    )
    .context("failed to replay committed application state")?;
    // When bootstrap is requested, defer speculative replay until after the
    // importer has installed the new terminal safety state. Otherwise the
    // pre-bootstrap QC branch would be validated against the old head and the
    // final child candidate would not be the one handed to consensus.
    if sync_peer.is_none() {
        replay_speculative_application(
            persistent_store.as_ref(),
            &mut app_hook,
            context,
            &committee,
        )
        .context("failed to replay speculative application state")?;
    }

    // Import is deliberately completed before binding either the API listener
    // or the consensus network. A requested sync failure therefore leaves the
    // process stopped and never exposes a partially recovered node.
    if let Some(peer) = sync_peer.as_deref() {
        bootstrap_from_peer(
            peer,
            persistent_store.as_ref(),
            &mut app_hook,
            context,
            &committee,
        )
        .await
        .context("verified startup bootstrap failed")?;

        replay_speculative_application(
            persistent_store.as_ref(),
            &mut app_hook,
            context,
            &committee,
        )
        .context("failed to replay imported speculative application state")?;
    }

    // Bind the API before starting consensus execution.  This keeps a bad API
    // address from leaving a live runner behind.
    let api_listener = tokio::net::TcpListener::bind(api_addr)
        .await
        .with_context(|| format!("failed to bind API listener on {api_addr}"))?;

    let network = TcpNetwork::new(network)
        .await
        .context("failed to create TCP network")?;
    network
        .start()
        .await
        .context("failed to start TCP network")?;
    shared.set_user_transaction_publisher(Arc::new(network.transaction_broadcaster()));
    network
        .wait_for_peers(Duration::from_millis(args.peer_wait_ms))
        .await
        .context("peer readiness failed")?;

    let mut runner =
        ConsensusRunner::new_with_recovery(consensus, network, persistent_store.clone())
            .await
            .context("failed to create consensus runner")?
            .with_app(app_hook);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    let api_store: Arc<dyn PersistentStore + Send + Sync> = persistent_store.clone();
    let app =
        create_router_with_store_and_mode(shared.clone(), Some(api_store), runtime_config.mode)
            .layer(cors)
            .layer(TraceLayer::new_for_http());

    let (api_shutdown_tx, api_shutdown_rx) = oneshot::channel::<()>();
    let api_server = async move {
        axum::serve(
            api_listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = api_shutdown_rx.await;
        })
        .await
    };
    tokio::pin!(api_server);

    let user_transaction_rebroadcast = run_user_transaction_rebroadcast(shared.clone());
    tokio::pin!(user_transaction_rebroadcast);

    let initial_height = runner.committed_height();
    println!(
        "ready node={} epoch={} committee={} committed_height={} api_addr={}",
        hash_short(&node_id),
        context.epoch,
        hash_short(&context.committee_hash),
        initial_height,
        api_addr,
    );

    let result = {
        let consensus = async {
            if let Some(target_height) = args.blocks {
                runner.run_until_committed(target_height).await
            } else {
                runner.run().await
            }
        };
        tokio::pin!(consensus);

        tokio::select! {
            result = &mut consensus => {
                let consensus_result = result;
                api_shutdown_tx
                    .send(())
                    .map_err(|_| anyhow::anyhow!("API shutdown signal was not received"))?;
                api_server
                    .await
                    .context("API server failed during shutdown")?;
                consensus_result
            }
            result = &mut api_server => {
                let result = result.context("API server failed")?;
                anyhow::bail!("API server stopped unexpectedly: {result:?}");
            }
            _ = &mut user_transaction_rebroadcast => {
                anyhow::bail!("user transaction rebroadcast worker stopped unexpectedly");
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to wait for Ctrl-C")?;
                api_shutdown_tx
                    .send(())
                    .map_err(|_| anyhow::anyhow!("API shutdown signal was not received"))?;
                api_server
                    .await
                    .context("API server failed during shutdown")?;
                Ok(())
            }
        }
    };
    result?;

    println!(
        "exit node={} epoch={} committee={} committed_height={}",
        hash_short(&node_id),
        context.epoch,
        hash_short(&context.committee_hash),
        runner.committed_height()
    );

    Ok(())
}
