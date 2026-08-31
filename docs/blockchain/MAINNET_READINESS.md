# Mainnet Readiness

> **NOT MAINNET READY.** Do not custody real funds, bridge real assets, or operate public
> validators from this repository. This is an engineering audit of the current development
> tree, not an independent security audit or a launch approval.

로컬 실행 명령과 host/Docker fixture는 [config/local README](../../config/local/README.md)에
고정되어 있다. 통합 checkpoint는 [hl-node 통합 작업 로그](WORKLOG_2026-08-11_HL_NODE_RUNTIME_INTEGRATION.md)에
기록하고, P0 변경의 상세 내역은 [P0 메인넷 하드닝 작업 로그](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md)에
기록한다.

Commitment v2 artifact와 consensus activation은
[Commitment v2 activation 작업 로그](WORKLOG_2026-08-21_COMMITMENT_V2_CONSENSUS_ACTIVATION.md)에 기록한다.
Primary state semantic validation과 snapshot/import fail-closed 경계는
[Primary State 불변식 작업 로그](WORKLOG_2026-08-13_PRIMARY_STATE_INVARIANTS.md)에 기록한다.
Snapshot/Sync byte budget과 현재 단일-object snapshot의 한계는
[Snapshot/Sync 자원 한도 작업 로그](WORKLOG_2026-08-13_SNAPSHOT_SYNC_RESOURCE_LIMITS.md)에 기록한다.
Schema-v3 state-root consensus activation은
[State-root activation 작업 로그](WORKLOG_2026-08-14_CONSENSUS_STATE_ROOT_ACTIVATION.md)에 기록한다.

## Executive assessment

The repository now has a canonical **local** `hl-node` path for the current tranche, but it is
not a production runtime. The standalone `hl-server` binary/path has been removed; API/WS now
live beside the consensus runner in `hl-node`. The P0 tranche added chain/genesis binding,
authenticated EIP-712 envelopes, BLS PoP, durable RocksDB recovery, gossip admission, and
trusted-download-only ActiveSync. Schema-v3 full state is authenticated as `Block.app_hash`, and
Commitment v2 receipts/events are authenticated as `Block.commitment_root`. Verified import,
epoch/committee transition, proof-serving and operational gates are still required for a launch.

## Current architecture audit

The standalone server path is removed. The current tree has one canonical local runtime and
one separate development harness:

| Binary | Current path | Readiness consequence |
| --- | --- | --- |
| `hl-node` | Shared genesis/node loader, env-provided BLS seed, authenticated TCP runner, canonical API/WS, RocksDB commit/recovery, and the same binary for N=1 or N=4 with finite `--blocks`. It requires `MODE=dev`. | Canonical local runtime tranche only. State and execution-artifact roots are consensus-authenticated, but production startup and verified import/proof serving remain disabled. |
| `multinode` | TCP `ConsensusRunner` with BLS-authenticated peers and a per-node RocksDB store. | A development demo for multi-validator consensus and restart recovery; it still uses loopback addresses and deterministic development keys. It is not a deployment profile. |

The application layer still has boundaries that must remain explicit:

- Oracle updates must not call `process_update` directly from an external task. A production
  oracle update must be a signed, ordered, deterministic consensus transaction.
- `AppState::execute` and the canonical app hook validate the exact signed envelope payload;
  malformed payloads are rejected before voting. Protocol-owned `System(Transaction)` values
  are explicit and are not a general user bypass.
- The EIP-712 v1 envelope is now the canonical user object and is re-verified at ingress,
  mempool, block execution, and validator replay. Browser signing still needs a canonical
  bincode/signing-data integration before production wallet support.
- Development `Deposit` and `Withdraw` transactions simulate balance creation and burns.
  They are not evidence of an Ethereum deposit, and environment flags must never authorize
  an equivalent mainnet path.

Recent route guards reduce some of these surfaces outside development mode, but they do not
make the bridge, custody, or alternate operational path production-safe. API financial
intermediate-overflow handling was hardened; it is not a substitute for an accounting audit.

### Local canonical runtime checkpoint

The shared loader validates one genesis committee and each node file, including identity,
listen/peer addresses, duplicate peer addresses, schema-v2 BLS PoP, and the environment variable
containing the original BLS seed. Genesis domain V3 binds chain/committee parameters plus block
protocol V5, state-root schema v3, and Commitment v2 schema/version, so mixed protocol binaries
fail at the authenticated context.
`view_timeout` and peer readiness are explicit; TCP reconnect cleanup is generation-safe. Host
fixtures use unique loopback ports and Docker fixtures use service DNS on container port 9000.
`ConsensusConfig` debug output redacts secret bytes and full public-key bytes. When trusted
context/committee material is present, gossip admission runs before seen-cache/deliver/relay;
the Docker fixture enables it with `GOSSIP_ENABLED=true`.

This is the same-binary N=1/N=4 local model, not a mainnet claim. It follows the curated-genesis
stage of the Cosmos-like progression; permissionless validator selection remains a later,
audited transition.

Checkpoint evidence is intentionally kept qualitative until the P0 agents' final regression
transition is complete: single-node, host 4-validator, and Docker 4-validator paths use the
same `hl-node` loader/runtime; Docker enables gossip and mounts validator-specific named RocksDB
volumes; malformed context/signature/QC/PoP/envelope and untrusted ActiveSync responses have
negative-test coverage. Exact test totals and restart/Docker final measurements are reserved for
the final verification section of [the P0 work log](WORKLOG_2026-08-11_P0_MAINNET_HARDENING.md).
These checkpoints are not a mainnet-readiness claim.

## Target: one `hl-node` runtime

Make `hl-node` the only validator/RPC process that owns networking, mempool admission,
consensus, deterministic execution, persistence, recovery, and finalized-state queries.
The standalone `hl-server` path has been removed and its API/WS surfaces are integrated into
`hl-node`; `multinode` remains a test harness that uses the same runtime. No background task,
relayer, oracle fetcher, or API handler may mutate consensus state directly.

The target transaction and finality flow is:

1. A client submits a versioned, domain-separated signed transaction. The node verifies the
   signature, chain/genesis identifier, nonce, bounds, and admission policy before placing
   the exact envelope in the mempool.
2. The leader deterministically orders transactions and proposes a block containing its
   parent, payload hash, execution-domain/version, epoch, and active committee identifier.
3. Every validator verifies the proposal and its QC, executes the exact payload against the
   parent state, and derives the same state root and receipt/events root. Decode errors,
   missing payloads, and state-root mismatches reject the proposal.
4. Validators sign only the verified result. A stake-weighted QC requires a strict
   Byzantine quorum from the active committee; the HotStuff-2 two-chain rule then marks
   the appropriate ancestor finalized.
5. The node durably writes the finalized block, state roots, consensus safety state, and
   commit marker before serving finalized results. Indexers and WebSocket notifications
   consume committed events after that point.

## Validator rollout

### Phase 1: curated, stake-weighted PoS

Start with a small, explicitly curated committee. Put validator registration, operator key
identity, voting power, BLS keys, genesis allocations, slashing parameters, and committee
policy in signed genesis/on-chain state. Use stake weights for quorum and leader selection,
with a documented operator admission and key-rotation process. This phase is a controlled
testnet/mainnet candidate, not permissionless staking.

### Phase 2: permissionless delegation and validator selection

Add on-chain validator registration, self-bond, delegation, unbonding delay, rewards,
commission, jailing/slashing, minimums, and deterministic tie-breaking. At epoch boundaries,
derive the next committee from finalized state and activate it only through a validated
transition. Delegators must be able to verify the same active set and voting powers from
chain data; an operator-controlled environment list is not permissionless PoS.

### Static epoch-0 committee binding

The current committee remains curated and configuration/genesis-backed, but the static epoch
0 authentication context is now bound to the canonical `committee_hash` and `genesis_hash`.
Block, proposal, vote, QC, timeout/timeout certificate, ViewChange/ViewChangeCertificate/
NewView, safety and equivocation detector/proof, persisted consensus state, and sync object
context carry or validate this binding. Equivocation evidence preserves both conflicting
`app_hash` values and signatures. BLS PoP is required for the genesis validator keys. This is
a protocol-hardening boundary, not permissionless staking or launch approval.

### Dynamic committee limitation

Staking can produce a validator-set update, but dynamic transition is disabled. A transition
certificate, historical committee registry, and verified key/entry/exit rules are still
required before any next epoch can activate. Until that path exists, the network must treat
the committee as static and curated; do not advertise the current staking code as
permissionless PoS.

## Assets and an Ethereum bridge

Local development may use simulated balances and the faucet. Production accounting must
separate the execution state from the Ethereum bridge:

- A relayer watches Ethereum, obtains the required proof/finality evidence, and submits a
  bridge transaction to the node. It never writes balances directly.
- Validators independently verify the proof (or verify a consensus-defined attestation)
  and execute mint/credit only through the canonical state transition. Withdrawals create
  a finalized outbound intent; a relayer observes it and submits the Ethereum transaction,
  with completion recorded by a verified inbound proof/attestation.
- Only operational inputs—RPC URLs and secrets such as API credentials, signing keys, and
  relayer keys—may come from the environment or a secrets manager. Consensus rules, chain
  IDs, contract/token addresses, confirmation and finality thresholds, decimals, limits,
  pause policy, and genesis allocations belong in signed genesis/on-chain state and must be
  replayable.
- Bridge replay protection, message IDs, reorg handling, rate limits, accounting
  conservation, pause/recovery procedures, and independent proof verification are launch
  gates. A multisig alone is not a deposit proof.

## Future EVM boundary

EVM support can be added later without making it the current execution model. Treat it as a
separate application/execution interface and versioned domain under the same consensus:
explicit domain ID and VM version, transaction/mempool rules, gas schedule, state root,
receipts, and deterministic runtime limits. The perp/orderbook state and EVM state must not be
implicitly shared. A block must commit each domain's versioned result, and validators must run
the same implementation before voting. Adding JSON-RPC compatibility alone is not an EVM
integration or a safety boundary.

## Initial performance targets

These are acceptance targets, not measurements or claims about the current code. Publish
the workload, hardware, topology, and p50/p95/p99 results with every test. The smaller
correctness gate comes first; the intended curated network target follows only after it is
stable:

| Gate | Initial target |
| --- | --- |
| Correctness gate | 4 curated validators across at least two failure domains; ≥100 signed actions/second for 10 minutes; tolerate one Byzantine validator with no safety violation. |
| Curated network | 7–15 validators across at least three regions/failure domains. |
| Finality | Target ~250 ms blocks, normal finality under 1 second, and p99 under 2 seconds, including execution and durable commit. Validate feasibility under WAN faults before making a launch claim. |
| Sustained load | Target 5,000 signed actions/second across 50–100 markets, with no state divergence, dropped committed transactions, or safety violation. Treat order matching and consensus throughput as separate measurements. |
| Recovery | Restart and replay from durable state with a documented bound, while preserving the last finalized block and safety state. |

Order matching latency and orders/second must be measured separately from consensus throughput;
the old roadmap numbers are not benchmark evidence.

## Readiness gates

### P0 — block mainnet work

- **Completed in the current local P0 tranche:** use durable RocksDB atomic commit/recovery,
  persist follower and leader-self vote intent before send/aggregation,
  publish canonical application/API/WS state only after the synced finalized write,
  require exact committed-head parent ancestry at QC, application, and storage boundaries,
  retire the standalone `hl-server` process path, bind chain/genesis context, require BLS PoP
  in schema v2, use the canonical EIP-712 envelope, validate gossip before seen-cache/deliver/
  relay, and make ActiveSync verified-download-only with no state mutation.
- **Completed as a genesis-wide local consensus activation:** ordered transaction receipts,
  stable numeric statuses/error codes, transaction/system event separation, deterministic
  receipt/event roots, V5 block/QC authentication, and atomic finalized RocksDB persistence with
  restart comparison.
- **Completed as a local execution/import guard:** primary market/order/account/staking/trigger/
  oracle/funding invariants, trusted-domain validator PoP verification on snapshot import, and
  primary+derived fail-closed candidate/private-replay validation.
- **Completed as a transport/storage resource guard:** bounded 64 MiB snapshot storage/decode,
  32 MiB HTTP/P2P sync responses and TCP frames, bounded height-window reads, and pre-allocation
  HTTP ActiveSync/TCP ingress checks. These are operational guards, not verified snapshot import.
- **Completed as a genesis-wide local consensus activation:** schema-v5 full state is
  `Block.app_hash` and Commitment v2 is `Block.commitment_root`; V5 block hashes, proposer
  signatures, votes and QCs authenticate both, while leader/follower/observer, storage and
  restart fail closed on mismatches.
- **Completed for static epoch-0 double-vote evidence:** canonical proof serialization,
  trusted committee/BLS verification at network and application boundaries, a bounded durable
  RocksDB journal, a dedicated proposal reserve, deterministic system-transaction execution,
  and journal cleanup only after durable block plus application commit. Genesis committee
  members are bootstrapped as slashable application records; historical/dynamic committees are
  deliberately still disabled.
- **Completed for the local static curated staking boundary:** one HYCK is 1,000,000 staking
  base units, genesis voting power is whole bonded HYCK, the active-set ceiling is 21 across
  genesis/consensus/application validation, and the canonical static runtime does not emit an
  unsupported automatic epoch update at view 54,000. Static BLS key rotation is rejected and
  recovery rejects application/consensus epoch disagreement.
- **Completed for the local native-asset accounting boundary:** genesis issues exactly
  1,000,000,000 HYCK with six decimals, collateral and HYCK use separate account fields, validator
  bonds and explicit allocations are treasury-funded, and canonical preflight checks conservation
  across liquid/bonded/unbonding/reward buckets. A genesis-funded 388,880,000 HYCK emissions
  reserve pays the deterministic inverse-square-root staking curve; it is a transfer from fixed
  issuance, not an inflationary mint. Reward clocks, eligibility, remainder and reserve are covered
  by schema-v5 state roots/snapshots, and settlements emit authenticated Commitment v2 epoch events
  with canonical recipient credits and compounding records. Slashing returns value to treasury.
  In the current static curated phase it does not yet remove voting power from the epoch-0 consensus
  committee; that requires the authenticated committee-transition activation described above. This
  is not a bridge or permissionless staking launch claim.
- **Still required for a launch:** verified block import, snapshot manifest/proof, external
  indexer artifact/range/inclusion-proof boundaries, and deterministic replay across all
  application domains.
- Continue adversarial and long-run testing of the existing epoch/committee/genesis bindings
  for votes, QCs, timeouts, view changes, equivocation, replay, malformed payloads, and
  conflicting state roots.
- Keep epoch transitions disabled until transition certificates, historical committee
  validation, PoP-backed key registration, production genesis asset/operator allocations, and
  entry/exit rules are verified. Static-mode key rotation is currently rejected; it must not
  mutate the epoch-0 application key before an authenticated transition exists.
- Specify key custody/rotation, genesis ceremony, network authentication, rate limits,
  resource bounds, telemetry, incident response, and a threat model. No real bridge yet.

### Remaining launch blockers after local P0

- epoch transition activation handshake, historical committee registry, evidence/unbonding hold,
  and production HYCK allocation ceremony before permissionless PoS
- verified block import, snapshot proof/manifest, and indexer artifact/range/inclusion-proof API
- keep the legacy in-memory `Engine` outside default production features and preserve the
  existing count/per-response/cumulative sync byte budgets as protocols evolve
- Ethereum bridge proof/finality/reorg/replay/accounting and pause/recovery procedures
- validator key custody, upgrades/reproducible builds, telemetry/alerting, incident response,
  chaos/long-run testing, and independent consensus/cryptography/accounting/security audits

### P1 — curated testnet gate

- Run a long-lived multi-host testnet with curated stake weights, signed genesis, durable
  recovery, rolling restarts, byzantine/fault injection, deterministic replay, and public
  load results.
- Add external review of consensus, cryptography, matching/liquidation/accounting, API
  admission, and state storage. Test bridge proofs on a separate Ethereum testnet with
  pause and recovery drills.
- Demonstrate operational key management, upgrades, monitoring/alerting, backups, and a
  documented incident/runbook process.

### P2 — curated mainnet candidate

- Complete independent bridge/proof and economic/security reviews, reproducible builds,
  chaos/long-run testing, public bug bounty, genesis ceremony, staged rollout, and explicit
  halt/pause authority.
- Re-run the published performance and recovery gates at the intended validator count and
  topology. A green local demo is not sufficient evidence.

### P3 — permissionless PoS transition

- Activate audited on-chain validator registration and selection, delegation economics,
  epoch-bound committee transition certificates, slashing/jailing, governance, and
  deterministic upgrades.
- Exercise validator entry/exit, key rotation, unbonding, evidence retention, and economic
  attacks on a public testnet before changing the curated mainnet committee policy.

Until at least P0–P2 are accepted by the project’s operators and independent reviewers, the
correct status remains **not mainnet ready**. P3 is the separate permissionless end state.
