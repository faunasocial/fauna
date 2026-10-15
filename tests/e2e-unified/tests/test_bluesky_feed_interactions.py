"""Liking or reposting a Bluesky post from Fauna, and undoing it, does the same
on Bluesky through the linked account.

`docs/goal/behavior/bridges.md` § Interactions (like / repost / unlike /
unrepost on a ``bluesky``-source post → ``createRecord`` / ``deleteRecord``
through ``route_unified_interaction``) and `docs/goal/ui/feed.md`
§ Interaction bar → *The viewer-state carrier* (a Bluesky row carries the
viewer's own like/repost, so the second tap is the undo). Catalog outcome
``atproto`` 41.

The far end, the link through the real OAuth callback, the poll-now seam and
the sign-in-on-the-ingested-post precondition are imported from
`test_bluesky_feed_ingest.py` / `test_bluesky_feed_reference.py` rather than
copied (e2e rule 8b: preconditions, not shortcuts). The subject — the two taps
on each button — is the app's.

Between the taps the test waits on the app's own viewer state (the feed
snapshot's ``viewer_liked`` / ``viewer_repost_id``) rather than a delay
(convention 14): the second tap is only an undo once the first has landed.

tier_3: a locally-built nest and the real app; the atproto network is a fake.
tui first, the lead app; linux and web follow; the remaining apps gain their
markers in the batched trickle-down.
"""

from __future__ import annotations

import time

import pytest

from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    _link_through_callback,
    atproto_far_end,
    bluesky_feed_nest,
    bluesky_feed_spa_url,
)
from tests.test_bluesky_feed_reference import (
    _call,
    _signed_in_on_the_ingested_post,
)

pytestmark = [pytest.mark.tier_3]

#: verb → (the interaction-bar tap, the record collection, the snapshot's
#: viewer-state read for "on").
VERBS = {
    "like": ("like_post", "app.bsky.feed.like", lambda row: bool(row.get("viewer_liked"))),
    "repost": ("repost_post", "app.bsky.feed.repost",
               lambda row: row.get("viewer_repost_id") is not None),
}


def _wait(predicate, what: str, diag, timeout: float = 30.0):
    deadline = time.monotonic() + timeout
    value = predicate()
    while not value and time.monotonic() < deadline:
        time.sleep(0.3)  # sleep-ok: pacing between poll iterations
        value = predicate()
    assert value, f"{what}; {diag()}"
    return value


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("atproto")
@pytest.mark.parametrize("verb", sorted(VERBS))
def test_a_bluesky_like_or_repost_and_its_undo_reach_bluesky(
    request, app, bluesky_feed_nest, atproto_far_end, verb,
):
    """First tap → a ``createRecord`` of ``app.bsky.feed.<verb>`` whose
    ``subject.uri`` is the post; second tap → a ``deleteRecord`` of exactly
    that record, and the button reads off again."""
    nest, far = bluesky_feed_nest, atproto_far_end
    tap, collection, is_on = VERBS[verb]
    _link_through_callback(nest, far)
    post_uri, index = _signed_in_on_the_ingested_post(
        request, app, nest, far, f"a bluesky post to {verb} and undo", f"3k{verb}undo",
    )
    post_id = next(
        p["post_id"] for p in _call(nest, "fauna.feed.local.posts", {"limit": 50})["posts"]
        if p.get("source") == "bluesky" and p.get("body") == f"a bluesky post to {verb} and undo"
    )

    def diag():
        return (f"createRecord: {far.created_snapshot()!r}; deleteRecord: "
                f"{far.deleted_snapshot()!r}; app row: {app.feed.post_state_by_id(post_id)!r}; "
                f"app error={app.error_text()!r}")

    def creates():
        return [
            (n, r) for n, r in enumerate(far.created_snapshot(), start=1)
            if r.get("collection") == collection
            and ((r.get("record") or {}).get("subject") or {}).get("uri") == post_uri
        ]

    getattr(app.feed, tap)(index)
    created = _wait(creates, f"the {verb} must publish an {collection} record of {post_uri}", diag)
    assert len(created) == 1, f"one tap, one record: {created!r}"
    n, record = created[0]
    assert ((record.get("record") or {}).get("subject") or {}).get("cid"), (
        f"the {verb} record must carry the subject's cid: {record!r}"
    )
    _wait(lambda: is_on(app.feed.post_state_by_id(post_id) or {}),
          f"after the {verb} lands the app must read the post as {verb}d by me", diag)

    getattr(app.feed, tap)(index)
    deleted = _wait(
        lambda: [d for d in far.deleted_snapshot()
                 if d.get("collection") == collection and d.get("rkey") == f"fake{n}"],
        f"the second tap must delete the {collection} record fake{n}", diag,
    )
    assert len(deleted) == 1, f"one undo, one delete: {deleted!r}"
    assert deleted[0].get("repo") == far.did, f"the delete names the linked repo: {deleted!r}"
    _wait(lambda: not is_on(app.feed.post_state_by_id(post_id) or {}),
          f"after the undo the app must read the post as not {verb}d", diag)
    assert len(creates()) == 1, f"the undo must not publish another record: {diag()}"
