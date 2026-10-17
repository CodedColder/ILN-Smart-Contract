# Invoice Liquidity Network — Smart Contracts

Invoice Liquidity Network (ILN) is a two-sided protocol on Stellar/Soroban that lets invoice
holders (freelancers, SMEs) get paid early by liquidity providers, who fund invoices at a
discount and collect the face value at maturity.

This repository is the protocol monorepo: the Soroban smart contracts, the TypeScript SDK and
CLI built on top of them, and the off-chain services (event indexer, notifications) that support
them.

---

## Table of Contents

1. [Repository Layout](#repository-layout)
2. [Smart Contracts](#smart-contracts)
3. [Architecture](#architecture)
4. [Getting Started](#getting-started)
5. [Building & Testing](#building--testing)
6. [Deploying to Testnet](#deploying-to-testnet)
7. [Mainnet Contract Addresses](#mainnet-contract-addresses)
8. [Documentation](#documentation)
9. [Contributing](#contributing)
10. [Security](#security)
11. [License](#license)

---

## Repository Layout

| Path | Type | What it is |
|------|------|------------|
| [`contracts/`](contracts/) | Rust / Soroban | The on-chain protocol — see [Smart Contracts](#smart-contracts) below |
| [`sdk/`](sdk/) | TypeScript (`@iln/sdk`) | Typed client library for calling the contracts from JS/TS |
| [`cli/`](cli/) | TypeScript (`@iln/cli`) | Terminal wallet and invoice-management tool built on the SDK |
| [`indexer/`](indexer/) | TypeScript (`@iln/indexer`) | Streams Soroban/Horizon events into Postgres and exposes a REST API |
| [`notifications/`](notifications/) | TypeScript (`@iln/notifications`) | Webhook, Slack, and email delivery service for invoice lifecycle events |
| [`frontend/`](frontend/) | TypeScript (`@iln/frontend`, Next.js) | Component library / Storybook workspace for the ILN UI (the deployed web app lives in the separate `ILN-Frontend` repo) |
| [`packages/`](packages/) | TypeScript | Shared workspace packages: `types` (domain types), `test-utils`, `eslint-config` |
| [`scripts/`](scripts/) | Bash / TypeScript | Deploy, seed, health-check, spec-generation, and release scripts used by `make` |
| [`tests/e2e/`](tests/e2e/) | TypeScript | Cross-component end-to-end suite (SDK + live local Stellar node + indexer) |
| [`docs/`](docs/) | Markdown | Protocol, architecture, security, and integration documentation |

The Rust crates are a Cargo workspace (see [`Cargo.toml`](Cargo.toml)); the TypeScript packages
are a pnpm workspace (see [`pnpm-workspace.yaml`](pnpm-workspace.yaml)) orchestrated with Turborepo.

---

## Smart Contracts

All contracts live under [`contracts/`](contracts/), compile to Soroban WASM
(`wasm32v1-none`), and are tested natively via `soroban-sdk` test utilities — `cargo test`
does not require a live network.

| Crate | Path | Responsibility |
|-------|------|-----------------|
| `invoice_liquidity` | [`contracts/invoice_liquidity/`](contracts/invoice_liquidity/) | Core escrow contract: submit, fund, settle, cancel, and default invoices; reputation scoring; multi-token support; optional payer oracle |
| `iln_governance` | [`contracts/iln_governance/`](contracts/iln_governance/) | On-chain governance: proposals, voting, delegation, quorum, and timelocked admin actions |
| `iln_distribution` | [`contracts/iln_distribution/`](contracts/iln_distribution/) | Yield and incentive distribution for LPs, freelancers, and payers |
| `reputation_bonus` | [`contracts/reputation_bonus/`](contracts/reputation_bonus/) | Reputation-based discount bonuses and related invoice hooks |
| `insurance_pool` | [`contracts/insurance_pool/`](contracts/insurance_pool/) | Default-protection insurance pool for liquidity providers |
| `iln_fuzz` | [`contracts/fuzz/`](contracts/fuzz/) | Property-based fuzz tests against the core invoice flows |
| *(integration tests)* | [`contracts/tests/`](contracts/tests/) | Cross-contract tests with mock tokens and oracles |

For function signatures, error codes, storage keys, and emitted events, see the
[Contract ABI](docs/contract-abi.md), [Error Codes](docs/error-codes.md),
[Storage Layout](docs/storage-layout.md), and [Events](docs/events.md) docs.

---

## Architecture

```
                    Frontend (web / mobile / dashboard)
                                  │
                          ┌───────▼────────┐
                          │    @iln/sdk    │
                          │ TypeScript SDK │
                          └───────┬────────┘
              ┌───────────────────┼───────────────────┐
              ▼                   ▼                    ▼
      ┌──────────────┐   ┌────────────────┐   ┌─────────────────┐
      │  @iln/cli    │   │ Soroban Smart  │   │  @iln/indexer   │
      │  Terminal    │◄─►│  Contracts     │◄─►│  REST API /     │
      │  Interface   │   │  (WASM)        │   │  event indexer  │
      └──────────────┘   └───────┬────────┘   └────────┬────────┘
                                  │                     │
                         ┌────────▼────────┐   ┌────────▼─────────┐
                         │ Stellar Network │   │ @iln/notifications│
                         │ (Horizon + RPC) │   │ Webhook / email    │
                         └─────────────────┘   └────────────────────┘
```

The SDK talks directly to the contracts over Soroban RPC; the indexer independently streams
contract events from Horizon into Postgres so the CLI, frontend, and integrators can query
history and aggregates without re-simulating transactions. The notifications service consumes
the same event stream to fan out webhooks, Slack messages, and email.

Full write-up, including the on-chain state machine and money flow: [docs/Architecture.md](docs/Architecture.md).

---

## Getting Started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (stable) with the `wasm32v1-none` target:
  `rustup target add wasm32v1-none`
- [Stellar CLI](https://developers.stellar.org/docs/tools/developer-tools#cli) for building,
  testing, and deploying contracts
- Node.js 22+ and [pnpm](https://pnpm.io/) 9 for the TypeScript packages
- Docker (optional) for running a local Stellar node and the indexer/notifications databases

### Clone & install

```bash
git clone https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
make install     # installs deps for sdk, indexer, notifications, tests/e2e
```

A step-by-step walkthrough (toolchain setup through your first testnet deploy) is in the
[Developer Quickstart](docs/developer-quickstart.md). To run the full local stack (contracts,
Docker services, SDK, CLI, indexer, notifications) see the
[Local Development Guide](docs/local-development.md).

---

## Building & Testing

Everything is driven through the root [`Makefile`](Makefile) (`make help` lists all targets):

| Command | Description |
|---------|-------------|
| `make build` | Build contract WASM + the `@iln/sdk` package |
| `make build-rust` | Build optimized contract WASM only (`wasm32v1-none`, release) |
| `make test` | Run the full Rust workspace test suite |
| `make test-invoice` / `test-governance` / `test-distribution` / `test-insurance` | Run tests for a single contract |
| `make fuzz` | Run the property/fuzz test suite (`iln_fuzz`) |
| `make test-e2e` | Run the cross-component end-to-end suite in `tests/e2e/` |
| `make lint` | `cargo fmt --check` + `cargo clippy -D warnings` |
| `make fmt` | Format all Rust code in place |
| `make coverage` | Generate an HTML coverage report with `cargo-tarpaulin` |
| `make spec` | Regenerate the contract ABI/spec JSON (`docs/contract-spec.json`) |
| `make docs` | Regenerate the SDK's TypeDoc API docs |
| `make health` | Run the deployment health check against a live deployment |

For the TypeScript packages individually, use the package manager inside each directory
(`sdk/`, `cli/`, `indexer/`, `notifications/`) — each has its own README with local dev
instructions — or run `pnpm turbo build` / `pnpm turbo test` from the repo root to fan the
same scripts out across the whole workspace.

---

## Deploying to Testnet

```bash
export STELLAR_ACCOUNT=your-account-alias
make build
make deploy-testnet
```

`make deploy-testnet` builds all contracts and deploys them to Stellar testnet via
[`scripts/deploy-testnet.sh`](scripts/deploy-testnet.sh). Use `make seed` to populate a fresh
deployment with sample invoices, and `make reset-testnet` to reset local/testnet state.

---

## Mainnet Contract Addresses

The table below is published by [`scripts/publish-mainnet-contracts.sh`](scripts/publish-mainnet-contracts.sh)
after a mainnet deployment passes `scripts/verify-deployment.ts` (see the
[Mainnet Deployment Runbook](docs/mainnet-deployment-runbook.md)). It is generated from
`.contracts-mainnet.env`, never edited by hand, so the addresses below always match what
verification actually checked.

<!-- MAINNET_CONTRACT_IDS_START -->
| Resource | Contract ID | Notes |
|----------|-------------|-------|
| **`invoice_liquidity`** | `CBUFYH7WGPXJJQPQGRR2HZXNC5DO2WFGVJVWGSLHZ2RP5ZMVDRMVEJ7A` | Primary integration contract; used in [SDK examples](docs/sdk-integration.md) |
| **`iln_governance`** | `CB62YJBLHGYCKMMEWDCSCZM7HHQIXJXXZWVFPWDEWSCMQVZUNZPBHDZ4` | Governance proposals and voting |
| **`iln_distribution`** | `CARUAJQ3UYXDL47WXWFGVHJXNC5LO2WFGVJVWGSLHZ2RP5ZMVDRMVEJ7A` | Rewards distribution |
| **`reputation_bonus`** | `CCWFMXDL3HYOFPQZ56RSTUV2WXYZ3CDEFGHIJKLMNOPQRSTUVWXYZ4ABCD` | Reputation-based bonus rules |
| **`insurance_pool`** | `CDXYZ789ABCDEFGHIJKLMNOPQRSTUVWXYZ012345PQRSTUVWXYZ6789ABCD` | Default-protection insurance pool |
| **Mainnet USDC (SAC)** | `CBMYUV3TBGZB4WF7Y4Z5ABCDEFGHIJKLMNOPQRSTUVWXYZ2EGHIJKLMNO` | Referenced in SDK integration guide |
<!-- MAINNET_CONTRACT_IDS_END -->

---

## Documentation

This README covers the repository as a whole. Deeper documentation lives in [`docs/`](docs/) —
start at the [Documentation Index](docs/index.md), or jump to:

| Topic | Doc |
|-------|-----|
| Contract functions & error codes | [Contract ABI](docs/contract-abi.md), [Error Codes](docs/error-codes.md) |
| Events | [Events](docs/events.md) |
| Governance | [Governance](docs/governance.md) |
| Storage layout | [Storage Layout](docs/storage-layout.md) |
| Security model & risks | [Threat Model](docs/threat-model.md), [Access Control](docs/access-control.md) |
| Upgrades | [Upgrade Guide](docs/upgrade-guide.md) |
| Mainnet deployment | [Mainnet Deployment Runbook](docs/mainnet-deployment-runbook.md) |
| SDK usage | [SDK Integration Guide](docs/sdk-integration.md), [`sdk/README.md`](sdk/README.md) |
| CLI usage | [`cli/README.md`](cli/README.md) |
| Indexer REST API | [Indexer API Reference](docs/api-reference.md), [`indexer/README.md`](indexer/README.md) |
| Notifications (webhooks, HMAC, email) | [`notifications/README.md`](notifications/README.md) |
| End-to-end test suite | [`tests/e2e/README.md`](tests/e2e/README.md) |
| Design decisions | [Architecture Decision Records](docs/adr/README.md) |
| Terminology | [Glossary](docs/glossary.md) |
| Public protocol status (solvency health, oracle uptime) | [Public Status Page](docs/public-status-page.md) |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for commit conventions, changesets, PR size guidelines,
code style, and the review process.

For bug reports, integration questions, and feature requests, see
[Support Channels](docs/support-channels.md) (GitHub Issues and private security
reporting are live; Discussions/Discord are deferred pre-launch).

---

## Security

Do not open a public issue for vulnerabilities. See [SECURITY.md](SECURITY.md) and
[docs/security.md](docs/security.md) for the reporting process, severity classification, and
safe harbor terms.

---

## License

[MIT](LICENSE)
