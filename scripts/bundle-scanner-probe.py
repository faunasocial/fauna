#!/usr/bin/env python3
"""bundle-scanner-probe.py — the release pipeline's gate on the content-scan sidecars.

Run as ``uv run --no-project python scripts/bundle-scanner-probe.py`` (stdlib
only; the repo never invokes a bare ``python3``).

Why this exists
---------------
The mail bridge's scan gate is **default-on and fail-closed**: an unreachable
clamd or rspamd 451s every inbound message, indefinitely
(``docker-compose.yml`` § Content-scan sidecars; ``bins/fauna-bridges/internal/
scan/scan.go``). Until this gate existed, *nothing in the release pipeline
observed whether those daemons serve at all*:

* ``scripts/image-upgrade-smoke.sh`` boots the **nest image alone** via
  ``docker run`` — no compose, no sidecars;
* ``scripts/deploy-verify-serving.sh`` asserts ``/api/v1/health`` plus TLS
  *banners* on the mail ports — it never reaches a DATA phase, so the scan gate
  is never exercised, and ``routes.rs::health`` reports nothing about the
  scanners in any case.

So a broken scanner sidecar reached production silently. That became sharper
once the Fauna-namespace mirror landed (2026-08-29,
``release-integrity.md`` § Third-party container images part 4): the pipeline
now *re-publishes and promotes* the sidecar manifests itself, so it would be
moving :latest onto a bundle whose mail path it had never checked.

What it asserts
---------------
The observable is deliberately **the bridge's own wire**, not a liveness ping:

1. clamd answers ``zINSTREAM`` (the exact framing ``scan.Clamd`` writes) with
   ``stream: OK`` for a clean body, **and**
2. ``FOUND`` for the EICAR test vector — which is what distinguishes a daemon
   that is *serving* from one that merely accepts connections, and
3. rspamd answers ``POST /checkv2`` (the exact call ``scan.Rspamd`` makes) with
   HTTP 200 and a JSON body carrying a ``score``.

Both verdict directions are asserted for clamd on purpose: a daemon that
answers ``FOUND`` to everything would 550 every legitimate message and is just
as broken as one that answers nothing.

Anti-vacuity
------------
A probe that passes when the scanners are *absent* converts an unknown into a
false assurance, which is worse than no probe. So the required services must be
present in the compose file, and their absence is a FAIL — never a skip. The
home bundle (``docker-compose.home.yml``) legitimately ships no scanners
because the home box terminates no SMTP; it is simply never passed to this
script, rather than being special-cased into a pass.

The sidecars publish no ports in the shipped bundle (they sit on the internal
compose network by design). The probe never changes that: it layers a
throwaway override that publishes ephemeral **loopback-only** ports for the
duration of the run, and the shipped file is read-only here.

Usage
-----
    uv run --no-project python scripts/bundle-scanner-probe.py \\
        [--compose-file docker-compose.yml] \\
        [--mirror-tag sha-<sha>] [--image clamd=<ref>] \\
        [--timeout 900] [--keep-up]

Bare, it probes the refs the shipped bundle names. ``--mirror-tag`` probes the
Fauna-namespace *mirror* at that tag instead — which repo each service maps to
is joined from the vetted table through ``vetted_images.py
--print-mirror-images``, the same interface ``mirror-third-party-images.sh``
reads, so no second list of images exists. ``--image`` overrides one service
explicitly and wins over both.

Exit 0 = GATE PASS (the bundle's scanners serve). Exit 1 = GATE FAIL.
Exit 2 = usage.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# The two services this gate exists for. Named here rather than discovered, so
# that *deleting* a scanner from the bundle reddens the gate instead of quietly
# shrinking its coverage.
CLAMD_SERVICE = "clamd"
RSPAMD_SERVICE = "rspamd"
CLAMD_PORT = 3310
RSPAMD_PORT = 11333

# No host-side signature cache, deliberately. Mounting a persisted
# /var/lib/clamav would shave a little off each run and buy two problems: two
# sessions probing at once would have their freshclams fighting over one
# volume, and — the reason that settles it — a warm cache can make a scanner
# look healthy whose IMAGE ships a broken or empty database, which is exactly
# the assurance this gate is supposed to give. Every run therefore starts from
# the image's own state and nothing else. (Detection *currency* is a separate
# runtime data plane freshclam owns over ClamAV's signature-verified channel —
# release-integrity.md § Third-party container images part 3 — not this gate's
# question.)

# A generous ceiling, not a tuned delay (e2e convention 14: assert
# latency-independent state, never wall-clock timing). clamd loads its full
# signature database before it binds; on a busy shared runner that can be
# minutes. A green run pays only the time the daemons actually take.
DEFAULT_TIMEOUT_S = 900
POLL_INTERVAL_S = 3

# clamd's INSTREAM framing, mirrored from `scan.Clamd`
# (bins/fauna-bridges/internal/scan/scan.go): "zINSTREAM\0", then repeated
# <uint32-BE len><chunk>, then a zero-length frame; reply is one
# null-terminated line.
CLAMD_CHUNK = 8192
MAX_CLAMD_REPLY = 4096

CLEAN_BODY = (
    b"From: probe@example.invalid\r\n"
    b"To: probe@example.invalid\r\n"
    b"Subject: bundle scanner probe\r\n"
    b"\r\n"
    b"This message is clean.\r\n"
)


def eicar() -> bytes:
    """The EICAR anti-malware test vector, assembled at run time.

    Deliberately never written as one literal in a tracked file: dev boxes run
    real antivirus, and a checked-in EICAR string gets the *source file*
    quarantined. ``tests/e2e-unified/fakes/fake_clamd.py`` avoids it the same
    way, for the same reason.

    It is not malware — it is the industry-standard 68-byte string every
    scanner is required to report, and it is what makes this probe impossible
    to satisfy with a stub: Fauna's own fake clamd answers ``OK`` to it.
    """
    return (
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$"
        + b"EICAR" + b"-STANDARD-" + b"ANTIVIRUS-" + b"TEST-FILE!"
        + b"$H+H*"
    )


class GateFail(Exception):
    """A probe verdict of FAIL, with the reason already phrased for the log."""


def log(msg: str) -> None:
    print(f"[scanner-probe] {msg}", flush=True)


def run(argv: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(argv, capture_output=True, text=True, **kw)


def compose_services(compose_file: Path) -> list[str]:
    proc = run(["docker", "compose", "-f", str(compose_file), "config", "--services"])
    if proc.returncode != 0:
        raise GateFail(f"{compose_file} does not parse: {proc.stderr.strip()}")
    return [s.strip() for s in proc.stdout.splitlines() if s.strip()]


def compose_images(compose_file: Path) -> dict[str, str]:
    """Each service's image ref, as the shipped bundle names it."""
    proc = run(["docker", "compose", "-f", str(compose_file), "config", "--format", "json"])
    if proc.returncode != 0:
        raise GateFail(f"{compose_file} does not parse: {proc.stderr.strip()}")
    parsed = json.loads(proc.stdout)
    return {name: svc.get("image", "") for name, svc in parsed["services"].items()}


def fetch_mirror_plan() -> dict[str, str]:
    """The vetted table's mirror worklist: third-party ref -> Fauna mirror repo.

    Read through the installer self-test's ``--print-mirror-images``, the same
    interface ``mirror-third-party-images.sh`` consumes, so which images are
    mirrored has exactly ONE definition — the table a human vetted. Never
    hard-code the repo names here; a second copy is how the probe would end up
    checking a different set than the pipeline publishes.
    """
    proc = run([
        "uv", "run", "--no-project", "python",
        str(REPO_ROOT / "scripts" / "vetted_images.py"), "--print-mirror-images",
    ])
    if proc.returncode != 0:
        raise GateFail(f"could not read the vetted mirror plan: {proc.stderr.strip()}")
    plan: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        if "\t" not in line:
            continue
        repo, source = line.split("\t", 1)
        plan[source.strip()] = repo.strip()
    if not plan:
        raise GateFail("the vetted table produced an EMPTY mirror plan")
    return plan


def repo_of(ref: str) -> str:
    """The repository part of an image ref — no ``:tag``, no ``@digest``."""
    ref = ref.split("@", 1)[0]
    head, sep, tail = ref.rpartition(":")
    # A colon in the registry host (`registry:5000/x`) is not a tag separator.
    return head if sep and "/" not in tail else ref


def resolve_mirror_images(images: dict[str, str], plan: dict[str, str],
                          tag: str) -> dict[str, str]:
    """Map each scanner service onto its mirrored ref at ``tag``.

    Derived by joining the bundle's own image ref to the vetted table, so the
    probe follows a mirror rename or a digest bump automatically.

    Two shapes join, because the bundle is mid-migration: today it names the
    third-party ref, which the table maps to a mirror repo; after part 4's
    bundle flip it will name the mirror repo itself, which needs only
    re-tagging. Anything else is a FAIL rather than a silent fall-back to the
    shipped ref — the pipeline promotes what it mirrors, and probing something
    else would vouch for the wrong bytes.
    """
    mirror_repos = set(plan.values())
    resolved: dict[str, str] = {}
    for service in (CLAMD_SERVICE, RSPAMD_SERVICE):
        ref = images.get(service, "")
        repo = plan.get(ref)
        if not repo and repo_of(ref) in mirror_repos:
            repo = repo_of(ref)
        if not repo:
            raise GateFail(
                f"the bundle's {service} image {ref!r} is neither a vetted "
                "mirror SOURCE nor a Fauna mirror repo, so there is no "
                "mirrored manifest to probe"
            )
        resolved[service] = f"{repo}:{tag}"
    return resolved


def write_override(path: Path, images: dict[str, str]) -> None:
    """A throwaway compose override: loopback port publishing (+ image swaps).

    Everything else — the image refs, rspamd's inline ``worker-normal.inc``
    bind, the logging caps — comes from the shipped file, so what the probe
    exercises is the bundle as users receive it. The publishing is the one
    unavoidable deviation: the sidecars are unpublished by design (internal
    compose network), and a host-side probe needs a door. It is loopback-only
    and lives as long as the run.
    """
    lines = ["services:"]
    for service, port in ((CLAMD_SERVICE, CLAMD_PORT), (RSPAMD_SERVICE, RSPAMD_PORT)):
        lines.append(f"  {service}:")
        if service in images:
            lines.append(f"    image: {images[service]}")
        lines.append("    ports:")
        lines.append(f'      - "127.0.0.1::{port}"')
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def published_port(compose: list[str], service: str, container_port: int) -> int | None:
    proc = run([*compose, "port", service, str(container_port)])
    if proc.returncode != 0:
        return None
    text = proc.stdout.strip()
    if not text or ":" not in text:
        return None
    return int(text.rsplit(":", 1)[1])


def clamd_instream(port: int, payload: bytes, timeout: float = 30.0) -> str:
    """Speak clamd's INSTREAM wire and return its reply line.

    Byte-for-byte the sequence ``scan.Clamd`` writes, so a daemon this probe
    calls healthy is one the bridge can actually use.
    """
    with socket.create_connection(("127.0.0.1", port), timeout=timeout) as sock:
        sock.settimeout(timeout)
        sock.sendall(b"zINSTREAM\x00")
        for off in range(0, len(payload), CLAMD_CHUNK):
            chunk = payload[off : off + CLAMD_CHUNK]
            sock.sendall(struct.pack(">I", len(chunk)) + chunk)
        sock.sendall(struct.pack(">I", 0))
        reply = b""
        while len(reply) <= MAX_CLAMD_REPLY:
            block = sock.recv(MAX_CLAMD_REPLY)
            if not block:
                break
            reply += block
    return reply.rstrip(b"\x00").decode("utf-8", "replace").strip()


def rspamd_checkv2(port: int, payload: bytes, timeout: float = 30.0) -> dict:
    """POST /checkv2 exactly as ``scan.Rspamd`` does; return the parsed JSON."""
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/checkv2",
        data=payload,
        method="POST",
        headers={"Content-Type": "text/plain"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        if resp.status != 200:
            raise GateFail(f"rspamd /checkv2 answered HTTP {resp.status}")
        return json.loads(resp.read().decode("utf-8", "replace"))


def probe_clamd(port: int) -> str:
    """Both verdict directions, or a raised failure describing which broke."""
    clean = clamd_instream(port, CLEAN_BODY)
    if "FOUND" in clean or "ERROR" in clean or "OK" not in clean:
        raise GateFail(
            f"clamd did not scan a CLEAN body clean — replied {clean!r}. "
            "A daemon that condemns everything 550s every legitimate message."
        )
    infected = clamd_instream(port, eicar())
    if "FOUND" not in infected:
        raise GateFail(
            f"clamd did not detect the EICAR test vector — replied {infected!r}. "
            "The daemon answers but is not scanning; every message would pass "
            "unscanned or the signature database never loaded."
        )
    return f"clean={clean!r} eicar={infected!r}"


def probe_rspamd(port: int) -> str:
    body = rspamd_checkv2(port, CLEAN_BODY)
    if "score" not in body:
        raise GateFail(
            f"rspamd /checkv2 returned no score: {json.dumps(body)[:200]}. "
            "The bridge parses this reply and fails closed without it."
        )
    return f"score={body['score']} action={body.get('action')!r}"


def poll_until(compose: list[str], service: str, container_port: int,
               probe, deadline: float) -> str:
    """Bounded deadline poll for one service's wire (convention 14).

    The port mapping itself is polled too: a container that has not started
    yet has no published port, and a container that exited loses it.
    """
    last = "no attempt made"
    while True:
        port = published_port(compose, service, container_port)
        if port is not None:
            try:
                return probe(port)
            except GateFail:
                raise
            except Exception as exc:  # not serving *yet* — keep polling
                last = f"{type(exc).__name__}: {exc}"
        else:
            last = f"{service} publishes no host port yet (not started?)"
        if time.time() >= deadline:
            raise GateFail(
                f"{service} was not serving within the budget — last: {last}"
            )
        time.sleep(POLL_INTERVAL_S)


def dump_logs(compose: list[str]) -> None:
    log("---- docker compose ps ----")
    print(run([*compose, "ps", "-a"]).stdout, flush=True)
    for service in (CLAMD_SERVICE, RSPAMD_SERVICE):
        log(f"---- docker compose logs {service} (tail) ----")
        proc = run([*compose, "logs", "--tail", "60", service])
        print(proc.stdout or proc.stderr, flush=True)


def parse_images(pairs: list[str]) -> dict[str, str]:
    images: dict[str, str] = {}
    for pair in pairs:
        if "=" not in pair:
            raise SystemExit(f"--image wants <service>=<ref>, got {pair!r}")
        service, ref = pair.split("=", 1)
        if service not in (CLAMD_SERVICE, RSPAMD_SERVICE):
            raise SystemExit(
                f"--image names {service!r}; this gate covers "
                f"{CLAMD_SERVICE} and {RSPAMD_SERVICE}"
            )
        images[service] = ref
    return images


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="bundle-scanner-probe.py",
        description="Assert the bundle's content-scan sidecars actually serve.",
    )
    parser.add_argument("--compose-file", default=str(REPO_ROOT / "docker-compose.yml"))
    parser.add_argument("--image", action="append", default=[],
                        metavar="SERVICE=REF",
                        help="override one service's image explicitly")
    parser.add_argument("--mirror-tag", default=None, metavar="TAG",
                        help="probe the Fauna-namespace MIRRORED manifests at "
                             "TAG (e.g. sha-<sha>), resolved from the vetted "
                             "table rather than named here; --image wins per "
                             "service")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT_S)
    parser.add_argument("--keep-up", action="store_true",
                        help="leave the containers running (debugging)")
    parser.add_argument("--project", default=None,
                        help="compose project name (default: a unique one)")
    args = parser.parse_args(argv)

    if shutil.which("docker") is None:
        log("GATE FAIL: docker is not on PATH")
        return 1

    compose_file = Path(args.compose_file).resolve()
    if not compose_file.is_file():
        log(f"GATE FAIL: no such compose file: {compose_file}")
        return 1

    try:
        images = parse_images(args.image)
    except SystemExit as exc:
        log(f"usage: {exc}")
        return 2

    project = args.project or f"scanprobe-{os.getpid()}-{int(time.time())}"
    tmpdir = Path(tempfile.mkdtemp(prefix="scanner-probe-"))
    override = tmpdir / "probe-override.yml"
    compose = ["docker", "compose", "-p", project,
               "-f", str(compose_file), "-f", str(override)]
    started = False

    try:
        # Anti-vacuity: the services must be there. Absent is a FAIL.
        services = compose_services(compose_file)
        missing = [s for s in (CLAMD_SERVICE, RSPAMD_SERVICE) if s not in services]
        if missing:
            raise GateFail(
                f"{compose_file.name} declares no {', '.join(missing)} service — "
                "this gate covers a mail-capable bundle, and a silent pass here "
                "would convert an unknown into a false assurance. (The home "
                "bundle ships no scanners by design and is not probed.)"
            )

        if args.mirror_tag:
            mirrored = resolve_mirror_images(
                compose_images(compose_file), fetch_mirror_plan(), args.mirror_tag
            )
            # An explicit --image stays authoritative: the tests induce their
            # defects that way, and a session debugging one ref should not have
            # to opt out of the whole mirror resolution.
            images = {**mirrored, **images}

        write_override(override, images)
        for service, ref in sorted(images.items()):
            log(f"image override: {service} -> {ref}")
        log(f"booting {CLAMD_SERVICE} + {RSPAMD_SERVICE} from {compose_file.name} "
            f"(project {project}, budget {args.timeout}s)")

        up = run([*compose, "up", "-d", CLAMD_SERVICE, RSPAMD_SERVICE])
        started = True
        if up.returncode != 0:
            raise GateFail(
                "the sidecars could not be started. A manifest that does not "
                "cover this platform ('no matching manifest for linux/...'), "
                "and a mirrored tag this run never published ('manifest "
                "unknown'), both surface here:\n" + up.stderr.strip()
            )

        deadline = time.time() + args.timeout
        rspamd_verdict = poll_until(compose, RSPAMD_SERVICE, RSPAMD_PORT,
                                    probe_rspamd, deadline)
        log(f"{RSPAMD_SERVICE} serves /checkv2: {rspamd_verdict}")
        clamd_verdict = poll_until(compose, CLAMD_SERVICE, CLAMD_PORT,
                                   probe_clamd, deadline)
        log(f"{CLAMD_SERVICE} serves INSTREAM: {clamd_verdict}")

    except GateFail as exc:
        log(f"GATE FAIL: {exc}")
        if started:
            dump_logs(compose)
        return 1
    finally:
        if started and not args.keep_up:
            run([*compose, "down", "--remove-orphans", "--timeout", "10"])
        shutil.rmtree(tmpdir, ignore_errors=True)

    log(f"GATE PASS: {compose_file.name}'s scan-gate dependencies serve "
        f"the wire the mail bridge dials")
    return 0


if __name__ == "__main__":
    sys.exit(main())
