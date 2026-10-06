//! Inline divider rendered between message bubbles when a message's
//! `subject_line` differs from the previous message's effective subject.
//!
//! Mirrors `apps/fauna-windows/.../Controls/SubjectDivider.xaml`. Layout:
//! `[ horizontal rule ]  italic subject text  [ horizontal rule ]`.
//!
//! The `subject-divider` test ID lives on the outer container; the AT-SPI
//! bridge resolves indexed siblings (`subject-divider[0]`, `[1]`, …) by
//! their position in the messages stream.

use fauna_ui_ids as ids;
use gtk::prelude::*;

pub fn build(subject: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    row.set_margin_start(16);
    row.set_margin_end(16);
    row.add_css_class("subject-divider");

    let left = gtk::Separator::new(gtk::Orientation::Horizontal);
    left.set_hexpand(true);
    left.set_valign(gtk::Align::Center);
    row.append(&left);

    let label = gtk::Label::new(Some(subject));
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_use_markup(false);
    // The action layer's `read_subject_dividers` reads the divider's
    // text via `get_text("subject-divider")`. The bridge's
    // `do_get_text` falls back to `get_name()` for widgets without a
    // text interface; on a GtkBox that returns the widget_name (=
    // "subject-divider"), losing the actual subject. Put the test id
    // on the inner Label instead — its accessible name is derived from
    // label text, so `get_text` returns the subject string itself.
    crate::testid::set_test_id(&label, ids::SUBJECT_DIVIDER);
    row.append(&label);

    let right = gtk::Separator::new(gtk::Orientation::Horizontal);
    right.set_hexpand(true);
    right.set_valign(gtk::Align::Center);
    row.append(&right);

    row
}
