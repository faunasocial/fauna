"""Shared Docker test helpers.

Provides image build, container lifecycle, health checks, and port
allocation for all Docker-based e2e tests.
"""

import contextlib
import datetime
import email.message
import email.utils
import functools
import json
import os
import secrets
import socket
import ssl
import subprocess
import sys
import time
import urllib.request
import uuid
from pathlib import Path

from drivers.browser import launch_kwargs as browser_launch_kwargs
from helpers.mail_aliases import add_exact_alias_as
from helpers.recipient_seal_key import provision_recipient_seal_key


IMAGE_TAG = "fauna-nest-test:local"
BUILD_TIMEOUT = 1800
HEALTH_TIMEOUT = 60


def get_repo_root() -> Path:
    here = Path(__file__).resolve().parent
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True,
        cwd=here,
    )
    return Path(result.stdout.strip())


def fence_cpuset_args() -> list[str]:
    """``["--cpuset-cpus", <the build helper's CPU set>]`` for every container the
    harness ``docker run -d``s, or ``[]`` where there is no CPU set to join. Docker
    places a container in ``system.slice``, outside the two slices that set
    covers, so without this a harness nest runs on every CPU while that set
    shrinks around it; a background agent moves the labelled ones
    (``fauna-e2e``) with the set after that. The set is read through
    ``scripts/build-slot.py`` (``_fence_cpuset_spec``); a checkout without the
    fleet tooling, or one whose copy predates it, pins nothing. Read at each
    start: the fence moves while a run lasts."""
    mod = _build_slot()
    cpus = getattr(mod, "_fence_cpuset_spec", lambda: None)() if mod else None
    return ["--cpuset-cpus", cpus] if cpus else []


@functools.cache
def _build_slot():
    """This checkout's ``scripts/build-slot.py``, imported once, or None."""
    slot = Path(__file__).resolve().parents[5] / "scripts" / "build-slot.py"
    if not slot.exists():
        return None
    import importlib.util
    spec = importlib.util.spec_from_file_location("fauna_build_slot_fence", str(slot))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def find_free_port() -> int:
    with socket.socket() as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def find_free_ports(count: int) -> list[int]:
    """Allocate ``count`` distinct free ports, holding every socket open
    simultaneously so the kernel can't hand out the same ephemeral port
    twice, then releasing them. (Calling ``find_free_port`` N times in a row
    can return duplicates when the OS reuses a just-closed port — a problem
    when one ``docker run`` maps several host ports at once.)"""
    socks = []
    try:
        for _ in range(count):
            s = socket.socket()
            s.bind(("", 0))
            socks.append(s)
        return [s.getsockname()[1] for s in socks]
    finally:
        for s in socks:
            s.close()


#: How far the tag may lag HEAD before the run says so. Not a refusal threshold:
#: an older image still proves plenty, and rebuilding is barred on a dev VM
#: (below), so the honest move is to make the drift visible and let the reader
#: judge. Chosen as "older than a working week" — long enough that an ordinary
#: pull-and-retag keeps every run quiet, short enough that a *wire* change is
#: unlikely to have slipped underneath it unnoticed.
STALE_IMAGE_DAYS = 7

_age_reported = False


def report_image_age(created_iso: str) -> str | None:
    """Say how far `IMAGE_TAG` lags HEAD, once per run, when it lags at all.

    **The failure this exists to name.** `docker_build` reuses whatever image
    carries the tag, and its own contract says it "cannot know whether a local
    image predates the commit under test". On 2026-08-29 that abdication cost a
    session: the tag on the primary dev VM had been built 2026-08-14, three days
    before the actor key went tagged-only (and its transition machinery was
    deleted), so every authenticated call to it answers
    `fauna.auth.signature_failed`. A docker-mode sweep recorded 11 of those as an
    unexplained tail. The controlled differential is stark — the `tests/api/`
    slice is 107 passed against the pulled release image and **108 errors, every
    one `signature_failed`**, against the stale tag, same tests and same commit.

    A wire-affecting commit between the image and HEAD makes EVERY test in this
    package fail in a way that reads like a product defect, so a run that cannot
    rule that out must at least say so out loud. Returns the message it printed,
    or `None` when the image is current enough to keep quiet.
    """
    global _age_reported
    if _age_reported or not created_iso:
        return None
    try:
        stamp = created_iso.split(".")[0].rstrip("Z")
        built = datetime.datetime.fromisoformat(stamp)
    except ValueError:
        return None
    days = (datetime.datetime.now() - built).days
    if days < STALE_IMAGE_DAYS:
        return None
    _age_reported = True
    message = (
        f"\n⚠ {IMAGE_TAG} was built {days} days ago ({built.date()}), and this "
        f"package's tests run against IT, not against anything the run names.\n"
        f"  A wire-affecting commit since then makes every authenticated call "
        f"fail in a way that reads like a product defect — measured "
        f"2026-08-29: a 15-day-old tag turned a 107-passed slice into 108 "
        f"`fauna.auth.signature_failed` errors.\n"
        f"  Refresh it with:  docker pull ghcr.io/faunasocial/nest:dev && "
        f"docker tag ghcr.io/faunasocial/nest:dev {IMAGE_TAG}\n"
        f"  (`:dev` is every image build's push; `:latest` moves only on a "
        f"production promote and can itself be weeks behind.)\n"
    )
    print(message, file=sys.stderr)
    return message


def docker_build(repo_root: Path) -> str:
    """Build the Docker image and return the tag.

    **This helper does not build the nest image, by design** — the same rule the
    nest-mode axis's ``_DockerProvider`` already states, now applied to the tier_4
    suite that used to contradict it. Building ``ghcr.io/faunasocial/nest`` belongs
    to the self-hosted runner via ``build-nest-image.yml`` (``build-system.md``
    § Image tags & channels): the three dev VMs share one physical host, so a build
    here starves every sibling session — the standing user stop of 2026-06-13.

    That contradiction had teeth. ``tests/platform/docker/`` modules carry
    ``@pytest.mark.feature``, so a ``--feature`` selected docker-mode sweep pulls
    them in like any other test — and on 2026-08-28 one silently started
    ``docker buildx build`` on the primary dev VM (twice; it respawned when the
    first was killed) while sibling sessions queued for build slots on that box.

    An absent image is therefore a loud refusal naming both remedies, never a
    20-minute surprise build. ``FAUNA_ALLOW_NEST_IMAGE_BUILD=1`` is the explicit
    opt-in for the machine whose job that is.

    ``FAUNA_REUSE_IMAGE=1`` was the old opt-in for this behaviour; reuse is now
    the default, so the variable is accepted and ignored.

    **The tag's currency is still the caller's to own, but no longer silently
    so.** This helper cannot know whether the image implements the commit under
    test — a run wanting a *known* artifact should pass ``--nest docker:<ref>``
    against a pulled published image instead. What it can know, and now says, is
    how OLD the tag is: that abdication cost a session on 2026-08-29, when a
    15-day-old tag predating the tagged-only actor key turned every
    authenticated call into ``fauna.auth.signature_failed`` and a docker-mode
    sweep filed 11 of them as an unexplained tail. See ``report_image_age``.
    """
    created = subprocess.run(
        ["docker", "image", "inspect", "--format", "{{.Created}}", IMAGE_TAG],
        capture_output=True, text=True, timeout=30,
    )
    if created.returncode == 0:
        report_image_age(created.stdout.strip())
        return IMAGE_TAG

    if os.environ.get("FAUNA_ALLOW_NEST_IMAGE_BUILD") != "1":
        raise RuntimeError(
            f"the tier_4 docker suite needs the image {IMAGE_TAG!r}, which is "
            f"not present on this machine. This helper deliberately does NOT "
            f"build it: building the nest image on a dev VM is forbidden — the "
            f"dev VMs share one physical host and the build starves them "
            f"(build-system.md § Image tags & channels; the 2026-06-13 user "
            f"stop). Either\n"
            f"  • docker pull ghcr.io/faunasocial/nest:latest, then retag it "
            f"{IMAGE_TAG!r}, or\n"
            f"  • dispatch a real build: gh workflow run build-nest-image.yml "
            f"--ref main, then pull the resulting tag.\n"
            f"The machine whose job IS building the image sets "
            f"FAUNA_ALLOW_NEST_IMAGE_BUILD=1."
        )

    head_sha = subprocess.run(
        ["git", "-C", str(repo_root), "rev-parse", "HEAD"],
        capture_output=True, text=True, timeout=30,
    ).stdout.strip()
    result = subprocess.run(
        [
            "docker", "buildx", "build",
            "-t", IMAGE_TAG,
            # Same value the release-candidate gate reads off `.Config.Env`
            # (feature_ledger.py::release_candidate_refusal_for_image) and the
            # new `org.opencontainers.image.revision` label reads from — a
            # build through this helper should be self-identifying too,
            # not just the pipeline's (testing.md class (7)).
            "--build-arg", f"FAUNA_BUILD_COMMIT={head_sha or 'dev'}",
            "--load", str(repo_root),
        ],
        capture_output=True, text=True,
        timeout=BUILD_TIMEOUT,
    )
    if result.returncode != 0:
        raise RuntimeError(f"docker build failed:\n{result.stderr[-2000:]}")
    return IMAGE_TAG


def remove_container(name: str):
    """Force-remove a container by name, ignoring errors."""
    subprocess.run(
        ["docker", "rm", "-f", name],
        capture_output=True, timeout=30,
    )


def start_container(name: str, port: int, env: dict | None = None) -> str:
    """Start a fauna-nest container. Returns container ID."""
    remove_container(name)
    # FAUNA_PORT=3000: the production nest internal port (the entrypoint /
    # Dockerfile HEALTHCHECK / SNI-router default). Set explicitly — and the
    # host-port mapping below targets 3000 — so the in-container listener
    # matches the real deploy and the tier_4 image tests mirror production.
    cmd = [
        "docker", "run", "-d", *fence_cpuset_args(),
        "--name", name,
        "-p", f"127.0.0.1:{port}:3000",
        "-e", "FAUNA_PORT=3000",
    ]
    for k, v in (env or {}).items():
        cmd.extend(["-e", f"{k}={v}"])
    cmd.append(IMAGE_TAG)
    result = subprocess.run(
        cmd, capture_output=True, text=True, timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(f"docker run failed:\n{result.stderr}")
    return result.stdout.strip()


def wait_for_health(port: int, container_name: str,
                    timeout: float = HEALTH_TIMEOUT):
    """Poll until the nest's health endpoint responds.

    A deploy nest with a configured domain serves **https** on its main port
    (a real ACME cert, or — for a localhost/LAN/IP deploy with ACME off — a
    self-signed bootstrap cert it synthesizes at boot so the in-container mail
    bridge can dial it over loopback TLS; see `nest::main` self-hosted bootstrap
    and `mail-bridge-lifecycle.md` § TLS provisioning). Probe https (cert
    verification off — the cert is self-signed) and fall back to http so a
    future plain-HTTP deploy still works; mirrors the image's own
    dual-scheme HEALTHCHECK."""
    _unverified = ssl._create_unverified_context()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        for url, ctx in (
            (f"https://127.0.0.1:{port}/api/v1/health", _unverified),
            (f"http://127.0.0.1:{port}/api/v1/health", None),
        ):
            try:
                resp = urllib.request.urlopen(url, timeout=5, context=ctx)
                if resp.status == 200:
                    _note_booted_artifact(container_name, resp)
                    return
            except Exception:
                pass
        time.sleep(1.0)
    logs = subprocess.run(
        ["docker", "logs", container_name],
        capture_output=True, text=True, timeout=10,
    )
    out = logs.stdout + logs.stderr
    # When the nest fails to boot (e.g. a migration panic on an upgraded /data),
    # the in-container mail bridge spams "connection refused" dialing it, which
    # fills the tail and buries the nest's actual error. Surface the nest's
    # boot/migration lines from the FULL log alongside the raw tail.
    keys = ("migrat", "reconcil", "panic", "sqlite", "no such", "fatal",
            "thread '", "backtrace", "segment_record", "record_cid", "record_id",
            "constraint", "nest.db", "claim", "run_migrations")
    nest_lines = [ln for ln in out.splitlines() if any(k in ln.lower() for k in keys)]
    raise TimeoutError(
        f"Nest on port {port} did not start within {timeout}s.\n"
        f"── nest boot/migration lines (full log) ──\n" + "\n".join(nest_lines[-50:]) +
        f"\n── raw tail ──\n{out[-1200:]}"
    )


def _note_booted_artifact(container_name: str, resp) -> None:
    """Tell the feature ledger what artifact just came up.

    `wait_for_health` is the one door every container fixture in this package
    passes through, and the health body is the image's own word on its `version`
    and `commit` — which is what a record of an own-image test has to carry in
    place of the run's (`helpers/feature_ledger.note_container`; the fixture that
    is being set up owns the note). Bookkeeping never fails a test: a capture that
    goes wrong leaves the ledger to DECLINE the record, out loud in the terminal
    summary, rather than guess.
    """
    try:
        body = json.loads(resp.read().decode("utf-8", "replace"))
        if not isinstance(body, dict):
            body = {}
    except Exception:
        body = {}
    try:
        from helpers import feature_ledger

        feature_ledger.note_container(container_name, body)
    except Exception:
        pass


def generate_claim_code() -> str:
    """Generate a 40-bit claim code in the shared display format.

    Mirrors ``fauna_core::claim_code::generate``: 8 chars from the
    ambiguity-free 32-symbol alphabet (A–Z without I/O, plus 2–9), grouped in
    hyphenated 4-char runs — i.e. ``ABCD-EFGH``. The nest accepts whatever we
    write to ``/data/claim-code`` (it normalizes both sides), but using the real
    shape exercises the hyphenated code end-to-end through the web input.

    Kept in step with the Rust generator by hand — if ``CODE_LEN`` moves there,
    move it here. (Length went 26 → 8 on 2026-07-24; see that module's doc for
    why, and why the claim throttles must not be loosened at this length.)
    """
    alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
    raw = "".join(secrets.choice(alphabet) for _ in range(8))
    return "-".join(raw[i : i + 4] for i in range(0, len(raw), 4))


def launch_browser(playwright):
    """Launch Chromium via the shared resolution order (`drivers/browser.py`):
    Playwright's bundled build first — no machine-wide singleton, so this needs
    no serialization against a concurrent `--client web` run or sibling
    session (the snap-Chromium flock was removed 2026-07-14; testing.md
    § Cross-app e2e conventions, point 9)."""
    return playwright.chromium.launch(**browser_launch_kwargs(playwright))


@contextlib.contextmanager
def browser_session(ignore_https_errors: bool = True):
    """Yield a Playwright `page`, closing the browser on exit (even if the
    body raises, so an assertion failure mid-flow never leaks a browser).

    `ignore_https_errors=True` is the default because tier_4 nests serve a
    self-signed bootstrap cert (the deploy image's TLS bootstrap).
    """
    from playwright.sync_api import sync_playwright

    with sync_playwright() as p:
        browser = launch_browser(p)
        try:
            yield browser.new_page(ignore_https_errors=ignore_https_errors)
        finally:
            try:
                browser.close()
            except Exception:
                pass


def assert_s6_service_up(container_name: str, service: str):
    """Assert an s6 service is running inside the container."""
    result = subprocess.run(
        ["docker", "exec", container_name,
         "/command/s6-svstat", f"/run/service/{service}"],
        capture_output=True, text=True, timeout=10,
    )
    assert result.returncode == 0, (
        f"s6-svstat failed for {service}: {result.stderr}"
    )
    assert "up" in result.stdout, (
        f"Service {service} is not up: {result.stdout}"
    )


def claim_admin_api(port: int, claim_code: str, handle: str | None = "admin",
                    mail_domain: str | None = None) -> dict:
    """Claim admin on a nest via the ``fauna.auth.claim_admin`` WS-RPC kind.

    A thin wrapper over ``common.auth.claim_admin`` — the ONE admin contract
    every nest mode answers (``nest_instance["admin"]``). It was a second
    implementation of the same round trip until 2026-08-28, and the two return
    dicts had drifted: this one answered ``secret_hex``/``domain`` but not
    ``actor_id_bytes``/``deployment_seed``, so every mail-bridge fixture died
    under ``--nest docker`` on a bare ``KeyError: 'actor_id_bytes'``. What
    remains here is the one genuine difference — the container's listener always
    serves TLS, so the base URL is pinned ``https://`` rather than resolved.

    ``handle`` is **required by the wire** (the claim type-promotion made a
    handle-less ``ClaimAdminRequest`` fail to decode → ``fauna.protocol.malformed``)
    and becomes the email local-part; it defaults to ``"admin"``. Pass ``None``
    only to deliberately omit it (e.g. a negative decode test).

    ``mail_domain`` is the **domained claim** the domainless-boot model turns on:
    a real (non-local) domain carried here becomes the primary ``mail_domains``
    row = the deployment identity (``claim_core.rs`` →
    ``identity_domain_core::apply_primary_identity``), so the box acquires that
    domain's identity + ACME at claim without any ``FAUNA_DOMAIN`` boot env — the
    tier_4 twin of the tier_3 ``test_claim_primary_domain.py`` domained claim.
    Omitted (the default) claims a bare handle → the ``localhost`` identity
    fallback. ``reply["domain"]`` (``state.handle_domain()``, the just-registered
    primary) is returned as ``domain``.

    Drives the pre-identity anonymous WS connection — the
    `POST /api/v1/claim-admin` HTTP twin was removed in S4d
    (tracked internally). The same anonymous client already talks to
    docker-published nest ports for ``fauna.setup.status`` elsewhere in this
    suite. Raises ``RpcCallError`` on a server-side rejection.
    """
    from common.auth import claim_admin

    return claim_admin(
        port,
        claim_code,
        base_url=f"https://127.0.0.1:{port}",
        handle=handle,
        mail_domain=mail_domain,
    )


def factory_reset_and_reclaim_docker(nest, *, claim_code: str, timeout: float = 180.0):
    """Drive ``fauna.admin.factory_reset`` against a docker-image nest, let the
    **real s6 supervisor restart the nest**, and re-claim with the SAME admin
    identity — the tier_4 faithful twin of ``common.nest.factory_reset_and_restart``
    (tier_3).

    The crucial tier_4 difference: the tier_3 helper runs against a bare binary
    with no supervisor, so it re-spawns the binary ITSELF to run the boot-time
    wipe. Here ``fauna-nest`` is an s6 ``longrun`` (``docker/s6/fauna-nest``), so
    the handler's post-reply ``exit(0)`` is followed by s6 restarting the process
    for real into ``factory_reset::maybe_run_factory_reset`` — exactly the
    production restart-wipe (``docs/goal/architecture/nest/common.md`` § Factory
    reset). We only drive the reset, then wait for the nest to cycle back to
    fresh/unclaimed (reusing the shared ``_wait_nest_fresh`` poll, which tolerates
    the mid-restart connection window), then re-claim. The container itself stays
    up throughout — only the nest process restarts inside it.

    ``nest`` is mutated in place (new ``admin``); ``claim_code`` is pinned as the
    post-reset code so the re-claim reuses it. The wipe also clears local domains /
    storage mode / mail enablement, so the caller re-establishes those after.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import claim_admin
    from common.nest import _wait_nest_fresh

    admin_sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    ) as adm:
        reply = adm.call("fauna.admin.factory_reset", {"new_claim_code": claim_code})
    assert reply.get("claim_code") == claim_code, (
        f"factory_reset should echo the pinned claim code: {reply!r}"
    )
    # The handler exits ~750ms after replying; s6 restarts fauna-nest into the
    # boot-time wipe and it comes back fresh/unclaimed.
    _wait_nest_fresh(nest["url"], timeout=timeout)
    nest["admin"] = claim_admin(
        nest["port"], claim_code, base_url=nest["url"], signing_key=admin_sk
    )
    return nest


def start_container_with_ports(
    name: str,
    port_map: dict[int, int],
    env: dict | None = None,
    add_hosts: dict[str, str] | None = None,
    network: str | None = None,
    dns: str | None = None,
    mounts: list[tuple[str, str]] | None = None,
    data_volume: str | None = None,
    image: str = IMAGE_TAG,
    labels: dict | None = None,
    publish_host: str = "127.0.0.1",
    udp_port_map: dict[int, int] | None = None,
) -> str:
    """Start a fauna-nest container mapping container_port→host_port.

    ``image`` overrides the locally-built ``IMAGE_TAG`` — the live private-relay
    test runs the production ``ghcr.io/faunasocial/nest:latest`` pull directly,
    without retagging over a sibling session's locally-built test image.

    ``port_map`` keys are in-container ports (the nest listener, 25/465/587 SMTP,
    993 IMAPS, …), values the ``127.0.0.1`` host ports to bind them to.
    Returns container ID. Callers needing the nest API reachable must publish the
    nest's listener port and pass a matching ``FAUNA_PORT`` in ``env``.

    **Nest port.** Production binds the nest at ``3000`` (``FAUNA_PORT``, the
    entrypoint/HEALTHCHECK/router default) — kept off ``8443`` so ``8443`` means
    *CalDAV only* (a bare-IP box serves user-facing CalDAV at ``<ip>:8443``).
    The docker tests here publish ``{3000: …}`` + ``FAUNA_PORT=3000`` for the
    nest so the tier_4 suite mirrors the real deploy port. The internal nest
    port is pure IPC (callers reach the nest via the host port, and every
    in-container consumer — the SNI router, the bridge run-scripts, the
    entrypoint — dials ``${FAUNA_PORT:-3000}``), but it matters concretely when
    ``FAUNA_LAN_BIND_IP`` is set: then the MDA binds CalDAV directly on the LAN
    interface at the admin port (default ``8443``), so the nest **must** be on
    ``3000`` (or CalDAV on a non-8443 port) or the two ``EADDRINUSE``-collide
    (the home-relay bug fixed 2026-06-20; see ``test_mail_relay_two_nest.py``).

    ``add_hosts`` maps a hostname to a docker ``--add-host`` value; pass
    ``{"host.docker.internal": "host-gateway"}`` so a name like
    ``host.docker.internal`` *resolves* in the container (e.g. for the
    sender-domain DNS check — note the host itself may not be *reachable* from
    the container on this docker setup). ``network`` joins a user-defined
    network so the container can reach sidecars by name (the inbound round-trip's
    fake clamd/rspamd, which the nest can't reach on the host).

    ``dns`` sets the container's upstream nameserver (``docker run --dns``) — used
    by the anti-spoofing guard to point ``verify_inbound``'s resolver
    (``new_system_conf()``, which has no env override) at a ``fake_dns`` sidecar
    publishing a ``p=reject`` DMARC policy. Docker's embedded resolver still
    answers on-network container names and forwards everything else to ``dns``.

    ``mounts`` is a list of ``(host_path, container_path)`` pairs bind-mounted
    read-only (``-v host:container:ro``) — e.g. the ACME tier_4 test injects a
    pebble-CA-augmented trust bundle the nest reads via ``SSL_CERT_FILE``.

    ``data_volume`` mounts ``/data`` read-write so it persists across container
    removals — the schema-upgrade test reuses one across two container boots
    (serve → rewrite ``nest.db`` to an older schema → redeploy a fresh container
    on the same volume). Docker reads a ``/``-bearing value as a **host path**
    bind-mount and a bare name as a **named volume**; the nest-mode axis's
    ``_DockerProvider`` passes a host path deliberately, so ``nest.db`` stays
    readable from the harness (the image's ``fauna`` user is uid 1000, the same
    uid the dev VMs run as) and ``db_path`` stays a real capability in docker
    mode rather than a declared absence.

    ``publish_host`` is the host interface the ports are published on. It stays
    ``127.0.0.1`` for every ordinary container — a harness nest has no business
    on the LAN — and widens to ``0.0.0.0`` for the nest-mode axis's ``dial_host``
    option, which hands a CLIENT a non-loopback authority so it takes its
    SPKI-**pin** trust branch instead of its loopback short-circuit. Widening
    rather than *moving* the publication is the exact mirror of what
    ``common.nest.start_nest`` does to the standalone nest's own bind, and it is
    what keeps the harness's own health probe and claim call on loopback: those
    are not the thing under test.

    ``udp_port_map`` publishes UDP ports the same way (``port_map`` is TCP) —
    the relay's address-discovery port is the one UDP port the image serves.

    ``labels`` are ``docker run --label`` pairs. The nest-mode axis stamps every
    container it starts with ``fauna-e2e=1`` and ``fauna-e2e-run=<run id>``
    (``testing.md`` § Default app and nest mode, ruling (4)): convention 9 reaps
    every harness child with its process group, and a container is the one child
    the kernel does not reach, so an owner label is what makes a leaked one
    reclaimable by a later run instead of by a human.
    """
    remove_container(name)
    cmd = ["docker", "run", "-d", *fence_cpuset_args(), "--name", name]
    for k, v in (labels or {}).items():
        cmd.extend(["--label", f"{k}={v}"])
    if data_volume:
        cmd.extend(["-v", f"{data_volume}:/data"])
    if network:
        cmd.extend(["--network", network])
    if dns:
        cmd.extend(["--dns", dns])
    for host_path, container_path in (mounts or []):
        cmd.extend(["-v", f"{host_path}:{container_path}:ro"])
    for container_port, host_port in port_map.items():
        cmd.extend(["-p", f"{publish_host}:{host_port}:{container_port}"])
    for container_port, host_port in (udp_port_map or {}).items():
        cmd.extend(["-p", f"{publish_host}:{host_port}:{container_port}/udp"])
    for host, value in (add_hosts or {}).items():
        cmd.extend(["--add-host", f"{host}:{value}"])
    for k, v in (env or {}).items():
        cmd.extend(["-e", f"{k}={v}"])
    cmd.append(image)
    result = subprocess.run(
        cmd, capture_output=True, text=True, timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(f"docker run failed:\n{result.stderr}")
    return result.stdout.strip()


def wait_for_smtp_banner(host: str, port: int, timeout: float = 30.0):
    """Poll until host:port serves an SMTP banner (`220 ...`).

    docker-proxy accepts TCP at the host layer before the container-side
    listener exists, so a bare `wait_for_tcp` returns false-positive
    "ready" while the SMTP service is still spinning up. Read the first
    line and require a `220` start-of-banner; retry on disconnect /
    short read until the deadline.
    """
    deadline = time.monotonic() + timeout
    last_err: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=2) as s:
                s.settimeout(2.0)
                buf = b""
                while not buf.endswith(b"\r\n") and len(buf) < 512:
                    chunk = s.recv(64)
                    if not chunk:
                        break
                    buf += chunk
                if buf.startswith(b"220 ") or buf.startswith(b"220-"):
                    return
                last_err = RuntimeError(
                    f"unexpected first line: {buf!r}"
                )
        except OSError as e:
            last_err = e
        time.sleep(0.5)
    raise TimeoutError(
        f"{host}:{port} did not serve an SMTP banner within {timeout}s "
        f"(last error: {last_err})"
    )


def wait_for_tls_handshake(
    host: str,
    port: int,
    expect_banner_prefixes: tuple[bytes, ...] | None = None,
    timeout: float = 60.0,
):
    """Poll until host:port completes an implicit-TLS handshake.

    The bridge wraps its implicit-TLS listeners (SMTPS 465, IMAPS 993) in
    ``tls.NewListener`` whose ``GetCertificate`` only returns a cert once the
    TLS provider has fetched + unsealed the fanned-out blob — so a successful
    handshake is the observable proof that the cert actually reached the
    bridge, not just that the TCP port is bound. Cert verification is disabled
    (the deploy serves a self-signed cert). When ``expect_banner_prefixes`` is
    given, also read the first line served over the TLS channel and require
    one of the prefixes (``220`` for SMTPS, ``* OK`` / ``* `` for IMAPS),
    proving the application listener — not just the TLS terminator — is live.
    """
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE

    deadline = time.monotonic() + timeout
    last_err: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=3) as raw:
                with ctx.wrap_socket(raw, server_hostname=host) as tls_sock:
                    if expect_banner_prefixes is None:
                        return
                    tls_sock.settimeout(3.0)
                    buf = b""
                    while not buf.endswith(b"\r\n") and len(buf) < 512:
                        chunk = tls_sock.recv(64)
                        if not chunk:
                            break
                        buf += chunk
                    if any(buf.startswith(p) for p in expect_banner_prefixes):
                        return
                    last_err = RuntimeError(f"unexpected TLS banner: {buf!r}")
        except (ssl.SSLError, OSError) as e:
            last_err = e
        time.sleep(0.5)
    raise TimeoutError(
        f"{host}:{port} did not complete a TLS handshake"
        + (" + banner" if expect_banner_prefixes else "")
        + f" within {timeout}s (last error: {last_err})"
    )


# ── Mail-bridge deploy lifecycle (shared by test_mail_deploy_lifecycle +
#    test_mail_deploy_inbound_round_trip) ──────────────────────────────────
#
# The bring-up sequence (enable → s6 up → keypairs → approve → restart-into-
# approved → x25519 attest → cert fan-out → serve) and its leaf helpers, lifted
# here so the inbound round-trip test reuses the exact path
# `test_mail_deploy_lifecycle.py` proves step-by-step. The lifecycle test keeps
# its granular per-step assertions; the round-trip test wants the same end
# state (all four listeners serving) as a precondition, via
# `bring_bridges_to_serving`. Ordering rationale + the Gap B/C/E history live in
# `test_mail_deploy_lifecycle.py`'s docstring and memory
# `mail-deploy-bridge-serving-ordering`.

ROLES = ("mta", "mda")


def admin_ws(nest):
    """A `WsRpcAdminClient` for the nest's claimed admin actor (context-manager)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def admin_post(nest, path: str, body: dict) -> tuple[int, dict]:
    """POST a JSON body to an admin HTTP route with the admin bearer token.
    Returns (status, parsed-json-or-empty)."""
    req = urllib.request.Request(
        f"{nest['url']}{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json",
                 "Authorization": f"Bearer {nest['admin']['token']}"},
        method="POST",
    )
    ctx = ssl._create_unverified_context() if nest["url"].startswith("https://") else None
    resp = urllib.request.urlopen(req, timeout=15, context=ctx)
    raw = resp.read()
    return resp.status, (json.loads(raw) if raw else {})


def svstat(name: str, service: str) -> str:
    r = subprocess.run(
        ["docker", "exec", name, "/command/s6-svstat", f"/run/service/{service}"],
        capture_output=True, text=True, timeout=10,
    )
    return (r.stdout or r.stderr).strip()


def is_commanded_up(svstat_str: str) -> bool:
    """True once `s6-svc -u` has fired: the service reads `up …` or `want up`.
    The down-by-default idle state reads `down (exitcode 0) … normally up,
    ready` — neither — so this discriminates the supervisor-socket bring-up."""
    return svstat_str.startswith("up ") or "want up" in svstat_str


def flag_present(name: str, flag: str = "imap-enabled") -> bool:
    """True once nest has materialized `/data/<flag>` — the durable enable gate
    the s6 MDA/MTA run-scripts check (`mail_enable.rs`'s `*_ENABLE_FLAG`).

    The `flag` parameter exists because **the MDA gates on ANY of the four**:
    `docker/s6/fauna-mail-bridge-mda/run` re-downs itself only when none of
    `/data/{imap,caldav,carddav,webdav}-enabled` exists, since the one process
    hosts all four protocols and binds each per its own `fetch_config` flag
    (caldav-server.md / webdav-server.md § Independent enablement). The default
    keeps every existing caller — all of which drive the mail path — byte-
    identical; a CalDAV-only deployment enables no mail at all, so asking for
    `imap-enabled` there would wait forever on a flag nothing is going to write.
    `test_caldav_bare_ip_serving.py` had already hand-rolled exactly this
    function for `caldav-enabled` before the parameter existed.
    """
    r = subprocess.run(
        ["docker", "exec", name, "test", "-e", f"/data/{flag}"],
        capture_output=True, timeout=10,
    )
    return r.returncode == 0


def keyfile_pubkey(name: str, role: str) -> str | None:
    """Decode `/data/keys/{role}/{role}.key` (DAG-CBOR ServiceUserKeyfile) →
    Ed25519 pubkey hex. None until the bridge has generated it. The keyfile lives
    in a per-role 0700 subdir owned by the bridge's own UID (fauna-mta/fauna-mda)
    since the co-resident UID-isolation split (security.md § UID isolation); we
    `cat` as root (default docker-exec user) so the per-role 0600 mode is moot."""
    import cbor2
    from nacl.signing import SigningKey

    raw = subprocess.run(
        ["docker", "exec", name, "cat", f"/data/keys/{role}/{role}.key"],
        capture_output=True, timeout=10,
    ).stdout
    if not raw:
        return None
    return bytes(SigningKey(cbor2.loads(raw)["ed25519_seed"]).verify_key).hex()


def register_primary_domain(nest, domain: str) -> None:
    """Admin registers the primary mail domain over WS-RPC
    (`fauna.bridges.add_local_domain`; no-HTTP directive). Must precede enable:
    mta/mda.Run idle with no listeners while `LocalDomains` is empty and there is
    no config-change push, so the domain has to be in the fetch_config snapshot at
    the bridges' post-approval cold boot for them to build a TLS provider + bind.
    `is_primary` is nest-derived (first active domain = primary), not a request
    field, and the kind is idempotent on domain_name."""
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.add_local_domain",
                   {"domain": domain,
                    "mta_sts_cert_mode": "self_signed"})


def add_local_domain(nest, domain: str) -> None:
    """Register an ADDITIONAL local domain (`fauna.bridges.add_local_domain`).

    `is_primary` is nest-derived (first active domain wins), so a domain added
    after `register_primary_domain` is a non-primary local domain — it routes +
    validates recipients but the listener's TLS still anchors on the primary's
    cert (the self-signed bootstrap covers the primary). The anti-spoofing guard
    uses this to host the spoofed `From:` domain (recipient + spoof both live on
    it) without disturbing the proven `localhost`-primary serving/cert path."""
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.add_local_domain",
                   {"domain": domain,
                    "mta_sts_cert_mode": "self_signed"})


def put_auth_policy(nest, **fields) -> None:
    """Set the inbound auth-enforcement policy (`fauna.bridges.put_auth_policy`).

    Fields map to `PutAuthPolicyRequest` (`enforce_dmarc`, `enforce_spf_hardfail`,
    `enforce_dkim`, `log_only`, …); omitted fields fall back to
    `AuthPolicy::default()`. Projected to the bridge via `fetch_config` — set
    before enable. The anti-spoofing guard sets `enforce_dmarc=true` +
    `log_only=false` explicitly (self-documenting; `enforce_dmarc` defaults true)
    so `mta/auth_enforce.go::applyDMARCRejectGate` rejects a DMARC-fail/`p=reject`
    message at DATA with `550 5.7.1`."""
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.put_auth_policy", dict(fields))


def await_pending_enrollment(admin, roles=ROLES, timeout: float = 60.0) -> dict[str, dict]:
    """Poll until every role's bridge has SELF-ENROLLED a pending row, keyed by
    role. Zero-touch (Gap B): the Go bridge calls `fauna.bridges.request_enrollment`
    over the loopback pre-identity WS on cold boot, so no admin
    pre-registration is needed — and pre-registering now 409s on the bridge's
    existing row. The per-container nest is fresh, so the only pending rows are
    this deploy's two bridges."""
    deadline = time.monotonic() + timeout
    by_role: dict[str, dict] = {}
    while time.monotonic() < deadline:
        rows = admin.call("fauna.bridges.list_service_users", {"status": "pending"})["service_users"]
        by_role = {b["role"]: b for b in rows}
        if set(roles) <= set(by_role):
            return by_role
        time.sleep(1.0)
    raise AssertionError(
        f"both bridges must self-enroll a pending row (zero-touch); got roles={list(by_role)}")


def await_approved_bridges(admin, roles=ROLES, timeout: float = 60.0) -> dict[str, dict]:
    """Poll until every role's bridge is APPROVED, keyed by role.

    With deployment mail enabled by the admin, a same-host MTA/MDA self-enrolling
    over the loopback pre-identity WS is auto-approved in ONE step (`573267935`;
    `mail-bridge-lifecycle.md` § Onboarding auto-approval) — it never rests in
    `pending`, so there is no pending row to observe and no manual
    `approve_pending_bridge`. (A bridge that happened to enroll before mail was
    flipped on self-heals pending→approved on its next enroll poll, so waiting on
    `approved` is robust either way.) Supersedes the `await_pending_enrollment` +
    explicit-approve pattern for the deploy tests, which all enable mail before
    bring-up."""
    deadline = time.monotonic() + timeout
    by_role: dict[str, dict] = {}
    while time.monotonic() < deadline:
        by_role = approved_docker_bridges(admin)
        if set(roles) <= set(by_role):
            return by_role
        time.sleep(1.0)
    raise AssertionError(
        f"both bridges must reach approved (auto-approved on admin mail-enable); "
        f"got roles={list(by_role)}")


def provision_self_signed_cert(nest, domain: str) -> dict:
    """Admin synthesizes a self-signed cert for `domain` and fans it out, sealed,
    to every approved bridge with an attested x25519, over WS-RPC
    (`fauna.bridges.provision_self_signed_cert`; no-HTTP directive). Returns the
    reply (`bridges_sealed_to` / `bridges_skipped_no_x25519`)."""
    with admin_ws(nest) as admin:
        return admin.call("fauna.bridges.provision_self_signed_cert",
                          {"domain": domain, "additional_dns_sans": []})


def sighup_service(name: str, service: str) -> None:
    """`s6-svc -h`: SIGHUP the bridge → immediate TLS-cert refresh rather than
    waiting out the 1-minute refresh backoff after the cert is fanned out."""
    subprocess.run(
        ["docker", "exec", name, "/command/s6-svc", "-h", f"/run/service/{service}"],
        capture_output=True, timeout=10,
    )


def restart_service(name: str, service: str) -> None:
    """`s6-svc -r`: restart so the bridge re-runs its cold boot against the now-
    approved state immediately. With the zero-touch poll loop the
    bridge self-advances after approval, so this is a deterministic nudge rather
    than a necessity; re-enrollment is idempotent (returns the approved status)."""
    subprocess.run(
        ["docker", "exec", name, "/command/s6-svc", "-r", f"/run/service/{service}"],
        capture_output=True, timeout=10,
    )


def await_keypairs(name: str, roles=ROLES, timeout: float = 30.0) -> dict[str, str]:
    """Poll until every role has self-generated its keypair; return {role: hex}."""
    pubkeys: dict[str, str] = {}
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and len(pubkeys) < len(roles):
        for r in roles:
            if r not in pubkeys:
                pk = keyfile_pubkey(name, r)
                if pk:
                    pubkeys[r] = pk
        if len(pubkeys) < len(roles):
            time.sleep(0.5)
    assert set(pubkeys) == set(roles), f"both roles must self-generate a keypair; got {list(pubkeys)}"
    for r, pk in pubkeys.items():
        assert len(bytes.fromhex(pk)) == 32, f"{r} pubkey must be 32 bytes"
    return pubkeys


def approved_docker_bridges(admin) -> dict[str, dict]:
    """The approved bridge_service_users, keyed by role. The per-container nest is
    fresh, so the only service users are this deploy's two self-enrolled bridges
    (their synthesized `bridge_id` is `{role}-{pubkey[:8]}`, not a fixed suffix)."""
    rows = admin.call("fauna.bridges.list_service_users", {"status": "approved"})["service_users"]
    return {b["role"]: b for b in rows}


def bridge_diag(name: str, roles=ROLES) -> str:
    """Recent bridge-relevant log lines + s6 state — appended to failure
    messages so a serving/round-trip failure diagnoses itself."""
    logs = subprocess.run(
        ["docker", "logs", "--tail", "100", name],
        capture_output=True, text=True, timeout=15,
    )
    keep = ("whoami", "register", "x25519", "404", "approv", "fatal", "error",
            "tls", "listener", "serving", "ready", "dial", "refresh", "clamd",
            "rspamd", "scan", "451", "550", "recipient", "ingest", "seal")
    lines = [ln for ln in (logs.stdout + logs.stderr).splitlines()
             if any(k in ln.lower() for k in keep)]
    st = {r: svstat(name, f"fauna-mail-bridge-{r}") for r in roles}
    return f"svstat={st!r}\n  " + "\n  ".join(lines[-26:])


def bring_bridges_to_serving(name: str, nest, mail_ports: dict[int, int],
                             domain: str, roles=ROLES) -> None:
    """Drive a claimed nest with a registered primary domain to all four mail
    listeners *serving* (220 on 25/587, TLS handshake on 465/993). The exact
    sequence `test_mail_deploy_lifecycle.py` proves step-by-step; here it's a
    round-trip precondition. Raises with `bridge_diag` appended on any failure.

    Precondition: `register_primary_domain(nest, domain)` already called (the
    domain must be present at the bridges' post-approval cold boot — see the
    lifecycle test docstring).
    """
    with admin_ws(nest) as admin:
        assert admin.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True
    await_bridges_serving(name, nest, mail_ports, domain, roles)


def await_bridges_serving(name: str, nest, mail_ports: dict[int, int],
                          domain: str, roles=ROLES) -> None:
    """The post-enable half of :func:`bring_bridges_to_serving`: wait for
    `/data/imap-enabled` + both s6 services commanded up, the keypairs, the
    auto-approval, the cert fan-out, and finally all four listeners serving.

    Split out for the same reason `await_mda_only_serving` was: a caller that
    flips `set_mail_enabled` **some other way** needs the identical convergence
    logic without a second enable. That caller used to be only
    `test_private_relay_hetzner.py`'s client-UI toggle; it is now also the docker
    mail VENUE (`conftest.py::_DockerMailVenueHandle.rebind_after_enable`), where
    the app under test has just driven the enable through its own mail-settings
    page and the venue's whole job is to answer "are the bridges serving again
    yet" — `testing.md` § Default app and nest mode, ruling (3): the docker shape
    of a mail nest is the image's own s6 bridges, so playing supervisor is a
    *different act* there, not a declared absence.
    """
    with admin_ws(nest) as admin:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if flag_present(name) and all(is_commanded_up(svstat(name, f"fauna-mail-bridge-{r}")) for r in roles):
                break
            time.sleep(0.5)
        assert flag_present(name), "/data/imap-enabled must materialize on set_mail_enabled(true)"
        for r in roles:
            st = svstat(name, f"fauna-mail-bridge-{r}")
            assert is_commanded_up(st), f"{r} s6 service must be commanded up; got: {st!r}"

        try:
            await_keypairs(name, roles)

            # Auto-approval (`573267935`; `mail-bridge-lifecycle.md` § Onboarding
            # auto-approval): a same-host MTA/MDA self-enrolling over the loopback
            # pre-identity WS while admin mail is enabled is approved in ONE step —
            # it never rests in `pending`, so there's no pending row to observe and
            # no manual `approve_pending_bridge`. Wait for both to reach `approved`.
            approved_by_role = await_approved_bridges(admin, roles)
            for r in roles:
                assert r in approved_by_role, f"{r} bridge must reach approved (auto-approve on mail-enable); approved={list(approved_by_role)}"
        except (AssertionError, TimeoutError) as e:
            # Keypair-gen / self-enrollment / approval failures need the bridge logs
            # too (the cold-boot loopback `request_enrollment` dial, x25519, 404s) —
            # not just the cert/serving tail below.
            raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name, roles)}") from e

        for r in roles:
            restart_service(name, f"fauna-mail-bridge-{r}")

        try:
            # Poll the (idempotent) cert provisioning until it fans out sealed to
            # every role — the observable proof each bridge re-dialed + attested
            # its x25519 on the restart-as-approved cold boot (Gap E).
            deadline = time.monotonic() + 60
            sealed_roles: set[str] = set()
            reply: dict = {}
            while time.monotonic() < deadline:
                reply = provision_self_signed_cert(nest, domain)
                sealed_roles = {b["role"] for b in reply.get("bridges_sealed_to", [])}
                if set(roles) <= sealed_roles:
                    break
                time.sleep(3.0)
            assert set(roles) <= sealed_roles, (
                f"cert must fan out sealed to {roles} (proves x25519 attestation); "
                f"last sealed={sealed_roles}, "
                f"skipped={[b['role'] for b in reply.get('bridges_skipped_no_x25519', [])]}"
            )

            wait_for_smtp_banner("127.0.0.1", mail_ports[25], timeout=60)
            wait_for_smtp_banner("127.0.0.1", mail_ports[587], timeout=60)
            for r in roles:
                sighup_service(name, f"fauna-mail-bridge-{r}")
            wait_for_tls_handshake("127.0.0.1", mail_ports[465], expect_banner_prefixes=(b"220 ", b"220-"), timeout=60)
            wait_for_tls_handshake("127.0.0.1", mail_ports[993], expect_banner_prefixes=(b"* OK", b"* "), timeout=60)
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name, roles)}") from e


# ── ATProto PDS bridge bring-up (F1; the atproto analogue of the mail path) ──

ATPROTO_ENABLE_FLAG_PATH = "/data/atproto-enabled"
ATPROTO_SERVICE = "fauna-atproto-bridge"
ATPROTO_ROLE = "atproto.pds"


def enable_atproto_bridge(name: str) -> None:
    """Infra-level enable of the ATProto PDS bridge in the deploy container.

    There is no production caller of nest's `set_atproto_enabled` yet — the clean
    admin `atproto_enabled` DB state is the deferred S4 deliverable
    (`atproto-pds-full.md` § Implementation status → "Not yet landed (F1
    remainder)"). So a tier_4 test arranges the precondition the way nest
    eventually will: write the `/data/atproto-enabled` flag the s6 run-script
    gates on, THEN `s6-svc -u` the down-by-default service. Order matters — the
    run-script `s6-svc -d`s itself when the flag is absent, so it must exist
    first. (Fixture setup arranging the world — e2e taxonomy carve-out (b), not
    the behaviour under test, which is the SNI routing + real-client-IP path.)"""
    subprocess.run(["docker", "exec", name, "touch", ATPROTO_ENABLE_FLAG_PATH],
                   capture_output=True, timeout=10, check=True)
    subprocess.run(["docker", "exec", name, "/command/s6-svc", "-u",
                    f"/run/service/{ATPROTO_SERVICE}"],
                   capture_output=True, timeout=10, check=True)


def atproto_flag_present(name: str) -> bool:
    r = subprocess.run(
        ["docker", "exec", name, "test", "-e", ATPROTO_ENABLE_FLAG_PATH],
        capture_output=True, timeout=10,
    )
    return r.returncode == 0


def atproto_bridge_logs(name: str) -> str:
    logs = subprocess.run(
        ["docker", "logs", name], capture_output=True, text=True, timeout=15)
    return logs.stdout + logs.stderr


def bring_atproto_bridge_to_serving(name: str, nest, domain: str) -> None:
    """Drive a claimed nest (primary domain already registered) to the ATProto PDS
    bridge SERVING its TLS-terminating `--xrpc-listen` on `pds.<domain>`. The
    atproto analogue of `bring_bridges_to_serving`; reused by every atproto tier_4
    test (priority #2).

    Two ways it differs from the mail path: (1) enable is infra-level — no prod
    `set_atproto_enabled` caller yet (see `enable_atproto_bridge`); (2) the
    `atproto.pds` role NEVER auto-approves (`bridge_blob_handlers.rs`
    `AtprotoPds => false`, even with every mail axis on), so this MANUALLY approves
    the self-enrolled pending row via `fauna.bridges.approve_pending_bridge`.

    Precondition: `register_primary_domain(nest, domain)` — the bridge reads
    `PrimaryDomain` from its fetch_config snapshot to derive `pds.<domain>`, the
    service DID, and the cert fetch key, and the self-signed floor must carry the
    `pds.<domain>` SAN (resynthesized on domain registration) at the bridge's
    post-approval boot. The floor already covers `pds.<domain>`, so no
    `provision_self_signed_cert` call is needed (and it would narrow the on-disk
    SAN to `[domain]`)."""
    enable_atproto_bridge(name)

    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if is_commanded_up(svstat(name, ATPROTO_SERVICE)):
            break
        time.sleep(0.5)
    assert atproto_flag_present(name), (
        f"{ATPROTO_ENABLE_FLAG_PATH} must exist after enable_atproto_bridge")
    assert is_commanded_up(svstat(name, ATPROTO_SERVICE)), (
        f"{ATPROTO_SERVICE} must be commanded up (touch flag + s6-svc -u); "
        f"got: {svstat(name, ATPROTO_SERVICE)!r}")

    with admin_ws(nest) as admin:
        try:
            # Zero-touch self-enroll over the loopback pre-identity WS, then a
            # MANUAL approve (atproto.pds is the one role auto-approve refuses).
            pending = await_pending_enrollment(admin, roles=[ATPROTO_ROLE])
            row = pending[ATPROTO_ROLE]
            approve = admin.call(
                "fauna.bridges.approve_pending_bridge",
                {"ed25519_pubkey": bytes(row["ed25519_pubkey"]), "role": ATPROTO_ROLE},
            )
            assert approve.get("ok") is True, (
                f"approve_pending_bridge(atproto.pds) failed: {approve}")
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(
                f"{e}\n\n── atproto bridge log tail ──\n"
                + "\n".join(atproto_bridge_logs(name).splitlines()[-40:])) from e

    # Restart → clean approved cold boot: the bridge attests x25519, unseals the
    # HS256 session secret, seal-reads the `pds.<domain>` floor cert, and brings up
    # the TLS-terminating XRPC listener. `"xrpc listener up"` (main.go) ⟹ approved
    # + x25519-attested + serving. Count occurrences so a pre-restart "up" (if the
    # bridge self-advanced after approval) can't false-positive the fresh boot.
    before = atproto_bridge_logs(name).count("xrpc listener up")
    restart_service(name, ATPROTO_SERVICE)
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if atproto_bridge_logs(name).count("xrpc listener up") > before:
            return
        time.sleep(1.0)
    raise AssertionError(
        "atproto XRPC listener never came up after approval (x25519-attested + "
        "cert-sealed?):\n" + "\n".join(atproto_bridge_logs(name).splitlines()[-40:]))


# ── Mail user provisioning + inbound round-trip wire (new deploy model) ─────


def relax_spam_policy(nest, *, helo_identity_required: bool | None = None) -> None:
    """Set the MTA spam policy permissively so an inbound from the docker
    host-gateway peer is accepted (the same settings the process-level
    `mail_bridge_mta` fixture uses): no DNSBL (loopback/gateway peers else 550
    at CONNECT), greylist off (first RCPT accepted — single connection),
    fcrdns off (no PTR/forward-A lookup on the synthetic peer), high conn-rate.
    Projected to the bridge via fetch_config — set before enable.

    `helo_identity_required` defaults to None (the field is omitted, so the nest
    keeps its catalog default of `true`) — every loopback-delivery caller leaves
    it that way because the HELO gate is loopback-exempt for them. A *cross-
    container* SMTP caller (the real MTA→MTA relay, where the peer is a non-
    loopback network IP) passes `False` to drop the HELO-identity check, whose
    EHLO host is the sender box's primary mail domain (`localhost`) and so cannot
    A-resolve to the connecting container IP. The write is REPLACE-semantics
    (`mail_policy.rs::write_policy_overrides` writes the whole row), so this field
    rides in the same put as the rest — it can't be a second call."""
    policy = {"dnsbl_servers": [], "greylist_enabled": False, "greylist_delay_secs": 0,
              "fcrdns_mode": "off", "max_conn_per_min": 1000,
              "baseline_standing_publish": False}
    if helo_identity_required is not None:
        policy["helo_identity_required"] = helo_identity_required
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.put_spam_policy", policy)


def enforce_mail_perimeter(nest, *, domain: str, cross_container_peer: bool = False) -> None:
    """Put a started+claimed box into the live-box ("prod-parity") inbound AUTH
    posture — the *default* tier_4 shape of ``docs/goal/architecture/testing.md``
    § Gap 2 Target. Three calls in one, the slice-1/2 pattern lifted out of
    ``test_mail_relay_two_nest_smtp.py`` (priority #2) so every mail tier_4 test
    can adopt the enforced perimeter without re-deriving it:

      1. ``register_primary_domain(domain)`` — the mail-cert anchor / outbound EHLO
         host / submission sender domain (set before enable).
      2. ``relax_spam_policy`` — relax ONLY the orthogonal spam gates a synthetic
         docker peer can never satisfy (DNSBL / greylist / FCrDNS / conn-rate);
         these are real-PTR/RBL gates, not the AUTH perimeter.
      3. ``put_auth_policy(enforce_dmarc=True, log_only=False)`` — ENFORCE the DMARC
         gate: ``mta/auth_enforce.go::applyDMARCRejectGate`` rejects a
         DMARC-fail/``p=reject`` message at DATA with ``550 5.7.1``.

    The caller MUST also (a) commit the storage mode — **Encrypted** for full prod
    parity, the example.com default — and (b) publish a passing SPF + ``_dmarc …
    p=reject`` for every sender domain via :func:`publish_passing_mail_dns`, so the
    enforced gate is cleared by a genuine auth pass, not a no-policy pass.

    ``cross_container_peer=True`` additionally drops the HELO-identity check: the
    peer is a non-loopback container IP whose EHLO host (the mail primary) cannot
    A-resolve to it, so the real MTA→MTA relay would 554 on it. A loopback-delivered
    peer (``deliver_inbound_loopback_curl``) leaves the HELO gate on — it is
    loopback-exempt — while the enforced DMARC gate still fires (proven by the
    ``550`` negative control in ``test_mail_security_accept.py``), so the enforced
    perimeter is non-vacuous on loopback too."""
    register_primary_domain(nest, domain)
    relax_spam_policy(nest, helo_identity_required=False if cross_container_peer else None)
    put_auth_policy(nest, enforce_dmarc=True, log_only=False)


def provision_mail_recipient(nest, run_seal_helper, *, domain: str, local_part: str,
                             password: str, actor: tuple[bytes, bytes] | None = None) -> dict:
    """Stand in for a user's primary client: provision a recipient that can both
    *receive* inbound mail (MSEK-derived recipient MLS pubkey the MTA seals to)
    and *read it back decrypted* over IMAP (wrapped-MSEK AUTH credential + MLS
    snapshot carrying the matching leaf secret). All keyed off ONE MSEK so the
    seal/open halves match — exactly the `mail_bridge_mda` fixture's recipe, run
    against the containerized nest. Returns
    {actor_id, username, password, msek, signing_key} (`signing_key` is the
    actor's Ed25519 seed bytes, so a caller can compose a *send* capability onto
    the same identity — see `provision_mail_user`).

    By default a fresh actor is registered. Pass ``actor=(actor_id, signing_seed)``
    to provision the recipe onto an EXISTING keyed actor instead — e.g. the
    claimed admin, so mail sealed to it (the forwarder NDR's ``forwarder_actor``
    IS the creating admin) becomes readable over IMAP (`provision_admin_mailbox`).
    """
    import base64
    import secrets

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register

    if actor is not None:
        # Provision the recipe onto an already-registered keyed actor; the
        # self-class blob uploads below authenticate AS it, so skip the create.
        actor_id, signing_key = actor
    else:
        # Register over WS-RPC (`fauna.admin.users.create`) by passing the claimed
        # admin's SIGNING KEY: the WS-RPC-everywhere rip-out deleted the
        # `POST /admin/api/users` HTTP twin, so the legacy `admin_token`
        # path no longer works (it hits nest's SPA 200 fallback → the helper's
        # `assert 201` trips). `admin_signing_key=` is the working path the shared
        # helper exposes (the docker half of the `register_user` migration to the
        # WS-RPC-everywhere work).
        recipient = create_actor_and_register(
            nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])
        actor_id = recipient["actor_id_bytes"]
        signing_key = bytes(recipient["signing_key"])
    msek = secrets.token_bytes(32)
    msek_b64 = base64.b64encode(msek).decode()
    actor_id_b64 = base64.b64encode(actor_id).decode()

    wrapped_msek = run_seal_helper("seal-wrapped-msek", {
        "msek_b64": msek_b64,
        "actor_id_b64": actor_id_b64,
        "credential_id": "default",
        "credential_kind": "plain",
        "credential_b64": base64.b64encode(password.encode()).decode(),
    })
    snapshot = run_seal_helper("seal-mls-snapshot", {
        "msek_b64": msek_b64, "actor_id_b64": actor_id_b64,
    })

    with admin_ws(nest) as admin:
        with WsRpcAdminClient(nest["url"], actor_id=actor_id,
                              signing_key=signing_key) as member:
            add_exact_alias_as(member, domain, local_part)
        provision_recipient_seal_key(
            admin, actor_id, msek=msek, run_seal_helper=run_seal_helper)

    # `provision_wrapped_mls_blob` / `provision_mls_snapshot_blob` are User-class
    # self-registration (keyed on the caller's own actor_id) — authenticate AS
    # the recipient, mirroring the user's own client uploading them.
    recipient_ws = WsRpcAdminClient(nest["url"], actor_id=actor_id,
                                    signing_key=signing_key)
    with recipient_ws:
        recipient_ws.call("fauna.bridges.provision_wrapped_mls_blob",
                          {"actor_id": actor_id, "credential_id": "default", "blob": wrapped_msek})
        recipient_ws.call("fauna.bridges.provision_mls_snapshot_blob", {"blob": snapshot})

    return {"actor_id": actor_id, "username": f"{local_part}@{domain}",
            "password": password, "msek": msek,
            "signing_key": signing_key}


# Pure-stdlib python image for the test sidecars — fake clamd/rspamd + the stub
# MX (the nest image has no python, and the host's helpers aren't reachable from
# the container). Pinned to the project's Python version (the host test runner is
# 3.14) so the sidecars run the same stdlib our shared helpers (`stub_mx.py`,
# `fake_*.py`) are written against — a lagging image would silently break a
# sidecar on a version-specific idiom even though the host suite passes.
SCANNER_SIDECAR_IMAGE = "python:3.14-slim"


def ensure_image(image: str) -> None:
    """Pull `image` if it isn't already present locally."""
    if subprocess.run(["docker", "image", "inspect", image],
                      capture_output=True, timeout=30).returncode == 0:
        return
    r = subprocess.run(["docker", "pull", image], capture_output=True, text=True, timeout=300)
    if r.returncode != 0:
        raise RuntimeError(f"docker pull {image} failed:\n{r.stderr[-600:]}")


def create_network(name: str, labels: dict | None = None) -> None:
    """Create a user-defined network, replacing any existing one of that name.

    ``labels`` mirror ``start_container_with_ports``' — the nest-mode axis stamps
    its per-run network with the same owner pair its containers carry, so the
    sweep reclaims both halves of a dead run's footprint rather than leaving an
    orphan network behind every crashed session.
    """
    subprocess.run(["docker", "network", "rm", name], capture_output=True, timeout=30)
    cmd = ["docker", "network", "create"]
    for k, v in (labels or {}).items():
        cmd.extend(["--label", f"{k}={v}"])
    cmd.append(name)
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    if r.returncode != 0:
        raise RuntimeError(f"docker network create {name} failed: {r.stderr}")


def remove_network(name: str) -> None:
    subprocess.run(["docker", "network", "rm", name], capture_output=True, timeout=30)


def container_ip(name: str) -> str:
    """The container's IPv4 on its (single) user-defined network.

    Used so a sidecar's address (the MTA's `mta_mx_override` target, the scan
    gate's clamd/rspamd addrs) is an **IP literal** the nest dials WITHOUT docker
    embedded DNS. Docker's embedded resolver (127.0.0.11) intermittently returns
    SERVFAIL ("server misbehaving") for an on-network container *name* under
    churn — a latent flake in the outbound-relay + scan-gate paths (it bit the
    outbound STARTTLS round-trip's single immediate attempt; the nest then
    reschedules on a minutes-scale curve, past the test's read window). Sidecars
    here join exactly one network (`--network <net>`), so ranging the Networks
    map yields that one IP. Call after the container is confirmed Running."""
    r = subprocess.run(
        ["docker", "inspect", "-f",
         "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}", name],
        capture_output=True, text=True, timeout=10,
    )
    ip = r.stdout.strip()
    if not ip:
        raise RuntimeError(
            f"could not determine container IP for {name}: "
            f"stdout={r.stdout!r} stderr={r.stderr!r}")
    return ip


def start_fake_scanner_sidecars(
    network: str,
    fakes_dir: str,
    *,
    clamd_name: str,
    rspamd_name: str,
    clamd_port: int = 3310,
    rspamd_port: int = 11333,
) -> dict[str, str]:
    """Run the proven `fakes/{fake_clamd,fake_rspamd}` as sidecar containers on
    `network`, each binding a fixed port the nest dials by container name. The
    nest container can't reach host listeners on this docker setup, so the
    fail-closed scan gate needs the scanners *inside* its network — this is the
    docker-compose-equivalent the milestone plan anticipated, via plain
    `docker run` + a mounted python image. Returns the operator-hatch values
    (`clamd_addr` / `rspamd_url`) to wire into the nest container's env.
    """
    ensure_image(SCANNER_SIDECAR_IMAGE)

    def _run(cname: str, snippet: str) -> None:
        remove_container(cname)
        r = subprocess.run(
            ["docker", "run", "-d", *fence_cpuset_args(), "--name", cname, "--network", network,
             "-v", f"{fakes_dir}:/fakes:ro", SCANNER_SIDECAR_IMAGE,
             "python", "-c", snippet],
            capture_output=True, text=True, timeout=60,
        )
        if r.returncode != 0:
            raise RuntimeError(f"start sidecar {cname} failed: {r.stderr}")
        # Guard against an import/bind crash: the python process binds in the
        # fake's __init__, so a still-running container after a beat means it's
        # listening (the scan gate dials it minutes later, after serving).
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            st = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", cname],
                                capture_output=True, text=True, timeout=10)
            if st.stdout.strip() == "true":
                return
            time.sleep(0.5)
        logs = subprocess.run(["docker", "logs", cname], capture_output=True, text=True, timeout=10)
        raise RuntimeError(f"sidecar {cname} exited early:\n{logs.stdout}\n{logs.stderr}")

    _run(clamd_name,
         f"import sys, time; sys.path.insert(0, '/fakes'); "
         f"from fake_clamd import FakeClamd; "
         f"FakeClamd(host='0.0.0.0', port={clamd_port}).start(); time.sleep(10**9)")
    _run(rspamd_name,
         f"import sys, time; sys.path.insert(0, '/fakes'); "
         f"from fake_rspamd import FakeRspamd; "
         f"FakeRspamd(host='0.0.0.0', port={rspamd_port}).start(); time.sleep(10**9)")
    # IP literals (not container names) so the nest's scan-gate dials bypass
    # docker embedded DNS — see container_ip() for the SERVFAIL flake it avoids.
    return {"clamd_addr": f"{container_ip(clamd_name)}:{clamd_port}",
            "rspamd_url": f"http://{container_ip(rspamd_name)}:{rspamd_port}"}


def start_fake_dns_sidecar(
    network: str,
    fakes_dir: str,
    *,
    name: str,
    txt_records: dict[str, str],
    a_records: dict[str, str] | None = None,
    records_dir: str | None = None,
    port: int = 53,
) -> str:
    """Run `fakes/fake_dns` as a sidecar container on `network`, serving the given
    TXT/A records on UDP `port`. Returns the sidecar's container IP — pass it to
    `start_container_with_ports(dns=...)` so the nest's resolver
    (`verify_inbound`'s `new_system_conf()`, no env override) reads the published
    policy through docker's embedded forwarder.

    The anti-spoofing guard publishes `_dmarc.<domain> TXT "v=DMARC1; p=reject"`
    plus `<domain> TXT "v=spf1 -all"`; everything else NXDOMAINs (the deploy
    nest's localhost+ACME-off cold boot needs no external DNS — bridge enrollment
    is loopback). Models `start_fake_scanner_sidecars`.

    `records_dir` (optional) is a host directory bind-mounted at `/dns`; the fake
    re-reads `/dns/records.json` (mtime-gated) and overlays it on the constructor
    records. The two-box SMTP-relay test uses it to publish each box's A/SPF
    records AFTER the boxes start (so it can fill in their container IPs) via
    `write_dns_records(records_dir, ...)` — natural MX/A resolution then routes
    box→box over the production `LiveMXResolver` path (no `mta_mx_override`)."""
    ensure_image(SCANNER_SIDECAR_IMAGE)
    remove_container(name)
    records_kw = ", records_path='/dns/records.json'" if records_dir else ""
    snippet = (
        f"import sys, time; sys.path.insert(0, '/fakes'); "
        f"from fake_dns import FakeDns; "
        f"FakeDns(host='0.0.0.0', port={port}, txt_records={txt_records!r}, "
        f"a_records={a_records or {}!r}{records_kw}).start(); time.sleep(10**9)"
    )
    mounts = ["-v", f"{fakes_dir}:/fakes:ro"]
    if records_dir:
        mounts += ["-v", f"{records_dir}:/dns:ro"]
    r = subprocess.run(
        ["docker", "run", "-d", *fence_cpuset_args(), "--name", name, "--network", network,
         *mounts, SCANNER_SIDECAR_IMAGE,
         "python", "-c", snippet],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise RuntimeError(f"start fake_dns sidecar {name} failed: {r.stderr}")
    # The UDP socket binds in FakeDns.__init__, so a still-running container after
    # a beat means it's listening (the nest queries it minutes later, at DATA).
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        st = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", name],
                            capture_output=True, text=True, timeout=10)
        if st.stdout.strip() == "true":
            return container_ip(name)
        time.sleep(0.5)
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=10)
    raise RuntimeError(f"fake_dns sidecar {name} exited early:\n{logs.stdout}\n{logs.stderr}")


def write_dns_records(records_dir: str, *, txt: dict[str, str] | None = None,
                      a: dict[str, str] | None = None,
                      mx: dict[str, str] | None = None) -> None:
    """Publish DNS records to a `records_dir`-backed fake_dns sidecar (started with
    `start_fake_dns_sidecar(records_dir=...)`). Writes `<records_dir>/records.json`
    atomically (temp + `os.replace`) so the bind-mounted sidecar never reads a
    half-written file; its mtime bump triggers the sidecar's reload on the next
    query. Use after the containers are up to fill in their learned container IPs
    (A records) — the chicken-and-egg the two-box relay can't solve at boot.
    `mx` maps a domain to `"<pref> <exchange>"` (or a bare exchange) for the
    outbound `LookupMX` next-hop; pair it with an A record for the exchange."""
    payload = json.dumps({"txt": txt or {}, "a": a or {}, "mx": mx or {}})
    tmp = os.path.join(records_dir, f".records.{os.getpid()}.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(payload)
    os.replace(tmp, os.path.join(records_dir, "records.json"))


def publish_passing_mail_dns(records_dir: str, domain_ips: dict[str, str], *,
                             spf_ip: str | None = None) -> None:
    """Publish a PASSING mail perimeter for each ``{domain: container_ip}`` in
    ``domain_ips`` into a ``records_dir``-backed fake_dns (see
    :func:`write_dns_records`): an A record, a self-MX (``0 <domain>``), a passing
    SPF (``v=spf1 ip4:<spf_ip or the domain's ip> -all``), and a ``_dmarc … p=reject``
    policy. The companion of :func:`enforce_mail_perimeter` — under its enforced
    DMARC gate a delivered message is accepted ONLY because it DMARC-passes, via the
    aligned SPF pass (From:/envelope on the sender domain ⇒ aspf-aligned), which is
    the prod-parity perimeter-PASSING shape (``testing.md`` § Gap 2 Target).

    ``spf_ip`` overrides the SPF-authorized address for *every* domain: pass
    ``"127.0.0.1"`` for a loopback-delivered sender (``deliver_inbound_loopback_curl``
    connects from inside the container, so the MTA sees the loopback as the client
    IP, not the container IP — the proven ``test_mail_security_accept.py`` pattern).
    The default authorizes each domain's own mapped container IP — the cross-container
    real-MTA→MTA leg, where SPF must pass against the connecting container IP.

    One atomic ``write_dns_records`` call (the sidecar reloads the whole file), so
    pass ALL sender domains at once — a second call would overwrite the first."""
    write_dns_records(
        records_dir,
        a=dict(domain_ips),
        mx={d: f"0 {d}" for d in domain_ips},
        txt={
            **{d: f"v=spf1 ip4:{spf_ip or ip} -all" for d, ip in domain_ips.items()},
            **{f"_dmarc.{d}": "v=DMARC1; p=reject" for d in domain_ips},
        },
    )


def deliver_inbound_loopback_curl(
    name: str,
    *,
    mail_from: str,
    rcpt_to: str,
    subject: str,
    body_text: str,
) -> str:
    """Deliver one inbound message to the MTA on port 25 **from inside the
    container over loopback**, via the image's ``curl`` (STARTTLS).

    Why loopback-via-exec rather than the host SMTP client: the MTA's inbound
    perimeter (``internal/mta/policy.go``) runs DNS-dependent checks — HELO
    identity (the HELO domain must A-resolve to the peer) and FCrDNS — that are
    **exempted for loopback peers** (`isLoopbackIP`). A host SMTP client reaches
    the container as the docker bridge-gateway IP (non-loopback), so those checks
    fire and reject a synthetic HELO. Delivering from ``127.0.0.1`` inside the
    container takes the same exempted path the proven process-level
    ``test_mail_bridge_mta.py`` round-trip uses. The sender-domain A/MX check
    (``SenderDomainChecker``) is **also loopback-exempt**, so over loopback
    ``mail_from`` may use a synthetic ``.test`` domain even with no A record
    published (``test_mail_security_accept.py`` delivers from ``sender-ok.test``,
    which publishes only SPF/``_dmarc``, and is accepted). Two caller shapes for the
    sender domain: a non-enforced caller can use ``host.docker.internal`` (resolves
    via ``--add-host``); an **enforced-DMARC** caller points the container's
    ``--dns`` at a fake_dns sidecar publishing the sender domain's passing SPF +
    ``_dmarc … p=reject``, so the enforced gate is cleared by an aligned auth pass
    (``test_mail_security_accept.py``, ``test_mail_deploy_bidirectional_round_trip.py``).
    Port 25 demands STARTTLS (``--ssl-reqd``); the self-signed cert is accepted with ``-k``.

    Returns the Message-ID for the read-back assertion.
    """
    msg_id = f"<{uuid.uuid4().hex}@e2e.test>"
    msg = email.message.EmailMessage()
    msg["From"] = mail_from
    msg["To"] = rcpt_to
    msg["Subject"] = subject
    msg["Message-ID"] = msg_id
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content(body_text)

    proc = subprocess.run(
        ["docker", "exec", "-i", name, "curl", "--silent", "--show-error",
         "--ssl-reqd", "-k", "--url", "smtp://127.0.0.1:25",
         "--mail-from", mail_from, "--mail-rcpt", rcpt_to, "-T", "-"],
        input=msg.as_bytes(),
        capture_output=True, timeout=60,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"inbound curl delivery failed (exit {proc.returncode}): "
            f"{proc.stderr.decode(errors='replace')}"
        )
    return msg_id


def deliver_inbound_attempt_loopback_curl(
    name: str,
    *,
    mail_from: str,
    rcpt_to: str,
    subject: str,
    body_text: str,
) -> tuple[int, str]:
    """The non-raising twin of ``deliver_inbound_loopback_curl``: deliver one full
    inbound message over loopback (STARTTLS, port 25) and return ``(returncode,
    combined_output)`` WITHOUT raising — for security tests that expect a
    *rejection at DATA* rather than acceptance.

    Unlike ``relay_attempt_loopback_curl`` (a minimal relay probe rejected at
    RCPT, no ``From:`` header), this sends a complete RFC 5322 message with a
    real ``From:`` — required for the anti-spoofing guard, where the verdict keys
    on the ``From:``-header domain (DMARC). The recipient is a valid local
    address, so the message clears RCPT and reaches DATA, where the auth-enforce
    stage rejects it (``550``). Self-signed cert accepted with ``-k``.

    Runs ``curl --verbose`` (not ``--silent``): on a **DATA-stage** rejection
    curl exits 8 ("Weird server reply") and its *plain* output does NOT carry the
    SMTP reply code (unlike a RCPT rejection, surfaced as "RCPT failed: 550…").
    The verbose trace logs every reply line (``< 550 5.7.1 DMARC reject``) to
    stderr, so the combined output the caller asserts on contains the code +
    enhanced status + message."""
    msg = email.message.EmailMessage()
    msg["From"] = mail_from
    msg["To"] = rcpt_to
    msg["Subject"] = subject
    msg["Message-ID"] = f"<{uuid.uuid4().hex}@e2e.test>"
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content(body_text)

    proc = subprocess.run(
        ["docker", "exec", "-i", name, "curl", "--verbose", "--show-error",
         "--ssl-reqd", "-k", "--url", "smtp://127.0.0.1:25",
         "--mail-from", mail_from, "--mail-rcpt", rcpt_to, "-T", "-"],
        input=msg.as_bytes(),
        capture_output=True, timeout=60,
    )
    combined = (proc.stdout.decode(errors="replace")
                + proc.stderr.decode(errors="replace"))
    return proc.returncode, combined


def deliver_inbound_raw_loopback_curl(
    name: str,
    *,
    mail_from: str,
    rcpt_to: str,
    raw: bytes,
) -> tuple[int, str]:
    """Deliver a caller-supplied **raw** RFC 5322 message over loopback (STARTTLS,
    port 25) and return ``(returncode, combined_output)`` WITHOUT raising — the
    byte-exact twin of ``deliver_inbound_loopback_curl``.

    Unlike the build-from-parts inbound helpers (which serialise an
    ``EmailMessage`` internally), the caller owns the exact bytes on the wire. This
    is required for **DKIM**: the signature covers specific header + body bytes, so
    the message must be signed and then delivered verbatim — a helper that
    re-serialises the message would change the signed bytes and break the body
    hash. Same loopback-exempt path the other inbound helpers document
    (HELO-identity / FCrDNS / sender-domain MX/A checks skipped for ``127.0.0.1``);
    the auth-enforce gate (SPF/DKIM/DMARC in ``verify_inbound``) still runs against
    the real client IP + the published DNS. Runs ``curl --verbose`` so a DATA-stage
    ``550`` — which curl surfaces as exit 8 with no plain reply code — still carries
    its ``< 550 5.7.1`` reply line in the combined output for the caller to assert
    on (the negative-control path)."""
    proc = subprocess.run(
        ["docker", "exec", "-i", name, "curl", "--verbose", "--show-error",
         "--ssl-reqd", "-k", "--url", "smtp://127.0.0.1:25",
         "--mail-from", mail_from, "--mail-rcpt", rcpt_to, "-T", "-"],
        input=raw,
        capture_output=True, timeout=60,
    )
    combined = (proc.stdout.decode(errors="replace")
                + proc.stderr.decode(errors="replace"))
    return proc.returncode, combined


def relay_attempt_loopback_curl(
    name: str,
    *,
    mail_from: str,
    rcpt_to: str,
) -> tuple[int, str]:
    """Attempt to relay one message to an **external** recipient over port 25
    **from inside the container over loopback**, and return ``(returncode,
    combined_output)`` WITHOUT raising — the security counterpart of
    ``deliver_inbound_loopback_curl``.

    The open-relay test: an unauthenticated port-25 RCPT TO for a domain not in
    the deployment's ``local_domains`` list must be rejected
    (``server.go::inboundSession.Rcpt`` → ``550 5.7.1 Relay access denied``). That
    relay gate is **unconditional** — it has no loopback exemption (unlike the
    HELO-identity / FCrDNS / sender-domain checks), so delivering from loopback
    (which clears those DNS-dependent perimeter checks) still hits it. So a
    loopback attempt isolates the relay decision: if the box were an open relay,
    curl would reach DATA and exit 0; because it isn't, the server 550s at RCPT
    and curl exits non-zero with the rejection on the wire.

    ``mail_from`` uses a resolvable domain (``host.docker.internal``) so the
    sender-domain check — which has no loopback exemption — passes, ensuring the
    rejection we observe is the *relay* gate, not a sender-domain reject.
    """
    proc = subprocess.run(
        ["docker", "exec", "-i", name, "curl", "--silent", "--show-error",
         "--ssl-reqd", "-k", "--url", "smtp://127.0.0.1:25",
         "--mail-from", mail_from, "--mail-rcpt", rcpt_to, "-T", "-"],
        input=b"Subject: relay probe\r\n\r\nThis must never relay.\r\n",
        capture_output=True, timeout=60,
    )
    combined = (proc.stdout.decode(errors="replace")
                + proc.stderr.decode(errors="replace"))
    return proc.returncode, combined


def unauth_submission_mail_code(host: str, port: int, *, implicit_tls: bool,
                                mail_from: str) -> tuple[int, str]:
    """Open a submission session (587 STARTTLS or 465 implicit-TLS) WITHOUT
    authenticating, issue ``MAIL FROM``, and return the server's ``(code,
    message)`` without smtplib raising.

    The submission-auth test: the 465/587 listeners set ``AllowInsecureAuth =
    false`` and gate every ``MAIL FROM`` on a prior successful AUTH
    (``submission.go::submissionSession.Mail`` → ``530 5.7.0 Authentication
    required`` when unauthenticated). An unauthenticated relay attempt on these
    ports must therefore be refused at ``MAIL FROM`` — there is no unauthenticated
    submission path. The TLS cert is self-signed in this harness, so we connect
    with verification disabled (we are asserting the *auth* gate, not the cert).
    """
    import smtplib

    ctx = ssl._create_unverified_context()
    if implicit_tls:
        client = smtplib.SMTP_SSL(host, port, context=ctx, timeout=30)
    else:
        client = smtplib.SMTP(host, port, timeout=30)
    try:
        client.ehlo()
        if not implicit_tls:
            client.starttls(context=ctx)
            client.ehlo()
        return client.docmd("MAIL", f"FROM:<{mail_from}>")
    finally:
        try:
            client.close()
        except Exception:
            pass


def imap_fetch_only_inbox_message(host: str, port: int, server_name: str,
                                  username: str, password: str,
                                  timeout: float = 90.0) -> bytes:
    """Drive raw-socket IMAPS (the go-imap fork is IMAP4rev2-only, which
    `imaplib` rejects): AUTHENTICATE PLAIN → poll SELECT INBOX until ≥1 EXISTS →
    UID FETCH BODY[] the message → return the decrypted RFC 5322 bytes. The MDA
    decrypts server-side from the snapshot it unwrapped at AUTH, so the client
    sees plaintext. For a freshly-provisioned recipient there is exactly one
    inbound message; return it. Raises TimeoutError with the last seen INBOX
    count on miss.

    Polls on ONE held connection: AUTHENTICATE PLAIN once, then re-SELECT INBOX
    each second until EXISTS≥1. A fresh AUTH happens only on a *connection* error
    (reconnect fallback), never per poll. This matters because each AUTH triggers
    a nest ``fetch_wrapped_mls_blob`` RPC, which is rate-limited to 30 events /
    60 s per (bridge, recipient, credential) (``bridge_rate_limit.rs``); the old
    reconnect-every-poll loop did ~90 AUTHs over a 90 s wait, so any delivery
    slower than ~30 s (e.g. the async CalDAV→MDA-gateway→``enqueue_outbound_mail``
    in-domain auto-schedule path) tripped the limit → ``AUTH … got NO`` → false
    timeout. A real IMAP client holds its connection and re-SELECTs / NOOPs to
    poll; so do we.

    The socket-level IMAP primitives are shared with the process-level
    mail-bridge tests via ``helpers.mail_wire`` (priority #4 — one raw-IMAP wire
    impl, not a copy). This reader connects by mapped *host port*
    (``_imaps_connect_addr``) rather than a bridge handle."""
    from helpers.mail_wire import (
        _imap_auth_plain, _imap_fetch_body_cmd, _imap_read_tagged, _imaps_connect_addr,
    )

    deadline = time.monotonic() + timeout
    last_count = -1
    last_err: Exception | None = None
    sock = buf = None
    seq = 0
    try:
        while time.monotonic() < deadline:
            try:
                if sock is None:
                    # First poll, or reconnect after a connection error. A fresh
                    # AUTH here is rare (not per-poll) so it never storms the
                    # per-credential fetch_wrapped_mls_blob rate limit.
                    sock, buf = _imaps_connect_addr(host, port, server_name, deadline)
                    status = _imap_auth_plain(sock, buf, "a0", username, password, deadline)
                    assert status == "OK", f"AUTH PLAIN must succeed for {username}; got {status}"

                seq += 1
                sel_tag = f"s{seq}"
                sock.sendall(f"{sel_tag} SELECT INBOX\r\n".encode())
                status, untagged = _imap_read_tagged(sock, buf, sel_tag, deadline)
                assert status == "OK", f"SELECT INBOX failed: {status}: {untagged!r}"
                exists = [ln for ln in untagged if ln.upper().endswith("EXISTS")]
                last_count = int(exists[0].split()[1]) if exists else 0

                if last_count >= 1:
                    seq += 1
                    _status, body = _imap_fetch_body_cmd(
                        sock, buf, f"f{seq}", "UID FETCH 1:* BODY[]", deadline)
                    if body is not None:
                        try:
                            sock.sendall(b"bye LOGOUT\r\n")
                        except OSError:
                            pass
                        return body
            except (ssl.SSLError, OSError, AssertionError) as e:
                last_err = e
                # Drop the (possibly broken) connection; the next iteration
                # reconnects + re-AUTHs. Reconnects are error-driven and rare.
                if sock is not None:
                    try:
                        sock.close()
                    except OSError:
                        pass
                    sock = buf = None
            time.sleep(1.0)
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
    raise TimeoutError(
        f"IMAP did not surface an INBOX message for {username} on {host}:{port} "
        f"within {timeout}s (last INBOX count={last_count}, last error: {last_err})"
    )


# ── Outbound submission round-trip wire (the send half) ─────────────────────


def _provision_submission_token(nest, run_seal_helper, *, actor_id: bytes,
                                signing_seed: bytes, credential: str) -> None:
    """Seal a wrapped submission token bound to ``signing_seed`` under
    ``credential`` and upload it AS that actor — the send-capability primitive
    shared by ``provision_mail_sender`` (a send-only identity) and
    ``provision_mail_user`` (the bidirectional user). The token is User-class:
    the nest keys it on the *caller's* actor_id, so it must be provisioned over a
    connection authenticated as that actor, not the admin. Mirrors the
    ``mail_bridge_mta`` fixture's submission-token block (conftest.py § Submission
    identity)."""
    import base64

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    now = int(time.time())
    token_blob = run_seal_helper("seal-submission-token", {
        "signing_seed_b64": base64.b64encode(signing_seed).decode(),
        "credential_id": "default",
        "credential_kind": "plain",
        "credential_b64": base64.b64encode(credential.encode()).decode(),
        "issued_at": now,
        "expires_at": now + 86400,
        "max_recipients": 100,
        "max_messages_per_day": 1000,
    })
    user_ws = WsRpcAdminClient(nest["url"], actor_id=actor_id, signing_key=signing_seed)
    with user_ws:
        user_ws.provision_wrapped_submission_token(token_blob)


def provision_mail_sender(nest, run_seal_helper, *, domain: str, local_part: str,
                          credential: str) -> dict:
    """Stand in for a user's primary client provisioning a *send-only* submission
    identity (the send-half counterpart of ``provision_mail_recipient``). Creates
    the sender actor, its account alias (so ``validate_recipient`` resolves the
    AUTH username), an MLS pubkey, and the wrapped submission token. Mirrors the
    ``mail_bridge_mta`` fixture's submission-identity block (conftest.py §
    Submission identity), run against the containerized nest. Returns
    {actor_id, username, credential}.

    The recipient MLS pubkey is a *throwaway* X25519 key: the bridge seals the
    sender's own "Sent" copy to it (550 5.1.1 otherwise), and this send-only
    identity never reads that copy back — so it needn't be MSEK-derived. The
    bidirectional ``provision_mail_user`` instead reuses its **MSEK-derived**
    recipient pubkey (which it *can* decrypt) for both inbound and the Sent copy."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register

    # WS-RPC register via the shared helper's `admin_signing_key=` path (see
    # provision_mail_recipient — the deleted `POST /admin/api/users` twin).
    sender = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])
    sender_actor_id = sender["actor_id_bytes"]
    sender_seed = bytes(sender["signing_key"])

    with admin_ws(nest) as admin:
        with WsRpcAdminClient(nest["url"], actor_id=sender_actor_id,
                              signing_key=sender_seed) as member:
            add_exact_alias_as(member, domain, local_part)
        provision_recipient_seal_key(
            admin, sender_actor_id, run_seal_helper=run_seal_helper)

    _provision_submission_token(nest, run_seal_helper, actor_id=sender_actor_id,
                                signing_seed=sender_seed, credential=credential)

    return {"actor_id": sender_actor_id, "username": f"{local_part}@{domain}",
            "credential": credential}


def provision_mail_user(nest, run_seal_helper, *, domain: str, local_part: str,
                        password: str) -> dict:
    """Provision ONE actor/MSEK that is both a mail *recipient* (receives inbound +
    reads it back decrypted over IMAP) and a *sender* (submits authenticated mail
    the nest DKIM-signs + the MTA relays out) — the single-user-bidirectional identity Stage
    5 Slice 7 proves end-to-end in one deploy.

    Composes the two single-purpose helpers on one identity: the full
    ``provision_mail_recipient`` recipe (alias + MSEK-derived recipient MLS pubkey +
    wrapped-MSEK AUTH credential + MLS snapshot) PLUS a wrapped submission token
    bound to the same actor under the **same** ``password`` — one MUA credential for
    both IMAP and SMTP, exactly as a real client uses. The alias + recipient MLS
    pubkey are already set by the recipient half, so the send half adds only the
    token; the user's own "Sent" copy therefore seals to the MSEK-derived pubkey it
    can decrypt (strictly more faithful than ``provision_mail_sender``'s throwaway
    key, which suffices only because the send-only test never reads the Sent copy).
    Returns {actor_id, username, password, msek, signing_key}."""
    user = provision_mail_recipient(
        nest, run_seal_helper, domain=domain, local_part=local_part, password=password)
    _provision_submission_token(
        nest, run_seal_helper, actor_id=user["actor_id"],
        signing_seed=user["signing_key"], credential=password)
    return user


def provision_admin_mailbox(nest, run_seal_helper, *, domain: str, local_part: str,
                            password: str) -> dict:
    """Provision the full recipient recipe onto the claimed admin's EXISTING
    actor, so mail sealed to the admin is readable over IMAP at
    ``<local_part>@<domain>``. The forwarder NDR seals its DSN to the
    ``forwarder_actor`` — which, for an admin ``create_forwarder``, IS the
    creating admin — so the deploy-image forwarder-NDR test reads the bounce out
    of the admin's INBOX. The admin claimed via ``claim_admin_api`` is a real
    keyed actor; reuse its key for the self-class blob uploads instead of minting
    a throwaway recipient. Returns the same dict as ``provision_mail_recipient``."""
    admin_sk = nest["admin"]["signing_key"]
    return provision_mail_recipient(
        nest, run_seal_helper, domain=domain, local_part=local_part, password=password,
        actor=(bytes(admin_sk.verify_key), bytes(admin_sk)),
    )


def start_stub_mx_sidecar(network: str, helpers_dir: str, out_dir: str,
                          *, name: str, port: int = 2525,
                          require_starttls: bool = False) -> str:
    """Run ``helpers/stub_mx.py`` as a sidecar container on ``network`` — the
    external SMTP MX the MTA's outbound worker relays to. The bridge dials it by
    container name + ``port`` (its ``mta_mx_override`` target); each delivered
    message lands in the mounted ``out_dir`` (the sidecar's in-memory list is
    cross-process-unreachable, so it persists ``.eml`` files the host test reads).
    Stdlib-only, so the same ``python:3.13-slim`` image as the scan sidecars runs
    it with no pip install. Returns the ``host:port`` override target (``name:port``).

    Default: plaintext-only (no STARTTLS) so the bridge's opportunistic sender
    falls back to cleartext (the Slice-6 relay proof). ``require_starttls=True``
    makes the stub advertise STARTTLS and refuse cleartext mail-flow, so a
    delivery proves the bridge upgraded to TLS (RFC 7435 opportunistic security)
    — the deploy-image outbound-over-STARTTLS proof, hardening the real-Gmail TLS
    leg."""
    ensure_image(SCANNER_SIDECAR_IMAGE)
    remove_container(name)
    snippet = ("import sys; sys.path.insert(0, '/helpers'); "
               "import stub_mx; stub_mx._main()")
    cmd = ["docker", "run", "-d", *fence_cpuset_args(), "--name", name, "--network", network,
           "-v", f"{helpers_dir}:/helpers:ro", "-v", f"{out_dir}:/out",
           "-e", f"STUB_MX_PORT={port}", "-e", "STUB_MX_OUT_DIR=/out"]
    if require_starttls:
        cmd.extend(["-e", "STUB_MX_REQUIRE_STARTTLS=1"])
    cmd.extend([SCANNER_SIDECAR_IMAGE, "python", "-c", snippet])
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    if r.returncode != 0:
        raise RuntimeError(f"start stub-mx sidecar {name} failed: {r.stderr}")
    # Wait for the listener-ready log line, NOT just `State.Running`: in STARTTLS
    # mode `_main` loads the TLS cert in StubMX.__init__ before printing this, and
    # a container that boots then crashes (e.g. a missing dep) is briefly Running
    # — so a Running-only check passes a stub that's already dead, surfacing later
    # as `connection refused` at relay time. Fail fast (with logs) if it exits.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=10)
        if "stub-mx listening" in (logs.stdout + logs.stderr):
            # IP literal (not the container name) so the MTA's outbound dial
            # bypasses docker embedded DNS — see container_ip() for the SERVFAIL
            # flake it avoids (it bit this very round-trip's STARTTLS variant).
            return f"{container_ip(name)}:{port}"
        running = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", name],
                                 capture_output=True, text=True, timeout=10)
        if running.stdout.strip() != "true":
            break  # crashed before serving (the readiness print never landed)
        time.sleep(0.5)
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=10)
    raise RuntimeError(f"stub-mx sidecar {name} not ready:\n{logs.stdout}\n{logs.stderr}")


def submit_message_tls(host: str, port: int, server_name: str, *,
                       sender: str, password: str, rcpt: str, raw_message: bytes) -> None:
    """Submit one message as an authenticated user over implicit TLS (port 465),
    the deploy-image counterpart of the process-level ``test_submission_round_trip``.
    SASL PLAIN AUTH (the wrapped submission token, AEAD-unsealed + inner-Ed25519-
    verified bridge-side), then MAIL/RCPT/DATA. CERT_NONE — the listener serves the
    admin-synthesized self-signed cert. Raises on any non-2xx step."""
    import base64
    import smtplib

    auth_plain = base64.b64encode(
        b"\x00" + sender.encode() + b"\x00" + password.encode()
    ).decode()
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with smtplib.SMTP_SSL(host, port, local_hostname=server_name, context=ctx,
                          timeout=30) as smtp:
        smtp.ehlo(server_name)
        code, resp = smtp.docmd("AUTH", "PLAIN " + auth_plain)
        if code != 235:
            raise AssertionError(f"AUTH PLAIN failed for {sender}: {code} {resp!r}")
        smtp.sendmail(sender, [rcpt], raw_message)


def read_stub_mx_message(out_dir: str, token: str, timeout: float = 30.0) -> bytes | None:
    """Poll the stub-MX sidecar's mounted out-dir for a delivered ``.eml`` that
    carries ``token`` (a per-run marker in the Subject/Message-ID); return its raw
    bytes, or None on timeout. The sidecar writes each message atomically
    (temp + rename), so a partial file is never read."""
    deadline = time.monotonic() + timeout
    needle = token.encode()
    while time.monotonic() < deadline:
        try:
            names = sorted(fn for fn in os.listdir(out_dir) if fn.endswith(".eml"))
        except OSError:
            names = []
        for fn in names:
            try:
                raw = Path(out_dir, fn).read_bytes()
            except OSError:
                continue
            if needle in raw:
                return raw
        time.sleep(0.5)
    return None


# ── Two-box home-relay (Slice 6) helpers ────────────────────────────────────
#
# The home-with-public-relay deployment: a PUBLIC encrypted-mode relay box (MTA
# inbound + MDA) on a VPS and a PRIVATE plaintext-mode home box (FAUNA_MODE=
# private, no MTA, MDA serves LAN IMAP) paired so inbound mail relays public→
# private over the federation channel, after which the public box holds no
# readable/persistent copy. See
# docs/goal/architecture/nest/deployment-home-with-public-relay.md +
# docs/goal/architecture/installers/home-relay.md. These compose the existing
# single-box mail helpers above into the two-box shape.


def node_id(nest) -> bytes:
    """The nest's 32-byte Ed25519 nest id via the anonymous ``fauna.nest.info``
    kind (the same discovery path the federation pool uses to resolve a peer URL
    → nest id). Needed to seed the two pairing rows — each names the *other*
    box's nest id (deployment-home-with-public-relay.md § Pairing)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest["url"]) as anon:
        reply = anon.call("fauna.nest.info", {})
    return bytes.fromhex(reply["nest_id"])


def add_user_pairing(nest, *, actor_id: bytes, signing_key: bytes,
                     other_nest_id: bytes, capabilities: list[str],
                     nest_url: str | None = None) -> None:
    """``fauna.pair.add`` as the USER (owner-scoped, User class) over the nest's
    HTTPS WS-RPC — the user authorizes ``other_nest_id`` with ``capabilities``.

    The two-row both-ends pairing model (§ Pairing): on the PUBLIC box the row
    names the private box (caps incl. ``mail_pull``) and authorizes the pull; on
    the PRIVATE box the row names the public box and its ``nest_url`` — the
    target the home box's sync/relay worker dials, and what makes it fire at all
    (``private-mode.md`` § Implementation status today). The tier_3
    ``ws_api.add_pairing`` can't drive this — it hardcodes ``http://127.0.0.1``
    (docker nests are HTTPS) and its default caps omit ``mail_pull`` (the relay
    grant)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    user_ws = WsRpcAdminClient(nest["url"], actor_id=actor_id, signing_key=signing_key)
    with user_ws:
        payload = {
            "private_nest_id": bytes(other_nest_id),
            "capabilities": list(capabilities),
        }
        if nest_url is not None:
            payload["nest_url"] = nest_url
        user_ws.call("fauna.pair.add", payload)


def provision_relay_user(public_nest, private_nest, run_seal_helper, *,
                         domain: str, local_part: str, password: str,
                         home_owner: dict | None = None) -> dict:
    """Provision ONE user (one actor identity + one MSEK) across BOTH boxes so the
    public box can *receive* inbound (its MTA seals to the recipient) and both
    MDAs can *serve* it decrypted over IMAP — the public box transiently (until
    the relay purges it), the private box as the canonical store. The actor is
    registered on both nests (the relay + both pairing rows key on it); the three
    seal-helper artifacts all derive from the single MSEK so the seal/open halves
    match across the relay (the mirror of ``provision_mail_recipient``'s recipe,
    split across two nests). Returns {actor_id, signing_key, username, password,
    msek}.

    ``home_owner`` (an admin dict from :func:`claim_admin_api` on the private
    box) makes the box's own admin the user, registered on the public box —
    the deployment the home bundle builds, and the one in which the home box
    may dial its relay at a private address: the address guard exempts a
    pairing row's URL only when the row's actor is an admin of the private
    nest (``private-mode.md`` § Pairing Flow). Omitted, a fresh actor is
    created on the public box and registered on the private one."""
    import base64
    import secrets

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import create_actor_and_register, register_user

    # One actor identity, registered on BOTH boxes (relay + pairing rows key on
    # it). Create on the public box, then register the SAME actor id on the
    # private box. Both register over WS-RPC via the admin signing key — the
    # POST /admin/api/users twin was ripped and an admin_token-only
    # call hits the SPA fallback (200, no registration). See common.auth.register_user
    # (the docker half of the register_user migration to the WS-RPC-everywhere work).
    if home_owner is not None:
        actor_id = home_owner["actor_id_bytes"]
        signing_key = bytes(home_owner["signing_key"])
        register_user(public_nest["port"], home_owner["actor_id_hex"],
                      admin_signing_key=public_nest["admin"]["signing_key"],
                      base_url=public_nest["url"])
    else:
        actor = create_actor_and_register(
            public_nest["port"], admin_signing_key=public_nest["admin"]["signing_key"],
            base_url=public_nest["url"])
        actor_id = actor["actor_id_bytes"]
        signing_key = bytes(actor["signing_key"])
        register_user(private_nest["port"], actor["actor_id_hex"],
                      admin_signing_key=private_nest["admin"]["signing_key"],
                      base_url=private_nest["url"])

    # One MSEK → the three artifacts (mirror of provision_mail_recipient): the
    # recipient MLS pubkey the MTA seals to, the wrapped-MSEK AUTH credential, and
    # the MLS snapshot the MDA opens the mail with.
    msek = secrets.token_bytes(32)
    msek_b64 = base64.b64encode(msek).decode()
    actor_id_b64 = base64.b64encode(actor_id).decode()
    wrapped_msek = run_seal_helper("seal-wrapped-msek", {
        "msek_b64": msek_b64, "actor_id_b64": actor_id_b64,
        "credential_id": "default", "credential_kind": "plain",
        "credential_b64": base64.b64encode(password.encode()).decode(),
    })
    snapshot = run_seal_helper("seal-mls-snapshot", {
        "msek_b64": msek_b64, "actor_id_b64": actor_id_b64,
    })

    # Recipient side on BOTH boxes: the account alias (login→actor for IMAP AUTH;
    # RCPT validation for the public MTA), written by the member themselves, +
    # the recipient MLS pubkey (the MTA seals to it — load-bearing on the public
    # box; harmless on the private box, which has no MTA; Admin-class).
    for nest in (public_nest, private_nest):
        with admin_ws(nest) as admin:
            with WsRpcAdminClient(nest["url"], actor_id=actor_id,
                                  signing_key=signing_key) as member:
                add_exact_alias_as(member, domain, local_part)
            provision_recipient_seal_key(
                admin, actor_id, msek=msek, run_seal_helper=run_seal_helper)

    # Reader side on BOTH boxes (User-class self-registration, keyed on the
    # caller's own actor_id): the wrapped-MSEK AUTH credential + the MLS snapshot
    # the MDA AEAD-unwraps at AUTH to open the mail. The public box's MDA serves
    # the user until the relay purges (the no-readable-copy assertion reads its
    # INBOX 1→0); the private box's MDA is the canonical reader.
    for nest in (public_nest, private_nest):
        user_ws = WsRpcAdminClient(nest["url"], actor_id=actor_id, signing_key=signing_key)
        with user_ws:
            user_ws.call("fauna.bridges.provision_wrapped_mls_blob",
                         {"actor_id": actor_id, "credential_id": "default",
                          "blob": wrapped_msek})
            user_ws.call("fauna.bridges.provision_mls_snapshot_blob", {"blob": snapshot})

    return {"actor_id": actor_id, "signing_key": signing_key,
            "username": f"{local_part}@{domain}", "password": password, "msek": msek}


def bring_mda_only_to_serving(name: str, nest, imap_host_port: tuple[str, int],
                              domain: str) -> None:
    """Drive a PRIVATE-axis (``FAUNA_MODE=private``) home box's MDA to *serving*
    IMAP — and assert the MTA stays DOWN. Unlike ``bring_bridges_to_serving``
    (both roles + all four MTA/MDA listeners), this brings up only the MDA and
    asserts the perimeter parser is **not started** (the ``mta_should_run`` nest
    gate + the ``FAUNA_MODE=private`` s6 run-script re-down —
    deployment-home-with-public-relay.md § Plaintext-mode behavior).

    Precondition: ``register_primary_domain(nest, domain)`` already called (the
    domain must be present at the bridge's post-approval cold boot)."""
    with admin_ws(nest) as admin:
        assert admin.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True
    await_mda_only_serving(name, nest, imap_host_port, domain)


def await_mda_only_serving(name: str, nest, imap_host_port: tuple[str, int],
                           domain: str) -> None:
    """The post-enable half of :func:`bring_mda_only_to_serving`: wait for
    ``/data/imap-enabled`` + the MDA to come up, assert the MTA stays DOWN,
    wait for the bridge's auto-approval, provision the self-signed cert fan-out,
    and wait for the IMAPS handshake. Split out so a caller that flips
    ``set_mail_enabled`` through the client UI instead (the
    ``admin-mail-enabled-toggle`` — ``test_private_relay_hetzner.py``) reuses the
    same convergence logic."""
    with admin_ws(nest) as admin:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if flag_present(name) and is_commanded_up(svstat(name, "fauna-mail-bridge-mda")):
                break
            time.sleep(0.5)
        assert flag_present(name), "/data/imap-enabled must materialize on set_mail_enabled(true)"
        assert is_commanded_up(svstat(name, "fauna-mail-bridge-mda")), \
            f"MDA must be commanded up; got: {svstat(name, 'fauna-mail-bridge-mda')!r}"
        # No-MTA boot: the home box's perimeter parser stays DOWN even though mail
        # is enabled (it serves IMAP, never SMTP). This is the property Slice 6
        # validates end-to-end in the real image.
        mta_state = svstat(name, "fauna-mail-bridge-mta")
        assert not is_commanded_up(mta_state), (
            f"private home box MTA must stay DOWN (perimeter parser not started); "
            f"got: {mta_state!r}")

        await_keypairs(name, ("mda",))
        # Auto-approve on admin mail-enable (see bring_bridges_to_serving): the MDA
        # self-enrolls + auto-approves in one step — wait for `approved`, no manual
        # approve_pending_bridge.
        assert "mda" in await_approved_bridges(admin, ("mda",)), "mda bridge must reach approved"
        restart_service(name, "fauna-mail-bridge-mda")

    try:
        deadline = time.monotonic() + 60
        sealed: set[str] = set()
        reply: dict = {}
        while time.monotonic() < deadline:
            reply = provision_self_signed_cert(nest, domain)
            sealed = {b["role"] for b in reply.get("bridges_sealed_to", [])}
            if "mda" in sealed:
                break
            time.sleep(3.0)
        assert "mda" in sealed, f"cert must fan out sealed to mda; last sealed={sealed}"
        sighup_service(name, "fauna-mail-bridge-mda")
        host, port = imap_host_port
        wait_for_tls_handshake(host, port, expect_banner_prefixes=(b"* OK", b"* "), timeout=60)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name, ('mda',))}") from e


def imap_inbox_count(host: str, port: int, server_name: str,
                     username: str, password: str, timeout: float = 30.0) -> int:
    """AUTH PLAIN → SELECT INBOX → the EXISTS count (0 when empty). Single
    connection (no reconnect-poll). Used for the no-readable-copy assertion: the
    public relay box's INBOX goes 1→0 once the home box acks and the public box
    tombstones the relayed records (the IMAP fetch path filters ``tombstoned =
    0``, so the drop is observable immediately, not only after compaction)."""
    from helpers.mail_wire import (
        _imap_auth_plain, _imap_read_tagged, _imaps_connect_addr,
    )

    deadline = time.monotonic() + timeout
    sock, buf = _imaps_connect_addr(host, port, server_name, deadline)
    with sock:
        status = _imap_auth_plain(sock, buf, "a1", username, password, deadline)
        assert status == "OK", f"AUTH PLAIN must succeed for {username}; got {status}"
        sock.sendall(b"a2 SELECT INBOX\r\n")
        status, untagged = _imap_read_tagged(sock, buf, "a2", deadline)
        assert status == "OK", f"SELECT INBOX failed: {status}: {untagged!r}"
        sock.sendall(b"a3 LOGOUT\r\n")
        exists = [ln for ln in untagged if ln.upper().endswith("EXISTS")]
        return int(exists[0].split()[1]) if exists else 0


def sni_https_request(host_port: int, sni: str, method: str, path: str,
                      timeout: float = 15.0,
                      extra_headers: dict[str, str] | None = None,
                      body: bytes | None = None,
                      ) -> tuple[int, dict[str, str], bytes]:
    """One HTTP/1.1 request to ``127.0.0.1:host_port`` over TLS, sending ``sni``
    as the ClientHello server_name (the routing key the L4 ``fauna-sni-router``
    peeks) while the TCP target stays loopback. ``extra_headers`` are appended
    verbatim (e.g. an ``Authorization: Basic …`` header to exercise the CalDAV
    auth path). ``body`` is the request payload for a POST (e.g. an XRPC
    ``createSession`` JSON body); the caller supplies its own ``Content-Type``
    via ``extra_headers``. Cert verification is off (self-signed deploy certs).
    Returns ``(status_code, headers_lower, body)``.

    Raw socket rather than ``http.client`` because the SNI must differ from the
    connect address: ``http.client`` derives ``server_hostname`` from the connect
    host, so it can't send ``mail.<domain>`` to ``127.0.0.1``. Each call is one
    short-lived connection (``Connection: close``) — faithful to a stateless
    HTTP-Basic CalDAV client, where every request re-presents credentials and
    re-runs the MDA AUTH flow.

    Shared infra for any SNI-routed tier_4 CalDAV test (the SNI-router split
    acceptance and the auth-material-cache burst lock); keep it here, not
    duplicated per module (priority #2)."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE

    raw = socket.create_connection(("127.0.0.1", host_port), timeout=timeout)
    try:
        with ctx.wrap_socket(raw, server_hostname=sni) as tls:
            tls.settimeout(timeout)
            header_lines = [
                f"{method} {path} HTTP/1.1",
                f"Host: {sni}",
                "Connection: close",
                f"Content-Length: {len(body) if body else 0}",
            ]
            for k, v in (extra_headers or {}).items():
                header_lines.append(f"{k}: {v}")
            req = ("\r\n".join(header_lines) + "\r\n\r\n").encode()
            if body:
                req += body
            tls.sendall(req)
            buf = b""
            while b"\r\n\r\n" not in buf:
                chunk = tls.recv(4096)
                if not chunk:
                    break
                buf += chunk
            # Drain the body (Connection: close) so the assertion can read it.
            while True:
                try:
                    chunk = tls.recv(4096)
                except (ssl.SSLError, OSError):
                    break
                if not chunk:
                    break
                buf += chunk
    finally:
        try:
            raw.close()
        except OSError:
            pass

    head, _, body = buf.partition(b"\r\n\r\n")
    lines = head.split(b"\r\n")
    status = int(lines[0].split(b" ")[1]) if lines and len(lines[0].split(b" ")) > 1 else 0
    headers: dict[str, str] = {}
    for ln in lines[1:]:
        k, _, v = ln.partition(b":")
        if v:
            headers[k.decode().strip().lower()] = v.decode().strip()
    return status, headers, body


def make_ca_issued_leaf_pem(names: list[str]) -> tuple[bytes, bytes]:
    """A leaf signed by a throwaway CA (issuer DN ≠ subject DN → ``is_floor``
    false) covering ``names``. Returns ``(fullchain_pem [leaf+CA], leaf_key_pem)``.
    The nest only inspects issuer-vs-subject to decide floor-ness — no real trust
    anchor is needed for the cert to read as 'trusted' (and so withdraw the
    floor-key TLSA / restore the ``enforce`` MTA-STS mode).

    Shared by every cert-honesty tier_4 test (DANE-TLSA withdrawal,
    MTA-STS floor→trusted) — the floor→trusted flip is the same trigger; keep it
    here, not duplicated per module (priority #2)."""
    import datetime

    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    now = datetime.datetime.now(datetime.timezone.utc)
    not_before = now - datetime.timedelta(days=1)
    not_after = now + datetime.timedelta(days=365)

    ca_key = ec.generate_private_key(ec.SECP256R1())
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Fauna Test Root CA")])
    ca_cert = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .sign(ca_key, hashes.SHA256())
    )

    leaf_key = ec.generate_private_key(ec.SECP256R1())
    leaf_cert = (
        x509.CertificateBuilder()
        .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, names[0])]))
        .issuer_name(ca_name)  # issuer (CA) != subject (leaf) → not the self-signed floor
        .public_key(leaf_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .add_extension(
            x509.SubjectAlternativeName([x509.DNSName(n) for n in names]), critical=False
        )
        .sign(ca_key, hashes.SHA256())
    )
    fullchain = leaf_cert.public_bytes(serialization.Encoding.PEM) + ca_cert.public_bytes(
        serialization.Encoding.PEM
    )
    key_pem = leaf_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    return fullchain, key_pem


def install_acme_default_cert(container: str, fullchain_pem: bytes, key_pem: bytes,
                              acme_dir: str = "/data/acme") -> None:
    """Land a ``(leaf, key)`` pair as the container's default cert at ``acme_dir``,
    the way a real ACME issuance does — the cert-watcher hot-reloads it on the
    inotify Modify event (2 s debounce, both files must exist). ``docker cp`` writes
    as root, so chown to the unprivileged ``fauna`` user (UID 1000) and 0600 the
    key, else the nest can't read it and ``reload_default_from_pem`` fails silently.

    Shared by the cert-honesty tier_4 tests (priority #2)."""
    import tempfile

    with tempfile.NamedTemporaryFile(suffix=".pem") as cf, \
            tempfile.NamedTemporaryFile(suffix=".pem") as kf:
        kf.write(key_pem); kf.flush()
        cf.write(fullchain_pem); cf.flush()
        # Key first, then chain: when the watcher debounces and reads, both are new.
        subprocess.run(["docker", "cp", kf.name, f"{container}:{acme_dir}/privkey.pem"],
                       check=True, timeout=30)
        subprocess.run(["docker", "cp", cf.name, f"{container}:{acme_dir}/fullchain.pem"],
                       check=True, timeout=30)
    subprocess.run(
        ["docker", "exec", container, "sh", "-c",
         f"chown fauna:fauna {acme_dir}/privkey.pem {acme_dir}/fullchain.pem && "
         f"chmod 0600 {acme_dir}/privkey.pem && chmod 0644 {acme_dir}/fullchain.pem && "
         f"touch {acme_dir}/fullchain.pem"],
        check=True, timeout=30,
    )


# A QUIC version no server speaks: RFC 9000 § 15 reserves every 0x?a?a?a?a
# version for exactly this, forcing version negotiation.
_QUIC_GREASE_VERSION = 0x1A2A3A4A


def quic_version_negotiation(host: str, port: int, timeout: float = 3.0):
    """Send one unsolicited QUIC long-header datagram on UDP ``host:port`` and
    return ``(versions, reply_len)`` from the Version Negotiation packet a QUIC
    server answers with, or ``None`` when nothing answers within ``timeout``.

    The datagram offers a reserved version (``_QUIC_GREASE_VERSION``) padded to
    the 1200 bytes a client Initial must fill, so any QUIC server on the port
    answers — no TLS, no certificate, no QUIC library needed. A Version
    Negotiation packet carries only the two connection ids it echoes and the
    versions the server speaks: proof a QUIC server listens, and nothing more.
    """
    dcid, scid = secrets.token_bytes(8), secrets.token_bytes(8)
    header = (bytes([0xC0]) + _QUIC_GREASE_VERSION.to_bytes(4, "big")
              + bytes([len(dcid)]) + dcid + bytes([len(scid)]) + scid)
    datagram = header + bytes(1200 - len(header))
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(timeout)
        sock.sendto(datagram, (host, port))
        try:
            reply, _ = sock.recvfrom(65535)
        except (socket.timeout, ConnectionRefusedError):
            return None
    # Long header, version 0 = Version Negotiation; it echoes our scid as its
    # dcid and our dcid as its scid (RFC 9000 § 17.2.1).
    if len(reply) < 7 or not reply[0] & 0x80 or reply[1:5] != bytes(4):
        raise AssertionError(f"UDP {host}:{port} answered with no Version "
                             f"Negotiation packet: {reply[:32].hex()}")
    i = 5
    rd = reply[i + 1:i + 1 + reply[i]]
    i += 1 + reply[i]
    rs = reply[i + 1:i + 1 + reply[i]]
    i += 1 + reply[i]
    if rd != scid or rs != dcid:
        raise AssertionError("Version Negotiation did not echo our connection ids")
    versions = [int.from_bytes(reply[j:j + 4], "big")
                for j in range(i, len(reply) - 3, 4)]
    return versions, len(reply)
