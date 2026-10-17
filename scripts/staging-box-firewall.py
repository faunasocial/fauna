#!/usr/bin/env -S uv run --quiet --no-project
"""Give a box its OWN Hetzner Cloud firewall, as a copy of another box's.

WHY THIS EXISTS: a box the app provisions is public — the app sets up no
provider-side firewall. A kept box is meant to sit behind the same allowlist
a reference box has, but as a SEPARATE firewall resource, so that either
box's rules can later be changed — or the reference box deleted — without
touching the other. Attaching the reference box's own firewall to both would
tie them together; this copies the rules instead. dev.fauna.build is the
reference new boxes copy from (its own rules were copied from the box it
replaced, in the other cloud project — the reason the two hosts may sit in
different projects).

WHAT IT DOES (talks only to api.hetzner.cloud; no SSH):
  1. Reads the Cloud API token of each host's project ($HETZNER_API_TOKEN,
     else that project's token file — scripts/_cloud_project.py; one
     `--token-file` serves both hosts).
  2. Resolves each host to a server in its token's project by public IP.
  3. Reads the rules of every firewall applied to the reference server.
  4. Creates ONE new firewall holding a copy of those rules, applied to the
     target server only, and waits until the provider reports it applied.

It only ever CREATES. It refuses when a firewall of the chosen name already
exists (rerunning never overwrites rules someone has since edited), and it
never modifies or deletes any firewall, the reference's least of all.
`--dry-run` stops after step 3 and prints the plan without writing anything.

⚠ ORDER MATTERS on a fresh box: if the reference admits nobody on port 80, the
copy closes it on the target too, and a nest obtains and renews its certificate
over HTTP-01 on that port. Run this only AFTER the box serves its trusted
certificate; from then on its renewals are the DNS-01 path
(docs/goal/architecture/nest/tls-certificates.md § B).

Usage:
    scripts/staging-box-firewall.py test.fauna.build --copy-from dev.fauna.build --dry-run
    scripts/staging-box-firewall.py test.fauna.build --copy-from dev.fauna.build
    scripts/staging-box-firewall.py test.fauna.build --copy-from dev.fauna.build --name test-uat

Exit codes:
    0  created and applied (or --dry-run printed the plan)
    1  refused — a firewall of that name exists, or the target already has one
    2  cannot proceed — API/DNS error, a host is not among the token's servers,
       the reference has no firewall to copy, or the apply did not confirm
    3  no token

Dependencies: Python stdlib only. Shares the read plumbing of
scripts/deploy-verify-cloud-firewall.py (the gate that checks the result).
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

_GATE_PATH = Path(__file__).resolve().parent / "deploy-verify-cloud-firewall.py"
_spec = importlib.util.spec_from_file_location("deploy_verify_cloud_firewall", _GATE_PATH)
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

# The keys a rule may carry on create. Anything else the API returns on a read
# is dropped rather than echoed back.
_RULE_KEYS = ("direction", "protocol", "port", "source_ips", "destination_ips", "description")


# ── Pure helpers (unit-tested in tests/e2e-unified/tests/) ───────────────────


def copy_rules(firewalls: list[dict]) -> list[dict]:
    """The rules of ``firewalls``, in order, in the shape the create call
    takes: known keys only, and no null/empty ``port`` / ``description`` /
    address list (the API rejects a null port on a portless protocol and an
    empty address list on the wrong direction). Exact duplicates — the same
    rule on two of the reference's firewalls — are emitted once."""
    out: list[dict] = []
    for fw in firewalls:
        for rule in fw.get("rules", []):
            copied = {k: rule[k] for k in _RULE_KEYS if rule.get(k) not in (None, "", [])}
            if copied not in out:
                out.append(copied)
    return out


def default_name(server: dict) -> str:
    """The new firewall's name when none is given: the target server's own."""
    return str(server.get("name") or f"server-{server.get('id')}")


def describe(rules: list[dict]) -> list[str]:
    lines = []
    for r in rules:
        arrow, ips = ("←", r.get("source_ips")) if r.get("direction") == "in" \
            else ("→", r.get("destination_ips"))
        lines.append(
            f"{r.get('direction', '?'):>3} {r.get('protocol', '?')} "
            f"{r.get('port') or 'any'} {arrow} {', '.join(ips or []) or '?'}"
        )
    return lines


def internal_ports_opened(rules: list[dict]) -> list[int]:
    """Internal-only ports the copy would open — the set the gate reds on."""
    return [p for p in gate.FORBIDDEN_PORTS if any(gate._rule_covers_port(r, p) for r in rules)]


# ── API plumbing ─────────────────────────────────────────────────────────────


def _server_for(host: str, servers: list[dict]) -> dict | None:
    return gate._find_server_by_ips(servers, gate._resolve_ips(host))


def _applied_firewall_ids(server: dict) -> set:
    return {f.get("id") for f in (server.get("public_net") or {}).get("firewalls", [])}


def _post_json(path: str, body: dict, token: str, timeout: float) -> dict:
    req = urllib.request.Request(
        f"{gate.HETZNER_API}{path}",
        data=json.dumps(body).encode("utf-8"),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Create a box's own cloud firewall as a copy of another box's.",
    )
    ap.add_argument("host", help="the box to protect (test.fauna.build / https://test.fauna.build)")
    ap.add_argument("--copy-from", required=True, metavar="HOST",
                    help="the box whose applied firewall rules are copied")
    ap.add_argument("--name", help="name of the new firewall (default: the target server's name)")
    ap.add_argument("--dry-run", action="store_true", help="print the plan; write nothing")
    ap.add_argument("--token-file", default=None,
                    help="one token file for both hosts (default: $HETZNER_API_TOKEN, else "
                    "each host's cloud project token file)")
    ap.add_argument("--timeout", type=float, default=30.0, help="API timeout per request (s)")
    ap.add_argument("--apply-timeout", type=float, default=300.0,
                    help="how long to wait for the provider to report the firewall applied (s)")
    args = ap.parse_args()

    host = gate._normalize_host(args.host)
    ref_host = gate._normalize_host(args.copy_from)
    print(f"== staging-box firewall: {host} ← a copy of {ref_host}'s ==")

    token = gate._read_token(host, args.token_file)
    ref_token = gate._read_token(ref_host, args.token_file)
    for h, tok in ((host, token), (ref_host, ref_token)):
        if tok is None:
            where = (f"create {args.token_file}" if args.token_file
                     else gate.cloud_project.where_to_put(h))
            print(f"ERROR: no API token for {h} — {where}.", file=sys.stderr)
            return 3

    try:
        servers = gate._get_paged("/servers", "servers", token, timeout=args.timeout)
        firewalls = gate._get_paged("/firewalls", "firewalls", token, timeout=args.timeout)
        if ref_token == token:
            ref_servers, ref_firewalls = servers, firewalls
        else:  # the reference lives in another project
            ref_servers = gate._get_paged("/servers", "servers", ref_token, timeout=args.timeout)
            ref_firewalls = gate._get_paged("/firewalls", "firewalls", ref_token,
                                            timeout=args.timeout)
        target = _server_for(host, servers)
        reference = _server_for(ref_host, ref_servers)
    except (gate._TransientApiError, OSError) as e:
        print(f"ERROR: cannot read the current state: {e}", file=sys.stderr)
        return 2
    for label, h, server in (("target", host, target), ("reference", ref_host, reference)):
        if server is None:
            print(f"ERROR: the {label} host {h} is not a server in this token's project.",
                  file=sys.stderr)
            return 2
    if ref_token == token and target.get("id") == reference.get("id"):
        print("ERROR: the target and the reference are the same server.", file=sys.stderr)
        return 2
    print(f"  target:    {target.get('name')!r} (id {target.get('id')})")
    print(f"  reference: {reference.get('name')!r} (id {reference.get('id')})")

    ref_ids = _applied_firewall_ids(reference)
    rules = copy_rules([fw for fw in ref_firewalls if fw.get("id") in ref_ids])
    if not rules:
        print("ERROR: the reference server has no cloud-firewall rule to copy.", file=sys.stderr)
        return 2
    name = args.name or default_name(target)
    print(f"  new firewall {name!r}, {len(rules)} rule(s):")
    for line in describe(rules):
        print(f"    {line}")
    opened = internal_ports_opened(rules)
    if opened:
        print(f"  ⚠ the copy opens internal-only port(s) {opened} — the reference does too; "
              "deploy-verify-cloud-firewall.py reds on this.", file=sys.stderr)

    if any(fw.get("name") == name for fw in firewalls):
        print(f"REFUSED: a firewall named {name!r} already exists — this script only "
              "creates, so that its rules are never overwritten. Pick another --name, or "
              "edit that firewall directly.", file=sys.stderr)
        return 1
    if _applied_firewall_ids(target):
        print(f"REFUSED: {host} already has cloud firewall(s) "
              f"{sorted(_applied_firewall_ids(target))} applied.", file=sys.stderr)
        return 1

    if args.dry_run:
        print("DRY RUN — nothing was created.")
        return 0

    try:
        created = _post_json(
            "/firewalls",
            {
                "name": name,
                "rules": rules,
                "labels": {"copied-from": str(reference.get("name") or reference.get("id"))},
                "apply_to": [{"type": "server", "server": {"id": target["id"]}}],
            },
            token, args.timeout,
        )
    except urllib.error.HTTPError as e:
        print(f"ERROR: the provider refused the create ({e.code}): "
              f"{e.read().decode('utf-8', 'replace')[:600]}", file=sys.stderr)
        return 2
    except OSError as e:
        print(f"ERROR: the create did not go through: {e}", file=sys.stderr)
        return 2
    fw_id = (created.get("firewall") or {}).get("id")
    print(f"  created firewall {fw_id}; waiting for the provider to apply it …")

    deadline = time.monotonic() + args.apply_timeout
    status = None
    while time.monotonic() < deadline:
        try:
            body = gate._api_get_json(
                f"{gate.HETZNER_API}/servers/{target['id']}", token, args.timeout
            )
            status = {
                f.get("id"): f.get("status")
                for f in ((body.get("server") or {}).get("public_net") or {}).get("firewalls", [])
            }.get(fw_id)
        except (gate._TransientApiError, OSError):
            status = None
        if status == "applied":
            print(f"DONE: firewall {fw_id} ({name!r}) is applied to {host}.")
            return 0
        time.sleep(5)
    print(f"ERROR: firewall {fw_id} was created but the provider did not report it applied "
          f"within {args.apply_timeout:.0f}s (last status: {status!r}).", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
