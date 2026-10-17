#!/usr/bin/env -S uv run --quiet --no-project
"""Deploy-verify GATE G1-cloud — the provider-side half of the G1 firewall check.

WHY THIS EXISTS — the gap the outside-in G1 probe cannot close:
  `deploy-verify-binaries.py` § G1 proves, by probing from the gate's own host,
  that no internal-only port answers THAT host. But it sees one source address
  only. The deployment also has a firewall layer IN FRONT of the box — the
  Hetzner Cloud firewalls attached to the server — and the probe cannot read
  it: if every cloud firewall were detached (a console mis-click, an automation
  regression), inbound from everywhere would be filtered by nothing but ufw,
  and the probe would still pass green. Conversely, a cloud
  rule could open an internal-only port (nest :3000, CalDAV :8443/:8444,
  watchtower :8080) without touching the box at all.

WHAT THIS GATE DOES (read-only, no SSH — Hetzner rate-limits SSH, and this
gate spends none of that budget; it talks only to api.hetzner.cloud):
  1. Reads the Cloud API token of the project the host lives in —
     $HETZNER_API_TOKEN, else that project's token file
     (scripts/_cloud_project.py; `--token-file` names one explicitly).
  2. Resolves the target host's public IP(s) and matches them against
     `GET /v1/servers` to find the server — no hard-coded server id, so the
     gate follows a re-provision or IP move.
  3. Reads the firewalls applied to that server (`GET /v1/firewalls`) and
     asserts the SAFETY INVARIANT:
       (a) at least one cloud firewall is applied — a fully-detached server
           means the cloud-side layer is silently gone (inbound wide open up
           to ufw), which is exactly the drift to catch;
       (b) NO applied inbound rule covers an internal-only port
           {3000, 8443, 8444, 8080} — from ANY source: the loopback-trust
           model has no legitimate off-box caller for these, so even an
           allowlisted source is a violation;
       (c) prints the effective inbound (port → sources) allow table so a
           human reviews drift the invariant can't formalize.

  Deliberately NOT asserted: which ports are open to which sources. The
  box's inbound allowlist is an operational choice that may legitimately
  change (tighten or open) without a code change; hard-coding today's rrset
  would turn every legitimate edit into a red gate. The invariant above is
  the part that must never drift.

Usage:
    scripts/deploy-verify-cloud-firewall.py dev.fauna.build
    scripts/deploy-verify-cloud-firewall.py https://dev.fauna.build   # scheme and user@ stripped
    scripts/deploy-verify-cloud-firewall.py dev.fauna.build --warn-only

Exit codes:
    0  invariant holds ((a)+(b)), or --warn-only
    1  violation — no applied cloud firewall, or an internal port is opened
    2  cannot verify — API unreachable (after retries), DNS resolution failed,
       or the host is not among the token's servers
    3  no token ($HETZNER_API_TOKEN unset and the host's project token file
       missing, empty or readable by other users)

Dependencies: Python stdlib only (urllib) — safe from any venv or none — plus
its sibling scripts/_cloud_project.py (the box→project map).
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HETZNER_API = "https://api.hetzner.cloud/v1"


def _load_cloud_project():
    import importlib.util
    from pathlib import Path

    path = Path(__file__).resolve().parent / "_cloud_project.py"
    spec = importlib.util.spec_from_file_location("_cloud_project", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


#: The box→cloud-project map and the per-project token files — shared with
#: the other cloud scripts and the live test harness.
cloud_project = _load_cloud_project()

# Internal-only ports that must NEVER be opened by a cloud rule (same set as
# the in-box G1 spot-check): nest :3000, CalDAV bare-IP :8443 + domain
# loopback :8444, watchtower API :8080.
FORBIDDEN_PORTS = (3000, 8443, 8444, 8080)


class _TransientApiError(Exception):
    """A retryable API failure (5xx or a network-level error)."""


# ── Pure helpers (unit-tested in tests/e2e-unified/tests/) ───────────────────


def _normalize_host(host: str) -> str:
    """`root@example.com` / `https://example.com/` / `example.com` → bare hostname."""
    host = host.strip()
    if host.startswith(("http://", "https://")):
        host = urllib.parse.urlparse(host).netloc
    if "@" in host:
        host = host.split("@", 1)[1]
    return host.rstrip("/").split(":")[0]


def _parse_port_spec(spec: str | None) -> tuple[int, int]:
    """Hetzner rule port spec → inclusive (lo, hi). `"80"` → (80, 80),
    `"80-85"` → (80, 85). A missing/empty/`any` spec is treated as covering
    every port (fail-closed)."""
    if not spec or spec.strip().lower() == "any":
        return (1, 65535)
    spec = spec.strip()
    if "-" in spec:
        lo, hi = spec.split("-", 1)
        return (int(lo), int(hi))
    p = int(spec)
    return (p, p)


def _rule_covers_port(rule: dict, port: int) -> bool:
    """True iff this rule is an inbound tcp/udp rule whose port range covers
    `port`. udp counts too — a udp allowance on an internal-only port is still
    posture drift with no legitimate use. Portless protocols (icmp/esp/gre)
    cannot expose a TCP service."""
    if rule.get("direction") != "in":
        return False
    if rule.get("protocol") not in ("tcp", "udp"):
        return False
    lo, hi = _parse_port_spec(rule.get("port"))
    return lo <= port <= hi


def _find_server_by_ips(servers: list[dict], ips: set[str]) -> dict | None:
    """The server whose public IPv4/IPv6 matches any resolved IP, or None."""
    for s in servers:
        pn = s.get("public_net") or {}
        v4 = (pn.get("ipv4") or {}).get("ip", "")
        v6 = (pn.get("ipv6") or {}).get("ip", "")  # a /64 net like "2001:db8::/64"
        if v4 in ips:
            return s
        if v6:
            prefix = v6.split("/", 1)[0].rstrip(":")
            if prefix and any(ip.startswith(prefix) for ip in ips):
                return s
    return None


class _Posture:
    """Evaluation result: hard violations, advisories, and the human-review
    allow table."""

    def __init__(self):
        self.violations: list[str] = []
        self.warnings: list[str] = []
        self.allow_table: list[str] = []
        self.cannot_verify_reason: str | None = None


def _evaluate_posture(applied_firewalls: list[dict], statuses: dict) -> _Posture:
    """Assert the safety invariant over the firewalls applied to the server.
    `statuses` maps firewall id → apply status ("applied" / "pending")."""
    result = _Posture()

    # (a) ≥1 cloud firewall applied — a detached box = the layer silently gone.
    if not applied_firewalls:
        result.violations.append(
            "no cloud firewall is applied to the server — the provider-side "
            "inbound filter is GONE (drift: someone detached it)"
        )
        return result

    any_confirmed_applied = False
    for fw in applied_firewalls:
        fw_label = f"firewall {fw.get('id')} ({fw.get('name', '?')!r})"
        # Only a status of exactly "applied" means the provider has confirmed
        # the ruleset is live; "pending" means it's attached but not yet
        # enforced, so it does NOT count toward invariant (a) below.
        if statuses.get(fw.get("id")) == "applied":
            any_confirmed_applied = True
        else:
            result.warnings.append(
                f"{fw_label} apply status is PENDING — rules may not be live yet"
            )
        for rule in fw.get("rules", []):
            if rule.get("direction") != "in":
                continue
            sources = ", ".join(rule.get("source_ips", [])) or "?"
            proto = rule.get("protocol", "?")
            port = rule.get("port") or "any"
            result.allow_table.append(f"{fw_label}: {proto} {port} ← {sources}")
            # (b) no inbound rule may cover an internal-only port — from ANY
            # source (these ports have no legitimate off-box caller).
            hit = [p for p in FORBIDDEN_PORTS if _rule_covers_port(rule, p)]
            if hit:
                result.violations.append(
                    f"{fw_label} rule ({proto} {port} ← {sources}) opens "
                    f"internal-only port(s) {hit}"
                )

    # (a), tightened: an all-pending posture has NOT confirmed the
    # provider-side filter is live — a mid-detach/re-attach or mid-apply race
    # must not silently read as PASS. A real violation (above) still outranks
    # this — a bad rule is worse than "can't verify yet" even if pending.
    if not any_confirmed_applied and not result.violations:
        result.cannot_verify_reason = (
            f"all {len(applied_firewalls)} attached firewall(s) are apply-status "
            "PENDING — cannot verify the provider-side filter is live yet"
        )
    return result


# ── Token / DNS / API plumbing ───────────────────────────────────────────────


def _read_token(host: str, path: str | None = None) -> str | None:
    """The API token for host's cloud project (`cloud_project.token_for`:
    $HETZNER_API_TOKEN, else the project's token file), or — with an explicit
    `path` — that file's first non-empty line. None when there is none, or when
    the project's file is readable by other users (said on stderr)."""
    if path is None:
        try:
            return cloud_project.token_for(host) or None
        except cloud_project.TokenFileModeError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return None
    try:
        with open(os.path.expanduser(path), encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    return line
    except OSError:
        pass
    return None


def _resolve_ips(host: str) -> set[str]:
    """All public A/AAAA addresses of `host`."""
    ips: set[str] = set()
    for _family, _type, _proto, _canon, sockaddr in socket.getaddrinfo(host, None):
        ips.add(sockaddr[0])
    return ips


def _api_get_json(url: str, token: str, timeout: float) -> dict:
    """One GET against the Cloud API. Raises _TransientApiError on 5xx or a
    network-level failure (both retryable); other HTTP errors propagate."""
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        if e.code >= 500:
            raise _TransientApiError(f"{e.code} from API: {url}") from e
        raise
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise _TransientApiError(f"network error against {url}: {e}") from e


def _get_paged(path: str, key: str, token: str, timeout: float = 30.0, retries: int = 3) -> list[dict]:
    """Paged GET (Hetzner meta.pagination), retrying each page's transient
    failures with backoff (a bare 503 has been observed to clear on retry)."""
    out: list[dict] = []
    page = 1
    while True:
        url = f"{HETZNER_API}{path}?per_page=50&page={page}"
        last_err: Exception | None = None
        for attempt in range(retries):
            try:
                body = _api_get_json(url, token, timeout)
                last_err = None
                break
            except _TransientApiError as e:
                last_err = e
                if attempt < retries - 1:
                    time.sleep(2.0 * (attempt + 1))
        if last_err is not None:
            raise last_err
        out.extend(body.get(key, []))
        nxt = ((body.get("meta") or {}).get("pagination") or {}).get("next_page")
        if not nxt:
            return out
        page = nxt


# ── Gate ─────────────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Deploy-verify gate G1-cloud: the provider-side firewall safety invariant.",
    )
    ap.add_argument("host", help="nest host (dev.fauna.build / https://dev.fauna.build)")
    ap.add_argument(
        "--token-file",
        default=None,
        help="Hetzner Cloud API token file (default: $HETZNER_API_TOKEN, else the "
        "host's cloud project token file — scripts/_cloud_project.py)",
    )
    ap.add_argument(
        "--warn-only",
        action="store_true",
        help="report violations but always exit 0 (advisory)",
    )
    ap.add_argument("--timeout", type=float, default=30.0, help="API timeout per request (s)")
    ap.add_argument("--retries", type=int, default=3, help="attempts per API page on transient 5xx")
    args = ap.parse_args()

    host = _normalize_host(args.host)
    print(f"== GATE G1-cloud: provider firewall safety invariant — {host} ==")

    token = _read_token(host, args.token_file)
    if token is None:
        where = (f"create {args.token_file} (one line, the Cloud API token)"
                 if args.token_file else cloud_project.where_to_put(host))
        print(f"GATE G1-cloud ERROR: no API token — {where}.", file=sys.stderr)
        return 3

    try:
        ips = _resolve_ips(host)
    except OSError as e:
        print(f"GATE G1-cloud ERROR: cannot resolve {host}: {e}", file=sys.stderr)
        return 2
    if not ips:
        print(f"GATE G1-cloud ERROR: {host} resolves to no addresses", file=sys.stderr)
        return 2

    try:
        servers = _get_paged("/servers", "servers", token,
                             timeout=args.timeout, retries=args.retries)
        firewalls = _get_paged("/firewalls", "firewalls", token,
                               timeout=args.timeout, retries=args.retries)
    # HTTPError is an OSError subclass, so this covers non-5xx API errors,
    # exhausted-retry transients, and any raw network failure alike.
    except (_TransientApiError, OSError) as e:
        print(f"GATE G1-cloud ERROR: Cloud API error: {e}", file=sys.stderr)
        return 2

    server = _find_server_by_ips(servers, ips)
    if server is None:
        print(
            f"GATE G1-cloud ERROR: no server in this token's project has the "
            f"public IP(s) of {host} ({sorted(ips)}) — wrong token/project, or "
            "the box moved.",
            file=sys.stderr,
        )
        return 2
    print(f"  server: {server.get('name')!r} (id {server.get('id')})")

    # Which firewalls are applied to THIS server + their apply status. The
    # server object's own list covers label-selector applications too.
    statuses = {
        f.get("id"): f.get("status")
        for f in (server.get("public_net") or {}).get("firewalls", [])
    }
    applied = [fw for fw in firewalls if fw.get("id") in statuses]

    result = _evaluate_posture(applied, statuses)

    for line in result.allow_table:
        print(f"  inbound: {line}")
    for w in result.warnings:
        print(f"  ⚠ {w}", file=sys.stderr)
    for v in result.violations:
        print(f"  ❌ {v}", file=sys.stderr)

    if result.violations:
        verdict = "WARN" if args.warn_only else "FAIL"
        print(
            f"GATE G1-cloud: {verdict} — {len(result.violations)} violation(s) of the "
            "cloud-firewall safety invariant (see ❌ above).",
            file=sys.stderr,
        )
        return 0 if args.warn_only else 1

    if result.cannot_verify_reason:
        print(f"GATE G1-cloud ERROR: {result.cannot_verify_reason}", file=sys.stderr)
        return 2

    print(
        f"GATE G1-cloud: PASS — {len(applied)} firewall(s) applied, no rule "
        f"opens an internal-only port {list(FORBIDDEN_PORTS)}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
