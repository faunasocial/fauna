"""tier_3: mail received before FOUR rotations of the mail keys is still
readable afterwards — in the app and through a normal IMAP mail app.

The witness for ``docs/goal/architecture/owner-key-material.md`` § Path
B-sibling-2 → *Pre-rotation mail at rest* (ruled 2026-10-06): *the owner keeps
every MSEK generation ever retired, and every holder of the standing set holds
the complete one.* Before the ruling the custody kept a window of two retired
generations and the MDA snapshot three keypairs, so the third rotation lost
every record sealed before the first — on every app and every MUA, for good.
The shared Rust is pinned in unit tests (the plane's generation rows, the
uncapped snapshot, the seal-time trial order); this is the journey through the
two holders of the standing set:

  mail on (PLAIN credential) → email A arrives, sealed to the first key
  generation, and shows decrypted in Conversations → the user rotates the mail
  keys FOUR times (Settings → Mail → Rotate mail keys → confirm, each one
  observed to finish and to republish the recipient key) → **the app is
  restarted** → A still shows decrypted and the page carries no
  unopenable-mail notice → a normal MUA authenticates over IMAPS with the
  surviving credential and FETCHes A's body.

**Why four.** A is then the oldest of five generations — one past the old
three-keypair snapshot and two past the old two-prior custody window, so the
retired cap fails BOTH legs, not just one.

**Why the restart is load-bearing.** Within one process A is already past the
receive cursor and on screen; a fresh process re-drains the mailbox from UID 0
under the keys the account holds *now* — the only moment the client's standing
set is exercised (``test_mail_client_receive_after_rotation.py``, the
one-rotation sibling, restarts for the same reason).

**Why the IMAP leg.** The MDA opens with the snapshot the last rotation
provisioned, not with the client's custody: the same-set invariant says what
one holder opens the other can too, so the witness reads A through both.

**Non-vacuous.** Each rotation must change the recipient key the nest seals
inbound mail to (``actor_mls_pubkeys``) — four distinct republished keys, so
A really is four generations back.

tier_3: every binary real, the real SMTP and IMAPS wires, a real process
restart (``hard_reload()``, replaying the login).
"""

from __future__ import annotations

import sqlite3
import time
import uuid

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, ORCHESTRATION_STEP_S
from helpers.mail_client_ui import (
    deliver_inbound,
    plain_message,
    wait_for_thread_with_nonce,
)
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_wire import (
    _imap_auth_plain,
    _imap_cmd,
    _imap_seq_fetch_body,
    _imaps_connect,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.real_conversations,
]

SENDER = "sender@external.test"
ROTATIONS = 4
_MUA_PASSWORD = "four-rotations-plain-pw-1"


@pytest.mark.feature("email-in-conversations")
def test_mail_received_before_four_key_rotations_stays_readable_in_the_app_and_over_imap(
    app, dedicated_mail_nest, request
):
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # ── 1. Mail on (PLAIN), an address routed to this account, the gates
    # rebound (the binaries e2e has no supervisor — see the MUA round-trip).
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    recipient = alias_admin_to_address(nest, domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "enabling mail must flip the page to enabled; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()

    # ── 2. Email A arrives under the first key generation and shows.
    nonce_a = f"fourrot{uuid.uuid4().hex[:10]}qx"
    deliver_inbound(
        handle.mx_port, domain, SENDER, recipient,
        plain_message(SENDER, recipient, nonce_a), time.monotonic() + 40.0,
    )
    app.conversations.navigate()
    wait_for_thread_with_nonce(
        app, nonce_a, what="before any rotation", budget_s=MLS_HANDSHAKE_S,
    )

    # ── 3. Four rotations, each observed to finish and to republish the key.
    seen_keys = {_recipient_keys(nest)}
    for n in range(1, ROTATIONS + 1):
        app.mail_settings.navigate()
        app.mail_settings.rotate_keys()
        assert app.mail_settings.wait_for_rotation_to_finish(ORCHESTRATION_STEP_S), (
            f"rotation {n} never finished: the rotate form is still open with "
            f"progress {app.driver.get_text('mail-rotate-keys-progress-indicator')!r}"
        )
        page_error = (
            app.driver.get_text("error-message") if app.driver.is_visible("error-message") else ""
        )
        assert not page_error, f"rotation {n} reported an error: {page_error!r}"
        assert app.mail_settings.wait_for_enabled_status(timeout=ORCHESTRATION_STEP_S), (
            f"rotation {n} must settle to up to date before the next; "
            f"status={app.mail_settings.status_text()!r}"
        )
        keys = _recipient_keys(nest)
        assert keys and keys not in seen_keys, (
            f"rotation {n} must republish the recipient key inbound mail is sealed "
            "to; it is one already seen, so A would not be four generations back"
        )
        seen_keys.add(keys)

    # ── 4. The MDA reads A through the snapshot the last rotation provisioned.
    _imap_fetch_a(handle, recipient, nonce_a, "before the relaunch")

    # ── 5. Restart: a fresh receive loop re-drains the mailbox under the keys
    # the account holds now — A is sealed to the oldest of five generations.
    app.driver.hard_reload()
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    app.conversations.navigate()
    found = wait_for_thread_with_nonce(
        app, nonce_a, what=f"after {ROTATIONS} rotations + restart",
        budget_s=MLS_HANDSHAKE_S, bridge_log=handle.bridge_log_hint(),
    )
    assert found.rail == "Smtp", f"received mail must land on the Smtp rail; got {found.rail!r}"
    notice = app.error_text() or ""
    assert "could not be opened" not in notice, (
        "no received record may be unopenable after a rotation — every "
        f"generation is carried; the conversations page says {notice!r}"
    )

    # ── 6. ...and again after the relaunch: the snapshot the relaunched app
    # re-provisions must still carry every generation, sealed under the MSEK
    # the credential unwraps.
    _imap_fetch_a(handle, recipient, nonce_a, "after the relaunch")


def _imap_fetch_a(handle, recipient: str, nonce_a: str, when: str) -> None:
    """The MDA holds the same complete set: a normal MUA AUTHs with the
    surviving credential (re-wrapped at every rotation) and FETCHes A."""
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        assert _imap_auth_plain(sock, buf, "c1", recipient, _MUA_PASSWORD, deadline) == "OK", (
            f"{when}: IMAP AUTH must succeed with the surviving credential after "
            f"{ROTATIONS} rotations (see {handle.bridge_log_hint()})"
        )
        status, resp = _imap_cmd(sock, buf, "c2", "SELECT INBOX", deadline)
        assert status == "OK", f"{when}: SELECT INBOX must succeed; got {status}: {resp!r}"
        fetch_status, fetched = _imap_seq_fetch_body(sock, buf, "c3", 1, deadline)
        assert fetch_status == "OK" and fetched is not None, (
            f"{when}: FETCH 1 BODY[] must open A, sealed {ROTATIONS} rotations ago; "
            f"got {fetch_status} (see {handle.bridge_log_hint()})"
        )
        assert nonce_a.encode() in fetched, (
            f"{when}: the MDA must decrypt A's body under the oldest generation it "
            f"carries; got {fetched[:200]!r}"
        )
        sock.sendall(b"c99 LOGOUT\r\n")


def _recipient_keys(nest) -> frozenset[bytes]:
    """Every recipient key the nest seals inbound mail to — on this dedicated
    nest, the one mail-enabled account's."""
    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        rows = conn.execute("SELECT mls_pubkey FROM actor_mls_pubkeys").fetchall()
    finally:
        conn.close()
    return frozenset(r[0] for r in rows)
