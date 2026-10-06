"""Tier 3: what a user's several mail apps see of each other — folders and
subscriptions shared across credentials, changes pushed to a parallel IDLE
session, and the per-actor mailbox quota — witnessed over the real MDA bridge
and a real nest (`docs/goal/behavior/imap-server.md` § Subscriptions, § Push
wiring, § Quota enforcement points).

Every recipient is fresh (`_provision_msek_recipient`), so no assertion depends
on the session-shared recipient's accumulated mailboxes. Assertions read tagged
completions or unsolicited responses the server writes as a consequence of the
other session's tagged completion — convention 14; the IDLE reads are bounded by
a deadline, never a sleep.
"""

import base64
import re
import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _provision_msek_recipient, _run_seal_helper
from helpers.mail_wire import (
    _imap_append,
    _imap_auth_plain,
    _imap_cmd,
    _imap_read_tagged,
    _imaps_connect,
    _recv_line,
)

pytestmark = pytest.mark.tier_3


def _fresh(mda, nest_instance, seal_helper_binary, tag):
    local = f"ses{tag}{secrets.token_hex(3)}"
    return _provision_msek_recipient(
        nest_instance=nest_instance, seal_helper_binary=seal_helper_binary,
        domain=mda.domain, local_part=local, password=f"{local}-password-1")


def _add_credential(nest_instance, seal_helper_binary, r, credential_id, password):
    """A second mail credential for `r`: the same MSEK sealed under another
    password and credential id, uploaded by the user — what the mail-settings
    page's add-credential does. Its MUA username is `<local>+<id>@<domain>`
    (`mail-credentials.md` § MUA-username convention)."""
    blob = _run_seal_helper(seal_helper_binary, "seal-wrapped-msek", {
        "msek_b64": base64.b64encode(r.msek).decode(),
        "actor_id_b64": base64.b64encode(r.actor_id).decode(),
        "credential_id": credential_id,
        "credential_kind": "plain",
        "credential_b64": base64.b64encode(password.encode()).decode(),
    })
    with WsRpcAdminClient(nest_instance["url"], actor_id=r.actor_id,
                          signing_key=bytes(r.recipient["signing_key"])) as ws:
        ws.call("fauna.bridges.provision_wrapped_mls_blob", {
            "actor_id": r.actor_id, "credential_id": credential_id, "blob": blob,
        })
    local, domain = r.username.split("@", 1)
    return f"{local}+{credential_id}@{domain}"


class _Session:
    def __init__(self, mda, username, password, deadline, prefix="s"):
        self.deadline = deadline
        self.username = username
        self.prefix = prefix
        self.sock, self.buf = _imaps_connect(mda, deadline)
        self.n = 0
        assert _imap_auth_plain(self.sock, self.buf, self.tag(), username, password,
                                deadline) == "OK", f"AUTH PLAIN as {username} must succeed"

    def tag(self):
        self.n += 1
        return f"{self.prefix}{self.n}"

    def cmd(self, cmd):
        return _imap_cmd(self.sock, self.buf, self.tag(), cmd, self.deadline)

    def ok(self, cmd):
        status, text = self.cmd(cmd)
        assert status == "OK", f"{cmd!r} must succeed; got {status}: {text}"
        return text

    def append(self, mailbox, subject):
        msg = (f"From: peer@external.test\r\nTo: {self.username}\r\n"
               f"Subject: {subject}\r\nMessage-ID: <{secrets.token_hex(8)}@mda.fauna.test>\r\n"
               f"\r\n{subject} body\r\n").encode()
        return _imap_append(self.sock, self.buf, self.tag(), mailbox, msg, self.deadline)

    def close(self):
        try:
            self.sock.sendall(b"zz LOGOUT\r\n")
        finally:
            self.sock.close()


def _names(listing):
    out = set()
    for ln in listing.splitlines():
        m = re.match(r'\* (?:LIST|LSUB) \([^)]*\) (?:"[^"]*"|NIL) "?([^"]+?)"?$', ln)
        if m:
            out.add(m.group(1))
    return out


@pytest.mark.feature("standard-mail-apps")
def test_folders_and_subscriptions_are_the_same_in_every_mail_app(
        mail_bridge_mda, nest_instance, seal_helper_binary):
    """A folder one mail app creates and subscribes to is listed, and listed as
    subscribed, by a second mail app signed in with the user's OTHER credential;
    a folder still holding mail is not deleted; another user sees none of it."""
    mda = mail_bridge_mda
    r = _fresh(mda, nest_instance, seal_helper_binary, "sub")
    phone_user = _add_credential(nest_instance, seal_helper_binary, r, "phone",
                                 "phone-password-1")
    stranger = _fresh(mda, nest_instance, seal_helper_binary, "other")
    folder = f"Proj-{secrets.token_hex(3)}"
    deadline = time.monotonic() + 90.0

    desk = _Session(mda, r.username, r.password, deadline, "d")
    try:
        desk.ok(f'CREATE "{folder}"')
        desk.ok(f'SUBSCRIBE "{folder}"')
        status, _ = desk.append(folder, "kept")
        assert status == "OK", f"APPEND into the new folder must succeed; got {status}"
    finally:
        desk.close()

    phone = _Session(mda, phone_user, "phone-password-1", deadline, "p")
    try:
        assert folder in _names(phone.ok('LIST "" "*"')), (
            "a folder created in one mail app must be listed in the other")
        assert folder in _names(phone.ok('LSUB "" "*"')), (
            "a subscription made in one mail app must show in the other — "
            "subscriptions are per user, not per credential")
        status, text = phone.cmd(f'DELETE "{folder}"')
        assert status == "NO", f"a folder that still holds mail must not be deleted; got {status}: {text}"
        assert folder in _names(phone.ok('LIST "" "*"')), "the refused DELETE must keep the folder"
    finally:
        phone.close()

    other = _Session(mda, stranger.username, stranger.password, deadline, "o")
    try:
        assert folder not in _names(other.ok('LIST "" "*"')), "folders are per user"
        assert folder not in _names(other.ok('LSUB "" "*"')), "subscriptions are per user"
    finally:
        other.close()


def _await_untagged(sock, buf, pattern, deadline, what):
    seen = []
    try:
        while time.monotonic() < deadline:
            line = _recv_line(sock, buf, deadline)
            seen.append(line)
            if re.search(pattern, line, re.IGNORECASE):
                return line
    except TimeoutError:
        pass
    raise AssertionError(f"the IDLE session never received {what}; saw {seen!r}")


@pytest.mark.feature("standard-mail-apps")
def test_changes_in_one_mail_app_are_pushed_to_another(mail_bridge_mda, nest_instance,
                                                        seal_helper_binary):
    """With one mail app idling on INBOX, a flag set, a message deleted and a
    message moved away in a second mail app — and a message read in the Fauna
    app — each reach the idling app as an unsolicited update."""
    mda = mail_bridge_mda
    r = _fresh(mda, nest_instance, seal_helper_binary, "push")
    deadline = time.monotonic() + 120.0

    writer = _Session(mda, r.username, r.password, deadline, "w")
    idler = _Session(mda, r.username, r.password, deadline, "i")
    try:
        uids = []
        for subject in ("flag me", "delete me", "move me", "read me in the app"):
            status, uid = writer.append("INBOX", subject)
            assert status == "OK" and uid is not None
            uids.append(uid)
        flag_uid, del_uid, move_uid, app_uid = uids

        idler.ok("SELECT INBOX")
        idle_tag = idler.tag()
        idler.sock.sendall(f"{idle_tag} IDLE\r\n".encode())
        assert _recv_line(idler.sock, idler.buf, deadline).startswith("+"), "IDLE continuation"

        step = lambda: time.monotonic() + 20.0  # noqa: E731
        # Barrier: the idling session's push subscription is live once it has
        # been told about a message the other session added. Without it, a
        # change made before the subscription registered would be missed for a
        # reason that has nothing to do with the push path under test.
        status, _ = writer.append("INBOX", "wake the idler")
        assert status == "OK"
        _await_untagged(idler.sock, idler.buf, r"^\* \d+ EXISTS", step(),
                        "the new-message push that proves the subscription is live")

        writer.ok("SELECT INBOX")
        writer.ok(f"UID STORE {flag_uid} +FLAGS (\\Flagged)")
        _await_untagged(idler.sock, idler.buf, r"^\* \d+ FETCH \(.*\\Flagged", step(),
                        "the flag set in the other mail app")

        writer.ok(f"UID STORE {del_uid} +FLAGS (\\Deleted)")
        writer.ok(f"UID EXPUNGE {del_uid}")
        _await_untagged(idler.sock, idler.buf, r"^\* (\d+ EXPUNGE|VANISHED)", step(),
                        "the deletion made in the other mail app")

        writer.ok(f"UID MOVE {move_uid} Archive")
        _await_untagged(idler.sock, idler.buf, r"^\* (\d+ EXPUNGE|VANISHED)", step(),
                        "the move made in the other mail app")

        with WsRpcAdminClient(nest_instance["url"], actor_id=r.actor_id,
                              signing_key=bytes(r.recipient["signing_key"])) as ws:
            ws.call("fauna.email.inbox.mark_seen", {"uids": [app_uid]})
        _await_untagged(idler.sock, idler.buf, r"^\* \d+ FETCH \(.*\\Seen", step(),
                        "the read made in the Fauna app")

        idler.sock.sendall(b"DONE\r\n")
        status, _ = _imap_read_tagged(idler.sock, idler.buf, idle_tag, deadline)
        assert status == "OK", f"IDLE DONE must complete; got {status}"
    finally:
        idler.close()
        writer.close()


def _admin_ws(nest_instance):
    sk = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(nest_instance["url"], actor_id=bytes(sk.verify_key),
                            signing_key=bytes(sk))


@pytest.mark.feature("standard-mail-apps")
def test_a_full_mailbox_refuses_saves_but_never_deletes(mail_bridge_mda, nest_instance,
                                                         seal_helper_binary):
    """At its message-count ceiling a user's APPEND, COPY and MOVE are refused
    `[OVERQUOTA]`; deleting still works and frees room; another user on the same
    nest, under the same ceiling, is unaffected."""
    mda = mail_bridge_mda
    full = _fresh(mda, nest_instance, seal_helper_binary, "full")
    roomy = _fresh(mda, nest_instance, seal_helper_binary, "roomy")
    deadline = time.monotonic() + 90.0
    s = _Session(mda, full.username, full.password, deadline, "q")
    try:
        status, first = s.append("INBOX", "one")
        assert status == "OK"
        status, second = s.append("INBOX", "two")
        assert status == "OK"
        with _admin_ws(nest_instance) as ws:
            ws.call("fauna.bridges.put_imap_policy", {"message_count_default": 2})
        try:
            status, _ = s.append("INBOX", "three")
            assert status == "NO", f"APPEND past the ceiling must be refused; got {status}"
            s.ok("SELECT INBOX")
            for verb in ("COPY", "MOVE"):
                status, text = s.cmd(f"UID {verb} {first} Archive")
                assert status == "NO" and "OVERQUOTA" in text.upper(), (
                    f"{verb} past the ceiling must answer NO [OVERQUOTA]; got {status}: {text}")

            other = _Session(mda, roomy.username, roomy.password, deadline, "r")
            try:
                status, _ = other.append("INBOX", "someone else's mail")
                assert status == "OK", (
                    "one user's full mailbox must not refuse another user's save")
            finally:
                other.close()

            s.ok(f"UID STORE {second} +FLAGS (\\Deleted)")
            s.ok(f"UID EXPUNGE {second}")
            status, _ = s.append("INBOX", "fits again")
            assert status == "OK", f"deleting must free room for a new save; got {status}"
        finally:
            with _admin_ws(nest_instance) as ws:
                ws.call("fauna.bridges.put_imap_policy", {})
    finally:
        s.close()
