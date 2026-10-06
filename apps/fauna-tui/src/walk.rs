//! Systematic UI-path exploration — tier_1, offline, in-process.
//!
//! The hand-authored tests in this crate (and the e2e suite) each drive ONE
//! specific path and assert ONE specific outcome; three live-user-found bugs
//! (the settings focus-highlight collision, the sidebar return-to-Settings
//! stale sub-page, the Media empty-set dead end) violated no assertion any of
//! them made. This module is the other half of the methodology (convention:
//! `docs/goal/architecture/testing.md` § Cross-app e2e conventions, point 17;
//! design record tracked internally, 2026-08-03):
//! walk many reachable states — exhaustively where the state space is
//! enumerable, randomly (with shrinking) where it is not — and assert GENERAL
//! invariants after every step, instead of one hand-picked final state.
//!
//! The walk drives the same doors a real keystroke or terminal-mouse click
//! drives (`App::handle_key`, `App::click_sidebar`, `App::click_page_element`)
//! — never a private seam — so a bug that lives in the difference between the
//! human path and the agent path (`set_page`'s edge detection was exactly
//! that) is reachable. Everything runs on the offline `authed_app()` fixture:
//! no nest, no IO, deterministic; spawned page ops dial a closed port and
//! their outcomes land in a dropped channel, harmlessly.
//!
//! The invariants here are deliberately few and TRUE BY CONSTRUCTION of the
//! UI model — a checker that cries wolf gets deleted. Each one names the real
//! bug class it catches.

use crate::app::tests::{authed_app, key};
use crate::app::{App, Zone};
use crate::pages::Page;
use crossterm::event::KeyCode;

/// I1 — **at most one painted line ever wears the focus affordance**, and it
/// is exactly the line of the element the focus model says is focused.
///
/// Catches the id-collision class: paint used to compare focus by element ID,
/// and the Settings rail's ~19 deliberately id-less rows all compared equal —
/// a live user's "every menu row is highlighted at once". The count is read
/// through [`crate::ui::focused_line_count`], the exact `element_lines` call
/// the terminal paints through, so the check can never drift from the screen.
fn check_focus_paint(app: &App, ctx: &str) -> Result<(), String> {
    let painted = crate::ui::focused_line_count(app);
    let in_sidebar = app.zone == Zone::Sidebar;
    let expected = usize::from(!in_sidebar && app.focused_index().is_some());
    if painted != expected {
        return Err(format!(
            "{ctx}: {painted} lines paint the focus affordance, expected {expected} \
             (page {:?}, in_sidebar {in_sidebar}, focused_index {:?})",
            app.page,
            app.focused_index(),
        ));
    }
    Ok(())
}

/// I2 — **the focus ring and the element list agree on which element is
/// focused.** [`App::focused`] indexes the focusable-only ring while
/// [`App::focused_index`] is an absolute position in [`App::page_elements`];
/// the two models must resolve to the same element, and that element must be
/// focusable and in bounds.
fn check_focus_model(app: &App, ctx: &str) -> Result<(), String> {
    let Some(i) = app.focused_index() else {
        return Ok(());
    };
    let elements = app.page_elements();
    let Some(absolute) = elements.get(i) else {
        return Err(format!(
            "{ctx}: focused_index {i} out of bounds ({} elements) on {:?}",
            elements.len(),
            app.page
        ));
    };
    if !absolute.focusable() {
        return Err(format!(
            "{ctx}: focused_index {i} lands on a non-focusable {:?} ({:?}) on {:?}",
            absolute.role, absolute.id, app.page
        ));
    }
    let ring = app
        .focused()
        .ok_or_else(|| format!("{ctx}: focused_index is Some({i}) but focused() is None"))?;
    if ring.id != absolute.id || ring.text != absolute.text {
        return Err(format!(
            "{ctx}: ring element ({:?} / {:?}) != absolute element ({:?} / {:?}) at index {i}",
            ring.id, ring.text, absolute.id, absolute.text
        ));
    }
    Ok(())
}

/// I3 — **no input prompts with its own element id.** An unlabelled input's
/// paint falls back to the element id (all a page without labels can offer),
/// which shows a machine string to the human — a live surface really shipped
/// "search-query-field: _" as its prompt (the copy-audit corpus, 2026-08-03).
/// The rule is generic so a NEW page's unlabelled input fails here instead of
/// waiting for the next human report (the dynamic-enforcement half of tui.md
/// § Control vocabulary).
fn check_input_prompts(app: &App, ctx: &str) -> Result<(), String> {
    use crate::element::Role;
    for e in app.page_elements() {
        if matches!(e.role, Role::Input(_) | Role::InputCommit { .. }) && e.label.is_none() {
            return Err(format!(
                "{ctx}: input {:?} has no label — it prompts with its raw element id",
                e.id
            ));
        }
    }
    Ok(())
}

/// I4 — **a back affordance paints as a way out.** Every `*-nav-back` button
/// carries [`crate::element::Element::nav_back`], so it paints `◂ Back` rather
/// than `[ Back ]` (an action) or `Back ▸` (the wrong direction).
///
/// Generic on purpose. The id family is the contract — ui.yaml scopes exactly
/// these two ids as the rail-exit affordance — and the sub-pages carrying them
/// are still multiplying (eight more are queued), so a per-page assertion would
/// be stale the week it was written. This fails the suite on the class instead.
fn check_nav_back(app: &App, ctx: &str) -> Result<(), String> {
    for e in app.page_elements() {
        if e.id.ends_with("-nav-back") && !e.nav_back {
            return Err(format!(
                "{ctx}: {:?} is a back affordance but is not `nav_back` — \
                 it paints as an action, or points forward",
                e.id
            ));
        }
    }
    Ok(())
}

/// I5 — **a one-of-N group either shows which one, or the surface says why
/// not.** Radios form a group (an explicit `group` attr where a page has more
/// than one); a group with no marked member paints N identical `( )` rows and
/// answers nothing, which is precisely the "unknowable" state a live user
/// reported (`apps/tui.md` § Rendering → *Control vocabulary*, rule 1).
///
/// **Two selected is always a bug** — mutually exclusive means one. **Zero**
/// needs a reason on screen, and there are two ways to carry one: the page's
/// `error-message` (the default — the current value genuinely isn't known
/// because the fetch is out or failed), or an explainer declaring
/// `explains-unset` (a first-time choice nobody has made yet — see the contract
/// inside). Either way this is rule 3 — *disabled-with-no-reason is a copy
/// bug* — generalized from disabled controls to state, and the same rule 6
/// (*state legibility is copy's twin*) that the Task-delegation load half was
/// fixed under. Guessing a default instead would paint a value the server may
/// not hold, which is worse than silence.
///
/// Implicit-by-default like [`check_input_prompts`]: a page that declares
/// nothing still gets checked, so the invariant cannot be dodged by omission.
/// Two-plus-group pages opt into `group` and the check follows them.
fn check_radio_groups(app: &App, ctx: &str) -> Result<(), String> {
    use crate::element::Role;
    use std::collections::BTreeMap;

    let attr = |e: &crate::element::Element, k: &str| {
        e.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone())
    };
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for e in app.page_elements() {
        let Role::Radio { selected, .. } = &e.role else {
            continue;
        };
        let group = attr(&e, "group").unwrap_or_default();
        let entry = groups.entry(group).or_default();
        entry.0 += 1;
        entry.1 += usize::from(*selected);
    }
    // **The `explains-unset` contract.** Zero-marked has two very different
    // causes, and only one of them is the bug I5 exists to catch:
    //
    //   * A group DISPLAYING a value the app holds — Privacy's inbox mode — is
    //     blank because the value isn't known. That is the caught class: the
    //     page owes the reason, and by default the `error-message` carries it.
    //   * A group ASKING for a first-time choice — the wizard's provider rows —
    //     is blank because the user hasn't chosen yet, which is the whole point
    //     of the screen. Nothing is wrong; what rule 5 wants is the line that
    //     says so ("Choose who hosts your server to continue.").
    //
    // The second kind is excused by the EXPLAINER declaring what it covers:
    // `.attr("explains-unset", <group>)`, matching the group's own `group` attr
    // (`""` for a page's single undeclared group). Deliberately not a boolean
    // opt-out on the rows — an excuse must be a line that is actually ON SCREEN
    // and actually SAYS something, so the check resolves the declaration to a
    // painted element with non-empty text. That keeps the invariant honest in
    // the way it most needed: `dns-status-text` was in exactly this role while
    // returning an EMPTY key in its gating state, painting a blank line beside
    // a dead Continue on all 7 apps (fixed 2026-08-05) — under this contract
    // that regression reds I5 rather than passing as "declared, therefore fine".
    //
    // Declaring on the explainer rather than the rows is also what lets an
    // un-id'd chrome line serve, which rule 5's own exemplars are: ids are
    // ui.yaml's domain and an explainer earns none (`ui/README.md` rule 5).
    let explained = |group: &str| {
        app.page_elements().iter().any(|e| {
            attr(e, "explains-unset").as_deref() == Some(group) && !e.text.trim().is_empty()
        })
    };
    let stated_reason = app.error_line_text().is_some();
    for (group, (total, selected)) in groups {
        let named = if group.is_empty() {
            "the page's undeclared radio group".to_string()
        } else {
            format!("radio group {group:?}")
        };
        if selected > 1 {
            return Err(format!(
                "{ctx}: {named} marks {selected} of {total} options current on {:?} \
                 — mutually exclusive means one",
                app.page
            ));
        }
        if selected == 0 && !stated_reason && !explained(&group) {
            return Err(format!(
                "{ctx}: {named} paints {total} options and marks NONE current on \
                 {:?}, and the surface states no reason — N dead rows that answer \
                 nothing (state legibility is copy's twin)",
                app.page
            ));
        }
    }
    Ok(())
}

/// I6 — **nothing that needs a nest is actuable without one.** No painted
/// affordance whose wire kind is `OnlineOnly` may be enabled while the app has
/// no live connection (W4 (account-data-plane.md § Workstreams) phase 4 — `account-data-plane.md` § The
/// offline-mutation contract, class 3: "UI desensitizes these offline").
///
/// True by construction of the UI model: [`App::page_elements`] runs the gate
/// over every element it returns, so the only way to violate this is to bypass
/// that seam. That is exactly the bug class it catches — a page that builds its
/// own element list somewhere the gate does not reach.
///
/// The whole walk runs on the offline `authed_app()` fixture (module docs), so
/// every state it reaches is a state this invariant genuinely constrains; the
/// online direction is pinned by the unit tests in [`crate::app`].
fn check_offline_gate(app: &App, ctx: &str) -> Result<(), String> {
    for element in app.page_elements() {
        if !element.enabled {
            continue;
        }
        let Some(kind) = element.gesture().and_then(|g| g.wire_kind()) else {
            continue;
        };
        if !fauna_protocol::offline_class::affordance(kind, app.connection_state_word())
            .is_available()
        {
            return Err(format!(
                "{ctx}: {:?} on {:?} is enabled while {} — but it issues {kind}, \
                 which needs a live nest",
                element.id,
                app.page,
                app.connection_state_word(),
            ));
        }
    }
    Ok(())
}

/// I7 — **a declared wire kind is a kind the table knows.** Every kind a
/// painted element's gesture declares must be registered in
/// `fauna_protocol::offline_class`.
///
/// This is I6's silent-failure twin, and the reason it needs its own invariant:
/// [`affordance`](fauna_protocol::offline_class::affordance) reads an
/// unregistered kind as `Available` **on purpose** (its ruling 2 — an older app
/// meeting a future kind must keep its controls live rather than grey out a
/// working button). That forward-compat choice means a *typo* in one of this
/// app's own declarations does not fail: it disables the gate for that
/// affordance and nothing anywhere says so — I6 included, since an ungated
/// element is a passing element. Inside one binary the polarity is the
/// opposite one: tui compiles against this very table, so a kind it declares
/// and the table does not know is a mistake, every time.
///
/// Checked over **every** painted element, enabled or not — a disabled control
/// is exempt from the gate, never from being spelled correctly.
fn check_declared_kinds_registered(app: &App, ctx: &str) -> Result<(), String> {
    for element in app.page_elements() {
        let Some(kind) = element.gesture().and_then(|g| g.wire_kind()) else {
            continue;
        };
        if fauna_protocol::offline_class::offline_class(kind).is_none() {
            return Err(format!(
                "{ctx}: {:?} on {:?} declares the wire kind {kind}, which is not \
                 registered in `fauna_protocol::offline_class` — an unregistered \
                 kind reads as Available, so this affordance is silently ungated",
                element.id, app.page,
            ));
        }
    }
    Ok(())
}

fn check_all(app: &App, ctx: &str) -> Result<(), String> {
    check_focus_paint(app, ctx)?;
    check_focus_model(app, ctx)?;
    check_input_prompts(app, ctx)?;
    check_nav_back(app, ctx)?;
    check_radio_groups(app, ctx)?;
    check_offline_gate(app, ctx)?;
    check_declared_kinds_registered(app, ctx)?;
    Ok(())
}

/// Navigate to `target` through the REAL sidebar ring — `focus_next`/`focus_prev`
/// in the sidebar zone, where "selection *is* navigation" — never a direct page
/// write. This is the exact human path the stale-sub-page bug lived in (the
/// ring's page writes used to bypass the nav-edge reset).
fn sidebar_ring_to(app: &mut App, target: Page) {
    app.enter_sidebar_zone();
    for _ in 0..app.sidebar_pages().len() {
        if app.page == target {
            return;
        }
        let rows = app.sidebar_pages();
        let cur = rows.iter().position(|p| *p == app.page).unwrap_or(0);
        let tgt = rows.iter().position(|p| *p == target).unwrap_or(0);
        if tgt > cur {
            app.focus_next();
        } else {
            app.focus_prev();
        }
    }
    assert_eq!(app.page, target, "sidebar ring never reached {target:?}");
}

/// Walk the page's whole focus ring (one full wrap plus a step) in the page
/// zone, checking every stop.
fn walk_full_ring(app: &mut App, ctx: &str) -> Result<(), String> {
    let focusable = app.page_elements().iter().filter(|e| e.focusable()).count();
    for step in 0..=focusable {
        check_all(app, &format!("{ctx}, ring step {step}/{focusable}"))?;
        app.focus_next();
    }
    Ok(())
}

/// The authed fixture with both gated sidebar rows visible: `am_i_admin` (the
/// `fauna.account.am_i_admin` gate, `docs/goal/ui/README.md` § Auth model) and
/// `has_family`. Offline like [`authed_app`] — the admin/family pages render
/// their empty/default states, which is exactly what the gated-surface sweeps
/// and the corpus dump need to see.
fn admin_app() -> App {
    let mut app = authed_app();
    app.am_i_admin = true;
    app.has_family = true;
    app
}

/// Sweep-1 body: every visible sidebar page, every focus position.
fn sweep_every_sidebar_page(app: &mut App) {
    for page in app.sidebar_pages().clone() {
        sidebar_ring_to(app, page);
        check_all(app, &format!("{page:?} (sidebar preview)")).unwrap();
        app.enter_page_zone();
        walk_full_ring(app, &format!("{page:?}")).unwrap();
        app.enter_sidebar_zone();
    }
}

/// Exhaustive sweep 1 — every sidebar page, every focus position: the focus
/// affordance is unique and the focus models agree. Would have caught the
/// settings-highlight collision on its first visit to Settings.
#[tokio::test]
async fn every_sidebar_page_paints_at_most_one_focused_line_at_every_focus_position() {
    sweep_every_sidebar_page(&mut authed_app());
}

/// Sweep 1 over the GATED sidebar rows too — Admin and Family only join
/// [`App::sidebar_pages`] when their gates pass, so the plain fixture's sweep
/// can never reach them and their surfaces would otherwise carry no invariant
/// coverage at all (the Privacy-page precedent: I5's first run found a live
/// defect exactly where no sweep had ever looked).
#[tokio::test]
async fn every_admin_and_family_page_paints_at_most_one_focused_line_too() {
    sweep_every_sidebar_page(&mut admin_app());
}

/// Exhaustive sweep 2 — drill into every focusable child of the Settings rail
/// (the ~21 sub-pages, through their REAL rail-row gestures), walking each
/// surface's whole ring. The rail rows are deliberately id-less, which is
/// exactly why no per-id e2e query can cover this and an in-process sweep can.
#[tokio::test]
async fn every_settings_sub_surface_paints_at_most_one_focused_line() {
    let mut app = authed_app();
    let rail_len = {
        sidebar_ring_to(&mut app, Page::Settings);
        app.enter_page_zone();
        app.page_elements().len()
    };
    for child in 0..rail_len {
        // A fresh Settings entry per child: ring away and back — the nav-edge
        // reset itself restores the rail Root baseline.
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Settings);
        assert_eq!(
            app.settings.sub,
            crate::settings::SubPage::Root,
            "re-entering Settings via the sidebar must land on the rail Root"
        );
        app.enter_page_zone();
        app.click_page_element(child);
        walk_full_ring(&mut app, &format!("Settings child {child}")).unwrap();
        // Esc pops whatever the click opened (sub-page, overlay) — and the
        // popped-back surface must uphold the invariants too.
        app.handle_key(key(KeyCode::Esc));
        walk_full_ring(&mut app, &format!("after Esc from Settings child {child}")).unwrap();
    }
}

/// The Devices page with the T16 custody families PAINTED — the offline
/// `authed_app` fixture can only ever walk them empty, so this drives the
/// page's sanctioned fixture door (`inject_custody_facet_for_corpus`, the
/// wizard corpus seam's twin) with all three families in their distinct
/// states (live + pending owner rows, running + stopped holds, a pending
/// offer) and walks the whole ring under the standard invariant bundle —
/// focus, prompts, radio groups, nav-back, the offline gate over the new
/// gestures' declared kinds, and I7's kind registration.
#[tokio::test]
async fn the_devices_custody_families_uphold_the_invariants_when_painted() {
    let mut app = authed_app();
    sidebar_ring_to(&mut app, Page::Settings);
    app.settings.sub = crate::settings::SubPage::Devices;
    app.settings
        .inject_custody_facet_for_corpus(crate::settings::devices::custody_walk_facet());
    app.enter_page_zone();
    check_all(&app, "Devices with custody families painted").unwrap();
    walk_full_ring(&mut app, "Devices custody families").unwrap();
}

/// Exhaustive sweep 3 — the stale-sub-page class: for every Settings rail row,
/// drill in, leave via the REAL sidebar ring, return via the REAL sidebar
/// ring, and require the rail Root — the documented canonical entry state
/// (`set_page`'s nav-edge reset). Red before the `set_page` unification: the
/// ring's direct page writes made the edge check always read false, so a user
/// returning to Settings landed on the stale sub-page "stuck forever".
#[tokio::test]
async fn returning_to_settings_via_the_sidebar_always_lands_on_the_rail_root() {
    let mut app = authed_app();
    sidebar_ring_to(&mut app, Page::Settings);
    app.enter_page_zone();
    let rail_len = app.page_elements().len();
    for child in 0..rail_len {
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Settings);
        app.enter_page_zone();
        app.click_page_element(child);
        // Leave through the ring, return through the ring — the human path.
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Settings);
        assert_eq!(
            app.settings.sub,
            crate::settings::SubPage::Root,
            "sidebar return to Settings after opening child {child} kept a stale sub-page"
        );
        check_all(&app, &format!("Settings re-entry after child {child}")).unwrap();
    }
}

/// Exhaustive sweep 4 — drill into every admin rail row (the tui adaptation of
/// the GUI's admin sidebar-swap; the rows are deliberately id-less like the
/// Settings rail's), walking each sub-page's whole ring. The admin shell is
/// sweep 2's exact structural twin, so it inherits the same blind spot if only
/// hand-authored tests cover it.
#[tokio::test]
async fn every_admin_sub_surface_paints_at_most_one_focused_line() {
    let mut app = admin_app();
    for child in 0..crate::admin::AdminPage::BUILT.len() {
        // A fresh Admin entry per child — the nav-edge reset restores the
        // Dashboard baseline, sweep 2's own re-entry idiom.
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Admin);
        assert_eq!(
            app.admin.sub,
            crate::admin::AdminPage::Dashboard,
            "re-entering Admin via the sidebar must land on the Dashboard"
        );
        app.enter_page_zone();
        app.click_page_element(child);
        walk_full_ring(&mut app, &format!("Admin child {child}")).unwrap();
    }
}

// ---------------------------------------------------------------------------
// The copy-audit corpus dump — a reader, not an invariant.
// ---------------------------------------------------------------------------

/// One element's row in the corpus table: kind, id, enabled, and the texts a
/// human or driver reads off it.
fn element_row(e: &crate::element::Element) -> String {
    use crate::element::Role;
    let kind = match &e.role {
        Role::Label => "label",
        Role::Button(_) => "button",
        Role::Input(_) => "input",
        Role::InputCommit { .. } => "input-commit",
        Role::Checkbox { .. } => "checkbox",
        Role::Radio { .. } => "radio",
        Role::Select { .. } => "select",
    };
    let id = if e.id.is_empty() { "(chrome)" } else { &e.id };
    let mut row = format!("~ {kind:<12} {id:<36} {:?}", e.text);
    if let Some(l) = &e.label {
        row.push_str(&format!(" label={l:?}"));
    }
    if !e.enabled {
        row.push_str(" DISABLED");
    }
    row
}

/// Print one surface: the literal painted rows (what a terminal shows —
/// `ui::painted_page_text` reuses paint's own `element_lines` call), then the
/// element kind/id table (what automation and the focus ring see).
fn dump_surface(app: &App, ctx: &str) {
    println!("\n=== {ctx} [page {:?}] ===", app.page);
    for line in crate::ui::painted_page_text(app) {
        println!("|{line}");
    }
    for e in app.page_elements() {
        println!("{}", element_row(&e));
    }
    if let Some(err) = app.error_line_text() {
        println!("~error       |{err}");
    }
}

/// [`dump_surface`] for the unauthenticated screen (wizard / launch / unlock):
/// same painted rows + element table — `page_elements` routes to the launch
/// surface pre-auth — plus the screen chrome a human also reads there, which
/// the page pane doesn't have: the pane title and the DIM description block
/// (`render_screen` paints both above the elements).
fn dump_screen(app: &App, ctx: &str) {
    println!("\n=== {ctx} [screen title {:?}] ===", app.screen_title());
    for d in app.screen_description() {
        println!("~desc        |{d}");
    }
    for line in crate::ui::painted_page_text(app) {
        println!("|{line}");
    }
    for e in app.page_elements() {
        println!("{}", element_row(&e));
    }
    if let Some(err) = app.screen_error_text() {
        println!("~error       |{err}");
    }
}

/// **Not an assertion — the copy-comprehensibility audit's corpus reader.**
/// Dumps every surface tui can paint: every sidebar page (gated Admin/Family
/// rows included), every Settings child surface (and its post-Esc state),
/// every admin rail sub-page, the folder wizard's steps and the other
/// machine-backed overlays (share form, conflicts, media detail + its three
/// confirms), every onboarding wizard step, and every launch/unlock surface —
/// each as the literal painted rows plus the element kind/id table — so an
/// auditor can read "every string tui shows a human" without hand-visiting
/// each screen.
///
/// Machine-backed overlays open off INJECTED snapshots/state (the offline
/// fixture wires no machines, so the real doors can't open them): sanctioned
/// here because this is a reader, not a walk — convention 17's no-pre-seeding
/// discipline binds the invariant walks above, not the corpus. The injected
/// values come from the machines' own canonical producers, so labels resolve
/// exactly as production resolves them.
/// Ignored by default; run:
///
/// ```text
/// cargo test -p fauna-tui dump_screen_text_corpus -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "corpus dump for the copy audit, not an invariant"]
async fn dump_screen_text_corpus() {
    let mut app = authed_app();

    // Every sidebar page, at its entry state.
    for page in app.sidebar_pages().clone() {
        sidebar_ring_to(&mut app, page);
        app.enter_page_zone();
        dump_surface(&app, &format!("{page:?}"));
        app.enter_sidebar_zone();
    }

    // Every Settings child surface (sweep-2 navigation: fresh entry per child).
    sidebar_ring_to(&mut app, Page::Settings);
    app.enter_page_zone();
    let rail_len = app.page_elements().len();
    for child in 0..rail_len {
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Settings);
        app.enter_page_zone();
        let row = app
            .page_elements()
            .get(child)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        app.click_page_element(child);
        dump_surface(&app, &format!("Settings child {child} ({row})"));
        app.handle_key(key(KeyCode::Esc));
    }

    // ── The folder surfaces the offline fixture can't open through the real
    // doors: navigate to the Folders sub-page (found by its
    // `folder-add-button` rather than a hardcoded rail index — the rail rows
    // are deliberately id-less), then INJECT the devices snapshot per state.
    for child in 0..rail_len {
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Settings);
        app.enter_page_zone();
        app.click_page_element(child);
        if app
            .page_elements()
            .iter()
            .any(|e| e.id == "folder-add-button")
        {
            break;
        }
        app.handle_key(key(KeyCode::Esc));
    }
    dump_folder_overlays(&mut app);

    // ── Media item detail + its three confirms (machine-backed: the detail
    // surface only paints once a machine exists, so wire one against a closed
    // port — the walk fixture's own no-IO idiom).
    dump_media_detail(&mut app);

    // ── The gated sidebar rows: every admin rail sub-page, then Family.
    let mut app = admin_app();
    for child in 0..crate::admin::AdminPage::BUILT.len() {
        sidebar_ring_to(&mut app, Page::Feed);
        sidebar_ring_to(&mut app, Page::Admin);
        app.enter_page_zone();
        let row = app
            .page_elements()
            .get(child)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        app.click_page_element(child);
        dump_surface(&app, &format!("Admin child {child} ({row})"));
    }
    sidebar_ring_to(&mut app, Page::Family);
    app.enter_page_zone();
    dump_surface(&app, "Family");

    // ── The unauthenticated world: every onboarding wizard step, the
    // outcome-keyed "Almost ready" surface, and every launch/unlock surface.
    dump_onboarding_and_launch_surfaces();
}

/// The folder creation wizard's four steps (plus the Review submit phases),
/// the expanded row + share form, and a conflicts row — all painted by
/// injecting `DevicesSnapshot` state on the Folders sub-page the caller has
/// already navigated to.
fn dump_folder_overlays(app: &mut App) {
    use fauna_devices_machine::{ConflictCandidateSummary, ConflictSummary, DevicesSnapshot};
    use fauna_folders_machine::{
        DevicePlacesSnapshot, FolderWizardSnapshot, FolderWizardStep, NameSnapshot, ReviewSnapshot,
        SubmitPhase, WizardDevice,
    };
    use fauna_protocol::folders::PlaceFlags;

    // Two seats at two different flag points, so the corpus carries both a
    // fully-ticked and a partly-ticked device row.
    let seat = |device_id: &str, label: &str, selected: bool, f: PlaceFlags| WizardDevice {
        device_id: device_id.into(),
        label: label.into(),
        selected,
        originates: f.originates,
        accepts: f.accepts,
        applies_deletes: f.applies_deletes,
    };
    let seats = || {
        vec![
            seat("aa11", "field-laptop", true, PlaceFlags::default_place()),
            seat("bb22", "shelf-nas", false, PlaceFlags::archive_place()),
        ]
    };

    let base_wizard = |step: FolderWizardStep| FolderWizardSnapshot {
        step,
        name: NameSnapshot {
            name: "corpus".into(),
            continue_enabled: true,
        },
        device_places: DevicePlacesSnapshot {
            devices: seats(),
            continue_enabled: true,
        },
        review: ReviewSnapshot {
            name: "corpus".into(),
            retention: None,
            enrolled: Vec::new(),
            create_enabled: true,
            phase: SubmitPhase::Idle,
            created: false,
            failed_members: Vec::new(),
            error: None,
        },
    };
    let with_wizard = |wizard: FolderWizardSnapshot| DevicesSnapshot {
        wizard: Some(wizard),
        ..Default::default()
    };

    // Step 1 in its two states a user actually reads: fresh (empty name,
    // Next gated), and named.
    let mut fresh = base_wizard(FolderWizardStep::Name);
    fresh.name.name = String::new();
    fresh.name.continue_enabled = false;
    app.settings
        .inject_devices_snapshot_for_corpus(with_wizard(fresh));
    dump_surface(app, "folder wizard step 1 Name (empty name)");
    app.settings
        .inject_devices_snapshot_for_corpus(with_wizard(base_wizard(FolderWizardStep::Name)));
    dump_surface(app, "folder wizard step 1 Name (named)");
    app.settings
        .inject_devices_snapshot_for_corpus(with_wizard(base_wizard(FolderWizardStep::Devices)));
    dump_surface(app, "folder wizard step 2 Devices (two devices)");
    for (label, phase, created, failed, error) in [
        ("Idle", SubmitPhase::Idle, false, vec![], None),
        ("Submitting", SubmitPhase::Submitting, false, vec![], None),
        (
            "Failed (set created, one member add failed)",
            SubmitPhase::Failed,
            true,
            vec!["shelf-nas".to_string()],
            // The key + arg the machine's own submit path produces
            // (`fauna-folders-machine::submit_error`, MEMBER_ERROR_KEY).
            Some(fauna_core::localized::LocalizedText::key_arg(
                "devices.wizard.create_member_error",
                "message",
                "nest unreachable".to_string(),
            )),
        ),
    ] {
        let mut w = base_wizard(FolderWizardStep::Review);
        w.review.phase = phase;
        w.review.created = created;
        w.review.failed_members = failed;
        w.review.error = error;
        app.settings
            .inject_devices_snapshot_for_corpus(with_wizard(w));
        dump_surface(app, &format!("folder wizard step 4 Review ({label})"));
    }

    // The list surfaces behind the wizard: rows in each mode, the expanded
    // row's body, its armed share form, and an unresolved conflict.
    let rows_snapshot = DevicesSnapshot::default;
    let mut listed = rows_snapshot();
    listed.conflicts = vec![ConflictSummary {
        id: 1,
        folder: "photos".into(),
        device_id: "aa11".into(),
        path: "cat.jpg".into(),
        conflict_type: "concurrent_edit".into(),
        details: None,
        created_at: 1_700_000_000,
        candidates: vec![ConflictCandidateSummary {
            manifest_hash: "deadbeef".into(),
            device_id: "bb22".into(),
            size_bytes: 1024,
            created_at: 1_700_000_100,
            content_key_version: None,
        }],
        resolved_at: None,
        resolution: None,
        winning_manifest_hash: None,
        has_other_version: true,
        file_info: "1.0 KB, edited on two devices".into(),
    }];
    app.settings.inject_devices_snapshot_for_corpus(listed);
    dump_surface(app, "folders page with an unresolved conflict");

    // Rows in two modes, the expanded row's body, and its armed share form —
    // expansion and arming go through the REAL click doors (they are local UI
    // state); only the row data itself is injected.
    // Struct-update form: `FolderSummary` grows wire-additively, and the corpus
    // fixture must not be a compile break for whoever adds the next field.
    let folder = |id: i64, name: &str| fauna_devices_machine::FolderSummary {
        id,
        name: name.to_string(),
        cached_snapshot_count: 2,
        cached_total_bytes: 4096,
        cached_last_snapshot_at: Some(1_700_000_000),
        ..Default::default()
    };
    let mut with_rows = rows_snapshot();
    with_rows.folders = vec![folder(1, "photos"), folder(2, "site")];
    app.settings.inject_devices_snapshot_for_corpus(with_rows);
    dump_surface(app, "folders page with rows (two folders, collapsed)");
    if let Some(i) = app
        .page_elements()
        .iter()
        .position(|e| e.id == "folder-row")
    {
        app.click_page_element(i);
        dump_surface(app, "folder row expanded (sync)");
        if let Some(i) = app
            .page_elements()
            .iter()
            .position(|e| e.id == "folder-share-button")
        {
            app.click_page_element(i);
            dump_surface(app, "folder share form armed");
        }
    }
}

/// The media detail sub-page and its three armed confirms, painted by wiring a
/// no-IO machine (closed port) and injecting `DetailState` directly.
fn dump_media_detail(app: &mut App) {
    use crate::media::DetailState;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    app.media = crate::media::init(
        fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        ),
        [7u8; 32],
        std::sync::Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
        &tx,
        &[],
        &[],
        &[],
        crate::settings::follows_door(Default::default()),
    );
    sidebar_ring_to(app, Page::Media);
    app.enter_page_zone();
    dump_surface(app, "Media (machine wired, no items)");

    let detail = |versions| DetailState {
        folder: "photos".into(),
        path: "cat.jpg".into(),
        name: "cat.jpg".into(),
        versions,
        show_pruned: false,
        arming: None,
        external_arming: false,
        delete_arming: false,
        followed_scope_value: None,
    };
    app.media.detail = Some(detail(None));
    dump_surface(app, "media item detail (versions loading)");
    let version = || fauna_media_machine::FileVersionSummary {
        version_num: 1,
        manifest_hash: "deadbeef".into(),
        size_bytes: 2048,
        created_at: 1_700_000_000_000,
        content_key_version: None,
        author_display: "field-laptop".into(),
        pruned: false,
        purge_after: None,
    };
    app.media.detail = Some(detail(Some(vec![version()])));
    dump_surface(app, "media item detail (one version)");
    for (label, arm) in [
        ("restore confirm armed", 0),
        ("external-open confirm armed", 1),
        ("delete confirm armed", 2),
    ] {
        let mut d = detail(Some(vec![version()]));
        match arm {
            0 => d.arming = Some(0),
            1 => d.external_arming = true,
            _ => d.delete_arming = true,
        }
        app.media.detail = Some(d);
        dump_surface(app, &format!("media item detail ({label})"));
    }
}

/// The wizard twin of [`sidebar_ring_to`]: put the app on one onboarding step,
/// with the launch surface owning the screen exactly as it does signed out.
///
/// **Why a driver and not another per-invariant loop.** The five invariants are
/// already surface-agnostic — every one of them reads the app through
/// [`crate::app::App::page_elements`], [`crate::app::App::focused_index`] and
/// [`crate::app::App::error_line_text`], and *all three of those already route
/// to the wizard* when [`crate::app::App::showing_launch_surface`] holds
/// (`app.rs`'s launch-surface arm returns `self.wizard.elements()`, and
/// `error_line_text` returns `self.wizard.error_text()`). So the wizard was
/// never blind for lack of a *checker* — it was blind for lack of a *visit*:
/// the sweeps above all start from `authed_app()`/`admin_app()`, which are
/// signed in, so no walk had ever put the app on a launch surface at all.
///
/// That makes the fix a fixture, not a fourth copy of the rule: one driver
/// hands the wizard to the *existing* [`check_all`], and every invariant — the
/// three that were still blind (I1, I4, I5), plus any invariant added later —
/// covers the fifteen onboarding screens for free, with no per-surface
/// branching anywhere in the checkers. The bolt-on `every_wizard_input_carries
/// _a_label` this replaced could only ever be I3.
fn wizard_step_to(step: fauna_onboarding_machine::OnboardingStep) -> App {
    let app = crate::app::tests::test_app();
    // Signed out, so the launch surface owns the screen and `launch` starts at
    // `Wizard` — the real state a first-run user is in, not an injected one.
    assert!(
        app.showing_launch_surface(),
        "the signed-out fixture must hand the screen to a launch surface"
    );
    app.wizard.machine.set_step_for_test(step);
    app
}

/// Exhaustive sweep 5 — **every onboarding wizard step, every focus position,
/// the whole invariant battery.** The unauthenticated twin of sweep 1.
///
/// The fifteen onboarding screens are the most-seen surface in the product and
/// had never been walked by any invariant. The cost of that was proven the
/// moment the first one was pointed at them: applying I3's rule to the wizard
/// found **six** inputs prompting the user with their raw element id — the very
/// first field of onboarding shipped painting `handle-input: _` — across
/// `handle-input`, `paste-secret-field`, `recovery-entry-phrase-field`,
/// `recovery-entry-account-field`, `invite-code-input` and `claim-code-input`
/// (found and fixed 2026-08-05).
///
/// This walks the ring the way sweep 1 does, so I1's focus-affordance
/// uniqueness, I2's two-focus-model agreement, I4's `*-nav-back` direction and
/// I5's one-of-N marking now all cover the wizard too.
/// Failures are collected across all fifteen steps and reported together: this
/// is a *class* sweep, and stopping at the first red turns closing the class
/// into fifteen rounds of whack-a-mole.
#[tokio::test]
async fn every_wizard_step_upholds_every_invariant_at_every_focus_position() {
    let mut failures = Vec::new();
    for step in ALL_WIZARD_STEPS {
        let mut app = wizard_step_to(step);
        if let Err(e) = walk_full_ring(&mut app, &format!("wizard {step:?}")) {
            failures.push(e);
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} wizard steps break an invariant:\n  - {}",
        failures.len(),
        ALL_WIZARD_STEPS.len(),
        failures.join("\n  - ")
    );
}

/// The wizard's inputs specifically — a named guard on the class sweep 5's I3
/// leg covers, kept because it also pins that the wizard still *renders* inputs
/// at all. A step list that silently stopped rendering would leave sweep 5
/// walking empty rings and passing vacuously.
#[test]
fn the_wizard_still_exposes_the_inputs_the_invariant_sweep_checks() {
    use crate::element::Role;

    let checked: usize = ALL_WIZARD_STEPS
        .iter()
        .map(|step| {
            wizard_step_to(*step)
                .page_elements()
                .iter()
                .filter(|e| matches!(e.role, Role::Input(_) | Role::InputCommit { .. }))
                .count()
        })
        .sum();
    assert!(
        checked >= 5,
        "expected the wizard to expose several inputs; saw {checked} — did the \
         step list stop rendering?"
    );
}

/// Every step the wizard's own exhaustive match renders (`Done` has no page —
/// the "Almost ready" outcome surface or the authed shell owns it). Shared by
/// the corpus dump and the wizard invariant above so the two cannot drift.
const ALL_WIZARD_STEPS: [fauna_onboarding_machine::OnboardingStep; 15] = {
    use fauna_onboarding_machine::OnboardingStep as S;
    [
        S::IdentityChoice,
        S::IdentityCreated,
        S::IdentityImport,
        S::RecoveryKit,
        S::RecoveryEntry,
        S::HandleEntry,
        S::InviteRequest,
        S::ClaimCode,
        S::NatModeChoice,
        S::DnsConfig,
        S::VpsConfig,
        S::NestProvisioning,
        S::DnsPostInstructions,
        S::NestRecovery,
        S::RecoverSelfhostedInstructions,
    ]
};

fn dump_onboarding_and_launch_surfaces() {
    use crate::app::tests::test_app;
    use crate::launch::LaunchSurface;
    use fauna_onboarding_machine::{DnsRecordPlain, OnboardingStep, WizardOutcome};

    let app = test_app();
    // Every step the wizard's own exhaustive match renders (`Done` has no page
    // — the "Almost ready" outcome surface or the authed shell owns it).
    for step in [
        OnboardingStep::IdentityChoice,
        OnboardingStep::IdentityCreated,
        OnboardingStep::IdentityImport,
        OnboardingStep::RecoveryKit,
        OnboardingStep::RecoveryEntry,
        OnboardingStep::HandleEntry,
        OnboardingStep::InviteRequest,
        OnboardingStep::ClaimCode,
        OnboardingStep::NatModeChoice,
        OnboardingStep::DnsConfig,
        OnboardingStep::VpsConfig,
        OnboardingStep::NestProvisioning,
        OnboardingStep::DnsPostInstructions,
        OnboardingStep::NestRecovery,
        OnboardingStep::RecoverSelfhostedInstructions,
    ] {
        app.wizard.machine.set_step_for_test(step);
        dump_screen(&app, &format!("onboarding {step:?}"));
    }
    app.wizard
        .machine
        .set_wizard_outcome_for_test(WizardOutcome::AwaitingManualDns {
            nest_url: "https://nest.example".into(),
            dns_records: vec![DnsRecordPlain {
                record_type: "A".into(),
                name: "@".into(),
                value: "203.0.113.7".into(),
                ttl: 300,
                priority: None,
            }],
            claim_code: "CLAIM-1234".into(),
        });
    dump_screen(&app, "onboarding Almost ready (AwaitingManualDns)");

    // The launch flow's own surfaces, each arm with representative data.
    let mut app = test_app();
    for (label, surface) in [
        ("launch Launching", LaunchSurface::Launching),
        (
            "launch TransientRetry (no recover boxes)",
            LaunchSurface::TransientRetry {
                error: "connection timed out".into(),
                recover_boxes: Vec::new(),
            },
        ),
        (
            "launch TransientRetry (one recover box)",
            LaunchSurface::TransientRetry {
                error: "connection timed out".into(),
                recover_boxes: vec!["nest.example".into()],
            },
        ),
        (
            "launch NeedsUpdate",
            LaunchSurface::NeedsUpdate {
                error: "this app is older than the nest allows".into(),
            },
        ),
        (
            "launch AccountLocked",
            LaunchSurface::AccountLocked {
                locked_until_secs: 1_800_000_000,
            },
        ),
        (
            "launch IdentityStolenEntry",
            LaunchSurface::IdentityStolenEntry {
                locked_until_secs: 1_800_000_000,
            },
        ),
        (
            "launch IdentityChanged",
            LaunchSurface::IdentityChanged {
                error: "the nest's identity changed".into(),
            },
        ),
        // The unreadable account index — all three states, because they paint
        // different elements: the version verdict offers nothing, and the
        // malformed one reveals its confirm only after the first press.
        (
            "launch AccountIndexUnreadable (a newer build wrote it)",
            LaunchSurface::AccountIndexUnreadable {
                refusal: fauna_launch_machine::AccountIndexRefusal::NewerBuild {
                    index_v: 2,
                    index_min: 2,
                    bin_v: 1,
                },
                confirming: false,
                error: None,
            },
        ),
        (
            "launch AccountIndexUnreadable (malformed)",
            LaunchSurface::AccountIndexUnreadable {
                refusal: fauna_launch_machine::AccountIndexRefusal::Malformed,
                confirming: false,
                error: None,
            },
        ),
        (
            "launch AccountIndexUnreadable (malformed, confirming)",
            LaunchSurface::AccountIndexUnreadable {
                refusal: fauna_launch_machine::AccountIndexRefusal::Malformed,
                confirming: true,
                error: None,
            },
        ),
        (
            "launch AccountIndexUnreadable (malformed, confirm refused — another window)",
            LaunchSurface::AccountIndexUnreadable {
                refusal: fauna_launch_machine::AccountIndexRefusal::Malformed,
                confirming: true,
                error: Some(
                    fauna_i18n::strings::onboarding::launch::INDEX_MALFORMED_RESET_BLOCKED_OTHER_WINDOW
                        .into(),
                ),
            },
        ),
        (
            "launch InstanceChooser",
            LaunchSurface::InstanceChooser {
                served_label: "alice@nest.example".into(),
                served_actor: "aa00".into(),
                choices: vec![("aa11".into(), "bob@nest.example".into())],
                error: None,
            },
        ),
        (
            "launch InstanceChooser (pick lost the race)",
            LaunchSurface::InstanceChooser {
                served_label: "alice@nest.example".into(),
                served_actor: "aa00".into(),
                choices: vec![("aa11".into(), "bob@nest.example".into())],
                error: Some("that account is now served by another instance".into()),
            },
        ),
        ("unlock Unlock", LaunchSurface::Unlock),
        ("unlock CreatePassphrase", LaunchSurface::CreatePassphrase),
    ] {
        app.launch = surface;
        dump_screen(&app, label);
    }
}

// ---------------------------------------------------------------------------
// Random walks — the frontier past the enumerable sweeps above. proptest
// shrinks a failing sequence to a minimal reproduction; failures persist in
// `proptest-regressions/` (commit that file if one ever appears).
// ---------------------------------------------------------------------------

/// One externally-drivable input, exactly as a human hand could produce it.
#[derive(Debug, Clone)]
enum Step {
    /// `Down` — ring forward (sidebar: next page; page zone: next focusable).
    FocusNext,
    /// `Up` — ring backward.
    FocusPrev,
    /// `Left` — hand the keyboard to the sidebar.
    ZoneSidebar,
    /// `Right` — step into the page pane.
    ZonePage,
    /// `Enter` — actuate whatever the ring sits on.
    Actuate,
    /// `Esc` — pop the innermost overlay / sub-page.
    Back,
    /// A terminal-mouse click on sidebar row N (out of range = no-op by design).
    ClickSidebar(usize),
    /// A terminal-mouse click on page element N (out of range / a label = no-op).
    ClickPage(usize),
}

fn apply_step(app: &mut App, step: &Step) {
    match step {
        Step::FocusNext => app.handle_key(key(KeyCode::Down)),
        Step::FocusPrev => app.handle_key(key(KeyCode::Up)),
        Step::ZoneSidebar => app.handle_key(key(KeyCode::Left)),
        Step::ZonePage => app.handle_key(key(KeyCode::Right)),
        Step::Actuate => app.handle_key(key(KeyCode::Enter)),
        Step::Back => app.handle_key(key(KeyCode::Esc)),
        Step::ClickSidebar(i) => app.click_sidebar(*i),
        Step::ClickPage(i) => app.click_page_element(*i),
    }
}

fn step_strategy() -> impl proptest::strategy::Strategy<Value = Step> {
    use proptest::prelude::*;
    prop_oneof![
        Just(Step::FocusNext),
        Just(Step::FocusPrev),
        Just(Step::ZoneSidebar),
        Just(Step::ZonePage),
        Just(Step::Actuate),
        Just(Step::Back),
        (0usize..24).prop_map(Step::ClickSidebar),
        (0usize..48).prop_map(Step::ClickPage),
    ]
}

fn run_walk_from(mut app: App, steps: &[Step]) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        apply_step(&mut app, step);
        check_all(&app, &format!("after step {i} {step:?} of {steps:?}"))?;
    }
    Ok(())
}

/// Run `steps` on a current-thread runtime (the walk actuates real gestures,
/// whose spawn half needs a runtime; spawned ops dial a closed port and report
/// into a dropped channel) and assert the invariants held at every state.
fn assert_walk_holds(app: App, steps: &[Step]) -> Result<(), proptest::test_runner::TestCaseError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let outcome = rt.block_on(async { run_walk_from(app, steps) });
    proptest::prop_assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 64,
        ..Default::default()
    })]

    /// Random multi-step walks over the whole drivable input surface uphold
    /// the global paint/focus invariants at every intermediate state.
    #[test]
    fn random_walks_uphold_the_global_focus_invariants(
        steps in proptest::collection::vec(step_strategy(), 1..40)
    ) {
        assert_walk_holds(authed_app(), &steps)?;
    }

    /// The same walks with the gated Admin/Family rows visible — the extra
    /// sidebar rows shift every ring index, and the admin shell's 13-row rail
    /// is otherwise unreachable by any random walk.
    #[test]
    fn random_walks_uphold_the_invariants_with_the_gated_rows_visible(
        steps in proptest::collection::vec(step_strategy(), 1..40)
    ) {
        assert_walk_holds(admin_app(), &steps)?;
    }
}
