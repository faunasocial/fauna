// web's engagement-cue **capture shell** for the Feed post list
// (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation).
//
// This module is a geometry probe plus scheduling, lifecycle and emit, and
// nothing else. Every piece of bookkeeping above the probe — visibility
// bucketing, dwell credit and its stall cap, the hold-vs-leave policy, the
// single-sample noise floor, `is_media` stamping, observation assembly — is the
// shared `fauna_feed::CueTracker` behind `WasmCueTracker`, and all derivation
// below it is the feed manager's `CueEngine`. Do not add any of it here.
//
// - **Probe:** each feed card that shows its post (`data-cue-post` — muted,
//   blocked, collapsed and region placeholders carry none, so lingering on one
//   is no exposure) measured with `getBoundingClientRect`, against the scroll
//   container's rect clipped to the window. One coordinate space: the client
//   viewport's.
// - **Leave model:** `hold-unmeasured`. The feed renders every loaded post as a
//   retained DOM node (a plain `{#each}`, no virtual scroller), so a card
//   missing from a probe read is mid-rebuild — held, never read as gone.
// - **Scheduling:** the shared `cueSampleIntervalMs()` tick while the feed is
//   on screen, so dwell accrues while the user holds still, plus an extra
//   sample on every scroll frame.
// - **Lifecycle:** built with the page, stopped with it. Leaving the list —
//   the route unmounting, a post-detail or compose dialog covering it, the tab
//   going hidden — drains every tracked card (off-screen is off-viewport). The
//   route leaving and the tab going hidden also flush the rollup
//   (`flushCuesNow`, which the root layout calls beside its draft flushes).
//
// Two clocks by contract: `performance.now()` feeds dwell credit, `Date.now()`
// only stamps the observation. Feed media renders as stills with no playback
// surface, so `media_played_pm` is always `null`.

import type { WasmFeedManager } from '../../static/fauna_wasm.js';
import { feedManagerIfReady } from './feed';
import {
  cueSampleIntervalMs,
  logMessage,
  newCueTracker,
  type CueExposure,
  type CueRow,
  type CueTrackerHandle,
} from './wasm';

/** What the page tells the probe each sample: whether the post list is what
 *  the user is looking at, and every post id in the loaded window. */
export interface CueListView {
  showing: boolean;
  windowPostIds: string[];
}

/** The attribute a feed card carries only while it shows its post. */
export const CUE_POST_ATTR = 'data-cue-post';
/** The card's post `has_media`, as `"true"`/`"false"`. */
export const CUE_MEDIA_ATTR = 'data-cue-media';

let active: CueCapture | null = null;
const hydrated = new WeakMap<WasmFeedManager, Promise<void>>();

/** Fetch-on-session-start for the sealed `cues:v1` rollup, and the Layer-B
 *  opt-in cache beside it — ONCE per manager. `hydrateCues` replaces the live
 *  engine, so a second call would discard every verdict folded since the first;
 *  the feed page re-mounting must not re-run it. A failed hydrate is forgotten,
 *  so the next mount retries, and rejects: the caller surfaces it and does not
 *  capture into an unhydrated engine. */
export function hydrateCuesOnce(manager: WasmFeedManager): Promise<void> {
  let done = hydrated.get(manager);
  if (!done) {
    done = manager.hydrateCues().then(() => {
      void manager.hydrateSignalOptin().catch((e: unknown) =>
        logMessage('warn', 'fauna_web::feed_cues', `signal opt-in hydrate failed: ${e}`),
      );
    });
    done.catch(() => hydrated.delete(manager));
    hydrated.set(manager, done);
  }
  return done;
}

export class CueCapture {
  private readonly tracker: CueTrackerHandle;
  private timer: ReturnType<typeof setInterval> | null = null;
  private frame: number | null = null;
  /** Whether the previous sample found the list on screen — turns "not on
   *  screen now" into a leave edge exactly once. */
  private showing = false;
  private stopped = false;
  private readonly onScroll = () => {
    if (this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.sample();
    });
  };

  constructor(
    private readonly manager: WasmFeedManager,
    private readonly container: HTMLElement,
    private readonly view: () => CueListView,
    private readonly onError: (message: string) => void,
  ) {
    this.tracker = newCueTracker('hold-unmeasured');
  }

  start(): void {
    this.timer = setInterval(() => this.sample(), cueSampleIntervalMs());
    this.container.addEventListener('scroll', this.onScroll, { passive: true });
    active = this;
  }

  /** One probe read. */
  sample(): void {
    if (this.stopped) return;
    const { showing, windowPostIds } = this.view();
    if (!showing || document.visibilityState === 'hidden') {
      if (this.showing) {
        this.showing = false;
        void this.emit(this.tracker.drainAll(Date.now()));
      }
      return;
    }
    this.showing = true;
    const box = this.container.getBoundingClientRect();
    const viewportStart = Math.max(box.top, 0);
    const viewportEnd = Math.min(box.bottom, window.innerHeight);
    const rows: CueRow[] = [];
    for (const card of this.container.querySelectorAll<HTMLElement>(`[${CUE_POST_ATTR}]`)) {
      const rect = card.getBoundingClientRect();
      rows.push({
        post_id: card.getAttribute(CUE_POST_ATTR)!,
        top: rect.top,
        height: rect.height,
        is_media: card.getAttribute(CUE_MEDIA_ATTR) === 'true',
        media_played_pm: null,
      });
    }
    void this.emit(
      this.tracker.sample(rows, windowPostIds, viewportStart, viewportEnd, performance.now(), Date.now()),
    );
  }

  /** Everything tracked has left: drain, report, then put the rollup. */
  async leave(): Promise<void> {
    const left = this.stopped ? [] : this.tracker.drainAll(Date.now());
    this.showing = false;
    await this.emit(left);
    await flushRollup(this.manager);
  }

  /** The page is going away: stop sampling, then leave. */
  async stop(): Promise<void> {
    if (this.stopped) return;
    if (this.timer !== null) clearInterval(this.timer);
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.container.removeEventListener('scroll', this.onScroll);
    if (active === this) active = null;
    const left = this.tracker.drainAll(Date.now());
    this.stopped = true;
    this.tracker.free();
    await this.emit(left);
    await flushRollup(this.manager);
  }

  private async emit(exposures: CueExposure[]): Promise<void> {
    await Promise.all(
      exposures.map((o) =>
        this.manager
          .recordObservation(
            o.content_id,
            o.is_media,
            o.media_played_pm ?? undefined,
            BigInt(o.dwell_ms_at_skip_visibility),
            BigInt(o.dwell_ms_at_long_visibility),
            BigInt(o.observed_at_ms),
          )
          .catch((e: unknown) => this.onError(String(e))),
      ),
    );
  }
}

async function flushRollup(manager: WasmFeedManager): Promise<void> {
  try {
    await manager.flushCues();
  } catch (e) {
    logMessage('warn', 'fauna_web::feed_cues', `cue rollup flush failed: ${e}`);
  }
}

/** The tab-leave flush (`visibilitychange` → hidden, `pagehide`): end every
 *  exposure the live capture holds, then put the rollup. Off the feed there is
 *  nothing to drain, but verdicts folded earlier may still be waiting out the
 *  put debounce, so the flush runs whenever a manager exists. Best-effort, like
 *  every handler on those events. */
export function flushCuesNow(): void {
  if (active) {
    void active.leave();
    return;
  }
  const manager = feedManagerIfReady();
  if (manager) void flushRollup(manager);
}
