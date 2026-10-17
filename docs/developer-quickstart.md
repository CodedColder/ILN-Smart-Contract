# Contract Developer Quickstart

This guide walks a new contributor through everything needed to build, test,
and deploy the ILN smart contracts from a fresh machine.

---

## Prerequisites

| Tool | Minimum version |
|------|----------------|
| Rust | 1.74 |
| Git  | any recent |
| curl | any recent |

---

## 1. Rust Toolchain Setup

### Install rustup

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Add the WASM target

Soroban contracts compile to `wasm32v1-none` (Wasm 2.0, no WASI):

```bash
rustup target add wasm32v1-none
```

### Verify

```bash
rustc --version          # should be ≥ 1.74
cargo --version
rustup target list --installed | grep wasm32v1
```

---

## 2. Stellar CLI Installation

The Stellar CLI is required for deploying and invoking contracts on-chain.

```bash
cargo install --locked stellar-cli
stellar --version
```

> **Note:** If you already have an older version installed, re-run the command
> above to upgrade.

---

## 3. Clone and Build

```bash
git clone https://github.com/Invoice-Liquidity-Network/ILN-Smart-Contract.git
cd ILN-Smart-Contract
cargo build
```

The `build` step compiles all workspace members in debug mode (no WASM output
yet) and verifies that the dependency graph resolves correctly.

---

## 4. Running Unit Tests

All unit and integration tests run on your native architecture (no WASM
required) via the `soroban-sdk` test utilities:

```bash
# Run every test in the workspace
cargo test

# Run tests for a single contract
cargo test -p invoice_liquidity
cargo test -p iln_governance
cargo test -p reputation_bonus
cargo test -p iln_distribution
```

### Useful flags

```bash
# Show stdout from tests (useful with proptest failure output)
cargo test -p invoice_liquidity -- --nocapture

# Filter by test name
cargo test -p invoice_liquidity test_update_max_discount

# Run a specific test file / module
cargo test -p invoice_liquidity tests_discount_invariants
cargo test -p invoice_liquidity governance_main_integration_test
```

### Property-based tests

The property tests (`tests_fuzz`, `tests_discount_invariants`) run thousands of
random cases and may take a minute or two.  To run only the fast suite during
development, filter them out:

```bash
cargo test -p invoice_liquidity -- --skip prop_
```

---

## 5. Running the Fuzz Suite

The `iln_fuzz` crate runs `proptest`-based fuzzing against `submit_invoice`:

```bash
cargo test -p iln_fuzz
```

---

## 6. Building WASM

Compile all contracts to optimised WASM using the workspace alias defined in
`.cargo/config.toml`:

```bash
cargo build-wasm
```

This is equivalent to:

```bash
cargo build --target wasm32v1-none --release
```

Output files appear in `target/wasm32v1-none/release/*.wasm`.

> **Tip:** The release profile enables LTO and size optimisation (`opt-level =
> "z"`); binary sizes are typically 10–80 KB per contract.

---

## 7. Deploying to Testnet

### 7.1 Configure the Stellar CLI

```bash
stellar network add \
  --global testnet \
  --rpc-url https://soroban-testnet.stellar.org \
  --network-passphrase "Test SDF Network ; September 2015"
```

### 7.2 Create and fund a test account

```bash
stellar keys generate --global alice
stellar keys address alice       # prints the public key
stellar network fund alice --network testnet   # airdrops 10 000 XLM
```

### 7.3 Upload the WASM

```bash
stellar contract upload \
  --network testnet \
  --source alice \
  --wasm target/wasm32v1-none/release/invoice_liquidity.wasm
# prints the WASM hash — save it
```

### 7.4 Deploy the contract

```bash
stellar contract deploy \
  --network testnet \
  --source alice \
  --wasm-hash <WASM_HASH_FROM_ABOVE>
# prints the contract ID — save it
```

### 7.5 Initialise the contract

Replace the placeholder addresses with real testnet contract IDs:

```bash
stellar contract invoke \
  --network testnet \
  --source alice \
  --id <CONTRACT_ID> \
  -- initialize \
  --admin <ALICE_ADDRESS> \
  --usdc_token <USDC_SAC_ADDRESS> \
  --xlm_sac   <XLM_SAC_ADDRESS>
```

> XLM and USDC Stellar Asset Contract (SAC) addresses for testnet can be found
> in the Stellar documentation or by querying the Horizon API.

---

## 8. Troubleshooting

### `error[E0463]: can't find crate for 'std'` when building WASM

You are missing the `wasm32v1-none` target.  Run:

```bash
rustup target add wasm32v1-none
```

### `error: toolchain 'stable-…' is not installed`

Install or update the default toolchain:

```bash
rustup update stable
```

### `stellar: command not found`

Ensure `~/.cargo/bin` is on your `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

Add the line to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) to persist it.

### `cargo test` hangs or is very slow

Property-based tests (`prop_*`) generate thousands of cases.  You can limit
the case count with an environment variable:

```bash
PROPTEST_CASES=100 cargo test -p invoice_liquidity
```

### `linker 'cc' not found` on Linux

Install the C build toolchain:

```bash
sudo apt-get install build-essential   # Debian / Ubuntu
sudo dnf install gcc                   # Fedora / RHEL
```

### `error: no network named 'testnet'` in Stellar CLI

Re-add the network configuration (see step 7.1).

---

## 9. Next Steps

- **Architecture overview** — [`docs/Architecture.md`](Architecture.md)
- **Contract ABI** — [`docs/contract-abi.md`](contract-abi.md)
- **Governance protocol** — [`docs/governance.md`](governance.md)
- **Reputation model** — [`docs/reputation.md`](reputation.md)
- **Storage layout** — [`docs/storage-layout.md`](storage-layout.md)
- **Threat model** — [`docs/threat-model.md`](threat-model.md)
- **Contributing guide** — [`CONTRIBUTING.md`](../CONTRIBUTING.md)

## SDK Examples
Check out the runnable examples in the `sdk/examples/` directory for full invoice, governance, and insurance workflows.
