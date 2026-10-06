//! Press-pair arbitration: how two plain pointer presses become one press or a
//! pair, and which of an element's two gestures that makes fire.
//!
//! **Why any of this exists.** A single press and a double press at the same
//! element are inherently ambiguous: you only learn a press was the first half
//! of a pair when the second one arrives. Acting on the first press immediately
//! is what made the double press *unreachable for a real user* on three apps at
//! once — the one element family that defines both gestures is the month grid's
//! `events-day-cell-{YYYY-MM-DD}` (single press drills into Day view for that
//! date, double press opens the new-event compose prefilled with it —
//! `docs/goal/ui/events.md` § Layout & flow, the Outlook model), and the
//! drill-in *repaints the page*, so the second press lands on whatever the Day
//! view just put at those coordinates. On tui and windows that was measured as a
//! compose opening at a time-slot's own time rather than the cell's; on linux
//! the drill-in unmaps the cell outright.
//!
//! **The shape.** Defer the first press's gesture until the double-press window
//! closes, so exactly one of the two ever fires. Two deliberate properties:
//!
//! - **Only an element that actually defines a second gesture is ever held.**
//!   An ordinary button still actuates on the press that hit it — no control in
//!   the app pays this latency for a gesture it does not have.
//! - **A pair is decided by element *identity*, never by position.** A push
//!   event or a poll tick can repaint between the two presses and renumber the
//!   list; the id survives that, and a caller re-resolves through it rather than
//!   trusting the position it recorded.
//!
//! [`Arbiter`] is UI-free and clock-injected — the caller owns the timer, and
//! every method takes `now` — so the decision logic is pinned by tier_1 tests
//! with synthetic instants rather than by a test racing a wall clock
//! (`docs/goal/architecture/e2e-conventions.md` point 14). It never reads the
//! clock itself, which is also what keeps it compiling for wasm, where
//! `Instant::now()` panics.
//!
//! **Lifted here from `apps/fauna-tui/src/press.rs` 2026-08-12** (priority #2)
//! when linux needed the same machine: the state machine is the shared half and
//! only the payload differs per app — tui pairs a `HitTarget`, linux a calendar
//! date. Each app keeps its own thin actuation glue. windows' equivalent is
//! `FaunaApp.Core.Calendar.DayCellClickArbiter` (C#); apple needs none, because
//! SwiftUI orders `.onTapGesture(count: 2)` ahead of `count: 1` for you.

use std::time::{Duration, Instant};

/// How long after a press a second one still pairs with it.
///
/// A constant rather than the OS's own double-click time, which windows reads
/// from `GetDoubleClickTime()`: a terminal application has no such setting to
/// read on any platform tui runs on, and GTK's `gtk-double-click-time` is a
/// per-desktop setting linux would have to plumb through the same seam for no
/// behavioural gain. Ratified for tui in `docs/goal/ui/events.md`
/// § Implementation status today ("the 400 ms threshold that synthesizes a
/// double-press from two real mouse presses lives only on the human path").
pub const DOUBLE_PRESS_WINDOW: Duration = Duration::from_millis(400);

/// One press, as the frame that received it saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit<P> {
    /// Whatever the caller needs to actuate this press later — a position in
    /// *that* frame's lists, a date, a widget handle. Opaque here: the arbiter
    /// never compares it, precisely because a payload that encodes a position
    /// goes stale across a repaint.
    pub payload: P,
    /// The element's id — the identity that survives a repaint, which a
    /// position does not. This is what decides a pair.
    pub id: String,
    /// Whether the element defines a usable second gesture. Mirrors the
    /// caller's own actuation gate, so a press is never held for a gesture that
    /// would then be refused.
    pub has_double: bool,
}

/// What one press does, once the arbiter has decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act<P> {
    /// Actuate the element's first gesture — a plain click.
    Single(Hit<P>),
    /// Actuate the element's **second** gesture: the pair closed.
    Double(Hit<P>),
}

/// Everything one press causes, in the order given.
///
/// Two things can happen at once because a held press is not cancelled by a
/// press somewhere else — the user made both, and they made the held one first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision<P> {
    /// A held press this one did not pair with. Actuate its **first** gesture
    /// before the new press's, so an earlier click is never swallowed.
    pub flush: Option<Hit<P>>,
    /// What the new press itself does.
    pub act: Option<Act<P>>,
    /// A newly held press's deadline — arm the timer here.
    pub hold_until: Option<Instant>,
}

// Hand-written rather than derived: `#[derive(Default)]` on a generic struct
// bounds `P: Default`, and a payload has no reason to have a default.
impl<P> Default for Decision<P> {
    fn default() -> Self {
        Self {
            flush: None,
            act: None,
            hold_until: None,
        }
    }
}

/// The press-chain state machine. UI-free and clock-injected: every method
/// takes `now`, so tier_1 tests drive it with synthetic instants.
#[derive(Debug)]
pub struct Arbiter<P> {
    window: Duration,
    /// The press waiting to learn whether it was half of a pair, and when that
    /// question closes.
    held: Option<(Hit<P>, Instant)>,
}

impl<P> Arbiter<P> {
    pub fn new(window: Duration) -> Self {
        Self { window, held: None }
    }

    /// When the held press's window closes, or `None` with nothing held — the
    /// deadline the event loop sleeps until.
    ///
    /// Absolute, not a duration: the loop re-arms this every spin, and a
    /// relative sleep would restart the countdown on every unrelated event.
    pub fn deadline(&self) -> Option<Instant> {
        self.held.as_ref().map(|&(_, due)| due)
    }

    /// A left press landed on `hit` (`None` off every painted element).
    pub fn press(&mut self, hit: Option<Hit<P>>, now: Instant) -> Decision<P> {
        // A pair is two presses at the same ELEMENT inside the window. Compared
        // by id: a payload is a position in one frame's element list, and an
        // async repaint between the two presses renumbers it.
        let paired = match (&self.held, &hit) {
            (Some((held, due)), Some(new)) => held.id == new.id && now <= *due,
            _ => false,
        };
        if paired {
            let (held, _) = self.held.take().expect("a pair implies a held press");
            // The pair ends the chain: a third press starts a fresh one rather
            // than firing the second gesture again.
            return Decision {
                act: Some(Act::Double(held)),
                ..Decision::default()
            };
        }
        // Not a pair, so whatever was held was a single press after all.
        let flush = self.held.take().map(|(held, _)| held);
        let Some(hit) = hit else {
            return Decision {
                flush,
                ..Decision::default()
            };
        };
        if !hit.has_double {
            return Decision {
                flush,
                act: Some(Act::Single(hit)),
                ..Decision::default()
            };
        }
        let due = now + self.window;
        self.held = Some((hit, due));
        Decision {
            flush,
            hold_until: Some(due),
            ..Decision::default()
        }
    }

    /// Drop the held press **without** actuating it, returning it.
    ///
    /// For a caller whose toolkit decided the pair itself: GTK reports
    /// `n_press == 2` directly, so linux closes the pair on the toolkit's word
    /// and must then discard the half it was holding rather than let it drill
    /// in a window later. Distinct from [`expire`](Self::expire), which is the
    /// window closing *unpaired* and therefore actuates.
    pub fn cancel(&mut self) -> Option<Hit<P>> {
        self.held.take().map(|(hit, _)| hit)
    }

    /// The window closed with no second press: the held one was single after
    /// all. `None` while it is still open, so a spurious wake-up is harmless.
    pub fn expire(&mut self, now: Instant) -> Option<Hit<P>> {
        let &(_, due) = self.held.as_ref()?;
        if now < due {
            return None;
        }
        self.held.take().map(|(hit, _)| hit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload stands in for whatever the app carries: a frame-local
    /// position, exactly the thing that goes stale across a repaint.
    fn hit(id: &str, index: usize, has_double: bool) -> Hit<usize> {
        Hit {
            payload: index,
            id: id.to_string(),
            has_double,
        }
    }

    /// The whole point of the module: a control with no second gesture keeps
    /// actuating on the press that hit it. A blanket deferral would put 400 ms
    /// between every click in the app and its effect.
    #[test]
    fn an_ordinary_control_actuates_on_the_first_press() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        let decision = arbiter.press(Some(hit("feed-refresh-button", 3, false)), now);
        assert_eq!(
            decision.act,
            Some(Act::Single(hit("feed-refresh-button", 3, false)))
        );
        assert_eq!(decision.hold_until, None, "nothing to wait for");
        assert_eq!(arbiter.deadline(), None);
    }

    /// An element that defines both gestures holds its first one until the
    /// question "was that half of a pair?" can be answered.
    #[test]
    fn an_element_with_two_gestures_holds_its_first_press() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        let decision = arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        assert_eq!(decision.act, None, "nothing fires yet");
        assert_eq!(decision.hold_until, Some(now + DOUBLE_PRESS_WINDOW));
        assert_eq!(arbiter.deadline(), Some(now + DOUBLE_PRESS_WINDOW));
    }

    /// Second press at the same element inside the window: the pair closes and
    /// only the second gesture fires — the first one never does.
    #[test]
    fn a_second_press_at_the_same_element_closes_the_pair() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        let decision = arbiter.press(
            Some(hit("events-day-cell-2026-07-09", 7, true)),
            now + Duration::from_millis(120),
        );
        assert_eq!(
            decision.act,
            Some(Act::Double(hit("events-day-cell-2026-07-09", 7, true)))
        );
        assert_eq!(decision.flush, None, "the held press must not ALSO fire");
        assert_eq!(arbiter.deadline(), None, "the chain is over");
    }

    /// **The regression this module exists for.** A repaint between the two
    /// presses renumbers the element list; the pair is decided by id, so it
    /// survives that. tui's pre-2026-08-11 code compared the position and could
    /// not.
    #[test]
    fn a_pair_survives_the_list_being_renumbered_between_the_two_presses() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        // Same element, now painted three rows further down.
        let decision = arbiter.press(
            Some(hit("events-day-cell-2026-07-09", 10, true)),
            now + Duration::from_millis(120),
        );
        assert!(
            matches!(decision.act, Some(Act::Double(ref h)) if h.id == "events-day-cell-2026-07-09"),
            "a moved element is still the same element: {:?}",
            decision.act
        );
    }

    /// The converse, and the reason position alone was never enough: a
    /// *different* element that happens to have taken over the recorded index
    /// is not the other half of a pair.
    #[test]
    fn a_different_element_at_the_same_index_is_not_a_pair() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        let decision = arbiter.press(
            Some(hit("events-day-cell-2026-07-16", 7, true)),
            now + Duration::from_millis(120),
        );
        assert_eq!(decision.act, None, "the new press holds on its own account");
        assert_eq!(
            decision.flush,
            Some(hit("events-day-cell-2026-07-09", 7, true)),
            "the first cell's own single-press gesture must still fire"
        );
    }

    /// Too slow to pair: the held press was a single one, and the new press
    /// starts its own chain.
    #[test]
    fn a_press_past_the_window_flushes_the_held_one_and_starts_a_new_chain() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        let late = now + DOUBLE_PRESS_WINDOW + Duration::from_millis(1);
        let decision = arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), late);
        assert_eq!(
            decision.flush,
            Some(hit("events-day-cell-2026-07-09", 7, true)),
            "the first press drills in after all"
        );
        assert_eq!(decision.act, None, "the second press is now a first press");
        assert_eq!(decision.hold_until, Some(late + DOUBLE_PRESS_WINDOW));
    }

    /// The timer path: nothing is released while the window is still open, so a
    /// spurious wake-up costs nothing.
    #[test]
    fn expiry_releases_the_held_press_but_only_once_the_window_closed() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        assert_eq!(
            arbiter.expire(now + Duration::from_millis(399)),
            None,
            "still inside the window"
        );
        assert_eq!(
            arbiter.expire(now + DOUBLE_PRESS_WINDOW),
            Some(hit("events-day-cell-2026-07-09", 7, true))
        );
        assert_eq!(arbiter.deadline(), None);
        assert_eq!(
            arbiter.expire(now + DOUBLE_PRESS_WINDOW),
            None,
            "a released press is released once"
        );
    }

    /// A press on empty space still ends the held press's chain — the user
    /// moved on, and the click they made first must not be swallowed.
    #[test]
    fn a_press_off_every_painted_row_still_flushes_the_held_press() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        let decision = arbiter.press(None, now + Duration::from_millis(120));
        assert_eq!(
            decision.flush,
            Some(hit("events-day-cell-2026-07-09", 7, true))
        );
        assert_eq!(decision.act, None);
        assert_eq!(arbiter.deadline(), None);
    }

    /// A pair ends the chain: the third press of a fast triple starts a fresh
    /// one rather than firing the second gesture again.
    #[test]
    fn a_third_press_starts_a_fresh_chain() {
        let mut arbiter = Arbiter::new(DOUBLE_PRESS_WINDOW);
        let now = Instant::now();
        arbiter.press(Some(hit("events-day-cell-2026-07-09", 7, true)), now);
        arbiter.press(
            Some(hit("events-day-cell-2026-07-09", 7, true)),
            now + Duration::from_millis(80),
        );
        let decision = arbiter.press(
            Some(hit("events-day-cell-2026-07-09", 7, true)),
            now + Duration::from_millis(160),
        );
        assert_eq!(decision.act, None, "a fresh chain holds");
        assert_eq!(decision.flush, None, "nothing was pending to flush");
        assert_eq!(
            decision.hold_until,
            Some(now + Duration::from_millis(160) + DOUBLE_PRESS_WINDOW)
        );
    }
}
