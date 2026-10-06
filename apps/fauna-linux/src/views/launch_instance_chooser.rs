//! Launch-collision chooser — the colliding instance's surface.
//!
//! Shown when a **plain interactive** launch finds that the account it would
//! open is already served by a live instance (`account-scoping.md`
//! § Concurrent instances → "the colliding instance's surface"). ui.yaml page
//! `launch_instance_chooser`, `platforms: [windows, linux, tui]` — linux is
//! the first client to render it, so this file is the reference the windows
//! and tui legs lift from rather than re-derive.
//!
//! Three ways forward, one per ratified element:
//!
//! - `launch-instance-chooser-item` (indexed) — the registry's
//!   **not-currently-served** accounts, from the shared display-only probe
//!   [`fauna_client_accounts::AccountInstanceLock::not_currently_served`].
//!   Picking one makes **this** process that account's bound instance
//!   (`bind_account` + the bound launch seam) — deliberately *not* a third
//!   process: the collision already gave us a spare one.
//! - `launch-instance-focus-existing-button` — raise the running window and
//!   exit, which is what preserves the pre-re-key raise-on-relaunch UX that
//!   GApplication's D-Bus uniqueness used to give every relaunch.
//! - `launch-instance-add-account-button` — *forward* an add-account intent to
//!   the running instance and exit. The onboarding scratchpad belongs to the
//!   primary, so a colliding process never runs the wizard itself (the same
//!   ownership rule as the bound-wizard refusal in `main.rs::refuse_if_bound`).
//!
//! Plus the canonical `error-message` (convention 2), which carries the two
//! ways this surface can fail *after* it renders: the pick lost the race
//! (someone took the account between render and click — arbitration stays at
//! acquire), and the running instance could not be reached.
//!
//! The chooser is strictly a human affordance: a `FAUNA_BOUND_ACCOUNT` launch
//! that collides stays terminally refused in `app.rs`, never routed here —
//! wired IPC must be deterministic.

use fauna_ui_ids as ids;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{Button, Label, Orientation};

use crate::i18n::strings as i18n;
use crate::testid::set_test_id;

/// Handles the chooser exposes to its caller.
pub struct InstanceChooserView {
    pub window: adw::ApplicationWindow,
    /// Per the `OnboardingResult` convention — mirrored into the test-agent
    /// JSON payload by `start_test_agent_if_enabled`.
    pub error_label: Label,
    /// Surface a failure on the canonical `error-message` element without
    /// tearing the chooser down: both callers' failures (`account_taken`,
    /// `no_running_instance`) leave every other choice still valid.
    pub show_error: Rc<dyn Fn(&str)>,
}

/// Build the chooser window.
///
/// `served_label` is the display label of the account that collided (the
/// reason we are here); `choices` are `(actor_id, display label)` pairs for
/// the accounts this process may become, in registry order — already filtered
/// by the shared probe, so an empty slice means "every account is open
/// somewhere" and the list is replaced by that explanation.
///
/// `on_pick` receives the chosen actor id. It is called on the GTK main
/// thread and owns the whole binding outcome — including reporting failure
/// back through [`InstanceChooserView::show_error`].
pub fn build_instance_chooser_window<P, F, A>(
    app: &adw::Application,
    served_label: &str,
    choices: &[(String, String)],
    on_pick: P,
    on_focus_existing: F,
    on_add_account: A,
) -> InstanceChooserView
where
    P: Fn(&str) + 'static,
    F: Fn() + 'static,
    A: Fn() + 'static,
{
    let content = build_chooser_content(
        served_label,
        choices,
        on_pick,
        on_focus_existing,
        on_add_account,
    );

    let window = adw::ApplicationWindow::builder()
        .default_width(560)
        .default_height(420)
        .resizable(false)
        .content(&content.root)
        .build();
    window.set_application(Some(app));

    // Same reasoning as the launch screen: `app.hold()` in `main` keeps the
    // process alive across window rebuilds, so a pre-session window must quit
    // on close or leak a headless zombie. Closing the chooser is a deliberate
    // "never mind" — the running instance is untouched.
    {
        let app_for_quit = app.clone();
        window.connect_close_request(move |_| {
            app_for_quit.quit();
            gtk::glib::Propagation::Proceed
        });
    }

    InstanceChooserView {
        window,
        error_label: content.error_label,
        show_error: content.show_error,
    }
}

/// The chooser's widget tree, window-free.
///
/// Split out from [`build_instance_chooser_window`] so the surface can be
/// unit-tested without an `adw::Application` — the window is a shell, every
/// ratified element lives in here.
pub(crate) struct ChooserContent {
    pub root: gtk::Box,
    pub error_label: Label,
    pub show_error: Rc<dyn Fn(&str)>,
}

pub(crate) fn build_chooser_content<P, F, A>(
    served_label: &str,
    choices: &[(String, String)],
    on_pick: P,
    on_focus_existing: F,
    on_add_account: A,
) -> ChooserContent
where
    P: Fn(&str) + 'static,
    F: Fn() + 'static,
    A: Fn() + 'static,
{
    let outer = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(0)
        .build();
    // The page anchor itself (ui.yaml `launch-instance-chooser`). The
    // automation agent matches on `widget_name()` for any widget, so the
    // container carries it — no shim element standing in for the page.
    set_test_id(&outer, ids::LAUNCH_INSTANCE_CHOOSER);

    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(true);
    header.set_show_start_title_buttons(true);
    outer.append(&header);

    // Convention 2: every page has an `error-message`. Hidden until something
    // fails, so `has_error()` stays false on the happy path.
    let error_label = Label::new(None);
    error_label.set_halign(gtk::Align::Fill);
    error_label.set_xalign(0.0);
    error_label.set_wrap(true);
    error_label.add_css_class("error-banner");
    error_label.set_visible(false);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    outer.append(&error_label);

    let center = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .hexpand(true)
        .build();
    center.set_margin_top(36);
    center.set_margin_bottom(36);
    center.set_margin_start(36);
    center.set_margin_end(36);
    outer.append(&center);

    let title = Label::new(Some(i18n::onboarding::instance_chooser::TITLE));
    title.add_css_class("title-2");
    center.append(&title);

    // Names the account that is already open, so the collision is legible
    // rather than a bare "already running".
    let subtitle = Label::new(Some(&i18n::onboarding::instance_chooser::subtitle(
        served_label,
    )));
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);
    subtitle.set_max_width_chars(52);
    subtitle.add_css_class("dim-label");
    center.append(&subtitle);

    // ── The pickable accounts ─────────────────────────────────────────
    if choices.is_empty() {
        // Every registered account is served somewhere. The list is replaced
        // rather than rendered empty: an empty listbox reads as a loading
        // state, and the user needs to know there is nothing to pick.
        let none = Label::new(Some(i18n::onboarding::instance_chooser::NONE_AVAILABLE));
        none.set_wrap(true);
        none.set_justify(gtk::Justification::Center);
        none.set_max_width_chars(52);
        none.add_css_class("dim-label");
        none.set_margin_top(8);
        center.append(&none);
    } else {
        let group = adw::PreferencesGroup::builder()
            .title(i18n::onboarding::instance_chooser::CHOOSE_ACCOUNT)
            .build();
        group.set_margin_top(8);

        let on_pick = Rc::new(on_pick);
        for (actor_id, label) in choices {
            let row = adw::ActionRow::builder()
                .title(label)
                .activatable(true)
                .build();
            set_test_id(&row, ids::LAUNCH_INSTANCE_CHOOSER_ITEM);
            let actor_for_pick = actor_id.clone();
            let on_pick = on_pick.clone();
            row.connect_activated(move |_| on_pick(&actor_for_pick));
            group.add(&row);
        }
        center.append(&group);
    }

    // ── The two exits ─────────────────────────────────────────────────
    let buttons = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Center)
        .build();
    buttons.set_margin_top(12);

    let focus_btn = Button::with_label(i18n::onboarding::instance_chooser::FOCUS_EXISTING);
    focus_btn.add_css_class("suggested-action");
    set_test_id(&focus_btn, ids::LAUNCH_INSTANCE_FOCUS_EXISTING_BUTTON);
    {
        let on_focus_existing = Rc::new(on_focus_existing);
        focus_btn.connect_clicked(move |_| on_focus_existing());
    }
    buttons.append(&focus_btn);

    let add_btn = Button::with_label(i18n::onboarding::instance_chooser::ADD_ACCOUNT);
    set_test_id(&add_btn, ids::LAUNCH_INSTANCE_ADD_ACCOUNT_BUTTON);
    {
        let on_add_account = Rc::new(on_add_account);
        add_btn.connect_clicked(move |_| on_add_account());
    }
    buttons.append(&add_btn);

    center.append(&buttons);

    let show_error: Rc<dyn Fn(&str)> = {
        let error_label = error_label.clone();
        Rc::new(move |msg: &str| {
            crate::settings::render_error_label(&error_label, Some(msg));
        })
    };

    ChooserContent {
        root: outer,
        error_label,
        show_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use crate::testid::widget_names;

    fn choices() -> Vec<(String, String)> {
        vec![
            ("a".repeat(64), "@ana".to_string()),
            ("b".repeat(64), "@bo".to_string()),
        ]
    }

    fn content_with(choices: &[(String, String)]) -> ChooserContent {
        build_chooser_content("@served", choices, |_| {}, || {}, || {})
    }

    /// The page exposes every ui.yaml ID for `launch_instance_chooser`.
    #[test]
    fn page_exposes_all_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let content = content_with(&choices());
            let names = widget_names(&content.root);
            for id in [
                "launch-instance-chooser",
                "launch-instance-chooser-item",
                "launch-instance-focus-existing-button",
                "launch-instance-add-account-button",
                "error-message",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// `launch-instance-chooser-item` is `indexed: true` in ui.yaml — one row
    /// per offered account, so `chooser-item[1]` addresses the second one.
    #[test]
    fn one_indexed_item_per_offered_account() {
        crate::testid::run_on_gtk_thread(|| {
            let content = content_with(&choices());
            let items = widget_names(&content.root)
                .into_iter()
                .filter(|n| n == "launch-instance-chooser-item")
                .count();
            assert_eq!(items, 2, "one row per offered account");
        });
    }

    /// Clicking a row hands back that row's **actor id**, not its display
    /// label — the label is a handle (or a truncated id) and would bind the
    /// wrong account, or none.
    #[test]
    fn activating_a_row_reports_its_actor_id() {
        crate::testid::run_on_gtk_thread(|| {
            let picked: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
            let sink = picked.clone();
            let content = build_chooser_content(
                "@served",
                &choices(),
                move |actor| sink.borrow_mut().push(actor.to_string()),
                || {},
                || {},
            );
            let mut rows: Vec<gtk::Widget> = Vec::new();
            crate::automation::find::collect_in(
                &content.root.clone().upcast(),
                "launch-instance-chooser-item",
                &mut rows,
            );
            assert_eq!(rows.len(), 2);
            rows[1]
                .clone()
                .downcast::<adw::ActionRow>()
                .expect("chooser items are ActionRows")
                .emit_activate();
            assert_eq!(picked.borrow().as_slice(), [("b".repeat(64))]);
        });
    }

    /// Every account already open elsewhere: the list is replaced by the
    /// explanation, and the two exits survive — a chooser with nothing to
    /// pick must still let the user reach the running window.
    #[test]
    fn with_no_free_accounts_the_exits_remain() {
        crate::testid::run_on_gtk_thread(|| {
            let content = content_with(&[]);
            let names = widget_names(&content.root);
            assert!(
                !names.iter().any(|n| n == "launch-instance-chooser-item"),
                "no rows when every account is served; have {names:?}"
            );
            for id in [
                "launch-instance-focus-existing-button",
                "launch-instance-add-account-button",
            ] {
                assert!(names.iter().any(|n| n == id), "missing exit {id:?}");
            }
        });
    }

    /// `error-message` exists from the first render (convention 2) but stays
    /// hidden until something fails, so `has_error()` is false on arrival and
    /// true once a pick loses the race.
    #[test]
    fn error_message_is_present_but_silent_until_a_failure() {
        crate::testid::run_on_gtk_thread(|| {
            let content = content_with(&choices());
            assert!(!content.error_label.is_visible(), "silent on arrival");
            (content.show_error)("that account was just opened");
            assert!(content.error_label.is_visible());
            assert_eq!(content.error_label.text(), "that account was just opened");
        });
    }
}
