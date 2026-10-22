// The engagement-cue test clock's e2e commands — web's twins of tui's and
// linux's `feed_cue_clock_*` / `feed_cue_probe` arms, plus the pixel step the
// journeys scroll by (`feed_cue_scroll_by`, linux's twin). The clock itself is
// the shared tracker's (`fauna_feed::CueTracker` § The test clock), reached
// through `WasmCueTracker`'s `test-helpers` faces; this module only finds the
// live tracker and lands each step with a sample of its own.
//
// This module exists ONLY in builds made for testing (testing.md § convention
// 15): its one importer, `#lib/e2e-automation`, sits behind `+layout.svelte`'s
// `if (__FAUNA_E2E_AUTOMATION__)`, so a production `vite build` folds it away.
//
// Every reply is the shared shape: `{ok: true}` for a step, the `CueProbe`
// object for the probe, `{error}` for a refusal — a refused advance is loud,
// never a silent no-op (convention 11).

import { feedManagerIfReady } from '#lib/feed';
import { activeCueCapture, cueTrackerFor } from '#lib/feed-cues';
import { registerE2eCommands } from '#lib/e2e-commands';
import type { CueTrackerHandle } from '#lib/wasm';

/** `WasmCueTracker`'s `test-helpers` faces (`libs/fauna-wasm/src/feed.rs`). */
interface CueTrackerTestSeams {
  freezeClockForTest(monoNowMs: number, wallNowMs: number): void;
  advanceClockForTest(ms: number): void;
  thawClockForTest(): void;
  probeForTest(): { frozen: boolean } & Record<string, unknown>;
}

function seams(tracker: CueTrackerHandle): CueTrackerTestSeams {
  const t = tracker as unknown as Partial<CueTrackerTestSeams>;
  if (typeof t.probeForTest !== 'function') {
    throw new Error('WasmCueTracker has no test clock — wasm built without `test-helpers`');
  }
  return t as CueTrackerTestSeams;
}

/** The feed manager's tracker — the one every mount of the feed page samples
 *  into, so a freeze outlives a trip off the page, as on tui and linux. */
function liveTracker(): CueTrackerTestSeams {
  const manager = feedManagerIfReady();
  if (!manager) throw new Error('no feed cue capture is wired (pre-auth)');
  return seams(cueTrackerFor(manager));
}

/** One sample now, when the feed page is mounted — the step's barrier. Layout
 *  is synchronous in the browser, so the sample reads the scroll just made;
 *  it resolves once every exposure it ended is recorded, the Layer-A train and
 *  its put included, so a command's ack means nothing more is on its way. */
async function sampleNow(): Promise<void> {
  await activeCueCapture()?.sample();
}

const FEED_CUE_COMMANDS = [
  'feed_cue_clock_freeze',
  'feed_cue_clock_thaw',
  'feed_cue_clock_advance',
  'feed_cue_probe',
  'feed_cue_scroll_by',
] as const;

/** Register the cue-clock commands. Called once from `#lib/e2e-automation`. */
export function registerFeedCueCommands(): void {
  registerE2eCommands(FEED_CUE_COMMANDS, async (action, p) => {
    try {
      switch (action) {
        case 'feed_cue_clock_freeze':
          liveTracker().freezeClockForTest(performance.now(), Date.now());
          return { ok: true };
        case 'feed_cue_clock_thaw':
          liveTracker().thawClockForTest();
          return { ok: true };
        case 'feed_cue_clock_advance': {
          // Sample at once, so each step lands as exactly one sample's credit.
          liveTracker().advanceClockForTest(Number(p.ms ?? 0));
          await sampleNow();
          return { ok: true };
        }
        case 'feed_cue_probe': {
          // Frozen, a sample credits nothing — it only measures, so the probe
          // reads the layout as it stands, not the last tick's.
          const tracker = liveTracker();
          if (tracker.probeForTest().frozen) await sampleNow();
          return tracker.probeForTest();
        }
        case 'feed_cue_scroll_by': {
          // Scroll the post list by `px` (the user's own scroll position, which
          // the capture reads) and sample, answering with the probe as THAT
          // sample left it — a later read could already show the feed's
          // re-rank in answer to an exposure the step ended. `moved: false` at
          // the end of the list.
          const capture = activeCueCapture();
          if (!capture) throw new Error('the feed page is not mounted');
          const before = capture.container.scrollTop;
          capture.container.scrollTop = before + Number(p.px ?? 0);
          const landed = capture.sample();
          const probe = seams(capture.tracker).probeForTest();
          await landed;
          return { ok: true, moved: capture.container.scrollTop !== before, probe };
        }
      }
    } catch (e) {
      return { error: e instanceof Error ? e.message : String(e) };
    }
    throw new Error(`unhandled feed cue command ${action}`);
  });
}
