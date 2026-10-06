//! Which of a month day cell's two gestures fires, for a **real** pointer.
//!
//! The cell defines both (`docs/goal/ui/events.md` § Layout & flow, the Outlook
//! model): a single click drills into Day view for that date, a double click
//! opens the new-event compose prefilled with it. Acting on the first click
//! immediately made the second one unreachable — `switch_view` swaps the
//! `gtk::Stack` synchronously, which **unmaps the cell**, so GTK resets its
//! `GtkGestureClick` and the user's second press hit-tests against the Day
//! timeline that just repainted underneath. linux tiles that timeline with 96
//! `events-time-slot-{HH-MM}` markers, so the second press quick-created an
//! event at *the slot's* time instead of the cell's — the same symptom tui
//! measured (`week_grid.rs::add_time_slot_markers`; goal doc § Implementation
//! status today, the *day cell's two gestures collide* bullet).
//!
//! So the drill-in is **deferred** until the double-click window closes, and
//! exactly one of the two gestures ever fires. The pairing state machine is
//! shared with tui ([`fauna_client_core::press`]); what lives here is the part
//! that is genuinely GTK's, and it is kept UI-free so tier_1 tests drive it
//! with synthetic instants rather than by racing a wall clock
//! (`docs/goal/architecture/e2e-conventions.md` point 14).
//!
//! **Why a `clicked` *and* an `n_press == 2` door.** They are not the same
//! input. `clicked` is every activation GTK has — pointer release, keyboard
//! Space/Enter, AT-SPI `do_action`, and the e2e agent's `activate()` — while
//! `n_press == 2` comes only from the capture-phase gesture. A real double
//! click therefore delivers **three** events, in this order:
//!
//! ```text
//! press(1) → release → clicked      ← the pair's first half
//! press(2) → n_press == 2           ← the pair closes here, before the release
//!          → release → clicked      ← the *same* press's release, arriving late
//! ```
//!
//! That trailing `clicked` is the whole reason [`Outcome`] exists rather than a
//! bare call into the arbiter: left alone it starts a fresh chain and drills in
//! 400 ms after the compose already opened, which is the bug wearing a new hat.
//! [`DayCellPress::double_press`] therefore arms a one-shot swallow.

use std::time::{Duration, Instant};

use fauna_client_core::press::{Act, Arbiter, Hit};

/// A calendar date as the grid addresses it: `(year, month, day)`.
pub type Date = (i32, u32, u32);

/// The desktop's own double-click time, which is what a GTK user has already
/// tuned for every other double click on their machine.
///
/// The shared machine takes its window as a parameter precisely so each app can
/// answer this its own way: tui hard-codes 400 ms because a terminal has no
/// such setting to read, windows asks `GetDoubleClickTime()`, and linux asks
/// GTK. Falls back to the shared constant when there are no settings — a
/// headless test process, before any display is opened.
pub fn desktop_double_click_window() -> Duration {
    gtk::Settings::default()
        .map(|settings| Duration::from_millis(settings.gtk_double_click_time().max(0) as u64))
        .filter(|window| !window.is_zero())
        .unwrap_or(fauna_client_core::press::DOUBLE_PRESS_WINDOW)
}

/// The id the cell carries, and the identity a pair is decided by — the same
/// `events-day-cell-{YYYY-MM-DD}` ui.yaml stamps on the widget.
pub fn day_cell_id((y, m, d): Date) -> String {
    format!("events-day-cell-{y:04}-{m:02}-{d:02}")
}

fn hit(date: Date) -> Hit<Date> {
    Hit {
        payload: date,
        id: day_cell_id(date),
        // Every day cell defines the compose gesture, so every one of them is
        // held. No other widget in the app routes through here at all, which is
        // what keeps the deferral off every ordinary button.
        has_double: true,
    }
}

/// Everything one input event causes, in the order given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Drill into Day view for this date — an earlier press that turned out to
    /// be single after all. Runs before `compose`, so a click the user made
    /// first is never swallowed by one they made second.
    pub drill: Option<Date>,
    /// Open the quick-create compose prefilled with this date.
    pub compose: Option<Date>,
    /// A newly held press's deadline — arm the timer here.
    pub hold_until: Option<Instant>,
}

/// The day cell's press chain: the shared arbiter plus GTK's own event order.
#[derive(Debug)]
pub struct DayCellPress {
    arbiter: Arbiter<Date>,
    /// Set when a pair closed on `press(2)`: GTK still owes us the `clicked`
    /// for that same press's release, and it must not open a new chain.
    swallow_next_click: bool,
}

impl DayCellPress {
    /// `window` is how long a second click still pairs with the first —
    /// the desktop's own `gtk-double-click-time` at the call site, so linux
    /// honours the user's pointer settings while running the shared machine.
    pub fn new(window: Duration) -> Self {
        Self {
            arbiter: Arbiter::new(window),
            swallow_next_click: false,
        }
    }

    /// When the held press's window closes — the deadline to arm a timer for.
    pub fn deadline(&self) -> Option<Instant> {
        self.arbiter.deadline()
    }

    /// A `clicked` on the cell for `date`: pointer release, keyboard, AT-SPI,
    /// or the e2e agent. Holds rather than drilling, so the cell is still
    /// mapped if a second press follows.
    pub fn click(&mut self, date: Date, now: Instant) -> Outcome {
        if self.swallow_next_click {
            // The release half of a press that already closed a pair.
            self.swallow_next_click = false;
            return Outcome::default();
        }
        let decision = self.arbiter.press(Some(hit(date)), now);
        let flushed = decision.flush.map(|h| h.payload);
        let (acted_drill, compose) = match decision.act {
            // A cell always defines both gestures, so `Single` can only mean the
            // arbiter declined to hold — drill rather than drop the click.
            Some(Act::Single(h)) => (Some(h.payload), None),
            Some(Act::Double(h)) => (None, Some(h.payload)),
            None => (None, None),
        };
        Outcome {
            drill: flushed.or(acted_drill),
            compose,
            hold_until: decision.hold_until,
        }
    }

    /// GTK's capture-phase gesture reported `n_press == 2` at `date` — the
    /// pair's second half, arriving *before* its own release.
    pub fn double_press(&mut self, date: Date, now: Instant) -> Outcome {
        let decision = self.arbiter.press(Some(hit(date)), now);
        let compose = match decision.act {
            Some(Act::Double(h)) => Some(h.payload),
            // No pair to close (the automation agent dispatches `n_press == 2`
            // straight at the widget without a first press, and so does a
            // double click whose first half landed on a different cell). The
            // gesture is still unambiguously a request to compose — but the
            // press just recorded is now that compose's own first half, so
            // discard it rather than let it drill in a window later.
            _ => {
                self.arbiter.cancel();
                Some(date)
            }
        };
        // Either way a press was consumed here, and GTK owes us its release.
        self.swallow_next_click = true;
        Outcome {
            drill: decision.flush.map(|h| h.payload),
            compose,
            // GTK decided this pair; nothing is left waiting on a timer.
            hold_until: None,
        }
    }

    /// The deferral window closed with no second press: drill in after all.
    /// `None` while it is still open, so a spurious wake-up is harmless.
    pub fn expire(&mut self, now: Instant) -> Option<Date> {
        self.arbiter.expire(now).map(|h| h.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_millis(400);
    const CELL: Date = (2026, 7, 9);
    const OTHER: Date = (2026, 7, 16);

    /// **The bug this module exists for.** A single click must not drill in
    /// while a second one could still be coming — that is what unmapped the
    /// cell and made the compose unreachable for a real user.
    #[test]
    fn a_first_click_does_not_drill_in_yet() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        let outcome = presses.click(CELL, now);
        assert_eq!(outcome.drill, None, "the drill-in must wait for the pair");
        assert_eq!(outcome.compose, None);
        assert_eq!(outcome.hold_until, Some(now + WINDOW));
    }

    /// A lone click still drills in — just once the window has closed.
    #[test]
    fn a_lone_click_drills_in_when_the_window_closes() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        assert_eq!(presses.expire(now + Duration::from_millis(399)), None);
        assert_eq!(presses.expire(now + WINDOW), Some(CELL));
        assert_eq!(presses.deadline(), None);
    }

    /// The pair closes on `n_press == 2`: the compose opens and the drill-in
    /// never fires.
    #[test]
    fn a_double_click_composes_and_never_drills() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        let outcome = presses.double_press(CELL, now + Duration::from_millis(120));
        assert_eq!(outcome.compose, Some(CELL));
        assert_eq!(outcome.drill, None, "the held click must not ALSO fire");
        assert_eq!(presses.expire(now + Duration::from_secs(1)), None);
    }

    /// **The trailing release.** GTK delivers the second press's `clicked`
    /// after the pair already closed; it must not open a fresh chain, or the
    /// compose is followed by a drill-in one window later.
    #[test]
    fn the_release_after_a_pair_is_swallowed() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        presses.double_press(CELL, now + Duration::from_millis(120));

        let trailing = presses.click(CELL, now + Duration::from_millis(130));
        assert_eq!(trailing, Outcome::default(), "the release is not a click");
        assert_eq!(presses.deadline(), None, "no new chain was opened");
        assert_eq!(presses.expire(now + Duration::from_secs(1)), None);
    }

    /// Only *one* release is swallowed: the next real click starts its own
    /// chain, so a double click followed by a single one still drills.
    #[test]
    fn a_click_after_the_swallowed_release_is_honoured() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        presses.double_press(CELL, now + Duration::from_millis(120));
        presses.click(CELL, now + Duration::from_millis(130));

        let later = now + Duration::from_secs(2);
        assert_eq!(presses.click(CELL, later).hold_until, Some(later + WINDOW));
        assert_eq!(presses.expire(later + WINDOW), Some(CELL));
    }

    /// A click at a *different* cell inside the window is not a pair: the first
    /// cell drills in (the user did click it) and the second starts its own
    /// chain. Decided by id, so this holds however the grid repainted between.
    #[test]
    fn a_click_at_another_cell_drills_the_first_and_holds_the_second() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        let outcome = presses.click(OTHER, now + Duration::from_millis(120));
        assert_eq!(outcome.drill, Some(CELL));
        assert_eq!(outcome.compose, None);
        assert_eq!(
            outcome.hold_until,
            Some(now + Duration::from_millis(120) + WINDOW)
        );
    }

    /// Two clicks too far apart are two single clicks, not a double.
    #[test]
    fn two_slow_clicks_drill_twice_and_never_compose() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        presses.click(CELL, now);
        let late = now + WINDOW + Duration::from_millis(1);
        let outcome = presses.click(CELL, late);
        assert_eq!(outcome.drill, Some(CELL), "the first click was single");
        assert_eq!(outcome.compose, None);
        assert_eq!(presses.expire(late + WINDOW), Some(CELL));
    }

    /// The automation door dispatches `n_press == 2` straight at the widget
    /// with no first press — convention 11/15's direct actuation. It must still
    /// compose, and it must not leave a chain armed.
    #[test]
    fn a_bare_double_press_still_composes() {
        let mut presses = DayCellPress::new(WINDOW);
        let now = Instant::now();
        let outcome = presses.double_press(CELL, now);
        assert_eq!(outcome.compose, Some(CELL));
        assert_eq!(outcome.drill, None);
        assert_eq!(presses.expire(now + Duration::from_secs(1)), None);
    }

    /// The id is the ui.yaml one, zero-padded — the identity the shared arbiter
    /// pairs on, and the same string the cell is stamped with.
    #[test]
    fn the_pairing_id_is_the_ui_yaml_day_cell_id() {
        assert_eq!(day_cell_id((2026, 7, 9)), "events-day-cell-2026-07-09");
    }
}
