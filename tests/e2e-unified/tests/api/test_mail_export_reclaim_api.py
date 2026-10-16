"""tier_3 — what happens to a finished export when its owner goes away, from outside.

Two `[nest]` outcomes of `docs/features/mailbox-export.md` (``mail-export.md``
§ Reclaim): an export is a row and a blob file under ``<data-dir>/exports/``,
and both must go when the account does.

* **38** — deleting your account deletes your exports with it.
* **39** — after you move to a new identity the exports made under the old one are
  gone, and you export again under the new one.

The file is observed where the person cannot look but a witness can: the blob on
the nest's disk (its bytes are sealed, so only its presence matters) and the
download door's answer to the owner's old bearer.
"""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from common.auth import mint_token_via_handshake
from conftest import _bridge_admin_post, _make_user
from helpers.recipient_seal_key import provision_recipient_seal_key
from helpers.succession import register_recovery_kit, succeed_identity
from tests.api.test_mail_export_nest_api import _download, _finish, _sessions, _start, _ws

pytestmark = pytest.mark.tier_3


def _blob_files(nest) -> set[Path]:
    """Every file under an ``exports`` directory in the nest's data directory."""
    return {
        p
        for p in Path(nest["tmp_dir"]).rglob("*")
        if p.is_file() and "exports" in p.parts[len(Path(nest["tmp_dir"]).parts) :]
    }


def _seal_key(nest, user) -> None:
    with _ws(nest, user) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])


def _finished_export(nest, user) -> tuple[str, set[Path]]:
    """A finished export with a blob. Returns its id and the blob file(s) it added."""
    before = _blob_files(nest)
    with _ws(nest, user) as ws:
        session_id = _start(ws)
        done = _finish(ws, session_id)
    assert done["state"] == "completed", done
    added = _blob_files(nest) - before
    assert added, "precondition: a finished export must have put a blob file on the nest's disk"
    return session_id, added


def _gone(paths: set[Path], what: str) -> None:
    deadline = time.monotonic() + 30.0
    while any(p.exists() for p in paths):
        assert time.monotonic() < deadline, f"{what}: the blob is still on the nest's disk: {paths}"
        time.sleep(0.25)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14)


# ── 38: deleting your account deletes your exports ───────────────────────────


@pytest.mark.feature("mailbox-export")
def test_deleting_your_account_deletes_your_exports_with_it(delete_account_nest):
    nest = delete_account_nest
    user = nest["user"]
    _seal_key(nest, user)
    session_id, blobs = _finished_export(nest, user)
    token = mint_token_via_handshake(nest["url"], user["signing_key"])
    before = _download(nest, token, session_id)
    assert before.status_code == 200, (
        f"the control: the owner's export downloads before the account goes: {before.status_code}"
    )

    with _ws(nest, user) as ws:
        ws.call("fauna.account.delete", {})
    ran = _bridge_admin_post(
        nest["url"], nest["admin"]["token"], "/api/v1/test/pending_actions/run_due", {}
    )
    assert ran.get("executed", 0) >= 1, f"the account deletion never applied: {ran}"

    _gone(blobs, "account deletion")
    after = _download(nest, token, session_id)
    assert after.status_code in (401, 404), (
        f"a deleted account's export must not download: {after.status_code} {after.text[:200]}"
    )
    assert after.content != before.content, "the refusal must not carry the sealed blob"


# ── 39: moving to a new identity burns the old identity's exports ────────────


@pytest.mark.feature("mailbox-export")
def test_after_moving_to_a_new_identity_the_old_exports_are_gone_and_you_export_again(
    nest_instance,
):
    nest = nest_instance
    user = _make_user(nest)
    _seal_key(nest, user)
    session_id, blobs = _finished_export(nest, user)
    old_token = mint_token_via_handshake(nest["url"], user["signing_key"])
    assert _download(nest, old_token, session_id).status_code == 200, "the control: it downloads"

    kit = register_recovery_kit(
        nest["url"],
        actor_id_hex=user["actor_id_hex"],
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )
    successor_key = SigningKey(os.urandom(32))
    succeed_identity(
        nest["url"],
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit,
        successor_seed_hex=bytes(successor_key).hex(),
        old_seed_hex=bytes(user["signing_key"]).hex(),
    )

    _gone(blobs, "identity succession")
    assert _download(nest, old_token, session_id).status_code in (401, 404), (
        "the old identity's export must not download after the move"
    )

    successor = {
        "actor_id_bytes": bytes(successor_key.verify_key),
        "actor_id_hex": bytes(successor_key.verify_key).hex(),
        "signing_key": successor_key,
    }
    with _ws(nest, successor) as ws:
        assert [s["session_id"] for s in _sessions(ws)] == [], (
            "the new identity must not inherit the old identity's exports"
        )
        provision_recipient_seal_key(ws, successor["actor_id_bytes"])
        again = _finish(ws, _start(ws))
    assert again["state"] == "completed", f"you export again under the new identity: {again!r}"
