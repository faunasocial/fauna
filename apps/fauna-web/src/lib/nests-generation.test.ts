// Deno tests for the Nests page's retained-generation pure render logic. Run
// via `just web-unit-test` (`deno test apps/fauna-web/src/lib/`). Mirrors
// linux's `linked_nests.rs` generation-row test suite (nests.md § Trust
// facet — generation recovery, ratified 2026-07-29) — the same two ratified
// honesty invariants, verified here rather than assumed.

import {
  generationPathText,
  generationShowsRestore,
  generationNoticeText,
  type TrustGenerationRow,
} from './nests-generation.ts';

function generationRow(status: 'Listed' | 'Unreachable', path: string | null): TrustGenerationRow {
  return {
    status,
    destination_id: 'dest-1',
    destination_label: 'Recovery nest',
    folder_name: '__mail',
    path,
    path_hash: 'abc123deadbeef',
    manifest_hash: 'manifest-hex',
    size_bytes: 4096,
    superseded_at: 1_700_000_000,
    expires_at: 1_700_100_000,
  };
}

// Mutation 1 (nests.md:122): an Unreachable row must never offer the restore
// affordance — there is no address to restore, and offering it would imply
// we knew something we do not.
Deno.test('an unreachable row offers no restore', () => {
  const row = generationRow('Unreachable', null);
  if (generationShowsRestore(row)) {
    throw new Error('an unreachable row must not show the restore affordance');
  }
});

Deno.test('a listed row offers restore', () => {
  const row = generationRow('Listed', '/Mail/2026');
  if (!generationShowsRestore(row)) {
    throw new Error('a listed row must show the restore affordance');
  }
});

// Mutation 2 (nests.md:123): a path-less Listed row (a sealed custody
// row with its path scrubbed) must render its hash, never be hidden or skipped —
// the rows a rogue source produced are exactly the ones a user needs to see.
Deno.test('a path-less listed row renders the hash, not hidden', () => {
  const row = generationRow('Listed', null);
  const text = generationPathText(row);
  if (!text) {
    throw new Error('a path-less row must still render identity text');
  }
  if (!text.includes(row.path_hash)) {
    throw new Error(`no plaintext path ⇒ the hash fallback, got: ${text}`);
  }
});

Deno.test('a listed row with a path renders the plaintext path', () => {
  const row = generationRow('Listed', '/Mail/2026');
  if (!generationPathText(row).includes('/Mail/2026')) {
    throw new Error('expected the plaintext path in the rendered identity text');
  }
});

// An unreachable row's identity leaf names the DESTINATION that went dark,
// not a generation that does not exist.
Deno.test('an unreachable row names the destination, not a generation', () => {
  const row = generationRow('Unreachable', null);
  if (generationPathText(row) !== row.destination_label) {
    throw new Error('an unreachable row must name the destination');
  }
});

Deno.test('no restore outcome renders an empty notice', () => {
  if (generationNoticeText(null) !== '') {
    throw new Error('a null outcome must render an empty notice');
  }
});

Deno.test('a restored outcome renders its own notice text', () => {
  if (!generationNoticeText('Restored')) {
    throw new Error('a Restored outcome must render non-empty notice text');
  }
});

Deno.test('a past-recovery-window outcome renders its own notice text, never "failed"', () => {
  const text = generationNoticeText('PastRecoveryWindow');
  if (!text) {
    throw new Error('a PastRecoveryWindow outcome must render non-empty notice text');
  }
  if (/fail/i.test(text)) {
    throw new Error(`PastRecoveryWindow is a product state, not a failure — must never say "failed", got: ${text}`);
  }
});
