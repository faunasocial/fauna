// Deno tests for the feed verb-error copy. Run via:
//
//     deno test apps/fauna-web/src/lib/feed-verb-error.test.ts
//
// A refused reply or quote under a restricted post paints the shared
// `feed.reference_restricted` string; any other failure keeps its own text.
import { assertEquals } from 'jsr:@std/assert';
import { verbErrorCopy } from './feed-verb-error.ts';
import { resolveKey } from './i18n/localized.ts';

const REFUSAL =
  'this post is restricted to a smaller audience, and a reply or quote with text ' +
  'would be public — it was not sent';
// Stands in for the wasm `feedRefusalI18nKey` (the real recognizer is shared Rust).
const recognizer = (text: string) => (text === REFUSAL ? 'feed.reference_restricted' : null);

Deno.test('a restricted reply refusal paints the shared string', () => {
  assertEquals(
    verbErrorCopy(new Error(REFUSAL), recognizer),
    resolveKey('feed.reference_restricted'),
  );
});

Deno.test('any other failure keeps its own text', () => {
  assertEquals(verbErrorCopy(new Error('nest unreachable'), recognizer), 'nest unreachable');
  assertEquals(verbErrorCopy('plain string', recognizer), 'plain string');
});
