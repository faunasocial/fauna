"""tier_3: the **conversations oracle's send gates** — the LLM's send is refused
wherever your own send would be, your half-written draft untouched, and its
operation ceiling holds across a restart of the serving app
(``docs/features/connected-apps.md`` outcome 51;
``docs/goal/behavior/conversations-oracle.md`` § The two classes — the
``conversation.send`` row: *the room is one the account can send in under the
page's own capability vector*; § The ceiling and the page: *the
``RateWindow`` kept on the custodian's own per-principal row in the app's
account-scoped store, so it binds across restarts and across the apps that take
turns serving*; § Don't do these: *a half-written draft under the user's cursor
survives an LLM send*; § Implementation status today: ``ConversationsManager::
send_text`` gated on ``can_send`` and the guardian's ``Blocked``, the draft
untouched, the ceiling's row in the account store's replica-local meta rows).
The ceiling's numbers are ``key-material-hierarchy.md`` § *The oracle*'s
(``fauna_core::identity_op::{OPS_PER_WINDOW, WINDOW_SECS}``: 60 operations per
60-second fixed window, per principal).

The app under test is the custodian — tui, the linux app, or the macOS app (never iOS:
``conversations-oracle.md`` § The custodian and the door) — alice's seat on
the consent nest, as in
``test_cli_conversations.py``, whose fixture and door helpers this module
adopts. The room the gates are proven on is a **bridged room**, made the way a
bridge makes one (a consented bridge principal deposits a sealed message —
``test_bridged_conversation.py``'s helpers), because it is the room whose send
the user can turn off from the app: revoking the bridge on Connected apps leaves
the room readable and never writable (``apps/bridges.md`` § Phase G → *When the
bridge stops serving*; ``RoomSnapshot::gate`` withholds ``can_send``). One
journey:

- tui approves the cli's card; alice opens the bridged room R and types a
  half-written draft into its composer, never sending it;
- **served room** — the cli's ``conversations_send`` into R is admitted: the
  bridge drains it and opens it under its holder key, the message is alice's
  own on tui, and R's composer still holds the draft, character for character;
- alice revokes the bridge on tui; R's page withholds the send
  (``dm-text-field`` / ``dm-send-button`` disabled, the ``bridge-disconnected``
  notice) — **and so does the door**: the cli lists R with ``can_send`` false and
  its send into R is refused with the page's own sentence, nothing new reaching
  the thread, the draft still untouched;
- **ceiling across a restart** — the cli reads until the ceiling refuses, tui
  is quit and relaunched on its own store (``preserve_state_across_relaunch``,
  the same device restarting), and the first operation the relaunched custodian
  serves is refused by the SAME window. The window is a fixed 60 s, so a
  relaunch that outlasts it legitimately admits the next operation; that
  reading is checked for consistency (an admit before the refused window's
  reported end is a reset ceiling, red) and the leg re-arms, never passing
  vacuously.

**Not covered here: the hand-over between two serving apps.** It needs a second
app process on the same device mounting the same account store and private
runtime dir while the first signs out; the harness isolates every launch
(convention 10), so no seat shape shares a store today. The two-runtime
hand-over over one endpoint is tier_1 (``fauna_client_conversations_oracle::
serve``'s tests); the guardian's ``Blocked`` arm needs a family-supervised
account and a guardian verdict, a journey of its own.
"""

import os
import re
import time
from types import SimpleNamespace

import pytest
from cryptography.hazmat.primitives.asymmetric import ed25519

import fauna_ffi
from drivers.cli_mcp import CliMcpSession
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_spa_url,
)
from helpers.budgets import (
    APP_RELAUNCH_S,
    ORCHESTRATION_STEP_S,
    RECEIVE_CYCLE_S,
    RPC_ROUNDTRIP_S,
    UI_SETTLE_S,
)
from helpers.client_metadata_server import (
    CLI_CLIENT_ID,
    CLI_CLIENT_NAME,
    CLI_DOCUMENT_FILE,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api.test_bridged_conversation_kinds import _ok
from tests.test_bridged_conversation import (
    BRIDGE,
    PUBLISHER,
    REDIRECT_URI,
    SCOPE,
    _consented_bridge,
    _deposit,
    _page_error,
    _rooms,
)
from tests.test_cli_conversations import (  # noqa: F401 — `custodian` is a pytest fixture adopted by import
    _door_location,
    _until_served,
    custodian,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

#: Where the bridge's own client document is served — a path of its own beside
#: the cli's, on its own publisher host (``ClientMetadataServer`` shares paths
#: across hosts).
_BRIDGE_DOCUMENT_PATH = "/bridge-client-metadata.json"

INBOUND_BODY = "a bridged hello the assistant answers"
DRAFT = "half-written, not yet sent: meet at"
SENT = "sent by an assistant into the served room"
REFUSED = "an assistant's send the page would refuse"

#: The page's own sentence for a room that takes no send
#: (``fauna_client_conversations_oracle``'s ``send``, ``BackendError::NotSupported``).
_PAGE_REFUSAL = "does not take messages right now"
#: The ceiling's typed refusal (``fauna_ipc::oracle::OracleRefusal::CeilingReached``).
_CEILING = re.compile(r"too many conversation operations; try again in (\d+) seconds")
#: Reads the burn may issue before the ceiling must have refused — three full
#: windows, so a window that rolls mid-burn still ends in a refusal. A safety
#: ceiling on the loop, never the assertion.
_BURN_CAP = 3 * 60
#: A relaunch that outlasts the 60 s window re-arms the leg; this many tries.
_RESTART_TRIES = 3
#: Slack on the window's end: the refusal reports whole seconds from the
#: custodian's truncated clock.
_CLOCK_SLACK_S = 2.0


@pytest.fixture(scope="module")
def metadata(tmp_path_factory):
    """One loopback HTTPS server for both third parties this journey consents:
    the cli's published document at its own host and path, and the bridge's
    signed manifest under ``example.com``."""
    import urllib.parse

    cli_url = urllib.parse.urlsplit(CLI_CLIENT_ID)
    server = ClientMetadataServer(
        cli_url.hostname, tmp_path_factory.mktemp("send-gates-metadata"), extra_hosts=(PUBLISHER,)
    )
    server.serve_file(cli_url.path, CLI_DOCUMENT_FILE.read_bytes())
    assert server.client_id(cli_url.path) == CLI_CLIENT_ID
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(PUBLISHER, publisher_key, [])
    payload["bridge"] = BRIDGE
    bridge_id = server.client_id(_BRIDGE_DOCUMENT_PATH, host=PUBLISHER)
    server.documents[_BRIDGE_DOCUMENT_PATH] = client_document(
        bridge_id, REDIRECT_URI, SCOPE, manifest_jws=sign_manifest(publisher_key, payload)
    )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_env(metadata):
    """The consent nest's metadata fetcher pointed at ``metadata``."""
    return metadata.nest_env()


def _bridge_client_id(metadata) -> str:
    return metadata.client_id(_BRIDGE_DOCUMENT_PATH, host=PUBLISHER)


def _bubbles(app) -> list[str]:
    return [t or "" for t in app.driver.get_texts("dm-message-text")]


def _await_bubble(app, body: str) -> None:
    wait_until(
        lambda: any(body in t for t in _bubbles(app)),
        RECEIVE_CYCLE_S,
        diagnose=lambda: f"bubbles: {_bubbles(app)!r}; error: {_page_error(app)!r}",
    )


def _await_draft(app, expected: str) -> str:
    """R's composer body, re-read until it equals ``expected`` or the settle
    budget passes — the caller asserts on what it returns (convention 14)."""
    conv = app.conversations
    last = {"body": None}

    def _read():
        last["body"] = conv.compose_body_text()
        return last["body"] == expected

    try:
        wait_until(_read, UI_SETTLE_S)
    except AssertionError:
        pass
    return last["body"]


def _approve_cli(alice, cli: CliMcpSession) -> None:
    """The cli's device code, approved on tui's Connected apps card."""
    started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
    assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
    code = started["structuredContent"]["user_code"]
    ca = alice.app.connected_apps
    ca.navigate()
    ca.connect_with_code(code)
    index = ca.wait_for_request_code(code)
    assert index is not None, f"typing {code} must open its card. error: {ca.current_error_text()!r}"
    card = ca.request_text(index)
    assert CLI_CLIENT_NAME in card, f"the card names the published client: {card!r}"
    ca.approve_request(index)
    assert ca.current_error_text() == "", ca.current_error_text()


def _room(cli: CliMcpSession, room_id: str) -> dict:
    listed = _until_served(cli, "conversations_list")
    assert not listed["isError"], f"listing through the custodian: {listed!r}\n{cli.diagnostics()}"
    rooms = {r["room_id"]: r for r in listed["structuredContent"]["rooms"]}
    assert room_id in rooms, f"room {room_id} is listed: {sorted(rooms)!r}"
    return rooms[room_id]


def _burn_to_ceiling(cli: CliMcpSession) -> float:
    """Read until the ceiling refuses; the unix time its window rolls, as the
    refusal reports it."""
    for _ in range(_BURN_CAP):
        result = cli.call("conversations_list")
        if not result["isError"]:
            continue
        text = cli.text(result)
        match = _CEILING.search(text)
        assert match, f"the burn met a refusal that is not the ceiling: {text!r}\n{cli.diagnostics()}"
        return time.time() + int(match.group(1))
    raise AssertionError(
        f"{_BURN_CAP} reads never met the ceiling (60 per 60 s window)\n{cli.diagnostics()}"
    )


def _first_served(cli: CliMcpSession) -> tuple[dict, float]:
    """The relaunched custodian's first answer that is a verdict — admitted, or
    the ceiling's refusal — and when it arrived. Until the door is served again
    the cli relays "open Fauna" / "not serving"; those are polled past."""
    deadline = time.monotonic() + APP_RELAUNCH_S + ORCHESTRATION_STEP_S
    while True:
        result = cli.call("conversations_list")
        at = time.time()
        if not result["isError"] or _CEILING.search(cli.text(result)):
            return result, at
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the relaunched custodian never served the door: {result!r}\n{cli.diagnostics()}"
            )
        time.sleep(0.5)  # sleep-ok: paces a deadline poll on the door's own answer, never asserted on


@pytest.mark.feature("connected-apps")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
def test_the_cli_send_is_gated_as_the_page_is_and_its_ceiling_outlives_a_restart(
    app, custodian, consent_nest, metadata, cli_binary, tmp_path
):
    # `app` is the custodian's own launch (the imported `custodian` fixture
    # signs it in); named here so the catalog scan sees the app this test drives.
    alice = custodian
    app = alice.app
    conv = app.conversations

    # ── A bridged room R, made the way a bridge makes one. ──
    tail = os.urandom(3).hex()
    peer = f"@dana-{tail}:example.org"
    far_room = f"!send-gates-{tail}:example.org"
    session, holder_secret, key = _consented_bridge(
        consent_nest, SimpleNamespace(client_id=lambda: _bridge_client_id(metadata))
    )
    _deposit(
        session,
        key,
        far_room_id=far_room,
        far_message_id=f"$send-gates-{tail}",
        sender=peer,
        body=INBOUND_BODY,
    )

    cli = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, **_door_location(alice))
    try:
        cli.initialize()
        _approve_cli(alice, cli)

        conv.navigate()
        thread = wait_until(
            lambda: next((t for t in conv.list_threads() if peer in (t.label or "")), None),
            RECEIVE_CYCLE_S,
            diagnose=lambda: (
                f"no bridged room for {peer}: {[(t.label, t.rail) for t in conv.list_threads()]!r}; "
                f"error: {_page_error(app)!r}"
            ),
        )
        room_id = thread.thread_id
        conv.open_thread_by_id(room_id)
        _await_bubble(app, INBOUND_BODY)

        # ── alice's half-written draft, never sent. ──
        app.driver.wait_for("dm-text-field", timeout=UI_SETTLE_S)
        app.driver.type_text("dm-text-field", DRAFT)
        typed = _await_draft(app, DRAFT)
        assert typed == DRAFT, (
            f"precondition: the draft is in R's composer before the cli acts: {typed!r}; "
            f"error: {_page_error(app)!r}"
        )

        # ── Served room: the cli's send is admitted and leaves the draft alone. ──
        served = _room(cli, room_id)
        assert served["can_send"], f"a served bridged room takes a send: {served!r}"
        sent = cli.call("conversations_send", {"room_id": room_id, "text": SENT})
        assert not sent["isError"], f"the served room takes the cli's send: {sent!r}\n{cli.diagnostics()}"
        items = wait_until(
            lambda: _ok(session, "fauna.bridges.conversation.outbox.fetch", {})["items"],
            RECEIVE_CYCLE_S,
            diagnose=lambda: f"the cli's send never queued to the bridge\n{cli.diagnostics()}",
        )
        opened = [fauna_ffi.bridged_open_as_bridge(holder_secret, bytes(i["ciphertext"])) for i in items]
        assert SENT.encode() in opened, f"the bridge opens the cli's send: {opened!r}"
        assert DRAFT.encode() not in opened, "the draft never left as a message"
        _ok(session, "fauna.bridges.conversation.outbox.ack", {"ids": [i["id"] for i in items]})
        session.ws.close()
        _await_bubble(app, SENT)
        assert not any(DRAFT in t for t in _bubbles(app)), (
            f"the draft is not a sent message: {_bubbles(app)!r}"
        )
        after_send = _await_draft(app, DRAFT)
        assert after_send == DRAFT, (
            f"the cli's send leaves R's half-written draft untouched: {after_send!r}"
        )

        # ── alice revokes the bridge: the page withholds the send. ──
        ca = app.connected_apps
        assert ca.visit(), f"Connected apps did not load; error: {ca.current_error_text()!r}"
        row = ca.wait_for_item_containing(_bridge_client_id(metadata))
        assert row is not None, "the consented bridge is on the roster"
        ca.revoke_item(row)
        assert ca.wait_for_no_item_containing(_bridge_client_id(metadata)), (
            f"the revoked bridge leaves the roster; error: {ca.current_error_text()!r}"
        )
        room = next(r for r in _rooms(consent_nest) if r["far_room_id"] == far_room)
        assert room.get("disconnected") is True, f"the nest marks R disconnected: {room!r}"

        conv.navigate()
        conv.open_thread_by_id(room_id)

        def _withheld():
            try:
                state = app.driver.get_attr("thread-room-notice", "state")
                disabled = app.driver.get_attr("dm-send-button", "disabled")
            except Exception:
                return None
            return state == "bridge-disconnected" and disabled == "true"

        wait_until(
            _withheld,
            RECEIVE_CYCLE_S,
            diagnose=lambda: (
                f"notice={app.driver.get_texts('thread-room-notice')!r}; error: {_page_error(app)!r}"
            ),
        )
        assert app.driver.get_text("thread-room-notice") == (
            S.conversations.unified.room_notice_bridge_disconnected
        )
        for control in ("dm-text-field", "dm-send-button"):
            assert app.driver.get_attr(control, "disabled") == "true", (
                f"{control} is withheld on a room no bridge serves"
            )

        # ── …and so does the door: refused with the page's sentence. ──
        withheld = _room(cli, room_id)
        assert withheld["can_send"] is False, f"the cli lists R as taking no send: {withheld!r}"
        refused = cli.call("conversations_send", {"room_id": room_id, "text": REFUSED})
        assert refused["isError"] and _PAGE_REFUSAL in cli.text(refused), (
            f"the custodian refuses the send the page refuses: {refused!r}\n{cli.diagnostics()}"
        )
        assert not any(REFUSED in t for t in _bubbles(app)), (
            f"a refused send reaches no thread: {_bubbles(app)!r}"
        )
        page = cli.call("conversations_read", {"room_id": room_id})
        assert not page["isError"], f"R stays readable: {page!r}\n{cli.diagnostics()}"
        bodies = [m["body"] for m in page["structuredContent"]["messages"]]
        assert REFUSED not in bodies and DRAFT not in bodies, bodies
        after_refusal = _await_draft(app, DRAFT)
        assert after_refusal == DRAFT, (
            f"a refused cli send leaves R's half-written draft untouched: {after_refusal!r}"
        )

        # ── The ceiling holds across a restart of the custodian. ──
        assert app.driver.preserve_state_across_relaunch(), (
            "the custodian pins its store, so the relaunch is the same device restarting"
        )
        held = None
        readings = []
        for _ in range(_RESTART_TRIES):
            rolls_at = _burn_to_ceiling(cli)
            app.driver.hard_reload()
            conv.enable_real_faunamls()
            first, at = _first_served(cli)
            if first["isError"]:
                held = (first, rolls_at)
                break
            # Admitted: the relaunch outlasted the window. Only consistent if
            # it came after the refused window's reported end — earlier is a
            # ceiling the restart reset.
            assert at >= rolls_at - _CLOCK_SLACK_S, (
                f"the relaunched custodian admitted an operation {rolls_at - at:.1f}s before the "
                f"refused window's end: the restart reset the ceiling\n{cli.diagnostics()}"
            )
            readings.append(round(at - rolls_at, 1))
        assert held is not None, (
            f"every relaunch outlasted the 60 s window (admitted this many seconds after its end: "
            f"{readings!r}), so the ceiling's survival was never observed"
        )
        first, rolls_at = held
        match = _CEILING.search(cli.text(first))
        assert match, f"the relaunched custodian's refusal is the ceiling's: {first!r}"
        again_rolls_at = time.time() + int(match.group(1))
        assert again_rolls_at <= rolls_at + _CLOCK_SLACK_S, (
            f"the refusal after the restart is the SAME window's (ends {again_rolls_at:.0f}, "
            f"refused window ended {rolls_at:.0f}), not one the relaunch opened"
        )
    finally:
        cli.close()
