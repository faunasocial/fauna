"""tier_3: the **conversations oracle** end to end — the cli app reads and sends
in the user's rooms through the Fauna app signed in on the same device, holding
no key (``docs/goal/behavior/conversations-oracle.md`` § Done definition, the
journey item; § What each shell serves).

tui is the custodian: alice's seat on the consent nest hosts her conversations
engine and serves the account's door at login. A second tui seat (bob) is the
far member. One journey:

- alice holds three rooms, each made the way a user makes it (convention 8): an
  end-to-end 1:1 with bob, a community room bob joins, and a Nostr-bridged room
  a far Nostr user's gift wrap opens (``test_bridged_conversation.py``'s leg);
- **red first** — before any grant the cli's conversations tools tell the host
  to connect; the catalog offers no membership tool, and a host asking for one
  is refused;
- tui approves the cli's typed code, its card naming both classes
  (``conversation.read`` / ``conversation.send`` — the consent asks for every
  arm the catalog needs);
- the cli lists the three rooms with their classes — the classes the rooms
  already had — and reads each one's messages through tui;
- the cli sends into the end-to-end room and bob's seat decrypts it in his 1:1
  with alice: an MLS application message signed by alice's own leaf, the only
  leaf that room has for her;
- tui's roster row carries the oracle's record of those operations — its
  latest one naming the room the cli read, and the count in the ceiling's
  window the custodian carries with its record batches;
- alice revokes the cli on tui, and its next read is refused;
- ``thread-room-class`` reads the same on every seat before and after —
  the oracle never moves a room's class.

**The door's path.** The harness isolates every app launch, so the custodian's
door sits under the tui seat's own private ``XDG_RUNTIME_DIR``; the shell is
launched with that runtime dir, the place a host on the same session would find
it (``drivers/cli_mcp.py``).

The catalog page ``docs/features/connected-apps.md`` maps this journey as
outcome 42 (the coverage contract is not a public text — ``public-texts.md``
§ The boundary — so it landed directly, with the tag below).
"""

import os
import subprocess
import time
import urllib.parse
from pathlib import Path

import pytest

import fauna_ffi
from drivers.cli_mcp import CliMcpSession
from helpers import nostr_relay
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_nest_env,
    consent_spa_url,
)
from helpers.budgets import ORCHESTRATION_STEP_S, RECEIVE_CYCLE_S, RPC_ROUNDTRIP_S
from helpers.principal_session import alice_client
from helpers.room_seats import (
    MESSAGE_BUDGET_S,
    Seat,
    found_community_room,
    launch_seat,
    seat_app_path,
    seat_story,
    teardown_seats,
    thread_by_channel,
    thread_by_id,
    wait_for,
    wait_snippet,
)
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3]

_REPO = Path(__file__).resolve().parents[3]

#: The scope the catalog asks for (`fauna_cli::catalog::consent_scope`).
_CONSENT_SCOPE = (
    "fauna:feed:read fauna:feed:read:account fauna:identity:op:post.author "
    "fauna:notifications:read fauna:notifications:write "
    "fauna:identity:op:profile.author fauna:identity:op:conversation.read "
    "fauna:identity:op:conversation.send fauna:profile:read"
)
_CLIENT_ID = "http://localhost?scope=" + urllib.parse.quote(_CONSENT_SCOPE, safe="")

E2E_FIRST = "hi bob, the end-to-end one"
COMMUNITY_FIRST = "welcome to the community room"
NOSTR_BODY = "a gift-wrapped hello the assistant reads"
SENT = "sent by an assistant through the oracle"

_MEMBERSHIP_VERBS = ("invite", "remove", "leave", "rename", "member", "transfer", "policy")


@pytest.fixture(scope="module")
def cli_binary():
    """The device shell, through its build-if-stale recipe (never bare cargo)."""
    built = subprocess.run(["just", "cli-debug"], cwd=_REPO, capture_output=True, text=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR") or _REPO / "target")
    binary = target / "debug" / ("fauna-cli.exe" if os.name == "nt" else "fauna-cli")
    if built.returncode != 0 or not binary.exists():
        pytest.fail(
            f"`just cli-debug` did not build the device shell:\n"
            f"{built.stdout[-2000:]}\n{built.stderr[-2000:]}"
        )
    return str(binary)


@pytest.fixture
def custodian(request, app, consent_nest, consent_bridge):
    """alice's tui seat on the consent nest — the custodian — with the real
    conversations backend addressable and mail enabled through the app's own UI
    (the recipient key a bridged room's rows are sealed to)."""
    from conftest import _login_app_as

    _login_app_as(
        app, request, consent_nest, consent_nest["user"], spa_url_fixture="consent_spa_url"
    )
    app.conversations.enable_real_faunamls()
    alice = consent_nest["user"]
    wait_for(
        "alice's key packages reaching the nest (keypackage.addressable)",
        lambda: conv_api.actor_addressable(consent_nest["port"], alice, alice["actor_id_hex"]),
        bool,
        MESSAGE_BUDGET_S,
    )
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    return Seat(app=app, actor=alice)


@pytest.fixture
def far_seat(request, consent_nest, custodian):
    """bob's tui seat — the far member of alice's rooms; he accepts her as a
    contact so her Welcome is admitted under the default inbox mode."""
    seat = launch_seat(consent_nest, seat_app_path(request, "tui"), "tui", request)
    conv_api.accept_contact(consent_nest["port"], seat.actor, custodian.actor_hex)
    yield seat
    teardown_seats([seat])


def _runtime_dir(seat: Seat) -> str:
    """The custodian launch's private ``XDG_RUNTIME_DIR`` — the agent socket is
    ``<runtime>/fauna/sync-agent.sock`` and the door sits beside it."""
    socket = seat.app.driver.sync_agent_socket
    assert socket, "the tui seat publishes its agent socket on linux"
    return os.path.dirname(os.path.dirname(socket))


def _end_to_end_room(alice: Seat, bob: Seat) -> tuple[str, str]:
    """alice's 1:1 with bob, resolved and sent from her composer; her thread
    id and the channel both seats agree on, once bob has decrypted it."""
    alice.app.conversations.real_resolve_send_new(bob.actor_hex, E2E_FIRST)
    thread = wait_for(
        "alice's 1:1 binding to its channel",
        lambda: next(
            (
                t
                for t in alice.app.conversations.list_threads()
                if t.rail == "FaunaMls" and E2E_FIRST in (t.snippet or "") and t.channel_id_hex
            ),
            None,
        ),
        lambda t: t is not None,
        MESSAGE_BUDGET_S,
        diagnose=lambda: seat_story(alice),
    )
    wait_snippet(bob, thread.channel_id_hex, E2E_FIRST, what="bob decrypting alice's 1:1")
    return thread.thread_id, thread.channel_id_hex


def _nostr_room(alice: Seat, nest: dict) -> str:
    """A far Nostr user's gift wrap to alice's custodial key, opened as a
    bridged room on her seat; her thread id."""
    app = alice.app
    app.nostr.navigate()
    assert app.nostr.ensure_linked(), (
        f"could not link a generated Nostr key: {app.nostr.page_error_text()!r}"
    )
    with alice_client(nest) as ws:
        bridges = ws.call("fauna.bridges.list", {})["bridges"]
    npub = next(b for b in bridges if b["id"] == "nostr")["identity"]["value"]
    accepted, message = nostr_relay.publish(
        nostr_relay.nest_relay_url(nest),
        fauna_ffi.nostr_gift_wrap_dm(os.urandom(32), npub, NOSTR_BODY),
    )
    assert accepted, f"the relay refused the gift wrap: {message!r}"
    app.conversations.navigate()
    thread = wait_for(
        "the Nostr-bridged room on alice's seat",
        lambda: next(
            (t for t in app.conversations.list_threads() if t.rail == "Bridged"), None
        ),
        lambda t: t is not None,
        RECEIVE_CYCLE_S,
        diagnose=lambda: seat_story(alice),
    )
    return thread.thread_id


def _classes(seat: Seat, thread_ids: list[str]) -> dict[str, str | None]:
    """``thread-room-class`` of each thread on ``seat``, read off its header."""
    conv = seat.app.conversations
    conv.navigate()
    out = {}
    for tid in thread_ids:
        conv.open_thread_by_id(tid)
        out[tid] = conv.room_class()
    return out


def _until_served(cli: CliMcpSession, tool: str, arguments: dict | None = None) -> dict:
    """Call ``tool`` until the shell's consent poll has become a session
    (convention 14: the wait is the nest's poll interval, the assertion the
    state)."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while True:
        result = cli.call(tool, arguments or {})
        if not result["isError"] or "Waiting" not in cli.text(result):
            return result
        if time.monotonic() >= deadline:
            return result
        time.sleep(1.0)  # sleep-ok: paces a deadline poll on the shell's own state, never asserted on


@pytest.mark.feature("connected-apps")
@pytest.mark.tui
def test_the_cli_reads_and_sends_through_tui_as_custodian_until_revoked(
    custodian, far_seat, consent_nest, cli_binary, tmp_path
):
    alice, bob = custodian, far_seat

    # ── alice's three rooms, each made the way a user makes it. ──
    e2e_tid, e2e_channel = _end_to_end_room(alice, bob)
    community_channel, community_tid, bob_community_tid = found_community_room(
        alice, bob, COMMUNITY_FIRST
    )
    nostr_tid = _nostr_room(alice, consent_nest)
    bob_e2e_tid = thread_by_channel(bob.app, e2e_channel).thread_id

    alice_rooms = [e2e_tid, community_tid, nostr_tid]
    bob_rooms = [bob_e2e_tid, bob_community_tid]
    expected = {e2e_tid: "end-to-end", community_tid: "community", nostr_tid: "transport-only"}
    alice_before = _classes(alice, alice_rooms)
    bob_before = _classes(bob, bob_rooms)
    assert alice_before == expected, f"alice's rooms are the three classes: {alice_before!r}"
    assert bob_before == {bob_e2e_tid: "end-to-end", bob_community_tid: "community"}, bob_before

    cli = CliMcpSession(
        cli_binary, consent_nest["url"], tmp_path, runtime_dir=_runtime_dir(alice)
    )
    try:
        cli.initialize()

        # ── Red first: nothing before the grant; no membership verb at all. ──
        before = cli.call("conversations_list")
        assert before["isError"] and "call connect" in cli.text(before), (
            f"before any grant a conversations tool tells the host to connect: {before!r}\n"
            f"{cli.diagnostics()}"
        )
        conversation_tools = [n for n in cli.tool_names() if n.startswith("conversations_")]
        assert sorted(conversation_tools) == [
            "conversations_list",
            "conversations_read",
            "conversations_search",
            "conversations_send",
        ], conversation_tools
        assert not [n for n in conversation_tools if any(v in n for v in _MEMBERSHIP_VERBS)]
        refused = cli.request_error(
            "tools/call", {"name": "conversations_invite", "arguments": {"room_id": e2e_tid}}
        )
        assert refused["code"] == -32602, refused

        # ── tui approves the cli's card, which names both classes. ──
        started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
        assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
        code = started["structuredContent"]["user_code"]
        ca = alice.app.connected_apps
        ca.navigate()
        ca.connect_with_code(code)
        index = ca.wait_for_request_code(code)
        assert index is not None, (
            f"typing {code} must open its card. error: {ca.current_error_text()!r}"
        )
        card = ca.request_text(index)
        ca.approve_request(index)
        assert ca.current_error_text() == "", ca.current_error_text()

        # ── The cli lists alice's rooms through tui, with the classes they had. ──
        listed = _until_served(cli, "conversations_list")
        assert not listed["isError"], (
            f"conversation.read lists the rooms through the custodian: {listed!r}\n"
            f"card: {card!r}\n{cli.diagnostics()}\n{seat_story(alice)}"
        )
        rooms = {r["room_id"]: r for r in listed["structuredContent"]["rooms"]}
        for tid, cls in expected.items():
            assert tid in rooms and rooms[tid]["class"] == cls, (
                f"room {tid} is listed as {cls}: {rooms.get(tid)!r}"
            )
        assert rooms[e2e_tid]["can_send"], rooms[e2e_tid]

        # ── It reads each room's messages through tui. ──
        for tid, body in (
            (e2e_tid, E2E_FIRST),
            (community_tid, COMMUNITY_FIRST),
            (nostr_tid, NOSTR_BODY),
        ):
            page = cli.call("conversations_read", {"room_id": tid})
            assert not page["isError"], f"reading {tid}: {page!r}\n{cli.diagnostics()}"
            bodies = [m["body"] for m in page["structuredContent"]["messages"]]
            assert body in bodies, f"{expected[tid]} room {tid} reads its message: {bodies!r}"

        # ── It sends into the end-to-end room; bob's seat decrypts it. ──
        sent = cli.call("conversations_send", {"room_id": e2e_tid, "text": SENT})
        assert not sent["isError"], (
            f"conversation.send sends as the account: {sent!r}\n{cli.diagnostics()}"
        )
        wait_snippet(bob, e2e_channel, SENT, what="bob decrypting the cli's send from alice")
        after_send = cli.call("conversations_read", {"room_id": e2e_tid})
        own = [m for m in after_send["structuredContent"]["messages"] if m["body"] == SENT]
        assert own and own[0]["is_own"], f"the send is alice's own message: {after_send!r}"

        # ── tui's roster row carries the oracle's record of those operations. ──
        ca.navigate()
        row = ca.wait_for_oracle_item_containing(_CLIENT_ID)
        assert row is not None, (
            f"the cli's roster row carries the oracle's record. error: {ca.current_error_text()!r}"
        )
        # The row shows the operations two ways: the latest one, whose detail
        # is the room it named — the read after the send — and the count in
        # the ceiling's current window, which the custodian keeps on the device
        # and carries with each record batch. The outbox pump submits on its
        # own tick, so the row is re-read until both land (convention 14). The
        # window is a rolling one: a record that lands after it rolled reads a
        # count of 0, which is the ceiling's truth, so the cli reads once more
        # to open a fresh window rather than the test waiting on the clock.
        deadline = time.monotonic() + ORCHESTRATION_STEP_S
        while True:
            row = ca.item_index_containing(_CLIENT_ID)
            text = ca.item_text(row) if row is not None else ""
            ops = ca.item_oracle_ops(row) if row is not None else None
            counted = ops is not None and ops.isdigit() and int(ops) >= 1
            if (e2e_tid in text and counted) or time.monotonic() >= deadline:
                break
            if e2e_tid in text:
                cli.call("conversations_read", {"room_id": e2e_tid})
            time.sleep(1.0)  # sleep-ok: paces a deadline poll on the roster's own state, never asserted on
            ca.navigate()
        assert e2e_tid in text, (
            f"the roster row's latest operation names the room the cli read: {text!r}"
        )
        assert counted, (
            f"the roster row counts the custodian's operations in its window: oracle_ops={ops!r}"
        )

        # ── alice revokes the cli; its next read is refused. ──
        ca.revoke_item(row)
        assert ca.current_error_text() == "", ca.current_error_text()
        deadline = time.monotonic() + ORCHESTRATION_STEP_S
        while True:
            after = cli.call("conversations_read", {"room_id": e2e_tid})
            if after["isError"] or time.monotonic() >= deadline:
                break
            time.sleep(1.0)  # sleep-ok: paces a deadline poll on the door's own answer, never asserted on
        assert after["isError"], (
            f"after the revoke the custodian refuses the next read: {after!r}\n{cli.diagnostics()}"
        )
    finally:
        cli.close()

    # ── The class never moved, on either seat. ──
    assert _classes(alice, alice_rooms) == alice_before
    assert _classes(bob, bob_rooms) == bob_before
    assert thread_by_id(alice.app, e2e_tid) is not None
