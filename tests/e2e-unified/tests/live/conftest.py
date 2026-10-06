"""Fixtures + gating for the **live Hetzner provisioning** e2e
(`test_hetzner_provision.py`).

This is the one e2e tier that stands up a *real, paid* VPS at a real cloud
provider, drives DNS in a real zone, and tears it all down. It is therefore
**opt-in** and never runs in default CI:

  * ``HETZNER_API_TOKEN`` — REQUIRED, from the env **or the file
    ``~/.hetzner-token``** (the fleet's per-machine home for it; the env wins
    when both are set — ``helpers.live_provision.hetzner_token``). A single
    Hetzner **Cloud** API token with Read&Write on the project. It covers *both*
    server provisioning and DNS (Hetzner's standalone DNS API went read-only
    2026-05-20; zones are now Cloud resources under the same Bearer token —
    ``i18n/providers.yaml`` hetzner ``api-token`` ``kinds:[vps,dns]``). This is
    the *only* required input.
  * ``FAUNA_E2E_LIVE=1`` — REQUIRED explicit opt-in (belt-and-suspenders so a
    stray token in the env can never trigger a paid provision). ``just
    e2e-live-provision`` sets it for you.
  * ``FAUNA_E2E_ZONE`` — OPTIONAL. The apex DNS zone to deploy a throwaway
    subdomain under. If unset, the token's zones are listed and exactly one must
    exist (else the test skips, telling you to set this). Honors "the API key is
    the only input" for the common single-zone account.
  * ``FAUNA_E2E_KEEP=1`` — OPTIONAL. Skip teardown so you can inspect the box.
  * ``FAUNA_E2E_VERIFY_DKIM=0`` — OPTIONAL. Disable the DKIM publish-match gate
    (gate 6) during early bring-up before the box's mail domain serves.

Teardown ALWAYS runs (even on assertion failure / abort) unless
``FAUNA_E2E_KEEP``: it deletes this run's server + DNS records (keyed by a unique
``e2e-<runid>`` anchor, so concurrent runs never collide), and age-sweeps any
``e2e-*`` server older than 1h left behind by a crashed prior run.
"""
from __future__ import annotations

import os
import time
import uuid
from datetime import datetime, timezone

import pytest
import requests

from helpers.live_provision import hetzner_token

HETZNER_API = "https://api.hetzner.cloud/v1"
# A server older than this whose name starts with `e2e-` is a crashed-run orphan
# (a live run finishes in well under 15 min), so the sweep can delete it without
# risking a concurrent run's fresh box.
ORPHAN_MAX_AGE_S = 3600
# Stable anchor: the provisioned domain is `e2e-<runid>.<zone>` and the orchestrator
# names the server `domain.replace('.', '-')`, so both start with this prefix.
E2E_PREFIX = "e2e-"
# Provider-side label the crate attaches to every e2e box (via the machine's
# `set_provision_labels` IPC — production provisions no labels). Teardown's orphan
# sweep selects by this `label_selector`, cleaner than the name-prefix heuristic
# and robust to a server-name scheme change. The test injects it; canonical here.
E2E_LABEL_KEY = "fauna-e2e"
E2E_LABEL_VALUE = "1"
E2E_LABEL_SELECTOR = f"{E2E_LABEL_KEY}={E2E_LABEL_VALUE}"


def _truthy(v: str | None) -> bool:
    return (v or "").strip().lower() in {"1", "true", "yes", "on"}


def _zone_file_txt(value: str) -> str:
    """Wrap a bare TXT value in the zone-file quoting Hetzner Cloud DNS requires:
    one or more ``"…"`` character-strings, split into <=255-octet chunks for long
    values (e.g. a DKIM key). Mirrors ``dns/hetzner.rs::to_zone_file_txt`` so the
    test publishes DKIM in the same form the production adapter does."""
    if value.startswith('"'):
        return value
    if len(value) <= 255:
        return f'"{value}"'
    return " ".join(f'"{value[i:i + 255]}"' for i in range(0, len(value), 255))


def pytest_collection_modifyitems(items):
    """Raise the per-test timeout for live tests that lack an explicit mark.

    The harness-wide default (pytest.ini ``timeout = 900``) fits every local
    tier, but a live provision legitimately exceeds it: the real-ACME TLS-trust
    gate alone budgets ~40 min for Hetzner DNS propagation (testing.md § Gap 3).
    Bounded is still the law — 90 min covers provision + the 40-min gate with
    margin, and a wedged live run dies loudly instead of holding the flock."""
    for item in items:
        if item.get_closest_marker("timeout") is None:
            item.add_marker(pytest.mark.timeout(5400))


# ── Opt-in gate ────────────────────────────────────────────────────────────
@pytest.fixture(autouse=True)
def _live_provisioning_gate(request):
    """Skip every ``live_provisioning`` test unless the token AND the explicit
    opt-in are both present. No-op for any other test (this conftest only loads
    for ``tests/live/``, but the marker check keeps it harmless regardless)."""
    if request.node.get_closest_marker("live_provisioning") is None:
        return
    token = hetzner_token()
    if not token:
        pytest.skip(
            "live Hetzner provisioning needs HETZNER_API_TOKEN (a Cloud R/W token) in the "
            "env or in ~/.hetzner-token"
        )
    # A file-sourced token is promoted into the env so the four live tests and
    # the `hetzner` fixture keep their one name (`os.environ["HETZNER_API_TOKEN"]`)
    # — one resolver, one home for every reader downstream of this gate.
    os.environ.setdefault("HETZNER_API_TOKEN", token)
    if not _truthy(os.environ.get("FAUNA_E2E_LIVE")):
        pytest.skip(
            "live Hetzner provisioning is opt-in (real € cost): set FAUNA_E2E_LIVE=1 "
            "or run `just e2e-live-provision`"
        )


# ── Hetzner Cloud API (server + DNS, one token) ────────────────────────────
class HetznerApi:
    """Thin direct client for the Hetzner Cloud API, mirroring the wire shapes
    the production crate uses (`libs/fauna-provisioning/src/{vps,dns}/hetzner.rs`).
    Used only for zone resolution, the DKIM TXT publish, and teardown — the
    *provisioning* itself goes through the real crate via the onboarding bridge."""

    def __init__(self, token: str, base: str = HETZNER_API, timeout: float = 30.0):
        self._h = {"Authorization": f"Bearer {token}"}
        self._base = base
        self._timeout = timeout

    def _get_paged(self, path: str, key: str, extra_params: dict | None = None) -> list[dict]:
        out: list[dict] = []
        page = 1
        while True:
            params = {"per_page": 50, "page": page}
            if extra_params:
                params.update(extra_params)
            r = requests.get(
                f"{self._base}{path}",
                headers=self._h,
                params=params,
                timeout=self._timeout,
            )
            r.raise_for_status()
            body = r.json()
            out.extend(body.get(key, []))
            nxt = (body.get("meta", {}).get("pagination", {}) or {}).get("next_page")
            if not nxt:
                return out
            page = nxt

    # zones: GET /v1/zones -> {"zones": [{"id": <int>, "name": <str>}]}
    def list_zones(self) -> list[dict]:
        return self._get_paged("/zones", "zones")

    # servers: GET /v1/servers -> {"servers": [{"id": <int>, "name", "created", ...}]}
    def list_servers(self) -> list[dict]:
        return self._get_paged("/servers", "servers")

    def list_servers_labeled(self, selector: str) -> list[dict]:
        """Servers matching a Hetzner ``label_selector`` (e.g. ``fauna-e2e=1``).
        The crate now tags every e2e box via the machine's `set_provision_labels`
        IPC, so teardown can select boxes by label instead of a name-prefix
        heuristic — cleaner, and robust to a server-name scheme change."""
        return self._get_paged("/servers", "servers", {"label_selector": selector})

    def delete_server(self, server_id) -> None:
        r = requests.delete(
            f"{self._base}/servers/{server_id}", headers=self._h, timeout=self._timeout
        )
        if r.status_code not in (200, 202, 204, 404):
            r.raise_for_status()

    # GET /v1/zones/{id}/rrsets -> {"rrsets": [{"name","type","ttl","records":[{"value"}]}]}
    def list_rrsets(self, zone_id) -> list[dict]:
        return self._get_paged(f"/zones/{zone_id}/rrsets", "rrsets")

    def publish_txt(self, zone_id, name: str, value: str, ttl: int = 120) -> None:
        """Create a TXT RRset (used to publish the nest's DKIM public value).
        Mirrors `dns/hetzner.rs::create_record`'s create branch — Hetzner Cloud
        requires the TXT value zone-file-quoted."""
        r = requests.post(
            f"{self._base}/zones/{zone_id}/rrsets",
            headers=self._h,
            json={"name": name, "type": "TXT", "ttl": ttl,
                  "records": [{"value": _zone_file_txt(value)}]},
            timeout=self._timeout,
        )
        if r.status_code not in (200, 201, 202):
            r.raise_for_status()

    def remove_rrset_records(self, zone_id, name: str, rtype: str, values: list[str]) -> None:
        """Value-scoped remove (the only delete the crate proves against the real
        API — `dns/hetzner.rs::delete_record`); passing every value empties the
        RRset. Idempotent: removing an absent value is a no-op."""
        if not values:
            return
        r = requests.post(
            f"{self._base}/zones/{zone_id}/rrsets/{name}/{rtype}/actions/remove_records",
            headers=self._h,
            json={"records": [{"value": v} for v in values]},
            timeout=self._timeout,
        )
        if r.status_code not in (200, 201, 202, 404):
            r.raise_for_status()


@pytest.fixture
def hetzner() -> HetznerApi:
    return HetznerApi(hetzner_token())


@pytest.fixture
def e2e_zone(hetzner: HetznerApi) -> dict:
    """Resolve the DNS zone (apex) to deploy under. ``FAUNA_E2E_ZONE`` picks it
    explicitly; otherwise the token must manage exactly one zone."""
    zones = hetzner.list_zones()
    want = os.environ.get("FAUNA_E2E_ZONE", "").strip().rstrip(".").lower()
    if want:
        for z in zones:
            if z["name"].rstrip(".").lower() == want:
                return {"id": z["id"], "name": z["name"].rstrip(".")}
        pytest.skip(
            f"FAUNA_E2E_ZONE={want!r} not among the token's zones "
            f"({[z['name'] for z in zones]})"
        )
    if len(zones) == 1:
        return {"id": zones[0]["id"], "name": zones[0]["name"].rstrip(".")}
    pytest.skip(
        "token manages "
        + (f"{len(zones)} zones {[z['name'] for z in zones]}; set FAUNA_E2E_ZONE to pick one"
           if zones else "no DNS zones; create one or point the token at a project that has one")
    )


@pytest.fixture
def runid() -> str:
    # pytest may use time/uuid (the Date.now/random ban is Workflow-script-only).
    return f"{int(time.time())}-{uuid.uuid4().hex[:6]}"


class Deployment:
    """Mutable record the test populates as it learns the box's identity, so the
    teardown fixture can clean up even when the test aborts mid-flight."""

    def __init__(self, hetzner: HetznerApi, zone: dict, runid: str):
        self.hetzner = hetzner
        self.zone = zone
        self.runid = runid
        self.subdomain = f"{E2E_PREFIX}{runid}.{zone['name']}"  # e.g. e2e-<runid>.example.com
        self.handle = f"admin@{self.subdomain}"
        self.identity_secret_hex = os.urandom(32).hex()
        self.server_id = None
        self.ipv4 = None
        self.fqdn = None
        self.published_txt: list[tuple[str, str]] = []  # (name, value) DKIM TXT we published

    def rrset_name(self, fqdn: str) -> str:
        """Relativize a fully-qualified owner name to this run's zone, mirroring
        the production crate's ``dns_record_name`` (``orchestrator.rs``). Hetzner
        Cloud RRset names are **zone-relative**, so posting an FQDN owner
        double-suffixes the record (``sel._domainkey.sub.zone.tld.zone.tld``)."""
        zone = self.zone["name"]
        if fqdn == zone:
            return "@"
        suffix = f".{zone}"
        return fqdn[: -len(suffix)] if fqdn.endswith(suffix) else fqdn


@pytest.fixture
def box(hetzner: HetznerApi, e2e_zone: dict, runid: str):
    """Yield a :class:`Deployment` and ALWAYS tear it down afterward (server +
    DNS), unless ``FAUNA_E2E_KEEP``."""
    dep = Deployment(hetzner, e2e_zone, runid)
    try:
        yield dep
    finally:
        if _truthy(os.environ.get("FAUNA_E2E_KEEP")):
            # Look the box up by its unique runid anchor so we can name the id
            # even when provisioning failed before `dep.server_id` was recorded.
            ids = [(s.get("id"), s.get("name")) for s in _this_run_servers(dep)]
            print(
                f"\n[live] FAUNA_E2E_KEEP set — leaving server(s) "
                f"{ids or f'id={dep.server_id}'} ip={dep.ipv4} subdomain={dep.subdomain} "
                f"(+ its DNS records) for inspection. Delete from the Hetzner console."
            )
        else:
            _teardown(dep)


def _this_run_servers(dep: Deployment) -> list[dict]:
    """Servers belonging to THIS run, matched by the unique ``e2e-<runid>`` name
    anchor (the orchestrator names the box ``<subdomain>``.replace('.', '-')).
    Lets teardown delete the box even when provisioning failed *before* the test
    recorded ``dep.server_id`` (the id is only set after full success) — the gap
    that otherwise leaks a paid box on any pre-`result` failure."""
    anchor = f"{E2E_PREFIX}{dep.runid}"
    try:
        return [s for s in dep.hetzner.list_servers() if anchor in s.get("name", "")]
    except Exception as e:  # noqa: BLE001 — teardown must never raise
        print(f"[live] teardown WARNING: list servers for runid match failed: {e!r}")
        return []


def _teardown(dep: Deployment) -> None:
    h = dep.hetzner
    # 1. Delete THIS run's server by id (the primary, collision-free cleanup).
    if dep.server_id is not None:
        try:
            h.delete_server(dep.server_id)
            print(f"\n[live] teardown: deleted server {dep.server_id}")
        except Exception as e:  # noqa: BLE001 — teardown must never raise
            print(f"\n[live] teardown WARNING: delete server {dep.server_id} failed: {e!r}")
    # 1b. Belt-and-suspenders: delete THIS run's box by its unique runid name
    #     anchor, covering the case where the box was created but provisioning
    #     failed before `dep.server_id` was set. Safe — the runid is unique to
    #     this run, so it never touches a concurrent run's box (no age-gate
    #     needed, unlike the generic orphan sweep below).
    for s in _this_run_servers(dep):
        if s.get("id") == dep.server_id:
            continue  # already deleted above
        try:
            h.delete_server(s["id"])
            print(f"[live] teardown: deleted this-run server {s['id']} ({s.get('name')}) by runid")
        except Exception as e:  # noqa: BLE001
            print(f"[live] teardown WARNING: delete this-run server {s.get('id')} failed: {e!r}")
    # 2. Age-gated orphan sweep of crashed prior runs (older than 1h). Primary
    #    selector is the `fauna-e2e=1` provider label (set on every box the crate
    #    now provisions for the e2e); unioned with the legacy `e2e-` name-prefix so
    #    boxes created before labels landed are still swept. Age-gating keeps a
    #    concurrent run's fresh box safe (it also carries the label).
    try:
        now = datetime.now(timezone.utc)
        try:
            labeled = h.list_servers_labeled(E2E_LABEL_SELECTOR)
        except Exception as e:  # noqa: BLE001 — label query failure falls back to name-prefix
            print(f"[live] teardown WARNING: label sweep query failed, name-prefix only: {e!r}")
            labeled = []
        candidates: dict = {}
        for s in labeled:
            candidates[s.get("id")] = s
        for s in h.list_servers():
            if s.get("name", "").startswith(E2E_PREFIX):
                candidates.setdefault(s.get("id"), s)
        for s in candidates.values():
            if s.get("id") == dep.server_id:
                continue
            name = s.get("name", "")
            created = s.get("created")
            try:
                age = (now - datetime.fromisoformat(created.replace("Z", "+00:00"))).total_seconds()
            except Exception:  # noqa: BLE001
                age = ORPHAN_MAX_AGE_S + 1  # unparseable timestamp -> treat as old orphan
            if age > ORPHAN_MAX_AGE_S:
                try:
                    h.delete_server(s["id"])
                    print(f"[live] teardown: swept orphan server {s['id']} ({name}, age {int(age)}s)")
                except Exception as e:  # noqa: BLE001
                    print(f"[live] teardown WARNING: sweep server {s['id']} failed: {e!r}")
    except Exception as e:  # noqa: BLE001
        print(f"[live] teardown WARNING: orphan sweep failed: {e!r}")
    # 3. Delete THIS run's DNS records: every RRset whose name carries the runid
    #    anchor (the A/MX/SPF/DMARC the orchestrator published + the DKIM TXT we
    #    published). Keyed by runid, so concurrent runs never touch each other.
    anchor = f"{E2E_PREFIX}{dep.runid}"
    try:
        for rs in h.list_rrsets(dep.zone["id"]):
            name = rs.get("name", "")
            if anchor not in name:
                continue
            values = [r.get("value", "") for r in rs.get("records", []) if r.get("value")]
            try:
                h.remove_rrset_records(dep.zone["id"], name, rs.get("type", "TXT"), values)
                print(f"[live] teardown: cleared RRset {name} {rs.get('type')} ({len(values)} values)")
            except Exception as e:  # noqa: BLE001
                print(f"[live] teardown WARNING: clear RRset {name} failed: {e!r}")
    except Exception as e:  # noqa: BLE001
        print(f"[live] teardown WARNING: DNS sweep failed: {e!r}")
