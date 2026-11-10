"""A post written on a private home nest reaches the public through its relay,
served as the author's own (docs/goal/architecture/nest/private-mode.md § Post
Forwarding; feature page `docs/features/home-nest-behind-a-relay.md`, outcome 13).

The happy path of post forwarding, with no app in the loop. A private HOME nest
queues each new post of a user whose own pairing row there names a public RELAY
with `post_forward`; its outbox worker sends the post's stored bytes verbatim
over the federation channel (`fauna.federation.post.forward`); the relay, running
`paired_only`, admits it because the same user's row on the relay names the home
nest with `post_forward`, and serves it from its own feed. The refusal-and-
recovery path through the Nests page is `tests/test_forward_queue.py`'s
(nests-and-trust); this module witnesses the paired path both halves set up.

What "served as yours" means here: the bytes the relay serves verify under the
author's own key (sign-over-CID) and are byte-identical to the home nest's
stored wire, so the relay derives the same `post_id`; nothing the relay serves
to another of its users carries a nest signature or the home nest's id or
address. The federation channel itself authenticates the home nest to the relay
at handshake — that identity is the `paired_only` check and is required by
design (`private-mode.md` § Post Forwarding), so it is not asserted absent.

Unlinking: revoking the user's home-side row stops new forwards. The home nest
decides at create time, inline, whether a post is queued, so the observable is
the user's own forward queue (`fauna.pair.list`) staying empty for the new post;
the relay not holding it is read alongside, with the first post's arrival as the
positive control that the channel still works.

Topology and seeds as `tests/test_forward_queue.py`: two locally-built nests
started directly through `common.start_nest` with `config/test-private-paired.toml`
(home) and `config/test-public-paired-only.toml` (relay). Standalone-only — it
requests `nest_binary`, which docker and live refuse.

Every wait is a deadline poll on latent state (convention 14).

tier_3: two real `fauna-nest` binaries and the real federation channel between
them; API-only, no app.
"""
from __future__ import annotations

import json
import subprocess
import time

import blake3
import cbor2
import pytest
from nacl.signing import VerifyKey

from common import register_user, start_nest
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

# The outbox worker polls every 5 s when idle; a paired forward lands on its
# first pass. Generous, since the box is shared.
_DELIVERY_S = 90.0


def _stop(nest):
    nest["proc"].terminate()
    try:
        nest["proc"].wait(timeout=10)
    except subprocess.TimeoutExpired:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def home_paired_with_relay(nest_binary, tmp_path_factory):
    """A private HOME nest and a `paired_only` public RELAY, linked both ways.

    The member is registered on both nests. Home-side half: the member's row on
    the home nest names the relay's URL with `post_forward` (what makes home
    forward their posts). Relay-side half: the member's row on the relay names
    the home nest's id with `post_forward` (what the relay's `paired_only`
    check reads). A second relay user, the `reader`, stands in for the public.
    """
    from conftest import _as_nest_handle, _make_user
    from drivers.port_util import find_free_port

    relay = start_nest(
        nest_binary, str(tmp_path_factory.mktemp("hr-relay")), find_free_port(),
        config_name="test-public-paired-only.toml",
    )
    home = None
    try:
        home = start_nest(
            nest_binary, str(tmp_path_factory.mktemp("hr-home")), find_free_port(),
            config_name="test-private-paired.toml",
        )
        home_h, relay_h = _as_nest_handle(home), _as_nest_handle(relay)
        member = _make_user(home_h)
        register_user(
            relay_h["port"], member["actor_id_hex"],
            admin_signing_key=relay_h["admin"]["signing_key"],
        )
        ws_api.add_pairing(
            home_h["port"], member, bytes.fromhex(relay["nest_id"]),
            nest_url=relay_h["peer_url"],
        )
        ws_api.add_pairing(relay_h["port"], member, bytes.fromhex(home["nest_id"]))
        reader = _make_user(relay_h)
        yield {
            "home": home_h, "relay": relay_h,
            "home_nest_id": home["nest_id"], "relay_nest_id": relay["nest_id"],
            "member": member, "reader": reader,
        }
    finally:
        if home is not None:
            _stop(home)
        _stop(relay)


def _publish_on_home(world, text: str, created_at_us: int) -> tuple[str, bytes]:
    wire = sign_and_encode_post(world["member"]["signing_key"], created_at_us, text)
    post_id = ws_api.create_post(world["home"]["port"], world["member"], wire)
    return post_id, wire


def _relay_feed_item(world, post_id: str):
    posts = ws_api.local_feed_posts(world["relay"]["port"], world["reader"])
    return next((p for p in posts if p.get("post_id") == post_id), None)


def _home_queue(world) -> dict:
    return ws_api.list_pairings(world["home"]["port"], world["member"])["forward_queue"]


def _assert_verifies_as_the_author(stored: bytes, author_hex: str) -> None:
    """The served wire is `EmbedAsBytes{bytes, envelope}` with a 100-byte
    `cid || sig` envelope signed by the author over the CID of `bytes`."""
    parts = cbor2.loads(stored)
    inner, envelope = bytes(parts["bytes"]), bytes(parts["envelope"])
    assert set(parts) == {"bytes", "envelope"}, (
        f"served wire must be the bare author envelope, got keys {sorted(parts)}"
    )
    assert len(envelope) == 100, f"envelope must be cid||sig (100 B), got {len(envelope)}"
    cid, sig = envelope[:36], envelope[36:]
    assert cid[4:] == blake3.blake3(inner).digest(), "envelope CID must address the post bytes"
    post = cbor2.loads(inner)
    assert bytes(post["author"]).hex() == author_hex, (
        f"served post's author {bytes(post['author']).hex()} is not the member {author_hex}"
    )
    VerifyKey(bytes.fromhex(author_hex)).verify(cid, sig)  # raises on a bad signature


@pytest.mark.feature("home-nest-behind-a-relay")
def test_a_post_on_the_home_nest_reaches_the_public_through_the_relay_as_the_authors(
    home_paired_with_relay,
):
    """Post on the home nest → the relay's feed serves it to another relay user
    under the same `post_id`, verifying under the author's own key and carrying
    no nest signature, home nest id or home address → the home queue drains →
    the member unlinks the relay on the home nest → a new post is never queued
    and never reaches the relay."""
    world = home_paired_with_relay
    member_hex = world["member"]["actor_id_hex"]
    home_markers = {
        world["home_nest_id"].lower(),
        world["home"]["url"],
        world["home"]["peer_url"],
    }

    text = "written at home, read through the relay"
    post_id, home_wire = _publish_on_home(world, text, int(time.time() * 1_000_000))

    item = wait_until(
        lambda: _relay_feed_item(world, post_id),
        _DELIVERY_S,
        diagnose=lambda: (
            f"relay never served post {post_id}; home forward queue reads "
            f"{_home_queue(world)!r}"
        ),
    )
    assert item.get("author") == member_hex, (
        f"the relay must serve the post as the member's, got author {item.get('author')!r}"
    )
    assert text in (item.get("body") or ""), f"served body {item.get('body')!r}"

    served = ws_api.get_post(world["relay"]["port"], world["reader"], post_id)
    assert served == home_wire, "the relay must serve the home nest's stored wire verbatim"
    assert blake3.blake3(served).digest().hex() == post_id, (
        "the relay's post_id must be the content address the home nest derived"
    )
    _assert_verifies_as_the_author(served, member_hex)

    item_text = json.dumps(item, default=lambda v: v.hex() if isinstance(v, bytes) else str(v))
    leaked = [m for m in home_markers if m in item_text]
    assert not leaked, f"the relay's feed item names the home nest: {leaked} in {item_text}"
    assert bytes.fromhex(world["home_nest_id"]) not in served, (
        "the served post bytes carry the home nest's id"
    )

    wait_until(
        lambda: _home_queue(world)["queued"] == 0,
        _DELIVERY_S,
        diagnose=lambda: f"home queue never drained: {_home_queue(world)!r}",
    )

    # Unlink: the member revokes the relay's row on the home nest.
    ws_api.revoke_pairing(
        world["home"]["port"], world["member"], bytes.fromhex(world["relay_nest_id"])
    )
    assert not ws_api.list_pairings(world["home"]["port"], world["member"])["pairings"], (
        "the home-side row must be gone after revoke"
    )

    kept = "written after unlinking, stays home"
    kept_id, _ = _publish_on_home(world, kept, int(time.time() * 1_000_000))
    # The queue decision is made inline at create: once create returns, an
    # unlinked author's post is either queued now or never.
    queue = _home_queue(world)
    assert queue["queued"] == 0, f"an unlinked author's post must not be queued: {queue!r}"
    assert ws_api.get_post(world["home"]["port"], world["member"], kept_id), (
        "the post is still on the home nest"
    )
    assert _relay_feed_item(world, kept_id) is None, (
        "a post written after unlinking must not be on the relay"
    )
