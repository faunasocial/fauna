"""Tests for device cards on the devices page (Linux, Web, iOS, macOS)."""
import os
import secrets
import time
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import PLACE_BACKUP, add_folder_member, user_create_folder
from i18n.strings import S

# reclaim_cycle: the devices area's representative in the post-reclaim gate —
# under `--reclaim-cycle` the shared nest is wiped + re-claimed before this runs,
# so sync-device registration + card rendering is exercised post-reclaim.
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.reclaim_cycle]


def _register_test_device(
    nest_url: str,
    actor_id: bytes,
    signing_key: bytes,
    label: str = "test-device",
    device_id: str | None = None,
) -> None:
    """Register a sync device the way a client's sync daemon does — the
    `fauna.sync.register` WS-RPC kind, as the device-owning actor.

    NOT the HTTP twin `POST /api/v1/sync/register`: the sync control plane
    (register / changes / status / files / devices / backup-status) moved to the
    `fauna.sync.*` kinds in the WS-RPC-everywhere rip-out and the HTTP twins were
    deleted (`bins/fauna-nest/src/sync_routes.rs` keeps only the byte-download
    route). A POST to the dead path silently lands on the SPA `.fallback` → 200
    and registers nothing, so every app's device list stayed empty — exactly
    the trap `test_devices_conflicts.py` already avoids by reporting conflicts
    over WS-RPC.

    **The label is SEALED, the way a real daemon registers it (S9 flip,
    2026-08-02).** The nest rests no plaintext label for a user-chosen one
    (`register_sync_device`, `bins/fauna-nest/src/db/sync_storage.rs`), so a
    sealless register here seeds a row whose `device-name` reads `''` forever.
    Note what that does NOT break: the card still renders and `device-card`
    counts stay correct — this plane's degrade keeps a nameless row rather than
    dropping it (`DevicesMachine::render_devices`), which is precisely what makes
    the mistake easy to miss. The seal rides the real funnel
    (`fauna_ffi.seal_device_label` → `fauna_core::label_custody::seal_device_label`)
    rather than being reimplemented here: a drifted seal fails silently, as an
    empty name, never as an error.
    """
    from fauna_ffi import seal_device_label

    if device_id is None:
        device_id = os.urandom(32).hex()
    label_sealed = seal_device_label(signing_key, bytes.fromhex(device_id), label)
    payload = {"device_id": device_id, "label": label, "capabilities": "read,write"}
    if label_sealed is not None:
        payload["label_sealed"] = label_sealed
    with WsRpcAdminClient(nest_url, actor_id, signing_key) as client:
        client.call("fauna.sync.register", payload)


def _device_card_index_by_name(driver, name: str) -> int:
    """Position of the `device-card` whose `device-name` reads `name` — order-
    independent, so the test never depends on the nest's device sort. Mirrors
    `test_family.py`'s helper of the same name."""
    for i in range(driver.count("device-card")):
        if driver.get_text("device-name", index=i) == name:
            return i
    raise AssertionError(f"no device-card named {name!r}: {driver.diagnose('device-name')}")


def _refresh_devices(app) -> None:
    """Force a Devices-page refresh.

    linux/GTK's `DevicesMachine` only re-fetches on a `connect_map` transition
    (the sub-page becoming visible), which a `navigate_devices()` call while
    ALREADY on that page does not re-fire — so re-issuing it in a poll loop is
    a no-op there. Navigate away and back, mirroring
    `test_devices_conflicts.py`'s `_refresh_folders` (same `DevicesMachine`,
    same `connect_map` wiring for both its sub-pages).

    **linux-only.** Web polls its own snapshot on an interval and a fresh nav
    restarts that cycle from an empty render — yanking it away every 0.5s
    (this helper's own poll cadence) means it can never complete a fetch, so
    forcing the same away-and-back trick there is actively counterproductive.
    Other apps keep their own plain re-navigate idiom.
    """
    if app.driver.is_linux():
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        time.sleep(0.3)
    app.backups.navigate_devices()


def _card_frame(driver, index: int) -> str:
    """The `device-card[index]` row's own frame, for a chip-visibility failure.

    A chip's frame alone does not classify the failure on windows: UIA reports
    an empty rect both for an element the layout gave no size and for one
    clipped wholly out of its scroll viewport. The enclosing card's frame tells
    the two apart — an on-screen card around an empty chip is a layout bug, an
    empty card is a scroll the check did not reach."""
    try:
        return f"device-card[{index}] frame={driver.get_attr('device-card', 'frame', index)!r}"
    except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the assertion
        return f"device-card[{index}] frame=<{type(e).__name__}: {e}>"


# Windows rebuilds a fresh `DevicesPage` on every `navigate_devices()` call,
# restarting its own async load (`DevicesPage.xaml.cs`'s `Page_Loaded` ->
# `_machine.Refresh()`) rather than pushing live updates, so re-navigating on
# every poll tick can starve the page of the time it needs to ever finish
# loading (convention 14's rider, `e2e-latency-independent-assertions.md:27`).
# Poll loops below re-read the mounted page and re-navigate no more often
# than this.
_DEVICES_NAV_SETTLE_S = 2.0


def _find_device_card_by_name(driver, name: str) -> int | None:
    """Index of the `device-card` whose `device-name` reads `name`, or
    `None` if no such row is currently painted — the non-raising twin of
    `_device_card_index_by_name`, for a caller riding out a transient miss
    instead of failing on the first one."""
    for i in range(driver.count("device-card")):
        if driver.get_text("device-name", index=i) == name:
            return i
    return None


def _wait_for_device_card_by_name(app, name: str, timeout: float = 15.0) -> int:
    """Index of the `device-card` whose `device-name` reads `name`, polling
    for it in place rather than raising on the first miss.

    By-label, not by-novelty: every app this suite drives now decodes sealed
    device labels (web since 2026-08-13), so a device
    registered under a CHOSEN label paints under that exact name — no need
    to infer it by elimination against a "before" snapshot, which risked
    mistaking the app's own self device (`fauna`, `label_custody.rs:541`)
    for the one under test whenever that snapshot was taken before the self
    device had finished painting.
    """
    deadline = time.monotonic() + timeout
    last_nav = time.monotonic()
    while True:
        index = _find_device_card_by_name(app.driver, name)
        if index is not None:
            return index
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"no device-card named {name!r} after {timeout}s: "
                f"{app.driver.diagnose('device-name')}"
            )
        time.sleep(0.5)
        if time.monotonic() - last_nav >= _DEVICES_NAV_SETTLE_S:
            _refresh_devices(app)
            last_nav = time.monotonic()


#: The apps whose This-device badge reads the row the machine ENROLLED on
#: (`AccountStoreHandle::enrolled_device_row` through
#: `fauna_devices_machine::this_device_row`); the rest still compare their own
#: id — `devices.md` § Implementation status today.
_ENROLLED_ROW_MARKER_APPS = frozenset({"tui", "linux"})

#: The badge settling on the enrolled row's card: a nav-edge hydrate plus, at
#: worst, an adopt pass moving the latch mid-poll. A green run pays only the
#: real latency (convention 14).
_THIS_DEVICE_MARK_CONVERGE_S = 60.0


class TestDeviceCards:
    """Verify device-card elements on the devices page."""

    @pytest.fixture(autouse=True)
    def setup(self, logged_in_app, nest_instance, test_user):
        self.app = logged_in_app
        self.nest_url = nest_instance["url"]
        self.nest_port = nest_instance["port"]
        self.admin_signing_key = nest_instance["admin"]["signing_key"]
        self.actor_id = bytes.fromhex(test_user["actor_id_hex"])
        self.signing_key = bytes(test_user["signing_key"])

    def _nest_device_count(self) -> int:
        """How many sync devices the nest holds for this actor, over the same
        `fauna.sync.devices.list` the page's own fetch uses."""
        with WsRpcAdminClient(self.nest_url, self.actor_id, self.signing_key) as client:
            return len(client.call("fauna.sync.devices.list", {}).get("devices", []))

    @pytest.mark.feature("devices")
    def test_devices_page_matches_nest_roster(self):
        """The page paints exactly the devices the nest holds for this actor —
        no stale rows, no dropped ones.

        **This replaced a `count == 0` empty-roster assertion (2026-08-13), and
        the reason is a goal-doc contract, not a flake.** `devices.md` § New
        Platform Implementation Checklist steps 4-5 say every conformant client
        generates a `device_id` and registers itself for sync at login — so on
        any app that runs a sync engine (tui does, eagerly at session establish)
        the roster is **never** empty after `logged_in_app`, and "loads empty"
        was a precondition the suite could not keep. Asserting the page against
        the nest's own list keeps the original intent (the page loads, and shows
        nothing it shouldn't) while being true on every app whether or not it
        self-registers: an app that registers nothing still matches at zero.

        It is also strictly stronger than what it replaced — an empty count
        cannot distinguish "painted nothing" from "painted rows the nest no
        longer has", which is exactly the failure mode
        (`DevicesMachine::render_devices` going stale) the old assertion was
        reached for and could never have caught.
        """
        # 2026-06-28 unification: the roster is reached via backups.navigate_devices()
        # (Settings → Devices on web; the combined top-level page on not-yet-migrated
        # clients) — no literal {"view":"devices"} assertion, since web reports the
        # Settings-shell view there.
        self.app.backups.navigate_devices()

        # Convention 14: a generous named budget + deadline poll, never a sleep.
        # The app's OWN registration is in flight concurrently with this read, so
        # both sides are re-read each pass — the assertion is that they converge,
        # which is latency-independent; a green run pays nothing.
        #
        # Re-read the mounted page rather than re-navigating on every pass —
        # see `_DEVICES_NAV_SETTLE_S`'s module comment: on windows a `navigate_devices()` call rebuilds a fresh
        # page that restarts its own async load, so hammering it every 0.5s
        # can starve the roster of the time it needs to ever finish painting.
        ROSTER_CONVERGE_BUDGET_S = 30.0
        deadline = time.monotonic() + ROSTER_CONVERGE_BUDGET_S
        last_nav = time.monotonic()
        painted = self.app.driver.count("device-card")
        expected = self._nest_device_count()
        while time.monotonic() < deadline and painted != expected:
            time.sleep(0.5)
            if time.monotonic() - last_nav >= _DEVICES_NAV_SETTLE_S:
                self.app.backups.navigate_devices()
                last_nav = time.monotonic()
            painted = self.app.driver.count("device-card")
            expected = self._nest_device_count()

        # Convention 6: the failure diagnoses itself — WHICH rows are painted is
        # the whole diagnosis. Rows the nest does not have read completely
        # differently from rows it has and the page dropped.
        assert painted == expected, (
            f"the page paints {painted} device-card(s) but the nest holds "
            f"{expected} for this actor after {ROSTER_CONVERGE_BUDGET_S}s: "
            f"{[self.app.driver.get_text('device-name', index=i) for i in range(painted)]} "
            f"{self.app.driver.diagnose('device-card')}"
        )

    @pytest.mark.feature("devices")
    def test_device_card_shows_after_register(self):
        """After registering a device, device-card appears with name and status."""
        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label="my-linux-box")

        self.app.backups.navigate_devices()
        self.app.driver.wait_for("device-card", timeout=15)

        assert self.app.driver.count("device-card") >= 1, (
            "a registered device should render a device-card: "
            f"{self.app.driver.diagnose('device-card')}"
        )

        name = self.app.driver.get_text("device-name", index=0)
        assert name is not None and len(name) > 0, (
            f"the device-card should show a non-empty name, got {name!r}: "
            f"{self.app.driver.diagnose('device-name')}"
        )

        status = self.app.driver.get_text("device-status", index=0)
        assert status in ("Online", "Offline"), (
            f"device-status should read Online/Offline, got {status!r}: "
            f"{self.app.driver.diagnose('device-status')}"
        )

    @pytest.mark.feature("devices")
    def test_device_card_marks_this_device(self):
        """Exactly one card renders `device-this-mark-badge`, and it is the row
        the app ENROLLED on — the app's own device id only when nothing has
        enrolled (`devices.md` § This-device marker: "the enrolled row wins; the
        app's own id is the fallback").

        Which row that is, per enrollment state, on tui and linux (the two apps
        whose badge reads the enrolled row — `devices.md` § Implementation
        status today, *Live — the This-device marker reads the ENROLLED row*):

        * **Nothing latched yet** (the enrollment's nest legs have not first
          succeeded) → the app's own id. `logged_in_app`'s session patch
          (`_login_app_as`, conftest.py) seeds it as the well-known
          `_E2E_LOGIN_DEVICE_ID`, and the app adopts that as its real sync
          identity (`media::adopt_device_id_hex`) — so the fixture row this
          test registers under that id, `this-device-under-test`, carries it.
        * **Latched** → the machine's named row, the same card: since the
          one-credential shape (`apps/sync-agent-credentials.md` § Credential
          model, RULED 2026-09-28) every enrollment targets the app's own id,
          so the enrolled row and the own id agree. A latch naming any other
          row is a regression, and the poll below never converges on it.

        The other five apps still compare their own id, so for them the expected row is always the own id.

        **Why a convergence poll, never a single read (convention 14).** The
        badge paints at its own pace behind the enrollment, so each poll reads
        the latch host-side (`enrollment.latched_row_in`, the same record
        `AccountStoreHandle::enrolled_device_row` reports) on both sides of
        reading the page, and accepts only a placement that matches the rule for
        a latch that did not move in between. Until the one-credential shape
        the latch could also name a writer-key placeholder row, whose card this
        test then accepted; that state no longer exists.

        ⚠ The own-id premise is a real contract the app has to honour: until
        2026-08-13 tui's marker compared a randomly-minted `device.db` the
        session patch never reached. An app that fails the nothing-latched
        state the same way has the same bug — check that its local device-id
        store honours the patch before touching the assertions."""
        from conftest import _E2E_LOGIN_DEVICE_ID
        from common.cred_store import attach_account_store
        from helpers import enrollment
        from helpers.app_surface import app_name

        _register_test_device(
            self.nest_url, self.actor_id, self.signing_key,
            label="this-device-under-test", device_id=_E2E_LOGIN_DEVICE_ID,
        )
        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label="other-device")

        driver = self.app.driver
        client = app_name(driver)
        actor_hex = self.actor_id.hex()
        if client in _ENROLLED_ROW_MARKER_APPS:
            store = attach_account_store(client, driver)

            def read_enrolled():
                return enrollment.latched_row_in(store.read_map(), actor_hex)
        else:
            def read_enrolled():
                return None

        def badge_in(index):
            return index is not None and driver.is_visible_scrolled(
                "device-this-mark-badge", scope=f"device-card[{index}]"
            )

        last: dict = {}

        def placement_matches_rule():
            before = read_enrolled()
            expected = before or _E2E_LOGIN_DEVICE_ID
            this_index = _find_device_card_by_name(driver, "this-device-under-test")
            other_index = _find_device_card_by_name(driver, "other-device")
            badges = driver.count("device-this-mark-badge")
            on_this, on_other = badge_in(this_index), badge_in(other_index)
            after = read_enrolled()
            last.clear()
            last.update(
                enrolled_before=before, enrolled_after=after, expected_row=expected,
                badges=badges, on_this_device_under_test=on_this, on_other_device=on_other,
                painted=[driver.get_text("device-name", index=i)
                         for i in range(driver.count("device-card"))],
            )
            if before != after or this_index is None or other_index is None:
                return None
            # A latch on any row but the named one is the regression the
            # one-credential shape rules out — never a state to accept.
            if expected != _E2E_LOGIN_DEVICE_ID:
                return None
            if badges != 1 or on_other:
                return None
            return "own-or-named" if on_this else None

        self.app.backups.navigate_devices()
        driver.wait_for("device-card", timeout=15)
        deadline = time.monotonic() + _THIS_DEVICE_MARK_CONVERGE_S
        last_nav = time.monotonic()
        state = placement_matches_rule()
        while state is None and time.monotonic() < deadline:
            time.sleep(0.5)
            # Re-enter the page on a cadence, never every tick: the nav edge is
            # what re-hydrates the enrolled row on tui (and `connect_map` on
            # linux — `_refresh_devices`), and a windows re-nav restarts its
            # load (`_DEVICES_NAV_SETTLE_S`).
            if time.monotonic() - last_nav >= _DEVICES_NAV_SETTLE_S:
                _refresh_devices(self.app)
                last_nav = time.monotonic()
            state = placement_matches_rule()

        # Convention 6: which state the enrollment was in, where the badge
        # landed, and every painted name ARE the diagnosis. An id-shaped name
        # means a sealed label did not decode (label custody unwired), not a
        # marker bug; a badge on `this-device-under-test` while the latch names
        # another row is the own-id regression this test exists to catch.
        print(f"[this-device-mark] {client}: converged in state {state!r}: {last!r}", flush=True)
        assert state is not None, (
            "device-this-mark-badge never settled on exactly the enrolled row's card "
            f"(the own id's when nothing is enrolled) within {_THIS_DEVICE_MARK_CONVERGE_S}s; "
            f"last read: {last!r}. "
            f"{driver.diagnose('device-this-mark-badge')}"
        )

    @pytest.mark.feature("devices")
    def test_device_remove_button_visible(self):
        """Each device card has a visible remove button."""
        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label="removable-device")

        self.app.backups.navigate_devices()
        self.app.driver.wait_for("device-card", timeout=15)

        assert self.app.driver.is_visible("device-remove-button"), (
            "each device-card should expose a visible remove button: "
            f"{self.app.driver.diagnose('device-remove-button')}"
        )

    @pytest.mark.feature("devices")
    def test_device_fileset_role_badge_shows_role_chip(self):
        """A device holding a place in a folder paints one
        `device-folder-role-badge` chip per folder, scoped to that device's own
        card — text composed by the shared
        `fauna_core::format::device_place_label` from the place's flags and
        rendered through the app's NESTED resolver (the two-flag template's
        arguments are themselves i18n keys, so a plain resolve would paint raw
        keys).

        Built on web/linux/android/tui (2026-08-13), macos/ios (via the shared
        FaunaKit `DeviceFolderRoleBadge` view), and windows (`DevicesPage.xaml`'s `ItemsControl` over
        `DeviceRow.FolderRoleBadges`) — every app now renders this leg.
        """
        run_tag = secrets.token_hex(4)
        self.app.backups.navigate_devices()

        role_device_name = f"role-chip-device-{run_tag}"
        device_id = secrets.token_hex(32)
        _register_test_device(
            self.nest_url, self.actor_id, self.signing_key,
            label=role_device_name, device_id=device_id,
        )
        index = _wait_for_device_card_by_name(self.app, role_device_name)

        # No folder membership yet: no chip renders (the guard `if
        # !device.folders.is_empty()` every app leg carries).
        #
        # Every visibility read of a chip in this test is `is_visible_scrolled`,
        # never a bare `is_visible`: the roster accumulates devices from every
        # earlier test sharing this nest, so the card under test can sit below
        # the fold of the page's scroll container. Windows' `is_visible` reads
        # UIA `IsOffscreen`, so a bare read reports a painted-but-unscrolled chip
        # as absent — which fails the positive check below and makes these
        # negative ones pass whether or not a chip was painted. The scroll is
        # best-effort (`drivers/base.py`), so an absent chip still reads False
        # and an app without scroll support degrades to a plain `is_visible`.
        assert not self.app.driver.is_visible_scrolled(
            "device-folder-role-badge", scope=f"device-card[{index}]"
        ), "a device with no folder membership must not paint a role chip"

        set_name = f"role-chip-set-{run_tag}"
        secret_key = bytes(self.signing_key).hex()
        user_create_folder(
            self.nest_port, set_name, secret_key=secret_key, )
        add_folder_member(
            self.nest_port, set_name, device_id, PLACE_BACKUP,
            admin_signing_key=self.admin_signing_key, actor_id=self.actor_id.hex(),
        )

        # Re-find the row by name on every pass (via the non-raising
        # `_find_device_card_by_name`) rather than the raising
        # `_device_card_index_by_name`: a fresh `DevicesPage` can briefly
        # paint zero rows while its own async load is still in flight right
        # after `_refresh_devices()` re-navigates, and that must ride out
        # inside this deadline rather than fail the test on the first
        # miss.
        index = _wait_for_device_card_by_name(self.app, role_device_name)
        deadline = time.monotonic() + 15.0
        last_nav = time.monotonic()
        badge_visible = self.app.driver.is_visible_scrolled(
            "device-folder-role-badge", scope=f"device-card[{index}]"
        )
        while time.monotonic() < deadline and not badge_visible:
            time.sleep(0.5)
            if time.monotonic() - last_nav >= _DEVICES_NAV_SETTLE_S:
                _refresh_devices(self.app)
                last_nav = time.monotonic()
            found = _find_device_card_by_name(self.app.driver, role_device_name)
            if found is not None:
                index = found
                badge_visible = self.app.driver.is_visible_scrolled(
                    "device-folder-role-badge", scope=f"device-card[{index}]"
                )

        assert self.app.driver.is_visible_scrolled(
            "device-folder-role-badge", scope=f"device-card[{index}]"
        ), (
            "the device now holds a place in one folder and must "
            "paint the chip: "
            f"{self.app.driver.diagnose('device-folder-role-badge', attrs=('frame',), scope=f'device-card[{index}]')} "
            f"{_card_frame(self.app.driver, index)}"
        )
        text = self.app.driver.get_text(
            "device-folder-role-badge", scope=f"device-card[{index}]"
        )
        # PLACE_BACKUP = originates + accepts: the two-flag template, each
        # argument resolved to its own label.
        expected = S.devices.place_two(
            first=S.devices.wizard.place_originates,
            second=S.devices.wizard.place_accepts,
        )
        assert text == expected, (
            f"the chip must render the composed place label {expected!r} "
            f"(nested-resolved, never raw keys); got {text!r}"
        )

        # A different, unrelated device carries no membership and must not
        # be affected — the join is per-device, never fleet-wide.
        no_role_device_name = f"no-role-device-{run_tag}"
        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label=no_role_device_name)
        other_index = _wait_for_device_card_by_name(self.app, no_role_device_name)
        assert not self.app.driver.is_visible_scrolled(
            "device-folder-role-badge", scope=f"device-card[{other_index}]"
        ), "an unrelated device must not inherit another device's role chip"

    @pytest.mark.feature("devices")
    def test_peer_actor_id_copy_button_is_single_instance(self):
        """`peer-actor-id-copy-btn` is page-level (ui.yaml `indexed: false`,
        `devices.md` § Layout & flow point 2: a single, non-per-row copy of
        THIS client's own actor ID, for handing to a new device being
        paired) — exactly one instance regardless of roster size, and never
        scoped inside a `device-card` row.

        Historically android/apple/tui rendered it once PER `device-card`,
        copying that row's `device_id` instead of the client's own actor ID
        (fixed 2026-08-13;
        windows was already correct and needed no change). Clipboard
        content itself isn't asserted here: only `windows.py` implements
        `get_clipboard_text()` (see `drivers/base.py`), so cross-app
        coverage of "copies the right VALUE" lives at the tier_1 layer per
        app (`settings/devices.rs`'s
        `actor_id_copy_button_is_single_instance_and_copies_the_clients_own_actor_id`
        on tui; `DevicesRosterContentTest`'s
        `actorIdCopyButtonCopiesTheClientsOwnActorIdNotADeviceId` on android)
        — this test covers the structural bug class instead, which every
        app's driver CAN assert headlessly.

        The per-row `peer-wg-key-copy-btn` this docstring used to carve out
        no longer exists on any app: it was deleted 2026-08-23 with the rest
        of the WireGuard stack (user-directed), so there is only the
        page-level button left to get wrong.
        """
        # Even with an empty roster the page-level button renders — pairing
        # the FIRST device is exactly when copying this client's own actor
        # ID is needed.
        self.app.backups.navigate_devices()
        self.app.driver.wait_for("peer-actor-id-copy-btn", timeout=15)
        assert self.app.driver.count("peer-actor-id-copy-btn") == 1, (
            "expected exactly one peer-actor-id-copy-btn with an empty roster: "
            f"{self.app.driver.diagnose('peer-actor-id-copy-btn')}"
        )

        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label="first-device")
        _register_test_device(self.nest_url, self.actor_id, self.signing_key, label="second-device")
        self.app.backups.navigate_devices()
        self.app.driver.wait_for("device-card", timeout=15)
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and self.app.driver.count("device-card") < 2:
            time.sleep(0.5)
            self.app.backups.navigate_devices()
        assert self.app.driver.count("device-card") >= 2, (
            f"expected 2 device-cards: {self.app.driver.diagnose('device-card')}"
        )

        assert self.app.driver.count("peer-actor-id-copy-btn") == 1, (
            "peer-actor-id-copy-btn must stay a SINGLE instance even with "
            f"multiple device-cards: {self.app.driver.diagnose('peer-actor-id-copy-btn')}"
        )
        for i in range(self.app.driver.count("device-card")):
            # Scrolled, so a card below the fold cannot pass this vacuously.
            assert not self.app.driver.is_visible_scrolled(
                "peer-actor-id-copy-btn", scope=f"device-card[{i}]"
            ), (
                f"peer-actor-id-copy-btn must NOT be scoped inside device-card[{i}] "
                "— it copies the client's OWN actor id, never a listed device's id"
            )
