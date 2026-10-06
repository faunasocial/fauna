"""HTTP fake for the wizard's VPS / DNS / nest backends.

Wraps pytest-httpserver to provide a single port hosting three route
prefixes (/hetzner-cloud, /cloudflare, /nest). The wizard's
provider_base_urls override redirects HTTP calls to this fake so e2e
tests can drive the orchestrator end-to-end without real cloud APIs.

**CORS.** On web the wizard runs in WASM and its provider HTTP calls go
through the browser's `fetch`, so they are subject to CORS. In production
CORS-restricted providers are routed via `proxy.fauna.social`
(`libs/fauna-provisioning/src/proxy.rs`), which injects the headers; the
`provider_base_urls` override bypasses that proxy and points straight at
this fake, so the fake itself must emit `Access-Control-Allow-*` and
answer the preflight `OPTIONS` (cloudflare/hetzner send an `Authorization`
header → non-simple request → preflight). Native drivers (reqwest, no
browser) ignore these headers.

The fake serves only the endpoints the **provisioning orchestrator** hits
over HTTP — the cloud-provider APIs and the nest `/api/v1/health` poll.
The onboarding invite/claim/recheck paths moved to WS-RPC (the anonymous
`GET /api/v1/ws` connection, `invite_handlers.rs`), so their old HTTP
twins were retired and are NOT mocked here.

Design tracked internally.
"""
from __future__ import annotations

import json
import re

import pytest
from pytest_httpserver import HTTPServer
from werkzeug.wrappers import Response as _Resp

# This module is imported both as the `fakes.fake_cloud` package member (the
# tests' conftest) and as a top-level `fake_cloud` (fakes/conftest.py), so the
# sibling import has to work under either spelling.
try:
    from .fake_bundled import BundledHandlers
except ImportError:  # top-level import from within fakes/
    from fake_bundled import BundledHandlers

# Permissive CORS so the browser allows the WASM wizard's cross-origin
# fetch at the fake (it lives on a different port than the SPA origin).
# `Allow-Headers` lists each provider auth header explicitly — the `*`
# wildcard does not cover them per the Fetch spec, and a header the
# preflight response omits makes the browser block the *real* request
# (only the OPTIONS preflight reaches the fake). Every live provider now
# authenticates with `Authorization: Bearer …` (Cloudflare, Hetzner — both
# VPS *and* DNS since the 2026-06-05 move of Hetzner DNS onto the Hetzner
# Cloud API; the old `Auth-API-Token` DNS header is retired).
_CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, PUT, DELETE, OPTIONS",
    "Access-Control-Allow-Headers": "Authorization, Content-Type",
}


@pytest.fixture(scope="session")
def httpserver_listen_address() -> tuple[str, int]:
    """Bind pytest-httpserver to 127.0.0.1 (not 'localhost') so the URL map
    exposes a numeric IP — matches the wizard's expectation that
    provider_base_urls override targets are loopback-numeric URLs. Port 0
    lets the OS pick a free port. Shared by both `fakes/conftest.py` (the
    fixture's own self-tests) and `tests/conftest.py` (every app-facing
    e2e test) so the bind policy is defined once."""
    return ("127.0.0.1", 0)


@pytest.fixture(scope="session")
def make_httpserver(httpserver_listen_address: tuple[str, int]):
    """Override pytest-httpserver's own session fixture to add `threaded=True`
    (e2e-conventions.md point 9 "the harness is self-terminating" / point 6
    "failures must diagnose themselves"). The default (`threaded=False`) is single-threaded, so ONE idle bare
    connection — no request ever sent on it, exactly what a browser's
    preconnect or an HTTP client's connection pool leaves behind — makes the
    server stop answering every OTHER client entirely (measured: 0/1/2/6/32
    idle squatters all `ReadTimeout` before this fix, all served in <0.01s
    after). `threaded` is `HTTPServer`'s own documented public constructor
    kwarg (pytest-httpserver >=1.0.11) — no hand-rolled server needed. Shared
    by both `fakes/conftest.py` and `tests/conftest.py` so every fixture that
    depends on `httpserver` (directly or via `fake_cloud`) gets it."""
    host, port = httpserver_listen_address
    server = HTTPServer(host=host, port=port, threaded=True)
    server.start()
    yield server
    server.clear()
    if server.is_running():
        server.stop()


@pytest.fixture
def fake_cloud(httpserver: HTTPServer) -> "FakeCloud":
    fc = FakeCloud(httpserver)
    fc.install_default_routes()
    return fc


class FakeCloud:
    """Single-port HTTP fake for VPS / DNS / nest backends."""

    def __init__(self, httpserver: HTTPServer) -> None:
        self._h = httpserver
        # Per-backend handler classes installed in install_default_routes.
        self.hetzner_cloud = HetznerCloudHandlers(httpserver, "/hetzner-cloud")
        self.cloudflare = CloudflareHandlers(httpserver, "/cloudflare")
        self.hetzner_dns = HetznerDnsHandlers(httpserver, "/hetzner-dns")
        self.nest = NestHandlers(httpserver, "/nest")
        # The bundled-provider reference server (fake_bundled.py) — reached by
        # the base URL the test TYPES into the wizard, not by an override.
        self.bundled = BundledHandlers(httpserver, "/bundled")

    def url_map(self) -> dict[str, str]:
        base = self._h.url_for("").rstrip("/")
        return {
            "vps": f"{base}/hetzner-cloud",
            "dns": f"{base}/cloudflare",
            "nest": f"{base}/nest",
        }

    # `nest_only_url_map()` — `{"nest": <this fake's /nest>}` — is DELETED, not
    # merely unused. Its one caller was the bundled-provider journey, and
    # pointing the nest leg here is precisely what broke it: since
    # `onboarding.md` § 6 *Provisioning = build + claim* a standard-path run
    # ends by claiming the box over WS-RPC, which this fake cannot serve (see
    # the module docstring). A journey that needs the nest leg overridden wants
    # a REAL nest — the `provision_target_nest` fixture — so there is nothing
    # left for a fake-nest-only map to be right for.

    def dns_base_for(self, provider_id: str) -> str:
        """Base URL for the `provider_base_urls["dns"]` override, keyed to
        the selected DNS provider's wire shape. Hetzner DNS (now the Hetzner
        Cloud API, `api.hetzner.cloud/v1`) returns a different JSON envelope
        than Cloudflare (`{"zones":[{"id":<int>,…}]}` vs
        `{"success":…,"result":[…]}`), so it has its own stub prefix. Callers
        build the override map as
        ``{**fc.url_map(), "dns": fc.dns_base_for(provider_id)}``."""
        base = self._h.url_for("").rstrip("/")
        if provider_id == "hetzner":
            return f"{base}/hetzner-dns"
        return f"{base}/cloudflare"

    def install_default_routes(self) -> None:
        """Install canned-success defaults for every endpoint the
        orchestrator hits. Tests override per-route via the per-backend
        handler classes' mutators."""
        # CORS preflight: any path, any prefix. Method-scoped so it never
        # shadows the GET/POST handlers below.
        self._h.expect_request(
            re.compile(r".*"), method="OPTIONS"
        ).respond_with_data("", status=200, headers=_CORS)
        self.hetzner_cloud.install_defaults()
        self.cloudflare.install_defaults()
        self.hetzner_dns.install_defaults()
        self.nest.install_defaults()
        self.bundled.install_defaults()


class HetznerCloudHandlers:
    """Stubs for the Hetzner cloud API endpoints the orchestrator hits.
    See libs/fauna-provisioning/src/vps/hetzner.rs for the endpoint list."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._prefix = prefix
        # Boxes `list_managed_servers` will report, and the ids `delete_server`
        # has removed — the retire view's two provider-side effects, kept as
        # state so a test asserts what the run actually did rather than that a
        # canned reply came back.
        self.managed_servers: list[dict] = []
        self.deleted_server_ids: list[str] = []
        # The Hetzner Cloud API serves DNS on the SAME base as servers
        # (`api.hetzner.cloud/v1`), and the retire machine points one base URL
        # at both — so the zone/RRset routes are mounted here too, on their own
        # state. `FakeCloud.hetzner_dns` stays the wizard's separate DNS prefix.
        self.dns = HetznerDnsHandlers(httpserver, prefix)

    def install_defaults(self) -> None:
        p = self._prefix
        # GET /locations — what the provider's verify reads. (It read
        # GET /datacenters until the real API removed that endpoint.)
        self._h.expect_request(f"{p}/locations", method="GET").respond_with_json(
            {
                "locations": [
                    {
                        "name": "fsn1",
                        "description": "Falkenstein DC Park 1",
                        "city": "Falkenstein",
                        "country": "DE",
                    }
                ]
            },
            headers=_CORS,
        )
        # POST /servers
        self._h.expect_request(f"{p}/servers", method="POST").respond_with_json(
            {
                "server": {
                    "id": 12345,
                    "public_net": {"ipv4": {"ip": "203.0.113.5", "dns_ptr": None}},
                }
            },
            status=201,
            headers=_CORS,
        )
        # GET /server_types — names MUST be in Hetzner's curated_offers
        # (providers_generated.rs: ["cx23","cx33","cx43","ccx13","ccx23"]);
        # list_server_types() drops any type not on that list, so a
        # non-curated name (e.g. "cax11") would yield zero radios.
        self._h.expect_request(f"{p}/server_types", method="GET").respond_with_json(
            {
                "server_types": [
                    {
                        "name": "cx23",
                        "cores": 2,
                        "memory": 4.0,
                        "disk": 40,
                        "prices": [{"location": "fsn1", "price_monthly": {"gross": "4.51"}}],
                    },
                    {
                        "name": "cx33",
                        "cores": 4,
                        "memory": 8.0,
                        "disk": 80,
                        "prices": [{"location": "fsn1", "price_monthly": {"gross": "8.49"}}],
                    },
                    {
                        "name": "cx43",
                        "cores": 8,
                        "memory": 16.0,
                        "disk": 160,
                        "prices": [{"location": "fsn1", "price_monthly": {"gross": "16.99"}}],
                    },
                ]
            },
            headers=_CORS,
        )
        # POST /servers/{id}/actions/change_dns_ptr  — match any numeric id
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/servers/\d+/actions/change_dns_ptr$"),
            method="POST",
        ).respond_with_json({"action": {"id": 1, "status": "success"}}, headers=_CORS)
        # GET /servers — two callers share this (uri, method), and
        # pytest-httpserver matches the FIRST registered handler, so one
        # handler branches on the query:
        #   ?name=...           find_server_by_name  → empty (nothing exists)
        #   ?label_selector=... list_managed_servers → self.managed_servers
        # The retire view drives the second (nest-retirement.md § Where logic
        # lives); the filter is honoured rather than ignored, because the
        # adapter re-checks the marker client-side and a fake that returned
        # unmarked rows would silently pass a test the real API would fail.
        self._h.expect_request(f"{p}/servers", method="GET").respond_with_handler(
            self._list_servers
        )
        # DELETE /servers/{id} — the decommission primitive. 404 is a success
        # for the adapter (idempotent), so a second delete is faithful too.
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/servers/\d+$"),
            method="DELETE",
        ).respond_with_handler(self._delete_server)
        # GET /servers/{id} — a seeded box answers as itself (its PTR is what
        # the retire view attributes a domain from); any other id gets the
        # provisioning orchestrator's canned box.
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/servers/\d+$"),
            method="GET",
        ).respond_with_handler(self._get_server)
        self.dns.install_defaults()

    def _get_server(self, request):
        sid = request.path.rsplit("/", 1)[-1]
        seeded = next(
            (s for s in self.managed_servers if str(s.get("id")) == sid), None
        )
        server = seeded or {
            "id": 12345,
            "public_net": {
                "ipv4": {"ip": "203.0.113.5", "dns_ptr": "test-fauna.example.test"}
            },
        }
        return _Resp(
            json.dumps({"server": server}),
            status=200,
            content_type="application/json",
            headers=list(_CORS.items()),
        )

    def _list_servers(self, request):
        selector = request.args.get("label_selector")
        if selector is None:
            # find_server_by_name's pre-flight: nothing exists yet.
            return _Resp(
                json.dumps({"servers": [], "meta": {"pagination": {"next_page": None}}}),
                status=200,
                content_type="application/json",
                headers=list(_CORS.items()),
            )
        key, _, value = selector.partition("=")
        out = [
            s
            for s in self.managed_servers
            if s.get("labels", {}).get(key) == value
        ]
        return _Resp(
            json.dumps({"servers": out, "meta": {"pagination": {"next_page": None}}}),
            status=200,
            content_type="application/json",
            headers=list(_CORS.items()),
        )

    def _delete_server(self, request):
        sid = request.path.rsplit("/", 1)[-1]
        before = len(self.managed_servers)
        self.managed_servers = [
            s for s in self.managed_servers if str(s.get("id")) != sid
        ]
        self.deleted_server_ids.append(sid)
        status = 204 if len(self.managed_servers) < before else 404
        return _Resp(b"", status=status, headers=list(_CORS.items()))

    def seed_managed_server(
        self,
        server_id: int = 12345,
        name: str = "example-test",
        ipv4: str = "203.0.113.5",
        labels: dict | None = None,
        ptr: str | None = "",
    ) -> dict:
        """Put one box in the account for `list_managed_servers` to find.

        Defaults carry the `managed-by=fauna` marker, since an unmarked box is
        exactly what the adapter must *not* surface — pass `labels={}` to seed
        a non-fauna server and assert it stays hidden.
        """
        server = {
            "id": server_id,
            "name": name,
            "labels": {"managed-by": "fauna"} if labels is None else labels,
            "created": "2026-01-02T03:04:05+00:00",
            # `ptr=""` keeps the historical `mail.<name>` default; pass the
            # real `mail.<domain>` (or None) to steer attribution.
            "public_net": {
                "ipv4": {"ip": ipv4, "dns_ptr": f"mail.{name}" if ptr == "" else ptr}
            },
        }
        self.managed_servers.append(server)
        return server

    def create_server_always_fails(self) -> None:
        """`POST /servers` returns 500 for the rest of the run.

        The Server step's own failure, chosen over an unreachable fake because
        it lands *after* the pending-provision slot is written and before any
        box exists — the one window in which the slot describes a machine that
        was ordered and never came (`onboarding.md` § 6 *The pending-provision
        slot*: the write precedes `create_server`). `_drop_handlers_for` first,
        because pytest-httpserver matches the FIRST registered handler.
        """
        p = self._prefix
        _drop_handlers_for(self._h, f"{p}/servers", "POST")
        self._h.expect_request(f"{p}/servers", method="POST").respond_with_json(
            {"error": {"code": "server_error", "message": "provider is down"}},
            status=500,
            headers=_CORS,
        )


class CloudflareHandlers:
    """Stubs for the Cloudflare API endpoints the orchestrator hits.
    See libs/fauna-provisioning/src/dns/cloudflare.rs."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._prefix = prefix

    def install_defaults(self) -> None:
        p = self._prefix
        # GET /zones?per_page=50  — return a single matching zone
        self._h.expect_request(f"{p}/zones", method="GET").respond_with_json(
            {
                "success": True,
                "errors": [],
                "messages": [],
                "result": [
                    {"id": "zone-abc", "name": "example.test", "status": "active"}
                ],
            },
            headers=_CORS,
        )
        # POST /zones/{id}/dns_records
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/zones/[^/]+/dns_records$"),
            method="POST",
        ).respond_with_json(
            {
                "success": True,
                "errors": [],
                "messages": [],
                "result": {"id": "rec-001"},
            },
            headers=_CORS,
        )
        # GET /zones/{id}/dns_records
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/zones/[^/]+/dns_records$"),
            method="GET",
        ).respond_with_json(
            {
                "success": True,
                "errors": [],
                "messages": [],
                "result": [],  # empty so create_record_idempotent always creates
            },
            headers=_CORS,
        )


class HetznerDnsHandlers:
    """Stubs for Hetzner DNS via the **Hetzner Cloud API** (RRset model,
    ``api.hetzner.cloud/v1``). The standalone ``dns.hetzner.com/api/v1`` went
    read-only 2026-05-20 and DNS moved into the Cloud API (Bearer auth,
    INTEGER zone ids, records grouped into RRsets keyed by ``(name, type)``). Its JSON envelope still differs from Cloudflare,
    so it keeps a distinct stub prefix — see
    libs/fauna-provisioning/src/dns/hetzner.rs. Used by the
    same-provider-for-VPS preselect flow, where the DNS provider is Hetzner
    (the only provider with both DNS + VPS capability).

    **The retire view's DNS half** (nest-retirement.md § DNS cleanup) needs
    records to exist: ``seed_record`` puts a value in an RRset, the RRset read
    answers from that store, and ``remove_records`` takes the value out of it —
    so a re-run finds nothing left to remove, exactly as the real zone would.
    The store starts empty, which keeps every provisioning path on its
    create-when-absent branch."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._prefix = prefix
        # (name, type, value) per `remove_records` / `add_records` call, in
        # order — the witness for value-scoped deletion.
        self.removed_records: list[tuple[str, str, str]] = []
        self.added_records: list[tuple[str, str, str]] = []
        # The zones `verify()` reports. Cloud-API zone ids are INTEGERS.
        self.zones: list[dict] = [{"id": 42, "name": "example.test"}]
        # Per-token zone lists, when a test needs two credentials to see
        # different zones (the retire view's held-credential arm: the entered
        # token holds some unrelated zone, the held one the box's). A bearer
        # absent from the map sees `zones`.
        self.zones_by_token: dict[str, list[dict]] = {}
        # The bearer token behind every `remove_records` call, in order — the
        # witness for WHICH credential cleaned the zone.
        self.removal_tokens: list[str] = []
        # (zone id, relative name, type) → the RRset's record values.
        self.rrsets: dict[tuple[str, str, str], list[str]] = {}
        # How many further `remove_records` calls answer 500 — the retire
        # run's failed-DNS branch. `-1` fails every one.
        self.fail_removals = 0

    def seed_record(self, name: str, record_type: str, value: str, zone_id: int = 42) -> None:
        """Put one record value in the zone, under its zone-relative ``name``
        (``@`` at the apex) — what the RRset read then reports."""
        self.rrsets.setdefault((str(zone_id), name, record_type), []).append(value)

    def records(self, zone_id: int = 42) -> list[tuple[str, str, str]]:
        """Every ``(name, type, value)`` still in the zone."""
        return [
            (name, rtype, value)
            for (zid, name, rtype), values in self.rrsets.items()
            if zid == str(zone_id)
            for value in values
        ]

    @staticmethod
    def _bearer(request) -> str:
        return request.headers.get("Authorization", "").removeprefix("Bearer ").strip()

    def _zones(self, request):
        zones = self.zones_by_token.get(self._bearer(request), self.zones)
        return _Resp(
            json.dumps({"zones": zones}),
            status=200,
            content_type="application/json",
            headers=list(_CORS.items()),
        )

    def _rrset_read(self, request):
        # .../zones/{zone}/rrsets?name=…&type=…
        zone = request.path.split("/")[-2]
        want_name = request.args.get("name")
        want_type = request.args.get("type")
        rrsets = [
            {
                "name": name,
                "type": rtype,
                "ttl": 300,
                "records": [{"value": v} for v in values],
            }
            for (zid, name, rtype), values in self.rrsets.items()
            if zid == zone
            and values
            and (want_name is None or name == want_name)
            and (want_type is None or rtype == want_type)
        ]
        return _Resp(
            json.dumps({"rrsets": rrsets}),
            status=200,
            content_type="application/json",
            headers=list(_CORS.items()),
        )

    def _rrset_action(self, request):
        # .../zones/{zone}/rrsets/{name}/{type}/actions/{add|remove}_records
        parts = request.path.split("/")
        action = parts[-1]
        record_type = parts[-3]
        name = parts[-4]
        zone = parts[-6]
        if action == "remove_records":
            self.removal_tokens.append(self._bearer(request))
        if action == "remove_records" and self.fail_removals != 0:
            if self.fail_removals > 0:
                self.fail_removals -= 1
            return _Resp(
                json.dumps({"error": {"code": "server_error", "message": "dns is down"}}),
                status=500,
                content_type="application/json",
                headers=list(_CORS.items()),
            )
        body = request.get_json(force=True, silent=True) or {}
        sink = self.removed_records if action == "remove_records" else self.added_records
        for rec in body.get("records", []):
            value = rec.get("value", "")
            sink.append((name, record_type, value))
            if action == "remove_records":
                held = self.rrsets.get((zone, name, record_type), [])
                if value in held:
                    held.remove(value)
        return _Resp(
            json.dumps({"action": {"id": 1, "status": "success"}}),
            status=200,
            content_type="application/json",
            headers=list(_CORS.items()),
        )

    def install_defaults(self) -> None:
        p = self._prefix
        # GET /zones?per_page=50 — verify(). Cloud-API zone ids are INTEGERS
        # (mapped to strings client-side via `z.id.to_string()`); a string id
        # fails the `HetznerZone { id: i64 }` decode → verify() Err →
        # ProviderUnauthorized ("provider hetzner rejected the credentials").
        # The preselect flow exercises only this endpoint.
        self._h.expect_request(f"{p}/zones", method="GET").respond_with_handler(
            self._zones
        )
        # GET /zones/{zone}/rrsets — find_records() and create_record()'s
        # existence probe, answered from the seeded store (empty by default,
        # so provisioning's create always creates).
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/zones/[^/]+/rrsets$"),
            method="GET",
        ).respond_with_handler(self._rrset_read)
        # POST /zones/{zone}/rrsets — create_record() when the RRset is absent.
        self._h.expect_request(
            re.compile(rf"^{re.escape(p)}/zones/[^/]+/rrsets$"),
            method="POST",
        ).respond_with_json(
            {"rrset": {"id": "rec-htz"}}, status=201, headers=_CORS
        )
        # POST /zones/{zone}/rrsets/{name}/{type}/actions/{add,remove}_records
        # — create_record()'s add-to-existing-RRset path and delete_record().
        # Value-aware: every removed value is recorded, so a test can assert
        # the retire run deleted only records pointing at the dying box
        # (nest-retirement.md § DNS cleanup: value-scoped, never a name
        # sweep). A stub that answered success without reading the body made
        # a sweep and a scoped delete look identical.
        self._h.expect_request(
            re.compile(
                rf"^{re.escape(p)}/zones/[^/]+/rrsets/[^/]+/[^/]+/actions/"
                r"(add|remove)_records$"
            ),
            method="POST",
        ).respond_with_handler(self._rrset_action)


class NestHandlers:
    """Stubs for the nest endpoints the orchestrator hits over HTTP.

    Only `/api/v1/health` (the provisioning Online step's poll) is a live
    HTTP path. The invite-request / claim / recheck endpoints moved to
    WS-RPC, so their HTTP twins are retired and not mocked here."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._prefix = prefix

    def install_defaults(self) -> None:
        p = self._prefix
        # Wrapper exposes the health mutators on the parent FakeCloud.nest.health surface.
        self._health_handler = HealthHandler(self._h, p)
        self._health_handler.always_succeed()

    @property
    def health(self) -> "HealthHandler":
        return self._health_handler


def _drop_handlers_for(httpserver: HTTPServer, uri, method: str) -> None:
    """Remove any previously-registered permanent or oneshot handlers that
    match (uri, method). pytest-httpserver's RequestHandlerList.match
    returns the FIRST match, so a re-registration alone won't override
    a default — the prior entry must be removed first."""
    def matches(handler) -> bool:
        m = handler.matcher
        if m.method != method:
            return False
        if isinstance(uri, re.Pattern):
            return isinstance(m.uri, re.Pattern) and m.uri.pattern == uri.pattern
        return m.uri == uri

    httpserver.handlers[:] = [h for h in httpserver.handlers if not matches(h)]
    httpserver.oneshot_handlers[:] = [
        h for h in httpserver.oneshot_handlers if not matches(h)
    ]


class HealthHandler:
    """Mutator-rich wrapper around the nest /api/v1/health endpoint."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._path = f"{prefix}/api/v1/health"

    def always_succeed(self) -> None:
        _drop_handlers_for(self._h, self._path, "GET")
        self._h.expect_request(self._path, method="GET").respond_with_json(
            {"status": "ok"}, status=200, headers=_CORS
        )

    def always_fail(self) -> None:
        """Every poll returns 503 until the test flips back to
        `always_succeed()`. Used by the cancel-mid-Online test to hold the
        Online step in its retry backoff (a deterministic cancel window)
        without coupling run-1 timing to run-2 success the way the
        counter-based `fail_first_then_succeed` does — the Online step
        issues a preflight GET *and* a poll GET per attempt, so a fixed
        count succeeds before the test can react."""
        _drop_handlers_for(self._h, self._path, "GET")
        self._h.expect_request(self._path, method="GET").respond_with_json(
            {"error": "transient"}, status=503, headers=_CORS
        )

    def fail_first_then_succeed(self, n: int) -> None:
        """First `n` calls return 503, then 200. Used by the
        cancel-mid-Online test to give the test time to click Cancel
        before the step succeeds."""
        from werkzeug.wrappers import Response
        counter = {"n": 0}

        def handler(_request) -> Response:
            counter["n"] += 1
            status = 503 if counter["n"] <= n else 200
            body = '{"error":"transient"}' if status == 503 else '{"status":"ok"}'
            return Response(
                response=body,
                status=status,
                content_type="application/json",
                headers=_CORS,
            )

        _drop_handlers_for(self._h, self._path, "GET")
        self._h.expect_request(self._path, method="GET").respond_with_handler(handler)
