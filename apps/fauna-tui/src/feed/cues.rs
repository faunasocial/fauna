//! tui's engagement-cue **capture shell** for the Feed post list
//! (`docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation).
//!
//! **This module is a geometry probe and nothing else.** It measures where each
//! feed card landed in the frame the terminal actually painted, decides when to
//! sample, and notices when the list goes away; every piece of bookkeeping above
//! that — visibility bucketing, dwell credit and its stall cap, the hold-vs-leave
//! policy, the single-sample noise floor, `is_media` stamping, [`CueObservation`]
//! assembly — is the shared [`fauna_feed::CueTracker`], and all derivation below
//! it is `fauna_feed::CueEngine`. tui is Rust-native, so it constructs the
//! tracker directly, as linux does.
//!
//! # The geometry is the painted frame's, never the element list's
//!
//! tui's element list is its automation registry and names every post in the
//! window; only the viewport decides what gets pixels ([`crate::element`]). So a
//! card being *listed* says nothing about it being *seen*. The honest probe is
//! the frame itself: [`crate::ui::PaintedPage`] records, for the element list a
//! draw really painted, the lines each element occupies in the scrolled page and
//! the band of those lines the viewport showed. The coordinate space is the
//! page's own line space — one line is one unit — which the tracker accepts as
//! any other (it never assumes the viewport starts at zero).
//!
//! The viewport moves only when the focus ring does ([`crate::ui`]'s
//! `scroll_offset`): moving focus IS scrolling in this app, for a human's arrow
//! keys and the automation agent's scroll-into-view alike. That is the whole
//! reason this shell reads the painted frame: a dwell measured anywhere else
//! would measure nothing the user did.
//!
//! # Scheduling, lifecycle, leave model
//!
//! - **Sampling:** once after every draw — which covers the extra sample on
//!   every scroll, since a focus move always redraws — plus the shared
//!   [`CUE_SAMPLE_INTERVAL_MS`] tick while the list is on screen, so dwell keeps
//!   accruing while the user holds still (`main.rs` owns the tick).
//! - **Lifecycle:** one [`CueCapture`] per feed manager, built with it in
//!   [`crate::feed::init`] (a re-auth rebuilds both together, so the tracker
//!   can never sample one actor's cards into another's manager). Leaving the
//!   list — another page, post detail, the create-feed form — drains every
//!   tracked card, since off-screen is off-viewport; quitting drains and then
//!   flushes the rollup (`main.rs`).
//! - **[`LeaveModel::HoldUnmeasured`]:** every card keeps its lines whether or
//!   not they paint, so a card is always measurable and a card missing from a
//!   probe read is a placeholder or a card mid-rebuild — held, never read as
//!   gone. A card leaves on positive evidence only: it was measured scrolled
//!   out of view, or its post left the loaded window.
//!
//! tui renders feed media as half-block stills with no playback surface, so
//! `media_played_pm` is always `None`: the media watch-complete gate cannot
//! fire here — only the dwell-derived non-media gate and the skip gate can.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS;
use fauna_feed::{CueObservation, CueRow, CueTracker, LeaveModel};
use fauna_ui_ids as ids;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, PageOutcome, UiMessage};
use crate::ui::{LineSpan, PaintedPage};

/// The shared sampling cadence, read from the shared constant — never
/// re-declared per app.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(CUE_SAMPLE_INTERVAL_MS);

/// The post a feed card shows, as the card's head row carries it
/// ([`crate::element::Element::cue`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueSubject {
    pub post_id: String,
    /// The post's `has_media`, from the same snapshot row the card was built
    /// from. Stamped once per exposure by the tracker.
    pub is_media: bool,
}

/// The shared tracker plus this manager generation's monotonic epoch.
///
/// `Instant` has no absolute reading, so the probe measures `mono_now_ms` from
/// a zero taken at construction — the tracker only ever takes differences.
pub struct CueCapture {
    tracker: CueTracker,
    mono_epoch: Instant,
    /// Whether the previous sample found the post list on screen — the memory
    /// that turns "not on screen now" into a leave edge exactly once.
    showing: bool,
}

impl Default for CueCapture {
    fn default() -> Self {
        CueCapture {
            tracker: CueTracker::new(LeaveModel::HoldUnmeasured),
            mono_epoch: Instant::now(),
            showing: false,
        }
    }
}

impl CueCapture {
    fn mono_now_ms(&self) -> u64 {
        self.mono_epoch.elapsed().as_millis() as u64
    }

    /// Whether the post list was on screen at the last sample — the tick's
    /// gate, so an idle terminal on any other page is never woken for it.
    pub fn showing(&self) -> bool {
        self.showing
    }

    /// One probe read. `list` is the painted page and the loaded window's post
    /// ids while the post list is on screen, `None` otherwise — and the first
    /// `None` after a showing sample is the leave edge that drains everything.
    ///
    /// Returns the finished exposures, already past the tracker's noise floor.
    pub fn sample(
        &mut self,
        list: Option<(&PaintedPage, &[String])>,
        wall_now_ms: u64,
    ) -> Vec<CueObservation> {
        let Some((page, window_post_ids)) = list else {
            if !self.showing {
                return Vec::new();
            }
            self.showing = false;
            return self.tracker.drain_all(wall_now_ms);
        };
        self.showing = true;
        let (top, bottom) = page.band();
        let mono_now_ms = self.mono_now_ms();
        self.tracker.sample(
            &card_rows(page),
            window_post_ids,
            top as f64,
            bottom as f64,
            mono_now_ms,
            wall_now_ms,
        )
    }

    /// Everything tracked has left — the quit path's drain.
    pub fn drain(&mut self, wall_now_ms: u64) -> Vec<CueObservation> {
        self.showing = false;
        self.tracker.drain_all(wall_now_ms)
    }
}

/// One [`CueRow`] per feed card the frame painted: the card's head row plus
/// every element painted inside it (`.within(post-card, k)`, nested embeds
/// included), spanned in the page's line space.
///
/// A card whose head carries no [`CueSubject`] — a muted, blocked or
/// content-collapsed placeholder — is left out: it hides what the post says, so
/// lingering on it is not an exposure to the post. A card that painted no line
/// at all is left out too; the tracker holds an omitted row rather than reading
/// its absence as a leave.
pub(crate) fn card_rows(page: &PaintedPage) -> Vec<CueRow> {
    let mut rows = Vec::new();
    // The card's occurrence among the list's top-level `post-card` rows — the
    // index its children's scope step carries.
    let mut occurrence = 0usize;
    for (index, element) in page.elements.iter().enumerate() {
        if element.id != ids::POST_CARD || !element.path.is_empty() {
            continue;
        }
        let card = occurrence;
        occurrence += 1;
        let Some(subject) = &element.cue else {
            continue;
        };
        let span = page
            .elements
            .iter()
            .enumerate()
            .filter(|(j, e)| {
                *j == index
                    || e.path
                        .first()
                        .is_some_and(|(id, n)| id == ids::POST_CARD && *n == card)
            })
            .filter_map(|(j, _)| page.span(j))
            .reduce(LineSpan::union);
        let Some(span) = span else {
            continue;
        };
        rows.push(CueRow {
            post_id: subject.post_id.clone(),
            top: span.first as f64,
            height: span.count as f64,
            is_media: subject.is_media,
            // Half-block stills only — no playback surface to read.
            media_played_pm: None,
        });
    }
    rows
}

/// Whether the feed's post list is what the page shows right now.
fn list_on_screen(app: &App) -> bool {
    app.authenticated()
        && !app.showing_launch_surface()
        && app.page == crate::pages::Page::Feed
        && app.feed.mode == crate::feed::Mode::List
}

/// Sample the frame just drawn and hand anything finished to the shared engine.
/// Called once per draw, from the render loop.
pub fn sample_frame(app: &mut App, page: &PaintedPage, tx: &UnboundedSender<UiMessage>) {
    let Some(manager) = app.feed.manager.clone() else {
        return;
    };
    let window: Option<Vec<String>> = list_on_screen(app).then(|| {
        manager
            .snapshot()
            .posts
            .iter()
            .map(|p| p.post_id.clone())
            .collect()
    });
    let finished = app.feed.cue_capture.sample(
        window.as_deref().map(|ids| (page, ids)),
        fauna_core::data::Timestamp::now_millis_or_zero(),
    );
    emit(&manager, finished, tx, app.session_generation);
}

/// Hand finished exposures to `FeedManager::record_observation`, in order, off
/// the render loop; a failure lands on the Feed page's `error-message`.
///
/// `session_generation` rides [`DataMessage::Page`]'s envelope — the identity
/// seam that drops a failure landing after an actor change.
fn emit(
    manager: &Arc<crate::feed::CliFeedManager>,
    observations: Vec<CueObservation>,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    if observations.is_empty() {
        return;
    }
    let manager = Arc::clone(manager);
    let tx = tx.clone();
    tokio::spawn(async move {
        for observation in observations {
            if let Err(message) = manager.record_observation(observation).await {
                let _ = tx.send(UiMessage::Data(DataMessage::Page(
                    session_generation,
                    PageOutcome::Feed(crate::feed::Outcome::Error(message)),
                )));
                return;
            }
        }
    });
}

/// The quit path: drain every tracked card (the one on screen is an exposure in
/// progress), fold what finished, then force the rollup put the debounce would
/// otherwise hold back — each step bounded, so an unreachable nest cannot hang
/// quitting.
pub async fn flush_on_quit(app: &mut App, budget: Duration) {
    let Some(manager) = app.feed.manager.clone() else {
        return;
    };
    let finished = app
        .feed
        .cue_capture
        .drain(fauna_core::data::Timestamp::now_millis_or_zero());
    let _ = tokio::time::timeout(budget, async {
        for observation in finished {
            if manager.record_observation(observation).await.is_err() {
                break;
            }
        }
    })
    .await;
    let _ = tokio::time::timeout(budget, manager.flush_cues()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Element;

    fn subject(post_id: &str, is_media: bool) -> CueSubject {
        CueSubject {
            post_id: post_id.to_string(),
            is_media,
        }
    }

    /// Two cards, each a head row plus two children, one line apiece.
    fn two_cards() -> Vec<Element> {
        vec![
            Element::label(ids::PAGE_HEADING, "Feed"),
            Element::label(ids::POST_CARD, "first").cue(subject("aa", false)),
            Element::label(ids::POST_AUTHOR, "alice").within(ids::POST_CARD, 0),
            Element::label(ids::FEED_POST_TEXT, "first").within(ids::POST_CARD, 0),
            Element::label(ids::POST_CARD, "second").cue(subject("bb", true)),
            Element::label(ids::POST_AUTHOR, "bob").within(ids::POST_CARD, 1),
            Element::label(ids::FEED_POST_TEXT, "second").within(ids::POST_CARD, 1),
        ]
    }

    /// A card spans its head row AND everything painted inside it, in the
    /// page's own line space — the geometry the tracker buckets.
    #[test]
    fn a_card_spans_its_head_and_its_children() {
        let page = PaintedPage::for_test(two_cards(), 0, 40);
        let rows = card_rows(&page);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].post_id.as_str(), rows[0].top, rows[0].height),
            ("aa", 1.0, 3.0)
        );
        assert_eq!(
            (rows[1].post_id.as_str(), rows[1].top, rows[1].height),
            ("bb", 4.0, 3.0)
        );
        assert!(!rows[0].is_media && rows[1].is_media);
    }

    /// A placeholder card (no subject) is not an exposure, but it still counts
    /// as a card: the NEXT card's children carry occurrence 1, not 0, so
    /// skipping the placeholder must not shift which children belong to whom.
    #[test]
    fn a_placeholder_card_is_skipped_without_shifting_the_next_cards_children() {
        let mut elements = two_cards();
        elements[1].cue = None;
        let rows = card_rows(&PaintedPage::for_test(elements, 0, 40));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].post_id.as_str(), rows[0].top, rows[0].height),
            ("bb", 4.0, 3.0)
        );
    }

    /// Dwell accrues only while a card is inside the painted band. The same
    /// card held for longer than the long-dwell gate derives `watch-complete`
    /// once it is scrolled out of the band — and never while it sits there.
    #[test]
    fn a_held_card_emits_its_dwell_when_scrolled_out_of_the_band() {
        use fauna_core::scoring::cues::CUE_DWELL_LONG_MS;
        let window = vec!["aa".to_string(), "bb".to_string()];
        let mut capture = CueCapture::default();
        // Band [0, 4): card "aa" (lines 1..4) fully in view, "bb" (4..7) out.
        let shown = PaintedPage::for_test(two_cards(), 0, 4);
        let mut now = 0u64;
        let mut out = Vec::new();
        while now <= CUE_DWELL_LONG_MS + 1_000 {
            out.extend(
                capture
                    .tracker
                    .sample(&card_rows(&shown), &window, 0.0, 4.0, now, now),
            );
            now += CUE_SAMPLE_INTERVAL_MS;
        }
        assert!(
            out.is_empty(),
            "nothing leaves while it stays in view: {out:?}"
        );
        // Scroll: the band moves to [4, 8) — "aa" is now measured out of view.
        let scrolled = PaintedPage::for_test(two_cards(), 4, 4);
        let left = capture
            .tracker
            .sample(&card_rows(&scrolled), &window, 4.0, 8.0, now, now);
        let aa = left.iter().find(|o| o.content_id == "aa").expect("aa left");
        assert!(
            aa.dwell_ms_at_long_visibility >= CUE_DWELL_LONG_MS,
            "{aa:?}"
        );
    }

    /// Leaving the list drains what is tracked exactly once; a second
    /// off-list sample has nothing left to say.
    #[test]
    fn leaving_the_list_drains_once() {
        let window = vec!["aa".to_string(), "bb".to_string()];
        let mut capture = CueCapture::default();
        let page = PaintedPage::for_test(two_cards(), 0, 40);
        for _ in 0..3 {
            capture.sample(Some((&page, &window)), 0);
        }
        assert!(capture.showing());
        assert_eq!(capture.tracker.tracked(), 2);
        capture.sample(None, 0);
        assert!(!capture.showing());
        assert_eq!(capture.tracker.tracked(), 0);
        assert!(capture.sample(None, 0).is_empty());
    }
}
