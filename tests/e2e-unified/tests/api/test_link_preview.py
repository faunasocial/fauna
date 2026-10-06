"""tier_3 e2e for the D4 link-preview resolver — ``fauna.linkpreview.resolve``.

The producer is **nest-side** (render-model.md § D4): a per-app render manager
resolves a bare-url post's OpenGraph metadata through this authenticated kind so
the client never contacts the third party (a client-side fetch leaks the user's
IP to every linked site and is CORS-blocked on web). The nest does the
SSRF-guarded fetch, the OpenGraph/meta parse, the og:image content-addressed
blob store, and the by-url cache.

The real outbound fetcher correctly refuses every test-local address (there is
**no loopback carve-out** — a user could post ``http://127.0.0.1:<port>/``), so
the served-OG success path is driven through the ``test-hooks`` fixture override
(``POST /api/v1/test/linkpreview/fixture``): a *mapped* url is served from the
fixture, an *unmapped* url still falls through to the real SSRF-guarded fetcher —
so the served-OG success and the genuine private-IP rejection are both exercised
without loosening production SSRF.
"""

import json
import urllib.error
import urllib.request

from common import create_actor_and_register, port_base_url

import pytest

from tests.api import ws_api

pytestmark = pytest.mark.tier_3

PAGE_URL = "http://og.test.example/article"
IMAGE_URL = "http://og.test.example/cover.png"


def _install_fixture(port: int, payload: dict) -> None:
    req = urllib.request.Request(
        f"{port_base_url(port)}/api/v1/test/linkpreview/fixture",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    urllib.request.urlopen(req).read()


def _install_page(port: int, url: str, html: str) -> None:
    _install_fixture(port, {"url": url, "html": html})


def _install_image(port: int, url: str) -> None:
    _install_fixture(port, {"url": url, "image": True})


def _download_blob(port: int, blob_hash: str) -> tuple[int, bytes]:
    """GET a blob by hash → (status, body). The og:image is served through the
    unauthenticated media route the client resolves images with."""
    req = urllib.request.Request(
        f"{port_base_url(port)}/api/v1/blob/{blob_hash}", method="GET"
    )
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, b""


@pytest.mark.feature("link-previews")
def test_resolves_served_og_page_with_image(two_nodes):
    """A served OpenGraph page → Resolved with title/description and an og:image
    stored as a content-addressed blob the client can fetch."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    html = (
        "<html><head>"
        '<meta property="og:title" content="A Real Article">'
        '<meta property="og:description" content="An informative description.">'
        f'<meta property="og:image" content="{IMAGE_URL}">'
        "</head></html>"
    )
    _install_image(port, IMAGE_URL)
    _install_page(port, PAGE_URL, html)

    reply = ws_api.resolve_link_preview(port, actor, PAGE_URL)
    assert reply["outcome"] == "resolved", reply
    assert reply["title"] == "A Real Article", reply
    assert reply["description"] == "An informative description.", reply

    image_hash = reply["image_hash"]
    assert image_hash and len(image_hash) == 64, reply

    status, body = _download_blob(port, image_hash)
    assert status == 200, status
    assert body[:4] == b"\x89PNG", body[:8]


@pytest.mark.feature("link-previews")
def test_private_ip_url_fails(two_nodes):
    """An un-installed private-IP url falls through to the real SSRF-guarded
    fetcher (the fixture override only serves *mapped* urls), which refuses it →
    Failed. No IP leak, no internal reach — even with the override active."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    # A decoy fixture makes the override path active; 10.0.0.5 is still unmapped,
    # so it must hit the production SSRF rejection.
    _install_page(port, PAGE_URL, "<html></html>")

    reply = ws_api.resolve_link_preview(port, actor, "http://10.0.0.5/")
    assert reply["outcome"] == "failed", reply


def test_non_og_page_fails(two_nodes):
    """A page with no OpenGraph/title/description → Failed (plain-link fallback)."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    _install_page(port, PAGE_URL, "<html><body><p>no metadata here</p></body></html>")

    reply = ws_api.resolve_link_preview(port, actor, PAGE_URL)
    assert reply["outcome"] == "failed", reply


def test_oversized_page_fails(two_nodes):
    """A page body past the 512 KiB HTML cap is refused before parse → Failed."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    huge = "<html><head><title>x</title></head><body>" + ("a" * 600_000) + "</body></html>"
    _install_page(port, PAGE_URL, huge)

    reply = ws_api.resolve_link_preview(port, actor, PAGE_URL)
    assert reply["outcome"] == "failed", reply


def test_overlong_url_rejected(two_nodes):
    """A url past the MAX_URL_LEN (8 KiB) byte cap is rejected as malformed
    *before* it becomes a cache key or reaches the fetcher (LP-2 hardening)."""
    from clients.ws_rpc_admin_client import RpcCallError

    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    overlong = "http://og.test.example/" + ("a" * 9000)  # > 8 KiB cap
    with pytest.raises(RpcCallError) as excinfo:
        ws_api.resolve_link_preview(port, actor, overlong)
    assert excinfo.value.code == "fauna.protocol.malformed", excinfo.value.code


@pytest.mark.feature("link-previews")
def test_cache_hit_returns_first_result(two_nodes):
    """The nest caches by url: a second resolve of the same url returns the first
    result without re-fetching, even after the served page changes."""
    port = two_nodes["port_a"]
    actor = create_actor_and_register(port, admin_signing_key=two_nodes["admin_sk_a"])

    _install_page(
        port, PAGE_URL, '<html><head><meta property="og:title" content="First"></head></html>'
    )
    first = ws_api.resolve_link_preview(port, actor, PAGE_URL)
    assert first["outcome"] == "resolved" and first["title"] == "First", first

    # Swap the served page; a cache hit must still return the FIRST title.
    _install_page(
        port, PAGE_URL, '<html><head><meta property="og:title" content="Second"></head></html>'
    )
    second = ws_api.resolve_link_preview(port, actor, PAGE_URL)
    assert second["outcome"] == "resolved" and second["title"] == "First", second
