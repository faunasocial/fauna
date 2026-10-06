"""tier_4 e2e: a user's own site can never shadow the SPA at ``/app``.

**The arm no tier_3 nest can witness.** ``web-content-hosting.md`` § Same-origin
security model, *Invariants this model relies on* no. 3, reserves three PATHS
from user content: ``/api/v1/*``, ``/app`` and ``/.well-known/*``. The nest
registers two of them unconditionally, so a locally-built binary can witness
those, and
``tests/api/test_web_host_routing.py::test_no_user_site_answers_at_the_reserved_hosts_and_paths``
does — it seeds a published site with files AT those paths and asserts the nest's
own routes answer instead.

``/app`` is different: it is a **deployment-configured** mount. ``build_router``
calls ``mount_spa`` only when ``node.static_dir`` is set
(``bins/fauna-nest/src/lib.rs``), and a tier_3 nest's ``config/default.toml``
sets none — so on the harness's own nest there is no ``/app`` route at all, the
request falls through to ``.fallback(web_content_or_info)``, and
``web_content::serve::resolve_path`` (which reserves no path, and resolves
``/app``, ``/app/`` and ``/app/index.html`` alike to the stored
``app/index.html``) serves the user's bytes. On a tier_3 nest this invariant is
not merely untested but *unobservable*.

That is not a production defect. A shipped deployment writes ``static_dir``
itself (``docker/nest-toml-overlay.sh``, reconciled on every boot by
``docker/entrypoint.sh``) and the mount then sits above the fallback exactly as
``/api/v1/*`` does. But *"the deployment artifact configures the mount, and the
mount outranks the user-content fallback"* is a property of the ARTIFACT — the
same mechanism-tested / wiring-untested split ``test_spa_serving.py`` documents
at length for the eleven-day 2026-07-13 regression, one invariant along.

**Why the control file is load-bearing.** Read positively, this test would pass
just as well against a site that serves nothing at all: ``/app/`` would answer
with the SPA and the user's marker would be absent for entirely the wrong
reason. So the same site is seeded at an ordinary path too, and that path is
asserted to serve — the refusals mean something only because the serve walk is
live and would otherwise have served them. Same reasoning, same shape, as the
tier_3 sibling's control.

**Red-verified, and every address of the four is a real shadow.** Measured
2026-09-22 against ``ghcr.io/faunasocial/nest:latest`` by seeding exactly what
this test seeds and then deleting ``static_dir`` from the container's
``nest.toml`` and restarting the nest service — i.e. reproducing a tier_3
nest's world inside the image. All four ``/app`` addresses flipped from
*SPA, no user marker* to **the user's own bytes**, ``/app/steal.html`` included,
so neither seed and no address here is decoration. That also re-measures what
this test is for: the difference between green and broken is one line of
artifact wiring, and nothing in the nest itself.

**Not ``self_contained_docker``.** The seed rides the production ingest rail
(chunk POST → manifest POST → ``fauna.sync.changes.record``), whose chunk and
manifest bytes come from the cargo-built ``fauna_ffi`` cdylib
(``seal_folder_file``) — so this module needs more than the image, which is
exactly what that marker promises its CI runner it will not.

Run against a **published** image rather than a local build (local nest-image
builds are forbidden on dev VMs — ``helpers.docker_build``)::

    docker pull ghcr.io/faunasocial/nest:latest
    docker tag ghcr.io/faunasocial/nest:latest fauna-nest-test:local
    pytest tests/e2e-unified/tests/platform/docker/test_spa_shadow_by_user_site.py -v
"""

import subprocess

import pytest

import fauna_ffi

from common.auth import (
    mark_tls_nest,
    open_registration,
    register_handled_actor,
    unmark_tls_nest,
)

from tests.api.test_web_folder_audience import DEVICE_ID, _seed_public_file
from tests.api.test_web_host_routing import _get
from tests.api.test_web_paywall_folder import _actor_client

from .helpers import (
    claim_admin_api,
    docker_build,
    find_free_port,
    get_repo_root,
    remove_container,
    start_container,
    wait_for_health,
)
from .test_spa_serving import INFO_PAGE_MARKER, SPA_MARKER

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# Deliberately WITHOUT `self_contained_docker` (see the module docstring): the
# seed needs the cargo-built `fauna_ffi` cdylib, which that marker's whole
# promise is that a toolchain-light docker-only runner will never need.
pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

#: The domain the admin claims WITH, so the box acquires a handle domain and the
#: subdomain resolver has a `.<domain>` suffix to strip. Non-local, because the
#: claim-time domain registration is gated to `is_public_dns_name` targets; and
#: deliberately neither `example.com` nor `example.com` — the publish scrub
#: rewrites the former to the latter, so a nest apex on the scrub target reads
#: differently in the public tree than it does here.
DOMAIN = "spa-shadow.example"
CLAIM_CODE = "SPASHDW1"
HANDLE = "bodil"
SITE = "bodil-site"

MARKER = "user-site-app-path-shadow-marker"
BODY = f"<h1>Mine</h1>\n<p>{MARKER}</p>\n".encode()

#: An ordinary path, to prove the site serves at all (see the docstring).
CONTROL_PATH = "probe/marker.html"

#: Two distinct shadows, because `mount_spa` answers them by two different
#: mechanisms. `app/index.html` is the one the SPA's OWN `index.html` would lose
#: to if the mount sat below the fallback; `app/steal.html` is the arm where the
#: SPA holds no such file at all, so what must answer is `ServeDir`'s
#: `.fallback(ServeFile::new(<static_dir>/index.html))`. Seeding only the first
#: would leave the second — the larger surface, since it is every path under
#: `/app/` — unwitnessed.
SHADOW_PATHS = ("app/index.html", "app/steal.html")

#: Every address the reserved `/app` path is spelled at. `resolve_path` folds all
#: four onto the same stored `app/index.html` (exact match, then the
#: directory-like `index.html` probe), so a user site would answer at every one
#: of them on a nest with no mount.
APP_ADDRESSES = ("/app", "/app/", "/app/index.html", "/app/steal.html")


@pytest.fixture(scope="module")
def docker_image():
    return docker_build(get_repo_root())


@pytest.fixture(scope="module")
def site_nest(docker_image):
    """A claimed container with a handle domain and open self-service
    registration, so a *handled* user can be provisioned over the wire and
    resolved as ``<handle>.<domain>`` — the tier_4 twin of
    ``test_web_host_routing``'s ``routing_nest``.

    The domain arrives through the **claim** rather than a ``FAUNA_DOMAIN`` boot
    env: a domained claim IS how a domainless box acquires its identity
    (``helpers.claim_admin_api``), and ``apply_primary_identity`` then swaps the
    live ``HostResolver`` apex with no restart, which is what makes the subdomain
    below resolvable.

    ``mark_tls_nest`` is not decoration: the image's listener is HTTPS-only, and
    the chunk/manifest rail the seed rides (``_post_bytes``) reads its scheme
    from ``common.auth.port_base_url`` rather than being handed one.
    """
    port = find_free_port()
    name = f"fauna-nest-spa-shadow-{port}"
    start_container(name, port, env={"FAUNA_CLAIM_CODE": CLAIM_CODE})
    try:
        wait_for_health(port, name)
        mark_tls_nest(port)
        admin = claim_admin_api(port, CLAIM_CODE, handle="admin", mail_domain=DOMAIN)
        assert admin["domain"] == DOMAIN, (
            f"the domained claim must leave {DOMAIN!r} as the handle domain, or "
            f"the handled registration below is a `fauna.auth.signature_failed` "
            f"reject (the register signature is over the nest's OWN resolved "
            f"domain): got {admin['domain']!r}"
        )
        nest = {
            "name": name,
            "port": port,
            "url": f"https://127.0.0.1:{port}",
            "admin": admin,
        }
        open_registration(nest)
        yield nest
    finally:
        # A port outlives its nest (`find_free_port` hands it out again), so the
        # https posture must not.
        unmark_tls_nest(port)
        remove_container(name)


@pytest.mark.feature("personal-website")
def test_a_user_site_never_shadows_the_mounted_spa_at_app(site_nest):
    """A published site holding files at ``app/…`` still never answers there on
    an image whose SPA is mounted — at any of the four addresses the reserved
    path is spelled.

    The site really does hold those files, and really does serve its own bytes
    at an ordinary path; both halves are asserted, because without the second
    the refusals below would prove nothing.
    """
    nest = site_nest
    url, port = nest["url"], nest["port"]
    actor = register_handled_actor(port, handle=HANDLE, domain=DOMAIN, base_url=url)
    fqdn = f"{HANDLE}.{DOMAIN}"

    # ── A born-public website folder with a write-capable seed device, and the
    # author opted into their own subdomain. ──
    with _actor_client(url, actor) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(actor["signing_key"]),
            {"name": SITE,
             "audience": "public"},
        )
        ws.call("fauna.folders.update", {"name": SITE, "website_enabled": True})
        ws.call(
            "fauna.sync.register",
            # `_seed_public_file` records as this device; any other id is
            # refused with `fauna.sync.device_unregistered`.
            {"device_id": DEVICE_ID.hex(), "label": "seed",
             "capabilities": "read,write"},
        )
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})

    for path in (CONTROL_PATH, *SHADOW_PATHS):
        _seed_public_file(url, port, actor, SITE, path, BODY)

    # ── The control: this site does serve its own files, at its own host. ──
    status, body = _get(url, f"/{CONTROL_PATH}", fqdn)
    assert status == 200 and MARKER in body, (
        f"the site must serve an ordinary user file, or the refusals below prove "
        f"nothing — a site serving nothing at all would pass them: {status} {body}"
    )

    # ── The reserved path: the SPA answers, the user's bytes never do. ──
    for address in APP_ADDRESSES:
        status, body = _get(url, address, fqdn)
        assert MARKER not in body, (
            f"a user site's file shadowed the SPA at {address} asked as {fqdn} — "
            f"invariant 3 of web-content-hosting.md § Same-origin security model "
            f"is broken on the real image: the /app mount is sitting BELOW the "
            f"web-content fallback, or is absent (static_dir unwritten) and the "
            f"fallback answered. {status} {body[:600]}"
        )
        assert status == 200 and SPA_MARKER in body, (
            f"{address} must serve the bundled SPA, not merely refuse the user "
            f"file: {status} {body[:600]}"
        )
        assert INFO_PAGE_MARKER not in body, (
            f"{address} served the nest INFO PAGE — the 2026-07-13 static_dir "
            f"regression (test_spa_serving.py). The info page is also 200 + "
            f"text/html, which is why the marker assert above is not enough: "
            f"{status} {body[:600]}"
        )
