# =============================================================================
# ILN Smart Contract — Developer Makefile
# =============================================================================
# Usage:
#   make build          Build optimised WASM for all contracts
#   make test           Run the full test suite (native target)
#   make fmt            Format all Rust source code
#   make lint           Run Clippy linter across the workspace
#   make deploy-testnet Deploy all contracts to Stellar testnet
#   make coverage       Run tarpaulin code-coverage report
#   make clean          Remove build artefacts
#   make help           Print this help message
# =============================================================================

.PHONY: build test fmt lint deploy-testnet coverage clean help

# Default target
.DEFAULT_GOAL := help

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

## build: Compile all contracts to optimised WASM (wasm32v1-none, release profile)
build:
	cargo build-wasm
	@echo "--- Built WASM artefacts ---"
	@ls -lh target/wasm32v1-none/release/*.wasm

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

## test: Run the full test suite on the native target (includes unit + integration tests)
test:
	cargo test-native

# ---------------------------------------------------------------------------
# Format
# ---------------------------------------------------------------------------

## fmt: Format all Rust source files using rustfmt
fmt:
	cargo fmt --all

# ---------------------------------------------------------------------------
# Lint
# ---------------------------------------------------------------------------

## lint: Run Clippy with workspace-wide checks and deny warnings
lint:
	cargo clippy --all-targets --all-features -- -D warnings

# ---------------------------------------------------------------------------
# Deploy
# ---------------------------------------------------------------------------

## deploy-testnet: Deploy all contracts to the Stellar testnet
##   Requires the Stellar CLI to be installed and STELLAR_ACCOUNT to be set.
##   Example: STELLAR_ACCOUNT=my-account make deploy-testnet
deploy-testnet: build
	@if [ -z "$$STELLAR_ACCOUNT" ]; then \
		echo "Error: STELLAR_ACCOUNT is not set. Export your Stellar account alias before deploying."; \
		exit 1; \
	fi
	@echo "--- Deploying invoice_liquidity ---"
	stellar contract deploy \
		--wasm target/wasm32v1-none/release/invoice_liquidity.wasm \
		--source "$$STELLAR_ACCOUNT" \
		--network testnet
	@echo "--- Deploying iln_governance ---"
	stellar contract deploy \
		--wasm target/wasm32v1-none/release/iln_governance.wasm \
		--source "$$STELLAR_ACCOUNT" \
		--network testnet
	@echo "--- Deploying iln_distribution ---"
	stellar contract deploy \
		--wasm target/wasm32v1-none/release/iln_distribution.wasm \
		--source "$$STELLAR_ACCOUNT" \
		--network testnet
	@echo "--- Deploying reputation_bonus ---"
	stellar contract deploy \
		--wasm target/wasm32v1-none/release/reputation_bonus.wasm \
		--source "$$STELLAR_ACCOUNT" \
		--network testnet

# ---------------------------------------------------------------------------
# Coverage
# ---------------------------------------------------------------------------

## coverage: Run cargo-tarpaulin and produce an HTML coverage report
##   Requires cargo-tarpaulin: cargo install cargo-tarpaulin
coverage:
	cargo tarpaulin \
		--target x86_64-unknown-linux-gnu \
		--workspace \
		--exclude iln_fuzz \
		--out Html \
		--output-dir coverage/
	@echo "--- Coverage report written to coverage/tarpaulin-report.html ---"

# ---------------------------------------------------------------------------
# Clean
# ---------------------------------------------------------------------------

## clean: Remove all build artefacts
clean:
	cargo clean

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------

## help: Print all documented Makefile targets
help:
	@echo ""
	@echo "ILN Smart Contract — available targets:"
	@echo ""
	@grep -E '^## ' Makefile | sed 's/^## /  /'
	@echo ""
.PHONY: help install build build-rust build-sdk test test-rust test-e2e fuzz test-fuzz test-formal lint \
        fmt fmt-check clippy deploy-testnet seed reset reset-testnet docs spec \
        changelog soroban-optimize health test-insurance test-invoice test-governance test-distribution


# Package manager for the TypeScript workspaces (sdk, indexer, notifications,
# tests/e2e). Prefer pnpm when available, otherwise fall back to npm.
PKG := $(shell command -v pnpm >/dev/null 2>&1 && echo pnpm || echo npm)
NODE_PKGS := sdk indexer notifications tests/e2e

# Default target: list everything.
.DEFAULT_GOAL := help

help: ## Show this help message
	@echo "ILN Smart Contract — developer workflows"
	@echo ""
	@echo "Usage: make <target>"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

install: ## Install Node dependencies across all TypeScript packages
	@for pkg in $(NODE_PKGS); do \
		echo "📦 installing $$pkg ($(PKG))"; \
		(cd $$pkg && $(PKG) install) || exit 1; \
	done

build: build-rust build-sdk ## Build everything (contracts + SDK)

build-rust: ## Build optimized contract WASM (wasm32v1-none release)
	cargo build --target wasm32v1-none --release

build-sdk: ## Build the @iln/sdk TypeScript package
	cd sdk && $(PKG) run build

build-insurance: ## Build the insurance pool contract to optimised WASM
	cargo build --target wasm32v1-none --release -p insurance_pool
	@echo "--- Built insurance_pool WASM artefact ---"
	@ls -lh target/wasm32v1-none/release/insurance_pool.wasm

soroban-optimize: ## Build WASM via the wasm32-unknown-unknown target
	cargo build --release --target wasm32-unknown-unknown

test: test-rust ## Run the Rust unit/integration test suite
test-rust: ## Run cargo test for the whole workspace
	cargo test

test-invoice: ## Run tests for the invoice liquidity contract
	cargo test -p invoice_liquidity

test-governance: ## Run tests for the governance contract
	cargo test -p iln_governance

test-distribution: ## Run tests for the distribution contract
	cargo test -p iln_distribution

test-insurance: ## Run tests for the insurance pool contract
	cargo test -p insurance_pool

test-e2e: ## Run the end-to-end test suite (tests/e2e)
	cd tests/e2e && $(PKG) run test:e2e

fuzz: ## Run the property/fuzz test suite
	cargo test -p iln_fuzz

test-fuzz: ## Run fuzz tests for the iln_fuzz crate
	cargo test -p iln_fuzz

test-formal: ## Gate on formal counterexamples + proof obligations (#1100, #1101)
	bash scripts/run-formal-verification.sh

lint: fmt-check clippy check-no-unwrap event-coverage ## Lint everything (rustfmt check + clippy + panic-path gate + event coverage)

fmt: ## Format all Rust code in place
	cargo fmt --all

fmt-check: ## Verify Rust formatting without modifying files
	cargo fmt --all -- --check

clippy: ## Run clippy with warnings denied
	cargo clippy --all-targets -- -D warnings

check-no-unwrap: ## Gate: fail if unwrap()/expect() appears in non-test contract source (#845)
	bash scripts/check-no-unwrap-in-contract-source.sh

event-coverage: ## Gate: every state-mutating entrypoint emits an event or is an approved exemption (#858)
	npx --no-install tsx --test scripts/check-event-coverage.test.ts
	npx --no-install tsx scripts/check-event-coverage.ts --check

deploy-testnet: ## Deploy all contracts to Stellar testnet
	bash scripts/deploy-testnet.sh

deploy-mainnet-dry-run: ## Dry-run the mainnet deployment (no transactions submitted)
	bash scripts/deploy-mainnet.sh --dry-run

deploy-mainnet: ## Deploy all contracts to Stellar mainnet (requires CONFIRM="DEPLOY TO MAINNET")
	bash scripts/deploy-mainnet.sh

seed: ## Seed the testnet deployment with sample data
	npx tsx scripts/seed.ts

reset: reset-testnet ## Alias for reset-testnet
reset-testnet: ## Reset local/testnet state
	bash scripts/reset-testnet.sh

health: ## Run the deployment health check (JSON metrics)
	npx tsx scripts/check-contract-health.ts --pretty

verify-mainnet: ## Run mainnet-readiness verification (writes verification-report.mainnet.json)
	NETWORK=mainnet npx tsx scripts/verify-deployment.ts

publish-mainnet: ## Publish verified mainnet contract IDs/SAC addresses to README.md
	bash scripts/publish-mainnet-contracts.sh

docs: ## Generate SDK API documentation (typedoc)
	cd sdk && $(PKG) run docs

# Generate the contract ABI/spec JSON (Issue #111).
# Every `pub fn` in the `#[contractimpl]` block is exported into the Soroban
# spec automatically. When the `stellar` CLI and a built WASM are available we
# emit the canonical embedded spec; otherwise we fall back to a toolchain-free
# source generator that produces an equivalent JSON. Output: docs/contract-spec.json
spec: ## Generate the contract ABI/spec JSON (docs/contract-spec.json)
	@if command -v stellar >/dev/null 2>&1 && [ -f target/wasm32v1-none/release/invoice_liquidity.wasm ]; then \
		stellar contract inspect --wasm target/wasm32v1-none/release/invoice_liquidity.wasm --output json > docs/contract-spec.json && \
		echo "✅ spec from 'stellar contract inspect' -> docs/contract-spec.json"; \
	else \
		echo "ℹ️  stellar CLI/WASM not found; generating spec from source"; \
		npx --yes tsx scripts/gen-spec.ts; \
	fi

# Generate CHANGELOG.md from conventional commits using git-cliff.
# Install: cargo install git-cliff
# Usage:
#   make changelog            # update for unreleased commits
#   make changelog TAG=v1.0.0 # generate up to a specific tag
changelog: ## Generate CHANGELOG.md from conventional commits (git-cliff)
	git cliff $(if $(TAG),--tag $(TAG)) --output CHANGELOG.md
