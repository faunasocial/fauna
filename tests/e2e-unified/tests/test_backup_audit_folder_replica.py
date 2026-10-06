"""tier_3 — the backup audit's covered-folder mirror plane, anchored in this
device's own synced replica (`backup-destinations.md` § State & data shape →
*Ordinary-folder coverage* → *Retention + audit*).

What this witnesses, end to end over two real nests and the real sync agent:
a destination that **de-lists** a covered folder's mirror row (drops the
custody row and stops listing it) is caught by the owner's next audit pass —
because the population is the head set this device's replica
(`fsid-<ref>.db`, written by the agent's resident engine) names, not the
destination's own list. Before the shells handed the replica in
(`fauna_sync_engine::segment_backup::bound_replica_folder_index`) that plane
kept hash-verified presence over the list alone, and a de-listed row simply
vanished from the population: the de-listed case below is green only through
the wiring. The control case — the identical journey with nothing de-listed —
must stay quiet, so the alert cannot come from the index misfiring on a
healthy destination.

Flow under test::

    enroll (UI)       → a fresh destination on `second_nest` (fresh uuid ⇒
                        fresh audit record, its first pass due)
    folder (UI)       → create + bind a location + drop a file; the agent
                        uploads it (its own `file uploaded` log line)
    attach (UI)       → the folder's destination place; the coverage row's
                        `added_at` is the mirror plane's attach floor
    replica fresh     → the agent's replica DB records a consistency stamp
                        after the attach (a completed transfer or clean pass)
    run-now sweep     → the source nest mirrors the folder: a custody row on
                        the destination under `hex(path_hash(file))`
    [de-list]         → that row is deleted on the destination's own DB — the
                        hostile destination's act, not the user's (convention 8)
    audit at +~48 h   → `backup_audit_run_now`: the index names the head, the
                        list lacks it ⇒ a certain miss ⇒ `backup-audit-alert`

**Why the clock lands where it does (convention 14 — state, never timing).**
The reconcile counts a named head as missing only once the source's sweep has
had the freshness slack to run (`now ≥ max(recorded_at, attach) + 48 h`), and
trusts the replica only while it was itself consistent inside that slack
(`now − consistent_at ≤ 48 h`, `audit.rs::reconcile_with_folder_indexes`). So
the pass runs at `consistent_at + 48 h − MARGIN`, which satisfies both once
the replica's stamp is at least `MARGIN` past the attach — a condition this
test *reads* from the replica DB and waits on, never assumes. Freshness
(`local − destination` high-water) and overdue (7 d) do not move with `now`
at this offset, so neither arm can raise the banner on its own — the control
case pins exactly that.
"""

import secrets
import sqlite3
import time
from pathlib import Path

import blake3
import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import register_user
from helpers.folder_content import atomic_write, await_agent_upload, bind_location_under_set
from helpers.sync_agent_config import agent_state_base
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui leads; linux hands the same shared composition in (both run the real
    # agent as a direct-spawned child over the launch's isolated XDG world).
    # macOS passes `FaunaClient.syncStateDir` — the user-domain actor dir under
    # its pinned HOME, where the agent writes `fsid-<ref>.db`; it spawns that
    # agent only under `real_sync_agent`. windows runs no in-app engine for
    # bound folders: it hands in the detached agent's per-actor dir
    # (`AccountStateDir.SyncAgentStateDir`, over the shared
    # `local_agent_state_dir`), which `isolated_sync_agent` pins to this run's
    # own `--data-dir` — the base `_replica_db` reads back through
    # `driver.sync_agent_state_base`. Without that marker the windows agent would
    # not run on this run's pipe at all; it is a no-op on tui/linux/macos.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.windows,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
    pytest.mark.feature("backup-destinations-and-restore"),
]

FRESHNESS_SLACK_SECS = 48 * 60 * 60  # audit.rs::FRESHNESS_SLACK_SECS
# Headroom between the offset being computed and the pass reading its clock,
# and the minimum gap the replica's stamp must hold past the attach.
MARGIN_SECS = 120
# A ceiling, not an expectation: the agent re-scans every 30 s under e2e, and
# each drained pass (or a nudge upload) advances the stamp.
REPLICA_FRESH_CEILING_S = 420


def _user_client(nest_instance, test_user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )


def _replica_db(app, test_user, folder_id: int) -> Path:
    """The agent's per-set state DB for `folder_id` under this actor's scope —
    the file `ReplicaFolderIndex` reads (`FolderRef::Local(id).state_db_path`)."""
    base = agent_state_base(
        getattr(app.driver, "config_home", None),
        getattr(app.driver, "sync_agent_state_base", None),
    )
    return base / test_user["actor_id_hex"].lower() / f"fsid-local-{folder_id}.db"


def _replica_consistent_at(db: Path) -> int | None:
    """`TransferBacklog::last_sync_at` read back: max of the two anchors."""
    if not db.exists():
        return None
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT value FROM sync_anchor WHERE key IN "
            "('last_transfer_at', 'last_clean_pass_at')"
        ).fetchall()
    finally:
        conn.close()
    return max((r[0] for r in rows), default=None)


def _mirror_rows(second_nest, path_hex: str) -> list[tuple]:
    conn = sqlite3.connect(second_nest["db_path"], timeout=10.0)
    try:
        return conn.execute(
            "SELECT folder_id, path FROM backup_custody "
            "WHERE path = ? AND manifest_hash IS NOT NULL",
            (path_hex,),
        ).fetchall()
    finally:
        conn.close()


def _custody_paths(second_nest) -> list[str]:
    conn = sqlite3.connect(second_nest["db_path"], timeout=10.0)
    try:
        return [r[0] for r in conn.execute("SELECT path FROM backup_custody").fetchall()]
    finally:
        conn.close()


def _remove_destination_if_any(backups) -> None:
    """Finalizer: best-effort, so a failed test keeps its own diagnosis."""
    try:
        backups.navigate()
        while backups.destination_count() > 0:
            n = backups.destination_count()
            backups.remove_destination(0)
            backups.wait_for_destination_count(n - 1)
    except Exception:
        pass


@pytest.mark.parametrize("delist", [False, True], ids=["control", "delisted"])
def test_a_delisted_covered_folder_row_is_caught_by_the_replica_anchor(
    logged_in_app, nest_instance, second_nest, test_user, tmp_path, request, delist
):
    app = logged_in_app
    b = app.backups
    b.require_destination_management_supported()
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass

    # The audit clock offset is a process-wide static a sibling test may have
    # left shifted; zero it before anything is stamped through it.
    b.navigate()
    app.driver.call_command("backup_audit_run_now", {"now_offset_secs": 0}, timeout=60)
    _remove_destination_if_any(b)
    request.addfinalizer(lambda: _remove_destination_if_any(b))

    b.add_destination(second_nest["url"], name="Replica-anchor")
    b.wait_for_destination_count(1)

    # ── A synced folder with one uploaded file (UI + the real agent). ──
    name = f"anchored-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    location = tmp_path / "bound"
    location.mkdir()
    bind_location_under_set(app, name, location, seat="owner")
    filename = f"kept-{secrets.token_hex(3)}.txt"
    atomic_write(location / filename, "a head the destination must keep\n")
    await_agent_upload(app, filename, seat="owner")

    # ── Attach, through the folder's own expanded row. ──
    client = _user_client(nest_instance, test_user)
    with client:
        before = client.call("fauna.backup.destination.list", {})
    covered_before = {c["folder_id"] for d in before["destinations"] for c in d.get("covered_folders", [])}
    destination_id = before["destinations"][0]["destination_id"]
    b.navigate_folders()
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-destination-attach-select", timeout=15.0)
    app.driver.select("folder-destination-attach-select", destination_id)
    app.driver.click("folder-destination-attach-button")
    app.driver.wait_for("folder-destination-row", timeout=15.0)
    attached_by = int(time.time()) + 1
    with client:
        after = client.call("fauna.backup.destination.list", {})
    new = [
        c
        for d in after["destinations"]
        for c in d.get("covered_folders", [])
        if c["folder_id"] not in covered_before
    ]
    assert len(new) == 1, f"one new coverage row after the attach: {after!r}"
    folder_id = int(new[0]["folder_id"])

    # ── The replica must be known consistent MARGIN past the attach. ──
    db = _replica_db(app, test_user, folder_id)
    target = attached_by + MARGIN_SECS
    nudges = 0
    deadline = time.monotonic() + REPLICA_FRESH_CEILING_S
    consistent_at = _replica_consistent_at(db)
    while (consistent_at or 0) < target and time.monotonic() < deadline:
        # A nudge upload is a completed transfer — it advances the stamp even
        # if the idle scan does not. Its own head is recorded after the attach,
        # so it is "not yet expected" at the pass below and never a miss.
        if int(time.time()) >= target:
            nudges += 1
            atomic_write(location / f"nudge-{nudges}.txt", f"nudge {nudges}\n")
        time.sleep(5.0)  # sleep-ok: poll cadence of a deadline loop on the replica DB stamp
        consistent_at = _replica_consistent_at(db)
    assert consistent_at is not None and consistent_at >= target, (
        f"the agent's replica {db} never recorded a consistency stamp "
        f"{MARGIN_SECS}s past the attach (want >= {target}, have {consistent_at!r}, "
        f"{nudges} nudge upload(s)); without it the replica cannot witness and the "
        f"pass below would test nothing. db exists={db.exists()} "
        f"error={app.error_text()!r}"
    )

    # ── The source nest mirrors the folder (causal barrier, not a sleep). ──
    resp = requests.post(f"{nest_instance['url']}/api/v1/test/backup/run-now", json={}, timeout=120)
    assert resp.status_code == 200 and resp.json().get("ok") is True, resp.text
    path_hex = blake3.blake3(filename.encode()).hexdigest()
    rows = _mirror_rows(second_nest, path_hex)
    assert rows, (
        f"the sweep mirrored no custody row for {filename!r} (path {path_hex}) on the "
        f"destination — nothing to de-list, and the control proves nothing. sweep="
        f"{resp.json()!r} destination custody paths={_custody_paths(second_nest)!r}"
    )

    if delist:
        conn = sqlite3.connect(second_nest["db_path"], timeout=10.0)
        try:
            with conn:
                conn.execute("DELETE FROM backup_custody WHERE path = ?", (path_hex,))
        finally:
            conn.close()
        assert not _mirror_rows(second_nest, path_hex), "the de-list did not take"

    # ── The audit pass, inside both slack windows. ──
    consistent_at = _replica_consistent_at(db) or consistent_at
    offset = consistent_at + FRESHNESS_SLACK_SECS - MARGIN_SECS - int(time.time())
    b.navigate()
    b.wait_for_destination_count(1)
    app.driver.call_command("backup_audit_run_now", {"now_offset_secs": offset}, timeout=120)

    if delist:
        wait_until(
            lambda: app.driver.count("backup-audit-alert") >= 1,
            30.0,
            diagnose=lambda: (
                f"the destination de-listed {filename!r}'s mirror row but the audit "
                f"raised no backup-audit-alert: the covered-folder plane did not "
                f"anchor in this device's replica ({db}, consistent_at="
                f"{consistent_at}, offset={offset}) — the shell is still handing "
                f"in NoFolderIndex, or the bound nest id did not resolve. "
                f"error={app.error_text()!r}"
            ),
        )
    else:
        # The pass has already rendered when the command acks (tui awaits the
        # op; linux's poll below covers its own render), so quiet is quiet.
        assert app.driver.count("backup-audit-alert") == 0, (
            f"nothing was de-listed, yet the audit raised "
            f"{app.driver.count('backup-audit-alert')} backup-audit-alert(s) at "
            f"offset={offset}: the replica anchor misfires on a healthy destination, "
            f"or freshness/overdue moved with the clock. "
            f"text={app.driver.get_text('backup-audit-alert')!r} "
            f"error={app.error_text()!r}"
        )
    assert not app.has_error(), f"error={app.error_text()!r}"
