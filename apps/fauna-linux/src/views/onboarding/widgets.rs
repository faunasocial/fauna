use adw::prelude::*;

/// Primary action button — pill, suggested-action, hexpand. Matches
/// identity_choice's "create-identity-button" / "import-identity-button"
/// visual language.
pub fn primary_button(label: &str, testid: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("pill");
    b.add_css_class("suggested-action");
    b.set_hexpand(true);
    crate::testid::set_test_id(&b, testid);
    b
}

/// Secondary action button — pill, no suggested-action accent. Use for
/// back/cancel-style affordances.
pub fn secondary_button(label: &str, testid: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("pill");
    b.set_hexpand(true);
    crate::testid::set_test_id(&b, testid);
    b
}

/// Build a wizard back button with the standard "setup.back" i18n label
/// and the given testid (e.g. "nest-mode-select-back-button").
pub fn back_button(testid: &str) -> gtk::Button {
    let label = crate::i18n::strings::lookup("setup.back")
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Back".to_string());
    let b = gtk::Button::with_label(&label);
    crate::testid::set_test_id(&b, testid);
    b
}

/// Build a horizontal nav row at the bottom of a wizard page.
/// Back is left-aligned; the primary action(s) are right-aligned. The row
/// itself hexpands so the two ends spread to the page width, with a flexible
/// spacer in between. Mutates the buttons it receives (sets `halign` and
/// resets `hexpand`) so callers should build the buttons first, then call
/// `nav_row` and append the returned row to the page.
///
/// For pages with two primary buttons toggled by state (e.g. server's
/// continue/provision pair), pass both — the spacer keeps `back` on the left
/// while both candidates sit on the right; only one is `set_visible(true)`
/// at any given moment.
pub fn nav_row(back: &gtk::Button, primary: &[&gtk::Button]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_hexpand(true);
    row.set_margin_top(12);

    back.set_halign(gtk::Align::Start);
    back.set_hexpand(false);
    row.append(back);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);

    for btn in primary {
        btn.set_halign(gtk::Align::End);
        btn.set_hexpand(false);
        row.append(*btn);
    }

    row
}

/// Build a wizard-page outer container that matches identity_choice:
/// vertical box, halign=Center, 32px top/bottom margin, 48px left/right
/// margin. Returns (outer, content) — outer goes into the GTK Stack;
/// callers append children to content.
pub fn wizard_page() -> (gtk::Box, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_valign(gtk::Align::Center);
    outer.set_halign(gtk::Align::Center);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(32);
    content.set_margin_bottom(32);
    content.set_margin_start(48);
    content.set_margin_end(48);
    content.set_halign(gtk::Align::Center);

    outer.append(&content);
    (outer, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multiple helpers exercised in a single `#[test]` to amortize the
    /// GTK init cost. Cross-test thread coordination is handled by
    /// `run_on_gtk_thread()` (see testid.rs).
    #[test]
    fn widgets_helpers_have_expected_shape() {
        crate::testid::run_on_gtk_thread(|| {
            let primary = primary_button("Verify", "test-verify");
            assert!(primary.css_classes().contains(&"pill".into()));
            assert!(primary.css_classes().contains(&"suggested-action".into()));
            assert_eq!(primary.widget_name(), "test-verify");
            assert!(primary.hexpands());

            let secondary = secondary_button("Back", "test-back");
            assert!(secondary.css_classes().contains(&"pill".into()));
            assert!(!secondary.css_classes().contains(&"suggested-action".into()));
            assert_eq!(secondary.widget_name(), "test-back");

            // nav_row: back on the left, primary on the right, spacer between.
            // Building bare gtk::Buttons matches Task 5 usage (Task 6 swaps to
            // primary_button / secondary_button).
            let back = gtk::Button::with_label("Back");
            let cont = gtk::Button::with_label("Continue");
            let row = nav_row(&back, &[&cont]);
            assert!(row.hexpands());
            assert_eq!(back.halign(), gtk::Align::Start);
            assert!(!back.hexpands());
            assert_eq!(cont.halign(), gtk::Align::End);
            assert!(!cont.hexpands());
            // Three children: back, spacer, primary.
            let mut child_count = 0;
            let mut child = row.first_child();
            while let Some(w) = child {
                child_count += 1;
                child = w.next_sibling();
            }
            assert_eq!(
                child_count, 3,
                "nav_row must contain back + spacer + 1 primary"
            );

            // Multi-primary case (used by ServerPage's continue + provision pair):
            // both candidates appended on the right, spacer keeps back on the left.
            let back2 = gtk::Button::with_label("Back");
            let p1 = gtk::Button::with_label("Continue");
            let p2 = gtk::Button::with_label("Provision");
            let row2 = nav_row(&back2, &[&p1, &p2]);
            let mut count2 = 0;
            let mut c = row2.first_child();
            while let Some(w) = c {
                count2 += 1;
                c = w.next_sibling();
            }
            assert_eq!(
                count2, 4,
                "nav_row with 2 primaries must contain back + spacer + 2 primaries"
            );
        });
    }
}
