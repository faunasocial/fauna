"""tier_3 E2E: windows' home-screen widget — the taskbar badge — read from
outside the app.

`docs/features/home-screen-widget.md` outcomes 1 and 2, on the windows column.
Authority: `docs/goal/architecture/apps/windows.md` § Home-screen widget (the
surface: the numeric badge on Fauna's taskbar button, `BadgeUpdateManager`; the
number: the shared `ConversationsManager.unread_total()` fold) and `common.md`
§ Home-screen widget (the cross-app promise: how many unread messages are
waiting, kept current in the background without the app being opened).

**What is asserted, and what deliberately is not.** The app's job is the badge
update; painting it on the taskbar button is the shell's, and no test can look
at a taskbar. So the witness reads what the shell reads — the badge the OS
notification platform holds on record for the app's AUMID
(`helpers/taskbar_badge.py`, the store the taskbar paints from) — and checks the
number against the app's own thread list, because the promise is that the
widget never shows a count the app would not. The windows twin of the linux
witness (`test_home_screen_widget_launcher_badge.py`), whose bus subscriber is
the dock.

**Identity.** Badges are keyed on package identity, which a bare build output
lacks; the fixture lends the build the sparse identity package for the test's
duration (`helpers/taskbar_badge.py::registered_identity` — `Add-AppxPackage
-Register`, non-elevated, Developer Mode, exactly the Store-registration test's
path), bound to the exe by the `msix` element in `FaunaApp/app.manifest.in`. A box
already holding a real registration of that identity (an MSI install) is left
alone: the test steps aside as an environment skip rather than displace it.

**"Without the app being opened", on windows.** The OS keeps the badge across a
window close and even a process exit, and the resident app keeps it current
(`windows.md` § Home-screen widget → *Background currency*): auto-start plus
close-to-tray (both default ON) leave Fauna resident from sign-in with its
WS-RPC subscription live. So outcome 2's leg here is: close the window (it
hides to the tray; the process stays), plant a message, and read the badge move
while nothing is on screen.

**Latency independence (convention 14).** Every read polls the OS's own store
for the value it expects, up to a budget; there is no sleep-then-assert.

windows-only, marked: the surface is windows' own (`feature-catalog.md` § Cell
semantics, the marked-witness rule) — linux's launcher badge, android's Glance
widget and apple's WidgetKit each get their own witness.
"""

from __future__ import annotations

import os
import sys
import time
import uuid

import pytest

from actions.conversations import ConversationsActions
from conftest import _seeded_environment
from drivers import create_driver
from helpers import taskbar_badge
from helpers.app_surface import skip_environment
from helpers.budgets import RPC_ROUNDTRIP_S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

_FEED = {"nav": {"stack": [{"view": "feed"}]}}
# Any element that is on screen while the main window is mapped and gone once
# it is hidden — the feed tab is on every authenticated screen.
_ON_SCREEN = "feed-tab"


def _repo_root() -> str:
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.abspath(os.path.join(here, "..", "..", ".."))


@pytest.fixture
def lent_identity(windows_app_path, tmp_path):
    """The build output registered under the sparse identity package for this
    test; yields the app's AUMID. Give-back is unconditional."""
    if sys.platform != "win32":
        skip_environment("the taskbar badge is a Windows surface")
    foreign = taskbar_badge.foreign_registration()
    exe_dir = os.path.dirname(os.path.abspath(str(windows_app_path)))
    if foreign and os.path.normcase(os.path.abspath(foreign)) != os.path.normcase(exe_dir):
        skip_environment(
            f"a real {taskbar_badge.IDENTITY_NAME} registration ({foreign}) holds the "
            "identity on this box; the witness will not displace an installed Fauna"
        )
    repo = _repo_root()
    aumid = taskbar_badge.registered_identity(
        str(windows_app_path),
        os.path.join(repo, "apps", "fauna-windows", "installer", "sparse", "AppxManifest.xml.in"),
        os.path.join(repo, "apps", "fauna-windows", "installer", "sparse", "Assets"),
        str(tmp_path / "identity"),
    )
    try:
        yield aumid
    finally:
        taskbar_badge.remove_identity()


@pytest.fixture
def badged_app(request, nest_instance, test_user, windows_app_path, lent_identity):
    """A logged-in windows app running WITH package identity, and a reader on the
    OS badge store for its AUMID."""
    reader = taskbar_badge.TaskbarBadgeReader(lent_identity)
    driver = create_driver("windows")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": windows_app_path,
            "environment": _seeded_environment(request, nest_instance),
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
        yield driver, reader
    finally:
        try:
            driver.teardown()
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


def _why(driver, reader: taskbar_badge.TaskbarBadgeReader, expected: int, got: int | None) -> str:
    log = driver.app_log_text() or ""
    badge_lines = [ln for ln in log.splitlines() if "[badge]" in ln]
    return (
        f"the OS holds badge {got!r} for {reader.aumid}, the app's own list says {expected}"
        + (f"; app log: {' | '.join(badge_lines)}" if badge_lines else
           "; the app logged no [badge] line (a '[badge] taskbar badge unavailable' line "
           "would mean the process ran WITHOUT package identity)")
    )


@pytest.mark.feature("home-screen-widget")
def test_taskbar_badge_shows_the_unread_count_and_moves_while_hidden(badged_app):
    """Outcome 1: the badge shows how many unread messages are waiting.
    Outcome 2: it keeps itself current while the window is closed."""
    driver, reader = badged_app
    conv = ConversationsActions(driver)
    nonce = uuid.uuid4().hex[:8]

    # ── Outcome 1: a message you have not read is counted on the badge ──────
    # The session-scoped account may already hold unread threads from earlier
    # tests; the assertion is against the app's own number, whatever it is.
    conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"badge-one-{nonce}@self-nest.test",
        subject=None,
        body="a message waiting on the taskbar",
    )
    expected = _total_unread(conv)
    assert expected >= 1, "the planted inbound must be unread in the app's own list"
    got = reader.await_value(expected, timeout=RPC_ROUNDTRIP_S)
    assert got == expected, _why(driver, reader, expected, got)

    # ── Outcome 2: the count moves while nothing is on screen ───────────────
    driver.window_close()
    assert _await_hidden(driver, RPC_ROUNDTRIP_S), (
        "with close-to-tray on (the default) the window close must HIDE the window "
        "(`windows.md` § App Lifecycle); the badge leg needs the app resident and unopened"
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
    got = reader.await_value(expected_hidden, timeout=RPC_ROUNDTRIP_S)
    assert got == expected_hidden, _why(driver, reader, expected_hidden, got)
    # windows-only, and the read is "the window is hidden": a hidden WinUI window
    # leaves the UIA tree the bridge searches, so nothing here can sit below a fold.
    # negative-visibility-ok: a hidden window has no fold — the element leaves the search roots entirely
    assert not driver.is_visible(_ON_SCREEN), (
        "the window came back on its own — the badge must have moved with the "
        "app unopened"
    )
