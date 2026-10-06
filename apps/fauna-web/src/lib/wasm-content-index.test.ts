import {
  contentIndexCreateInRam,
  contentIndexAddDoc,
  contentIndexCommit,
  contentIndexQuery,
} from './wasm-content-index.ts';

function eq<T>(actual: T, expected: T, msg: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${msg}\n  expected: ${e}\n  actual:   ${a}`);
}

// SKIPPED — the fauna-wasm-content-index crate is on hold. It compiles to
// wasm32 but tantivy panics at runtime ("Failed to spawn segment updater
// thread"; tantivy spawns background threads, unsupported on
// wasm32-unknown-unknown). Verified via `wasm-pack test --headless
// --chrome` (2026-05-11). Web search goes through WS-RPC against the nest
// instead (Plan 6). Re-enable this suite if/when the crate is resurrected
// (wasm-threads / upstream tantivy fix) — tracked internally.
Deno.test.ignore('content-index WASM bindings (ON HOLD — tantivy not browser-compatible): round-trips add → commit → query', async () => {
  await contentIndexCreateInRam('/app');
  await contentIndexAddDoc('/app', {
    kind: 'mail',
    content_id: [0xaa, 0xbb, 0xcc, 0xdd],
    timestamp_ns: 1_000,
    sender_actor_id: null,
    fields: [{ kind: 'body', text: 'hello from typescript' }],
  });
  await contentIndexCommit('/app');
  const hits = await contentIndexQuery('/app', {
    query: 'typescript',
    kinds: ['mail'],
    range: null,
    limit: 10,
  });
  eq(hits.length, 1, 'one hit');
  eq(hits[0].kind, 'mail', 'hit kind');
  eq(hits[0].content_id, [0xaa, 0xbb, 0xcc, 0xdd], 'hit content_id');
});

Deno.test.ignore('content-index WASM bindings (ON HOLD — tantivy not browser-compatible): empty query yields no hits', async () => {
  await contentIndexCreateInRam('/app');
  await contentIndexAddDoc('/app', {
    kind: 'post',
    content_id: [1, 2, 3],
    timestamp_ns: 0,
    sender_actor_id: null,
    fields: [{ kind: 'body', text: 'anything' }],
  });
  await contentIndexCommit('/app');
  const hits = await contentIndexQuery('/app', {
    query: '',
    kinds: ['post'],
    range: null,
    limit: 10,
  });
  eq(hits.length, 0, 'no hits');
});
