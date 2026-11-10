"""The running app raises a system notification for a new message — except in
the conversation you already have open.

`docs/features/conversations.md` outcome 11, which until this file was witnessed
on **no column at all**, on either of the two apps that had built the firing.
Authority: `docs/goal/ui/conversations.md` § Where logic lives — the *when /
for-whom* decision is the shared `MessageNotificationTracker`'s and has exactly
three rules (seed silently on the first non-empty snapshot; fire when a thread's
`unread_count` rises, a brand-new thread with unread messages included, while its
newest activity is at or past the launch floor; suppress the selected
thread), while the *firing* is the only per-app glue.

**What is asserted, and what deliberately is not.** The app appends every banner
it hands to the platform's toast API to the shared fired-banner log, published as
`fauna_e2e_agent::MESSAGE_BANNERS_KEY`; this file reads that log. It never looks
at the OS notification centre — that is the last inch (does the toast *appear*,
and look right), not the mechanism, and no test on any platform can read it
anyway. The log is appended at the firing site, after every suppression the app
applies, so it means "the user was shown this", not "the tracker returned this":
glue that swallowed a decision still fails here.

**Latency independence (convention 14).** Two of the three rules are *negative* —
a banner that must NOT appear — and there is nothing on screen to watch for its
non-arrival. Every negative read is therefore anchored on
`waiting.await_banner_pass_after`, which waits for a diff tick that *began* after
the message was planted to finish; no wall-clock sleep appears here, and none
may be added (a timing-keyed test of this shape is defunct, not flaky).

**Why one journey rather than three test functions.** The tracker is stateful
across snapshot ticks and the app process is module-scoped, so the three rules
are three states of one machine reached in order: the login load must be silent
*before* the tracker is seeded, and the open-thread suppression is only
meaningful *after* a fire has been proven possible. Split across functions they
would share the same live tracker anyway, in whatever order pytest picked.
"""

from __future__ import annotations

import uuid

import pytest

from helpers import waiting
from helpers.app_log_section import app_text
from helpers.app_surface import app_name, launched_as_a_bundle, skip_environment


def _why(driver):
    """Extra diagnosis for a banner that did not fire (convention 6).

    A missing banner has two very different causes — the shared tracker decided
    against it, or the app's firing glue dropped what it decided — and the log
    alone cannot tell them apart. Every app's firing path says so in the app's
    own words when it declines or drops a fire (apple's
    `MessageBannerObserver` logs `[banner] not raised …` at `.warn`), so hand
    the matching lines to the failure instead of making the next reader re-run
    a 25-minute suite to see them.

    Read through `app_log_section.app_text`, which is the one place the
    `app_log_text` / `app_stderr_text` pair is walked — every driver family
    keeps the app's own words under one of those two names, so this filter
    reaches all of them without any driver growing a third spelling of it.
    """
    lines = [ln for ln in app_text(driver).splitlines() if "banner" in ln.lower()]
    # The tracker fires only on a rise of a thread's unread count whose activity
    # is at or past the launch floor: a thread reading 0 unread never rose, one
    # reading ≥1 that fired nothing was held under the floor.
    try:
        threads = driver.get_state("data.conversation_threads") or []
        unread = {t.get("thread_id"): t.get("unread_count") for t in threads}
        selected = driver.get_state("data.selected_thread_id")
    except Exception as exc:  # never let the diagnostic mask the real assert
        unread, selected = f"(unreadable: {exc!r})", None
    why = f"\nThreads' unread counts: {unread}; selected thread: {selected!r}"
    return why + (f"\nApp log (banner lines): {lines[-5:]}" if lines else "")


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

# Every app has built the *firing* leg and publishes the log. The one short
# column is a LAUNCH SHAPE, not an app: macOS shares every line of the firing leg
# with `ios` (both are FaunaKit's `MessageBannerObserver`), and what decides its
# column is run-level rather than per-app. See `helpers.app_surface.launched_as_a_bundle`.


#: Why the macOS bare-binary column is short, in the goal doc's own words. The
#: pointer is the status row itself — `conversations.md` § Implementation status
#: today, the *DM OS-toast firing* row.
_BARE_BINARY = (
    "the firing leg IS built and IS witnessed — by `--app ios` on the same "
    "shared FaunaKit code, and by replaying THIS module run-level under "
    "`--macos-artifact`. An ordinary `--app macos` run launches the BARE "
    "`FaunaMacOS` binary, and a non-bundle process has no notification host "
    "at all (`NotificationHost.isAvailable`), so the app hands no banner to "
    "the platform and honestly records none. Not app work and not debt: "
    "replay it with `pytest tests/e2e-unified/tests/"
    "test_conversations_message_banner.py --app macos --macos-artifact` "
    "(conversations.md § Implementation status today, DM OS-toast firing)"
)


def _require_banner_leg(driver):
    """Skip with a declared reason when the launch shape cannot fire a banner."""
    if app_name(driver) == "macos" and not launched_as_a_bundle(driver):
        skip_environment(_BARE_BINARY)


@pytest.mark.feature("conversations")
def test_new_message_raises_a_banner_except_in_the_open_thread(logged_in_app):
    """The three rules of the shared `MessageNotificationTracker`, end to end.

    1. The threads already there when you signed in raise nothing.
    2. A message in a thread you do not have open raises a banner naming it.
    3. A message in the thread you *do* have open raises nothing.
    """
    d = logged_in_app.driver
    conv = logged_in_app.conversations
    _require_banner_leg(d)

    nonce = uuid.uuid4().hex[:8]

    # ── Rule 3: the login load is silent ────────────────────────────────────
    # Whatever this (session-scoped, state-accumulating) account already held,
    # the app's module-boundary cold relaunch means the log starts empty and the
    # login load is the first batch this tracker ever sees. It must have seeded
    # silently — a toast storm on sign-in is the failure this rule exists to
    # stop.
    threads_at_login = conv.list_threads()
    banners = waiting.require_message_banners(d)
    assert banners["fired"] == [], (
        "signing in raised "
        f"{len(banners['fired'])} new-message banner(s) for the "
        f"{len(threads_at_login)} thread(s) already in this account "
        f"({banners['fired']}). Threads present at login are not new — the "
        "shared tracker seeds silently on its first non-empty snapshot."
    )

    # ── Seed the tracker, and assert the seeding batch stayed silent ────────
    # After this inject the tracker is certainly seeded, whichever way the
    # account was loaded. WHICH snapshot seeded it is the tracker's to say, not
    # the thread list read above: the rails load asynchronously, so a list read
    # before the login load landed looks empty while the load seeds the tracker
    # a moment later — and `alpha` is then genuinely new, rightly bannered (a
    # 2026-10-12 sweep red read exactly that as a seed that fired). So the
    # silent-seed rule is asserted against the batch the tracker names.
    planted = banners["started"]
    alpha = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"alpha-banner-{nonce}@self-nest.test",
        subject=None,
        body="the thread that seeds the tracker",
    )
    waiting.await_banner_pass_after(d, planted, what="the seeding thread")
    seeded_with = waiting.require_message_banners(d).get("seeded_with")
    assert seeded_with, (
        "the banner log names no seeding batch after a non-empty snapshot "
        f"({waiting.message_banners(d)}) — `message_banners_json` owes "
        "`seeded_with` (fauna_conversations::notification)."
    )
    fired_from_seed = [t for t in waiting.fired_banner_threads(d) if t in seeded_with]
    assert fired_from_seed == [], (
        f"thread(s) {fired_from_seed} of the snapshot that seeded the tracker "
        f"raised a banner (seeding batch: {seeded_with}). The seeding snapshot "
        "is silent — the login load is not news."
    )

    # ── Rule 1: a message in a thread you do not have open raises a banner ──
    before = waiting.fired_banner_threads(d)
    planted = waiting.message_banners(d)["started"]
    beta = conv.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"beta-banner-{nonce}@self-nest.test",
        subject=None,
        body="a message you are not looking at",
    )
    assert beta != alpha, "the two injects merged into one thread — nonce collision"
    waiting.await_banner_pass_after(d, planted, what="the unopened thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [beta], (
        "a new message in a thread that is NOT open must raise exactly one "
        f"banner for that thread. Fired since the plant: {fired[len(before):]}; "
        f"expected [{beta!r}]. Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # ── Rule 2: a message in the thread you have open raises nothing ────────
    # Opening it is a real UI gesture (convention 8), which is also what puts
    # the thread in the snapshot's `selected_thread_id` the tracker suppresses.
    conv.open_thread_by_id(beta)
    before = waiting.fired_banner_threads(d)
    planted = waiting.message_banners(d)["started"]
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"beta-banner-{nonce}@self-nest.test",
        subject=None,
        body="a message in the conversation you are already reading",
    )
    waiting.await_banner_pass_after(d, planted, what="the open thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired == before, (
        "a new message in the thread the user ALREADY HAS OPEN must raise no "
        f"banner — the user is looking at it. Fired since the plant: "
        f"{fired[len(before):]}. Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # The suppression must be the *open thread*, not the app being frontmost:
    # prove the tracker is still live by landing one more banner on the other
    # thread while `beta` stays open. Without this the assertion above would
    # also pass on an app that had simply stopped firing altogether.
    planted = waiting.message_banners(d)["started"]
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=f"alpha-banner-{nonce}@self-nest.test",
        subject=None,
        body="still talking over here",
    )
    waiting.await_banner_pass_after(d, planted, what="the other thread's message")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [alpha], (
        "with one thread open, a message in a DIFFERENT thread must still raise "
        f"its banner. Fired since the plant: {fired[len(before):]}; expected "
        f"[{alpha!r}]. If this is empty while the previous assertion passed, the "
        "app has stopped firing entirely rather than suppressing the open "
        f"thread. Full log: {waiting.message_banners(d)}{_why(d)}"
    )




def _native_thread(conv, channel_hex: str):
    """The FaunaMls thread for ``channel_hex`` in the app's current list, or
    ``None`` — resolved by the channel id, which survives the reload an
    identity switch performs (the app-assigned ``thread_id`` need not)."""
    return next(
        (
            t
            for t in conv.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )


@pytest.mark.feature("conversations")
@pytest.mark.real_conversations
def test_switching_identity_raises_no_banner_for_the_incoming_identitys_threads(
    logged_in_app, nest_instance, test_user, request
):
    """``conversations`` outcome 21 — ``message-banners.md`` § Where logic lives:
    an identity change reseeds the tracker, so the incoming identity's restored
    threads are not news.

    The signed-in identity holds two native 1:1 threads with an unread message
    each, delivered through the nest by two API-tier senders after this launch
    (so their activity is past the launch floor, and an unseeded tracker WOULD
    banner them). The app switches to a second identity and back, in the same
    process (the session's own nest: no relaunch), and the restored threads
    raise nothing; a genuinely new message afterwards still raises its banner,
    which proves the tracker is live rather than silent altogether. The
    fired-banner log is process-wide, so it spans both switches.

    The new message is a third sender's first message, minted only once the
    switch back has finished. A message minted with the planted ones and posted
    later would carry the sender stamp of its minting, below the floor the
    switch back reset (``message-banners.md``: the floor is compared with the
    thread's newest stamp, so a backdated stamp raises no banner), and the
    positive assertion would read the declared under-report instead of the
    tracker; minting after the switch keeps the stamp past the floor however
    long the switch took.

    The threads are REAL nest-held channels, never the in-memory test inject:
    an identity switch rebuilds the conversations state from the incoming
    identity's store and the nest (tui replaces ``ConversationsState`` at both
    ends of an identity's life), so an injected thread — which lives only in
    the outgoing manager's memory — has nothing to restore from, and the
    no-banner read after the switch back would read an empty list.
    """
    from common.auth import create_actor_and_register
    from helpers.budgets import MLS_HANDSHAKE_S
    from helpers.e2e_session import e2e_login_device_id, login_as
    from tests.api import conv_api

    d = logged_in_app.driver
    conv = logged_in_app.conversations
    _require_banner_leg(d)
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    conv.enable_real_faunamls()
    waiting.await_account_runtime_assembled(d)

    def _open_thread_from_a_new_sender(body: str) -> str:
        """A fresh sender opens a 1:1 with the app's user and posts ``body``,
        minted now; returns the channel hex."""
        sender = create_actor_and_register(
            port, admin_signing_key=nest_instance["admin"]["signing_key"]
        )
        # Each fetch consumes one key package: wait for one per sender.
        waiting.wait_until(
            lambda: conv_api.keypackage_count(port, test_user, me) > 0,
            MLS_HANDSHAKE_S,
            diagnose=lambda: "waiting for the app to publish a key package",
        )
        kp = conv_api.keypackage_fetch(port, sender, me)
        assert kp is not None, "the app's user should have a fetchable key package"
        channel_hex, welcome, msgs = conv_api.mint_group_welcome_with_messages(
            bytes(sender["signing_key"]), kp, [body]
        )
        conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
        inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
        conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
        conv_api.channel_send(port, sender, channel_hex, msgs[0])
        return channel_hex

    # The outgoing identity's threads: two senders, one unread message each.
    planted = waiting.require_message_banners(d)["started"]
    first_hex = _open_thread_from_a_new_sender("unread before the switch")
    second_hex = _open_thread_from_a_new_sender("also unread before the switch")
    assert first_hex != second_hex, "two senders' 1:1s collapsed into one channel"

    def _both_unread() -> bool:
        a, b = _native_thread(conv, first_hex), _native_thread(conv, second_hex)
        return bool(a and b and a.unread_count and b.unread_count)

    waiting.wait_until(
        _both_unread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "the two planted threads never showed unread before the switch; "
            f"threads: {[(t.channel_id_hex, t.unread_count) for t in conv.list_threads()]}"
        ),
    )
    waiting.await_banner_pass_after(d, planted, what="the threads planted before the switch")

    def _login(user, handle):
        login_as(
            logged_in_app,
            nest_instance,
            user,
            handle=handle,
            device_id=e2e_login_device_id(logged_in_app),
            request=request,
            spa_url_fixture="spa_url",
        )
        conv.enable_real_faunamls()
        waiting.await_account_runtime_assembled(d)

    # Switch to a second identity: whatever it holds is its login load.
    other = create_actor_and_register(
        port,
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    before = waiting.fired_banner_threads(d)
    planted = waiting.message_banners(d)["started"]
    _login(other, other["handle"])
    waiting.await_banner_pass_after(d, planted, what="the second identity's login load")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [], (
        "switching to another identity raised banner(s) "
        f"{fired[len(before):]} — its threads are its login load, not news. "
        f"Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # Switch back: the first identity's threads restore, unread, silently.
    planted = waiting.message_banners(d)["started"]
    _login(test_user, test_user.get("handle") or "e2e-user")
    restored: dict = {}

    def _restored():
        nonlocal restored
        restored = {
            t.channel_id_hex: t.unread_count
            for t in conv.list_threads()
            if t.rail == "FaunaMls"
        }
        return bool(restored.get(first_hex)) and bool(restored.get(second_hex))

    waiting.wait_until(
        _restored,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the switched-back identity's channels {first_hex} and {second_hex} "
            "never restored holding their unread message (the read positions "
            "decide unread once they arrive — conversation-read-state.md § How "
            "the carriers meet the in-memory set), so the no-banner assertion "
            f"would read nothing; FaunaMls channels (hex → unread): {restored}"
        ),
    )
    waiting.await_banner_pass_after(d, planted, what="the restored threads' snapshot")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [], (
        "switching back raised banner(s) for the restored threads "
        f"{fired[len(before):]} — an identity change reseeds the tracker, and "
        f"restored threads are not news. Full log: {waiting.message_banners(d)}{_why(d)}"
    )

    # A genuinely new message after the switch still raises its banner —
    # minted now, so its stamp is past the floor the switch back reset.
    planted = waiting.message_banners(d)["started"]
    new_hex = _open_thread_from_a_new_sender("a new message after the switch")
    waiting.wait_until(
        lambda: (_native_thread(conv, new_hex) or _ThreadAbsent).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the app never decrypted the message sent after the switch",
    )
    new_thread = _native_thread(conv, new_hex).thread_id
    waiting.await_banner_pass_after(d, planted, what="the message after the switch")
    fired = waiting.fired_banner_threads(d)
    assert fired[len(before):] == [new_thread], (
        "a new message after the identity switch must raise exactly one banner "
        f"for its thread. Fired since the switch: {fired[len(before):]}; expected "
        f"[{new_thread!r}]. Full log: {waiting.message_banners(d)}{_why(d)}"
    )


class _ThreadAbsent:
    """Stand-in for a thread not (yet) in the list: reads zero messages."""

    message_count = 0
