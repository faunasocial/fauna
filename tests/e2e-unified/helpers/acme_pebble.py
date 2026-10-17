"""A pebble ACME CA plus a fake authoritative name server, for walks that open a
real certificate order FROM AN APP.

The certificate authority is a constant no human chooses
(`tls-certificates.md` § ACME settings — constants, not choices), so an app
reaches this CA only through the compile-gated e2e ACME directory seed
(`e2e-automation-surface-gating.md` § The ACME directory seed):
``FAUNA_E2E_ACME_DIRECTORY_URL`` / ``FAUNA_E2E_ACME_CA_PEM`` /
``FAUNA_E2E_ACME_NAMESERVER``, read by `fauna_client_dns::e2e_acme_seam` and
installed by the shared native admin-dns machine builder. :meth:`AcmePebble.env`
is exactly that seed.

The stack is two containers on a private docker network:

- ``fake_dns`` (`fakes/fake_dns.py`) — the name server pebble validates DNS-01
  against (``-dnsserver``) AND the app's propagation gate polls (the seed's
  name-server variable). Its records come from a read-only bind-mounted
  ``records.json`` re-read on every query, so a test decides when the
  challenge TXT appears (:meth:`AcmePebble.publish_txt`) — or, on the managed
  path, the app's fake DNS provider writes it itself, as an RFC 2136 dynamic
  UPDATE over TCP into the fake's in-memory overlay, as a real provider's zone
  would serve it. Pebble validates against the sidecar's container address on
  the docker network; every app reaches it at ``127.0.0.1:<port>``, the port
  published for TCP and UDP and taken through the nest-port seam
  (:func:`lease_name_server_port`), so an android venue run's device reaches
  it through the tunnel over TCP — the propagation probe's RFC 7766 retry
  (`e2e-automation-surface-gating.md` § The ACME directory seed → *The name
  server's reach*).
- ``pebble`` — its directory published on a loopback port taken through the
  nest-port seam (:func:`lease_directory_port`), so an android venue run's
  device can reach it through the tunnel. Pebble's own
  directory certificate carries the SAN ``127.0.0.1``, and its CA is copied out
  and handed to the app as PEM text, so the app's ACME client trusts that CA
  and nothing else.

Both container names carry the pid, so concurrent sessions on one machine never
collide; the docker-tier HTTP-01 test shares the image pin, the config and the
CA extraction from here.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path

#: Pinned to the pebble release whose ACME wire shape `instant-acme 0.7.2`
#: parses — same pin + rationale as `libs/fauna-client-dns/tests/pebble_dns01.rs`.
PEBBLE_IMAGE = "ghcr.io/letsencrypt/pebble:2.9.0"
PEBBLE_CA_PATH = "/test/certs/pebble.minica.pem"


def pebble_config(http_port: int = 5002, *, validity_secs: int | None = None) -> dict:
    """pebble's baked default config (extracted from the 2.9.0 image), with
    ``httpPort`` settable — the docker-tier HTTP-01 test repoints it at the
    nest's challenge listener. Mounted over the image's default config path so
    `/app -dnsserver <addr>` picks it up; relative cert paths resolve against
    pebble's WorkingDir `/`, so the baked `/test/certs/localhost/*` directory
    cert (SANs `localhost, pebble, 127.0.0.1`) serves the ACME API.

    ``validity_secs`` sets the ``default`` profile's certificate lifetime — the
    profile an ACME client that names none (the nest's) is issued under — so a
    renewal witness can be handed a leaf already inside the nest's 30-day
    renewal lead."""
    config = {
        "pebble": {
            "listenAddress": "0.0.0.0:14000",
            "managementListenAddress": "0.0.0.0:15000",
            "certificate": "test/certs/localhost/cert.pem",
            "privateKey": "test/certs/localhost/key.pem",
            "httpPort": http_port,
            "tlsPort": 5001,
            "ocspResponderURL": "",
            "externalAccountBindingRequired": False,
            "domainBlocklist": ["blocked-domain.example"],
            "retryAfter": {"authz": 3, "order": 5},
            "keyAlgorithm": "ecdsa",
        }
    }
    if validity_secs is not None:
        config["pebble"]["profiles"] = {
            "default": {
                "description": "short-lived, for the renewal witness",
                "validityPeriod": validity_secs,
            }
        }
    return config


def start_pebble(
    network: str,
    name: str,
    config_path: str,
    dnsserver: str,
    host_port: int,
    *,
    va_nosleep: bool = False,
) -> None:
    """Run pebble on `network` with the mounted config, validating DNS through
    `dnsserver`, its directory published at ``127.0.0.1:<host_port>``.
    `PEBBLE_VA_ALWAYS_VALID=0` keeps real validation; `PEBBLE_WFE_NONCEREJECT=0`
    makes nonces deterministic; `va_nosleep` skips pebble's artificial
    validation delay."""
    from tests.platform.docker.helpers import fence_cpuset_args, remove_container

    remove_container(name)
    env = ["-e", "PEBBLE_VA_ALWAYS_VALID=0", "-e", "PEBBLE_WFE_NONCEREJECT=0"]
    if va_nosleep:
        env += ["-e", "PEBBLE_VA_NOSLEEP=1"]
    r = subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--name", name, "--network", network,
         "-p", f"127.0.0.1:{host_port}:14000",
         "-v", f"{config_path}:/test/config/pebble-config.json:ro",
         *env,
         PEBBLE_IMAGE, "-dnsserver", dnsserver],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(f"docker run pebble failed:\n{r.stderr}")


def extract_pebble_ca(name: str) -> bytes:
    """`docker cp` pebble's throwaway directory CA out (retried while the
    container filesystem comes up)."""
    dest = os.path.join(tempfile.gettempdir(), f"{name}-{os.getpid()}-minica.pem")
    for attempt in range(40):
        cp = subprocess.run(
            ["docker", "cp", f"{name}:{PEBBLE_CA_PATH}", dest],
            capture_output=True, text=True, timeout=30,
        )
        if cp.returncode == 0 and os.path.exists(dest):
            with open(dest, "rb") as f:
                data = f.read()
            os.remove(dest)
            return data
        if attempt == 39:
            raise RuntimeError(f"could not copy pebble CA from {name}:{PEBBLE_CA_PATH}: {cp.stderr}")
        time.sleep(0.25)
    raise AssertionError("unreachable")


def lease_directory_port():
    """The host port pebble's directory is published on -> ``(port, release)``.

    Taken through `helpers.android_venue.nest_port`, the seam every nest port
    comes from: outside an android venue run it is a free port, and inside one
    a lease on the tunnelled range. An android app reaches the directory as its
    own loopback through an ``adb reverse``, which the venue carries home only
    for that range (`testing.md` § Default app and nest mode → *Android's run
    venue*, constraint 3)."""
    from helpers import android_venue
    from tests.platform.docker.helpers import find_free_ports

    return android_venue.nest_port(lambda: find_free_ports(1)[0])


def lease_name_server_port():
    """The host port the fake name server is published on, TCP and UDP ->
    ``(port, release)``. The same seam as :func:`lease_directory_port`, for the
    same reason: in an android venue run the device reaches it only as its own
    loopback through an ``adb reverse`` of the tunnelled range."""
    from helpers import android_venue
    from tests.platform.docker.helpers import find_free_ports

    return android_venue.nest_port(lambda: find_free_ports(1)[0])


def name_server_port(env: dict) -> int | None:
    """The port of the name server a launch environment's ACME seed names, or
    None when it names none — the second port an android launch must reverse."""
    ns = env.get("FAUNA_E2E_ACME_NAMESERVER")
    return int(ns.rsplit(":", 1)[1]) if ns else None


def directory_port(env: dict) -> int | None:
    """The port of the directory a launch environment's ACME seed names, or
    None when the environment carries no seed — the port an android launch
    must reverse."""
    from urllib.parse import urlparse

    url = env.get("FAUNA_E2E_ACME_DIRECTORY_URL")
    return urlparse(url).port if url else None


def wait_tcp(host: str, port: int, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            time.sleep(0.25)
    raise RuntimeError(f"{host}:{port} not reachable within {timeout}s")


class AcmePebble:
    """The pebble + fake_dns stack for one test session. :meth:`start` brings
    it up; :meth:`env` is the app's launch-environment seed; :meth:`stop`
    removes both containers and the network."""

    def __init__(self) -> None:
        tag = f"{os.getpid()}"
        self.network = f"fauna-e2e-acme-net-{tag}"
        self.dns_name = f"fauna-e2e-acme-dns-{tag}"
        self.pebble_name = f"fauna-e2e-pebble-{tag}"
        self.workdir = tempfile.mkdtemp(prefix="fauna-e2e-acme-")
        self.records_dir = os.path.join(self.workdir, "dns")
        os.makedirs(self.records_dir, exist_ok=True)
        self.directory_url = ""
        self.ca_pem = ""
        self.nameserver = ""
        self._txt: dict[str, str] = {}
        self._releases: list = []

    def start(self) -> "AcmePebble":
        from tests.platform.docker.helpers import (
            create_network,
            start_fake_dns_sidecar,
        )

        host_port, release = lease_directory_port()
        self._releases.append(release)
        ns_port, release = lease_name_server_port()
        self._releases.append(release)
        try:
            create_network(self.network)
            fakes_dir = str(Path(__file__).resolve().parent.parent / "fakes")
            dns_ip = start_fake_dns_sidecar(
                self.network, fakes_dir, name=self.dns_name, txt_records={},
                records_dir=self.records_dir, log_queries=True, publish_port=ns_port,
            )
            config_path = os.path.join(self.workdir, "pebble-config.json")
            with open(config_path, "w", encoding="utf-8") as f:
                json.dump(pebble_config(), f)
            start_pebble(
                self.network, self.pebble_name, config_path, f"{dns_ip}:53", host_port,
                va_nosleep=True,
            )
            wait_tcp("127.0.0.1", host_port, timeout=30)
            wait_tcp("127.0.0.1", ns_port, timeout=30)
            self.ca_pem = extract_pebble_ca(self.pebble_name).decode()
        except Exception:
            self.stop()
            raise
        self.directory_url = f"https://127.0.0.1:{host_port}/dir"
        self.nameserver = f"127.0.0.1:{ns_port}"
        return self

    def env(self) -> dict[str, str]:
        """The e2e ACME directory seed for an app launch."""
        return {
            "FAUNA_E2E_ACME_DIRECTORY_URL": self.directory_url,
            "FAUNA_E2E_ACME_CA_PEM": self.ca_pem,
            # Also where the e2e fake DNS provider writes every TXT it publishes
            # (an UPDATE over TCP), so a MANAGED order's challenge is served
            # exactly as a real provider's zone would serve it.
            "FAUNA_E2E_ACME_NAMESERVER": self.nameserver,
        }

    def publish_txt(self, name: str, value: str) -> None:
        """Serve ``name TXT value`` from the fake name server from the next
        query on — the admin "pasting" the challenge at their registrar. Merged
        over the file's current TXT records; a name the app's fake DNS provider
        has UPDATEd is served from the fake's overlay above the file."""
        from tests.platform.docker.helpers import write_dns_records

        try:
            with open(os.path.join(self.records_dir, "records.json"), encoding="utf-8") as f:
                current = json.load(f).get("txt") or {}
        except (OSError, ValueError):
            current = {}
        self._txt = {**current, name.rstrip(".").lower(): value}
        write_dns_records(self.records_dir, txt=dict(self._txt))

    def txt_queries(self, name: str) -> int:
        """How many ``TXT`` queries for ``name`` the fake name server has
        answered so far — from its query log (``log_queries``), so a test can
        wait for a probe to have happened rather than sleep past it."""
        logs = subprocess.run(
            ["docker", "logs", self.dns_name], capture_output=True, text=True, timeout=30,
        )
        want = f"fake_dns query {name.rstrip('.').lower()} 16"
        return sum(1 for ln in logs.stdout.splitlines() if ln.strip() == want)

    def stop(self) -> None:
        from tests.platform.docker.helpers import remove_container, remove_network

        for name in (self.pebble_name, self.dns_name):
            remove_container(name)
        remove_network(self.network)
        for release in self._releases:
            release()
        self._releases = []


def skip_android_until_it_reaches_the_acme_stack(request) -> None:
    """Android is a declared environment skip (convention 7) for every witness
    that orders from pebble: the witness is unverified.

    The whole stack reaches the app: `launchApp()` puts the three
    `FAUNA_E2E_ACME_*` keys on the intent and `MainActivity` re-exports them;
    pebble's directory and the fake name server are each published on a venue
    lease the launch reverses (:func:`lease_directory_port`,
    :func:`lease_name_server_port`), and the propagation probe's TCP retry and
    the managed mirror's UPDATE both ride that TCP-only reverse. But no android
    e2e run has gone through the venue yet, so it stays skipped until that first
    run shows it green.

    Call it from an autouse fixture. It reads the app from the parametrization,
    not the driver, so the skip comes before the app fixture builds and installs
    an APK it will never drive."""
    from helpers.app_surface import skip_environment

    if request.node.callspec.params.get("app") != "android":
        return
    skip_environment(
        "android: the ACME seed is forwarded and pebble's and the name server's "
        "ports are reversed, but no android run has gone through the venue yet "
        "to verify this test"
    )
