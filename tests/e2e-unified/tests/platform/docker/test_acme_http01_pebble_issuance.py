"""tier_4 e2e: **real ACME (HTTP-01) issuance on the Docker image**, end-to-end
through the packaged image's OWN ACME client + challenge listener + cert-watcher,
against an in-network `pebble` test-CA — flipping `mail.<primary>` floor→trusted
via the *real* issuance path.

This is the issuance complement to ``test_dane_tlsa_cert_coupling.py``. That test
proves the **cert-honesty coupling** (floor-MX TLSA present → trusted ⇒ withdrawn)
by writing a CA-issued leaf into the container's ``acme_dir`` **directly** — which
exercises the cert-watcher + DNS-handler wiring but *not* the image's ACME client,
HTTP-01 challenge listener, or order machinery. Only this test drives a **real CA
conversation** all the way through the packaged binary:

    nest boot self-signed floor → ``acme_http01::cert_lifecycle_task`` self-heals →
    ``obtain_certificate`` (instant-acme) creates an ACME account against the
    pebble directory, opens an order, serves the HTTP-01 token on its own :8080
    listener, pebble validates it (resolving the SAN's A record via the
    ``fake_dns`` sidecar and fetching ``http://<nest>:8080/.well-known/...``),
    finalize → fetch → write ``fullchain.pem``/``privkey.pem`` → ``cert_watcher_task``
    hot-reloads it as the default cert.

That is the image/packaging guarantee tier_3 cannot give: that the ACME client is
in the image, the s6 challenge listener is up, the directory-URL knob is plumbed,
and a renewed chain actually installs — through the real Dockerfile build
(``--features bluesky,nostr,activitypub``; **no** ``test-hooks``).

What it asserts (``tls-certificates.md`` § B.1 + § D):

  1. **Real ACME ran.** The leaf the listener serves for the apex (read off the
     live TLS handshake) is **issued by pebble's CA** (issuer contains "Pebble"),
     not the boot-time self-signed floor — the proof the account→order→HTTP-01
     →finalize→fetch path ran inside the image.
  2. **Cert-status projection flips.** ``fauna.tls.cert_status`` reports the apex
     as ``ValidTrusted`` / ``is_floor == false`` (the badge every admin client
     renders).
  3. **TLSA withdrawn (coupling end-state).** The served default cert is what
     ``mail.<primary>`` resolves to (it is never in the resolver ``domains`` map →
     ``cert_for_sni`` falls to the default), so a trusted default flips
     ``served_cert_facts("mail.<primary>").is_floor`` to false and
     ``fauna.dns.list_records`` **withdraws** the floor-key ``_25._tcp.mail.<primary>``
     TLSA — driven here by the *real* issuance, not a hand-written PEM.

The deterministic present→withdrawn temporal coupling is
``test_dane_tlsa_cert_coupling.py``'s burden; this test owns the real-issuance
path and the withdrawn end-state. The floor-MX TLSA is captured *opportunistically*
before issuance completes (the lifecycle's first attempt and the test's
claim/register race), and its served-SPKI pin asserted when caught.

**Why no production code change for pebble's self-signed directory TLS.** pebble
serves its ACME API over HTTPS signed by its own throwaway CA; the nest's
``instant-acme`` client trusts it via the **system trust store**
(``hyper-rustls .with_native_roots()`` → ``rustls-native-certs``, which honours
``SSL_CERT_FILE``). The test mounts a bundle = the image's CA bundle + pebble's CA
and points ``SSL_CERT_FILE`` at it — exactly how a real private/internal-ACME-CA
deployment is trusted. The only production change Track 5 needed is the
``[acme].directory_url`` override (``FAUNA_ACME_DIRECTORY_URL``).
"""

import json
import os
import socket
import subprocess
import tempfile
import time

import pytest

from helpers.tls_spki import served_leaf, spki_sha256_hex


from .helpers import (
    fence_cpuset_args,
    IMAGE_TAG,
    admin_ws,
    claim_admin_api,
    container_ip,
    create_network,
    docker_build,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    wait_for_health,
    write_dns_records,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

# A real-looking apex (pebble refuses to issue for `localhost`, and the nest only
# runs ACME for a real orderable domain — non-empty and not `localhost`; the derived
# gate in `acme::build_acme_config`, no longer an entrypoint `[acme].enabled` flag).
# The fake_dns answers its A record (and `mail.<domain>`'s) → the nest's container IP
# so pebble's HTTP-01 validation reaches the nest's :8080 challenge listener.
DOMAIN = "nest.test"
MX_SNI = f"mail.{DOMAIN}"
TLSA_NAME = f"_25._tcp.mail.{DOMAIN}"
CLAIM_CODE = "ACME44"

# Pinned to the pebble release whose ACME wire shape `instant-acme 0.7.2` (the
# nest's production HTTP-01 client) parses — same pin + rationale as
# `libs/fauna-client-dns/tests/pebble_dns01.rs`.
PEBBLE_IMAGE = "ghcr.io/letsencrypt/pebble:2.9.0"
PEBBLE_CA_PATH = "/test/certs/pebble.minica.pem"
# The nest's HTTP-01 challenge listener port inside the container (hardcoded in
# `main.rs`; `acme_http01::start_http01_listener`). Pebble's VA must validate
# against THIS port, so the mounted pebble config sets `httpPort` to it (the image
# default is 5002).
NEST_HTTP01_PORT = 8080

# pebble's baked default config (extracted from the 2.9.0 image) with `httpPort`
# repointed at the nest's challenge listener. Mounted over the image's default
# config path so `/app -dnsserver <addr>` picks it up; relative cert paths resolve
# against pebble's WorkingDir `/` (so the baked `/test/certs/localhost/*` directory
# cert — SANs `localhost, pebble, 127.0.0.1` — still serves the ACME API, which is
# why the nest reaches it at `https://pebble:14000/dir`).
PEBBLE_CONFIG = {
    "pebble": {
        "listenAddress": "0.0.0.0:14000",
        "managementListenAddress": "0.0.0.0:15000",
        "certificate": "test/certs/localhost/cert.pem",
        "privateKey": "test/certs/localhost/key.pem",
        "httpPort": NEST_HTTP01_PORT,
        "tlsPort": 5001,
        "ocspResponderURL": "",
        "externalAccountBindingRequired": False,
        "domainBlocklist": ["blocked-domain.example"],
        "retryAfter": {"authz": 3, "order": 5},
        "keyAlgorithm": "ecdsa",
    }
}


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def acme_stack(docker_image):
    """A docker network running `pebble` (real ACME CA) + a `fake_dns` sidecar +
    a fresh claimed nest configured to issue via pebble (`FAUNA_ACME_DIRECTORY_URL`),
    trusting pebble's directory CA via `SSL_CERT_FILE`. The nest's `cert_lifecycle_task`
    self-heals its boot self-signed floor into a real pebble-issued cert. Yields the
    nest dict; tears the whole stack down after."""
    net = f"fauna-acme-net-{os.getpid()}"
    dns_name = f"fauna-acme-dns-{os.getpid()}"
    pebble_name = "pebble"  # must match the directory cert SAN + the URL below
    (api_port, pebble_host_port) = find_free_ports(2)
    nest_name = f"fauna-acme-nest-{api_port}"

    workdir = tempfile.mkdtemp(prefix="fauna-acme-")
    records_dir = os.path.join(workdir, "dns")
    os.makedirs(records_dir, exist_ok=True)

    started = []
    try:
        create_network(net)

        # fake_dns: pebble resolves the order's SANs through it (-dnsserver). A
        # records are filled in AFTER the nest starts (its IP isn't known until
        # then) via write_dns_records; pebble validates HTTP-01 against that IP.
        fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
        dns_ip = start_fake_dns_sidecar(
            net, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir
        )
        started.append(dns_name)

        # pebble (the ACME CA) — mount the httpPort-repointed config, validate DNS
        # via the fake_dns, publish :14000 to the host so we can poll readiness.
        config_path = os.path.join(workdir, "pebble-config.json")
        with open(config_path, "w", encoding="utf-8") as f:
            json.dump(PEBBLE_CONFIG, f)
        _start_pebble(net, pebble_name, config_path, f"{dns_ip}:53", pebble_host_port)
        started.append(pebble_name)
        _wait_tcp("127.0.0.1", pebble_host_port, timeout=30)
        pebble_ca = _extract_pebble_ca(pebble_name)

        # The nest trusts pebble's directory CA via the system trust store: a
        # bundle = the image's CA bundle + pebble's CA, pointed to by SSL_CERT_FILE
        # (rustls-native-certs honours it; instant-acme uses .with_native_roots()).
        bundle_path = os.path.join(workdir, "ssl-bundle.pem")
        with open(bundle_path, "wb") as f:
            f.write(_image_ca_bundle())
            f.write(b"\n")
            f.write(pebble_ca)
        os.chmod(bundle_path, 0o644)

        # Domainless boot (no FAUNA_DOMAIN — the entrypoint ignores it now): the box
        # comes up on the `localhost` identity fallback, then acquires `nest.test` as
        # its primary identity from `register_primary_domain(nest, DOMAIN)` below,
        # which flips the always-spawned ACME task's apex to `nest.test` and issues.
        start_container_with_ports(
            nest_name,
            {3000: api_port},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
            },
            network=net,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
        )
        started.append(nest_name)

        # Publish the SANs' A records → the nest's container IP, ASAP, so pebble's
        # HTTP-01 validation (which fires once the lifecycle task opens an order)
        # resolves them and reaches the nest's :8080 listener. Both the apex and
        # `mail.<domain>` are covered so issuance succeeds whether or not the
        # primary mail domain is registered before the order's SAN set is derived.
        nest_ip = container_ip(nest_name)
        write_dns_records(records_dir, a={DOMAIN: nest_ip, MX_SNI: nest_ip})

        wait_for_health(api_port, nest_name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")
        url = f"https://127.0.0.1:{api_port}"
        nest = {"name": nest_name, "port": api_port, "url": url, "admin": admin}
        register_primary_domain(nest, DOMAIN)
        yield nest
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


# ── Pebble lifecycle ──────────────────────────────────────────────────


def _start_pebble(network: str, name: str, config_path: str, dnsserver: str, host_port: int) -> None:
    """Run pebble on `network` with the mounted (httpPort-repointed) config,
    validating DNS through `dnsserver`. `PEBBLE_VA_ALWAYS_VALID=0` keeps real
    HTTP-01 validation; `PEBBLE_WFE_NONCEREJECT=0` makes nonces deterministic.
    NOSLEEP is deliberately left UNSET so pebble's validation sleep widens the
    floor window the opportunistic TLSA capture reads."""
    remove_container(name)
    r = subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--name", name, "--network", network,
         "-p", f"127.0.0.1:{host_port}:14000",
         "-v", f"{config_path}:/test/config/pebble-config.json:ro",
         "-e", "PEBBLE_VA_ALWAYS_VALID=0",
         "-e", "PEBBLE_WFE_NONCEREJECT=0",
         PEBBLE_IMAGE, "-dnsserver", dnsserver],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(f"docker run pebble failed:\n{r.stderr}")


def _extract_pebble_ca(name: str) -> bytes:
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


def _image_ca_bundle() -> bytes:
    """The nest image's system CA bundle (Debian `ca-certificates`), so the
    SSL_CERT_FILE override keeps the public roots and only ADDS pebble's CA."""
    r = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "cat", IMAGE_TAG,
         "/etc/ssl/certs/ca-certificates.crt"],
        capture_output=True, timeout=30,
    )
    if r.returncode != 0:
        raise RuntimeError(f"could not read image CA bundle: {r.stderr.decode(errors='replace')}")
    return r.stdout


def _wait_tcp(host: str, port: int, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=2):
                return
        except OSError:
            time.sleep(0.25)
    raise TimeoutError(f"{host}:{port} never became reachable within {timeout}s")


# ── Read helpers ──────────────────────────────────────────────────────


def _list_records(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.dns.list_records", {})["domains"]


def _floor_mx_tlsa_rows(domains: list[dict]) -> list[dict]:
    primary = next(d for d in domains if d["is_primary"])
    return [
        r for r in primary["records"]
        if r["record_type"] == "TLSA" and r["name"] == TLSA_NAME
    ]


def _cert_status(nest, domains: list[str]) -> dict[str, dict]:
    with admin_ws(nest) as admin:
        reply = admin.call("fauna.tls.cert_status", {"domains": domains})
    return {s["domain"]: s for s in reply["statuses"]}


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("certificates-by-themselves")
def test_real_acme_issuance_flips_floor_to_trusted(acme_stack):
    nest = acme_stack
    host, port = "127.0.0.1", nest["port"]

    # Opportunistic floor capture: if the lifecycle hasn't issued yet, the served
    # leaf is the self-signed floor and the floor-MX TLSA pins its SPKI. (If
    # issuance already won the race the rows are gone — the deterministic
    # present→withdrawn proof is test_dane_tlsa_cert_coupling.py's; here it only
    # strengthens the case when caught.)
    rows = _floor_mx_tlsa_rows(_list_records(nest))
    if rows:
        assert len(rows) == 1, f"one floor-MX TLSA on the floor, got {rows}"
        rdata = rows[0]["expected"]
        assert rdata.startswith("3 1 1 "), f"DANE-EE/SPKI/SHA-256 record, got {rdata!r}"
        served = served_leaf(host, port, MX_SNI)
        assert spki_sha256_hex(served) == rdata.split()[-1].lower(), (
            "the floor-MX TLSA must pin the SPKI of the leaf the image actually "
            "serves for the MX while on the floor"
        )

    # Poll until the image's own ACME machinery has issued + installed a real
    # pebble cert: the served leaf flips self-signed floor → pebble-issued, the
    # cert-status badge flips to ValidTrusted, and the floor-MX TLSA withdraws.
    deadline = time.monotonic() + 150
    last = ""
    while True:
        leaf = served_leaf(host, port, DOMAIN)
        issuer = leaf.issuer.rfc4514_string()
        pebble_issued = "Pebble" in issuer
        status = _cert_status(nest, [DOMAIN])[DOMAIN]
        trusted = (status["state"] == "ValidTrusted") and (status["is_floor"] is False)
        tlsa_withdrawn = not _floor_mx_tlsa_rows(_list_records(nest))
        if pebble_issued and trusted and tlsa_withdrawn:
            break
        last = (
            f"issuer={issuer!r} pebble_issued={pebble_issued} "
            f"cert_status={status} tlsa_withdrawn={tlsa_withdrawn}"
        )
        if time.monotonic() > deadline:
            logs = subprocess.run(
                ["docker", "logs", "--tail", "60", nest["name"]],
                capture_output=True, text=True, timeout=15,
            )
            raise AssertionError(
                "the image's ACME client never issued+installed a pebble cert in "
                f"time. last={last}\n--- nest logs (tail) ---\n{logs.stdout}\n{logs.stderr}"
            )
        time.sleep(2)

    # Final positive assertions (race-free end state).
    leaf = served_leaf(host, port, DOMAIN)
    assert "Pebble" in leaf.issuer.rfc4514_string(), (
        "the served apex leaf must be issued by pebble's CA (real issuance ran "
        "through the image's own ACME client), not the boot-time self-signed floor"
    )
    status = _cert_status(nest, [DOMAIN, MX_SNI])
    assert status[DOMAIN]["state"] == "ValidTrusted" and status[DOMAIN]["is_floor"] is False
    # The withdrawn TLSA is a real absence in a live matrix (the primary domain is
    # registered + emits its other records), not a missing/empty domain.
    domains = _list_records(nest)
    primary = next(d for d in domains if d["is_primary"])
    assert primary["domain"] == DOMAIN
    assert any(r["record_type"] == "MX" for r in primary["records"]), (
        "the primary domain's matrix is populated (MX present) — the TLSA's "
        "absence is a genuine withdrawal, not an empty matrix"
    )
    assert not _floor_mx_tlsa_rows(domains), (
        "a trusted default cert (what mail.<primary> serves) must withdraw the "
        "floor-key TLSA"
    )
