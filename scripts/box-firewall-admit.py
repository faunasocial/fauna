#!/usr/bin/env -S uv run --quiet --no-project
"""Admit one CI run to a firewalled box, for that run only, from its own address only.

WHY THIS EXISTS: a staging box sits behind its own Hetzner Cloud firewall,
which admits its serving ports from a short allowlist. The image pipeline's
verify job runs on a GitHub-hosted runner whose public address changes with
every run, so it cannot be on that allowlist — yet it has to observe the box
from outside (health, the serving gate, the TLS banners). This script opens
the box to exactly that runner, on exactly the verify ports, for exactly the
run, and closes it again (docs/goal/architecture/image-pipeline.md § Image tags
& channels → *The two pipelines on GitHub-hosted runners*, part 2).

THE SUBCOMMANDS (talk only to api.hetzner.cloud, the echo services and, for
`sweep`, api.github.com; no SSH):
  admit  <host> --run-id N   learns this machine's public IPv4 address from
         independent echo services (they must agree), then adds one inbound
         rule per verify port (tcp 443, 993, 465, 587) whose only source is
         that /32, each described `gh-verify <owner/repo>#<run-id> <stamp>`.
  revoke <host> --run-id N   removes exactly that run's rules — and nothing
         else. Run it in an `always()` step.
  sweep  <host>              removes any `gh-verify` rule whose run has
         completed (asked of the GitHub API) or whose stamp is older than
         `--older-than` (default 2h), so a killed job leaves nothing open.
         `--run-id N` exempts the calling run's own rules.
  held   <host> [--run-id N] writes nothing: lists the `gh-verify` rules the
         firewall holds — only that run's, with `--run-id` — and exits 1 when
         there is one. A run's last step asserts with it, after `revoke`, that
         it left nothing open.

Every write is a read-modify-write of the box's ONE applied firewall
(`set_rules` replaces the whole set): the script reads the rules, changes only
`gh-verify` rules, writes them back, waits for the provider's action to finish
and re-reads to confirm, retrying when a concurrent writer clobbered the
change. Every rule that is not a `gh-verify` rule is written back unchanged.

THE TOKEN, AND ITS SELF-TEST: the provider has no firewall-only scope, so the
token is read+write on its whole project. It is read from $HETZNER_API_TOKEN
(how CI hands the job its one token), else — on a dev host — from the token
file of the box's own cloud project (scripts/_cloud_project.py), never from a
file kept for another project. Before
anything else, every subcommand lists the token's servers and REFUSES (exit 4)
when any `--refuse-if-lists` host is among them: a token that reaches a box CI
must never reach (a production front door, say) was placed in CI by mistake,
and is caught before a single firewall write.

Usage (in a workflow; GITHUB_REPOSITORY and GITHUB_RUN_ID are set by Actions):
    scripts/box-firewall-admit.py sweep  staging.example.org --refuse-if-lists example.org --run-id "$GITHUB_RUN_ID"
    scripts/box-firewall-admit.py admit  staging.example.org --refuse-if-lists example.org --run-id "$GITHUB_RUN_ID"
    scripts/box-firewall-admit.py revoke staging.example.org --refuse-if-lists example.org --run-id "$GITHUB_RUN_ID"
    scripts/box-firewall-admit.py held   staging.example.org --refuse-if-lists example.org --run-id "$GITHUB_RUN_ID"
    scripts/box-firewall-admit.py admit  staging.example.org --refuse-if-lists example.org --run-id 1 --repo o/r --dry-run

Exit codes:
    0  done (or --dry-run printed the plan); `held`: no admission is held
    1  the write did not take — the re-read still disagrees after the retries;
       `held`: an admission is held
    2  cannot proceed — API/DNS/echo error, the echo services disagree, the box
       is not among the token's servers, or it has no (or several) firewalls
    3  no token ($HETZNER_API_TOKEN unset and no token file for the box's project)
    4  refused — the token lists a `--refuse-if-lists` host

Dependencies: Python stdlib only. Shares the read plumbing of
scripts/deploy-verify-cloud-firewall.py (the gate that checks the box's
posture) and the
rule shape of scripts/staging-box-firewall.py.
"""

from __future__ import annotations

import argparse
import importlib.util
import ipaddress
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import NamedTuple


def _load_sibling(stem: str, module_name: str):
    path = Path(__file__).resolve().parent / f"{stem}.py"
    spec = importlib.util.spec_from_file_location(module_name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gate = _load_sibling("deploy-verify-cloud-firewall", "deploy_verify_cloud_firewall")
sbf = _load_sibling("staging-box-firewall", "staging_box_firewall")

# The ports the verify job observes: 443 for health and the serving gate,
# 993/465/587 for the TLS banners.
VERIFY_PORTS = (443, 993, 465, 587)

TAG = "gh-verify"
_STAMP_FORMAT = "%Y%m%dT%H%MZ"
_DESCRIPTION_RE = re.compile(
    rf"^{TAG} (?P<repo>[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)#(?P<run_id>\d+) (?P<stamp>\d{{8}}T\d{{4}}Z)$"
)

DEFAULT_ECHO_SERVICES = (
    "https://api.ipify.org",
    "https://checkip.amazonaws.com",
    "https://ipv4.icanhazip.com",
)
GITHUB_API = "https://api.github.com"
_USER_AGENT = "box-firewall-admit"


class RefusedError(Exception):
    """Something this script will not proceed past (exit 2)."""


# ── Pure helpers (unit-tested in tests/e2e-unified/tests/) ───────────────────


class Admission(NamedTuple):
    repo: str
    run_id: str
    stamp: datetime


def admission_description(repo: str, run_id: str, now: datetime) -> str:
    """The `description` every rule of one run's admission carries."""
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise RefusedError(f"not an owner/repo name: {repo!r}")
    if not re.fullmatch(r"\d+", str(run_id)):
        raise RefusedError(f"not a run id: {run_id!r}")
    return f"{TAG} {repo}#{run_id} {now.astimezone(timezone.utc).strftime(_STAMP_FORMAT)}"


def parse_admission(description: str | None) -> Admission | None:
    """The admission a rule's description names, or None for any other rule."""
    m = _DESCRIPTION_RE.match(description or "")
    if not m:
        return None
    stamp = datetime.strptime(m["stamp"], _STAMP_FORMAT).replace(tzinfo=timezone.utc)
    return Admission(repo=m["repo"], run_id=m["run_id"], stamp=stamp)


def admission_rules(address: str, repo: str, run_id: str, now: datetime) -> list[dict]:
    """One inbound tcp rule per verify port, sourced from ``address`` alone."""
    ip = ipaddress.ip_address(address)
    source = f"{ip}/{ip.max_prefixlen}"
    description = admission_description(repo, run_id, now)
    return [
        {"direction": "in", "protocol": "tcp", "port": str(port),
         "source_ips": [source], "description": description}
        for port in VERIFY_PORTS
    ]


def is_run_rule(rule: dict, repo: str, run_id: str) -> bool:
    a = parse_admission(rule.get("description"))
    return a is not None and a.repo == repo and a.run_id == str(run_id)


def without(rules: list[dict], drop) -> list[dict]:
    """``rules`` minus every rule ``drop(rule)`` is true for, order kept."""
    return [r for r in rules if not drop(r)]


def agreed_address(answers: list[str | None]) -> str:
    """The one public IPv4 address the echo services agree on.

    ``answers`` holds each service's raw reply (None for a service that did
    not answer). At least two must answer, every answer must be the same
    global IPv4 address — a disagreement means a proxy or a hijacked reply
    sits in the path, and admitting either address would be a guess."""
    got: list[str] = []
    for raw in answers:
        if raw is None:
            continue
        text = raw.strip()
        try:
            ip = ipaddress.ip_address(text)
        except ValueError:
            raise RefusedError(f"an echo service answered something that is not an address: {text[:60]!r}")
        if ip.version != 4 or not ip.is_global:
            raise RefusedError(f"an echo service answered {ip}, not a public IPv4 address")
        got.append(str(ip))
    if len(got) < 2:
        raise RefusedError(f"only {len(got)} echo service(s) answered; two must agree")
    if len(set(got)) != 1:
        raise RefusedError(f"the echo services disagree: {sorted(set(got))}")
    return got[0]


def parse_duration(text: str) -> timedelta:
    """`90m`, `2h`, `1d` → a timedelta."""
    m = re.fullmatch(r"(\d+)([mhd])", text.strip())
    if not m:
        raise argparse.ArgumentTypeError(f"not a duration like 90m / 2h / 1d: {text!r}")
    n = int(m[1])
    return {"m": timedelta(minutes=n), "h": timedelta(hours=n), "d": timedelta(days=n)}[m[2]]


def stale_admissions(rules: list[dict], now: datetime, older_than: timedelta,
                     run_status, keep_run: tuple[str, str] | None = None) -> set[tuple[str, str]]:
    """The (repo, run_id) admissions a sweep removes.

    One is stale when its stamp is older than ``older_than``, or when
    ``run_status(repo, run_id)`` reports the run `completed`. Any other answer
    — `in_progress`, `queued`, None for "could not ask" — keeps a young
    admission: the age window is the backstop. ``keep_run`` is never swept."""
    seen: dict[tuple[str, str], datetime] = {}
    for r in rules:
        a = parse_admission(r.get("description"))
        if a is not None:
            key = (a.repo, a.run_id)
            seen[key] = min(seen.get(key, a.stamp), a.stamp)
    stale = set()
    for key, stamp in seen.items():
        if key == keep_run:
            continue
        if now - stamp > older_than or run_status(*key) == "completed":
            stale.add(key)
    return stale


def listed_hosts(servers: list[dict], host_ips: dict[str, set[str]]) -> list[str]:
    """The hosts (of ``host_ips``) whose address belongs to one of ``servers``."""
    return [h for h, ips in host_ips.items() if gate._find_server_by_ips(servers, ips)]


# ── API plumbing ─────────────────────────────────────────────────────────────


def _api_post(path: str, body: dict, token: str, timeout: float) -> dict:
    req = urllib.request.Request(
        f"{gate.HETZNER_API}{path}",
        data=json.dumps(body).encode("utf-8"),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _fetch_text(url: str, timeout: float) -> str | None:
    req = urllib.request.Request(url, headers={"User-Agent": _USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.read(256).decode("ascii", "replace")
    except (urllib.error.URLError, TimeoutError, OSError):
        return None


def _github_run_status(repo: str, run_id: str, timeout: float) -> str | None:
    """`completed` / `in_progress` / `queued` / … — None when it cannot be asked
    (a private repository without $GITHUB_TOKEN, a network error)."""
    headers = {"User-Agent": _USER_AGENT, "Accept": "application/vnd.github+json"}
    gh_token = os.environ.get("GITHUB_TOKEN", "").strip()
    if gh_token:
        headers["Authorization"] = f"Bearer {gh_token}"
    req = urllib.request.Request(f"{GITHUB_API}/repos/{repo}/actions/runs/{run_id}", headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8")).get("status")
    except (urllib.error.URLError, TimeoutError, OSError, ValueError):
        return None


def _box_firewall(host: str, servers: list[dict], token: str, timeout: float) -> dict:
    """The one firewall applied to host's server, read fresh."""
    server = gate._find_server_by_ips(servers, gate._resolve_ips(host))
    if server is None:
        raise RefusedError(f"{host} is not a server in this token's project")
    ids = sorted({f.get("id") for f in (server.get("public_net") or {}).get("firewalls", [])})
    if len(ids) != 1:
        raise RefusedError(
            f"{host} has {len(ids)} cloud firewalls applied ({ids}); admission edits exactly one"
        )
    return _read_firewall(ids[0], token, timeout)


def _read_firewall(fw_id, token: str, timeout: float) -> dict:
    return gate._api_get_json(f"{gate.HETZNER_API}/firewalls/{fw_id}", token, timeout)["firewall"]


def _set_rules(fw_id, rules: list[dict], token: str, timeout: float, wait: float) -> None:
    """`set_rules` and wait for every action it starts to finish."""
    body = _api_post(f"/firewalls/{fw_id}/actions/set_rules", {"rules": rules}, token, timeout)
    deadline = time.monotonic() + wait
    for action in body.get("actions", []):
        while True:
            status = gate._api_get_json(
                f"{gate.HETZNER_API}/actions/{action['id']}", token, timeout
            )["action"].get("status")
            if status == "success":
                break
            if status == "error":
                raise RefusedError(f"the provider's action {action['id']} failed")
            if time.monotonic() > deadline:
                raise RefusedError(f"the provider's action {action['id']} did not finish in {wait:.0f}s")
            time.sleep(2)


def _rewrite(fw_id, token: str, args, change, done) -> int:
    """Read-modify-write the firewall's rules until ``done(rules)`` holds."""
    for attempt in range(3):
        current = sbf.copy_rules([_read_firewall(fw_id, token, args.timeout)])
        if done(current):
            return 0
        _set_rules(fw_id, change(current), token, args.timeout, args.apply_timeout)
        if done(sbf.copy_rules([_read_firewall(fw_id, token, args.timeout)])):
            return 0
        print(f"  the re-read disagrees (a concurrent writer?) — retry {attempt + 1}", file=sys.stderr)
    print("ERROR: the change did not take after 3 attempts.", file=sys.stderr)
    return 1


# ── Subcommands ──────────────────────────────────────────────────────────────


def _admit(fw: dict, token: str, args, now: datetime) -> int:
    answers = [_fetch_text(u, args.timeout) for u in args.echo]
    address = agreed_address(answers)
    wanted = admission_rules(address, args.repo, args.run_id, now)
    print(f"  this run's address: {address}; admitting it on tcp {', '.join(map(str, VERIFY_PORTS))}")
    for line in sbf.describe(wanted):
        print(f"    + {line}")
    if args.dry_run:
        print("DRY RUN — nothing was written.")
        return 0

    def mine(rules):
        return [r for r in rules if is_run_rule(r, args.repo, args.run_id)]

    def done(rules):
        return sorted((r["port"], r["source_ips"][0]) for r in mine(rules)) == \
            sorted((r["port"], r["source_ips"][0]) for r in wanted)

    rc = _rewrite(fw["id"], token, args,
                  lambda rules: without(rules, lambda r: is_run_rule(r, args.repo, args.run_id)) + wanted,
                  done)
    if rc == 0:
        print(f"ADMITTED: run {args.repo}#{args.run_id} from {address}.")
    return rc


def _revoke(fw: dict, token: str, args, now: datetime) -> int:
    def drop(r):
        return is_run_rule(r, args.repo, args.run_id)

    present = [r for r in sbf.copy_rules([fw]) if drop(r)]
    for line in sbf.describe(present):
        print(f"    - {line}")
    if not present:
        print(f"REVOKED: run {args.repo}#{args.run_id} holds no rule.")
        return 0
    if args.dry_run:
        print("DRY RUN — nothing was written.")
        return 0
    rc = _rewrite(fw["id"], token, args, lambda rules: without(rules, drop),
                  lambda rules: not any(drop(r) for r in rules))
    if rc == 0:
        print(f"REVOKED: run {args.repo}#{args.run_id}.")
    return rc


def _sweep(fw: dict, token: str, args, now: datetime) -> int:
    keep = (args.repo, str(args.run_id)) if args.run_id else None
    stale = stale_admissions(
        sbf.copy_rules([fw]), now, args.older_than,
        lambda repo, run_id: _github_run_status(repo, run_id, args.timeout),
        keep_run=keep,
    )

    def drop(r):
        a = parse_admission(r.get("description"))
        return a is not None and (a.repo, a.run_id) in stale

    for repo, run_id in sorted(stale):
        print(f"    - {repo}#{run_id}")
    if not stale:
        print("SWEPT: no stale admission.")
        return 0
    if args.dry_run:
        print("DRY RUN — nothing was written.")
        return 0
    rc = _rewrite(fw["id"], token, args, lambda rules: without(rules, drop),
                  lambda rules: not any(drop(r) for r in rules))
    if rc == 0:
        print(f"SWEPT: {len(stale)} stale admission(s).")
    return rc


def held_admissions(rules: list[dict], repo: str | None = None,
                    run_id: str | None = None) -> set[tuple[str, str]]:
    """The (repo, run_id) admissions ``rules`` hold — only that run's when
    ``run_id`` is given."""
    held = set()
    for r in rules:
        a = parse_admission(r.get("description"))
        if a is None:
            continue
        if run_id is not None and (a.repo, a.run_id) != (repo, str(run_id)):
            continue
        held.add((a.repo, a.run_id))
    return held


def _held(fw: dict, token: str, args, now: datetime) -> int:
    held = held_admissions(sbf.copy_rules([fw]), args.repo if args.run_id else None, args.run_id)
    for repo, run_id in sorted(held):
        print(f"    ! {repo}#{run_id}")
    if held:
        print(f"HELD: {len(held)} admission(s) still open.")
        return 1
    print("HELD: none.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Admit one CI run to a firewalled box on the verify ports, then revoke or sweep it.",
    )
    ap.add_argument("command", choices=("admit", "revoke", "sweep", "held"))
    ap.add_argument("host", help="the firewalled box (staging.example.org / https://staging.example.org)")
    ap.add_argument("--refuse-if-lists", action="append", required=True, metavar="HOST",
                    help="refuse to run when the token's project holds this host's server (repeatable)")
    ap.add_argument("--run-id", default=os.environ.get("GITHUB_RUN_ID"),
                    help="the CI run (default $GITHUB_RUN_ID); required by admit/revoke")
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"),
                    help="owner/repo of the run (default $GITHUB_REPOSITORY)")
    ap.add_argument("--older-than", type=parse_duration, default=timedelta(hours=2),
                    help="sweep: the age past which an admission is removed regardless (default 2h)")
    ap.add_argument("--echo", action="append", default=None, metavar="URL",
                    help="admit: an address echo service (repeatable; default: three public ones)")
    ap.add_argument("--dry-run", action="store_true", help="print the plan; write nothing")
    ap.add_argument("--timeout", type=float, default=30.0, help="per-request timeout (s)")
    ap.add_argument("--apply-timeout", type=float, default=120.0,
                    help="how long to wait for the provider to finish a rule change (s)")
    args = ap.parse_args()
    args.echo = args.echo or list(DEFAULT_ECHO_SERVICES)
    if args.command in ("admit", "revoke") and not args.run_id:
        ap.error(f"{args.command} needs --run-id (or $GITHUB_RUN_ID)")
    if (args.command in ("admit", "revoke") or args.run_id) and not args.repo:
        ap.error("--repo (or $GITHUB_REPOSITORY) is required with a run id")

    host = gate._normalize_host(args.host)
    print(f"== box-firewall-admit {args.command}: {host} ==")
    token = gate._read_token(host)
    if not token:
        print(f"ERROR: no API token — {gate.cloud_project.where_to_put(host)}.", file=sys.stderr)
        return 3

    now = datetime.now(timezone.utc)
    try:
        servers = gate._get_paged("/servers", "servers", token, timeout=args.timeout)
        refuse_ips = {gate._normalize_host(h): gate._resolve_ips(gate._normalize_host(h))
                      for h in args.refuse_if_lists}
        reached = listed_hosts(servers, refuse_ips)
        if reached:
            print(f"REFUSED: this token's project holds {', '.join(reached)} — a token that "
                  "reaches it must never be used here. Replace the token.", file=sys.stderr)
            return 4
        fw = _box_firewall(host, servers, token, args.timeout)
        print(f"  firewall {fw.get('name')!r} (id {fw.get('id')})")
        return {"admit": _admit, "revoke": _revoke, "sweep": _sweep,
                "held": _held}[args.command](fw, token, args, now)
    except RefusedError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2
    except urllib.error.HTTPError as e:
        print(f"ERROR: the provider refused ({e.code}): "
              f"{e.read().decode('utf-8', 'replace')[:600]}", file=sys.stderr)
        return 2
    except (gate._TransientApiError, OSError) as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
