"""tier_4 e2e: the mail-bridge **deploy lifecycle** inside the real Docker image.

Stage 5 of the mail-deployment VPS milestone
(tracked internally). This is the
only test that drives the *new* packaged deploy model end-to-end against the
built image — s6 services, the ``fauna-supervisor`` sidekick socket, the
``/data/imap-enabled`` flag, and the WS-RPC enable/approve surface running
together for the first time. The process-level ``mail_bridge_mta`` /
``mail_bridge_mda`` conftest fixtures deliberately bypass all of that
(operator-hatch ports + legacy-HTTP approve), so they cannot catch the
between-binaries wiring this exercises.

**Spec:** ``docs/goal/behavior/mail-bridge-lifecycle.md`` § Default-off on first
claim (the enable handshake), § Pending approval (enrollment), § Wire shapes;
``docs/goal/architecture/installers/docker.md`` § Supervisor sidekick socket.

What this asserts (the full enable → approve → serve lifecycle):
  A. A fresh claimed nest boots in the image (regression guard for the
     duplicate-``[acme]`` entrypoint crash-loop fixed in this milestone).
  B. ``fauna.bridges.set_mail_enabled(true)`` materializes ``/data/imap-enabled``
     and the supervisor sidekick socket flips **both** ``fauna-mail-bridge-{mta,
     mda}`` s6 services from idle-down to commanded-up (``s6-svc -u`` fired).
  C. Each role's bridge process auto-generates its Ed25519 service-user keypair
     at ``/data/keys/{mta,mda}.key`` on first run (no operator seeding).
  D. The admin takes both bridges ``pending → approved`` over WS-RPC; each role,
     on its restart-as-approved cold boot, attests its real x25519 pubkey (Gap E
     fix), and the admin's self-signed cert (``POST …/self_signed_cert``) then
     fans out **sealed** to both (``bridges_sealed_to``) — which the cert handler
     only does for approved bridges with an attested x25519, so it is the
     observable proof of the attestation. Corroborated by ``has_x25519`` in
     ``list_service_users``.
  E. The MTA serves a plaintext ``220`` banner on 25 (inbound MX) and 587
     (submission STARTTLS) — these bind once the bridge has a primary domain.
  G. After a SIGHUP cert-refresh, 465 (SMTPS) and 993 (IMAPS) complete a TLS
     handshake — the observable proof the cert reached + unsealed on the bridge.

The primary mail domain is registered **before** enable on purpose: ``mta.Run``/
``mda.Run`` idle with no listeners while ``LocalDomains`` is empty, and there is
no config-change push yet, so the domain must be present at the post-approval
cold boot for the bridges to build a TLS provider and bind 465/587/993.

**Gap B** (zero-touch enrollment) has landed: each Go bridge
self-enrolls a ``pending`` row over the loopback pre-identity WS
(``fauna.bridges.request_enrollment``) on cold boot, so this test **waits for the
auto-enrolled pending rows** and approves them — no admin pre-registration (that
path now 409s on the bridge's existing row).
"""

import subprocess
import time

import pytest


from .helpers import (
    IMAGE_TAG,
    ROLES,
    admin_ws as _admin_ws,
    approved_docker_bridges as _approved_docker_bridges,
    await_keypairs as _await_keypairs,
    bridge_diag as _bridge_diag,
    claim_admin_api,
    docker_build,
    find_free_ports,
    flag_present as _flag_present,
    get_repo_root,
    await_approved_bridges as _await_approved_bridges,
    is_commanded_up as _is_commanded_up,
    provision_self_signed_cert as _provision_self_signed_cert,
    register_primary_domain as _register_primary_domain,
    remove_container,
    restart_service as _restart_service,
    sighup_service as _sighup_service,
    start_container_with_ports,
    svstat as _svstat,
    wait_for_health,
    wait_for_smtp_banner,
    wait_for_tls_handshake,
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

DOMAIN = "localhost"
CLAIM_CODE = "DEPL5T"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module. Tag is shared with the other
    docker tests; build is layer-cached so a rebuild is cheap."""
    docker_build(get_repo_root())
    yield IMAGE_TAG
    # Leave the image in place — the tag is the shared, long-lived asset every
    # docker module (and every sibling session) reuses since 2026-08-28.


@pytest.fixture()
def claimed_nest(docker_image):
    """A fresh container with the nest HTTP API + all four mail listener ports
    (25/465/587/993) mapped to the host, admin claimed, storage mode committed.
    Yields a dict with the mapped host ports + admin creds. Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-deploy-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        # TLS cert provisioning (and CalDAV) need a committed storage mode;
        # plaintext is the "I trust the box" deploy default.
        yield {
            "name": name,
            "port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
        }
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_mail_deploy_lifecycle_to_serving(claimed_nest):
    nest = claimed_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port

    # (pre) Register the primary mail domain BEFORE enabling. mta/mda.Run idle
    # with no listeners while LocalDomains is empty (and no config-change push
    # exists), so the domain must be present at the post-approval cold boot for
    # the bridges to build a TLS provider and bind 465/587/993.
    _register_primary_domain(nest, DOMAIN)

    with _admin_ws(nest) as admin:
        # (B) Enable mail → flag file + supervisor-socket bring-up of both roles.
        assert admin.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if _flag_present(name) and all(_is_commanded_up(_svstat(name, f"fauna-mail-bridge-{r}")) for r in ROLES):
                break
            time.sleep(0.5)
        assert _flag_present(name), "/data/imap-enabled must materialize on set_mail_enabled(true)"
        for r in ROLES:
            st = _svstat(name, f"fauna-mail-bridge-{r}")
            assert _is_commanded_up(st), f"{r} s6 service must be commanded up via the supervisor socket; got: {st!r}"

        # (C) Each role auto-generates its keypair on first run.
        _await_keypairs(name)

        # (D) Zero-touch enrollment + auto-approval: each bridge self-enrolls over
        # the loopback pre-identity WS (Gap B) and, because admin mail
        # is already enabled, is auto-approved in ONE step (`573267935`;
        # `mail-bridge-lifecycle.md` § Onboarding auto-approval) — no pending window
        # to observe, no manual `approve_pending_bridge`. Wait for both `approved`.
        approved_by_role = _await_approved_bridges(admin)
        for r in ROLES:
            assert r in approved_by_role, f"{r} bridge must reach approved (auto-approve on mail-enable); approved={list(approved_by_role)}"
            assert approved_by_role[r]["status"] == "approved"

        # Restart both bridges so each re-runs its cold boot against the
        # now-approved state immediately. With the zero-touch poll loop the bridge
        # self-advances after approval, so this is a deterministic nudge (re-enroll
        # is idempotent), not a necessity.
        for r in ROLES:
            _restart_service(name, f"fauna-mail-bridge-{r}")

        try:
            # (D+F) Each role, on its restart-as-approved cold boot, attests its
            # real x25519 pubkey (Gap E), and the admin's self-signed cert then
            # fans out *sealed* to it. approve leaves x25519 NULL and the cert
            # handler skips approved bridges without an attested x25519
            # (bridges_skipped_no_x25519), so "cert sealed to BOTH roles" is the
            # observable proof the register_service_user round-trip landed — a
            # stronger gate than reading nest's projection. The cert call is
            # idempotent; poll it because the bridge needs a few seconds to
            # re-dial + attest after the restart.
            deadline = time.monotonic() + 60
            sealed_roles: set[str] = set()
            reply: dict = {}
            while time.monotonic() < deadline:
                reply = _provision_self_signed_cert(nest, DOMAIN)
                sealed_roles = {b["role"] for b in reply.get("bridges_sealed_to", [])}
                if set(ROLES) <= sealed_roles:
                    break
                time.sleep(3.0)
            assert set(ROLES) <= sealed_roles, (
                "cert must fan out sealed to both roles (proves x25519 attestation, "
                f"Gap E); last sealed={sealed_roles}, "
                f"skipped={[b['role'] for b in reply.get('bridges_skipped_no_x25519', [])]}"
            )
            # Corroborate at the nest-state level — list_service_users reports a
            # per-row `has_x25519` bool (the hex pubkey is only on the whoami reply).
            by_role = _approved_docker_bridges(admin)
            for r in ROLES:
                assert by_role.get(r, {}).get("has_x25519") is True, (
                    f"{r} must report has_x25519 after attestation; got {by_role.get(r)}"
                )

            # (E) 25 (inbound MX) + 587 (submission STARTTLS) serve a plaintext
            # 220 banner once the bridge has a primary domain — no cert needed
            # (STARTTLS is advertised, not required, for the banner itself).
            wait_for_smtp_banner("127.0.0.1", mp[25], timeout=60)
            wait_for_smtp_banner("127.0.0.1", mp[587], timeout=60)

            # (G) Nudge an immediate cert refresh (SIGHUP) rather than waiting
            # out the 1-minute backoff, then assert the implicit-TLS listeners
            # complete a handshake — the observable proof the cert reached +
            # unsealed on the bridge. 465 = SMTPS (220 over TLS), 993 = IMAPS
            # (* greeting over TLS).
            for r in ROLES:
                _sighup_service(name, f"fauna-mail-bridge-{r}")
            wait_for_tls_handshake("127.0.0.1", mp[465], expect_banner_prefixes=(b"220 ", b"220-"), timeout=60)
            wait_for_tls_handshake("127.0.0.1", mp[993], expect_banner_prefixes=(b"* OK", b"* "), timeout=60)
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{_bridge_diag(name)}") from e
