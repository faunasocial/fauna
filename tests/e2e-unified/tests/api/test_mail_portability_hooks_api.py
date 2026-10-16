"""tier_3 — the export ceilings and the thirty-day forgetting, witnessed from outside.

Four `[nest]` outcomes that no app and no shipped writer can reach at the
product defaults, driven through the nest's ``test-hooks`` routes
(``bins/fauna-nest/src/mail_portability_test_hook.rs``) and then observed only
through the production kinds and the download door:

* ``mailbox-export`` **31** — a single export over the size limit is refused
  (``mail-export.md`` § Quota composition; ``session_blob_oversize``).
* ``mailbox-export`` **33** — one person's exports never hold more than the
  per-person footprint (§ Quota composition; ``export_footprint_exceeded``).
* ``mailbox-export`` **30** — a finished export is kept thirty days, then gone,
  and its link answers not found (§ Expiry).
* ``mailbox-import`` **48** — an import untouched for thirty days is forgotten
  (``mailbox-migration.md`` § Resume).

The policy is nest-wide, so 31 and 33 each run on a nest of their own.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from clients._ws_rpc_core import RpcCallError
from common.auth import mint_token_via_handshake
from conftest import _bridge_admin_post, _make_user, _start_dedicated_nest
from helpers.recipient_seal_key import provision_recipient_seal_key
from tests.api.test_mail_export_nest_api import _action, _download, _finish, _sessions, _start, _ws
from tests.api.test_mail_import_nest_api import IMPORTED, _import, _item, _raw, _session

pytestmark = pytest.mark.tier_3

OVERSIZE = "fauna.bridges.session_blob_oversize"
FOOTPRINT = "fauna.bridges.export_footprint_exceeded"
HOOK = "/api/v1/test/mail_portability"


def _member(nest) -> dict:
    user = _make_user(nest)
    with _ws(nest, user) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])
    return user


def _upload(ws, session_id: str, n: int, idx: int = 0) -> None:
    ws.call(
        "fauna.bridges.upload_export_chunk",
        {
            "session_id": session_id,
            "chunk_idx": idx,
            "sealed_chunk": b"s" * n,
            "exported_delta": 1,
            "skipped_delta": 0,
            "errored_delta": 0,
            "last_processed_message_id": f"m{idx}",
        },
    )


def _set_policy(nest, **policy) -> None:
    _bridge_admin_post(nest["url"], nest["admin"]["token"], f"{HOOK}/export_policy", policy)


def _age(nest, user, **kinds) -> dict:
    return _bridge_admin_post(
        nest["url"],
        nest["admin"]["token"],
        f"{HOOK}/age",
        {"actor_id": user["actor_id_hex"], **kinds},
    )


@pytest.fixture
def policy_nest(request, nest_mode, tmp_path_factory):
    """A nest of this test's own, so a lowered export policy reaches no sibling test."""
    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "export-policy-nest")
    try:
        yield nest
    finally:
        cleanup()


# ── 31: one export over the size limit is refused ────────────────────────────


@pytest.mark.feature("mailbox-export")
def test_an_export_over_the_size_limit_is_refused(policy_nest):
    _set_policy(policy_nest, max_blob_bytes=1000, max_concurrent_per_user=3)
    user = _member(policy_nest)
    with _ws(policy_nest, user) as ws:
        session_id = _start(ws)
        _upload(ws, session_id, 500)  # the control: under the limit it uploads
        with pytest.raises(RpcCallError) as refused:
            _upload(ws, session_id, 800, idx=1)  # 500 + 800 passes the 1000-byte limit
    assert refused.value.code == OVERSIZE, (
        f"an export growing past the size limit must be refused with its own code: {refused.value.code!r}"
    )


# ── 33: one person's exports never hold more than the footprint ──────────────


@pytest.mark.feature("mailbox-export")
def test_one_persons_exports_never_hold_more_than_the_per_person_footprint(policy_nest):
    # Footprint = size limit x concurrent cap = 2000 bytes here.
    _set_policy(policy_nest, max_blob_bytes=1000, max_concurrent_per_user=2)
    user = _member(policy_nest)
    with _ws(policy_nest, user) as ws:
        held = [_start(ws), _start(ws)]
        for session_id in held:
            _upload(ws, session_id, 900)
            assert _action(ws, "finalize_export_session", session_id)["state"] == "completed"
        # Finished exports free their concurrency slot but keep their blob: 1800 held.
        third = _start(ws)  # the control: the person is under the footprint, so a start is allowed
        with pytest.raises(RpcCallError) as refused:
            _upload(ws, third, 900)  # 1800 + 900 passes 2000
        assert refused.value.code == FOOTPRINT, (
            f"a chunk taking the person past the footprint must be refused with its own code: "
            f"{refused.value.code!r}"
        )
        # Giving a finished export up frees its share, and the same chunk then lands.
        discarded = ws.call("fauna.bridges.discard_export_blob", {"session_id": held[0]})
        assert discarded["existed"]
        _upload(ws, third, 900)


# ── 30: a finished export is kept thirty days, then gone ─────────────────────


def _blob_files(nest) -> set[Path]:
    root = Path(nest["tmp_dir"])
    return {p for p in root.rglob("*") if p.is_file() and "exports" in p.parts[len(root.parts) :]}


@pytest.mark.feature("mailbox-export")
def test_a_finished_export_is_kept_thirty_days_then_gone(nest_instance):
    user = _member(nest_instance)
    before = _blob_files(nest_instance)
    with _ws(nest_instance, user) as ws:
        session_id = _start(ws)
        assert _finish(ws, session_id)["state"] == "completed"
    blobs = _blob_files(nest_instance) - before
    assert blobs, "precondition: a finished export puts a blob on the nest's disk"
    token = mint_token_via_handshake(nest_instance["url"], user["signing_key"])
    assert _download(nest_instance, token, session_id).status_code == 200, (
        "the control: a fresh export downloads"
    )

    aged = _age(nest_instance, user, exports=True)
    assert aged["aged_exports"] >= 1, aged

    deadline = time.monotonic() + 30.0
    while any(p.exists() for p in blobs):
        assert time.monotonic() < deadline, f"the expired export's blob is still on disk: {blobs}"
        time.sleep(0.25)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14)
    assert _download(nest_instance, token, session_id).status_code == 404, (
        "an expired export's link must answer not found"
    )
    with _ws(nest_instance, user) as ws:
        assert session_id not in [s["session_id"] for s in _sessions(ws)]


# ── import 48: an import untouched for thirty days is forgotten ──────────────


@pytest.mark.feature("mailbox-import")
def test_an_import_untouched_for_thirty_days_is_forgotten(nest_instance):
    user = _member(nest_instance)
    mid = "forgotten-after-thirty-days@example.org"
    with _ws(nest_instance, user) as ws:
        sid = _session(ws)
        assert _import(ws, sid, _item(_raw(mid), mid)) == IMPORTED
        listed = ws.call("fauna.bridges.list_import_sessions", {})["sessions"]
        assert sid in [s["session_id"] for s in listed], "the control: a fresh import is remembered"

        aged = _age(nest_instance, user, imports=True)
        assert aged["aged_imports"] >= 1, aged

        listed = ws.call("fauna.bridges.list_import_sessions", {})["sessions"]
        assert sid not in [s["session_id"] for s in listed], (
            f"an import untouched for thirty days must be forgotten: {listed!r}"
        )

        # Running the wizard again starts a fresh session and skips what already arrived.
        again = _session(ws)
        assert again != sid
        assert _import(ws, again, _item(_raw(mid), mid)) == "skipped:dedup", (
            "the mail the forgotten import brought in must still be recognised as already here"
        )
