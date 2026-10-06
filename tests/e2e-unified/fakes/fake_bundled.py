"""Reference server for the open **Fauna Bundled Provider API v1**
(`docs/goal/architecture/provisioning/bundled-provider-api.md`) — the
intermediary the tier_2 onboarding journey buys its domain AND server
through, on a single provider row.

It implements the WHOLE spec (every endpoint in § Endpoints, the RFC 8628
device-authorization flow in § Authentication, the error envelope in § Error
envelope, permissive CORS in § CORS), statefully, on the shared
pytest-httpserver port `fake_cloud` already owns — `FakeCloud.bundled` is
the instance. Because it is the spec end to end rather than the subset one
test happens to hit, the wizard is exercised against the contract an
implementer must meet; the client-side pins live in
`libs/fauna-provisioning/tests/bundled_conformance.rs`.

State a test can read after a run: `registered` (every `POST /v1/domains`
body, in order), `servers`, `records`, `ptr`, `device_codes`. Knobs:
`auto_approve` (the hosted sign-in approves itself on the first token poll —
the user's browser step, collapsed), `registration_cents` /
`renewal_cents` / `currency`, `unsupported_tlds`, `taken_names`.
"""
from __future__ import annotations

import itertools
import json
import re
from urllib.parse import parse_qs

from pytest_httpserver import HTTPServer
from werkzeug.wrappers import Request, Response

# Spec § CORS: mandatory on every endpoint, auth included — the web app calls
# the intermediary directly, with no proxy hop.
_CORS = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, PUT, DELETE, OPTIONS",
    "Access-Control-Allow-Headers": "Authorization, Content-Type",
}

API_VERSION = 1


def _json(body, status: int = 200) -> Response:
    return Response(
        response=json.dumps(body),
        status=status,
        content_type="application/json",
        headers=_CORS,
    )


def _error(status: int, code: str, message: str, details: dict | None = None) -> Response:
    """Spec § Error envelope."""
    err = {"code": code, "message": message}
    if details:
        err["details"] = details
    return _json({"error": err}, status)


class BundledHandlers:
    """One conformant intermediary, mounted at `prefix` on `httpserver`."""

    def __init__(self, httpserver: HTTPServer, prefix: str) -> None:
        self._h = httpserver
        self._prefix = prefix
        self._ids = itertools.count(1)
        # --- knobs ---
        self.token = "bundled-e2e-token"
        self.auto_approve = True
        self.registration_cents = 1099
        self.renewal_cents = 1499
        self.currency = "EUR"
        self.unsupported_tlds = {"zz"}
        self.taken_names: set[str] = set()
        # When set, `GET /v1/domains/{name}/auth-code` answers the spec's
        # `202 available_after` arm instead of handing the code over — a
        # registry-imposed transfer lock, which is NEVER a refusal
        # (bundled-provider-api.md § Exit guarantee 1). The retire view
        # renders the date the lock lifts.
        self.transfer_lock_until: str | None = None
        # --- state ---
        self.device_codes: dict[str, dict] = {}
        self.registered: list[dict] = []
        self.zones: dict[str, dict] = {}
        self.records: dict[str, list[dict]] = {}
        self.servers: dict[str, dict] = {}
        self.ptr: dict[str, str | None] = {}

    # -- addressing ---------------------------------------------------------

    def base_url(self) -> str:
        """What the user types into the wizard's `base-url` field."""
        return self._h.url_for("").rstrip("/") + self._prefix

    def install_defaults(self) -> None:
        pattern = re.compile(rf"^{re.escape(self._prefix)}/v1/.*$")
        for m in ("GET", "POST", "PUT", "DELETE"):
            self._h.expect_request(pattern, method=m).respond_with_handler(self._dispatch)
        self._h.expect_request(pattern, method="OPTIONS").respond_with_data(
            "", status=204, headers=_CORS
        )

    # -- routing ------------------------------------------------------------

    def _dispatch(self, request: Request) -> Response:
        path = request.path[len(self._prefix):]
        method = request.method

        # Unauthenticated endpoints (spec § Endpoints).
        if path == "/v1/auth/device" and method == "POST":
            return self._auth_device(request)
        if path == "/v1/auth/token" and method == "POST":
            return self._auth_token(request)
        if path == "/v1/pricing/tlds" and method == "GET":
            return self._pricing()

        auth = request.headers.get("Authorization", "")
        if auth != f"Bearer {self.token}":
            return _error(401, "unauthorized", "missing or revoked token")

        routes = [
            (r"^/v1/me$", "GET", self._me),
            (r"^/v1/domains/check$", "GET", self._domains_check),
            (r"^/v1/domains$", "POST", self._domains_register),
            (r"^/v1/contact$", "GET", self._contact),
            (r"^/v1/domains/(?P<name>[^/]+)/auth-code$", "GET", self._auth_code),
            (r"^/v1/zones/(?P<zone>[^/]+)/records$", "GET", self._records_list),
            (r"^/v1/zones/(?P<zone>[^/]+)/records$", "POST", self._records_create),
            (r"^/v1/zones/(?P<zone>[^/]+)/records/(?P<rid>[^/]+)$", "DELETE", self._records_delete),
            (r"^/v1/server_types$", "GET", self._server_types),
            (r"^/v1/servers$", "GET", self._servers_list),
            (r"^/v1/servers$", "POST", self._servers_create),
            (r"^/v1/servers/(?P<sid>[^/]+)$", "DELETE", self._servers_delete),
            (r"^/v1/servers/(?P<sid>[^/]+)/ptr$", "GET", self._ptr_get),
            (r"^/v1/servers/(?P<sid>[^/]+)/ptr$", "PUT", self._ptr_put),
        ]
        for pat, m, fn in routes:
            match = re.match(pat, path)
            if match and m == method:
                return fn(request, **match.groupdict())
        return _error(404, "not_found", f"no route for {method} {path}")

    # -- hosted-auth (RFC 8628) ----------------------------------------------

    def _auth_device(self, request: Request) -> Response:
        form = parse_qs(request.get_data(as_text=True))
        if form.get("client_id") != ["fauna"]:
            return _json({"error": "invalid_client"}, 400)
        n = next(self._ids)
        code = f"dc-{n}"
        self.device_codes[code] = {
            "user_code": f"FAUNA-{n:04d}",
            "approved": False,
            "polls": 0,
        }
        return _json({
            "device_code": code,
            "user_code": self.device_codes[code]["user_code"],
            "verification_uri": f"{self.base_url()}/activate",
            "verification_uri_complete": f"{self.base_url()}/activate?user_code={self.device_codes[code]['user_code']}",
            "expires_in": 600,
            # 1 s keeps the journey quick without a wall-clock assertion
            # anywhere: the client polls until the token lands, however long
            # that takes (convention 14).
            "interval": 1,
        })

    def approve(self, user_code: str) -> None:
        """The user's browser step: approve the code shown on the hosted page."""
        for entry in self.device_codes.values():
            if entry["user_code"] == user_code:
                entry["approved"] = True
                return
        raise KeyError(user_code)

    def _auth_token(self, request: Request) -> Response:
        form = parse_qs(request.get_data(as_text=True))
        if form.get("grant_type") != ["urn:ietf:params:oauth:grant-type:device_code"]:
            return _json({"error": "unsupported_grant_type"}, 400)
        entry = self.device_codes.get((form.get("device_code") or [""])[0])
        if entry is None:
            return _json({"error": "expired_token"}, 400)
        entry["polls"] += 1
        if self.auto_approve:
            entry["approved"] = True
        if not entry["approved"]:
            return _json({"error": "authorization_pending"}, 400)
        return _json({"access_token": self.token, "token_type": "bearer", "scope": "provisioning"})

    # -- identity + catalog -------------------------------------------------

    def _me(self, request: Request) -> Response:
        return _json({
            "api_version": API_VERSION,
            "account": {"id": "acct-e2e"},
            "zones": list(self.zones.values()),
            "locations": [
                {"id": "eu-1", "name": "Europe 1", "city": "Falkenstein", "country": "DE"},
            ],
        })

    def _pricing(self) -> Response:
        return _json({
            "currency": self.currency,
            "tlds": [
                {"tld": tld, "registration_cents": self.registration_cents, "renewal_cents": self.renewal_cents}
                for tld in ("io", "com", "net", "test")
            ],
        })

    # -- registrar ----------------------------------------------------------

    def _domains_check(self, request: Request) -> Response:
        name = request.args.get("name", "").lower()
        tld = name.rsplit(".", 1)[-1] if "." in name else ""
        if tld in self.unsupported_tlds:
            return _json({"name": name, "status": "tld_not_supported"})
        if name in self.taken_names or any(z["name"] == name for z in self.zones.values()):
            return _json({"name": name, "status": "unavailable"})
        return _json({
            "name": name,
            "status": "available",
            "currency": self.currency,
            "registration_cents": self.registration_cents,
            "renewal_cents": self.renewal_cents,
        })

    def _domains_register(self, request: Request) -> Response:
        body = request.get_json(force=True, silent=True) or {}
        name = str(body.get("name", "")).lower()
        if not name or not body.get("contact"):
            return _error(400, "invalid_request", "name and contact are required")
        if body.get("agreed_price_cents") != self.registration_cents:
            return _error(
                409, "price_changed", "the quote changed",
                {"registration_cents": self.registration_cents},
            )
        if name in self.taken_names:
            return _error(422, "domain_unavailable", "already registered")
        self.registered.append(body)
        zone_id = f"z-{next(self._ids)}"
        self.zones[zone_id] = {"id": zone_id, "name": name}
        self.records.setdefault(zone_id, [])
        return _json(
            {"name": name, "nameservers": ["ns1.bundle.test", "ns2.bundle.test"]},
            201,
        )

    def _contact(self, request: Request) -> Response:
        # No account-default contact: the wizard collects it (registrant =
        # the user, spec § Exit).
        return _error(404, "not_found", "no default contact on this account")

    def _auth_code(self, request: Request, name: str) -> Response:
        if not any(z["name"] == name.lower() for z in self.zones.values()):
            return _error(404, "not_found", "not registered here")
        if self.transfer_lock_until is not None:
            # 202, not 4xx: the door is never closed, only dated.
            return _json({"available_after": self.transfer_lock_until}, 202)
        return _json({"auth_code": f"EPP-{name.upper()}"})

    # -- dns ----------------------------------------------------------------

    def _records_list(self, request: Request, zone: str) -> Response:
        if zone not in self.zones:
            return _error(404, "not_found", "no such zone")
        want_name = request.args.get("name")
        want_type = request.args.get("type")
        out = [
            r for r in self.records.get(zone, [])
            if (want_name is None or r["name"] == want_name)
            and (want_type is None or r["type"] == want_type)
        ]
        return _json({"records": out})

    def _records_create(self, request: Request, zone: str) -> Response:
        if zone not in self.zones:
            return _error(404, "not_found", "no such zone")
        body = request.get_json(force=True, silent=True) or {}
        rec = {
            "id": f"r-{next(self._ids)}",
            "type": body.get("type"),
            "name": body.get("name"),
            "value": body.get("value"),
            "ttl": body.get("ttl", 300),
        }
        if body.get("priority") is not None:
            rec["priority"] = body["priority"]
        self.records.setdefault(zone, []).append(rec)
        return _json(rec, 201)

    def _records_delete(self, request: Request, zone: str, rid: str) -> Response:
        recs = self.records.get(zone, [])
        before = len(recs)
        recs[:] = [r for r in recs if r["id"] != rid]
        return Response(status=204 if len(recs) < before else 404, headers=_CORS)

    # -- vps ----------------------------------------------------------------

    def _server_types(self, request: Request) -> Response:
        # The intermediary's own curated catalog, in display order (spec
        # § Endpoints): the wizard shows these verbatim.
        return _json({"server_types": [
            {"id": "small", "vcpu": 2, "mem_gb": 2.0, "disk_gb": 40, "price_monthly_cents": 599, "currency": self.currency},
            {"id": "medium", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 80, "price_monthly_cents": 999, "currency": self.currency},
            {"id": "large", "vcpu": 4, "mem_gb": 8.0, "disk_gb": 160, "price_monthly_cents": 1899, "currency": self.currency},
        ]})

    def _servers_list(self, request: Request) -> Response:
        name = request.args.get("name")
        label = request.args.get("label")
        out = list(self.servers.values())
        if name is not None:
            out = [s for s in out if s["name"] == name]
        if label is not None and "=" in label:
            k, v = label.split("=", 1)
            out = [s for s in out if s.get("labels", {}).get(k) == v]
        return _json({"servers": out})

    def _servers_create(self, request: Request) -> Response:
        body = request.get_json(force=True, silent=True) or {}
        sid = f"s-{next(self._ids)}"
        server = {
            "id": sid,
            "name": body.get("name"),
            "ipv4": "203.0.113.5",
            "status": "running",
            "labels": body.get("labels") or {},
            # Kept for assertions — the cloud-init the box would boot with.
            "user_data": body.get("user_data", ""),
            "location": body.get("location"),
            "server_type": body.get("server_type"),
        }
        self.servers[sid] = server
        self.ptr[sid] = None
        public = {k: server[k] for k in ("id", "name", "ipv4", "status", "labels")}
        return _json(public, 201)

    def _servers_delete(self, request: Request, sid: str) -> Response:
        existed = self.servers.pop(sid, None) is not None
        self.ptr.pop(sid, None)
        return Response(status=204 if existed else 404, headers=_CORS)

    def _ptr_get(self, request: Request, sid: str) -> Response:
        if sid not in self.servers:
            return _error(404, "not_found", "no such server")
        return _json({"ptr": self.ptr.get(sid)})

    def _ptr_put(self, request: Request, sid: str) -> Response:
        if sid not in self.servers:
            return _error(404, "not_found", "no such server")
        body = request.get_json(force=True, silent=True) or {}
        self.ptr[sid] = body.get("ptr")
        return Response(status=204, headers=_CORS)
