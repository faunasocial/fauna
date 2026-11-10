"""Two of the nest's promises about the user's linked Bluesky account, read from
outside a running nest — `docs/features/atproto.md` outcomes 54 and 55.

**Outcome 54** — `docs/goal/behavior/takedown-and-block.md` § Legal takedown →
*The linked Bluesky account's cross-post*: a takedown runs the author's own
delete leg (``bluesky::spawn_write_through_delete``) on the record the nest's
``write_through`` cross-post made, the leg retries a failed ``deleteRecord``
(retaining the post → AT-URI mapping across failures), and a restore re-creates
nothing — the member posts again. The Rust pins (``bins/fauna-nest/tests/
bluesky_legal_takedown_delete.rs``, which holds no PDS, and ``post_delete_redrive``'s
unit tests) see only the attempt; this sees the far end refuse the first
``deleteRecord`` and receive the second. `test_legal_takedown_off_box_retraction.py`
(admin-nest outcome 39) is the plain success arm beside the Nostr leg.

**Outcome 55** — `docs/goal/behavior/web-content-hosting.md` § Implementation
status today, *One SSRF-safe media-proxy core*: the Bluesky picture route
(``/api/v1/bluesky/media``, the path every ingested picture rests as) fetches
only ``https``, only from the bsky CDN, follows no redirect, never dials a
non-global address, and serves only an inert content type, with ``nosniff`` and
``inline``. The CDN host is a loopback HTTPS server (:class:`helpers.
remote_media_server.RemoteMediaServer`) reached through the proxy's two
``test-hooks`` seams, which are mapped-host-only: every other host meets the
full guard. The private-address arm runs on both doors of the one core — the
Bluesky route refuses any host off its allowlist, and the shared
``/api/v1/media/proxy`` (whose only policy is the core's) refuses a name that
resolves to loopback and a literal private address.

No app is driven: the author's settings, post and the admin's takedown go over
WS-RPC, the media fetches over the user's bearer. Every wait is on recorded
far-side state, never on a clock (convention 14); "nothing was re-created" is
bounded by a sentinel post's create reaching the far end after the restore.

tier_3: a locally built nest (``bluesky`` + ``test-hooks``); the networks are
fakes.
"""

from __future__ import annotations

import secrets
import time

import pytest
import requests
from clients._ws_rpc_core import RpcCallError

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.test_bluesky_feed_ingest import (  # noqa: F401 — fixtures
    FEED_DOMAIN,
    _link_through_callback,
    atproto_far_end,
)
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

#: `write_through` 1 — automatic cross-posting (`bluesky/bridge_provider.rs`).
AUTO = 1

#: A fire-and-forget leg reaching a far end — the delete leg's first retry
#: sleeps 1 s (`spawn_write_through_delete`'s ladder), well inside it.
REACH_S = 30.0

POST_COLLECTION = "app.bsky.feed.post"

#: The host the Bluesky media route allowlists
#: (`fauna_bridge_atproto::media::validate_media_url`).
CDN_HOST = "cdn.bsky.app"

JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01fake-picture-bytes\xff\xd9"
SVG = (b'<svg xmlns="http://www.w3.org/2000/svg">'
       b"<script>alert(document.domain)</script></svg>")
HTML = b"<html><body><script>alert(document.domain)</script></body></html>"

#: Every type the proxy may answer with (`media_proxy_routes::SAFE_MEDIA_TYPES`'s
#: images plus the relabel target) — the inert set this test reads against.
INERT_FOR_THIS_TEST = {"image/jpeg", "application/octet-stream"}


@pytest.fixture
def cdn(tmp_path_factory):
    from helpers.remote_media_server import RemoteMediaServer

    server = RemoteMediaServer(
        CDN_HOST, tmp_path_factory.mktemp("bsky-cdn"),
        files={
            "/img/picture.jpg": (JPEG, "image/jpeg"),
            "/img/picture.svg": (SVG, "image/svg+xml"),
            "/img/page.html": (HTML, "text/html; charset=utf-8"),
            "/img/redirect-target.jpg": (JPEG, "image/jpeg"),
        },
        redirects={"/img/moved.jpg": f"https://{CDN_HOST}/img/redirect-target.jpg"},
    )
    yield server
    server.close()


@pytest.fixture
def bluesky_nest(bluesky_nest_binary, tmp_path_factory, atproto_far_end, cdn):
    """A `bluesky` nest claimed onto a public domain (its Bluesky OAuth client
    derives from it), its Bluesky client routed to :func:`atproto_far_end` and
    its media proxy's CDN host to :func:`cdn`, with one registered user."""
    from conftest import _make_nest, _make_user

    nest, cleanup = _make_nest(
        bluesky_nest_binary, tmp_path_factory, "bluesky-takedown-media",
        claim_domain=FEED_DOMAIN,
        extra_env={
            "FAUNA_TEST_ATPROTO_FAR_END": atproto_far_end.url,
            **cdn.nest_env(),
        },
    )
    nest["user"] = _make_user(nest)
    yield nest
    cleanup()


def _admin(nest) -> dict:
    sk = nest["admin"]["signing_key"]
    return {"actor_id_bytes": bytes(sk.verify_key), "signing_key": sk}


def _created(far, text: str) -> list[tuple[int, dict]]:
    """The far end's ``createRecord`` bodies publishing ``text``, each with its
    1-based ordinal — the fake answers the n-th with rkey ``fake<n>``."""
    return [
        (n, r) for n, r in enumerate(far.created_snapshot(), start=1)
        if r.get("collection") == POST_COLLECTION
        and (r.get("record") or {}).get("text") == text
    ]


@pytest.mark.feature("atproto")
def test_a_takedown_deletes_the_bluesky_cross_post_retrying_and_a_restore_reposts_nothing(
    bluesky_nest, atproto_far_end,
):
    """Outcome 54 — a post cross-posted to the author's linked Bluesky account
    is taken down while the PDS refuses deletes: the first ``deleteRecord`` is
    refused, the nest tries again and the record goes. The admin then restores
    the post, and the far end is never asked to create it again."""
    nest, far = bluesky_nest, atproto_far_end
    user, port = nest["user"], nest["port"]

    _link_through_callback(nest, far, user)
    ws_api.bridge_set_settings(port, user, "bluesky", {"write_through": AUTO})

    def post(text: str) -> str:
        return ws_api.create_post(
            port, user,
            sign_and_encode_post(user["signing_key"], int(time.time() * 1_000_000), text),
        )

    doomed = f"a post a court orders down {secrets.token_hex(4)}"
    doomed_id = post(doomed)
    [(n, _)] = wait_until(
        lambda: _created(far, doomed), REACH_S,
        diagnose=lambda: f"the post was never cross-posted; createRecord: {far.created_snapshot()!r}",
    )

    # The delete leg reads the post → AT-URI mapping the cross-post wrote when
    # `createRecord` answered; `bluesky.feed.thread` by post id refuses with
    # "no Bluesky AT-URI mapping" until it lands.
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

    # The PDS is down for the first delete.
    far.refuse_next_deletes(1)
    taken = ws_api.moderation_legal_takedown(
        port, _admin(nest), doomed_id, legal_reference="EU-DSA-2026/0054",
    )
    assert taken.get("status") == "taken_down", taken

    def ours(bodies: list[dict]) -> list[dict]:
        return [d for d in bodies
                if d.get("collection") == POST_COLLECTION and d.get("rkey") == f"fake{n}"]

    deleted = wait_until(
        lambda: ours(far.deleted_snapshot()), REACH_S,
        diagnose=lambda: (
            f"the takedown must retry until the Bluesky copy fake{n} is deleted; "
            f"deleteRecord attempts: {far.delete_attempts_snapshot()!r}"
        ),
    )
    attempts = ours(far.delete_attempts_snapshot())
    assert len(attempts) >= 2, (
        f"the refused delete was the first attempt and a retry succeeded: {attempts!r}"
    )
    assert len(deleted) == 1 and deleted[0].get("repo") == far.did, deleted

    # The overturn: the post is servable again on the nest, and nothing
    # re-creates its Bluesky copy — the member posts again if they want one.
    restored = ws_api.moderation_legal_takedown(port, _admin(nest), doomed_id, restore=True)
    assert restored.get("status") == "restored", restored

    # The bound on "nothing happened": a fresh post's create reaches the far
    # end after the restore, on the same write-through path a re-create would
    # take.
    sentinel = f"a post after the overturn {secrets.token_hex(4)}"
    post(sentinel)
    wait_until(
        lambda: _created(far, sentinel), REACH_S,
        diagnose=lambda: f"the sentinel post was never cross-posted; createRecord: {far.created_snapshot()!r}",
    )
    assert len(_created(far, doomed)) == 1, (
        f"a restore must not re-post the taken-down post to Bluesky: {far.created_snapshot()!r}"
    )
    assert len(ours(far.deleted_snapshot())) == 1, (
        f"the record is deleted once, and nothing chases it after the restore: "
        f"{far.deleted_snapshot()!r}"
    )


def _get(nest, path: str, url: str) -> requests.Response:
    return requests.get(
        f"{nest['url']}{path}", params={"url": url},
        headers={"Authorization": f"Bearer {nest['user']['token']}"},
        allow_redirects=False, verify=False, timeout=60,
    )


def _bsky_media(nest, url: str) -> requests.Response:
    return _get(nest, "/api/v1/bluesky/media", url)


@pytest.mark.feature("atproto")
def test_bluesky_pictures_are_fetched_only_over_public_https_without_redirects_and_served_inert(
    bluesky_nest, cdn,
):
    """Outcome 55 — through the Bluesky picture route a JPEG on the CDN is
    served as itself with ``nosniff``; an SVG and an HTML page are relabelled
    to an inert type; an ``http`` URL, a host off the CDN and a private address
    are refused without a dial; a redirect is not followed. The shared media
    core refuses a loopback name and a private literal too."""
    nest = bluesky_nest

    # The one fetch that is allowed.
    resp = _bsky_media(nest, cdn.url("/img/picture.jpg"))
    assert resp.status_code == 200, f"a CDN JPEG is served: {resp.status_code} {resp.text[:200]!r}"
    assert resp.content == JPEG
    assert resp.headers.get("content-type") == "image/jpeg", resp.headers
    assert resp.headers.get("x-content-type-options") == "nosniff", resp.headers
    assert resp.headers.get("content-disposition") == "inline", resp.headers

    # Script-capable bytes come back only as an inert type, never as themselves.
    for path in ("/img/picture.svg", "/img/page.html"):
        resp = _bsky_media(nest, cdn.url(path))
        assert resp.status_code == 200, f"{path}: {resp.status_code} {resp.text[:200]!r}"
        ctype = resp.headers.get("content-type", "")
        assert ctype == "application/octet-stream", (
            f"{path} must be relabelled inert, never reflected as {ctype!r}"
        )
        assert resp.headers.get("x-content-type-options") == "nosniff", resp.headers
        assert ctype in INERT_FOR_THIS_TEST

    # A redirect is answered as a failure and its target never fetched.
    before = list(cdn.requests)
    resp = _bsky_media(nest, cdn.url("/img/moved.jpg"))
    assert resp.status_code == 502, f"a redirect is not followed: {resp.status_code} {resp.text!r}"
    assert "/img/moved.jpg" in cdn.requests[len(before):], (
        f"the redirecting URL itself was fetched: {cdn.requests!r}"
    )
    assert "/img/redirect-target.jpg" not in cdn.requests, (
        f"the proxy must never follow the redirect to its target: {cdn.requests!r}"
    )

    # Refused before any dial: plain http, a host off the CDN, private addresses.
    before = list(cdn.requests)
    for url in (
        f"http://{CDN_HOST}/img/picture.jpg",
        f"https://localhost:{cdn.port}/img/picture.jpg",
        "https://127.0.0.1/img/picture.jpg",
        "https://10.0.0.1/img/picture.jpg",
        "https://evil.example/img/picture.jpg",
    ):
        resp = _bsky_media(nest, url)
        assert resp.status_code == 400, f"{url} must be refused: {resp.status_code} {resp.text!r}"
    assert cdn.requests == before, f"a refused URL reached the remote: {cdn.requests!r}"

    # The shared core both media routes funnel through refuses a name that
    # resolves to loopback (the CDN server itself is listening there) and a
    # private literal, with no host allowlist in front of it.
    for url in (
        f"https://localhost:{cdn.port}/img/picture.jpg",
        "https://10.0.0.1/img/picture.jpg",
        "https://[fd00::1]/img/picture.jpg",
    ):
        resp = _get(nest, "/api/v1/media/proxy", url)
        assert resp.status_code == 400, (
            f"the media core must refuse a non-public address {url}: "
            f"{resp.status_code} {resp.text!r}"
        )
    assert cdn.requests == before, f"a refused URL reached the remote: {cdn.requests!r}"
