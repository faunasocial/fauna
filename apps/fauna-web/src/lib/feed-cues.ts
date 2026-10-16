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
// - **Lifecycle:** built with the page, stopped with it — over ONE tracker per
//   feed manager (`cueTrackerFor`), as tui and linux keep one per auth: every
//   leave drains it empty, so a re-mount starts clean, and the frozen test
//   clock (`$lib/feed-cues-e2e`) survives a trip off the page. Leaving the list —
//   the route unmounting, a post-detail or compose dialog covering it, the tab
//   going hidden — drains every tracked card (off-screen is off-viewport). The
//   route leaving and the tab going hidden also flush the rollup
//   (`flushCuesNow`, which the root layout calls beside its draft flushes).
//
// Two clocks by contract: `performance.now()` feeds dwell credit, `Date.now()`
// only stamps the observation.
//
// - **Playback:** an inline `VideoThumbnail` player notes its progress
//   (`notePlayback`, on `timeupdate` and `ended`) into a per-post unread peak;
//   each sample takes it for the post's row (`takePlayedPm`) — the contract on
//   the shared `CueRow::media_played_pm` — so a video played to the end derives
//   the media `watch-complete`.

import type { WasmFeedManager } from '../../static/fauna_wasm.js';
import { feedManagerIfReady } from './feed';
import {
  cuePlayedPerMille,
  cueSampleIntervalMs,
  logMessage,
  newCueTracker,
  type CueExposure,
  type CueRow,
  type CueTrackerHandle,
} from './wasm';

/** Each post's highest played per-mille since the probe last took it. */
const unreadPlayedPm = new Map<string, number>();

/** An inline player's progress report: `position` and `duration` in its own
 *  unit (a `<video>`'s seconds). Folded by max into the post's unread peak. */
export function notePlayback(postId: string, position: number, duration: number): void {
  const pm = cuePlayedPerMille(position, duration);
  if (pm === null) return;
  unreadPlayedPm.set(postId, Math.max(pm, unreadPlayedPm.get(postId) ?? 0));
}

/** The probe's read: the peak played on `postId` since the last take, cleared
 *  by the take; `null` when nothing played since. */
export function takePlayedPm(postId: string): number | null {
  const pm = unreadPlayedPm.get(postId);
  unreadPlayedPm.delete(postId);
  return pm ?? null;
}

/** What the page tells the probe each sample: whether the post list is what
 *  the user is looking at, and every post id in the loaded window. */
export interface CueListView {
  showing: boolean;
  windowPostIds: string[];
}

/** The attribute a feed card carries only while it shows its post. */
export const CUE_POST_ATTR = 'data-cue-post';
/** The card's post `plays_media` (never `has_media` — a still image is a
 *  non-media post), as `"true"`/`"false"`. */
export const CUE_MEDIA_ATTR = 'data-cue-media';

let active: CueCapture | null = null;
const hydrated = new WeakMap<WasmFeedManager, Promise<void>>();
const trackers = new WeakMap<WasmFeedManager, CueTrackerHandle>();

/** The manager's one capture tracker, built on first use. */
export function cueTrackerFor(manager: WasmFeedManager): CueTrackerHandle {
  let tracker = trackers.get(manager);
  if (!tracker) {
    tracker = newCueTracker('hold-unmeasured');
    trackers.set(manager, tracker);
  }
  return tracker;
}

/** The capture sampling the mounted feed page, if any. */
export function activeCueCapture(): CueCapture | null {
  return active;
}

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
  readonly tracker: CueTrackerHandle;
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
      void this.sample();
    });
  };

  constructor(
    private readonly manager: WasmFeedManager,
    readonly container: HTMLElement,
    private readonly view: () => CueListView,
    private readonly onError: (message: string) => void,
  ) {
    this.tracker = cueTrackerFor(manager);
  }

  start(): void {
    this.timer = setInterval(() => void this.sample(), cueSampleIntervalMs());
    this.container.addEventListener('scroll', this.onScroll, { passive: true });
    active = this;
  }

  /** One probe read. Resolves once every exposure it ended is recorded — the
   *  Layer-A train and its put included; the ticks never wait on it. */
  sample(): Promise<void> {
    if (this.stopped) return Promise.resolve();
    const { showing, windowPostIds } = this.view();
    if (!showing || document.visibilityState === 'hidden') {
      if (this.showing) {
        this.showing = false;
        return this.emit(this.tracker.drainAll(Date.now()));
      }
      return Promise.resolve();
    }
    this.showing = true;
    const box = this.container.getBoundingClientRect();
    const viewportStart = Math.max(box.top, 0);
    const viewportEnd = Math.min(box.bottom, window.innerHeight);
    const rows: CueRow[] = [];
    for (const card of this.container.querySelectorAll<HTMLElement>(`[${CUE_POST_ATTR}]`)) {
      const rect = card.getBoundingClientRect();
      const postId = card.getAttribute(CUE_POST_ATTR)!;
      const isMedia = card.getAttribute(CUE_MEDIA_ATTR) === 'true';
      rows.push({
        post_id: postId,
        top: rect.top,
        height: rect.height,
        is_media: isMedia,
        media_played_pm: isMedia ? takePlayedPm(postId) : null,
      });
    }
    return this.emit(
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
    await this.emit(left);
    await flushRollup(this.manager);
  }

  private async emit(exposures: CueExposure[]): Promise<void> {
    await Promise.all(
      exposures.map((o) =>
        this.manager
          .recordObservation(o)
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
