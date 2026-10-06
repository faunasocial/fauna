//! The **human** mouse path: how two plain terminal `Down`s become one press or
//! a pair, and which of an element's two gestures that makes fire.
//!
//! A terminal never labels a double press — it delivers `Down` after `Down` —
//! so this is where one gets synthesized, exactly as every GUI toolkit does it.
//! The one element family that defines both gestures is the month grid's
//! `events-day-cell-{YYYY-MM-DD}`: a single press drills into Day view for that
//! date, a double press opens the new-event compose prefilled with it
//! (`docs/goal/ui/events.md` § Layout & flow, the Outlook model).
//!
//! **Why the two need arbitrating at all.** They are inherently ambiguous: you
//! only learn a press was the first half of a pair when the second one arrives.
//! Acting on the first press immediately is what made the double press
//! *unreachable for a real user* here — the drill-in repaints the whole page,
//! so the second press lands on whatever the Day view just put at those
//! coordinates. tui's old test compared `ui::HitTarget::Page(index)` across the
//! two frames, and a position means nothing once the list under it changed.
//! Same class of bug as windows' (fixed 2026-08-11) and linux's; **apple** is
//! the one app that was correct by construction, because SwiftUI orders
//! `.onTapGesture(count: 2)` ahead of `count: 1` for you.
//!
//! **The shape, shared with windows:** defer the first press's gesture until the
//! double-press window closes, so exactly one of the two ever fires. Two
//! deliberate properties beyond windows' `DayCellClickArbiter`:
//!
//! - **Only an element that actually defines a second gesture is ever held.**
//!   An ordinary button still actuates on the press that hit it — no control in
//!   the app pays this latency for a gesture it does not have.
//! - **A pair is decided by element *identity*, never by position.** A push
//!   event or a poll tick can repaint between the two presses and renumber the
//!   list; the id survives that, and [`actuate`] re-resolves through it rather
//!   than trusting the index it recorded.
//!
//! [`Arbiter`] itself is UI-free and clock-injected — the caller owns the timer
//! — so the decision logic is pinned by tier_1 tests with synthetic instants
//! rather than by a test racing a wall clock
//! (`docs/goal/architecture/e2e-conventions.md` point 14). The automation door
//! is untouched and still dispatches an element's second gesture directly
//! ([`crate::app::App::double_click_page_element`]), so no test ever has to beat
//! this threshold.
//!
//! **The state machine itself now lives in shared Rust** —
//! [`fauna_client_core::press`] — lifted there 2026-08-12 when linux needed the
//! same arbitration for its GTK month grid (priority #2). What stays here is
//! tui's own half: reading what a frame paints at a coordinate ([`describe`])
//! and actuating a decided press back into the app ([`actuate`]). tui pairs a
//! [`HitTarget`] payload; linux pairs a calendar date.

use std::time::Instant;

use crate::app::App;
use crate::ui::{HitTarget, RowHit};

pub use fauna_client_core::press::{Act, Arbiter as SharedArbiter, DOUBLE_PRESS_WINDOW};

/// One press as tui saw it: the shared [`Hit`](fauna_client_core::press::Hit)
/// carrying this frame's [`HitTarget`] as its payload.
pub type Hit = fauna_client_core::press::Hit<HitTarget>;

/// tui's press-chain state machine — the shared one, pairing `HitTarget`s.
pub type Arbiter = SharedArbiter<HitTarget>;

/// Drive one left press at `(col, row)` against the frame that painted `hits` —
/// the whole human mouse path behind one door, which is what lets a tier_1 test
/// feed two real presses across two real frames.
pub fn left_press(
    app: &mut App,
    arbiter: &mut Arbiter,
    hits: &[RowHit],
    col: u16,
    row: u16,
    now: Instant,
) {
    let hit = crate::ui::hit_test(hits, col, row).map(|target| describe(app, target));
    let decision = arbiter.press(hit, now);
    if let Some(flushed) = decision.flush {
        actuate(app, &flushed, false);
    }
    match decision.act {
        Some(Act::Single(hit)) => actuate(app, &hit, false),
        Some(Act::Double(hit)) => actuate(app, &hit, true),
        None => {}
    }
}

/// A held press's window closed: actuate its first gesture.
pub fn expire(app: &mut App, arbiter: &mut Arbiter, now: Instant) {
    if let Some(held) = arbiter.expire(now) {
        actuate(app, &held, false);
    }
}

/// What this frame paints at `target`, in the terms the arbiter reasons about.
fn describe(app: &App, target: HitTarget) -> Hit {
    match target {
        // A sidebar row is a page, and no page carries a second gesture — so
        // this never holds, and its id is only ever used to tell one row from
        // another.
        HitTarget::Sidebar(index) => Hit {
            payload: target,
            id: app
                .sidebar_pages()
                .get(index)
                .map(|page| page.tab_id().to_string())
                .unwrap_or_default(),
            has_double: false,
        },
        HitTarget::Page(index) => {
            let elements = app.page_elements();
            let element = elements.get(index);
            Hit {
                payload: target,
                id: element.map(|e| e.id.clone()).unwrap_or_default(),
                has_double: element.is_some_and(|e| e.dbl.is_some() && e.focusable() && e.enabled),
            }
        }
    }
}

/// Actuate a press, re-resolving its element **by identity**: `HitTarget::Page`
/// indexes one frame's element list, and a repaint between the press and this
/// call renumbers it. Nothing happens once the element is gone — the honest
/// answer when the page the user pressed on is no longer there.
fn actuate(app: &mut App, hit: &Hit, second: bool) {
    match hit.payload {
        HitTarget::Sidebar(index) => app.click_sidebar(index),
        HitTarget::Page(index) => {
            let Some(index) = resolve(app, index, &hit.id) else {
                return;
            };
            // Falls back to a plain click when the second gesture is refused,
            // so a fast double press on an ordinary control is the two clicks
            // it is.
            if second && app.double_click_page_element(index) {
                return;
            }
            app.click_page_element(index);
        }
    }
}

/// The element's position *now*: the recorded one while it still carries the
/// recorded id, else wherever that id moved to.
fn resolve(app: &App, index: usize, id: &str) -> Option<usize> {
    let elements = app.page_elements();
    if elements.get(index).is_some_and(|e| e.id == id) {
        return Some(index);
    }
    elements.iter().position(|e| e.id == id)
}

/// The human path end to end: a real terminal frame, a real hit-test, two real
/// presses, and the app state they leave behind.
///
/// These are the tests the automation door structurally cannot be — the agent
/// dispatches an element's second gesture straight to the retained element, so
/// a green e2e says nothing about whether two presses from a *hand* ever reach
/// it. That bypass is what let this class of bug survive on three apps at once
/// (`docs/goal/ui/events.md` § Implementation status today, the *day cell's two
/// gestures collide* bullet).
#[cfg(test)]
mod journey {
    use super::*;
    use crate::events::{Mode, ViewMode};
    use crate::pages::Page;
    use std::time::Duration;

    /// Wide enough for the month grid's 7 × `MONTH_CELL_WIDTH` cells beside the
    /// sidebar, tall enough for its six week rows.
    const WIDTH: u16 = 120;
    const HEIGHT: u16 = 40;

    fn month_view_app() -> App {
        let mut app = crate::app::tests::authed_app();
        app.events = crate::events::init(
            fauna_client::NestClient::new(
                "http://127.0.0.1:1".to_string(),
                fauna_core::identity::ActorKeypair::generate(),
            ),
            "aa",
            "alice@example.com",
            std::sync::Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        );
        app.page = Page::Events;
        app.events.view_mode = ViewMode::Month;
        app.events.focus_year = 2026;
        app.events.focus_month = 7;
        app.events.focus_day = 15;
        app
    }

    /// Paint a whole frame at a known size and keep its hit map — the only
    /// record of where this frame's clickable rows landed, exactly as the event
    /// loop keeps it.
    fn frame_hits(app: &App) -> Vec<RowHit> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(WIDTH, HEIGHT)).unwrap();
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                let frame_hits = crate::ui::render(frame, app).hits;
                hits = frame_hits;
            })
            .unwrap();
        hits
    }

    /// Where a user would put the cursor to press `id` — answered by asking the
    /// production hit-test, not by re-deriving paint geometry.
    fn coords_of(app: &App, hits: &[RowHit], id: &str) -> (u16, u16) {
        let elements = app.page_elements();
        for row in 0..HEIGHT {
            for col in 0..WIDTH {
                if let Some(HitTarget::Page(index)) = crate::ui::hit_test(hits, col, row)
                    && elements.get(index).is_some_and(|e| e.id == id)
                {
                    return (col, row);
                }
            }
        }
        panic!("this frame paints no cell a press at any coordinate resolves to {id:?}");
    }

    /// **The bug this track was opened to verify.** Two presses from a hand, at
    /// one day cell, across two successive frames — the compose must open
    /// prefilled with the day that was pressed.
    ///
    /// Before the arbiter this failed: the first press drilled into Day view
    /// immediately, the frame repainted, and the second press hit whatever the
    /// timeline had put at those coordinates.
    #[test]
    fn two_presses_at_a_day_cell_open_the_compose_prefilled_with_that_date() {
        let mut app = month_view_app();
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let start = Instant::now();

        let hits = frame_hits(&app);
        let (col, row) = coords_of(&app, &hits, "events-day-cell-2026-07-09");
        left_press(&mut app, &mut arbiter, &hits, col, row, start);

        // The second press lands on the NEXT frame, because the loop redraws
        // between events. Re-rendering here is the whole point of the test.
        let hits = frame_hits(&app);
        left_press(
            &mut app,
            &mut arbiter,
            &hits,
            col,
            row,
            start + Duration::from_millis(120),
        );

        assert_eq!(
            app.events.mode,
            Mode::CreateEvent,
            "two presses at a day cell open the new-event compose"
        );
        // The whole datetime, not just the date — the date alone cannot tell
        // the CELL's own second gesture from a fall-through onto the drilled-in
        // Day view's time slots, which is precisely what the old code did and
        // what windows' probe had to measure by hand. A day cell carries no
        // time (`at: None`), so its prefill is the day's working start; a slot
        // would write its own snapped `HH:MM` here.
        assert_eq!(
            app.events.new_event_dtstart, "2026-07-09T09:00",
            "the CELL's second gesture prefills the day's working start"
        );
        assert_eq!(
            app.events.view_mode,
            ViewMode::Month,
            "exactly one of the cell's two gestures fires — the drill-in must not \
             also have run underneath the compose"
        );
    }

    /// The other half of the same contract: one press, left alone, still drills
    /// into Day view once the window closes. A deferral that lost the single
    /// press would trade one broken gesture for the other.
    #[test]
    fn one_press_at_a_day_cell_drills_in_once_the_window_closes() {
        let mut app = month_view_app();
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let start = Instant::now();

        let hits = frame_hits(&app);
        let (col, row) = coords_of(&app, &hits, "events-day-cell-2026-07-09");
        left_press(&mut app, &mut arbiter, &hits, col, row, start);
        assert_eq!(
            app.events.view_mode,
            ViewMode::Month,
            "held, not fired: the pair's second half could still arrive"
        );

        expire(&mut app, &mut arbiter, start + DOUBLE_PRESS_WINDOW);
        assert_eq!(app.events.view_mode, ViewMode::Day);
        assert_eq!(app.events.focus(), (2026, 7, 9), "focus follows the cell");
        assert_eq!(
            app.events.mode,
            Mode::List,
            "a single press must not open the compose"
        );
    }

    /// A control with no second gesture is unaffected: it actuates on the press
    /// that hit it, with no window to wait out. Pinned through the same real
    /// frame, because a latency regression here would reach every page.
    #[test]
    fn an_ordinary_control_still_actuates_on_the_press_that_hit_it() {
        let mut app = month_view_app();
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);

        let hits = frame_hits(&app);
        let (col, row) = coords_of(&app, &hits, "calendar-view-day");
        left_press(&mut app, &mut arbiter, &hits, col, row, Instant::now());

        assert_eq!(
            app.events.view_mode,
            ViewMode::Day,
            "the view-mode control switches on the first press"
        );
        assert_eq!(arbiter.deadline(), None, "and nothing is left holding");
    }
}
