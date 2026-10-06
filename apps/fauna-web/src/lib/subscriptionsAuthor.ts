// Web runtime wiring for the shared author-side subscription reconciliation —
// the browser twin of linux `apps/fauna-linux/src/subscriptions_author.rs` and
// windows `SubscriptionsAuthorPump`. Started once from the root layout
// (`+layout.svelte`), alongside the conversations receive poll: a self-gating
// loop that, whenever an authenticated identity is present, runs one shared
// `subscriptionsReconcileOnce` tick (`SubscriptionsAuthor::reconcile_once`) —
// on connect and on a poll backstop.
//
// The tick itself — resume any crash-staged subscriber-removal, *then*
// auto-approve every queued `auto_approve` **subscribe** request (minting the
// covering `KeyBlob` per row) — is shared policy in `fauna-client-subscriptions`
// (`monetization.md` § Pillar 1 → *Where the logic lives*: "An app MUST NOT
// re-derive either"); this file owns only the scheduling. Draining is what
// makes an encrypted-mode **follow** frictionless: the nest cannot mint the
// KeyBlob, so a follow *enqueues* (`Queued`) even for the `auto_approve` rank-0
// `followers` tier, and the author's own client grants it here — with no
// manual approve (`monetization.md` § The unifying model, grant path 2).
//
// The web has no push subscriptions, so polling is the delivery floor: a follow
// that arrives while the author is online lands within one interval; a follow
// that accumulated while the author was offline is drained on the connect-time
// first pass (which is what the tier_3 e2e proves — the follower subscribes
// before the author logs in). All mint / seal / rotation crypto stays in shared
// Rust; this file is transport + scheduling glue only (priority #2). The
// cadence is `pollIntervalMs()` — a deliberate carve-out, *not* re-derivation:
// the shared `author_poll_secs()` reads an env var for its cadence, which a
// browser has none of, so web keeps its own interval (the same one the
// conversations receive poll uses — 30s prod, 2s under the e2e agent) while
// still running the shared tick.

import { get } from 'svelte/store';
import { identity } from './store';
import { ensureWasm } from './wasm';
import { pollIntervalMs } from './conversations';
import { subscriptionsReconcileOnce } from './rpc';

let pumpStarted = false;

/** Start the app-wide author auto-approve pump. Idempotent (at most one loop
 *  runs). Started from the root layout once (not gated on the current route / a
 *  present session) — the layout mounts once, so the loop self-gates instead:
 *  it no-ops each tick until an identity appears, then reads the current
 *  `secretHex` each tick (an identity swap is picked up on the next tick, the
 *  same way the conversations receive poll rebuilds its client). Fire-and-forget. */
export async function startSubscriptionsAuthorPump(): Promise<void> {
  if (pumpStarted) return;
  pumpStarted = true;
  await ensureWasm();
  for (;;) {
    const id = get(identity);
    if (id?.secretHex) {
      try {
        await subscriptionsReconcileOnce(id.secretHex);
      } catch {
        // Transient (disconnect / not-yet-connected) — retry next tick.
      }
    }
    await new Promise((r) => setTimeout(r, pollIntervalMs()));
  }
}
