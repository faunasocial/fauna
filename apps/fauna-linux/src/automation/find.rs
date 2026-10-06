//! Find a widget by test-id and read its introspectable state.
//!
//! Test IDs ride the GTK widget name (`crate::testid::set_test_id` →
//! `set_widget_name`), so finding is a depth-first walk of the widget tree
//! matching `widget_name()`. All functions here are GTK-main-thread only.
use adw::prelude::*;
use std::cell::RefCell;

thread_local! {
    /// The running application, set once by the agent. Lets the search target
    /// the *active* window only — excluding stale windows/views left behind by
    /// in-process `reset`/rebuilds (the AT-SPI path avoids these by relaunching
    /// the process). Empty in unit tests → falls back to visible toplevels.
    static APP: RefCell<Option<gtk::Application>> = const { RefCell::new(None) };
}

/// Register the application so searches target its active window.
pub fn set_app(app: &gtk::Application) {
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
}

/// The application's main window — for window-level automation ops such as
/// simulating the titlebar close button (`/window/close`), which must exercise
/// the real `connect_close_request` handler (close-to-tray hide-vs-quit, Track 4).
pub fn active_window() -> Option<gtk::Window> {
    APP.with(|a| {
        let borrow = a.borrow();
        let app = borrow.as_ref()?;
        app.active_window()
            .or_else(|| app.windows().into_iter().rev().find(|w| w.is_visible()))
    })
}

/// The window(s) to search: the application's active window (the current main /
/// onboarding window) — which already contains any embedded `adw::AlertDialog`
/// confirm (`crate::confirm_dialog`'s ten sites;
/// `wizard.rs`'s own `adw::Dialog` proved this reach first) — **plus any
/// visible separate-window dialog**: a modal or transient-for toplevel such as
/// the rename / add-participant `adw::MessageDialog`, which the
/// active-window-only search would miss (the AT-SPI path saw every toplevel).
/// Hidden windows left behind by an in-process `reset`/rebuild are excluded by
/// the `is_visible` filter, and stale *main* windows can't sneak back in
/// because they're neither modal nor transient. Falls back to the most-recent
/// visible window, then all visible toplevels.
pub(crate) fn search_roots() -> Vec<gtk::Widget> {
    APP.with(|a| {
        if let Some(app) = a.borrow().as_ref() {
            let mut roots: Vec<gtk::Widget> = Vec::new();
            if let Some(win) = app.active_window() {
                roots.push(win.upcast());
            }
            // Dialogs still on `adw::MessageDialog` (rename / add-participant,
            // the feed train-target sheet, the folder share dialog) are
            // separate toplevels that set `transient_for` + modal but are NOT
            // registered in `app.windows()`, so scan the global toplevel list
            // for them. `AlertDialog` confirms need none of this — they embed
            // directly in `app.active_window()`'s own tree. The visible +
            // dialog-ish filter keeps stale main windows (neither modal nor
            // transient) and hidden leftovers out.
            let tl = gtk::Window::toplevels();
            for i in 0..tl.n_items() {
                let Some(w) = tl.item(i).and_then(|o| o.downcast::<gtk::Window>().ok()) else {
                    continue;
                };
                if !w.is_visible() {
                    continue;
                }
                let is_dialog = w.is_modal() || w.transient_for().is_some();
                let w: gtk::Widget = w.upcast();
                if is_dialog && !roots.contains(&w) {
                    roots.push(w);
                }
            }
            if !roots.is_empty() {
                return roots;
            }
            if let Some(win) = app.windows().into_iter().rev().find(|w| w.is_visible()) {
                return vec![win.upcast()];
            }
        }
        let tl = gtk::Window::toplevels();
        (0..tl.n_items())
            .filter_map(|i| tl.item(i).and_then(|o| o.downcast::<gtk::Widget>().ok()))
            .filter(|w| w.is_visible())
            .collect()
    })
}

/// Whether the tree walk should descend into / count `w`: it must be visible in
/// its own right, child-visible in its parent's layout, AND — under a
/// `gtk::Stack` — the page that stack is currently on.
///
/// This prunes exactly the subtrees the AT-SPI tree omitted — the bridge only
/// ever saw *showing* widgets. `gtk::ListBox` sets filtered-out rows'
/// child-visible to `false`, so pruning on `is_child_visible` drops the
/// search-filtered post rows that otherwise inflated `count`. Crucially it does
/// *not* use `is_mapped`: a freshly-rendered-but-not-yet-allocated row is
/// `!is_mapped` yet `is_child_visible`, so this avoids the flake the earlier
/// blanket `is_mapped` count filter hit. Explicitly hidden widgets
/// (`set_visible(false)`) are pruned too (own `is_visible` is false).
///
/// ⚠ This comment used to say `gtk::Stack` "sets every non-current page's
/// child-visible to `false`" and stop there. That is true only once a
/// transition has *settled*; the explicit `visible_child` test below is what
/// covers the window while one is running, and its absence is the whole of
/// cluster E — see there for the measurement.
pub(crate) fn is_showing(w: &gtk::Widget) -> bool {
    if !(w.is_visible() && w.is_child_visible()) {
        return false;
    }
    // ...and, for a `gtk::Stack` page, it must be the page the stack is
    // *currently on*. `is_child_visible` alone is not that test while a
    // transition runs: GTK keeps the OUTGOING page child-visible and mapped for
    // the whole animation, and both navigation stacks in the shipped window
    // animate (`app.rs`'s content stack and `views/settings_shell.rs`'s
    // sub-page stack, both `Crossfade`, both 200 ms). Every automation read
    // lands inside that window — a nav is two or three localhost round trips,
    // not 200 ms — so the walk saw the page being LEFT as well as the one being
    // entered, and document order puts the one being left first (`feed` is
    // added to the content stack before `settings`). That is how
    // `get_text("page-heading")` returned `'Feeds'` on Account / Privacy /
    // Encryption with `count=3` in four consecutive whole-suite
    // sweeps.
    //
    // Pruning on the stack's own `visible_child` instead makes the answer the
    // logical current page regardless of animation state, which is what every
    // other app's automation surface already reports (AT-SPI and the other six
    // expose only the visible view) — so this closes a per-app divergence
    // rather than adding one. It subsumes the settled case too: a background
    // page is both non-child-visible and non-current.
    if let Some(stack) = w.parent().and_then(|p| p.downcast::<gtk::Stack>().ok())
        && stack.visible_child().as_ref() != Some(w)
    {
        return false;
    }
    true
}

/// Depth-first search from `root` (inclusive) for the first **showing** widget
/// whose GTK widget name equals `test_id`. Non-showing subtrees (background
/// stack pages, filtered rows, hidden widgets) are skipped — see [`is_showing`].
///
/// Note `widget_name()` returns the *type* name (e.g. `GtkBox`) when no name was
/// set, so this only matches widgets that explicitly carry a test id — our IDs
/// are kebab-case and won't collide with type names.
// No production caller today — every live call site (`views/contacts/list.rs`,
// `views/contacts/find.rs`, `recipient_picker.rs`, `walk.rs`) is `#[cfg(test)]`
// gated, so a non-test `cargo check` sees it as unreachable. Kept for its
// `_scoped`-independent generality (finding under an arbitrary widget root,
// not just the live toplevels `find_scoped` searches) — used as the direct
// verification tool in tests that build a widget tree by hand.
#[allow(dead_code)]
pub fn find_in(root: &gtk::Widget, test_id: &str) -> Option<gtk::Widget> {
    if root.widget_name() == test_id {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if is_showing(&c)
            && let Some(found) = find_in(&c, test_id)
        {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Resolve a single widget for `test_id`, **preferring a mapped (on-screen)
/// match** and falling back to the first in document order.
///
/// The tree walk already prunes non-showing subtrees (see [`is_showing`]), so
/// `find`/`count`/indexed lookups only ever see *showing* widgets — matching the
/// AT-SPI tree. A single id can still legitimately match more than one showing
/// widget (e.g. `recipient-picker-input` in two simultaneously-presented
/// surfaces), so single-element resolution additionally prefers a *mapped*
/// match (realized + allocated) over a merely-child-visible one.
pub fn find(test_id: &str) -> Option<gtk::Widget> {
    prefer_mapped(find_all(test_id))
}

/// Pick the first mapped widget, else the first overall — the single-element
/// resolution rule shared by [`find`] and [`find_scoped`].
fn prefer_mapped(matches: Vec<gtk::Widget>) -> Option<gtk::Widget> {
    matches
        .iter()
        .find(|w| w.is_mapped())
        .cloned()
        .or_else(|| matches.into_iter().next())
}

/// Count widgets under `root` whose name equals `test_id` — the `/element/count`
/// surface for indexed test elements (e.g. list items).
// Same test-only status as `find_in` above (no production caller).
#[allow(dead_code)]
pub fn count_in(root: &gtk::Widget, test_id: &str) -> usize {
    let mut n = usize::from(root.widget_name() == test_id);
    let mut child = root.first_child();
    while let Some(c) = child {
        if is_showing(&c) {
            n += count_in(&c, test_id);
        }
        child = c.next_sibling();
    }
    n
}

/// Collect every widget under `root` whose name equals `test_id`, in document
/// (depth-first, pre-order) order — the ordering the e2e `index=`/`count`
/// contract relies on.
pub fn collect_in(root: &gtk::Widget, test_id: &str, out: &mut Vec<gtk::Widget>) {
    if root.widget_name() == test_id {
        out.push(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if is_showing(&c) {
            collect_in(&c, test_id, out);
        }
        child = c.next_sibling();
    }
}

/// All widgets matching `test_id` across the **visible** toplevel windows, in
/// document order. The server's entry point for `count`/indexed lookups.
///
/// Restricting to visible windows matters: the in-process `reset` rebuilds the
/// app window without destroying the old one, so `gtk::Window::toplevels()`
/// accumulates stale (hidden) windows whose widgets would otherwise pollute
/// `count`/indexing and resolve a scope to an off-screen ghost. The AT-SPI path
/// sidesteps this by relaunching the whole process; we filter instead.
pub fn find_all(test_id: &str) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    for win in search_roots() {
        collect_in(&win, test_id, &mut out);
    }
    out
}

/// The `index`-th toplevel widget matching `test_id` (document order).
// Same test-only status as `find_in` above (no production caller — production
// indexed lookups go through the scoped `find_indexed_scoped`).
#[allow(dead_code)]
pub fn find_indexed(test_id: &str, index: usize) -> Option<gtk::Widget> {
    find_all(test_id).into_iter().nth(index)
}

/// A scope step: the `index`-th widget matching `id`, used to narrow a lookup to
/// a subtree (the e2e `scope="post-card[2]/msg[1]"` DSL, parsed driver-side).
/// The canonical type lives in the shared agent crate.
pub use fauna_e2e_agent::ScopeStep;

/// The subtree a scope step narrows to once it has matched `widget` as `id`: the
/// nearest ancestor that declared itself `id`'s scope
/// ([`crate::testid::set_test_scope`]), else `widget` itself.
///
/// ⚠ Without it, a control whose id sits on a leaf — `thread-member-chip`'s
/// label, the widget a driver presses and a role greys — scopes to a widget with
/// no children, and every element ui.yaml renders inside that chip reads absent
/// however plainly it is painted.
pub fn scope_container(widget: gtk::Widget, id: &str) -> gtk::Widget {
    let class = format!("test-scope-{id}");
    let mut ancestor = widget.parent();
    while let Some(a) = ancestor {
        if a.has_css_class(&class) {
            return a;
        }
        ancestor = a.parent();
    }
    widget
}

/// Resolve a scope path to its root widget by descending each step. `None` if
/// any step's match is absent (caller treats absent scope as "no results").
pub fn scope_root(scope: &[ScopeStep]) -> Option<gtk::Widget> {
    let mut current: Option<gtk::Widget> = None;
    for (id, index) in scope {
        let matches = match &current {
            None => find_all(id),
            Some(root) => {
                let mut v = Vec::new();
                collect_in(root, id, &mut v);
                v
            }
        };
        current = Some(scope_container(matches.into_iter().nth(*index)?, id));
    }
    current
}

/// Find `id` within an optional scope (empty scope = global, document order).
/// Mapped-filtered like the rest of the server path.
pub fn find_scoped(scope: &[ScopeStep], id: &str) -> Option<gtk::Widget> {
    if scope.is_empty() {
        return find(id);
    }
    let root = scope_root(scope)?;
    let mut out = Vec::new();
    collect_in(&root, id, &mut out);
    prefer_mapped(out)
}

/// All `id` matches within an optional scope (empty scope = global).
pub fn find_all_scoped(scope: &[ScopeStep], id: &str) -> Vec<gtk::Widget> {
    if scope.is_empty() {
        return find_all(id);
    }
    match scope_root(scope) {
        Some(root) => {
            let mut v = Vec::new();
            collect_in(&root, id, &mut v);
            v
        }
        None => Vec::new(),
    }
}

/// The `index`-th `id` match within an optional scope.
pub fn find_indexed_scoped(scope: &[ScopeStep], id: &str, index: usize) -> Option<gtk::Widget> {
    // Index 0 is the "single element" read (get_text / click / type with no
    // explicit index). Prefer the *mapped* (on-screen) match so it agrees with
    // `is_visible`/`find_scoped`: a build-once shell (e.g. the Settings
    // sidebar-swap, whose sub-pages all coexist in the stack) holds several
    // widgets of the same id across not-currently-shown sub-pages, only one of
    // which is mapped. Raw tree order would otherwise read/actuate a hidden
    // duplicate while `is_visible` reports the mapped one — the get_text↔is_visible
    // split that left `error-message` reads empty. Indexed reads (index > 0) keep
    // strict tree order for `post-card[2]`-style scoped access.
    if index == 0 {
        return find_scoped(scope, id);
    }
    find_all_scoped(scope, id).into_iter().nth(index)
}

/// Whether a widget is actually on screen — the `/element/visible` contract.
/// `is_mapped()` matches AT-SPI's SHOWING state (realized + visible + every
/// ancestor visible), which is what the e2e `is_visible`/`wait_for` expect.
pub fn is_visible(widget: &gtk::Widget) -> bool {
    widget.is_mapped()
}

/// The user-visible text of a widget, matching the e2e `get_text` contract for
/// the common widget kinds. Empty string when the kind carries no text.
pub fn text_of(widget: &gtk::Widget) -> String {
    // An explicit declaration always wins over the widget-kind inference below
    // — the linux peer of apple's `.automationValue(id, text:)` and of the text
    // tui carries on its `Element`. Needed wherever inference cannot tell a
    // caption/value row from a content row; see `testid::set_test_text`.
    if let Some(text) = crate::testid::test_text(widget) {
        return text;
    }
    if let Some(l) = widget.downcast_ref::<gtk::Label>() {
        return l.text().to_string();
    }
    if let Some(e) = widget.downcast_ref::<gtk::Entry>() {
        return e.text().to_string();
    }
    if let Some(tv) = widget.downcast_ref::<gtk::TextView>() {
        // Multi-line compose field: read the whole TextBuffer, INCLUDING
        // hidden chars. The compose field's inline markdown decoration
        // (compose_decoration.rs) tags a completed marker span `md-hidden`
        // (invisible) once the caret moves past it, so it displays cleanly —
        // but the underlying buffer still holds the full markdown SOURCE
        // (`compose_bar.rs`'s own body-change handler reads with
        // `include_hidden_chars: true` for exactly this reason: "the draft
        // forwarded to the manager … MUST be the full markdown SOURCE, not
        // the marker-stripped visible text"). Reading with `false` here
        // returned only the post-decoration visible text, so a test typing
        // "a *b* `c`" and reading it back saw the markers silently stripped
        // the moment the caret moved off that span. No other `gtk::TextView`
        // in this app uses invisible tags, so this is a no-op everywhere else.
        let buffer = tv.buffer();
        return buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
    }
    if let Some(b) = widget.downcast_ref::<gtk::Button>() {
        // Covers ToggleButton (a Button subclass) too.
        return b.label().map(|s| s.to_string()).unwrap_or_default();
    }
    if let Some(c) = widget.downcast_ref::<gtk::CheckButton>() {
        return c.label().map(|s| s.to_string()).unwrap_or_default();
    }
    // Selectors read back as their selected option's display string —
    // the cross-app `get_text` contract for a `<select>` (web reads
    // the chosen option's text). Mirrors `select`'s `StringObject` model.
    if let Some(d) = widget.downcast_ref::<gtk::DropDown>() {
        return d
            .selected_item()
            .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            .map(|s| s.string().to_string())
            .unwrap_or_default();
    }
    if let Some(c) = widget.downcast_ref::<adw::ComboRow>() {
        return c
            .selected_item()
            .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            .map(|s| s.string().to_string())
            .unwrap_or_default();
    }
    // Anything else implementing `gtk::Editable` reads back as its VALUE — the
    // read-side peer of `agent::editable_of`, which already resolves these for
    // typing. Covers `adw::EntryRow` / `adw::PasswordEntryRow` (a
    // `PreferencesRow`, NOT an `ActionRow`, and not a `gtk::Entry` either: it
    // delegates `Editable` to an inner `gtk::Text`), plus `gtk::SpinButton` and
    // `gtk::EditableLabel`. Without this an `EntryRow` fell through to the
    // descendant-label join below and read back as its own static TITLE — so a
    // test that typed a value and read it back got the caption instead, and a
    // round-trip assertion could only fail (found by the Slice-E screen-time
    // journey, whose three inputs are `EntryRow`s). Must come after the
    // DropDown/ComboRow branches, which are not `Editable`, and before the
    // label join.
    if let Some(e) = widget.downcast_ref::<gtk::Editable>() {
        return e.text().to_string();
    }
    // adw rows surface their *value* as the subtitle (the title is the static
    // caption). Read the subtitle directly so the automation agent sees a row's
    // value without a shadow marker label — e.g. the identity actor-id row,
    // whose value lives in the `adw::ActionRow` subtitle (the same string its
    // copy button reads). Must come *after* the ComboRow branch (ComboRow is an
    // ActionRow subclass). Title-only rows (empty subtitle) fall through to the
    // descendant-label join below, preserving their prior behaviour.
    if let Some(r) = widget.downcast_ref::<adw::ActionRow>() {
        let subtitle = r.subtitle().map(|s| s.to_string()).unwrap_or_default();
        if !subtitle.is_empty() {
            return subtitle;
        }
    }
    // A container with no text of its own (e.g. the `restore-history-item` /
    // `post-card` rows are a `gtk::Box`/`ListBoxRow`): the AT-SPI bridge read
    // such rows' accessible *name*, which GTK derived from — or these rows set
    // explicitly to — their child label text. GTK4 exposes no getter for the
    // accessible name, so reproduce it by joining the visible text of every
    // descendant label (document order). Substring-based e2e reads
    // (`text in card_text`) match on the body regardless of the surrounding
    // author/timestamp labels.
    let joined = collect_label_text(widget);
    joined.join(" ")
}

/// Visible text of every `gtk::Label` descendant (and the widget itself if it
/// is one), document order — the subtree-name fallback for [`text_of`].
fn collect_label_text(widget: &gtk::Widget) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(w: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            let t = l.text();
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, out);
            child = c.next_sibling();
        }
    }
    walk(widget, &mut out);
    out
}

/// Whether a widget is interactive — the `/element/enabled` contract.
pub fn is_enabled(widget: &gtk::Widget) -> bool {
    widget.is_sensitive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ui_ids as ids;

    /// **A scope step continues from the container its match declared.** The
    /// member chip's id sits on its label — the widget a driver presses and a
    /// role greys — while the review pair renders beside that label inside the
    /// chip's pill; a scope that stopped at the label found neither half of the
    /// pair however plainly it was painted, so a successor's review mark read
    /// absent on linux. The undeclared twin is the non-vacuity half: the same
    /// shape without the declaration still resolves to nothing.
    #[test]
    fn a_scope_step_continues_from_the_container_its_match_declared() {
        crate::testid::run_on_gtk_thread(|| {
            let pill = |chip_id: &str, inner_id: &str, declared: bool| {
                let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                let label = gtk::Label::new(Some("bob"));
                crate::testid::set_test_id(&label, chip_id);
                pill.append(&label);
                let inner = gtk::Label::new(Some("review"));
                crate::testid::set_test_id(&inner, inner_id);
                pill.append(&inner);
                if declared {
                    crate::testid::set_test_scope(&pill, chip_id);
                }
                pill
            };
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&pill("scope-probe-chip", "scope-probe-mark", true));
            root.append(&pill(
                "scope-probe-bare-chip",
                "scope-probe-bare-mark",
                false,
            ));
            // `find_scoped` searches the visible toplevels, exactly as the agent
            // does, so the fixture has to be on screen.
            let window = gtk::Window::new();
            window.set_child(Some(&root));
            window.set_visible(true);

            let declared = find_scoped(&[("scope-probe-chip".to_string(), 0)], "scope-probe-mark");
            let bare = find_scoped(
                &[("scope-probe-bare-chip".to_string(), 0)],
                "scope-probe-bare-mark",
            );
            window.destroy();

            assert!(
                declared.is_some(),
                "the element beside a declared chip's label must resolve inside that chip"
            );
            assert!(
                bare.is_none(),
                "without the declaration a leaf scopes to nothing — or this test pins nothing"
            );
        });
    }

    /// An `adw::EntryRow` reads back as its typed VALUE, not its static title.
    ///
    /// The write side (`agent::editable_of`) has resolved `EntryRow` to its
    /// `Editable` for a long time, but the read side had no matching branch, so
    /// an `EntryRow` fell through to the descendant-label join and `get_text`
    /// returned the row's caption — twice, once per internal label. Any e2e
    /// that types a value into one and reads it back could therefore only fail,
    /// and would look like a product bug rather than an agent gap (the Slice-E
    /// screen-time journey found it exactly that way). Asserting the title is
    /// NOT the answer is the half that pins the regression.
    #[test]
    fn an_entry_row_reads_back_its_value_not_its_title() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let row = adw::EntryRow::builder()
                .title("Usable from (HH:MM)")
                .build();
            crate::testid::set_test_id(&row, ids::WINDOW_START);
            root.append(&row);
            let root: gtk::Widget = root.upcast();

            let found = find_in(&root, "window-start").expect("found the entry row");
            assert_eq!(text_of(&found), "", "an untouched row reads back empty");

            row.set_text("21:00");
            assert_eq!(text_of(&found), "21:00");
            assert!(
                !text_of(&found).contains("Usable from"),
                "the row's static title must never stand in for its value"
            );
        });
    }

    #[test]
    fn finds_and_reads_named_widgets() {
        crate::testid::run_on_gtk_thread(|| {
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let label = gtk::Label::new(Some("Hello"));
            crate::testid::set_test_id(&label, "greeting");
            let button = gtk::Button::with_label("Click me");
            crate::testid::set_test_id(&button, "go");
            button.set_sensitive(false);
            root.append(&label);
            root.append(&button);
            let root: gtk::Widget = root.upcast();

            let found = find_in(&root, "greeting").expect("found the label");
            assert_eq!(text_of(&found), "Hello");
            assert!(is_enabled(&found));

            let btn = find_in(&root, "go").expect("found the button");
            assert_eq!(text_of(&btn), "Click me");
            assert!(
                !is_enabled(&btn),
                "a desensitised button reports not enabled"
            );

            assert!(find_in(&root, "no-such-id").is_none());
            assert_eq!(count_in(&root, "greeting"), 1);
            assert_eq!(count_in(&root, "no-such-id"), 0);
        });
    }

    #[test]
    fn action_row_reads_its_subtitle_value() {
        crate::testid::run_on_gtk_thread(|| {
            // The identity actor-id row shape: an `adw::ActionRow` whose *value*
            // lives in the subtitle (the title is the static caption). The agent
            // must read the subtitle directly — the former shadow-marker label
            // (which double-rendered the value) is gone. `subtitle()` is a property
            // read, so this holds without realizing/mapping the row.
            let row = adw::ActionRow::builder()
                .title("Actor ID")
                .subtitle("abc123def456")
                .build();
            crate::testid::set_test_id(&row, ids::ACCOUNT_ACTOR_ID);
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&row);
            let root: gtk::Widget = root.upcast();

            let found = find_in(&root, "account-actor-id").expect("found the row");
            assert_eq!(
                text_of(&found),
                "abc123def456",
                "an ActionRow's text is its subtitle (the value), not the title+subtitle join"
            );
        });
    }

    /// A content row — title is the identity, subtitle a secondary detail —
    /// reads back as its declared text, not as the subtitle.
    ///
    /// This is the read-only member `folder-row` shape verbatim
    /// (`views/devices_folders/folders.rs::build_member_folder_row`). Without
    /// the `set_test_text` declaration, `text_of` takes the ActionRow-subtitle
    /// branch above and returns the *mode* — so `get_text("folder-row")` was
    /// the literal string `"sync"` for every such row, and a cross-nest foreign
    /// row's name was unreadable by any test. It presented as the nest having
    /// delivered an empty `set_name`, and the whole server-side resolution chain
    /// was searched before the read itself was suspected (2026-08-11).
    ///
    /// RED-verify by commenting out the `set_test_text` call in
    /// `build_member_folder_row`'s peer below: this asserts both halves, so the
    /// wrong answer is named, not merely absent.
    #[test]
    fn a_content_row_reads_its_declared_text_not_its_subtitle() {
        crate::testid::run_on_gtk_thread(|| {
            let row = adw::ActionRow::builder()
                .title("xnest-c48805") // the set name — the row's identity
                .subtitle("sync") // the mode — a secondary detail
                .build();
            crate::testid::set_test_id(&row, ids::FOLDER_ROW);
            crate::testid::set_test_text(&row, "xnest-c48805");
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&row);
            let root: gtk::Widget = root.upcast();

            let found = find_in(&root, "folder-row").expect("found the row");
            assert_eq!(
                text_of(&found),
                "xnest-c48805",
                "a declared automation text must win over the ActionRow subtitle \
                 inference — a `folder-row` reads back as the set NAME on every app"
            );
            assert_ne!(
                text_of(&found),
                "sync",
                "reading the subtitle here is the 2026-08-11 bug: the mode string \
                 stands in for the set name and looks like empty nest data"
            );
        });
    }

    /// The declaration is opt-in: a row that does not declare one keeps the
    /// widget-kind inference (the caption/value shape above still reads its
    /// subtitle). Pins that the override changed nothing for existing rows.
    #[test]
    fn an_undeclared_row_keeps_the_widget_kind_inference() {
        crate::testid::run_on_gtk_thread(|| {
            let row = adw::ActionRow::builder()
                .title("Actor ID")
                .subtitle("abc123def456")
                .build();
            crate::testid::set_test_id(&row, ids::ACCOUNT_ACTOR_ID);
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&row);
            let root: gtk::Widget = root.upcast();

            let found = find_in(&root, "account-actor-id").expect("found the row");
            assert_eq!(text_of(&found), "abc123def456");
            assert_eq!(
                crate::testid::test_text(&found),
                None,
                "no declaration was made, so none must be readable"
            );
        });
    }

    /// The agent must see inside a popped-up `gtk::Popover` — the quick-create
    /// compose the events grids open (`event_popover::show_quick_create`) lives
    /// there, and nothing in any app has ever asserted a popover's contents.
    ///
    /// Shape mirrors production exactly: a `gtk::Popover` `set_parent`ed to a
    /// mapped `gtk::Button` inside a window, holding a test-id'd `gtk::Entry`.
    #[test]
    fn descends_into_a_popped_up_popover() {
        crate::testid::run_on_gtk_thread(|| {
            let window = gtk::Window::new();
            let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let anchor = gtk::Button::with_label("New Event");
            crate::testid::set_test_id(&anchor, ids::NEW_EVENT_BTN);
            page.append(&anchor);
            window.set_child(Some(&page));

            let popover = gtk::Popover::new();
            popover.set_parent(&anchor);
            popover.set_autohide(true);
            let entry = gtk::Entry::builder().text("2026-07-31").build();
            crate::testid::set_test_id(&entry, ids::EVENT_DTSTART);
            popover.set_child(Some(&entry));

            // Pump to a *deadline* on observable state, never a fixed amount of
            // work (`e2e-conventions.md` point 14): a green run stops the
            // instant the condition holds and pays nothing for the ceiling,
            // while a loaded machine gets as long as it needs instead of
            // failing because 500 iterations happened not to be enough.
            // `iteration(false)` keeps reporting work (the frame clock), so the
            // deadline — not exhaustion — is what ends the loop.
            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            fn pump_until(cond: impl Fn() -> bool) -> bool {
                let ctx = gtk::glib::MainContext::default();
                let deadline = std::time::Instant::now() + MAP_BUDGET;
                loop {
                    if cond() {
                        return true;
                    }
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    if !ctx.iteration(false) {
                        // Nothing pending: yield rather than spin the CPU. This
                        // paces the poll; it is not a wall-clock assertion.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }

            window.present();
            assert!(
                pump_until(|| window.is_mapped()),
                "the test window never mapped within {MAP_BUDGET:?}"
            );
            popover.popup();
            let popped_up = pump_until(|| popover.is_mapped() && entry.is_mapped());

            let root: gtk::Widget = window.clone().upcast();
            let found = find_in(&root, "event-dtstart");
            assert!(
                found.is_some(),
                "the walk must descend into a popped-up popover: \
             popped_up={}, window.is_mapped()={}, anchor.is_mapped()={}, \
             popover.parent()={:?}, popover.is_visible()={}, \
             popover.is_child_visible()={}, popover.is_mapped()={}, anchor children={:?}",
                popped_up,
                window.is_mapped(),
                anchor.is_mapped(),
                popover.parent().map(|p| p.widget_name().to_string()),
                popover.is_visible(),
                popover.is_child_visible(),
                popover.is_mapped(),
                {
                    let mut names = Vec::new();
                    let mut c = anchor.first_child();
                    while let Some(w) = c {
                        names.push(format!("{}({})", w.widget_name(), w.type_().name()));
                        c = w.next_sibling();
                    }
                    names
                }
            );
            // Reads the entry's *value*. Popping the popover leaves this entry
            // focused with its whole value selected, so on a shared display any
            // key event delivered anywhere on that desktop would replace it —
            // which is precisely what made this assertion flake until the GTK
            // test thread got a display of its own (`crate::testid`
            // § `start_private_display`).
            assert_eq!(text_of(&found.unwrap()), "2026-07-31");

            // A popover is `set_parent`ed, so it must be unparented explicitly;
            // otherwise the anchor is finalized still holding it and GTK warns.
            popover.unparent();
            window.destroy();
        });
    }

    /// A stack page the app has just navigated AWAY from must answer no id
    /// lookup — even while its crossfade is still running.
    ///
    /// **The bug this pins (cluster E of the 2026-09-11 linux sweep).** Both
    /// navigation stacks in the shipped window animate: the main content stack
    /// (`app.rs`, `Crossfade`) and the settings shell's sub-page stack
    /// (`views/settings_shell.rs`, `Crossfade`). GTK keeps the OUTGOING page
    /// `child_visible` — and mapped — for the whole transition, so for the
    /// ~200 ms after a navigation the tree holds two (or, with both stacks
    /// moving at once, three) widgets carrying the same id. [`is_showing`]
    /// pruned on `is_child_visible` alone, so the walk saw them all, and
    /// document order puts the page being LEFT first: `feed` is added to the
    /// content stack before `settings`.
    ///
    /// The e2e reads land inside that window every time — a nav is two or three
    /// localhost round trips, not 200 ms — so `get_text("page-heading")`
    /// returned `'Feeds'` on Account, Privacy, Encryption and Logs in all four
    /// sweeps (2026-08-28, 09-02, 09-10, 09-11).
    /// It is not the missing-id bug those tests were written for: every
    /// sub-page does register the id.
    ///
    /// ⚠ The blast radius is why this is pinned at the walker rather than in
    /// the four tests: ANY unscoped single-element read taken just after a
    /// navigation could resolve against the page being left.
    /// `test_general_sub_page_heading_is_visible` was passing *vacuously* on
    /// exactly this — it asserts only that the heading is non-empty, and
    /// `'Feeds'` is non-empty.
    #[test]
    fn a_stack_page_being_navigated_away_from_answers_no_lookup() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();

            // The settings shell: an inner stack whose sub-pages each carry
            // `page-heading` (`settings/{general,account}.rs` →
            // `wrap_page_with_heading`).
            let shell = gtk::Stack::new();
            shell.set_transition_type(gtk::StackTransitionType::Crossfade);
            for (name, title) in [("general", "General"), ("account", "Account")] {
                let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let heading = gtk::Label::new(Some(title));
                crate::testid::set_test_id(&heading, ids::PAGE_HEADING);
                page.append(&heading);
                shell.add_named(&page, Some(name));
            }
            shell.set_visible_child_name("general");

            // The main content stack, with `feed` added BEFORE `settings` —
            // the document order that decides which match wins.
            let content = gtk::Stack::new();
            content.set_transition_type(gtk::StackTransitionType::Crossfade);
            let feed = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let feed_heading = gtk::Label::new(Some("Feeds"));
            crate::testid::set_test_id(&feed_heading, ids::PAGE_HEADING);
            feed.append(&feed_heading);
            content.add_named(&feed, Some("feed"));
            content.add_named(&shell, Some("settings"));
            content.set_visible_child_name("feed");

            let window = gtk::Window::new();
            window.set_child(Some(&content));

            const MAP_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
            fn pump_until(cond: impl Fn() -> bool) -> bool {
                let ctx = gtk::glib::MainContext::default();
                let deadline = std::time::Instant::now() + MAP_BUDGET;
                loop {
                    if cond() {
                        return true;
                    }
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    if !ctx.iteration(false) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            window.present();
            assert!(
                pump_until(|| feed_heading.is_mapped()),
                "the test window never mapped within {MAP_BUDGET:?}"
            );

            // Navigate exactly as `navigate_shell_subpage` does: the content
            // stack to the settings shell, then the shell's own stack to the
            // sub-page. Nothing is pumped afterwards — the read below stands
            // where every e2e read stands, with both transitions still running.
            content.set_visible_child_name("settings");
            crate::views::nav_rail::set_visible_child_forced(&shell, "account");

            // The precondition, asserted rather than assumed: if GTK ever
            // settles a transition synchronously this test proves nothing, and
            // a silent vacuous pass is the failure mode this whole row is
            // about. Both outgoing pages must still be child-visible here.
            assert!(
                feed.is_child_visible()
                    && shell.child_by_name("general").unwrap().is_child_visible(),
                "precondition: both outgoing pages must still be child-visible \
                 (mid-crossfade) for this test to exercise anything — GTK now \
                 settles transitions synchronously, so re-derive the repro"
            );

            let root: gtk::Widget = window.clone().upcast();
            let mut out = Vec::new();
            collect_in(&root, ids::PAGE_HEADING, &mut out);
            assert_eq!(
                out.iter().map(text_of).collect::<Vec<_>>(),
                vec!["Account".to_owned()],
                "only the page being navigated TO may answer `page-heading`; a \
                 crossfading outgoing page must be pruned like any other \
                 non-current stack page"
            );
            assert_eq!(
                text_of(&prefer_mapped(out).expect("one match")),
                "Account",
                "the single-element read must land on the current sub-page"
            );

            window.destroy();
        });
    }
}
