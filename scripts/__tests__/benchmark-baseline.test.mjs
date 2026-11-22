// Issue #1262: audit benchmark fixtures for realistic state size.
//
// Static consistency checks between the recorded baseline, the benchmark
// source that is supposed to produce it, the CI comparison script and the
// published docs. None of these need a Rust toolchain, so they run on every
// PR and catch a baseline that drifts away from the code that measures it.
//
// Run: node --test scripts/__tests__/benchmark-baseline.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (p) => readFileSync(path.join(ROOT, p), 'utf8').replace(/\r/g, '');

const BASELINE_PATH = 'contracts/invoice_liquidity/benchmarks/baseline.json';
const BENCH_SRC = 'contracts/invoice_liquidity/src/tests_benchmarks.rs';
const LIB_SRC = 'contracts/invoice_liquidity/src/lib.rs';
const CONSTANTS_SRC = 'contracts/invoice_liquidity/src/constants.rs';

const baseline = JSON.parse(read(BASELINE_PATH)).benchmarks;
const benchSrc = read(BENCH_SRC);

function rustConst(src, name) {
  const m = src.match(new RegExp(`const ${name}: u64 = ([0-9_ *]+);`));
  assert.ok(m, `${name} not found`);
  return m[1]
    .replace(/_/g, '')
    .split('*')
    .map((x) => Number(x.trim()))
    .reduce((a, b) => a * b, 1);
}

test('every baseline entry is produced by a named measure() call', () => {
  for (const name of Object.keys(baseline)) {
    assert.match(
      benchSrc,
      new RegExp(`measure\\([^,]+,\\s*"${name}"`),
      `${name} is in baseline.json but no benchmark measures it`,
    );
  }
});

test('baseline values are positive integers within the absolute ceilings', () => {
  const maxCpu = rustConst(benchSrc, 'MAX_CPU_INSTRUCTIONS');
  const maxMem = rustConst(benchSrc, 'MAX_MEMORY_BYTES');
  const script = read('scripts/check_benchmark_regression.sh');
  assert.match(script, new RegExp(`MAX_CPU_INSTRUCTIONS:-${maxCpu}\\}`), 'script/Rust CPU ceiling mismatch');
  assert.match(script, new RegExp(`MAX_MEMORY_BYTES:-${maxMem}\\}`), 'script/Rust memory ceiling mismatch');
  for (const [name, { cpu, mem }] of Object.entries(baseline)) {
    assert.ok(Number.isInteger(cpu) && cpu > 0, `${name}.cpu`);
    assert.ok(Number.isInteger(mem) && mem > 0, `${name}.mem`);
    assert.ok(cpu <= maxCpu, `${name}.cpu exceeds MAX_CPU_INSTRUCTIONS`);
    assert.ok(mem <= maxMem, `${name}.mem exceeds MAX_MEMORY_BYTES`);
  }
});

test('max-depth fixtures use the real queue bound, not a hard-coded size', () => {
  const constants = read(CONSTANTS_SRC);
  assert.match(constants, /pub const MAX_FUND_QUEUE_LEN: u32 = \d+;/);
  for (const name of ['join_fund_queue_max_depth', 'resolve_fund_queue_max_depth', 'extension_cost_at_maximum_depth']) {
    const start = benchSrc.indexOf(`fn benchmark_${name}()`);
    assert.ok(start >= 0, `benchmark_${name} missing`);
    const body = benchSrc.slice(start, benchSrc.indexOf('\n}\n', start));
    assert.match(body, /MAX_FUND_QUEUE_LEN/, `${name} must fill the queue to MAX_FUND_QUEUE_LEN`);
  }
});

test('docs/benchmarks.md table matches baseline.json', () => {
  const docs = read('docs/benchmarks.md');
  for (const [name, { cpu, mem }] of Object.entries(baseline)) {
    assert.match(
      docs,
      new RegExp(`\\|\\s*${name}\\s*\\|\\s*${cpu}\\s*\\|\\s*${mem}\\s*\\|`),
      `docs/benchmarks.md row for ${name} does not match baseline.json`,
    );
  }
});

// Verified finding (#1262): the benchmark module is not compiled into the
// test crate, so CI never produces BENCHMARK lines and the baseline above is
// unverified. This stays a visible TODO until the module is repaired and
// registered; flip it to a hard assertion at that point.
test('tests_benchmarks is registered in lib.rs', { todo: !/^\s*mod tests_benchmarks;/m.test(read(LIB_SRC)) }, () => {
  assert.match(read(LIB_SRC), /^\s*mod tests_benchmarks;/m);
});
