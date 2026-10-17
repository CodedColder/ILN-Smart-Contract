#!/usr/bin/env node
/**
 * check-event-coverage.ts — automated event-coverage audit (Issue #858).
 *
 * Cross-references every state-mutating entrypoint (pub fn inside a
 * `#[contractimpl]` impl block — or any fn inside a trait impl, which
 * soroban-sdk also exports; see syn_ext.rs `impl.trait_.is_some() || vis == pub`)
 * across all five contracts against whether it publishes an event via
 * `env.events().publish()`, directly or through a same-crate helper.
 *
 * Modes:
 *   node scripts/check-event-coverage.ts --check   (default) exit 1 when a
 *       mutating entrypoint has neither an event nor a documented exemption
 *   node scripts/check-event-coverage.ts --report  print the full matrix
 *   node scripts/check-event-coverage.ts --help
 *
 * Runtime: Node >= 22.18 (native TypeScript type stripping). The script is
 * dependency-free and also runs under `npx tsx scripts/check-event-coverage.ts`.
 *
 * Tests:
 *   node --test scripts/check-event-coverage.test.ts
 *
 * Detection rules (kept deliberately simple + explicit so the output is
 * auditable):
 *   - MUTATING: the entrypoint body — or a helper it calls (resolved
 *     transitively through same-crate fn definitions, cycle-safe, depth-capped)
 *     — contains `.set(`, `.remove(`, `.transfer(`, `require_admin(` or
 *     `check_rate_limit(`.
 *   - EMITS: the entrypoint body — or a resolved helper — contains
 *     `.events().publish(`.
 *   - Read-only views (pure `.get(` / computation) classify as non-mutating
 *     and are not required to emit.
 *   - Callee resolution is module-aware: `mod::fn(...)` resolves against
 *     `src/mod.rs`, bare calls resolve via the file's `use` imports, then the
 *     same file, then a unique definition — ambiguous names are never guessed
 *     (guessing caused a false "emits" for `initialize_multisig_admin`, whose
 *     `multisig::initialize` helper was matched against the unrelated
 *     entrypoint `initialize`).
 *   - Documented non-emitting mutators live in EXEMPTIONS below, each with a
 *     reason string; reasons are mirrored in docs/events.md ("Known Gaps").
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

// ── Types ────────────────────────────────────────────────────────────────────

export interface EntryPoint {
  name: string;
  /** Body text (including outer braces) after comment stripping. */
  body: string;
  line: number;
  /** fn appeared in an `impl Trait for Type` block (all fns exported). */
  fromTraitImpl: boolean;
  /** fn is declared `pub`. */
  isPub: boolean;
}

export interface FnDef {
  name: string;
  body: string;
  file: string;
  line: number;
}

export interface CoverageRow {
  contract: string;
  fn: string;
  line: number;
  mutating: boolean;
  emits: boolean;
  mutationSignals: string[];
  /** Topic symbol strings found in publish() calls (direct + helpers). */
  eventSymbols: string[];
  /** Exemption reason when mutating && !emits, else "". */
  exemption: string;
}

export interface SourceFile {
  /** Path relative to the contract crate root (used for diagnostics). */
  path: string;
  source: string;
}

// ── Config ───────────────────────────────────────────────────────────────────

export const CONTRACTS = [
  "invoice_liquidity",
  "iln_governance",
  "iln_distribution",
  "insurance_pool",
  "reputation_bonus",
] as const;

/** Repo root, resolved from this file's location (scripts/ -> repo root). */
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(SCRIPT_DIR, "..");

/** Storage/mutation signals (issue #858 spec). */
export const MUTATION_SIGNALS: ReadonlyArray<{ name: string; re: RegExp }> = [
  { name: "storage.set", re: /\.set\s*\(/ },
  { name: "storage.remove", re: /\.remove\s*\(/ },
  { name: "token.transfer", re: /\.transfer\s*\(/ },
  { name: "admin.gate", re: /\brequire_admin\s*\(/ },
  { name: "rate.limit", re: /\bcheck_rate_limit\s*\(/ },
];

/** Event publication signal. */
export const PUBLISH_RE = /\bevents\s*\(\s*\)\s*\.publish\s*\(/;

/** Max helper-resolution hops (entrypoint -> helper -> helper's helper ...). */
export const MAX_HELPER_DEPTH = 3;

/** Path segments that carry no module information. */
const RESERVED_SEGMENTS = new Set(["crate", "self", "super", "Self"]);

/**
 * Documented non-emitting mutators. Keyed `contract/function`; every entry
 * MUST carry a real, auditable reason. These are mirrored in the
 * "Known Gaps — accepted non-emitting mutators" section of docs/events.md.
 *
 * Adding an entry here is a documentation decision, not a code fix: prefer
 * adding the event in the contract (or fixing this script) over exempting.
 */
export const EXEMPTIONS: Readonly<Record<string, string>> = {
  "invoice_liquidity/initialize_multisig_admin":
    "One-shot multisig bootstrap (signer set + threshold). Writes only multisig " +
    "config storage; every subsequent state change made through the multisig " +
    "(pause/unpause/token removal/fee changes/signer rotation) emits its own " +
    "event from execute_proposal, so the bootstrap itself adds no new " +
    "observable state transition worth an event.",
  "invoice_liquidity/propose_pause":
    "Multisig proposal bookkeeping only (stores a pending proposal). The effect " +
    "becomes observable when execute_proposal applies it and emits " +
    "ContractPaused; proposal creation is signer-gated and reversible via the " +
    "proposal expiry path.",
  "invoice_liquidity/propose_unpause":
    "Multisig proposal bookkeeping only; observable effect is the " +
    "ContractUnpaused event emitted by execute_proposal.",
  "invoice_liquidity/propose_remove_token":
    "Multisig proposal bookkeeping only; observable effect is the TokenRemoved " +
    "event emitted by execute_proposal.",
  "invoice_liquidity/propose_set_fee_rate":
    "Multisig proposal bookkeeping only; observable effect is the " +
    "ParameterUpdated event emitted by execute_proposal.",
  "invoice_liquidity/propose_set_max_discount":
    "Multisig proposal bookkeeping only; observable effect is the " +
    "ParameterUpdated event emitted by execute_proposal.",
  "invoice_liquidity/propose_update_multisig":
    "Multisig proposal bookkeeping only; signer-set changes are applied " +
    "silently by execute_proposal and are readable via get_multisig_admin " +
    "(accepted gap — no dedicated event for signer-set changes).",
  "invoice_liquidity/propose_rotate_signer":
    "Multisig proposal bookkeeping only; observable effect is the " +
    "SignerRotationScheduled event emitted by execute_proposal.",
  "invoice_liquidity/sign_proposal":
    "Records a signer's approval on a pending multisig proposal (per-proposal " +
    "signature counter). Effect is signer-gated, reversible until threshold, " +
    "and observable via the event emitted when the proposal executes.",
  "invoice_liquidity/record_twap_sample":
    "High-frequency keeper operation (Issue #859 rate-limit exemption): a " +
    "single sample is transient accumulator input, not a discrete state " +
    "transition — the aggregate effect is observable through the price reads " +
    "it feeds (and PriceOutlierRejected on outlier handling).",
  "invoice_liquidity/set_insurance_pool":
    "Admin-gated, rate-limited pointer swap; the new address is immediately " +
    "readable via get_insurance_pool, and every subsequent claim flow " +
    "references the pool through its own events (InsuranceClaimAttempted) — " +
    "accepted gap, no dedicated event.",
  "invoice_liquidity/set_max_price_deviation_bps":
    "Admin-gated oracle tuning knob (rate-limited); the threshold only " +
    "materializes on the next price read, whose PriceOutlierRejected events " +
    "expose its effect. Readable via get_max_price_deviation_bps.",
  "invoice_liquidity/set_twap_enabled":
    "Admin-gated per-feed TWAP opt-in flag (rate-limited); changes how " +
    "subsequent Price reads are computed rather than marking a discrete " +
    "transition. Readable via is_twap_enabled.",
  "invoice_liquidity/set_twap_window":
    "Admin-gated TWAP window bound (rate-limited, range-validated); affects " +
    "subsequent windowed reads only. Readable via get_twap_window_ledgers.",
  "iln_governance/set_proposal_deposit_sink":
    "Admin-gated treasury pointer on the governance contract (separate " +
    "admin gate, no shared rate-limit infra in that crate — Issue #859 " +
    "matrix); readable via get_proposal_deposit_sink and only consulted " +
    "during proposal lifecycle events that emit their own audit trail.",
  "insurance_pool/increment_default_count":
    "Internal bookkeeping counter updated as a side effect of default " +
    "processing; per-pair state is exposed by the pair views and surfaced " +
    "again by claim/payout events — no independent transition to announce.",
  "insurance_pool/set_base_premium_rate_bps":
    "Admin-gated premium tuning on the insurance pool (admin gate; " +
    "Issue #859 matrix); applies to future enrollments only with no " +
    "retroactive effect. Readable via get_base_premium_rate_bps.",
  "insurance_pool/set_risk_multiplier":
    "Admin-gated pricing multiplier on the insurance pool (admin gate; " +
    "Issue #859 matrix); applies to future enrollments only. Readable via " +
    "get_risk_multiplier_numerator/denominator.",
};

// ── Source preprocessing ─────────────────────────────────────────────────────

/**
 * Blank out `//` line comments while preserving offsets and line numbers
 * (comment characters become spaces; newlines are kept). String literals are
 * respected so URLs etc. are not treated as comments.
 */
export function stripComments(source: string): string {
  const out = source.split("");
  let inString = false;
  let i = 0;
  while (i < source.length) {
    const c = source[i];
    if (inString) {
      if (c === "\\") {
        i += 2;
        continue;
      }
      if (c === '"') inString = false;
      i++;
      continue;
    }
    if (c === '"') {
      inString = true;
      i++;
      continue;
    }
    if (c === "/" && source[i + 1] === "/") {
      while (i < source.length && source[i] !== "\n") {
        out[i] = " ";
        i++;
      }
      continue;
    }
    i++;
  }
  return out.join("");
}

/**
 * Blank out `#[cfg(test)] mod ... { ... }` blocks so mock contracts inside
 * unit-test modules are never treated as production entrypoints.
 * Offsets/line numbers are preserved (replaced by spaces).
 */
export function stripTestModules(source: string): string {
  let out = source;
  const headerRe = /#\s*\[cfg\s*\(\s*test\s*\)\]\s*(?:#\[[^\]]*\]\s*)*mod\s+[A-Za-z_]\w*\s*\{/g;
  let m: RegExpExecArray | null;
  while ((m = headerRe.exec(out)) !== null) {
    const braceStart = out.indexOf("{", m.index + m[0].length - 1);
    const end = matchBrace(out, braceStart);
    if (end < 0) break;
    out = blanked(out, m.index, end + 1);
    headerRe.lastIndex = m.index;
  }
  return out;
}

/** Replace [from, to) with spaces, preserving newlines. */
function blanked(s: string, from: number, to: number): string {
  let repl = "";
  for (let i = from; i < to; i++) repl += s[i] === "\n" ? "\n" : " ";
  return s.slice(0, from) + repl + s.slice(to);
}

/** Return index of the `}` matching the `{` at `open`, or -1. */
export function matchBrace(s: string, open: number): number {
  let depth = 0;
  let inString = false;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (inString) {
      if (c === "\\") {
        i++;
        continue;
      }
      if (c === '"') inString = false;
      continue;
    }
    if (c === '"') inString = true;
    else if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/** Return index of the `)` matching the `(` at `open`, or -1. */
function matchParen(s: string, open: number): number {
  let depth = 0;
  let inString = false;
  for (let i = open; i < s.length; i++) {
    const c = s[i];
    if (inString) {
      if (c === "\\") {
        i++;
        continue;
      }
      if (c === '"') inString = false;
      continue;
    }
    if (c === '"') inString = true;
    else if (c === "(") depth++;
    else if (c === ")") {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/** Preprocess: strip comments then test modules (both offset-preserving). */
export function preprocess(source: string): string {
  return stripTestModules(stripComments(source));
}

// ── Parsing ──────────────────────────────────────────────────────────────────

function lineOf(s: string, index: number): number {
  let line = 1;
  for (let i = 0; i < index && i < s.length; i++) if (s[i] === "\n") line++;
  return line;
}

/**
 * Extract the `{ ... }` body of a fn starting from `from` (index just after
 * the fn name). Returns [bodyStart, bodyEnd] inclusive, or null when the fn
 * has no body (trait method signature ending in `;`).
 */
function fnBody(s: string, from: number): [number, number] | null {
  let paren = 0;
  let inString = false;
  let i = from;
  for (; i < s.length; i++) {
    const c = s[i];
    if (inString) {
      if (c === "\\") {
        i++;
        continue;
      }
      if (c === '"') inString = false;
      continue;
    }
    if (c === '"') inString = true;
    else if (c === "(") paren++;
    else if (c === ")") paren--;
    else if (paren === 0 && (c === "{" || c === ";")) {
      if (c === ";") return null;
      const end = matchBrace(s, i);
      if (end < 0) return null;
      return [i, end];
    }
  }
  return null;
}

/**
 * Parse every `#[contractimpl]` impl block in `source` (already preprocessed)
 * and return its exported functions.
 *
 * soroban-sdk exports all fns of a trait impl (`impl Trait for Type`) but only
 * `pub fn`s of an inherent impl — mirrored here via
 * `fromTraitImpl || isPub`.
 */
export function parseContractimplBlocks(source: string): EntryPoint[] {
  const s = preprocess(source);
  const out: EntryPoint[] = [];
  const attrRe = /#\s*\[contractimpl\]/g;
  let m: RegExpExecArray | null;
  while ((m = attrRe.exec(s)) !== null) {
    // Skip whitespace / further attributes until `impl`.
    let j = m.index + m[0].length;
    for (;;) {
      while (j < s.length && /\s/.test(s[j])) j++;
      if (s[j] === "[") {
        const close = s.indexOf("]", j);
        if (close < 0) return out;
        j = close + 1;
        continue;
      }
      break;
    }
    if (s.slice(j, j + 4) !== "impl") continue;
    const headerEnd = s.indexOf("{", j);
    if (headerEnd < 0) continue;
    const header = s.slice(j, headerEnd);
    const isTraitImpl = /\bfor\b/.test(header);
    const blockEnd = matchBrace(s, headerEnd);
    if (blockEnd < 0) continue;

    // Walk top-level fns of the block (depth 1 relative to the impl body).
    let i = headerEnd + 1;
    let depth = 1;
    while (i < blockEnd) {
      const c = s[i];
      if (c === "{") {
        depth++;
        i++;
        continue;
      }
      if (c === "}") {
        depth--;
        i++;
        continue;
      }
      const fnMatch = /^(?:pub(?:\s*\([^)]*\))?\s+)?fn\s+([A-Za-z_]\w*)/.exec(s.slice(i));
      if (depth === 1 && fnMatch && (i === 0 || !/[\w.]/.test(s[i - 1]))) {
        const name = fnMatch[1];
        const isPub = /\bpub\s+(?:fn\b|(?:\([^)]*\)\s+)?fn\b)/.test(
          s.slice(Math.max(0, i - 40), i + fnMatch[0].length)
        );
        const body = fnBody(s, i + fnMatch[0].length);
        if (body) {
          out.push({
            name,
            body: s.slice(body[0], body[1] + 1),
            line: lineOf(s, i),
            fromTraitImpl: isTraitImpl,
            isPub,
          });
          i = body[1] + 1;
          continue;
        }
      }
      i++;
    }
  }
  return out;
}

/** Parse every fn definition (any visibility, nested or not) in a source. */
export function parseAllFns(source: string, file = "src/lib.rs"): FnDef[] {
  const s = preprocess(source);
  const out: FnDef[] = [];
  const re = /(?:^|[^\w.])(?:pub(?:\s*\([^)]*\))?\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)/g;
  let m: RegExpExecArray | null;
  let cursor = 0;
  while ((m = re.exec(s)) !== null) {
    if (m.index < cursor) {
      re.lastIndex = cursor;
      continue;
    }
    const nameIdx = m.index + m[0].length - m[1].length;
    const body = fnBody(s, m.index + m[0].length);
    if (!body) continue;
    out.push({ name: m[1], body: s.slice(body[0], body[1] + 1), file, line: lineOf(s, nameIdx) });
    cursor = body[1] + 1;
    re.lastIndex = cursor;
  }
  return out;
}

// ── Signal detection ─────────────────────────────────────────────────────────

/** Names of mutation signals present directly in `body`. */
export function mutationSignals(body: string): string[] {
  const hits: string[] = [];
  for (const sig of MUTATION_SIGNALS) if (sig.re.test(body)) hits.push(sig.name);
  return hits;
}

/** Whether `body` publishes an event directly. */
export function publishesDirectly(body: string): boolean {
  return PUBLISH_RE.test(body);
}

/**
 * Extract topic symbol strings from every publish() call in `body`. The first
 * argument (topics tuple) is located with bracket-depth tracking so nested
 * commas — e.g. `Symbol::new(&env, "paused")` — do not truncate it.
 */
export function publishSymbols(body: string): string[] {
  const symbols: string[] = [];
  const re = /\.events\s*\(\s*\)\s*\.publish\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body)) !== null) {
    const open = m.index + m[0].length - 1;
    const close = matchParen(body, open);
    if (close < 0) continue;
    let depth = 0;
    let comma = -1;
    for (let i = open + 1; i < close; i++) {
      const c = body[i];
      if (c === "(" || c === "[" || c === "{") depth++;
      else if (c === ")" || c === "]" || c === "}") depth--;
      else if (c === "," && depth === 0) {
        comma = i;
        break;
      }
    }
    const firstArg = body.slice(open + 1, comma > 0 ? comma : close);
    const strRe = /"([^"]+)"/g;
    let sm: RegExpExecArray | null;
    while ((sm = strRe.exec(firstArg)) !== null) symbols.push(sm[1]);
  }
  return symbols;
}

/** A function call site found in a body (method calls `.foo()` excluded). */
export interface CalleeRef {
  /** Normalised path as written, e.g. `crate::storage::set_insurance_pool`. */
  path: string;
  /** Final segment — the callee's function name. */
  name: string;
  /** Module hint: second-to-last segment, unless it is crate/self/super/Self. */
  module: string | null;
}

/** Callee references in `body` (path calls only; `.method(` excluded). */
export function callees(body: string): CalleeRef[] {
  const out = new Map<string, CalleeRef>();
  const re = /(^|[^.\w])([A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*)\s*\(/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body)) !== null) {
    const path = m[2].replace(/\s+/g, "");
    const segments = path.split("::");
    const name = segments[segments.length - 1]!;
    let module: string | null = null;
    if (segments.length >= 2) {
      const cand = segments[segments.length - 2]!;
      if (!RESERVED_SEGMENTS.has(cand)) module = cand;
    }
    if (!out.has(path)) out.set(path, { path, name, module });
  }
  return [...out.values()];
}

/**
 * Map of `local name -> module` for one source file, derived from its `use`
 * statements so bare calls like `emit_config_set(...)` resolve to
 * `events.rs` instead of being guessed by name alone.
 */
export function parseUseMap(source: string): Map<string, string> {
  const s = stripComments(source);
  const map = new Map<string, string>();
  const re = /\buse\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*(?:::\{([^{}]*)\})?\s*;/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    const path = m[1]!.replace(/\s+/g, "");
    const items = m[2];
    const segs = path.split("::");
    if (items === undefined) {
      if (segs.length < 2) continue; // `use foo;` imports a module, not a fn
      const mod = segs[segs.length - 2]!;
      if (RESERVED_SEGMENTS.has(mod)) continue;
      map.set(segs[segs.length - 1]!, mod);
      continue;
    }
    const mod = segs[segs.length - 1]!;
    if (RESERVED_SEGMENTS.has(mod)) continue;
    for (const raw of items.split(",")) {
      let item = raw.trim();
      if (!item) continue;
      const alias = /\s+as\s+([A-Za-z_]\w*)$/.exec(item);
      if (alias) item = item.slice(0, alias.index).trim();
      const isegs = item.split("::").filter(Boolean);
      const local = isegs[isegs.length - 1];
      if (local) map.set(local, mod);
    }
  }
  return map;
}

/** Module name a source file belongs to (`src/foo.rs` -> `foo`). */
export function fileModule(file: string): string {
  const base = path.basename(file, ".rs");
  return base === "mod" ? path.basename(path.dirname(file)) : base;
}

/**
 * Resolve a call site to a same-crate fn definition. Never guesses when a
 * bare name has multiple candidates across files: ambiguity resolves to
 * `undefined` (a false "missing signal", caught by --check), whereas a wrong
 * guess can silently mark a non-emitting mutator as covered.
 */
export function resolveCallee(
  ref: CalleeRef,
  callerFile: string,
  selfKey: string,
  fnIndex: Map<string, FnDef[]>,
  useMaps: Map<string, Map<string, string>>
): FnDef | undefined {
  const defs = fnIndex.get(ref.name) ?? [];
  if (defs.length === 0) return undefined;
  const pool = defs.filter((d) => `${d.file}:${d.line}` !== selfKey);
  if (pool.length === 0) return undefined;

  let module = ref.module;
  if (!module) {
    const imported = useMaps.get(callerFile)?.get(ref.name);
    if (imported && !RESERVED_SEGMENTS.has(imported)) module = imported;
  }
  if (module) {
    const hit = pool.find((d) => fileModule(d.file) === module);
    if (hit) return hit;
  }
  const sameFile = pool.filter((d) => d.file === callerFile);
  if (sameFile.length === 1) return sameFile[0];
  if (pool.length === 1) return pool[0];
  return undefined;
}

export interface Analysis {
  mutating: boolean;
  emits: boolean;
  mutationSignals: string[];
  eventSymbols: string[];
  /** Helper names that contributed signals (for --report explainability). */
  viaHelpers: string[];
}

/**
 * Classify one entrypoint: resolve helpers transitively (module/use-aware
 * same-crate fn index, cycle-safe, depth-capped) and collect mutation/event
 * signals.
 */
export function analyzeEntryPoint(
  entry: EntryPoint,
  fnIndex: Map<string, FnDef[]>,
  useMaps: Map<string, Map<string, string>>,
  selfFile: string,
  selfLine: number,
  depth = MAX_HELPER_DEPTH,
  seen: Set<string> = new Set(),
  via: string[] = []
): Analysis {
  const directMut = mutationSignals(entry.body);
  const directEmit = publishesDirectly(entry.body);
  const directSymbols = publishSymbols(entry.body);
  const analysis: Analysis = {
    mutating: directMut.length > 0,
    emits: directEmit,
    mutationSignals: [...directMut],
    eventSymbols: [...directSymbols],
    viaHelpers: [...via],
  };
  if (depth === 0) return analysis;

  const selfKey = `${selfFile}:${selfLine}`;
  for (const ref of callees(entry.body)) {
    const def = resolveCallee(ref, selfFile, selfKey, fnIndex, useMaps);
    if (!def) continue;
    const defKey = `${def.file}:${def.line}`;
    if (seen.has(defKey)) continue;
    seen.add(defKey);

    const mut = mutationSignals(def.body);
    if (mut.length > 0) {
      analysis.mutating = true;
      for (const s of mut) if (!analysis.mutationSignals.includes(s)) analysis.mutationSignals.push(s);
      if (!analysis.viaHelpers.includes(ref.name)) analysis.viaHelpers.push(ref.name);
    }
    if (publishesDirectly(def.body)) {
      analysis.emits = true;
      if (!analysis.viaHelpers.includes(ref.name)) analysis.viaHelpers.push(ref.name);
      for (const s of publishSymbols(def.body)) analysis.eventSymbols.push(s);
    }
    if ((!analysis.mutating || !analysis.emits) && depth > 1) {
      const sub = analyzeEntryPoint(
        { ...entry, body: def.body },
        fnIndex,
        useMaps,
        def.file,
        def.line,
        depth - 1,
        seen,
        analysis.viaHelpers
      );
      if (sub.mutating) {
        analysis.mutating = true;
        for (const s of sub.mutationSignals)
          if (!analysis.mutationSignals.includes(s)) analysis.mutationSignals.push(s);
      }
      if (sub.emits) {
        analysis.emits = true;
        for (const s of sub.eventSymbols) analysis.eventSymbols.push(s);
      }
      analysis.viaHelpers = sub.viaHelpers;
    }
  }
  return analysis;
}

// ── Coverage analysis ────────────────────────────────────────────────────────

/** Analyse all entrypoints of one contract crate from its source files. */
export function analyzeContract(contract: string, files: SourceFile[]): CoverageRow[] {
  const fnIndex = new Map<string, FnDef[]>();
  const useMaps = new Map<string, Map<string, string>>();
  for (const f of files) {
    useMaps.set(f.path, parseUseMap(f.source));
    for (const def of parseAllFns(f.source, f.path)) {
      const list = fnIndex.get(def.name) ?? [];
      list.push(def);
      fnIndex.set(def.name, list);
    }
  }

  const rows: CoverageRow[] = [];
  for (const f of files) {
    for (const entry of parseContractimplBlocks(f.source)) {
      if (!(entry.fromTraitImpl || entry.isPub)) continue; // internal impl helper
      const analysis = analyzeEntryPoint(entry, fnIndex, useMaps, f.path, entry.line);
      const key = `${contract}/${entry.name}`;
      const exempt = EXEMPTIONS[key] ?? "";
      rows.push({
        contract,
        fn: entry.name,
        line: entry.line,
        mutating: analysis.mutating,
        emits: analysis.emits,
        mutationSignals: analysis.mutationSignals,
        eventSymbols: [...new Set(analysis.eventSymbols)],
        exemption: analysis.mutating && !analysis.emits ? exempt : "",
      });
    }
  }
  rows.sort((a, b) => (a.contract === b.contract ? a.fn.localeCompare(b.fn) : contractOrder(a.contract) - contractOrder(b.contract)));
  return rows;
}

function contractOrder(name: string): number {
  const i = (CONTRACTS as readonly string[]).indexOf(name);
  return i < 0 ? CONTRACTS.length : i;
}

/** Violations: mutating, no event, no exemption. */
export function violations(rows: CoverageRow[]): CoverageRow[] {
  return rows.filter((r) => r.mutating && !r.emits && !r.exemption);
}

/** Exemptions whose function no longer (or never) matches the trigger. */
export function staleExemptions(rows: CoverageRow[], exemptions = EXEMPTIONS): string[] {
  const keys = new Set(rows.map((r) => `${r.contract}/${r.fn}`));
  const covered = new Set(
    rows.filter((r) => r.mutating && !r.emits && r.exemption).map((r) => `${r.contract}/${r.fn}`)
  );
  return Object.keys(exemptions)
    .filter((k) => !keys.has(k) || !covered.has(k))
    .sort();
}

// ── Repo I/O ─────────────────────────────────────────────────────────────────

/** Recursively list `.rs` files below `dir`, skipping test modules. */
export function listContractSources(dir: string): string[] {
  const out: string[] = [];
  const walk = (d: string) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith(".rs") && !e.name.startsWith("test")) out.push(p);
    }
  };
  if (fs.existsSync(dir)) walk(dir);
  return out.sort();
}

/** Read every contract crate's sources (test files/modules excluded). */
export function loadContracts(root = REPO_ROOT): Map<string, SourceFile[]> {
  const all = new Map<string, SourceFile[]>();
  for (const contract of CONTRACTS) {
    const dir = path.join(root, "contracts", contract, "src");
    const files: SourceFile[] = listContractSources(dir).map((p) => ({
      path: path.relative(dir, p),
      source: fs.readFileSync(p, "utf8"),
    }));
    all.set(contract, files);
  }
  return all;
}

/** Full matrix across all five contracts, in stable order. */
export function buildReport(root = REPO_ROOT): CoverageRow[] {
  const rows: CoverageRow[] = [];
  for (const [contract, files] of loadContracts(root)) {
    rows.push(...analyzeContract(contract, files));
  }
  rows.sort((a, b) => (a.contract === b.contract ? a.fn.localeCompare(b.fn) : contractOrder(a.contract) - contractOrder(b.contract)));
  return rows;
}

// ── Output ───────────────────────────────────────────────────────────────────

function pad(s: string, n: number): string {
  return s.length >= n ? s : s + " ".repeat(n - s.length);
}

/** Render the human-readable matrix (stable column widths, stable rows). */
export function renderTable(rows: CoverageRow[]): string {
  const header = ["Contract", "Function", "Mutating", "Event", "Signals / exemption"];
  const cells = rows.map((r) => {
    let notes = "";
    if (r.mutating && !r.emits && r.exemption) notes = `EXEMPT: ${r.exemption}`;
    else if (r.mutating && !r.emits) notes = "MISSING EVENT";
    else if (!r.mutating) notes = "view (no mutation)";
    else notes = r.mutationSignals.join("+") + (r.eventSymbols.length ? ` -> ${r.eventSymbols.join(",")}` : "");
    return [r.contract, r.fn, r.mutating ? "yes" : "no", r.emits ? "yes" : "no", notes];
  });
  const widths = header.map((h, i) =>
    Math.min(Math.max(h.length, ...cells.map((c) => c[i].length)), i === 4 ? 64 : 34)
  );
  const line = (c: string[]) => c.map((v, i) => pad(v, widths[i]!)).join("  ").trimEnd();
  const sep = widths.map((w) => "-".repeat(w)).join("  ");
  return [line(header), sep, ...cells.map(line)].join("\n");
}

// ── CLI ──────────────────────────────────────────────────────────────────────

const USAGE = `Usage: node scripts/check-event-coverage.ts [--check | --report] [--help]

  --check   (default) exit 1 if any mutating entrypoint lacks an event and
            an exemption; also exits 1 on stale exemptions
  --report  print the full fn | contract | mutating | event | notes matrix
  --help    show this message
`;

export function main(argv: string[]): number {
  const mode = argv.find((a) => a === "--report" || a === "--check" || a === "--help" || a === "-h");
  if (mode === "--help" || mode === "-h") {
    process.stdout.write(USAGE);
    return 0;
  }

  const rows = buildReport();
  const viol = violations(rows);
  const stale = staleExemptions(rows);

  if (mode === "--report") {
    process.stdout.write(renderTable(rows) + "\n");
    const mutating = rows.filter((r) => r.mutating).length;
    const emitting = rows.filter((r) => r.mutating && r.emits).length;
    const exempt = rows.filter((r) => r.exemption).length;
    process.stdout.write(
      `\n${rows.length} entrypoints | ${mutating} mutating | ${emitting} emit | ` +
        `${exempt} exempt | ${viol.length} violating\n`
    );
    if (stale.length) {
      process.stdout.write(`Stale exemptions (no longer triggered): ${stale.join(", ")}\n`);
    }
    return viol.length > 0 || stale.length > 0 ? 1 : 0;
  }

  // --check
  if (viol.length === 0 && stale.length === 0) {
    const mutating = rows.filter((r) => r.mutating).length;
    const exempt = rows.filter((r) => r.exemption).length;
    process.stdout.write(
      `event-coverage: OK — ${mutating} mutating entrypoints all publish an event ` +
        `(${exempt} documented exemptions), ${rows.length} entrypoints checked.\n`
    );
    return 0;
  }
  process.stderr.write(`event-coverage: FAILED\n`);
  if (viol.length) {
    process.stderr.write(`\nMutating entrypoints with no event and no exemption:\n`);
    for (const r of viol) {
      process.stderr.write(
        `  - ${r.contract}::${r.fn} (line ${r.line}) signals=[${r.mutationSignals.join(", ")}]\n`
      );
    }
  }
  if (stale.length) {
    process.stderr.write(`\nStale exemptions (function absent or no longer triggers):\n`);
    for (const k of stale) process.stderr.write(`  - ${k}\n`);
  }
  process.stderr.write(
    `\nAdd the missing event in the contract, document a reason in EXEMPTIONS ` +
      `(scripts/check-event-coverage.ts) and docs/events.md, or fix the classifier.\n`
  );
  return 1;
}

// Run only when executed directly (not when imported by tests).
const invokedDirectly =
  process.argv[1] !== undefined &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  process.exit(main(process.argv.slice(2)));
}
