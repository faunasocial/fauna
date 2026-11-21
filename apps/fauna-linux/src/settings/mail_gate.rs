//! The mail-usable gate on linux (`docs/goal/ui/mail-settings.md` § Errors &
//! edge cases, the mail-usable gate; `ui/README.md` § Offer only what is
//! possible, rules 2–7).
//!
//! Every mail sub-page — Mail aliases, Mail spam, Export mailbox, Import
//! mailbox, Mail lists, Mail list members — and its settings rail row paints
//! from ONE answer, the shared `MailSettingsSnapshot::mail_usable()` over the
//! session's mail machine (the one the Mail & Calendar page builds and
//! publishes at shell build, so the answer is decided before the person reaches
//! the rail). This app never derives a condition of its own: a domain list or
//! the snapshot's `enabled` read by hand would be a second answer.
//!
//! - absent → the rows are not shown; a page reached by another door paints the
//!   Mail & Calendar page's own unavailable notice;
//! - blocked → the rows are disabled with the shared reason, and a page opened
//!   anyway shows only its heading and the reason (the rail's
//!   `settings-nav-back` is the Back, present on every sub-page);
//! - possible → everything as built.
//!
//! The tui is the reference (`apps/fauna-tui/src/settings/root.rs`'s rail rows
//! and `settings/mod.rs`'s `gate_mail_subpage`).

use adw::prelude::*;
use fauna_core::possibility::Possibility;

/// The rail child names of the six gated sub-pages, in rail order.
pub const MAIL_SUBPAGES: [&str; 6] = [
    "mail-aliases",
    "mail-spam",
    "mail-export",
    "mail-import",
    "mail-lists",
    "mail-list-members",
];

/// The shared answer over the session's LIVE mail machine — turning mail on
/// lights the rows at once. No machine means its build failed (or there is no
/// client at all), which is the shared unbuilt answer: blocked with its own
/// reason, never silent controls (rule 7).
pub(crate) fn mail_usable() -> Possibility {
    match super::mail_machine() {
        Some(machine) => machine.snapshot().mail_usable(),
        None => fauna_client_mail_settings::mail_usable_unbuilt(),
    }
}

/// The text painted for an answer that is not possible — the blocked reason,
/// or the unavailable notice a page reached by another door shows when the box
/// can never serve mail. `None` when possible.
pub(crate) fn gate_text(answer: &Possibility) -> Option<String> {
    match answer {
        Possibility::Possible => None,
        Possibility::Blocked { reason } => Some(reason.resolve(crate::i18n::strings::lookup)),
        Possibility::Absent => {
            Some(crate::i18n::strings::mail_settings::UNAVAILABLE_NOTICE.to_string())
        }
    }
}

/// Put the unbuilt-mail-machine reason on a mail page's `error-message`
/// (`ui/README.md` § Offer only what is possible, rule 7: never silent). A
/// gesture reaching a mail page whose own machine was never built — no
/// client, or its build failed — says so with the shared
/// `mail_usable_unbuilt()` reason instead of doing nothing (tui's
/// `refuse_mail_unbuilt`).
pub(crate) fn refuse_unbuilt(error_label: &gtk::Label) {
    let text = gate_text(&fauna_client_mail_settings::mail_usable_unbuilt());
    super::render_error_label(error_label, text.as_deref());
}

/// Wire each gesture of a mail page whose machine was never built to
/// [`refuse_unbuilt`] on `error_label` — the page's `wire_machine` calls it on
/// its no-machine path, where no live handler gets connected.
pub(crate) fn wire_unbuilt(error_label: &gtk::Label, gestures: &[&gtk::Button]) {
    for button in gestures {
        let error_label = error_label.clone();
        button.connect_clicked(move |_| refuse_unbuilt(&error_label));
    }
}

/// [`wire_unbuilt`] for a page's other gesture kinds: a switch refuses and
/// stays where it was, a dropdown's pick and an entry's commit refuse — none
/// of them is left doing nothing (rule 7).
pub(crate) fn wire_unbuilt_controls(
    error_label: &gtk::Label,
    switches: &[&gtk::Switch],
    dropdowns: &[&gtk::DropDown],
    entries: &[&gtk::Entry],
) {
    for switch in switches {
        let error_label = error_label.clone();
        switch.connect_state_set(move |_, _| {
            refuse_unbuilt(&error_label);
            gtk::glib::Propagation::Stop
        });
    }
    for dropdown in dropdowns {
        let error_label = error_label.clone();
        dropdown.connect_selected_notify(move |_| refuse_unbuilt(&error_label));
    }
    for entry in entries {
        let error_label = error_label.clone();
        entry.connect_activate(move |_| refuse_unbuilt(&error_label));
    }
}

/// A mail sub-page behind the gate: a two-child stack, `open` (the page as
/// built) and `gated` (its heading and the reason only — no form, sheet or
/// wizard whose submit could not succeed).
pub(crate) struct GatedPage {
    pub stack: gtk::Stack,
    reason: gtk::Label,
    #[cfg_attr(not(test), allow(dead_code))]
    heading: gtk::Label,
}

impl GatedPage {
    /// Wrap `page`, whose heading reads `title`.
    pub(crate) fn wrap(page: &impl IsA<gtk::Widget>, title: &str) -> Self {
        let stack = gtk::Stack::new();
        stack.add_named(page, Some("open"));

        let gated = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title(title);
        // The heading the eye reads is the group's title; the marker carries
        // the id and must read the same text back, or `page-heading` answers
        // '' and no test can tell which gated page is up.
        let heading = super::marker(fauna_ui_ids::PAGE_HEADING);
        crate::testid::set_test_text(&heading, title);
        group.set_header_suffix(Some(&heading));
        let reason = gtk::Label::new(None);
        crate::testid::set_test_id(&reason, fauna_ui_ids::MAIL_SUBPAGE_GATE_REASON);
        reason.set_wrap(true);
        reason.set_xalign(0.0);
        reason.set_halign(gtk::Align::Start);
        group.add(&reason);
        gated.add(&group);
        stack.add_named(&gated, Some("gated"));
        stack.set_visible_child_name("open");
        Self {
            stack,
            reason,
            heading,
        }
    }

    /// Show the page, or its heading and the reason, from the one answer.
    pub(crate) fn paint(&self, answer: &Possibility) {
        match gate_text(answer) {
            None => self.stack.set_visible_child_name("open"),
            Some(text) => {
                self.reason.set_label(&text);
                self.stack.set_visible_child_name("gated");
            }
        }
    }

    /// Whether the page is showing its gated face (tests).
    #[cfg(test)]
    pub(crate) fn is_gated(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("gated")
    }

    /// The reason the gated face shows (tests).
    #[cfg(test)]
    pub(crate) fn reason_text(&self) -> String {
        self.reason.label().to_string()
    }

    /// What automation reads back from the gated face's heading (tests).
    #[cfg(test)]
    pub(crate) fn heading_text(&self) -> String {
        crate::automation::find::text_of(self.heading.upcast_ref())
    }

    /// The gated face's heading test id (tests).
    #[cfg(test)]
    pub(crate) fn heading_test_id(&self) -> String {
        self.heading.widget_name().to_string()
    }

    /// The reason label's test id, as automation finds it (tests).
    #[cfg(test)]
    pub(crate) fn reason_test_id(&self) -> String {
        self.reason.widget_name().to_string()
    }
}

/// Paint one rail row from the answer: absent → not shown; blocked → disabled,
/// the reason on the row's caption (within eyeshot, rule 2) and its tooltip;
/// possible → live.
pub(crate) fn paint_rail_row(row: &gtk::ListBoxRow, answer: &Possibility) {
    row.set_visible(answer.is_shown());
    row.set_sensitive(answer.is_possible());
    let reason = match answer {
        Possibility::Blocked { reason } => Some(reason.resolve(crate::i18n::strings::lookup)),
        _ => None,
    };
    row.set_tooltip_text(reason.as_deref());
    if let Some(caption) = crate::views::nav_rail::row_caption(row) {
        caption.set_label(reason.as_deref().unwrap_or(""));
        caption.set_visible(reason.is_some());
    }
    // What was just painted, read by the e2e driver's `blocked_reason`
    // (`reason`, the `possible::paint` contract); empty while the row is live.
    crate::testid::set_test_attr_text(row, "reason", reason.as_deref().unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;
    use fauna_core::localized::LocalizedText;

    fn rail_with_one_row() -> (gtk::Box, gtk::ListBoxRow) {
        let stack = gtk::Stack::new();
        stack.add_named(&gtk::Label::new(None), Some("mail-spam"));
        let back = gtk::Button::new();
        let rail = crate::views::nav_rail::build_nav_rail(
            &back,
            &[("mail-spam", "Spam", "dialog-warning-symbolic", true)],
            &stack,
            fauna_ui_ids::SETTINGS_NAV_ROW,
            |_| None,
        );
        let row = crate::views::nav_rail::rail_list(&rail)
            .row_at_index(0)
            .expect("one row");
        (rail, row)
    }

    /// The rail rows follow the one shared answer: disabled with the reason
    /// on the row while blocked, not shown while absent, live while possible.
    /// Red-verify by making `paint_rail_row` a no-op.
    #[test]
    fn a_rail_row_follows_the_shared_mail_usable_answer() {
        run_on_gtk_thread(|| {
            let (_rail, row) = rail_with_one_row();
            let caption = crate::views::nav_rail::row_caption(&row).expect("caption");

            let off =
                Possibility::blocked(LocalizedText::key("mail_settings.usable_needs_mail_on"));
            paint_rail_row(&row, &off);
            assert!(row.is_visible() && !row.is_sensitive());
            assert!(caption.is_visible());
            assert_eq!(
                caption.label().as_str(),
                crate::i18n::strings::mail_settings::USABLE_NEEDS_MAIL_ON
            );
            assert_eq!(
                crate::testid::test_attr_text(&row, "reason").as_deref(),
                Some(crate::i18n::strings::mail_settings::USABLE_NEEDS_MAIL_ON),
                "the painted reason is published for automation's blocked_reason"
            );

            paint_rail_row(&row, &Possibility::Absent);
            assert!(!row.is_visible());

            paint_rail_row(&row, &Possibility::Possible);
            assert!(row.is_visible() && row.is_sensitive());
            assert!(!caption.is_visible());
            assert_eq!(
                crate::testid::test_attr_text(&row, "reason").as_deref(),
                Some("")
            );
        });
    }

    /// A sub-page opened while mail is not possible shows only its heading
    /// and the reason; a box that can never serve mail shows the unavailable
    /// notice. Red-verify by making `GatedPage::paint` a no-op.
    #[test]
    fn a_mail_subpage_opened_while_not_possible_shows_heading_and_reason_only() {
        run_on_gtk_thread(|| {
            let page = adw::PreferencesPage::new();
            let gated = GatedPage::wrap(&page, "Spam");

            gated.paint(&fauna_client_mail_settings::mail_usable_unbuilt());
            assert!(gated.is_gated());
            // The gated face's heading reads its title back to automation.
            assert_eq!(gated.heading_test_id(), fauna_ui_ids::PAGE_HEADING);
            assert_eq!(gated.heading_text(), "Spam");
            assert_eq!(
                gated.reason_test_id(),
                fauna_ui_ids::MAIL_SUBPAGE_GATE_REASON
            );
            assert_eq!(
                gated.reason_text(),
                crate::i18n::strings::mail_settings::USABLE_UNBUILT
            );

            gated.paint(&Possibility::Absent);
            assert!(gated.is_gated());
            assert_eq!(
                gated.reason_text(),
                crate::i18n::strings::mail_settings::UNAVAILABLE_NOTICE
            );

            gated.paint(&Possibility::Possible);
            assert!(!gated.is_gated());
        });
    }

    /// With no mail machine published (its build failed, or no client), the
    /// answer is the shared unbuilt one — never possible.
    #[test]
    fn no_mail_machine_answers_unbuilt() {
        run_on_gtk_thread(|| {
            assert_eq!(
                mail_usable(),
                fauna_client_mail_settings::mail_usable_unbuilt()
            );
        });
    }
}
