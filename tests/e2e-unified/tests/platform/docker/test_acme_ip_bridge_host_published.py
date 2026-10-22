"""tier_4 e2e: **the IP bridge cert ordered from the host-published address set**,
end-to-end through the packaged image against an in-network ``pebble`` test CA
(``tls-certificates.md`` § B-IP *When*, source (2), and *Testing*).

A provisioned VPS runs the nest in a container behind published ports, so the
container's own interface table holds only a bridge-network RFC 1918 address and
the derive finds nothing to order there. The cloud-init artifact therefore
publishes the HOST's global-unicast addresses into the root-owned, read-only
host→nest channel (``/opt/fauna/maintenance-host/host-addresses`` →
``/data/maintenance-host/host-addresses``; format owner ``installers/vps.md``
§ Host OS Maintenance → 3), and the nest unions that set into the derive.

This test plants exactly that file — naming the nest container's own address —
in a directory bind-mounted ``:ro`` at ``/data/maintenance-host``, boots a
DOMAINLESS, unclaimed nest (the pre-claim window the bridge exists for) against
pebble, and asserts the file→order→serve flow: the leaf served to a **no-SNI**
dial flips from the self-signed floor to a pebble-issued certificate carrying an
``iPAddress`` SAN for that address.

The harness address is RFC 1918, which the derive's global-unicast filter
refuses; § B-IP *Testing* rules a widening bound to the ``[acme] directory_url``
override (``FAUNA_ACME_DIRECTORY_URL``), under which a host-published
non-loopback, non-link-local address passes. The bound itself — without the
override the same file orders nothing — is pinned by the nest's unit test
``acme::tests::ip_bridge_test_ca_widening_admits_a_host_published_private_address``:
a tier_4 run without the override would aim the box at Let's Encrypt production.

The container address is pinned BEFORE boot (a subnet'd network + ``--ip``)
because the lifecycle's first tick runs at boot and its steady poll is minutes
long: the file must already be there, as cloud-init's first-boot publish makes
it on a real box.

The live half — a browser at ``https://<ip>`` on a real provisioned box —
rides the live Hetzner run, not this test.
"""

import json
import os
import subprocess
import tempfile

import pytest
from cryptography import x509

from helpers.tls_spki import served_leaf
from helpers.waiting import wait_until

from .helpers import (
    IMAGE_TAG,
    create_subnet_network,
    docker_build,
    find_free_ports,
    get_repo_root,
    remove_container,
    remove_network,
    start_container_with_ports,
    wait_for_health,
)
from .test_acme_http01_pebble_issuance import (
    PEBBLE_CONFIG,
    _extract_pebble_ca,
    _image_ca_bundle,
    _start_pebble,
    _wait_tcp,
)

# Ceiling, not a target (convention 14): pebble's validation plus one retry of
# the lifecycle's first attempt fit far inside it; a green run returns at once.
IP_BRIDGE_ISSUE_BUDGET_S = 240
DOCKER_CLI_TIMEOUT_S = 30

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=DOCKER_CLI_TIMEOUT_S)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

# pebble's baked config with the nest's challenge port, plus the `shortlived`
# profile every IP-identifier order names (`fauna_acme_http01::IP_CERT_PROFILE`).
PEBBLE_IP_CONFIG = {
    "pebble": {
        **PEBBLE_CONFIG["pebble"],
        "profiles": {
            "default": {"description": "default", "validityPeriod": 7776000},
            "shortlived": {"description": "IP bridge", "validityPeriod": 576000},
        },
    }
}


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def ip_bridge_stack(docker_image):
    """pebble + a domainless nest whose host-published address file names its
    own (pinned) container address. Yields ``(api_port, nest_ip, nest_name)``."""
    net = f"fauna-ipbridge-net-{os.getpid()}"
    pebble_name = "pebble"  # must match the directory cert SAN + the URL below
    (api_port, pebble_host_port) = find_free_ports(2)
    nest_name = f"fauna-ipbridge-nest-{api_port}"
    workdir = tempfile.mkdtemp(prefix="fauna-ipbridge-")
    started = []
    try:
        subnet = create_subnet_network(net)
        nest_ip = str(subnet.network_address + 10)

        config_path = os.path.join(workdir, "pebble-config.json")
        with open(config_path, "w", encoding="utf-8") as f:
            json.dump(PEBBLE_IP_CONFIG, f)
        # An IP identifier needs no DNS; pebble still wants a resolver flag.
        _start_pebble(net, pebble_name, config_path, "127.0.0.11:53", pebble_host_port)
        started.append(pebble_name)
        _wait_tcp("127.0.0.1", pebble_host_port, timeout=30)
        pebble_ca = _extract_pebble_ca(pebble_name)

        bundle_path = os.path.join(workdir, "ssl-bundle.pem")
        with open(bundle_path, "wb") as f:
            f.write(_image_ca_bundle())
            f.write(b"\n")
            f.write(pebble_ca)
        os.chmod(bundle_path, 0o644)

        # The host→nest channel as cloud-init leaves it: a 0755 dir holding
        # `host-addresses`, exported read-only to the container. (On a real box
        # the dir is root-owned; the harness cannot chown to root, and the `:ro`
        # mount is what keeps the nest from writing it either way.)
        host_dir = os.path.join(workdir, "maintenance-host")
        os.makedirs(host_dir)
        os.chmod(host_dir, 0o755)
        with open(os.path.join(host_dir, "host-addresses"), "w", encoding="utf-8") as f:
            f.write(f"address={nest_ip}\n")
        os.chmod(os.path.join(host_dir, "host-addresses"), 0o644)

        start_container_with_ports(
            nest_name,
            {443: api_port},
            env={
                "FAUNA_PORT": "3000",
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
            },
            network=net,
            ip=nest_ip,
            mounts=[
                (bundle_path, "/etc/fauna/ssl-bundle.pem"),
                (host_dir, "/data/maintenance-host"),
            ],
        )
        started.append(nest_name)
        wait_for_health(api_port, nest_name)
        yield api_port, nest_ip, nest_name
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=DOCKER_CLI_TIMEOUT_S)


def _ip_sans(leaf: x509.Certificate) -> list[str]:
    try:
        san = leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName).value
    except x509.ExtensionNotFound:
        return []
    return [str(ip) for ip in san.get_values_for_type(x509.IPAddress)]


@pytest.mark.feature("certificates-by-themselves")
def test_host_published_address_orders_and_serves_the_ip_bridge(ip_bridge_stack):
    api_port, nest_ip, nest_name = ip_bridge_stack
    # Python sends no SNI for an IP-literal server_hostname — the no-SNI dial a
    # browser at https://<ip> makes, which the resolver answers with the bridge.
    last = {}

    def _bridge_served():
        leaf = served_leaf("127.0.0.1", api_port, "127.0.0.1")
        last["issuer"] = leaf.issuer.rfc4514_string()
        last["ip_sans"] = _ip_sans(leaf)
        return "Pebble" in last["issuer"] and nest_ip in last["ip_sans"]

    def _diagnose():
        logs = subprocess.run(
            ["docker", "logs", "--tail", "60", nest_name],
            capture_output=True, text=True, timeout=DOCKER_CLI_TIMEOUT_S,
        )
        return (
            f"the nest never ordered + served the IP bridge cert for its "
            f"host-published address {nest_ip}. last={last}\n"
            f"--- nest logs (tail) ---\n{logs.stdout}\n{logs.stderr}"
        )

    wait_until(_bridge_served, IP_BRIDGE_ISSUE_BUDGET_S, interval=2, diagnose=_diagnose)

    leaf = served_leaf("127.0.0.1", api_port, "127.0.0.1")
    assert "Pebble" in leaf.issuer.rfc4514_string(), (
        "the no-SNI leaf must be the pebble-issued bridge, not the self-signed floor"
    )
    assert _ip_sans(leaf) == [nest_ip], (
        "the bridge must cover exactly the host-published address as an "
        f"iPAddress SAN, got {_ip_sans(leaf)}"
    )
