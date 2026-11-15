"""tier_3 E2E: windows' home-screen widget — the taskbar badge — read from
outside the app.

`docs/features/home-screen-widget.md` outcomes 1, 2 and 3 (and 4, 5, 6 in the
journeys below), on the windows column.
Authority: `docs/goal/architecture/apps/windows.md` § Home-screen widget (the
surface: the numeric badge on Fauna's taskbar button, `BadgeUpdateManager`; the
number: the shared `ConversationsManager.unread_total()` fold; the badge ending
with the account at an actor change) and `common.md` § Home-screen widget (the
cross-app promise: how many unread messages are waiting, kept current in the
background without the app being opened).

**Zero reads as no badge.** The app clears the badge at zero rather than paint a
"0" (`TaskbarBadgePolicy`), and the reader reads a cleared badge as ``None`` —
so every expectation goes through :func:`_badge`, which maps an app total of 0
to ``None``.

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
from actions.settings import SettingsActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from conftest import _alice_extra_seat, _apply_real_conversations_env, _seeded_environment
from drivers import create_driver
from helpers import taskbar_badge
from helpers.app_surface import skip_environment
from helpers.budgets import MLS_HANDSHAKE_S, RPC_ROUNDTRIP_S
from helpers.waiting import (
    await_account_runtime_assembled,
    await_channel_bound,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api

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
    OS badge store for its AUMID. A test carrying the `real_conversations` marker
    (outcome 4's journey, which needs the real FaunaMls receive loop) launches
    behind the open gate, through the one writer of its environment; every other
    test here launches on the mock backend, whatever else the invocation selected —
    this app is the test's own, never the session-cached one the gate is
    session-wide for, so the inject journeys need not share its real receive loop."""
    reader = taskbar_badge.TaskbarBadgeReader(lent_identity)
    environment = _seeded_environment(request, nest_instance)
    if request.node.get_closest_marker("real_conversations"):
        _apply_real_conversations_env(environment, request)
    driver = create_driver("windows")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": windows_app_path,
            "environment": environment,
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


def _badge(total: int) -> int | None:
    """What the OS holds for an app total of ``total``: the number itself, or no
    badge at zero (`TaskbarBadgePolicy` clears rather than paint a "0", and
    :meth:`TaskbarBadgeReader.read` reads a cleared badge as ``None``)."""
    return total or None


def _await_hidden(driver, budget_s: float) -> bool:
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if not driver.is_visible(_ON_SCREEN):
            return True
        time.sleep(0.1)
    return False


def _why(driver, reader: taskbar_badge.TaskbarBadgeReader, expected: int | None,
         got: int | None) -> str:
    log = driver.app_log_text() or ""
    badge_lines = [ln for ln in log.splitlines() if "[badge]" in ln]
    return (
        f"the OS holds badge {got!r} for {reader.aumid}, wanted {expected!r} "
        "(None = no badge)"
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


@pytest.mark.feature("home-screen-widget")
def test_a_search_in_the_conversations_list_never_changes_the_taskbar_badge(badged_app):
    """Outcome 6 (`common.md` § Home-screen widget: the account's total, never
    a search-narrowed list's; `windows.md` § Home-screen widget: the badge reads
    `unread_total()` over every thread, not the snapshot list): with a search
    narrowing the list to one thread, a new message raises the badge to the
    UNFILTERED total. A badge summed over the narrowed list could only ever say
    the narrowed number."""
    driver, reader = badged_app
    conv = ConversationsActions(driver)
    nonce = uuid.uuid4().hex[:8]

    for word in ("alpha", "bravo"):
        conv.inject_and_resolve_thread(
            rail="FaunaMls",
            sender=f"badge-{word}-{nonce}@self-nest.test",
            subject=None,
            body=f"{word}-{nonce} waiting unread",
        )
    total = _total_unread(conv)
    assert total >= 2, "both planted inbounds must be unread in the app's own list"
    got = reader.await_value(_badge(total), timeout=RPC_ROUNDTRIP_S)
    assert got == _badge(total), _why(driver, reader, _badge(total), got)

    conv.navigate()
    conv.search_conversations(f"alpha-{nonce}")
    wait_until(
        lambda: len(conv.list_threads()) == 1,
        RPC_ROUNDTRIP_S,
        interval=0.1,
        diagnose=lambda: f"the search never narrowed the list to one thread: {conv.list_threads()!r}",
    )

    # A plain inject: the new thread does not match the search, so it never
    # enters the narrowed list a resolve would poll — which is the point.
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"badge-charlie-{nonce}@self-nest.test",
        subject=None,
        body=f"charlie-{nonce} arriving while the list is narrowed",
    )
    got = reader.await_value(_badge(total + 1), timeout=RPC_ROUNDTRIP_S)
    assert got == _badge(total + 1), _why(driver, reader, _badge(total + 1), got)


@pytest.mark.feature("home-screen-widget")
def test_signing_out_never_leaves_the_previous_accounts_count_on_the_taskbar_badge(badged_app):
    """Outcome 5 (the cross-app promise in `common.md` § Home-screen widget, the
    mechanism `windows.md` § Home-screen widget → *The badge ends with the
    account*): with an unread message counted on the badge, signing out through
    the Account page takes the count off the taskbar button. The OS keeps a
    badge across process exits and nothing reads the fold after a sign-out, so
    the badge clears only because the actor change ends it — the windows seat
    of the linux badge witness and of apple's and android's snapshot clear."""
    driver, reader = badged_app
    conv = ConversationsActions(driver)
    nonce = uuid.uuid4().hex[:8]

    conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"badge-before-{nonce}@self-nest.test",
        subject=None,
        body="a message counted before signing out",
    )
    counted = _total_unread(conv)
    assert counted >= 1, "the planted inbound must be unread in the app's own list"
    got = reader.await_value(_badge(counted), timeout=RPC_ROUNDTRIP_S)
    assert got == _badge(counted), _why(driver, reader, _badge(counted), got)

    SettingsActions(driver).sign_out()

    got = reader.await_value(None, timeout=RPC_ROUNDTRIP_S)
    assert got is None, (
        f"a signed-out app leaves no count on its taskbar button (it showed {counted}); "
        + _why(driver, reader, None, got)
    )


@pytest.fixture
def badge_other_device(request, test_user, nest_instance):
    """alice's OTHER device for outcome 4, launched on demand — once the replica
    holds the group, so its login restore finds it: a **tui** seat of the same
    account (`_alice_extra_seat`: alice's secret, a distinct device id, a fresh
    empty state root). Yields ``launch() -> (device_b, driver_b)``; every seat it
    launched is torn down at the end.

    **Why tui, not a second windows seat.** The badge is keyed on the app's AUMID,
    and a second FaunaApp started from the same build output runs under the same
    lent identity — so it would push ITS OWN unread total onto the very badge this
    test reads, and the badge would show whichever process wrote last. A real box
    runs one FaunaApp per user (the single-instance gate), so that collision is the
    harness's, never the product's; tui paints no badge (`common.md` § Home-screen
    widget declares it absent), so nothing it does can touch the witness, and the
    read is still a UI gesture on another device of the account. The literal
    ``"tui"`` is what the prebuild hoist (`conftest._CROSS_APP_FIXTURE_APPS`)
    reads, so the tui build runs outside the test's own budget."""
    live: list = []

    def launch():
        gen = _alice_extra_seat(
            "tui", request, test_user, nest_instance,
            screenshot_tag="badge-second-device",
        )
        seat = next(gen)
        live.append(gen)
        return seat

    try:
        yield launch
    finally:
        while live:
            # Runs the launcher's own `finally`: screenshot, then teardown.
            live.pop().close()


def _channel_unread(conv: ConversationsActions, channel_hex: str) -> int | None:
    t = next(
        (
            t
            for t in conv.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )
    return None if t is None else t.unread_count


@pytest.mark.real_conversations
@pytest.mark.feature("home-screen-widget")
def test_a_conversation_read_on_another_device_stops_counting_on_the_taskbar_badge(
    badged_app, badge_other_device, nest_instance, test_user
):
    """Outcome 4 (`common.md` § Home-screen widget; the carrier is
    `conversation-read-state.md` § How the carriers meet the in-memory set: "a
    raise arriving from another device removes every id at or below it"): a
    native message counted on this device's badge stops counting once the same
    account reads it on ANOTHER device — this one never opening the thread.

    The linux witness's journey (`test_home_screen_widget_launcher_badge.py`): a
    real 1:1 sealed by an API-tier `mls-group-gen` sender — an injected mock
    thread has no channel position and no read marker, so nothing another
    device does could reach it — device B restoring device A's group state from
    the replica and decrypting the same channel, the read a UI gesture on
    device B, device A never touched after the message lands. Device B is a tui
    seat (`badge_other_device` says why)."""
    driver, reader = badged_app
    conv = ConversationsActions(driver)
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    nonce = uuid.uuid4().hex[:8]
    body = f"read me on your other device {nonce}"

    # A readiness poll on windows (the real session comes from the launch gate).
    conv.enable_real_faunamls()
    await_account_runtime_assembled(driver)

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    with _alice_client() as c:
        provider_before = c.call("fauna.mls.get", {"path": "provider"}).get("blob")

    # ── A sender seats alice in a 1:1 (the message is held back for now). ──
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to publish a key package",
    )
    my_kp = conv_api.keypackage_fetch(port, sender, me)
    assert my_kp is not None, "alice should have a fetchable key package"
    channel_hex, welcome, (envelope,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), my_kp, [body]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    # The acked Welcome is the join witness: a Welcome-only 1:1 is listed on
    # no rail until its first message.
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S, who="device A")

    # ── Device A's replica now carries the group, so device B can restore it. ──
    wait_rail_blob_settled(
        _alice_client, "fauna.mls.get", "provider",
        superseding=provider_before, timeout=MLS_HANDSHAKE_S,
    )
    device_b, driver_b = badge_other_device()
    # Device B restores — and binds — the channel at launch. The bound set, not
    # the thread list, is the witness: the 1:1 holds only its Welcome yet, so it
    # is listed on no rail until the message lands.
    await_channel_bound(
        driver_b, channel_hex, budget_s=MLS_HANDSHAKE_S,
        who="device B (restoring the sender's 1:1 from the replica)",
    )

    # ── The message lands: unread on both devices, counted on A's badge. ──
    conv_api.channel_send(port, sender, channel_hex, envelope)
    wait_until(
        lambda: _channel_unread(conv, channel_hex) == 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A should hold the message unread; its thread "
        f"reads {_channel_unread(conv, channel_hex)!r}",
    )
    counted = _total_unread(conv)
    got = reader.await_value(_badge(counted), timeout=RPC_ROUNDTRIP_S)
    assert got == _badge(counted), _why(driver, reader, _badge(counted), got)
    wait_until(
        lambda: (_channel_unread(device_b.conversations, channel_hex) or 0) >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B should decrypt the message too; its thread "
        f"reads {_channel_unread(device_b.conversations, channel_hex)!r}",
    )

    # ── Device B reads it, through its own conversations list. ──
    device_b.conversations.navigate()
    device_b.conversations.open_thread_by_channel(channel_hex)

    # ── Device A's badge drops by exactly that thread, A untouched. ──
    got = reader.await_value(_badge(counted - 1), timeout=MLS_HANDSHAKE_S)
    assert got == _badge(counted - 1), _why(driver, reader, _badge(counted - 1), got)
    assert _channel_unread(conv, channel_hex) == 0, (
        "the badge dropped, but device A's own list still counts the thread "
        f"({_channel_unread(conv, channel_hex)!r}) — the two numbers must agree"
    )
