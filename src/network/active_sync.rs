//! Verified active block download.
//!
//! Active sync is deliberately a transport component, not a second state
//! machine.  A peer can provide bytes and a height hint, but it cannot choose
//! the consensus context, committee, quorum keys, or application state that
//! this client trusts.  Callers import the returned blocks through the normal
//! consensus/store path.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::debug;

use crate::consensus::verify_certificate;
use crate::types::{
    Block, Certificate, Committee, ConsensusContext, Hash, NodeId, MAX_SYNC_RESPONSE_BYTES,
};

/// Maximum number of blocks requested in one HTTP response.
const MAX_BLOCKS_PER_REQUEST: u64 = 100;

/// Maximum number of blocks returned by one active-sync download window.
///
/// The peer response is paginated, so the per-page limit above is not enough
/// to bound the `Vec<Block>` retained by the caller.  Keep this fixed and
/// conservative until a streaming/import path replaces the returned vector.
const MAX_ACTIVE_SYNC_BLOCKS: u64 = 1_000;

/// Hard cap for the complete JSON response envelope before deserialization.
///
/// `Block::validate` still enforces the 10 MB per-block payload bound.  This
/// separate batch bound prevents a peer from making the downloader allocate
/// an unbounded JSON/base64 envelope before those per-block checks run.
const MAX_BLOCK_RANGE_RESPONSE_BYTES: usize = MAX_SYNC_RESPONSE_BYTES;

/// Maximum raw JSON bytes retained across all pages of one active-sync
/// download.  A page is still capped independently by
/// [`MAX_BLOCK_RANGE_RESPONSE_BYTES`].
const MAX_ACTIVE_SYNC_TOTAL_BYTES: usize = 4 * MAX_BLOCK_RANGE_RESPONSE_BYTES;

/// Certificate export from a peer.
///
/// The committee and public-key fields are claims made by the peer.  They are
/// parsed for exact sizes and are accepted only when the trusted committee
/// verifier proves that they match the local committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCertificateExport {
    pub epoch: u64,
    #[serde(rename = "committeeHash")]
    pub committee_hash: String,
    #[serde(rename = "genesisHash")]
    pub genesis_hash: String,
    pub view: u64,
    #[serde(rename = "blockHash")]
    pub block_hash: String,
    #[serde(rename = "appHash", default)]
    pub app_hash: Option<String>,
    pub voters: Vec<String>,
    #[serde(rename = "blsPubkeys", default)]
    pub bls_pubkeys: Vec<String>,
    #[serde(rename = "aggSignature")]
    pub agg_signature: String,
}

/// Block export from a peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerBlockExport {
    pub epoch: u64,
    #[serde(rename = "committeeHash")]
    pub committee_hash: String,
    #[serde(rename = "genesisHash")]
    pub genesis_hash: String,
    pub height: u64,
    pub view: u64,
    /// Claimed block hash.  This is checked against `Block::hash()` exactly.
    pub hash: String,
    #[serde(rename = "parentHash")]
    pub parent_hash: String,
    #[serde(rename = "appHash")]
    pub app_hash: String,
    #[serde(rename = "commitmentRoot")]
    pub commitment_root: String,
    pub proposer: String,
    pub timestamp: u64,
    pub payload: Option<String>,
    pub justify: Option<PeerCertificateExport>,
}

/// Block range response from a peer.
#[derive(Debug, Serialize, Deserialize)]
pub struct PeerBlockRangeResponse {
    pub blocks: Vec<PeerBlockExport>,
    #[serde(rename = "nextHeight")]
    pub next_height: Option<u64>,
}

/// Two-chain finality proof response from a peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerFinalityProofExport {
    pub target: PeerBlockExport,
    pub child: PeerBlockExport,
    #[serde(rename = "commitQc")]
    pub commit_qc: PeerCertificateExport,
}

/// A finality proof that has passed local block, context, timestamp, and
/// trusted-committee QC verification.
#[derive(Debug, Clone)]
pub struct VerifiedFinalityProof {
    pub target: Block,
    pub child: Block,
    pub commit_qc: Certificate,
}

/// Downloaded blocks whose terminal block is accompanied by a verified
/// two-chain finality proof.  This is the only active-sync result suitable for
/// a finalized import boundary; the legacy range method below remains a
/// transport-compatible, non-finality API.
#[derive(Debug, Clone)]
pub struct VerifiedFinalizedBatch {
    pub blocks: Vec<Block>,
    pub proof: VerifiedFinalityProof,
}

/// Result type retained for network API compatibility.
///
/// Active sync no longer mutates state, persists blocks, or restores
/// snapshots.  The verified blocks themselves are returned by
/// [`ActiveSyncClient::download_verified_blocks`].  That method does not
/// obtain a finality proof and must not be used as a finalized import proof.
#[derive(Debug, PartialEq, Eq)]
pub enum SyncResult {
    AlreadySynced,
    VerifiedBlocks { from: u64, to: u64 },
    Failed(String),
}

/// Configuration for a verified active-sync client.
///
/// There is intentionally no `Default` implementation and no constructor
/// that omits the trusted context or committee.  The trusted values are kept
/// private so callers cannot construct a client with an implicitly empty
/// committee through a struct literal.
#[derive(Debug, Clone)]
pub struct ActiveSyncConfig {
    /// Optional peer allow-list retained for callers that manage endpoints.
    /// Peer status is never used as a trust anchor.
    pub peers: Vec<String>,
    /// HTTP request timeout for block-range downloads.
    pub request_timeout: Duration,
    trusted_context: ConsensusContext,
    committee: Committee,
}

impl ActiveSyncConfig {
    /// Construct active-sync configuration from a caller-provided trust root.
    pub fn try_new(
        peers: Vec<String>,
        request_timeout: Duration,
        trusted_context: ConsensusContext,
        committee: Committee,
    ) -> Result<Self, String> {
        if request_timeout.is_zero() {
            return Err("active-sync request timeout must be non-zero".to_string());
        }
        committee.validate_context(trusted_context)?;
        Ok(Self {
            peers,
            request_timeout,
            trusted_context,
            committee,
        })
    }

    pub fn trusted_context(&self) -> ConsensusContext {
        self.trusted_context
    }

    pub fn committee(&self) -> &Committee {
        &self.committee
    }
}

/// Verified block downloader.
pub struct ActiveSyncClient {
    trusted_context: ConsensusContext,
    committee: Committee,
    http_client: reqwest::Client,
}

impl ActiveSyncClient {
    /// Create a client only from a configuration that carries a valid trust
    /// root.  The context and committee are cloned into immutable client
    /// fields; no peer response can replace them.
    pub fn try_new(config: ActiveSyncConfig) -> Result<Self, String> {
        config.committee.validate_context(config.trusted_context)?;
        let http_client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| format!("failed to create HTTP client: {error}"))?;
        Ok(Self {
            trusted_context: config.trusted_context,
            committee: config.committee,
            http_client,
        })
    }

    /// Download and verify a block range using a caller-trusted local anchor.
    ///
    /// This method never executes transactions, replaces an `AppState`, saves
    /// a snapshot, or commits to storage.  The caller owns import and state
    /// transition after all blocks have passed verification.  This is a
    /// compatibility transport method only; it does not prove finality and
    /// must not be used as a finalized import boundary.
    pub async fn download_verified_blocks(
        &self,
        peer: &str,
        trusted_anchor: &Block,
        to_height: u64,
    ) -> Result<Vec<Block>, String> {
        validate_anchor(trusted_anchor, self.trusted_context, &self.committee)?;

        if to_height < trusted_anchor.height {
            return Err(format!(
                "target height {} is below trusted anchor height {}",
                to_height, trusted_anchor.height
            ));
        }
        if to_height == trusted_anchor.height {
            return Ok(Vec::new());
        }

        let from_height = trusted_anchor
            .height
            .checked_add(1)
            .ok_or_else(|| "trusted anchor height overflow".to_string())?;
        let requested_blocks = to_height - trusted_anchor.height;
        ensure_block_budget(requested_blocks, MAX_ACTIVE_SYNC_BLOCKS)?;
        let blocks = self.download_blocks(peer, from_height, to_height).await?;
        verify_block_batch(
            &blocks,
            trusted_anchor,
            to_height,
            self.trusted_context,
            &self.committee,
        )?;
        Ok(blocks)
    }

    /// Download a block range and require a verified two-chain proof for its
    /// terminal block before returning it as finalized-batch data.
    pub async fn download_verified_finalized_batch(
        &self,
        peer: &str,
        trusted_anchor: &Block,
        to_height: u64,
    ) -> Result<VerifiedFinalizedBatch, String> {
        let blocks = self
            .download_verified_blocks(peer, trusted_anchor, to_height)
            .await?;
        let terminal = blocks.last().ok_or_else(|| {
            "finalized batch must contain at least one downloaded block".to_string()
        })?;

        let url = format!(
            "{}/api/v1/sync/finality/{}",
            peer.trim_end_matches('/'),
            terminal.height
        );
        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|error| format!("download finality proof failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "download finality proof failed: HTTP {}",
                response.status()
            ));
        }
        let (export, _) =
            read_bounded_json::<PeerFinalityProofExport>(response, MAX_BLOCK_RANGE_RESPONSE_BYTES)
                .await?;
        let proof = convert_finality_proof_export(export)?;

        if proof.target.hash() != terminal.hash() || proof.target.justify != terminal.justify {
            return Err(
                "finality proof target does not exactly match downloaded terminal block"
                    .to_string(),
            );
        }
        verify_finality_proof(&proof, self.trusted_context, &self.committee)?;

        Ok(VerifiedFinalizedBatch { blocks, proof })
    }

    async fn download_blocks(&self, peer: &str, from: u64, to: u64) -> Result<Vec<Block>, String> {
        self.download_blocks_with_limits(
            peer,
            from,
            to,
            MAX_ACTIVE_SYNC_BLOCKS,
            MAX_ACTIVE_SYNC_TOTAL_BYTES,
        )
        .await
    }

    async fn download_blocks_with_limits(
        &self,
        peer: &str,
        from: u64,
        to: u64,
        max_blocks: u64,
        max_bytes: usize,
    ) -> Result<Vec<Block>, String> {
        let requested_blocks = inclusive_block_count(from, to)?;
        ensure_block_budget(requested_blocks, max_blocks)?;
        if max_bytes == 0 {
            return Err("active-sync total response byte budget must be non-zero".to_string());
        }

        let capacity = usize::try_from(requested_blocks)
            .map_err(|_| "active-sync requested block count overflows usize".to_string())?;
        let mut blocks = Vec::with_capacity(capacity);
        let mut next_from = from;
        let mut total_blocks = 0u64;
        let mut total_bytes = 0usize;

        while next_from <= to {
            if total_blocks >= max_blocks {
                return Err(format!(
                    "active-sync block budget of {} blocks exhausted before pagination completed",
                    max_blocks
                ));
            }
            let remaining_bytes = max_bytes.checked_sub(total_bytes).ok_or_else(|| {
                format!(
                    "active-sync total response byte budget of {} bytes exhausted",
                    max_bytes
                )
            })?;
            if remaining_bytes == 0 {
                return Err(format!(
                    "active-sync total response byte budget of {} bytes exhausted",
                    max_bytes
                ));
            }

            let url = format!(
                "{}/api/v1/sync/blocks?from={}&to={}&limit={}&includePayload=true",
                peer.trim_end_matches('/'),
                next_from,
                to,
                MAX_BLOCKS_PER_REQUEST
            );
            let response = self
                .http_client
                .get(url)
                .send()
                .await
                .map_err(|error| format!("download blocks failed: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "download blocks failed: HTTP {}",
                    response.status()
                ));
            }

            let (response, page_bytes): (PeerBlockRangeResponse, usize) =
                read_bounded_json(response, remaining_bytes).await?;
            if !total_budget_fits(total_bytes, page_bytes, max_bytes) {
                return Err(format!(
                    "active-sync total response byte budget of {} bytes exceeded",
                    max_bytes
                ));
            }
            total_bytes += page_bytes;
            if response.blocks.is_empty() {
                return Err(format!(
                    "peer returned an empty block page at requested height {next_from}"
                ));
            }
            if response.blocks.len() as u64 > MAX_BLOCKS_PER_REQUEST {
                return Err(format!(
                    "peer returned {} blocks, maximum is {}",
                    response.blocks.len(),
                    MAX_BLOCKS_PER_REQUEST
                ));
            }
            let page_blocks = response.blocks.len() as u64;
            if !block_budget_fits(total_blocks, page_blocks, max_blocks) {
                return Err(format!(
                    "active-sync block budget of {} blocks exceeded",
                    max_blocks
                ));
            }
            total_blocks += page_blocks;

            let page_start = response.blocks[0].height;
            if page_start != next_from {
                return Err(format!(
                    "block page starts at height {page_start}, expected {next_from}"
                ));
            }

            let mut expected_height = next_from;
            for export in response.blocks {
                if export.height != expected_height || export.height > to {
                    return Err(format!(
                        "block page has non-sequential/out-of-range height {}, expected {}..{}",
                        export.height, expected_height, to
                    ));
                }
                blocks.push(convert_block_export(export)?);
                expected_height = expected_height
                    .checked_add(1)
                    .ok_or_else(|| "block height overflow".to_string())?;
            }

            let last_height = expected_height - 1;
            match response.next_height {
                Some(next) => {
                    if next != expected_height || next <= next_from || next > to {
                        return Err(format!(
                            "invalid pagination nextHeight {next}; expected {}..{}",
                            expected_height, to
                        ));
                    }
                    next_from = next;
                }
                None if last_height == to => break,
                None => {
                    return Err(format!(
                        "peer ended pagination at height {last_height}, requested through {to}"
                    ));
                }
            }
        }

        Ok(blocks)
    }
}

/// Read a response body without trusting `Content-Length` as the only size
/// check.  Chunked responses and incorrect length headers are both bounded by
/// checking every received chunk before appending it to the deserialization
/// buffer.
async fn read_bounded_json<T>(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<(T, usize), String>
where
    T: DeserializeOwned,
{
    let max_bytes = max_bytes.min(MAX_BLOCK_RANGE_RESPONSE_BYTES);
    if max_bytes == 0 {
        return Err("block range response byte limit must be non-zero".to_string());
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(format!(
            "block range response exceeds {} byte limit",
            max_bytes
        ));
    }

    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed reading block range response: {error}"))?
    {
        if !response_chunk_fits_with_limit(body.len(), chunk.len(), max_bytes) {
            return Err(format!(
                "block range response exceeds {} byte limit",
                max_bytes
            ));
        }
        body.extend_from_slice(&chunk);
    }

    let body_len = body.len();
    let value = serde_json::from_slice(&body)
        .map_err(|error| format!("invalid block range response: {error}"))?;
    Ok((value, body_len))
}

#[cfg(test)]
fn response_chunk_fits(received: usize, incoming: usize) -> bool {
    response_chunk_fits_with_limit(received, incoming, MAX_BLOCK_RANGE_RESPONSE_BYTES)
}

fn response_chunk_fits_with_limit(received: usize, incoming: usize, limit: usize) -> bool {
    received <= limit && incoming <= limit.saturating_sub(received)
}

fn total_budget_fits(received: usize, incoming: usize, limit: usize) -> bool {
    response_chunk_fits_with_limit(received, incoming, limit)
}

fn block_budget_fits(received: u64, incoming: u64, limit: u64) -> bool {
    received <= limit && incoming <= limit.saturating_sub(received)
}

fn inclusive_block_count(from: u64, to: u64) -> Result<u64, String> {
    to.checked_sub(from)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| format!("invalid active-sync block range {from}..{to}"))
}

fn ensure_block_budget(requested: u64, max_blocks: u64) -> Result<(), String> {
    if requested > max_blocks {
        return Err(format!(
            "active-sync requested block span of {} exceeds {} block limit",
            requested, max_blocks
        ));
    }
    Ok(())
}

fn parse_exact_hash(value: &str, field: &str) -> Result<Hash, String> {
    let bytes = hex::decode(value).map_err(|error| format!("invalid {field}: {error}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("{field} must be exactly 32 bytes"))
}

fn parse_exact_node_id(value: &str, field: &str) -> Result<NodeId, String> {
    parse_exact_hash(value, field)
}

fn parse_bls_pubkey(value: &str) -> Result<Vec<u8>, String> {
    let bytes = hex::decode(value).map_err(|error| format!("invalid BLS public key: {error}"))?;
    if bytes.len() != 48 {
        return Err("BLS public key must be exactly 48 bytes".to_string());
    }
    Ok(bytes)
}

fn convert_certificate_export(export: PeerCertificateExport) -> Result<Certificate, String> {
    let voters = export
        .voters
        .iter()
        .map(|voter| parse_exact_node_id(voter, "certificate voter"))
        .collect::<Result<Vec<_>, _>>()?;
    let bls_pubkeys = export
        .bls_pubkeys
        .iter()
        .map(String::as_str)
        .map(parse_bls_pubkey)
        .collect::<Result<Vec<_>, _>>()?;
    let app_hash = export
        .app_hash
        .as_deref()
        .map(|value| parse_exact_hash(value, "certificate app hash"))
        .transpose()?;
    let agg_signature = hex::decode(&export.agg_signature)
        .map_err(|error| format!("invalid aggregate signature: {error}"))?;

    Ok(Certificate {
        epoch: export.epoch,
        committee_hash: parse_exact_hash(&export.committee_hash, "certificate committee hash")?,
        genesis_hash: parse_exact_hash(&export.genesis_hash, "certificate genesis hash")?,
        view: export.view,
        block_hash: parse_exact_hash(&export.block_hash, "certificate block hash")?,
        app_hash,
        votes: vec![],
        voters,
        bls_pubkeys,
        agg_signature,
    })
}

fn convert_block_export(export: PeerBlockExport) -> Result<Block, String> {
    let payload = match export.payload {
        Some(payload) => BASE64
            .decode(payload)
            .map_err(|error| format!("invalid block payload: {error}"))?,
        None => Vec::new(),
    };
    let block = Block {
        epoch: export.epoch,
        committee_hash: parse_exact_hash(&export.committee_hash, "block committee hash")?,
        genesis_hash: parse_exact_hash(&export.genesis_hash, "block genesis hash")?,
        height: export.height,
        view: export.view,
        parent: parse_exact_hash(&export.parent_hash, "block parent hash")?,
        payload,
        proposer: parse_exact_node_id(&export.proposer, "block proposer")?,
        commitment_root: parse_exact_hash(&export.commitment_root, "block commitment root")?,
        app_hash: parse_exact_hash(&export.app_hash, "block app hash")?,
        timestamp: export.timestamp,
        justify: export.justify.map(convert_certificate_export).transpose()?,
    };
    block
        .validate()
        .map_err(|error| format!("invalid block: {error}"))?;

    let claimed_hash = parse_exact_hash(&export.hash, "claimed block hash")?;
    let actual_hash = block.hash();
    if claimed_hash != actual_hash {
        return Err(format!(
            "claimed block hash {} does not match reconstructed hash {}",
            hex::encode(claimed_hash),
            hex::encode(actual_hash)
        ));
    }
    Ok(block)
}

fn convert_finality_proof_export(
    export: PeerFinalityProofExport,
) -> Result<VerifiedFinalityProof, String> {
    Ok(VerifiedFinalityProof {
        target: convert_block_export(export.target)?,
        child: convert_block_export(export.child)?,
        commit_qc: convert_certificate_export(export.commit_qc)?,
    })
}

fn verify_finality_proof(
    proof: &VerifiedFinalityProof,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    proof.target.validate_context(expected_context)?;
    proof
        .target
        .validate()
        .map_err(|error| format!("invalid finality target: {error}"))?;
    if proof.target.height == 0 {
        let genesis = Block::genesis(expected_context);
        if proof.target.hash() != genesis.hash() || proof.target.justify.is_some() {
            return Err("finality target is not canonical genesis".to_string());
        }
    } else {
        if committee.member(&proof.target.proposer).is_none() {
            return Err("finality target proposer is not in the trusted committee".to_string());
        }
        if committee.leader(proof.target.view) != proof.target.proposer {
            return Err("finality target proposer is not the scheduled leader".to_string());
        }
    }

    proof.child.validate_context(expected_context)?;
    proof
        .child
        .validate()
        .map_err(|error| format!("invalid finality child: {error}"))?;
    if committee.member(&proof.child.proposer).is_none() {
        return Err("finality child proposer is not in the trusted committee".to_string());
    }
    if committee.leader(proof.child.view) != proof.child.proposer {
        return Err("finality child proposer is not the scheduled leader".to_string());
    }
    let expected_height = proof
        .target
        .height
        .checked_add(1)
        .ok_or_else(|| "finality child height overflows u64".to_string())?;
    if proof.child.height != expected_height || proof.child.parent != proof.target.hash() {
        return Err("finality child is not the exact child of target".to_string());
    }
    proof
        .child
        .validate_parent_timestamp(proof.target.timestamp)
        .map_err(|error| format!("finality child timestamp is invalid: {error}"))?;

    let justify = proof
        .child
        .justify
        .as_ref()
        .ok_or_else(|| "finality child is missing the QC for target".to_string())?;
    verify_certificate(
        committee,
        justify,
        expected_context,
        proof.target.view,
        &proof.target.hash(),
        Some(&proof.target.app_hash),
        true,
    )
    .map_err(|error| format!("finality child justification is invalid: {error}"))?;
    verify_certificate(
        committee,
        &proof.commit_qc,
        expected_context,
        proof.child.view,
        &proof.child.hash(),
        Some(&proof.child.app_hash),
        true,
    )
    .map_err(|error| format!("finality commit QC is invalid: {error}"))?;
    Ok(())
}

fn validate_anchor(
    anchor: &Block,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    anchor.validate_context(expected_context)?;
    anchor
        .validate()
        .map_err(|error| format!("trusted anchor is invalid: {error}"))?;

    if anchor.height == 0 {
        let canonical_genesis = Block::genesis(expected_context);
        if anchor.hash() != canonical_genesis.hash() || anchor.justify.is_some() {
            return Err("trusted anchor is not the canonical genesis block".to_string());
        }
        return Ok(());
    }

    if committee.member(&anchor.proposer).is_none() {
        return Err("trusted anchor proposer is not in the trusted committee".to_string());
    }
    if committee.leader(anchor.view) != anchor.proposer {
        return Err("trusted anchor proposer is not the scheduled committee leader".to_string());
    }
    if anchor.height == 1 {
        if anchor.justify.is_some() {
            return Err("height-1 trusted anchor must not carry a justification".to_string());
        }
        let canonical_genesis = Block::genesis(expected_context);
        if anchor.parent != canonical_genesis.hash() {
            return Err("height-1 trusted anchor does not point to canonical genesis".to_string());
        }
    } else if anchor.justify.is_none() {
        return Err("trusted anchor above height 1 is missing a justification".to_string());
    } else if let Some(justify) = anchor.justify.as_ref() {
        // The caller supplies a trusted non-genesis anchor.  Without the
        // parent block we cannot independently check its QC's parent view or
        // app hash, but we can still enforce its context, target hash, and
        // required app-hash structure before using it as a batch boundary.
        justify.validate_context(expected_context)?;
        if justify.block_hash != anchor.parent {
            return Err("trusted anchor justification does not target its parent".to_string());
        }
        if justify.app_hash.is_none() {
            return Err("trusted anchor justification is missing app hash".to_string());
        }
    }
    Ok(())
}

/// Verify a complete downloaded batch without touching application state.
///
/// This helper is intentionally pure so malformed peer data can be tested
/// without starting an HTTP server or constructing an application runtime.
fn verify_block_batch(
    blocks: &[Block],
    trusted_anchor: &Block,
    to_height: u64,
    expected_context: ConsensusContext,
    committee: &Committee,
) -> Result<(), String> {
    let expected_count = to_height
        .checked_sub(trusted_anchor.height)
        .ok_or_else(|| "target height is below trusted anchor".to_string())?;
    if blocks.len() as u64 != expected_count {
        return Err(format!(
            "downloaded {} blocks, expected {}",
            blocks.len(),
            expected_count
        ));
    }

    let mut parent = trusted_anchor;
    for (index, block) in blocks.iter().enumerate() {
        let expected_height = trusted_anchor
            .height
            .checked_add(index as u64 + 1)
            .ok_or_else(|| "block height overflow".to_string())?;
        block.validate_context(expected_context)?;
        block
            .validate()
            .map_err(|error| format!("invalid block at height {}: {error}", block.height))?;
        if block.height != expected_height {
            return Err(format!(
                "non-sequential block height {}, expected {}",
                block.height, expected_height
            ));
        }
        if block.parent != parent.hash() {
            return Err(format!(
                "block {} parent does not match trusted previous block",
                block.height
            ));
        }
        block
            .validate_parent_timestamp(parent.timestamp)
            .map_err(|error| {
                format!(
                    "invalid parent timestamp at height {}: {error}",
                    block.height
                )
            })?;
        if committee.member(&block.proposer).is_none() {
            return Err(format!(
                "block {} proposer is not in the trusted committee",
                block.height
            ));
        }
        if committee.leader(block.view) != block.proposer {
            return Err(format!(
                "block {} proposer is not the scheduled committee leader",
                block.height
            ));
        }

        if block.height == 1 {
            if block.justify.is_some() {
                return Err("height-1 block must not carry a justification".to_string());
            }
        } else {
            let justify = block
                .justify
                .as_ref()
                .ok_or_else(|| format!("block {} is missing a justification", block.height))?;
            verify_certificate(
                committee,
                justify,
                expected_context,
                parent.view,
                &parent.hash(),
                Some(&parent.app_hash),
                true,
            )
            .map_err(|error| {
                format!("invalid justification at height {}: {error}", block.height)
            })?;
        }

        debug!(height = block.height, "verified active-sync block");
        parent = block;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::form_certificate;
    use crate::crypto::bls::{aggregate_signatures, BlsSecretKey};
    use crate::types::{CommitmentV2, ConsensusConfig, Vote};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct Fixture {
        committee: Committee,
        context: ConsensusContext,
        voters: Vec<NodeId>,
        secrets: Vec<BlsSecretKey>,
        genesis: Block,
    }

    fn fixture_with_powers(powers: &[u64]) -> Fixture {
        let voters: Vec<NodeId> = (1..=powers.len()).map(|id| [id as u8; 32]).collect();
        let secrets: Vec<_> = (1..=powers.len())
            .map(|id| {
                let mut seed = [0u8; 32];
                seed[0] = id as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: voters[0],
            validators: voters.clone(),
            voting_powers: powers.to_vec(),
            view_timeout_ms: 1000,
            bls_pubkeys: secrets
                .iter()
                .map(|secret| secret.public_key().to_bytes().to_vec())
                .collect(),
            bls_secret_key: Some(secrets[0].to_bytes()),
        };
        let committee = config.committee().unwrap();
        let context = committee.context_with_genesis(0, [9u8; 32]);
        let genesis = Block::genesis(context);
        Fixture {
            committee,
            context,
            voters,
            secrets,
            genesis,
        }
    }

    fn make_block(
        fixture: &Fixture,
        parent: &Block,
        height: u64,
        view: u64,
        with_justify: bool,
    ) -> Block {
        let justify = if with_justify {
            let votes: Vec<_> = fixture
                .secrets
                .iter()
                .zip(fixture.voters.iter())
                .take(3)
                .map(|(secret, voter)| {
                    Vote::new_bls(
                        fixture.context,
                        parent.view,
                        parent.hash(),
                        parent.app_hash,
                        *voter,
                        secret,
                    )
                })
                .collect();
            Some(form_certificate(&fixture.committee, fixture.context, votes, true).unwrap())
        } else {
            None
        };
        Block {
            epoch: fixture.context.epoch,
            committee_hash: fixture.context.committee_hash,
            genesis_hash: fixture.context.genesis_hash,
            view,
            height,
            parent: parent.hash(),
            payload: vec![],
            proposer: fixture.committee.leader(view),
            commitment_root: CommitmentV2::default().root().unwrap(),
            app_hash: [height as u8; 32],
            timestamp: height,
            justify,
        }
    }

    fn export(block: &Block) -> PeerBlockExport {
        PeerBlockExport {
            epoch: block.epoch,
            committee_hash: hex::encode(block.committee_hash),
            genesis_hash: hex::encode(block.genesis_hash),
            height: block.height,
            view: block.view,
            hash: hex::encode(block.hash()),
            parent_hash: hex::encode(block.parent),
            commitment_root: hex::encode(block.commitment_root),
            app_hash: hex::encode(block.app_hash),
            proposer: hex::encode(block.proposer),
            timestamp: block.timestamp,
            payload: Some(BASE64.encode(&block.payload)),
            justify: block
                .justify
                .as_ref()
                .map(|certificate| PeerCertificateExport {
                    epoch: certificate.epoch,
                    committee_hash: hex::encode(certificate.committee_hash),
                    genesis_hash: hex::encode(certificate.genesis_hash),
                    view: certificate.view,
                    block_hash: hex::encode(certificate.block_hash),
                    app_hash: certificate.app_hash.map(hex::encode),
                    voters: certificate.voters.iter().map(hex::encode).collect(),
                    bls_pubkeys: certificate.bls_pubkeys.iter().map(hex::encode).collect(),
                    agg_signature: hex::encode(&certificate.agg_signature),
                }),
        }
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

    fn qc_for(fixture: &Fixture, block: &Block) -> Certificate {
        let votes: Vec<_> = fixture
            .secrets
            .iter()
            .zip(fixture.voters.iter())
            .take(3)
            .map(|(secret, voter)| {
                Vote::new_bls(
                    fixture.context,
                    block.view,
                    block.hash(),
                    block.app_hash,
                    *voter,
                    secret,
                )
            })
            .collect();
        form_certificate(&fixture.committee, fixture.context, votes, true).unwrap()
    }

    fn finality_proof(fixture: &Fixture) -> VerifiedFinalityProof {
        let first = make_block(fixture, &fixture.genesis, 1, 0, false);
        let target = make_block(fixture, &first, 2, 1, true);
        let child = make_block(fixture, &target, 3, 2, true);
        VerifiedFinalityProof {
            target,
            child: child.clone(),
            commit_qc: qc_for(fixture, &child),
        }
    }

    fn client(fixture: &Fixture) -> ActiveSyncClient {
        let config = ActiveSyncConfig::try_new(
            vec![],
            Duration::from_secs(1),
            fixture.context,
            fixture.committee.clone(),
        )
        .unwrap();
        ActiveSyncClient::try_new(config).unwrap()
    }

    fn page(blocks: Vec<PeerBlockExport>, next_height: Option<u64>) -> Vec<u8> {
        serde_json::to_vec(&PeerBlockRangeResponse {
            blocks,
            next_height,
        })
        .unwrap()
    }

    async fn spawn_http_pages(pages: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for body in pages {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).await.unwrap();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn config_requires_trusted_context_and_committee() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let config = ActiveSyncConfig::try_new(
            vec![],
            Duration::from_secs(1),
            fixture.context,
            fixture.committee.clone(),
        )
        .unwrap();
        assert_eq!(config.trusted_context(), fixture.context);
        assert_eq!(config.committee().hash(), fixture.committee.hash());
    }

    #[test]
    fn response_chunk_budget_accepts_boundary_and_rejects_one_byte_over() {
        assert!(response_chunk_fits(MAX_BLOCK_RANGE_RESPONSE_BYTES - 1, 1));
        assert!(!response_chunk_fits(MAX_BLOCK_RANGE_RESPONSE_BYTES - 1, 2));
        assert!(!response_chunk_fits(MAX_BLOCK_RANGE_RESPONSE_BYTES + 1, 0));
    }

    #[test]
    fn total_response_budget_accepts_exact_boundary_and_rejects_one_byte_over() {
        assert!(total_budget_fits(
            MAX_ACTIVE_SYNC_TOTAL_BYTES - 1,
            1,
            MAX_ACTIVE_SYNC_TOTAL_BYTES
        ));
        assert!(!total_budget_fits(
            MAX_ACTIVE_SYNC_TOTAL_BYTES - 1,
            2,
            MAX_ACTIVE_SYNC_TOTAL_BYTES
        ));
    }

    #[test]
    fn requested_block_span_is_bounded_before_download() {
        assert!(ensure_block_budget(MAX_ACTIVE_SYNC_BLOCKS, MAX_ACTIVE_SYNC_BLOCKS).is_ok());
        let error =
            ensure_block_budget(MAX_ACTIVE_SYNC_BLOCKS + 1, MAX_ACTIVE_SYNC_BLOCKS).unwrap_err();
        assert!(error.contains("requested block span"));
    }

    #[tokio::test]
    async fn oversized_requested_span_is_rejected_before_http() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let error = client(&fixture)
            .download_verified_blocks("not a URL", &fixture.genesis, MAX_ACTIVE_SYNC_BLOCKS + 1)
            .await
            .unwrap_err();
        assert!(error.contains("requested block span"));
    }

    #[tokio::test]
    async fn cumulative_response_budget_rejects_second_page() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let second = make_block(&fixture, &first, 2, 1, true);
        let first_page = page(vec![export(&first)], Some(2));
        let second_page = page(vec![export(&second)], None);
        let byte_budget = first_page.len() + second_page.len() - 1;
        let (peer, server) = spawn_http_pages(vec![first_page, second_page]).await;

        let error = client(&fixture)
            .download_blocks_with_limits(&peer, 1, 2, 2, byte_budget)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error.contains("byte limit"));
    }

    #[tokio::test]
    async fn cumulative_response_budget_accepts_exact_boundary() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let second = make_block(&fixture, &first, 2, 1, true);
        let first_page = page(vec![export(&first)], Some(2));
        let second_page = page(vec![export(&second)], None);
        let byte_budget = first_page.len() + second_page.len();
        let (peer, server) = spawn_http_pages(vec![first_page, second_page]).await;

        let blocks = client(&fixture)
            .download_blocks_with_limits(&peer, 1, 2, 2, byte_budget)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[tokio::test]
    async fn ordinary_verified_range_succeeds() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let body = page(vec![export(&first)], None);
        let (peer, server) = spawn_http_pages(vec![body]).await;

        let blocks = client(&fixture)
            .download_verified_blocks(&peer, &fixture.genesis, 1)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].hash(), first.hash());
    }

    #[tokio::test]
    async fn verified_finalized_batch_requires_and_returns_terminal_proof() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let second = make_block(&fixture, &first, 2, 1, true);
        let third = make_block(&fixture, &second, 3, 2, true);
        let child = make_block(&fixture, &third, 4, 3, true);
        let proof = PeerFinalityProofExport {
            target: export(&third),
            child: export(&child),
            commit_qc: export_certificate(&qc_for(&fixture, &child)),
        };
        let proof_body = serde_json::to_vec(&proof).unwrap();
        let block_body = page(vec![export(&first), export(&second), export(&third)], None);
        let (peer, server) = spawn_http_pages(vec![block_body, proof_body]).await;

        let batch = client(&fixture)
            .download_verified_finalized_batch(&peer, &fixture.genesis, 3)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(batch.blocks.last().unwrap().hash(), third.hash());
        assert_eq!(batch.proof.target.hash(), third.hash());
        assert_eq!(batch.proof.child.hash(), child.hash());
    }

    #[tokio::test]
    async fn verified_finalized_batch_rejects_forged_terminal_target() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let second = make_block(&fixture, &first, 2, 1, true);
        let third = make_block(&fixture, &second, 3, 2, true);
        let child = make_block(&fixture, &third, 4, 3, true);
        let proof = PeerFinalityProofExport {
            target: export(&second),
            child: export(&child),
            commit_qc: export_certificate(&qc_for(&fixture, &child)),
        };
        let block_body = page(vec![export(&first), export(&second), export(&third)], None);
        let proof_body = serde_json::to_vec(&proof).unwrap();
        let (peer, server) = spawn_http_pages(vec![block_body, proof_body]).await;

        let error = client(&fixture)
            .download_verified_finalized_batch(&peer, &fixture.genesis, 3)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error.contains("exactly match downloaded terminal"));
    }

    #[test]
    fn claimed_hash_mismatch_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let block = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let mut peer = export(&block);
        peer.hash = hex::encode([0u8; 32]);
        let error = convert_block_export(peer).unwrap_err();
        assert!(error.contains("claimed block hash"));
    }

    #[test]
    fn wrong_context_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let block = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let mut wrong_context = block.clone();
        wrong_context.genesis_hash = [8u8; 32];
        verify_block_batch(
            &[wrong_context],
            &fixture.genesis,
            1,
            fixture.context,
            &fixture.committee,
        )
        .unwrap_err();
    }

    #[test]
    fn nonleader_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut block = make_block(&fixture, &fixture.genesis, 1, 0, false);
        block.proposer = fixture
            .committee
            .members()
            .iter()
            .map(|member| member.node_id)
            .find(|node_id| *node_id != fixture.committee.leader(block.view))
            .unwrap();
        verify_block_batch(
            &[block],
            &fixture.genesis,
            1,
            fixture.context,
            &fixture.committee,
        )
        .unwrap_err();
    }

    #[test]
    fn broken_sequence_and_parent_are_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut block = make_block(&fixture, &fixture.genesis, 1, 0, false);
        block.height = 2;
        verify_block_batch(
            &[block],
            &fixture.genesis,
            2,
            fixture.context,
            &fixture.committee,
        )
        .unwrap_err();

        let mut block = make_block(&fixture, &fixture.genesis, 1, 0, false);
        block.parent = [4u8; 32];
        verify_block_batch(
            &[block],
            &fixture.genesis,
            1,
            fixture.context,
            &fixture.committee,
        )
        .unwrap_err();
    }

    #[test]
    fn rogue_self_signed_committee_key_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let parent = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let mut child = make_block(&fixture, &parent, 2, 1, true);
        let certificate = child.justify.as_mut().unwrap();
        let rogue = {
            let mut seed = [0u8; 32];
            seed[0] = 99;
            BlsSecretKey::from_seed(&seed)
        };
        certificate.bls_pubkeys[0] = rogue.public_key().to_bytes().to_vec();
        verify_block_batch(&[child], &parent, 2, fixture.context, &fixture.committee).unwrap_err();
    }

    #[test]
    fn insufficient_weighted_quorum_is_rejected() {
        let fixture = fixture_with_powers(&[5, 1, 1, 1]);
        let parent = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let vote = Vote::new_bls(
            fixture.context,
            parent.view,
            parent.hash(),
            parent.app_hash,
            fixture.voters[1],
            &fixture.secrets[1],
        );
        let signature = crate::crypto::bls::BlsSignature::from_slice(&vote.signature).unwrap();
        let aggregate = aggregate_signatures(&[signature]).unwrap();
        let certificate = Certificate::new_bls(
            fixture.context,
            parent.view,
            parent.hash(),
            vec![vote],
            aggregate.to_bytes().to_vec(),
        )
        .unwrap();
        let mut child = make_block(&fixture, &parent, 2, 1, false);
        child.justify = Some(certificate);
        verify_block_batch(&[child], &parent, 2, fixture.context, &fixture.committee).unwrap_err();
    }

    #[test]
    fn valid_trusted_batch_is_accepted() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let second = make_block(&fixture, &first, 2, 1, true);
        let third = make_block(&fixture, &second, 3, 2, true);
        verify_block_batch(
            &[first, second, third],
            &fixture.genesis,
            3,
            fixture.context,
            &fixture.committee,
        )
        .unwrap();
    }

    #[test]
    fn deterministic_parent_timestamp_is_enforced_during_batch_verification() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let first = make_block(&fixture, &fixture.genesis, 1, 0, false);
        let mut second = make_block(&fixture, &first, 2, 1, true);
        second.timestamp = first.timestamp + crate::types::MAX_BLOCK_TIMESTAMP_STEP_MS + 1;
        let error = verify_block_batch(
            &[first, second],
            &fixture.genesis,
            2,
            fixture.context,
            &fixture.committee,
        )
        .unwrap_err();
        assert!(error.contains("parent timestamp"));
    }

    #[test]
    fn valid_two_chain_finality_proof_is_accepted() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        verify_finality_proof(
            &finality_proof(&fixture),
            fixture.context,
            &fixture.committee,
        )
        .unwrap();
    }

    #[test]
    fn forged_finality_context_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut proof = finality_proof(&fixture);
        proof.child.genesis_hash = [8u8; 32];
        let error = verify_finality_proof(&proof, fixture.context, &fixture.committee).unwrap_err();
        assert!(error.contains("context"));
    }

    #[test]
    fn forged_finality_qc_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut proof = finality_proof(&fixture);
        proof.commit_qc.agg_signature[0] ^= 1;
        let error = verify_finality_proof(&proof, fixture.context, &fixture.committee).unwrap_err();
        assert!(error.contains("commit QC"));
    }

    #[test]
    fn forged_finality_app_hash_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut proof = finality_proof(&fixture);
        proof.child.app_hash = [0xabu8; 32];
        let error = verify_finality_proof(&proof, fixture.context, &fixture.committee).unwrap_err();
        assert!(error.contains("commit QC") || error.contains("child justification"));
    }

    #[test]
    fn forged_finality_timestamp_is_rejected() {
        let fixture = fixture_with_powers(&[1, 1, 1, 1]);
        let mut proof = finality_proof(&fixture);
        proof.child.timestamp =
            proof.target.timestamp + crate::types::MAX_BLOCK_TIMESTAMP_STEP_MS + 1;
        let error = verify_finality_proof(&proof, fixture.context, &fixture.committee).unwrap_err();
        assert!(error.contains("timestamp"));
    }
}
