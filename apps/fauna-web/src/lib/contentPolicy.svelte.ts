// Client render-enforcement state + verdict, shared across the two social
// surfaces (feed + conversations) — the web twin of linux `content_policy.rs`
// (`family-safety.md` § Content policy). The shared engine
// `fauna_core::obligation::render_verdict_entries` (over wasm as
// `contentRenderVerdict`) is the pure computation; this module holds the render
// STATE the verdict composes — the viewer's own spam/phishing thresholds and,
// for a supervised account, the guardian floor — and exposes the one
// `contentVerdict(labels)` both the feed and conversations pages call, so the
// two surfaces can never drift on how a content floor is enforced (priority #2/#4).
//
// A `.svelte.ts` module so the cache is `$state`: reading it inside a component's
// reactive context (template `{@const}`) makes that surface re-render when a
// late-arriving successful status read moves the floor — the reactive
// equivalent of linux rebuilding its list after the post-auth read lands.

import {
  contentRenderVerdict,
  probabilityToPerMille,
  type ContentLabelEntry,
  type ContentPolicyValue,
  type ContentRenderVerdict,
  type ContentRender,
  type RegionItem,
} from '$lib/wasm';
import { familyStatus, spamGetPreferences } from '$lib/rpc';
import { regionRender } from '$lib/region.svelte';
import { registerActorScopedReset } from '$lib/actorScope';

// The supervised viewer's own guardian content policy (`fauna.family.status`).
// `null` for an unsupervised account — no floor; only the viewer's own
// thresholds (if any) decide.
let wardContentPolicy = $state<ContentPolicyValue | null>(null);
// The viewer's OWN spam/phishing thresholds, per-mille (converted from the
// probability floats `fauna.spam.get_preferences` returns). `undefined` until
// the read lands — then no own-threshold rule composes. Every user gets this,
// supervised or not; a guardian floor composes ON TOP, strictest-wins.
let ownSpamPermille = $state<number | undefined>(undefined);
let ownPhishingPermille = $state<number | undefined>(undefined);
// The guardian's Guardian Notify knob (`content_notify`), from `fauna.family.status`
// (`family-safety.md` § Guardian Notify). When on, the ward's client counts its
// guardian-floor enforcement events and reports coarse per-category aggregates.
let contentNotifyEnabled = $state<boolean>(false);

/** Move the guardian half — the floor + the Guardian Notify knob — to what a
 *  SUCCESSFUL `fauna.family.status` read, or the launch restore, established.
 *  Callers pass the reply's `supervision` fold, never `policy` straight off
 *  the reply: the fold's graduation gate is what makes a guardian-less reply
 *  clear both (family-safety.md § Content policy, clause 1 — a successful read
 *  may move enforcement state, including to unsupervised; a failed one never
 *  does).
 *
 *  Every successful read the app makes writes here: the root layout's (at
 *  launch AND on every WS reconnect, clause 1's two moments — a social
 *  surface's `onMount` does not re-run on a reconnect, so without this writer
 *  a guardian's edit binds only at the ward's next login), the Family page's,
 *  and each social surface's `hydrateContentPolicy`; plus the launch restore
 *  (`supervision.svelte.ts`), ahead of the first read. */
export function setGuardianHalf(policy: ContentPolicyValue | null, notify: boolean): void {
  wardContentPolicy = policy;
  contentNotifyEnabled = notify;
}

/** Cache the viewer's own spam/phishing thresholds. The wasm engine keys on
 *  per-mille `u16`, so the [0,1] probability floats convert via the shared
 *  `probabilityToPerMille` (the same scale the settings slider writes to the
 *  per-mille wire). */
export function setSpamPreferences(spamThreshold?: number, phishingThreshold?: number): void {
  ownSpamPermille = spamThreshold === undefined ? undefined : probabilityToPerMille(spamThreshold);
  ownPhishingPermille =
    phishingThreshold === undefined ? undefined : probabilityToPerMille(phishingThreshold);
}

/** Hydrate the cache from the post-auth reads (call in each social surface's
 *  `onMount`; idempotent + cached), so the surface's FIRST paint already carries
 *  its verdict. Best-effort per read — a failure leaves that half as it was
 *  (for the guardian half, clause 1's keep-last-known), so the surface renders
 *  normally: the fail-OPEN is on the READ, while enforcement stays fail-CLOSED
 *  only on a value the client cannot PARSE (`ContentFloor::Unknown → block`,
 *  handled in shared Rust). */
export async function hydrateContentPolicy(secretHex: string): Promise<void> {
  try {
    const { supervision } = await familyStatus(secretHex);
    setGuardianHalf(supervision.content_policy, supervision.content_notify);
  } catch {
    // No information — the guardian half keeps its last-known value.
  }
  try {
    const prefs = await spamGetPreferences(secretHex);
    setSpamPreferences(prefs.spam_threshold, prefs.phishing_threshold);
  } catch {
    // No preferences read — no own-threshold rule composes.
  }
}

/** The content-policy render verdict for a piece of content, keyed on its
 *  labels (the reduced `[{category, confidence_per_mille}]` shape on
 *  `PostSummary.labels` / `MessageSnapshot.labels`). Composes the cached own
 *  thresholds + guardian floor, strictest-wins, entirely in shared Rust. */
export function contentVerdict(labels: ContentLabelEntry[] | undefined): ContentRenderVerdict {
  return contentRenderVerdict(labels ?? [], wardContentPolicy, ownSpamPermille, ownPhishingPermille);
}

/** The render decision for one item with the device's REGION composed in as
 *  the third source (`region-blocking.md` § The content plane) — the ONE call
 *  the feed cards, the post detail and the conversation bubbles make, so no
 *  surface composes one source while bypassing another. `placeholder` is set
 *  exactly when the region drove a `block`/`collapse`; the surface paints it
 *  ahead of the family arm. Before the plane is open this is the family/own
 *  verdict alone (nothing held yet, so nothing regional to apply). */
export function contentRender(labels: ContentLabelEntry[] | undefined, item: RegionItem): ContentRender {
  const composed = regionRender(labels ?? [], wardContentPolicy, ownSpamPermille, ownPhishingPermille, item);
  return composed ?? { verdict: contentVerdict(labels), placeholder: null };
}

/** The cached guardian content policy (or `null` for an unsupervised viewer) — the
 *  Guardian Notify buffer reads it to attribute enforcement events to categories. */
export function guardianContentPolicy(): ContentPolicyValue | null {
  return wardContentPolicy;
}

/** Whether the guardian's Guardian Notify knob is on — the buffer counts + reports
 *  only while true (`family-safety.md` § Guardian Notify). */
export function contentNotifyOn(): boolean {
  return contentNotifyEnabled;
}

/** Drop the cached policy on an actor change (`actorScope.ts`).
 *
 *  Every field here is account-scoped class-1 data — one identity's guardian floor
 *  and their own spam/phishing thresholds — so under the switch/sign-out isolation
 *  contract (account-scoping.md) none of it may survive into the next account's
 *  render. Re-hydration is `hydrateContentPolicy`, which each social surface calls
 *  for the incoming actor.
 *
 *  ⚠ Clearing is load-bearing in BOTH directions, and the dangerous direction is
 *  the quiet one. `hydrateContentPolicy`'s reads fail OPEN by design (an
 *  unreachable `fauna.family.status` must not block an unsupervised user's feed),
 *  so without this drop a failed re-hydration leaves the PREVIOUS actor's values
 *  in place indefinitely: a supervised ward switching in would render against
 *  whatever floor the outgoing account had — including *no* floor — which is a
 *  guardian content floor silently not applying. Dropping first turns that same
 *  failure into "no floor composes for anyone", the state the fail-open was
 *  actually designed around. */
function resetContentPolicy(): void {
  wardContentPolicy = null;
  ownSpamPermille = undefined;
  ownPhishingPermille = undefined;
  contentNotifyEnabled = false;
}

registerActorScopedReset(resetContentPolicy);
