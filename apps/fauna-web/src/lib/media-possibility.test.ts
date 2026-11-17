// The Media page's upload, delete and restore paint from the media machine's
// answers (`docs/goal/ui/media.md` § Errors & edge cases → *What this person
// may write*): blocked with its reason before the first read and on a reader's
// or metadata-only target; a reader's item detail offers no delete or restore.
import { assert, assertEquals } from 'jsr:@std/assert@1';
import { uploadPaint, writePaint, type MediaPossibilitySource } from './media-possibility.ts';

/** A machine stand-in answering in the wasm getters' serde JSON. */
function source(upload: unknown, write: Record<string, unknown>): MediaPossibilitySource {
  return {
    uploadPossibilityJson: () => JSON.stringify(upload),
    writePossibilityJson: (folder: string) => JSON.stringify(write[folder] ?? { state: 'possible' }),
  };
}

Deno.test('the upload is shown disabled with its reason before the first read', () => {
  const loading = { state: 'blocked', reason: { key: 'common.still_loading' } };
  const paint = uploadPaint(source(loading, {}));
  assert(paint.shown);
  assert(paint.disabled);
  assertEquals(paint.reason?.key, 'common.still_loading');
});

Deno.test('the upload is live once the machine answers possible', () => {
  const paint = uploadPaint(source({ state: 'possible' }, {}));
  assert(paint.shown && !paint.disabled);
  assertEquals(paint.reason, null);
});

Deno.test('the upload is absent inside a followed scope', () => {
  assert(!uploadPaint(source({ state: 'absent' }, {})).shown);
});

Deno.test('the upload is dead before the machine exists', () => {
  const paint = uploadPaint(null);
  assert(paint.shown && paint.disabled);
});

Deno.test("a reader's item detail offers no delete and no restore", () => {
  const machine = source({ state: 'possible' }, { 'local:9': { state: 'absent' } });
  assert(!writePaint(machine, 'local:9', false).shown);
  const own = writePaint(machine, 'photos', false);
  assert(own.shown && !own.disabled);
});

Deno.test('a followed item is head-only: no delete, no restore', () => {
  const machine = source({ state: 'possible' }, {});
  assert(!writePaint(machine, 'photos', true).shown);
});
