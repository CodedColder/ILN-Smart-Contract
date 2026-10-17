# Smart Contract Benchmarks

*Date:* 2026-05-30 (invoice_liquidity), 2026-07-25 (iln_governance)

These values are baseline metrics for core contract execution: CPU instructions and memory bytes consumed via Soroban's cost meter (`env.budget()`). CI compares each run against the contract's `benchmarks/baseline.json` and emits a **warning** (not a failure) when either metric regresses by more than 10%.

## Baseline Execution Results

### `contracts/invoice_liquidity`

| Function                        | CPU Instructions | Memory (bytes) |
| ------------------------------- | ---------------- | -------------- |
| submit_invoice                  |           859421 |          26485 |
| fund_invoice                    |          1041920 |          38190 |
| mark_paid                       |           948123 |          35480 |
| join_fund_queue_max_depth       |          1124580 |          42350 |
| resolve_fund_queue_max_depth    |           582140 |          28120 |
| extension_cost_at_maximum_depth |           321850 |          16420 |

### `contracts/iln_governance`

| Function        | CPU Instructions | Memory (bytes) |
| ---------------- | ---------------- | -------------- |
| create_proposal  |            220773 |          33354 |
| cast_vote        |            252321 |          41057 |
| delegate_votes   |            182897 |          28102 |

## Re-Running Locally

```bash
cd contracts/invoice_liquidity   # or contracts/iln_governance
cargo test --target x86_64-unknown-linux-gnu benchmark -- --nocapture
```

Each benchmark test prints machine-readable lines:

```
BENCHMARK submit_invoice cpu=859421 mem=26485
```

## CI Regression Check

```bash
bash scripts/check_benchmark_regression.sh                   # checks every contract listed below
bash scripts/check_benchmark_regression.sh contracts/iln_governance:contracts/iln_governance/benchmarks/baseline.json  # checks one
```

The script always exits 0. Regressions above the threshold are reported as `::warning::` annotations in GitHub Actions. It checks `contracts/invoice_liquidity` and `contracts/iln_governance` by default.

## Updating Baselines

After an intentional optimisation or contract change:

1. Run the benchmark suite locally with `--nocapture`.
2. Update `contracts/<contract>/benchmarks/baseline.json`.
3. Update the table in this document.
