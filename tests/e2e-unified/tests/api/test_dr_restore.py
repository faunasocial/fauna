"""Disaster-recovery restore — full-stack (tier_3) E2E.

Exercises the message-kind snapshot create → restore → restore-history
read path end-to-end through the real nest WS-RPC surface + real SQLite,
plus the CalDAV restore-divergence (γ) path that backs the Backups page
"N writes lost" banner.

Scope (per the IMAP/CalDAV restore plan, Task 13, tracked internally;
reframed against current code):

  * The in-scope flow here is the **mail** create→restore round trip
    (functional since T15 pinned the placement manifest at create) plus
    the **CalDAV divergence** detection at sync-collection.
  * ⚠ Calendar/card create→restore is no longer out of reach: S6.9
    (2026-07-10) replaced `calendar_not_implemented()` with a real create
    arm pinning both manifests, and added the card twin. Its round trip
    is NOT driven here, because a faithful one needs a *sealed* event/card
    body at rest, which needs an MSEK recipient + a real sealing MDA —
    this module's actors are bare registered actors with no MLS key. It
    lives in `tests/test_dav_content_at_rest_e2e.py` (S6.11), on the
    `restartable_mda_nest` fixture, which has both.
  * This is a **single-nest** DR exercise: restore replays the snapshot's
    pinned placement manifest back into `bridge_imap_*` on the same nest.
    Cross-nest segment transport (copying chunks from one nest to
    another) is fauna-sync's job and explicitly out of Plan 2 scope.

Each test gets its **own** nest (the `dr_nest` fixture). The reserved
folder name (`__mail`) is globally UNIQUE in nest's schema — a known
latent multi-user bug noted at `bins/fauna-nest/src/db/mod.rs:4856` — so
only one actor per nest can own a `__mail` snapshot. A per-test nest
keeps the snapshot-creating tests independent of each other and of suite
ordering, without papering over the bug.

Process safety: no `pkill`/`killall`; the nest is started via
the framework's `start_nest` helper (the same path `test_caldav.py` /
`test_cross_nest_api.py` use) and only this test's own process handle is
killed on teardown. The single direct-SQLite write (the BridgeMda
service-user seed) mirrors the `mail_bridge_mta` conftest fixture, which
hand-pokes the same DB while the nest runs.
"""

from __future__ import annotations

import secrets
import sqlite3
import time

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def dr_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest, exclusive to one test (see module docstring re: the
    globally-UNIQUE `__mail` folder name)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "dr-nest")
    yield nest
    cleanup()


def _seed_approved_mda(db_path: str, ed25519_pubkey: bytes, bridge_id: str) -> None:
    """Enroll `ed25519_pubkey` as an approved MDA bridge service user.

    `caller_class_for_actor` (bins/fauna-nest/src/bridge_method_allowlist.rs)
    resolves an approved `role='mda'` row to `CallerClass::BridgeMda`, the
    class `fauna.bridges.sync_calendar_since` requires. The real enrollment
    path is admin pre-register + admin approve over WS-RPC; seeding the row
    directly is the test-side shortcut (same pattern as the MTA fixture
    seeding `recipient_routes`). `x25519_pubkey` is unused for class
    resolution, so it is left NULL — matching what production writes for an
    approved-but-not-yet-self-attested bridge, which
    `self_signed_cert.rs`'s cert fan-out already skips gracefully
    (`bridges_skipped_no_x25519`). A non-NULL placeholder is `Some(...)` and
    sails past that skip-check, so on a fixture whose nest outlives this
    test it can poison a later `provision_self_signed_cert` call's HPKE
    seal — this file's own `nest` fixture is function-scoped so it can't,
    but `tests/test_backups_restore.py::_seed_approved_mda` (session-scoped
    `nest_instance`) hit exactly that.
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


def _ws_client(nest: dict, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.feature("backup-destinations-and-restore")
def test_mail_snapshot_create_restore_and_caldav_divergence(dr_nest):
    """Mail create→restore→history + CalDAV ahead-token divergence.

    Phase A (owner-implicit WS-RPC):
      1. create_message_kind(kind="mail")        → snapshot_id
      2. restore_message_kind(snapshot_id)        → ok (writes restore_history)
      3. list_restore_history                     → one row for the snapshot

    Phase B (CalDAV γ divergence):
      4. owner provisions a calendar (fresh → highestmodseq = 1)
      5. a BridgeMda-class actor calls sync_calendar_since with a
         sync_token far ahead of highestmodseq               → Stale
      6. list_restore_divergence(snapshot_id)     → one caldav row keyed
         to the mail restore, "~98 writes lost"
    """
    owner = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )

    # ── Phase A: mail create → restore → history ──────────────────────
    with _ws_client(dr_nest, owner) as owner_ws:
        create_reply = owner_ws.call(
            "fauna.filesync.snapshot.create_message_kind",
            {"kind": "mail"},
        )
        snapshot_id = create_reply["snapshot_id"]
        assert isinstance(snapshot_id, int) and snapshot_id > 0
        assert create_reply["kind"] == "mail"

        restore_reply = owner_ws.call(
            "fauna.filesync.snapshot.restore_message_kind",
            {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
        )
        assert restore_reply["snapshot_id"] == snapshot_id
        assert restore_reply["kind"] == "mail"
        # No wrapped-MLS blob bundle was provisioned for this fresh actor,
        # so the restore is advisory-incomplete (bridge AUTH would fail
        # until the bundle is restored) — surfaced as config_present=False.
        assert restore_reply["config_present"] is False
        assert restore_reply["note"]  # non-empty warning string

        history = owner_ws.call(
            "fauna.filesync.snapshot.list_restore_history",
            {"limit": 0},
        )
        rows = history["rows"]
        assert len(rows) == 1, f"expected one restore_history row, got {rows!r}"
        assert rows[0]["snapshot_id"] == snapshot_id
        assert rows[0]["kinds_restored"] == "mail"
        assert rows[0]["source_member_id"] is None  # local snapshot

        # ── Phase B (part 1): owner provisions a calendar ─────────────
        calendar_id = secrets.token_bytes(32)
        prov_reply = owner_ws.call(
            "fauna.bridges.provision_calendar",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                # Opaque to nest; just needs to be non-empty.
                "encrypted_metadata": b"{}",
                "update_metadata": False,
            },
        )
        assert prov_reply["outcome"] == "created"

    # ── Phase B (part 2): MDA-class actor drives sync_calendar_since ──
    mda = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )
    _seed_approved_mda(
        dr_nest["db_path"],
        mda["actor_id_bytes"],
        bridge_id=f"e2e-dr-restore-{secrets.token_hex(6)}",
    )

    ahead_token = 99  # >> the fresh calendar's highestmodseq (== 1)
    with _ws_client(dr_nest, mda) as mda_ws:
        sync_reply = mda_ws.call(
            "fauna.bridges.sync_calendar_since",
            {
                "actor_id": owner["actor_id_bytes"],
                "calendar_id": calendar_id,
                "sync_token": str(ahead_token),
                "limit": 0,
                "mua_id": "Fauna-DR-Test/1.0",
            },
        )
        # MUA ahead of server → Stale, MUA falls through to full PROPFIND.
        assert sync_reply["outcome"] == "stale", f"got {sync_reply!r}"
        server_modseq = sync_reply["server_modseq"]
        assert server_modseq == 1, f"fresh calendar highestmodseq, got {server_modseq}"

    # ── Phase B (part 3): owner reads back the divergence row ─────────
    with _ws_client(dr_nest, owner) as owner_ws:
        div = owner_ws.call(
            "fauna.filesync.snapshot.list_restore_divergence",
            {"snapshot_id": snapshot_id},
        )
        drows = div["rows"]
        assert len(drows) == 1, f"expected one divergence row, got {drows!r}"
        row = drows[0]
        assert row["snapshot_id"] == snapshot_id  # keyed to the mail restore
        assert row["protocol"] == "caldav"
        assert row["collection"] == calendar_id.hex()
        assert row["client_modseq"] == ahead_token
        assert row["server_modseq"] == server_modseq
        assert row["lost_event_count"] == ahead_token - server_modseq  # 98
        assert row["mua_id"] == "Fauna-DR-Test/1.0"


@pytest.mark.feature("backup-destinations-and-restore")
def test_restore_message_kind_confirm_mismatch_rejected(dr_nest):
    """A confirm_id that doesn't match the snapshot id is rejected, no-op.

    Pins the friction-bar precondition independently of the happy path so
    a regression there can't hide behind the round-trip assertions.
    """
    owner = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )
    with _ws_client(dr_nest, owner) as owner_ws:
        create_reply = owner_ws.call(
            "fauna.filesync.snapshot.create_message_kind",
            {"kind": "mail"},
        )
        snapshot_id = create_reply["snapshot_id"]

        with pytest.raises(RpcCallError) as excinfo:
            owner_ws.call(
                "fauna.filesync.snapshot.restore_message_kind",
                {"snapshot_id": snapshot_id, "confirm_id": "not-the-id"},
            )
        assert excinfo.value.code == "fauna.filesync.snapshot.confirm_mismatch"

        # The rejected restore wrote no restore_history row.
        history = owner_ws.call(
            "fauna.filesync.snapshot.list_restore_history",
            {"limit": 0},
        )
        assert history["rows"] == []


def test_snapshot_list_returns_owner_message_kind_snapshots(dr_nest):
    """`fauna.filesync.snapshot.list` enumerates the bearer's message-kind
    snapshots so the Backups restore picker has a local snapshot to target.

    Owner-implicit (bearer scopes the query); message-kind-scoped (only
    rows with `message_kind IS NOT NULL`). A second actor's list is empty
    — the picker never leaks another owner's snapshots.
    """
    owner = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )

    with _ws_client(dr_nest, owner) as owner_ws:
        create_reply = owner_ws.call(
            "fauna.filesync.snapshot.create_message_kind",
            {"kind": "mail"},
        )
        snapshot_id = create_reply["snapshot_id"]

        # Unfiltered list: the one mail snapshot shows up.
        listed = owner_ws.call("fauna.filesync.snapshot.list", {"limit": 0})
        rows = listed["rows"]
        assert len(rows) == 1, f"expected one snapshot, got {rows!r}"
        assert rows[0]["id"] == snapshot_id
        assert rows[0]["message_kind"] == "mail"

        # Filter to a kind with no snapshot → empty.
        cal = owner_ws.call(
            "fauna.filesync.snapshot.list", {"message_kind": "calendar", "limit": 0}
        )
        assert cal["rows"] == []

        # Filter to mail → the one row.
        mail = owner_ws.call(
            "fauna.filesync.snapshot.list", {"message_kind": "mail", "limit": 0}
        )
        assert len(mail["rows"]) == 1
        assert mail["rows"][0]["id"] == snapshot_id

    # A different actor sees none of the owner's snapshots.
    other = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )
    with _ws_client(dr_nest, other) as other_ws:
        assert other_ws.call("fauna.filesync.snapshot.list", {"limit": 0})["rows"] == []


def _channel_with_one_message(nest: dict, sender: dict) -> str:
    """A real channel carrying one real `ChannelEnvelope::Application`, with
    `sender` on its roster. Returns the channel id (hex).

    A fake byte blob will not do: `SealedStorage` strict-decodes the body
    (`classify_envelope_shape`), so the envelope is minted from a throwaway
    one-off MLS group exactly as `test_mls_channels.py` does. The send is what
    puts the message in `conv_segments` — without one the channel's manifest
    pins no segment and the snapshot would be of nothing — and it is also what
    auto-registers the sender on the channel's roster, which is the membership
    both `create_conv` and the conv restore gate on.
    """
    import os

    throwaway_kp = conv_api.mint_key_packages(os.urandom(32), 1)[0]
    _, _, envelope = conv_api.mint_group_welcome_with_message(
        bytes(sender["signing_key"]), throwaway_kp, "conversation to be restored"
    )
    channel_hex = secrets.token_bytes(32).hex()
    seq = conv_api.channel_send(nest["port"], sender, channel_hex, envelope)
    assert seq >= 1, f"the seeding send did not land: seq={seq!r}"
    return channel_hex


@pytest.mark.feature("backup-destinations-and-restore")
def test_conv_snapshot_create_restore_and_history(dr_nest):
    """A conversation is snapshotted and restored the way mail is.

    Witnesses `backup-destinations-and-restore` outcome 17
    (``docs/goal/behavior/backup-restore.md`` § 6. Message-kind restore). The
    conv arm of ``create_message_kind`` / ``restore_message_kind`` has been
    served since S6.9 (``create_conv`` / ``restore_conv`` in
    ``filesync_handlers.rs``) and no e2e ever called it with ``kind = "conv"``
    — the mail twin above was the only message-kind round trip in the tree.

    The same three calls as the mail phase, in the same order, which is the
    outcome's own "the way mail is":

      1. ``create_message_kind {kind: "conv", actor_id: <channel>}`` → snapshot
      2. ``restore_message_kind`` → ok
      3. ``list_restore_history`` → one row, ``kinds_restored == "conv"``

    **Conv's authorization is the thing that differs, so it is asserted.** Mail
    is owner-only (bearer == the folder's actor); a conv snapshot's scope key
    IS the channel, so owner-equality is meaningless and the gate is channel
    membership instead. A non-member must be refused at BOTH ends — creating
    and restoring — or "the way mail is" would have silently become "more
    freely than mail".
    """
    member = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )
    channel_hex = _channel_with_one_message(dr_nest, member)
    # `SnapshotCreateMessageKindRequest.actor_id` is typed `Option<ActorId>`,
    # which rides as a 32-byte CBOR byte string — the same shape as its
    # `ByteBuf` siblings on adjacent kinds (`AdminFolderCreateRequest.actor_id`
    # and the rest), so the raw bytes are sent as-is.
    channel_scope = bytes.fromhex(channel_hex)

    with _ws_client(dr_nest, member) as member_ws:
        create_reply = member_ws.call(
            "fauna.filesync.snapshot.create_message_kind",
            {"kind": "conv", "actor_id": channel_scope},
        )
        snapshot_id = create_reply["snapshot_id"]
        assert isinstance(snapshot_id, int) and snapshot_id > 0, create_reply
        assert create_reply["kind"] == "conv", create_reply
        # Scoped to the CHANNEL, one row for it — not to the member's actor,
        # and not the batched all-my-channels shape.
        assert create_reply["snapshot_ids"] == [snapshot_id], create_reply

        restore_reply = member_ws.call(
            "fauna.filesync.snapshot.restore_message_kind",
            {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
        )
        assert restore_reply["snapshot_id"] == snapshot_id, restore_reply
        assert restore_reply["kind"] == "conv", restore_reply
        # No bridge serves conversations, so the mail twin's "wrapped-MLS blob bundle not
        # present, bridge AUTH will fail" advisory is N/A here and must not
        # ride along on a successful conv restore.
        assert restore_reply["config_present"] is True, restore_reply
        assert restore_reply["note"] == "", restore_reply

        history = member_ws.call(
            "fauna.filesync.snapshot.list_restore_history", {"limit": 0}
        )
        rows = history["rows"]
        assert len(rows) == 1, f"expected one restore_history row, got {rows!r}"
        assert rows[0]["snapshot_id"] == snapshot_id, rows[0]
        assert rows[0]["kinds_restored"] == "conv", (
            f"a conv restore must be listed like any other restore: {rows[0]!r}"
        )
        assert rows[0]["source_member_id"] is None, rows[0]  # local snapshot

    # The conversation survived its own restore: the message is still readable.
    messages = conv_api.channel_fetch(dr_nest["port"], member, channel_hex, after=0)
    assert len(messages) >= 1, (
        f"the restore must leave the channel's messages readable, got {messages!r}"
    )

    # ── The membership gate, both ends ────────────────────────────────
    # A plain registered actor who has never touched the channel. Neither
    # `channel_fetch` nor `channel_send` is called for them anywhere — both
    # auto-register the caller on the roster, which would hand them the very
    # membership this arm exists to deny.
    outsider = create_actor_and_register(
        dr_nest["port"], admin_signing_key=dr_nest["admin"]["signing_key"]
    )
    with _ws_client(dr_nest, outsider) as outsider_ws:
        with pytest.raises(RpcCallError) as create_err:
            outsider_ws.call(
                "fauna.filesync.snapshot.create_message_kind",
                {"kind": "conv", "actor_id": channel_scope},
            )
        assert "member" in str(create_err.value).lower(), create_err.value

        with pytest.raises(RpcCallError) as restore_err:
            outsider_ws.call(
                "fauna.filesync.snapshot.restore_message_kind",
                {"snapshot_id": snapshot_id, "confirm_id": str(snapshot_id)},
            )
        assert "member" in str(restore_err.value).lower(), restore_err.value
