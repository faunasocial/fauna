"""What a mail-import witness reads OUTSIDE the client to prove a claim the
client makes — shared by the app journeys (`tests/test_mail_import.py`) and the
app-free witnesses (`tests/api/test_mail_import_client_api.py`), so one
outcome's evidence is read one way whichever surface drove the import.

Owner of the claims: `docs/goal/behavior/mailbox-migration.md`.
"""

from __future__ import annotations

import re
import sqlite3
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from helpers.mail_wire import (
    _imap_auth_plain,
    _imap_read_tagged,
    _imaps_connect,
    _recv_exact,
    _recv_line,
)


def assert_never_reached_the_nest(nest: dict, secret: bytes, what: str) -> None:
    """``secret``'s bytes are nowhere the nest keeps anything — every file
    under its data directory (the database and its write-ahead log included)
    and its log. ``what`` names the secret in the failure."""
    assert len(secret) >= 16, f"a short {what} could match by accident"
    files = [p for p in Path(nest["tmp_dir"]).rglob("*") if p.is_file()]
    log = nest.get("log_path")
    if log:
        files.append(Path(log))
    holding = []
    for path in files:
        try:
            if secret in path.read_bytes():
                holding.append(str(path))
        except FileNotFoundError:
            continue  # a `-shm`/`-wal` side file SQLite removed mid-scan
    assert not holding, f"the {what} is in {holding}"


def assert_the_password_never_reached_the_nest(nest: dict, source) -> None:
    """Outcome 14 (`mailbox-migration.md` § Credential handling): after an
    import that signed in to ``source`` (a `fakes.fake_imap_source.FakeImapSource`)
    with ``source.password``, its bytes are nowhere the nest keeps anything
    (`assert_never_reached_the_nest`), and the session row names the source by
    provider, user and host only."""
    assert_never_reached_the_nest(nest, source.password.encode(), "old provider's password")

    db = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        descriptors = [r[0] for r in db.execute("SELECT source_descriptor FROM import_sessions")]
    finally:
        db.close()
    assert descriptors == [f"imap:{source.username}@localhost"], (
        f"the session row describes the source as {descriptors}"
    )


def import_session_counts(nest: dict) -> list[tuple[str, int, int, int, int]]:
    """Every import session the nest holds, as ``(state, total, imported,
    skipped, errored)`` — the nest's own record of what an import brought in
    (`mailbox-migration.md` § Progress lives nest-side), read straight off its
    database rather than through any app's rendering of it."""
    db = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        return [
            tuple(r)
            for r in db.execute(
                "SELECT state, total_count, imported_count, skipped_count, errored_count "
                "FROM import_sessions"
            )
        ]
    finally:
        db.close()


def message_allowance(nest: dict) -> int | None:
    """The per-user message allowance the admin saved (`admin-mail`'s IMAP
    group, the `mail_imap_policy` overrides row), or ``None`` while the
    catalog default stands — read off the nest's database, so a test can tell
    the admin's Save landed before it relies on the allowance."""
    import json

    db = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        row = db.execute("SELECT overrides_json FROM mail_imap_policy WHERE id = 1").fetchone()
    finally:
        db.close()
    return json.loads(row[0]).get("message_count_default") if row else None


def stored_message_count(nest: dict) -> int:
    """How many distinct messages the nest holds across its users' mailboxes:
    every message it stores, however it arrived, records its dedup key once
    (`mailbox-migration.md` § Dedup vs. existing mail — first writer wins), so a
    message stored twice still counts once here and a message never stored is
    missing. Read before and after an import, the difference is what arrived."""
    db = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        return db.execute("SELECT COUNT(*) FROM actor_message_dedup").fetchone()[0]
    finally:
        db.close()


@dataclass(frozen=True)
class ImapMessage:
    """One message as a standard mail app sees it: its subject, its flags
    (``\\Recent`` left out — a server's own session mark, never the source's)
    and its INTERNALDATE as an aware datetime."""

    subject: str
    flags: frozenset[str]
    internal_date: datetime


def imap_internaldate(value: str) -> datetime:
    """An RFC 3501 ``date-time`` (``01-Mar-2024 09:00:00 +0000``) as an aware
    datetime, so two spellings of one instant compare equal."""
    return datetime.strptime(value.strip().strip('"'), "%d-%b-%Y %H:%M:%S %z")


_LIST_LINE = re.compile(r'^\* LIST \([^)]*\) (?:"[^"]*"|NIL) (?P<name>"(?:[^"\\]|\\.)*"|\S+)$')
_FETCH_FLAGS = re.compile(r"FLAGS \(([^)]*)\)")
_FETCH_DATE = re.compile(r'INTERNALDATE "([^"]+)"')
_LITERAL = re.compile(r"\{(\d+)\}$")


def mailboxes_over_imap(handle, address: str, password: str, *, timeout: float = 60.0
                        ) -> dict[str, list[ImapMessage]]:
    """Every mailbox ``address`` has on the nest behind ``handle`` (a
    `dedicated_mail_nest` handle) and every message in each, read as a standard
    mail app reads them: an IMAP session against the nest's own mail bridge —
    ``LIST``, then ``EXAMINE`` and one read-only ``UID FETCH`` per mailbox.

    The outside reader for what an import landed (`mailbox-migration.md`
    § Goal: mailboxes, dates and flags kept; § Wizard steps step 3: a source
    mailbox matching no standard one lands under its own name). It reads the
    nest's answer to a mail app, never the importing app's account of it."""
    deadline = time.monotonic() + timeout
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status = _imap_auth_plain(sock, buf, "r1", address, password, deadline)
        assert status == "OK", f"IMAP sign-in as {address} was refused: {status}"
        sock.sendall(b'r2 LIST "" "*"\r\n')
        status, untagged = _imap_read_tagged(sock, buf, "r2", deadline)
        assert status == "OK", f"LIST failed: {status} {untagged!r}"
        names = []
        for line in untagged:
            m = _LIST_LINE.match(line)
            if m:
                name = m.group("name")
                names.append(name[1:-1].replace('\\"', '"') if name.startswith('"') else name)
        mailboxes: dict[str, list[ImapMessage]] = {}
        for i, name in enumerate(names):
            tag = f"e{i}"
            sock.sendall(f'{tag} EXAMINE "{name}"\r\n'.encode())
            status, untagged = _imap_read_tagged(sock, buf, tag, deadline)
            assert status == "OK", f"EXAMINE {name} failed: {status} {untagged!r}"
            exists = next(int(ln.split()[1]) for ln in untagged if ln.upper().endswith(" EXISTS"))
            mailboxes[name] = _fetch_examined(sock, buf, f"f{i}", deadline) if exists else []
        sock.sendall(b"r9 LOGOUT\r\n")
    return mailboxes


def _fetch_examined(sock, buf, tag: str, deadline: float) -> list[ImapMessage]:
    """``UID FETCH 1:*`` of the examined mailbox's flags, dates and subjects.
    Each answer is a line ending in the subject's literal, the literal, and the
    line that closes it; the flags and date may sit on either line."""
    sock.sendall(
        f"{tag} UID FETCH 1:* (FLAGS INTERNALDATE BODY.PEEK[HEADER.FIELDS (SUBJECT)])\r\n".encode()
    )
    messages = []
    while True:
        line = _recv_line(sock, buf, deadline)
        if line.startswith(tag + " "):
            assert line.split(" ", 2)[1] == "OK", f"UID FETCH failed: {line!r}"
            return messages
        text, header = line, b""
        m = _LITERAL.search(line)
        if m:
            header = _recv_exact(sock, buf, int(m.group(1)), deadline)
            text += " " + _recv_line(sock, buf, deadline)
        flags = _FETCH_FLAGS.search(text)
        date = _FETCH_DATE.search(text)
        assert flags and date, f"a FETCH answer without FLAGS and INTERNALDATE: {text!r}"
        subject = header.decode("utf-8", "replace").strip()
        if ":" in subject:
            subject = subject.split(":", 1)[1].strip()
        messages.append(ImapMessage(
            subject=subject,
            flags=frozenset(f for f in flags.group(1).split() if f != "\\Recent"),
            internal_date=imap_internaldate(date.group(1)),
        ))
