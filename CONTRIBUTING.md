# Contributing to ILN Smart Contracts

Thank you for contributing to the Invoice Liquidity Network!  
This guide covers everything you need to go from a fresh machine to an accepted pull request.

---

## Table of Contents

- [Monorepo & Cross-Repo Workflow](#-monorepo--cross-repo-workflow)
- [Commit Messages](#-commit-messages)
- [Changesets & Releases](#-changesets--releases)
- [PR Size Guidelines](#-pr-size-guidelines)
- [Issue Assignment & Wave Participation](#-issue-assignment--wave-participation)
- [Formatting](#-formatting)
- [Code Owners](#-code-owners)
- Smart-contract workflow:
  [Environment Setup](#1-environment-setup) ·
  [Building](#2-building-the-contracts) ·
  [Testing](#3-running-tests) ·
  [Code Style](#4-code-style) ·
  [PR Requirements](#5-pr-requirements) ·
  [Review Process](#6-review-process) ·
  [Soroban Gotchas](#7-soroban-specific-gotchas)

---

## 🗂️ Monorepo & Cross-Repo Workflow

ILN is developed across a small set of repositories. Most contributors touch
only one, but anything user-facing usually spans two.

| Repository | What lives there |
|------------|------------------|
| `ILN-Smart-Contract` (this repo) | Rust/Soroban contracts, the `@iln/sdk` TypeScript package, the indexer, the notifications service, scripts, and docs |
| `ILN-Frontend` | The web app that consumes `@iln/sdk` |

### TypeScript package development

The TypeScript packages in this repo (`sdk/`, `indexer/`, `notifications/`,
`tests/e2e/`) are independent npm packages. The `frontend/` directory is a
Storybook workspace for the ILN component library. They are intended to be driven
through the root [`Makefile`](Makefile), which prefers `pnpm` when available and
falls back to `npm`:

```bash
make install      # install dependencies across all TS packages
make build        # build the contracts (cargo) + the @iln/sdk package
make test         # cargo test (Rust workspace)
make test-insurance  # cargo test -p insurance_pool (insurance pool contract)
make test-e2e     # end-to-end suite in tests/e2e
make lint         # cargo fmt --check + clippy
make docs         # regenerate the SDK API docs
make help         # list every target
```

> If you have Turborepo or a pnpm workspace configured at the org root, the
> equivalent commands are `pnpm install`, `pnpm turbo build`, and
> `pnpm turbo test` — they fan the same scripts out across packages.

### Making a cross-repo change

When an SDK change requires a matching frontend update (e.g. a new method or a
changed signature), keep them releasable together:

1. **Land the SDK change first** in this repo behind a new version. Add a
   changeset (see below) describing the public API change.
2. **Publish** `@iln/sdk` (handled by the release workflow on merge to `main`).
3. **Bump the dependency** in `ILN-Frontend` to the new SDK version and make the
   matching UI change in a separate PR there.
4. **Link the two PRs** to each other in their descriptions so reviewers can see
   the full change set.

Never make a breaking SDK change and rely on an unpublished local build in the
frontend — every PR must build against published, versioned packages.

---

## 📝 Commit Messages

This project uses [Conventional Commits](https://www.conventionalcommits.org/) so that the changelog can be generated automatically with `make changelog`.

**Format:** `<type>(<optional scope>): <description>`

| Type | When to use |
|------|-------------|
| `feat` | A new feature or contract function |
| `fix` | A bug fix |
| `refactor` | Code change that neither fixes a bug nor adds a feature |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `docs` | Documentation only changes |
| `build` | Build system or external dependency changes |
| `ci` | CI configuration and workflow changes |
| `chore` | Tooling or housekeeping that doesn't touch `src` |

### Scopes per package

The optional scope tells readers (and the changelog) which package changed. Use
the package directory name:

| Scope | Area |
|-------|------|
| `contracts` *(or a crate name: `invoice_liquidity`, `iln_governance`, `iln_distribution`, `reputation_bonus`, `insurance_pool`)* | Soroban contract code |
| `sdk` | The `@iln/sdk` TypeScript package |
| `indexer` | The event indexer service |
| `notifications` | The notification service |
| `scripts` | Operational / deployment scripts |
| `docs` | Documentation |
| `ci` | Workflows and pipeline config |

**Examples:**
```
feat(governance): add quorum requirement for proposal passing
fix(sdk): handle missing allowance in fundInvoice
docs(sdk): write comprehensive integration guide
test(indexer): add reputation endpoint coverage
chore(scripts): add contract health check monitoring script
```

Breaking changes must include `BREAKING CHANGE:` in the commit footer:
```
feat(sdk)!: rename fund_invoice to fund

BREAKING CHANGE: fund_invoice has been renamed to fund in the invoice_liquidity contract.
```

---

## 🔖 Changesets & Releases

Version management for the published TypeScript packages uses
[Changesets](https://github.com/changesets/changesets). Any PR that changes the
public surface of a published package (today that's `@iln/sdk`, and soon `@iln/cli`)
must include a changeset so the version bump and changelog entry are generated
automatically.

```bash
# From the repo root, after staging your code change:
npm run changeset
```

The wizard asks which packages changed and whether the bump is **patch**
(bug fix), **minor** (backwards-compatible feature), or **major** (breaking
change), then writes a markdown file under `.changeset/`. Commit that file with
your change.

Example commit message:
`feat: add Changesets for coordinated SDK and CLI version management`

- A PR with no changeset is fine for changes that don't affect a published
  package (contract-only work, docs, CI).
- On merge to `main`, the release workflow consumes pending changesets, bumps
  versions, updates `CHANGELOG.md`, and publishes.

Rust crate versions and the contract changelog continue to be managed with
`make changelog` (git-cliff) — see the [Commit Messages](#-commit-messages)
section.

---

## 📏 PR Size Guidelines

Prefer **small, focused PRs** — they get reviewed faster and merged sooner.

- Aim for **under ~400 lines of diff** (excluding generated files, lockfiles,
  and snapshots). Larger changes are fine when they're mechanical, but flag them
  in the description.
- **One logical change per PR.** Don't mix a refactor with a feature, or a
  contract change with an unrelated docs cleanup.
- Split large efforts into a reviewable sequence: scaffolding → core logic →
  tests → docs, each as its own PR where practical.
- If a PR *must* be large (e.g. a new contract), call it out up front and add a
  reviewer guide in the description ("start with `X`, then `Y`").

---

## 🌊 Issue Assignment & Wave Participation

Contributions are organised into **Waves** — time-boxed batches of issues.

1. **Find an issue.** Browse open issues; those tagged for the current Wave are
   labelled accordingly. Good first issues are labelled `good first issue`.
2. **Get assigned before you start.** Comment on the issue to request it and wait
   for a maintainer to assign you, so two people don't build the same thing. One
   active issue per contributor at a time unless told otherwise.
3. **Reference the issue** in your branch, commits, and PR. Close it from the PR
   with a `Closes #<n>` footer.
4. **Stay responsive.** If an assigned issue goes quiet for several days a
   maintainer may unassign it so someone else can pick it up.
5. **Ask early.** Use a [GitHub Discussion](https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract/discussions)
   or the issue thread if scope is unclear — clarifying before coding saves a
   round of review.


## 🎨 Formatting

All code must be formatted with `rustfmt` before committing. CI will reject unformatted code.

```bash
cargo fmt --all
```

Project-specific settings are in [`rustfmt.toml`](rustfmt.toml) (`max_width = 100`, `edition = "2021"`).


## 👥 Code Owners

This repo uses a [CODEOWNERS](.github/CODEOWNERS) file to automatically request reviews from the right team on every PR.

| Path | Owner |
|------|-------|
| `contracts/` | `@Keengfk/contracts-team` |
| `docs/` | `@Keengfk/docs-lead` |
| `scripts/`, `.github/workflows/` | `@Keengfk/devops` |
| `SECURITY.md` | `@Keengfk/security-lead` |
| everything else | `@Keengfk/maintainers` |

CODEOWNER approval is required before merging (enforced via branch protection on `main`). To enable this on a new repo, go to **Settings → Branches → Branch protection rules** and check **Require review from Code Owners**.


## 🔧 Smart-Contract Workflow

The sections below cover the Rust/Soroban contribution loop end to end:

1. [Environment Setup](#1-environment-setup)
2. [Building the Contracts](#2-building-the-contracts)
3. [Running Tests](#3-running-tests)
4. [Code Style](#4-code-style)
5. [PR Requirements](#5-pr-requirements)
6. [Review Process](#6-review-process)
7. [Soroban-Specific Gotchas](#7-soroban-specific-gotchas)

---

## 1. Environment Setup

### Rust toolchain

```bash
# Install rustup if you don't have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Minimum supported version: 1.74
rustup update stable

# Add the WASM target used by Soroban
rustup target add wasm32v1-none

# Add formatting and linting components
rustup component add rustfmt clippy
```

### Stellar CLI

```bash
cargo install --locked stellar-cli
stellar --version
```

> Re-run the install command to upgrade an existing installation.

### Clone the repo

```bash
git clone https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
```

For a more detailed walkthrough (testnet account setup, troubleshooting) see
[`docs/developer-quickstart.md`](docs/developer-quickstart.md).

---

## 2. Building the Contracts

```bash
# Debug build (native, fast — used for tests)
cargo build

# Optimised WASM build (required before deployment)
cargo build --release --target wasm32-unknown-unknown

# Alternative Soroban-specific optimized build
# `cargo build-wasm` is a workspace alias defined in `.cargo/config.toml`.
# It builds optimized WASM to the Soroban-specific target:
# cargo build --target wasm32v1-none --release
# or use the Makefile shortcut:
# make soroban-optimize
```

WASM output lands in `target/wasm32v1-none/release/*.wasm`.

> The `build-wasm` alias is defined in `.cargo/config.toml`.  
> The release profile enables LTO and `opt-level = "z"` — typical output is 10–80 KB per contract.

---

## 3. Running Tests

### Unit and integration tests

```bash
# Entire workspace
cargo test

# Single contract
cargo test -p invoice_liquidity
cargo test -p iln_governance
cargo test -p iln_distribution
cargo test -p reputation_bonus
cargo test -p insurance_pool

# Useful flags
cargo test -p invoice_liquidity -- --nocapture   # show stdout
cargo test -p invoice_liquidity test_name        # filter by name
```

Tests run on your native architecture via `soroban-sdk` test utilities — no WASM build needed.

### Fuzz / property-based tests

```bash
cargo test -p iln_fuzz
```

Property tests generate thousands of random cases and may take a few minutes.
To skip them during rapid iteration:

```bash
cargo test -p invoice_liquidity -- --skip prop_
# or limit case count
PROPTEST_CASES=100 cargo test -p invoice_liquidity
```

### Coverage

CI enforces **≥ 95 % line coverage** on `invoice_liquidity` using
[cargo-tarpaulin](https://github.com/xd009642/tarpaulin).  Run it locally
before pushing if you touch that crate:

```bash
cargo install cargo-tarpaulin --locked
cargo tarpaulin -p invoice_liquidity --fail-under 95
```

---

## 4. Code Style

### Formatting

All code must be formatted with `rustfmt` using the workspace defaults:

```bash
cargo fmt --all
```

CI will reject PRs with formatting differences.

### Linting

Zero Clippy warnings are required:

```bash
cargo clippy --all-targets -- -D warnings
```

Fix every warning before opening a PR.  Do not use `#[allow(...)]` to silence
warnings without a comment explaining why.

### General conventions

- Keep functions small and single-purpose.
- Prefer explicit error types over `unwrap` / `expect` in contract code.
- Document public functions with a `///` doc comment.
- Avoid introducing new dependencies without discussion in an issue first.

---

## 5. PR Requirements

### Branch naming

```
<type>/<short-description>
```

Examples: `feat/multi-token-support`, `fix/overflow-in-discount`, `docs/contributing`.

### Commit messages — Conventional Commits

Follow the [Conventional Commits](https://www.conventionalcommits.org/) spec:

```
<type>(<optional scope>): <short summary>

[optional body]

[optional footer — e.g. Closes #101]
```

Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`.

Example:

```
docs: add CONTRIBUTING.md for smart contract contributors

Covers env setup, build, test, style, PR process, and Soroban gotchas.

Closes #101
```

### Checklist before opening a PR

- [ ] `cargo fmt --all` — no diff
- [ ] `cargo clippy --all-targets -- -D warnings` — zero warnings
- [ ] `cargo test` — all tests pass
- [ ] `cargo test -p iln_fuzz` — fuzz suite passes
- [ ] `cargo test -p insurance_pool` — insurance pool tests pass (if `insurance_pool` was modified)
- [ ] Coverage ≥ 95 % if `invoice_liquidity` was modified
- [ ] New behaviour is covered by tests
- [ ] `cargo build-wasm` succeeds (required for any contract change)
- [ ] PR description explains *what* changed and *why*
- [ ] Related issue linked in the PR description or footer (`Closes #<n>`)

---

## 6. Review Process

1. Open a PR against `main` on the upstream repo
   (`Invoice-Liquidity-Network/ILN-Smart-Contract`).
2. CI runs automatically: `test → clippy → benchmarks → coverage`.
   All jobs must be green before review begins.
3. At least one maintainer approval is required to merge.
4. Address review comments with new commits (do not force-push during review).
5. A maintainer will squash-merge once approved.

---

## 7. Soroban-Specific Gotchas

### WASM target is `wasm32v1-none`, not `wasm32-unknown-unknown`

Soroban uses Wasm 2.0 with no WASI.  Always use:

```bash
rustup target add wasm32v1-none
cargo build --target wasm32v1-none --release
```

Using the old `wasm32-unknown-unknown` target will produce a binary that the
Stellar runtime rejects.

### `std` is not available in contract code

Contract crates use `#![no_std]`.  Use `soroban-sdk` types (`Vec`, `Map`,
`String`, …) instead of `std` equivalents.  The `std` crate is only available
in test code gated behind `#[cfg(test)]`.

### `cargo test` does not test the WASM binary

Unit tests run on native via the SDK's mock environment.  Always do a
`cargo build-wasm` before deploying to confirm the WASM compiles cleanly —
some `no_std` violations only surface at WASM compile time.

### Testnet deployment

See [`docs/developer-quickstart.md § 7`](docs/developer-quickstart.md) for the
full deploy workflow.  The live testnet contract ID is:

```
CD3TE3IAHM737P236XZL2OYU275ZKD6MN7YH7PYYAXYIGEH55OPEWYJC
```

### Benchmark regression guard

The `scripts/check_benchmark_regression.sh` script compares instruction counts
against stored baselines for `invoice_liquidity` and `iln_governance`.  CI
runs it as a warning-only step, but a large regression will be flagged during
review.  Run it locally after performance-sensitive changes:

```bash
bash scripts/check_benchmark_regression.sh
```

---



## Economics & risk documentation reviews

Assumptions in the LP risk guide, governance playbook, and token economics paper
are reviewed on a fixed cadence (quarterly, plus usage-growth and incident
triggers). See [`docs/review-cadence.md`](docs/review-cadence.md) for ownership,
scope, and required PR outputs. The reading order is indexed under
[Protocol Economics & Risk](docs/index.md#protocol-economics--risk).

## Questions?

See [`docs/support-channels.md`](docs/support-channels.md) for the live support
paths (GitHub Issues today; Discussions when enabled). You can also comment on
the relevant issue.
## Dependabot
Dependabot is configured to automatically submit PRs to update dependencies. Maintainers should review and merge them as appropriate.
