"""Two headers a real nest must never send, whatever it serves — the
``web-content-hosting.md`` § Invariants this model relies on nos. 2 and 4, on a
real ``fauna-nest`` binary with a real handle domain and a published site.

* **No cookie, ever** (invariant 2: "No auth cookie may ever be introduced while
  untrusted content shares the site"). Every ``*.<domain>`` is one *site*, so an
  ambient credential would let any user's page act as its visitor against the
  nest. The walk below asks every door a browser can reach on this nest — its
  own pages, its API's anonymous routes, a member's site with its pages, feed,
  files and misses — and asserts not one answer carries ``Set-Cookie``. The
  paywall's own paths are asserted in ``test_web_paywall.py``; the app's page,
  which a tier_3 nest does not ship, in
  ``tests/platform/docker/test_spa_serving.py``.
* **No scope widening for a member's scripts** (invariant 4: "The nest must not
  emit a permissive ``Service-Worker-Allowed`` header for user content"). A
  browser lets a service worker control pages beyond its own directory only
  when the script's response carries that header; the request a browser makes
  to register one carries ``Service-Worker: script``. So the site holds a worker
  script one directory down, the test fetches it exactly as a browser
  registering it would, and asserts the header is absent — on that script and
  on every other answer the site gives.

The site is real: the walk first proves the worker script and an ordinary page
are served (status 200, their own bytes), or the absences prove nothing.
"""

import ssl
import urllib.error
import urllib.request

import pytest

import fauna_ffi

from common.auth import open_registration, register_handled_actor

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_web_folder_audience import DEVICE_ID, _seed_public_file
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

#: Same reasoning as ``test_web_host_routing.NEST_DOMAIN``: a real apex the
#: resolver can strip, and not the publish scrub's target.
NEST_DOMAIN = "sub.example"
HANDLE = "wenche"
SITE = "wenche-site"
WORKER_PATH = "blog/sw.js"
WORKER_BODY = b"self.addEventListener('fetch', () => {});\n"
PAGE_PATH = "about.html"
PAGE_BODY = b"<h1>About</h1><p>served-headers-marker</p>\n"


@pytest.fixture(scope="module")
def served_headers_site_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest with a handle domain and one member whose site holds a
    published post, an ordinary page and a service-worker script.

    Dedicated for the same reason as ``test_web_host_routing.routing_nest``:
    claiming a domain on the shared nest would change every sibling's nest.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-served-headers",
        claim_domain=NEST_DOMAIN,
    )
    open_registration(nest)
    try:
        url, port = nest["url"], nest["port"]
        actor = register_handled_actor(port, handle=HANDLE, domain=NEST_DOMAIN)
        post_bytes = sign_and_encode_post(
            actor["signing_key"], 1_760_000_000_000_000, "Headers Post"
        )
        post_id = ws_api.create_post(port, actor, post_bytes)
        ws_api.web_publish_set(port, actor, post_id, "headers-post")
        with _actor_client(url, actor) as ws:
            fauna_ffi.harness_create_set(
                url, bytes(actor["signing_key"]), {"name": SITE, "audience": "public"},
            )
            ws.call("fauna.folders.update", {"name": SITE, "website_enabled": True})
            ws.call(
                "fauna.sync.register",
                {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
            )
            ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})
        _seed_public_file(url, port, actor, SITE, WORKER_PATH, WORKER_BODY)
        _seed_public_file(url, port, actor, SITE, PAGE_PATH, PAGE_BODY)
        yield nest
    finally:
        cleanup()


def _get(url: str, path: str, host: str, extra: dict | None = None):
    """GET ``path`` as ``host`` → (status, headers, body bytes), never raising
    on an HTTP error status: a 404's headers are as much under test as a 200's."""
    req = urllib.request.Request(
        url.rstrip("/") + path, method="GET", headers={"Host": host, **(extra or {})}
    )
    # A docker-mode nest answers over its self-signed TLS.
    context = ssl._create_unverified_context() if url.startswith("https://") else None
    try:
        resp = urllib.request.urlopen(req, context=context)
        return resp.status, resp.headers, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.headers, e.read()


def _walk(nest) -> list[tuple[str, str, int, object, bytes]]:
    """Every door this nest offers a browser: (host, path, status, headers, body)."""
    site = f"{HANDLE}.{NEST_DOMAIN}"
    doors = [
        # The nest's own pages and anonymous routes, at its apex and as the site.
        (NEST_DOMAIN, "/"),
        (NEST_DOMAIN, "/app/"),
        (NEST_DOMAIN, "/api/v1/health"),
        (NEST_DOMAIN, "/.well-known/nodeinfo"),
        (NEST_DOMAIN, "/.well-known/webfinger?resource=acct:wenche@sub.example"),
        (NEST_DOMAIN, "/no-such-page"),
        (f"app.{NEST_DOMAIN}", "/"),
        (f"mail.{NEST_DOMAIN}", "/"),
        # The member's site: its front page, post, feed, files, and a miss.
        (site, "/"),
        (site, "/post/headers-post"),
        (site, "/feed.xml"),
        (site, f"/{PAGE_PATH}"),
        (site, f"/{WORKER_PATH}"),
        (site, "/no-such-page.html"),
    ]
    out = []
    for host, path in doors:
        status, headers, body = _get(nest["url"], path, host)
        out.append((host, path, status, headers, body))
    return out


@pytest.mark.feature("nest-serves-the-app")
def test_no_door_of_the_nest_ever_sets_a_cookie(served_headers_site_nest):
    answers = _walk(served_headers_site_nest)

    served = {(h, p): (s, b) for h, p, s, _, b in answers}
    site = f"{HANDLE}.{NEST_DOMAIN}"
    status, body = served[(site, f"/{PAGE_PATH}")]
    assert status == 200 and b"served-headers-marker" in body, (
        f"the member's page must be served, or the walk proves nothing: {status} {body[:200]!r}"
    )

    cookies = [
        f"{h}{p} ({s}): {hd.get_all('Set-Cookie')}"
        for h, p, s, hd, _ in answers
        if hd.get_all("Set-Cookie")
    ]
    assert not cookies, (
        "the nest handed a browser a cookie — auth must never be ambient while "
        "members' sites share the nest's site (web-content-hosting.md invariant 2):\n"
        + "\n".join(cookies)
    )


@pytest.mark.feature("personal-website")
def test_a_members_script_is_never_given_a_wider_scope(served_headers_site_nest):
    nest = served_headers_site_nest
    site = f"{HANDLE}.{NEST_DOMAIN}"

    # Exactly the request a browser makes to register a service worker.
    status, headers, body = _get(
        nest["url"], f"/{WORKER_PATH}", site, {"Service-Worker": "script"}
    )
    assert status == 200 and body == WORKER_BODY, (
        f"the member's worker script must be served, or its headers prove nothing: "
        f"{status} {body[:200]!r}"
    )
    assert headers.get("Service-Worker-Allowed") is None, (
        f"a member's worker script was allowed a scope beyond /blog/: "
        f"Service-Worker-Allowed={headers.get('Service-Worker-Allowed')!r}"
    )

    widened = [
        f"{h}{p} ({s}): {hd.get('Service-Worker-Allowed')!r}"
        for h, p, s, hd, _ in _walk(nest)
        if hd.get("Service-Worker-Allowed") is not None
    ]
    assert not widened, (
        "an answer carried Service-Worker-Allowed (web-content-hosting.md "
        "invariant 4):\n" + "\n".join(widened)
    )
