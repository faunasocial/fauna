"""tier_4: a fallen scanner sidecar reaches none of the nest container's internal
listeners — ``deployment-threat-model.md`` § Concern 1a, the container-hardening
paragraph (the lateral half of tcp 25 → the sidecars).

ClamAV and rspamd parse what a stranger mails, so they run in their own
containers with no secret, no socket and no volume. They still share the compose
network with the nest container, so the image binds every listener nothing
outside the container dials on container LOOPBACK: the nest's own ``:3000``
(clients arrive through the SNI router's ``:443``) and the MDA's plaintext
STARTTLS IMAP ``:143``. ``:443`` and the published mail ports stay on every
interface — they face the internet anyway, a sidecar reaching them is a stranger.

**Where the probe stands.** The nest runs with the compose bundle's shape: on a
user-defined network beside the clamd and rspamd sidecars, mail enabled and both
bridges serving. The probe joins the clamd sidecar's own network namespace
(``--network container:<clamd>``) — literally the address a clamd RCE would
dial from — and opens a TCP connection to each port on the nest container's
address.

**Why the refusals are not vacuous.** From the same position ``:443`` and
``:993`` connect (the nest container is reachable, the MDA is up), and inside the
nest container ``:143`` answers its IMAP greeting on loopback — so a refused
``:143`` is the bind, not a listener that never started. The nest's health
answering through the router proves ``:3000`` is up the same way.

The ClamAV-version half (the pinned clamd meets the vetted table's floor) runs
the real third-party image, so it lives in ``test_mail_scan_real_clamd.py``
behind that file's owner-approval opt-in.
"""

from __future__ import annotations

import json
import subprocess

import pytest

from .helpers import (
    IMAGE_TAG,
    SCANNER_SIDECAR_IMAGE,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_scanner_sidecars,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
]

CLAIM_CODE = "LATRL1"
DOMAIN = "localhost"

#: What the scanner sidecar's network position must and must not reach.
MUST_REFUSE = (3000, 143)
MUST_CONNECT = (443, 993)

#: Connects to each port of $TARGET and prints {port: "open"|"refused"|<error>}.
_PROBE = (
    "import json, os, socket\n"
    "out = {}\n"
    "for p in json.loads(os.environ['PORTS']):\n"
    "    s = socket.socket(); s.settimeout(5)\n"
    "    try:\n"
    "        s.connect((os.environ['TARGET'], p)); out[p] = 'open'\n"
    "    except ConnectionRefusedError:\n"
    "        out[p] = 'refused'\n"
    "    except OSError as e:\n"
    "        out[p] = type(e).__name__\n"
    "    finally:\n"
    "        s.close()\n"
    "print(json.dumps(out))\n"
)


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def lateral_nest(docker_image):
    """The compose shape: nest + clamd/rspamd sidecars on one network, mail
    serving. Everything is removed afterwards."""
    suffix = find_free_port()
    network = f"fauna-lateral-net-{suffix}"
    clamd_name = f"fauna-lateral-clamd-{suffix}"
    rspamd_name = f"fauna-lateral-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-lateral-{http_port}"
    create_network(network)
    try:
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        start_container_with_ports(
            name,
            {443: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
                "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
            },
            network=network,
        )
        try:
            wait_for_health(http_port, name)
            admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
            nest = {
                "name": name,
                "port": http_port,
                "url": f"https://127.0.0.1:{http_port}",
                "admin": admin,
                "mail_ports": mail_ports,
                "clamd": clamd_name,
            }
            register_primary_domain(nest, DOMAIN)
            bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
            yield nest
        finally:
            remove_container(name)
    finally:
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


def probe_from(netns_of: str, target: str, ports) -> dict[int, str]:
    """TCP-connect each port of `target` from inside `netns_of`'s network
    namespace; only the verdict words are read back."""
    r = subprocess.run(
        ["docker", "run", "--rm", "--network", f"container:{netns_of}",
         "-e", f"TARGET={target}", "-e", f"PORTS={json.dumps(list(ports))}",
         SCANNER_SIDECAR_IMAGE, "python", "-c", _PROBE],
        capture_output=True, text=True, timeout=120,
    )
    assert r.returncode == 0, f"probe from {netns_of} failed: rc={r.returncode}"
    return {int(k): v for k, v in json.loads(r.stdout.strip().splitlines()[-1]).items()}


def imap_greets_on_loopback(name: str) -> bool:
    """Inside the nest container: does 127.0.0.1:143 answer an IMAP greeting?"""
    r = subprocess.run(
        ["docker", "exec", name, "bash", "-c",
         "exec 3<>/dev/tcp/127.0.0.1/143 && timeout 10 head -c 4 <&3"],
        capture_output=True, text=True, timeout=30,
    )
    return r.returncode == 0 and r.stdout.startswith("* OK")


def test_the_scanner_sidecar_reaches_no_internal_listener(lateral_nest):
    nest_ip = container_ip(lateral_nest["name"])
    seen = probe_from(lateral_nest["clamd"], nest_ip, MUST_REFUSE + MUST_CONNECT)

    for port in MUST_CONNECT:
        assert seen[port] == "open", (
            f"control: from the clamd sidecar's position {nest_ip}:{port} must connect "
            f"(got {seen[port]!r}) — without it a refusal below proves nothing"
        )
    assert imap_greets_on_loopback(lateral_nest["name"]), (
        "control: the MDA's plaintext IMAP must answer on 127.0.0.1:143 inside the "
        "nest container, or a refused :143 is a listener that never started"
    )
    for port in MUST_REFUSE:
        assert seen[port] == "refused", (
            f"the clamd sidecar reached the nest container's internal :{port} "
            f"(got {seen[port]!r}). The image must bind it on container loopback — "
            "deployment-threat-model.md § Concern 1a, the container-hardening paragraph."
        )
