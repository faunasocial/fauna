"""The apple home-screen widget shows the app's own unread count, and keeps it current.

`apps/common.md` § Home-screen widget: a Fauna widget on the platform's home
screen shows how many unread messages are waiting — **the number the app's own
conversations list shows**, the shared `fauna_conversations` fold summed, never a
per-app tally or a second count query — and keeps that count current in the
background without the app being opened.

**How the widget is observed.** A widget lives outside the app's automation
surface, so these tests never ask the app what its widget shows. The apple
widget is a sandboxed extension holding no credential, and android's Glance
widget never computes a count either; each renders the snapshot the app
publishes for it (`WidgetUnreadPublisher` → `UnreadSnapshotStore`, one file
format on both), so the witness reads that snapshot from outside the app —
straight off disk on apple, through adb `run-as` on android
(`helpers.home_screen_widget`, `driver.widget_dir()`). What the widget paints
for a given snapshot is pinned headlessly beside the widget code
(`HomeScreenWidgetTests` under `just swift-test`, `WidgetUnreadPublisherTest`
under `just android-host-test`); these tests prove the app's own number reaches
the file the widget reads, and moves with it.

**The oracle is the app's own list, never a constant** — the linux witness's
shape (`test_home_screen_widget_launcher_badge.py`): the session-scoped account
may already hold unread threads from earlier tests, so every assertion compares
the snapshot against the app's thread list summed at that moment.

**Marked `macos` / `ios` / `android` — the apps whose driver can read the
snapshot** (`feature-catalog.md` § Cell semantics, the marked-witness rule).
Outcome 2's test here is `macos` only: macOS keeps the app — and so the
conversations observer that publishes — running with its window closed, which is
exactly what it proves. iOS's and android's background legs are tasks the OS
alone schedules (a `BGAppRefreshTask`, a WorkManager periodic worker), so a
foreground run here would prove the wrong mechanism; their witness is
`test_home_screen_widget_background_refresh.py`, which drives that task's
production body through the convention-14 poke.
"""

from __future__ import annotations

import uuid

import pytest

from actions import ActionLayer
from actions.conversations import ConversationsActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from common.launch_harness import reached_authenticated_app
from conftest import _login_app_as, _seeded_environment
from drivers import create_driver
from helpers.budgets import MLS_HANDSHAKE_S, RPC_ROUNDTRIP_S
from helpers.home_screen_widget import (
    app_unread_total,
    await_widget_converged,
    plant_unread,
    widget_snapshot,
)
from helpers.waiting import await_channel_bound, wait_rail_blob_settled, wait_until
from tests.api import conv_api

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_the_widget_shows_the_apps_own_unread_count(logged_in_app):
    """A message you have not read → the widget's number is the list's total."""
    conv = ConversationsActions(logged_in_app.driver)
    before = app_unread_total(conv)
    plant_unread(conv, "one")
    await_widget_converged(logged_in_app.driver, conv, above=before)


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_a_search_in_the_conversations_list_never_changes_the_widget_count(logged_in_app):
    """Outcome 6 (`common.md` § Home-screen widget: the account's total, never a
    search-narrowed list's): with a search narrowing the list to one thread, a
    new message raises the snapshot to the UNFILTERED total. A count summed over
    the narrowed list could only ever say the narrowed number. The widget apps'
    seat of the linux badge witness (`test_home_screen_widget_launcher_badge.py`),
    read off the snapshot file the widget renders."""
    driver = logged_in_app.driver
    conv = ConversationsActions(driver)
    nonce = uuid.uuid4().hex[:8]

    before = app_unread_total(conv)
    for word in ("alpha", "bravo"):
        conv.inject_and_resolve_thread(
            rail="FaunaMls",
            sender=f"widget-{word}-{nonce}@self-nest.test",
            subject=None,
            body=f"{word}-{nonce} waiting unread",
        )
    total = await_widget_converged(driver, conv, above=before)

    conv.navigate()
    conv.search_conversations(f"alpha-{nonce}")
    wait_until(
        lambda: len(conv.list_threads()) == 1 or None,
        RPC_ROUNDTRIP_S,
        interval=0.1,
        diagnose=lambda: f"the search never narrowed the list to one thread: {conv.list_threads()!r}",
    )

    # A plain inject: the new thread does not match the search, so it never
    # enters the narrowed list a resolve would poll — which is the point.
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"widget-charlie-{nonce}@self-nest.test",
        subject=None,
        body=f"charlie-{nonce} arriving while the list is narrowed",
    )
    wait_until(
        lambda: (widget_snapshot(driver) or {}).get("count") == total + 1 or None,
        RPC_ROUNDTRIP_S,
        interval=0.3,
        diagnose=lambda: (
            f"widget snapshot {widget_snapshot(driver)!r}, wanted the unfiltered "
            f"total {total + 1} while the list shows one thread"
        ),
    )


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_signing_out_never_leaves_the_previous_accounts_count_on_the_widget(logged_in_app):
    """Outcome 5 (`ios.md` § Home-screen widget, the cross-app promise in
    `common.md`): with an unread message counted on the widget, signing out
    through the Account page takes the snapshot away — the widget never keeps
    saying the signed-out account's number. The widget apps' seat of the linux
    badge witness (android: `AccountSettingsVM.signOut` →
    `WidgetDataWorker.refoldForAccountSwitch` → `WidgetUnreadPublisher.clear`;
    apple: the teardown's `ActorScope` drop → `WidgetUnreadPublisher.clear`)."""
    driver = logged_in_app.driver
    conv = ConversationsActions(driver)

    before = app_unread_total(conv)
    plant_unread(conv, "signout")
    counted = await_widget_converged(driver, conv, above=before)
    assert counted >= 1

    logged_in_app.settings.sign_out()

    def _cleared():
        snap = widget_snapshot(driver)
        return True if snap is None or snap.get("count") == 0 else None

    wait_until(
        _cleared,
        RPC_ROUNDTRIP_S,
        interval=0.3,
        diagnose=lambda: (
            f"after sign-out the widget snapshot still reads {widget_snapshot(driver)!r} "
            f"(was {counted})"
        ),
    )


@pytest.mark.real_conversations
@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
def test_a_conversation_read_on_another_device_stops_counting_on_the_widget(
    real_faunamls_app, request, nest_instance, test_user
):
    """Outcome 4 (`common.md` § Home-screen widget; the carrier is
    `conversation-read-state.md` § How the carriers meet the in-memory set): a
    native message counted on the widget stops counting once the same account
    reads it on ANOTHER device — this one never opening the thread. The widget
    apps' seat of the linux badge witness
    (`test_home_screen_widget_launcher_badge.py`): a real 1:1 sealed by an
    API-tier sender, device B the seat `alice_second_device` pairs with this app
    (it restores the group from the replica and decrypts the same channel), the
    read a UI gesture on device B, the observable the snapshot file (read off
    disk on apple, through adb `run-as` on android)."""
    driver = real_faunamls_app.driver
    conv = ConversationsActions(driver)
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    body = f"read me on your other device {uuid.uuid4().hex[:8]}"

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    with _alice_client() as c:
        provider_before = c.call("fauna.mls.get", {"path": "provider"}).get("blob")

    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to publish a key package",
    )
    my_kp = conv_api.keypackage_fetch(port, sender, me)
    assert my_kp is not None, "alice should have a fetchable key package"
    channel_hex, welcome, (envelope,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), my_kp, [body]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S, who="the app")

    # The replica now carries the group, so the second device can restore it.
    wait_rail_blob_settled(
        _alice_client, "fauna.mls.get", "provider",
        superseding=provider_before, timeout=MLS_HANDSHAKE_S,
    )
    device_b, driver_b = request.getfixturevalue("alice_second_device")
    await_channel_bound(
        driver_b, channel_hex, budget_s=MLS_HANDSHAKE_S,
        who="device B (restoring the sender's 1:1 from the replica)",
    )

    conv_api.channel_send(port, sender, channel_hex, envelope)

    def _unread(c):
        t = next(
            (t for t in c.list_threads()
             if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex),
            None,
        )
        return None if t is None else t.unread_count

    wait_until(
        lambda: _unread(conv) == 1 or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the app should hold the message unread; its thread reads {_unread(conv)!r}",
    )
    counted = await_widget_converged(driver, conv, above=0)
    wait_until(
        lambda: (_unread(device_b.conversations) or 0) >= 1 or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device B should decrypt the message too; reads {_unread(device_b.conversations)!r}",
    )

    device_b.conversations.navigate()
    device_b.conversations.open_thread_by_channel(channel_hex)

    wait_until(
        lambda: (widget_snapshot(driver) or {}).get("count") == counted - 1 or None,
        MLS_HANDSHAKE_S,
        interval=0.3,
        diagnose=lambda: (
            f"widget snapshot {widget_snapshot(driver)!r}, wanted {counted - 1} "
            "after device B read the thread"
        ),
    )
    assert _unread(conv) == 0, (
        "the widget dropped, but the app's own list still counts the thread "
        f"({_unread(conv)!r}) — the two numbers must agree"
    )


@pytest.fixture
def macos_app_alone(request, nest_instance, test_user):
    """A logged-in macOS app of this test's own: the test closes its window,
    which must not strand the session-shared app every later test drives."""
    driver = create_driver("macos")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": request.getfixturevalue("macos_app_path"),
            "environment": _seeded_environment(request, nest_instance),
        })
        app = ActionLayer(driver)
        _login_app_as(app, request, nest_instance, test_user)
        yield app
    finally:
        driver.teardown()


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
def test_the_count_keeps_itself_current_without_opening_the_app(macos_app_alone):
    """The window closed, nothing on screen — a new arrival still reaches the widget.

    On macOS a window close is never a quit (`apps/macos.md` § Sync), so the app,
    its receive path and the conversations observer that publishes the count all
    keep running; that residency is what "without opening the app" rests on here.
    """
    driver = macos_app_alone.driver
    conv = ConversationsActions(driver)

    before = app_unread_total(conv)
    plant_unread(conv, "open")
    shown = await_widget_converged(driver, conv, above=before)

    driver.window_close()
    assert driver.is_app_alive(), "a macOS window close must keep the app running"

    plant_unread(conv, "closed")
    await_widget_converged(driver, conv, above=shown)


@pytest.mark.feature("home-screen-widget")
@pytest.mark.macos
def test_an_autostart_launch_is_resident_with_no_window_and_keeps_the_count_current(
    macos_app_alone,
):
    """The sign-in leg of "without opening the app": the login launch comes up
    signed in with no main window, and a new arrival still reaches the widget.

    `apps/macos.md` § App Lifecycle → *Auto-start at sign-in*: the registered
    LaunchAgent starts the app with `--autostart`, and an auto-start launch that
    lands authenticated is resident — menu bar, no window. The OS registration
    itself is gated off under e2e (its truth table is `AutoStartTests` under
    `just swift-test`); this drives the launch the job would make, argv and all.
    """
    driver = macos_app_alone.driver
    assert driver.preserve_state_across_relaunch(), (
        "the relaunch must come back signed in, or it routes to onboarding — "
        "which is loud by design"
    )
    assert driver.relaunch_with_args("--autostart"), "the --autostart relaunch did not come up"
    reached_authenticated_app(driver, timeout=90)

    wait_until(
        lambda: driver.visible_windows() == [] or None,
        RPC_ROUNDTRIP_S,
        interval=0.3,
        diagnose=lambda: f"main windows still on screen: {driver.visible_windows()!r}",
    )
    assert driver.is_app_alive(), "the hidden auto-start launch must stay resident"

    conv = ConversationsActions(driver)
    before = app_unread_total(conv)
    plant_unread(conv, "autostart")
    await_widget_converged(driver, conv, above=before)
    assert driver.visible_windows() == [], "a new arrival must not raise the window"
