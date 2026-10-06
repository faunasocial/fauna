// The unattested-member review roster, as the SPA holds it — web's twin of
// tui's `App::member_reviews` (`docs/goal/behavior/succession-aftermath.md`
// § Propagation → *MLS groups*, item 3a).
//
// **Why a module store rather than page state.** The roster is raised by the
// *aftermath*, which the root layout starts at the actor settle that follows a
// successor's sign-in; it is *read* on the conversations page's member chips,
// which the user reaches later and which mounts fresh. Page-local state would
// therefore be empty at exactly the moment a successor's FIRST session needs it
// — the one session the whole surface exists for. Same reasoning, same shape as
// `$lib/succession-aftermath`; a native app gets this for free from one
// process-wide `App`.
//
// **Cached, not re-read per paint (the ratified rule).** A member list paints on
// every frame while the succession ledger changes only when a ceremony raises
// items or the owner answers one, so this store is refreshed at exactly the two
// points § Propagation names — **behind the aftermath's raise**
// (`routes/+layout.svelte`, on the post-store-ready pass's `configStageSettled`)
// and **after every adjudication** (a Keep press). Anything else re-reads the
// ledger to answer a question that cannot have changed.
//
// **Hex, never bytes.** The roster is kept as the `person` hex strings the shared
// face already produces, and handed back to Rust in that form: the join that
// decides which chip is flagged is `fauna_conversations::member_review_flags`,
// not anything here (see `memberReviewFlagsForThread`).

import { writable } from 'svelte/store';
import { registerActorScopedReset } from './actorScope';
import { memberReviewList } from './conversations';

/** The `person` hex of every OPEN review item for the current identity. Empty is
 *  the ordinary state: an identity that never succeeded has raised nothing, and
 *  an owner who worked their backlog through has closed everything — an
 *  adjudicated item is kept at rest carrying its verdict, so "open" is the
 *  question this list answers, never "was ever raised". */
export const memberReviewRoster = writable<string[]>([]);

/** Re-read the open roster. Call ONLY at the two ratified refresh points.
 *
 *  Log-only on failure, and deliberately non-clearing: a transport failure on a
 *  sign-in must not blank a flagged person's mark, because items raised by
 *  *earlier* ceremonies are already at rest and the failure mode this whole
 *  surface exists to prevent is a flagged person who renders unflagged. */
export async function refreshMemberReviewRoster(): Promise<void> {
  try {
    const reviews = await memberReviewList();
    memberReviewRoster.set(reviews.map((r) => r.person));
  } catch (e) {
    console.warn('[member-review] roster read failed; keeping the last one:', e);
  }
}

// Actor-scoped: a switch drops the previous identity's roster before the new
// identity's handlers run, so a successor never renders marks raised about the
// account it left.
registerActorScopedReset(() => memberReviewRoster.set([]));
