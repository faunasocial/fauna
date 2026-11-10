"""A legal takedown withdraws the post where the nest had published it, on
Bluesky and on Nostr — `docs/features/admin-nest.md` outcome 39.

`docs/goal/behavior/takedown-and-block.md` § Legal takedown: the off-box
publish surfaces withhold a taken-down post, and a post already projected off
the box is retracted — the linked Bluesky account's cross-post with a real
``com.atproto.repo.deleteRecord``, the Nostr note with a signed NIP-09 kind-5
naming the event the nest published (`docs/goal/architecture/
content-moderation-and-ranking.md` § Implementation status today, leg 6). The
headless halves are pinned in Rust (``bins/fauna-nest/tests/
bluesky_legal_takedown_delete.rs`` — which holds no PDS, so it sees only the
attempt — and ``nostr_legal_takedown_kind5.rs``); this is the same flow through
a running nest whose network is canned, observed where the post went:

* the linked Bluesky account's far end, :class:`helpers.atproto_fakes.
  FakeAtprotoFarEnd`, which records every ``createRecord`` and ``deleteRecord``
  body (the link through the real OAuth callback is
  `test_bluesky_feed_ingest.py`'s, imported);
* the author's publish relay, :class:`helpers.nostr_relay.FakeRelay`, which
  records every event the nest sends it; and the nest's own relay, read as an
  anonymous Nostr client.

No app is driven: the author's post and settings go over the author's own
WS-RPC calls and the takedown over the admin's — `fauna.moderation.legal_takedown`,
the call the admin-nest console's takedown form sends (the console journey is
`tests/test_admin_legal_takedown.py`'s). Every assertion waits on the far
side's recorded state, never on a clock (convention 14).

tier_3: a locally built nest (``bluesky`` + ``nostr`` + ``test-hooks``, the
loopback relay admitted by the test build's env escape); the networks are fakes.
"""

from __future__ import annotations

import json
import secrets
import time

import pytest
from clients._ws_rpc_core import RpcCallError

import fauna_ffi

from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.nostr_relay import FakeRelay, await_received, tag_values
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    FEED_DOMAIN,
    _link_through_callback,
    atproto_far_end,
)

pytestmark = [pytest.mark.tier_3]

#: `write_through` 1 — automatic cross-posting (`bluesky/bridge_provider.rs`).
AUTO = 1

#: A fire-and-forget leg reaching a far end.
REACH_S = 30.0

POST_COLLECTION = "app.bsky.feed.post"


@pytest.fixture
def publish_relay():
    relay = FakeRelay()
    yield relay
    relay.close()


@pytest.fixture
def takedown_nest(bluesky_nest_binary, tmp_path_factory, atproto_far_end):
    """A nest claimed onto a public domain (Bluesky OAuth derives its client
    from it) whose Bluesky client talks to :func:`atproto_far_end` and which may
    dial a loopback Nostr relay, with one registered author."""
    from conftest import _make_nest, _make_user

    nest, cleanup = _make_nest(
        bluesky_nest_binary, tmp_path_factory, "legal-takedown-off-box",
        claim_domain=FEED_DOMAIN,
        extra_env={
            "FAUNA_TEST_ATPROTO_FAR_END": atproto_far_end.url,
            **nostr_relay.ALLOW_LOOPBACK_ENV,
        },
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


def _admin(nest) -> dict:
    sk = nest["admin"]["signing_key"]
    return {"actor_id_bytes": bytes(sk.verify_key), "signing_key": sk}


def _published_to_bluesky(far, text: str) -> list[tuple[int, dict]]:
    """The far end's ``createRecord`` bodies publishing ``text``, each with its
    1-based ordinal — the fake answers the n-th with rkey ``fake<n>``."""
    return [
        (n, r) for n, r in enumerate(far.created_snapshot(), start=1)
        if r.get("collection") == POST_COLLECTION
        and (r.get("record") or {}).get("text") == text
    ]


@pytest.mark.feature("admin-nest")
def test_a_legal_takedown_withdraws_the_post_from_bluesky_and_nostr(
    takedown_nest, atproto_far_end, publish_relay,
):
    """Outcome 39 — a post the nest cross-posted to the author's linked Bluesky
    account and published to their Nostr relay is taken down under a legal
    order: the Bluesky copy is deleted with ``deleteRecord``, the relay is sent
    a signed kind-5 naming the published note, and the nest's own relay stops
    serving it. A post nobody took down keeps both copies."""
    nest, far, relay = takedown_nest, atproto_far_end, publish_relay
    user = nest["user"]
    port = nest["port"]

    # The author's two off-box surfaces, set up as their apps do.
    _link_through_callback(nest, far, user)
    ws_api.bridge_set_settings(port, user, "bluesky", {"write_through": AUTO})
    linked = ws_api.bridge_link(port, user, "nostr", "generate")
    assert linked["linked"], linked
    pubkey = fauna_ffi.nostr_npub_to_hex(linked["identity"]["value"])
    ws_api.bridge_set_settings(port, user, "nostr", {
        "relay_list": json.dumps([relay.url]), "auto_publish": True,
    })

    def post(text: str) -> str:
        return ws_api.create_post(
            port, user,
            sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), text),
        )

    doomed = f"a post a court orders down {secrets.token_hex(4)}"
    kept = f"a post nobody orders down {secrets.token_hex(4)}"
    doomed_id = post(doomed)
    post(kept)

    # Both posts reached both surfaces before the order.
    [(n, _)] = wait_until(
        lambda: _published_to_bluesky(far, doomed), REACH_S,
        diagnose=lambda: f"the post was never cross-posted; createRecord: {far.created_snapshot()!r}",
    )
    wait_until(
        lambda: _published_to_bluesky(far, kept), REACH_S,
        diagnose=lambda: f"the kept post was never cross-posted; createRecord: {far.created_snapshot()!r}",
    )
    note = await_received(
        relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 1 and doomed in e["content"],
        RPC_ROUNDTRIP_S, f"the note {doomed!r}",
    )
    kept_note = await_received(
        relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 1 and kept in e["content"],
        RPC_ROUNDTRIP_S, f"the note {kept!r}",
    )

    # The Bluesky leg reads the post → AT-URI mapping the cross-post wrote; it
    # lands when `createRecord` answers. `bluesky.feed.thread` by post id
    # refuses with "no Bluesky AT-URI mapping" until then.
    def mapped() -> bool:
        try:
            ws_api._client_for(ws_api._base_url(port), user).call(
                "bluesky.feed.thread", {"PostId": {"post_id": bytes.fromhex(doomed_id)}},
            )
        except RpcCallError as e:
            return "mapping" not in str(e.details)
        return True

    wait_until(
        mapped, REACH_S,
        diagnose=lambda: f"the cross-post mapping never landed; far end saw {far.requests!r}",
    )

    taken = ws_api.moderation_legal_takedown(
        port, _admin(nest), doomed_id, legal_reference="EU-DSA-2026/0039",
    )
    assert taken.get("status") == "taken_down", taken

    # Bluesky: exactly the record the cross-post made is deleted, in the
    # linked account's repo.
    deleted = wait_until(
        lambda: [d for d in far.deleted_snapshot()
                 if d.get("collection") == POST_COLLECTION and d.get("rkey") == f"fake{n}"],
        REACH_S,
        diagnose=lambda: (
            f"the takedown must delete the Bluesky copy fake{n}; deleteRecord "
            f"bodies seen: {far.deleted_snapshot()!r}; far end saw {far.requests!r}"
        ),
    )
    assert len(deleted) == 1 and deleted[0].get("repo") == far.did, deleted

    # Nostr: a kind-5 signed by the author's key names the published note.
    deletion = await_received(
        relay,
        lambda e: e["pubkey"] == pubkey and e["kind"] == 5 and [note["id"]] in [
            v[:1] for v in tag_values(e, "e")
        ],
        RPC_ROUNDTRIP_S, f"a deletion of {note['id'][:16]}",
    )
    assert fauna_ffi.nostr_verify_event(deletion), deletion
    assert deletion["content"] == "", (
        f"the deletion says only that the note is gone, never why: {deletion!r}"
    )

    # The nest's own relay serves the kept note and no longer the taken-down one.
    own = nostr_relay.query(
        nostr_relay.nest_relay_url(nest), {"authors": [pubkey], "kinds": [1]},
    )
    own_text = [e["content"] for e in own]
    assert any(kept in c for c in own_text), f"the kept note is still served: {own_text!r}"
    assert not any(doomed in c for c in own_text), (
        f"the nest's own relay must stop serving the taken-down note: {own_text!r}"
    )

    # Only the ordered post was withdrawn.
    assert len(far.deleted_snapshot()) == 1, (
        f"only the taken-down post's copy may go: {far.deleted_snapshot()!r}"
    )
    assert not [e for e in relay.received_by(pubkey, 5)
                if [kept_note["id"]] in [v[:1] for v in tag_values(e, "e")]], (
        "the kept note must not be deleted"
    )
