//! File-backed validator configuration for the canonical node runtime.
//!
//! The file format intentionally separates shared genesis data from the
//! process-local node file.  The genesis contains the static epoch-0
//! committee; the node file contains this process's identity, network
//! addresses, and the name of an environment variable holding its BLS seed.
//!
//! `chain_id`, the committee, and the canonical application-genesis material
//! are cryptographically bound into the consensus context before any listener
//! starts.  The resulting genesis domain is carried by every block and
//! consensus signature.  PoP bytes are verified against that domain but are
//! not included in its preimage.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::app::staking::StaticValidatorBootstrap;
use crate::crypto::bls::{BlsProofOfPossession, BlsPublicKey, BlsSecretKey};
use crate::network::NetworkConfig;
use crate::types::{
    genesis_domain_hash_with_application, ConsensusConfig, GenesisApplicationValidator, NodeId,
    GENESIS_APPLICATION_POLICY, HYCK_GENESIS_ALLOCATABLE_SUPPLY_BASE_UNITS,
    HYCK_MAX_SUPPLY_BASE_UNITS, MAX_COMMITTEE_MEMBERS,
};

/// The only supported on-disk node configuration schema.
pub const NODE_CONFIG_SCHEMA_VERSION: u32 = 2;

/// Shared static genesis configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisFile {
    /// On-disk schema version.  Only [`NODE_CONFIG_SCHEMA_VERSION`] is valid.
    pub schema_version: u32,
    /// Operational metadata for this local configuration tranche; see the
    /// module-level documentation for the current consensus binding limit.
    pub chain_id: String,
    /// Static consensus epoch.  The current protocol accepts epoch 0 only.
    pub epoch: u64,
    /// Initial view timeout in milliseconds.
    pub view_timeout_ms: u64,
    /// Canonical validator committee members.
    pub validators: Vec<GenesisValidator>,
    /// Optional liquid native HYCK allocations in base units.  Validator
    /// self-stakes are derived separately from `validators` and are not
    /// repeated here.
    #[serde(default, alias = "allocations")]
    pub hyck_allocations: Vec<GenesisAllocation>,
}

/// One explicit liquid native HYCK allocation in genesis.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisAllocation {
    /// Canonical lower-case account address.
    pub address: String,
    /// Amount in native HYCK base units.
    pub amount: i64,
}

/// A validator declared by the shared genesis file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisValidator {
    /// 32-byte validator identity encoded as exactly 64 hexadecimal chars.
    pub node_id: String,
    /// Positive voting power.
    pub voting_power: u64,
    /// 48-byte compressed BLS public key encoded as exactly 96 hexadecimal
    /// chars.
    pub bls_public_key: String,
    /// 96-byte compressed BLS proof-of-possession signature encoded as exactly
    /// 192 hexadecimal chars.
    pub bls_proof_of_possession: String,
    /// Optional application operator identity for the static bootstrap.
    /// Local fixtures may omit it and use the deterministic system operator.
    #[serde(default)]
    pub operator: Option<String>,
    /// Optional self-stake in HYCK base units. When omitted, local fixtures
    /// derive `voting_power * HYCK_BASE_UNITS_PER_HYCK` deterministically.
    #[serde(default)]
    pub self_stake: Option<i64>,
    /// Optional commission in basis points. Defaults to zero for local
    /// curated fixtures.
    #[serde(default)]
    pub commission_bps: Option<i64>,
}

/// Process-local node configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeFile {
    /// 32-byte local validator identity encoded as exactly 64 hexadecimal
    /// chars.
    pub node_id: String,
    /// Local authenticated TCP listen address.
    pub listen_addr: String,
    /// Local HTTP/WebSocket API listen address.
    pub api_listen_addr: String,
    /// Every other validator, exactly once.
    pub peers: Vec<NodePeer>,
    /// Name of the environment variable containing the original 32-byte BLS
    /// seed, encoded as hexadecimal.
    pub bls_secret_seed_env: String,
}

/// A process-local peer address.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePeer {
    /// 32-byte validator identity encoded as exactly 64 hexadecimal chars.
    pub node_id: String,
    /// Authenticated TCP peer address.
    pub address: String,
}

/// Validated runtime configuration shared by the node binary's startup path.
///
/// The BLS secret is intentionally not copied into this wrapper separately:
/// it is held only by the already-required `ConsensusConfig` seed and the
/// authenticated `NetworkConfig` key object.  The custom `Debug` impl below
/// never formats either secret-bearing value.
#[derive(Clone)]
pub struct ResolvedNodeConfig {
    /// Chain identifier from genesis.
    pub chain_id: String,
    /// Local HTTP/WebSocket API listen address.
    pub api_listen_addr: String,
    /// Committee-bound consensus configuration for this local validator.
    pub consensus: ConsensusConfig,
    /// PoP-bearing application records used to bootstrap the curated static
    /// committee before replay. This is not consensus state by itself.
    pub staking_bootstrap: Vec<StaticValidatorBootstrap>,
    /// Canonical explicit liquid HYCK allocations to apply after staking
    /// bootstrap, funded solely by the native treasury reserve.
    pub hyck_allocations: Vec<GenesisAllocation>,
    /// Authenticated TCP configuration for this local validator.
    pub network: NetworkConfig,
}

impl fmt::Debug for ResolvedNodeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedNodeConfig")
            .field("chain_id", &self.chain_id)
            .field(
                "consensus_node_id",
                &crate::types::hash_short(&self.consensus.node_id),
            )
            .field("validator_count", &self.consensus.validators.len())
            .field(
                "committee_hash",
                &crate::types::hash_short(
                    &self
                        .consensus
                        .context()
                        .ok()
                        .map(|context| context.committee_hash)
                        .unwrap_or([0u8; 32]),
                ),
            )
            .field("listen_addr", &self.network.listen_addr)
            .field("api_listen_addr", &self.api_listen_addr)
            .field("peer_count", &self.network.peers.len())
            .field("authenticated", &self.network.require_authenticated_peers)
            .finish()
    }
}

/// Load a genesis file and a process-local node file, then read the local BLS
/// seed from the environment variable named by `NodeFile::bls_secret_seed_env`.
pub fn load_node_runtime_config(
    genesis_path: impl AsRef<Path>,
    node_path: impl AsRef<Path>,
) -> Result<ResolvedNodeConfig> {
    let genesis: GenesisFile = read_json_file(genesis_path.as_ref(), "genesis")?;
    let node: NodeFile = read_json_file(node_path.as_ref(), "node")?;

    validate_seed_env_name(&node.bls_secret_seed_env)?;
    let seed_hex = std::env::var(&node.bls_secret_seed_env).map_err(|error| {
        anyhow!(
            "BLS seed environment variable `{}` could not be read: {}",
            node.bls_secret_seed_env,
            error
        )
    })?;

    resolve_node_runtime_config(&genesis, &node, &seed_hex)
}

/// Resolve already-parsed configuration and a caller-provided BLS seed.
///
/// This function is pure with respect to process environment and filesystem,
/// making the complete committee/network validation directly testable.
pub fn resolve_node_runtime_config(
    genesis: &GenesisFile,
    node: &NodeFile,
    seed_hex: &str,
) -> Result<ResolvedNodeConfig> {
    validate_genesis_shape(genesis)?;
    validate_seed_env_name(&node.bls_secret_seed_env)?;
    validate_address(&node.listen_addr, "node.listen_addr")?;
    validate_address(&node.api_listen_addr, "node.api_listen_addr")?;
    if addresses_collide(&node.listen_addr, &node.api_listen_addr) {
        bail!("node.api_listen_addr must not collide with node.listen_addr");
    }

    let local_node_id = decode_node_id(&node.node_id, "node.node_id")?;
    let local_seed = decode_fixed_hex::<32>(seed_hex, "BLS secret seed")?;
    let local_secret_key = BlsSecretKey::from_seed(&local_seed);
    let local_public_key = local_secret_key.public_key();

    let mut validators = Vec::with_capacity(genesis.validators.len());
    let mut voting_powers = Vec::with_capacity(genesis.validators.len());
    let mut bls_pubkeys = Vec::with_capacity(genesis.validators.len());
    let mut validator_pubkeys = HashMap::with_capacity(genesis.validators.len());
    let mut validator_ids = HashSet::with_capacity(genesis.validators.len());
    let mut bls_keys = HashSet::with_capacity(genesis.validators.len());
    let mut proof_bindings = Vec::with_capacity(genesis.validators.len());
    let mut local_genesis_public_key = None;

    for (index, validator) in genesis.validators.iter().enumerate() {
        if validator.voting_power == 0 {
            bail!("genesis.validators[{index}].voting_power must be greater than zero");
        }

        let node_id = decode_node_id(
            &validator.node_id,
            &format!("genesis.validators[{index}].node_id"),
        )?;
        if !validator_ids.insert(node_id) {
            bail!("genesis.validators[{index}].node_id duplicates another validator");
        }

        let public_key_bytes = decode_fixed_hex::<48>(
            &validator.bls_public_key,
            &format!("genesis.validators[{index}].bls_public_key"),
        )?;
        let public_key = BlsPublicKey::from_bytes(&public_key_bytes).map_err(|_| {
            anyhow!("genesis.validators[{index}].bls_public_key is not a valid BLS public key")
        })?;
        if !bls_keys.insert(public_key_bytes) {
            bail!("genesis.validators[{index}].bls_public_key duplicates another validator");
        }
        let proof_bytes = decode_fixed_hex::<96>(
            &validator.bls_proof_of_possession,
            &format!("genesis.validators[{index}].bls_proof_of_possession"),
        )?;
        let proof = BlsProofOfPossession::from_bytes(&proof_bytes).map_err(|_| {
            anyhow!("genesis.validators[{index}].bls_proof_of_possession is not a valid BLS proof")
        })?;

        validators.push(node_id);
        voting_powers.push(validator.voting_power);
        bls_pubkeys.push(public_key_bytes.to_vec());
        validator_pubkeys.insert(node_id, public_key);
        proof_bindings.push((node_id, public_key_bytes, proof));

        if node_id == local_node_id {
            local_genesis_public_key = Some(public_key_bytes);
        }
    }

    let configured_local_public_key = local_genesis_public_key.ok_or_else(|| {
        anyhow!(
            "node.node_id {} is not present in genesis.validators",
            crate::types::hash_short(&local_node_id)
        )
    })?;
    if local_public_key.to_bytes() != configured_local_public_key {
        bail!(
            "local BLS secret does not match the configured public key for node {}",
            crate::types::hash_short(&local_node_id)
        );
    }

    validate_peers(node, local_node_id, &validator_ids)?;

    let mut consensus = ConsensusConfig {
        epoch: genesis.epoch,
        genesis_hash: [0u8; 32],
        node_id: local_node_id,
        validators,
        voting_powers,
        view_timeout_ms: genesis.view_timeout_ms,
        bls_pubkeys,
        bls_secret_key: Some(local_seed),
    };

    // This performs the canonical committee sort/hash and enforces the
    // existing static epoch-0 committee invariants before any listener starts.
    consensus
        .committee()
        .map_err(|error| anyhow!("invalid genesis committee: {error}"))?;
    let committee_hash = consensus
        .committee()
        .map_err(|error| anyhow!("invalid genesis committee: {error}"))?
        .hash();
    let hyck_allocations = canonical_hyck_allocations(genesis)?;
    let allocation_pairs: Vec<_> = hyck_allocations
        .iter()
        .map(|allocation| (allocation.address.clone(), allocation.amount))
        .collect();
    // Resolve the effective application records before computing the domain.
    // Defaults are part of the authenticated preimage just like explicit
    // values; PoP bytes are intentionally absent because PoP signs this hash.
    let application_validators = canonical_genesis_application_validators(genesis)?;
    let staking_bootstrap = canonical_staking_bootstrap(genesis, &proof_bindings)?;
    consensus.genesis_hash = genesis_domain_hash_with_application(
        &genesis.chain_id,
        genesis.epoch,
        genesis.view_timeout_ms,
        committee_hash,
        &application_validators,
        &allocation_pairs,
    );
    for (index, (node_id, public_key_bytes, proof)) in proof_bindings.iter().enumerate() {
        let public_key = BlsPublicKey::from_bytes(public_key_bytes)
            .map_err(|_| anyhow!("genesis.validators[{index}] has an invalid BLS public key"))?;
        if !public_key.verify_proof_of_possession(&consensus.genesis_hash, node_id, proof) {
            bail!(
                "genesis.validators[{index}].bls_proof_of_possession does not bind the configured chain domain, node_id, and BLS public key"
            );
        }
    }
    if !consensus
        .context()
        .map_err(|error| anyhow!("invalid genesis committee: {error}"))?
        .has_genesis_domain()
    {
        bail!("validated genesis produced an empty consensus domain");
    }

    let context = consensus
        .context()
        .map_err(|error| anyhow!("invalid genesis committee: {error}"))?;

    let network = NetworkConfig {
        node_id: local_node_id,
        listen_addr: node.listen_addr.clone(),
        peers: node
            .peers
            .iter()
            .map(|peer| {
                Ok((
                    decode_node_id(&peer.node_id, "node.peers[].node_id")?,
                    peer.address.clone(),
                ))
            })
            .collect::<Result<Vec<_>>>()?,
        require_authenticated_peers: true,
        bls_secret_key: Some(local_secret_key),
        validator_pubkeys,
        gossip_validation: Some(crate::network::GossipValidationConfig {
            context,
            committee: consensus
                .committee()
                .map_err(|error| anyhow!("invalid genesis committee: {error}"))?,
            allow_dev_envelopes: false,
        }),
    };

    // Re-run the transport-level checks, including local key membership and
    // every configured peer's public-key availability, before returning.
    network
        .handshake_config()
        .context("invalid authenticated network configuration")?;

    Ok(ResolvedNodeConfig {
        chain_id: genesis.chain_id.clone(),
        api_listen_addr: node.api_listen_addr.clone(),
        consensus,
        staking_bootstrap,
        hyck_allocations,
        network,
    })
}

fn read_json_file<T: DeserializeOwned>(path: &Path, kind: &str) -> Result<T> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read {kind} configuration `{}`", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {kind} configuration `{}`", path.display()))
}

fn validate_genesis_shape(genesis: &GenesisFile) -> Result<()> {
    if genesis.schema_version != NODE_CONFIG_SCHEMA_VERSION {
        bail!(
            "unsupported genesis schema_version {}; expected {}",
            genesis.schema_version,
            NODE_CONFIG_SCHEMA_VERSION
        );
    }
    if genesis.chain_id.trim().is_empty() {
        bail!("genesis.chain_id must be nonempty");
    }
    if genesis.view_timeout_ms == 0 {
        bail!("genesis.view_timeout_ms must be greater than zero");
    }
    if genesis.validators.is_empty() {
        bail!("genesis.validators must contain at least one validator");
    }
    if genesis.validators.len() > MAX_COMMITTEE_MEMBERS {
        bail!("genesis.validators must contain at most {MAX_COMMITTEE_MEMBERS} validators");
    }
    Ok(())
}

/// Resolve the effective application fields that are committed by a
/// file-backed genesis.  This deliberately does not include BLS PoP bytes:
/// those proofs authenticate the resulting domain and would otherwise create
/// a circular preimage.
fn canonical_genesis_application_validators(
    genesis: &GenesisFile,
) -> Result<Vec<GenesisApplicationValidator>> {
    let policy = GENESIS_APPLICATION_POLICY;
    let mut validators = Vec::with_capacity(genesis.validators.len());
    let mut operators = HashSet::with_capacity(genesis.validators.len());

    for (index, validator) in genesis.validators.iter().enumerate() {
        let node_id = decode_node_id(
            &validator.node_id,
            &format!("genesis.validators[{index}].node_id"),
        )?;
        let voting_power = u128::from(validator.voting_power);
        let derived_stake = voting_power
            .checked_mul(policy.hyck_base_units_per_hyck as u128)
            .and_then(|stake| i64::try_from(stake).ok())
            .ok_or_else(|| anyhow!("genesis.validators[{index}] voting power overflows stake"))?;
        let operator = validator
            .operator
            .clone()
            .unwrap_or_else(|| format!("system:genesis:{}", hex::encode(node_id)));
        if operator.trim().is_empty() || !operators.insert(operator.clone()) {
            bail!("genesis.validators[{index}].operator must be unique and nonempty");
        }
        let self_stake = validator.self_stake.unwrap_or(derived_stake);
        if self_stake != derived_stake {
            bail!(
                "genesis.validators[{index}].self_stake must equal voting_power * HYCK_BASE_UNITS_PER_HYCK ({derived_stake})"
            );
        }
        let commission_bps = validator.commission_bps.unwrap_or(0);
        if !(0..=policy.max_commission_bps).contains(&commission_bps) {
            bail!("genesis.validators[{index}].commission_bps is out of range");
        }
        validators.push(GenesisApplicationValidator {
            node_id,
            operator,
            voting_power,
            self_stake,
            commission_bps,
        });
    }

    Ok(validators)
}

/// Attach the already validated BLS material to the canonical application
/// records returned to the node runtime.  The commitment itself only uses the
/// application fields, committee hash, and policy constants.
fn canonical_staking_bootstrap(
    genesis: &GenesisFile,
    proof_bindings: &[(NodeId, [u8; 48], BlsProofOfPossession)],
) -> Result<Vec<StaticValidatorBootstrap>> {
    let application_validators = canonical_genesis_application_validators(genesis)?;
    if application_validators.len() != proof_bindings.len() {
        bail!("genesis validator material length does not match PoP material");
    }

    Ok(application_validators
        .into_iter()
        .zip(proof_bindings.iter())
        .map(
            |(application, (node_id, public_key_bytes, proof))| StaticValidatorBootstrap {
                operator: application.operator,
                node_id: *node_id,
                voting_power: application.voting_power,
                bls_pubkey: public_key_bytes.to_vec(),
                bls_proof_of_possession: proof.to_bytes().to_vec(),
                self_stake: application.self_stake,
                commission_bps: application.commission_bps,
            },
        )
        .collect())
}

fn canonical_hyck_allocations(genesis: &GenesisFile) -> Result<Vec<GenesisAllocation>> {
    let mut allocations = Vec::with_capacity(genesis.hyck_allocations.len());
    let mut addresses = HashSet::with_capacity(genesis.hyck_allocations.len());
    let mut total = 0i128;

    for (index, allocation) in genesis.hyck_allocations.iter().enumerate() {
        let canonical = allocation.address.trim().to_lowercase();
        if canonical.is_empty()
            || canonical != allocation.address
            || canonical == crate::app::staking::HYCK_TREASURY_ADDRESS
        {
            bail!("genesis.hyck_allocations[{index}].address must be a nonempty canonical address");
        }
        if allocation.amount <= 0 {
            bail!("genesis.hyck_allocations[{index}].amount must be greater than zero");
        }
        if !addresses.insert(canonical.clone()) {
            bail!("genesis.hyck_allocations[{index}].address duplicates another allocation");
        }
        total = total
            .checked_add(i128::from(allocation.amount))
            .ok_or_else(|| anyhow!("genesis HYCK allocations overflow their total"))?;
        allocations.push(GenesisAllocation {
            address: canonical,
            amount: allocation.amount,
        });
    }

    let validator_stake = genesis.validators.iter().enumerate().try_fold(
        0i128,
        |total, (index, validator)| {
            let derived_stake = u128::from(validator.voting_power)
                .checked_mul(GENESIS_APPLICATION_POLICY.hyck_base_units_per_hyck as u128)
                .and_then(|stake| i64::try_from(stake).ok())
                .ok_or_else(|| {
                    anyhow!("genesis.validators[{index}] voting power overflows stake")
                })?;
            let self_stake = validator.self_stake.unwrap_or(derived_stake);
            if self_stake != derived_stake {
                bail!(
                    "genesis.validators[{index}].self_stake must equal voting_power * HYCK_BASE_UNITS_PER_HYCK ({derived_stake})"
                );
            }
            total
                .checked_add(i128::from(self_stake))
                .ok_or_else(|| anyhow!("genesis validator stake overflows its total"))
        },
    )?;
    let supply = i128::from(HYCK_MAX_SUPPLY_BASE_UNITS);
    let emissions_reserve =
        i128::from(GENESIS_APPLICATION_POLICY.hyck_emissions_reserve_base_units);
    let genesis_allocatable_supply = i128::from(HYCK_GENESIS_ALLOCATABLE_SUPPLY_BASE_UNITS);
    if emissions_reserve < 0 || emissions_reserve > supply {
        bail!(
            "genesis HYCK emissions reserve {} is outside fixed native HYCK supply {}",
            emissions_reserve,
            supply
        );
    }
    if validator_stake < 0 || validator_stake > supply {
        bail!(
            "genesis validator stake {} exceeds native HYCK supply {}",
            validator_stake,
            supply
        );
    }
    let accounted = validator_stake
        .checked_add(total)
        .ok_or_else(|| anyhow!("genesis HYCK accounting overflows"))?;
    if accounted > genesis_allocatable_supply {
        bail!(
            "genesis validator stake plus explicit HYCK allocations ({accounted}) exceeds the allocatable HYCK supply {genesis_allocatable_supply}; {} base units are reserved for future emissions",
            emissions_reserve
        );
    }

    allocations.sort_by(|left, right| left.address.cmp(&right.address));
    Ok(allocations)
}

fn validate_seed_env_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("node.bls_secret_seed_env must be nonempty");
    }
    if name
        .chars()
        .any(|character| character.is_whitespace() || character == '=' || character == '\0')
    {
        bail!("node.bls_secret_seed_env contains invalid characters");
    }
    Ok(())
}

fn validate_peers(
    node: &NodeFile,
    local_node_id: NodeId,
    validator_ids: &HashSet<NodeId>,
) -> Result<()> {
    let expected_peer_count = validator_ids.len().saturating_sub(1);
    if node.peers.len() != expected_peer_count {
        bail!(
            "node.peers must contain exactly {} entries (got {})",
            expected_peer_count,
            node.peers.len()
        );
    }

    let mut peer_ids = HashSet::with_capacity(node.peers.len());
    let mut addresses = HashSet::with_capacity(node.peers.len() + 1);
    addresses.insert(node.listen_addr.as_str());
    for (index, peer) in node.peers.iter().enumerate() {
        let peer_id = decode_node_id(&peer.node_id, &format!("node.peers[{index}].node_id"))?;
        validate_address(&peer.address, &format!("node.peers[{index}].address"))?;
        if !addresses.insert(peer.address.as_str()) {
            bail!("node.peers[{index}].address duplicates another configured address");
        }

        if peer_id == local_node_id {
            bail!("node.peers[{index}] must not contain the local node");
        }
        if !validator_ids.contains(&peer_id) {
            bail!("node.peers[{index}] references a node that is not in genesis.validators");
        }
        if !peer_ids.insert(peer_id) {
            bail!("node.peers[{index}] duplicates another peer");
        }
    }

    if peer_ids.len() != expected_peer_count
        || validator_ids
            .iter()
            .filter(|validator_id| **validator_id != local_node_id)
            .any(|validator_id| !peer_ids.contains(validator_id))
    {
        bail!("node.peers must list every non-local genesis validator exactly once");
    }

    Ok(())
}

fn validate_address(address: &str, field: &str) -> Result<()> {
    if address.is_empty() || address.chars().any(char::is_whitespace) {
        bail!("{field} must be a nonempty host:port address");
    }

    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let closing = rest
            .find(']')
            .ok_or_else(|| anyhow!("{field} has an invalid bracketed host"))?;
        let host = &rest[..closing];
        let port = rest
            .get(closing + 1..)
            .and_then(|suffix| suffix.strip_prefix(':'))
            .ok_or_else(|| anyhow!("{field} must include a port"))?;
        (host, port)
    } else {
        let (host, port) = address
            .rsplit_once(':')
            .ok_or_else(|| anyhow!("{field} must be in host:port form"))?;
        if host.is_empty() || host.contains(':') {
            bail!("{field} must bracket IPv6 hosts");
        }
        (host, port)
    };

    if host.is_empty() {
        bail!("{field} host must be nonempty");
    }
    let port = port
        .parse::<u16>()
        .map_err(|_| anyhow!("{field} port must be an integer from 1 to 65535"))?;
    if port == 0 {
        bail!("{field} port must be nonzero");
    }
    Ok(())
}

fn addresses_collide(left: &str, right: &str) -> bool {
    let Some((left_host, left_port)) = address_host_port(left) else {
        return false;
    };
    let Some((right_host, right_port)) = address_host_port(right) else {
        return false;
    };

    left_port == right_port
        && (left_host == right_host || is_wildcard_host(left_host) || is_wildcard_host(right_host))
}

fn address_host_port(address: &str) -> Option<(&str, u16)> {
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let closing = rest.find(']')?;
        let host = &rest[..closing];
        let port = rest.get(closing + 1..)?.strip_prefix(':')?;
        (host, port)
    } else {
        address.rsplit_once(':')?
    };

    Some((host, port.parse().ok()?))
}

fn is_wildcard_host(host: &str) -> bool {
    matches!(host, "0.0.0.0" | "::")
}

fn decode_node_id(value: &str, field: &str) -> Result<NodeId> {
    decode_fixed_hex::<32>(value, field)
}

fn decode_fixed_hex<const N: usize>(value: &str, field: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        bail!(
            "{field} must contain exactly {} hexadecimal characters",
            N * 2
        );
    }
    let bytes = hex::decode(value).map_err(|_| anyhow!("{field} must contain hexadecimal data"))?;
    let mut decoded = [0u8; N];
    decoded.copy_from_slice(&bytes);
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(index: u8) -> [u8; 32] {
        [index; 32]
    }

    fn node_id(index: u8) -> NodeId {
        [index; 32]
    }

    fn validator(index: u8) -> GenesisValidator {
        let secret = BlsSecretKey::from_seed(&seed(index));
        GenesisValidator {
            node_id: hex::encode(node_id(index)),
            voting_power: 1,
            bls_public_key: hex::encode(secret.public_key().to_bytes()),
            bls_proof_of_possession: String::new(),
            operator: None,
            self_stake: None,
            commission_bps: None,
        }
    }

    fn genesis(count: u8) -> GenesisFile {
        let mut genesis = GenesisFile {
            schema_version: NODE_CONFIG_SCHEMA_VERSION,
            chain_id: "local-chain".to_string(),
            epoch: 0,
            view_timeout_ms: 100,
            validators: (1..=count).map(validator).collect(),
            hyck_allocations: Vec::new(),
        };
        let config = ConsensusConfig {
            epoch: genesis.epoch,
            genesis_hash: [0u8; 32],
            node_id: node_id(1),
            validators: genesis
                .validators
                .iter()
                .map(|validator| decode_node_id(&validator.node_id, "test node id").unwrap())
                .collect(),
            voting_powers: genesis
                .validators
                .iter()
                .map(|validator| validator.voting_power)
                .collect(),
            view_timeout_ms: genesis.view_timeout_ms,
            bls_pubkeys: genesis
                .validators
                .iter()
                .map(|validator| hex::decode(&validator.bls_public_key).unwrap())
                .collect(),
            bls_secret_key: None,
        };
        let application_validators = canonical_genesis_application_validators(&genesis).unwrap();
        let domain = genesis_domain_hash_with_application(
            &genesis.chain_id,
            genesis.epoch,
            genesis.view_timeout_ms,
            config.committee().unwrap().hash(),
            &application_validators,
            &[],
        );
        for validator in &mut genesis.validators {
            let node_id = decode_node_id(&validator.node_id, "test node id").unwrap();
            let seed = [node_id[0]; 32];
            let proof =
                BlsSecretKey::from_seed(&seed).create_proof_of_possession(&domain, &node_id);
            validator.bls_proof_of_possession = hex::encode(proof.to_bytes());
        }
        genesis
    }

    fn node(index: u8, count: u8) -> NodeFile {
        NodeFile {
            node_id: hex::encode(node_id(index)),
            listen_addr: format!("127.0.0.1:{}", 9000 + u16::from(index)),
            api_listen_addr: format!("127.0.0.1:{}", 8000 + u16::from(index)),
            peers: (1..=count)
                .filter(|peer| *peer != index)
                .map(|peer| NodePeer {
                    node_id: hex::encode(node_id(peer)),
                    address: format!("127.0.0.1:{}", 9000 + u16::from(peer)),
                })
                .collect(),
            bls_secret_seed_env: format!("TEST_BLS_SEED_{index}"),
        }
    }

    #[test]
    fn resolves_authenticated_single_validator() {
        let genesis = genesis(1);
        let node = node(1, 1);
        let resolved = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect("single-validator config should resolve");

        assert_eq!(resolved.chain_id, "local-chain");
        assert_eq!(resolved.api_listen_addr, "127.0.0.1:8001");
        assert_eq!(resolved.consensus.n(), 1);
        assert_eq!(resolved.network.peers.len(), 0);
        assert!(resolved.network.require_authenticated_peers);
        assert!(resolved.network.handshake_config().is_ok());
    }

    #[test]
    fn checked_in_local_genesis_proofs_match_protocol_v5_schema_v5_domain() {
        let four_genesis: GenesisFile =
            serde_json::from_str(include_str!("../config/local/genesis.json")).unwrap();
        let four_node: NodeFile =
            serde_json::from_str(include_str!("../config/local/host-4/node0.json")).unwrap();
        let four = resolve_node_runtime_config(
            &four_genesis,
            &four_node,
            "01000000000000000000000000000000000000000000000000000000000000be",
        )
        .expect("checked-in four-validator fixture must validate");
        assert_eq!(
            hex::encode(four.consensus.genesis_hash),
            "adbeaea4f29c7a381302e5a0e66ad5fda518077656821661a1318f071125aa00"
        );

        let single_genesis: GenesisFile =
            serde_json::from_str(include_str!("../config/local/single-genesis.json")).unwrap();
        let single_node: NodeFile =
            serde_json::from_str(include_str!("../config/local/host-single/node.json")).unwrap();
        let single = resolve_node_runtime_config(
            &single_genesis,
            &single_node,
            "01000000000000000000000000000000000000000000000000000000000000be",
        )
        .expect("checked-in single-validator fixture must validate");
        assert_eq!(
            hex::encode(single.consensus.genesis_hash),
            "7ec4f5cbcfbbefc8c1e9f70665703428558dc6947ba50901a78101d7dbfbd60b"
        );
    }

    #[test]
    fn all_four_nodes_resolve_to_the_same_committee_context() {
        let genesis = genesis(4);
        let contexts: Vec<_> = (1..=4)
            .map(|index| {
                resolve_node_runtime_config(&genesis, &node(index, 4), &hex::encode(seed(index)))
                    .expect("four-validator config should resolve")
                    .consensus
                    .context()
                    .expect("committee context should be valid")
            })
            .collect();

        assert!(contexts.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(contexts[0].has_genesis_domain());
    }

    #[test]
    fn changing_chain_id_invalidates_existing_genesis_proofs() {
        let genesis = genesis(1);
        let node = node(1, 1);
        let mut other_genesis = genesis.clone();
        other_genesis.chain_id = "another-local-chain".to_string();
        let error = resolve_node_runtime_config(&other_genesis, &node, &hex::encode(seed(1)))
            .expect_err("changing chain ID must invalidate the existing PoP");
        assert!(error.to_string().contains("does not bind"));
    }

    #[test]
    fn changing_application_genesis_fields_invalidates_existing_genesis_proofs() {
        let node = node(1, 1);
        let cases = [
            "operator",
            "voting_power_and_derived_self_stake",
            "commission",
            "allocation",
        ];

        for case in cases {
            let mut configured = genesis(1);
            match case {
                "operator" => configured.validators[0].operator = Some("operator:changed".into()),
                "voting_power_and_derived_self_stake" => {
                    configured.validators[0].voting_power = 2;
                    configured.validators[0].self_stake = Some(2 * 1_000_000);
                }
                "commission" => configured.validators[0].commission_bps = Some(1),
                "allocation" => configured.hyck_allocations.push(GenesisAllocation {
                    address: "alice".into(),
                    amount: 1,
                }),
                _ => unreachable!(),
            }

            let error = resolve_node_runtime_config(&configured, &node, &hex::encode(seed(1)))
                .expect_err("changing authenticated application genesis must invalidate PoP");
            assert!(
                error.to_string().contains("does not bind"),
                "{case} should fail PoP binding, got: {error}"
            );
        }
    }

    #[test]
    fn omitted_bootstrap_defaults_have_the_same_canonical_context_as_explicit_values() {
        let genesis = genesis(1);
        let node_file = node(1, 1);
        let implicit = resolve_node_runtime_config(&genesis, &node_file, &hex::encode(seed(1)))
            .expect("implicit bootstrap defaults should resolve");

        let mut explicit_genesis = genesis.clone();
        explicit_genesis.validators[0].operator =
            Some(format!("system:genesis:{}", hex::encode(node_id(1))));
        explicit_genesis.validators[0].self_stake = Some(1_000_000);
        explicit_genesis.validators[0].commission_bps = Some(0);
        let explicit =
            resolve_node_runtime_config(&explicit_genesis, &node_file, &hex::encode(seed(1)))
                .expect("explicit bootstrap defaults should resolve");

        assert_eq!(
            implicit.consensus.context().unwrap(),
            explicit.consensus.context().unwrap()
        );
    }

    #[test]
    fn genesis_hyck_allocations_are_canonical_and_supply_bounded() {
        let mut configured = genesis(1);
        configured.hyck_allocations = vec![
            GenesisAllocation {
                address: "bob".to_string(),
                amount: 22,
            },
            GenesisAllocation {
                address: "alice".to_string(),
                amount: 11,
            },
        ];
        let canonical = canonical_hyck_allocations(&configured).unwrap();
        assert_eq!(
            canonical
                .iter()
                .map(|allocation| allocation.address.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob"]
        );

        let mut duplicate = configured.clone();
        duplicate.hyck_allocations.push(GenesisAllocation {
            address: "ALICE".to_string(),
            amount: 1,
        });
        let error = canonical_hyck_allocations(&duplicate).unwrap_err();
        assert!(
            error.to_string().contains("canonical") || error.to_string().contains("duplicates")
        );

        let mut over_supply = configured;
        over_supply.hyck_allocations = vec![GenesisAllocation {
            address: "alice".to_string(),
            amount: crate::app::staking::HYCK_TOTAL_SUPPLY,
        }];
        let error = canonical_hyck_allocations(&over_supply).unwrap_err();
        assert!(error.to_string().contains("allocatable HYCK supply"));
    }

    #[test]
    fn genesis_allocations_preserve_the_future_emissions_reserve() {
        let mut configured = genesis(1);
        let remaining = HYCK_GENESIS_ALLOCATABLE_SUPPLY_BASE_UNITS
            - GENESIS_APPLICATION_POLICY.hyck_base_units_per_hyck;
        configured.hyck_allocations = vec![GenesisAllocation {
            address: "alice".to_string(),
            amount: remaining,
        }];
        canonical_hyck_allocations(&configured).expect("allocatable supply should be usable");

        configured.hyck_allocations[0].amount += 1;
        let error = canonical_hyck_allocations(&configured)
            .expect_err("validator stake plus allocations must leave emissions reserve");
        assert!(error.to_string().contains("reserved for future emissions"));
    }

    #[test]
    fn rejects_secret_that_does_not_match_local_committee_key() {
        let genesis = genesis(1);
        let node = node(1, 1);
        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(2)))
            .expect_err("mismatched local seed must be rejected");

        assert!(error.to_string().contains("does not match"));
        assert!(!error.to_string().contains(&hex::encode(seed(2))));
    }

    #[test]
    fn rejects_missing_peer() {
        let genesis = genesis(4);
        let mut node = node(1, 4);
        node.peers.pop();

        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect_err("missing peer must be rejected");
        assert!(error.to_string().contains("exactly 3 entries"));
    }

    #[test]
    fn rejects_duplicate_peer() {
        let genesis = genesis(4);
        let mut node = node(1, 4);
        node.peers[1].node_id = node.peers[0].node_id.clone();

        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect_err("duplicate peer must be rejected");
        assert!(error.to_string().contains("duplicates another peer"));
    }

    #[test]
    fn rejects_duplicate_peer_address() {
        let genesis = genesis(4);
        let mut node = node(1, 4);
        node.peers[1].address = node.peers[0].address.clone();

        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect_err("duplicate peer addresses must be rejected");
        assert!(error
            .to_string()
            .contains("duplicates another configured address"));
    }

    #[test]
    fn rejects_peer_address_equal_to_listen_address() {
        let genesis = genesis(4);
        let mut node = node(1, 4);
        node.peers[0].address = node.listen_addr.clone();

        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect_err("peer address equal to listen address must be rejected");
        assert!(error
            .to_string()
            .contains("duplicates another configured address"));
    }

    #[test]
    fn rejects_api_address_colliding_with_consensus_listener() {
        let genesis = genesis(1);
        let mut node = node(1, 1);
        node.api_listen_addr = node.listen_addr.clone();

        let error = resolve_node_runtime_config(&genesis, &node, &hex::encode(seed(1)))
            .expect_err("API and consensus listeners must not collide");
        assert!(error.to_string().contains("must not collide"));
    }

    #[test]
    fn rejects_malformed_schema_and_address() {
        let mut malformed_genesis = genesis(1);
        malformed_genesis.schema_version = 99;
        let node_file = node(1, 1);
        let error =
            resolve_node_runtime_config(&malformed_genesis, &node_file, &hex::encode(seed(1)))
                .expect_err("unsupported schema must be rejected");
        assert!(error.to_string().contains("schema_version"));

        let genesis_file = genesis(1);
        let mut malformed_node = node(1, 1);
        malformed_node.listen_addr = "127.0.0.1:0".to_string();
        let error =
            resolve_node_runtime_config(&genesis_file, &malformed_node, &hex::encode(seed(1)))
                .expect_err("zero listen port must be rejected");
        assert!(error.to_string().contains("port must be nonzero"));

        let mut malformed_api_node = node(1, 1);
        malformed_api_node.api_listen_addr = "127.0.0.1:0".to_string();
        let error =
            resolve_node_runtime_config(&genesis_file, &malformed_api_node, &hex::encode(seed(1)))
                .expect_err("zero API port must be rejected");
        assert!(error.to_string().contains("node.api_listen_addr"));
    }

    #[test]
    fn rejects_missing_or_malformed_genesis_proof() {
        let node_file = node(1, 1);
        let mut missing = genesis(1);
        missing.validators[0].bls_proof_of_possession.clear();
        let error = resolve_node_runtime_config(&missing, &node_file, &hex::encode(seed(1)))
            .expect_err("missing PoP must be rejected");
        assert!(error.to_string().contains("bls_proof_of_possession"));

        let mut malformed = genesis(1);
        malformed.validators[0].bls_proof_of_possession = "00".repeat(96);
        let error = resolve_node_runtime_config(&malformed, &node_file, &hex::encode(seed(1)))
            .expect_err("malformed PoP must be rejected");
        assert!(error.to_string().contains("bls_proof_of_possession"));
    }

    #[test]
    fn rejects_cross_domain_wrong_node_and_wrong_key_proofs() {
        let node_file = node(1, 1);

        let mut cross_domain = genesis(1);
        cross_domain.chain_id = "another-local-chain".to_string();
        let error = resolve_node_runtime_config(&cross_domain, &node_file, &hex::encode(seed(1)))
            .expect_err("PoP from another chain domain must be rejected");
        assert!(error.to_string().contains("does not bind"));

        let mut wrong_node = genesis(1);
        let wrong_node_id = node_id(2);
        let wrong_proof = BlsSecretKey::from_seed(&seed(1))
            .create_proof_of_possession(&[0u8; 32], &wrong_node_id)
            .to_bytes();
        wrong_node.validators[0].bls_proof_of_possession = hex::encode(wrong_proof);
        let error = resolve_node_runtime_config(&wrong_node, &node_file, &hex::encode(seed(1)))
            .expect_err("PoP for another node identity must be rejected");
        assert!(error.to_string().contains("does not bind"));

        let mut wrong_key = genesis(1);
        let other_key = BlsSecretKey::from_seed(&seed(2));
        wrong_key.validators[0].bls_public_key = hex::encode(other_key.public_key().to_bytes());
        let error = resolve_node_runtime_config(&wrong_key, &node_file, &hex::encode(seed(1)))
            .expect_err("PoP for another BLS key must be rejected");
        assert!(
            error.to_string().contains("does not match")
                || error.to_string().contains("does not bind")
        );
    }

    #[test]
    fn rejects_duplicate_genesis_bls_key() {
        let mut duplicate = genesis(2);
        duplicate.validators[1].bls_public_key = duplicate.validators[0].bls_public_key.clone();
        let error = resolve_node_runtime_config(&duplicate, &node(1, 2), &hex::encode(seed(1)))
            .expect_err("duplicate BLS keys must be rejected");
        assert!(error.to_string().contains("bls_public_key"));
    }

    #[test]
    fn rejects_genesis_with_more_than_the_protocol_member_limit() {
        let genesis = GenesisFile {
            schema_version: NODE_CONFIG_SCHEMA_VERSION,
            chain_id: "local-chain".to_string(),
            epoch: 0,
            view_timeout_ms: 100,
            validators: (1..=22).map(validator).collect(),
            hyck_allocations: Vec::new(),
        };
        let error = validate_genesis_shape(&genesis)
            .expect_err("genesis with more than 21 validators must fail");

        assert!(error.to_string().contains("at most 21"));
    }

    #[test]
    fn resolved_debug_does_not_include_bls_seed() {
        let genesis = genesis(1);
        let node = node(1, 1);
        let seed_hex = hex::encode(seed(1));
        let resolved =
            resolve_node_runtime_config(&genesis, &node, &seed_hex).expect("config should resolve");

        assert!(!format!("{resolved:?}").contains(&seed_hex));
    }
}
