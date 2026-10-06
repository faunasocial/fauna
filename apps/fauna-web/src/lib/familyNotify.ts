// Ward-side Guardian Notify counter (family-safety.md § Guardian Notify) — the web
// twin of linux `content_policy.rs`'s NotifyAccumulator. Counts the viewer's own
// GUARDIAN-floor render-enforcement events per category (deduped per item per local
// day) and batches them for `fauna.family.notify_report` (category + count, NEVER a
// content id), flushing at most once per the shared NOTIFY_REPORT_MIN_INTERVAL_SECS.
//
// *Which* categories count is the shared `guardianEnforcedCategories` (only the
// guardian floor, never the ward's own thresholds), so web and linux never drift on
// what Notify reports. State + timer live here (the moderation.ts LabelBuffer shape);
// the drift-critical decision is shared Rust over wasm.

import { guardianEnforcedCategories, notifyReportMinIntervalSecs, type ContentLabelEntry } from './wasm';
import { contentNotifyOn, guardianContentPolicy } from './contentPolicy.svelte';
import { familyNotifyReport, type FamilyContentNotice } from './rpc';
import { registerActorScopedReset } from './actorScope';
import { NotifyBufferState } from './familyNotifyBuffer';
import { utcOffsetMinutes } from './utcOffset';

// How often the flush timer wakes to check whether a report is due. The report
// itself fires at most hourly (the interval gate below); this only bounds the
// latency of the first report after the ward flags something, so the check is cheap
// and the send is rare.
const CHECK_INTERVAL_MS = 5_000;

function nowSecs(): number {
  return Math.floor(Date.now() / 1000);
}

class NotifyBuffer {
  private secretHex: string | null = null;
  /** The counter's rules (dedup, day boundary, batching, the actor-change drop)
   *  live in a dependency-free module so `deno test` can reach them — this class
   *  owns only what needs the SPA: the wasm category decision, the RPC send and
   *  the flush timer. */
  private state = new NotifyBufferState();
  private timer: ReturnType<typeof setInterval> | null = null;

  /** Set the identity used to sign notify reports. Call on login/hydrate. */
  setIdentity(secretHex: string): void {
    this.secretHex = secretHex;
  }

  /** Count any GUARDIAN-floor enforcement on `itemId` (a feed post / DM message).
   *  A no-op unless the ward's content_notify knob is on AND the guardian floor
   *  bites on one of this item's labels. Deduped per item per local day, so a
   *  re-render never re-counts. */
  record(itemId: string, labels: ContentLabelEntry[] | undefined): void {
    if (!contentNotifyOn()) return;
    const cats = guardianEnforcedCategories(labels ?? [], guardianContentPolicy());
    if (cats.length === 0) return;

    if (this.state.record(itemId, cats, nowSecs(), utcOffsetMinutes())) {
      this.ensureTimer();
    }
  }

  private ensureTimer(): void {
    if (this.timer) return;
    // Under the Playwright e2e agent, don't arm the real interval at all — the
    // first flush is eager (below), so a live 5s tick racing a multi-step test
    // (WS-RPC round trips, page loads) would flush before the test can reach its
    // own switch step, exactly the wall-clock race testing.md convention 14
    // forbids. Tests drive checks explicitly via the `family_notify_check_now`
    // e2e command (`$lib/family-notify-e2e`), which calls `flushIfDue()` directly
    // — the same "hasAgent" idiom as `conversations.ts`'s `pollIntervalMs()`.
    const hasAgent =
      __FAUNA_E2E_AUTOMATION__ &&
      typeof window !== 'undefined' &&
      (window as unknown as { __faunaTestAgent?: unknown }).__faunaTestAgent != null;
    if (hasAgent) return;
    this.timer = setInterval(() => this.flushIfDue(), CHECK_INTERVAL_MS);
  }

  /** Force an immediate due-check, bypassing `CHECK_INTERVAL_MS`'s wall-clock
   *  wait — the e2e test-hook poke (testing.md convention 14): the real cadence
   *  gate (`notifyReportMinIntervalSecs()`) still applies, only the interval that
   *  decides WHEN to check is skipped. Driven by the `family_notify_check_now`
   *  e2e command; harmless to call outside a test (a no-op unless something is
   *  pending). */
  checkNow(): void {
    this.flushIfDue();
  }

  private flushIfDue(): void {
    // No identity, nothing to sign the report with — and after an actor change
    // that is the *only* correct outcome, since the counts were another ward's.
    if (!this.secretHex) return;
    const batch = this.state.takeDue(nowSecs(), notifyReportMinIntervalSecs());
    if (!batch) return;

    const entries: FamilyContentNotice[] = batch.entries.map(({ category, count }) => ({
      category,
      count,
    }));

    // Fire-and-forget best-effort telemetry (a modified client under-reports —
    // family-safety.md § Guardian Notify trust bound).
    familyNotifyReport(this.secretHex, entries, batch.offsetMinutes).catch(() => {
      // Silently drop — coarse, best-effort.
    });
  }

  /** Clean up the timer on page unmount; attempt a final due flush. */
  destroy(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
    this.flushIfDue();
  }

  /** Drop this actor's buffered counts + dedup state on an actor change
   *  (`actorScope.ts`).
   *
   *  Everything held here is one ward's enforcement history, so it is
   *  account-scoped class-1 data under the isolation contract. The `seen` set is
   *  the sharp edge: it is a per-item/per-local-day dedup, so carrying it across a
   *  switch makes the incoming ward's first enforcement on an item the outgoing
   *  one already saw silently *uncounted* — an under-report to their guardian that
   *  looks exactly like "nothing happened".
   *
   *  Pending counts are dropped rather than flushed: they were accrued under the
   *  outgoing actor and `secretHex` is cleared with them, so there is no identity
   *  left to sign that report with. Guardian Notify is explicitly coarse and
   *  best-effort (family-safety.md § Guardian Notify trust bound), so losing a
   *  partial bucket at a switch is within its contract — attributing it to the
   *  wrong actor would not be. */
  resetForActorChange(): void {
    this.secretHex = null;
    this.state.reset();
  }
}

/** Singleton Guardian Notify buffer, shared across the social surfaces. */
export const notifyBuffer = new NotifyBuffer();

registerActorScopedReset(() => notifyBuffer.resetForActorChange());
