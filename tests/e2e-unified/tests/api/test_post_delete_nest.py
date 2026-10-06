"""tier_3 nest-side witnesses for author-only post deletion (`feed.md` § Post deletion).

The four `[nest]` outcomes of `docs/features/feed-delete-own-post.md` — the half
the app-tier `tests/test_feed_post_delete.py` cannot see, because it drives one
signed-in seat through one app's UI and the sentences here are about what the
*nest* does to everybody else's reads:

* **outcome 3** — only the author can delete; anyone else's attempt is refused
  and the post stays. Two of the three author checks `delete_post_core` makes
  are reachable over the wire and both are exercised: a non-author's own signed
  tombstone naming someone else's post (the *stored post's author* check), and
  a genuine author-signed tombstone replayed over a different connection (the
  *connection actor* check). The third — the envelope signature against
  `tombstone.author` — is unforgeable without the author's key, so it has no
  wire arm to drive.
* **outcome 4** — a deleted post is gone, not hidden: it leaves every feed for
  every reader and `fauna.posts.get` answers `not_found`.
* **outcome 5** — deleting a reply/repost/quote takes its count back off the
  post it referenced (the `engagement_events` reversal, the exact mirror of the
  increment `test_engagement_counts.py` pins).
* **outcome 6** — deleting a still-web-published post takes its rendered page
  down, through the REAL HTTP serve door.

Production flow asserted end-to-end, per outcome: the app's `fauna.posts.delete`
call → `posts_handlers::posts_delete_handler` (verify-then-decode the signed
tombstone) → `routes::delete_post_core` (three author checks → projection-row
delete → segment tombstone → interaction-counter reversal → `RenderSite::Now`)
→ what every *other* door then answers: `fauna.feed.local.posts` /
`fauna.feed.posts` (the projection is the serving gate), `fauna.posts.get`
(`not_found`), the target's `FeedPostItem.{reply,repost,quote}_count`, and
`GET /post/<slug>.html` on the web-serving surface.

Every assertion is a state read taken after a call the nest has already
acknowledged — the removal order is synchronous inside `delete_post_core` and
the re-render is `RenderSite::Now` — so nothing here waits on wall-clock
(convention 14).
"""

import time
import urllib.error
import urllib.request

import pytest
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common import create_actor_and_register

from tests.api import ws_api
from tests.api.bare import post_reference, sign_and_encode_post, sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3

INFO_PAGE_MARKER = "Fauna Nest API"


def _now_us() -> int:
    return int(time.time() * 1_000_000)


def _feed_ids(posts: list[dict]) -> set[str]:
    return {p["post_id"] for p in posts}


def _post_in_feed(port: int, actor: dict, post_id: str) -> dict:
    """The single ``FeedPostItem`` dict for ``post_id`` from the local feed."""
    for p in ws_api.local_feed_posts(port, actor):
        if p["post_id"] == post_id:
            return p
    raise AssertionError(f"post {post_id[:16]} not in local feed")


def _get(url: str, path: str, host: str | None = None) -> tuple[int, str]:
    """GET `path` on the nest's web-serving surface, returning (status, body)."""
    headers = {"Host": host} if host else {}
    req = urllib.request.Request(url.rstrip("/") + path, method="GET", headers=headers)
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


@pytest.mark.feature("feed-delete-own-post")
def test_only_the_author_can_delete_a_post(two_nodes):
    """A non-author's delete is refused and the post stays; the author's succeeds."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    mallory = create_actor_and_register(port, admin_signing_key=admin_sk)

    post_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], _now_us(), "Mine to delete")
    )
    assert _post_in_feed(port, author, post_id), "precondition: the post is live"

    # ── check 3 (the STORED post's author) — mallory's own, perfectly valid
    # tombstone naming a post she does not own. Signature verifies against
    # `tombstone.author` = mallory and the connection actor IS mallory, so this
    # is exactly the check that has to refuse it.
    mallory_tombstone = sign_and_encode_tombstone(
        mallory["signing_key"], post_id, _now_us()
    )
    with pytest.raises(RpcCallError) as refused:
        ws_api.delete_post(port, mallory, mallory_tombstone)
    assert refused.value.code == "fauna.posts.permission_denied", refused.value.code
    assert _post_in_feed(port, author, post_id), "a refused delete leaves the post standing"
    assert ws_api.get_post(port, mallory, post_id), "...and still readable by everyone"

    # ── check 1 (the CONNECTION actor) — the author's OWN signed tombstone,
    # replayed over mallory's connection. The bytes are genuine and verify
    # against `tombstone.author` = the author, so the signature check passes and
    # only the connection binding stands between mallory and someone else's
    # post. The SAME bytes go through for the author below, which is what makes
    # this arm about the connection and nothing else.
    author_tombstone = sign_and_encode_tombstone(
        author["signing_key"], post_id, _now_us()
    )
    with pytest.raises(RpcCallError) as replayed:
        ws_api.delete_post(port, mallory, author_tombstone)
    assert replayed.value.code == "fauna.posts.permission_denied", replayed.value.code
    assert _post_in_feed(port, author, post_id), "a replayed tombstone leaves the post standing"

    # ── the positive control: the author self-serves, same bytes. Without this
    # the two refusals above would also be green on a nest that refused EVERY
    # delete.
    reply = ws_api.delete_post(port, author, author_tombstone)
    assert reply["deleted"] is True, reply
    assert post_id not in _feed_ids(ws_api.local_feed_posts(port, author)), (
        "the author's own delete removes the post"
    )


@pytest.mark.feature("feed-delete-own-post")
def test_a_deleted_post_is_gone_for_every_reader(two_nodes):
    """DELETE, not hide: no feed returns it to ANY reader and `get` answers not_found."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    reader = create_actor_and_register(port, admin_signing_key=admin_sk)

    post_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], _now_us(), "Here now, gone soon")
    )
    # A second reader's own catch-all feed — the `fauna.feed.posts` door, which
    # reads the same `content` projection by a different query than the local
    # feed, so a projection row that survived for one would survive for it.
    reader_feed = ws_api.create_feed(port, reader, "Everything", rules=[])

    assert post_id in _feed_ids(ws_api.local_feed_posts(port, author)), "author's local feed"
    assert post_id in _feed_ids(ws_api.local_feed_posts(port, reader)), "reader's local feed"
    assert post_id in _feed_ids(ws_api.feed_posts(port, reader, reader_feed)), "reader's feed"
    assert ws_api.get_post(port, reader, post_id), "a reader can open it by id"

    ws_api.delete_post(
        port, author, sign_and_encode_tombstone(author["signing_key"], post_id, _now_us())
    )

    # Gone for the author...
    assert post_id not in _feed_ids(ws_api.local_feed_posts(port, author))
    # ...and for a reader who is not the author, on both feed doors.
    assert post_id not in _feed_ids(ws_api.local_feed_posts(port, reader))
    assert post_id not in _feed_ids(ws_api.feed_posts(port, reader, reader_feed))

    # Not hidden-but-fetchable: the by-id door answers not_found to everyone,
    # the author included (it is destroyed, not merely unlisted).
    for who, label in ((reader, "reader"), (author, "author")):
        with pytest.raises(RpcCallError) as gone:
            ws_api.get_post(port, who, post_id)
        assert gone.value.code == "fauna.posts.not_found", f"{label}: {gone.value.code}"

    # Deleting again is the idempotent success, never an error (`deleted:false`).
    again = ws_api.delete_post(
        port, author, sign_and_encode_tombstone(author["signing_key"], post_id, _now_us())
    )
    assert again["deleted"] is False, again


@pytest.mark.parametrize(
    "kind,count_field",
    [
        ("Reply", "reply_count"),
        ("Repost", "repost_count"),
        ("Quote", "quote_count"),
    ],
)
@pytest.mark.feature("feed-delete-own-post")
def test_deleting_a_reference_post_reverses_the_targets_count(two_nodes, kind, count_field):
    """Deleting a reply/repost/quote takes its count back off the referenced post."""
    port = two_nodes["port_a"]
    admin_sk = two_nodes["admin_sk_a"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    now_us = _now_us()
    target_id = ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, "Reference me")
    )
    assert _post_in_feed(port, author, target_id)[count_field] == 0

    referencing_id = ws_api.create_post(
        port,
        actor,
        sign_and_encode_post(
            actor["signing_key"],
            now_us + 1_000_000,
            f"a {kind} of the target",
            references=[post_reference(kind, target_id)],
        ),
    )
    assert _post_in_feed(port, author, target_id)[count_field] == 1, "the increment landed"

    # The reversal: the referencing post's author deletes it, and the TARGET's
    # counter — a different post, owned by someone else — comes back down.
    reply = ws_api.delete_post(
        port, actor, sign_and_encode_tombstone(actor["signing_key"], referencing_id, _now_us())
    )
    assert reply["deleted"] is True, reply
    assert _post_in_feed(port, author, target_id)[count_field] == 0, (
        f"deleting the {kind} must take its {count_field} back off the target"
    )

    # The target itself is untouched — a delete never destroys the post it
    # referenced (§ Post deletion, "references TO the deleted post dangle by
    # design" read from the other side).
    assert ws_api.get_post(port, author, target_id), "the referenced post survives"


@pytest.mark.feature("feed-delete-own-post")
def test_deleting_a_published_post_takes_its_web_page_down(nest_instance):
    """A still-published post's rendered page stops serving the moment it is deleted.

    The author publishes **two** posts and deletes one. Two is what makes the
    assertions say what the outcome says: the site survives the delete, so the
    re-rendered index is a live document that dropped exactly one entry rather
    than a site that ceased to exist. (A site whose last page is deleted answers
    404 everywhere, which is the ratified whole-site-or-none shape — § Routing,
    render, serving — and would make a one-post version of this test pass for
    the wrong reason.)
    """
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    kept_title = "Still Published Afterwards"
    doomed_title = "Published Then Deleted"
    now_us = _now_us()
    kept_id = ws_api.create_post(
        port,
        actor,
        sign_and_encode_post(
            actor["signing_key"], now_us, f"{kept_title}\n\nThis one stays up."
        ),
    )
    doomed_id = ws_api.create_post(
        port,
        actor,
        sign_and_encode_post(
            actor["signing_key"],
            now_us + 1_000_000,
            f"{doomed_title}\n\nServed from my own nest, for now.",
        ),
    )
    kept_slug = ws_api.web_publish_set(port, actor, kept_id, slug="still-published")
    doomed_slug = ws_api.web_publish_set(port, actor, doomed_id, slug="published-then-deleted")

    # Serve the author's site at the apex — the shared session nest has no
    # domain, so the apex catch-all answers every host (the same seam
    # `test_web_apex_hosting` and `test_web_paywall` use).
    admin_ws = WsRpcAdminClient(
        url, actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )
    with admin_ws:
        admin_ws.call("fauna.web.set_apex_actor", {"actor_id": actor["actor_id_bytes"]})

    try:
        # Precondition: both published posts really are being served as pages,
        # and the site index links both.
        for slug, title in ((kept_slug, kept_title), (doomed_slug, doomed_title)):
            status, body = _get(url, f"/post/{slug}.html")
            assert status == 200, f"precondition: {slug} renders a page; got {status}"
            assert title in body, body
        status, index = _get(url, "/")
        assert status == 200, index
        assert f"/post/{kept_slug}" in index and f"/post/{doomed_slug}" in index, (
            f"precondition: the site index links both published posts; got {index}"
        )

        ws_api.delete_post(
            port, actor, sign_and_encode_tombstone(actor["signing_key"], doomed_id, _now_us())
        )

        # The delete fires the re-render itself (`RenderSite::Now`) — the page is
        # gone by the time the call returns, not at some later sweep.
        status, body = _get(url, f"/post/{doomed_slug}.html")
        assert status == 404, (
            f"a deleted post's page must stop serving at once; got {status}: {body}"
        )
        # ...and the site it was part of no longer advertises it either — a 404
        # under a stale index would still leave the post visible to a reader.
        status, index = _get(url, "/")
        assert status == 200, index
        assert doomed_title not in index and f"/post/{doomed_slug}" not in index, (
            f"the re-rendered index still lists the deleted post: {index}"
        )
        # The rest of the site is untouched: deleting one post re-renders the
        # site, it does not take it down.
        assert f"/post/{kept_slug}" in index, (
            f"the re-render dropped the surviving post from the index: {index}"
        )
        status, body = _get(url, f"/post/{kept_slug}.html")
        assert status == 200 and kept_title in body, (
            f"the surviving post's page must keep serving; got {status}: {body}"
        )
    finally:
        # Leave the shared session nest apex-clean for sibling tests.
        with admin_ws:
            admin_ws.call("fauna.web.set_apex_actor", {"actor_id": None})
