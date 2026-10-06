"""Admin navigation model — the distinct admin shell + the uniform
`admin-nav-back` "leave admin" affordance (admin.md § Navigation model,
ratified 2026-06-01).

`admin-nav-back` is the single way to exit the admin shell back to the
non-admin app; it is present on every admin page (in the shell, not as an
inter-admin-page jump). It lands on the **primary view** (Conversations / the
main view), the same landing as `settings-nav-back` — corrected 2026-06-07
(user): admin is a top-level nav peer, not nested under Settings, so exiting it
no longer chains through the Settings shell. Reference implementation: linux
(-> primary view); web lifted it 2026-06-08; windows lifted it 2026-06-08
(MainPage.LeaveAdmin → the shared NavigateToPrimaryView, same landing as
LeaveSettings); macos lifted it 2026-06-08 (AdminNavRail → Conversations). ios
still owes its shell (admin.md § Implementation status — navigation).

Client scoping is **per test** (not a single module-level marker): the
shell-exit tests below are shared by every app that renders the shell
(web/linux/windows/macos); the primary-view-landing test is marked for the
apps that have the corrected landing (linux + web + windows + macos today).
As the remaining clients lift, add their `pytest.mark.<client>` to that test.
"""

import time

import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]
# macos built the shell + admin-nav-back 2026-06-08 (a macos admin-users follow-up) and
# the three shell-exit tests below are green --client macos (2026-06-08), so they
# carry @pytest.mark.macos. macos lands on the primary view too (the 2026-06-07
# correction), so it's on the lands-on-primary test as well. iOS confirmed green
# 2026-06-17 (in-process driver) — the shared FaunaKit
# admin shell + admin-nav-back render the same on iOS, so all three carry ios too.
# android: the shell itself (state-protocol entry via NavRouteResolver, the
# admin-dashboard-heading, and now admin-nav-back — added to
# AdminDashboardScreen.kt, the sole admin screen missing the tag every other
# admin screen already carries) all check out, so the three shell-exit tests
# carry android too. The admin-tab visibility tests below do NOT: android
# nests admin under Settings like web/ios, not as a primary-nav tab, so they
# need the same navigate-to-settings variant web/ios are still owed.


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("admin-dashboard")
def test_admin_nav_back_present(admin_app):
    """Every admin shell exposes the `admin-nav-back` button."""
    admin_app.admin.navigate_dashboard()
    assert admin_app.admin.is_dashboard_visible(), (
        f"admin dashboard did not load. error: {admin_app.error_text()!r}"
    )
    assert admin_app.admin.nav_back_present(), (
        f"admin-nav-back missing from the admin shell (admin.md § Navigation "
        f"model requires it on every admin page). error: {admin_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("admin-dashboard")
def test_admin_nav_back_exits_shell(admin_app):
    """Clicking `admin-nav-back` leaves the admin shell for the non-admin app."""
    admin_app.admin.navigate_dashboard()
    assert admin_app.admin.is_dashboard_visible()
    admin_app.admin.leave_admin()
    # Leaving the shell tears the admin dashboard heading out of the tree. The
    # settle loop and the assertion below share ONE predicate (`is_absent`): a
    # loop on `is_visible` would exit the instant the heading was merely
    # offscreen, and the count-based assertion would then read a heading that is
    # still mid-teardown.
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and not admin_app.driver.is_absent(
        "admin-dashboard-heading"
    ):
        time.sleep(0.2)
    assert admin_app.driver.is_absent("admin-dashboard-heading"), (
        "admin-nav-back did not leave the admin shell — admin-dashboard-heading "
        "is still visible after clicking it."
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("admin-dashboard")
def test_admin_nav_back_lands_on_primary_view(admin_app):
    """`admin-nav-back` returns to the primary view (Conversations), not the
    Settings shell (admin.md § Navigation model — corrected 2026-06-07).

    linux is the reference; web lifted it 2026-06-08 (`admin/+layout.svelte`
    nav-back `href` → `/app/conversations`, mirroring `settings-nav-back`).
    windows lifted it 2026-06-08 (`MainPage.LeaveAdmin` → the shared
    `NavigateToPrimaryView`, the same exit path as `LeaveSettings`).
    """
    admin_app.admin.navigate_dashboard()
    assert admin_app.admin.is_dashboard_visible()
    admin_app.admin.leave_admin()
    # The primary view (Conversations) carries `new-conversation-button`; the
    # Settings shell carries `settings-nav-back`. Landing on the primary view
    # means the former appears and the latter does not.
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and not admin_app.driver.is_visible(
        "new-conversation-button"
    ):
        time.sleep(0.2)
    assert admin_app.driver.is_visible("new-conversation-button"), (
        "admin-nav-back did not land on the primary view — "
        "new-conversation-button (Conversations) is not visible."
    )
    assert admin_app.driver.is_absent("settings-nav-back"), (
        "admin-nav-back landed on the Settings shell (settings-nav-back visible) "
        "instead of the primary view (admin.md § Navigation model, 2026-06-07)."
    )


# ── The gated admin entry's VISIBILITY (the am_i_admin gate itself) ──────────
#
# Every other admin test navigates DIRECTLY into the admin shell via the state
# protocol (`navigate_to("admin")` / `set_state(nav=admin)`), which on several
# clients force-reveals the admin nav as a side effect (windows
# NavigateToAdminSubPage -> ShowAdminNavItems) — so none of them exercise the
# client-side admin-status gate that actually controls whether a user SEES the
# Admin entry. These two pin that `admin-tab` tracks `fauna.account.am_i_admin`:
# shown to an admin (on the primary view, NOT inside the shell), hidden from a
# regular user. Regression guard for the windows bug where the gate probed the
# deleted GET /admin/api/stats, threw, and fail-closed-hid the menu from the
# admin.
#
# ⚠ The admin-entry NAV MODEL diverges across clients, so these are NOT uniformly
# cross-app:
#   • windows / linux put a gated `admin-tab` on the MAIN nav (ui.yaml
#     visible: when_admin) — the gate runs on the primary view (windows
#     MainPage.CheckAdminStatusAsync). The positive test fits here.
#   • web reaches admin via Settings: `checkIsAdmin` runs only in the settings /
#     admin layouts (lib/api.ts; routes/settings + routes/admin/+layout.svelte),
#     NEVER on the feed layout, so `admin-tab` is never on the web primary view.
#     A web positive test would need a navigate-to-settings variant (TODO).
# Hence: positive = windows (linux shares the model — add its marker once
# verified against the linux app); negative = web + windows (a non-admin sees no admin-tab on
# the primary view on both).
#
# macos: the am-i-admin gate IS implemented (2026-06-16, an apple admin-tab-gate follow-up
#   — the Admin sidebar row renders only when MacAppState.isAdmin, fail-closed) and
#   admin-tab is on the MAIN nav (sidebar) like windows/linux. The 2026-06-17 sidebar
#   `*-tab` registration gap is now FIXED (NavigationSplitView
#   columnVisibility=.all under FaunaE2E.isActive + .automationActivate on the standard
#   rows): the harness confirmed test_element_diag::test_sidebar_elements[macos] GREEN, 6/6
#   STANDARD tabs (feed/conversations/contacts/events/media/settings) now register in-process
#   (groups-tab is correctly absent — folded into unified conversations).
#   Was NOT macos-markable through 2026-06-18 — the root cause moved DEEPER across the hand-backs
#   below; RESOLVED 2026-06-21 (see the closing note):
#   • The 2026-06-18 `.task` race IS FIXED (ContentView.swift:226
#     is now `.task(id: client != nil)`, mirroring AdminShellView; re-fires when the client wires).
#   • Registration is also CORRECT: the gated row (SidebarView.swift:46-47) carries BOTH
#     `.accessibilityIdentifier("admin-tab")` AND `.automationActivate("admin-tab")`, identical to
#     the standard rows — so it WOULD register the instant it inserts.
#   • Yet `test_admin_tab_visible_for_admin[macos]` STILL fails: admin-tab count=0 / is_visible False,
#     error_text() empty; settings-tab (a standard row) is visible. count=0 with the registration
#     modifier present ⇒ the row never INSERTS ⇒ `appState.isAdmin` stays False (confirmed by a
#     tree()+state probe, 2026-06-18).
#   • Since the SAME set_state session-login wires a working client (logged_in_app renders the feed
#     via client reads), the keyed `.task` DOES fire with a non-nil client and runs
#     refreshAdminStatus() (ContentView.swift:239) → `(try? await client.api.amIAdmin()) ?? false`.
#     So the remaining gap is `amIAdmin()` returning/throwing false IN-PROCESS for a set_state admin
#     session — NOT the `.task` keying and NOT row registration. Consistent with admin_app
#     (force-nav) + admin DATA pages passing on macos: those load admin pages without depending on a
#     successful amIAdmin() round-trip — flagged for apple follow-up.
#   • RESOLVED 2026-06-21 (prime APIClient secret at FaunaClient.init):
#     amIAdmin() now resolves true in-process for a set_state admin session, so the admin row inserts.
#     BOTH tests are now macos-marked + GREEN — the positive SEES admin-tab; the negative hides it
#     NON-vacuously (the positive proves admin-tab CAN appear). Confirmed
#     2026-06-21 (test_admin_nav.py 5/5 --client macos).
# ios: the gate is implemented too (in-Settings "Nest Admin" Button gated on
#   AppState.isAdmin), but — like web — admin-tab is reached via Settings, never on
#   the primary view, so these primary-view tests do NOT fit ios; it needs a
#   navigate-to-settings variant (the same TODO web owes).


@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.feature("admin-dashboard")
def test_admin_tab_visible_for_admin(admin_main_app):
    """An admin on the primary view SEES the gated `admin-tab` — revealed by the
    client's `am_i_admin` gate, with no admin-shell navigation forcing it.

    windows/macos/tui (main-nav gated admin-tab; linux not yet marker-verified —
    see the module comment above); web gates the admin entry in Settings, not the
    feed view. tui puts a gated `admin-tab` sidebar row on the primary view like
    windows/linux/macos, revealed by the shared `fauna.account.am_i_admin` gate
    (fail-closed) — `apps/fauna-tui/src/admin.rs` + `App::sidebar_pages`."""
    # `is_nav_tab_revealed`, not a bare `is_visible`: the gated admin row is the LAST
    # entry in the primary nav, so on a pane taller than the window it renders
    # correctly but below the fold (measured on windows: it is present in the UIA tree
    # with count=1, and only its rect is 0x0/offscreen until scrolled). A bare
    # is_visible reads that as "the gate never revealed it" — the exact misreading that
    # kept this test red across three sessions. The reveal-is-tree-membership check
    # avoids scrolling the below-fold row (a UIA nav-pane scroll hung the FlaUI bridge): windows answers it hang-free via count()>=1 (admin-tab is
    # Collapsed-until-revealed), other apps keep the scroll+is_visible behaviour.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not admin_main_app.driver.is_nav_tab_revealed("admin-tab"):
        time.sleep(0.3)
    assert admin_main_app.driver.is_nav_tab_revealed("admin-tab"), (
        "admin-tab not visible to an admin on the primary view — the am_i_admin "
        f"gate failed to reveal it. diagnose: {admin_main_app.driver.diagnose('admin-tab')} "
        f"error: {admin_main_app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.feature("admin-dashboard")
def test_admin_tab_hidden_for_non_admin(logged_in_app):
    """A regular (non-admin) user must NOT see the gated `admin-tab` on the primary
    view (the gate is fail-closed). Counterpart to test_admin_tab_visible_for_admin."""
    # Let the async admin-status gate settle before asserting it did NOT reveal it.
    time.sleep(3.0)
    # Symmetric with the positive above: `is_nav_tab_revealed` asserts the row is
    # genuinely absent (not merely below the fold). On windows a non-revealed gated
    # tab is Collapsed → absent from the UIA tree → count()==0, so this reads False
    # exactly as required — and without the below-fold scroll that hangs the bridge.
    assert not logged_in_app.driver.is_nav_tab_revealed("admin-tab"), (
        "admin-tab visible to a non-admin user — the am_i_admin gate should hide it."
    )
