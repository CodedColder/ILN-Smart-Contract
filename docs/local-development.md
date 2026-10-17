# Local Development Guide

This guide sets up the full ILN local stack: Soroban contracts, SDK, CLI, indexer, notifications service, and the Docker services used by local integration work.

## Prerequisites

| Tool | Version | Why it is needed |
|------|---------|------------------|
| Git | Recent | Clone the repository and submodules. |
| Node.js | 20.x LTS or newer | Run the SDK, CLI, indexer, notifications service, and TypeScript scripts. |
| pnpm | 9.x or newer | Preferred package manager for contributors. The repo also includes npm lockfiles, so `npm ci` is acceptable per service. |
| Docker Desktop / Docker Engine | 24.x or newer | Run the local Stellar quickstart node and notifications Postgres container. |
| Docker Compose | v2.x | Start and inspect the local stack. |
| Rust | 1.74 or newer | Build and test Soroban smart contracts. |
| Stellar CLI | Latest stable | Configure local/testnet networks, fund accounts, deploy, and invoke contracts. |
| curl | Recent | Health checks and install scripts. |

Windows contributors should use Ubuntu on WSL2 and run all commands from the WSL terminal. Docker Desktop must have WSL integration enabled for that distribution.

## Clone

Clone with submodules so future vendored examples or contract fixtures are included:

```bash
git clone --recurse-submodules https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
git submodule update --init --recursive
```

If you already cloned without submodules, run:

```bash
git submodule update --init --recursive
```

## Install Toolchains

Install Rust and the Soroban WASM target:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup update stable
rustup target add wasm32v1-none
```

Install the Stellar CLI:

```bash
cargo install --locked stellar-cli
stellar --version
```

Install Node.js 20 and pnpm:

```bash
node --version
corepack enable
corepack prepare pnpm@latest --activate
pnpm --version
```

The repo currently has separate package manifests under `sdk/`, `cli/`, `indexer/`, and `notifications/` rather than a root pnpm workspace. Run package-manager commands from the service directory.

## Environment Variables

Local contract deployment writes `.contracts-local.env`. Source it before running scripts that need contract IDs:

```bash
source .contracts-local.env
```

| Variable | Used by | Default | Description |
|----------|---------|---------|-------------|
| `INVOICE_LIQUIDITY_ID` | Local deploy output, SDK examples, scripts | none | Deployed `invoice_liquidity` contract ID. Created by `scripts/deploy-local.sh`. |
| `ILN_GOVERNANCE_ID` | Local deploy output | none | Deployed governance contract ID. |
| `ILN_DISTRIBUTION_ID` | Local deploy output | none | Deployed distribution contract ID. |
| `REPUTATION_BONUS_ID` | Local deploy output | none | Deployed reputation bonus contract ID. |
| `INSURANCE_POOL_ID` | Local deploy output | none | Deployed `insurance_pool` contract ID. Created by `scripts/deploy-insurance-pool.sh` (also runnable via `docker compose --profile insurance up insurance-pool-deploy`). |
| `INSURANCE_POOL_ADMIN` | `scripts/deploy-insurance-pool.sh` | deploying account's own address | Address passed to `insurance_pool`'s `initialize(admin, coverage)`. |
| `INSURANCE_POOL_COVERAGE` | `scripts/deploy-insurance-pool.sh` | `1000000000` | Flat per-claim compensation cap (token stroops) passed to `initialize`. |
| `INSURANCE_POOL_CONTRACT_ID` | Docker Compose indexer/notifications containers | none | Deployed `insurance_pool` contract ID, made available to local services once deployed. Set to `$INSURANCE_POOL_ID` after running the deploy script. |
| `NETWORK` | Local deploy output | `local` | Stellar CLI network name used during deployment. |
| `SOURCE` | Local deploy output | `alice` | Stellar CLI key name used to deploy local contracts. |
| `SOROBAN_RPC_URL` | `scripts/smoke-test.ts`, migration scripts | `https://soroban-testnet.stellar.org` | RPC endpoint for smoke tests or migration helpers. Use `http://localhost:8000` for local quickstart. |
| `NETWORK_PASSPHRASE` | `scripts/smoke-test.ts`, migration scripts | Stellar testnet passphrase | Network passphrase for the target chain. Local standalone passphrase is `Standalone Network ; February 2021`. |
| `CONTRACT_ID` | `scripts/smoke-test.ts` | none | Contract ID to smoke test. Usually set to `$INVOICE_LIQUIDITY_ID`. |
| `DISTRIBUTION_CONTRACT_ID` | `scripts/smoke-test.ts` | none | Optional. Set to `$ILN_DISTRIBUTION_ID` to also run the governance-token SAC-admin check (`verify_mint_authority()`, Issue #861); the check is skipped when unset. |
| `ADMIN_SECRET` | `scripts/migrate-v1-v2.ts` | none | Secret key for the admin account authorized to migrate contracts. Do not commit this value. |
| `V1_WASM` | `scripts/migrate-v1-v2.ts` | none | Path to the previous WASM when testing migrations. |
| `V2_WASM` | `scripts/migrate-v1-v2.ts` | none | Path to the new WASM when testing migrations. |
| `V2_WASM_HASH` | `scripts/migrate-v1-v2.ts` | none | Uploaded hash for the new WASM. |
| `STELLAR_TESTNET_DEPLOYER_SECRET` | `scripts/deploy-testnet.sh`, deploy workflow | none | Testnet deployer secret key. Store only in shell secrets or GitHub Actions secrets. |
| `TEST_SUBMITTER_SECRET` | SDK integration tests | none | Friendbot-funded testnet submitter secret. Integration tests skip when absent. |
| `TEST_LP_SECRET` | SDK integration tests | none | Friendbot-funded testnet liquidity-provider secret. Integration tests skip when absent. |
| `TEST_RPC_URL` | SDK integration tests | Stellar testnet RPC | Optional SDK integration RPC override. |
| `PORT` | Indexer and notifications | `3001` | HTTP listen port. Use different ports if running both services at once. |
| `DB_PATH` | Indexer | `./indexer.db` | SQLite database path for indexed state. |
| `CACHE_TTL_MS` | Indexer | `60000` | Cache time-to-live in milliseconds for API responses. |
| `DATABASE_URL` | Docker Compose notifications container | `postgres://notifications:notifications@notifications-db:5432/notifications` | Postgres DSN passed to the container. The current local service code uses its in-memory/default store, but Compose defines this for database-backed deployments. |

Recommended local shell setup:

```bash
export SOROBAN_RPC_URL=http://localhost:8000
export NETWORK_PASSPHRASE="Standalone Network ; February 2021"
export PORT=3002
```

## Install Service Dependencies

Install dependencies in each Node service:

```bash
(cd sdk && pnpm install)
(cd cli && pnpm install)
(cd indexer && pnpm install)
(cd notifications && pnpm install)
```

If pnpm is unavailable, use the checked-in npm lockfiles:

```bash
(cd sdk && npm ci)
(cd cli && npm ci)
(cd indexer && npm ci)
(cd notifications && npm install)
```

## Start Docker Services

The checked-in `docker-compose.yml` starts:

| Service | Port | Health check |
|---------|------|--------------|
| `stellar` | `8000`, `11626` | `curl http://localhost:8000/ledger` |
| `notifications-db` | host `5433` to container `5432` | `pg_isready -U notifications` |
| `notifications` | `3001` | `curl http://localhost:3001/health` |

Start the local Stellar node first:

```bash
docker compose up -d stellar
docker compose ps
curl -s http://localhost:8000/ledger
```

Then start the notifications database and containerized notifications service when needed:

```bash
docker compose up -d notifications-db notifications
docker compose ps
curl -s http://localhost:3001/health
```

Inspect logs:

```bash
docker compose logs -f stellar
docker compose logs -f notifications
```

Stop services:

```bash
docker compose down
```

Remove local volumes only when you want a clean chain/database:

```bash
docker compose down -v
```

## Configure Local Stellar

The helper script verifies Docker, Rust, Stellar CLI, and the WASM target; starts the Stellar container; configures a `local` Stellar CLI network; creates `alice`; and funds it.

```bash
./scripts/setup-local-env.sh
```

Manual equivalent:

```bash
docker compose up -d stellar
stellar network add \
  --global local \
  --rpc-url http://localhost:8000 \
  --network-passphrase "Standalone Network ; February 2021" \
  --override
stellar keys generate --global alice
stellar account fund alice --network local
```

Verify:

```bash
stellar network ls
stellar keys address alice
curl -s http://localhost:8000/ledger
```

## Build And Deploy Contracts

Run native Rust checks:

```bash
cargo build
cargo test
```

Build optimized Soroban WASM:

```bash
make build
# or
cargo build --target wasm32v1-none --release
```

Deploy every contract to the local network:

```bash
./scripts/deploy-local.sh local alice
source .contracts-local.env
echo "$INVOICE_LIQUIDITY_ID"
```

Run the smoke test against the local deployment:

```bash
CONTRACT_ID="$INVOICE_LIQUIDITY_ID" \
SOROBAN_RPC_URL=http://localhost:8000 \
NETWORK_PASSPHRASE="Standalone Network ; February 2021" \
npx --yes tsx scripts/smoke-test.ts
```

`deploy-local.sh` deploys every contract, including `insurance_pool`. To deploy
just `insurance_pool` on its own (e.g. after changing only that contract), run
`./scripts/deploy-insurance-pool.sh local alice` directly, or use the opt-in
Docker Compose profile, which installs the Stellar CLI and runs the same
script inside a container:

```bash
docker compose --profile insurance up insurance-pool-deploy
```

This is not part of the default `docker compose up` — it's opt-in because it
installs the Stellar CLI via `cargo install` on first run, which can take
several minutes.

## Run Services Individually

### SDK

```bash
cd sdk
pnpm install
pnpm run build
pnpm test
```

The SDK is a library, so it does not start a server. Use `docs/sdk-integration.md` and `sdk/README.md` for client examples.

### CLI

```bash
cd cli
pnpm install
pnpm run build
node dist/index.js config set network testnet
node dist/index.js config set rpcUrl https://soroban-testnet.stellar.org
node dist/index.js wallet list
```

For local-network experiments, set the RPC URL to `http://localhost:8000`. The current CLI config accepts `testnet` or `mainnet` as network names, so local RPC use is best kept to commands that do not validate the network enum.

### Indexer

```bash
cd indexer
pnpm install
PORT=3002 DB_PATH=./indexer.db CACHE_TTL_MS=60000 pnpm run dev
```

Verify in another terminal:

```bash
curl -s http://localhost:3002/health || true
curl -s http://localhost:3002/stats || true
```

Routes may vary as the indexer evolves; see `indexer/src/app.ts` for the current API.

### Notifications

Run directly:

```bash
cd notifications
pnpm install
PORT=3001 pnpm run dev
curl -s http://localhost:3001/health
```

Run with Docker Compose:

```bash
docker compose up -d notifications-db notifications
curl -s http://localhost:3001/health
```

## Run Tests

| Area | Command |
|------|---------|
| All Rust contracts | `cargo test` |
| Contract WASM build | `make build` |
| Fuzz/property crate | `cargo test -p iln_fuzz` |
| SDK unit tests | `(cd sdk && pnpm test)` |
| SDK integration tests | `(cd sdk && TEST_SUBMITTER_SECRET=S... TEST_LP_SECRET=S... pnpm run test:integration)` |
| CLI tests | `(cd cli && pnpm test)` |
| Indexer tests | `(cd indexer && pnpm test)` |
| Notifications tests | `(cd notifications && pnpm test)` |
| Notifications coverage | `(cd notifications && pnpm run test:coverage)` |

## Common Errors

### macOS

| Error | Fix |
|-------|-----|
| `xcrun: error: invalid active developer path` | Run `xcode-select --install`, then retry `cargo build`. |
| Docker containers start but ports are unavailable | Check Docker Desktop is running and no local process owns ports `8000`, `3001`, or `5433` with `lsof -i :8000`. |
| `stellar: command not found` after install | Add `export PATH="$HOME/.cargo/bin:$PATH"` to `~/.zshrc` and open a new shell. |
| Slow first Rust build | Expected on first run. Keep `target/` between runs and avoid `cargo clean` unless needed. |

### Ubuntu

| Error | Fix |
|-------|-----|
| `linker 'cc' not found` | Install build tools: `sudo apt-get update && sudo apt-get install -y build-essential pkg-config libssl-dev`. |
| Docker permission denied | Add your user to the Docker group: `sudo usermod -aG docker "$USER"`, log out, and log back in. |
| `error[E0463]: can't find crate for 'core'` when building WASM | Run `rustup target add wasm32v1-none`. |
| `cargo install stellar-cli` fails while compiling OpenSSL crates | Install `pkg-config` and `libssl-dev`, then retry. |

### Windows WSL

| Error | Fix |
|-------|-----|
| `Cannot connect to the Docker daemon` | Open Docker Desktop, enable WSL integration for your Ubuntu distro, then restart the WSL shell. |
| Filesystem is very slow | Keep the repository under the Linux filesystem, for example `~/src/ILN-Smart-Contract`, not `/mnt/c/...`. |
| Port already in use from Windows | Stop the Windows process or change the service `PORT`. Check Windows with PowerShell `netstat -ano | findstr :3001`. |
| CRLF script errors such as `/bin/bash^M` | Run `git config core.autocrlf input` and re-checkout the affected script, or run `dos2unix scripts/*.sh`. |

### Cross-platform

| Error | Fix |
|-------|-----|
| `wasm32v1-none` not installed | `rustup target add wasm32v1-none`. |
| `stellar network add` says the network exists | Add `--override` or remove the old entry with `stellar network rm local`. |
| Local ledger health check fails | Run `docker compose logs stellar`, then recreate with `docker compose down -v && docker compose up -d stellar`. |
| SDK integration tests skip | Set both `TEST_SUBMITTER_SECRET` and `TEST_LP_SECRET`; the suite intentionally skips without them. |
| Jest ESM errors in the indexer | Use Node.js 20 and the package script: `(cd indexer && pnpm test)`, which sets `NODE_OPTIONS`. |
| Notifications container is unhealthy | Check `docker compose logs notifications`; confirm port `3001` is free and rebuild with `docker compose build notifications`. |

## Fresh-machine Verification

Before changing this guide, verify the happy path on at least two environments, preferably macOS and Ubuntu or Windows WSL:

```bash
git clone --recurse-submodules https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
./scripts/setup-local-env.sh
cargo test
make build
./scripts/deploy-local.sh local alice
(cd sdk && pnpm install && pnpm test)
(cd cli && pnpm install && pnpm test)
(cd indexer && pnpm install && pnpm test)
(cd notifications && pnpm install && pnpm test)
docker compose up -d notifications-db notifications
curl -s http://localhost:3001/health
```

Record any new failure and fix in the troubleshooting section before merging.
