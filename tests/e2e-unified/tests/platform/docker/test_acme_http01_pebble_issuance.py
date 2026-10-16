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

The module's later tests reuse the same stack (``pebble_nest_stack``, one
precondition knob each) to witness keeping the cert alive
(``certificates-by-themselves`` outcomes 7, 8, 11 and 13): paced retries across
a restart, renewal inside the lead, a service name joining once it resolves,
and the plain-http redirect.

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

import contextlib
import datetime
import http.client
import json
import os
import re
import socket
import subprocess
import tempfile
import time

import pytest
from cryptography import x509

from helpers.tls_spki import served_leaf, spki_sha256_hex
from helpers.waiting import wait_until
from helpers.acme_pebble import (
    extract_pebble_ca,
    pebble_config,
    start_pebble,
)


from .helpers import (
    ATPROTO_ROLE,
    IMAGE_TAG,
    admin_ws,
    await_pending_enrollment,
    claim_admin_api,
    container_ip,
    create_network,
    docker_build,
    enable_atproto_bridge,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    remove_data_dir,
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

# The pebble image pin, its baked config and the CA extraction live in
# `helpers/acme_pebble.py`, shared with the app-side ACME directory seed's stack.
# The nest's HTTP-01 challenge listener port inside the container (hardcoded in
# `main.rs`; `acme_http01::start_http01_listener`). Pebble's VA must validate
# against THIS port, so the mounted pebble config sets `httpPort` to it (the image
# default is 5002). The nest reaches pebble at `https://pebble:14000/dir`, a SAN of
# pebble's baked directory cert.
NEST_HTTP01_PORT = 8080
PEBBLE_CONFIG = pebble_config(NEST_HTTP01_PORT)


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@contextlib.contextmanager
def pebble_nest_stack(
    *,
    publish_a: bool = True,
    nest_dns: bool = False,
    data_dir: str | None = None,
    extra_ports: dict[int, int] | None = None,
    validity_secs: int | None = None,
    register: bool = True,
):
    """A docker network running `pebble` (real ACME CA) + a `fake_dns` sidecar +
    a fresh claimed nest configured to issue via pebble (`FAUNA_ACME_DIRECTORY_URL`),
    trusting pebble's directory CA via `SSL_CERT_FILE`. Yields the nest dict
    (``name``, ``port``, ``url``, ``admin``, ``ip``, ``records_dir``, ``ports``);
    tears the whole stack down after.

    The knobs are the witnesses' preconditions, each off by default so the
    plain issuance test keeps its shape: ``publish_a=False`` withholds the
    apex/mail A records (pebble's HTTP-01 validation then fails — the paced-retry
    witness), ``nest_dns`` points the nest's OWN resolver at the fake_dns (the
    resolve-gated service names read it), ``data_dir`` bind-mounts ``/data`` from
    the host so a restart keeps it and the harness can read it, ``extra_ports``
    publishes more container ports (container → host), ``validity_secs`` shortens
    pebble's issued lifetime, and ``register=False`` leaves the domain for the
    test to register once its own precondition is in place."""
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
            json.dump(pebble_config(NEST_HTTP01_PORT, validity_secs=validity_secs), f)
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
        ports = {3000: api_port, **(extra_ports or {})}
        start_container_with_ports(
            nest_name,
            ports,
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
            },
            network=net,
            dns=dns_ip if nest_dns else None,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
            data_volume=data_dir,
        )
        started.append(nest_name)

        # Publish the SANs' A records → the nest's container IP, ASAP, so pebble's
        # HTTP-01 validation (which fires once the lifecycle task opens an order)
        # resolves them and reaches the nest's :8080 listener. Both the apex and
        # `mail.<domain>` are covered so issuance succeeds whether or not the
        # primary mail domain is registered before the order's SAN set is derived.
        nest_ip = container_ip(nest_name)
        if publish_a:
            write_dns_records(records_dir, a={DOMAIN: nest_ip, MX_SNI: nest_ip})

        wait_for_health(api_port, nest_name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")
        url = f"https://127.0.0.1:{api_port}"
        nest = {
            "name": nest_name, "port": api_port, "url": url, "admin": admin,
            "ip": nest_ip, "records_dir": records_dir, "ports": ports,
        }
        if register:
            register_primary_domain(nest, DOMAIN)
        yield nest
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


@pytest.fixture()
def acme_stack(docker_image):
    """The plain stack: A records published, the domain registered, so the
    nest's `cert_lifecycle_task` self-heals its boot self-signed floor into a
    real pebble-issued cert."""
    with pebble_nest_stack() as nest:
        yield nest


# ── Pebble lifecycle ──────────────────────────────────────────────────


# Kept under their old names: the sibling docker-tier ACME tests import them.
_start_pebble = start_pebble
_extract_pebble_ca = extract_pebble_ca


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


# ── Keeping the cert alive: the certificates-by-themselves witnesses ──────
#
# Each test below boots its own stack through `pebble_nest_stack` with the one
# precondition its outcome needs, and reads the result off the served TLS leaf,
# the nest's own state file, or the plain-http listener — never a timer.

# The ATProto PDS service's own name: the floor always carries it, the trusted
# cert takes it on only once the service is on AND the name resolves
# (`acme_http01::InfraSans`).
PDS_SNI = f"pds.{DOMAIN}"
# The nest's renewal lead (`acme::CERT_RENEWAL_LEAD_SECS`) — a leaf with less
# than this left is renewed on the lifecycle's next tick.
RENEWAL_LEAD_S = 30 * 24 * 3600
# The lifecycle's steady poll between ticks of a healthy cert (`acme_http01.rs`).
STEADY_POLL_S = 5 * 60
# The paced interval after one failed attempt: the window over the budget
# (`fauna_acme_http01::next_attempt_delay`, 3600 s / 4).
PACED_INTERVAL_S = 3600 // 4
RETRY_STATE = "acme-retry-state.json"
PACED_RESUME_LOG = "pacing first issuance attempt within the failed-validation budget after restart"


def _leaf_is_pebble(leaf) -> bool:
    return "Pebble" in leaf.issuer.rfc4514_string()


def _sans(leaf) -> list[str]:
    try:
        ext = leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName)
    except x509.ExtensionNotFound:
        return []
    return ext.value.get_values_for_type(x509.DNSName)


def _nest_log_tail(name: str, lines: int = 60) -> str:
    logs = subprocess.run(["docker", "logs", "--tail", str(lines), name],
                          capture_output=True, text=True, timeout=15)
    return logs.stdout + logs.stderr


def _await_pebble_leaf(nest, sni: str = DOMAIN, *, budget: float = 180, also=None,
                       what: str = "a pebble-issued leaf"):
    """The served leaf for `sni` once it is pebble-issued (and `also(leaf)`
    holds); the diagnosis names the last leaf and the nest's log tail."""
    host, port = "127.0.0.1", nest["port"]

    def _ready():
        leaf = served_leaf(host, port, sni)
        ok = _leaf_is_pebble(leaf) and (also is None or also(leaf))
        return leaf if ok else None

    def _diagnose():
        leaf = served_leaf(host, port, sni)
        return (
            f"never served {what} for {sni}: last issuer="
            f"{leaf.issuer.rfc4514_string()!r} sans={_sans(leaf)!r}\n"
            "--- nest logs (tail) ---\n" + _nest_log_tail(nest["name"])
        )

    return wait_until(_ready, budget, interval=2, diagnose=_diagnose)


def _retry_state(data_dir: str) -> dict:
    try:
        with open(os.path.join(data_dir, "acme", RETRY_STATE), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


@pytest.mark.feature("certificates-by-themselves")
def test_failed_issuance_stays_paced_across_a_restart_and_picks_up_the_fix(docker_image):
    """Outcome 7: a nest that cannot get a trusted certificate yet keeps trying
    at a pace the CA allows, even across restarts, and tries again within
    minutes once the cause is fixed (`tls-certificates.md` § Keeping the cert
    alive — the persisted `acme-retry-state.json` budget, resumed at task start).

    The cause is the commonest real one: the domain's A record does not point at
    the nest yet, so the CA's HTTP-01 validation fails. The first failure is the
    nest's own, recorded in its state file. The box is then stopped, the cause
    fixed (the record published) and the box started again — so an attempt
    fired straight after the restart would SUCCEED, and only a resumed budget
    keeps it waiting. One deliberate stand-in for the clock: while the box is
    down the recorded failure is aged so its paced retry falls due about a
    minute and a half after the restart instead of fifteen minutes; the nest
    still computes the wait itself, from its own file."""
    data_dir = tempfile.mkdtemp(prefix="fauna-acme-data-")
    try:
        with pebble_nest_stack(publish_a=False, data_dir=data_dir) as nest:
            name = nest["name"]

            # 1. The validation fails and the nest records it on its volume.
            state = wait_until(
                lambda: (s := _retry_state(data_dir)).get("recent_failures") and s,
                180, interval=2,
                diagnose=lambda: (
                    f"no failed attempt recorded in {RETRY_STATE}: {_retry_state(data_dir)!r}\n"
                    "--- nest logs (tail) ---\n" + _nest_log_tail(name)
                ),
            )
            assert len(state["recent_failures"]) == 1, state
            assert not _leaf_is_pebble(served_leaf("127.0.0.1", nest["port"], DOMAIN))

            # 2. Down; age the recorded failure; fix the cause while the box is off.
            subprocess.run(["docker", "stop", name], check=True, capture_output=True,
                           timeout=60)
            due_in = 90
            state["recent_failures"] = [int(time.time()) - PACED_INTERVAL_S + due_in]
            with open(os.path.join(data_dir, "acme", RETRY_STATE), "w",
                      encoding="utf-8") as f:
                json.dump(state, f)
            write_dns_records(nest["records_dir"],
                              a={DOMAIN: nest["ip"], MX_SNI: nest["ip"]})

            # 3. Up again: the restart resumes the budget, so the first attempt
            #    waits for its paced slot even though it would now succeed.
            subprocess.run(["docker", "start", name], check=True, capture_output=True,
                           timeout=60)
            wait_for_health(nest["port"], name)
            line = wait_until(
                lambda: next((ln for ln in _nest_log_tail(name, 400).splitlines()
                              if PACED_RESUME_LOG in ln), None),
                60, interval=1,
                diagnose=lambda: (
                    "the restarted nest did not pace its first attempt within the "
                    "budget it persisted — it reset the budget instead\n"
                    + _nest_log_tail(name)
                ),
            )
            m = re.search(r"wait_secs=(\d+)", re.sub(r"\x1b\[[0-9;]*m", "", line))
            assert m and 0 < int(m.group(1)) <= due_in, (
                f"the paced wait must come from the persisted failure: {line!r}")

            # 4. The paced attempt picks up the fix: a trusted cert within minutes,
            #    and the budget is healthy again.
            _await_pebble_leaf(nest, budget=due_in + 150,
                               what="the trusted cert the paced retry should obtain")
            after = _retry_state(data_dir)
            assert not after.get("recent_failures"), (
                f"a success leaves the failed-attempt budget healthy: {after!r}")
    finally:
        remove_data_dir(data_dir)


# Ten days: well inside the 30-day lead, so the first leaf is due at once.
SHORT_VALIDITY_S = 10 * 24 * 3600


@pytest.mark.feature("certificates-by-themselves")
def test_a_certificate_inside_its_renewal_lead_is_renewed_by_itself(docker_image):
    """Outcome 8, its first half: a public nest renews its trusted certificate
    by itself well before it expires (`tls-certificates.md` § Keeping the cert
    alive — renew inside the 30-day lead). Pebble issues ten-day leaves here, so
    the first one already sits inside the lead; with nobody acting, the
    lifecycle's next steady-poll tick replaces it with a new issuance for the
    same names. (The second half — re-issued when the served names change — is
    `test_domainless_add_domain_acme_serves_mail.py`'s second-domain test.)"""
    with pebble_nest_stack(validity_secs=SHORT_VALIDITY_S) as nest:
        first = _await_pebble_leaf(nest, what="the first trusted cert")
        now = datetime.datetime.now(datetime.timezone.utc)
        left = (first.not_valid_after_utc - now).total_seconds()
        assert 0 < left < RENEWAL_LEAD_S, (
            "the premise: pebble's short profile puts the first leaf inside the "
            f"renewal lead; it has {left / 86400:.1f} days left")

        renewed = _await_pebble_leaf(
            nest, budget=STEADY_POLL_S + 180,
            also=lambda leaf: leaf.serial_number != first.serial_number,
            what="a renewed cert (a new serial)",
        )
        assert set(_sans(renewed)) == set(_sans(first)), (
            f"the renewal must cover the same names: {_sans(first)!r} → {_sans(renewed)!r}")
        assert renewed.not_valid_after_utc > first.not_valid_after_utc
        # The admin's badge reads the renewed cert. A ten-day leaf is still
        # inside the lead, so it honestly says `Expiring` — trusted, not floor.
        def _status_reads_renewal():
            s = _cert_status(nest, [DOMAIN])[DOMAIN]
            ok = (s["is_floor"] is False and s["state"] == "Expiring"
                  and s["not_after_unix"] >= int(renewed.not_valid_after_utc.timestamp()))
            return s if ok else None

        wait_until(_status_reads_renewal, 30, interval=1,
                   diagnose=lambda: f"cert_status={_cert_status(nest, [DOMAIN])[DOMAIN]!r}")


def _approve_pds_bridge(nest) -> None:
    """Switch the ATProto PDS service on and approve its bridge — the condition
    the `pds.<apex>` SAN's first half reads (`pds_san_included`). It is the one
    role that is never auto-approved, so the admin approves the pending row."""
    enable_atproto_bridge(nest["name"])
    with admin_ws(nest) as admin:
        row = await_pending_enrollment(admin, roles=[ATPROTO_ROLE])[ATPROTO_ROLE]
        reply = admin.call(
            "fauna.bridges.approve_pending_bridge",
            {"ed25519_pubkey": bytes(row["ed25519_pubkey"]), "role": ATPROTO_ROLE},
        )
    assert reply.get("ok") is True, reply


@pytest.mark.feature("certificates-by-themselves")
def test_an_unresolved_service_name_never_holds_up_the_certificate_and_joins_once_it_resolves(
        docker_image):
    """Outcome 11: switching on a service with a name of its own before that
    name resolves never holds up the main certificate; the service's name joins
    the trusted certificate once it resolves (`tls-certificates.md` § B — the
    infra SANs are resolve-gated, because one unvalidatable name fails the whole
    all-or-nothing order). The nest resolves through the fake name server here,
    so its own gate sees exactly what the CA sees."""
    with pebble_nest_stack(nest_dns=True, register=False) as nest:
        # The service is on before the domain exists, so the very first order
        # is placed with it enabled and its name unpublished.
        _approve_pds_bridge(nest)
        register_primary_domain(nest, DOMAIN)

        first = _await_pebble_leaf(
            nest, also=lambda leaf: MX_SNI in _sans(leaf),
            what="the main trusted cert while the service name is unresolved",
        )
        assert PDS_SNI not in _sans(first), (
            f"the unresolved {PDS_SNI} was put into the order: {_sans(first)!r}")
        status = _cert_status(nest, [DOMAIN])[DOMAIN]
        assert status["state"] == "ValidTrusted" and status["is_floor"] is False, status

        # Publish the service's name: the next tick takes it into the cert.
        write_dns_records(nest["records_dir"],
                          a={DOMAIN: nest["ip"], MX_SNI: nest["ip"], PDS_SNI: nest["ip"]})
        joined = _await_pebble_leaf(
            nest, budget=STEADY_POLL_S + 180,
            also=lambda leaf: PDS_SNI in _sans(leaf),
            what=f"a trusted cert that {PDS_SNI} has joined",
        )
        assert {DOMAIN, MX_SNI} <= set(_sans(joined)), _sans(joined)


def _plain_http_get(port: int, host_header: str, path: str) -> tuple[int, str]:
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    try:
        conn.request("GET", path, headers={"Host": host_header})
        resp = conn.getresponse()
        resp.read()
        return resp.status, resp.getheader("Location") or ""
    finally:
        conn.close()


@pytest.mark.feature("certificates-by-themselves")
def test_plain_http_on_each_served_name_redirects_to_https_on_that_name(docker_image):
    """Outcome 13: opening any of the nest's names over plain http sends the
    visitor to the secure address on that same name (`domains-and-tls-bootstrap.md`
    § Implementation status today — the port-80 listener echoes the request's
    own host). Port 80 is the container's 8080 in a real deploy; it is published
    here the same way. The names are read off the served certificate, so every
    name the nest serves is checked, the service names included."""
    (http_port,) = find_free_ports(1)
    with pebble_nest_stack(extra_ports={NEST_HTTP01_PORT: http_port}) as nest:
        # The served cert once it carries the registered domain (the re-written
        # floor or the trusted cert, whichever lands first — both are served).
        def _served_names():
            n = _sans(served_leaf("127.0.0.1", nest["port"], DOMAIN))
            return n if {DOMAIN, MX_SNI} <= set(n) else None

        names = wait_until(
            _served_names, 60, interval=1,
            diagnose=lambda: (
                "the served cert never carried the registered domain: "
                f"{_sans(served_leaf('127.0.0.1', nest['port'], DOMAIN))!r}"),
        )
        path = "/some/page?x=1"
        for name in names:
            for host_header in (name, f"{name}:80"):
                status, location = _plain_http_get(http_port, host_header, path)
                assert status in (301, 302, 307, 308), (
                    f"plain http for Host {host_header!r} must redirect; got {status}")
                assert location == f"https://{name}{path}", (
                    f"Host {host_header!r} must redirect to https on that same name; "
                    f"got Location {location!r}")
