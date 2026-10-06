"""tier_4 e2e: the mail bridge serves after a schema UPGRADE against a
pre-existing ``/data`` — the deploy gate for the outage class.

Migration-safety Layer 3 (tracked internally;
``docs/goal/architecture/nest/common.md`` § Database — *Upgrade safety*). The
 example.com outage was a column added to a ``CREATE TABLE`` block with
no paired ``ALTER`` — a no-op on the long-lived ``/data``, so the column never
appeared and ``list_active_mail_domains``'s ``SELECT`` failed *"no such column"*,
crash-looping the mail bridge (no mail ports) while nest still reported
``health: ok``. Crucially, the **fresh**-tier_4 mail tests were GREEN on the
crashing commit — they spin a brand-new ``/data`` where the ``CREATE TABLE``
already has every column, so they never exercise the upgrade path
(memory ``deploy-verify-gates-vs-bridge-up``). Only booting the real image
against an *accumulated, older-schema* ``/data`` reproduces it.

This test manufactures that situation deterministically — no checked-in binary
fixture, no old image:

  Phase 1 — boot the current image on a persistent named volume, drive it to
            fully serving (the proof a fresh box works).
  Phase 2 — rewrite ``nest.db`` in place to an OLDER schema: drop two columns
            ``list_active_mail_domains`` SELECTs, as a ``/data`` written before
            an additive step added them would lack them. ``dkim_selector_
            activated_at`` is the column; ``dkim_rotation_days``
            is its sibling. Neither has a hand-written ``ALTER`` catch-up, so
            healing them proves the column reconciler runs in the real image.
  Phase 3 — redeploy: a FRESH container boots the SAME ``/data`` (the entrypoint
            takes its "existing config" path). The nest applies the genesis +
            the column reconciler, healing the dropped ``mail_domains`` columns;
            the bridge's ``fetch_config`` now succeeds and it serves all four
            mail ports. On the image this is exactly where the bridge
            crash-looped.

The headline gate is the full serve on the redeployed box (IMAP ``* OK`` + all
four ports), corroborated nest-side by ``mail_subsystem_ok`` (the Layer-4 signal
on ``fauna.setup.status``) being ``true`` — the same ``list_active_mail_domains``
query the bridge boots from, succeeding post-reconcile.

The non-additive (guarded-step) class has no phase here: the nest schema's
genesis (``docs/goal/architecture/nest/common.md`` § Database) carries no
non-additive step yet — the table rebuild this test used to seed was history the
genesis replaced — so that phase returns with the first post-genesis
non-additive step, seeding its pre-change shape the way Phase 2 seeds this one.

This is the one tier_4 test that exercises the *upgrade* path; the fresh-boot
serving lifecycle is ``test_mail_deploy_lifecycle.py``.

The sibling test in this file — ``test_incompatible_schema_boots_degraded_then_
recovers`` — exercises the OTHER end of the version-compatibility contract
(``docs/goal/architecture/version-compatibility.md`` § 2.2): a ``/data`` written
by a *newer* nest carrying a breaking schema change this image predates. There is
no migration that can heal it, so the guarantee under test is the inverse of the
upgrade gate — the real image must boot DEGRADED (``/api/v1/health`` stays 200,
no crash-loop — the off-box-brick guarantee, ``nest/common.md`` § Client-state
recoverability), answer the typed ``fauna.nest.outdated`` over anonymous WS-RPC
(not a raw SQL leak), and run NO destructive migration: re-stamping the reader
floor back lets the SAME image serve normally with the claimed admin intact. This
is the image/packaging counterpart of the binary-level
``tests/api/test_schema_version_compat.py`` (tier_3) — only tier_4 catches a
Dockerfile/s6 gap that would turn the degraded path into a crash-loop (e.g. an
s6 ``finish`` script that restart-loops on a non-zero exit, or a HEALTHCHECK that
the degraded liveness shape fails).
"""

import json
import os
import sqlite3
import ssl
import subprocess
import time
import urllib.request

import pytest


from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

from .helpers import (
    IMAGE_TAG,
    ROLES,
    admin_ws as _admin_ws,
    await_keypairs as _await_keypairs,
    bridge_diag as _bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    flag_present as _flag_present,
    get_repo_root,
    is_commanded_up as _is_commanded_up,
    keyfile_pubkey as _keyfile_pubkey,
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
CLAIM_CODE = "UPGRD1"

# Columns dropped to simulate an older ("pre-additive-step") schema. Both are
# read by ``list_active_mail_domains`` — the very query the mail bridge boots
# from — so a missing one is the failure shape. Neither has a manual
# ``ALTER`` guard, so their healing is attributable solely to the Layer-1 column
# reconciler running in the real image.
LEGACY_DROP_COLUMNS = ("dkim_selector_activated_at", "dkim_rotation_days")

@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (layer-cached; shared tag)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


def _quiesce_and_copy_node_db(name: str, dest: str) -> None:
    """Stop the container's nest service and copy ``/data/nest.db`` (plus any
    residual WAL/SHM) to ``dest`` on the host. Stopping the service first closes
    the SQLite handle and flushes the WAL, so the copy is a quiescent, self-
    contained DB. The bridges lose their nest connection — fine, the caller
    either mutates+redeploys (downgrade) or only reads (post-redeploy assert),
    and the container is removed shortly after."""
    subprocess.run(
        ["docker", "exec", name, "/command/s6-svc", "-wD", "-d", "/run/service/fauna-nest"],
        capture_output=True, timeout=30,
    )
    subprocess.run(
        ["docker", "cp", f"{name}:/data/nest.db", dest],
        check=True, capture_output=True, timeout=30,
    )
    # Pull any WAL/SHM alongside so a host checkpoint can replay uncommitted
    # frames (belt-and-suspenders vs the clean-shutdown flush).
    for suffix in ("-wal", "-shm"):
        subprocess.run(
            ["docker", "cp", f"{name}:/data/nest.db{suffix}", dest + suffix],
            capture_output=True, timeout=30,
        )


def _downgrade_schema_to_legacy(name: str, work_dir) -> None:
    """Rewrite the container's ``/data/nest.db`` to an OLDER schema — drop
    ``LEGACY_DROP_COLUMNS`` from ``mail_domains``, as a ``/data`` claimed before
    an additive step added them would lack them.

    Quiesces the DB, copies it out, mutates it on the host, copies it back, and
    drops the now-stale WAL/SHM."""
    local = os.path.join(str(work_dir), "nest.db")
    _quiesce_and_copy_node_db(name, local)

    if sqlite3.sqlite_version_info < (3, 35, 0):
        pytest.skip(f"host sqlite {sqlite3.sqlite_version} lacks ALTER TABLE DROP COLUMN (need 3.35+)")

    conn = sqlite3.connect(local)
    try:
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
        for col in LEGACY_DROP_COLUMNS:
            conn.execute(f"ALTER TABLE mail_domains DROP COLUMN {col};")
        conn.commit()
        md_present = {row[1] for row in conn.execute("PRAGMA table_info(mail_domains);")}
    finally:
        conn.close()
    still_there = set(LEGACY_DROP_COLUMNS) & md_present
    assert not still_there, f"mail_domains downgrade failed; columns still present: {still_there}"

    subprocess.run(
        ["docker", "cp", local, f"{name}:/data/nest.db"],
        check=True, capture_output=True, timeout=30,
    )
    # `docker cp` lands the file owned by root, but the nest runs as `fauna`
    # (UID 1000, Dockerfile) — a root-owned nest.db opens READ-ONLY, so the
    # redeployed nest's `run_migrations` dies "attempt to write a readonly
    # database" (SQLite error 8) and crash-loops without ever serving. Restore the
    # owner (as root, -u 0, since the container's default user may be unprivileged)
    # so the redeploy can write its migrations against the older-schema DB.
    subprocess.run(
        ["docker", "exec", "-u", "0", name, "chown", "1000:1000", "/data/nest.db"],
        check=True, capture_output=True, timeout=15,
    )
    # The old WAL/SHM are stale against the rewritten nest.db — remove them so
    # the redeployed nest opens a self-contained, checkpointed DB.
    subprocess.run(
        ["docker", "exec", "-u", "0", name, "rm", "-f", "/data/nest.db-wal", "/data/nest.db-shm"],
        capture_output=True, timeout=15,
    )


def _resume_serving_after_redeploy(name: str, nest: dict, mail_ports: dict, roles=ROLES) -> None:
    """Drive the redeployed bridges (fresh container, pre-existing ``/data``)
    back to serving all four ports. Mail-enable, enrollment, and approval all
    persist in the volume, so this is the post-approval tail only: wait for the
    auto-started s6 services + persisted keypairs, re-attest on cold boot,
    re-provision the (idempotent) cert, and assert the listeners serve."""
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if _flag_present(name) and all(
            _is_commanded_up(_svstat(name, f"fauna-mail-bridge-{r}")) for r in roles
        ):
            break
        time.sleep(0.5)
    assert _flag_present(name), "/data/imap-enabled must persist across the redeploy"
    for r in roles:
        st = _svstat(name, f"fauna-mail-bridge-{r}")
        assert _is_commanded_up(st), f"{r} s6 service must auto-start from the persisted flag; got {st!r}"

    _await_keypairs(name, roles)  # persisted in /data/keys → immediate

    with _admin_ws(nest) as admin:
        # The bridge should reconnect as APPROVED off the persisted service-user
        # row + keyfile (re-enrollment returns the approved status). Defensive:
        # if it somehow re-enrolled pending, approve it so the gate is robust to
        # either enrollment outcome — keyed by the persisted keyfile pubkey.
        pending = admin.call("fauna.bridges.list_service_users", {"status": "pending"})["service_users"]
        for row in pending:
            r = row["role"]
            pk = _keyfile_pubkey(name, r)
            if pk:
                admin.call(
                    "fauna.bridges.approve_pending_bridge",
                    {"ed25519_pubkey": bytes.fromhex(pk), "role": r},
                )

    # Nudge a clean cold-boot re-attest, then re-provision the cert (idempotent).
    for r in roles:
        _restart_service(name, f"fauna-mail-bridge-{r}")

    try:
        deadline = time.monotonic() + 90
        sealed: set[str] = set()
        reply: dict = {}
        while time.monotonic() < deadline:
            reply = _provision_self_signed_cert(nest, DOMAIN)
            sealed = {b["role"] for b in reply.get("bridges_sealed_to", [])}
            if set(roles) <= sealed:
                break
            time.sleep(3.0)
        assert set(roles) <= sealed, (
            f"cert must re-seal to {roles} after redeploy (proves x25519 re-attest); "
            f"last sealed={sealed}, "
            f"skipped={[b['role'] for b in reply.get('bridges_skipped_no_x25519', [])]}"
        )

        wait_for_smtp_banner("127.0.0.1", mail_ports[25], timeout=60)
        wait_for_smtp_banner("127.0.0.1", mail_ports[587], timeout=60)
        for r in roles:
            _sighup_service(name, f"fauna-mail-bridge-{r}")
        wait_for_tls_handshake("127.0.0.1", mail_ports[465], expect_banner_prefixes=(b"220 ", b"220-"), timeout=60)
        wait_for_tls_handshake("127.0.0.1", mail_ports[993], expect_banner_prefixes=(b"* OK", b"* "), timeout=60)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics (redeployed box) ──\n{_bridge_diag(name)}") from e


@pytest.mark.feature("mail-server")
def test_mail_bridge_serves_after_schema_upgrade(docker_image, tmp_path):
    vol = f"fauna-upgrade-data-{os.getpid()}-{find_free_ports(1)[0]}"
    subprocess.run(["docker", "volume", "create", vol], check=True, capture_output=True, timeout=30)
    name_a = name_b = None
    try:
        # ── Phase 1: a fresh box, on a persistent volume, driven to serving. ──
        http_a, *mail = find_free_ports(5)
        mail_a = dict(zip((25, 465, 587, 993), mail))
        name_a = f"fauna-upgrade-a-{http_a}"
        start_container_with_ports(
            name_a,
            {3000: http_a, **mail_a},
            env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
            data_volume=vol,
        )
        wait_for_health(http_a, name_a)
        admin = claim_admin_api(http_a, CLAIM_CODE, handle="admin")
        nest_a = {
            "name": name_a, "port": http_a, "url": f"https://127.0.0.1:{http_a}",
            "admin": admin, "mail_ports": mail_a,
        }
        _register_primary_domain(nest_a, DOMAIN)
        bring_bridges_to_serving(name_a, nest_a, mail_a, DOMAIN)

        # ── Phase 2: rewrite nest.db down to an older schema (two additive
        # columns dropped from mail_domains).
        _downgrade_schema_to_legacy(name_a, tmp_path)
        remove_container(name_a)
        name_a = None  # the volume (with the older-schema nest.db) persists

        # ── Phase 3: redeploy a FRESH container on the same /data — THE GATE. ──
        # The entrypoint takes its "existing config" path; the nest applies the
        # genesis + reconciles the dropped columns; the bridge's fetch_config succeeds and
        # it serves. On the image the bridge crash-looped here.
        http_b, *mail = find_free_ports(5)
        mail_b = dict(zip((25, 465, 587, 993), mail))
        name_b = f"fauna-upgrade-b-{http_b}"
        start_container_with_ports(
            name_b,
            {3000: http_b, **mail_b},
            # No claim code — the persisted /data is already claimed; the admin
            # actor (Ed25519) persists, so the same signing key still auths.
            env={"FAUNA_PORT": "3000"},
            data_volume=vol,
        )
        wait_for_health(http_b, name_b)
        nest_b = {
            "name": name_b, "port": http_b, "url": f"https://127.0.0.1:{http_b}",
            "admin": admin, "mail_ports": mail_b,
        }

        # Nest-side corroboration (Layer 4): the config query the bridge boots
        # from succeeds post-reconcile, so the mail-health signal is green — the
        # break is healed *before* we even probe the ports.
        with _admin_ws(nest_b) as admin_b:
            status = admin_b.call("fauna.setup.status", {})
            assert status.get("mail_subsystem_ok") is True, (
                "mail_subsystem_ok must be true after redeploy against the "
                f"older-schema /data (reconciler healed the SELECT); got {status!r}"
            )

        # The headline gate: the redeployed bridge serves all four mail ports.
        _resume_serving_after_redeploy(name_b, nest_b, mail_b)
    finally:
        if name_a:
            remove_container(name_a)
        if name_b:
            remove_container(name_b)
        subprocess.run(["docker", "volume", "rm", "-f", vol], capture_output=True, timeout=30)


# ── Incompatible-boot degraded-serve gate (version-compatibility.md § 2.2) ──────

# A reader floor far above any plausible ``CURRENT_SCHEMA_VERSION`` this image
# carries, so the verdict is unambiguously ``Incompatible`` regardless of future
# baseline bumps (mirrors the tier_3 ``test_schema_version_compat.py`` constant).
FUTURE_READER_FLOOR = 9999


def _stamp_schema_meta_in_container(
    name: str, work_dir, *, schema_version: int, min_reader_version: int,
) -> tuple[int, int]:
    """Force the container's ``/data/nest.db`` ``schema_meta`` row to
    ``(schema_version, min_reader_version)`` — the same host-side
    quiesce→copy-out→mutate→copy-in mechanism ``_downgrade_schema_to_legacy``
    uses, here writing the version pair a *newer* nest would have recorded.

    Returns the ``(schema_version, min_reader_version)`` that were there BEFORE
    the rewrite, so the caller can restore the baseline to recover (proving the
    incompatible boot ran no destructive migration). The fresh boot stamps
    ``(CURRENT_SCHEMA_VERSION, MIN_READER_SCHEMA_VERSION)``; capturing it keeps
    the recovery robust to a future baseline bump rather than hardcoding ``1``."""
    local = os.path.join(str(work_dir), f"node-stamp-{schema_version}-{min_reader_version}.db")
    _quiesce_and_copy_node_db(name, local)

    conn = sqlite3.connect(local)
    try:
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
        prev = conn.execute(
            "SELECT schema_version, min_reader_version FROM schema_meta WHERE id = 1"
        ).fetchone()
        assert prev is not None, (
            "the booted image must have recorded a schema_meta row on first boot "
            "(record_schema_meta at the end of run_migrations)"
        )
        updated = conn.execute(
            "UPDATE schema_meta SET schema_version = ?, min_reader_version = ? WHERE id = 1",
            (schema_version, min_reader_version),
        ).rowcount
        assert updated == 1, "schema_meta is a single id=1 row; the stamp must update exactly one"
        conn.commit()
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
    finally:
        conn.close()

    subprocess.run(
        ["docker", "cp", local, f"{name}:/data/nest.db"],
        check=True, capture_output=True, timeout=30,
    )
    # `docker cp` lands the file owned by root; the nest runs as UID 1000 (fauna)
    # and a root-owned nest.db opens READ-ONLY → the redeploy's run_migrations dies
    # "attempt to write a readonly database". Restore the owner (as root). Same
    # rationale as `_downgrade_schema_to_legacy`.
    subprocess.run(
        ["docker", "exec", "-u", "0", name, "chown", "1000:1000", "/data/nest.db"],
        check=True, capture_output=True, timeout=15,
    )
    # The old WAL/SHM are stale against the rewritten nest.db — remove them so the
    # redeployed nest opens a self-contained, checkpointed DB.
    subprocess.run(
        ["docker", "exec", "-u", "0", name, "rm", "-f", "/data/nest.db-wal", "/data/nest.db-shm"],
        capture_output=True, timeout=15,
    )
    return int(prev[0]), int(prev[1])


def _health_json(port: int) -> dict:
    """Read ``/api/v1/health`` and return its JSON body. The deploy image serves
    https with a self-signed bootstrap cert (verification off); fall back to http
    so a future plain-HTTP deploy still works — mirrors ``wait_for_health``."""
    unverified = ssl._create_unverified_context()
    last: Exception | None = None
    for url, ctx in (
        (f"https://127.0.0.1:{port}/api/v1/health", unverified),
        (f"http://127.0.0.1:{port}/api/v1/health", None),
    ):
        try:
            resp = urllib.request.urlopen(url, timeout=5, context=ctx)
            return json.loads(resp.read())
        except Exception as e:  # noqa: BLE001 — try the other scheme
            last = e
    raise AssertionError(f"could not read /api/v1/health on {port}: {last}")


def _nest_log_tail(name: str, n: int = 50) -> str:
    """Recent container log tail, appended to a failure message so a degraded-
    serve assertion failure (e.g. the image booted normally, or crash-looped)
    diagnoses itself."""
    logs = subprocess.run(
        ["docker", "logs", "--tail", str(n), name],
        capture_output=True, text=True, timeout=15,
    )
    return (logs.stdout + logs.stderr)[-2000:]


@pytest.mark.feature("upgrades-never-lose-data")
def test_incompatible_schema_boots_degraded_then_recovers(docker_image, tmp_path):
    """The real IMAGE, booting a ``/data`` whose ``schema_meta.min_reader_version``
    exceeds its ``CURRENT_SCHEMA_VERSION``, must boot DEGRADED — not migrate, not
    crash-loop — and recover losslessly when the floor is lowered again.

    The inverse of ``test_mail_bridge_serves_after_schema_upgrade``: that proves a
    *forward* migration heals an older ``/data``; this proves a *newer-breaking*
    ``/data`` (which no migration can heal) is handled honestly. Three boots on
    one persistent volume:

      Phase 1 — fresh image, claimed admin = real persistent user state.
      Phase 2 — stamp the reader floor above the image's CURRENT, redeploy a FRESH
                container on the SAME ``/data``. Assert (a) it stays UP
                (``/api/v1/health`` 200, ``status == needs_update``, NO crash-loop
                — the off-box-brick guarantee) and (b) every anonymous WS-RPC —
                ``fauna.nest.info`` included — returns the typed
                ``fauna.nest.outdated`` (not a raw SQL leak, not a hang).
      Phase 3 — re-stamp the captured baseline floor, redeploy again. Assert (c)
                the normal surface is back (``fauna.nest.info`` succeeds) AND the
                phase-1 admin still authenticates with ``claimed: true`` — proof
                the degraded boot ran NO destructive migration and dropped nothing.
    """
    vol = f"fauna-incompat-data-{os.getpid()}-{find_free_ports(1)[0]}"
    subprocess.run(["docker", "volume", "create", vol], check=True, capture_output=True, timeout=30)
    name_a = name_b = name_c = None
    try:
        # ── Phase 1: a fresh box on a persistent volume, claimed (real state). ──
        http_a = find_free_ports(1)[0]
        name_a = f"fauna-incompat-a-{http_a}"
        start_container_with_ports(
            name_a,
            {3000: http_a},
            env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
            data_volume=vol,
        )
        wait_for_health(http_a, name_a)
        admin = claim_admin_api(http_a, CLAIM_CODE, handle="admin")

        # ── Phase 2: stamp the reader floor above the image's CURRENT, redeploy. ──
        # Capture the baseline the fresh boot recorded so Phase 3 can restore it
        # (robust to a future baseline bump — never hardcodes the version pair).
        base_v, base_min = _stamp_schema_meta_in_container(
            name_a, tmp_path,
            schema_version=FUTURE_READER_FLOOR, min_reader_version=FUTURE_READER_FLOOR,
        )
        remove_container(name_a)
        name_a = None  # the volume (now stamped incompatible) persists

        http_b = find_free_ports(1)[0]
        name_b = f"fauna-incompat-b-{http_b}"
        start_container_with_ports(
            name_b,
            # No claim code — the persisted /data is already claimed.
            {3000: http_b},
            env={"FAUNA_PORT": "3000"},
            data_volume=vol,
        )
        # (a) The container stays UP. `wait_for_health` blocks on a 200 — if the
        # image instead crash-looped (the off-box brick this gate guards against)
        # it times out with the nest boot log. A 200 here is the degraded liveness
        # probe (the process is up; only the schema is incompatible).
        wait_for_health(http_b, name_b)
        health = _health_json(http_b)
        assert health.get("status") == "needs_update", (
            "the degraded image must report the needs-update liveness shape (not "
            f"the normal 'ok'), so the supervisor/watchdog does not restart-loop "
            f"it; got {health!r}\n── nest log ──\n{_nest_log_tail(name_b)}"
        )

        # (b) Every anonymous WS-RPC — fauna.nest.info included — gets the typed
        # fauna.nest.outdated, NOT a raw SQL leak and NOT a hang.
        with WsRpcAnonClient(f"https://127.0.0.1:{http_b}") as anon:
            with pytest.raises(RpcCallError) as exc:
                anon.call("fauna.nest.info", {})
        assert exc.value.code == "fauna.nest.outdated", (
            "the degraded image must answer fauna.nest.info with the typed "
            f"fauna.nest.outdated; got {exc.value.code!r} (details={exc.value.details!r})"
            f"\n── nest log ──\n{_nest_log_tail(name_b)}"
        )
        # The message is a localized key, not a raw server/SQL string (Dim 4).
        assert exc.value.message_key == "error.nest.outdated", (
            f"expected the localized error.nest.outdated key; got {exc.value.message_key!r}"
        )

        # ── Phase 3: re-stamp the captured baseline floor, redeploy → recovers. ──
        _stamp_schema_meta_in_container(
            name_b, tmp_path, schema_version=base_v, min_reader_version=base_min,
        )
        remove_container(name_b)
        name_b = None

        http_c = find_free_ports(1)[0]
        name_c = f"fauna-incompat-c-{http_c}"
        start_container_with_ports(
            name_c,
            {3000: http_c},
            env={"FAUNA_PORT": "3000"},
            data_volume=vol,
        )
        wait_for_health(http_c, name_c)
        url_c = f"https://127.0.0.1:{http_c}"

        # (c1) The normal surface is back — anonymous fauna.nest.info succeeds.
        with WsRpcAnonClient(url_c) as anon:
            info = anon.call("fauna.nest.info", {})
        assert isinstance(info, dict) and "version" in info, (
            "the recovered image must serve fauna.nest.info normally after the "
            f"reader floor is lowered again; got {info!r}"
        )

        # (c2) The claimed admin from Phase 1 still AUTHENTICATES (the actor row +
        # keys survived) and the box still reads claimed — the incompatible boot
        # ran NO destructive migration and dropped nothing (no user-data loss).
        nest_c = {"name": name_c, "port": http_c, "url": url_c, "admin": admin}
        with _admin_ws(nest_c) as adm:
            status = adm.call("fauna.setup.status", {})
        assert status.get("claimed") is True, (
            "the admin claim must survive the incompatible boot (the degraded path "
            f"opens no DB and runs no migration); got {status!r}"
        )
    finally:
        for n in (name_a, name_b, name_c):
            if n:
                remove_container(n)
        subprocess.run(["docker", "volume", "rm", "-f", vol], capture_output=True, timeout=30)
