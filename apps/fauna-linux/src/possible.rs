//! The possibility seam on linux: one place a widget is painted from its
//! shared-Rust predicate's answer (`docs/goal/ui/README.md` § Offer only what
//! is possible, rule 6).
//!
//! A page never decides on its own whether an affordance can succeed now. It
//! asks the predicate beside the action, which answers in
//! [`fauna_core::possibility::Possibility`], and hands that answer to
//! [`paint`]: `Absent` hides the widget, `Blocked` leaves it shown but
//! insensitive with the reason on the page's own reason line (rule 2: the
//! reason within eyeshot), `Possible` makes it live. The page writes neither
//! the sensitivity nor the reason line itself, so the two cannot disagree with
//! the answer.
//!
//! Every painted widget is recorded, like the offline gate's declarations
//! (`offline_gate.rs`), so the in-process walk's `enabled-means-possible`
//! invariant (`walk.rs`, I9) can check that each widget still shows what its
//! answer says. A widget a later write re-enabled past the seam is exactly
//! what that check reds on. This is the twin of tui's `Element::possible` and
//! `App::page_elements` seam (`apps/fauna-tui/src/app.rs`), and apple's
//! `.faunaPossible(_:)`.
//!
//! The offline gate composes with it unchanged: it records the page's own
//! sensitivity (what [`paint`] wrote) as the page's intent and only ever
//! narrows it while the link is down.

use fauna_core::possibility::Possibility;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;

/// One painted widget: the widget, the label its reason goes on (if the page
/// has one), and the answer it was last painted from.
struct Declaration {
    widget: glib::WeakRef<gtk::Widget>,
    /// Read only by the walk's I9 through [`declarations`] (test builds).
    #[cfg_attr(not(test), allow(dead_code))]
    reason_line: Option<glib::WeakRef<gtk::Label>>,
    answer: Possibility,
}

thread_local! {
    /// Every widget [`paint`] has painted on this GTK thread, dead entries
    /// pruned on each paint.
    static DECLARED: RefCell<Vec<Declaration>> = const { RefCell::new(Vec::new()) };
}

/// Paint `widget` from `answer`, and its reason onto `reason_line` when the
/// page has one (without one, the reason rides the widget's tooltip, linux's
/// disabled-with-a-reason idiom). Call it on every refresh, with the answer
/// the predicate gives now.
pub(crate) fn paint(
    widget: &impl IsA<gtk::Widget>,
    reason_line: Option<&gtk::Label>,
    answer: Possibility,
) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    widget.set_visible(answer.is_shown());
    widget.set_sensitive(answer.is_possible());
    let reason = reason_text(&answer);
    match reason_line {
        Some(line) => match &reason {
            Some(text) => {
                line.set_text(text);
                line.set_visible(true);
            }
            None => line.set_visible(false),
        },
        None => widget.set_tooltip_text(reason.as_deref()),
    }
    // What was just painted, read by the e2e driver's `blocked_reason`
    // (`reason`); empty while the widget is live.
    crate::testid::set_test_attr_text(widget, "reason", reason.as_deref().unwrap_or(""));
    DECLARED.with_borrow_mut(|declared| {
        declared.retain(|d| d.widget.upgrade().is_some());
        match declared
            .iter_mut()
            .find(|d| d.widget.upgrade().as_ref() == Some(widget))
        {
            Some(d) => d.answer = answer,
            None => declared.push(Declaration {
                widget: widget.downgrade(),
                reason_line: reason_line.map(|line| line.downgrade()),
                answer,
            }),
        }
    });
}

/// The reason `answer` gives, resolved to this app's string; `None` while it is
/// possible or absent.
pub(crate) fn reason_text(answer: &Possibility) -> Option<String> {
    answer
        .reason()
        .map(|reason| reason.resolve(crate::i18n::strings::lookup))
}

/// The answer `widget` was last painted from, for a press whose handler holds
/// no copy of the read it was painted from: the click asks it again and sends
/// nothing unless it is possible (`ui/README.md` § Offer only what is
/// possible, rule 6). `None` for a widget never painted through [`paint`].
pub(crate) fn painted_answer(widget: &impl IsA<gtk::Widget>) -> Option<Possibility> {
    let widget = widget.upcast_ref::<gtk::Widget>();
    DECLARED.with_borrow(|declared| {
        declared
            .iter()
            .find(|d| d.widget.upgrade().as_ref() == Some(widget))
            .map(|d| d.answer.clone())
    })
}

/// Whether `widget`'s painted answer (see [`painted_answer`]) is possible now.
pub(crate) fn painted_possible(widget: &impl IsA<gtk::Widget>) -> bool {
    painted_answer(widget).is_some_and(|answer| answer.is_possible())
}

/// The press half of a draft gate (`docs/goal/behavior/admin.md` § Errors &
/// edge cases, *A form's submit is live only for a draft the nest would
/// accept*): the submit asks the answer it was painted from again and, when
/// that answer is not possible, shows its reason on the page's error line
/// (`error_line`) and sends nothing. Returns `true` when the press is refused.
/// The typed draft is left as it stands.
pub(crate) fn refused_at_press(answer: &Possibility, error_line: &gtk::Label) -> bool {
    if answer.is_possible() {
        return false;
    }
    if let Some(text) = reason_text(answer) {
        error_line.set_text(&text);
        error_line.set_visible(true);
    }
    true
}

/// The press half for a switch whose flip writes what a read decides
/// (`docs/goal/behavior/admin.md` § Errors & edge cases, *Controls wait for
/// their read*): a `gtk::Switch` has already flipped when its `active-notify`
/// fires, so a refused flip — `answer` not possible — says why on
/// `error_line` and puts the switch back under `syncing` (the page's echo
/// guard), so nothing is sent and no guessed value is shown. Returns `true`
/// when the flip is refused.
pub(crate) fn refused_toggle(
    switch: &gtk::Switch,
    answer: &Possibility,
    error_line: &gtk::Label,
    syncing: &std::cell::Cell<bool>,
) -> bool {
    if !refused_at_press(answer, error_line) {
        return false;
    }
    syncing.set(true);
    switch.set_active(!switch.is_active());
    syncing.set(false);
    true
}

/// A live painted widget as the walk reads it (the walk is `#[cfg(test)]`).
#[cfg(test)]
pub(crate) struct Painted {
    pub widget: gtk::Widget,
    pub reason_line: Option<gtk::Label>,
    pub answer: Possibility,
}

/// Every painted widget still alive on this thread, in paint order.
#[cfg(test)]
pub(crate) fn declarations() -> Vec<Painted> {
    DECLARED.with_borrow(|declared| {
        declared
            .iter()
            .filter_map(|d| {
                Some(Painted {
                    widget: d.widget.upgrade()?,
                    reason_line: d.reason_line.as_ref().and_then(|l| l.upgrade()),
                    answer: d.answer.clone(),
                })
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::localized::LocalizedText;

    #[test]
    fn each_answer_paints_its_own_state() {
        crate::testid::run_on_gtk_thread(|| {
            let button = gtk::Button::with_label("Continue");
            let line = gtk::Label::new(None);
            paint(
                &button,
                Some(&line),
                Possibility::blocked(LocalizedText::key(
                    "onboarding.vps_config.status_pick_provider",
                )),
            );
            assert!(button.is_visible() && !button.is_sensitive());
            assert!(line.is_visible() && !line.text().is_empty());
            paint(&button, Some(&line), Possibility::Possible);
            assert!(button.is_visible() && button.is_sensitive());
            assert!(!line.is_visible());
            paint(&button, Some(&line), Possibility::Absent);
            assert!(!button.is_visible());
            let mine: Vec<_> = declarations()
                .into_iter()
                .filter(|p| p.widget == *button.upcast_ref::<gtk::Widget>())
                .collect();
            assert_eq!(mine.len(), 1, "a repaint updates its one record");
            assert_eq!(mine[0].answer, Possibility::Absent);
        });
    }

    /// `admin.md` § Errors & edge cases, *Controls wait for their read*: a
    /// switch flipped before its read is put back with the reason on the
    /// page's error line, the put-back under the echo guard; a possible flip
    /// stands.
    #[test]
    fn a_refused_toggle_is_put_back_with_its_reason() {
        crate::testid::run_on_gtk_thread(|| {
            let switch = gtk::Switch::new();
            let error = gtk::Label::new(None);
            error.set_visible(false);
            let syncing = std::cell::Cell::new(false);
            switch.set_active(true);
            assert!(refused_toggle(
                &switch,
                &Possibility::once_read(false),
                &error,
                &syncing
            ));
            assert!(!switch.is_active(), "the flip is put back");
            assert!(!syncing.get());
            assert!(error.is_visible());
            assert_eq!(
                error.text().as_str(),
                fauna_i18n::strings::common::STILL_LOADING
            );

            switch.set_active(true);
            assert!(!refused_toggle(
                &switch,
                &Possibility::Possible,
                &error,
                &syncing
            ));
            assert!(switch.is_active(), "a possible flip stands");
        });
    }
}
