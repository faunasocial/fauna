//! The ward's screen-time lock — linux's rendering of the cross-page
//! "screen time is off right now" surface (`docs/goal/behavior/family-safety.md`
//! § Screen time, Slice E; ui.yaml `global:` elements `screen-time-lock` /
//! `screen-time-lock-message`, user-approved 2026-07-15).
//!
//! **Client-enforced by construction.** The nest cannot see when a child's
//! device is in use and deliberately does not gate on it, so this overlay *is*
//! the enforcement — a conforming-client render rule, exactly like the content
//! pillar's `content_policy` module beside it. § Don't do these is explicit:
//! *"Don't put screen-time or content enforcement on the nest."*
//!
//! **Every decision is shared Rust — including the orchestration, not just
//! the policy.** The clock, the window/budget wiring, the test-clock skew and
//! the heartbeat glue all used to be hand-rolled here AND in tui's twin,
//! byte-identically save for how each app stores the instance — a seventh
//! tui↔linux twin, and the first whose duplication was a full orchestration
//! type rather than a formatter or a dispatch-and-log fn. All of it now lives
//! in [`fauna_core::screen_time::ScreenLockCore`]; this module is the
//! `thread_local!` GTK needs (its callbacks hand it no context, unlike tui's
//! threaded `&mut App`) plus the widgets.
//!
//! **The Family page stays reachable.** The goal-doc invariant is that a locked
//! ward can always see who supervises them and what the policy is, so the
//! overlay covers the page stack only: the header bar keeps the
//! `supervised-indicator` button (which navigates to `family`) live, and the
//! overlay hides itself outright while the family page is showing. A lock that
//! could not be read from is a lock the child cannot understand or appeal.

use fauna_ui_ids as ids;
use std::cell::RefCell;

use gtk::prelude::*;

use fauna_core::screen_time::{ScreenLockCore, ScreenTimePolicy};

/// How often the lock re-evaluates its own verdict against the local clock.
///
/// The policy only changes on a `fauna.family.status` read, but *time* passes
/// continuously, so the surface must re-ask on a tick or a ward already in the
/// app would sail past their bedtime. One minute is the resolution of the
/// policy itself (both bounds and the budget are whole minutes), so a finer
/// tick could not change an answer.
///
/// The same tick drives the usage heartbeat, which is why it must stay at or
/// under [`fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS`]: the engine credits
/// at most one step per call, so a slower tick would silently under-count.
const LOCK_TICK_SECS: u32 = 60;

thread_local! {
    /// The ward's screen-time orchestration — policy, guardian, heartbeat,
    /// test-clock skew, all of it. GTK hands its callbacks no context, so
    /// this lives in a cell rather than threaded through every handler (tui's
    /// twin, threaded per-call, is the ordinary struct field it can afford to
    /// be).
    static CORE: RefCell<ScreenLockCore> = RefCell::new(ScreenLockCore::default());

    /// The mounted lock overlay and the page stack it covers, registered once by
    /// `build_main_window`. Held here so anything that changes the verdict — a
    /// status read, a heartbeat reply, the tick — can repaint through
    /// [`repaint`] without threading two widgets through every call site.
    static SURFACE: RefCell<Option<(gtk::Box, gtk::Stack)>> = const { RefCell::new(None) };
}

/// Register the mounted lock overlay and the stack it covers. Called once from
/// `build_main_window`.
pub fn register_surface(overlay: &gtk::Box, stack: &gtk::Stack) {
    SURFACE.with(|s| *s.borrow_mut() = Some((overlay.clone(), stack.clone())));
}

/// Re-evaluate and repaint the lock wherever it is mounted. The one repaint
/// entry point for callers that do not already hold the widgets.
pub fn repaint() {
    SURFACE.with(|s| {
        if let Some((overlay, stack)) = s.borrow().as_ref() {
            refresh(overlay, stack.visible_child_name().as_deref());
        }
    });
}

/// Whether the app window currently has focus — the honest reading of "the
/// child is looking at this". `false` when no surface is mounted yet.
pub fn window_focused() -> bool {
    SURFACE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|(overlay, _)| overlay.root())
            .and_then(|r| r.downcast::<gtk::Window>().ok())
            .map(|w| w.is_active())
            .unwrap_or(false)
    })
}

/// Advance this module's clock by `secs` **for tests only**, in the accrual
/// steps a real caller would tick in, so the heartbeat credits time exactly as
/// it would over a real interval (the engine caps a single gap at
/// `MAX_ACCRUAL_STEP_SECS`, so one big jump would credit only one step).
///
/// This is convention 14's fake clock: the alternative — a test that sleeps for
/// a heartbeat — is *defunct* under `testing.md` § point 14, not merely slow.
/// Compiled out of release artifacts (convention 15).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub fn advance_test_clock(secs: i64) {
    CORE.with(|c| c.borrow_mut().advance_test_clock(secs));
}

/// Record the supervised viewer's own screen-time policy, guardian, and the
/// day's cross-device usage total, from `fauna.family.status`. Called on every
/// status read — post-auth and on each refresh — so a guardian's policy edit
/// takes effect on the ward's next read rather than needing a restart.
/// Unsupervised (`None` handle) clears the lock.
///
/// `usage_today_minutes` is the ward's own `usage_today_minutes` off that same
/// read; it seeds the heartbeat so the very first paint can already evaluate
/// the budget, instead of leaving a ward over their limit unlocked until the
/// first heartbeat round-trip completes.
pub fn set_ward_screen_time(
    policy: Option<ScreenTimePolicy>,
    guardian_handle: Option<String>,
    usage_today_minutes: Option<u32>,
) {
    CORE.with(|c| {
        c.borrow_mut()
            .set_ward_screen_time(policy, guardian_handle, usage_today_minutes)
    });
}

/// Drop the ward's screen-time state because the **identity is changing** —
/// sign-out, account switch, factory reset. Without this a new account inherits
/// the previous ward's lock, accusing a guardian the user does not have (the
/// bug class `critical_alerts::clear_for_identity_change` exists for).
pub fn clear_for_identity_change() {
    CORE.with(|c| c.borrow_mut().clear_for_identity_change());
}

/// Feed the heartbeat this moment's activity and ask whether a report is due.
///
/// `focused` is whether the app window has focus; the engine is told
/// `focused AND not locked`, because time spent staring at the lock screen is
/// not screen *time* — crediting it would inflate the guardian's readout with
/// minutes the child never spent (§ Screen time: the guardian's surface shows
/// per-ward usage).
///
/// Returns the `(minutes, utc_offset_minutes)` to send with
/// `fauna.family.usage_report`, or `None` when nothing is due. A `Some(0)` is a
/// real answer — the goal doc defines a zero-minute report as a *read*, and it
/// is what lifts a budget lock at local midnight or after a guardian raises the
/// limit. The caller MUST answer with [`report_succeeded`] or [`report_failed`].
pub fn take_due_report(focused: bool) -> Option<(u32, i32)> {
    CORE.with(|c| c.borrow_mut().take_due_report(focused))
}

/// Land a `fauna.family.usage_report` reply — the nest's stamped day bucket and
/// that day's cross-device total.
pub fn report_succeeded(day: i64, day_total_minutes: u32) {
    CORE.with(|c| c.borrow_mut().report_succeeded(day, day_total_minutes));
}

/// A report that never landed: its minutes go back on the unreported pile, so
/// a nest blip cannot quietly forgive a ward's screen time.
pub fn report_failed() {
    CORE.with(|c| c.borrow_mut().report_failed());
}

/// The current lock verdict as the localized message to display, or `None` for
/// "not locked". The single decision point: both the overlay's visibility and
/// its text come from here, so they cannot disagree.
fn current_lock_message() -> Option<String> {
    CORE.with(|c| c.borrow().lock_message(crate::i18n::strings::lookup))
}

/// Build the lock overlay. Mounted once, *over* the content stack (not over the
/// whole window) so the header bar's `supervised-indicator` stays clickable —
/// the ward's route to the Family page, which § Screen time requires stay
/// reachable read-only while locked.
///
/// Returns the widget to hand to `gtk::Overlay::add_overlay`.
pub fn build_lock_overlay() -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 12);
    container.set_widget_name("screen-time-lock");
    container.add_css_class("screen-time-lock");
    container.set_valign(gtk::Align::Fill);
    container.set_halign(gtk::Align::Fill);
    container.set_visible(false);
    // Opaque and hit-blocking: the point of the lock is that the page behind it
    // cannot be read or used. `can_target` on a visible overlay child already
    // swallows clicks; the background class makes it opaque.
    container.add_css_class("background");

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 12);
    inner.set_valign(gtk::Align::Center);
    inner.set_halign(gtk::Align::Center);
    inner.set_vexpand(true);

    let title = gtk::Label::new(Some(crate::i18n::strings::family::SCREEN_LOCK_TITLE));
    title.add_css_class("title-1");
    inner.append(&title);

    // `screen-time-lock-message` — the explanation line, naming the policy and
    // the guardian and nothing about what the ward was doing.
    let message = gtk::Label::new(None);
    message.add_css_class("body");
    message.set_wrap(true);
    message.set_justify(gtk::Justification::Center);
    crate::testid::set_test_id(&message, ids::SCREEN_TIME_LOCK_MESSAGE);
    inner.append(&message);

    let hint = gtk::Label::new(Some(crate::i18n::strings::family::SCREEN_LOCK_FAMILY_HINT));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_justify(gtk::Justification::Center);
    inner.append(&hint);

    container.append(&inner);
    container
}

/// Re-evaluate the lock and paint it. Called on every `fauna.family.status`
/// read, on every page change, and on the one-minute tick — the three ways the
/// verdict can change (policy, which page is showing, the clock).
///
/// `current_page` is the content stack's visible child name; the family page is
/// deliberately exempt so the ward can always read their own policy.
pub fn refresh(overlay: &gtk::Box, current_page: Option<&str>) {
    let on_family_page = current_page == Some("family");
    match current_lock_message() {
        Some(message) if !on_family_page => {
            if let Some(label) = find_message_label(overlay) {
                label.set_text(&message);
            }
            overlay.set_visible(true);
        }
        _ => overlay.set_visible(false),
    }
}

/// The `screen-time-lock-message` label inside the overlay built above.
fn find_message_label(overlay: &gtk::Box) -> Option<gtk::Label> {
    let inner = overlay.first_child()?;
    let mut child = inner.first_child();
    while let Some(w) = child {
        if let Ok(label) = w.clone().downcast::<gtk::Label>()
            && label.widget_name() == "screen-time-lock-message"
        {
            return Some(label);
        }
        child = w.next_sibling();
    }
    None
}

/// Start the one-minute re-evaluation tick. Idempotent per window: called once
/// from `build_main_window`.
pub fn start_lock_tick(overlay: gtk::Box, stack: gtk::Stack) {
    glib::timeout_add_seconds_local(LOCK_TICK_SECS, move || {
        refresh(&overlay, stack.visible_child_name().as_deref());
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy that locks at every minute of the day: an empty window is
    /// write-refused, so it can only be corrupt state, and it fails closed.
    fn always_locked() -> ScreenTimePolicy {
        ScreenTimePolicy {
            window_start: Some(600),
            window_end: Some(600),
            daily_minutes: None,
        }
    }

    /// The one rule this module owns that shared Rust cannot: the family page
    /// is exempt, so a locked ward can always read who supervises them and
    /// what the policy is (`family-safety.md` § Screen time — *"the Family page
    /// stays reachable read-only"*). Everything else about the verdict —
    /// including the clock/window/budget wiring this module used to hand-roll
    /// itself — is `fauna_core::screen_time::ScreenLockCore`'s own coverage
    /// now (`screen_lock_core_tests`); this is the one test left proving the
    /// `thread_local!` wiring above actually reaches it.
    ///
    /// Drives the real [`refresh`] against the real overlay widget rather than
    /// re-stating its condition — a test that restates the rule passes just as
    /// happily with the rule inverted.
    #[test]
    fn the_family_page_is_exempt_from_the_lock() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let overlay = build_lock_overlay();
            set_ward_screen_time(Some(always_locked()), Some("mum".into()), None);

            refresh(&overlay, Some("feed"));
            assert!(overlay.is_visible(), "an ordinary page locks");
            let painted = find_message_label(&overlay).expect("the message label exists");
            assert!(
                painted.text().contains("mum"),
                "the lock names the guardian, got {:?}",
                painted.text()
            );

            refresh(&overlay, None);
            assert!(overlay.is_visible(), "an unnamed page locks");

            refresh(&overlay, Some("family"));
            assert!(
                !overlay.is_visible(),
                "the family page stays reachable read-only while locked"
            );

            // …and leaving the family page locks again — the exemption is about
            // the page being shown, not a one-way unlock.
            refresh(&overlay, Some("feed"));
            assert!(overlay.is_visible(), "leaving the family page re-locks");

            clear_for_identity_change();
            refresh(&overlay, Some("feed"));
            assert!(!overlay.is_visible(), "an identity change drops the lock");
        });
    }
}
