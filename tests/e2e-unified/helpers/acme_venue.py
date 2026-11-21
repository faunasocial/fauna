"""The shared certificate-test venue on the Linux dev VM: one pebble ACME authority and
one fake name server, standing, for every machine's e2e harness.

`docs/goal/architecture/e2e-gated-seeds.md` § The ACME directory seed → *The
shared venue* rules it; this module is decisions 2–4 and 8 built:

- **Two containers under docker's ``unless-stopped`` policy** on a network of
  their own, so the daemon brings them back after a reboot. :func:`ensure`
  creates them when absent, starts them when stopped, and recreates both only
  when the hash label they carry (:func:`venue_hash` — the image pins, pebble's
  config, ``fake_dns.py`` and the venue's own addresses and ports) differs from
  the tree's.
- **Everything they read is copied in at creation, never bind-mounted** — the
  config, the minted directory keypair, ``fake_dns.py`` — so no checkout's
  retirement, rebase or scratch cleanup reaches a running venue.
- **Published on fixed ports on exactly two addresses**: the Linux dev VM's loopback and
  its address on the VMs' private network (``helpers/acme_pebble.py``'s
  constants), never ``0.0.0.0``.
- **The directory certificate is minted at each creation**: a throwaway
  authority (``EphemeralCA``) and a leaf naming both published addresses, the
  authority behind the leaf in the chain pebble serves, so a harness reads it
  off the handshake.

Run on the Linux dev VM by ``just acme-venue ensure|status|down`` (its two triggers:
the Linux dev VM's SessionStart hook and the ``acme_pebble`` session fixture through
``acme_pebble.venue_ready``). ``down`` is for the session that changes the
venue, never a routine step. Harness code: it serves a test double on a
host-internal network and nothing in any product reads it.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_E2E = _HERE.parent
if str(_E2E) not in sys.path:
    sys.path.insert(0, str(_E2E))

from helpers.acme_pebble import (  # noqa: E402
    PEBBLE_IMAGE,
    VENUE_DIRECTORY_PORT,
    VENUE_LOOPBACK_ADDR,
    VENUE_NAME_SERVER_PORT,
    VENUE_PRIVATE_ADDR,
    pebble_config,
    wait_tcp,
)

#: The name server's image — the scanner-sidecar image every fake sidecar runs
#: (`tests/platform/docker/helpers.py`'s ``SCANNER_SIDECAR_IMAGE``; the venue
#: gating test pins the two together, so this module imports nothing heavy).
SIDECAR_IMAGE = "python:3.14-slim"
FAKE_DNS = _E2E / "fakes" / "fake_dns.py"
EPHEMERAL_CA = _HERE / "ephemeral_ca.py"

NETWORK = "fauna-acme-venue"
#: A fixed subnet so the name server keeps one address pebble's ``-dnsserver``
#: names across a daemon restart; outside docker's default pools (172.17–) and
#: the VMs' private network.
SUBNET = "172.29.53.0/24"
NAME_SERVER_IP = "172.29.53.53"
PEBBLE_CONTAINER = "fauna-acme-venue-pebble"
DNS_CONTAINER = "fauna-acme-venue-dns"
CONTAINERS = (DNS_CONTAINER, PEBBLE_CONTAINER)
#: The label carrying :func:`venue_hash`. Deliberately NOT the harness's
#: ``fauna-e2e`` label: the venue is no run's, and no dead-run sweep may take it.
HASH_LABEL = "fauna-acme-venue-hash"
#: Bumped by hand when the venue's shape changes in a way the hashed inputs miss
#: (2: both containers join the core fence — its label and CPUs, which docker
#: cannot add to a standing container's labels).
VENUE_FORMAT = 2

_PUBLISHED_ADDRS = (VENUE_LOOPBACK_ADDR, VENUE_PRIVATE_ADDR)
_DNS_SNIPPET = (
    "import sys, time; sys.path.insert(0, '/venue'); "
    "from fake_dns import FakeDns; "
    "FakeDns(host='0.0.0.0', port=53).start(); "
    "print('fake_dns venue ready', flush=True); time.sleep(10**9)"
)


def _venue_pebble_config() -> dict:
    config = pebble_config()
    config["pebble"]["certificate"] = "/venue/cert.pem"
    config["pebble"]["privateKey"] = "/venue/key.pem"
    return config


def venue_hash() -> str:
    """The hash of everything the venue is built from, minted key excepted."""
    material = {
        "format": VENUE_FORMAT,
        "pebble_image": PEBBLE_IMAGE,
        "sidecar_image": SIDECAR_IMAGE,
        "pebble_config": _venue_pebble_config(),
        "fake_dns": hashlib.sha256(FAKE_DNS.read_bytes()).hexdigest(),
        "minting": hashlib.sha256(EPHEMERAL_CA.read_bytes()).hexdigest(),
        "dns_snippet": _DNS_SNIPPET,
        "network": [NETWORK, SUBNET, NAME_SERVER_IP],
        "published": [list(_PUBLISHED_ADDRS), VENUE_DIRECTORY_PORT, VENUE_NAME_SERVER_PORT],
    }
    blob = json.dumps(material, sort_keys=True).encode()
    return hashlib.sha256(blob).hexdigest()[:16]


def _docker(*args: str, timeout: float = 60) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], capture_output=True, text=True, timeout=timeout)


def docker_usable() -> bool:
    if shutil.which("docker") is None:
        return False
    try:
        return _docker("info", "--format", "{{.ServerVersion}}", timeout=30).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def _inspect(name: str) -> dict | None:
    r = _docker("inspect", "--type", "container", name, timeout=30)
    if r.returncode != 0:
        return None
    try:
        return json.loads(r.stdout)[0]
    except (ValueError, IndexError):
        return None


def _state(info: dict | None) -> tuple[bool, bool]:
    """``(current, running)`` of one container's inspect record."""
    if info is None:
        return False, False
    labels = (info.get("Config") or {}).get("Labels") or {}
    return labels.get(HASH_LABEL) == venue_hash(), bool((info.get("State") or {}).get("Running"))


def _publish(host_port: int, container_port: int, *, udp: bool = False) -> list[str]:
    """``-p`` arguments publishing ``container_port`` at ``host_port`` on the
    two venue addresses and nowhere else (decision 3)."""
    args: list[str] = []
    for addr in _PUBLISHED_ADDRS:
        args += ["-p", f"{addr}:{host_port}:{container_port}"]
        if udp:
            args += ["-p", f"{addr}:{host_port}:{container_port}/udp"]
    return args


def _remove_all() -> None:
    for name in CONTAINERS:
        _docker("rm", "-f", name, timeout=60)
    _docker("network", "rm", NETWORK, timeout=30)


def _check(r: subprocess.CompletedProcess, what: str) -> None:
    if r.returncode != 0:
        raise RuntimeError(f"{what} failed: {r.stderr.strip()}")


def _create() -> None:
    """Create both containers from the tree, everything copied in."""
    from helpers.ephemeral_ca import EphemeralCA

    _remove_all()
    _check(_docker("network", "create", "--subnet", SUBNET, NETWORK), "docker network create")
    from tests.platform.docker.helpers import fence_cpuset_args

    # The fence's label and CPUs (core-bound.md § Per-core tokens, the Linux host
    # core-fence bullet): the venue stands for days in docker's system.slice,
    # outside the fence; carrying the label, the core agent moves it with the
    # fence. Not the harness's `fauna-e2e` label: no dead-run sweep may take it.
    label = ["--label", f"{HASH_LABEL}={venue_hash()}", *fence_cpuset_args()]
    with tempfile.TemporaryDirectory(prefix="fauna-acme-venue-") as tmp:
        dns_dir = Path(tmp) / "dns"
        dns_dir.mkdir()
        shutil.copy2(FAKE_DNS, dns_dir / "fake_dns.py")
        _check(_docker(
            "create", "--name", DNS_CONTAINER, "--network", NETWORK, "--ip", NAME_SERVER_IP,
            "--restart", "unless-stopped", *label, *_publish(VENUE_NAME_SERVER_PORT, 53, udp=True),
            SIDECAR_IMAGE, "python", "-c", _DNS_SNIPPET,
        ), "docker create (name server)")
        _check(_docker("cp", str(dns_dir), f"{DNS_CONTAINER}:/venue"), "docker cp (name server)")

        ca = EphemeralCA()
        leaf = ca.issue(VENUE_LOOPBACK_ADDR, VENUE_PRIVATE_ADDR, "localhost")
        peb_dir = Path(tmp) / "pebble"
        peb_dir.mkdir()
        # The authority rides behind the leaf, so a harness reads it off the
        # handshake (decision 4); the key is the leaf's, never the authority's.
        (peb_dir / "cert.pem").write_bytes(leaf.cert_pem + ca.cert_pem)
        (peb_dir / "key.pem").write_bytes(leaf.key_pem)
        (peb_dir / "pebble-config.json").write_text(json.dumps(_venue_pebble_config()))
        _check(_docker(
            "create", "--name", PEBBLE_CONTAINER, "--network", NETWORK,
            "--restart", "unless-stopped", *label, *_publish(VENUE_DIRECTORY_PORT, 14000),
            "-e", "PEBBLE_VA_ALWAYS_VALID=0", "-e", "PEBBLE_WFE_NONCEREJECT=0",
            "-e", "PEBBLE_VA_NOSLEEP=1",
            PEBBLE_IMAGE, "-config", "/venue/pebble-config.json",
            "-dnsserver", f"{NAME_SERVER_IP}:53",
        ), "docker create (pebble)")
        _check(_docker("cp", str(peb_dir), f"{PEBBLE_CONTAINER}:/venue"), "docker cp (pebble)")
    for name in CONTAINERS:
        _check(_docker("start", name), f"docker start {name}")


def _wait_up() -> None:
    for addr in _PUBLISHED_ADDRS:
        wait_tcp(addr, VENUE_DIRECTORY_PORT, timeout=30)
        wait_tcp(addr, VENUE_NAME_SERVER_PORT, timeout=30)


def ensure() -> str:
    """Bring the venue to the tree's state; returns one line saying what it did.

    Idempotent and quiet when current: two ``docker inspect`` calls."""
    if not docker_usable():
        return "no docker on this machine: the venue stands on the Linux dev VM only"
    infos = {name: _inspect(name) for name in CONTAINERS}
    states = {name: _state(info) for name, info in infos.items()}
    if all(cur and run for cur, run in states.values()):
        return "current"
    if all(cur for cur, _ in states.values()):
        for name, (_, running) in states.items():
            if not running:
                _check(_docker("start", name), f"docker start {name}")
        _wait_up()
        return "started (was stopped)"
    _create()
    _wait_up()
    return f"created ({venue_hash()})"


def venue_status() -> tuple[bool, list[str]]:
    """``(healthy, lines)`` — each container's state, and the reach of both
    published addresses."""
    from helpers.acme_pebble import directory_authority_pem, directory_answers

    lines, healthy = [f"venue hash (tree): {venue_hash()}"], True
    for name in CONTAINERS:
        info = _inspect(name)
        current, running = _state(info)
        healthy &= current and running
        if info is None:
            lines.append(f"{name}: absent")
        else:
            label = ((info.get("Config") or {}).get("Labels") or {}).get(HASH_LABEL)
            lines.append(f"{name}: {'running' if running else 'stopped'}, hash {label}"
                         f"{'' if current else ' (STALE)'}")
    for addr in _PUBLISHED_ADDRS:
        up = directory_answers(addr)
        healthy &= up
        detail = ""
        if up:
            try:
                directory_authority_pem(addr)
                detail = ", authority in the chain"
            except Exception as exc:  # noqa: BLE001
                healthy = False
                detail = f", handshake read failed: {exc}"
        lines.append(f"directory at {addr}:{VENUE_DIRECTORY_PORT}: "
                     f"{'answers' if up else 'NO ANSWER'}{detail}")
    return healthy, lines


def down() -> str:
    _remove_all()
    return "removed"


def main(argv: list[str]) -> int:
    verb = argv[0] if argv else "status"
    if verb == "ensure":
        quiet = "--quiet" in argv
        try:
            what = ensure()
        except Exception as exc:  # noqa: BLE001
            print(f"acme-venue: ensure FAILED: {exc}", file=sys.stderr)
            return 1
        if not (quiet and what == "current"):
            print(f"acme-venue: {what}")
        return 0
    if verb == "status":
        healthy, lines = venue_status()
        for line in lines:
            print(line)
        return 0 if healthy else 1
    if verb == "down":
        print(f"acme-venue: {down()}")
        return 0
    print("usage: acme_venue.py ensure [--quiet] | status | down", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
