"""tier_3 E2E: linux's home-screen widget — the launcher badge — read from
outside the app.

`docs/features/home-screen-widget.md` outcomes 1 and 2, on the linux column.
Authority: `docs/goal/architecture/apps/linux.md` § Home-screen widget (the
surface: a `com.canonical.Unity.LauncherEntry.Update` broadcast the desktop's
dock paints on the launcher icon; the number: the tray's `sum_unread` over the
shared conversations snapshot) and `common.md` § Home-screen widget (the
cross-app promise: how many unread messages are waiting, kept current in the
background without the app being opened).

**What is asserted, and what deliberately is not.** The app's job is the
broadcast; painting it is the dock's, and no test can look at a dock. So the
witness is a subscriber on the app's private session bus
(`helpers/launcher_entry.py`) that reads exactly what a dock would — the
`app_uri` it keys on and the `count` it paints — and checks the number against
the app's own thread list, because the promise is that the widget never shows
a count the app would not.

**"Without the app being opened", on linux.** The badge lives while the
process does (`linux.md` § Home-screen widget → *Background currency*): the
app is resident from sign-in (autostart) and, where a tray host exists, stays
resident across a window close. So outcome 2's leg here is: with a tray host
present, close the window (it hides, the process stays), plant a message, and
read the badge move while nothing is on screen. A fake
`org.kde.StatusNotifierWatcher` on the private bus is what makes the close a
hide (`helpers/tray_bus.py`, the tray tests' precedent).

**Latency independence (convention 14).** Every read blocks on the bus for the
update it expects, up to a budget; there is no sleep-then-assert, and the
window-hidden check polls the automation surface for the hide the same way the
tray tests do.

linux-only, marked: the surface is linux's own (`feature-catalog.md` § Cell
semantics, the marked-witness rule) — android's Glance widget, apple's WidgetKit
and windows' surface each get their own witness.
"""

from __future__ import annotations

import time
import uuid

import pytest

from actions.conversations import ConversationsActions
from conftest import _seeded_environment
from drivers import create_driver
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.launcher_entry import LauncherEntryListener
from helpers.tray_bus import TrayBus

pytestmark = [pytest.mark.tier_3, pytest.mark.linux]

# The desktop id every non-snap channel installs (`packaging_identity_test.rs`
# pins the spelling); the harness launches the bare binary, so no FLATPAK_ID
# or SNAP_INSTANCE_NAME is set and the native id is the one to expect.
_APP_URI = "application://social.fauna.fauna.desktop"

_FEED = {"nav": {"stack": [{"view": "feed"}]}}
# Any element that is on screen while the main window is mapped and gone once
# it is hidden — the tray tests read the General page; the feed tab is on
# every authenticated screen.
_ON_SCREEN = "feed-tab"


@pytest.fixture
def badged_app(request, nest_instance, test_user, linux_app_path):
    """A logged-in linux app on a private session bus with a tray host, and a
    LauncherEntry subscriber on that same bus, attached before the launch so
    the app's very first publish is on the record."""
    bus = TrayBus().start()
    bus.start_watcher()
    listener = LauncherEntryListener(bus.address).start()
    driver = create_driver("linux")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": linux_app_path,
            "environment": {
                **_seeded_environment(request, nest_instance),
                "DBUS_SESSION_BUS_ADDRESS": bus.address,
                "FAUNA_DNS_PROVIDER_FAKE": "1",
                # The private bus does no service activation (see the tray
                # tests): skip GTK's a11y-bus lookup.
                "GTK_A11Y": "none",
            },
        })
        secret_hex = test_user["signing_key"].encode().hex()
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": secret_hex,
                "handle": "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "badge-e2e",
            },
            **_FEED,
        })
        driver.wait_for(_ON_SCREEN, timeout=RPC_ROUNDTRIP_S)
        yield driver, listener
    finally:
        listener.stop()
        try:
            driver.teardown()
        except Exception:
            pass
        try:
            bus.stop()
        except Exception:
            pass


def _total_unread(conv: ConversationsActions) -> int:
    """The number the app itself would show: its thread list's unread, summed."""
    return sum(t.unread_count for t in conv.list_threads())


def _await_hidden(driver, budget_s: float) -> bool:
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if not driver.is_visible(_ON_SCREEN):
            return True
        time.sleep(0.1)
    return False


@pytest.mark.feature("home-screen-widget")
def test_launcher_badge_shows_the_unread_count_and_moves_while_hidden(badged_app):
    """Outcome 1: the badge shows how many unread messages are waiting.
    Outcome 2: it keeps itself current while the window is closed."""
    driver, listener = badged_app
    conv = ConversationsActions(driver)
    nonce = uuid.uuid4().hex[:8]

    # ── Outcome 1: a message you have not read is counted on the badge ──────
    # The session-scoped account may already hold unread threads from earlier
    # tests; the assertion is against the app's own number, whatever it is.
    conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"badge-one-{nonce}@self-nest.test",
        subject=None,
        body="a message waiting on the home screen",
    )
    expected = _total_unread(conv)
    assert expected >= 1, "the planted inbound must be unread in the app's own list"
    update = listener.await_count(expected, timeout=RPC_ROUNDTRIP_S)
    assert update.app_uri == _APP_URI, (
        f"the badge is keyed on {update.app_uri!r}; a dock knows this app as "
        f"{_APP_URI!r} and paints nothing for any other id"
    )
    assert update.count_visible, "a count above zero must be painted (count-visible)"

    # ── Outcome 2: the count moves while nothing is on screen ───────────────
    driver.window_close()
    assert _await_hidden(driver, RPC_ROUNDTRIP_S), (
        "with a tray host present the window close must HIDE the window "
        "(`linux.md` § System Tray); the badge leg needs the app resident and "
        "unopened"
    )
    assert driver.is_app_alive(), "the hide must keep the process alive"

    conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"badge-two-{nonce}@self-nest.test",
        subject=None,
        body="a second message, arriving while the window is closed",
    )
    expected_hidden = _total_unread(conv)
    assert expected_hidden > expected, (
        "a second unopened inbound must raise the app's own unread total "
        f"({expected} → {expected_hidden})"
    )
    update = listener.await_count(expected_hidden, timeout=RPC_ROUNDTRIP_S)
    assert update.app_uri == _APP_URI and update.count_visible
    # linux-only, and the read is "the window is hidden": a hidden GTK window
    # drops every element from the automation surface's visible search roots
    # (the tray tests' own hide read), so nothing here can sit below a fold.
    # negative-visibility-ok: a hidden window has no fold — the element leaves the search roots entirely
    assert not driver.is_visible(_ON_SCREEN), (
        "the window came back on its own — the badge must have moved with the "
        "app unopened"
    )
