"""tier_3 E2E: linux's home-screen widget — the launcher badge — read from
outside the app.

`docs/features/home-screen-widget.md` outcomes 1 and 2 (and 4, 5, 6 in the
journeys below), on the linux column.
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

import contextlib
import time
import uuid

import pytest

from actions.conversations import ConversationsActions
from actions.settings import SettingsActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from conftest import _alice_extra_seat, _seeded_environment
from drivers import create_driver
from helpers.budgets import MLS_HANDSHAKE_S, RPC_ROUNDTRIP_S
from helpers.launcher_entry import LauncherEntryListener
from helpers.tray_bus import TrayBus
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api

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


@pytest.mark.feature("home-screen-widget")
def test_a_search_in_the_conversations_list_never_changes_the_badge(badged_app):
    """Outcome 6 (`common.md` § Home-screen widget: the account's total, never
    a search-narrowed list's): with a search narrowing the list to one thread,
    a new message raises the badge to the UNFILTERED total. A badge summed over
    the narrowed list could only ever say the narrowed number."""
    driver, listener = badged_app
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
    listener.await_count(total, timeout=RPC_ROUNDTRIP_S)

    conv.navigate()
    conv.search_conversations(f"alpha-{nonce}")
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while len(conv.list_threads()) != 1:
        assert time.monotonic() < deadline, (
            f"the search never narrowed the list to one thread: {conv.list_threads()!r}"
        )
        time.sleep(0.1)

    # A plain inject: the new thread does not match the search, so it never
    # enters the narrowed list a resolve would poll — which is the point.
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"badge-charlie-{nonce}@self-nest.test",
        subject=None,
        body=f"charlie-{nonce} arriving while the list is narrowed",
    )
    update = listener.await_count(total + 1, timeout=RPC_ROUNDTRIP_S)
    assert update.count_visible


@pytest.mark.feature("home-screen-widget")
def test_signing_out_never_leaves_the_previous_accounts_count(badged_app):
    """Outcome 5 (`ios.md` § Home-screen widget, the cross-app promise in
    `common.md`): with an unread message counted on the badge, signing out
    through the Account page takes the count off the launcher icon — the
    badge never keeps saying the signed-out account's number."""
    driver, listener = badged_app
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
    assert listener.await_count(counted, timeout=RPC_ROUNDTRIP_S).count_visible

    SettingsActions(driver).sign_out()

    update = listener.await_count(0, timeout=RPC_ROUNDTRIP_S)
    assert update.app_uri == _APP_URI
    assert not update.count_visible, (
        f"a signed-out app paints no count on its icon: {update!r}"
    )


def _channel_thread(conv: ConversationsActions, channel_hex: str):
    return next(
        (
            t
            for t in conv.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )


def _channel_unread(conv: ConversationsActions, channel_hex: str) -> int | None:
    t = _channel_thread(conv, channel_hex)
    return None if t is None else t.unread_count


@pytest.mark.real_conversations
@pytest.mark.feature("home-screen-widget")
def test_a_conversation_read_on_another_device_stops_counting_on_the_badge(
    badged_app, request, nest_instance, test_user
):
    """Outcome 4 (`common.md` § Home-screen widget; the carrier is
    `conversation-read-state.md` § How the carriers meet the in-memory set: "a
    raise arriving from another device removes every id at or below it"): a
    native message counted on this device's badge stops counting once the same
    account reads it on ANOTHER device — this one never opening the thread.

    A real thread is the point: an injected mock thread has no channel position
    and no read marker, so nothing another device does can reach it. The sender
    is an API-tier `mls-group-gen` engine (the read-state relaunch witness's
    shape); device B is a second linux seat of the same account with an empty
    state root, which restores device A's group state from the replica and so
    decrypts the same channel (`devices.md` § Cross-device MLS group-state
    sync). The read is a UI gesture on device B; device A is never touched
    after the message lands."""
    driver, listener = badged_app
    conv = ConversationsActions(driver)
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    nonce = uuid.uuid4().hex[:8]
    body = f"read me on your other device {nonce}"

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
    conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    wait_until(
        lambda: _channel_thread(conv, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to join the sender's Welcome",
    )

    # ── Device A's replica now carries the group, so device B can restore it. ──
    wait_rail_blob_settled(
        _alice_client, "fauna.mls.get", "provider",
        superseding=provider_before, timeout=MLS_HANDSHAKE_S,
    )
    seat = contextlib.contextmanager(_alice_extra_seat)(
        "linux", request, test_user, nest_instance,
        screenshot_tag="badge-second-device",
    )
    with seat as (device_b, _driver_b):
        wait_until(
            lambda: _channel_thread(device_b.conversations, channel_hex) is not None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: "device B never restored the sender's 1:1 from the "
            f"replica: {[(t.rail, t.channel_id_hex) for t in device_b.conversations.list_threads()]!r}",
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
        update = listener.await_count(counted, timeout=RPC_ROUNDTRIP_S)
        assert update.app_uri == _APP_URI and update.count_visible
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
        update = listener.await_count(counted - 1, timeout=MLS_HANDSHAKE_S)
        assert update.app_uri == _APP_URI
        assert _channel_unread(conv, channel_hex) == 0, (
            "the badge dropped, but device A's own list still counts the thread "
            f"({_channel_unread(conv, channel_hex)!r}) — the two numbers must agree"
        )
