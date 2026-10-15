//! The shared engagement-cue **capture** tracker — everything between a
//! client's geometry probe and [`crate::cues::CueEngine`]
//! (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation).
//!
//! **Boundary (revised 2026-07-29 — one layer lower than the 2026-07-12
//! original).** A capture shell owns only what is genuinely platform-bound: the
//! per-row geometry probe (each row's `(post_id, top, height)` plus the viewport
//! bounds, all in any one coordinate space), the sampling-tick + extra-sample-
//! on-scroll scheduling, lifecycle (construct on wire-up, drain on leave), and
//! handing the returned [`CueObservation`]s to the manager. Everything else —
//! visibility bucketing, per-sample credit arithmetic with its stall cap, the
//! hold-vs-leave policy, the single-sample noise floor, `is_media` stamping,
//! `CueObservation` assembly — is [`CueTracker`], here, once. A shell MUST NOT
//! bucket dwell at any fraction itself.
//!
//! *Why the revision:* the original line was drawn before any shell existed.
//! Four per-app reimplementations of the identical arithmetic then accumulated
//! (linux/windows/android/apple, byte-identical constants, hand-mirrored doc
//! comments) — and had already drifted on a semantic point: windows and android
//! fed the **wall** clock into dwell credit, linux and apple the **monotonic**
//! clock, so a date change or NTP step inflated dwell on exactly two of four
//! platforms. The drift the original rule existed to prevent was happening one
//! layer below where it drew the line.
//!
//! **Two clocks, by contract.** `mono_now_ms` feeds credit — an NTP step or a
//! user date change can never inflate dwell. `wall_now_ms` only stamps
//! [`CueObservation::observed_at_ms`], which the engine reads for pacing and the
//! rollup stores as `last_at`. They are separate parameters precisely so a shell
//! cannot accidentally pass one clock for both.
//!
//! **The one genuine platform divergence is typed, not prose** — see
//! [`LeaveModel`]. Native shells reach this type through
//! `fauna_ffi::FfiCueTracker`, web through `fauna_wasm::WasmCueTracker`; the
//! Rust-native linux shell constructs it directly.

use std::collections::{HashMap, HashSet};

use fauna_core::scoring::cues::{
    CUE_LONG_DWELL_VISIBLE_PM, CUE_MAX_SAMPLE_CREDIT_MS, CUE_MIN_VISIBLE_SAMPLES,
    CUE_SKIP_VISIBLE_PM,
};

use crate::cues::CueObservation;

/// How a shell's list container signals that a tracked row has left the
/// viewport. The one genuine platform divergence in cue capture, typed so it
/// cannot be re-derived (or silently mis-copied) per client.
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveModel {
    /// **Eager / retained containers** — every row keeps a live view regardless
    /// of scroll position, so there is no container-disposal signal to read. A
    /// tracked row has left only on *positive* evidence: it was measured below
    /// skip-visibility, or its post is gone from the loaded window entirely (a
    /// feed switch). A row that merely could not be measured this sample is
    /// mid-layout, not gone — it is HELD, uncredited, never flushed. Treating
    /// "unmeasurable" as "left" would fragment a real dwell across rebuilds and
    /// fabricate sub-`CUE_SKIP_MS` exposures the engine derives as skips the
    /// user never made.
    ///
    /// Used by linux's GTK `ListBox`, windows' non-virtualizing `StackPanel`,
    /// apple's eager `ScrollView { VStack }`.
    HoldUnmeasured,
    /// **Virtualizing containers** — a row scrolled well away is *disposed* and
    /// simply absent from the layout, while its post is still in the loaded
    /// window. Under [`LeaveModel::HoldUnmeasured`] that row would be held
    /// forever and never emit its dwell, so here absence from a **non-empty**
    /// measured set IS positive leave-evidence.
    ///
    /// The "non-empty" qualifier preserves the same invariant the other model
    /// states directly: measuring *nothing* (no layout yet, or mid-measure) is
    /// "unmeasurable", not "everything left".
    ///
    /// Used by android's Compose `LazyColumn`.
    AbsenceIsLeave,
}

/// How a tracked exposure ended — stamped by the tracker into
/// [`CueObservation::ended_by`], read by [`crate::cues::CueEngine`]: a `skip`
/// derives only from [`ExposureEnd::ScrolledPast`]
/// (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation, the
/// 2026-10-10 ruling). A shell never chooses it — it relays what the tracker
/// emitted, because it has no honest way of its own to know how an exposure
/// ended.
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExposureEnd {
    /// The user moved on: positively measured below skip-visibility, or — under
    /// [`LeaveModel::AbsenceIsLeave`] — disposed by the virtualizing container
    /// while its post is still in the loaded window.
    ScrolledPast,
    /// Its post left the loaded window (a feed switch, a refresh that dropped
    /// it, a mute or a delete) — checked before either leave model's own arm.
    WindowDropped,
    /// [`CueTracker::drain_all`]: the page left, the tab hid, a detail or
    /// compose dialog covered the list, the app quit.
    Drained,
}

/// One row of a shell's geometry probe: the row's post identity, its position
/// and rendered size along the scroll axis, and what the shell's live post
/// window says about it.
///
/// `top`/`height` are in the **same coordinate space** as the `viewport_start`/
/// `viewport_end` passed alongside — whichever space that is (a GTK
/// `compute_bounds` against the `ScrolledWindow`, a WinUI `TransformToVisual`,
/// a Compose `LazyListLayoutInfo` main axis, a SwiftUI anchor preference). The
/// tracker never assumes the viewport starts at zero.
///
/// A row the shell could not measure at all (no bounds yet) is simply **omitted**
/// from the probe read; a row it measured as *not yet arranged* (non-positive
/// `height`) is included and handled as unmeasurable here. Both are held, in
/// both leave models — the shell does not have to know the difference.
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CueRow {
    /// The row's own post identity — read from the row itself (its key / test
    /// attribute), NEVER an index-into-the-list read, which mid-rebuild
    /// attributes one post's dwell to another.
    pub post_id: String,
    /// The row's leading edge along the scroll axis.
    pub top: f64,
    /// The row's rendered extent along the scroll axis. Non-positive means "not
    /// yet arranged" — unmeasurable, never a fabricated 0-visibility reading.
    pub height: f64,
    /// Whether the row's post is playable media, from the shell's live post
    /// window — the snapshot's `PostSummary::plays_media`, never `has_media`
    /// (a still image is a non-media post). The exposure keeps the latest
    /// visible sample's reading.
    pub is_media: bool,
    /// Peak playback fraction reached so far, per-mille, for a media row; `None`
    /// for a non-media row or one that never played. The tracker folds the
    /// **maximum** across the exposure's samples, which is exactly
    /// [`CueObservation::media_played_pm`]'s "peak reached".
    ///
    /// **What a shell passes: the peak its inline player reached since the
    /// previous sample, taken.** The player notes every position it reports
    /// (through [`fauna_core::scoring::cues::played_per_mille`], never a local
    /// clamp), its end included, into a per-post "unread" maximum; the probe
    /// takes that value for the row (clearing it) and passes `None` when the
    /// player made no progress since. Taking, rather than re-passing the
    /// player's lifetime peak, is what keeps one play from crediting a second
    /// exposure when the reader scrolls back to a video they already finished
    /// (completions accumulate); noting on the player's own progress events,
    /// rather than reading the position at the tick, is what catches the last
    /// frame of a play that ends between two ticks. The fold here turns the
    /// takes back into the exposure's peak.
    pub media_played_pm: Option<u32>,
}

/// One tracked row's accumulated exposure — the side-table value. Private: a
/// shell only ever sees the finished [`CueObservation`].
#[derive(Debug)]
struct DwellState {
    is_media: bool,
    dwell_ms_at_skip_visibility: u64,
    dwell_ms_at_long_visibility: u64,
    /// Samples that measured this row at/above skip-visibility. The noise floor
    /// ([`CUE_MIN_VISIBLE_SAMPLES`]) reads it at emit.
    visible_samples: u32,
    media_played_pm: Option<u32>,
}

/// The dwell side-table (`post_id` → accumulated exposure) plus the ONE shared
/// previous-sample instant the elapsed credit derives from — a single clock for
/// the whole table, not one per entry: a row that re-enters mid-session inherits
/// whatever credit has accrued since the last global tick.
///
/// Constructed once per feed wire-up with the container's [`LeaveModel`], fed
/// one probe read per tick, drained on page leave. Pure arithmetic over
/// primitives — no I/O, no clock of its own, no manager coupling (the tracker is
/// synchronous 4 Hz arithmetic; the manager is async and locked — the shell's
/// glue bridges them).
#[derive(Debug)]
pub struct CueTracker {
    leave_model: LeaveModel,
    entries: HashMap<String, DwellState>,
    /// The previous sample's **monotonic** reading. `None` means the credit
    /// baseline is unset — the next sample credits nothing. Reset by an empty
    /// probe read and by [`CueTracker::drain_all`], so credit only ever spans
    /// between two successful measurements.
    last_sample_mono_ms: Option<u64>,
    /// The e2e clock seam (§ *The test clock*, below). `None` in every
    /// ordinary run; absent from a release build.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    test_clock: Option<FrozenClock>,
    /// The last sample's probe read and viewport, for
    /// [`CueTracker::probe_for_test`].
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    last_probe: Option<(Vec<CueRow>, f64, f64)>,
}

/// The two readings a frozen tracker uses in place of the ones the shell
/// passes: `mono_ms` on the shell's own monotonic scale, `wall_ms` epoch
/// milliseconds.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrozenClock {
    mono_ms: u64,
    wall_ms: u64,
}

impl CueTracker {
    /// A tracker for a container with the given leave model.
    pub fn new(leave_model: LeaveModel) -> Self {
        CueTracker {
            leave_model,
            entries: HashMap::new(),
            last_sample_mono_ms: None,
            #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
            test_clock: None,
            #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
            last_probe: None,
        }
    }

    /// The container's leave model (the shell fixes it at construction).
    pub fn leave_model(&self) -> LeaveModel {
        self.leave_model
    }

    /// How many rows are currently tracked — a shell-facing diagnostic only; the
    /// bookkeeping itself is entirely internal.
    pub fn tracked(&self) -> usize {
        self.entries.len()
    }

    /// The post ids of the exposures in progress — every row last measured at
    /// or above skip-visibility and not yet emitted — sorted. A shell-facing
    /// diagnostic like [`CueTracker::tracked`]: it is how a test watches an
    /// exposure begin and end without reading the bookkeeping.
    pub fn tracked_post_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.entries.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// One honest sample: credit elapsed monotonic time to every row measured
    /// at/above each visibility fraction, then emit + forget every tracked row
    /// that positively left the viewport.
    ///
    /// `rows` is the shell's probe read for this tick; `window_post_ids` is every
    /// post currently in the loaded window (not just the realized rows) — the
    /// left-the-window check under [`LeaveModel::HoldUnmeasured`].
    ///
    /// Returns the finished exposures, already past the single-sample noise
    /// floor: the shell hands each straight to `FeedManager::record_observation`.
    pub fn sample(
        &mut self,
        rows: &[CueRow],
        window_post_ids: &[String],
        viewport_start: f64,
        viewport_end: f64,
        mono_now_ms: u64,
        wall_now_ms: u64,
    ) -> Vec<CueObservation> {
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        let (mono_now_ms, wall_now_ms) = self.clock_readings(mono_now_ms, wall_now_ms);
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        {
            self.last_probe = Some((rows.to_vec(), viewport_start, viewport_end));
        }
        // Bucket every MEASURABLE row. A non-positive height is "not yet
        // arranged" — unmeasurable, never a fabricated 0-visibility reading that
        // would flush a row merely mid-layout.
        let mut measured: HashMap<&str, u32> = HashMap::with_capacity(rows.len());
        for row in rows {
            if row.height <= 0.0 {
                continue;
            }
            measured.insert(
                row.post_id.as_str(),
                visible_fraction_pm(row.top, row.height, viewport_start, viewport_end),
            );
        }

        // Credit is measurement-bracketed: it only ever spans between two
        // successful reads, so a gap in which nothing painted is never dwell.
        let credit_ms = if measured.is_empty() {
            self.last_sample_mono_ms = None;
            0
        } else {
            let credit = self
                .last_sample_mono_ms
                .map(|prev| {
                    mono_now_ms
                        .saturating_sub(prev)
                        .min(CUE_MAX_SAMPLE_CREDIT_MS)
                })
                .unwrap_or(0);
            self.last_sample_mono_ms = Some(mono_now_ms);
            credit
        };

        // Measuring nothing is not evidence that everything was disposed.
        if measured.is_empty() && self.leave_model == LeaveModel::AbsenceIsLeave {
            return Vec::new();
        }

        for row in rows {
            let Some(&fraction_pm) = measured.get(row.post_id.as_str()) else {
                continue;
            };
            if fraction_pm < CUE_SKIP_VISIBLE_PM {
                continue;
            }
            let entry = self
                .entries
                .entry(row.post_id.clone())
                .or_insert_with(|| DwellState {
                    is_media: row.is_media,
                    dwell_ms_at_skip_visibility: 0,
                    dwell_ms_at_long_visibility: 0,
                    visible_samples: 0,
                    media_played_pm: None,
                });
            // The latest reading wins: a post's media-ness only becomes better
            // known (`PostSummary::plays_media` reads an unresolved attachment
            // as playable until its media resolves), so an image post first
            // sighted before its resolve still ends on the dwell arm.
            entry.is_media = row.is_media;
            entry.visible_samples += 1;
            entry.dwell_ms_at_skip_visibility += credit_ms;
            if fraction_pm >= CUE_LONG_DWELL_VISIBLE_PM {
                entry.dwell_ms_at_long_visibility += credit_ms;
            }
            entry.media_played_pm = max_opt(entry.media_played_pm, row.media_played_pm);
        }

        // Rows the shell reported but could not measure are held under BOTH
        // leave models — the probe saw them, so they were not disposed.
        let unmeasurable: HashSet<&str> = rows
            .iter()
            .filter(|r| r.height <= 0.0)
            .map(|r| r.post_id.as_str())
            .collect();
        let window: HashSet<&str> = window_post_ids.iter().map(String::as_str).collect();

        let gone: Vec<(String, ExposureEnd)> = self
            .entries
            .keys()
            .filter_map(|id| {
                let key = id.as_str();
                let end = match measured.get(key) {
                    // Positively measured out of the viewport.
                    Some(&fraction) if fraction < CUE_SKIP_VISIBLE_PM => ExposureEnd::ScrolledPast,
                    Some(_) => return None,
                    None if unmeasurable.contains(key) => return None,
                    // Its post left the loaded window entirely (a feed switch,
                    // a refresh, a mute) — checked before either leave model's
                    // arm, so a disposal-by-switch is never read as a scroll.
                    None if !window.contains(key) => ExposureEnd::WindowDropped,
                    None => match self.leave_model {
                        // Still loaded, merely unmeasured: mid-layout, held.
                        LeaveModel::HoldUnmeasured => return None,
                        // Absent from a non-empty measured set while still
                        // loaded ⇒ the container disposed it: scrolled away.
                        LeaveModel::AbsenceIsLeave => ExposureEnd::ScrolledPast,
                    },
                };
                Some((id.clone(), end))
            })
            .collect();

        let left: Vec<(String, DwellState, ExposureEnd)> = gone
            .into_iter()
            .filter_map(|(id, end)| self.entries.remove_entry(&id).map(|(id, d)| (id, d, end)))
            .collect();
        finish(left, wall_now_ms)
    }

    /// Everything tracked has left the viewport (the page unmapped, navigated
    /// away, or the observer was cancelled) — drain, emit, and reset the credit
    /// baseline so a later reuse of this same tracker never credits the
    /// elapsed-while-away gap as dwell.
    pub fn drain_all(&mut self, wall_now_ms: u64) -> Vec<CueObservation> {
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        let (_, wall_now_ms) = self.clock_readings(0, wall_now_ms);
        self.last_sample_mono_ms = None;
        let all: Vec<(String, DwellState, ExposureEnd)> = self
            .entries
            .drain()
            .map(|(id, d)| (id, d, ExposureEnd::Drained))
            .collect();
        finish(all, wall_now_ms)
    }
}

/// # The test clock
///
/// A skip is an exposure *shorter* than `CUE_SKIP_MS`, and a fast-scroll burst
/// is two exposures ending *less* than `CUE_BURST_MIN_GAP_MS` apart — upper
/// bounds on time, which a loaded box can only break. A long dwell is a lower
/// bound, and a loaded box breaks that too: the per-sample credit cap
/// ([`CUE_MAX_SAMPLE_CREDIT_MS`]) is the product's own refusal to count a
/// stalled main loop as dwell, so a wall-clock hold on a busy machine earns
/// less than it waited. So an e2e witness of any of the three cannot read the
/// real clocks (`e2e-latency-independent-assertions.md`, convention 14:
/// advance the clock, never sleep). These seams freeze BOTH readings the
/// tracker uses — the monotonic one that credits dwell and the wall one the
/// engine's pace gate reads (`observed_at_ms`) — and move them only when the
/// test says, by exact amounts. Everything else is real: the shell's geometry
/// probe, its scroll, the tracker's bookkeeping, the engine.
///
/// The seam lives here, beside the bookkeeping, so every capture shell gets it
/// the same way: a shell keeps passing its real readings, and a frozen tracker
/// substitutes its own. A frozen clock credits nothing between advances: a row
/// accrues exactly the time the test advanced while it was in view. One
/// sample's credit is capped, so [`CueTracker::advance_clock_for_test`] refuses
/// a larger step rather than let the excess vanish — and each step needs a
/// sample of its own to land (tui draws after every agent request; linux's
/// advance command samples itself).
///
/// The freeze lives on this tracker, so a re-auth (which rebuilds the shell's
/// capture) thaws it.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl CueTracker {
    fn clock_readings(&self, mono_now_ms: u64, wall_now_ms: u64) -> (u64, u64) {
        match self.test_clock {
            Some(clock) => (clock.mono_ms, clock.wall_ms),
            None => (mono_now_ms, wall_now_ms),
        }
    }

    /// Freeze both readings at the shell's current real ones. Idempotent: a
    /// second freeze keeps the clocks where the test left them.
    pub fn freeze_clock_for_test(&mut self, mono_now_ms: u64, wall_now_ms: u64) {
        if self.test_clock.is_none() {
            self.test_clock = Some(FrozenClock {
                mono_ms: mono_now_ms,
                wall_ms: wall_now_ms,
            });
        }
    }

    /// Move both frozen readings forward by `ms`. Refused when nothing is
    /// frozen or when `ms` exceeds one sample's credit cap.
    pub fn advance_clock_for_test(&mut self, ms: u64) -> Result<(), String> {
        let Some(clock) = self.test_clock.as_mut() else {
            return Err("the cue clock is not frozen".to_string());
        };
        if ms > CUE_MAX_SAMPLE_CREDIT_MS {
            return Err(format!(
                "one advance of {ms} ms exceeds the per-sample credit cap of \
                 {CUE_MAX_SAMPLE_CREDIT_MS} ms; advance in steps"
            ));
        }
        clock.mono_ms += ms;
        clock.wall_ms += ms;
        Ok(())
    }

    /// Back to the shell's real readings. The monotonic reading jumps to real
    /// time, so the next sample's credit is capped like any stall.
    pub fn thaw_clock_for_test(&mut self) {
        self.test_clock = None;
    }

    /// Whether the clock is frozen.
    pub fn clock_frozen_for_test(&self) -> bool {
        self.test_clock.is_some()
    }

    /// What the tracker holds right now, for a test to steer its scrolling by
    /// and to read an exposure's progress from: the exposures in progress with
    /// the credit each has accrued, and the rows and viewport the last sample
    /// measured, in the shell's coordinate space.
    pub fn probe_for_test(&self) -> CueProbe {
        let (cards, band) = match &self.last_probe {
            Some((rows, start, end)) => (
                rows.iter()
                    .map(|r| CueProbeCard {
                        post_id: r.post_id.clone(),
                        top: r.top,
                        height: r.height,
                    })
                    .collect(),
                Some((*start, *end)),
            ),
            None => (Vec::new(), None),
        };
        let mut exposures: Vec<CueProbeExposure> = self
            .entries
            .iter()
            .map(|(id, d)| CueProbeExposure {
                post_id: id.clone(),
                dwell_ms_at_skip_visibility: d.dwell_ms_at_skip_visibility,
                dwell_ms_at_long_visibility: d.dwell_ms_at_long_visibility,
                visible_samples: d.visible_samples,
                media_played_pm: d.media_played_pm,
            })
            .collect();
        exposures.sort_by(|a, b| a.post_id.cmp(&b.post_id));
        CueProbe {
            frozen: self.test_clock.is_some(),
            tracked: self.tracked_post_ids(),
            band,
            cards,
            exposures,
        }
    }
}

/// [`CueTracker::probe_for_test`]'s reading — the one shape every app's
/// `feed_cue_probe` agent command publishes.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CueProbe {
    /// Whether the test clock is frozen.
    pub frozen: bool,
    /// The exposures in progress (at least half in view at their last
    /// measurement), sorted.
    pub tracked: Vec<String>,
    /// The last sample's viewport `[start, end]`; `None` before any sample.
    pub band: Option<(f64, f64)>,
    /// The rows the last sample measured.
    pub cards: Vec<CueProbeCard>,
    /// The credit each exposure in progress has accrued, sorted by post id.
    pub exposures: Vec<CueProbeExposure>,
}

/// One measured row of a [`CueProbe`].
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CueProbeCard {
    pub post_id: String,
    pub top: f64,
    pub height: f64,
}

/// One exposure in progress of a [`CueProbe`].
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CueProbeExposure {
    pub post_id: String,
    pub dwell_ms_at_skip_visibility: u64,
    pub dwell_ms_at_long_visibility: u64,
    pub visible_samples: u32,
    /// The peak playback per-mille folded so far — what the media arm of
    /// `watch-complete` will read at emit.
    pub media_played_pm: Option<u32>,
}

/// Apply the single-sample noise floor and assemble the [`CueObservation`]s,
/// each stamped with how its exposure ended.
fn finish(left: Vec<(String, DwellState, ExposureEnd)>, wall_now_ms: u64) -> Vec<CueObservation> {
    left.into_iter()
        .filter(|(_, d, _)| d.visible_samples >= CUE_MIN_VISIBLE_SAMPLES)
        .map(|(content_id, d, ended_by)| CueObservation {
            content_id,
            is_media: d.is_media,
            media_played_pm: d.media_played_pm,
            dwell_ms_at_skip_visibility: d.dwell_ms_at_skip_visibility,
            dwell_ms_at_long_visibility: d.dwell_ms_at_long_visibility,
            observed_at_ms: wall_now_ms,
            ended_by,
        })
        .collect()
}

/// `max` over two optional per-mille readings, treating `None` as "no reading"
/// rather than zero.
fn max_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (some, None) | (None, some) => some,
    }
}

/// The visible fraction (per-mille, truncated toward zero) of a row whose
/// leading edge sits at `top` and whose rendered extent is `height`, against the
/// viewport `[viewport_start, viewport_end]` — all in one coordinate space.
///
/// Returns 0 for a non-positive `height`; callers must already have treated that
/// as unmeasurable (this function only computes the number, it is not the
/// unmeasurable-vs-gone decision). The result can never exceed 1000: the overlap
/// is bounded by the row's own extent, so a row taller than the viewport reports
/// the fraction of *itself* on screen.
fn visible_fraction_pm(top: f64, height: f64, viewport_start: f64, viewport_end: f64) -> u32 {
    if height <= 0.0 {
        return 0;
    }
    let overlap = (top + height).min(viewport_end) - top.max(viewport_start);
    if overlap <= 0.0 {
        return 0;
    }
    ((overlap / height) * 1000.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKIP_PM: u32 = CUE_SKIP_VISIBLE_PM;
    const LONG_PM: u32 = CUE_LONG_DWELL_VISIBLE_PM;

    /// A row whose geometry lands it at exactly `fraction_pm` of the viewport
    /// `[0, 1000]` — the tests speak in fractions, so the geometry cases below
    /// are the ones that pin the arithmetic itself.
    fn row_at(post_id: &str, fraction_pm: u32) -> CueRow {
        // height 1000, top chosen so `overlap / height` is the wanted fraction.
        CueRow {
            post_id: post_id.to_string(),
            top: 0.0,
            height: 1000.0,
            is_media: false,
            media_played_pm: None,
        }
        .with_fraction(fraction_pm)
    }

    impl CueRow {
        /// Reposition so the row reads `fraction_pm` against a `[0, 1000]`
        /// viewport: a row of height 1000 with its top at `1000 - fraction`
        /// overlaps the viewport by exactly `fraction`.
        fn with_fraction(mut self, fraction_pm: u32) -> Self {
            self.height = 1000.0;
            self.top = 1000.0 - f64::from(fraction_pm);
            self
        }

        fn media(mut self, is_media: bool) -> Self {
            self.is_media = is_media;
            self
        }

        fn played(mut self, pm: u32) -> Self {
            self.media_played_pm = Some(pm);
            self
        }

        fn unarranged(mut self) -> Self {
            self.height = 0.0;
            self
        }
    }

    fn window(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_string()).collect()
    }

    /// `sample` against the `[0, 1000]` viewport the row helpers assume, with
    /// the two clocks locked together (the default for cases that don't test
    /// clock separation).
    fn tick(
        t: &mut CueTracker,
        rows: Vec<CueRow>,
        win: &[&str],
        now_ms: u64,
    ) -> Vec<CueObservation> {
        t.sample(&rows, &window(win), 0.0, 1000.0, now_ms, now_ms)
    }

    fn hold() -> CueTracker {
        CueTracker::new(LeaveModel::HoldUnmeasured)
    }

    fn absence() -> CueTracker {
        CueTracker::new(LeaveModel::AbsenceIsLeave)
    }

    // ── Geometry ─────────────────────────────────────────────────────────────

    #[test]
    fn fully_visible_row_is_one_thousand_per_mille() {
        assert_eq!(visible_fraction_pm(0.0, 100.0, 0.0, 100.0), 1000);
    }

    #[test]
    fn row_half_off_the_top_is_five_hundred_per_mille() {
        assert_eq!(visible_fraction_pm(-50.0, 100.0, 0.0, 100.0), 500);
    }

    #[test]
    fn row_half_off_the_bottom_is_five_hundred_per_mille() {
        assert_eq!(visible_fraction_pm(50.0, 100.0, 0.0, 100.0), 500);
    }

    #[test]
    fn row_entirely_outside_the_viewport_is_zero() {
        assert_eq!(visible_fraction_pm(200.0, 100.0, 0.0, 100.0), 0);
        assert_eq!(visible_fraction_pm(-200.0, 100.0, 0.0, 100.0), 0);
    }

    #[test]
    fn fraction_respects_a_non_zero_viewport_start() {
        // Viewport [100, 200]; a row at [150, 250] overlaps by 50 of its 100.
        assert_eq!(visible_fraction_pm(150.0, 100.0, 100.0, 200.0), 500);
    }

    #[test]
    fn non_positive_height_is_zero_and_never_divides_by_zero() {
        assert_eq!(visible_fraction_pm(0.0, 0.0, 0.0, 100.0), 0);
        assert_eq!(visible_fraction_pm(0.0, -10.0, 0.0, 100.0), 0);
    }

    #[test]
    fn a_row_taller_than_the_viewport_reports_the_fraction_of_itself_on_screen() {
        // A 1000-tall row against a 100-tall viewport: 10 % of the ROW is shown.
        assert_eq!(visible_fraction_pm(0.0, 1000.0, 0.0, 100.0), 100);
    }

    #[test]
    fn fraction_truncates_toward_zero() {
        // 999/1000 of a 3-unit row = 0.999 → 999, never rounded up to 1000.
        assert_eq!(visible_fraction_pm(0.0, 1000.0, 0.0, 999.9), 999);
    }

    // ── Credit arithmetic ────────────────────────────────────────────────────

    #[test]
    fn the_first_sample_credits_no_dwell() {
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 0);
        // Leave it immediately: two sightings, but only one inter-sample gap —
        // and the first sample had no baseline to measure from.
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 0);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 0);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 0);
    }

    #[test]
    fn dwell_accrues_at_skip_visibility_but_long_dwell_only_above_the_long_gate() {
        let mut t = hold();
        const { assert!(SKIP_PM + 100 < LONG_PM) };
        let visible = row_at("p", SKIP_PM + 100); // above skip, below long
        tick(&mut t, vec![visible.clone()], &["p"], 0);
        tick(&mut t, vec![visible], &["p"], 250);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 500);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
        assert_eq!(left[0].dwell_ms_at_long_visibility, 0);
    }

    #[test]
    fn dwell_above_the_long_gate_accrues_in_both_buckets() {
        let mut t = hold();
        let visible = row_at("p", LONG_PM + 100);
        tick(&mut t, vec![visible.clone()], &["p"], 0);
        tick(&mut t, vec![visible], &["p"], 250);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 500);
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
        assert_eq!(left[0].dwell_ms_at_long_visibility, 250);
    }

    #[test]
    fn a_row_below_skip_visibility_never_accrues_and_is_never_tracked() {
        let mut t = hold();
        let barely = row_at("p", SKIP_PM - 1);
        tick(&mut t, vec![barely.clone()], &["p"], 0);
        let left = tick(&mut t, vec![barely], &["p"], 250);
        assert!(left.is_empty(), "never tracked ⇒ nothing to emit");
        assert_eq!(t.tracked(), 0);
    }

    #[test]
    fn a_long_gap_between_samples_is_capped_at_the_stall_ceiling() {
        let mut t = hold();
        let visible = row_at("p", 1000);
        tick(&mut t, vec![visible.clone()], &["p"], 0);
        // A 60 s stall (suspend / blocking dialog) credits at most the ceiling.
        tick(&mut t, vec![visible], &["p"], 60_000);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 60_250);
        assert_eq!(
            left[0].dwell_ms_at_skip_visibility,
            CUE_MAX_SAMPLE_CREDIT_MS
        );
    }

    // ── Leave policy: HoldUnmeasured ─────────────────────────────────────────

    #[test]
    fn hold_an_unmeasured_row_still_in_the_window_is_not_flushed() {
        let mut t = hold();
        let p = row_at("p", 1000);
        let q = row_at("q", 1000);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p.clone(), q], &["p", "q"], 250);

        // q is mid-rebuild this tick — absent from the probe, still in the window.
        let left = tick(&mut t, vec![p.clone()], &["p", "q"], 500);
        assert!(left.is_empty());
        assert_eq!(t.tracked(), 2, "q is held, not forgotten");

        // Both then measured out.
        let both = tick(
            &mut t,
            vec![p.with_fraction(0), row_at("q", 0)],
            &["p", "q"],
            750,
        );
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn hold_a_row_gone_from_the_loaded_window_has_left() {
        let mut t = hold();
        let p = row_at("p", 1000);
        let q = row_at("q", 1000);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p.clone(), q], &["p", "q"], 250);

        // Feed switched: q is no longer loaded at all.
        let left = tick(&mut t, vec![p], &["p"], 500);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content_id, "q");
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
    }

    #[test]
    fn hold_an_empty_probe_read_with_an_empty_window_still_flushes() {
        // The window check is independent of measurement: a post gone from the
        // window is positive evidence whether or not anything else was measured.
        let mut t = hold();
        let p = row_at("p", 1000);
        tick(&mut t, vec![p.clone()], &["p"], 0);
        tick(&mut t, vec![p], &["p"], 250);
        let left = tick(&mut t, vec![], &[], 500);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content_id, "p");
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
    }

    // ── Leave policy: AbsenceIsLeave ─────────────────────────────────────────

    #[test]
    fn absence_a_row_missing_from_a_non_empty_read_has_left() {
        let mut t = absence();
        let p = row_at("p", 1000);
        let q = row_at("q", 1000);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p.clone(), q], &["p", "q"], 250);

        // q was virtualized away, though its post is still loaded.
        let left = tick(&mut t, vec![p], &["p", "q"], 500);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content_id, "q");
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
    }

    #[test]
    fn absence_an_empty_probe_read_holds_everything_and_emits_nothing() {
        let mut t = absence();
        let p = row_at("p", 1000);
        let q = row_at("q", 1000);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p, q], &["p", "q"], 250);

        let left = tick(&mut t, vec![], &["p", "q"], 500);
        assert!(
            left.is_empty(),
            "measuring nothing is not measuring absence"
        );
        assert_eq!(t.tracked(), 2);
    }

    // ── The three ratified unifications ──────────────────────────────────────

    #[test]
    fn an_empty_probe_read_drops_the_credit_baseline_in_both_leave_models() {
        // Unification 2 (previously android-only): credit is bracketed by
        // measurements, so the unmeasured gap is never credited on resume.
        for mut t in [hold(), absence()] {
            let p = row_at("p", 1000);
            tick(&mut t, vec![p.clone()], &["p"], 0);
            tick(&mut t, vec![p.clone()], &["p"], 250); // 250 ms credited
            tick(&mut t, vec![], &["p"], 500); // nothing measurable: baseline dropped
            tick(&mut t, vec![p.clone()], &["p"], 5_000); // resume: no baseline ⇒ 0
            tick(&mut t, vec![p], &["p"], 5_250); // 250 ms credited
            let left = t.drain_all(0);
            assert_eq!(
                left[0].dwell_ms_at_skip_visibility, 500,
                "the 4.5 s unmeasured gap must not be credited"
            );
        }
    }

    #[test]
    fn a_row_with_non_positive_height_is_held_in_both_leave_models() {
        // Unification 3: an unarranged row is unmeasurable, never a fabricated
        // 0-visibility reading — under AbsenceIsLeave too, where its presence in
        // the probe read is exactly what distinguishes it from a disposed row.
        for mut t in [hold(), absence()] {
            let p = row_at("p", 1000);
            let q = row_at("q", 1000);
            tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
            tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 250);

            let left = tick(&mut t, vec![p, q.unarranged()], &["p", "q"], 500);
            assert!(left.is_empty(), "an unarranged row is held, not flushed");
            assert_eq!(t.tracked(), 2);
        }
    }

    #[test]
    fn credit_reads_the_monotonic_clock_and_the_stamp_reads_the_wall_clock() {
        // Unification 1 — THE DRIFT FIX. The wall clock steps a full day FORWARD
        // mid-exposure (an NTP correction, a user changing the date, a device
        // waking with a fresh time sync). Dwell is unmoved, because credit reads
        // only the monotonic clock; the stamp is whatever the wall clock says.
        //
        // Against the pre-revision windows/android behaviour (wall clock into
        // credit) the jumped tick credits the full stall ceiling instead of one
        // tick — 1 250 ms of "dwell" for 500 ms of watching, which crosses the
        // CUE_SKIP_MS gate and turns a real skip into no verdict at all.
        const WALL: u64 = 1_700_000_000_000;
        const DAY_MS: u64 = 86_400_000;
        let mut t = hold();
        let p = row_at("p", 1000);
        let win = window(&["p"]);
        let visible = vec![p.clone()];
        t.sample(&visible, &win, 0.0, 1000.0, 0, WALL);
        t.sample(&visible, &win, 0.0, 1000.0, 250, WALL + 250);
        t.sample(&visible, &win, 0.0, 1000.0, 500, WALL + DAY_MS);
        let left = t.sample(
            &[p.with_fraction(0)],
            &win,
            0.0,
            1000.0,
            750,
            WALL + DAY_MS + 250,
        );
        assert_eq!(left.len(), 1);
        assert_eq!(
            left[0].dwell_ms_at_skip_visibility, 500,
            "dwell is monotonic-only: a wall-clock step can never inflate it \
             (1250 here would be the pre-revision wall-clock credit)"
        );
        assert_eq!(
            left[0].observed_at_ms,
            WALL + DAY_MS + 250,
            "the stamp is the wall clock — the engine's pacing reads real time"
        );
    }

    // ── Exposure bookkeeping ─────────────────────────────────────────────────

    #[test]
    fn media_ness_follows_the_latest_reading() {
        // An image post first sighted before its media resolved reads as
        // playable, then as not — the exposure ends on the dwell arm.
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000).media(true)], &["p"], 0);
        tick(&mut t, vec![row_at("p", 1000).media(false)], &["p"], 250);
        let left = t.drain_all(9);
        assert!(!left[0].is_media);

        // And the other way: a video resolving mid-exposure is media.
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000).media(false)], &["p"], 0);
        tick(&mut t, vec![row_at("p", 1000).media(true)], &["p"], 250);
        let left = t.drain_all(9);
        assert!(left[0].is_media);
    }

    #[test]
    fn peak_playback_fraction_is_the_maximum_across_the_exposure() {
        let mut t = hold();
        tick(
            &mut t,
            vec![row_at("p", 1000).media(true).played(300)],
            &["p"],
            0,
        );
        tick(
            &mut t,
            vec![row_at("p", 1000).media(true).played(900)],
            &["p"],
            250,
        );
        // Playback rewound; the PEAK reached is what the observation carries.
        tick(
            &mut t,
            vec![row_at("p", 1000).media(true).played(100)],
            &["p"],
            500,
        );
        let left = t.drain_all(0);
        assert_eq!(left[0].media_played_pm, Some(900));
    }

    #[test]
    fn a_non_playing_row_carries_no_playback_reading() {
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 0);
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 250);
        let left = t.drain_all(0);
        assert_eq!(left[0].media_played_pm, None);
    }

    #[test]
    fn drain_all_emits_every_tracked_row_and_forgets_them() {
        let mut t = hold();
        let rows = vec![row_at("p", 1000), row_at("q", 1000)];
        tick(&mut t, rows.clone(), &["p", "q"], 0);
        tick(&mut t, rows, &["p", "q"], 250);
        let mut ids: Vec<String> = t.drain_all(42).into_iter().map(|o| o.content_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["p".to_string(), "q".to_string()]);
        assert_eq!(t.tracked(), 0);
        assert!(t.drain_all(42).is_empty());
    }

    #[test]
    fn drain_all_resets_the_credit_baseline() {
        let mut t = hold();
        let p = row_at("p", 1000);
        tick(&mut t, vec![p.clone()], &["p"], 0);
        tick(&mut t, vec![p.clone()], &["p"], 250);
        t.drain_all(0);

        // The same tracker reused after a long absence credits nothing for the gap.
        tick(&mut t, vec![p.clone()], &["p"], 60_000);
        tick(&mut t, vec![p], &["p"], 60_250);
        let left = t.drain_all(0);
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
    }

    #[test]
    fn a_left_row_is_forgotten_and_re_entry_is_a_fresh_exposure() {
        let mut t = hold();
        let p = row_at("p", 1000);
        tick(&mut t, vec![p.clone()], &["p"], 0);
        tick(&mut t, vec![p.clone()], &["p"], 250);
        let first = tick(&mut t, vec![row_at("p", 0)], &["p"], 500);
        assert_eq!(first[0].dwell_ms_at_skip_visibility, 250);
        assert_eq!(t.tracked(), 0, "the left row is forgotten, not parked");

        // Scrolled back: a NEW exposure. Its first sighting is still credited
        // from the last GLOBAL sample — one clock for the whole table — so the
        // three re-entry ticks accrue 250 + 250.
        tick(&mut t, vec![p.clone()], &["p"], 750);
        tick(&mut t, vec![p], &["p"], 1_000);
        let second = tick(&mut t, vec![row_at("p", 0)], &["p"], 1_250);
        assert_eq!(
            second[0].dwell_ms_at_skip_visibility, 500,
            "the fresh exposure carries only its own dwell — 750 would mean the \
             first exposure's 250 ms was resumed rather than forgotten"
        );
    }

    #[test]
    fn a_single_sample_sighting_is_dropped_as_layout_shift_noise() {
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 0);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 250);
        assert!(left.is_empty(), "one tick is a flash, not an exposure");
    }

    #[test]
    fn two_samples_is_a_real_exposure() {
        let mut t = hold();
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 0);
        tick(&mut t, vec![row_at("p", 1000)], &["p"], 250);
        let left = tick(&mut t, vec![row_at("p", 0)], &["p"], 500);
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn multiple_rows_are_tracked_independently() {
        let mut t = hold();
        // p stays fully visible; q sits between the two gates.
        let p = row_at("p", 1000);
        let q = row_at("q", SKIP_PM + 100);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 250);
        tick(&mut t, vec![p, q], &["p", "q"], 500);

        let mut left = t.drain_all(0);
        left.sort_by(|a, b| a.content_id.cmp(&b.content_id));
        assert_eq!(
            left[0].dwell_ms_at_long_visibility, 500,
            "p held long-visible"
        );
        assert_eq!(left[1].dwell_ms_at_skip_visibility, 500);
        assert_eq!(
            left[1].dwell_ms_at_long_visibility, 0,
            "q never crossed the long gate"
        );
    }

    // ── How an exposure ended (the 2026-10-10 ruling) ────────────────────────

    #[test]
    fn a_row_measured_out_of_the_viewport_ended_scrolled_past() {
        for mut t in [hold(), absence()] {
            let p = row_at("p", 1000);
            tick(&mut t, vec![p.clone()], &["p"], 0);
            tick(&mut t, vec![p.clone()], &["p"], 250);
            let left = tick(&mut t, vec![p.with_fraction(0)], &["p"], 500);
            assert_eq!(left[0].ended_by, ExposureEnd::ScrolledPast);
        }
    }

    #[test]
    fn absence_a_disposed_row_still_in_the_window_ended_scrolled_past() {
        let mut t = absence();
        let p = row_at("p", 1000);
        let q = row_at("q", 1000);
        tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
        tick(&mut t, vec![p.clone(), q], &["p", "q"], 250);
        let left = tick(&mut t, vec![p], &["p", "q"], 500);
        assert_eq!(left[0].content_id, "q");
        assert_eq!(left[0].ended_by, ExposureEnd::ScrolledPast);
    }

    #[test]
    fn a_row_gone_from_the_loaded_window_ended_window_dropped_in_both_models() {
        // Under AbsenceIsLeave too: a feed switch disposes the row AND drops its
        // post from the window — the window check comes first, so it is never
        // mislabelled as a scroll.
        for mut t in [hold(), absence()] {
            let p = row_at("p", 1000);
            let q = row_at("q", 1000);
            tick(&mut t, vec![p.clone(), q.clone()], &["p", "q"], 0);
            tick(&mut t, vec![p.clone(), q], &["p", "q"], 250);
            let left = tick(&mut t, vec![p], &["p"], 500);
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].content_id, "q");
            assert_eq!(left[0].ended_by, ExposureEnd::WindowDropped);
        }
    }

    #[test]
    fn drain_all_stamps_every_exposure_drained() {
        let mut t = hold();
        let rows = vec![row_at("p", 1000), row_at("q", 1000)];
        tick(&mut t, rows.clone(), &["p", "q"], 0);
        tick(&mut t, rows, &["p", "q"], 250);
        let left = t.drain_all(0);
        assert_eq!(left.len(), 2);
        assert!(left.iter().all(|o| o.ended_by == ExposureEnd::Drained));
    }

    #[test]
    fn the_stamp_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&ExposureEnd::ScrolledPast).unwrap(),
            "\"scrolled-past\""
        );
        assert_eq!(
            serde_json::to_string(&ExposureEnd::WindowDropped).unwrap(),
            "\"window-dropped\""
        );
        assert_eq!(
            serde_json::to_string(&ExposureEnd::Drained).unwrap(),
            "\"drained\""
        );
    }

    #[test]
    fn the_leave_model_is_readable_back() {
        assert_eq!(hold().leave_model(), LeaveModel::HoldUnmeasured);
        assert_eq!(absence().leave_model(), LeaveModel::AbsenceIsLeave);
    }

    // ── The test clock ───────────────────────────────────────────────────────

    /// Frozen, a row accrues exactly the time the test advanced while it was
    /// in view — whatever the real readings the shell kept passing — and the
    /// finished exposure carries the frozen WALL reading the engine's pace
    /// gate compares, moved by the same advances.
    #[test]
    fn a_frozen_clock_credits_exactly_what_the_test_advanced() {
        let mut t = hold();
        tick(&mut t, vec![row_at("a", 1000)], &["a"], 1_000);
        t.freeze_clock_for_test(1_000, 50_000);
        // Real readings racing ahead (a stalled loop, a slow agent) credit
        // nothing while frozen.
        for real in [5_000, 9_000, 30_000] {
            tick(&mut t, vec![row_at("a", 1000)], &["a"], real);
        }
        for _ in 0..9 {
            t.advance_clock_for_test(CUE_MAX_SAMPLE_CREDIT_MS).unwrap();
            tick(&mut t, vec![row_at("a", 1000)], &["a"], 99_999);
        }
        let probe = t.probe_for_test();
        assert!(probe.frozen);
        assert_eq!(probe.tracked, vec!["a".to_string()]);
        assert_eq!(probe.band, Some((0.0, 1000.0)));
        assert_eq!(
            probe.exposures,
            vec![CueProbeExposure {
                post_id: "a".into(),
                dwell_ms_at_skip_visibility: 9_000,
                dwell_ms_at_long_visibility: 9_000,
                visible_samples: 13,
                media_played_pm: None,
            }]
        );
        let left = tick(&mut t, vec![row_at("a", 0)], &["a"], 123);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].dwell_ms_at_long_visibility, 9_000);
        assert_eq!(left[0].observed_at_ms, 50_000 + 9_000);
        assert_eq!(left[0].ended_by, ExposureEnd::ScrolledPast);
    }

    /// Two advances with no sample between land as one sample's credit, which
    /// the cap clips — why each step needs its own sample, and why one step
    /// larger than the cap is refused rather than silently clipped.
    #[test]
    fn the_test_clock_refuses_what_it_cannot_honour() {
        let mut t = hold();
        assert!(t.advance_clock_for_test(100).is_err(), "not frozen");
        t.freeze_clock_for_test(0, 0);
        assert!(
            t.advance_clock_for_test(CUE_MAX_SAMPLE_CREDIT_MS + 1)
                .is_err()
        );
        t.advance_clock_for_test(CUE_MAX_SAMPLE_CREDIT_MS).unwrap();
        t.thaw_clock_for_test();
        assert!(!t.clock_frozen_for_test());
        assert!(t.advance_clock_for_test(100).is_err(), "thawed");
    }

    /// A drain stamps the frozen wall reading too (the quit path).
    #[test]
    fn a_frozen_drain_stamps_the_frozen_wall_reading() {
        let mut t = hold();
        t.freeze_clock_for_test(0, 7_000);
        tick(&mut t, vec![row_at("a", 1000)], &["a"], 1);
        t.advance_clock_for_test(500).unwrap();
        tick(&mut t, vec![row_at("a", 1000)], &["a"], 2);
        let drained = t.drain_all(999_999);
        assert_eq!(drained[0].observed_at_ms, 7_500);
        assert_eq!(drained[0].dwell_ms_at_skip_visibility, 500);
    }
}
