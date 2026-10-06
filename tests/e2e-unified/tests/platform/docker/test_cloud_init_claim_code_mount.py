"""tier_4: a cloud-init box with the claim code provided as a READ-ONLY MOUNT
(not the ``FAUNA_CLAIM_CODE`` env) boots and is claimable end-to-end.

This closes a real coverage gap. Every other tier_4 test injects the claim code
via the ``FAUNA_CLAIM_CODE`` env path (``entrypoint.sh`` writes it into the
*writable* ``/data``), so the cloud-init **mount** path — the one real boxes use
(``libs/fauna-provisioning/src/cloud_init.rs``) — had **zero** coverage. That gap
hid a CLAIM-BLOCKING regression from the per-role-UID split: the
cloud-init mounted the seed directly at ``/data/claim-code:ro``, so the
entrypoint's first-run ``chown -R /data`` and the sensitive-file ``chmod 0600``
EROFS'd on the read-only bind under ``set -euo pipefail`` → ``fauna-nest exited
with code 1 (restarting)`` — a boot **crash loop**, never reaching a running
nest. It was live-confirmed 2026-06-30 (a user hand-running the cloud-init box).

The fix stages the seed OUTSIDE ``/data`` at ``/run/fauna/claim-code-seed`` and
has the entrypoint copy it (as root) into the writable volume on first boot, so
the uid-1000 nest can both **read** the code (at claim) and **delete** it
(single-use), and ``chown -R /data`` never traverses a read-only bind.

A complementary follow-up covers the leg the
staging-copy + DB-positive gate don't: a ``/data/claim-code`` that is **present
but UNREADABLE** by the uid-1000 nest (the direct ``:ro`` ``root:root 0600``
mount a hand-rolled compose still produces). On an *unclaimed* box
(``admin_count()==0``) the claim flow reaches ``read_to_string`` and the old code
mapped **every** read error → a false ``already_claimed``. The fix
(``claim_core.rs``) distinguishes ``NotFound`` (genuine single-use replay →
``already_claimed``) from ``PermissionDenied``/IO (present-but-unreadable → the
truthful terminal ``fauna.auth.claim_code_unreadable``), and the entrypoint's
**unconditional per-boot** ``chown fauna:fauna /data/claim-code``
(``entrypoint.sh`` ~:382, guarded ``2>/dev/null || true``) self-heals a
root-owned **real** file on the next restart. The two legs below
(``test_unclaimed_box_with_unreadable_claim_code_reports_claim_code_unreadable``
and ``test_unreadable_claim_code_self_heals_on_restart``) prove both on the real
image.

Faithfulness note: cloud-init's seed is ``root:root 0600``; this test can't
``chown`` to root without privilege, so the host seed is created ``0600`` owned
by the test user. That does **not** weaken the regression guard: the boot crash
is caused purely by the read-only ``:ro`` bind (independent of file ownership),
and the entrypoint stage-copy runs **as root**, which reads the seed regardless
of its mode — exactly the root:root case a real box hits.

Authority: ``docs/goal/architecture/nest/common.md`` § Client-state
recoverability (a wedged claimable-but-claimed box is an off-box-only-fixable
brick the invariant outlaws); ``docs/goal/behavior/onboarding.md`` § 3a.
"""

import subprocess
import tempfile
from pathlib import Path

import pytest

from .helpers import (
    fence_cpuset_args,
    IMAGE_TAG,
    claim_admin_api,
    docker_build,
    find_free_port,
    generate_claim_code,
    get_repo_root,
    remove_container,
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

SEED_MOUNT = "/run/fauna/claim-code-seed"


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (reuse-aware via FAUNA_REUSE_IMAGE)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def mounted_claim_nest(docker_image, tmp_path):
    """A nest whose claim code is provided ONLY as a read-only mount at the
    staging path (no ``FAUNA_CLAIM_CODE`` env). Yields ``(port, name, code)``.

    The box reaching health *is* the boot-crash-loop regression proof.
    """
    code = generate_claim_code()
    # Mirror cloud-init's `write_files: permissions: '0600'` host seed.
    seed = tmp_path / "claim-code"
    seed.write_text(code)
    seed.chmod(0o600)

    port = find_free_port()
    name = f"fauna-nest-claimmount-{port}"
    remove_container(name)
    cmd = [
        "docker", "run", "-d", *fence_cpuset_args(),
        "--name", name,
        "-p", f"127.0.0.1:{port}:3000",
        "-e", "FAUNA_MODE=public",
        "-e", "FAUNA_PORT=3000",
        # The cloud-init claim-code mount: read-only, staged OFF /data.
        "-v", f"{seed}:{SEED_MOUNT}:ro",
        IMAGE_TAG,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f"docker run failed:\n{result.stderr}")
    try:
        # If the entrypoint EROFS-crash-looped (the bug), the nest never serves
        # and this raises TimeoutError with the boot/migration log lines.
        wait_for_health(port, name)
        yield (port, name, code)
    finally:
        remove_container(name)


def _claim_code_file_absent(name: str) -> bool:
    """True iff /data/claim-code does not exist inside the container."""
    r = subprocess.run(
        ["docker", "exec", "-u", "0", name, "test", "-e", "/data/claim-code"],
        capture_output=True, timeout=15,
    )
    return r.returncode != 0  # `test -e` exits non-zero when absent


def _setup_status(port: int) -> dict:
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(f"https://127.0.0.1:{port}") as anon:
        return anon.call("fauna.setup.status", {})


def _nest_can_read_claim_code(name: str) -> bool:
    """True iff uid 1000 (the ``fauna`` nest) can read /data/claim-code inside
    the container — the exact read the claim handler does at claim time."""
    r = subprocess.run(
        ["docker", "exec", "-u", "1000", name, "cat", "/data/claim-code"],
        capture_output=True, timeout=15,
    )
    return r.returncode == 0


def _make_root_owned_unreadable(name: str):
    """Force /data/claim-code to ``root:root 0600`` — present but unreadable by
    the uid-1000 nest, exactly the state a direct ``:ro`` root:root claim-code
    bind mount leaves it in (a hand-rolled compose using the retired mount form)."""
    r = subprocess.run(
        ["docker", "exec", "-u", "0", name, "sh", "-c",
         "chown 0:0 /data/claim-code && chmod 0600 /data/claim-code"],
        capture_output=True, text=True, timeout=15,
    )
    assert r.returncode == 0, r.stderr


@pytest.mark.feature("claim-a-fresh-nest")
def test_mounted_claim_code_box_boots_and_is_claimable(mounted_claim_nest):
    """The box boots from a read-only claim-code MOUNT, claims with the staged
    code, deletes it on claim, and reports claimed — the full cloud-init path."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    port, name, code = mounted_claim_nest

    # 1. Boots (no crash loop) AND is unclaimed with the staged code present.
    pre = _setup_status(port)
    assert pre["admin_exists"] is False, pre
    assert pre["claimed"] is False, (
        f"a staged-but-unclaimed box must read claimed=False: {pre}")
    assert not _claim_code_file_absent(name), (
        "the staged claim code must be present before claim")

    # 2. Claim with the SAME code carried by the mount — proves it was copied
    #    into the writable volume readable by the uid-1000 nest.
    admin = claim_admin_api(port, code)
    with WsRpcAdminClient(
        f"https://127.0.0.1:{port}",
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        assert client.call("fauna.account.am_i_admin", {})["admin"] is True

    # 3. Single-use: the staged file is a real volume file, so the post-claim
    #    unlink succeeds — and claimed-state flips to True.
    assert _claim_code_file_absent(name), (
        "the claim code must be deleted after a successful claim (deletable "
        "staged copy, not an un-unlinkable read-only mountpoint)")
    post = _setup_status(port)
    assert post["admin_exists"] is True, post
    assert post["claimed"] is True, post


@pytest.mark.feature("claim-a-fresh-nest")
def test_lingering_claim_code_cannot_create_second_admin(mounted_claim_nest):
    """Even if the single-use code file LINGERS (a post-claim delete that failed
    on a read-only mount / transient FS error), a DIFFERENT actor must not become
    a second admin — the DB-positive ``already_claimed`` gate rejects it. Closes
    the latent second-admin hole on the real image (step 2)."""
    port, name, code = mounted_claim_nest

    # Claim the box; the staged code is deleted on success.
    claim_admin_api(port, code)
    assert _claim_code_file_absent(name)

    # Simulate the post-claim delete having FAILED: put the same valid code back,
    # fauna-readable, as if an un-unlinkable read-only mountpoint had survived.
    restore = subprocess.run(
        ["docker", "exec", "-u", "0", name, "sh", "-c",
         f"printf '%s' '{code}' > /data/claim-code && chown fauna:fauna /data/claim-code"],
        capture_output=True, text=True, timeout=15,
    )
    assert restore.returncode == 0, restore.stderr
    assert not _claim_code_file_absent(name), "the lingering code must be present"

    # A second, different actor presents the same valid code → must be rejected,
    # and the box must still report claimed (DB-positive, not file-absence).
    with pytest.raises(Exception) as exc:
        claim_admin_api(port, code, handle="intruder")
    assert "already_claimed" in str(exc.value), (
        f"a lingering claim code must not create a second admin: {exc.value!r}")
    assert _setup_status(port)["claimed"] is True


@pytest.fixture()
def env_claim_nest(docker_image):
    """A nest booted with the claim code via ``FAUNA_CLAIM_CODE`` env → a REAL,
    writable ``/data/claim-code`` file (``fauna:fauna 0600``, nest-readable),
    healthy + unclaimed. Yields ``(port, name, code)``.

    The unreadable-claim-code legs drive off this by mutating ownership with
    ``docker exec -u 0`` — a real ``:ro`` root:root mount produces the same
    present-but-unreadable state, but a bind mount can't be chowned back, so the
    self-heal leg needs a real file the per-boot chown can fix.
    """
    code = generate_claim_code()
    port = find_free_port()
    name = f"fauna-nest-claimunread-{port}"
    remove_container(name)
    cmd = [
        "docker", "run", "-d", *fence_cpuset_args(),
        "--name", name,
        "-p", f"127.0.0.1:{port}:3000",
        "-e", "FAUNA_MODE=public",
        "-e", "FAUNA_PORT=3000",
        "-e", f"FAUNA_CLAIM_CODE={code}",
        IMAGE_TAG,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f"docker run failed:\n{result.stderr}")
    try:
        wait_for_health(port, name)
        yield (port, name, code)
    finally:
        remove_container(name)


def test_unclaimed_box_with_unreadable_claim_code_reports_claim_code_unreadable(
        env_claim_nest):
    """An UNCLAIMED box whose ``/data/claim-code`` is present but ``root:root
    0600`` (unreadable by the uid-1000 nest) must reject a claim with the
    TRUTHFUL ``fauna.auth.claim_code_unreadable`` — NOT a false
    ``already_claimed``. This is the leg the DB-positive gate can't cover:
    ``admin_count()==0``, so step-3a passes and the flow reaches the read-fail
    (``claim_core.rs`` ~:151)."""
    port, name, code = env_claim_nest

    # Precondition: unclaimed, and the code currently readable by the nest.
    pre = _setup_status(port)
    assert pre["claimed"] is False, pre
    assert _nest_can_read_claim_code(name), "claim code must start nest-readable"

    # Misprovision: root-own it 0600 → unreadable by the running uid-1000 nest
    # (the per-boot chown won't re-run until a restart, so the read fails NOW).
    _make_root_owned_unreadable(name)
    assert not _nest_can_read_claim_code(name), (
        "a root:root 0600 claim code must be unreadable by the uid-1000 nest")

    # Claim → truthful claim_code_unreadable, and explicitly NOT already_claimed.
    with pytest.raises(Exception) as exc:
        claim_admin_api(port, code)
    err = str(exc.value)
    assert "claim_code_unreadable" in err, (
        f"an unreadable claim code must surface claim_code_unreadable: {err!r}")
    assert "already_claimed" not in err, (
        f"an unreadable code on an UNCLAIMED box must NOT misreport "
        f"already_claimed: {err!r}")
    # Non-destructive: the box is still unclaimed and recoverable.
    assert _setup_status(port)["claimed"] is False


def test_unreadable_claim_code_self_heals_on_restart(env_claim_nest):
    """A root-owned (uid-1000-unreadable) REAL ``/data/claim-code`` self-heals on
    the next boot: the entrypoint's unconditional per-boot ``chown fauna:fauna
    /data/claim-code`` (``entrypoint.sh`` ~:382) restores readability → the box
    is claimable after a restart."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    port, name, code = env_claim_nest

    # Misprovision while running; the per-boot chown won't re-run until restart.
    _make_root_owned_unreadable(name)
    assert not _nest_can_read_claim_code(name)

    # Restart → the unconditional per-boot chown self-heals ownership.
    r = subprocess.run(
        ["docker", "restart", "-t", "20", name],
        capture_output=True, text=True, timeout=60,
    )
    assert r.returncode == 0, r.stderr
    wait_for_health(port, name)
    assert _nest_can_read_claim_code(name), (
        "the per-boot chown must restore fauna ownership after restart")

    # The healed box claims successfully with the same code.
    admin = claim_admin_api(port, code)
    with WsRpcAdminClient(
        f"https://127.0.0.1:{port}",
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        assert client.call("fauna.account.am_i_admin", {})["admin"] is True
    assert _setup_status(port)["claimed"] is True
