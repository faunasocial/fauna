"""tier_3 E2E: the Linux close-to-tray no-tray-host guard (Track 4).

Out-of-the-box-safety invariant (`docs/goal/architecture/apps/linux.md`
§ System Tray): enabling/using close-to-tray must NEVER strand the window when
no system-tray host is present (stock GNOME with no `StatusNotifierWatcher`).
The guard has two observable halves:

1. The "Close to tray" toggle is greyed (insensitive) with an explanatory note
   when no host is present, and enabled when one is.
2. Closing the window (the titlebar X / `connect_close_request`) **quits** when
   there is no host to restore from — even with close-to-tray ON — and only
   *hides* when a host is actually present.

The tray-host signal is whether `org.kde.StatusNotifierWatcher` is on the
client's session bus. Each test here runs the client on a private session bus
(`helpers/tray_bus.py`) it fully controls: no watcher → host absent; a fake
watcher → host present; stopping the watcher mid-run → the host disappears at
runtime (drives ksni `watcher_offine`).

This replaces the former manual hand-check (disabling the GNOME appindicator
extension is session-bus-wide and would disturb sibling clients).

Every *other* linux e2e now gets a private, watcher-less bus by default
(`drivers/linux.py::_wants_private_bus`), so close always quits for them. These
tests keep building their bus by hand because they are the ones that must drive
BOTH conditions — a fixed default cannot express host-present. The last test
covers the seam itself: that a default launch really does ignore the box's bus.
"""

import time

import pytest

from common.launch_harness import reached_authenticated_app
from conftest import _seeded_environment
from drivers import create_driver
from helpers.budgets import APP_EXIT_S
from helpers.tray_bus import TrayBus

# Real client + real binary nest (login reaches the authenticated main window
# where Settings → General lives). linux-only: the StatusNotifierWatcher / ksni
# mechanism is platform-specific; windows has its own Win32 tray model.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

CLOSE_TO_TRAY = "close-to-tray-toggle"
_GENERAL = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "general"}]}}
_PRIVACY = {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "privacy"}]}}
_FEED = {"nav": {"stack": [{"view": "feed"}]}}


@pytest.fixture
def tray_app(request, nest_instance, test_user, linux_app_path):
    """Factory: `make(host_present=bool) -> (driver, bus)`.

    Launches a fresh authenticated linux app on a private session bus, with or
    without a fake StatusNotifierWatcher. Tears every bus + driver down.

    **No `nest_binary` here, deliberately** — it used to be in this signature
    and was never referenced in the body. It was harmless boilerplate when it
    was added (`nest_instance` declared the name itself back then, so it said
    nothing extra), and the 2026-08-02 change to resolve the binary lazily
    turned it into a live exclusion signal: the nest-mode classifier reads the
    fixture closure, saw a binary-shaped name, and held all five tray tests out
    of every non-standalone mode. The nest here is only a URL to authenticate
    against, and `nest_instance` is already mode-routed, so the tests belong in
    whatever mode the run asked for.

    ⚠ The same syntax means the OPPOSITE in `tests/platform/windows/`, where
    both `harness` fixtures declare `nest_binary` without referencing it and it
    is load-bearing: `SyncTestHarness.start` calls `build_node()` where no
    closure can see it, so the parameter is the only thing that declares the
    dependency. Never sweep "unused fixture arguments" across this tree — only
    reading the fixture body tells the two apart.
    """
    created: list[tuple[object, TrayBus]] = []

    def make(host_present: bool):
        bus = TrayBus().start()
        if host_present:
            # Own the name BEFORE launch so `watcher_online` fires during the
            # client's tray startup (main.rs:52), before the main window builds.
            bus.start_watcher()
        driver = create_driver("linux")
        driver.launch({
            "url": nest_instance["url"],
            "app_path": linux_app_path,
            "environment": {
                **_seeded_environment(request, nest_instance),
                "DBUS_SESSION_BUS_ADDRESS": bus.address,
                "FAUNA_DNS_PROVIDER_FAKE": "1",
                # The private bus does no service activation, so skip GTK's a11y-bus
                # lookup (it would just `ServiceUnknown`-warn); the automation agent
                # reads the widget tree directly, not via AT-SPI.
                "GTK_A11Y": "none",
            },
        })
        _authenticate(driver, nest_instance, test_user)
        created.append((driver, bus))
        return driver, bus

    yield make

    for driver, bus in created:
        try:
            driver.teardown()
        except Exception:
            pass
        try:
            bus.stop()
        except Exception:
            pass


def _authenticate(driver, nest_instance, test_user):
    """Drive the app to the authenticated main window (mirrors `logged_in_app`)."""
    secret_hex = test_user["signing_key"].encode().hex()
    driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest_instance["url"],
            "secret_hex": secret_hex,
            "handle": "e2e-user",
            "actor_id": test_user["actor_id_hex"],
            "device_id": "tray-e2e",
        },
        **_FEED,
    })


def _goto_general(driver):
    """Navigate to Settings → General (fires the page's on-visible tray refresh)."""
    driver.set_state(_GENERAL)
    driver.wait_for(CLOSE_TO_TRAY, timeout=10)


def _revisit_general(driver):
    """Leave and re-enter General so its visible-child-notify refresh re-runs."""
    driver.set_state(_PRIVACY)
    time.sleep(0.3)
    driver.set_state(_GENERAL)
    driver.wait_for(CLOSE_TO_TRAY, timeout=10)


@pytest.mark.feature("general-settings")
def test_no_host_greys_toggle_and_quits_on_close(tray_app):
    """No tray host: the toggle is greyed with the explanatory note, and closing
    the window QUITS (never hides into a tray that isn't there)."""
    driver, _bus = tray_app(host_present=False)
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "disabled") == "true", (
        "close-to-tray must be greyed when no StatusNotifierWatcher is present"
    )
    subtitle = driver.get_text(CLOSE_TO_TRAY)
    assert "No system tray detected" in subtitle, (
        f"expected the no-host explanatory subtitle, got {subtitle!r}"
    )

    driver.window_close()
    assert driver.wait_app_exit(timeout=APP_EXIT_S), (
        "closing the window with no tray host must quit the app, not hide it "
        f"(still alive={driver.is_app_alive()})"
    )


@pytest.mark.feature("general-settings")
def test_host_present_enables_toggle_and_hides_on_close(tray_app):
    """A tray host is present: the toggle is enabled; with close-to-tray ON,
    closing the window HIDES it (process stays alive) rather than quitting."""
    driver, _bus = tray_app(host_present=True)
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "disabled") == "false", (
        "close-to-tray must be enabled when a StatusNotifierWatcher is present"
    )
    # Turn close-to-tray ON, then close: should hide, not quit.
    if driver.get_attr(CLOSE_TO_TRAY, "state") != "true":
        driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true"

    driver.window_close()
    # Window hidden → its elements leave the visible search roots; process alive.
    hidden = False
    for _ in range(20):
        if not driver.is_visible(CLOSE_TO_TRAY):
            hidden = True
            break
        time.sleep(0.1)
    assert driver.is_app_alive(), "close-to-tray hide must keep the app running"
    assert hidden, "with a tray host + close-to-tray ON, closing must hide the window"


@pytest.mark.feature("general-settings")
def test_close_to_tray_defaults_on_for_a_fresh_install(tray_app):
    """Shape A's default: a fresh install with a tray host present has
    close-to-tray already ON — nobody has to find the toggle for a window close
    to stop killing file sync (`apps/windows.md` § App Lifecycle → *Window
    close*; the app hosts the in-process `fauna-sync-engine`).

    Deliberately asserts the state WITHOUT touching the toggle first — the other
    tests here normalize with a toggle-if-needed idiom, so none of them would
    notice the default regressing to OFF.
    """
    driver, _bus = tray_app(host_present=True)
    _goto_general(driver)

    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "close-to-tray must default ON for a fresh install (no app-settings.json yet)"
    )


def _relaunch_to_general(driver):
    """Force-quit + relaunch keeping the client-local store, then land on General.

    The relaunched app signs ITSELF back in from the pinned credential store (the
    silent challenge) — what a real user's relaunch does, and the only version of
    this that exercises `apply_saved_close_to_tray()` on the real launch path. Do
    NOT re-inject the session with `set_state`: the patch's onboarding→main
    transition is gated on the launch screen's empty stack, so it would build a
    *second* main window racing the app's own and leave the nav on an orphaned stack.
    """
    assert driver.preserve_state_across_relaunch(), (
        "the durability assertion is vacuous unless the client-local store is pinned"
    )
    assert driver.recover(), "the app did not come back up after a relaunch"
    reached_authenticated_app(driver, timeout=90)
    _goto_general(driver)


@pytest.mark.feature("general-settings")
def test_close_to_tray_choice_survives_a_relaunch(tray_app):
    """A close-to-tray choice round-trips a real process boundary, both ways.

    The store's unit tests prove the JSON round-trips in-process, and
    `test_close_to_tray_defaults_on_for_a_fresh_install` proves
    `apply_saved_close_to_tray()` runs at startup — but nothing pinned the two
    together across a relaunch, which is the only thing the user experiences.
    Before the fix there was no persistence at all: the setting silently reset
    on every relaunch.

    BOTH halves are load-bearing, and the ON half is what makes this non-vacuous:
    `tray.rs`'s bare atomic is `false`, so a relaunch that never ran
    `apply_saved_close_to_tray()` would read OFF and let a one-directional
    "explicit OFF survives" assertion pass while nothing persisted at all. Only a
    persisted `true` can survive to the second assertion. Together they catch both
    regressions: an ON default overwriting an explicit OFF, and a store that never
    loads.
    """
    driver, _bus = tray_app(host_present=True)
    _goto_general(driver)

    # Explicit OFF, against the shape-A ON default.
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "precondition: a fresh install defaults close-to-tray ON"
    )
    driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "false"

    _relaunch_to_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "false", (
        "an explicitly-OFF close-to-tray must survive a relaunch — the ON default "
        "must never overwrite a user's explicit choice"
    )

    # ...and back ON must survive too (the non-vacuous half — see the docstring).
    driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true"

    _relaunch_to_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true", (
        "a persisted ON must survive a relaunch — reading OFF here means the saved "
        "choice was never loaded (tray.rs's bare atomic default is false)"
    )


def test_default_launch_ignores_an_ambient_tray_host(
    monkeypatch, nest_instance, test_user, linux_app_path, request
):
    """A launch that does NOT ask for a bus must not inherit the box's one.

    This is the whole-class guard, and the only test here that exercises the
    DEFAULT launch path every other linux e2e takes. Close-to-tray is persisted
    and default-ON, so `should_hide_to_tray()` reduces to "is a
    StatusNotifierWatcher on the bus we inherited" — and on a dev box gnome-shell
    owns that name and drops it at will. Every linux assertion about window close
    was therefore reading the developer's desktop session: `test_engagement_cues.py`'s
    dwell test failed its `wait_app_exit(15)` whenever a watcher happened to be up,
    and three sessions misread that as a shared-Rust flush hang (diagnosed 2026-07-17).

    A watcher on the *ambient* bus is simulated rather than borrowed from the real
    session: pointing `os.environ` at a watcher-owning `TrayBus` is exactly what
    the driver sees on a GNOME box (it inherits `os.environ`), but it holds on a
    headless runner too — so this proves the property everywhere instead of only
    where a human happens to be logged in.
    """
    ambient = TrayBus().start()
    ambient.start_watcher()  # the box "has" a tray host, as a real GNOME session does
    monkeypatch.setenv("DBUS_SESSION_BUS_ADDRESS", ambient.address)

    driver = create_driver("linux")
    try:
        # No bus/tray-related environment: the default path, as `logged_in_app`
        # takes — only the escrow-trust seed every launch carries.
        driver.launch({
            "url": nest_instance["url"],
            "app_path": linux_app_path,
            "environment": _seeded_environment(request, nest_instance),
        })
        _authenticate(driver, nest_instance, test_user)

        log = driver.app_stderr_text()
        assert "no StatusNotifierWatcher" in log, (
            "the app found a tray host, so the driver leaked the ambient bus into "
            f"the launch; [tray] lines = "
            f"{[ln for ln in log.splitlines() if '[tray]' in ln]!r}"
        )

        driver.window_close()
        assert driver.wait_app_exit(timeout=APP_EXIT_S), (
            "a default launch must quit on window close regardless of the box's "
            "tray host — this is the assertion that silently depended on whether "
            f"gnome-shell was publishing a watcher (still alive={driver.is_app_alive()})"
        )
    finally:
        try:
            driver.teardown()
        except Exception:
            pass
        ambient.stop()


@pytest.mark.feature("general-settings")
def test_host_disappears_at_runtime_regreys_and_quits(tray_app):
    """Dynamic transition (the Track-4 greying fix): a host present at launch
    disappears at runtime → the toggle re-greys on the next General visit, and
    closing the window QUITS even though close-to-tray was turned ON earlier."""
    driver, bus = tray_app(host_present=True)
    _goto_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "disabled") == "false"
    if driver.get_attr(CLOSE_TO_TRAY, "state") != "true":
        driver.click(CLOSE_TO_TRAY)
    assert driver.get_attr(CLOSE_TO_TRAY, "state") == "true"

    # Host disappears (e.g. the user disables the appindicator extension).
    bus.stop_watcher()
    time.sleep(1.0)  # let ksni process NameOwnerChanged → watcher_offine

    _revisit_general(driver)
    assert driver.get_attr(CLOSE_TO_TRAY, "disabled") == "true", (
        "toggle must re-grey once the tray host disappears (dynamic re-eval)"
    )

    driver.window_close()
    assert driver.wait_app_exit(timeout=APP_EXIT_S), (
        "with the host gone, closing must quit even though close-to-tray was ON "
        f"(still alive={driver.is_app_alive()})"
    )
