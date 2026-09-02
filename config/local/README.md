# Local validator fixtures

이 디렉터리는 **dev-only local harness** 전용이다. mainnet/testnet secret로 재사용하지
않는다. 일반 사용자는 genesis/config 경로와 Cargo 옵션을 직접 조합할 필요 없이 저장소
루트에서 `./scripts/local-node`를 실행하면 된다. launcher가 아래 파일을 선택하고
`MODE=dev`와 `--locked`를 내부에서 설정한다.

```text
./scripts/local-node
```

직접 `hl-node`를 실행하거나 Docker command를 재현할 때만 `--genesis`, `--config`,
`--blocks`, `--peer-wait-ms`, `--data-dir`를 사용한다. `--bin hl-node`와 `--locked`는
launcher가 처리하므로 일반적인 local 실행 명령에 직접 넣지 않는다.

## Single-node 빠른 확인

저장소 루트에서 `./scripts/local-node`만 실행하면 된다. 출력되는
`ready ... committed_height=0`은 정상이다. `ready`는 API/network listener가 열리고
consensus runner를 시작할 준비가 됐다는 뜻이며, height 0은 canonical genesis의 높이다.
그 뒤 single validator가 자동으로 proposal을 만들고 2-chain 규칙에 따라 블록을 확정한다.
`ready`에서 멈춘 것이 아니다.

launcher 기본 로그 레벨은 `RUST_LOG=warn`이므로 정상적인 proposal/commit 로그가 보이지
않는다. 진행을 눈으로 확인하려면 `RUST_LOG=info`로 실행하고, 다른 터미널에서 status를
조회한다.

```bash
RUST_LOG=info ./scripts/local-node

curl -s http://127.0.0.1:8080/api/v1/chain/status
watch -n 1 'curl -s http://127.0.0.1:8080/api/v1/chain/status'
```

`watch`가 설치되지 않은 macOS에서는 `curl`을 반복 실행하거나 간단한 shell loop로 같은
URL을 조회하면 된다. `--blocks N`은 목표 committed height이며, 해당 높이에 도달하면
프로세스가 종료된다. 옵션을 생략하면 Ctrl-C까지 계속 실행한다.

기본 data directory는 chain domain과 node ID에 묶인
`.hyperlicked/data/<genesis-domain>/<node-id>`이고 RocksDB를 사용한다. 같은
`--data-dir`를 다시 지정하면 기존 chain을 복원해 이어서 실행한다. 깨끗한 fixture가
필요하면 임시 경로를 사용한다.

```bash
hl_fresh_dir="$(mktemp -d)"
RUST_LOG=info ./scripts/local-node --blocks 3 --data-dir "$hl_fresh_dir"
```

`--blocks 3`처럼 실행할 때 기존 data directory의 높이가 이미 3 이상이면 즉시 종료할 수
있으므로, 재현 가능한 smoke test에는 매번 새 `mktemp -d` 경로를 사용한다.

## 파일 구성

- `genesis.json`: 4-validator, equal-power, `f=1` 하네스의 공통 genesis다.
- `genesis.json`과 `single-genesis.json`은 PoP가 포함된 `schema_version=2` 형식이다.
  각 validator의 `bls_proof_of_possession`은 canonical application genesis domain, node ID,
  BLS public key에 결합된 96-byte 서명이며, 노드가 listener를 열기 전에 검증한다. PoP
  바이트 자체는 domain preimage에 들어가지 않아 circularity가 없다.
- `host-single/node.json`: consensus는 `127.0.0.1:9100`, API는 `127.0.0.1:8080`을
  listen하는 single-validator 설정이다.
- `host-4/node{0..3}.json`: 호스트 consensus는 `127.0.0.1:9101..9104`, API는
  `127.0.0.1:8180..8183`을 listen하는 설정이다.
- `docker-4/node{0..3}.json`: Compose용 설정이다. 모든 컨테이너는 consensus `9000`과
  API `8080`을 내부에서 listen하고 `validator0`~`validator3` 서비스 DNS로 연결한다.
  API는 호스트 `18080..18083`으로 매핑된다.
- `single-genesis.json` + `host-single/node.json`: 독립 single-validator smoke다.
  현재 스키마는 genesis validator 수와 peer 수를 일치시켜야 하므로, 4-validator
  genesis를 그대로 single-validator로 재사용할 수 없다.

## Deterministic dev-only BLS fixtures

현재 multinode fixture와 동일하게 seed는 32바이트이며 첫 바이트만 validator index,
마지막 바이트는 `0xbe`, 나머지는 0이다. 각 public key는 정확히
`BlsSecretKey::from_seed(seed).public_key()`로 유도했다. 아래 값은 공개 테스트 fixture이며
보안 키가 아니다. JSON에는 seed를 넣지 않는다.

| validator | seed environment variable | seed hex | BLS public key hex |
| --- | --- | --- | --- |
| 0 | `HL_LOCAL_BLS_SEED_1` | `01000000000000000000000000000000000000000000000000000000000000be` | `83e8d85cad9f339a8797d495327a6aeb163263b2f0a289cc45c443e4e0b14a141c4a0077e4ca090dbf1d714a86a3bf8b` |
| 1 | `HL_LOCAL_BLS_SEED_2` | `02000000000000000000000000000000000000000000000000000000000000be` | `97caf53f20123edb3b3414ecd867863e561fe7a74eb7ef1ced9790053b90faf3564d73b95d08cdab33b315ba638a06c7` |
| 2 | `HL_LOCAL_BLS_SEED_3` | `03000000000000000000000000000000000000000000000000000000000000be` | `b0bb7edd539b7e074039e3dc6158a773d28b5d3e74c047c7dfdfdc6016517f4c8c86632e99d44ac0e140149b35f8d889` |
| 3 | `HL_LOCAL_BLS_SEED_4` | `04000000000000000000000000000000000000000000000000000000000000be` | `b41dd293a0dc48b17465af5c0371e21a1ad985209739154165d932f633b2ff962bd6b0a751306903248f44fa3a3322f4` |

`./scripts/local-node`는 위 표의 validator0 seed를 환경변수로만 export한 뒤
`single-genesis.json`과 `host-single/node.json`으로 `hl-node`를 시작한다. 이 seed는
공개된 development-only fixture이며 production secret이 아니다. 호스트에서 직접
실행할 때에는 위 표의 값을 해당 shell 환경에만 export한다. 저장소에 `.env`나
seed 파일을 만들지 않는다. Compose는 `docker-compose.validator4.yml`의 environment
항목으로만 seed를 주입한다. 현재 canonical runner는 아직 `MODE=dev`만 허용하며,
`SKIP_SIG_VERIFY`와 `SKIP_QC_VERIFY`는 활성화하지 않는다. Compose/local Docker fixture는
이제 `GOSSIP_ENABLED=true`로 semantic pre-relay admission을 exercise한다.

4-validator genesis의 canonical committee hash는 다음과 같다.

```text
9c3b730a0da99a2b0d9ce16018f33af7c3f2090adabb36f3957cc7b04651f758
```

Genesis domain V4는 chain ID뿐 아니라 block-hash protocol V5, authenticated state-root
schema, Commitment v2 artifact schema/version, canonical validator operator/defaults,
voting power와 derived self-stake, commission, canonical allocations를 포함한다. 또한
HYCK 6 decimals, 1B HYCK max supply, 388,880,000 HYCK future-emissions reserve와
reward policy/formula version, 237 bps APY, 400,000,000 HYCK anchor, 90-minute
accrual cadence, 365-day reward year, 24-hour auto-compound cadence를 함께 인증한다. 따라서
validator committee가 같아도 bootstrap/economic/reward semantics가 다른 노드는 같은
context로 시작할 수 없다. Genesis에서 validator stake와 explicit liquid allocation의
합은 `611,120,000 HYCK`(max supply - future-emissions reserve)를 넘을 수 없다.

```text
4-validator: adbeaea4f29c7a381302e5a0e66ad5fda518077656821661a1318f071125aa00
single-validator: 7ec4f5cbcfbbefc8c1e9f70665703428558dc6947ba50901a78101d7dbfbd60b
```

## 실행 예

호스트 4-node 실행은 네 터미널에서 같은 `genesis.json`과 각 `host-4/nodeN.json`을
사용한다. single local run은 저장소 루트에서 `./scripts/local-node`를 실행하고,
웹 클라이언트는 별도 터미널에서 `cd web`, 최초 한 번 `bun install`, `bun run dev`를
실행한다. `hl-node`와 web은
서로 다른 프로세스이므로 web을 띄우려면 별도 터미널이 필요하다. Docker 하네스는
저장소 루트에서 다음과 같이 실행한다.

```bash
docker compose -f docker-compose.validator4.yml build --pull
docker compose -f docker-compose.validator4.yml up --build
docker compose -f docker-compose.validator4.yml down
```

이 구성은 4개 프로세스의 동일 genesis, authenticated peer addressing, gossip admission,
3-block progress를 확인하는 로컬 재현용이다. Compose command가 각 validator에
`--blocks 3`을 전달하므로 모든 validator가 committed height 3에 도달하면 컨테이너가
정상 종료한다. 따라서 `up --build`가 계속 실행되는 장기 네트워크가 아닌 유한한 smoke
test이며, production 배포 또는 mainnet readiness 증명이 아니다. 각 validator는
`/app/data`를 서로 다른 named RocksDB volume에 마운트하므로 컨테이너를 재생성해도 local
committed/speculative recovery를 재현할 수 있다. `down`은 기본적으로 volume을 보존하고,
완전히 새 fixture가 필요할 때만 명시적으로 volume을 삭제한다.

## RocksDB 재시작 예

single node에서 같은 data directory를 다시 사용하면 finalized chain을 replay하고,
저장된 high/locked QC가 있는 경우 speculative branch도 복원한다.

```bash
hl_restart_dir="$(mktemp -d)"
./scripts/local-node --blocks 3 --data-dir "$hl_restart_dir"
./scripts/local-node --blocks 5 --data-dir "$hl_restart_dir"
```

첫 실행은 committed height 3까지 진행한 뒤 종료하고, 두 번째 실행은 같은 디렉터리에서
복원해 height 5까지 진행한 뒤 종료한다. `--data-dir`를 생략하면
`.hyperlicked/data/<genesis-domain>/<node-id>`가 기본값이다. multi-node에서는 validator별로
별도 경로를 사용해야 한다. BLS seed는 위원회 public
key를 복원하는 32-byte secret input이며, local fixture에서만 재현을 위해 사용한다.
실제 운영에서는 seed를 환경변수나 이미지에 넣지 말고 별도 key custody/HSM을 사용한다.
