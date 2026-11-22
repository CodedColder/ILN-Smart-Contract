# =============================================================================
# ILN Smart Contract — Developer Makefile
# =============================================================================
# Run `make help` for every target. Each target is defined exactly once: an
# earlier copy of this file was concatenated with a second, newer copy, so
# GNU make printed "overriding recipe" warnings and silently used whichever
# definition came last (Issue #1263).
# =============================================================================

.PHONY: help install build build-rust build-sdk test test-rust test-e2e fuzz test-fuzz test-formal lint \
        fmt fmt-check clippy deploy-testnet seed reset reset-testnet docs spec \
        changelog soroban-optimize health test-insurance test-invoice test-governance test-distribution \
        check-wasm-size check-benchmarks check-no-unwrap event-coverage release-gate coverage clean


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

lint: fmt-check clippy check-no-unwrap event-coverage check-wasm-size ## Lint everything (rustfmt check + clippy + panic-path gate + event coverage + WASM size gate)

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

check-wasm-size: ## Gate: fail if any contract's optimized WASM exceeds the size threshold (#1066)
	bash scripts/check_wasm_size.sh

check-benchmarks: ## Run execution-cost benchmarks and report against the recorded baseline (#76, warning-only)
	bash scripts/check_benchmark_regression.sh

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

# Release gate for security-sensitive contract changes (Issue #1263).
# Runs format, clippy, panic-path, tests, WASM build + size, ABI/access-matrix
# drift and the mainnet deploy dry-run, and exits non-zero if any step fails.
# See docs/release-gate.md for the step list, failure behavior and overrides.
release-gate: ## Gate a release: lint + tests + ABI drift + WASM size + deploy dry-run (#1263)
	bash scripts/release-gate.sh

coverage: ## Run cargo-tarpaulin and produce an HTML coverage report (needs cargo-tarpaulin)
	cargo tarpaulin \
		--target x86_64-unknown-linux-gnu \
		--workspace \
		--exclude iln_fuzz \
		--out Html \
		--output-dir coverage/

clean: ## Remove all build artefacts
	cargo clean
