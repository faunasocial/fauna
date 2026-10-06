// The Nests page's forward-queue block (docs/goal/ui/nests.md § Forward queue):
// the view it paints, and the render-inert property of its relay-chosen reason
// (`private-mode.md` § Post Forwarding: the app renders it as untrusted text).
//
// The reason reaching this view has already been control-stripped by the shared
// projection (`fauna_client_pair::ForwardQueueStatus::from`, pinned there); what
// web owes on top is that markup in it stays text. Svelte's `{expr}` escapes, so
// that property is the SOURCE shape: the reason element interpolates, and the
// component carries no `{@html}` at all. Asserted over the source (convention 17)
// because a hostile reason needs a hostile relay to reach an e2e run.

import { forwardQueueView } from './nests-forward-queue.ts';
import { stripComments } from './source-contract.ts';

function assertEquals<T>(actual: T, expected: T): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

Deno.test('an absent or empty queue paints no block', () => {
  assertEquals(forwardQueueView(null), null);
  assertEquals(forwardQueueView(undefined), null);
  assertEquals(forwardQueueView({ queued: 0, stuck: 0, last_error: 'stale' }), null);
});

Deno.test('a waiting queue paints the count, the stuck hint and the reason', () => {
  const v = forwardQueueView({ queued: 3, stuck: 1, last_error: 'fauna.federation.forbidden' })!;
  if (!v.summary.startsWith('3 ') || !v.summary.includes('1 of them')) throw new Error(v.summary);
  if (!v.reason?.endsWith('fauna.federation.forbidden')) throw new Error(String(v.reason));
});

Deno.test('no reason line until a send has failed', () => {
  const v = forwardQueueView({ queued: 1, stuck: 0, last_error: null })!;
  assertEquals(v.reason, null);
  if (v.summary.includes('of them')) throw new Error('no stuck hint without stuck entries');
});

Deno.test('a hostile reason is carried verbatim, for text interpolation', () => {
  const v = forwardQueueView({ queued: 1, stuck: 0, last_error: '[31m<b>x</b>forbidden' })!;
  if (!v.reason?.includes('<b>x</b>')) throw new Error(String(v.reason));
});

Deno.test('NestsSection paints the reason by interpolation and has no {@html}', () => {
  const src = stripComments(
    Deno.readTextFileSync(new URL('./components/NestsSection.svelte', import.meta.url)),
  );
  if (src.includes('{@html')) throw new Error('NestsSection.svelte must not use {@html}');
  const tag = src.match(/<[a-z]+[^>]*IDS\.NESTS_FORWARD_QUEUE_REASON[^>]*>([^<]*)</);
  if (!tag) throw new Error('no nests-forward-queue-reason element');
  if (tag[1].trim() !== '{forwardView.reason}') {
    throw new Error(`the reason must be plain interpolation, got ${tag[1].trim()}`);
  }
});
