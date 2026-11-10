"""tier_3: **the LLM can search your conversations, across the device or within
one community room** — the cli's ``conversations_search`` answered through the
Fauna app signed in on the same device (``docs/goal/behavior/conversations-oracle.md``
§ What each shell serves; § The two classes, the ``conversation.read`` row:
"search the device's own conversation index and the community room's
position-only door"; § Implementation status today: ``conversations_search``
(device-wide, or one community room), the custodian's ``room_search_answer``,
the device's content index for search).

The cast is ``test_cli_conversations.py``'s, adopted by import: alice's seat
on the consent nest is the custodian (the app under test — tui, and linux, the
macOS and the windows app, which all serve the door and all build the device's
content index, ``content-index-ingest.md`` § The Conversation kind's catch-up),
a second seat of the same app (bob) is the far member, and the cli shell
(``drivers/cli_mcp.py``) is driven over MCP once alice approves its card. One
journey:

- alice holds two rooms, each made the way a user makes it (convention 8): an
  end-to-end 1:1 with bob and a community room bob joins; in each she sends one
  message carrying a word only that room has, and a word both rooms share;
- **red first** — before any grant ``conversations_search`` tells the host to
  connect;
- alice approves the cli's typed code on her seat;
- the list says which room is searchable through its home nest
  (``can_search_room``): the community room, not the 1:1;
- **device-wide** (no ``room_id``) the shared word finds both rooms, the
  end-to-end room included — its words reach the device's own index, never the
  nest — and each room's own word finds that room alone;
- **within the community room** (``room_id``) the shared word finds that room's
  hit and never the 1:1's, and the 1:1's own word finds nothing there;
- a ``room_id`` naming the end-to-end room is refused: only a community room is
  searched through its home nest.

**Latency-independent** (convention 14): the device index flushes on its own
debounce, with a periodic backstop (``fauna-client-index``'s lifecycle, 60 s),
and the home nest's index fills from its own read of the room, so every search
is a deadline poll on the tool's own answer, never a sleep then an assert.

The catalog page ``docs/features/connected-apps.md`` maps this journey as
outcome 49.
"""

import secrets

import pytest

from drivers.cli_mcp import CliMcpSession
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_spa_url,
)
from helpers.budgets import ORCHESTRATION_STEP_S, RPC_ROUNDTRIP_S
from helpers.client_metadata_server import (  # noqa: F401 — pytest fixtures adopted by import
    cli_metadata,
    consent_nest_env,
)
from helpers.room_seats import found_community_room, seat_story, wait_for, wait_snippet
from tests.api import conv_api
from tests.test_cli_conversations import (  # noqa: F401 — custodian/far_seat are pytest fixtures adopted by import
    _door_location,
    _end_to_end_room,
    _until_served,
    custodian,
    far_seat,
)

# `real_conversations`: the same rail marker as the journey this one extends.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# One fresh word per room and one both share, so a hit can only come from this
# run's messages. Letters only: the shared tokenizer splits on everything else.
_TAG = secrets.token_hex(4).translate(str.maketrans("0123456789", "ghijklmnop"))
E2E_WORD = f"heron{_TAG}"
COMMUNITY_WORD = f"lantern{_TAG}"
SHARED_WORD = f"orchard{_TAG}"
E2E_BODY = f"the {E2E_WORD} came back to the {SHARED_WORD}"
COMMUNITY_BODY = f"bring a {COMMUNITY_WORD} to the {SHARED_WORD}"
COMMUNITY_FIRST = "welcome to the searchable room"


def _send_in(seat, thread_id: str, body: str) -> None:
    """``body`` sent from ``seat``'s composer in ``thread_id``."""
    conv = seat.app.conversations
    conv.navigate()
    conv.open_thread_by_id(thread_id)
    conv.send_in_open_thread(body)
    assert not seat.app.has_error(), seat.app.error_text()


def _hit_rooms(result: dict) -> set[str]:
    """The rooms a successful ``conversations_search`` answer names."""
    if result["isError"]:
        return set()
    return {h["room_id"] for h in result["structuredContent"]["hits"]}


@pytest.mark.feature("connected-apps")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.windows
def test_the_cli_searches_the_device_and_one_community_room_through_the_custodian(
    app, custodian, far_seat, consent_nest, cli_binary, tmp_path
):
    # `app` is the custodian's own launch (the imported `custodian` fixture
    # signs it in); named here so the catalog scan sees the app this test drives.
    alice, bob = custodian, far_seat
    port = consent_nest["port"]

    # ── alice's two rooms, each carrying its own word and the shared one. ──
    e2e_tid, e2e_channel = _end_to_end_room(alice, bob)
    community_channel, community_tid, _bob_community_tid = found_community_room(
        alice, bob, COMMUNITY_FIRST
    )
    _send_in(alice, e2e_tid, E2E_BODY)
    wait_snippet(bob, e2e_channel, E2E_WORD, what="bob decrypting alice's searchable 1:1 message")
    _send_in(alice, community_tid, COMMUNITY_BODY)
    wait_snippet(
        bob, community_channel, COMMUNITY_WORD, what="bob opening alice's community room message"
    )

    def nest_side() -> str:
        """The home nest's own answer for the community room (convention 5):
        separates a door or custodian fault from an index the nest never filled."""
        try:
            hits = conv_api.room_search(port, alice.actor, community_channel, SHARED_WORD)
        except Exception as e:  # noqa: BLE001 — a diagnosis, never the assertion
            return f"room.search refused: {e!r}"
        return f"room.search({SHARED_WORD!r}) on the home nest: {hits!r}"

    cli = CliMcpSession(cli_binary, consent_nest["url"], tmp_path, **_door_location(alice))
    try:
        cli.initialize()

        # ── Red first: no search before the grant. ──
        before = cli.call("conversations_search", {"query": SHARED_WORD})
        assert before["isError"] and "call connect" in cli.text(before), (
            f"before any grant the search tells the host to connect: {before!r}\n"
            f"{cli.diagnostics()}"
        )

        # ── alice approves the cli's card on her seat. ──
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
        ca.approve_request(index)
        assert ca.current_error_text() == "", ca.current_error_text()

        # ── The list says which room its home nest searches. ──
        listed = _until_served(cli, "conversations_list")
        assert not listed["isError"], (
            f"conversation.read lists the rooms through the custodian: {listed!r}\n"
            f"{cli.diagnostics()}\n{seat_story(alice)}"
        )
        rooms = {r["room_id"]: r for r in listed["structuredContent"]["rooms"]}
        assert rooms.get(community_tid, {}).get("can_search_room") is True, (
            f"the community room is searchable through its home nest: {rooms.get(community_tid)!r}"
        )
        assert rooms.get(e2e_tid, {}).get("can_search_room") is False, (
            f"an end-to-end room has no home-nest search: {rooms.get(e2e_tid)!r}"
        )

        def search(query: str, room_id: str | None = None) -> dict:
            args = {"query": query}
            if room_id is not None:
                args["room_id"] = room_id
            return cli.call("conversations_search", args)

        # ── Device-wide: the shared word finds both rooms. The device's index
        # flushes on its own debounce, so the answer is polled (convention 14). ──
        wide = wait_for(
            "a device-wide search finding the shared word in both rooms",
            lambda: search(SHARED_WORD),
            lambda r: {e2e_tid, community_tid} <= _hit_rooms(r),
            ORCHESTRATION_STEP_S,
            diagnose=lambda: f"{cli.diagnostics()}\n{seat_story(alice)}",
        )
        for hit in wide["structuredContent"]["hits"]:
            if hit["room_id"] in (e2e_tid, community_tid):
                assert SHARED_WORD in hit["excerpt"], f"the hit's excerpt is its message: {hit!r}"
                assert hit["position"] is not None, f"a held message has a position: {hit!r}"
        assert not [h for h in wide["structuredContent"]["hits"] if h["room_id"] not in rooms], (
            f"every hit names a room the list holds: {wide!r}"
        )

        # Each room's own word finds that room alone — the end-to-end room's
        # words reach the device's index, and only that room's.
        e2e_only = search(E2E_WORD)
        assert not e2e_only["isError"], f"{e2e_only!r}\n{cli.diagnostics()}"
        assert _hit_rooms(e2e_only) == {e2e_tid}, (
            f"the 1:1's own word finds the 1:1 alone, device-wide: {e2e_only!r}"
        )
        community_only = search(COMMUNITY_WORD)
        assert not community_only["isError"], f"{community_only!r}\n{cli.diagnostics()}"
        assert _hit_rooms(community_only) == {community_tid}, (
            f"the community room's own word finds that room alone, device-wide: {community_only!r}"
        )

        # ── Within the community room: its home nest's door answers, and only
        # for that room. The nest's index fills from its own read (polled). ──
        scoped = wait_for(
            "the community room's search finding the shared word",
            lambda: search(SHARED_WORD, community_tid),
            lambda r: community_tid in _hit_rooms(r),
            ORCHESTRATION_STEP_S,
            diagnose=lambda: f"{nest_side()}\n{cli.diagnostics()}\n{seat_story(alice)}",
        )
        assert _hit_rooms(scoped) == {community_tid}, (
            f"a room-scoped search never answers from another room: {scoped!r}"
        )
        assert any(COMMUNITY_WORD in h["excerpt"] for h in scoped["structuredContent"]["hits"]), (
            f"the room's hit is alice's message in it: {scoped!r}"
        )
        elsewhere = search(E2E_WORD, community_tid)
        assert not elsewhere["isError"], f"{elsewhere!r}\n{cli.diagnostics()}"
        assert elsewhere["structuredContent"]["hits"] == [], (
            f"the 1:1's word is not in the community room: {elsewhere!r}"
        )

        # ── An end-to-end room is not searched through a nest: refused. ──
        refused = search(SHARED_WORD, e2e_tid)
        assert refused["isError"] and "community room" in cli.text(refused), (
            f"a room_id naming the end-to-end room is refused, naming why: {refused!r}\n"
            f"{cli.diagnostics()}"
        )
    finally:
        cli.close()
