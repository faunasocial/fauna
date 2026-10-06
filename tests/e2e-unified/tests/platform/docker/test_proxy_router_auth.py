"""tier_4 e2e: the **router-auth secret** that distinguishes the SNI router from
a co-resident bridge as the one legitimate PROXY-v2 header writer (security.md §
Co-resident process trust boundary).

After the UID split (slice 1), the mail bridges (`fauna-mta`/`fauna-mda`) are
*also* loopback peers of nest, so loopback origin alone no longer proves a PROXY
header came from the router — a compromised bridge could forge one to spoof a
source IP (evading per-source rate limits / poisoning the MDA AUTH-lockout key).
`SO_PEERCRED` can't tell them apart on the TCP-loopback router→nest hop, so the
router proves itself with a shared secret it appends as a PROXY-v2 TLV that nest
verifies. The *rejection logic* is proven by tier_3 unit tests
(`read_optional_proxy_header` / `fauna_proxy_protocol`); this tier_4 test covers
the **deployment/packaging** half those bypass — the secret's provisioning,
UID-isolation, and run-script env wiring inside the real image:

  A. The secret file `/data/keys/router/proxy-secret` is provisioned root-only
     0600, non-empty hex — and a bridge UID is DENIED reading it.
  B. Every process the secret is env-passed to carries it with the env channel
     UID-isolated: a bridge UID is DENIED reading their /proc/<pid>/environ. Since
     2026-08-14 that set is FOUR services, not two — the MDA's DAV listener and
     the ATProto PDS bridge's XRPC listener peel PROXY-v2 on their own loopback
     binds, so they verify the TLV too (this check is per-peel-point, not nest-only) and
     their run-scripts read the same file as root. The assertion below iterates
     whichever of the four are supervised-up, so on this always-up-only fixture it
     covers nest + router; a mail-enabled fixture below brings the MDA up and
     witnesses its isolation directly (the follow-on), rather than relying
     on the "skip if absent" fallback. Their isolation rests on the same property
     already witnessed here for nest/router — s6-setuidgid clears the dumpable bit.
  C. The secret is NOT on any process's world-readable /proc/<pid>/cmdline (it is
     exported, never passed on argv).
  D. nest + the always-up router still serve (the secret wiring didn't break
     boot).

Only tier_4 catches A–C: the wiring lives entirely in the entrypoint + s6
run-scripts, which the binary-spawning tier_3 fixtures bypass.
"""

import re
import subprocess
import time

import pytest

from .helpers import (
    IMAGE_TAG,
    ROLES,
    admin_ws as _admin_ws,
    await_approved_bridges as _await_approved_bridges,
    await_keypairs as _await_keypairs,
    claim_admin_api,
    docker_build,
    find_free_ports,
    flag_present as _flag_present,
    get_repo_root,
    is_commanded_up as _is_commanded_up,
    register_primary_domain as _register_primary_domain,
    remove_container,
    restart_service as _restart_service,
    start_container_with_ports,
    svstat as _svstat,
    wait_for_health,
    wait_for_smtp_banner,
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

CLAIM_CODE = "PROXYAUTH"
DOMAIN = "localhost"
SECRET_PATH = "/data/keys/router/proxy-secret"
NEST_UID = 1000
ROUTER_UID = 1003
MTA_UID = 1001
MDA_UID = 1002


# ── docker-exec helpers (mirror test_uid_isolation.py; candidates for a future
# lift into helpers.py once the tier_4 docker suite consolidates them) ─────────


def _exec_as(name: str, uid: int, *cmd: str) -> tuple[int, bytes, str]:
    """Run a command in the container as a numeric UID; stdout BYTES."""
    r = subprocess.run(
        ["docker", "exec", "-u", str(uid), name, *cmd],
        capture_output=True, timeout=15,
    )
    return r.returncode, r.stdout, r.stderr.decode("utf-8", "replace")


def _service_pid(name: str, service: str) -> int | None:
    m = re.search(r"\(pid (\d+)", _svstat(name, service))
    return int(m.group(1)) if m else None


def _stat(name: str, path: str) -> tuple[int, int, str] | None:
    """(uid, gid, octal-mode) of an in-container path, statted as root."""
    r = subprocess.run(
        ["docker", "exec", "-u", "0", name, "stat", "-c", "%u %g %a", path],
        capture_output=True, text=True, timeout=10,
    )
    if r.returncode != 0:
        return None
    uid, gid, mode = r.stdout.split()
    return int(uid), int(gid), mode


# ── Fixture: a fresh image, just booted (no mail bring-up needed — the router is
# always-up and the secret is provisioned at entrypoint) ───────────────────────


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def booted_nest(docker_image):
    http_port, *_ = find_free_ports(1)
    name = f"fauna-proxy-auth-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        yield {"name": name, "port": http_port}
    finally:
        remove_container(name)


# ── Tests ──────────────────────────────────────────────────────────────


@pytest.mark.feature("nest-hardening")
def test_router_secret_file_is_root_only_and_bridges_denied(booted_nest):
    """(A) The secret is provisioned root:root 0600, non-empty hex; a bridge UID
    cannot read it."""
    name = booted_nest["name"]

    st = _stat(name, SECRET_PATH)
    assert st is not None, f"{SECRET_PATH} must be provisioned at entrypoint"
    uid, _gid, mode = st
    assert (uid, mode) == (0, "600"), f"{SECRET_PATH} should be root:* 0600; got {st}"

    rc, out, _ = _exec_as(name, 0, "cat", SECRET_PATH)
    assert rc == 0 and out, "root must read the secret"
    secret = out.decode("ascii", "strict").strip()
    assert len(secret) == 64 and re.fullmatch(r"[0-9a-f]+", secret), \
        f"secret should be 64 hex chars (32 bytes); got {len(secret)} chars"

    for uid in (MTA_UID, MDA_UID):
        rc, out, _ = _exec_as(name, uid, "cat", SECRET_PATH)
        assert rc != 0, f"uid {uid} must NOT read {SECRET_PATH} (rc=0)"
        assert not out, f"uid {uid} got secret bytes back"


@pytest.mark.feature("nest-hardening")
def test_secret_env_channel_is_non_dumpable_and_isolated(booted_nest):
    """(B) The env channel carrying the secret into nest + the router is fully
    isolated. nest and the router run privilege-dropped (s6-setuidgid), which
    clears the dumpable bit, so their /proc/<pid>/environ is unreadable by a
    co-resident bridge UID AND by root-without-CAP_SYS_PTRACE (the exec'd root
    here) — stronger than the file's 0600. So the secret can leak via neither the
    root-only file (asserted above) nor the env.

    We deliberately do NOT read the env value back to compare it to the file: a
    non-dumpable process's environ is the property under test, so it's
    unreadable by construction. That the run-script export actually wired the
    secret in is exercised by the router-fronted serving path itself — nest comes
    up router-fronted and serves (test D); the PROXY-header accept/reject *logic*
    is covered by the tier_3 `read_optional_proxy_header` / `fauna_proxy_protocol`
    units this file's docstring names."""
    name = booted_nest["name"]
    nest_pid = _service_pid(name, "fauna-nest")
    router_pid = _service_pid(name, "fauna-sni-router")
    assert nest_pid and router_pid, "nest + router must be supervised-up"

    # Every service whose run-script env-passes the secret. The two bridges peel
    # PROXY-v2 on their own loopback binds and so verify the TLV themselves
    # (this check is per-peel-point); they are down on this fixture, so they contribute
    # nothing here and everything on a fixture that brings them up.
    holders = [(router_pid, "router"), (nest_pid, "nest")]
    for svc, who in (
        ("fauna-mail-bridge-mda", "MDA"),
        ("fauna-atproto-bridge", "PDS bridge"),
    ):
        if pid := _service_pid(name, svc):
            holders.append((pid, who))

    # Neither a co-resident bridge UID nor root-sans-CAP_SYS_PTRACE can read any
    # holder's environ — the env channel is closed, not just the file.
    for pid, who in holders:
        for uid in (MTA_UID, MDA_UID, 0):
            rc, out, _ = _exec_as(name, uid, "cat", f"/proc/{pid}/environ")
            assert rc != 0 and not out, (
                f"uid {uid} must NOT read the {who}'s /proc/{pid}/environ "
                f"(non-dumpable, no CAP_SYS_PTRACE); got rc={rc}"
            )


@pytest.mark.feature("nest-hardening")
def test_secret_not_on_any_world_readable_cmdline(booted_nest):
    """(C) The secret is exported, never on argv — so it appears on no process's
    world-readable /proc/<pid>/cmdline (a bridge UID can read every cmdline)."""
    name = booted_nest["name"]
    file_secret = _exec_as(name, 0, "cat", SECRET_PATH)[1].decode("ascii").strip()
    # Grep every cmdline (read as an unprivileged bridge UID, the attacker's view).
    rc, out, _ = _exec_as(
        name, MTA_UID, "sh", "-c",
        "for f in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < \"$f\" 2>/dev/null; echo; done",
    )
    cmdlines = out.decode("utf-8", "replace")
    assert file_secret not in cmdlines, \
        "the router-auth secret must never appear on a world-readable cmdline"


def test_router_and_nest_still_serve(booted_nest):
    """(D) The secret wiring didn't break boot: nest healthy + router up."""
    name = booted_nest["name"]
    wait_for_health(booted_nest["port"], name)
    # s6-svstat prints "up (pid N pgid N) S seconds" — match the leading
    # "up (pid N", not a bare "(pid N)" (the pgid breaks a closing-paren anchor).
    assert re.search(r"up \(pid \d+", _svstat(name, "fauna-sni-router")), \
        f"the always-up SNI router must be supervised; got {_svstat(name, 'fauna-sni-router')!r}"


# ── Fixture: mail-enabled bring-up — the MDA's DAV
# listener is a second peel point that `booted_nest` above never brings
# up, so its own env-isolation coverage silently no-ops on that fixture (the
# "if pid" skip in test (B) above). Bring-up mirrors test_uid_isolation.py's
# `serving_nest`. ─────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def booted_nest_mail_enabled(docker_image):
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-proxy-auth-mail-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
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
        }
        # Domain BEFORE enable — mta/mda stay idle with no listeners until
        # LocalDomains is non-empty at their post-approval cold boot.
        _register_primary_domain(nest, DOMAIN)
        with _admin_ws(nest) as admin_conn:
            assert admin_conn.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if _flag_present(name) and all(
                    _is_commanded_up(_svstat(name, f"fauna-mail-bridge-{r}")) for r in ROLES
                ):
                    break
                time.sleep(0.5)
            assert _flag_present(name), "/data/imap-enabled must materialize on set_mail_enabled(true)"
            _await_keypairs(name)
            approved = _await_approved_bridges(admin_conn)
            for r in ROLES:
                assert r in approved, f"{r} bridge must auto-approve; got {list(approved)}"
            # Restart so each cold-boots as approved and re-binds under its own UID.
            for r in ROLES:
                _restart_service(name, f"fauna-mail-bridge-{r}")
        services = ("fauna-nest", "fauna-sni-router",
                    "fauna-mail-bridge-mta", "fauna-mail-bridge-mda")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if all(re.search(r"\(pid \d+\)", _svstat(name, s)) for s in services):
                break
            time.sleep(0.5)
        wait_for_smtp_banner("127.0.0.1", mail_ports[25], timeout=60)
        yield nest
    finally:
        remove_container(name)


@pytest.mark.feature("nest-hardening")
def test_mda_env_channel_is_non_dumpable_and_isolated(booted_nest_mail_enabled):
    """(B, mail-enabled) The MDA's DAV listener peels PROXY-v2 on its own
    loopback bind (this check is per-peel-point, not nest-only), so its run-script
    carries the router-auth secret the same way nest's and the router's do —
    this witnesses that isolation DIRECTLY on the MDA, rather than relying on
    the "skip if absent" fallback in the always-up-only fixture's version of
    this assertion. Same property, same reasoning as
    `test_secret_env_channel_is_non_dumpable_and_isolated`: neither a peer
    bridge UID nor root-sans-CAP_SYS_PTRACE can read the MDA's
    /proc/<pid>/environ."""
    name = booted_nest_mail_enabled["name"]
    mda_pid = _service_pid(name, "fauna-mail-bridge-mda")
    assert mda_pid is not None, (
        "the MDA must be supervised-up on the mail-enabled fixture — a missing "
        "pid here would silently no-op the isolation asserts below"
    )
    for uid in (MTA_UID, MDA_UID, 0):
        rc, out, _ = _exec_as(name, uid, "cat", f"/proc/{mda_pid}/environ")
        assert rc != 0 and not out, (
            f"uid {uid} must NOT read the MDA's /proc/{mda_pid}/environ "
            f"(non-dumpable, no CAP_SYS_PTRACE); got rc={rc}"
        )
