"""tier_4 e2e: the co-resident process **UID isolation** split inside the real
Docker image (security.md § Co-resident process trust boundary).

Slice 1 of the user-decided (2026-06-24) UID/container split: each network-facing
s6 service runs under its OWN non-root UID — nest=`fauna`(1000),
MTA=`fauna-mta`(1001), MDA=`fauna-mda`(1002), router=`fauna-router`(1003) — so a
compromised process (notably the hostile-MIME-parsing MDA) cannot read another
role's key material or nest's sealed store. Only tier_4 catches this: the UID
wiring lives entirely in the Dockerfile + entrypoint + s6 run-scripts, which the
binary-spawning tier_3 fixtures bypass.

What this asserts:
  A. Each service's running process has the expected distinct real-UID
     (s6-svstat pid → /proc/<pid>/status `Uid:`).
  B. Cross-UID reads are DENIED: a bridge UID cannot read the peer bridge's
     keyfile, nest's sealed store (nest.db / blobs), or the inbound-deliver-key.
  C. Each bridge CAN read what it legitimately needs: its own keyfile + the
     shared (secret-free) operator-hatch.toml.
  D. The keyfiles + per-role dirs have the expected owner + restrictive mode.
  E. After the split, every service still starts + serves (health, both bridge
     s6 services commanded-up, the MTA's 25/587 plaintext 220 banners, the
     always-up SNI router supervised).
  F. (slice 4) Both hostile-MIME parsers (MTA, MDA) run under fauna-sandbox —
     `Seccomp: 2` proves the seccomp half of the wrapper applied, and they still
     serve (the ambient-cap workaround keeps their privileged binds working).
  G. (slice 4) The Landlock filesystem default-deny is proven IN-DOMAIN: a read
     of the sealed store attempted inside a real `fauna-sandbox bridge` /
     `bridge-imap` Landlock domain — as ROOT, so plain DAC cannot be the denier
     — fails with EACCES, while the same unconfined read succeeds (sensitivity:
     these asserts go RED, not green, on a box where Landlock is off) and an
     allowed-path read under the same sandbox succeeds (selectivity: the wrapper
     isn't just broken). Each probe also asserts the wrapper reported an
     ENFORCED ruleset ("fully enforced" or "partially enforced" — never
     "not enforced"): `landlock::apply` deliberately continues UNSANDBOXED
     (warn-only) when the kernel lacks Landlock support
     (`bins/fauna-sandbox/src/landlock.rs`), a fallback `Seccomp: 2` alone can
     never distinguish from real enforcement. ("partially" is what current
     kernels report here empirically — an ABI-negotiation artifact, with the
     denials provably working; the BEHAVIORAL probes are the authority, the
     status line only rules out the silent-off state.)

The bring-up (enable → auto-approve → keypairs) mirrors test_mail_deploy_lifecycle
— the same supervisor-socket + zero-touch-enrollment path — so a regression that
breaks serving under the new UIDs fails here too (E), not just the isolation
checks.
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
    bridge_diag as _bridge_diag,
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

DOMAIN = "localhost"
CLAIM_CODE = "UIDISO"

# Expected real-UID per s6 service after the split (Dockerfile useradd UIDs).
EXPECTED_UID = {
    "fauna-nest": 1000,
    "fauna-mail-bridge-mta": 1001,
    "fauna-mail-bridge-mda": 1002,
    "fauna-sni-router": 1003,
}
MTA_UID = 1001
MDA_UID = 1002


# ── docker-exec helpers ───────────────────────────────────────────────


def _exec_as(name: str, uid: int, *cmd: str) -> tuple[int, bytes, str]:
    """Run a command in the container as a numeric UID; return (rc, out_bytes,
    err). stdout is BYTES — the keyfiles + nest.db are binary (DAG-CBOR / SQLite),
    so decoding as text would raise. Numeric `-u` works even when the UID has no
    shell / passwd-name resolution."""
    r = subprocess.run(
        ["docker", "exec", "-u", str(uid), name, *cmd],
        capture_output=True, timeout=15,
    )
    return r.returncode, r.stdout, r.stderr.decode("utf-8", "replace")


def _service_pid(name: str, service: str) -> int | None:
    """Parse the supervised process pid out of `s6-svstat` (`up (pid N pgid N) …`)."""
    m = re.search(r"\(pid (\d+)", _svstat(name, service))
    return int(m.group(1)) if m else None


def _proc_ruid(name: str, pid: int) -> int | None:
    """Real UID of an in-container process via /proc/<pid>/status `Uid:`."""
    out = subprocess.run(
        ["docker", "exec", "-u", "0", name, "cat", f"/proc/{pid}/status"],
        capture_output=True, text=True, timeout=10,
    ).stdout
    m = re.search(r"^Uid:\s+(\d+)", out, re.M)
    return int(m.group(1)) if m else None


def _proc_seccomp(name: str, pid: int) -> int | None:
    """Seccomp mode of an in-container process (/proc/<pid>/status `Seccomp:`):
    0=disabled, 1=strict, 2=filter. Under fauna-sandbox it is 2."""
    out = subprocess.run(
        ["docker", "exec", "-u", "0", name, "cat", f"/proc/{pid}/status"],
        capture_output=True, text=True, timeout=10,
    ).stdout
    m = re.search(r"^Seccomp:\s+(\d+)", out, re.M)
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


# ── Fixture: a fresh image brought all the way to mail-serving ─────────


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def serving_nest(docker_image):
    """One container, mail enabled + both bridges auto-approved + keypairs
    generated + serving. Module-scoped: the bring-up is expensive, and every
    assertion below is read-only against the same running container."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-uid-iso-{http_port}"
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
        # Domain BEFORE enable (mta/mda idle with no listeners until LocalDomains
        # is non-empty at their post-approval cold boot) — same as the lifecycle.
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
            _await_keypairs(name)  # both /data/keys/<role>/<role>.key generated
            approved = _await_approved_bridges(admin_conn)
            for r in ROLES:
                assert r in approved, f"{r} bridge must auto-approve; got {list(approved)}"
            # Restart so each cold-boots as approved (and re-binds its listeners
            # under its own UID) — deterministic nudge, re-enroll is idempotent.
            for r in ROLES:
                _restart_service(name, f"fauna-mail-bridge-{r}")
        # Settle: wait until every network-facing service reports a LIVE pid (not
        # merely "want up"), so the per-UID/process assertions see a stable state
        # and the MTA has re-bound its listeners under fauna-mta.
        services = ("fauna-nest", "fauna-sni-router",
                    "fauna-mail-bridge-mta", "fauna-mail-bridge-mda")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if all(re.search(r"\(pid \d+\)", _svstat(name, s)) for s in services):
                break
            time.sleep(1.0)
        wait_for_smtp_banner("127.0.0.1", mail_ports[25], timeout=60)
        yield nest
    finally:
        remove_container(name)


# ── Tests ──────────────────────────────────────────────────────────────


@pytest.mark.feature("nest-hardening")
def test_each_service_runs_under_its_own_uid(serving_nest):
    """(A) The four network-facing services run under distinct real-UIDs."""
    name = serving_nest["name"]
    for service, want in EXPECTED_UID.items():
        pid = _service_pid(name, service)
        assert pid is not None, f"{service} has no supervised pid: {_svstat(name, service)!r}"
        got = _proc_ruid(name, pid)
        assert got == want, f"{service} runs as uid {got}, expected {want}"


@pytest.mark.feature("nest-hardening")
def test_mail_bridges_run_under_fauna_sandbox(serving_nest):
    """(slice 4) Both hostile-MIME parsers (MTA, MDA) run under fauna-sandbox.
    `Seccomp: 2` (filter mode) proves the SECCOMP half of the wrapper applied.
    ⚠ It does NOT prove the Landlock half is enforced: `landlock::apply` warns
    and continues UNSANDBOXED on `RulesetStatus::NotEnforced` (a kernel without
    Landlock, or a container runtime whose seccomp profile blocks the landlock
    syscalls), and seccomp still applies afterwards — so `Seccomp: 2` greens on
    a box whose filesystem default-deny is silently OFF. The in-domain probes
    (`test_landlock_denies_sealed_store_inside_sandbox_domain`, (G)) are the
    enforcement proof. That the bridges still serve (the UID/serve tests + the
    lifecycle regression) proves the sandbox + the ambient-cap port-bind
    workaround did not break their 993/143 / 25/465/587 binds."""
    name = serving_nest["name"]
    for service in ("fauna-mail-bridge-mta", "fauna-mail-bridge-mda"):
        pid = _service_pid(name, service)
        assert pid is not None, f"{service} has no supervised pid: {_svstat(name, service)!r}"
        mode = _proc_seccomp(name, pid)
        assert mode == 2, (
            f"{service} must run under a seccomp filter (fauna-sandbox wrapper); "
            f"Seccomp={mode} (0=off,1=strict,2=filter)"
        )


@pytest.mark.feature("nest-hardening")
def test_bridge_uid_cannot_read_peer_key_or_sealed_store(serving_nest):
    """(B) A bridge UID is denied the peer bridge's key, nest's sealed store, and
    the nest-only inbound-deliver-key."""
    name = serving_nest["name"]

    # Positive control first: nest's OWN uid can read its sealed DB — proves the
    # path exists and is readable when DAC allows, so the denials below fail for
    # the permission reason, not because the file moved (a cat of a missing path
    # is rc!=0 too, which would green these asserts vacuously).
    rc, out, err = _exec_as(name, 1000, "head", "-c", "16", "/data/nest.db")
    assert rc == 0 and out.startswith(b"SQLite format 3"), \
        f"control: nest uid 1000 must read its own nest.db; rc={rc} out={out!r} err={err!r}"

    denied = [
        (MTA_UID, "/data/keys/mda/mda.key", "MTA must not read the MDA's keyfile"),
        (MDA_UID, "/data/keys/mta/mta.key", "MDA must not read the MTA's keyfile"),
        (MTA_UID, "/data/nest.db", "MTA must not read nest's sealed DB"),
        (MDA_UID, "/data/nest.db", "MDA must not read nest's sealed DB"),
        (MTA_UID, "/data/inbound-deliver-key", "MTA must not read the inbound-deliver-key"),
        (MDA_UID, "/data/inbound-deliver-key", "MDA must not read the inbound-deliver-key"),
    ]
    for uid, path, why in denied:
        rc, out, err = _exec_as(name, uid, "cat", path)
        assert rc != 0, f"{why}: cat as uid {uid} unexpectedly succeeded (rc=0)"
        assert not out, f"{why}: got bytes back as uid {uid}"

    # The blobs + acme dirs are 0700 nest-owned (sealed blobs / TLS private keys):
    # a bridge UID cannot even list them.
    for uid in (MTA_UID, MDA_UID):
        for d in ("/data/blobs", "/data/acme"):
            rc, _, _ = _exec_as(name, uid, "ls", d)
            assert rc != 0, f"uid {uid} must not be able to list {d}"


def test_each_bridge_reads_its_own_key_and_operator_hatch(serving_nest):
    """(C) Each bridge UID CAN read its own keyfile + the shared operator-hatch."""
    name = serving_nest["name"]
    for uid, role in ((MTA_UID, "mta"), (MDA_UID, "mda")):
        rc, out, err = _exec_as(name, uid, "cat", f"/data/keys/{role}/{role}.key")
        assert rc == 0 and out, f"{role} (uid {uid}) must read its own key; rc={rc} err={err!r}"
        rc, out, err = _exec_as(name, uid, "cat", "/data/operator-hatch.toml")
        assert rc == 0, f"{role} (uid {uid}) must read operator-hatch.toml; rc={rc} err={err!r}"


@pytest.mark.feature("nest-hardening")
def test_key_files_and_dirs_have_expected_owner_and_mode(serving_nest):
    """(D) Per-role key dirs/files owned by the bridge UID with restrictive modes;
    /data/keys is root-owned traverse-only; operator-hatch is world-readable."""
    name = serving_nest["name"]

    assert _stat(name, "/data/keys") == (0, 0, "711"), \
        f"/data/keys should be root:root 0711; got {_stat(name, '/data/keys')}"
    # Assert owner-UID + mode only; the GID is `useradd`-auto-assigned and
    # irrelevant at 0700/0600 (group has no access either way).
    for uid, role in ((MTA_UID, "mta"), (MDA_UID, "mda")):
        d = _stat(name, f"/data/keys/{role}")
        assert d is not None and (d[0], d[2]) == (uid, "700"), \
            f"/data/keys/{role} should be owner {uid} mode 0700; got {d}"
        f = _stat(name, f"/data/keys/{role}/{role}.key")
        assert f is not None and (f[0], f[2]) == (uid, "600"), \
            f"{role}.key should be owner {uid} mode 0600; got {f}"

    oh = _stat(name, "/data/operator-hatch.toml")
    assert oh is not None and oh[2] == "644", f"operator-hatch.toml should be 0644; got {oh}"
    # The sealed DB stays nest-owned and unreadable by others.
    db = _stat(name, "/data/nest.db")
    assert db is not None and db[0] == 1000 and db[2] == "600", \
        f"nest.db should be uid 1000 mode 0600; got {db}"


# The s6 run-scripts wire each role to its profile: MTA → `bridge`,
# MDA → `bridge-imap` (docker/s6/fauna-mail-bridge-{mta,mda}/run).
SANDBOX_PROFILE = {"mta": "bridge", "mda": "bridge-imap"}


def _sandboxed_as(name: str, uid: int, profile: str, *cmd: str):
    """Run a command INSIDE a fresh fauna-sandbox Landlock+seccomp domain — the
    same wrapper binary + profile the real bridge service execs under — as
    `uid`. This is what makes the probe a KERNEL-LSM probe: `docker exec -u
    <uid>` alone runs outside any Landlock domain (Landlock confines a process
    tree, not a UID), so its denials only ever prove DAC."""
    return _exec_as(name, uid, "fauna-sandbox", profile, "--", *cmd)


def _assert_landlock_enforced(err: str, ctx: str) -> None:
    """The wrapper must report an ENFORCED ruleset. `landlock::apply` warns and
    continues UNSANDBOXED on `RulesetStatus::NotEnforced` (kernel without
    Landlock / runtime seccomp blocking the landlock syscalls) — the silent-off
    state this rules out. Current kernels report "partially enforced" here
    empirically (an ABI-negotiation artifact; the denial probes prove the paths
    that matter are enforced), so accept fully OR partially — never the
    NotEnforced fallback, and never a missing status line."""
    assert "landlock: not enforced" not in err, \
        f"{ctx}: Landlock is NOT ENFORCED (silent warn-and-continue fallback " \
        f"— the fs default-deny is OFF); stderr={err!r}"
    assert ("landlock: fully enforced" in err
            or "landlock: partially enforced" in err), \
        f"{ctx}: no Landlock enforcement status logged — did the wrapper " \
        f"apply its ruleset at all?; stderr={err!r}"


@pytest.mark.feature("nest-hardening")
def test_landlock_denies_sealed_store_inside_sandbox_domain(serving_nest):
    """(G) The kernel-LSM half of slice 4, probed in-domain. Probes run as ROOT:
    in-container root holds CAP_DAC_OVERRIDE, so file permissions cannot be the
    denier — a denial is attributable to the Landlock ruleset alone. (The (B)
    denials as a bridge UID prove the DAC layer; this proves the kernel layer
    that survives a privilege escalation past the DAC UID — the whole point of
    slice 4.)

    Negative-control discipline (the falsely-green trap): each denied path is
    FIRST read unconfined as root and must SUCCEED — so on a lenient box
    (Landlock unsupported, or the profile regressed to grant /data) the
    sandboxed reads succeed and the denial asserts go RED instead of silently
    green. The allowed-path read under the same sandbox proves the wrapper
    still works at all (denials are the ruleset's, not a crashed probe), and
    the enforcement-status assert rules out the warn-and-continue NotEnforced
    fallback (`bins/fauna-sandbox/src/landlock.rs`; see
    `_assert_landlock_enforced`)."""
    name = serving_nest["name"]

    # Sensitivity controls (unconfined root): every probed path exists and is
    # accessible when no Landlock domain applies.
    rc, out, err = _exec_as(name, 0, "head", "-c", "16", "/data/nest.db")
    assert rc == 0 and out.startswith(b"SQLite format 3"), \
        f"control: unconfined root must read nest.db; rc={rc} out={out!r} err={err!r}"
    for d in ("/data/blobs", "/data/acme"):
        rc, _, err = _exec_as(name, 0, "ls", d)
        assert rc == 0, f"control: unconfined root must list {d}; rc={rc} err={err!r}"

    for profile in ("bridge", "bridge-imap"):
        # Selectivity control: the domain still reads a granted path (the RO
        # operator-hatch both profiles allow), and the wrapper reports full
        # enforcement — not the NotEnforced warn-and-continue fallback.
        rc, out, err = _sandboxed_as(name, 0, profile, "cat", "/data/operator-hatch.toml")
        assert rc == 0 and out, \
            f"[{profile}] control: sandboxed read of the GRANTED operator-hatch " \
            f"must succeed; rc={rc} err={err!r}"
        _assert_landlock_enforced(err, f"[{profile}] allowed-path control")

        # The sealed store is denied INSIDE the domain — even for root.
        for cmd, path in (
            ("cat", "/data/nest.db"),
            ("ls", "/data/blobs"),
            ("ls", "/data/acme"),
        ):
            rc, out, err = _sandboxed_as(name, 0, profile, cmd, path)
            assert rc != 0 and not out, \
                f"[{profile}] {path} must be Landlock-denied inside the domain " \
                f"even for root; rc={rc} out={out[:64]!r}"
            assert "Permission denied" in err, \
                f"[{profile}] {path} denial must be EACCES (the Landlock deny), " \
                f"not some other failure; stderr={err!r}"
            _assert_landlock_enforced(err, f"[{profile}] denied {path}")


@pytest.mark.feature("nest-hardening")
def test_landlock_denies_node_db_in_real_bridge_context(serving_nest):
    """(G) The same probe from each bridge's FULL production context — its own
    UID under its own profile, exactly as the s6 run-script execs it
    (`s6-setuidgid fauna-<role> fauna-sandbox <profile> -- …`). Here DAC and
    Landlock both stand between the parser and the sealed store; the read must
    still fail, inside an enforced domain."""
    name = serving_nest["name"]
    for role, uid in (("mta", MTA_UID), ("mda", MDA_UID)):
        rc, out, err = _sandboxed_as(
            name, uid, SANDBOX_PROFILE[role], "cat", "/data/nest.db"
        )
        assert rc != 0 and not out, \
            f"{role} (uid {uid}) must not read nest.db in its sandbox domain; rc={rc}"
        _assert_landlock_enforced(err, f"{role} (uid {uid}) nest.db probe")


@pytest.mark.feature("nest-hardening")
def test_bridges_self_report_their_confinement_over_the_wire(serving_nest):
    """(H) The no-SSH observable: every fact tests (A)–(G) establish by running
    `docker exec` against the container must ALSO be readable over admin WS-RPC,
    because that is the only channel a *deployed* box offers — a provisioned box
    carries no ssh key (`testing.md` § Gap 3), and the 2026-07-22 verification of
    these same facts on example.com needed a human, an SSH key, and a
    production-shell approval.

    This is deliberately the *weak* proof of the same properties: it asks the
    bridge what it observed rather than observing from outside, so a compromised
    bridge could lie (`security.md` § Confinement self-probe — provisioning
    diagnostics, never an attestation). Its value is coverage on boxes where
    (A)–(G) cannot run at all. So it is pinned here, beside the strong probes, and
    **cross-checked against them**: the UID each bridge reports must equal the UID
    test (A) reads from `/proc`, which is what would catch the self-report going
    stale, hard-coded, or wired to the wrong process.
    """
    name = serving_nest["name"]
    with _admin_ws(serving_nest) as admin_conn:
        reply = admin_conn.call("fauna.bridges.list_service_users", {})
    by_role = {u["role"]: u for u in reply.get("service_users", [])}
    for role in ROLES:
        assert role in by_role, \
            f"{role} bridge missing from list_service_users: {list(by_role)}"

    for role, want_uid in (("mta", MTA_UID), ("mda", MDA_UID)):
        conf = by_role[role].get("confinement")
        assert conf is not None, (
            f"{role} reported no confinement diagnostic. Either the bridge image "
            f"predates the self-probe, or register_service_user dropped the field "
            f"— the deployed box then has NO no-SSH way to prove slices 1+4."
        )
        # Cross-check against the ground truth (A) read from /proc: the
        # self-report must describe THIS process, not a constant.
        pid = _service_pid(name, f"fauna-mail-bridge-{role}")
        assert pid is not None
        assert conf["uid"] == want_uid == _proc_ruid(name, pid), (
            f"{role} self-reported uid {conf['uid']}, but /proc says "
            f"{_proc_ruid(name, pid)} and the image assigns {want_uid}"
        )
        # The property that bounds the blast radius, as the bridge itself
        # measured it from inside its sandbox — the wire twin of test (G).
        assert conf["sealed_store"] == "denied", (
            f"{role} reports it can reach the sealed store "
            f"(sealed_store={conf['sealed_store']!r}) — slice 4 is not holding "
            f"on this image"
        )
        # Three-state on purpose: current kernels report *partial* with the
        # denials provably working (see _assert_landlock_enforced), so only
        # `off`/`unknown` is a failure here.
        assert conf["landlock"] in ("fully", "partial"), (
            f"{role} reports landlock={conf['landlock']!r}. `off` = the "
            f"warn-and-continue fallback ran; `unknown` = the bridge was not "
            f"started through fauna-sandbox at all (a compose override?)"
        )
        assert conf["seccomp"] == "filter", \
            f"{role} reports seccomp={conf['seccomp']!r}, want the installed filter"
        assert by_role[role].get("confinement_reported_at", 0) > 0, \
            f"{role} report carries no nest-stamped timestamp — it could be stale"


@pytest.mark.feature("nest-hardening")
def test_all_services_still_serve_after_split(serving_nest):
    """(E) The split does not break serving: nest healthy, both bridge services +
    the SNI router supervised-up, and the MTA serves its plaintext 220 banners."""
    name = serving_nest["name"]
    port = serving_nest["port"]
    mp = serving_nest["mail_ports"]

    wait_for_health(port, name)
    for service in ("fauna-nest", "fauna-sni-router",
                    "fauna-mail-bridge-mta", "fauna-mail-bridge-mda"):
        assert _is_commanded_up(_svstat(name, service)), \
            f"{service} must be up after the UID split; got {_svstat(name, service)!r}"
    try:
        wait_for_smtp_banner("127.0.0.1", mp[25], timeout=60)
        wait_for_smtp_banner("127.0.0.1", mp[587], timeout=60)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{_bridge_diag(name)}") from e
