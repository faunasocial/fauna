//! Systematic UI-path exploration — tier_1, offline, in-process (linux leg).
//!
//! The convention-17 twin of `apps/fauna-tui/src/walk.rs`
//! (`docs/goal/architecture/e2e-conventions.md` point 17). Every hand-authored
//! test drives ONE path and asserts ONE hand-picked outcome, so a defect on a
//! surface nobody wrote a test for violates no assertion any test makes. This
//! module walks the surfaces instead, asserting GENERAL invariants on each.
//!
//! **What makes the walk faithful.** It builds the REAL `build_main_window`
//! over an offline client (a closed port — the tui walk's own idiom), presents
//! it on the private Xvfb display `run_on_gtk_thread` owns, and moves between
//! surfaces through the REAL doors: `select_row` on the main sidebar (what a
//! click emits) and `row-activated` on a shell's nav rail. It reads the surface
//! through `automation::find::is_showing` — the exact predicate the in-process
//! agent's `find`/`count` walk through — so a surface this walk calls reachable
//! is one the driver really resolves, and the two cannot drift.
//!
//! **Why the rail sweeps matter specifically here.** Both shell rails build
//! their rows as bare icon+label `ListBoxRow`s with no test id at all
//! (`views/nav_rail.rs`), exactly as tui's Settings rail does — so no per-id
//! e2e query can reach them, and an in-process sweep is the only thing that
//! can walk every sub-page a human can click to.

use crate::testid::run_on_gtk_thread;
use adw::prelude::*;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// The offline fixture
// ---------------------------------------------------------------------------

/// The real main window over a client whose nest is a closed port. Nothing
/// here dials and nothing waits.
fn offline_window() -> (adw::Application, crate::app::MainWindowResult) {
    let (app, result, rx) = build_offline_window();
    // The receiver must outlive the window: every `UiSender::send` onto a
    // dropped receiver is an error the producers would then log.
    std::mem::forget(rx);
    (app, result)
}

/// Same fixture as [`offline_window`], but keeps the UI channel's receiver
/// instead of forgetting it — for a test that needs to observe which
/// background fetches a real navigation edge actually triggered
/// (`returning_to_admin_refreshes_membership_tier_names`).
fn offline_window_with_channel() -> (
    adw::Application,
    crate::app::MainWindowResult,
    mpsc::Receiver<crate::app::UiMessage>,
) {
    build_offline_window()
}

fn build_offline_window() -> (
    adw::Application,
    crate::app::MainWindowResult,
    mpsc::Receiver<crate::app::UiMessage>,
) {
    let app = adw::Application::builder()
        .application_id("fan.fauna.linux.walk")
        .build();
    let (tx, rx) = crate::client::ui_channel();
    let machine = fauna_launch_machine::LaunchMachine::new(
        std::sync::Arc::new(fauna_launch_machine::NullObserver),
        std::sync::Arc::new(fauna_launch_machine::InMemoryPersistence::new()),
    );
    let client = Rc::new(crate::client::FaunaClient::new(
        "http://127.0.0.1:1".to_string(),
        "11".repeat(32),
        tx,
        machine,
    ));
    let result = crate::app::build_main_window(&app, &client);
    // GTK's `is_visible()` is ancestor-aware up to the toplevel, so an unshown
    // window reports every descendant hidden and every sweep below would pass
    // vacuously. Presenting it on the private display is what makes the walk
    // see what a user sees — and `assert_the_fixture_is_not_vacuous` pins it.
    result.window.set_visible(true);
    (app, result, rx)
}

// ---------------------------------------------------------------------------
// Reading the surface the way the driver reads it
// ---------------------------------------------------------------------------

/// A GObject type name — `GtkBox`, `AdwActionRow`. GTK answers `widget_name()`
/// with the type name when nothing set one; test ids are kebab-case by ui.yaml
/// convention, so the two vocabularies separate without a hard-coded list.
fn looks_like_a_gtype(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !name.contains('-')
}

/// Every test id under `root` that the DRIVER can currently see, in the
/// driver's own document order.
fn showing_ids(root: &impl IsA<gtk::Widget>) -> Vec<String> {
    fn walk(w: &gtk::Widget, out: &mut Vec<String>) {
        let name = w.widget_name();
        if !name.is_empty() && !looks_like_a_gtype(&name) {
            out.push(name.to_string());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if crate::automation::find::is_showing(&child) {
                walk(&child, out);
            }
            c = child.next_sibling();
        }
    }
    let mut out = Vec::new();
    walk(root.upcast_ref(), &mut out);
    out
}

// ---------------------------------------------------------------------------
// The invariants — few, and true by construction of the ID contract
// ---------------------------------------------------------------------------

/// The ids ui.yaml declares `indexed: true` — the ONLY ids a surface may show
/// more than once (list rows, stat cards, per-kind checkboxes).
///
/// Read from ui.yaml itself rather than restated here, because a hand-copied
/// list is a second authority that drifts the week it is written — ui.yaml is
/// the one owner of element IDs and their scope. The parse is
/// deliberately shallow — a registry entry is a 2-space-indented `id:` line and
/// its `indexed: true` sits under it — and it fails LOUD rather than quiet: if
/// the format ever changes, the set comes back short and [`I1`] starts flagging
/// legitimate indexed ids, which is a red, not a silent pass.
///
/// [`I1`]: check_no_duplicate_non_indexed_ids
fn indexed_ids() -> std::collections::BTreeSet<String> {
    const UI_YAML: &str = include_str!("../../../tests/e2e-unified/ui.yaml");
    let mut out = std::collections::BTreeSet::new();
    let mut current: Option<&str> = None;
    for line in UI_YAML.lines() {
        if let Some(rest) = line.strip_prefix("  ")
            && !rest.starts_with(' ')
            && let Some(id) = rest.strip_suffix(':')
            && !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            current = Some(id);
        } else if line.trim() == "indexed: true"
            && let Some(id) = current
        {
            out.insert(id.to_string());
        }
    }
    assert!(
        out.len() > 20,
        "the ui.yaml indexed-id scan found only {} ids — the registry format changed and this \
         parse no longer sees it; fix the parse rather than the invariant",
        out.len()
    );
    out
}

/// **I1 — a non-indexed id never resolves to more than one showing widget.**
///
/// `automation::find` returns the FIRST showing match (preferring a mapped one),
/// so a second widget wearing the same id makes every `get_text`/`click`/
/// `get_attr` on it a coin flip decided by tree order — the test reads a widget
/// its author never meant, and reports the wrong thing about the product rather
/// than failing. ui.yaml's `indexed: true` ids are the declared exception: they
/// are addressed positionally (`folder-row[2]`) precisely because they repeat.
///
/// True by construction of the ID contract, which is why it can be asserted on
/// every surface rather than page by page.
fn check_no_duplicate_non_indexed_ids(
    ids: &[String],
    indexed: &std::collections::BTreeSet<String>,
    ctx: &str,
) -> Result<(), String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *counts.entry(id.as_str()).or_default() += 1;
    }
    let offenders: Vec<String> = counts
        .iter()
        .filter(|(id, n)| **n > 1 && !indexed.contains(**id))
        .map(|(id, n)| format!("{id} ×{n}"))
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{ctx}: {} id(s) show more than once and are NOT `indexed: true` in ui.yaml, so \
         `find` resolves them by tree order: {}",
        offenders.len(),
        offenders.join(", ")
    ))
}

/// **I2 — every surface a user can reach shows a heading.**
///
/// ui.yaml's `global.elements` requires `page-heading` on every authenticated
/// page (ui.yaml § global — "MUST be present on every authenticated page in all
/// 7 apps"), and the smoke tests read it to know a page loaded. The admin
/// shell's sub-pages are the uniform cross-app exception: they carry their own
/// scoped `*-heading` id instead (`admin-dashboard-heading` — the same shape on
/// tui, android, apple and linux alike), so the invariant is "a heading shows",
/// not "this exact id shows". Two Settings sub-pages carry a THIRD shape —
/// ui.yaml's own page-landmark id in place of a `-heading`-suffixed one
/// (`muted-words`, `settings-logs`; windows and tui use the identical id for
/// the same purpose) — allowed here for the same reason. A surface showing
/// NONE of the three is one the user cannot name — the class this catches.
fn check_surface_has_a_heading(ids: &[String], ctx: &str) -> Result<(), String> {
    const PAGE_LANDMARK_HEADINGS: [&str; 2] = ["muted-words", "settings-logs"];
    if ids.iter().any(|id| {
        id == "page-heading"
            || id.ends_with("-heading")
            || PAGE_LANDMARK_HEADINGS.contains(&id.as_str())
    }) {
        return Ok(());
    }
    Err(format!(
        "{ctx}: shows no heading — neither the global `page-heading`, a page-scoped \
         `*-heading`, nor a ui.yaml page-landmark id — so nothing on screen says which page \
         this is ({} showing ids)",
        ids.len()
    ))
}

/// **I6 — no affordance whose wire kind is `OnlineOnly` is enabled while there
/// is no nest.** The twin of tui's I6 (`apps/fauna-tui/src/walk.rs`), asserting
/// the same charter sentence over the same shared rule
/// (`account-data-plane.md` § The offline-mutation contract, class 3).
///
/// The fixture's client dials a closed port and never connects, so the gate's
/// state is the app's own start state, `"disconnected"` — which is what makes
/// this assertable on every surface rather than only on one the test drove
/// offline by hand.
///
/// It reads the app's live declaration registry rather than the widget tree,
/// because a declaration whose widget has been dropped is exactly the case a
/// tree walk cannot distinguish from "correctly gated".
fn check_online_only_affordances_are_disabled(ctx: &str) -> Result<(), String> {
    let state = crate::offline_gate::connection_state();
    // The invariant is only a claim while the link is down. Every job on this
    // process's one GTK thread shares the gate's `thread_local!` state, so a
    // sibling test that left it `"connected"` would turn this into a silent
    // pass on every surface — the vacuity that must red, not slip through.
    if fauna_protocol::offline_class::affordance("fauna.admin.factory_reset", state).is_available()
    {
        return Err(format!(
            "{ctx}: the gate's state is {state:?}, which gates nothing — this invariant \
             would pass vacuously. The offline fixture must leave it offline."
        ));
    }
    for (id, kind, enabled) in crate::offline_gate::declarations() {
        let verdict = fauna_protocol::offline_class::affordance(kind, state);
        if !verdict.is_available() && enabled {
            return Err(format!(
                "{ctx}: `{id}` issues {kind}, which is OnlineOnly, and the link is \
                 {state} — but the control is still enabled"
            ));
        }
    }
    Ok(())
}

/// **I7 — a declared wire kind is one the classification table knows.**
///
/// Exists because [`fauna_protocol::offline_class::affordance`] reads an
/// unregistered kind as *available* by design (its ruling 2, so a typo can
/// never become a dead button in a user's hands) — which means a misspelled
/// declaration silently ungates its control and I6 cannot see it. tui states
/// the identical reason for its own I7.
fn check_declared_kinds_are_registered(ctx: &str) -> Result<(), String> {
    for (id, kind, _) in crate::offline_gate::declarations() {
        if fauna_protocol::offline_class::offline_class(kind).is_none() {
            return Err(format!(
                "{ctx}: `{id}` declares {kind}, which no registered kind matches — the \
                 gate reads an unknown kind as available, so this control is ungated"
            ));
        }
    }
    Ok(())
}

/// Every invariant over one surface.
fn check_all(
    ids: &[String],
    indexed: &std::collections::BTreeSet<String>,
    ctx: &str,
) -> Result<(), String> {
    check_no_duplicate_non_indexed_ids(ids, indexed, ctx)?;
    check_surface_has_a_heading(ids, ctx)?;
    check_online_only_affordances_are_disabled(ctx)?;
    check_declared_kinds_are_registered(ctx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The real doors
// ---------------------------------------------------------------------------

/// Every sidebar row the window builds, gated rows included. Not a hand-written
/// page table: the rows come from the app's own `SidebarItem` list, so a new
/// page joins these sweeps by existing.
fn all_sidebar_items() -> Vec<crate::views::sidebar::SidebarItem> {
    use crate::views::sidebar::SidebarItem as S;
    let mut items = S::ALL.to_vec();
    items.push(S::Admin);
    items.push(S::Family);
    items
}

/// Select main-sidebar row `index` the way a click does — `select_row` is what
/// a pointer press ends in, and it emits the `row-selected` the nav is wired to.
fn click_sidebar_row(result: &crate::app::MainWindowResult, index: usize) {
    let list = &result.widgets.sidebar_list_box;
    let row = list
        .row_at_index(index as i32)
        .unwrap_or_else(|| panic!("main sidebar has no row at index {index}"));
    list.select_row(Some(&row));
}

/// The nav rail belonging to a shell, found by its own nav-back button — the
/// rail rows carry no test id (`views/nav_rail.rs` builds bare icon+label
/// rows), so the button is the only anchor, exactly as tui's id-less rail rows
/// force a structural reach.
fn rail_list_for(window: &adw::ApplicationWindow, nav_back_id: &str) -> gtk::ListBox {
    let nav_back = crate::automation::find::find_in(window.upcast_ref(), nav_back_id)
        .unwrap_or_else(|| panic!("{nav_back_id} is not showing — is its shell open?"));
    let rail = nav_back
        .parent()
        .unwrap_or_else(|| panic!("{nav_back_id} has no parent rail"));
    let mut child = rail.first_child();
    while let Some(c) = child {
        if let Ok(list) = c.clone().downcast::<gtk::ListBox>() {
            return list;
        }
        child = c.next_sibling();
    }
    panic!("the rail holding {nav_back_id} has no ListBox of rows");
}

/// Activate rail row `index` the way a click does: `row-activated` is the
/// signal `nav_rail.rs` wires the sub-stack switch to, and it is what a pointer
/// press emits. `select_row` deliberately does NOT switch (the rail syncs its
/// selection FROM the sub-stack), so driving selection instead of activation
/// would walk a surface no user can reach.
fn click_rail_row(rail: &gtk::ListBox, index: usize) {
    let row = rail
        .row_at_index(index as i32)
        .unwrap_or_else(|| panic!("rail has no row at index {index}"));
    rail.emit_by_name::<()>("row-activated", &[&row]);
}

fn rail_row_count(rail: &gtk::ListBox) -> usize {
    let mut n = 0;
    while rail.row_at_index(n as i32).is_some() {
        n += 1;
    }
    n
}

/// The sub-stack a shell's rail drives, found from the shell's own content —
/// the `gtk::Stack` holding its sub-pages.
fn sub_stack_of(shell: &gtk::Widget) -> gtk::Stack {
    fn find(w: &gtk::Widget) -> Option<gtk::Stack> {
        if let Ok(s) = w.clone().downcast::<gtk::Stack>() {
            return Some(s);
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(found) = find(&child) {
                return Some(found);
            }
            c = child.next_sibling();
        }
        None
    }
    find(shell).expect("the shell has no sub-stack")
}

// ---------------------------------------------------------------------------
// Sweep 1 — every sidebar page
// ---------------------------------------------------------------------------

/// Exhaustive sweep 1 — **every page the main sidebar can reach**, entered
/// through the real row selection: the nav lands where the row says, and the
/// page it lands on upholds every invariant.
///
/// The gated Admin and Family rows are revealed first through their own
/// production doors (`show_admin_sidebar_row` / `show_family_sidebar_row`), so
/// the two surfaces no ungated walk can reach carry coverage too — tui's own
/// `admin_app()` lesson, where the first sweep pointed at a gated page found a
/// live defect on it.
#[test]
fn every_sidebar_page_upholds_the_invariants() {
    run_on_gtk_thread(|| {
        let indexed = indexed_ids();
        let (_app, result) = offline_window();
        crate::views::sidebar::show_admin_sidebar_row(&result.widgets.sidebar_list_box);
        crate::views::sidebar::show_family_sidebar_row(&result.widgets.sidebar_list_box);

        let mut failures = Vec::new();
        for (i, item) in all_sidebar_items().iter().enumerate() {
            click_sidebar_row(&result, i);
            let want = item.stack_name();
            assert_eq!(
                result.stack.visible_child_name().as_deref(),
                Some(want),
                "selecting the {want:?} sidebar row left the content stack on another page"
            );
            let page = result
                .stack
                .child_by_name(want)
                .unwrap_or_else(|| panic!("no stack child named {want:?}"));
            let ids = showing_ids(&page);
            assert!(
                !ids.is_empty(),
                "the {want:?} page shows no test ids at all — an empty surface, or one whose \
                 whole subtree is hidden"
            );
            if let Err(e) = check_all(&ids, &indexed, &format!("page {want:?}")) {
                failures.push(e);
            }
        }
        // Collected rather than fail-fast: this is a CLASS sweep, and stopping
        // at the first red turns closing the class into N rounds of whack-a-mole.
        assert!(
            failures.is_empty(),
            "{} of 13 sidebar pages break an invariant:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        );
    });
}

// ---------------------------------------------------------------------------
// Sweep 2 — every shell sub-page, through the id-less rails
// ---------------------------------------------------------------------------

/// Walk every rail row of the shell reached by `tab_index`, checking the
/// invariants on each sub-surface. Returns the sub-page names visited.
fn sweep_shell_rail(
    result: &crate::app::MainWindowResult,
    indexed: &std::collections::BTreeSet<String>,
    tab_index: usize,
    stack_name: &str,
    nav_back_id: &str,
    failures: &mut Vec<String>,
    headless: &mut Vec<String>,
) -> Vec<String> {
    click_sidebar_row(result, tab_index);
    let shell = result
        .stack
        .child_by_name(stack_name)
        .unwrap_or_else(|| panic!("no {stack_name:?} stack child"));
    let rail = rail_list_for(&result.window, nav_back_id);
    let sub_stack = sub_stack_of(&shell);
    let rows = rail_row_count(&rail);
    assert!(
        rows > 1,
        "the {stack_name} rail exposes {rows} row(s) — the sweep would cover nothing"
    );
    let mut visited = Vec::new();
    for row in 0..rows {
        click_rail_row(&rail, row);
        let sub = sub_stack
            .visible_child_name()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("<row {row}>"));
        let surface = sub_stack
            .visible_child()
            .unwrap_or_else(|| panic!("{stack_name} rail row {row} shows no sub-page"));
        let ids = showing_ids(&surface);
        let ctx = format!("{stack_name} sub-page {sub:?}");
        if let Err(e) = check_no_duplicate_non_indexed_ids(&ids, indexed, &ctx) {
            failures.push(e);
        }
        if let Err(e) = check_surface_has_a_heading(&ids, &ctx) {
            failures.push(e);
            headless.push(sub.clone());
        }
        // The offline invariants read a GLOBAL registry, so today they check the
        // same set on every surface. They are asserted here anyway because that
        // stops being true the moment a lazily-built page declares: the sweep
        // that walks to it is then the only one that can see its declarations.
        if let Err(e) = check_online_only_affordances_are_disabled(&ctx) {
            failures.push(e);
        }
        if let Err(e) = check_declared_kinds_are_registered(&ctx) {
            failures.push(e);
        }
        visited.push(sub);
    }
    visited
}

/// Exhaustive sweep 2 — **every Settings sub-page**, drilled into through the
/// rail's own `row-activated`.
///
/// The rail rows carry no test id, so this is coverage the e2e suite
/// structurally cannot have: its only route to a settings sub-page is the `nav`
/// patch, which names the sub-page directly and therefore can never discover
/// one whose rail row is mis-wired.
#[test]
fn every_settings_sub_page_upholds_the_invariants() {
    run_on_gtk_thread(|| {
        let indexed = indexed_ids();
        let (_app, result) = offline_window();
        let mut failures = Vec::new();
        let mut headless = Vec::new();
        let visited = sweep_shell_rail(
            &result,
            &indexed,
            settings_tab_index(),
            "settings",
            "settings-nav-back",
            &mut failures,
            &mut headless,
        );
        // No known-gap list here on purpose (mirrors sweep 3's admin twin,
        // below): every Settings sub-page shows a heading now that the eight
        // linux was missing paint one (`ui/README.md` § Navigation model, "a
        // shell sub-page must paint a real, visible heading").
        assert!(
            headless.is_empty(),
            "Settings sub-page(s) {headless:?} show no heading"
        );
        // Every rail row must land on a DISTINCT sub-page: two rows resolving
        // to one page means a row is mis-wired and a sub-page is unreachable by
        // click — the rail's index-to-name mapping is positional
        // (`nav_rail.rs`), so an entry list that drifts from the stack's child
        // order silently redirects rows.
        let mut seen = std::collections::BTreeSet::new();
        for name in &visited {
            assert!(
                seen.insert(name.clone()),
                "two Settings rail rows both land on {name:?} — the positional row→child \
                 mapping has drifted; visited: {visited:?}"
            );
        }
        assert!(
            failures.is_empty(),
            "{} of {} Settings sub-pages break an invariant:\n  - {}",
            failures.len(),
            visited.len(),
            failures.join("\n  - ")
        );
    });
}

/// Exhaustive sweep 3 — **every Admin sub-page**, the structural twin of sweep
/// 2 (`views/admin.rs` builds its rail through the same `build_nav_rail`), so
/// it inherits the same blind spot if only hand-authored tests cover it.
#[test]
fn every_admin_sub_page_upholds_the_invariants() {
    run_on_gtk_thread(|| {
        let indexed = indexed_ids();
        let (_app, result) = offline_window();
        crate::views::sidebar::show_admin_sidebar_row(&result.widgets.sidebar_list_box);
        let mut failures = Vec::new();
        let mut headless = Vec::new();
        let visited = sweep_shell_rail(
            &result,
            &indexed,
            admin_tab_index(),
            "admin",
            "admin-nav-back",
            &mut failures,
            &mut headless,
        );
        // No known-gap list here on purpose: every admin sub-page shows a
        // heading once `admin-web`'s invisible shim is a real title, so this
        // sweep binds the whole shell with no exceptions.
        assert!(
            headless.is_empty(),
            "admin sub-page(s) {headless:?} show no heading"
        );
        let mut seen = std::collections::BTreeSet::new();
        for name in &visited {
            assert!(
                seen.insert(name.clone()),
                "two Admin rail rows both land on {name:?} — the positional row→child mapping \
                 has drifted; visited: {visited:?}"
            );
        }
        assert!(
            failures.is_empty(),
            "{} of {} Admin sub-pages break an invariant:\n  - {}",
            failures.len(),
            visited.len(),
            failures.join("\n  - ")
        );
    });
}

/// Sweep 4's body — for every sub-page of `shell_stack_name`, drill in, leave
/// through the real sidebar, come back through the real sidebar, and assert the
/// shell landed on its canonical entry.
///
/// This is the class a live user hit on tui — returning to Settings landed on
/// the stale sub-page "stuck forever", one of convention 17's three motivating
/// bugs. Sweep 4 **measured linux as one of the two apps that did not reset**
/// (2026-08-13); the cross-app measurement that followed went 4–2 for resetting
/// and was ruled the same day: `ui/README.md` § Navigation model → *Entering a
/// shell lands on its canonical entry*. Sub-page position is **shell state, not
/// session state**, so crossing into a shell always lands on Settings → Status
/// / Admin → Dashboard, never on the sub-page a previous visit left open. linux
/// conforms as of that ruling (`app.rs`, the content-stack visible-child notify
/// that already swaps the rail now also seats the shell's sub-stack on its
/// root), and this sweep is what holds it there.
///
/// Two properties the sweep pins beyond the landing itself, both of which
/// survive any future re-ruling: re-entry is **deterministic** (twice in a row
/// lands identically — a coin-flip would be a defect under any rule), and the
/// surface it lands on is a **live** one, never a blank or dead sub-page.
///
/// **Red-verified 2026-08-13** (convention 17: a new invariant proves itself
/// against the defect it claims to catch, before it counts as coverage). With
/// `app.rs`'s reset temporarily disabled, both halves fail on the landing
/// assertion specifically — *"re-entering the admin shell after opening
/// `Some("users")` landed on `Some("users")`"*, expected `"dashboard"` — not on
/// the determinism or liveness checks, which stayed green. So the assertion
/// grades the ruled behavior and nothing weaker.
fn assert_shell_re_entry_lands_on_canonical_entry(
    shell_tab: usize,
    shell_stack_name: &str,
    nav_back_id: &str,
    canonical_entry: &str,
    result: &crate::app::MainWindowResult,
) {
    let indexed = indexed_ids();
    let feed = feed_tab_index();

    click_sidebar_row(result, shell_tab);
    let shell = result
        .stack
        .child_by_name(shell_stack_name)
        .unwrap_or_else(|| panic!("no {shell_stack_name} shell"));
    let sub_stack = sub_stack_of(&shell);
    let rail = rail_list_for(&result.window, nav_back_id);

    for row in 0..rail_row_count(&rail) {
        click_sidebar_row(result, shell_tab);
        click_rail_row(&rail, row);
        let drilled = sub_stack.visible_child_name().map(|s| s.to_string());

        let leave_and_return = || {
            click_sidebar_row(result, feed);
            click_sidebar_row(result, shell_tab);
            sub_stack.visible_child_name().map(|s| s.to_string())
        };
        let first = leave_and_return();
        let second = leave_and_return();
        assert_eq!(
            first, second,
            "leaving and re-entering the {shell_stack_name} shell after opening {drilled:?} \
             landed on {first:?} then {second:?} — re-entry is not deterministic"
        );
        assert_eq!(
            first.as_deref(),
            Some(canonical_entry),
            "leaving and re-entering the {shell_stack_name} shell after opening {drilled:?} \
             landed on {first:?} — `ui/README.md` § Navigation model rules that entering a shell \
             lands on its canonical entry ({canonical_entry:?}), never the sub-page a previous \
             visit left open (sub-page position is shell state, not session state)"
        );

        let surface = sub_stack
            .visible_child()
            .expect("re-entering the shell shows some sub-page");
        let ids = showing_ids(&surface);
        let ctx = format!("{shell_stack_name} re-entry after {drilled:?} (landed on {first:?})");
        check_no_duplicate_non_indexed_ids(&ids, &indexed, &ctx).expect("re-entry surface");
    }
}

/// Sweep 4 — the stale-sub-page class, Settings half.
#[test]
fn returning_to_settings_lands_on_the_rail_root() {
    run_on_gtk_thread(|| {
        let (_app, result) = offline_window();
        assert_shell_re_entry_lands_on_canonical_entry(
            settings_tab_index(),
            "settings",
            "settings-nav-back",
            "status",
            &result,
        );
    });
}

/// Sweep 4 — the stale-sub-page class, Admin half. The ruling binds every
/// shell, and linux drives both sub-stacks from the same notify, so pinning
/// only Settings would leave half the implementation ungraded.
#[test]
fn returning_to_admin_lands_on_the_dashboard() {
    run_on_gtk_thread(|| {
        let (_app, result) = offline_window();
        crate::views::sidebar::show_admin_sidebar_row(&result.widgets.sidebar_list_box);
        assert_shell_re_entry_lands_on_canonical_entry(
            admin_tab_index(),
            "admin",
            "admin-nav-back",
            "dashboard",
            &result,
        );
    });
}

/// The DATA half of the fix, sibling to the
/// NAVIGATION half pinned above: leaving and returning to the Admin shell
/// through the REAL sidebar door (`select_row` — what `click_sidebar_row`'s
/// own doc comment says a pointer press ends in) must also refetch the
/// shell's dynamic feeds (`FaunaClient::refresh_admin_shell`), the same
/// class of gap `app.rs:1338-1340` already documents for `events` — before
/// this fix, the only door into `refresh_admin_shell`'s constituent fetches
/// was the once-per-session `AdminStatusLoaded` handler (a real nest
/// handshake) and the test-agent's own WS-RPC nav-patch channel
/// (`main.rs`), NEITHER of which a real sidebar click drives.
///
/// Proven without a live nest (this fixture's nest is a closed port —
/// `offline_window`'s own doc comment): `fetch_own_membership_tier_names`
/// unconditionally reports `OwnMembershipTierNamesLoaded` over the UI
/// channel even when the RPC fails (its own doc comment: "A failed read
/// degrades to an empty list"), so observing that message land after a
/// real re-entry is a faithful causal proof that `refresh_admin_shell` ran
/// on this door — with the bug still present, this message never arrives
/// here at all, since this offline fixture never runs the real
/// `AdminStatusLoaded` handshake that is its only other caller.
#[test]
fn returning_to_admin_refreshes_membership_tier_names() {
    run_on_gtk_thread(|| {
        let (_app, result, rx) = offline_window_with_channel();
        crate::views::sidebar::show_admin_sidebar_row(&result.widgets.sidebar_list_box);

        let admin = admin_tab_index();
        let feed = feed_tab_index();

        // First entry (not the edge under test) + leave, so the messages it
        // produces don't get credited to the re-entry below.
        click_sidebar_row(&result, admin);
        click_sidebar_row(&result, feed);
        drain(&rx);

        click_sidebar_row(&result, admin);

        assert!(
            wait_for_own_membership_tier_names_loaded(&rx, Duration::from_secs(10)),
            "returning to the Admin shell via the real sidebar row must refetch \
             own_membership_tier_names (part of `refresh_admin_shell`) — no \
             `OwnMembershipTierNamesLoaded` arrived on the UI channel within the budget"
        );
    });
}

/// Discard every message currently queued, without waiting for more.
fn drain(rx: &mpsc::Receiver<crate::app::UiMessage>) {
    while rx.try_recv().is_ok() {}
}

/// Poll `rx` until `OwnMembershipTierNamesLoaded` arrives or `budget` elapses.
/// Convention 14: a named generous ceiling + deadline poll, never a fixed sleep.
fn wait_for_own_membership_tier_names_loaded(
    rx: &mpsc::Receiver<crate::app::UiMessage>,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match rx.recv_timeout(remaining) {
            Ok(crate::app::UiMessage::Data(
                crate::app::DataMessage::OwnMembershipTierNamesLoaded { .. },
            )) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture guards — a sweep that walks a hidden window passes vacuously
// ---------------------------------------------------------------------------

/// The fixture actually shows a surface. Without this, every sweep above could
/// go green over an empty id list forever: GTK's ancestor-aware `is_visible()`
/// reports every descendant of an unshown window hidden, which is exactly what
/// the first run of this walk measured (0 showing ids on all 13 pages).
#[test]
fn the_walk_sees_a_real_surface() {
    run_on_gtk_thread(|| {
        let (_app, result) = offline_window();
        let ids = showing_ids(&result.window);
        assert!(
            ids.len() > 20,
            "the offline fixture shows only {} test ids — the window is not really presented, \
             and every sweep in this module would pass vacuously; ids: {ids:?}",
            ids.len()
        );
        for tab in ["feed-tab", "settings-tab", "conversations-tab"] {
            assert!(
                ids.iter().any(|n| n == tab),
                "the sidebar row {tab} is not showing; ids: {ids:?}"
            );
        }
    });
}

/// The offline gate has something to say on this fixture — the twin guard for
/// invariants I6/I7, which are otherwise satisfied by an EMPTY registry.
///
/// It names the `admin-nest` declarations specifically rather than counting,
/// because a count says nothing about *which* control lost its declaration:
/// silently dropping the `declare_wire_kind` beside a `set_test_id` is the way
/// this mechanism decays, and it decays one control at a time. `build_main_window`
/// builds the admin view eagerly (`app.rs`), so these exist on every surface the
/// sweeps visit, which is what lets I6 run everywhere instead of on one page.
#[test]
fn the_offline_gate_holds_the_admin_nest_page() {
    run_on_gtk_thread(|| {
        let (_app, _result) = offline_window();
        let declared = crate::offline_gate::declarations();
        for id in [
            "admin-service-pairing-toggle",
            "admin-nest-serving-port-save-button",
            "admin-nest-nat-mode-save-button",
            "nest-os-restart-now-button",
            "admin-factory-reset-button",
            // The outside-app sign-in keys' two dispatching controls: the
            // ordinary rotation, and the one confirm both forced arms share
            // (declared with the key arm's kind until an arm re-declares it).
            // The two arm buttons dispatch nothing, so they declare nothing.
            "admin-nest-oauth-rotate-button",
            "admin-nest-oauth-confirm-button",
        ] {
            let (_, kind, enabled) = declared
                .iter()
                .find(|(name, _, _)| name == id)
                .unwrap_or_else(|| {
                    panic!(
                        "{id} declares no wire kind — its `declare_wire_kind` is gone, so the \
                         control is ungated offline and I6 cannot see it; declared: {:?}",
                        declared.iter().map(|(n, _, _)| n).collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                fauna_protocol::offline_class::offline_class(kind),
                Some(fauna_protocol::offline_class::OfflineClass::OnlineOnly),
                "{id} declares {kind}, which this test assumes is OnlineOnly"
            );
            assert!(
                !enabled,
                "{id} issues {kind} and the fixture has no nest, but it is still enabled"
            );
        }
    });
}

/// The twin guard for the mail-aliases and mail-lists add/edit sheets'
/// submit buttons. Both buttons are persistent widgets whose dispatched kind
/// depends on `ctx.form_mode`, not on anything the widget itself carries, so
/// unlike the toggle in `build_alias_row` (rebuilt fresh from the snapshot on
/// every render) a stale re-declare here is invisible to I6/I7's generic
/// sweep — it only ever sees whatever was declared LAST, correct or not.
/// Named, like `the_offline_gate_holds_the_admin_nest_page`, so a dropped
/// `declare_wire_kind` call fails here with the missing id rather than
/// silently vanishing from the registry.
///
/// Both sheets seed `FormMode::Add`, so the pages' construction-time
/// declaration (`Create`) is what this fixture — which never clicks Edit —
/// can observe; `open_add_sheet`/`open_edit_sheet`'s re-declare calls are the
/// same one-line shape, exercised by `just fauna-linux-test-check`'s existing
/// GTK compile check rather than re-proven here.
#[test]
fn the_offline_gate_holds_the_mail_aliases_and_mail_lists_submit_buttons() {
    run_on_gtk_thread(|| {
        let (_app, _result) = offline_window();
        let declared = crate::offline_gate::declarations();
        for (id, expected_kind) in [
            (
                "mail-aliases-add-sheet-submit-button",
                "fauna.bridges.create_account_alias",
            ),
            (
                "mail-lists-add-sheet-submit-button",
                "fauna.bridges.create_account_list",
            ),
        ] {
            let (_, kind, enabled) = declared
                .iter()
                .find(|(name, _, _)| name == id)
                .unwrap_or_else(|| {
                    panic!(
                        "{id} declares no wire kind — its `declare_wire_kind` is gone, so the \
                         control is ungated offline and I6 cannot see it; declared: {:?}",
                        declared.iter().map(|(n, _, _)| n).collect::<Vec<_>>()
                    )
                });
            assert_eq!(
                *kind, expected_kind,
                "{id} declares {kind}, not the seeded FormMode::Add kind {expected_kind}"
            );
            assert_eq!(
                fauna_protocol::offline_class::offline_class(kind),
                Some(fauna_protocol::offline_class::OfflineClass::OnlineOnly),
                "{id} declares {kind}, which this test assumes is OnlineOnly"
            );
            assert!(
                !enabled,
                "{id} issues {kind} and the fixture has no nest, but it is still enabled"
            );
        }
    });
}

fn tab_index_of(target: crate::views::sidebar::SidebarItem) -> usize {
    all_sidebar_items()
        .iter()
        .position(|i| *i == target)
        .expect("sidebar item")
}

fn settings_tab_index() -> usize {
    tab_index_of(crate::views::sidebar::SidebarItem::Settings)
}

fn admin_tab_index() -> usize {
    tab_index_of(crate::views::sidebar::SidebarItem::Admin)
}

fn feed_tab_index() -> usize {
    tab_index_of(crate::views::sidebar::SidebarItem::Feed)
}
