"""Private (fragment-keyed) share links — what the NEST serves, observed from
outside as a stranger and an unfurler see it: three things, none of them
plaintext, none naming the file, none carrying the key; nothing at all once the
link is revoked.

Owner doc: ``docs/goal/behavior/share-links.md`` § The private-file extension
(*What the nest stores*, *What the nest serves — three things, never
plaintext*, *What an unfurler gets*); the route and its refusals are
``docs/goal/architecture/core-client-kind-catalog.md`` § Share. Catalog
outcomes ``share-links`` 21, 22 and 27. The person opening the link in a
browser is ``tests/test_share_links_private.py``.

Fixture setup (convention 5): ``helpers/private_share.py``. The nest serves the
SPA build so its navigation answer is the real viewer page.
"""

from __future__ import annotations

import pytest

import fauna_ffi

from conftest import _make_user
from helpers.private_share import arms, get, leaks, seed_private_link, text_content
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

FILENAME = "holiday-plan.txt"


@pytest.mark.feature("share-links")
def test_the_nest_serves_a_private_link_blind_and_stops_on_revoke(share_viewer_nest):
    nest = share_viewer_nest
    owner = _make_user(nest)
    link = seed_private_link(nest, owner, text_content(), FILENAME)
    assert link.path_url.startswith(nest["url"].rstrip("/") + "/share/"), link.path_url

    # ── Outcome 21: the three answers carry no plaintext, no name, no key. ──
    served = [(path, *get(path, navigation=navigation)) for path, navigation in arms(link)]
    assert all(status == 200 for _, status, _ in served), "; ".join(
        f"{path[len(link.path_url):] or '(page)'} -> {status} {body[:80]!r}"
        for path, status, body in served
    )
    for path, _, body in served:
        assert not leaks(body, link.content), f"{path} carries plaintext"
        assert FILENAME.encode() not in body, f"{path} names the file"
        assert link.key.encode() not in body, f"{path} carries the link key"

    # ── Outcome 27: a fetch with no fragment — an unfurler's, a chat app's
    # pre-fetch — gets the viewer page's generic title and nothing else. ──
    status, page = get(link.path_url, navigation=True)
    assert status == 200
    assert b'id="share-viewer"' in page, "the navigation did not answer the viewer page"
    assert b"<title>A file shared with Fauna</title>" in page, page[:400]
    assert b"_app/immutable" not in page, "the navigation answered the app shell"

    # ── Outcome 22: a revoke stops the nest serving every part of it. ──
    with _actor_client(nest["url"], owner) as ws:
        ws.call("fauna.share.revoke", {"token_id": link.record["token_id"]})
    for path, navigation in arms(link):
        status, body = get(path, navigation=navigation)
        assert status == 410, (path, status)
        assert not leaks(body, link.content)


def test_the_link_alone_opens_the_file_through_the_viewers_shared_open(nest_instance):
    """The positive half of what the nest serves: the link and the nest's two
    data answers are enough to recover the file, through the viewer's own
    shared open + assemble (``fauna_ffi.share_viewer_open`` — the
    ``fauna_client_share::viewer`` functions the viewer page's wasm runs).
    No SPA is needed for the data arms, so this runs against the plain session
    nest; the stranger's helper for every app-driven private-link journey
    (``tests/test_media_upload_one_shape.py``) leans on it."""
    owner = _make_user(nest_instance)
    link = seed_private_link(nest_instance, owner, text_content(), FILENAME)

    def fetch(path_url: str) -> bytes:
        status, body = get(path_url)
        assert status == 200, (path_url, status, body[:120])
        return body

    name, got = fauna_ffi.share_viewer_open(link.url, fetch)
    assert name == FILENAME
    assert got == link.content

    # The key is the whole capability: the same answers under another key
    # open nothing.
    wrong = link.path_url + "#" + "A" * len(link.key)
    with pytest.raises(RuntimeError, match="viewer"):
        fauna_ffi.share_viewer_open(wrong, fetch)
