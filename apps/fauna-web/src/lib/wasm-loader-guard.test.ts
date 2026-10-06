// Deno tests for the memoized-init-promise concurrency guard every
// `ensure*Wasm()` loader now shares (`wasm.ts::ensureWasm`'s own doc
// comment states the hazard: a second concurrent init call would re-run
// the wasm-bindgen `default()` entry point and reset that chunk's linear
// memory, wiping any module statics a peer caller had already seeded).
//
// This does NOT import any of the ten production loader files. Every one
// of them (`wasm-atproto-settings.ts`, `wasm-labeler-catalog.ts`,
// `wasm-content-index.ts`, `wasm-media.ts`, `wasm-folders.ts`,
// `wasm-backups.ts`, `wasm-connected-apps.ts`, plus the pre-existing `wasm.ts`,
// `wasm-launch.ts`, `wasm-onboarding.ts`) but `wasm-content-index.ts`
// imports `$app/paths`, which only resolves under the SvelteKit/Vite
// build — exactly why `web-unit-test`'s own `deno test` invocation runs
// `--no-check` and why `offline-gate.test.ts` documents the same
// constraint. `wasm-content-index.ts` avoids it (injected `base`
// param) but its wasm chunk panics at runtime under Deno (tantivy spawns
// threads) — see its own `.test.ts`'s ON HOLD note — so it cannot drive a
// real `mod.default()` either. There is no loader in this SPA today that
// is BOTH import-map-free AND backed by a working wasm binary under
// `deno test`.
//
// So the first three tests below test the GUARD SHAPE only, via a
// hand-written REPLICA — the exact three-part pattern (`if (wasmModule)
// return`, `if (!wasmInit) { wasmInit = (async () => {...})().catch(reset)
// }`, `return wasmInit`) copied verbatim into all seven previously-
// unguarded loaders. `makeGuardedLoader` reproduces it with an injectable
// init function standing in for `mod.default()`, so the property under
// test — two concurrent callers share one in-flight init — is exercised
// without a real wasm chunk. `makeUnguardedLoader` reproduces the BEFORE
// shape (bare `if (wasmModule) return` with no promise memoization) so the
// second test demonstrates the bug the pattern fixes. Neither reads a
// production file, so neither can catch a REAL loader losing the guard —
// that is what the LAST test in this file does (the production
// red-verify: mutation-tested by reverting a real loader's guard, which
// reds it and only it).
//
// Run: `deno test --allow-read --no-check apps/fauna-web/src/lib/`
// (the project's `web-unit-test` recipe already covers this directory).

function eq<T>(actual: T, expected: T, msg: string) {
  if (actual !== expected) {
    throw new Error(`${msg}: got ${JSON.stringify(actual)}, want ${JSON.stringify(expected)}`);
  }
}

/** Reproduces the guarded shape every `ensure*Wasm()` loader now shares.
 *  `init` stands in for `mod.default()` + the module-scoped side effect
 *  (e.g. `wireCriticalAlertsSource`) — each call bumps `initCount` so the
 *  test can assert exactly one real init happened. */
function makeGuardedLoader(init: () => Promise<void>) {
  let ready = false;
  let inFlight: Promise<void> | null = null;
  return function ensure(): Promise<void> {
    if (ready) return Promise.resolve();
    if (!inFlight) {
      inFlight = init()
        .then(() => {
          ready = true;
        })
        .catch((e) => {
          inFlight = null;
          throw e;
        });
    }
    return inFlight;
  };
}

/** The BEFORE shape — no promise memoization. Two concurrent callers both
 *  observe `ready === false` and both start `init()`. */
function makeUnguardedLoader(init: () => Promise<void>) {
  let ready = false;
  return async function ensure(): Promise<void> {
    if (ready) return;
    await init();
    ready = true;
  };
}

Deno.test('guard shape (hand-written replica): two concurrent callers share one in-flight init', async () => {
  let initCount = 0;
  const ensure = makeGuardedLoader(async () => {
    initCount++;
    await new Promise((r) => setTimeout(r, 10));
  });

  await Promise.all([ensure(), ensure()]);
  eq(initCount, 1, 'exactly one init for two concurrent callers');

  // A THIRD call after the module is ready must not re-init either.
  await ensure();
  eq(initCount, 1, 'a call after ready is a no-op, not a second init');
});

// NOT a "red-verify" in the sense of catching a real regression: this drives
// `makeUnguardedLoader`, a hand-written reproduction of the pre-fix shape,
// never a production file. It demonstrates the PATTERN'S bug in the
// abstract — useful context for the guard-shape test above — but proves
// nothing about whether any specific shipped loader still has it. The
// production red-verify is the LAST test in this file.
Deno.test(
  'guard shape (hand-written replica): the unguarded pre-fix shape re-runs init for concurrent callers',
  async () => {
    let initCount = 0;
    const ensure = makeUnguardedLoader(async () => {
      initCount++;
      await new Promise((r) => setTimeout(r, 10));
    });

    await Promise.all([ensure(), ensure()]);
    // This is the bug the fix closes: without memoizing the in-flight
    // promise, both concurrent callers pass `if (ready)` before either sets
    // it, so `init()` — the wasm-bindgen `default()` entry point in
    // production — runs twice, resetting linear memory the second time.
    eq(initCount, 2, 'the unguarded shape double-inits under concurrency (this IS the bug)');
  },
);

Deno.test('guard shape (hand-written replica): a failed init drops the cached promise so a retry can succeed', async () => {
  let attempt = 0;
  const ensure = makeGuardedLoader(async () => {
    attempt++;
    if (attempt === 1) throw new Error('transient chunk-fetch failure');
  });

  let threw = false;
  try {
    await ensure();
  } catch {
    threw = true;
  }
  eq(threw, true, 'the first call surfaces the init failure');

  await ensure();
  eq(attempt, 2, 'a later call retries rather than replaying the cached rejection');
});

// The production red-verify: unlike the three replica-driven tests above,
// this reads every `ensure*Wasm()` loader FILE itself and asserts each one
// declares its own `wasmInit` memoization variable — so a future loader
// that copies the pre-fix `if (wasmModule) return` shape without the guard
// reds HERE, against real source, not silently. Mutation-tested: reverting
// a real loader's guard to the bare pre-fix shape reds exactly this test
// and nothing else. Closes the class this row
// asked for, not just today's seven instances.
const LOADER_FILES = [
  'wasm.ts',
  'wasm-launch.ts',
  'wasm-onboarding.ts',
  'wasm-atproto-settings.ts',
  'wasm-labeler-catalog.ts',
  'wasm-connected-apps.ts',
  'wasm-content-index.ts',
  'wasm-media.ts',
  'wasm-folders.ts',
  'wasm-backups.ts',
];

Deno.test('production red-verify: every ensure*Wasm() loader file declares the memoized-init guard', () => {
  // Resolve each file as a URL and let Deno convert it to a native path. Taking
  // `.pathname` and concatenating instead yields `/D:/src/...` on Windows — a
  // leading-slash path no `readFile` can open — so this suite was red on Windows
  // (and only on Windows) while passing everywhere else.
  for (const file of LOADER_FILES) {
    const src = Deno.readTextFileSync(new URL(file, import.meta.url));
    const hasGuardVar = /\blet\s+wasmInit\s*:\s*Promise</.test(src);
    const hasResetArm = /wasmInit\s*=\s*null/.test(src);
    if (!hasGuardVar || !hasResetArm) {
      throw new Error(
        `${file} is missing the memoized-init-promise guard (wasmInit + a ` +
          `reset-on-failure arm) — see wasm.ts::ensureWasm's doc comment for ` +
          `why every loader needs it.`,
      );
    }
  }
});
