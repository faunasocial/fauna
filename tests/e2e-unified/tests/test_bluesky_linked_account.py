"""What a nest does to a LINKED Bluesky account — the consume-side link to an
account whose home is someone else's PDS — observed at that account's far end.

`docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky): the
``write_through`` modes (``1`` auto cross-posts every Fauna post, ``2`` manual
only those carrying a ``crosspost`` tag facet) and the write-through delete leg
(deleting a cross-posted Fauna post removes its Bluesky record with
``com.atproto.repo.deleteRecord``); `docs/goal/ui/atproto.md` § Transition
semantics (Off/Linked → Hosted visible unlinks a consume-side link, and the
external account itself is untouched). Catalog outcomes ``atproto`` 21, 37, 38
and 39.

The far end, the nest built with the ``bluesky`` feature and the link through
the real OAuth callback are `test_bluesky_feed_ingest.py`'s, imported rather
than copied; read its module docstring for why linking over the user's own
``fauna.bridges.link`` and the callback is a precondition and not a shortcut
(e2e rule 8b). Every outcome here is the nest's, witnessed at the far end
(:class:`helpers.atproto_fakes.FakeAtprotoFarEnd` records each
``createRecord`` and ``deleteRecord`` body), so no app is driven: posts, the
mode, the delete and the level change go over the user's own WS-RPC calls —
the pragmatic exception `test_atproto_hosted_account.py` takes, the client-UI
journey that writes a post being ``test_feed.py::test_create_post``.

**Absence is witnessed against a later sentinel** (convention 14): a post the
mode must NOT publish is written first, then one it must; once the sentinel's
``createRecord`` lands, the earlier post's write-through has long decided —
its skip is a local table read, the sentinel's publish a network round trip.

tier_3: a locally-built nest; the atproto network is a fake.
"""

from __future__ import annotations

import time

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import USER_ROTATION_PUB
from helpers.waiting import wait_until
from tests.api.bare import sign_and_encode_post, sign_and_encode_tombstone
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    _link_through_callback,
    atproto_far_end,
    bluesky_feed_nest,
)

pytestmark = [pytest.mark.tier_3]

#: `write_through` — the Bluesky bridge's cross-posting mode
#: (`bluesky/bridge_provider.rs`): 0 disabled, 1 automatic, 2 manual.
AUTO, MANUAL = 1, 2

#: A fire-and-forget write-through leg reaching the far end.
REACH_S = 30.0

POST_COLLECTION = "app.bsky.feed.post"


def _call(nest, kind: str, params: dict) -> dict:
    user = nest["user"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as client:
        return client.call(kind, params)


def _set_mode(nest, mode: int) -> None:
    _call(nest, "fauna.bridges.set_settings", {
        "bridge_id": "bluesky", "settings": {"write_through": mode},
    })


def _post(nest, text: str, *, tagged: bool = False) -> str:
    """Write a post as the user; ``tagged`` carries the protocol-agnostic
    ``crosspost`` tag facet manual mode opts in on. Returns its post id."""
    body = sign_and_encode_post(
        nest["user"]["signing_key"], int(time.time() * 1_000_000), text,
        # A display-only tag facet, byte range empty — the shape every app's
        # composer gives a tag (`fauna_client_core::post::build_post`).
        tags=[("crosspost", 0, 0)] if tagged else None,
    )
    return _call(nest, "fauna.posts.create", {"body": body})["post_id"]


def _published(far, text: str) -> list[tuple[int, dict]]:
    """The far end's ``createRecord`` bodies publishing ``text``, each with its
    1-based ordinal — the fake answers the n-th with rkey ``fake<n>``."""
    return [
        (n, r) for n, r in enumerate(far.created_snapshot(), start=1)
        if r.get("collection") == POST_COLLECTION
        and (r.get("record") or {}).get("text") == text
    ]


def _await_published(far, text: str, what: str) -> list[tuple[int, dict]]:
    return wait_until(
        lambda: _published(far, text), REACH_S,
        diagnose=lambda: (
            f"{what}: no createRecord carried {text!r}; createRecord bodies "
            f"seen: {far.created_snapshot()!r}; far end saw {far.requests!r}"
        ),
    )


def _bluesky_linked(nest) -> bool:
    bridges = _call(nest, "fauna.bridges.list", {})["bridges"]
    return next(b for b in bridges if b["id"] == "bluesky")["linked"]


@pytest.mark.feature("atproto")
def test_on_automatic_every_post_is_published_to_the_linked_account(
    bluesky_feed_nest, atproto_far_end,
):
    """``write_through`` 1: a plain post and a tagged one both reach the linked
    PDS, each exactly once, in the linked account's own repo."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    _set_mode(nest, AUTO)

    plain, tagged = "an untagged post on automatic", "a tagged post on automatic"
    _post(nest, plain)
    _post(nest, tagged, tagged=True)

    for text in (plain, tagged):
        published = _await_published(far, text, "automatic cross-posting publishes every post")
        assert len(published) == 1, f"{text!r} must be published once: {published!r}"
        assert published[0][1].get("repo") == far.did, (
            f"the copy belongs in the linked account's repo {far.did!r}: {published!r}"
        )


@pytest.mark.feature("atproto")
def test_on_manual_only_posts_tagged_for_it_are_published(
    bluesky_feed_nest, atproto_far_end,
):
    """``write_through`` 2: an untagged post stays on Fauna; one carrying the
    ``crosspost`` tag facet reaches the linked PDS."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    _set_mode(nest, MANUAL)

    untagged, tagged = "an untagged post on manual", "a tagged post on manual"
    _post(nest, untagged)
    _post(nest, tagged, tagged=True)

    published = _await_published(far, tagged, "manual cross-posting publishes a tagged post")
    assert len(published) == 1, f"the tagged post must be published once: {published!r}"
    assert published[0][1].get("repo") == far.did, published
    assert _published(far, untagged) == [], (
        f"on manual an untagged post must stay on Fauna; createRecord bodies "
        f"seen: {far.created_snapshot()!r}"
    )


@pytest.mark.feature("atproto")
def test_deleting_a_cross_posted_post_removes_its_copy(
    bluesky_feed_nest, atproto_far_end,
):
    """Deleting a cross-posted post sends the linked PDS a ``deleteRecord`` for
    exactly the record its ``createRecord`` made — and the copy of a post that
    is kept stays."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    _set_mode(nest, AUTO)

    doomed, kept = "a cross-posted post its author deletes", "a cross-posted post its author keeps"
    doomed_id = _post(nest, doomed)
    _post(nest, kept)
    [(n, _)] = _await_published(far, doomed, "the post to delete is cross-posted first")
    _await_published(far, kept, "the post to keep is cross-posted")

    # The leg records the post → AT-URI mapping once `createRecord` answers;
    # the delete leg reads it. Its wire read — `bluesky.feed.thread` by post
    # id — refuses with "no Bluesky AT-URI mapping" until the mapping exists;
    # past that, whatever the far end answers the thread fetch is beside the
    # point.
    def mapped() -> bool:
        try:
            _call(nest, "bluesky.feed.thread", {"PostId": {"post_id": bytes.fromhex(doomed_id)}})
        except RpcCallError as e:
            return "mapping" not in str(e.details)
        return True

    wait_until(
        mapped, REACH_S,
        diagnose=lambda: f"the cross-post mapping never landed; far end saw {far.requests!r}",
    )

    tombstone = sign_and_encode_tombstone(
        nest["user"]["signing_key"], doomed_id, int(time.time() * 1_000_000),
    )
    assert _call(nest, "fauna.posts.delete", {"body": tombstone})["deleted"] is True

    deleted = wait_until(
        lambda: [d for d in far.deleted_snapshot()
                 if d.get("collection") == POST_COLLECTION and d.get("rkey") == f"fake{n}"],
        REACH_S,
        diagnose=lambda: (
            f"deleting the cross-posted post must delete its copy fake{n}; "
            f"deleteRecord bodies seen: {far.deleted_snapshot()!r}; "
            f"far end saw {far.requests!r}"
        ),
    )
    assert len(deleted) == 1 and deleted[0].get("repo") == far.did, (
        f"one delete, in the linked account's repo {far.did!r}: {deleted!r}"
    )
    assert len(far.deleted_snapshot()) == 1, (
        f"only the deleted post's copy may go; deleteRecord bodies: {far.deleted_snapshot()!r}"
    )


@pytest.mark.feature("atproto")
def test_starting_to_host_unlinks_the_linked_account_and_leaves_it_untouched(
    bluesky_feed_nest, atproto_far_end,
):
    """Linked → Hosted visible: the consume-side link is gone when the move
    answers, and the linked account's PDS was sent nothing that changes it —
    no delete, no write, no account call.

    The hosted half (the did:plc mint) is asynchronous and the subject of
    `test_atproto_did_method.py`; the unlink is the move's synchronous first
    effect (`bridge_atproto_handlers.rs`, teardown before buildup), so it is
    settled by the time the call returns."""
    nest, far = bluesky_feed_nest, atproto_far_end
    _link_through_callback(nest, far)
    assert _bluesky_linked(nest), "precondition: the Bluesky account is linked"
    before = len(far.requests)

    _call(nest, "fauna.bridges.atproto.set_integration_level", {
        "target_level": "hosted_visible",
        "did_method": "plc",
        "user_rotation_pub_did_key": USER_ROTATION_PUB,
        "history_backfill": False,
    })

    assert not _bluesky_linked(nest), (
        "starting to host must disconnect the linked Bluesky account"
    )
    during = far.requests[before:]
    mutations = [r for r in during if r[0] != "GET"]
    assert mutations == [], (
        f"the linked account itself must be left untouched; its PDS was sent {mutations!r}"
    )
    assert far.deleted_snapshot() == [] and far.created_snapshot() == [], (
        f"no record of the linked account may change: createRecord "
        f"{far.created_snapshot()!r}, deleteRecord {far.deleted_snapshot()!r}"
    )
