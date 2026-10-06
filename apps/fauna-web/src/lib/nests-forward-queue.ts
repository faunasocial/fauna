// The Nests page's forward-queue block (`nests-forward-*`, docs/goal/ui/nests.md
// § Forward queue): what `NestsSection.svelte` paints from the shared
// `LinkedNestsSnapshot.forward_queue`. Mirrors tui's `push_forward_queue` field
// for field — the count, the stuck half's what-to-check, the reason once a send
// has failed — and nothing more: the queue logic, the retry/discard actions and
// the control-strip of the relay-chosen reason all live in the shared machine
// (`fauna_client_pair::ForwardQueueStatus`).

import { t } from './i18n/strings.ts';

/** The shared `ForwardQueueStatus` across the serde_wasm_bindgen boundary. */
export interface ForwardQueueStatus {
  queued: number;
  stuck: number;
  last_error: string | null;
}

export interface ForwardQueueView {
  /** `nests-forward-queue` — the count, plus the stuck hint when any. */
  summary: string;
  /** `nests-forward-queue-reason`, or `null` — absent, never an empty line. */
  reason: string | null;
}

/**
 * The block to paint, or `null` when there is none: an absent queue
 * or a queue that is empty. The reason is relay-chosen
 * text (`private-mode.md` § Post Forwarding) — the caller interpolates it as
 * plain text, never through `{@html}`, markdown or a linkify pass.
 */
export function forwardQueueView(q: ForwardQueueStatus | null | undefined): ForwardQueueView | null {
  if (!q || q.queued <= 0) return null;
  let summary = t.nests.forward_queue_summary({ count: String(q.queued) });
  if (q.stuck > 0) summary += ' ' + t.nests.forward_queue_stuck({ count: String(q.stuck) });
  const reason = q.last_error ? t.nests.forward_queue_last_error({ error: q.last_error }) : null;
  return { summary, reason };
}
