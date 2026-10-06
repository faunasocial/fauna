/** The SPA's own-session-id set — the TypeScript twin of Rust's
 *  `fauna_protocol::auth::OwnSessionIds` (`docs/goal/behavior/devices.md`
 *  § The client's own session).
 *
 *  **Why a twin rather than shared Rust.** Every other bearer holder in the
 *  fleet keeps this set in the Rust type — `fauna_client::token_cache::TokenCache`
 *  (and through it all three `Ws*Bearer`s) and
 *  `fauna_launch_machine::LaunchMachine`. Web is the one seat whose bearer cache
 *  is not Rust: `$lib/api.ts`'s `tokenCache` is a plain module-level `Map`, and
 *  the goal doc names it directly (wasm `challengeVerify` emits it and the SPA's
 *  token cache keeps it). So this file mirrors the Rust rule — and nothing
 *  else: no verdict, no policy, no second definition of what "expired" means.
 *
 *  It is a module of its own, not a few lines inside `api.ts`, for the reason
 *  `offline-gate.ts` is: `api.ts` pulls in wasm at import time, so nothing in it
 *  can be reached by `deno test`. The rule that decides which sessions are the
 *  app's own is exactly the part that must be pinned.
 *
 *  **Memory only, never persisted** — so a reloaded tab has forgotten the
 *  previous page load's ids and that load's last token shows as an unmarked row
 *  for up to an hour, the bound the goal doc states rather than hides.
 *
 *  **Reads no clock.** Every method takes `now` (Unix **seconds**) as a
 *  parameter, exactly as the Rust twin does — which is what makes the pruning
 *  direction pinnable without mocking time. The caller supplies it;
 *  `$lib/api.ts` has its own `nowSecs()` for that.
 *
 *  One deliberate difference from the Rust twin: `OwnSessionIds::prune` there
 *  takes an `Option<u64>` because a native clock read can fail, and a failed
 *  read prunes nothing. `Date.now()` has no failure mode, so that arm has no
 *  counterpart here — the direction it protects (never drop a live own id) is
 *  the same one this file takes by construction.
 */

/** One session id this browser minted, plus the deadline that retires it. */
type Entry = { id: string; expiresAt: number };

export class OwnSessionIds {
  /** Oldest first; the last entry is the most recently minted. */
  #entries: Entry[] = [];

  /** Record a freshly-minted session id. Re-recording an id already held
   *  refreshes its deadline and moves it to the end rather than duplicating
   *  it, so a re-mint returning the same id cannot make one session paint as
   *  two. An empty id (defensively guarded) is not recorded — an
   *  unnamed session is not an own session. */
  record(id: string, expiresAt: number): void {
    if (!id) return;
    this.#entries = this.#entries.filter((e) => e.id !== id);
    this.#entries.push({ id, expiresAt });
  }

  /** Drop every id whose deadline has passed `now` (Unix **seconds**). */
  prune(now: number): void {
    this.#entries = this.#entries.filter((e) => e.expiresAt > now);
  }

  /** Every own id still live at `now`, oldest first — the rows the app folds
   *  into its one "this app" row. With only the current id the app's own
   *  previous token would paint as an unknown second session, and *sign out
   *  everywhere else* would appear to find and kill a stranger. */
  idsAt(now: number): string[] {
    return this.#entries.filter((e) => e.expiresAt > now).map((e) => e.id);
  }

  /** The **current** id — the most recently recorded one still live at `now`.
   *  This is what `keep_token_id` is read from, at call time and never from a
   *  previously painted list: a renewal between paint and press must not name
   *  a dead token. */
  currentAt(now: number): string | null {
    for (let i = this.#entries.length - 1; i >= 0; i--) {
      if (this.#entries[i].expiresAt > now) return this.#entries[i].id;
    }
    return null;
  }
}
