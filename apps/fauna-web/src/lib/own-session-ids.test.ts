// Deno tests for the SPA's own-session-id set. Run via:
//
//     deno test --allow-read --no-check apps/fauna-web/src/lib/own-session-ids.test.ts
//
// This is web's leg of `docs/goal/behavior/devices.md` § The client's own
// session. The same rule is pinned Rust-side on the shared type every other
// holder uses (`fauna_protocol::auth::OwnSessionIds`, `libs/fauna-protocol/
// src/auth.rs`); these tests exist because web's bearer cache is the one seat
// that is not Rust, so its copy of the rule can drift on its own.
//
// `api.ts` cannot be imported here — it pulls in wasm at module load — which
// is exactly why the rule lives in its own module rather than inside that
// file's private `tokenCache` Map.

import { OwnSessionIds } from './own-session-ids.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}: got ${a}, want ${e}`);
}

Deno.test('two successive mints keep both ids until the first expires', () => {
  const own = new OwnSessionIds();
  own.record('aaaaaaaaaaaaaaaa', 1_000_000_100);
  own.record('bbbbbbbbbbbbbbbb', 1_000_003_700);
  // The renewal case the set exists for: the predecessor row outlives the
  // renewal and both are this app's.
  eq(own.idsAt(1_000_000_000), ['aaaaaaaaaaaaaaaa', 'bbbbbbbbbbbbbbbb'], 'both live');
  eq(own.idsAt(1_000_000_200), ['bbbbbbbbbbbbbbbb'], 'predecessor lapsed');
});

Deno.test('current is the newest recorded id, read at call time', () => {
  const own = new OwnSessionIds();
  eq(own.currentAt(1_000_000_000), null, 'nothing minted yet');
  own.record('aaaaaaaaaaaaaaaa', 1_000_000_100);
  eq(own.currentAt(1_000_000_000), 'aaaaaaaaaaaaaaaa', 'first mint');
  own.record('bbbbbbbbbbbbbbbb', 1_000_003_700);
  // A renewal between paint and press must name the NEW token, never the one
  // the painted list happened to carry.
  eq(own.currentAt(1_000_000_000), 'bbbbbbbbbbbbbbbb', 'after renewal');
  // Once the newest has lapsed the answer falls back to a live predecessor
  // rather than naming a dead token.
  own.record('cccccccccccccccc', 1_000_000_050);
  eq(own.currentAt(1_000_000_060), 'bbbbbbbbbbbbbbbb', 'newest lapsed');
});

Deno.test('prune drops only lapsed ids', () => {
  const own = new OwnSessionIds();
  own.record('aaaaaaaaaaaaaaaa', 1_000_000_100);
  own.record('bbbbbbbbbbbbbbbb', 1_000_003_700);
  own.prune(1_000_000_200);
  eq(own.idsAt(1_000_000_200), ['bbbbbbbbbbbbbbbb'], 'one survivor');
  eq(own.currentAt(1_000_000_200), 'bbbbbbbbbbbbbbbb', 'survivor is current');
});

Deno.test('re-recording an id refreshes it rather than duplicating it', () => {
  const own = new OwnSessionIds();
  own.record('aaaaaaaaaaaaaaaa', 1_000_000_100);
  own.record('bbbbbbbbbbbbbbbb', 1_000_000_200);
  own.record('aaaaaaaaaaaaaaaa', 1_000_003_700);
  eq(
    own.idsAt(1_000_000_000),
    ['bbbbbbbbbbbbbbbb', 'aaaaaaaaaaaaaaaa'],
    'one entry per session',
  );
  eq(own.currentAt(1_000_000_000), 'aaaaaaaaaaaaaaaa', 're-recorded is current');
});

Deno.test('an unnamed session is not an own session', () => {
  // A mint reply with no id (defensively guarded). Recording '' would
  // make every such mint fold into one phantom row.
  const own = new OwnSessionIds();
  own.record('', 1_000_003_700);
  eq(own.idsAt(1_000_000_000), [], 'nothing recorded');
  eq(own.currentAt(1_000_000_000), null, 'no current');
});
