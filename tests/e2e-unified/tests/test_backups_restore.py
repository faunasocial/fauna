"""Backups restore surfaces — full-stack (tier_3) UI E2E.

Drives a real client (Linux; web where it lands) against a real
nest, seeding snapshot / restore_history / bridge_restore_divergence
state over WS-RPC as the SAME actor the app is logged in as (the
`test_user` whose key `logged_in_app` signs in with), then asserts the
Backups page renders the restore surfaces per `docs/goal/ui/backups.md`
§ Restore history / § Restore divergence / § Restore from backup
destination.

The API-level contract for the WS-RPC calls used here lives in
`tests/api/test_dr_restore.py`; this file is the UI counterpart.

Process safety: the nest is the shared `nest_instance`
fixture; no process is killed. The single direct-SQLite write (the
BridgeMda service-user seed for the CalDAV divergence path) mirrors the
`mail_bridge_mta` conftest fixture, which hand-pokes the same DB while
the nest runs.
"""

from __future__ import annotations

import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.waiting import wait_until
from i18n.strings import S

# The Backups restore surfaces (history / divergence / local-restore) are
# implemented on linux + web (an internal follow-up track + the 2026-05-31 web parity),
# android (2026-06-15 RestoreHistorySection/LocalRestoreSection), windows
# (tracked internally, 2026-06-15 — BackupsViewModel restore state + BackupsPage
# restore surface over the FfiSnapshotsClient seam), and now apple (macos + ios,
# 2026-07-17 — the shared FaunaKit RestoreSectionView + RestoreVM over the same
# FfiSnapshotsClient seam; the last ❌-owed cell on backups.md § Impl-status). The
# platform-agnostic BackupsActions restore helpers drive by element id, so these
# tests run on apple once the restore-* IDs render. tui joined 2026-07-24
# (`apps/fauna-tui/src/backups.rs` restore section, over the same shared
# `fauna-client-snapshots` SnapshotsClient linux reaches directly).
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


def _ws(nest: dict, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _seed_approved_mda(db_path: str, ed25519_pubkey: bytes, bridge_id: str) -> None:
    """Enroll `ed25519_pubkey` as an approved MDA bridge service user so
    `fauna.bridges.sync_calendar_since` resolves to CallerClass::BridgeMda.
    Mirrors `tests/api/test_dr_restore.py::_seed_approved_mda`.

    `x25519_pubkey` is left NULL — unused for class resolution, and this
    row lives in the SHARED session-scoped `nest_instance`, outliving this
    test. A non-NULL placeholder (`b"\\x00" * 32`, the original choice) is
    cryptographically invalid but still `Some(...)`, so it sails past
    `self_signed_cert.rs`'s `x25519_pubkey.is_some()` skip-check: any LATER
    test in the same pytest session that triggers
    `fauna.bridges.provision_self_signed_cert` for this domain fans an HPKE
    seal out to every approved bridge and deterministically fails
    encapsulating to the garbage key (`hpke::single_shot_seal:
    Encapsulation failed`) — order-dependent, not load-dependent, and it
    cost us two full >1h batched sweeps to
    trace (the labeler/rescore/mail-scored/mda-junk `fauna.protocol.internal`
    setup-ERROR cluster). NULL matches what production actually writes for
    an approved-but-not-yet-self-attested bridge, which the fan-out already
    skips gracefully (`bridges_skipped_no_x25519`).
    """
    now = int(time.time())
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        conn.execute(
            "INSERT OR REPLACE INTO bridge_service_users"
            " (ed25519_pubkey, x25519_pubkey, role, bridge_id, status,"
            "  created_at, approved_at)"
            " VALUES (?, ?, 'mda', ?, 'approved', ?, ?)",
            (ed25519_pubkey, None, bridge_id, now, now),
        )
        conn.commit()
    finally:
        conn.close()


def _create_mail_snapshot(nest: dict, owner: dict) -> int:
    with _ws(nest, owner) as ws:
        reply = ws.call("fauna.filesync.snapshot.create_message_kind", {"kind": "mail"})
        return reply["snapshot_id"]


def _restore(nest: dict, owner: dict, snapshot_id: int) -> None:
    with _ws(nest, owner) as ws:
        ws.call(
            "fauna.filesync.snapshot.restore_message_kind",
            {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
        )


@pytest.mark.feature("backup-destinations-and-restore")
def test_restore_history_renders(logged_in_app, nest_instance, test_user):
    """Create + restore a mail snapshot → the Backups page shows a
    restore-history row for it (`backups.md` § Restore history)."""
    snapshot_id = _create_mail_snapshot(nest_instance, test_user)
    _restore(nest_instance, test_user, snapshot_id)

    logged_in_app.backups.navigate()
    assert logged_in_app.driver.is_visible("restore-history-section"), (
        "Backups page should render the restore-history-section after a restore: "
        f"{logged_in_app.driver.diagnose('restore-history-section')} "
        f"error={logged_in_app.error_text()!r}"
    )

    # Poll: the section fetches on navigate; allow the round trip to land.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if logged_in_app.backups.restore_history_count() >= 1:
            break
        time.sleep(0.5)
    assert logged_in_app.backups.restore_history_count() >= 1, (
        f"error: {logged_in_app.driver.get_text('error-message')!r}"
    )
    # Local snapshot (no backup destination configured → source_member_id None).
    row_text = logged_in_app.backups.restore_history_item_text(0)
    assert S.backups.restore_source_local in row_text.lower()
    assert S.backups.restore_kinds_mail in row_text.lower()


@pytest.mark.feature("backup-destinations-and-restore")
def test_local_restore_action_restores_mail(logged_in_app, nest_instance, test_user):
    """The local restore action: pick the listed snapshot, re-type its id
    to arm the friction bar, restore (`backups.md` § Restore from backup
    destination — local path)."""
    snapshot_id = _create_mail_snapshot(nest_instance, test_user)

    logged_in_app.backups.navigate()

    # The local snapshot picker is populated from fauna.filesync.snapshot.list.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if logged_in_app.driver.is_visible("restore-snapshot-select"):
            break
        time.sleep(0.5)
    assert logged_in_app.driver.is_visible("restore-snapshot-select"), (
        "local snapshot picker should populate from fauna.filesync.snapshot.list: "
        f"{logged_in_app.driver.diagnose('restore-snapshot-select')} "
        f"error={logged_in_app.error_text()!r}"
    )

    # Friction bar: button disabled until the typed id matches the snapshot.
    assert not logged_in_app.backups.is_restore_button_enabled()
    logged_in_app.backups.type_restore_confirm("not-the-id")
    assert not logged_in_app.backups.is_restore_button_enabled()
    logged_in_app.backups.type_restore_confirm(str(snapshot_id))
    # The picker fills from its own fetch, which a visible (still empty) picker
    # does not prove has landed; the bar arms once the typed id matches the
    # SELECTED snapshot, so it may arm only when that fetch lands — a one-shot
    # read here raced it (the 2026-09-22 linux sweep's `assert False`).
    wait_until(
        logged_in_app.backups.is_restore_button_enabled,
        15,
        diagnose=lambda: (
            f"the restore button never armed for snapshot {snapshot_id}; "
            f"{logged_in_app.driver.diagnose('restore-snapshot-select')} "
            f"error={logged_in_app.error_text()!r}"
        ),
    )

    logged_in_app.backups.click_restore()

    # Restore writes a restore_history row; the history section reflects it.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if logged_in_app.backups.restore_history_count() >= 1:
            break
        time.sleep(0.5)
    assert logged_in_app.backups.restore_history_count() >= 1, (
        f"progress={logged_in_app.backups.restore_progress_text()!r} "
        f"error={logged_in_app.driver.get_text('error-message')!r}"
    )


@pytest.mark.feature("backup-destinations-and-restore")
def test_a_restore_reports_progress_and_says_when_it_is_done(
    logged_in_app, nest_instance, test_user
):
    """`restore-progress` starts at its idle prompt and reaches its DONE
    terminal state once the restore lands (`backups.md` § Restore from backup
    destination).

    The local-restore journey reads this element only inside a failure message,
    so a page whose progress line never moved off "Select a snapshot and re-type
    its id to restore." has always passed — the owner would run a restore and be
    told, for ever, to start one. **Measured while writing this: linux was
    exactly that page.** Its click armed "Restoring…" and nothing ever wrote a
    terminal state, so a finished restore read as still running; fixed with this
    test (`views/backups/restore.rs` + the `message_kind_restored` arms in
    `app.rs`). The other six already reached done.

    **The assertion is the pair of stable states, never the transient one**
    (convention 14). "Restoring…" is armed by the click and cleared by the reply,
    so waiting to *observe* it asserts a wall-clock window — defunct by
    construction. What is latency-independent is the baseline before the click
    and the terminal state after it, and the baseline is what makes the terminal
    assertion mean anything: without it a page hard-coded to "Done" passes.

    ⚠ `backups.md:56` also lists five per-STEP texts ("waiting for mail chunks",
    "replaying mail manifest", …). No app paints them — every one renders the
    three-state idle/running/done line instead. That is a check-the-app finding
    recorded in § Implementation status today, not something this test asserts.
    """
    snapshot_id = _create_mail_snapshot(nest_instance, test_user)

    logged_in_app.backups.navigate()

    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if logged_in_app.driver.is_visible("restore-snapshot-select"):
            break
        time.sleep(0.5)
    assert logged_in_app.driver.is_visible("restore-snapshot-select"), (
        "local snapshot picker should populate from fauna.filesync.snapshot.list: "
        f"{logged_in_app.driver.diagnose('restore-snapshot-select')} "
        f"error={logged_in_app.error_text()!r}"
    )

    # The baseline. A page that paints "Done" from the start satisfies the
    # terminal assertion below and reports nothing at all. A WAIT, not a
    # one-shot read: arriving on the page is itself what returns the line to its
    # idle prompt (tui's load rewrites the field; linux's nav handler resets the
    # label), and an earlier restore in this module leaves the page on "Done"
    # until that lands.
    before = logged_in_app.backups.wait_for_restore_progress(
        lambda t: t.strip() == S.backups.restore_progress_idle, timeout=30
    )
    assert before, (
        f"restore-progress reads "
        f"{logged_in_app.backups.restore_progress_text()!r} on a freshly opened "
        f"Backups page, not the idle prompt "
        f"{S.backups.restore_progress_idle!r}. Two things this catches: a line "
        f"hard-coded to its terminal text (which would satisfy the assertion "
        f"below while reporting nothing), and a page that keeps reporting the "
        f"LAST restore to an owner who has just walked in. "
        f"error={logged_in_app.error_text()!r}"
    )

    logged_in_app.backups.type_restore_confirm(str(snapshot_id))
    wait_until(
        logged_in_app.backups.is_restore_button_enabled,
        15,
        diagnose=lambda: (
            f"the restore button never armed for snapshot {snapshot_id}; "
            f"{logged_in_app.driver.diagnose('restore-snapshot-select')} "
            f"error={logged_in_app.error_text()!r}"
        ),
    )
    logged_in_app.backups.click_restore()

    done = logged_in_app.backups.wait_for_restore_progress(
        lambda t: t.strip() == S.backups.restore_progress_done
    )
    assert done, (
        f"restore-progress never reached "
        f"{S.backups.restore_progress_done!r}; it reads "
        f"{logged_in_app.backups.restore_progress_text()!r}. The restore's own "
        f"history row is the split verdict — "
        f"restore_history_count={logged_in_app.backups.restore_history_count()} — "
        f"so a non-zero count here means the restore LANDED and the page simply "
        f"never said so. error={logged_in_app.error_text()!r}"
    )
    assert not logged_in_app.error_text(), (
        "a restore that reached its done state raises no error banner: "
        f"{logged_in_app.error_text()!r}"
    )


def _wrapped_mls_blob_rows(db_path: str, actor_id: bytes) -> list[tuple[str, bytes]]:
    """The actor's `bridge_wrapped_mls_blobs` rows as `(credential_id, blob)` —
    the table the nest's restore reads for its `config_present` advisory. A read
    only; every write in this module goes through the owner's own WS-RPC kinds."""
    conn = sqlite3.connect(db_path, timeout=10.0)
    try:
        return [
            (credential_id, bytes(blob))
            for credential_id, blob in conn.execute(
                "SELECT credential_id, blob FROM bridge_wrapped_mls_blobs"
                " WHERE actor_id = ?",
                (actor_id,),
            )
        ]
    finally:
        conn.close()


def _provision_wrapped_mls_blob(ws, credential_id: str, blob: bytes) -> None:
    ws.call(
        "fauna.bridges.provision_wrapped_mls_blob",
        {"blob": blob, "credential_id": credential_id},
    )


def _restore_through_the_page(app, snapshot_id: int) -> None:
    """Drive one local restore of `snapshot_id` from a freshly opened Backups
    page and wait for its DONE terminal state (the barrier the warning's own
    render lands with).

    Leaves for another page first: navigating to Backups while already on it
    is no arrival at all on linux (the stack's visible child does not change)
    or web (same route, no remount), so the page would still be reporting the
    previous arm's restore."""
    app.settings.navigate()
    app.backups.navigate()
    wait_until(
        lambda: app.driver.is_visible("restore-snapshot-select"),
        15,
        diagnose=lambda: (
            "local snapshot picker should populate from fauna.filesync.snapshot.list: "
            f"{app.driver.diagnose('restore-snapshot-select')} "
            f"error={app.error_text()!r}"
        ),
    )
    idle = app.backups.wait_for_restore_progress(
        lambda t: t.strip() == S.backups.restore_progress_idle, timeout=30
    )
    assert idle, (
        f"restore-progress reads {app.backups.restore_progress_text()!r} on a "
        f"freshly opened Backups page, not the idle prompt; "
        f"error={app.error_text()!r}"
    )
    assert app.backups.restore_warning_text() == "", (
        "restore-warning is a one-shot advisory about the restore that produced "
        "it; a freshly opened page, before any restore, must not paint one — it "
        f"reads {app.backups.restore_warning_text()!r}"
    )
    app.backups.type_restore_confirm(str(snapshot_id))
    wait_until(
        app.backups.is_restore_button_enabled,
        15,
        diagnose=lambda: (
            f"the restore button never armed for snapshot {snapshot_id}; "
            f"{app.driver.diagnose('restore-snapshot-select')} "
            f"error={app.error_text()!r}"
        ),
    )
    app.backups.click_restore()
    done = app.backups.wait_for_restore_progress(
        lambda t: t.strip() == S.backups.restore_progress_done
    )
    assert done, (
        f"restore-progress never reached {S.backups.restore_progress_done!r}; it "
        f"reads {app.backups.restore_progress_text()!r}. "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("backup-destinations-and-restore")
def test_a_restore_without_the_account_configuration_says_so_and_a_complete_one_does_not(
    logged_in_app, nest_instance, test_user
):
    """`restore-warning` renders after a restore whose reply said
    `config_present == false`, and is absent after one that said `true`
    (`backups.md` § Restore from backup destination).

    The advisory is one-shot: the restore reply is its only carrier, no read
    reproduces it, so an app that drops it has destroyed the only copy — linux,
    tui and web did exactly that until this test, and the three apps that did
    paint it gave it no id, so no test could see it either.

    **Both arms, one account.** The nest computes `config_present` from whether
    the owner holds any `bridge_wrapped_mls_blobs` row (the same fixture shape
    `tests/api/test_dr_restore.py` pins at the API: a fresh actor's mail restore
    answers `False`). This module shares one session-scoped `test_user`, which an
    earlier test may already have given a blob, so the ABSENT arm first moves
    any such rows aside (through the owner's own revoke kind) and the finally
    puts them back byte-for-byte; the PRESENT arm provisions one blob under a
    credential of its own and revokes it after. Each arm restores a snapshot of
    its own, so neither reads the other's reply.

    The absence assertion is latency-independent: every app writes the warning
    in the same fold that writes the DONE state, so once DONE is read the
    warning's verdict is final (convention 14).
    """
    actor_id = test_user["actor_id_bytes"]
    db_path = nest_instance["db_path"]
    parked = _wrapped_mls_blob_rows(db_path, actor_id)
    own_credential = f"e2e-restore-warning-{secrets.token_hex(6)}"
    try:
        # ── Arm 1: no configuration → the warning renders ──
        with _ws(nest_instance, test_user) as ws:
            for credential_id, _blob in parked:
                ws.call(
                    "fauna.bridges.revoke_wrapped_mls_blob",
                    {"actor_id": actor_id, "credential_id": credential_id},
                )
        assert _wrapped_mls_blob_rows(db_path, actor_id) == [], (
            "the absent arm needs an owner with no wrapped blob at all"
        )
        absent_snapshot = _create_mail_snapshot(nest_instance, test_user)
        _restore_through_the_page(logged_in_app, absent_snapshot)
        warning = logged_in_app.backups.restore_warning_text()
        assert warning.strip() == S.backups.restore_warning_config_absent, (
            f"a restore whose reply said config_present == false must paint "
            f"restore-warning with {S.backups.restore_warning_config_absent!r}; it "
            f"reads {warning!r} "
            f"({logged_in_app.driver.diagnose('restore-warning')}). The restore "
            f"reached DONE, so the reply landed — an empty read here means the app "
            f"dropped the advisory. error={logged_in_app.error_text()!r}"
        )
        assert not logged_in_app.error_text(), (
            "a completed-with-caveat restore is not a failure and never reaches "
            f"error-message: {logged_in_app.error_text()!r}"
        )

        # ── Arm 2: configuration present → no warning ──
        with _ws(nest_instance, test_user) as ws:
            _provision_wrapped_mls_blob(ws, own_credential, b"\x00" * 32)
        present_snapshot = _create_mail_snapshot(nest_instance, test_user)
        _restore_through_the_page(logged_in_app, present_snapshot)
        assert logged_in_app.backups.restore_warning_text() == "", (
            "a restore whose reply said config_present == true must render no "
            f"restore-warning; it reads "
            f"{logged_in_app.backups.restore_warning_text()!r}"
        )
    finally:
        with _ws(nest_instance, test_user) as ws:
            ws.call(
                "fauna.bridges.revoke_wrapped_mls_blob",
                {"actor_id": actor_id, "credential_id": own_credential},
            )
            for credential_id, blob in parked:
                _provision_wrapped_mls_blob(ws, credential_id, blob)


@pytest.mark.feature("backup-destinations-and-restore")
def test_restore_divergence_flags_a_reconnected_mail_app_as_it_does_a_calendar(
    logged_in_app, nest_instance, test_user
):
    """A MAIL app that reconnects after a restore holding newer state is flagged
    with what it lost, exactly as a calendar app is.

    `backups.md` § Restore divergence names TWO seams, and the first is IMAP:
    *"at IMAP `SELECT` when a MUA supplies QRESYNC `(last_uid_validity,
    last_modseq)` and the modseq exceeds the server's
    `bridge_imap_mailbox_state.highestmodseq` for the target mailbox"*. The nest
    has written that row since T3.2-b (`bridge_imap_handlers.rs`, inside
    `fauna.bridges.select_mailbox`), and `test_restore_divergence_banner_and_modal`
    drives the CalDAV seam only — so the mail half of a page whose whole subject
    is mail restore has never been witnessed from any app. The two seams share
    the writer and the surface but not the handler, and a forwarding bug in the
    IMAP one (the Go bridge's QRESYNC parse reaches the nest through
    `SelectMailboxRequest.client_qresync`, which is `Option` and defaults to
    `None`) would be invisible to the calendar journey.

    The server's own `highestmodseq` is READ, not assumed: the first SELECT
    carries no QRESYNC hint, so it writes nothing and answers with the number the
    second one must exceed. That makes the lost-write count an exact, seeded
    delta rather than an arithmetic guess about a bootstrapped mailbox.
    """
    # Snapshot + restore: `write_divergence_row` keys the row to the actor's most
    # recent restore_history row and is a deliberate no-op without one.
    snapshot_id = _create_mail_snapshot(nest_instance, test_user)
    _restore(nest_instance, test_user, snapshot_id)

    mda = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    _seed_approved_mda(
        nest_instance["db_path"],
        mda["actor_id_bytes"],
        bridge_id=f"e2e-ui-imap-divergence-{secrets.token_hex(6)}",
    )

    mailbox = "INBOX"
    mua_id = "Fauna-UI-MUA/2.0"
    lost = 7
    with _ws(nest_instance, mda) as mda_ws:
        # No QRESYNC hint: bootstraps the standard mailboxes, writes no
        # divergence row, and reports the modseq the ahead client must exceed.
        baseline = mda_ws.call(
            "fauna.bridges.select_mailbox",
            {"actor_id": test_user["actor_id_bytes"], "mailbox": mailbox},
        )
        assert baseline["outcome"] == "selected", (
            f"the owner's {mailbox} must select before a QRESYNC hint can be "
            f"compared against it: {baseline!r}"
        )
        server_modseq = baseline["highestmodseq"]

        ahead = mda_ws.call(
            "fauna.bridges.select_mailbox",
            {
                "actor_id": test_user["actor_id_bytes"],
                "mailbox": mailbox,
                "client_qresync": {
                    "last_uid_validity": baseline["uid_validity"],
                    "last_modseq": server_modseq + lost,
                },
                "mua_id": mua_id,
            },
        )
        # Server state wins: the SELECT still succeeds (RFC 7162 §3.2.5.2
        # stale-modseq fallback). The divergence is forensic, never a refusal.
        assert ahead["outcome"] == "selected", (
            f"a QRESYNC modseq ahead of the server must still SELECT — the row "
            f"exists so the user can see what they lost, not to block the MUA: "
            f"{ahead!r}"
        )

    logged_in_app.backups.navigate()

    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if logged_in_app.backups.restore_history_count() >= 1 and (
            logged_in_app.backups.has_divergence_banner(0)
        ):
            break
        time.sleep(0.5)
    assert logged_in_app.backups.has_divergence_banner(0), (
        "expected a divergence banner on the restore-history row after an IMAP "
        "SELECT arrived with a modseq ahead of the server's. "
        f"restore_history_count={logged_in_app.backups.restore_history_count()} "
        f"error={logged_in_app.error_text()!r}"
    )

    logged_in_app.backups.open_divergence_modal(0)
    assert logged_in_app.backups.is_divergence_modal_visible()
    assert logged_in_app.backups.divergence_detail_count() >= 1
    detail = logged_in_app.backups.divergence_detail_text(0)
    assert mailbox in detail, (
        f"the forensic row reads {detail!r} and does not name the MAILBOX that "
        f"diverged. The collection is what tells the owner which part of their "
        f"mail the reconnecting app was holding newer state for."
    )
    assert mua_id in detail, (
        f"the forensic row reads {detail!r} and does not name the MUA that "
        f"reconnected ({mua_id!r}, carried on the SELECT's advisory `mua_id`)."
    )
    assert str(lost) in detail, (
        f"the forensic row reads {detail!r} and does not carry the ~{lost} lost "
        f"writes (client modseq {server_modseq + lost} - server modseq "
        f"{server_modseq}). The count is the whole content of the flag."
    )


@pytest.mark.feature("backup-destinations-and-restore")
def test_restore_divergence_banner_and_modal(logged_in_app, nest_instance, test_user):
    """A CalDAV ahead-token divergence against a restored snapshot surfaces
    the per-row banner + the forensic details modal (`backups.md`
    § Restore divergence)."""
    # Snapshot + restore (writes the restore_history row the divergence keys to).
    snapshot_id = _create_mail_snapshot(nest_instance, test_user)
    _restore(nest_instance, test_user, snapshot_id)

    # Owner provisions a fresh calendar (highestmodseq == 1).
    calendar_id = secrets.token_bytes(32)
    with _ws(nest_instance, test_user) as ws:
        prov = ws.call(
            "fauna.bridges.provision_calendar",
            {
                "actor_id": test_user["actor_id_bytes"],
                "calendar_id": calendar_id,
                "encrypted_metadata": b"{}",
                "update_metadata": False,
            },
        )
        assert prov["outcome"] == "created"

    # An MDA-class actor drives sync_calendar_since with an ahead token →
    # writes one bridge_restore_divergence row keyed to the latest restore.
    mda = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    _seed_approved_mda(
        nest_instance["db_path"],
        mda["actor_id_bytes"],
        bridge_id=f"e2e-ui-divergence-{secrets.token_hex(6)}",
    )
    ahead_token = 99
    with _ws(nest_instance, mda) as mda_ws:
        sync = mda_ws.call(
            "fauna.bridges.sync_calendar_since",
            {
                "actor_id": test_user["actor_id_bytes"],
                "calendar_id": calendar_id,
                "sync_token": str(ahead_token),
                "limit": 0,
                "mua_id": "Fauna-UI-Test/1.0",
            },
        )
        assert sync["outcome"] == "stale"

    logged_in_app.backups.navigate()

    # The history row for the restored snapshot shows a divergence banner.
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if logged_in_app.backups.restore_history_count() >= 1 and (
            logged_in_app.backups.has_divergence_banner(0)
        ):
            break
        time.sleep(0.5)
    assert logged_in_app.backups.has_divergence_banner(0), (
        "expected a divergence banner on the restore-history row"
    )
    assert "1" in logged_in_app.backups.divergence_banner_text(0)

    # Clicking the banner opens the forensic details modal.
    logged_in_app.backups.open_divergence_modal(0)
    assert logged_in_app.backups.is_divergence_modal_visible()
    details = [
        logged_in_app.backups.divergence_detail_text(i)
        for i in range(logged_in_app.backups.divergence_detail_count())
    ]
    assert details, "the divergence modal listed no forensic row"
    # The modal lists every divergence recorded against the restore it was opened
    # from, and the IMAP journey above records its own on the same actor, so the
    # row THIS journey wrote is found by the MUA it named, never by position:
    # position 0 reads the neighbour's row whenever the two share a restore.
    own = [d for d in details if "Fauna-UI-Test/1.0" in d]
    history = [
        (i, logged_in_app.backups.has_divergence_banner(i))
        for i in range(logged_in_app.backups.restore_history_count())
    ]
    assert own, (
        "no forensic row names this journey's MUA 'Fauna-UI-Test/1.0'. The modal "
        f"opened from history row 0 lists {details!r}; the history rows and "
        f"whether each shows a banner are {history!r}."
    )
    # ~98 writes lost (= ahead_token 99 − server highestmodseq 1).
    assert "98" in own[0], f"the forensic row reads {own[0]!r}; modal rows {details!r}"
