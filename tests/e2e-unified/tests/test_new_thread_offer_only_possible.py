"""The new-thread composer offers only what it can send: Send is disabled and
says to add a recipient until there is someone to send to; once the recipient
is on a network that carries no files or topics, attach and topic are disabled
with that reason; and a send the nest turns away keeps what was typed and says
why.

Goal: ``docs/goal/ui/conversations.md`` § Errors & edge cases → *The new-thread
composer offers only what it can send* (the shared predicate
``new_thread_possibility`` in ``libs/fauna-conversations/src/new_thread_controls.rs``
over ``Send`` / ``Attach`` / ``Topic``; tui's handler twin ``new_thread_control``),
applying ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; rule 4: no
input is spent on something that cannot be saved; rule 7: a refusal is never
silent). The catalog outcome is ``docs/features/conversations.md`` outcome 28.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. The shared predicate's unit tests and tui's in-process walk are
not that witness.

**The reason is read as painted.** ``PlatformDriver.blocked_reason`` answers
what the app shows beside the control, and the journey compares it with the
``S.`` string of the predicate's reason key, never a literal. An app whose
driver cannot read it yet takes the ``blocked-reason-read`` surface gate.

**Pressing a blocked control.** The disabled control IS the refusal: no person
can press it, and the test agent refuses a press on a disabled control
(convention 11), so a journey never presses one. The press half (the handler
refusing with the same reason) is the shared predicate's backstop.

**A recipient on a network with no files or topics.** The rail is decided by
the *deciding recipient*: the first chip, else the typed recipient the picker's
probe resolved. A ``Bridged`` recipient is the one such rail today, and the
picker reaches one only through the nest (``conversation.address.admit`` matches
a serving bridge's declared grammar — no app parses one). The nest's in-process
Nostr leg serves any account with a linked Nostr key and declares the
``npub1…`` grammar, decoding the address locally, so the journey links Nostr
through the Nostr page (the self-serve way ``test_forms_offer_only_possible.py``
does) and types a harness-minted ``npub`` (``helpers/nostr_keys.py``) — no far
network is dialled. The probe has no effect (``conversations.md`` § *The picker
tells the truth*, rule 4): once the address resolves and is a chip, no room is
listed for it — a room is opened only when a new thread is sent, and this
journey sends none. The teardown still sweeps any room for the address over the
user's own wire (fixture cleanup, not a step of the journey) and unlinks Nostr
if the journey linked it.

**A send that fails.** ``helpers/rpc_hold.refuse_rpc`` turns the send's own
request (``fauna.email.send``, the ``Smtp`` rail's send kind —
``Rail::send_wire_kind``) away at the nest without running its handler. The new
thread is materialized before its send (``send_new_thread``), so what stays
open is that thread's composer, with the typed body and the reason on
``error-message``; the journey releases the refusal, presses the same Send and
reads the message go out (the stub external MX receives it, through the
``mail_bridge_mta`` fixture) and the composer empty.

Every act the outcomes name goes through the page (convention 8); the waits are
generous deadlines over a local repaint or a named request, never settle sleeps
(convention 14).
"""

from __future__ import annotations

import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.app_surface import require_surface
from helpers.nostr_keys import new_keypair
from helpers.rpc_hold import refuse_rpc, release_rpc_hold, wait_for_refused_rpc
from helpers.waiting import wait_until
from i18n.strings import S

# `real_conversations`: the send and the bridged probe must run through the
# real shared session's rails, not the mock inject backend some apps launch
# with (tui, linux and web run the real session for every login anyway).
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

EMAIL_SEND = "fauna.email.send"

# Routed to the in-process stub MX by `mail_bridge_mta`.
EXTERNAL_DOMAIN = "external.test"

NOSTR_BRIDGE_ID = "nostr"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _diag(driver, element_id: str, scope: str | None = None) -> str:
    return (
        f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)}, "
        f"{driver.diagnose('error-message')}"
    )


def _blocked_with(driver, element_id: str, reason: str, scope: str | None) -> bool:
    if driver.is_absent(element_id, scope=scope):
        return False
    if driver.is_enabled(element_id, scope=scope):
        return False
    return driver.blocked_reason(element_id, scope=scope) == reason


def _assert_blocked_with(driver, element_id: str, reason: str, *, what: str,
                         scope: str | None = None) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. The poll is a deadline over the repaint that follows the last
    keystroke; the asserts after it name every part that failed (convention 6)."""
    try:
        wait_until(lambda: _blocked_with(driver, element_id, reason, scope),
                   budgets.UI_SETTLE_S)
    except AssertionError:
        pass  # the asserts below say which half failed
    diag = _diag(driver, element_id, scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while it cannot succeed: {diag}"
    )
    painted = driver.blocked_reason(element_id, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _assert_offered(driver, element_id: str, *, what: str, scope: str | None = None) -> None:
    """The precondition is met: the control is live (so the gate follows the
    draft, never a control that stays disabled)."""
    wait_until(
        lambda: driver.is_enabled(element_id, scope=scope),
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"{what}: {element_id} stayed disabled: "
                         f"{_diag(driver, element_id, scope)}",
    )


def _page_error(app) -> str:
    try:
        return app.error_text() or ""
    except Exception:  # noqa: BLE001 - an absent error element reads as none
        return ""


def _fresh_new_conversation(conv) -> None:
    """Open the new-thread composer with nothing in it: a new-thread draft
    survives stepping out (``conversations.md`` § Persistence), so an earlier
    journey's half-written one is discarded through the composer's own Cancel
    first."""
    conv.open_new_conversation()
    conv.cancel_new_conversation()
    conv.open_new_conversation()


def _user_ws(nest_instance, user) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    )


# ── conversations 28 ──────────────────────────────────────────────────────────


@pytest.mark.feature("conversations")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
def test_send_waits_for_a_recipient_and_a_refused_send_keeps_what_was_typed(
    logged_in_app, nest_instance, mail_bridge_mta,
):
    """A new conversation with no recipient: Send is shown, disabled, and says
    to add who it goes to, while attach and topic are live (no rail is chosen
    yet, and the draft holds both whatever it turns out to be). Once a
    recipient is added Send is live. A send the nest turns away keeps the
    composer with the typed text and says why on the page; the same press after
    the nest takes it again sends the message and empties the composer."""
    app = logged_in_app
    d = app.driver
    conv = app.conversations
    require_surface(d, "blocked-reason-read")
    port = nest_instance["port"]

    # 1. Nothing addressed: Send is blocked with its reason; attach and topic
    #    are not (rule 4 — the draft keeps them whatever the rail turns out to be).
    _fresh_new_conversation(conv)
    _assert_blocked_with(d, "dm-send-button", S.conversations.compose.needs_recipient,
                         what="a new conversation with no recipient")
    _assert_offered(d, "attachment-button", what="attach before any recipient")
    _assert_offered(d, "topic-toggle-button", what="topic before any recipient")

    # 2. A recipient added: Send goes live; a mail recipient carries files and
    #    a topic, so attach and topic stay live.
    token = _unique("offer-possible")
    recipient = f"{token}@{EXTERNAL_DOMAIN}"
    conv.add_recipient(recipient)
    _assert_offered(d, "dm-send-button", what="a new conversation with a recipient")
    _assert_offered(d, "attachment-button", what="attach for a mail recipient")
    _assert_offered(d, "topic-toggle-button", what="topic for a mail recipient")

    # 3. The send refused at the nest: the composer stays, with what was typed
    #    and the reason on the page.
    body = f"a message for {token}"
    d.type_text("dm-text-field", body)
    refuse_rpc(port, EMAIL_SEND)
    try:
        d.click("dm-send-button")
        wait_for_refused_rpc(port, EMAIL_SEND)
        error = wait_until(
            lambda: _page_error(app),
            budgets.UI_SETTLE_S,
            diagnose=lambda: "a refused send said nothing on error-message: "
                             f"{_diag(d, 'dm-send-button')}",
        )
        assert d.is_visible("dm-text-field"), (
            f"a refused send closed the composer (conversations.md § Errors & edge "
            f"cases: the composer stays open with what was typed); error={error!r}"
        )
        kept = conv.compose_body_text()
        assert kept == body, (
            f"a refused send must keep what was typed; got {kept!r}, error={error!r}"
        )
    finally:
        release_rpc_hold(port, EMAIL_SEND)

    # 4. The same press once the nest takes the send: it goes out and the
    #    composer empties.
    _assert_offered(d, "dm-send-button", what="the composer after a refused send")
    d.click("dm-send-button")

    def _relayed():
        return any(token.encode() in raw for raw in mail_bridge_mta.stub_mx.messages())

    wait_until(
        _relayed,
        budgets.MAIL_OUTBOUND_CYCLE_S,
        diagnose=lambda: f"the retried send never reached the outside recipient's "
                         f"server; error={_page_error(app)!r}; "
                         f"bridge log: {mail_bridge_mta.log_file}",
    )
    wait_until(
        lambda: conv.compose_body_text() == "",
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"the composer kept its text after the send went out: "
                         f"{conv.compose_body_text()!r}, error={_page_error(app)!r}",
    )
    conv.navigate()


@pytest.mark.feature("conversations")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
def test_attach_and_topic_are_blocked_with_their_reasons_for_a_bridged_recipient(
    logged_in_app, nest_instance, test_user,
):
    """A recipient the picker resolves onto a bridged network (here the nest's
    Nostr leg, by ``npub``) decides a rail that carries neither files nor a
    topic: attach and topic are shown, disabled, each with its reason, while
    Send stays live — there is someone to send to."""
    app = logged_in_app
    d = app.driver
    conv = app.conversations
    nostr = app.nostr
    require_surface(d, "blocked-reason-read")

    # The precondition: a Nostr account is linked, so the nest's Nostr leg
    # serves this account — taken through the Nostr page itself.
    nostr.navigate()
    assert nostr.is_page_visible(), f"nostr page unreachable; error={_page_error(app)!r}"
    we_linked = False
    if not nostr.is_linked():
        nostr.link_generate()
        assert nostr.wait_for_linked(), (
            f"generate-link should link the account; error={nostr.page_error_text()!r}"
        )
        we_linked = True
    _, npub, peer_hex = new_keypair()
    try:
        conv.navigate()
        conv.open_new_conversation()
        conv.cancel_new_conversation()
        state = conv.resolve_recipient(npub)
        assert state == "resolved", (
            f"an npub the linked Nostr leg's grammar admits should resolve as a "
            f"bridged recipient, resolve state {state!r}: "
            f"{d.diagnose('recipient-resolve-status')}, error={_page_error(app)!r}"
        )
        # Commit it as the first chip: the first committed chip is the deciding
        # recipient whatever the probe left on the typed line.
        conv.accept_recipient_chip(npub)
        wait_until(
            lambda: d.count("recipient-picker-chip") >= 1,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"the npub never became a chip: {d.diagnose('recipient-picker-chip')}, "
                             f"error={_page_error(app)!r}",
        )

        _assert_blocked_with(d, "attachment-button",
                             S.conversations.compose.no_attachments_here,
                             what="attach for a bridged recipient")
        _assert_blocked_with(d, "topic-toggle-button",
                             S.conversations.compose.no_topic_here,
                             what="topic for a bridged recipient")
        _assert_offered(d, "dm-send-button", what="Send for a bridged recipient")

        # Rule 4: the keystroke probe asked `conversation.address.admit`, which
        # opens nothing — no room exists for an address nobody has sent to.
        with _user_ws(nest_instance, test_user) as ws:
            rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
        probed = [r for r in rooms if r.get("bridge_id") == NOSTR_BRIDGE_ID and (
            peer_hex in r.get("participants", []) or r.get("far_room_id") == peer_hex)]
        assert not probed, (
            f"the recipient probe opened a bridged room for an address nobody sent to: {probed!r}"
        )

        conv.cancel_new_conversation()
    finally:
        # Fixture cleanup, not a step of the journey: a room for the minted
        # address (none, unless a regression let the probe open one) is removed
        # over the user's own wire so the shared session's list is as found.
        with _user_ws(nest_instance, test_user) as ws:
            rooms = ws.call("fauna.bridges.conversation.rooms.list", {})["rooms"]
            for room in rooms:
                if room.get("bridge_id") == NOSTR_BRIDGE_ID and (
                    peer_hex in room.get("participants", [])
                    or room.get("far_room_id") == peer_hex
                ):
                    ws.call("fauna.bridges.conversation.rooms.remove",
                            {"room_id": room["room_id"]})
        if we_linked:
            nostr.navigate()
            nostr.unlink()
            nostr.wait_for_unlinked()
        conv.navigate()
