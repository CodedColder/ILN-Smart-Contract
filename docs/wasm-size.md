# WASM Binary Size

This document records the optimized WASM build command for ILN smart contracts and the current size guidance for the generated binaries.

## Optimized build command

```bash
cargo build --release --target wasm32v1-none
```

The repository also exposes Makefile shortcuts:

```bash
make build            # all contracts, target/wasm32v1-none/release/*.wasm
make build-insurance  # insurance_pool only
```

(`make soroban-optimize` builds the legacy `wasm32-unknown-unknown` target
instead — kept for compatibility with older tooling, but `wasm32v1-none` is
the target every other build/deploy path in this repo uses, including
`make build`, `make deploy-testnet`, and `scripts/check_wasm_size.sh` below.)

## Current WASM size guidance

- Optimized Soroban contract binaries are expected to be small, typically in the range of 10 KB to 80 KB per contract when built with the release profile.
- Actual size should be verified after building with the command above.

## Absolute size threshold (Issue #1066)

The guidance above was previously just that — guidance, with no enforcement
and only a manual verification step. `scripts/check_wasm_size.sh` (wired
into `make check-wasm-size`, and into `make lint` as a prerequisite) now
builds each contract if needed and **fails** (non-zero exit) if any
optimized WASM exceeds `MAX_WASM_SIZE_BYTES` (default **131072 bytes / 128
KiB**, overridable via that environment variable).

**Before:** nothing failed, locally or in CI, no matter how large a
contract's compiled WASM grew — the only check was a human running `wc -c`
and eyeballing it against the "typically 10-80 KB" guidance above.

**After:** `make check-wasm-size` (and `make lint`, which now depends on
it) fails the moment any contract's optimized binary exceeds the
threshold, with a clear `::error::` line naming the contract and the
measured size.

128 KiB (131,072 bytes) was chosen as a conservative internal threshold —
roughly 2x the top of the "typical" range recorded above — before checking
it against any authoritative source. It turns out to match exactly:
`soroban-sdk` 27.0.6 bundles a snapshot of Stellar Mainnet's resource
limits (`soroban_sdk::testutils::cost_estimate::NetworkInvocationResourceLimits::mainnet()`,
dated 2026-07-10) whose `max_contract_code_entry_size_bytes` field is
`131072` — i.e. this script's default is the real current protocol-level
contract code size limit, not just an arbitrary guardrail. That snapshot
is baked into the SDK rather than pulled dynamically, so it can still
drift from the live network between SDK releases — reconcile against
`stellar network settings --network mainnet` or
<https://lab.stellar.org/network-limits> (or the network's live
`ConfigSettingContractCodeSizeV0` ledger entry) before relying on it for a
mainnet-blocking decision, and prefer upgrading `soroban-sdk` over
hand-editing the threshold if it's ever found to be stale.

**Migration note:** behavior-only addition — no contract code changed. If
any contract's current WASM already exceeds 128 KiB, `make lint` /
`make check-wasm-size` will start failing where it previously didn't; raise
`MAX_WASM_SIZE_BYTES` deliberately (with a comment explaining why) rather
than silently ignoring the failure, since the whole point of this gate is
to make that decision visible instead of invisible.

### Near-limit warning (Issue #1068)

Before a contract's WASM actually breaches `MAX_WASM_SIZE_BYTES`, the
script prints an `::warning::` (without failing) once it passes
`NEAR_LIMIT_WARNING_PERCENT` (default **70%**) of that threshold —
override with the `NEAR_LIMIT_WARNING_PERCENT` environment variable. This
is the WASM-size counterpart to the CPU/memory near-limit monitoring
described in `docs/benchmarks.md`; see that doc for why this kind of
monitoring has to live in the build/CI layer rather than on-chain.

## Verification

After building, check the size of the generated WASM file:

```bash
wc -c target/wasm32v1-none/release/<contract>.wasm
```

or

```bash
stat -c%s target/wasm32v1-none/release/<contract>.wasm
```

or, to run the enforced threshold check directly:

```bash
bash scripts/check_wasm_size.sh                    # all contracts, default threshold
bash scripts/check_wasm_size.sh invoice_liquidity   # one contract
MAX_WASM_SIZE_BYTES=262144 bash scripts/check_wasm_size.sh   # override the threshold
```
