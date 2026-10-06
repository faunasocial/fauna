"""W4 (account-data-plane.md § Workstreams) phase 4's UI desensitizing, seen from outside the app.

``docs/goal/architecture/account-data-plane.md`` § The offline-mutation contract
→ *How a surface asks*: a control that issues an ``OnlineOnly`` wire kind must
desensitize while there is no nest, and say why **beside itself** (never a global
"you are offline" banner — § R11). Classes 1 and 2 are precisely the ones that
work without a nest, so an offline-capable control beside it must stay live.

**Why the pairing is the test, not the disabled assert alone.** A gate that
disables *everything* offline passes a one-sided check and is badly wrong: it
would grey the offline-safe controls the contract exists to keep usable. So each
case reads a gated control and a live sibling **in the same modal, at the same
moment** — the assertion is the *difference* between them, which no blanket
disable and no blanket no-op can satisfy.

tier_3: a real nest binary, taken down mid-session so the client's own
connection-state subscription drives the gate. Latency-independent per
convention 14 — every wait is a named generous budget with a deadline poll (green
runs pay nothing), never a settle-sleep, and the assertions are on *state*, not
on how fast the transition arrived.

**App scope.** The gate seam is per-app: tui gates structurally inside
``App::page_elements``, linux registers per widget, and apple applies one
``.faunaGate(kind)`` modifier. This file asserts the *behaviour* all of them owe,
but is marked only for the apps that have gated **these specific controls** —
add a marker as each app gates its Backups page, rather than letting a test that
cannot pass yet report as coverage (convention 7: a skip is not coverage).
"""

from __future__ import annotations

import time

import pytest

from common.auth import register_user
from common.nest import start_nest_in_place, stop_nest
from helpers import registry_audit
from helpers.app_surface import skip_unbuilt
from helpers.budgets import UI_SETTLE_S

# NOT `standalone_only` any more (2026-08-29), for the same reason as
# `test_nest_flip_resilience.py`: `start_nest_in_place` now asks the nest's own
# provider to bring it back, and docker answers with `docker start` on the same
# container. Taking the nest away and giving it back is precisely what this module
# is about, so the deployed artifact is the more honest place to witness it.
#
# The two tests that take a `second_nest` stay standalone-only — but by INFERENCE,
# not by a marker: that fixture requests `nest_binary`, so the closure classifier
# excludes them itself (testing.md § Default app and nest mode). Declaring it here
# would have taken the other two down with them.
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# Generous budgets, deliberately far above any non-pathological transition on a
# loaded box: a green run costs only the poll that succeeds, and a slow machine
# never turns into a red (convention 14 — the `test_nest_flip_resilience`
# template these mirror).
DISCONNECT_WAIT_S = 90.0
CONNECT_WAIT_S = 120.0
GATE_WAIT_S = 60.0

CONNECTED = "Connected"


def _connection_status(app) -> str | None:
    """The global indicator's text, or ``None`` where it isn't rendered."""
    if not app.is_visible("connection-status"):
        return None
    return app.driver.get_text("connection-status")


def _require_connection_status(app) -> str:
    """The indicator's text, or a DECLARED skip where this app's shell does not
    paint it (convention 7: a bare skip reads as coverage, a declared one is
    counted and, under ``--strict-app``, fails).

    ``ui.yaml`` lists ``connection-status`` as implemented on all 7 apps, so its
    absence on a page an app reaches is parity debt, never a fact about the
    platform. Found on linux, whose admin shell paints none: the admin-plane
    journey below skipped there behind a message that named no surface.
    """
    text = _connection_status(app)
    if text is None:
        skip_unbuilt(
            app.driver,
            surface="the connection-status indicator on this page's shell",
            detail=(
                "these journeys read the nest connection's state off the global "
                "indicator, and this app's shell for the page under test paints "
                "none, though ui.yaml lists it as implemented on all 7 apps"
            ),
            tracked="transport-connection.md § Connection-status indicator (app UI)",
        )
    return text


def _wait_for_status(app, *, connected: bool, timeout: float) -> str | None:
    """Poll the indicator until it reads Connected (or stops reading Connected).

    Returns the last text seen. Polls rather than sleeps so a fast transition
    costs one read.
    """
    deadline = time.monotonic() + timeout
    last = _connection_status(app)
    while time.monotonic() < deadline:
        last = _connection_status(app)
        if last is None:
            return None
        if (last == CONNECTED) == connected:
            return last
        time.sleep(0.3)
    return last


def _wait_until_disabled(app, element_id: str, timeout: float = GATE_WAIT_S) -> bool:
    """Poll until `element_id` reads disabled. The gate re-evaluates from the
    connection-state subscription, which lands a moment after the indicator
    flips, so this is a state wait — not a timing assertion."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not app.driver.is_enabled(element_id):
            return True
        time.sleep(0.3)
    return False


def _enroll_one_destination(app, second_nest, test_user):
    """Give the page a destination to act on, WHILE the nest is still up.

    Enrolling is itself online-only, so it has to happen before the outage this
    file induces — the gate under test is about the *next* action, not this one.
    """
    app.backups.require_destination_management_supported()
    # The owner must be authorized on the destination nest for the enroll
    # handshake. Idempotent across re-runs of the session-scoped nest, the same
    # shape `test_backups.py` uses.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass
    app.backups.navigate()
    # `nest_instance`/`test_user` are session-scoped, so a sibling test in this
    # file may already have enrolled one — any destination will do, this file
    # never asserts on their contents. Adding unconditionally would race the
    # row-count wait against a set that started non-empty.
    if app.backups.destination_count() == 0:
        app.backups.add_destination(second_nest["url"], name="Gate-proof")
        app.backups.wait_for_destination_count(1)


def _dismiss_remove_modal(app):
    """Close the remove confirmation, whatever state the test left it in.

    Registered as a finalizer by every test that arms the modal, so it runs even
    when the test fails partway — the same lesson the nest restore below already
    learned, rediscovered for the modal by the red-verify pass on windows
    2026-08-24. Leaving it open leaks into the siblings sharing these
    session-scoped fixtures, and on a toolkit where a dialog is a singleton it
    breaks them outright: WinUI refuses the *next* ``ShowAsync`` ("only a single
    ContentDialog can be open") inside an ``async void`` that swallows the throw,
    so the sibling fails with an invisible confirm button and an EMPTY error
    string — which reads like a gate fault and is not one.

    Cancel is the right dismissal: it is the one control this file guarantees is
    live in every state it induces. Best-effort — a test that already failed must
    not have its own diagnosis replaced by a teardown error.

    The "is it open?" read is ``count`` plus ``is_visible_scrolled``, never a bare
    ``is_visible``: windows' ``is_visible`` is UIA ``IsOffscreen``, which reads an
    open-but-unarranged dialog button as False — the same false negative that hid
    gaps 1 and 2 of the row this fix closes — so the old form silently left the
    modal open for the next sibling. After the click it waits for the confirm
    button to disappear, so a dismissal that did not take is retried rather than
    assumed.
    """
    confirm = "backup-destination-remove-confirm-button"
    cancel = "backup-destination-remove-cancel-button"
    deadline = time.monotonic() + UI_SETTLE_S
    while True:
        try:
            if app.driver.count(confirm) == 0:
                return
            if app.driver.is_visible_scrolled(cancel):
                app.driver.click(cancel)
        except Exception:
            pass
        if time.monotonic() >= deadline:
            return
        time.sleep(0.3)


def _open_remove_modal(app):
    """Arm the remove confirmation for the first configured destination.

    The remove modal is the cleanest pairing on this page: its confirm button
    issues `fauna.backup.destination.remove` (online-only — the teardown runs at
    the destination nest *and* the source nest's registry), while its cancel
    button is pure local UI that must stay live with no nest at all.
    """
    app.driver.click("backup-destination-remove-button")
    assert app.driver.is_visible("backup-destination-remove-confirm-button"), (
        f"remove confirmation did not open; error={app.error_text()!r}"
    )


# linux joined: this is the REFERENCE offline-gate
# implementation the others port from (offline_gate.rs's own module doc:
# "the seam here is a registry" — windows' OfflineGate.cs is an explicit
# port of it) but the test file itself never carried linux's marker.
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
# tui joined 2026-08-21 (the tui-excluding-marker audit): the gate is
# fully structural (`App::apply_offline_gate`, applied to every element in
# `App::page_elements`) — a page author declares nothing beyond the gesture's
# `wire_kind()`, and `Action::ConfirmRemove -> fauna.backup.destination.remove`
# (OnlineOnly) is already declared + pinned by
# `backups_declares_the_exact_kind_per_gesture`. Nothing was missing.
@pytest.mark.tui
# windows joined 2026-08-24: the registry
# is a port of linux's (`FaunaApp.Core/Services/OfflineGate.cs`), and
# `backup-destination-remove-confirm-button` declares
# `fauna.backup.destination.remove` at `BackupsPage.xaml.cs`'s constructor. The
# reason renders as an inline caption beside the control rather than a tooltip —
# windows' own disabled-with-a-reason idiom — and is not asserted here for the
# same reason apple's is not: it is un-id'd chrome, invisible to a driver that
# resolves by AutomationId. Its decision is pinned in `OfflineGateTests`.
@pytest.mark.windows
@pytest.mark.feature("offline-aware-controls")
def test_online_only_control_desensitizes_with_no_nest_and_says_why(
    logged_in_app, nest_instance, second_nest, test_user, request
):
    """The gate's positive direction: nest down ⇒ the online-only control is
    disabled; nest back ⇒ it is live again.

    Both directions matter: a gate that only closes would strand the control
    after any offline blip, which is worse than no gate at all.
    """
    app = logged_in_app
    _require_connection_status(app)

    assert _wait_for_status(app, connected=True, timeout=CONNECT_WAIT_S) == CONNECTED, (
        "should be connected after login"
    )

    _enroll_one_destination(app, second_nest, test_user)
    _open_remove_modal(app)
    request.addfinalizer(lambda: _dismiss_remove_modal(app))
    assert app.driver.is_enabled("backup-destination-remove-confirm-button"), (
        "an online-only control must be live while the nest IS reachable — "
        "gating it when connected would be the gate over-claiming"
    )

    # --- Nest DOWN, and it stays down: the client's reconnect loop has nothing
    #     to reach, so the gate must see a non-connected state. ---
    #
    # Everything past this point runs under `try/finally`: a failure here used to
    # leave the nest DOWN for the rest of the module (the fixture is
    # session-scoped), so the next test failed on a connect precondition and
    # reported a second, misleading red. Restoring the nest is this test's own
    # cleanup, not the next test's problem — found by the red-verify pass.
    stop_nest(nest_instance, graceful=True)
    try:
        flipped = _wait_for_status(app, connected=False, timeout=DISCONNECT_WAIT_S)
        assert flipped != CONNECTED, (
            f"connection-status stayed {CONNECTED!r} with the nest down (read "
            f"{flipped!r}) — the gate reads this same subscription, so nothing "
            "downstream can be trusted until the indicator flips"
        )

        assert _wait_until_disabled(app, "backup-destination-remove-confirm-button"), (
            "an OnlineOnly control stayed ENABLED with no nest — W4 phase 4 requires "
            "it desensitize (account-data-plane.md § The offline-mutation contract). "
            f"error={app.error_text()!r}"
        )
    finally:
        # --- Nest BACK on the same port + data dir, whatever happened above. ---
        start_nest_in_place(nest_instance)
    # NOT asserted here: that the reason is *visibly* rendered — but the reason
    # for that changed on 2026-08-23 and the old one was wrong, so do not
    # reinstate it. This used to say apple carried the reason only on the
    # control "because a new element id is a ui.yaml change needing approval".
    # android refuted that premise (`account-data-plane.md` § Implementation
    # status → the android leg): the blocker was the reason's *addressability*,
    # not its visibility, and un-id'd chrome needs no id. apple now renders it
    # visibly too — `FaunaOfflineReason`, and this very control (the backup
    # destination remove confirm) is its first call site.
    #
    # It stays unasserted HERE because apple's driver resolves elements through
    # `AutomationRegistry`, which holds registered ids only, so un-id'd chrome
    # is invisible to it — the one axis on which the android finding does not
    # transfer. That does NOT leave the mechanism untested: the caption's whole
    # decision is a pure function pinned in FaunaKit's
    # `OfflineReasonCaptionTests` (captioned iff gated, for every connection
    # word), and `check-offline-gate-kinds.py`'s rule 5 statically enforces that
    # a caption's kind is one some control actually gates. What is left for a
    # driver-level assert is only "is that Text on screen", which needs an
    # element id and therefore rule-A approval.

    # The gate must reopen with no manual action, or an offline blip would
    # strand the control for good.
    assert _wait_for_status(app, connected=True, timeout=CONNECT_WAIT_S) == CONNECTED
    deadline = time.monotonic() + GATE_WAIT_S
    while time.monotonic() < deadline:
        if app.driver.is_enabled("backup-destination-remove-confirm-button"):
            break
        time.sleep(0.3)
    assert app.driver.is_enabled("backup-destination-remove-confirm-button"), (
        "the control did not come back when the nest did — a gate that only "
        "closes is a stranding, not a gate"
    )


# linux joined: this is the REFERENCE offline-gate
# implementation the others port from (offline_gate.rs's own module doc:
# "the seam here is a registry" — windows' OfflineGate.cs is an explicit
# port of it) but the test file itself never carried linux's marker.
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
# windows joined 2026-08-30: the FlaUI bridge now serves `GET /registry`
# (`flaui-bridge/Actions.cs::RegistrySnapshot`), so `WindowsBridgeDriver.
# registry_snapshot()` (inherited from `HttpBridgeDriver`) answers a real frame
# instead of `None` — this was the one thing keeping windows off this case
# (account-data-plane.md § Built — the windows leg's seam).
@pytest.mark.windows
@pytest.mark.feature("offline-aware-controls")
def test_the_admin_plane_desensitizes_with_no_nest(admin_app, nest_instance):
    """The fan-out, on the plane where almost everything is online-only.

    "Admin/provisioning" is the charter's own example of class 3, and the tui
    sweep bore it out: the deployment-mutating half of every admin action is
    ``OnlineOnly``. This asserts the apple fan-out on the pairing toggle, whose
    write is one ``fauna.admin.services.update`` row.

    **The pairing is unusually clean here, which is why this page was picked.**
    ``admin-service-pairing-toggle`` and ``admin-factory-reset-button`` render
    unconditionally (no loaded snapshot to lose when the nest goes), sit on the
    same page, and carry the *identical* own predicate (``!vm.isBusy``). The
    only difference between them is the gate: the toggle declares a kind,
    arming the reset confirm is local and declares none. So a blanket disable
    fails on the reset button, a no-op gate fails on the toggle, and neither can
    be satisfied by the page's own state.
    """
    app = admin_app
    _require_connection_status(app)

    assert _wait_for_status(app, connected=True, timeout=CONNECT_WAIT_S) == CONNECTED
    app.admin.navigate_to_pairing_control()
    assert app.driver.count("admin-service-pairing-toggle") > 0, (
        f"admin-nest did not render the pairing toggle; error={app.error_text()!r}"
    )
    assert app.driver.count("admin-factory-reset-button") > 0, (
        "the live sibling is missing, so a disabled-only assertion below would "
        f"prove nothing; error={app.error_text()!r}"
    )
    assert app.driver.is_enabled("admin-service-pairing-toggle"), (
        "an online-only control must be live while the nest IS reachable"
    )
    # The WHOLE-FRAME half (convention 17). The two hand-picked controls below
    # prove the gate's ruling on the two cases this page was chosen for; this
    # captures the same frame in full so the assertion after the disconnect can
    # be about every control on the page rather than about two of them.
    online_frame = app.driver.registry_snapshot()

    stop_nest(nest_instance, graceful=True)
    try:
        assert _wait_for_status(app, connected=False, timeout=DISCONNECT_WAIT_S) != CONNECTED
        assert _wait_until_disabled(app, "admin-service-pairing-toggle"), (
            "the admin pairing toggle stayed ENABLED with no nest — its write is "
            "`fauna.admin.services.update`, which is OnlineOnly. "
            f"error={app.error_text()!r}"
        )
        assert app.driver.is_enabled("admin-factory-reset-button"), (
            "arming the factory-reset confirm is LOCAL (the reset itself is the "
            "gated call, on the confirm button inside) — greying the opener is "
            "the gate over-claiming, and it is what a blanket disable would do"
        )
        # Read AFTER the two per-control assertions, so the frame is the settled
        # one they just proved things about rather than a racing intermediate.
        # `registry_audit` refuses a `None` frame rather than passing it, so a
        # build whose `/registry` route went missing reds here instead of
        # reporting a clean sweep of a surface nobody read. web joined
        # 2026-08-22 (`web-bridge/server.py`'s own `/registry` route) — every app this test runs on now has one.
        registry_audit.assert_offline_gate_reach(
            online_frame,
            app.driver.registry_snapshot(),
            surface="admin-nest (pairing control, nest down)",
        )
    finally:
        start_nest_in_place(nest_instance)


# linux joined: this is the REFERENCE offline-gate
# implementation the others port from (offline_gate.rs's own module doc:
# "the seam here is a registry" — windows' OfflineGate.cs is an explicit
# port of it) but the test file itself never carried linux's marker.
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
# tui joined 2026-08-21 (the tui-excluding-marker audit): same structural
# gate as the destination-remove case above — `Action::NestsSubmitAdd ->
# fauna.pair.add` (OnlineOnly) is declared, `NestsShowAddForm`/`NestsCancelAdd`
# declare no wire kind (pure local), so the gate leaves them live by
# construction. Nothing was missing.
@pytest.mark.tui
# windows joined 2026-08-24: `nests-add-submit-button` declares
# `fauna.pair.add` in `NestsPanel`'s constructor, while `nests-add-button` and
# `nests-add-cancel-button` declare nothing and stay live by construction. NB
# windows' submit carries NO predicate of its own (no `IsEnabled` writer at all),
# so the typed input this test performs is a no-op here rather than the
# precondition it is on tui — the gate is the only thing that can disable it,
# which is exactly what the disabled assertion wants.
@pytest.mark.windows
# android joined: LinkedNestsScreen.kt + LinkedNestsVM.kt
# carry exact Ids.NESTS_ADD_* matches, no backups/`/registry` dependency
# (unlike this file's other three tests — checked and left unmarked).
@pytest.mark.android
@pytest.mark.feature("offline-aware-controls")
def test_a_user_facing_page_desensitizes_with_no_nest(logged_in_app, nest_instance):
    """The fan-out on the OTHER plane — a page an ordinary user reaches.

    The admin case above proves the gate on the plane where "almost everything is
    online-only" is the expected answer. This proves it where that prior does not
    apply at all: Nests is a settings page, and most of what a user does there
    (the Now/History lens, revealing the add form, dismissing it) needs no nest.
    Only the submit does — ``LinkedNestsAction::Link`` → ``fauna.pair.add``.

    **The input is typed on purpose, and it is the sharp part of this test.**
    ``nests-add-submit-button`` carries its own predicate (a non-empty nest id),
    so an untyped form would leave it disabled for a reason that has nothing to do
    with the gate — and the disabled assertion below would pass against an app
    with no gate at all. Typing first satisfies the call site's own predicate, so
    the gate is the only thing left that can disable it. That is also a live
    assertion on the seam's automation fold: the registry entry reports
    ``gate && the call site's own predicate``, so this reads one answer, not two
    that happen to agree.
    """
    app = logged_in_app
    _require_connection_status(app)

    assert _wait_for_status(app, connected=True, timeout=CONNECT_WAIT_S) == CONNECTED
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"the Nests page did not render; error={app.error_text()!r}"
    )

    app.driver.click("nests-add-button")
    # The add form is an inline reveal — client-local state, no nest round-trip.
    app.driver.wait_for("nests-add-input", timeout=UI_SETTLE_S)
    # Any syntactically plausible nest id: this test never submits, so the value
    # only has to clear the button's own non-empty predicate.
    app.driver.clear_and_type("nests-add-input", "ab" * 32)
    assert app.driver.is_enabled("nests-add-submit-button"), (
        "an online-only control must be live while the nest IS reachable — if it "
        "is already disabled here, the outage assertion below proves nothing. "
        f"error={app.error_text()!r}"
    )

    stop_nest(nest_instance, graceful=True)
    try:
        assert _wait_for_status(app, connected=False, timeout=DISCONNECT_WAIT_S) != CONNECTED
        assert _wait_until_disabled(app, "nests-add-submit-button"), (
            "the add-a-nest submit stayed ENABLED with no nest — it dispatches "
            "`fauna.pair.add`, which is OnlineOnly. "
            f"error={app.error_text()!r}"
        )
        assert app.driver.is_enabled("nests-add-cancel-button"), (
            "dismissing the add form needs no nest; greying it strands the user "
            "in a form they cannot leave — the over-claim rulings 1-3 forbid"
        )
        assert app.driver.is_enabled("nests-add-button"), (
            "arming the add form is LOCAL (the pairing write is on the submit "
            "inside) — greying the opener is what a blanket disable would do"
        )
    finally:
        start_nest_in_place(nest_instance)


# linux joined: this is the REFERENCE offline-gate
# implementation the others port from (offline_gate.rs's own module doc:
# "the seam here is a registry" — windows' OfflineGate.cs is an explicit
# port of it) but the test file itself never carried linux's marker.
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
# tui joined 2026-08-21 (the tui-excluding-marker audit): the negative
# direction of the same structural gate — `Action::CancelRemove` declares no
# wire kind, so `backup-destination-remove-cancel-button` stays live by
# construction beside the gated confirm button.
@pytest.mark.tui
# windows joined 2026-08-24: the negative direction on windows is NOT
# structural the way tui's is — the registry only ever touches controls a page
# declared, so `backup-destination-remove-cancel-button` (which declares nothing)
# stays live because the gate never reaches it. That makes this the case that
# would catch a windows gate written as a blanket page-level disable.
@pytest.mark.windows
@pytest.mark.feature("offline-aware-controls")
def test_an_offline_capable_sibling_stays_live_beside_the_gated_one(
    logged_in_app, nest_instance, second_nest, test_user, request
):
    """The gate's negative direction, and the one a blanket disable fails.

    Cancel is local UI — it issues no wire kind at all — so it must stay live
    with no nest, *in the same modal, at the same moment* as the gated confirm
    button. Classes 1 and 2 are exactly what works without a nest; greying them
    is the over-claim rulings 1–3 exist to prevent.
    """
    app = logged_in_app
    _require_connection_status(app)

    assert _wait_for_status(app, connected=True, timeout=CONNECT_WAIT_S) == CONNECTED
    _enroll_one_destination(app, second_nest, test_user)
    _open_remove_modal(app)
    request.addfinalizer(lambda: _dismiss_remove_modal(app))

    stop_nest(nest_instance, graceful=True)
    try:
        assert _wait_for_status(app, connected=False, timeout=DISCONNECT_WAIT_S) != CONNECTED
        assert _wait_until_disabled(app, "backup-destination-remove-confirm-button"), (
          "precondition: the gated sibling must actually be gated, or this test "
          "would pass against an app with no gate at all"
        )

        assert app.driver.is_enabled("backup-destination-remove-cancel-button"), (
          "an offline-capable control was greyed with no nest. The gate must "
          "desensitize ONLY class-3 kinds — dismissing a modal needs no nest, and "
          "greying it strands the user inside a dialog they cannot leave"
        )
    finally:
        start_nest_in_place(nest_instance)
