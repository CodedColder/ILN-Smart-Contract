// Issue #1263: release gate for security-sensitive contract changes.
//
// These tests exercise scripts/release-gate.sh's control flow (scope check,
// fail-closed base handling, skip accounting) without needing a Rust
// toolchain: every real step is skipped through RELEASE_GATE_SKIP, which the
// gate reports as exit 2 rather than a pass.
//
// Run: node --test scripts/__tests__/release-gate.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const SCRIPT = path.join(ROOT, 'scripts', 'release-gate.sh');
const STEPS = [
  'fmt',
  'clippy',
  'no-unwrap',
  'test',
  'wasm-build',
  'wasm-size',
  'access-matrix',
  'public-docs',
  'abi-drift',
  'deploy-dry-run',
];

function runGate(env) {
  return spawnSync('bash', [SCRIPT], {
    cwd: ROOT,
    encoding: 'utf8',
    env: { ...process.env, ...env },
  });
}

test('every step is declared in the gate script', () => {
  const src = readFileSync(SCRIPT, 'utf8');
  for (const step of STEPS) {
    assert.match(src, new RegExp(`run_step ${step} `), `missing step ${step}`);
  }
});

test('Makefile defines each target once and exposes release-gate', () => {
  const makefile = readFileSync(path.join(ROOT, 'Makefile'), 'utf8').replace(/\r/g, '');
  const targets = [...makefile.matchAll(/^([A-Za-z0-9_-]+):(?!=)/gm)].map((m) => m[1]);
  const dupes = targets.filter((t, i) => targets.indexOf(t) !== i);
  assert.deepEqual(dupes, [], `duplicate Makefile targets: ${dupes.join(', ')}`);
  assert.match(makefile, /^release-gate:.*\n\tbash scripts\/release-gate\.sh$/m);
});

test('no contract changes against the base ref: gate is not required', () => {
  const r = runGate({ RELEASE_GATE_BASE: 'HEAD', RELEASE_GATE_FORCE: '0' });
  assert.equal(r.status, 0, r.stdout + r.stderr);
  assert.match(r.stdout, /gate not required/);
});

test('unknown base ref fails closed and runs the full gate', () => {
  const r = runGate({
    RELEASE_GATE_BASE: 'refs/heads/definitely-not-a-real-branch-1263',
    RELEASE_GATE_FORCE: '0',
    RELEASE_GATE_SKIP: STEPS.join(' '),
  });
  assert.match(r.stdout, /failing closed/);
  assert.equal(r.status, 2, 'all steps skipped must not report success');
});

test('skipped steps are reported and never count as a pass', () => {
  const r = runGate({ RELEASE_GATE_FORCE: '1', RELEASE_GATE_SKIP: STEPS.join(' ') });
  assert.equal(r.status, 2, r.stdout + r.stderr);
  for (const step of STEPS) {
    assert.match(r.stdout, new RegExp(`\\| ${step} \\| SKIPPED \\|`));
  }
  assert.doesNotMatch(r.stdout, /Release gate PASSED/);
});
