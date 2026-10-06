"""What a nest will and will not answer with, once a user has a site —
the host- and path-routing half of ``web-content-hosting.md``, on a real
``fauna-nest`` binary with a real handle domain.

Three promises, one nest:

* **Publishing is enough to have a site** (§ Routing, render, serving → *Render
  pipeline*: "If a user has posts but no templates, built-in minimal defaults
  render"). No ``.html.hbs``, no ``_site.json``, no folder at all — publish, and
  the front page lists the posts and ``feed.xml`` carries them.
* **A custom domain serves, and stops the moment it is withdrawn**
  (§ Registration and DNS verification, § Per-domain TLS → *Removal*).
* **Reserved hosts and paths are never user content** (§ Same-origin security
  model, *Invariants this model relies on* no. 3: "``/api/v1/*``, ``/app``,
  ``/.well-known/*`` and the hosts ``mail.<domain>`` / ``mta-sts.<domain>`` /
  ``app.<domain>`` are never served as user content").

**Where each test stops, stated rather than implied.**

* The custom-domain test drives the **routing** half end to end and neither the
  DNS nor the ACME half. The only writer of an ``active`` row is
  ``verify_pending_domains_once`` behind the shared public-recursive
  ``DnsVerifier``, which a loopback nest cannot satisfy honestly; that leg is
  pinned in Rust with a ``MockResolver``
  (``web_content::domain::tests::verify_pending_domains_once_advances_on_matching_txt``),
  and the ``test-hooks`` seam ``/api/v1/test/web/activate-domain`` stands in for
  it by calling the same two writers and the same routing reconcile the
  production pass calls — never by inserting a row. The **HTTPS** of "serves
  your site over HTTPS" is the per-domain SNI cert population
  (``acme::MultiDomainCertResolver``), pinned in Rust for the same reason the
  subdomain serve proof runs over plain loopback HTTP
  (``test_web_subdomain_hosting::test_subdomain_serves_via_host_header``).
* The reserved-**paths** half is witnessed here on ``/api/v1/*`` and
  ``/.well-known/*``, which the nest registers unconditionally. ``/app`` is
  mounted unconditionally too, but a tier_3 nest ships no SPA
  (``node.static_dir`` unset), so its bundled ``/app`` answers a bare 404 —
  witnessed with the web-app origin choice in ``test_web_app_origin.py``. What
  is witnessed here is the app's own **host** ``app.<domain>``, which the
  resolver refuses unconditionally and which is what invariant 1 means by "the
  trusted SPA lives on its own origin". The ``/app`` **path** with a shipped
  SPA is witnessed one artifact along, where the deployment writes
  ``static_dir`` and the mount really does outrank the user-content fallback:
  ``tests/platform/docker/test_spa_shadow_by_user_site.py::test_a_user_site_never_shadows_the_mounted_spa_at_app``
  (tier_4).
"""

import urllib.error
import urllib.request

import pytest

import fauna_ffi

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import open_registration, register_handled_actor

from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_web_folder_audience import DEVICE_ID, _seed_public_file
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

#: A real handle domain, so the subdomain resolver has a `.<domain>` suffix to
#: strip and the reserved-host guard has an apex to key off. Deliberately not
#: `example.com` / `example.com`: the publish scrub rewrites the former to the
#: latter, and a nest apex on the scrub target would collapse against the custom
#: domain below in the public tree only (`web_content::domain::tests`' ⚠ note).
NEST_DOMAIN = "sub.example"
INFO_PAGE_MARKER = "Fauna Nest API"
CUSTOM_DOMAIN = "carol-writes.example.net"


@pytest.fixture(scope="module")
def routing_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest with a handle domain and open self-service registration.

    Same shape and same reason as ``test_web_subdomain_hosting``'s
    ``subdomain_nest``: a *handled* user can be provisioned over the wire and
    resolved as ``<handle>.<domain>``, and `claim_domain` is a wire act every
    nest mode honours. Dedicated because the session-shared nest has no domain —
    and claiming one on it would change every sibling test's nest.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request,
        nest_mode,
        tmp_path_factory,
        "nest-web-routing",
        claim_domain=NEST_DOMAIN,
    )
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


def _get(url: str, path: str, host: str) -> tuple[int, str]:
    """GET `path` with an explicit `Host`, returning (status, body_text).

    Host-header routing is the whole subject of this file, so every request here
    names the host it is asking as — a localhost-bound tier_3 nest is how
    `https://<host>/<path>` is spelled.
    """
    req = urllib.request.Request(
        url.rstrip("/") + path, method="GET", headers={"Host": host}
    )
    try:
        resp = urllib.request.urlopen(req)
        return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")


def _publish(nest, actor, title: str, slug: str, *, offset_us: int = 0) -> str:
    """Create a post and publish it to the web. Apostrophe-free titles — the
    default templates HTML-escape them, defeating a literal assertion."""
    post_bytes = sign_and_encode_post(
        actor["signing_key"], 1_760_000_000_000_000 + offset_us, title
    )
    post_id = ws_api.create_post(nest["port"], actor, post_bytes)
    ws_api.web_publish_set(nest["port"], actor, post_id, slug)
    return post_id


@pytest.mark.feature("personal-website")
def test_publishing_alone_gives_a_front_page_and_a_feed(routing_nest):
    """An author who has written nothing but posts gets a site: a front page
    that lists them and a feed that carries them, with no template written.

    The "no template written" half is asserted, not assumed — the author holds no
    folders at all, so there is no `.html.hbs` and no `_site.json` anywhere for
    the render to have used. What answers is `render_default_index` /
    `generate_rss` (`web_content::service` steps 6–7).
    """
    nest = routing_nest
    handle = "annike"
    actor = register_handled_actor(nest["port"], handle=handle, domain=NEST_DOMAIN)
    fqdn = f"{handle}.{NEST_DOMAIN}"

    _publish(nest, actor, "First Light", "first-light")
    _publish(nest, actor, "Second Wind", "second-wind", offset_us=1_000_000)

    with _actor_client(nest["url"], actor) as ws:
        # Nothing but posts: no folder means no template and no site metadata.
        assert ws.call("fauna.folders.list", {})["folders"] == [], (
            "this author must hold no folders — a template would make the "
            "defaults untestable"
        )
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    # ── The front page: a real index listing both posts, with links to them. ──
    status, body = _get(nest["url"], "/", fqdn)
    assert status == 200, body
    assert INFO_PAGE_MARKER not in body, f"the site must shadow the info page: {body}"
    for title, slug in (("First Light", "first-light"), ("Second Wind", "second-wind")):
        assert title in body, f"the default front page must list {title!r}: {body}"
        assert f"/post/{slug}" in body, (
            f"the default front page must LINK {slug!r}, not merely name it: {body}"
        )

    # Those links are not decoration — they resolve.
    status, body = _get(nest["url"], "/post/first-light", fqdn)
    assert status == 200 and "First Light" in body, (
        f"a link the default front page prints must load: {status} {body}"
    )

    # ── The feed: subscribable, and carrying the same posts. ──
    status, feed = _get(nest["url"], "/feed.xml", fqdn)
    assert status == 200, feed
    assert "<rss" in feed and "<channel>" in feed, (
        f"feed.xml must be a feed a reader can subscribe to: {feed}"
    )
    for title in ("First Light", "Second Wind"):
        assert title in feed, f"the auto-generated feed must carry {title!r}: {feed}"


@pytest.mark.feature("personal-website")
def test_no_user_site_answers_at_the_reserved_hosts_and_paths(routing_nest):
    """A published site with files at the nest's own addresses still never
    answers there — not at ``/api/v1/*``, not at ``/.well-known/*``, and not on
    the ``mail.`` / ``mta-sts.`` / ``app.`` hosts.

    The site really does hold those files. A control file at an ordinary path
    proves the serve walk is live and would have served them, which is what makes
    the refusals below mean anything: without it the test would pass just as well
    against a site that serves nothing at all.
    """
    nest = routing_nest
    url, port = nest["url"], nest["port"]
    handle = "bodil"
    actor = register_handled_actor(port, handle=handle, domain=NEST_DOMAIN)
    fqdn = f"{handle}.{NEST_DOMAIN}"

    marker = "reserved-address-shadow-marker"
    body_bytes = f"<h1>Mine</h1>\n<p>{marker}</p>\n".encode()
    site = "bodil-site"
    control_path = "probe/marker.html"
    reserved_paths = ("api/v1/health", ".well-known/mta-sts.txt")

    _publish(nest, actor, "Bodil Writes", "bodil-writes")
    with _actor_client(url, actor) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(actor["signing_key"]),
            {"name": site, "audience": "public"},
        )
        ws.call("fauna.folders.update", {"name": site, "website_enabled": True})
        ws.call(
            "fauna.sync.register",
            # `_seed_public_file` records as this device; any other id is
            # refused with `fauna.sync.device_unregistered`.
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    for path in (control_path, *reserved_paths):
        _seed_public_file(url, port, actor, site, path, body_bytes)

    # ── The control: this site does serve its own files, at its own host. ──
    status, body = _get(url, f"/{control_path}", fqdn)
    assert status == 200 and marker in body, (
        f"the site must serve an ordinary user file, or the refusals below "
        f"prove nothing: {status} {body}"
    )

    # ── Reserved PATHS: the nest's own routes answer, the user's file never. ──
    status, body = _get(url, "/api/v1/health", fqdn)
    assert marker not in body, (
        f"a user file shadowed the nest's own API route: {status} {body}"
    )
    assert status == 200 and '"status"' in body, (
        f"/api/v1/health must still be the nest's health route: {status} {body}"
    )

    status, body = _get(url, "/.well-known/mta-sts.txt", fqdn)
    assert marker not in body, (
        f"a user file answered at /.well-known/mta-sts.txt: {status} {body}"
    )

    # ── Reserved HOSTS: never user content, whoever asks. ──
    for reserved in (
        f"mail.{NEST_DOMAIN}",
        f"mta-sts.{NEST_DOMAIN}",
        f"app.{NEST_DOMAIN}",
        f"_acme-challenge.{NEST_DOMAIN}",
    ):
        status, body = _get(url, "/", reserved)
        assert marker not in body and "Bodil Writes" not in body, (
            f"{reserved} served user content: {status} {body}"
        )
        assert INFO_PAGE_MARKER in body, (
            f"{reserved} must fall through to the nest's own info page, "
            f"never to a resolved site: {status} {body}"
        )


@pytest.mark.feature("personal-website")
def test_a_verified_custom_domain_serves_the_site_and_stops_when_withdrawn(routing_nest):
    """A domain of the author's own serves their site once verified, and stops
    serving it the moment they withdraw it.

    Three states, in the order a user passes through them:

    1. **pending** — registered, DNS not yet published. `web_domains` holds the
       row; routing projects `active` rows ONLY, so the domain answers with the
       nest's info page, never the site.
    2. **active** — DNS verification done. The routing reconcile projects the
       `active` set onto the live `HostResolver` **with no restart**, and the
       domain serves the owner's site.
    3. **withdrawn** — `fauna.web.domain.delete` drops the name from the
       resolver at once rather than at the next 5-minute pass, because routing
       that outlives its row means serving content the owner just withdrew
       (§ Per-domain TLS → *Removal*).

    Step 2's DNS leg is the `test-hooks` seam; see the module docstring for
    exactly what that stands in for and what it does not.
    """
    from conftest import _bridge_admin_post

    nest = routing_nest
    url, port = nest["url"], nest["port"]
    actor = register_handled_actor(port, handle="carol", domain=NEST_DOMAIN)

    _publish(nest, actor, "Carol On Her Own Domain", "own-domain")

    # ── 1. Registered, pending: a row exists and routes nowhere. ──
    with _actor_client(url, actor) as ws:
        reply = ws.call("fauna.web.domain.set", {"domain": CUSTOM_DOMAIN})
        assert reply["status"] == "pending", reply
        assert reply["verify_token"], reply
        assert CUSTOM_DOMAIN in reply["txt_record"], reply

        rows = {r["domain"]: r for r in ws.call("fauna.web.domain.get", {})["domains"]}
        assert rows[CUSTOM_DOMAIN]["status"] == "pending", rows

    status, body = _get(url, "/", CUSTOM_DOMAIN)
    assert "Carol On Her Own Domain" not in body, (
        f"a domain that has not finished verification must not serve: {status} {body}"
    )
    assert INFO_PAGE_MARKER in body, f"expected the info page while pending: {body}"

    # ── 2. Verified → active: the same two writers and the same routing
    # reconcile the lifecycle task runs, with no restart in between. ──
    activated = _bridge_admin_post(
        url,
        nest["admin"]["token"],
        "/api/v1/test/web/activate-domain",
        {"domain": CUSTOM_DOMAIN},
    )
    assert activated.get("ok"), f"the nest refused to activate the domain: {activated}"

    with _actor_client(url, actor) as ws:
        rows = {r["domain"]: r for r in ws.call("fauna.web.domain.get", {})["domains"]}
        assert rows[CUSTOM_DOMAIN]["status"] == "active", rows
        assert rows[CUSTOM_DOMAIN]["verified_at"] is not None, rows

    status, body = _get(url, "/", CUSTOM_DOMAIN)
    assert status == 200, body
    assert "Carol On Her Own Domain" in body, (
        f"an active custom domain must serve its owner's site: {status} {body}"
    )
    assert INFO_PAGE_MARKER not in body, f"the info page must be shadowed: {body}"

    # ── 3. Withdrawn: the very next request stops getting the site. ──
    with _actor_client(url, actor) as ws:
        deleted = ws.call("fauna.web.domain.delete", {"domain": CUSTOM_DOMAIN})
        assert deleted["ok"] is True, deleted
        assert ws.call("fauna.web.domain.get", {})["domains"] == [], (
            "a withdrawn domain must leave no row behind"
        )

    status, body = _get(url, "/", CUSTOM_DOMAIN)
    assert "Carol On Her Own Domain" not in body, (
        "a withdrawn domain kept serving the owner's site — the drop must be "
        f"immediate, not at the next 5-minute pass: {status} {body}"
    )
    assert INFO_PAGE_MARKER in body, f"expected the info page after withdrawal: {body}"
