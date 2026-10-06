// The pure state machine behind the ward-side Guardian Notify counter
// (`family-safety.md` § Guardian Notify) — the web twin of linux
// `content_policy.rs`'s `NotifyAccumulator`, and split out for the same reason
// that one is a plain struct: *which* categories count and *when* a batch is due
// is drift-critical logic, so it must be testable without the surfaces around it.
//
// Nothing here imports the SPA. `familyNotify.ts` owns the parts that do — the
// wasm category decision, the `contentPolicy` reads, the RPC send and the flush
// timer — and drives this with plain values. That split is what makes this file
// loadable under `deno test`: `familyNotify.ts` reaches `$app/paths` through
// `./wasm`, which the unit-test runner cannot resolve, so before this module the
// counter's rules were unpinnable on web while linux's twin was fully covered.

/** One category's pending delta, as `fauna.family.notify_report` carries it. */
export interface NotifyEntry {
  category: string;
  count: number;
}

/** A batch ready to send: the deltas plus the offset the nest stamps the
 *  ward-local day bucket from. */
export interface NotifyBatch {
  entries: NotifyEntry[];
  offsetMinutes: number;
}

export class NotifyBufferState {
  private pending = new Map<string, number>();
  private seen = new Set<string>();
  private seenDay = Number.NEGATIVE_INFINITY;
  private lastFlush: number | null = null;
  private offsetMinutes = 0;

  /** Count one guardian-floor enforcement on `itemId` for each of `categories`,
   *  deduped per `(item, category)` within the local day so a re-render never
   *  re-counts. Returns whether anything was added — the caller arms its flush
   *  timer only then. */
  record(itemId: string, categories: string[], nowSecs: number, offsetMinutes: number): boolean {
    if (categories.length === 0) return false;
    this.offsetMinutes = offsetMinutes;
    const localDay = Math.floor((nowSecs + offsetMinutes * 60) / 86_400);
    if (localDay !== this.seenDay) {
      this.seen.clear();
      this.seenDay = localDay;
    }
    let added = false;
    for (const category of categories) {
      // NUL-separated, as this counter has always been: a category never
      // contains one, so no `(item, category)` pair can collide with a
      // different pair whose item id happens to end in the separator.
      const key = `${itemId}\u0000${category}`;
      if (!this.seen.has(key)) {
        this.seen.add(key);
        this.pending.set(category, (this.pending.get(category) ?? 0) + 1);
        added = true;
      }
    }
    return added;
  }

  /** Drain the pending deltas if a batch is due — "batched (at most hourly)":
   *  at least `minIntervalSecs` between flushes, with the first report (no prior
   *  flush) eager. `null` when nothing is pending or the interval has not
   *  elapsed. */
  takeDue(nowSecs: number, minIntervalSecs: number): NotifyBatch | null {
    if (this.pending.size === 0) return null;
    if (this.lastFlush !== null && nowSecs - this.lastFlush < minIntervalSecs) return null;

    const entries = [...this.pending.entries()].map(([category, count]) => ({ category, count }));
    this.pending.clear();
    this.lastFlush = nowSecs;
    return { entries, offsetMinutes: this.offsetMinutes };
  }

  /** Drop this actor's counts and dedup state on an actor change.
   *
   *  Pending counts are dropped rather than flushed: they were accrued under the
   *  outgoing actor, and a Notify report carries no identity of its own, so
   *  draining them after a switch attributes one ward's enforcement to another.
   *  Guardian Notify is explicitly coarse and best-effort (§ Guardian Notify
   *  trust bound), so losing a partial bucket at a switch is within its
   *  contract — misattributing it would not be.
   *
   *  `seen` is the sharp edge, because it fails *silently*: carried across a
   *  switch it makes the incoming ward's first enforcement on an item the
   *  outgoing one already saw go uncounted — an under-report to their guardian
   *  that looks exactly like "nothing happened". */
  reset(): void {
    this.pending.clear();
    this.seen.clear();
    this.seenDay = Number.NEGATIVE_INFINITY;
    this.lastFlush = null;
    this.offsetMinutes = 0;
  }
}
