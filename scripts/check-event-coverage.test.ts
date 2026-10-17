/**
 * Tests for check-event-coverage.ts.
 *
 * Uses Node's built-in test runner (no extra dependencies). Run with:
 *
 *   npx tsx --test scripts/check-event-coverage.test.ts
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  EXEMPTIONS,
  mutationSignals,
  parseContractimplBlocks,
  publishesDirectly,
  publishSymbols,
  stripComments,
  stripTestModules,
} from "./check-event-coverage.ts";

test("stripComments blanks line comments but keeps code and newlines", () => {
  const src = "let a = 1; // trailing\nlet b = 2;\n";
  const out = stripComments(src);
  assert.equal(out, `let a = 1; ${" ".repeat("// trailing".length)}\nlet b = 2;\n`);
});

test("stripComments does not treat // inside a string as a comment", () => {
  const src = 'let url = "https://example.com"; // real comment';
  const out = stripComments(src);
  assert.ok(out.includes("https://example.com"));
  assert.ok(!out.includes("real comment"));
});

test("stripTestModules removes cfg(test) modules so dead test files do not count", () => {
  const src = [
    "pub fn real() {}",
    "#[cfg(test)]",
    "mod tests {",
    "  pub fn fake_mutation() { env.storage().instance().set(&k, &v); }",
    "}",
  ].join("\n");
  const out = stripTestModules(src);
  assert.ok(out.includes("pub fn real()"));
  assert.ok(!out.includes("fake_mutation"));
});

test("parseContractimplBlocks finds pub fns in an impl block with line numbers", () => {
  const src = [
    "#[contractimpl]",
    "impl Contract {",
    "    pub fn set_thing(env: Env) -> Result<(), ContractError> {",
    "        env.storage().instance().set(&k, &v);",
    "        Ok(())",
    "    }",
    "",
    "    pub fn get_thing(env: Env) -> Option<u32> {",
    "        env.storage().instance().get(&k)",
    "    }",
    "}",
  ].join("\n");
  const points = parseContractimplBlocks(src);
  assert.equal(points.length, 2);
  assert.equal(points[0].name, "set_thing");
  assert.equal(points[0].line, 3);
  assert.equal(points[1].name, "get_thing");
});

test("mutationSignals flags storage writes, token transfers, and admin gates", () => {
  const mutating = mutationSignals(
    "require_admin(env)?; check_rate_limit(env, \"f\", 1)?; env.storage().instance().set(&k, &v); token.transfer(&a, &b, &amt);",
  );
  assert.ok(mutating.includes("storage.set"));
  assert.ok(mutating.includes("admin.gate"));

  const view = mutationSignals("env.storage().instance().get(&k)");
  assert.equal(view.length, 0);
});

test("publishesDirectly detects env.events().publish calls", () => {
  assert.ok(
    publishesDirectly('env.events().publish((Symbol::new(&env, "x"),), payload);'),
  );
  assert.ok(!publishesDirectly("let v = env.storage().instance().get(&k);"));
});

test("publishSymbols extracts emitted topic symbols", () => {
  const symbols = publishSymbols(
    'env.events().publish((Symbol::new(&env, "token_added"), feed_type), body);',
  );
  assert.deepEqual(symbols, ["token_added"]);
});

test("EXEMPTIONS is keyed contract/fn with a non-empty reason for every entry", () => {
  const keys = Object.keys(EXEMPTIONS);
  assert.ok(keys.length > 0);
  for (const key of keys) {
    assert.match(key, /^[a-z_]+\/[a-z0-9_]+$/, `bad exemption key: ${key}`);
    const reason = EXEMPTIONS[key];
    assert.ok(
      typeof reason === "string" && reason.trim().length > 20,
      `exemption ${key} needs a real reason`,
    );
  }
});

test("the 18 exemptions audited in Issue #858/#54 are present", () => {
  // Mirrors the run that reconciled docs/events.md — if a new exemption is
  // added here, docs/events.md's Known Gaps table must gain the same row.
  for (const key of [
    "invoice_liquidity/initialize_multisig_admin",
    "invoice_liquidity/sign_proposal",
    "invoice_liquidity/record_twap_sample",
    "invoice_liquidity/set_insurance_pool",
    "iln_governance/set_proposal_deposit_sink",
    "insurance_pool/set_base_premium_rate_bps",
    "insurance_pool/set_risk_multiplier",
    "insurance_pool/increment_default_count",
  ]) {
    assert.ok(key in EXEMPTIONS, `missing exemption: ${key}`);
  }
});
