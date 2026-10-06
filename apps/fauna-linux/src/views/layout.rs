//! Shared widget-layout helpers for the settings + admin shells.

use adw::prelude::*;

/// Pin every clamped page column to the window's left edge instead of GTK's
/// default centering.
///
/// `adw::PreferencesPage` wraps its groups in an internal `AdwClampScrollable`
/// whose child defaults to `halign: Fill`, which `AdwClampLayout` renders
/// *centered* once the window grows wider than the clamp's maximum size — so on
/// a wide window every settings/admin page floats in the middle with large
/// empty gutters. Setting that child to `halign: Start` keeps the readable
/// max-width column but left-aligns it.
///
/// Recursively walks `root`'s widget tree and re-aligns every `adw::Clamp` /
/// `adw::ClampScrollable` it finds, so a single call on a shell's content box
/// covers all of its stacked pages — no per-page wiring at the ~20 mount sites.
/// Defensive: a clamp with no child (or a future libadwaita template change
/// that hides the clamp) is simply skipped, so the worst case is a page that
/// stays centered, never a panic.
pub fn left_align_clamped_pages(root: &impl IsA<gtk::Widget>) {
    let mut next = root.as_ref().first_child();
    while let Some(widget) = next {
        next = widget.next_sibling();

        // Either clamp variant exposes its single child the same way.
        let clamp_child = widget
            .downcast_ref::<adw::ClampScrollable>()
            .and_then(|c| c.child())
            .or_else(|| widget.downcast_ref::<adw::Clamp>().and_then(|c| c.child()));

        match clamp_child {
            Some(child) => child.set_halign(gtk::Align::Start),
            None => left_align_clamped_pages(&widget),
        }
    }
}

/// Remove every dynamically-added row from `group` before rebuilding it —
/// **never** a blind `while first_child() { remove }`. `AdwPreferencesGroup`'s
/// own header/title/description furniture ARE `first_child()` results too, not
/// just the rows a caller `.add()`ed; blindly removing them corrupts the
/// widget's internal structure (found 2026-08-15 chasing a feature-limits e2e
/// hang whose actual symptom was every automation command on the app going
/// unacknowledged — the corrupted group wedged the next GTK layout pass, not
/// the request handler itself). Only remove children that are actually rows
/// (`adw::ActionRow` or a plain `gtk::ListBoxRow`), and stop the moment a
/// non-row child is reached — `first_child()` order means anything after that
/// point is the group's own furniture, not more rows.
pub fn clear_preferences_group_rows(group: &adw::PreferencesGroup) {
    while let Some(child) = group.first_child() {
        if child.downcast_ref::<adw::ActionRow>().is_some()
            || child.downcast_ref::<gtk::ListBoxRow>().is_some()
        {
            group.remove(&child);
        } else {
            break;
        }
    }
}

/// Page-content scaffolding for a settings sub-page built outside
/// `adw::PreferencesPage` (which already centers/clamps its own content): a
/// centered, fixed-width column matching the rest of Settings. `spacing`
/// varies per page (Personalization uses 16, Devices/Folders 24 — a genuine
/// per-page choice both doc comments already called "matching the other
/// settings sub-pages" without ever centralizing the shared shape underneath
/// it — round 170 of the shared-Rust harvest sweep), so it stays a parameter rather than a hard-coded pick.
pub fn page_box(spacing: i32) -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .spacing(spacing)
        .halign(gtk::Align::Center)
        .width_request(600)
        .build()
}
