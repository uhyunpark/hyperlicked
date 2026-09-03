//! Versioned, deterministic full application-state commitments.
//!
//! This is the authenticated application-state commitment carried by
//! `Block::app_hash`.  The encoder is kept explicit rather than relying on
//! `serde`/`bincode`: several authoritative fields are backed by `HashMap`,
//! and a wire serializer is not a consensus canonicalizer unless map ordering
//! is specified separately.
//!
//! ## State boundary
//!
//! Included fields are the values that can affect replay or the canonical
//! progress of the application: accounts and positions, complete orderbooks,
//! market/risk configuration, prices, funding, staking, oracle state,
//! trigger orders, and block progress (`timestamp`, `current_view`, and
//! `committed_height`).  `chain_domain` is included in the root preimage as a
//! chain binding.  `allow_dev_envelopes` is local node policy and is excluded.
//!
//! The following values are deliberately not authoritative state and are
//! excluded: the mempool, pending event/output queues, execution artifacts,
//! and API-oriented trade/candle/daily-stat caches. Derived
//! indexes are excluded: import/recovery rebuilds them from
//! their primary records, while normal execution validates them without
//! mutation before a candidate can be accepted.
//! Pending staking evidence is included because it is an application queue,
//! unlike the pending event queues on `AppState`.

use std::cmp::Reverse;
use std::collections::VecDeque;

use sha2::{Digest, Sha256};

use crate::app::oracle::types::{OracleConfig, OraclePrice, PriceSource};
use crate::app::staking::types::{
    Delegation, EpochSnapshot, Evidence, EvidenceType, LivenessRecord, UnstakeRequest,
    ValidatorInfo, ValidatorStatus,
};
use crate::app::trigger::{TriggerCondition, TriggerOrder, TriggerOrderStatus, TriggerType};
use crate::app::{Account, MarketConfig, Order, OrderBook, OrderType, Side};
use crate::types::{Hash, NodeId};

/// Component-tree schema version.  The fixed component tree is intentionally
/// a new schema: its root is not byte-compatible with the historical flat
/// schema-v2 preimage above.
pub const COMPONENT_TREE_SCHEMA_VERSION: u16 = crate::types::CONSENSUS_STATE_ROOT_SCHEMA_VERSION;

/// Domain separator for the component-tree root.
pub const COMPONENT_TREE_ROOT_DOMAIN: &[u8] = b"HYPERLICKED_COMPONENT_TREE_ROOT\0";

/// Domain separator for each component leaf.
pub const COMPONENT_TREE_COMPONENT_DOMAIN: &[u8] = b"HYPERLICKED_COMPONENT_TREE_COMPONENT\0";

/// Schema/domain returned by [`super::AppState::compute_full_state_root`].
pub const FULL_STATE_SCHEMA_VERSION: u16 = COMPONENT_TREE_SCHEMA_VERSION;
pub const FULL_STATE_ROOT_DOMAIN: &[u8] = COMPONENT_TREE_ROOT_DOMAIN;

/// Number and fixed bit positions of the component leaves.  The dirty mask is
/// deliberately transient: it is never serialized or included in a root.
pub(crate) const COMPONENT_COUNT: usize = 9;
pub(crate) type ComponentDirtyMask = u16;
pub(crate) const COMPONENT_DIRTY_NONE: ComponentDirtyMask = 0;
pub(crate) const COMPONENT_DIRTY_METADATA: ComponentDirtyMask = 1 << 0;
pub(crate) const COMPONENT_DIRTY_ACCOUNTS: ComponentDirtyMask = 1 << 1;
pub(crate) const COMPONENT_DIRTY_ORDERBOOKS: ComponentDirtyMask = 1 << 2;
pub(crate) const COMPONENT_DIRTY_MARKET_CONFIGS: ComponentDirtyMask = 1 << 3;
pub(crate) const COMPONENT_DIRTY_PRICES: ComponentDirtyMask = 1 << 4;
pub(crate) const COMPONENT_DIRTY_FUNDING: ComponentDirtyMask = 1 << 5;
pub(crate) const COMPONENT_DIRTY_STAKING: ComponentDirtyMask = 1 << 6;
pub(crate) const COMPONENT_DIRTY_TRIGGERS: ComponentDirtyMask = 1 << 7;
pub(crate) const COMPONENT_DIRTY_ORACLE: ComponentDirtyMask = 1 << 8;
pub(crate) const COMPONENT_DIRTY_ALL: ComponentDirtyMask = (1 << COMPONENT_COUNT) - 1;
pub(crate) const COMPONENT_DIRTY_UNKNOWN: ComponentDirtyMask = 1 << 15;

/// Transient invalidation tracker. Cloning a state intentionally forgets a
/// clean baseline so an arbitrary branch cannot silently inherit one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DirtyTracker {
    mask: ComponentDirtyMask,
}

impl DirtyTracker {
    pub(crate) const fn all() -> Self {
        Self {
            mask: COMPONENT_DIRTY_ALL,
        }
    }

    pub(crate) const fn unknown() -> Self {
        Self {
            mask: COMPONENT_DIRTY_ALL | COMPONENT_DIRTY_UNKNOWN,
        }
    }

    pub(crate) const fn from_bits(mask: ComponentDirtyMask) -> Self {
        Self { mask }
    }

    pub(crate) fn mark(&mut self, mask: ComponentDirtyMask) {
        self.mask |= mask;
    }

    pub(crate) fn bits(&self) -> ComponentDirtyMask {
        self.mask
    }

    pub(crate) fn clear(&mut self) {
        self.mask = COMPONENT_DIRTY_NONE;
    }
}

impl Clone for DirtyTracker {
    fn clone(&self) -> Self {
        Self::unknown()
    }
}

/// Complete tree retained by a speculative candidate. The tree is a seal that
/// must agree with the authenticated block root; callers at persistence
/// boundaries independently recompute a fresh tree before accepting it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComponentTree {
    pub(crate) root: Hash,
    pub(crate) components: [Hash; COMPONENT_COUNT],
    pub(crate) chain_domain: Hash,
}

#[cfg(test)]
const FLAT_SCHEMA_V2_VERSION: u16 = 2;
#[cfg(test)]
const FLAT_SCHEMA_V2_ROOT_DOMAIN: &[u8] = b"HYPERLICKED_FULL_STATE_ROOT\0";

/// Fixed component order.  This is part of the component-tree schema and must
/// only change together with [`COMPONENT_TREE_SCHEMA_VERSION`].
const COMPONENT_NAMES: [&[u8]; 9] = [
    b"metadata",
    b"accounts",
    b"orderbooks",
    b"market_configs",
    b"prices",
    b"funding",
    b"staking",
    b"triggers",
    b"oracle",
];

/// Compute the fixed component-tree full-state root.
pub(crate) fn compute(state: &super::AppState) -> Hash {
    compute_tree(state).root
}

/// Compute every component and the parent root without consulting the dirty
/// tracker.  This is the independent oracle used by public root reads and by
/// preflight/commit seal verification.
pub(crate) fn compute_tree(state: &super::AppState) -> ComponentTree {
    let components = compute_component_roots(state);
    let root = compute_component_tree_root(&state.chain_domain, &components);
    ComponentTree {
        root,
        components,
        chain_domain: state.chain_domain,
    }
}

/// Derive a child tree from a verified parent tree. Only leaves named by the
/// known dirty mask are re-encoded; the parent root is always rebuilt from all
/// nine leaf hashes. Unknown bits conservatively force all leaf recomputation.
pub(crate) fn derive_tree(
    state: &super::AppState,
    parent: &ComponentTree,
    dirty: ComponentDirtyMask,
) -> ComponentTree {
    if parent.chain_domain != state.chain_domain {
        return compute_tree(state);
    }
    let mask = if dirty & !COMPONENT_DIRTY_ALL != 0 {
        COMPONENT_DIRTY_ALL
    } else {
        dirty & COMPONENT_DIRTY_ALL
    };
    let mut components = parent.components;
    for (index, component) in components.iter_mut().enumerate() {
        if mask & (1 << index) != 0 {
            *component = compute_component_root(state, index);
        }
    }
    let root = compute_component_tree_root(&state.chain_domain, &components);
    ComponentTree {
        root,
        components,
        chain_domain: state.chain_domain,
    }
}

fn compute_component_tree_root(
    chain_domain: &Hash,
    component_roots: &[Hash; COMPONENT_NAMES.len()],
) -> Hash {
    let mut root = Encoder::default();

    root.raw(COMPONENT_TREE_ROOT_DOMAIN);
    root.u16(COMPONENT_TREE_SCHEMA_VERSION);
    root.hash(chain_domain);
    root.u8(COMPONENT_NAMES.len() as u8);

    for (index, (name, component_root)) in COMPONENT_NAMES
        .iter()
        .zip(component_roots.iter())
        .enumerate()
    {
        // Include both the fixed index and name in the parent preimage.  A
        // reordered list can therefore never be interpreted as the same tree.
        root.u8(index as u8);
        root.bytes(name);
        root.hash(component_root);
    }

    Sha256::digest(root.finish()).into()
}

/// Compute the legacy flat schema-v2 layout for migration diagnostics. It is
/// not accepted as a current consensus root after the fixed-supply schema
/// bump; new callers must use the fixed component tree above.
#[cfg(test)]
fn compute_schema_v2(state: &super::AppState) -> Hash {
    let mut encoder = Encoder::default();

    // Domain, schema, and chain binding are outside the state field stream.
    // This prevents a state root from being replayed across chains and keeps
    // protocol-version changes cryptographically distinct.
    encoder.raw(FLAT_SCHEMA_V2_ROOT_DOMAIN);
    encoder.u16(FLAT_SCHEMA_V2_VERSION);
    encoder.hash(&state.chain_domain);

    encode_metadata(&mut encoder, state);
    encode_accounts(&mut encoder, state);
    encode_orderbooks(&mut encoder, state);
    encode_market_configs(&mut encoder, state);
    encode_prices(&mut encoder, state);
    encode_funding(&mut encoder, state);
    encode_staking(&mut encoder, state);
    encode_triggers(&mut encoder, state);
    encode_oracle(&mut encoder, state);

    Sha256::digest(encoder.finish()).into()
}

/// Compute one domain-separated leaf for every fixed component.
fn compute_component_roots(state: &super::AppState) -> [Hash; COMPONENT_NAMES.len()] {
    std::array::from_fn(|index| compute_component_root(state, index))
}

fn compute_component_root(state: &super::AppState, index: usize) -> Hash {
    let mut leaf = Encoder::default();
    leaf.raw(COMPONENT_TREE_COMPONENT_DOMAIN);
    leaf.u16(COMPONENT_TREE_SCHEMA_VERSION);
    leaf.u8(index as u8);
    leaf.bytes(COMPONENT_NAMES[index]);
    leaf.bytes(&encode_component(state, index));
    Sha256::digest(leaf.finish()).into()
}

fn encode_component(state: &super::AppState, index: usize) -> Vec<u8> {
    let mut encoder = Encoder::default();
    match index {
        0 => encode_metadata(&mut encoder, state),
        1 => encode_accounts(&mut encoder, state),
        2 => encode_orderbooks(&mut encoder, state),
        3 => encode_market_configs(&mut encoder, state),
        4 => encode_prices(&mut encoder, state),
        5 => encode_funding(&mut encoder, state),
        6 => encode_staking(&mut encoder, state),
        7 => encode_triggers(&mut encoder, state),
        8 => encode_oracle(&mut encoder, state),
        _ => unreachable!("component index is bounded by COMPONENT_NAMES"),
    }
    encoder.finish()
}

/// Small canonical byte writer.  Variable-length values and all collection
/// boundaries are length-prefixed; fixed-width integers use little endian.
#[derive(Default)]
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn len(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn bytes(&mut self, value: &[u8]) {
        self.len(value.len());
        self.raw(value);
    }

    fn hash(&mut self, value: &Hash) {
        self.raw(value);
    }

    fn node_id(&mut self, value: &NodeId) {
        self.raw(value);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn tag(&mut self, value: &'static [u8]) {
        self.bytes(value);
    }

    fn option<T>(&mut self, value: Option<&T>, encode: impl FnOnce(&mut Self, &T)) {
        match value {
            Some(value) => {
                self.u8(1);
                encode(self, value);
            }
            None => self.u8(0),
        }
    }
}

fn encode_metadata(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"metadata");
    e.u64(state.timestamp);
    e.u64(state.current_view);
    e.u64(state.committed_height);
}

fn encode_accounts(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"accounts");
    let mut accounts = state.accounts.all_accounts();
    accounts.sort_by(|left, right| left.address.cmp(&right.address));
    e.len(accounts.len());
    for account in accounts {
        encode_account(e, &account);
    }
}

fn encode_account(e: &mut Encoder, account: &Account) {
    e.string(&account.address);
    e.i64(account.hyck_balance);
    e.i64(account.balance);
    e.i64(account.locked);
    e.u64(account.nonce);

    e.tag(b"pending_nonces");
    e.len(account.pending_nonces.len());
    for nonce in &account.pending_nonces {
        e.u64(*nonce);
    }

    let mut positions: Vec<_> = account.positions.iter().collect();
    positions.sort_by(|left, right| left.0.cmp(right.0));
    e.tag(b"positions");
    e.len(positions.len());
    for (symbol, position) in positions {
        e.string(symbol);
        e.i64(position.size);
        e.i64(position.entry_price);
        e.i64(position.realized_pnl);
        e.i64(position.cumulative_funding);
        e.u64(position.last_funding_timestamp);
    }
}

fn encode_orderbooks(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"orderbooks");
    let mut books: Vec<_> = state.orderbooks.iter().collect();
    books.sort_by(|left, right| left.0.cmp(right.0));
    e.len(books.len());
    for (symbol, book) in books {
        e.string(symbol);
        encode_orderbook(e, book);
    }
}

fn encode_orderbook(e: &mut Encoder, book: &OrderBook) {
    // The authoritative order sequence and full FIFO queues are included.
    e.string(book.symbol());
    e.u64(book.seq);
    e.i64(book.last_price());

    e.tag(b"bids");
    e.len(book.bids.len());
    for (Reverse(price), orders) in &book.bids {
        e.i64(*price);
        encode_orders(e, orders);
    }

    e.tag(b"asks");
    e.len(book.asks.len());
    for (price, orders) in &book.asks {
        e.i64(*price);
        encode_orders(e, orders);
    }
}

fn encode_orders(e: &mut Encoder, orders: &VecDeque<Order>) {
    e.len(orders.len());
    for order in orders {
        encode_order(e, order);
    }
}

fn encode_order(e: &mut Encoder, order: &Order) {
    e.string(&order.id);
    e.string(&order.trader);
    e.string(&order.symbol);
    e.u8(side_code(order.side));
    e.i64(order.price);
    e.i64(order.size);
    e.i64(order.original_size);
    e.u8(order_type_code(order.order_type));
    e.bool(order.reduce_only);
    e.u64(order.timestamp);
    e.i64(order.locked_margin);
}

fn encode_market_configs(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"market_configs");
    let mut configs: Vec<_> = state.configs.iter().collect();
    configs.sort_by(|left, right| left.0.cmp(right.0));
    e.len(configs.len());
    for (symbol, config) in configs {
        e.string(symbol);
        encode_market_config(e, config);
    }
}

fn encode_market_config(e: &mut Encoder, config: &MarketConfig) {
    e.string(&config.symbol);
    e.i64(config.tick_size);
    e.i64(config.lot_size);
    e.i64(config.min_notional);
    e.i64(config.maker_fee);
    e.i64(config.taker_fee);
    e.u64(config.funding_interval_ms);
    e.i64(config.interest_rate_bps);
    e.i64(config.max_funding_rate_bps);
    e.i64(config.max_order_size);
    e.i64(config.max_position_size);
    e.u64(config.max_open_orders as u64);
    e.u64(config.max_price_levels as u64);
    e.i64(config.ema_alpha_bps);
}

fn encode_prices(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"mark_prices");
    encode_sorted_string_i64_map(e, &state.mark_prices);

    e.tag(b"mark_price_ema");
    encode_sorted_string_i64_map(e, &state.mark_price_ema);
}

fn encode_funding(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"insurance_fund");
    e.i64(state.insurance_fund);

    e.tag(b"premium_samples");
    let mut samples: Vec<_> = state.premium_samples.iter().collect();
    samples.sort_by(|left, right| left.0.cmp(right.0));
    e.len(samples.len());
    for (symbol, values) in samples {
        e.string(symbol);
        e.len(values.len());
        for value in values {
            e.i64(*value);
        }
    }

    e.tag(b"current_funding_rates");
    encode_sorted_string_i64_map(e, &state.current_funding_rates);

    e.tag(b"last_funding_times");
    let mut times: Vec<_> = state.last_funding_times.iter().collect();
    times.sort_by(|left, right| left.0.cmp(right.0));
    e.len(times.len());
    for (symbol, time) in times {
        e.string(symbol);
        e.u64(*time);
    }
}

fn encode_sorted_string_i64_map(e: &mut Encoder, values: &std::collections::HashMap<String, i64>) {
    let mut entries: Vec<_> = values.iter().collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    e.len(entries.len());
    for (key, value) in entries {
        e.string(key);
        e.i64(*value);
    }
}

fn encode_staking(e: &mut Encoder, state: &super::AppState) {
    let staking = &state.staking;
    e.tag(b"staking");
    e.u64(staking.current_epoch);
    e.i64(staking.total_staked);
    e.i64(staking.emissions_reserve);
    e.u64(staking.last_reward_accrual_timestamp);
    e.bool(staking.reward_clock_initialized);
    e.u64(staking.reward_accrual_remainder);
    e.u64(staking.last_reward_compound_timestamp);
    e.bool(staking.enabled);

    e.tag(b"validators");
    let mut validators: Vec<_> = staking.validators.iter().collect();
    validators.sort_by(|left, right| left.0.cmp(right.0));
    e.len(validators.len());
    for (operator, validator) in validators {
        e.string(operator);
        encode_validator(e, validator);
    }

    e.tag(b"delegations");
    let mut delegations: Vec<_> = staking.delegations.iter().collect();
    delegations.sort_by(|left, right| left.0.cmp(right.0));
    e.len(delegations.len());
    for ((delegator, validator), delegation) in delegations {
        e.string(delegator);
        e.string(validator);
        encode_delegation(e, delegation);
    }

    e.tag(b"unstake_queue");
    let mut queues: Vec<_> = staking.unstake_queue.iter().collect();
    queues.sort_by(|left, right| left.0.cmp(right.0));
    e.len(queues.len());
    for (delegator, requests) in queues {
        e.string(delegator);
        e.len(requests.len());
        for request in requests {
            encode_unstake_request(e, request);
        }
    }

    e.tag(b"epoch_snapshot");
    encode_epoch_snapshot(e, staking.epoch_snapshot.as_ref());

    e.tag(b"liveness");
    let mut liveness: Vec<_> = staking.liveness.iter().collect();
    liveness.sort_by(|left, right| left.0.cmp(right.0));
    e.len(liveness.len());
    for (node_id, record) in liveness {
        e.node_id(node_id);
        encode_liveness(e, record);
    }

    // Evidence is an ordered application queue.  Preserve queue order rather
    // than sorting: two pending proofs can have distinct processing order.
    e.tag(b"pending_evidence");
    e.len(staking.pending_evidence.len());
    for evidence in &staking.pending_evidence {
        encode_evidence(e, evidence);
    }
}

fn encode_validator(e: &mut Encoder, validator: &ValidatorInfo) {
    e.string(&validator.operator);
    e.node_id(&validator.node_id);
    e.bytes(&validator.bls_pubkey);
    e.bytes(&validator.bls_proof_of_possession);
    e.i64(validator.self_stake);
    e.i64(validator.total_stake);
    e.i64(validator.commission_bps);
    e.u8(validator_status_code(validator.status));
    e.i64(validator.pending_rewards);
    e.i64(validator.reward_eligible_stake);
    e.u64(validator.jail_until);
    e.u64(validator.missed_consecutive);
    e.u64(validator.blocks_proposed);
    e.u64(validator.votes_cast);
}

fn encode_delegation(e: &mut Encoder, delegation: &Delegation) {
    e.string(&delegation.delegator);
    e.string(&delegation.validator);
    e.i64(delegation.amount);
    e.i64(delegation.pending_rewards);
    e.i64(delegation.reward_eligible_stake);
}

fn encode_unstake_request(e: &mut Encoder, request: &UnstakeRequest) {
    e.string(&request.delegator);
    e.option(request.validator.as_ref(), |e, validator| {
        e.string(validator)
    });
    e.i64(request.amount);
    e.u64(request.completion_time);
}

fn encode_epoch_snapshot(e: &mut Encoder, snapshot: Option<&EpochSnapshot>) {
    e.option(snapshot, |e, snapshot| {
        e.u64(snapshot.epoch);
        e.u64(snapshot.start_view);
        e.len(snapshot.active_validators.len());
        for node_id in &snapshot.active_validators {
            e.node_id(node_id);
        }
        e.i64(snapshot.total_staked);
        e.u64(snapshot.timestamp);
    });
}

fn encode_liveness(e: &mut Encoder, record: &LivenessRecord) {
    e.u64(record.expected_proposals);
    e.u64(record.actual_proposals);
    e.u64(record.expected_votes);
    e.u64(record.actual_votes);
}

fn encode_evidence(e: &mut Encoder, evidence: &Evidence) {
    e.u8(evidence_type_code(evidence.evidence_type));
    e.node_id(&evidence.offender);
    e.u64(evidence.view);
    e.u64(evidence.timestamp);
    e.u64(evidence.context.epoch);
    e.hash(&evidence.context.committee_hash);
    e.hash(&evidence.context.genesis_hash);
    e.hash(&evidence.hash_a);
    e.hash(&evidence.app_hash_a);
    e.hash(&evidence.hash_b);
    e.hash(&evidence.app_hash_b);
    e.bytes(&evidence.signature_a);
    e.bytes(&evidence.signature_b);
}

fn encode_triggers(e: &mut Encoder, state: &super::AppState) {
    e.tag(b"trigger_orders");
    let mut orders: Vec<_> = state.trigger_orders.iter().collect();
    orders.sort_by(|left, right| left.0.cmp(right.0));
    e.len(orders.len());
    for (id, order) in orders {
        e.string(id);
        encode_trigger_order(e, order);
    }
    e.u64(state.trigger_seq);
}

fn encode_trigger_order(e: &mut Encoder, order: &TriggerOrder) {
    e.string(&order.id);
    e.option(order.cloid.as_ref(), |e, cloid| e.string(cloid));
    e.string(&order.trader);
    e.string(&order.symbol);
    e.u8(side_code(order.side));
    e.i64(order.size);
    e.u8(trigger_type_code(order.trigger_type));
    e.u8(trigger_condition_code(order.condition));
    e.i64(order.trigger_price);
    e.option(order.limit_price.as_ref(), |e, price| e.i64(*price));
    e.bool(order.reduce_only);
    e.u64(order.timestamp);
    e.u8(trigger_status_code(order.status));
}

fn encode_oracle(e: &mut Encoder, state: &super::AppState) {
    let oracle = &state.oracle;
    e.tag(b"oracle");
    e.bool(oracle.enabled);

    e.tag(b"prices");
    let mut prices: Vec<_> = oracle.prices.iter().collect();
    prices.sort_by(|left, right| left.0.cmp(right.0));
    e.len(prices.len());
    for (symbol, price) in prices {
        e.string(symbol);
        encode_oracle_price(e, price);
    }

    e.tag(b"source_prices");
    let mut source_prices: Vec<_> = oracle.source_prices.iter().collect();
    source_prices.sort_by(|left, right| left.0.cmp(right.0));
    e.len(source_prices.len());
    for (symbol, sources) in source_prices {
        e.string(symbol);
        let mut sources = sources.clone();
        // Source order is not meaningful to aggregation; sort to make this
        // audit state independent from input/vector insertion order.
        sources.sort_by(|left, right| {
            left.source_id
                .cmp(&right.source_id)
                .then(left.price.cmp(&right.price))
                .then(left.timestamp.cmp(&right.timestamp))
                .then(left.weight_bps.cmp(&right.weight_bps))
        });
        e.len(sources.len());
        for source in &sources {
            encode_price_source(e, source);
        }
    }

    e.tag(b"configs");
    let mut configs: Vec<_> = oracle.configs.iter().collect();
    configs.sort_by(|left, right| left.0.cmp(right.0));
    e.len(configs.len());
    for (symbol, config) in configs {
        e.string(symbol);
        encode_oracle_config(e, config);
    }

    e.tag(b"last_update");
    let mut updates: Vec<_> = oracle.last_update.iter().collect();
    updates.sort_by(|left, right| left.0.cmp(right.0));
    e.len(updates.len());
    for (symbol, timestamp) in updates {
        e.string(symbol);
        e.u64(*timestamp);
    }
}

fn encode_oracle_price(e: &mut Encoder, price: &OraclePrice) {
    e.string(&price.symbol);
    e.i64(price.price);
    e.u64(price.timestamp);
    e.u32(price.source_count);
    e.i64(price.confidence_bps);
}

fn encode_price_source(e: &mut Encoder, source: &PriceSource) {
    e.string(&source.source_id);
    e.i64(source.price);
    e.u64(source.timestamp);
    e.i64(source.weight_bps);
}

fn encode_oracle_config(e: &mut Encoder, config: &OracleConfig) {
    e.u64(config.max_staleness_ms);
    e.u32(config.min_sources);
    e.i64(config.max_deviation_bps);
    e.bool(config.fallback_to_mark);
}

fn side_code(side: Side) -> u8 {
    match side {
        Side::Bid => 0,
        Side::Ask => 1,
    }
}

fn order_type_code(order_type: OrderType) -> u8 {
    match order_type {
        OrderType::Gtc => 0,
        OrderType::Ioc => 1,
        OrderType::Alo => 2,
    }
}

fn validator_status_code(status: ValidatorStatus) -> u8 {
    match status {
        ValidatorStatus::Active => 0,
        ValidatorStatus::Inactive => 1,
        ValidatorStatus::Jailed => 2,
        ValidatorStatus::Tombstoned => 3,
    }
}

fn evidence_type_code(evidence_type: EvidenceType) -> u8 {
    match evidence_type {
        EvidenceType::DoubleVote => 0,
        EvidenceType::DoublePropose => 1,
    }
}

fn trigger_type_code(trigger_type: TriggerType) -> u8 {
    match trigger_type {
        TriggerType::StopLoss => 0,
        TriggerType::TakeProfit => 1,
    }
}

fn trigger_condition_code(condition: TriggerCondition) -> u8 {
    match condition {
        TriggerCondition::PriceAbove => 0,
        TriggerCondition::PriceBelow => 1,
    }
}

fn trigger_status_code(status: TriggerOrderStatus) -> u8 {
    match status {
        TriggerOrderStatus::Pending => 0,
        TriggerOrderStatus::Triggered => 1,
        TriggerOrderStatus::Cancelled => 2,
        TriggerOrderStatus::Failed => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::candles::Candle;
    use crate::app::MarketConfig;

    fn deterministic_state(domain: [u8; 32]) -> super::super::AppState {
        let mut state = super::super::AppState::new_with_chain_domain(domain);
        state.timestamp = 1_700_000_000_000;
        state.current_view = 17;
        state.committed_height = 9;
        state
    }

    #[test]
    fn component_tree_root_golden_vector() {
        let state = deterministic_state([0x11; 32]);
        let root = compute(&state);
        assert_eq!(
            hex::encode(root),
            "cb5c85d256e7e143e43bca8b1f7e6745a2441b8a47d8e93216790650432a8089"
        );
    }

    #[test]
    fn flat_schema_v2_root_golden_vector_for_fixed_supply_state() {
        let state = deterministic_state([0x11; 32]);
        assert_eq!(
            hex::encode(compute_schema_v2(&state)),
            "fbea657fc1837489fde5ce36da268b12788a7d01c69388727821cfd8d80fab49"
        );
    }

    #[test]
    fn reward_authority_fields_are_root_authenticated() {
        use crate::app::staking::{Delegation, ValidatorInfo};

        let base = deterministic_state([0x12; 32]);
        let root = compute(&base);
        for changed in [
            {
                let mut state = base.clone();
                state.staking.emissions_reserve -= 1;
                state
            },
            {
                let mut state = base.clone();
                state.staking.last_reward_accrual_timestamp = 7;
                state
            },
            {
                let mut state = base.clone();
                state.staking.reward_clock_initialized = true;
                state
            },
            {
                let mut state = base.clone();
                state.staking.reward_accrual_remainder = 9;
                state
            },
            {
                let mut state = base.clone();
                state.staking.last_reward_compound_timestamp = 11;
                state
            },
        ] {
            assert_ne!(root, compute(&changed));
        }

        let mut validator_state = base.clone();
        validator_state.staking.validators.insert(
            "validator".into(),
            ValidatorInfo::new(
                "validator".into(),
                [3u8; 32],
                vec![4u8; 48],
                vec![5u8; 96],
                10,
                0,
            ),
        );
        let validator_root = compute(&validator_state);
        validator_state
            .staking
            .validators
            .get_mut("validator")
            .unwrap()
            .reward_eligible_stake = 9;
        assert_ne!(validator_root, compute(&validator_state));

        let mut delegation_state = base;
        delegation_state.staking.delegations.insert(
            ("delegator".into(), "validator".into()),
            Delegation::new("delegator".into(), "validator".into(), 10),
        );
        let delegation_root = compute(&delegation_state);
        delegation_state
            .staking
            .delegations
            .get_mut(&("delegator".into(), "validator".into()))
            .unwrap()
            .reward_eligible_stake = 9;
        assert_ne!(delegation_root, compute(&delegation_state));
    }

    #[test]
    fn full_state_root_is_insertion_order_independent() {
        fn state_with_markets(order: &[&str]) -> super::super::AppState {
            let mut state = deterministic_state([0x22; 32]);
            state.orderbooks.clear();
            state.configs.clear();
            state.mark_prices.clear();
            for symbol in order {
                state.add_market(MarketConfig {
                    symbol: (*symbol).to_string(),
                    ..MarketConfig::default()
                });
            }
            for (address, amount) in [("bob", 20), ("alice", 10)] {
                state.accounts.deposit(address, amount).unwrap();
            }
            state.current_funding_rates.insert("ETH-USDT".into(), 4);
            state.current_funding_rates.insert("BTC-USDT".into(), 3);
            state
        }

        let left = state_with_markets(&["BTC-USDT", "ETH-USDT", "SOL-USDT"]);
        let right = state_with_markets(&["SOL-USDT", "ETH-USDT", "BTC-USDT"]);
        assert_eq!(compute(&left), compute(&right));
    }

    #[test]
    fn component_tree_binds_fixed_order_and_domains() {
        let state = deterministic_state([0x23; 32]);
        let leaves = compute_component_roots(&state);
        let root = compute_component_tree_root(&state.chain_domain, &leaves);

        let mut reordered = leaves;
        reordered.swap(0, 1);
        assert_ne!(
            root,
            compute_component_tree_root(&state.chain_domain, &reordered)
        );
        assert_eq!(FULL_STATE_SCHEMA_VERSION, COMPONENT_TREE_SCHEMA_VERSION);
        assert_eq!(FULL_STATE_ROOT_DOMAIN, COMPONENT_TREE_ROOT_DOMAIN);
        assert_ne!(COMPONENT_TREE_ROOT_DOMAIN, FLAT_SCHEMA_V2_ROOT_DOMAIN);
        assert_ne!(COMPONENT_TREE_COMPONENT_DOMAIN, COMPONENT_TREE_ROOT_DOMAIN);
        assert_ne!(compute(&state), compute_schema_v2(&state));
    }

    #[test]
    fn dirty_subtree_derivation_matches_fresh_for_each_component() {
        fn assert_component(
            parent: &super::super::AppState,
            parent_tree: &ComponentTree,
            dirty: ComponentDirtyMask,
            mutate: impl FnOnce(&mut super::super::AppState),
        ) {
            let mut child = parent.clone_for_verified_component_child();
            mutate(&mut child);
            child.mark_full_state_dirty(dirty);

            let derived = child.derive_full_state_tree(Some(parent_tree));
            let fresh = compute_tree(&child);
            assert_eq!(derived, fresh);
            assert_eq!(child.full_state_dirty(), COMPONENT_DIRTY_NONE);
        }

        let mut parent = deterministic_state([0x25; 32]);
        let parent_tree = parent.derive_full_state_tree(None);
        assert_eq!(parent.full_state_dirty(), COMPONENT_DIRTY_NONE);

        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_METADATA, |state| {
            state.timestamp += 1
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_ACCOUNTS, |state| {
            state.accounts.deposit("dirty-account", 1).unwrap()
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_ORDERBOOKS, |state| {
            state
                .orderbooks
                .get_mut("BTC-USDT")
                .unwrap()
                .add_bid(Order {
                    id: "BTC-USDT_dirty".into(),
                    trader: "dirty-order".into(),
                    symbol: "BTC-USDT".into(),
                    side: Side::Bid,
                    price: 4_999_900,
                    size: 100,
                    original_size: 100,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                    timestamp: 10,
                    locked_margin: 1,
                });
        });
        assert_component(
            &parent,
            &parent_tree,
            COMPONENT_DIRTY_MARKET_CONFIGS,
            |state| state.configs.get_mut("BTC-USDT").unwrap().tick_size += 1,
        );
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_PRICES, |state| {
            state.mark_prices.insert("BTC-USDT".into(), 5_000_001);
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_FUNDING, |state| {
            state.insurance_fund += 1
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_STAKING, |state| {
            state.staking.current_epoch += 1
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_TRIGGERS, |state| {
            state.trigger_seq += 1
        });
        assert_component(&parent, &parent_tree, COMPONENT_DIRTY_ORACLE, |state| {
            state.oracle.enabled = !state.oracle.enabled
        });
    }

    #[test]
    fn unknown_reserved_dirty_clone_and_chain_domain_change_force_full_derivation() {
        let mut parent = deterministic_state([0x26; 32]);
        let parent_tree = parent.derive_full_state_tree(None);

        let mut cloned = parent.clone();
        assert_eq!(
            cloned.full_state_dirty(),
            COMPONENT_DIRTY_ALL | COMPONENT_DIRTY_UNKNOWN,
            "ordinary AppState::clone must invalidate a clean baseline"
        );
        cloned.timestamp += 1;
        let cloned_derived = cloned.derive_full_state_tree(Some(&parent_tree));
        assert_eq!(cloned_derived, compute_tree(&cloned));
        assert_eq!(cloned.full_state_dirty(), COMPONENT_DIRTY_NONE);

        let mut unknown = parent.clone_for_verified_component_child();
        unknown.timestamp += 1;
        unknown.mark_full_state_dirty_unknown();
        let derived = unknown.derive_full_state_tree(Some(&parent_tree));
        assert_eq!(derived, compute_tree(&unknown));
        assert_eq!(unknown.full_state_dirty(), COMPONENT_DIRTY_NONE);

        let mut reserved = parent.clone_for_verified_component_child();
        reserved.timestamp += 1;
        let reserved_bit: ComponentDirtyMask = 1 << 14;
        reserved.mark_full_state_dirty(reserved_bit);
        let reserved_derived = reserved.derive_full_state_tree(Some(&parent_tree));
        assert_eq!(reserved_derived, compute_tree(&reserved));
        assert_eq!(reserved.full_state_dirty(), COMPONENT_DIRTY_NONE);

        let mut changed_domain = parent.clone_for_verified_component_child();
        changed_domain.set_chain_domain([0x27; 32]);
        let derived = changed_domain.derive_full_state_tree(Some(&parent_tree));
        assert_eq!(derived, compute_tree(&changed_domain));
        assert_eq!(changed_domain.full_state_dirty(), COMPONENT_DIRTY_NONE);
    }

    #[test]
    fn clone_branch_and_snapshot_start_with_independent_dirty_tracking() {
        let mut parent = deterministic_state([0x28; 32]);
        let _ = parent.derive_full_state_tree(None);
        let mut branch = parent.clone_for_verified_component_child();
        branch.accounts_mut().deposit("branch-account", 1).unwrap();

        assert_eq!(parent.full_state_dirty(), COMPONENT_DIRTY_NONE);
        assert_ne!(branch.full_state_dirty(), COMPONENT_DIRTY_NONE);
        let restored = super::super::AppState::try_from_snapshot_with_chain_domain(
            parent.create_snapshot(0),
            [0x28; 32],
            true,
        )
        .expect("valid snapshot should restore");
        assert_eq!(
            restored.full_state_dirty() & COMPONENT_DIRTY_ALL,
            COMPONENT_DIRTY_ALL
        );
    }

    #[test]
    fn each_authoritative_component_changes_only_its_leaf() {
        fn assert_component_mutation(
            state: super::super::AppState,
            component: usize,
            mutate: impl FnOnce(&mut super::super::AppState),
        ) {
            let before = compute_component_roots(&state);
            let before_root = compute(&state);
            let mut changed = state;
            mutate(&mut changed);
            let after = compute_component_roots(&changed);
            assert_ne!(before[component], after[component]);
            for index in 0..COMPONENT_NAMES.len() {
                if index != component {
                    assert_eq!(before[index], after[index], "component {index} changed");
                }
            }
            assert_ne!(before_root, compute(&changed));
        }

        let base = deterministic_state([0x24; 32]);
        assert_component_mutation(base.clone(), 0, |state| state.timestamp += 1);
        assert_component_mutation(base.clone(), 1, |state| {
            state.accounts.deposit("component-account", 1).unwrap();
        });
        assert_component_mutation(base.clone(), 2, |state| {
            state
                .orderbooks
                .get_mut("BTC-USDT")
                .unwrap()
                .add_bid(Order {
                    id: "BTC-USDT_7".into(),
                    trader: "component-order".into(),
                    symbol: "BTC-USDT".into(),
                    side: Side::Bid,
                    price: 4_999_900,
                    size: 100,
                    original_size: 100,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                    timestamp: 10,
                    locked_margin: 1,
                });
        });
        assert_component_mutation(base.clone(), 3, |state| {
            state.configs.get_mut("BTC-USDT").unwrap().tick_size += 1;
        });
        assert_component_mutation(base.clone(), 4, |state| {
            state.mark_prices.insert("BTC-USDT".into(), 5_000_001);
        });
        assert_component_mutation(base.clone(), 5, |state| state.insurance_fund += 1);
        assert_component_mutation(base.clone(), 6, |state| state.staking.current_epoch += 1);
        assert_component_mutation(base.clone(), 7, |state| state.trigger_seq += 1);
        assert_component_mutation(base, 8, |state| {
            state.oracle.enabled = !state.oracle.enabled
        });
    }

    #[test]
    fn full_state_root_changes_for_authoritative_and_ignores_transient_fields() {
        let base = deterministic_state([0x33; 32]);

        let mut changed_order = base.clone();
        let order = Order {
            id: "BTC-USDT_1".into(),
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 4_999_900,
            size: 100,
            original_size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 10,
            locked_margin: 1,
        };
        changed_order
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .add_bid(order);
        assert_ne!(compute(&base), compute(&changed_order));

        let mut transient = base.clone();
        transient.pending_fills.push(crate::app::Fill {
            taker_order_id: "t".into(),
            maker_order_id: "m".into(),
            taker: "alice".into(),
            maker: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 1,
            size: 1,
            timestamp: 1,
            maker_locked_margin: 0,
            maker_original_size: 0,
        });
        transient.trade_history.insert(
            "BTC-USDT".into(),
            VecDeque::from([transient.pending_fills[0].clone()]),
        );
        transient.candle_manager_mut().load_candles(
            "BTC-USDT",
            crate::app::Interval::Min1,
            vec![Candle {
                time: 1,
                open: 1,
                high: 1,
                low: 1,
                close: 1,
                volume: 1,
                trades: 1,
            }],
        );
        transient.prev_day_prices.insert("BTC-USDT".into(), 2);
        transient.day_start = 3;
        transient.day_volume.insert("BTC-USDT".into(), 4);
        transient.day_notional_volume.insert("BTC-USDT".into(), 5);
        transient
            .submit_tx(crate::app::Transaction::Deposit {
                trader: "mempool-only".into(),
                amount: 1,
            })
            .unwrap();
        let without_index_corruption = compute(&transient);
        assert_eq!(compute(&base), without_index_corruption);

        transient
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .order_index
            .insert("derived-only".into(), (Side::Bid, 1));
        transient
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .trader_order_counts
            .insert("derived-only".into(), 1);
        transient
            .staking
            .node_to_operator
            .insert([7u8; 32], "derived-only".into());
        transient
            .trigger_orders_by_trader
            .insert("derived-only".into(), vec!["missing".into()]);
        assert!(transient.validate_derived_indexes().is_err());
        assert_eq!(without_index_corruption, compute(&transient));

        let mut different_domain = base.clone();
        different_domain.chain_domain[0] ^= 1;
        assert_ne!(compute(&base), compute(&different_domain));
    }

    #[test]
    fn high_cardinality_cow_maps_isolate_changed_keys_and_match_fresh_root() {
        let mut parent = deterministic_state([0x35; 32]);
        let fill = crate::app::Fill {
            taker_order_id: "taker".into(),
            maker_order_id: "maker".into(),
            taker: "alice".into(),
            maker: "bob".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Bid,
            price: 5_000_000,
            size: 1,
            timestamp: 1,
            maker_locked_margin: 0,
            maker_original_size: 1,
        };
        parent
            .trade_history
            .insert("BTC-USDT".into(), VecDeque::from([fill.clone()]));
        parent
            .trade_history
            .insert("ETH-USDT".into(), VecDeque::from([fill]));

        let t1 = TriggerOrder {
            id: "T1".into(),
            cloid: Some("cloid-1".into()),
            trader: "alice".into(),
            symbol: "BTC-USDT".into(),
            side: Side::Ask,
            size: 1,
            trigger_type: TriggerType::StopLoss,
            condition: TriggerCondition::PriceBelow,
            trigger_price: 4_000_000,
            limit_price: None,
            reduce_only: true,
            timestamp: 1,
            status: TriggerOrderStatus::Pending,
        };
        let t2 = TriggerOrder {
            id: "T2".into(),
            cloid: None,
            trader: "bob".into(),
            symbol: "ETH-USDT".into(),
            side: Side::Bid,
            size: 1,
            trigger_type: TriggerType::TakeProfit,
            condition: TriggerCondition::PriceAbove,
            trigger_price: 6_000_000,
            limit_price: None,
            reduce_only: true,
            timestamp: 1,
            status: TriggerOrderStatus::Pending,
        };
        parent.trigger_orders.insert("T1".into(), t1);
        parent.trigger_orders.insert("T2".into(), t2);
        parent
            .trigger_orders_by_trader
            .insert("alice".into(), vec!["T1".into()]);
        parent
            .trigger_orders_by_trader
            .insert("bob".into(), vec!["T2".into()]);
        parent
            .trigger_orders_by_symbol
            .insert("BTC-USDT".into(), vec!["T1".into()]);
        parent
            .trigger_orders_by_symbol
            .insert("ETH-USDT".into(), vec!["T2".into()]);
        parent.trigger_orders_by_cloid.insert(
            ("alice".into(), "BTC-USDT".into(), "cloid-1".into()),
            "T1".into(),
        );

        let parent_root = compute(&parent);
        let mut child = parent.clone();
        let sibling = parent.clone();

        assert!(parent.trade_history.ptr_eq(&child.trade_history));
        assert!(parent
            .trade_history
            .value_ptr_eq("BTC-USDT", &child.trade_history));
        assert!(parent
            .trigger_orders
            .value_ptr_eq("T1", &child.trigger_orders));
        assert!(parent
            .trigger_orders_by_trader
            .value_ptr_eq("alice", &child.trigger_orders_by_trader));

        child
            .trade_history
            .entry("BTC-USDT".into())
            .or_default()
            .push_back(crate::app::Fill {
                taker_order_id: "child".into(),
                maker_order_id: "maker".into(),
                taker: "alice".into(),
                maker: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 4_999_000,
                size: 1,
                timestamp: 2,
                maker_locked_margin: 0,
                maker_original_size: 1,
            });
        child.trigger_orders.get_mut("T1").unwrap().trigger_price += 1;
        child
            .trigger_orders_by_trader
            .get_mut("alice")
            .unwrap()
            .push("child".into());
        child
            .trigger_orders_by_symbol
            .get_mut("BTC-USDT")
            .unwrap()
            .push("child".into());

        assert_eq!(parent.trade_history.get("BTC-USDT").unwrap().len(), 1);
        assert_eq!(child.trade_history.get("BTC-USDT").unwrap().len(), 2);
        assert!(!parent
            .trade_history
            .value_ptr_eq("BTC-USDT", &child.trade_history));
        assert!(parent
            .trade_history
            .value_ptr_eq("ETH-USDT", &child.trade_history));
        assert!(!parent
            .trigger_orders
            .value_ptr_eq("T1", &child.trigger_orders));
        assert!(parent
            .trigger_orders
            .value_ptr_eq("T2", &child.trigger_orders));
        assert!(!parent
            .trigger_orders_by_trader
            .value_ptr_eq("alice", &child.trigger_orders_by_trader));
        assert!(parent
            .trigger_orders_by_trader
            .value_ptr_eq("bob", &child.trigger_orders_by_trader));
        assert!(!parent
            .trigger_orders_by_symbol
            .value_ptr_eq("BTC-USDT", &child.trigger_orders_by_symbol));
        assert!(parent
            .trigger_orders_by_symbol
            .value_ptr_eq("ETH-USDT", &child.trigger_orders_by_symbol));
        assert_eq!(parent.trigger_order("T1").unwrap().trigger_price, 4_000_000);
        assert_eq!(
            sibling.trigger_order("T1").unwrap().trigger_price,
            4_000_000
        );
        assert_eq!(parent_root, compute(&parent));
        assert_ne!(parent_root, compute(&child));

        let parent_tree = {
            let mut sealed = parent.clone();
            sealed.derive_full_state_tree(None)
        };
        let mut derived_child = parent.clone_for_verified_component_child();
        derived_child
            .trigger_orders
            .get_mut("T1")
            .unwrap()
            .trigger_price += 1;
        derived_child.mark_full_state_dirty(COMPONENT_DIRTY_TRIGGERS);
        let derived = derived_child.derive_full_state_tree(Some(&parent_tree));
        assert_eq!(derived, compute_tree(&derived_child));
    }

    #[test]
    fn full_state_root_is_unchanged_after_failed_transaction() {
        let mut state = deterministic_state([0x34; 32]);
        state
            .execute_tx(crate::app::Transaction::Deposit {
                trader: "alice".into(),
                amount: 10,
            })
            .expect("initial deposit should succeed");
        let before_root = compute(&state);
        let before_balance = state.accounts.get("alice").unwrap().balance;

        assert!(state
            .execute_tx(crate::app::Transaction::Withdraw {
                trader: "alice".into(),
                amount: 11,
            })
            .is_err());

        assert_eq!(before_balance, state.accounts.get("alice").unwrap().balance);
        assert_eq!(before_root, compute(&state));
    }

    #[test]
    fn full_state_root_survives_snapshot_round_trip() {
        // Snapshot format intentionally carries the application fields that
        // make up the schema-v5 component root. Keep progress at its snapshot default;
        // committed height/view are replay metadata and are restored by the
        // canonical recovery path rather than by AppSnapshot itself.
        let mut state = super::super::AppState::new_with_chain_domain([0x44; 32]);
        state.staking.reward_clock_initialized = true;
        state.staking.last_reward_accrual_timestamp = 100;
        state.staking.last_reward_compound_timestamp = 100;
        state.staking.reward_accrual_remainder = 7;
        let root = compute(&state);
        let restored = super::super::AppState::try_from_snapshot_with_chain_domain(
            state.create_snapshot(0),
            [0x44; 32],
            true,
        )
        .expect("valid snapshot should restore");

        assert_eq!(root, compute(&restored));
    }
}
