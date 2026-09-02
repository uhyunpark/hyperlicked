# HyperLicked Frontend

Next.js 15 trading UI for the local HyperLicked node. The page currently
connects to the `hl-node` REST API and WebSocket, supports MetaMask/Rabby
wallet signing, and is intended for local development only.

> **Local prototype only.** `./scripts/local-node` runs a development
> validator with a public fixture key and simulated balances. Do not use this
> setup to custody real funds, bridge assets, or sign production transactions.

## Quick start

No environment file is required for the default local setup. Use two
terminals from the repository root:

```bash
# Terminal 1 — start the canonical local validator and API
./scripts/local-node

# Terminal 2 — start the frontend
cd web
bun install
bun run dev
```

Open [http://localhost:3000](http://localhost:3000). If that port is already
in use, Next.js prints the actual port it selected.

`./scripts/local-node` starts one `hl-node` process with:

- `MODE=dev`
- `config/local/single-genesis.json`
- `config/local/host-single/node.json`
- REST API and WebSocket on `127.0.0.1:8080`
- consensus transport on `127.0.0.1:9100`
- the development-only `HL_LOCAL_BLS_SEED_1` fixture

There is no separate Go server or `hl-server` process. The API and WebSocket
belong to `hl-node`.

To start a fresh chain without reusing the default RocksDB directory:

```bash
./scripts/local-node --data-dir "$(mktemp -d)"
```

For a finite consensus smoke test, use a fresh data directory and a target
height. The process exits after that committed height and shuts down its API:

```bash
./scripts/local-node --blocks 3 --data-dir "$(mktemp -d)"
```

## Checking the node

The first line printed by the node looks like this:

```text
ready node=01010101 epoch=0 committee=... committed_height=0 api_addr=127.0.0.1:8080
```

This means the API listener is ready. Height `0` is the committed genesis
block, so seeing `committed_height=0` on that startup line is expected; it is
not a frontend error. With no `--blocks` argument, the single-node validator
continues running and proposes/commits blocks, including empty blocks. Check
the live height instead of relying on the startup line:

```bash
curl http://127.0.0.1:8080/health
curl http://127.0.0.1:8080/api/v1/chain/health
curl http://127.0.0.1:8080/api/v1/chain/status
curl http://127.0.0.1:8080/api/v1/sync/block/0
```

The default launcher uses `RUST_LOG=warn`, so normal proposal/commit messages
are hidden. To watch consensus progress:

```bash
RUST_LOG=info ./scripts/local-node
```

The frontend uses these local endpoints by default:

```text
REST       http://localhost:8080/api/v1
WebSocket  ws://localhost:8080/ws
```

## Wallet network and signing

Install [Rabby](https://rabby.io/) or MetaMask, then click **Connect Wallet**.
The wallet is used to sign messages; the local node is not an EVM JSON-RPC
node and does not send Ethereum transactions. Consequently, there are no gas
fees, ETH transfers, or bridge deposits in this local flow.

The frontend has a wallet chain-ID guard for network UX. Its default values
are `1337` / `HyperLicked Devnet`, but `hl-node` does **not** provide the
`http://localhost:8545` EVM RPC advertised by the default metadata. Do not add
that network to a wallet unless a separate EVM RPC is actually running. If the
wallet is already on a network you want to use for signing, set the expected
metadata before starting the frontend; Ethereum Mainnet is one example:

```dotenv
# web/.env.local — optional; choose a network already available in the wallet
NEXT_PUBLIC_CHAIN_ID=1
NEXT_PUBLIC_CHAIN_NAME=Ethereum Mainnet
NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1
NEXT_PUBLIC_WS_URL=ws://localhost:8080/ws
```

The chain ID here only controls the warning/switch-network UI and custom
network metadata. It is not the HyperLicked application chain domain and does
not cause an EVM transaction. Restart `bun run dev` after changing env values.

### Canonical EIP-712 envelope

Orders, cancels, and TP/SL trigger actions use the canonical `eip712-v1`
envelope. The frontend obtains the live chain domain automatically from
`GET /api/v1/sync/block/0`; do not hard-code a genesis hash or replace it with
the wallet chain ID.

The exact typed data is:

```text
domain:
  name:    "HyperLicked"
  version: "1"
  salt:    0x + the node's 64-hex-character genesisHash

type HyperLickedTransaction:
  bytes32 chainDomain
  address signer
  uint64  nonce
  uint64  validAfter
  uint64  validUntil
  bytes32 actionHash
```

`chainDomain` is the same 32-byte genesis hash. `actionHash` is the Keccak
hash of `HYPERLICKED-ACTION-V1\0` followed by the canonical bincode action
bytes. The UI fetches the account nonce, sets `validAfter` to `0`, and gives a
new action a one-hour `validUntil` deadline. The submitted JSON includes
`signatureScheme: "eip712-v1"`.

The request lifecycle is:

```text
GET nonce + genesis domain
        ↓
wallet signs HyperLickedTransaction
        ↓
POST /orders (or trigger/cancel endpoint)
        ↓
{ status: "pending", tx_hash: ... }
        ↓
mempool → proposal/QC → committed block
        ↓
WebSocket transactionFinalized + REST refresh
```

The `pending` response means the node admitted the envelope; it is not yet a
finalized trade. A receipt can be queried with:

```text
GET /api/v1/transactions/:tx_hash
```

The agent-delegation UI is experimental. It can register a delegated key and
stores that key only in the browser, but canonical order submission currently
uses the connected wallet signer. Enabling delegation does not remove the
wallet signature prompt for canonical orders.

## Using the local trading page

1. Start `hl-node` and the frontend.
2. Connect a wallet.
3. Click **Get Test USDC ($100k)** in the trade panel. This calls the dev-only
   `/api/v1/deposit` simulation and credits test USDC; it is not an Ethereum
   deposit.
4. Enter a limit order and approve the EIP-712 signature in the wallet.
5. Wait for the `pending` transaction to commit. Open orders, balances,
   positions, fills, and order history refresh from the node.

The default page and public WebSocket subscription are currently fixed to
`BTC-USDT`. A fresh node has no orderbook liquidity or trade history, and the
artificial market-maker/oracle loops are not started by the canonical launcher.
An empty orderbook, zero current price, and an empty chart are therefore
normal until trades exist. To see a fill, fund two different wallet addresses
and submit crossing orders; one address cannot trade against itself.

Limit orders support GTC, IOC, and Post Only (ALO). Market orders are sent as
IOC. Reduce-only orders and optional TP/SL trigger orders are available. The
chart loads historical candles from the candles REST endpoint and aggregates
new WebSocket trades; the chart footer still contains placeholder display
values.

## Current feature status

Implemented in the current page:

- REST market, orderbook, trade, candle, funding, account, position, order,
  fill, and chain-status reads
- Public WebSocket orderbook/trade/market events with reconnect handling
- Wallet connect/disconnect and account/network change handling
- Canonical EIP-712 order, cancel, and TP/SL signing
- Dev faucet for test USDC
- Open orders, positions, balances, fills, funding history, and order history

Known limitations:

- The displayed market is `BTC-USDT`; a market selector is not implemented.
- Portfolio, Vaults, Staking, Leaderboard, and Referrals navigation tabs are
  disabled. TWAP is also disabled.
- Agent delegation remains experimental and is not the canonical order signer.
- The local runtime is `MODE=dev` only and is not mainnet-ready.
- No EVM JSON-RPC, real deposit/withdrawal, bridge, or real asset settlement is
  provided by this setup.

## Environment variables

All values are optional for local development. Next.js reads public variables
when the dev server/build starts.

| Variable | Default | Purpose |
| --- | --- | --- |
| `NEXT_PUBLIC_API_URL` | `http://localhost:8080/api/v1` | REST base URL |
| `NEXT_PUBLIC_WS_URL` | `ws://localhost:8080/ws` | WebSocket URL |
| `NEXT_PUBLIC_CHAIN_ID` | `1337` | Wallet warning/switch target only |
| `NEXT_PUBLIC_CHAIN_NAME` | `HyperLicked Devnet` | Wallet UI/network metadata |
| `NEXT_PUBLIC_RPC_URL` | `http://localhost:8545` | RPC metadata used only when adding a wallet network; no RPC is supplied by `hl-node` |
| `NEXT_PUBLIC_BLOCK_EXPLORER_URL` | empty | Optional wallet network metadata |
| `NEXT_PUBLIC_DEV_MODE` | unset | Set to `true` only to force dev UI behavior in a non-development build |

Do not configure `NEXT_PUBLIC_EIP712_NAME`, `NEXT_PUBLIC_EIP712_VERSION`, or
`NEXT_PUBLIC_VERIFYING_CONTRACT` as a substitute for the canonical domain. The
active canonical signer uses the node genesis hash as the EIP-712 `salt` and
the fixed protocol name/version described above.

## REST and WebSocket endpoints used by the UI

The complete API is mounted below `/api/v1`; the health endpoint is `/health`
and the WebSocket is `/ws`.

| Method | Path | Use |
| --- | --- | --- |
| GET | `/health` | Basic liveness check |
| GET | `/api/v1/chain/health` | Detailed health and committed height |
| GET | `/api/v1/chain/status` | Height, view, validator count, mempool size |
| GET | `/api/v1/markets` | Market list |
| GET | `/api/v1/markets/BTC-USDT/orderbook` | Orderbook snapshot |
| GET | `/api/v1/markets/BTC-USDT/trades` | Recent public trades |
| GET | `/api/v1/markets/BTC-USDT/candles` | OHLCV candles |
| GET | `/api/v1/accounts/:address` | Balance/equity |
| GET | `/api/v1/accounts/:address/nonce` | Next account nonce |
| GET | `/api/v1/accounts/:address/orders` | Open/historical orders |
| GET | `/api/v1/accounts/:address/positions` | Positions |
| GET | `/api/v1/accounts/:address/fills` | User fills |
| POST | `/api/v1/orders` | Canonical order transaction |
| POST | `/api/v1/orders/cancel` | Canonical cancel transaction |
| POST | `/api/v1/trigger-orders` | Canonical TP/SL transaction |
| POST | `/api/v1/trigger-orders/cancel` | Canonical TP/SL cancel |
| POST | `/api/v1/deposit` | Dev-only test balance faucet |
| GET | `/api/v1/sync/block/0` | Genesis hash used for canonical signing |
| GET | `/api/v1/transactions/:tx_hash` | Finalized transaction receipt |

The public WebSocket stream includes orderbook, trade, block, mark-price, and
asset-context events. After a user subscription, it also delivers fills,
order/position/balance updates, funding, liquidation, and trigger-order events.
In local dev mode, the frontend subscribes with the wallet address without an
additional WebSocket authentication signature. Non-dev deployments require the
protocol's authenticated EIP-191 subscription message.

API numeric units are intentionally integer-based:

- prices: cents (`5000000` = `$50,000.00`)
- sizes: satoshis (`100000000` = `1 BTC`)
- timestamps and validity deadlines: Unix milliseconds
- nonces: decimal `u64` strings where precision must be preserved

## Development commands

```bash
bun run dev        # Next.js development server with hot reload
bun run build      # Production build
bun run start      # Serve the production build
bun run lint       # Biome checks
bun test           # Frontend tests
```

For backend runtime details, local validator fixtures, and the public
development BLS seed warning, see [`config/local/README.md`](../config/local/README.md)
and the repository [README](../README.md).

## Project layout

```text
web/
├── app/
│   ├── page.tsx                         # Trading page
│   ├── layout.tsx                       # Root layout
│   └── globals.css                      # Theme and layout styles
├── components/trading/                  # Trading UI panels and tabs
├── components/ui/                       # Toasts, modal, error boundaries
├── lib/api.ts                           # REST client and unit conversion
├── lib/config.ts                        # Public runtime configuration
├── lib/useOrderSubmit.ts                # Canonical order submission flow
├── lib/useAccountData.ts                # Account reads and dev faucet
├── lib/wallet/canonicalAction.ts        # Rust-compatible action/EIP-712 encoding
├── lib/wallet/useWalletConnection.ts    # MetaMask/Rabby connection
├── lib/wallet/useWalletSigning.ts       # Wallet and canonical signing
├── lib/websocket/                       # WS lifecycle, auth, and event handlers
└── package.json
```
