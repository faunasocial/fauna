// The one-shot handoff a `dm-message-withheld-badge` tap leaves for the moderation queue
// (`moderation.md` § Legal takedown → *App render*, the fourth surface). The badge lives on
// the conversations page and the appeal form on the Privacy settings sub-page — two separate
// SvelteKit components — so the tap stashes the queue row's `content_id` here immediately
// before `goto()`-ing away and the settings page consumes it once its queue is read, the
// `lib/search.ts` pending-nav shape. The appeal that opens is the queue's own (one flow, the
// `appeal-*` ids), never a second form.

import { writable, get, type Writable } from 'svelte/store';

const pendingAppeal: Writable<string | null> = writable(null);

/** Called by the conversations page right before it navigates to the queue. */
export function setPendingAppeal(contentId: string): void {
  pendingAppeal.set(contentId);
}

/** Called once by the settings page after the queue is read: returns AND CLEARS the pending
 *  `content_id`; a plain page load (no badge tap routed here) sees `null`. */
export function consumePendingAppeal(): string | null {
  const v = get(pendingAppeal);
  if (v !== null) pendingAppeal.set(null);
  return v;
}
