# Contract release gate (Issue #1263)

`make release-gate` (`scripts/release-gate.sh`) is the single pass/fail check
that must be green before tagging or deploying a revision that changes
contract code.

## When it applies

The gate scans `RELEASE_GATE_BASE...HEAD` (default `origin/main`) for changes
under `contracts/`, `Cargo.toml` or `Cargo.lock`.

| Situation | Behavior |
| --------- | -------- |
| No contract changes in the range | Prints "gate not required", exits 0 |
| Contract changes | Runs every step below |
| Base ref cannot be resolved (shallow clone, typo) | **Fails closed**: runs every step |
| `RELEASE_GATE_FORCE=1` | Runs every step regardless of changes |

## Steps

Every step runs even if an earlier one failed, so one run reports every
problem. The script ends with a `| Step | Result |` table.

| Step | Command | Verifies |
| ---- | ------- | -------- |
| `fmt` | `cargo fmt --all -- --check` | formatting |
| `clippy` | `cargo clippy --all-targets -- -D warnings` | lint |
| `no-unwrap` | `scripts/check-no-unwrap-in-contract-source.sh` | no new panic paths (#845) |
| `test` | `cargo test` | full workspace test suite |
| `wasm-build` | `cargo build --target wasm32v1-none --release` | release artifacts build |
| `wasm-size` | `scripts/check_wasm_size.sh` | WASM under the network limit (#1066) |
| `access-matrix` | `generate-access-control-matrix.py --check` | access matrix matches code (#856) |
| `public-docs` | `check-contract-public-docs.py` | entry-point docs (#857) |
| `abi-drift` | `gen-abi.ts` + `make spec`, then `git diff --exit-code` | committed ABI/spec docs match the code |
| `deploy-dry-run` | `scripts/deploy-mainnet.sh --dry-run` | deploy pre-flight, no transactions |

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | All steps passed, or no contract changes |
| 1 | At least one step failed: do not tag or deploy |
| 2 | Nothing failed, but steps were skipped via `RELEASE_GATE_SKIP` |

`RELEASE_GATE_SKIP="abi-drift deploy-dry-run"` skips named steps. Skips are
printed as `SKIPPED` and return exit 2, so a partial run can never be mistaken
for a pass. Repeated runs are safe: the only files the gate writes are the
regenerated ABI/spec docs, which are left in place on drift for inspection.

## Residual notes

- `abi-drift` regenerates `docs/contract-abi.md` and `docs/contract-spec.json`.
  If those were already stale before this gate existed, the first run fails
  until they are regenerated and committed. That is intended.
- The gate does not run benchmarks; see `docs/benchmarks.md` (Issue #1262)
  for why the benchmark baseline is not yet a reliable release signal.
- `scripts/__tests__/release-gate.test.mjs` tests the gate's control flow
  (scope check, fail-closed base, skip accounting) without a Rust toolchain.
