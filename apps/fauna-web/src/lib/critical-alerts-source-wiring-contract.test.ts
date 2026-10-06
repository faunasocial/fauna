// The critical-alerts source-wiring contract, asserted as a general
// invariant over the source rather than as one more hand-picked outcome
// (testing.md convention 17).
//
// ── What this covers, and why it can't be a runtime test ───────────────────
//
// `critical-alerts.ts`'s aggregation layer merges alerts from every wasm
// chunk that hosts a feeder registry. Each such chunk wires itself in via
// `wireCriticalAlertsSource(...)` from inside its OWN loader's memoized
// `wasmInit = (async () => { ... })()` body (`wasm.ts::ensureWasm`,
// `wasm-atproto-settings.ts::ensureAtprotoSettingsWasm` — the same
// memoized-init-promise discipline `wasm-loader-guard.test.ts` pins the
// ADOPTION of). That placement is load-bearing, not incidental: the
// memoized promise is what makes `mod.default()` run exactly once per chunk
// load, and a source-wiring call anywhere else loses that guarantee —
// wired from a bare top-level call or a page-level `$effect`, it could
// re-register a duplicate source on every re-render (every `active()` call
// then double-counts that chunk's alerts) or, if placed after some other
// early return, never fire at all.
//
// `wasm-loader-guard.test.ts`'s production red-verify (its last test) only
// checks that a `wasmInit` variable and a reset-on-failure arm exist
// somewhere in the file — it cannot see WHERE inside a passing file a given
// call sits. A refactor that moved `wireCriticalAlertsSource(` out of the
// memoized body (into a sibling top-level statement, say) would leave that
// test green while breaking exactly the property this file exists to
// pin — verified by mutation-testing the claim below.
//
// Neither `ensureWasm()` nor `ensureAtprotoSettingsWasm()` can be driven for
// real under `deno test`: both import `$app/paths` (SvelteKit/Vite-only,
// `wasm-loader-guard.test.ts`'s own header explains why no loader is both
// import-map-free and Deno-safe), and there is no e2e hook that reads
// `critical-alerts.ts`'s module-private `sources` array either — the
// closest thing, `test_atproto_custody_alarm.py`'s row-count check, is a
// diagnostic, not an assertion. So — like every contract in this family —
// this reads the loader files as TEXT.
//
// ── Discovery, not a hand list ───────────────────────────────────────────
//
// Callers are found by scanning `src/lib/` for the STRIPPED call text,
// never hand-listed: a hand list is exactly the shape that let
// `wasm-loader-guard.test.ts`'s own filing claim "zero test hits"
// for this class while a doc comment mentioning the function sat three
// lines away in the same file (a reviewer caught it: one
// doc-comment mention, not zero). A future feeder chunk that wires a
// source is swept into this contract's census automatically instead of
// silently sitting outside it.

import { stripComments } from "./source-contract.ts";

const LIB_DIR = new URL(".", import.meta.url);

/** The function's own home — excluded from the caller scan below (its
 *  `export function wireCriticalAlertsSource(...)` declaration would
 *  otherwise match the call regex). */
const DEFINITION_FILE = "critical-alerts.ts";

/** Every `.ts` file under `src/lib/` (recursively, test files excluded)
 *  whose stripped source calls `wireCriticalAlertsSource(` — a real call,
 *  never the import line (`import { wireCriticalAlertsSource, ... }` has no
 *  `(` after the name) and never a doc comment mentioning it by name
 *  (stripped first, so prose can't satisfy or evade this). */
function findCallerFiles(dir: URL = LIB_DIR): string[] {
  const out: string[] = [];
  for (const entry of Deno.readDirSync(dir)) {
    if (entry.isDirectory) {
      out.push(...findCallerFiles(new URL(`${entry.name}/`, dir)));
      continue;
    }
    if (!entry.isFile || !entry.name.endsWith(".ts") || entry.name.endsWith(".test.ts")) continue;
    if (entry.name === DEFINITION_FILE) continue;
    const url = new URL(entry.name, dir);
    const src = stripComments(Deno.readTextFileSync(url));
    if (/\bwireCriticalAlertsSource\s*\(/.test(src)) out.push(url.href);
  }
  return out;
}

/** Byte ranges, in STRIPPED `src`, of every `wasmInit = (async () => {
 *  ... })()` body — the loader's own memoized-init-promise IIFE. Crude on
 *  purpose, like every contract in this family: a body's end is its own
 *  literal `})()` sequence, the first one after the async arrow's opening —
 *  good enough for the one shape every loader in this SPA actually uses
 *  (`wasm.ts`, `wasm-atproto-settings.ts`), not an adversary. */
function memoizedInitBodies(strippedSrc: string): Array<[number, number]> {
  const ranges: Array<[number, number]> = [];
  for (const m of strippedSrc.matchAll(/\bwasmInit\s*=\s*\(async\s*\(\)\s*=>\s*\{/g)) {
    const start = m.index! + m[0].length;
    const end = strippedSrc.indexOf("})()", start);
    if (end >= 0) ranges.push([start, end]);
  }
  return ranges;
}

/** Character offsets of every `wireCriticalAlertsSource(` call in `strippedSrc`
 *  that does NOT fall inside one of that file's memoized-init bodies. */
function callsOutsideMemoizedInit(strippedSrc: string): number[] {
  const bodies = memoizedInitBodies(strippedSrc);
  const violations: number[] = [];
  for (const m of strippedSrc.matchAll(/\bwireCriticalAlertsSource\s*\(/g)) {
    const at = m.index!;
    if (!bodies.some(([s, e]) => at >= s && at < e)) violations.push(at);
  }
  return violations;
}

Deno.test("critical-alerts source wiring — callers are discovered, not hand-listed", () => {
  const callers = findCallerFiles();
  if (callers.length === 0) {
    throw new Error(
      "no file under src/lib/ calls wireCriticalAlertsSource( — either the " +
        "directory scan above is broken (fix it before trusting the next " +
        "test) or every feeder chunk stopped wiring a source, which would " +
        "itself be a critical-alerts regression worth its own investigation.",
    );
  }
});

Deno.test("critical-alerts source wiring — every call sits inside its loader's memoized init", () => {
  const violations: string[] = [];
  for (const fileUrl of findCallerFiles()) {
    const stripped = stripComments(Deno.readTextFileSync(new URL(fileUrl)));
    const bad = callsOutsideMemoizedInit(stripped);
    if (bad.length > 0) {
      violations.push(`${fileUrl} (${bad.length} call(s) outside the memoized init)`);
    }
  }
  if (violations.length > 0) {
    throw new Error(
      "wireCriticalAlertsSource( is called outside its loader's memoized " +
        "`wasmInit = (async () => { ... })()` body:\n" +
        `${violations.join("\n")}\n` +
        "Wired inside that body, the call runs exactly once per chunk load — " +
        "the same guarantee the memoized-init-promise discipline gives " +
        "mod.default() itself. Wired outside it, a caller could register a " +
        "duplicate source on every re-render, or never fire at all — either " +
        "way silently, since wasm-loader-guard.test.ts's production " +
        "red-verify only checks that a `wasmInit` variable and its " +
        "reset-on-failure arm exist somewhere in the file, not where inside " +
        "it this call sits. Move the call inside the IIFE, right after the " +
        "chunk import resolves — see wasm-atproto-settings.ts's " +
        "ensureAtprotoSettingsWasm for the shape.",
    );
  }
});

// Guard the guard, red-verified inline: feed the matcher the exact shapes it
// exists to catch (and the one it must allow), so a regex edit that stops
// matching fails HERE rather than passing the tests above vacuously — the
// same discipline `manager-gate-contract.test.ts`'s self-check applies.
Deno.test("the memoized-init-body matcher itself still fires", () => {
  const cases: Array<[string, number]> = [
    [
      // The real shape: wired from inside the IIFE.
      "let wasmInit = null;\n" +
        "function ensureXWasm() {\n" +
        "  if (!wasmInit) {\n" +
        "    wasmInit = (async () => {\n" +
        "      wireCriticalAlertsSource({});\n" +
        "    })().catch((e) => { wasmInit = null; throw e; });\n" +
        "  }\n" +
        "  return wasmInit;\n" +
        "}",
      0,
    ],
    [
      // The failure this contract exists to catch: wired right after
      // installing the memo, but outside its body.
      "let wasmInit = null;\n" +
        "function ensureXWasm() {\n" +
        "  if (!wasmInit) {\n" +
        "    wasmInit = (async () => {\n" +
        "      // wired elsewhere\n" +
        "    })().catch((e) => { wasmInit = null; throw e; });\n" +
        "  }\n" +
        "  wireCriticalAlertsSource({});\n" +
        "  return wasmInit;\n" +
        "}",
      1,
    ],
    // No memoized init at all — every call is a violation.
    ["wireCriticalAlertsSource({});", 1],
  ];
  for (const [src, expected] of cases) {
    const got = callsOutsideMemoizedInit(stripComments(src)).length;
    if (got !== expected) {
      throw new Error(
        `matcher self-check failed: expected ${expected} violation(s), got ${got} for:\n${src}`,
      );
    }
  }
});
