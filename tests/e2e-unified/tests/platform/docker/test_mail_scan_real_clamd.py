"""tier_4 e2e: an inbound message is scanned by the REAL shipped clamd and
delivered carrying ``X-Fauna-Scan-Clamav: clean`` — on this machine's
architecture.

Why this exists. On 2026-08-26 the compose bundle named ``clamav/clamav:latest``,
an amd64-only tag, beside a nest image published for arm64 too: the scan gate is
default-on and fail-closed, so on an arm64 box every inbound message would have
451'd forever at shipped defaults. The tag fix (the ``-debian`` family) and its
pin (the installer self-test — every shipped scanner ref covers every
nest platform) are reasoning over registry metadata; ``test_bundle_scanner_probe_docker.py``
boots the sidecars alone and probes clamd's wire. Neither delivers mail. This
test is the end-to-end witness the owner doc asks for: the published nest image
and the clamd image the bundle ships, both at this host's architecture, one
clean message in over SMTP and read back over IMAPS with the clamd verdict
header stamped by the real daemon.

What it asserts:

  1. **Clean in, header out.** A plain inbound is accepted (250) and read back
     decrypted over IMAPS carrying ``X-Fauna-Scan-Clamav: clean``.
  2. **The verdict is the real daemon's (anti-vacuity).** An inbound carrying the
     EICAR test vector is refused at DATA. Fauna's own ``fakes/fake_clamd`` answers
     ``OK`` to EICAR, so a refusal here can only come from a real signature DB —
     without it, (1)'s ``clean`` could be a stub's.

The clamd image is resolved from ``docker-compose.yml``'s own digest-pinned ref:
the leg of that index matching the host architecture, pulled by digest (so a
tag repointed since the pin cannot slip in), run with the bundle's shipped
defaults plus ``no-new-privileges``, and REMOVED — container and image — when
the test ends. rspamd stays Fauna's fake: this test is about clamd, and the
fake rspamd answers the same wire the scan gate dials.

**Opt-in, not default (``FAUNA_E2E_REAL_CLAMD=1``).** Pulling and running a
third-party image on a dev machine needs the user's approval of that image
(no unvetted third-party software runs on a dev machine unasked), so an
ordinary tier_4 run skips this.
``FAUNA_E2E_REAL_CLAMD_DIGEST``, when set, is the per-platform digest the
approval named: the test stops BEFORE pulling if the pinned index resolves to
anything else.

First boot runs freshclam, which downloads the signature DB from the ClamAV
mirrors (outbound network), so the clamd readiness wait is minutes, bounded by
the image's own HEALTHCHECK (convention 14: a deadline poll, never a sleep).

Spec: ``docs/goal/behavior/mail-content-scanning.md`` § ClamAV configuration +
§ Implementation status today; ``docs/goal/architecture/installers/docker.md``
§ Platform Support.
"""

import json
import os
import platform
import re
import subprocess
import time

import pytest

from .helpers import (
    fence_cpuset_args,
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    deliver_inbound_loopback_curl,
    deliver_inbound_raw_loopback_curl,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_recipient,
    register_primary_domain,
    relax_spam_policy,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
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
    pytest.mark.skipif(
        os.environ.get("FAUNA_E2E_REAL_CLAMD") != "1",
        reason="pulls and runs the third-party clamd image — opt in with "
               "FAUNA_E2E_REAL_CLAMD=1 once the user has approved that image",
    ),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"
CLAIM_CODE = "CLAMD1"
RECIPIENT_LOCAL = "scan-user"
RECIPIENT_PASSWORD = "real-clamd-pass-password-1"
SENDER_DOMAIN = "sender-ok.test"  # SPF authorizes the loopback client IP

#: The image's HEALTHCHECK covers freshclam's first signature download.
CLAMD_READY_DEADLINE_S = 900


def _shipped_clamd_ref() -> str:
    """``docker-compose.yml``'s clamd image ref — the bundle's own pin."""
    compose = (get_repo_root() / "docker-compose.yml").read_text()
    m = re.search(r"^\s*image:\s*(clamav/clamav\S*)\s*$", compose, re.M)
    assert m, "docker-compose.yml names no clamav/clamav image"
    return m.group(1)


def _host_arch() -> str:
    machine = platform.machine().lower()
    return {"aarch64": "arm64", "arm64": "arm64", "x86_64": "amd64"}.get(machine, machine)


def _repo(ref: str) -> str:
    """``publisher/name`` of a ``publisher/name[:tag][@digest]`` ref."""
    return ref.split("@")[0].split(":")[0]


def _platform_digest(ref: str, arch: str) -> str:
    """The ``linux/<arch>`` manifest digest inside ``ref``'s index (metadata only).

    A ``tag@digest`` ref is inspected as ``repo@digest``: ``docker manifest
    inspect`` refuses the combined form ("manifest verification failed"), and the
    digest alone is what the pin means anyway."""
    if "@" in ref:
        ref = f"{_repo(ref)}@{ref.split('@', 1)[1]}"
    r = subprocess.run(["docker", "manifest", "inspect", ref],
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 0, f"manifest inspect {ref} failed: {r.stderr}"
    for m in json.loads(r.stdout).get("manifests", []):
        p = m.get("platform", {})
        if p.get("os") == "linux" and p.get("architecture") == arch:
            return m["digest"]
    raise AssertionError(f"{ref} publishes no linux/{arch} manifest — the bundle "
                         f"cannot run on this architecture")


def _eicar() -> bytes:
    """The EICAR anti-malware test vector, assembled at run time.

    Never one literal in a tracked file: dev boxes run real antivirus, and a
    checked-in EICAR string gets the source file quarantined (the scanner
    probe and ``fakes/fake_clamd.py`` avoid it the same way). It is not
    malware — it is the standard 68-byte string every scanner must report."""
    return (
        b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$"
        + b"EICAR" + b"-STANDARD-" + b"ANTIVIRUS-" + b"TEST-FILE!"
        + b"$H+H*"
    )


def _wait_healthy(name: str, deadline_s: float) -> None:
    deadline = time.monotonic() + deadline_s
    status = ""
    while time.monotonic() < deadline:
        r = subprocess.run(["docker", "inspect", "-f", "{{.State.Health.Status}}", name],
                           capture_output=True, text=True, timeout=30)
        status = r.stdout.strip()
        if status == "healthy":
            return
        time.sleep(5)  # sleep-ok: poll interval of a deadline poll on docker's own HEALTHCHECK state
    logs = subprocess.run(["docker", "logs", "--tail", "60", name],
                          capture_output=True, text=True, timeout=30)
    raise AssertionError(f"clamd {name} not healthy within {deadline_s}s "
                         f"(last status {status!r}):\n{logs.stdout}\n{logs.stderr}")


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def real_clamd_nest(docker_image):
    """A claimed nest on a network with the REAL shipped clamd (host-arch leg,
    pulled by digest), the fake rspamd and a fake DNS resolver. Container AND
    clamd image are removed afterwards."""
    ref = _shipped_clamd_ref()
    digest = _platform_digest(ref, _host_arch())
    expected = os.environ.get("FAUNA_E2E_REAL_CLAMD_DIGEST")
    if expected:
        assert digest == expected, (
            f"{ref} resolves to {digest} for linux/{_host_arch()}, not the approved "
            f"{expected} — stopping before any pull; ask the user again")
    clamd_image = f"{_repo(ref)}@{digest}"

    suffix = find_free_port()
    network = f"fauna-clamd-net-{suffix}"
    dns_name = f"fauna-clamd-dns-{suffix}"
    clamd_name = f"fauna-clamd-real-{suffix}"
    fake_clamd_name = f"fauna-clamd-fake-{suffix}"
    rspamd_name = f"fauna-clamd-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-mail-clamd-{http_port}"
    create_network(network)
    try:
        r = subprocess.run(["docker", "pull", clamd_image],
                           capture_output=True, text=True, timeout=900)
        assert r.returncode == 0, f"pull {clamd_image} failed: {r.stderr}"
        r = subprocess.run(
            ["docker", "run", "-d", *fence_cpuset_args(), "--name", clamd_name, "--network", network,
             "--security-opt", "no-new-privileges", clamd_image],
            capture_output=True, text=True, timeout=60)
        assert r.returncode == 0, f"run {clamd_image} failed: {r.stderr}"
        # rspamd from the fakes; their fake clamd is started and left unused.
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=fake_clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name,
            txt_records={SENDER_DOMAIN: "v=spf1 ip4:127.0.0.1 -all"})
        _wait_healthy(clamd_name, CLAMD_READY_DEADLINE_S)
        start_container_with_ports(
            name,
            {3000: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_CLAMD_ADDR": f"{container_ip(clamd_name)}:3310",
                "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
            },
            network=network,
            dns=dns_ip,
        )
        try:
            wait_for_health(http_port, name)
            admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
            yield {
                "name": name,
                "port": http_port,
                "url": f"https://127.0.0.1:{http_port}",
                "admin": admin,
                "mail_ports": mail_ports,
                "clamd_image": clamd_image,
            }
        finally:
            remove_container(name)
    finally:
        for c in (dns_name, clamd_name, fake_clamd_name, rspamd_name):
            remove_container(c)
        remove_network(network)
        subprocess.run(["docker", "image", "rm", clamd_image],
                       capture_output=True, text=True, timeout=120)


def test_real_clamd_stamps_clean_and_refuses_eicar(real_clamd_nest, run_seal_helper):
    nest = real_clamd_nest
    name = nest["name"]
    mp = nest["mail_ports"]

    register_primary_domain(nest, DOMAIN)
    relax_spam_policy(nest)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=RECIPIENT_LOCAL,
        password=RECIPIENT_PASSWORD,
    )
    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    diag = lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"  # noqa: E731
    arch = _host_arch()

    # (1) Clean in → accepted, read back with the real daemon's verdict header.
    subject = f"real clamd witness {arch}"
    try:
        deliver_inbound_loopback_curl(
            name,
            mail_from=f"bob@{SENDER_DOMAIN}",
            rcpt_to=recipient["username"],
            subject=subject,
            body_text="A clean message for the real scanner.\n",
        )
    except RuntimeError as e:
        raise AssertionError(
            f"CLEAN INBOUND REFUSED on {arch} with the shipped clamd "
            f"({nest['clamd_image']}): a 451 here is the 2026-08-26 failure — the "
            f"scan gate could not get a verdict.\n{e}{diag()}") from e
    raw = imap_fetch_only_inbox_message(
        "127.0.0.1", mp[993], DOMAIN, recipient["username"], recipient["password"])
    text = raw.decode("utf-8", errors="replace")
    assert subject in text, f"read-back lacks the Subject: {text[:400]!r}{diag()}"
    assert re.search(r"^X-Fauna-Scan-Clamav: clean\r?$", text, re.M), (
        f"the delivered message must carry `X-Fauna-Scan-Clamav: clean` from the "
        f"real clamd; headers: {text[:800]!r}{diag()}")

    # (2) EICAR in → refused. The fake clamd would say OK; only a real DB says FOUND.
    raw_eicar = (
        f"From: bob@{SENDER_DOMAIN}\r\n"
        f"To: {recipient['username']}\r\n"
        f"Subject: real clamd eicar control\r\n"
        f"Message-ID: <eicar-{int(time.time())}@{SENDER_DOMAIN}>\r\n"
        f"\r\n"
    ).encode() + _eicar() + b"\r\n"
    rc, output = deliver_inbound_raw_loopback_curl(
        name, mail_from=f"bob@{SENDER_DOMAIN}", rcpt_to=recipient["username"],
        raw=raw_eicar)
    assert rc != 0, (
        f"EICAR was ACCEPTED — the verdict is not a real signature DB's, so the "
        f"`clean` above proves nothing. Output: {output!r}{diag()}")
    assert re.search(r"\b5\d\d\b", output), (
        f"EICAR must be refused permanently (5xx, clamav policy `reject`), not "
        f"tempfailed; got {output!r}{diag()}")
