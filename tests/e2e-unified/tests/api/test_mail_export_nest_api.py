"""tier_3 — the nest's own promises about a mailbox export, witnessed from outside.

`docs/features/mailbox-export.md` carries `[nest]` outcomes the apps cannot
show: they are things the nest refuses, keeps or forgets, whatever app drives
the export. This file drives the production kinds
(``fauna.bridges.start_export_session`` and its siblings) and the download route
(``GET /api/v1/export/{session_id}``) with no app in the loop, the way the
shared export machine does, and reads the answers back where a client reads
them.

The nest never opens the archive (it is sealed to a key only the user's app
holds, ``mail-export.md`` § Sealed-blob delivery), so a witness here uploads
opaque bytes where the app would upload sealed frames: the sealing is the
client's half and the nest cannot tell the difference.

* **32** — the per-person concurrency cap (``mail-export.md`` § Quota composition).
* **35** — an admin cannot start an export of someone else's mailbox (§ Cross-actor isolation).
* **37** — a suspended or locked-out account cannot download an export (§ Download flow).
"""

from __future__ import annotations

import pytest
import requests

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import mint_token_via_handshake
from conftest import _make_user
from helpers.admin_wire import admin_client
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.succession import emergency_lockout

pytestmark = pytest.mark.tier_3

#: ``mail.export.max_concurrent_per_user`` default (``db/mail_policy.rs``). No
#: admin write path exists for it yet, so the shipped default is what a nest
#: runs and what the outcome names ("three by default").
DEFAULT_CONCURRENT_CAP = 3

CAP_CODE = "fauna.bridges.export_concurrency_cap_reached"


def _member(nest) -> dict:
    """A registered member whose mailbox the nest serves, with a seal key on file."""
    user = _make_user(nest)
    with _ws(nest, user) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])
    return user


def _ws(nest, user) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _start(ws: WsRpcAdminClient, **extra) -> str:
    reply = ws.call(
        "fauna.bridges.start_export_session",
        {
            "format": "mbox",
            "scope_descriptor": b"opaque-scope",
            "wrapped_session_key": b"wrapped-under-the-actor-key",
            "total_count": 1,
            **extra,
        },
    )
    return reply["session_id"]


def _action(ws: WsRpcAdminClient, kind: str, session_id: str) -> dict:
    return ws.call(f"fauna.bridges.{kind}", {"session_id": session_id})["session"]


def _finish(ws: WsRpcAdminClient, session_id: str, payload: bytes = b"sealed-frames") -> dict:
    """Upload one opaque chunk and finalize: a finished export with a blob."""
    ws.call(
        "fauna.bridges.upload_export_chunk",
        {
            "session_id": session_id,
            "chunk_idx": 0,
            "sealed_chunk": payload,
            "exported_delta": 1,
            "skipped_delta": 0,
            "errored_delta": 0,
            "last_processed_message_id": "m0",
        },
    )
    return _action(ws, "finalize_export_session", session_id)


def _download(nest, token: str, session_id: str) -> requests.Response:
    return requests.get(
        f"{nest['url']}/api/v1/export/{session_id}",
        headers={"Authorization": f"Bearer {token}"},
        timeout=30,
    )


def _sessions(ws: WsRpcAdminClient) -> list[dict]:
    return ws.call("fauna.bridges.list_export_sessions", {})["sessions"]


# ── 32: three at once, the fourth refused until one ends ─────────────────────


@pytest.mark.feature("mailbox-export")
def test_only_three_exports_run_at_once_and_another_is_refused_until_one_ends(nest_instance):
    member = _member(nest_instance)
    with _ws(nest_instance, member) as ws:
        running = [_start(ws) for _ in range(DEFAULT_CONCURRENT_CAP)]

        with pytest.raises(RpcCallError) as refused:
            _start(ws)
        assert refused.value.code == CAP_CODE, (
            f"a fourth export while three run must be refused with the cap's own code: "
            f"{refused.value.code!r}"
        )
        assert len(_sessions(ws)) == DEFAULT_CONCURRENT_CAP, (
            "the refused start must not leave a session behind"
        )

        # Paused still holds its slot: the cap counts running AND paused.
        _action(ws, "pause_export_session", running[0])
        with pytest.raises(RpcCallError) as still_full:
            _start(ws)
        assert still_full.value.code == CAP_CODE

        # Cancelled, failed and finished each give the slot back.
        _action(ws, "cancel_export_session", running[0])
        second = _start(ws)

        ws.call(
            "fauna.bridges.fail_export_session",
            {"session_id": running[1], "reason": "the source disappeared"},
        )
        third = _start(ws)

        _finish(ws, running[2])
        fourth = _start(ws)

        assert len({second, third, fourth}) == 3


# ── 35: an admin cannot start an export of someone else's mailbox ────────────


@pytest.mark.feature("mailbox-export")
def test_an_admin_cannot_start_an_export_of_someone_elses_mailbox(nest_instance):
    victim = _member(nest_instance)
    with _ws(nest_instance, victim) as ws:
        own = _start(ws)

    with admin_client(nest_instance) as admin:
        # The admin tries the one way the wire offers to name the victim: a
        # target riding the request. The nest reads the caller from the
        # authenticated connection and the kind takes no target, so whatever the
        # start does, it does for the admin's own mailbox — or is refused (the
        # admin account may not serve mail at all).
        before = {s["session_id"] for s in _sessions(admin)}
        try:
            smuggled = _start(admin, actor_id=victim["actor_id_bytes"])
        except RpcCallError:
            smuggled = None
        after = {s["session_id"] for s in _sessions(admin)}
        assert own not in after, "the victim's export must never appear in the admin's own list"
        assert after - before == ({smuggled} if smuggled else set()), (
            f"the admin's start must add at most one session, the admin's own: {after - before!r}"
        )

        # The victim's own session is not the admin's to see, drive or discard.
        for kind in ("pause_export_session", "cancel_export_session", "finalize_export_session"):
            with pytest.raises(RpcCallError):
                _action(admin, kind, own)
        assert not admin.call("fauna.bridges.discard_export_blob", {"session_id": own})["existed"], (
            "an admin's discard of someone else's export must find nothing to discard"
        )

    with _ws(nest_instance, victim) as ws:
        sessions = _sessions(ws)
        assert [s["session_id"] for s in sessions] == [own], (
            f"the admin's attempts must neither add to nor remove from the victim's exports: {sessions!r}"
        )
        assert smuggled not in {s["session_id"] for s in sessions}


# ── 37: a suspended or locked-out account cannot download ────────────────────
#
# `mail-export.md` § Download flow → *Standing and the owner's notice*: the door
# authenticates through the nest's one bearer validator, which refuses a
# suspended or locked-out actor's surviving bearer, `401` like every
# session-only byte door.


def _suspend(nest, member) -> None:
    with admin_client(nest) as admin:
        admin.call(
            "fauna.admin.users.suspend",
            {"actor_id": member["actor_id_bytes"], "reason": "abuse now", "category": "abuse"},
        )


def _lock_out(nest, member) -> None:
    emergency_lockout(
        nest["url"],
        actor_id_hex=member["actor_id_hex"],
        identity_seed_hex=bytes(member["signing_key"]).hex(),
    )


@pytest.mark.feature("mailbox-export")
@pytest.mark.parametrize("cut_off", [_suspend, _lock_out], ids=["suspended", "locked_out"])
def test_a_suspended_or_locked_out_account_cannot_download_an_export_with_a_sign_in_it_holds(
    nest_instance, cut_off
):
    member = _member(nest_instance)
    with _ws(nest_instance, member) as ws:
        session_id = _start(ws)
        done = _finish(ws, session_id)
    assert done["state"] == "completed", done

    token = mint_token_via_handshake(nest_instance["url"], member["signing_key"])
    before = _download(nest_instance, token, session_id)
    assert before.status_code == 200, (
        f"the bearer must work before the cut-off, or the refusal below proves nothing: "
        f"{before.status_code} {before.text[:200]}"
    )

    cut_off(nest_instance, member)

    after = _download(nest_instance, token, session_id)
    assert after.status_code == 401, (
        f"the sign-in the member already holds must stop opening the download: "
        f"{after.status_code} {after.text[:200]}"
    )
    assert after.content != before.content, "the refusal must not carry the sealed blob"
