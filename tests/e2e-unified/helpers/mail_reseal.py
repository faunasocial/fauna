"""The mail-key rotation journeys' shared reads: the sealed MDA snapshot the
nest holds, a barrier on a connect-time refresh having re-sealed it, and a
normal MUA's IMAPS AUTH over it.

Used by ``test_mail_relaunch_right_after_key_rotation.py`` and
``test_mail_stale_sibling_after_key_rotation.py`` — the witnesses of
``docs/goal/behavior/mail-credentials.md`` § Hard revoke → *Published before
the nest moves*.
"""

from __future__ import annotations

import sqlite3
import time

import pytest

from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.mail_wire import _imap_auth_plain, _imap_cmd, _imaps_connect


def imap_auth_and_select(handle, recipient: str, password: str, when: str) -> None:
    """A normal MUA AUTHs over IMAPS with the surviving credential (the MDA
    unwraps the MSEK from its blob and opens the snapshot under it) and
    SELECTs the inbox."""
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "a1", recipient, password, deadline) == "OK", (
            f"{when}: IMAP AUTH must succeed with the surviving credential — a "
            "failure here is the snapshot sealed under a key the credential's "
            f"blob does not name (see {handle.bridge_log_hint()})"
        )
        status, resp = _imap_cmd(sock, buf, "a2", "SELECT INBOX", deadline)
        assert status == "OK", f"{when}: SELECT INBOX must succeed; got {status}: {resp!r}"
        sock.sendall(b"a99 LOGOUT\r\n")


def wait_for_reseal(nest, before: bytes, when: str) -> bytes:
    """Wait until a connect-time refresh has re-provisioned the MDA snapshot —
    the row's bytes change, since every seal draws a fresh nonce — and answer
    the new bytes."""
    deadline = time.monotonic() + ORCHESTRATION_STEP_S
    while time.monotonic() < deadline:
        now = snapshot_blob(nest)
        if now != before:
            return now
        time.sleep(0.25)
    pytest.fail(
        f"{when}: the mail snapshot was never re-provisioned within "
        f"{ORCHESTRATION_STEP_S}s — the connect-time refresh did not run, or "
        "read a pending rotation the finished one should have cleared"
    )


def snapshot_blob(nest) -> bytes:
    """The sealed MDA snapshot the nest holds — on a dedicated mail nest, the
    one mail-enabled account's."""
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        rows = conn.execute("SELECT blob FROM bridge_mls_snapshot_blobs").fetchall()
    finally:
        conn.close()
    assert len(rows) == 1, f"one mail-enabled account holds one snapshot; got {len(rows)}"
    return bytes(rows[0][0])
