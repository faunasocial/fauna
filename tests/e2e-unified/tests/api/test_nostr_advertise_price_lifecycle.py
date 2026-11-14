"""tier_3 nest-side witness: a price change on an advertised sold post never
leaves the old price standing on Nostr (`docs/features/nostr.md` outcome 31).

``docs/goal/behavior/monetization.md`` § The Nostr advertisement of a sold post
→ the lifecycle bullet: (1) a price edit on the selling tier retracts the
published note (a kind 5 naming it) and re-publishes it with the new price tag;
(2) clearing the asking price retracts it and clears the advertise flag, so
nothing re-publishes it. The same cases are pinned in-process by
``bins/fauna-nest/tests/conformance_nostr_advertise.rs``; this file observes
them from outside, at the user's publish relay, through the production doors
only.

**The far end.** A :class:`~helpers.nostr_relay.FakeRelay` on loopback is the
user's publish relay, recording what the nest sends it, so the nest is a
locally built one with ``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` set (the reason is
in ``test_nostr_nest.py``'s module docstring).

**The absence assertion's anchor (convention 14).** After the price is cleared,
"no new note" is asserted only once a control post of the same account has
been advertised and its note has reached the same relay: advertising runs the
account's sweep, which would re-publish the cleared post first were its flag
still set, and the nest sends one ordered stream per relay. Never a wall-clock
wait.
"""

from __future__ import annotations

import json
import secrets

import blake3
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers import nostr_relay
from helpers.blob_upload import upload_blob
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

FIRST_MSATS = 21_000
EDITED_MSATS = 30_000


@pytest.fixture(scope="module")
def advertising_nest(nest_binary, tmp_path_factory):
    """A locally built nest allowed to dial a loopback publish relay."""
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "nostr-advertise-lifecycle",
        extra_env=dict(nostr_relay.ALLOW_LOOPBACK_ENV),
    )
    yield nest
    cleanup()


@pytest.fixture
def publish_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(actor["actor_id_bytes"]),
        signing_key=bytes(actor["signing_key"]),
    )


def _seller(nest, relay) -> tuple[dict, str]:
    """A member holding a nest-generated Nostr key, publishing to ``relay``:
    ``(member, hex pubkey)``."""
    member = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"],
    )
    linked = ws_api.bridge_link(nest["port"], member, "nostr", "generate")
    assert linked["linked"], linked
    ws_api.bridge_set_settings(
        nest["port"], member, "nostr", {"relay_list": json.dumps([relay.url])},
    )
    return member, fauna_ffi.nostr_npub_to_hex(linked["identity"]["value"])


def _sell_and_advertise(nest, member, msats: int) -> tuple[str, str, str]:
    """The "sell this post" flow over the production doors — the degenerate
    single-post tier priced and designating the post, the post gated to it
    (its sealed half uploaded first) — then *Advertise on Nostr*:
    ``(tier, post_id, teaser)``."""
    secret = bytes(member["signing_key"])
    tier = f"post-{secrets.token_hex(4)}"
    teaser = f"a post for sale {secrets.token_hex(4)}"
    period_key = secrets.token_bytes(32)
    body, sealed = fauna_ffi.build_gated_post(
        secret, teaser, f"{teaser}\n\nthe part you pay for", tier, 1,
        secrets.token_bytes(32), period_key,
    )
    post_id = blake3.blake3(body).hexdigest()
    with _ws(nest, member) as ws:
        created = ws.call("fauna.subscriptions.tiers.create", {
            "name": tier, "rank": 1, "auto_approve": False,
            "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, tier, period_key),
            "unlocks_post": post_id,
            "asking_price": {"value": msats, "unit": "msat"},
        })
        assert created.get("created") is True, f"the unlock tier: {created!r}"
    upload_blob(nest["port"], member["token"], sealed, audience_class="PeriodRestrictedPost")
    assert ws_api.create_post(nest["port"], member, body) == post_id
    with _ws(nest, member) as ws:
        advertised = ws.call("fauna.nostr.advertise.set", {"post_id": post_id})
        assert advertised.get("advertised_at"), f"advertise.set: {advertised!r}"
    return tier, post_id, teaser


def _offer(relay, pubkey, teaser, msats, what) -> dict:
    """The kind-1 offer of ``teaser`` at ``msats`` the relay received."""
    note = await_received(
        relay,
        lambda e: (
            e["pubkey"] == pubkey and e["kind"] == 1 and teaser in e["content"]
            and ["price", str(msats), "msat"] in e["tags"]
        ),
        RPC_ROUNDTRIP_S,
        what,
    )
    assert fauna_ffi.nostr_verify_event(note), note
    return note


def _retraction(relay, pubkey, event_id, what) -> dict:
    """The kind 5 naming ``event_id`` the relay received."""
    deletion = await_received(
        relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5 and event_id in [
            v[0] for v in tag_values(e, "e") if v
        ],
        RPC_ROUNDTRIP_S,
        what,
    )
    assert fauna_ffi.nostr_verify_event(deletion), deletion
    return deletion


def _position(relay, event) -> int:
    return next(i for i, e in enumerate(list(relay.received)) if e["id"] == event["id"])


@pytest.mark.feature("nostr")
def test_a_price_edit_or_a_cleared_price_withdraws_the_note_that_carried_the_old_price(
    advertising_nest, publish_relay,
):
    """Outcome 31 — advertise a post sold at 21 sats; edit the price to 30 →
    a kind 5 naming the 21-sat note, then a new note at 30; clear the price →
    a kind 5 naming the 30-sat note, and no note re-published after it."""
    nest, relay = advertising_nest, publish_relay
    member, pubkey = _seller(nest, relay)
    tier, post_id, teaser = _sell_and_advertise(nest, member, FIRST_MSATS)
    first = _offer(relay, pubkey, teaser, FIRST_MSATS, "the advertised offer at 21 sats")

    # (1) A price edit: the old offer withdrawn, the new one published.
    with _ws(nest, member) as ws:
        ws.call("fauna.subscriptions.tiers.update", {
            "name": tier, "asking_price": {"value": EDITED_MSATS, "unit": "msat"},
        })
    withdrawn = _retraction(relay, pubkey, first["id"], "the retraction of the 21-sat offer")
    second = _offer(relay, pubkey, teaser, EDITED_MSATS, "the re-published offer at 30 sats")
    assert second["id"] != first["id"]
    assert _position(relay, withdrawn) < _position(relay, second), (
        "the old offer is withdrawn before the new one goes out"
    )

    # (2) The price cleared: the offer withdrawn, nothing re-published.
    with _ws(nest, member) as ws:
        ws.call("fauna.subscriptions.tiers.clear_field", {"name": tier, "field": "asking_price"})
    _retraction(relay, pubkey, second["id"], "the retraction of the 30-sat offer")

    # The anchor: a control post of the same account advertised after the
    # clear runs the account's sweep; once its note has arrived, a re-publish
    # of the cleared post would have arrived before it.
    _, _, control = _sell_and_advertise(nest, member, FIRST_MSATS)
    _offer(relay, pubkey, control, FIRST_MSATS, "the control post's offer")
    offers = [e["id"] for e in relay.received_by(pubkey, kind=1) if teaser in e["content"]]
    assert offers == [first["id"], second["id"]], (
        f"a post whose price was cleared is never advertised again; offers of {post_id[:16]}: "
        f"{offers!r}"
    )
