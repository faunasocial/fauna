"""E2E test: Docker nest image integration test.

Boots the nest image (the shared `fauna-nest-test:local` tag — `helpers.docker_build`
reuses it and never builds on a dev VM), starts a container, verifies
health/setup-status endpoints, claims admin via API, and checks admin
status is visible in the web app via Playwright.
"""

import json
import pathlib
import ssl
import subprocess
import urllib.request

import pytest

from helpers.tls_spki import served_leaf

from .helpers import (
    claim_admin_api,
    docker_build,
    fence_cpuset_args,
    find_free_port,
    get_repo_root,
    remove_container,
    start_container,
    wait_for_health,
)

try:
    subprocess.run(
        ["docker", "info"],
        capture_output=True,
        timeout=10,
    )
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# `web` scopes these web-SPA-driven docker tests to --client web (they were
# wrongly "client-independent", so a non-web --include-independent run pulled
# them in); the marker is a no-op for the no-`--client` `just e2e-tier-4-test`.
pytestmark = [pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"), pytest.mark.tier_4, pytest.mark.self_contained_docker, pytest.mark.web]


DOMAIN = "localhost"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """The nest image tag — reused, never built here (`helpers.docker_build`)."""
    yield docker_build(get_repo_root())
    # Leave the tag in place: since reuse became the default (2026-08-28) it is the
    # shared, long-lived asset every docker module and every sibling session
    # boots. The `docker rmi -f` this fixture used to run on teardown untagged it
    # out from under whichever module ran next — and this module's private
    # `docker buildx build` copy, retired with it, was the dev-VM image build the
    # shared helper exists to refuse.


@pytest.fixture()
def nest_container(docker_image):
    """Start a nest container with injected claim code, wait for health.

    Yields (port, claim_code). Cleans up after.
    """
    port = find_free_port()
    name = f"fauna-nest-e2e-{port}"
    claim_code = "TE5T01"
    # No FAUNA_DOMAIN: the entrypoint is domainless now (it ignores the var).
    # The box boots on the `localhost` identity fallback; a domained claim (see
    # test_real_hostname_serves_floor_https_under_acme) is what gives it a real
    # identity.
    start_container(name, port, env={
        "FAUNA_MODE": "public",
        "FAUNA_CLAIM_CODE": claim_code,
    })
    try:
        wait_for_health(port, name)
        yield (port, claim_code)
    finally:
        remove_container(name)


# ── Tests ─────────────────────────────────────────────────────────────


@pytest.mark.feature("get-the-app")
def test_docker_image_structure(docker_image):
    """Verify the shipped binaries exist and fauna user has UID 1000.

    `fauna-bridge-imap` was retired (during the mail-bridge migration); the mail path
    now ships `fauna-mail-bridge` (MTA+MDA roles) + the `fauna-supervisor`
    sidekick. See docs/goal/behavior/mail-bridge-lifecycle.md. `fauna-dns` was
    removed when nest-side DNS writes were retired (only the client writes DNS).
    """
    binaries = [
        "fauna-nest",
        "fauna-mail-bridge",
        "fauna-supervisor",
        # The out-of-process ATProto PDS bridge (role atproto.pds; cgo since
        # S2 — it links the shared libfauna_ffi.so to unseal its key blob).
        # docs/goal/behavior/atproto-pds-bridge.md § Architecture.
        "fauna-atproto-bridge",
    ]
    for binary in binaries:
        result = subprocess.run(
            ["docker", "run", "--rm", "--entrypoint", "test",
             docker_image, "-f", f"/usr/local/bin/{binary}"],
            capture_output=True, text=True, timeout=30,
        )
        assert result.returncode == 0, f"Binary {binary} not found in image"

    # Convention 15 on the bridge that actually SHIPS
    # (e2e-automation-surface-gating.md § Implementation status today → the Go
    # bridges' leg): the image's fauna-atproto-bridge is the production flavor,
    # so none of the e2e seam literals — the hostile-rotation flag name and the
    # three FAUNA_ATPROTO_* harness redirect variables — may appear in it. The
    # recipe-level witness proves the justfile's build; this proves the
    # Dockerfile's, the one build path no recipe runs. `grep -a` inside the
    # image (the runtime layer has no binutils, so no `strings`); grep exits 1
    # when the pattern is absent, which is the pass.
    for literal in (
        "seize-did",
        "FAUNA_ATPROTO_FAKE_DNS_URL",
        "FAUNA_ATPROTO_PLC_DIRECTORY_URL",
        "FAUNA_ATPROTO_PROXY_FIXTURES",
    ):
        result = subprocess.run(
            ["docker", "run", "--rm", "--entrypoint", "grep", docker_image,
             "-a", "-q", "--", literal, "/usr/local/bin/fauna-atproto-bridge"],
            capture_output=True, text=True, timeout=60,
        )
        assert result.returncode == 1, (
            f"shipped fauna-atproto-bridge: e2e seam literal {literal!r} is "
            + ("PRESENT — the image carries a test-only seam (convention 15)"
               if result.returncode == 0
               else f"unverifiable, grep failed: {result.stderr.strip()}")
        )

    # Check fauna user UID
    result = subprocess.run(
        ["docker", "run", "--rm", "--entrypoint", "id",
         docker_image, "-u", "fauna"],
        capture_output=True, text=True, timeout=30,
    )
    assert result.returncode == 0, f"fauna user not found: {result.stderr}"
    assert result.stdout.strip() == "1000", (
        f"Expected UID 1000, got {result.stdout.strip()}"
    )


@pytest.mark.feature("get-the-app")
def test_docker_nest_health(nest_container):
    """Health endpoint returns ok."""
    import ssl
    port, _ = nest_container
    resp = urllib.request.urlopen(
        f"https://127.0.0.1:{port}/api/v1/health", timeout=10,
        context=ssl._create_unverified_context(),
    )
    data = json.loads(resp.read())
    assert data["status"] == "ok"
    assert "version" in data


def test_docker_nest_runs_inside_the_core_fence(nest_container):
    """A harness nest is pinned to the CPU set the build helper reports: docker
    places it in system.slice, outside the slices that set covers, so
    `docker run` pins it to those CPUs and a background agent keeps it in
    step with them after that. Docker's record
    and the kernel's agree, and name CPUs whenever there is a CPU set to join;
    where there is none the container is left on every CPU. (Which CPUs is
    the agent's to move — compared with no clock here.)"""
    port, _ = nest_container
    name = f"fauna-nest-e2e-{port}"
    inspect = subprocess.run(
        ["docker", "inspect", "-f", "{{.Id}}\t{{.HostConfig.CpusetCpus}}", name],
        capture_output=True, text=True, timeout=30,
    )
    assert inspect.returncode == 0, inspect.stderr
    cid, _, cpuset = inspect.stdout.strip().partition("\t")
    if not fence_cpuset_args():
        assert cpuset == "", f"no fence to join, yet {name} is pinned to {cpuset}"
        return
    assert cpuset, f"{name} runs on every CPU although the build helper reports a CPU set"
    cgroup = pathlib.Path(f"/sys/fs/cgroup/system.slice/docker-{cid}.scope/cpuset.cpus.effective")
    assert cgroup.read_text().strip() == cpuset, (
        f"docker says {name} is on {cpuset}; its cgroup says {cgroup.read_text().strip()}"
    )


def test_docker_nest_setup_status(nest_container):
    """Setup-status shows domain and unclaimed admin.

    Migrated off the deprecated ``GET /api/v1/setup-status`` HTTP twin (deleted
    in S4c2) onto the anonymous WS-RPC kind. The typed WS reply flattens the
    HTTP twin's nested ``admin: {exists}`` object to a top-level
    ``admin_exists`` boolean (same information — see
    ``libs/fauna-protocol/src/discovery.rs::SetupStatusReply``).
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    port, _ = nest_container
    with WsRpcAnonClient(f"https://127.0.0.1:{port}") as anon:
        data = anon.call("fauna.setup.status", {})
    assert data["domain"] == DOMAIN
    assert data["admin_exists"] is False


@pytest.mark.feature("claim-a-fresh-nest")
def test_docker_nest_claim_admin(nest_container):
    """Claim admin via the real wire, then verify am-i-admin over WS-RPC.

    Both legs go over the deployed image's only client surface: the anonymous
    WS-RPC ``fauna.auth.claim_admin`` (inside ``claim_admin_api``) and the
    bearer-authed ``fauna.account.am_i_admin`` read. The legacy
    ``GET /api/v1/am-i-admin`` HTTP twin was removed in the WS-RPC migration
    (the path now serves the landing-page HTML), so the typed reply is the
    only check left — ``AmIAdminReply { admin: bool }``
    (``libs/fauna-protocol/src/account.rs``).
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    port, claim_code = nest_container
    admin = claim_admin_api(port, claim_code)

    with WsRpcAdminClient(
        f"https://127.0.0.1:{port}",
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        reply = client.call("fauna.account.am_i_admin", {})
    assert reply["admin"] is True


@pytest.mark.feature("certificates-by-themselves")
def test_real_hostname_serves_floor_https_under_acme(docker_image):
    """A nest whose identity is a **real** hostname (not `localhost`) that can't
    complete a public ACME order — a fully-local LAN/home box behind NAT, no public
    DNS / inbound :80 — still serves HTTPS *immediately* via the always-live
    self-signed floor, so it is reachable for onboarding + CalDAV without any
    operator knob.

    Domainless boot (the entrypoint no longer reads `FAUNA_DOMAIN`): the box comes
    up on the `localhost` floor, then a **domained claim** (`mail_domain=
    fauna.home.test`) makes `fauna.home.test` the primary identity. ACME is derived
    on for a non-`localhost` domain, so the nest *does* launch an HTTP-01 order it
    can never complete. Before the unconditional floor that bound a *pending* TLS
    resolver and hung forever (`wait_for_health` timed out), which is why
    `FAUNA_SELF_SIGNED` existed. Now `main.rs`'s `prepare_listener_tls` writes
    `write_self_signed_bootstrap` **unconditionally** (tls-certificates.md § A: the
    floor is always live) and the claim self-heals it to cover `fauna.home.test`, so
    the floor serves the listener while ACME retries harmlessly in the background —
    and the retired `FAUNA_SELF_SIGNED` knob is no longer needed. Regression guard
    for the unconditional-floor change in `prepare_listener_tls` + the
    claim-authoritative identity (`identity_domain_core::apply_primary_identity`).
    (Health succeeding over verify-off https *is* the proof a cert is being served —
    the handshake completes.)

    But a completed handshake proves *a* cert is served, not *which*. The final
    assert therefore reads the served leaf's issuer: a self-signed floor has
    issuer DN == subject DN, a CA-issued leaf does not. Without it this test would
    still pass against a box that had somehow obtained a trusted cert — the
    opposite of what it exists to pin. It is also the assertion the firewalled-:80
    track needs, since the floor is exactly the certificate a browser will not
    carry a WSS handshake over (transport.md § Connection-status indicator).
    """
    port = find_free_port()
    name = f"fauna-nest-floorhttps-{port}"
    claim_code = "TE5T02"
    start_container(name, port, env={"FAUNA_MODE": "public", "FAUNA_CLAIM_CODE": claim_code})
    try:
        # Domainless boot serves the `localhost` floor over https first.
        wait_for_health(port, name)
        # A domained claim makes `fauna.home.test` the primary identity → ACME on for
        # a real (un-orderable — no public DNS) hostname + the claim self-heals the
        # floor to cover it. The always-live floor keeps serving https while the
        # never-completable order retries harmlessly — the case that used to hang
        # before the unconditional floor.
        claim_admin_api(port, claim_code, handle="admin", mail_domain="fauna.home.test")
        wait_for_health(port, name)

        leaf = served_leaf("127.0.0.1", port, sni="fauna.home.test")
        issuer = leaf.issuer.rfc4514_string()
        subject = leaf.subject.rfc4514_string()
        assert issuer == subject, (
            "expected the always-live SELF-SIGNED floor (issuer DN == subject DN) on a box "
            "that can never complete a public ACME order, but the served leaf is CA-issued: "
            f"issuer={issuer!r} subject={subject!r}"
        )
    finally:
        remove_container(name)
