//! Active Sync Client
//!
//! Background task that polls peers and catches up when behind.
//!
//! ## Architecture
//!
//! ```text
//! ActiveSyncClient
//! ├── peers: Vec<String>         // HTTP endpoints of validators
//! ├── poll_interval: Duration    // How often to check for new blocks
//! └── snapshot_threshold: u64    // Download snapshot if behind by this much
//!
//! Sync Flow:
//! 1. Poll peers for their height
//! 2. If behind by < threshold: download blocks, replay
//! 3. If behind by > threshold: download snapshot first, then blocks
//! 4. Execute and store each block
//! 5. Repeat
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::app::AppState;
use crate::config::Config;
use crate::consensus::AppHook;
use crate::storage::{AppSnapshot, ConsensusState, PersistentStore};
use crate::types::{Block, Certificate};

/// Threshold for downloading snapshot vs replaying blocks
const DEFAULT_SNAPSHOT_THRESHOLD: u64 = 1000;

/// Maximum blocks to request per HTTP call
const MAX_BLOCKS_PER_REQUEST: u64 = 100;

/// Default consecutive failures before blacklisting a peer
const DEFAULT_BLACKLIST_THRESHOLD: u32 = 5;

/// Default blacklist duration in milliseconds (60 seconds)
const DEFAULT_BLACKLIST_DURATION_MS: u64 = 60_000;

/// Peer reputation tracking for blacklisting unreliable peers
#[derive(Debug, Clone, Default)]
pub struct PeerReputation {
    /// Total successful interactions
    pub success_count: u64,
    /// Total failed interactions
    pub failure_count: u64,
    /// Consecutive failures (resets on success)
    pub consecutive_failures: u32,
    /// Blacklisted until this timestamp (ms since epoch), None if not blacklisted
    pub blacklisted_until: Option<u64>,
}

impl PeerReputation {
    /// Check if peer is currently blacklisted
    pub fn is_blacklisted(&self, current_time_ms: u64) -> bool {
        self.blacklisted_until
            .map(|until| current_time_ms < until)
            .unwrap_or(false)
    }
}

/// Manages peer reputations and blacklisting
#[derive(Debug)]
pub struct PeerReputationManager {
    /// Peer URL -> reputation
    peers: HashMap<String, PeerReputation>,
    /// Consecutive failures before blacklisting
    blacklist_threshold: u32,
    /// Duration to blacklist in milliseconds
    blacklist_duration_ms: u64,
}

impl PeerReputationManager {
    /// Create a new reputation manager with default settings
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
            blacklist_threshold: DEFAULT_BLACKLIST_THRESHOLD,
            blacklist_duration_ms: DEFAULT_BLACKLIST_DURATION_MS,
        }
    }

    /// Create with custom threshold and duration
    pub fn with_config(blacklist_threshold: u32, blacklist_duration_ms: u64) -> Self {
        Self {
            peers: HashMap::new(),
            blacklist_threshold,
            blacklist_duration_ms,
        }
    }

    /// Record a successful interaction with a peer
    pub fn record_success(&mut self, peer: &str) {
        let rep = self.peers.entry(peer.to_string()).or_default();
        rep.success_count = rep.success_count.saturating_add(1);
        rep.consecutive_failures = 0;
        // Clear blacklist on success (peer is working again)
        rep.blacklisted_until = None;
    }

    /// Record a failed interaction with a peer
    pub fn record_failure(&mut self, peer: &str, current_time_ms: u64) {
        let rep = self.peers.entry(peer.to_string()).or_default();
        rep.failure_count = rep.failure_count.saturating_add(1);
        rep.consecutive_failures = rep.consecutive_failures.saturating_add(1);

        // Blacklist if threshold exceeded
        if rep.consecutive_failures >= self.blacklist_threshold {
            let until = current_time_ms.saturating_add(self.blacklist_duration_ms);
            rep.blacklisted_until = Some(until);
            tracing::warn!(
                peer,
                consecutive_failures = rep.consecutive_failures,
                blacklisted_until_ms = until,
                "Peer blacklisted due to consecutive failures"
            );
        }
    }

    /// Check if a peer is currently blacklisted
    pub fn is_blacklisted(&self, peer: &str, current_time_ms: u64) -> bool {
        self.peers
            .get(peer)
            .map(|rep| rep.is_blacklisted(current_time_ms))
            .unwrap_or(false)
    }

    /// Get reputation for a peer (for monitoring)
    pub fn get_reputation(&self, peer: &str) -> Option<&PeerReputation> {
        self.peers.get(peer)
    }

    /// Get all non-blacklisted peers from a list
    pub fn filter_available<'a>(
        &self,
        peers: &'a [String],
        current_time_ms: u64,
    ) -> Vec<&'a String> {
        peers
            .iter()
            .filter(|p| !self.is_blacklisted(p, current_time_ms))
            .collect()
    }
}

impl Default for PeerReputationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Sync status response from peer
#[derive(Debug, Deserialize)]
pub struct PeerSyncStatus {
    pub height: u64,
    pub view: u64,
    #[serde(rename = "committedHash")]
    pub committed_hash: String,
    #[serde(rename = "latestSnapshotHeight")]
    pub latest_snapshot_height: Option<u64>,
}

/// Certificate export from peer (QC for block verification)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCertificateExport {
    pub view: u64,
    #[serde(rename = "blockHash")]
    pub block_hash: String, // hex
    /// App state hash that all voters agreed on (required for BLS verification)
    #[serde(rename = "appHash", default)]
    pub app_hash: Option<String>, // hex
    /// Voters who contributed (NodeId hex strings)
    pub voters: Vec<String>,
    /// BLS public keys (hex, 48 bytes each)
    #[serde(rename = "blsPubkeys", default)]
    pub bls_pubkeys: Vec<String>,
    /// Aggregated signature (hex, 96 bytes for BLS)
    #[serde(rename = "aggSignature")]
    pub agg_signature: String,
}

/// Block export from peer
#[derive(Debug, Deserialize)]
pub struct PeerBlockExport {
    pub height: u64,
    pub view: u64,
    pub hash: String,
    #[serde(rename = "parentHash")]
    pub parent_hash: String,
    #[serde(rename = "appHash")]
    pub app_hash: String,
    pub proposer: String,
    pub timestamp: u64,
    pub payload: Option<String>,
    /// QC that justifies this block (proves parent was certified)
    pub justify: Option<PeerCertificateExport>,
}

/// Block range response from peer
#[derive(Debug, Deserialize)]
pub struct PeerBlockRangeResponse {
    pub blocks: Vec<PeerBlockExport>,
    #[serde(rename = "nextHeight")]
    pub next_height: Option<u64>,
}

/// Snapshot metadata from peer
#[derive(Debug, Deserialize)]
pub struct PeerSnapshotMetadata {
    pub height: u64,
    pub timestamp: u64,
    #[serde(rename = "stateHash")]
    pub state_hash: String,
}

/// Snapshot export from peer
#[derive(Debug, Deserialize)]
pub struct PeerSnapshotExport {
    pub metadata: PeerSnapshotMetadata,
    pub data: String, // base64 encoded
}

/// Sync result for each sync attempt
#[derive(Debug)]
pub enum SyncResult {
    /// Already synced, no action needed
    AlreadySynced,
    /// Synced via block replay
    SyncedBlocks { from: u64, to: u64 },
    /// Synced via snapshot + blocks
    SyncedSnapshot { snapshot_height: u64, blocks_to: u64 },
    /// Sync failed (transient error, will retry)
    Failed(String),
    /// State corruption detected (app_hash mismatch after valid QC)
    ///
    /// This is a critical error requiring operator intervention.
    /// The node should NOT process further blocks.
    StateCorrupted(String),
}

/// Active sync client configuration
#[derive(Debug, Clone)]
pub struct ActiveSyncConfig {
    /// Peer HTTP endpoints
    pub peers: Vec<String>,
    /// Poll interval
    pub poll_interval: Duration,
    /// Download snapshot if behind by more than this
    pub snapshot_threshold: u64,
    /// Consecutive failures before blacklisting a peer
    pub blacklist_threshold: u32,
    /// Duration to blacklist in milliseconds
    pub blacklist_duration_ms: u64,
}

impl Default for ActiveSyncConfig {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            poll_interval: Duration::from_secs(1),
            snapshot_threshold: DEFAULT_SNAPSHOT_THRESHOLD,
            blacklist_threshold: DEFAULT_BLACKLIST_THRESHOLD,
            blacklist_duration_ms: DEFAULT_BLACKLIST_DURATION_MS,
        }
    }
}

/// Active sync client
///
/// Runs in background, polling peers and syncing when behind.
pub struct ActiveSyncClient {
    config: ActiveSyncConfig,
    http_client: reqwest::Client,
    reputation: PeerReputationManager,
}

impl ActiveSyncClient {
    pub fn new(config: ActiveSyncConfig) -> Self {
        let reputation = PeerReputationManager::with_config(
            config.blacklist_threshold,
            config.blacklist_duration_ms,
        );
        Self {
            config,
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            reputation,
        }
    }

    /// Run the sync loop
    ///
    /// This function runs forever, polling peers and syncing when behind.
    ///
    /// # Arguments
    /// * `app` - Application state to update
    /// * `store` - Optional persistent storage
    /// * `state_corrupted` - Optional flag to set on Byzantine detection (app_hash mismatch)
    pub async fn run(
        &mut self,
        app: Arc<RwLock<AppState>>,
        store: Option<Arc<dyn PersistentStore + Send + Sync>>,
        state_corrupted: Option<Arc<AtomicBool>>,
    ) {
        if self.config.peers.is_empty() {
            warn!("No peers configured for sync, sync client inactive");
            return;
        }

        info!(
            peers = ?self.config.peers,
            poll_interval_ms = self.config.poll_interval.as_millis(),
            "Starting active sync client"
        );

        loop {
            // Check if state is corrupted - stop syncing if so
            if let Some(ref flag) = state_corrupted {
                if flag.load(Ordering::Relaxed) {
                    warn!(
                        "State is corrupted - sync loop halted. \
                         Operator intervention required: resync from trusted snapshot \
                         or investigate potential Byzantine network."
                    );
                    // Keep sleeping but don't process blocks
                    tokio::time::sleep(self.config.poll_interval).await;
                    continue;
                }
            }

            // Get local height
            let local_height = {
                let app = app.read().await;
                app.committed_height()
            };

            // Poll peers for network height
            match self.get_network_height().await {
                Ok((peer, network_height)) => {
                    if network_height > local_height {
                        info!(
                            local = local_height,
                            network = network_height,
                            behind = network_height - local_height,
                            "Behind network, starting sync"
                        );

                        // Perform sync
                        let result = self
                            .catch_up(&peer, local_height, network_height, &app, &store, &state_corrupted)
                            .await;

                        match result {
                            SyncResult::SyncedBlocks { from, to } => {
                                info!(from, to, "Synced blocks");
                            }
                            SyncResult::SyncedSnapshot {
                                snapshot_height,
                                blocks_to,
                            } => {
                                info!(snapshot_height, blocks_to, "Synced from snapshot");
                            }
                            SyncResult::AlreadySynced => {
                                debug!("Already synced");
                            }
                            SyncResult::Failed(err) => {
                                error!(error = %err, "Sync failed");
                            }
                            SyncResult::StateCorrupted(err) => {
                                error!(error = %err, "State corruption detected - halting sync");
                                // The flag is already set by catch_up_blocks
                            }
                        }
                    } else {
                        debug!(height = local_height, "Synced with network");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to get network height");
                }
            }

            // Wait before next poll
            tokio::time::sleep(self.config.poll_interval).await;
        }
    }

    /// Get current time in milliseconds
    fn current_time_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    /// Get the highest height from any non-blacklisted peer
    async fn get_network_height(&mut self) -> Result<(String, u64), String> {
        let current_time = Self::current_time_ms();
        let available_peers = self.reputation.filter_available(&self.config.peers, current_time);

        if available_peers.is_empty() {
            return Err("No peers available (all blacklisted)".to_string());
        }

        let mut best: Option<(String, u64)> = None;

        for peer in available_peers {
            match self.get_peer_status(peer).await {
                Ok(status) => {
                    self.reputation.record_success(peer);
                    if best.as_ref().map(|(_, h)| *h).unwrap_or(0) < status.height {
                        best = Some((peer.clone(), status.height));
                    }
                }
                Err(e) => {
                    self.reputation.record_failure(peer, current_time);
                    debug!(peer, error = %e, "Failed to get peer status");
                }
            }
        }

        best.ok_or_else(|| "No peers responded".to_string())
    }

    /// Get sync status from a peer
    async fn get_peer_status(&self, peer: &str) -> Result<PeerSyncStatus, String> {
        let url = format!("{}/api/v1/sync/status", peer);
        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        resp.json::<PeerSyncStatus>()
            .await
            .map_err(|e| e.to_string())
    }

    /// Download blocks from a peer
    async fn download_blocks(
        &self,
        peer: &str,
        from: u64,
        to: u64,
    ) -> Result<Vec<Block>, String> {
        let mut all_blocks = Vec::new();
        let mut current_from = from;

        while current_from <= to {
            let url = format!(
                "{}/api/v1/sync/blocks?from={}&to={}&limit={}&includePayload=true",
                peer,
                current_from,
                to,
                MAX_BLOCKS_PER_REQUEST
            );

            let resp = self
                .http_client
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?;

            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status()));
            }

            let response: PeerBlockRangeResponse =
                resp.json().await.map_err(|e| e.to_string())?;

            if response.blocks.is_empty() {
                break;
            }

            // Convert to Block type
            for block_export in response.blocks {
                let block = self.convert_block_export(block_export)?;
                all_blocks.push(block);
            }

            // Check if more blocks available
            match response.next_height {
                Some(next) if next <= to => {
                    current_from = next;
                }
                _ => break,
            }
        }

        Ok(all_blocks)
    }

    /// Convert certificate export to Certificate type
    fn convert_certificate_export(
        &self,
        export: PeerCertificateExport,
    ) -> Result<Certificate, String> {
        let block_hash = hex::decode(&export.block_hash)
            .map_err(|e| format!("Invalid certificate block hash: {}", e))?;
        let block_hash: [u8; 32] = block_hash
            .try_into()
            .map_err(|_| "Invalid certificate block hash length")?;

        // Parse app_hash if present (required for BLS verification)
        let app_hash: Option<[u8; 32]> = match export.app_hash {
            Some(ref hash_hex) => {
                let bytes = hex::decode(hash_hex)
                    .map_err(|e| format!("Invalid certificate app hash: {}", e))?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| "Invalid certificate app hash length")?;
                Some(arr)
            }
            None => None,
        };

        let voters: Result<Vec<[u8; 32]>, String> = export
            .voters
            .iter()
            .map(|v| {
                let bytes = hex::decode(v).map_err(|e| format!("Invalid voter: {}", e))?;
                bytes
                    .try_into()
                    .map_err(|_| "Invalid voter length".to_string())
            })
            .collect();
        let voters = voters?;

        let bls_pubkeys: Result<Vec<Vec<u8>>, String> = export
            .bls_pubkeys
            .iter()
            .map(|pk| hex::decode(pk).map_err(|e| format!("Invalid BLS pubkey: {}", e)))
            .collect();
        let bls_pubkeys = bls_pubkeys?;

        let agg_signature =
            hex::decode(&export.agg_signature).map_err(|e| format!("Invalid signature: {}", e))?;

        Ok(Certificate {
            view: export.view,
            block_hash,
            app_hash,
            votes: vec![], // BLS mode doesn't store individual votes
            voters,
            bls_pubkeys,
            agg_signature,
        })
    }

    /// Convert block export to Block type
    fn convert_block_export(&self, export: PeerBlockExport) -> Result<Block, String> {
        let parent = hex::decode(&export.parent_hash)
            .map_err(|e| format!("Invalid parent hash: {}", e))?;
        let app_hash =
            hex::decode(&export.app_hash).map_err(|e| format!("Invalid app hash: {}", e))?;
        let proposer =
            hex::decode(&export.proposer).map_err(|e| format!("Invalid proposer: {}", e))?;

        let payload = match export.payload {
            Some(b64) => BASE64.decode(&b64).map_err(|e| format!("Invalid payload: {}", e))?,
            None => Vec::new(),
        };

        // Parse justify certificate if present
        let justify = match export.justify {
            Some(cert_export) => Some(self.convert_certificate_export(cert_export)?),
            None => None,
        };

        Ok(Block {
            view: export.view,
            height: export.height,
            parent: parent.try_into().map_err(|_| "Invalid parent hash length")?,
            payload,
            proposer: proposer.try_into().map_err(|_| "Invalid proposer length")?,
            app_hash: app_hash.try_into().map_err(|_| "Invalid app hash length")?,
            timestamp: export.timestamp,
            justify,
        })
    }

    /// Verify a block's QC (justify certificate)
    ///
    /// Returns Ok(()) if:
    /// - Block is genesis (height 1 or less, no QC needed)
    /// - QC is present and valid (BLS signature verifies)
    /// - SKIP_QC_VERIFY is set (dev mode only)
    fn verify_block_qc(&self, block: &Block, parent_hash: &[u8; 32]) -> Result<(), String> {
        // Genesis and first blocks don't need QC verification
        if block.height <= 1 {
            return Ok(());
        }

        // Check if QC verification is skipped (dev mode only)
        if Config::global().skip_qc_verify {
            debug!(height = block.height, "Skipping QC verification (dev mode)");
            return Ok(());
        }

        // Non-genesis blocks must have a justify certificate
        let justify = block
            .justify
            .as_ref()
            .ok_or_else(|| format!("Block {} missing QC", block.height))?;

        // QC must certify the parent block
        if justify.block_hash != *parent_hash {
            return Err(format!(
                "QC block_hash {} doesn't match parent {}",
                hex::encode(&justify.block_hash[..4]),
                hex::encode(&parent_hash[..4])
            ));
        }

        // Verify BLS signature if present
        if justify.is_bls() {
            if !self.verify_bls_certificate(justify) {
                return Err(format!(
                    "Invalid BLS signature on QC for block {}",
                    block.height
                ));
            }
        } else if justify.voters.is_empty() {
            return Err(format!("QC for block {} has no voters", block.height));
        }

        Ok(())
    }

    /// Verify BLS aggregated signature on a certificate
    ///
    /// Performs FULL cryptographic verification:
    /// 1. Certificate has voters and matching pubkeys
    /// 2. Certificate has app_hash (required for verification)
    /// 3. Aggregated BLS signature verifies against the signing message
    ///
    /// The signing message is: view || block_hash || app_hash
    fn verify_bls_certificate(&self, cert: &Certificate) -> bool {
        // Use the Certificate's built-in verification
        match cert.verify_bls() {
            Ok(()) => {
                debug!(
                    view = cert.view,
                    voters = cert.voters.len(),
                    "BLS certificate verified successfully"
                );
                true
            }
            Err(e) => {
                warn!(
                    view = cert.view,
                    error = %e,
                    "BLS certificate verification failed"
                );
                false
            }
        }
    }

    /// Download snapshot from a peer
    async fn download_snapshot(&self, peer: &str) -> Result<(u64, AppSnapshot), String> {
        let url = format!("{}/api/v1/sync/snapshot/latest", peer);
        let resp = self
            .http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }

        let export: PeerSnapshotExport = resp.json().await.map_err(|e| e.to_string())?;

        // Decode base64 snapshot
        let snapshot_bytes = BASE64
            .decode(&export.data)
            .map_err(|e| format!("Invalid snapshot data: {}", e))?;

        // Deserialize snapshot
        let snapshot: AppSnapshot =
            serde_json::from_slice(&snapshot_bytes).map_err(|e| format!("Invalid snapshot: {}", e))?;

        Ok((export.metadata.height, snapshot))
    }

    /// Catch up to network height
    async fn catch_up(
        &self,
        peer: &str,
        local_height: u64,
        network_height: u64,
        app: &Arc<RwLock<AppState>>,
        store: &Option<Arc<dyn PersistentStore + Send + Sync>>,
        state_corrupted: &Option<Arc<AtomicBool>>,
    ) -> SyncResult {
        let behind = network_height.saturating_sub(local_height);

        // If far behind, use snapshot
        if behind > self.config.snapshot_threshold {
            return self
                .catch_up_from_snapshot(peer, network_height, app, store, state_corrupted)
                .await;
        }

        // Otherwise, just replay blocks
        self.catch_up_blocks(peer, local_height + 1, network_height, app, store, state_corrupted)
            .await
    }

    /// Catch up by downloading and replaying blocks
    async fn catch_up_blocks(
        &self,
        peer: &str,
        from: u64,
        to: u64,
        app: &Arc<RwLock<AppState>>,
        store: &Option<Arc<dyn PersistentStore + Send + Sync>>,
        state_corrupted: &Option<Arc<AtomicBool>>,
    ) -> SyncResult {
        // Download blocks
        let blocks = match self.download_blocks(peer, from, to).await {
            Ok(b) => b,
            Err(e) => return SyncResult::Failed(format!("Download blocks failed: {}", e)),
        };

        if blocks.is_empty() {
            return SyncResult::AlreadySynced;
        }

        let first_height = blocks.first().map(|b| b.height).unwrap_or(from);
        let last_height = blocks.last().map(|b| b.height).unwrap_or(to);

        // Track whether QC was verified (for Byzantine detection severity)
        let qc_verified = !Config::global().skip_qc_verify;

        // Execute each block
        for block in &blocks {
            // Verify QC before committing (Byzantine detection)
            if let Err(e) = self.verify_block_qc(block, &block.parent) {
                error!(height = block.height, error = %e, "QC verification failed");
                return SyncResult::Failed(format!(
                    "QC verification failed at height {}: {}",
                    block.height, e
                ));
            }

            let mut app = app.write().await;
            let app_hash = app.execute(block);

            // Verify app hash - reject on mismatch (Byzantine detection)
            if app_hash != block.app_hash {
                let err_msg = format!(
                    "App hash mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(&block.app_hash[..4]),
                    hex::encode(&app_hash[..4])
                );

                if qc_verified {
                    // CRITICAL: QC was valid but app_hash differs
                    // This indicates either:
                    // 1. This node's state is corrupt and needs resync
                    // 2. The validator network is Byzantine (2f+1 colluding)
                    error!(
                        height = block.height,
                        expected = %hex::encode(&block.app_hash[..4]),
                        got = %hex::encode(&app_hash[..4]),
                        "CRITICAL: App hash mismatch after valid QC! \
                         This indicates either: \
                         (1) This node's state is corrupt - resync from trusted snapshot, or \
                         (2) The validator network is Byzantine (2f+1 colluding). \
                         Node is now HALTED - operator intervention required."
                    );

                    // Set the corruption flag for API exposure
                    if let Some(ref flag) = state_corrupted {
                        flag.store(true, Ordering::Relaxed);
                    }

                    return SyncResult::StateCorrupted(err_msg);
                } else {
                    // QC wasn't verified (dev mode), just log warning and fail
                    warn!(
                        height = block.height,
                        expected = %hex::encode(&block.app_hash[..4]),
                        got = %hex::encode(&app_hash[..4]),
                        "App hash mismatch during sync (QC not verified - dev mode)"
                    );
                    return SyncResult::Failed(err_msg);
                }
            }

            // Store block and consensus state
            if let Some(store) = store {
                let consensus_state = ConsensusState {
                    high_qc: block.justify.clone(),
                    locked_qc: None,
                    voted_views: vec![],
                    current_view: block.view,
                    committed_height: block.height,
                    committed_hash: app_hash,
                    consecutive_timeouts: 0,
                    vc_sent_for_view: None,
                };

                if let Err(e) = store.commit_block(block, &consensus_state) {
                    error!(error = %e, height = block.height, "Failed to store block");
                }
            }

            debug!(height = block.height, "Synced block");
        }

        SyncResult::SyncedBlocks {
            from: first_height,
            to: last_height,
        }
    }

    /// Catch up by downloading snapshot first, then replaying blocks
    async fn catch_up_from_snapshot(
        &self,
        peer: &str,
        network_height: u64,
        app: &Arc<RwLock<AppState>>,
        store: &Option<Arc<dyn PersistentStore + Send + Sync>>,
        state_corrupted: &Option<Arc<AtomicBool>>,
    ) -> SyncResult {
        // Download snapshot
        let (snapshot_height, snapshot) = match self.download_snapshot(peer).await {
            Ok(s) => s,
            Err(e) => return SyncResult::Failed(format!("Download snapshot failed: {}", e)),
        };

        info!(height = snapshot_height, "Downloaded snapshot");

        // Store snapshot if we have storage
        if let Some(store) = store {
            if let Err(e) = store.save_snapshot(snapshot_height, &snapshot) {
                error!(error = %e, "Failed to store snapshot");
            }
        }

        // Restore app state from snapshot
        {
            let mut app = app.write().await;
            *app = AppState::from_snapshot(snapshot);
        }

        // Now replay blocks from snapshot to current
        if snapshot_height < network_height {
            let result = self
                .catch_up_blocks(peer, snapshot_height + 1, network_height, app, store, state_corrupted)
                .await;

            match result {
                SyncResult::SyncedBlocks { to, .. } => SyncResult::SyncedSnapshot {
                    snapshot_height,
                    blocks_to: to,
                },
                SyncResult::Failed(e) => SyncResult::Failed(e),
                SyncResult::StateCorrupted(e) => SyncResult::StateCorrupted(e),
                _ => SyncResult::SyncedSnapshot {
                    snapshot_height,
                    blocks_to: snapshot_height,
                },
            }
        } else {
            SyncResult::SyncedSnapshot {
                snapshot_height,
                blocks_to: snapshot_height,
            }
        }
    }

    /// Sync once (non-blocking version for testing)
    pub async fn sync_once(
        &mut self,
        app: &Arc<RwLock<AppState>>,
        store: &Option<Arc<dyn PersistentStore + Send + Sync>>,
    ) -> SyncResult {
        self.sync_once_with_corruption_flag(app, store, &None).await
    }

    /// Sync once with corruption flag (non-blocking version)
    pub async fn sync_once_with_corruption_flag(
        &mut self,
        app: &Arc<RwLock<AppState>>,
        store: &Option<Arc<dyn PersistentStore + Send + Sync>>,
        state_corrupted: &Option<Arc<AtomicBool>>,
    ) -> SyncResult {
        if self.config.peers.is_empty() {
            return SyncResult::Failed("No peers configured".to_string());
        }

        let local_height = {
            let app = app.read().await;
            app.committed_height()
        };

        match self.get_network_height().await {
            Ok((peer, network_height)) => {
                if network_height > local_height {
                    self.catch_up(&peer, local_height, network_height, app, store, state_corrupted)
                        .await
                } else {
                    SyncResult::AlreadySynced
                }
            }
            Err(e) => SyncResult::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_config_default() {
        let config = ActiveSyncConfig::default();
        assert!(config.peers.is_empty());
        assert_eq!(config.snapshot_threshold, DEFAULT_SNAPSHOT_THRESHOLD);
        assert_eq!(config.blacklist_threshold, DEFAULT_BLACKLIST_THRESHOLD);
        assert_eq!(config.blacklist_duration_ms, DEFAULT_BLACKLIST_DURATION_MS);
    }

    #[test]
    fn test_sync_client_creation() {
        let config = ActiveSyncConfig {
            peers: vec!["http://localhost:8080".to_string()],
            poll_interval: Duration::from_secs(1),
            snapshot_threshold: 500,
            blacklist_threshold: 3,
            blacklist_duration_ms: 30_000,
        };
        let _client = ActiveSyncClient::new(config);
    }

    #[test]
    fn test_peer_reputation_success() {
        let mut manager = PeerReputationManager::new();
        let peer = "http://localhost:8080";

        manager.record_success(peer);
        let rep = manager.get_reputation(peer).unwrap();
        assert_eq!(rep.success_count, 1);
        assert_eq!(rep.failure_count, 0);
        assert_eq!(rep.consecutive_failures, 0);
    }

    #[test]
    fn test_peer_reputation_failure_blacklist() {
        let mut manager = PeerReputationManager::with_config(3, 60_000);
        let peer = "http://localhost:8080";
        let current_time = 1000000;

        // 2 failures - not yet blacklisted
        manager.record_failure(peer, current_time);
        manager.record_failure(peer, current_time);
        assert!(!manager.is_blacklisted(peer, current_time));

        // 3rd failure - now blacklisted
        manager.record_failure(peer, current_time);
        assert!(manager.is_blacklisted(peer, current_time));

        // Still blacklisted after 30 seconds
        assert!(manager.is_blacklisted(peer, current_time + 30_000));

        // Not blacklisted after 60 seconds
        assert!(!manager.is_blacklisted(peer, current_time + 60_001));
    }

    #[test]
    fn test_peer_reputation_success_clears_blacklist() {
        let mut manager = PeerReputationManager::with_config(2, 60_000);
        let peer = "http://localhost:8080";
        let current_time = 1000000;

        // Blacklist the peer
        manager.record_failure(peer, current_time);
        manager.record_failure(peer, current_time);
        assert!(manager.is_blacklisted(peer, current_time));

        // Success clears blacklist
        manager.record_success(peer);
        assert!(!manager.is_blacklisted(peer, current_time));

        let rep = manager.get_reputation(peer).unwrap();
        assert_eq!(rep.consecutive_failures, 0);
        assert_eq!(rep.failure_count, 2); // Total failures still tracked
    }

    #[test]
    fn test_filter_available_peers() {
        let mut manager = PeerReputationManager::with_config(2, 60_000);
        let current_time = 1000000;

        let peers = vec![
            "http://peer1:8080".to_string(),
            "http://peer2:8080".to_string(),
            "http://peer3:8080".to_string(),
        ];

        // Blacklist peer2
        manager.record_failure(&peers[1], current_time);
        manager.record_failure(&peers[1], current_time);

        let available = manager.filter_available(&peers, current_time);
        assert_eq!(available.len(), 2);
        assert!(available.contains(&&peers[0]));
        assert!(!available.contains(&&peers[1]));
        assert!(available.contains(&&peers[2]));
    }
}
