"""tier_4 e2e: the **enrollment proof-of-possession** gate that stops a
co-resident bridge UID from self-enrolling a rogue/cross-role identity over
loopback (security.md § Enrollment proof-of-possession contract, plus the
x25519 fold-in).

After the UID split (slice 1), the mail bridges are loopback peers of nest, so
loopback origin alone no longer proves an enrolling process is the legitimate
artifact-provisioned bridge — a compromised `fauna-mta`/`fauna-mda` could
self-enroll a *fresh* keypair and be auto-approved. Slice 2 closes that: the
deployment artifact MINTS each role's keypair and publishes its blessed Ed25519
pubkey to a root-owned, bridge-UID-unwritable registry; nest then requires an
enroller to BE that blessed key AND sign the enrollment, proving possession of
the UID-isolated private half a co-resident attacker cannot read.

The *verification logic* is proven by tier_3 nest unit tests
(`bridge_blob_handlers::check_enrollment_authorization` / the
`fauna_protocol::wrapped_blob::enrollment_signed_message` round-trips) and the Go
signer by `just mail-bridge-test`. This tier_4 test covers the
**deployment/packaging** half those bypass — the artifact mint, the registry's
ownership/mode/integrity, the nest run-script env wiring, and the end-to-end
accept/reject against the real image:

  A. The blessed registry `/data/keys/blessed/<role>.pub` is provisioned
     root-owned 0644 hex — and a bridge UID can READ it (it is public) but
     CANNOT substitute it (integrity is the property).
  B. The published blessed pubkey equals the bridge's actual keyfile pubkey
     (the artifact blessed the key the load-only bridge really loads).
  C. Positive: both artifact-minted, blessed, signing bridges enroll +
     auto-approve on the real image (ask 3a).
  D. Negative: a forged enrollment presenting a FRESH (non-blessed) pubkey over
     loopback — the compromised-co-resident exploit — is REJECTED (ask 3b).
     This *also* proves nest read the role's registry file under
     FAUNA_BLESSED_KEYS_DIR and is enforcing strict mode: a nest that hadn't
     read it would be lenient and auto-approve the forged key. (We can't
     assert the env var directly —
     nest runs privilege-dropped via s6-setuidgid, so its /proc/<pid>/environ
     is non-dumpable and unreadable even by root-without-CAP_SYS_PTRACE; the
     behavioural proof is both available and stronger.)
  E. Negative CONTROL (the falsely-green trap): on a box whose blessed registry
     is ABSENT, the SAME forged enrollment IS accepted — the documented
     deploy-safe lenient fallback (`security.md` § Implementation status,
     slice 2). This proves D's rejection is genuinely caused by the registry
     (remove the registry → D's asserts invert), i.e. that D would go RED on a
     lenient box rather than green for some unrelated reason. It also pins the
     two lenient-box observables: nest's provisioning-gap warning ("any
     loopback peer can enroll") in the logs, and `enrollment_strict` reporting
     `false` on the admin `list_service_users` reply — the no-SSH diagnostic a
     live e2e / admin uses on a deployed box (`testing.md` § Gap 3).

Only tier_4 catches A–B + D's packaging half: the mint + registry + env wiring
live entirely in the entrypoint + s6 run-scripts, which the binary-spawning
tier_3 fixtures bypass.
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
    start_container_with_ports,
    svstat as _svstat,
    wait_for_health,
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
CLAIM_CODE = "POPENROLL"
NEST_PORT = 3000
MTA_UID = 1001
MDA_UID = 1002
ROLE_UID = {"mta": MTA_UID, "mda": MDA_UID}
BLESSED = {r: f"/data/keys/blessed/{r}.pub" for r in ("mta", "mda")}
KEYFILE = {r: f"/data/keys/{r}/{r}.key" for r in ("mta", "mda")}
HEX64 = re.compile(r"\A[0-9a-f]{64}\Z")


# ── docker-exec helpers (mirror test_uid_isolation / test_proxy_router_auth;
# candidates for a lift into helpers.py once the tier_4 docker suite consolidates) ─


def _exec_as(name: str, uid: int, *cmd: str) -> tuple[int, bytes, str]:
    """Run a command in the container as a numeric UID; stdout BYTES."""
    r = subprocess.run(
        ["docker", "exec", "-u", str(uid), name, *cmd],
        capture_output=True, timeout=40,
    )
    return r.returncode, r.stdout, r.stderr.decode("utf-8", "replace")


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


def _read_as_root(name: str, path: str) -> str:
    rc, out, err = _exec_as(name, 0, "cat", path)
    assert rc == 0, f"root must read {path}: {err}"
    return out.decode("ascii", "strict").strip()


# ── Fixture: a fresh image brought all the way to mail-serving ─────────


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


def _bring_up_mail_nest(name_prefix: str) -> dict:
    """Start a fresh container and bring it all the way to mail-enabled with
    both artifact-minted bridges auto-approved. Shared by the strict fixture
    and the lenient-box negative control (one bring-up path, so the ONLY
    difference between the two boxes is the registry mutation)."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"{name_prefix}-{http_port}"
    start_container_with_ports(
        name,
        {NEST_PORT: http_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": str(NEST_PORT)},
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
            "approved": [],
        }
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
            nest["approved"] = sorted(approved)
        return nest
    except BaseException:
        remove_container(name)
        raise


@pytest.fixture(scope="module")
def serving_nest(docker_image):
    """One container, mail enabled + both artifact-minted bridges auto-approved.
    That the blessed+signing bridges reach `approved` IS the positive (ask 3a):
    if the slice-2 gate broke legitimate enrollment, this bring-up would fail."""
    nest = _bring_up_mail_nest("fauna-pop-enroll")
    try:
        yield nest
    finally:
        remove_container(nest["name"])


@pytest.fixture()
def lenient_nest(docker_image):
    """(E) A container whose blessed registry is REMOVED post-boot — the
    documented deploy-safe lenient fallback. nest re-reads
    `<FAUNA_BLESSED_KEYS_DIR>/<role>.pub` LIVE on every enrollment
    (`bridge_blob_handlers::blessed_pubkey_from_dir`), so no restart is needed
    for the removal to take effect. Function-scoped and never shared: the
    forged enrollment below POLLUTES nest state with an approved rogue row."""
    nest = _bring_up_mail_nest("fauna-pop-lenient")
    try:
        for role in ("mta", "mda"):
            rc, _, err = _exec_as(nest["name"], 0, "rm", BLESSED[role])
            assert rc == 0, f"root must be able to remove {BLESSED[role]}: {err}"
        yield nest
    finally:
        remove_container(nest["name"])


# ── Tests ──────────────────────────────────────────────────────────────


@pytest.mark.feature("nest-hardening")
def test_both_blessed_and_signing_bridges_auto_approve(serving_nest):
    """(D) Positive: the artifact-minted, blessed, signing bridges enroll +
    auto-approve on the real image — slice-2's gate did not break the legit path."""
    for r in ROLES:
        assert r in serving_nest["approved"], \
            f"{r} bridge must enroll + auto-approve under the PoP gate; got {serving_nest['approved']}"


@pytest.mark.parametrize("role", ["mta", "mda"])
def test_blessed_registry_provisioned_root_owned_hex(serving_nest, role):
    """(A) /data/keys/blessed/<role>.pub is root-owned 0644, 64-hex (a 32-byte
    Ed25519 pubkey)."""
    name = serving_nest["name"]
    st = _stat(name, BLESSED[role])
    assert st is not None, f"{BLESSED[role]} must be provisioned at entrypoint"
    uid, _gid, mode = st
    assert (uid, mode) == (0, "644"), f"{BLESSED[role]} should be root:* 0644; got {st}"
    value = _read_as_root(name, BLESSED[role])
    assert HEX64.match(value), f"{BLESSED[role]} should be 64 hex chars; got {value!r}"


@pytest.mark.feature("nest-hardening")
@pytest.mark.parametrize("role", ["mta", "mda"])
def test_bridge_uid_can_read_but_not_substitute_blessed_pubkey(serving_nest, role):
    """(A) Integrity, not confidentiality: a bridge UID may READ its blessed
    pubkey (it is public), but must NOT be able to SUBSTITUTE it — else a
    compromised bridge could bless its own forged key. Root-owned file + a
    non-bridge-writable registry dir enforce this."""
    name = serving_nest["name"]
    uid = ROLE_UID[role]
    before = _read_as_root(name, BLESSED[role])

    # Read is allowed (public pubkey).
    rc, out, _ = _exec_as(name, uid, "cat", BLESSED[role])
    assert rc == 0 and out.decode("ascii", "strict").strip() == before, \
        f"uid {uid} should be able to read its public blessed pubkey"

    # Overwrite-in-place is denied (file is root:root 0644).
    rc, _, _ = _exec_as(name, uid, "sh", "-c", f"printf deadbeef > {BLESSED[role]}")
    assert rc != 0, f"uid {uid} must NOT overwrite {BLESSED[role]}"
    # Replace-via-unlink is denied (registry dir is root-owned, not bridge-writable).
    rc, _, _ = _exec_as(name, uid, "rm", "-f", BLESSED[role])
    assert rc != 0, f"uid {uid} must NOT remove {BLESSED[role]}"

    assert _read_as_root(name, BLESSED[role]) == before, \
        "the blessed pubkey must be unchanged after a bridge-UID substitution attempt"


@pytest.mark.parametrize("role", ["mta", "mda"])
def test_blessed_pubkey_matches_bridge_keyfile(serving_nest, role):
    """(B) The published blessed pubkey equals the bridge's own keyfile pubkey —
    so nest blesses exactly the key the load-only bridge loads. Derived by running
    --print-pubkey as the bridge UID against its own 0600 keyfile (a pure load —
    the keyfile already exists, so no re-mint)."""
    name = serving_nest["name"]
    rc, out, err = _exec_as(
        name, ROLE_UID[role],
        "fauna-mail-bridge", "--keypair-file", KEYFILE[role], "--print-pubkey",
    )
    assert rc == 0, f"--print-pubkey on {KEYFILE[role]} as its own UID must succeed: {err}"
    keyfile_pub = out.decode("ascii", "strict").strip()
    assert HEX64.match(keyfile_pub), f"--print-pubkey output should be 64 hex; got {keyfile_pub!r}"
    assert keyfile_pub == _read_as_root(name, BLESSED[role]), \
        f"blessed {role}.pub must equal the bridge keyfile's actual Ed25519 pubkey"


@pytest.mark.feature("nest-hardening")
def test_forged_fresh_pubkey_enrollment_is_rejected(serving_nest):
    """(D) Negative — the compromised-co-resident exploit: a process running as a
    bridge UID self-enrolls a FRESH (non-blessed) keypair over loopback. nest's
    PoP gate rejects it (presented pubkey != the artifact-blessed key), so the
    bridge's enroll poll fails fatally (ok=false) instead of being auto-approved.

    This doubles as the proof that nest READ the role's blessed key from the
    FAUNA_BLESSED_KEYS_DIR registry and is enforcing strict mode: in this fixture mail is enabled, so a
    nest that had NOT read the blessed key would be lenient and auto-approve even
    this fresh forged pubkey (status "approved", the bridge proceeds) rather than
    reject it. The rejection is only reachable when the blessed registry is wired
    in. (Reading the env var directly is impossible — nest is non-dumpable under
    s6-setuidgid — so this behavioural assertion is the verification.)

    Run the real binary as fauna-mta with a fresh keyfile named `mta.key` (so
    role_hint resolves to "mta" and nest looks up the MTA's blessed key). It dials
    the in-container loopback, request_enrollment is rejected, and main exits 1
    with the wrapped server error on stderr."""
    name = serving_nest["name"]
    rc, out, err = _exec_as(
        name, MTA_UID, "sh", "-c",
        "rm -rf /tmp/forged && mkdir -p /tmp/forged && "
        "exec fauna-mail-bridge "
        "--keypair-file /tmp/forged/mta.key "
        f"--nest-endpoint https://127.0.0.1:{NEST_PORT} --log-level error",
    )
    combined = out.decode("utf-8", "replace") + err
    assert rc != 0, f"a forged (non-blessed) enrollment must fail; rc={rc}, output={combined!r}"
    # The rejection arrives as an ok=false reply to request_enrollment, surfaced
    # through the enroll poll → "await bridge approval". (ServerError.Error()
    # carries "server returned ok=false", not the wire `permission_denied` code.)
    assert "server returned ok=false" in combined and "await bridge approval" in combined, \
        f"forged enrollment must be rejected by request_enrollment; got {combined!r}"
    # The forged fresh pubkey must NOT have been enrolled (no row created — the
    # PoP check runs before any row insert): it never reaches an approved state.
    assert "enrollment approved" not in combined, \
        f"a forged pubkey must never be approved; got {combined!r}"


def _mint_forged_keypair(name: str) -> str:
    """Mint a fresh (non-blessed) keypair as the MTA UID in /tmp/forged and
    return its hex pubkey — the co-resident attacker's forged identity."""
    rc, out, err = _exec_as(
        name, MTA_UID, "sh", "-c",
        "rm -rf /tmp/forged && mkdir -p /tmp/forged && "
        "fauna-mail-bridge --keypair-file /tmp/forged/mta.key --print-pubkey",
    )
    assert rc == 0, f"forged keypair mint must succeed: {err}"
    forged_pub = out.decode("ascii", "strict").strip()
    assert HEX64.match(forged_pub), f"--print-pubkey should be 64 hex; got {forged_pub!r}"
    return forged_pub


@pytest.mark.feature("nest-hardening")
def test_lenient_box_forged_enrollment_is_accepted(lenient_nest):
    """(E) The negative CONTROL for test_forged_fresh_pubkey_enrollment_is_rejected:
    with the blessed registry removed, the IDENTICAL forged fresh-pubkey
    enrollment is now auto-approved — the documented deploy-safe lenient
    fallback. This is the permanent proof that the strict test's rejection is
    caused by the registry (and would go RED, not green, on a box the artifact
    failed to provision), plus the two lenient-box observables: nest's
    provisioning-gap warning, and `enrollment_strict: false` on the admin
    diagnostic."""
    name = lenient_nest["name"]
    forged_pub = _mint_forged_keypair(name)

    # The same enrollment the strict test proves rejected. On approval the
    # rogue bridge KEEPS RUNNING (it proceeds toward serving), so bound it
    # in-container; the authoritative accept-check is the admin roster below,
    # not the process's exit.
    rc, out, err = _exec_as(
        name, MTA_UID, "sh", "-c",
        "timeout 20 fauna-mail-bridge "
        "--keypair-file /tmp/forged/mta.key "
        f"--nest-endpoint https://127.0.0.1:{NEST_PORT} --log-level info",
    )
    combined = out.decode("utf-8", "replace") + err
    assert "server returned ok=false" not in combined, \
        f"lenient box must NOT reject the enrollment (registry is absent); got {combined!r}"

    with _admin_ws(lenient_nest) as admin_conn:
        reply = admin_conn.call(
            "fauna.bridges.list_service_users", {"status": "approved"}
        )
        pubs = {bytes(r["ed25519_pubkey"]).hex() for r in reply["service_users"]}
        assert forged_pub in pubs, (
            "the forged fresh pubkey must be AUTO-APPROVED on the registry-less "
            f"box (lenient fallback); approved roster pubkeys: {sorted(pubs)}"
        )

    # nest names the provisioning gap loudly (the box is router-fronted —
    # FAUNA_FRONTED_BY_ROUTER=1 in the image — with no blessed registry).
    logs = subprocess.run(
        ["docker", "logs", "--tail", "400", name],
        capture_output=True, text=True, timeout=15,
    )
    assert "any loopback peer can enroll" in (logs.stdout + logs.stderr), \
        "nest must log the blessed-registry provisioning-gap warning on a lenient enroll"

    # The no-SSH diagnostic must report the lenient posture explicitly. (Kept
    # LAST: against an image predating the field this is the only assert that
    # fails, so the behavioral negative control above still stands proven.)
    with _admin_ws(lenient_nest) as admin_conn:
        reply = admin_conn.call("fauna.bridges.list_service_users", {})
        strict = reply.get("enrollment_strict")
        assert strict is not None, (
            "admin list_service_users reply carries no enrollment_strict — this "
            "nest image predates the diagnostic (needs a build ≥ 2026-07-22)"
        )
        assert strict["mta"] is False and strict["mda"] is False, (
            f"registry removed → both roles must self-report lenient; got {strict}"
        )


@pytest.mark.feature("nest-hardening")
def test_enrollment_strict_reported_on_provisioned_box(serving_nest):
    """(E) The strict-side half of the diagnostic: on the artifact-provisioned
    box the admin reply reports strict enrollment for BOTH mail roles — the
    exact assert the live-Hetzner e2e / a example.com admin runs over the wire,
    since the no-SSH invariant (`testing.md` § Gap 3) leaves no shell to check
    the registry with on a deployed box."""
    with _admin_ws(serving_nest) as admin_conn:
        reply = admin_conn.call("fauna.bridges.list_service_users", {})
        strict = reply.get("enrollment_strict")
        assert strict is not None, (
            "admin list_service_users reply carries no enrollment_strict — this "
            "nest image predates the diagnostic (needs a build ≥ 2026-07-22)"
        )
        assert strict["mta"] is True and strict["mda"] is True, (
            "an artifact-provisioned box must self-report STRICT enrollment for "
            f"both roles (blessed registry present); got {strict} — a false here "
            "on a deployed box is the silent-lenient provisioning gap"
        )
