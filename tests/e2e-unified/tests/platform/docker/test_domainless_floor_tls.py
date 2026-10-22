"""tier_4 e2e: a nest with no name at all serves TLS from boot, on a
certificate it made itself (tls-certificates.md § A. The self-signed floor:
the floor is written unconditionally on every nest, a domainless one
included).

The box is booted from the real image with no domain and is never claimed, so
nothing but boot has run. The assertions read the served leaf off a TLS
handshake on the main port — never `wait_for_health`'s outcome, which accepts
plain http as well and so cannot tell an encrypted listener from an
unencrypted one.
"""

import datetime
import ssl
import subprocess
import tempfile
import time
import urllib.request

import pytest
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ed448, ed25519

from helpers.tls_spki import served_leaf, spki_sha256_hex
from helpers.waiting import wait_until

from .helpers import (
    HEALTH_TIMEOUT,
    docker_build,
    find_free_port,
    get_repo_root,
    read_data_file,
    remove_container,
    remove_data_dir,
    start_container,
    start_container_with_ports,
    write_data_file,
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


def _await_https_health(port: int, name: str) -> None:
    """Poll the health endpoint over https ONLY (verify off: the leaf is
    self-signed). Readiness is the condition, the timeout only a backstop."""
    ctx = ssl._create_unverified_context()
    url = f"https://127.0.0.1:{port}/api/v1/health"
    deadline = time.monotonic() + HEALTH_TIMEOUT
    last: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=5, context=ctx) as resp:
                if resp.status == 200:
                    return
        except Exception as e:  # not up yet
            last = e
        time.sleep(0.5)
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=10)
    raise TimeoutError(
        f"{name} never answered health over https: {last!r}\n"
        + "\n".join((logs.stdout + logs.stderr).splitlines()[-40:])
    )


@pytest.mark.feature("certificates-by-themselves")
def test_domainless_nest_serves_tls_on_its_own_certificate():
    """No domain, no claim: the main port completes a TLS handshake and
    presents a self-signed leaf (issuer DN == subject DN) — for an IP-dialled
    peer (no SNI) and for `localhost` alike."""
    docker_build(get_repo_root())
    port = find_free_port()
    name = f"fauna-nest-domainless-floor-{port}"
    start_container(name, port, env={"FAUNA_MODE": "public"})
    try:
        _await_https_health(port, name)
        for sni in ("127.0.0.1", "localhost"):
            leaf = served_leaf("127.0.0.1", port, sni=sni)
            issuer = leaf.issuer.rfc4514_string()
            subject = leaf.subject.rfc4514_string()
            assert issuer == subject, (
                f"a domainless nest must serve its own self-signed floor (SNI {sni!r}), "
                f"but the leaf is issued by another: issuer={issuer!r} subject={subject!r}"
            )
    finally:
        remove_container(name)


def _plant_near_expiry_floor(data_dir: str, days_left: int) -> None:
    """Rewrite the on-disk floor as the same self-signed cert (same subject,
    same names, same key) but expiring in `days_left` days — the floor as a
    long-lived nest would find it after most of its life has passed."""
    served = x509.load_pem_x509_certificate(read_data_file(data_dir, "acme/fullchain.pem"))
    key = serialization.load_pem_private_key(
        read_data_file(data_dir, "acme/privkey.pem"), password=None)
    now = datetime.datetime.now(datetime.timezone.utc)
    builder = (
        x509.CertificateBuilder()
        .subject_name(served.subject)
        .issuer_name(served.subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=days_left))
    )
    for ext in served.extensions:
        builder = builder.add_extension(ext.value, critical=ext.critical)
    algorithm = None if isinstance(key, (ed25519.Ed25519PrivateKey, ed448.Ed448PrivateKey)) \
        else hashes.SHA256()
    cert = builder.sign(key, algorithm)
    write_data_file(data_dir, "acme/fullchain.pem", cert.public_bytes(serialization.Encoding.PEM))


@pytest.mark.feature("certificates-by-themselves")
def test_the_floor_renews_itself_on_the_same_key():
    """The self-signed certificate renews itself and keeps the same key, so a
    mail app that accepted it once is not asked again (tls-certificates.md § A:
    the floor is re-synthesized from its persisted stable key inside the
    renewal lead). The floor's renew check runs when the nest starts and then
    twice a day, so the near-expiry floor is planted on a kept /data while the
    box is down and the box started again — no name is ever given, so no CA
    order can replace the floor first."""
    docker_build(get_repo_root())
    port = find_free_port()
    name = f"fauna-nest-floor-renew-{port}"
    data_dir = tempfile.mkdtemp(prefix="fauna-floor-data-")
    try:
        start_container_with_ports(name, {443: port},
                                   env={"FAUNA_MODE": "public", "FAUNA_PORT": "3000"},
                                   data_volume=data_dir)
        _await_https_health(port, name)
        original = served_leaf("127.0.0.1", port, sni="localhost")
        assert original.issuer == original.subject, "the premise: the floor is served"
        key_fingerprint = spki_sha256_hex(original)

        subprocess.run(["docker", "stop", name], check=True, capture_output=True, timeout=60)
        _plant_near_expiry_floor(data_dir, days_left=2)
        subprocess.run(["docker", "start", name], check=True, capture_output=True, timeout=60)
        _await_https_health(port, name)

        # The renewed floor: a validity well past the 30-day lead again.
        lead = datetime.timedelta(days=30)

        # A handshake can be reset while the router restarts once after the
        # nest's restart (it re-reads the sidecar token the nest re-mints on
        # every boot — `docker/s6/fauna-sni-router/run`): not yet, not a verdict.
        last: dict = {}

        def _renewed():
            try:
                leaf = served_leaf("127.0.0.1", port, sni="localhost")
            except OSError as e:
                last["error"] = repr(e)
                return None
            last["leaf"] = leaf
            left = leaf.not_valid_after_utc - datetime.datetime.now(datetime.timezone.utc)
            return leaf if left > lead else None

        def _diagnose():
            leaf = last.get("leaf")
            served = (leaf.not_valid_after_utc.isoformat() if leaf
                      else f"no handshake ({last.get('error')})")
            logs = subprocess.run(["docker", "logs", "--tail", "40", name],
                                  capture_output=True, text=True, timeout=10)
            return (f"the near-expiry floor was never renewed: served notAfter="
                    f"{served}\n{logs.stdout}{logs.stderr}")

        renewed = wait_until(_renewed, 60, interval=1, diagnose=_diagnose)
        assert renewed.issuer == renewed.subject, "the renewed floor is still self-signed"
        assert spki_sha256_hex(renewed) == key_fingerprint, (
            "the renewed floor must keep the key a mail app already accepted")
    finally:
        remove_container(name)
        remove_data_dir(data_dir)
