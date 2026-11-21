use adw::prelude::*;

/// Switch `stack`'s visible child to `name`, forcing a `visible-child-name`
/// notification even when `name` is already active.
///
/// `gtk::Stack::set_visible_child_name` early-returns with NO notify when the
/// requested child is already visible — correct for GTK's own purposes, but
/// it silently defeats every "re-read on becoming visible" refresh a sub-page
/// registers (`connect_visible_child_name_notify`, `settings_shell.rs`): a
/// user who leaves a page open, changes state elsewhere (another device
/// publishes, an author approves a request), and re-selects the SAME
/// already-open row sees stale data forever, because nothing they did ever
/// produced a real value change to notify on. Found via
/// `web-content-hosting.md`'s Published-posts section, whose own e2e re-visit
/// assertion is the exact "still on the same page" case — but the bug is
/// general to every on-visible-refresh page, not specific to that one.
pub fn set_visible_child_forced(stack: &gtk::Stack, name: &str) {
    if stack.visible_child_name().as_deref() == Some(name) {
        stack.notify("visible-child-name");
    } else {
        stack.set_visible_child_name(name);
    }
}

/// What a rail row is tagged with (`build_nav_rail`'s `page_key`).
pub enum RailRowId {
    /// The ui.yaml page key the row opens — tagged `<row_id>[<key>]`.
    PageKey(&'static str),
    /// A row that keeps an older id of its own (ui.yaml
    /// `navigation.sub_page_nav_rows.<shell>.keeps_older_id`).
    Own(&'static str),
}

/// Build a vertical sidebar-swap **navigation rail** — the shared shape used by
/// both the admin shell (`views::admin`) and the settings shell
/// (`views::settings_shell`); see `admin.md` / `settings.md` § Navigation model.
///
/// `nav_back` is the "leave this shell" row pinned to the top (the
/// `admin-nav-back` / `settings-nav-back` button). `entries` is
/// `(stack_child_name, label, symbolic_icon, indent)` in rail order, each
/// driving `stack` — the shell's content sub-stack; `indent` insets a row so it
/// reads as a child of the entry above it (the Settings rail nests its Mail
/// sub-pages under "Mail"). Returns the rail `gtk::Box` for
/// `app.rs` to swap into the split-view sidebar slot while the shell is showing,
/// replacing the main sidebar in place (no horizontal sub-tabs → no horizontal
/// width pressure).
///
/// The rows are built explicitly as icon+label `gtk::ListBoxRow`s (mirroring the
/// main sidebar's `build_sidebar_row`) rather than a `gtk::StackSidebar`, which
/// renders labels only with no icon API. A row click switches the sub-stack; the
/// rail selection follows the sub-stack's visible child (so the test agent's
/// name-based nav and the nav-back button highlight the right row). `select_row`
/// emits row-selected, NOT row-activated, so there is no feedback loop.
///
/// Each row is tagged `<row_id>[<page key>]` (`admin-nav-row[…]` /
/// `settings-nav-row[…]`, ui.yaml `navigation.sub_page_nav_rows`).
/// `page_key` maps a stack child name to what the row is tagged with
/// (several child names differ from their registry keys): a
/// [`RailRowId::PageKey`] takes the `<row_id>[…]` form, a [`RailRowId::Own`]
/// is a row that keeps an older id of its own (the Settings Account row's
/// `account-settings-link`, ui.yaml `keeps_older_id`).
pub fn build_nav_rail(
    nav_back: &impl IsA<gtk::Widget>,
    entries: &[(&'static str, &'static str, &'static str, bool)],
    stack: &gtk::Stack,
    row_id: &str,
    page_key: impl Fn(&str) -> Option<RailRowId>,
) -> gtk::Box {
    let rail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail.append(nav_back);

    let nav_list = gtk::ListBox::new();
    nav_list.set_vexpand(true);
    nav_list.add_css_class("navigation-sidebar");
    for &(child, label, icon, indent) in entries {
        let icon_w = gtk::Image::from_icon_name(icon);
        icon_w.set_margin_end(12);
        let label_w = gtk::Label::new(Some(label));
        label_w.set_halign(gtk::Align::Start);
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);
        // Indented entries (the Mail sub-pages under "Mail") get extra left
        // inset so they read as children of the entry above.
        hbox.set_margin_start(if indent { 32 } else { 12 });
        hbox.set_margin_end(12);
        hbox.append(&icon_w);
        // The label sits in a column with a caption, hidden until a gate paints
        // a reason on the row (`row_caption`; `ui/README.md` § Offer only what
        // is possible, rule 2: a disabled row carries its reason within eyeshot).
        let text_col = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_col.append(&label_w);
        let caption = gtk::Label::new(None);
        caption.add_css_class("caption");
        caption.add_css_class("dim-label");
        caption.set_wrap(true);
        caption.set_max_width_chars(24);
        caption.set_xalign(0.0);
        caption.set_halign(gtk::Align::Start);
        caption.set_visible(false);
        text_col.append(&caption);
        hbox.append(&text_col);
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&hbox));
        match page_key(child) {
            Some(RailRowId::PageKey(key)) => {
                crate::testid::set_test_id(&row, &format!("{row_id}[{key}]"));
            }
            Some(RailRowId::Own(id)) => crate::testid::set_test_id(&row, id),
            None => {}
        }
        nav_list.append(&row);
    }

    let names: Vec<&'static str> = entries.iter().map(|(n, _, _, _)| *n).collect();

    // Row click → switch the sub-stack.
    {
        let stack = stack.clone();
        let names = names.clone();
        nav_list.connect_row_activated(move |_, row| {
            if let Some(name) = names.get(row.index() as usize) {
                set_visible_child_forced(&stack, name);
            }
        });
    }
    // Keep the rail selection following the sub-stack's visible child.
    {
        let nav_list_for_sync = nav_list.clone();
        let names = names.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if let Some(cur) = s.visible_child_name()
                && let Some(idx) = names.iter().position(|n| *n == cur.as_str())
            {
                nav_list_for_sync.select_row(nav_list_for_sync.row_at_index(idx as i32).as_ref());
            }
        });
    }
    // Initial selection = the sub-stack's current child.
    if let Some(cur) = stack.visible_child_name()
        && let Some(idx) = names.iter().position(|n| *n == cur.as_str())
    {
        nav_list.select_row(nav_list.row_at_index(idx as i32).as_ref());
    }
    rail.append(&nav_list);
    rail
}

/// The `gtk::ListBox` of rows inside a rail [`build_nav_rail`] built.
pub fn rail_list(rail: &gtk::Box) -> gtk::ListBox {
    rail.last_child()
        .and_downcast::<gtk::ListBox>()
        .expect("build_nav_rail ends the rail with its row list")
}

/// The hidden caption under a rail row's label, where a gate paints the reason
/// the row is disabled. `None` for a row [`build_nav_rail`] did not build.
pub fn row_caption(row: &gtk::ListBoxRow) -> Option<gtk::Label> {
    row.child()?
        .last_child()?
        .last_child()?
        .downcast::<gtk::Label>()
        .ok()
}
