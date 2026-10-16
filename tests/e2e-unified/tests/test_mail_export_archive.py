"""E2E coverage for what the mail-export archive itself promises: its name, its
determinism, what each message keeps, how mailbox names land on the user's
disk, the owner's notice for every download, and that an export made before a
key rotation still opens after it.

Target state: `docs/goal/behavior/mail-export.md` § Goal (the byte-identical
bar), § Wizard steps (what each format preserves), § Format choices,
§ Mailbox names in archive paths — one injective encoding, § Archive paths on
the extracting filesystem — the escape ladder, § Compression wrapper (the
saved name), § Container shape (the determinism contract), § Key material (c)
(the standing key set), § Download flow (the `MailboxExportDownloaded`
notice). Catalog: `docs/features/mailbox-export.md`.

**Why the mailbox is filled by an import.** These promises are about a
mailbox shaped like a real one — custom mailboxes whose names differ only in
case, a name Windows reserves for a device, a name made of `..`, messages
carrying the read / flagged / answered / draft marks and arrival dates a user's
old provider gave them. A user's mailbox gets exactly that shape by moving
their mail in from elsewhere, so the journey does what the user does: the
import wizard reads it off a foreign IMAP server (`fakes/fake_imap_source.py`,
the one participant that is genuinely not ours), and the export wizard hands it
back. Both wizards are driven through the app UI (convention 8); the archive
read is the bytes the app saved into its download dir, never a nest API
standing in for the download.

**Every app that drives both wizards carries the marker** — the same five
`test_mail_import.py` runs on (its module docstring and
`mailbox-migration.md` § Implementation status today say why web and android
are not among them). The assertions are the shared machine's, so they hold or fail alike
everywhere.

Test taxonomy:
- `tier_3` (mocking depth): real app, real shared-Rust IMAP client over real
  TLS, real nest sealing every imported message, the real client drive loop
  sealing the archive's chunks onto real nest disk, the real download door.
"""

from __future__ import annotations

import calendar
import datetime
import json
import os
import re
import sys
import time
from pathlib import Path

import pytest

from actions.mail_export import MailExportActions
from actions.mail_import import MailImportActions
from helpers import budgets
from helpers.mail_dedicated_nest import dedicated_node_url, login_as_nest_admin
from helpers.mail_export_ui import archive_members, download_listing, saved_archives
from helpers.mail_import_ui import enable_mail_so_the_nest_can_seal, fill_generic_source
from helpers.waiting import wait_until
from i18n.strings import S

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_imap_source import FakeImapSource, SourceMailbox, session_cert  # noqa: E402

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # Seeds the source-IMAP trust anchor into every app launch, so the import
    # wizard can verify the certificate the fake source serves.
    pytest.mark.mail_import_source,
]


# ── The source mailbox ───────────────────────────────────────────────────────
#
# One message per mailbox, so "this mailbox's file holds this message, with
# these marks, on this date" is one unambiguous reading per format. Every
# INTERNALDATE lands on an even second (a zip entry's DOS time keeps two-second
# resolution) and on a two-digit day.

# (mailbox, marks, INTERNALDATE). `Work` and `work` differ only in case;
# `Aux` is a Windows device name; `../../escape` is a hierarchy of `..` steps.
_SOURCE = (
    ("INBOX", ("\\Seen", "\\Flagged"), "14-Mar-2021 05:06:08 +0000"),
    ("Work", ("\\Answered",), "15-Apr-2021 06:07:10 +0000"),
    ("work", ("\\Draft",), "16-May-2021 07:08:12 +0000"),
    ("Aux", (), "17-Jun-2021 08:09:14 +0000"),
    ("../../escape", ("\\Seen", "\\Answered", "\\Flagged"), "18-Jul-2021 09:10:16 +0000"),
)

_SENDER = "someone@example.com"


def _subject(mailbox: str) -> str:
    return f"exported from {mailbox} {_RUN_TAG}"


# Distinguishes this run's messages from anything else the archive could hold.
_RUN_TAG = f"xarc{int(time.time())}"


def _message(mailbox: str) -> bytes:
    slug = re.sub(r"[^a-z0-9]+", "-", mailbox.lower()).strip("-") or "m"
    return (
        f"From: {_SENDER}\r\n"
        f"To: user@example.com\r\n"
        f"Subject: {_subject(mailbox)}\r\n"
        f"Message-ID: <{slug}-{len(mailbox)}-{_RUN_TAG}@example.com>\r\n"
        f"Date: Wed, 01 Jan 2020 00:00:00 +0000\r\n"
        f"\r\n"
        f"the body of the message filed in {mailbox}\r\n"
    ).encode()


def _source_mailboxes() -> list[SourceMailbox]:
    return [
        SourceMailbox(
            name=name,
            messages=[_message(name)],
            flags=[flags],
            internal_dates=[date],
        )
        for name, flags, date in _SOURCE
    ]


def _epoch(imap_date: str) -> int:
    """An RFC 3501 `date-time` (all of ours are `+0000`) as epoch seconds."""
    return calendar.timegm(time.strptime(imap_date, "%d-%b-%Y %H:%M:%S +0000"))


@pytest.fixture
def imap_source():
    with FakeImapSource(_source_mailboxes(), cert=session_cert()) as source:
        yield source


# ── Driving the two wizards ──────────────────────────────────────────────────


def _import_everything(app, handle, request, source) -> None:
    """Sign in as the dedicated nest's admin, enable mail, and move every
    source mailbox in through the import wizard — the user's own gestures."""
    login_as_nest_admin(app, handle.nest, dedicated_node_url(app, handle, request))
    enable_mail_so_the_nest_can_seal(app, handle)

    wizard = MailImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()
    fill_generic_source(wizard, source, password=source.password)
    wizard.connect()
    names = wait_until(
        lambda: wizard.mailbox_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the import's Scope step never listed the source "
        f"mailboxes; error was {wizard.error_text()!r}",
    )
    assert sorted(names) == sorted(n for n, _f, _d in _SOURCE), names
    wizard.scope_next()
    wizard.start()
    done = wait_until(
        lambda: wizard.done_summary() or None,
        budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: "the import never reached Done; progress was "
        f"{wizard.progress_summary()!r}, errors {wizard.error_log()!r}",
    )
    assert f"{len(_SOURCE)} imported" in done, (
        f"the import must bring every source message in before the export can "
        f"be checked against them; Done reads {done!r}"
    )


def _export_to_done(app, format_label: str, *, leave_out: tuple[str, ...] = ()) -> MailExportActions:
    """Walk the export wizard in ``format_label`` with every imported mailbox
    in scope but ``leave_out``, which the user deselects, to its Done step."""
    export = MailExportActions(app.driver)
    export.navigate()
    assert export.is_page_visible(), (
        "the export wizard did not open on its Format step; "
        f"error-message: {export.error_text()!r}"
    )
    export.select_format(format_label)
    export.next()  # Format → Scope
    offered = wait_until(
        lambda: export.mailbox_names() or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the Scope step listed no mailboxes; error {export.error_text()!r}",
    )
    for name, _flags, _date in _SOURCE:
        if name in leave_out:
            continue
        assert name in offered and export.mailbox_selected(name), (
            f"the imported mailbox {name!r} must be offered and selected "
            f"(§ Wizard steps step 2: custom mailboxes pre-selected); offered: {offered}"
        )
    # A repeat pass may find the user's last deselection kept: deselect only
    # what is still selected, so the scope is the same either way.
    for name in leave_out:
        if not export.mailbox_selected(name):
            continue
        export.toggle_mailbox(name)
        wait_until(
            lambda: (not export.mailbox_selected(name)) or None,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"deselecting {name!r} on the Scope step did not take",
        )
    export.next()  # Scope → Confirm
    export.start()
    wait_until(
        lambda: app.driver.is_visible("mail-export-done-summary") or None,
        budgets.MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the {format_label} export never reached Done; "
            f"error-message: {export.error_text()!r}"
        ),
    )
    return export


def _download(app, export: MailExportActions) -> tuple[str, bytes]:
    """Press Download and return ``(saved file name, its bytes)``, then take the
    file out of the download dir so the next download — which may carry the
    same name — is unambiguously a new one."""
    assert saved_archives(app.driver) == [], (
        f"the download dir already holds an archive: {download_listing(app.driver)}"
    )
    export.download()
    path = wait_until(
        lambda: next(iter(saved_archives(app.driver)), None),
        budgets.MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "no export archive appeared in the download dir.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  dir now: {download_listing(app.driver)}"
        ),
    )
    with open(path, "rb") as fh:
        data = fh.read()
    os.remove(path)
    return os.path.basename(path), data


def _discard(app, export: MailExportActions) -> None:
    """Discard the finished export through the Done step's own button, so the
    next pass of the wizard opens on Format (it adopts the newest export not
    yet discarded — § Resume)."""
    export.discard()
    wait_until(
        lambda: (not app.driver.is_visible("mail-export-done-summary")) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"Discard did not leave the Done step; error {export.error_text()!r}",
    )


def _open(name: str, data: bytes, tmp_path) -> list:
    path = os.path.join(str(tmp_path), name)
    with open(path, "wb") as fh:
        fh.write(data)
    return archive_members(path, tmp_path)


# ── Reading archive paths the way an extractor does ──────────────────────────

_WINDOWS_DEVICES = (
    {"con", "prn", "aux", "nul", "conin$", "conout$"}
    | {f"{p}{d}" for p in ("com", "lpt") for d in "0123456789¹²³"}
)


def _decode_component(component: str) -> str:
    """§ Mailbox names in archive paths' decoding: `.` is a folded `/`, `%XX`
    is that byte, everything else is itself — context-free on every rung."""
    out = bytearray()
    i = 0
    while i < len(component):
        ch = component[i]
        if ch == "%" and re.fullmatch(r"[0-9A-F]{2}", component[i + 1 : i + 3]):
            out.append(int(component[i + 1 : i + 3], 16))
            i += 3
            continue
        out += b"/" if ch == "." else ch.encode()
        i += 1
    return out.decode()


def _assert_extracts_safely(names: list[str], *, windows_safe: bool) -> None:
    """What the user's unpacking does with these entry names: every one stays
    inside the folder it is unpacked into (outcome 27), and no two land on one
    file on a case-insensitive filesystem or on a Windows device (outcome 26).

    Read off the names themselves rather than by unpacking: the standard
    library's own extractor silently strips `..` and drive letters, so a test
    that unpacked with it could never see the escape it exists to catch.
    """
    for name in names:
        parts = name.rstrip("/").split("/")
        assert not name.startswith("/") and "\\" not in name and parts, (
            f"archive entry {name!r} is absolute or carries a backslash separator"
        )
        for part in parts:
            assert part not in ("", ".", ".."), (
                f"archive entry {name!r} has a {part!r} component — unpacking it "
                "writes outside the folder the user unpacks into"
            )
            if windows_safe:
                assert not set(part) & set(':*?"<>|'), (
                    f"archive entry {name!r} holds a character Windows refuses "
                    f"or reads as a stream in {part!r}"
                )
            stem = part.split(".", 1)[0].rstrip(" ").casefold()
            assert stem not in _WINDOWS_DEVICES, (
                f"archive entry {name!r}: component {part!r} opens as the Windows "
                f"device {stem.upper()!r} instead of a folder or file"
            )
    folded: dict[str, str] = {}
    for name in names:
        key = name.rstrip("/").casefold()
        assert key not in folded, (
            f"archive entries {folded[key]!r} and {name!r} are one file on a "
            "case-insensitive filesystem — one mailbox would be written over the other"
        )
        folded[key] = name


# ── The archive's own promises, format by format ─────────────────────────────

# (wizard label, wire name) — § Format choices; the wire name is what the saved
# file's name and the owner's notice both carry.
_FORMATS = (
    (S.mail_export.format_mbox, "mbox"),
    (S.mail_export.format_maildir, "maildir"),
    (S.mail_export.format_eml, "eml-zip"),
)

_MBOX_STATUS = {"\\Seen": "R"}
_MBOX_X_STATUS = (("\\Answered", "A"), ("\\Flagged", "F"), ("\\Draft", "T"))
_MAILDIR_LETTER = {"\\Seen": "S", "\\Answered": "R", "\\Flagged": "F", "\\Draft": "D"}


def _assert_file_name(name: str, wire: str, day_window: set[str], handle, nest) -> None:
    """Outcome 23: `fauna-export-<handle>-<format>-<iso-date>.zip.zst`, the
    handle being this user's, the format the one chosen, the date the day of
    the download (UTC — the name is the saving client's, § Compression wrapper)."""
    from common.auth import _authed_call

    m = re.fullmatch(
        rf"fauna-export-(?P<handle>.+)-{re.escape(wire)}-(?P<date>\d{{4}}-\d{{2}}-\d{{2}})\.zip\.zst",
        name,
    )
    assert m, f"the saved archive's name {name!r} does not say format {wire!r} and a date"
    assert m.group("date") in day_window, (
        f"the saved archive's date {m.group('date')!r} is not the day it was "
        f"downloaded ({sorted(day_window)})"
    )
    owner = _authed_call(
        nest["url"], nest["admin"]["signing_key"], "fauna.actor.by_handle",
        {"handle": _decode_component(m.group("handle"))},
    )
    owner_id = owner.get("actor_id")
    if isinstance(owner_id, (bytes, bytearray)):
        owner_id = bytes(owner_id).hex()
    assert str(owner_id).lower() == bytes(nest["admin"]["signing_key"].verify_key).hex(), (
        f"the handle in {name!r} names {owner!r}, not the user whose mailbox it is"
    )


def _assert_mbox(members, root: str) -> None:
    files = {info.filename: data for info, data in members if not info.is_dir()}
    infos = {info.filename: info for info, _ in members}
    for mailbox, flags, date in _SOURCE:
        hits = [
            n for n in files
            if n.endswith(".mbox") and n.startswith(f"{root}/")
            and _decode_component(n[len(root) + 1 : -len(".mbox")]) == mailbox
        ]
        assert len(hits) == 1, (
            f"mailbox {mailbox!r} must be exactly one `.mbox` file in the archive "
            f"that decodes back to its name; files: {sorted(files)}"
        )
        text = files[hits[0]].decode()
        epoch = _epoch(date)
        # The mailbox may hold more than our one message — every download
        # seals the owner a security notice into INBOX — so read the message
        # whose header carries our subject: its `From ` separator and stanza.
        separated = re.split(r"(?m)^(?=From )", text)
        ours = [m for m in separated if f"Subject: {_subject(mailbox)}" in m]
        assert len(ours) == 1, (
            f"{hits[0]}: the message filed in {mailbox!r} must be in its file "
            f"exactly once; the file holds {len(separated)} messages"
        )
        lines = ours[0].split("\n")
        assert lines[0] == f"From {_SENDER} {time.asctime(time.gmtime(epoch))}", (
            f"{hits[0]}: the `From ` line must carry the arrival date "
            f"{date!r}; it reads {lines[0]!r}"
        )
        status = "".join(v for k, v in _MBOX_STATUS.items() if k in flags) + "O"
        x_status = "".join(v for k, v in _MBOX_X_STATUS if k in flags)
        assert lines[1:3] == [f"Status: {status}", f"X-Status: {x_status}"], (
            f"{hits[0]}: marks {flags} must read `Status: {status}` / "
            f"`X-Status: {x_status}`; the stanza is {lines[1:4]!r}"
        )
        assert infos[hits[0]].date_time == time.gmtime(epoch)[:6], (
            f"{hits[0]}: the entry's time must be the arrival date {date!r}; "
            f"it is {infos[hits[0]].date_time}"
        )


def _assert_maildir(members, root: str) -> None:
    files = {info.filename: (info, data) for info, data in members if not info.is_dir()}
    for mailbox, flags, date in _SOURCE:
        epoch = _epoch(date)
        hits = [
            n for n in files
            if n.startswith(f"{root}/") and n.count("/") == 3
            and n.split("/")[2] == "cur"
            and _decode_component(n.split("/")[1]) == mailbox
            and _subject(mailbox).encode() in files[n][1]
        ]
        assert len(hits) == 1, (
            f"the message filed in {mailbox!r} must be in `cur/` of exactly one "
            f"directory that decodes back to that name; files: {sorted(files)}"
        )
        info, data = files[hits[0]]
        leaf = hits[0].rsplit("/", 1)[1]
        m = re.fullmatch(r"(?P<t>\d+)\.[0-9a-f]{16}(?:-\d+)?\.fauna\.invalid:2,(?P<flags>[A-Z]*)", leaf)
        assert m, f"{hits[0]}: not a pinned Maildir++ filename"
        assert int(m.group("t")) == epoch, (
            f"{hits[0]}: the filename's time must be the arrival date {date!r}"
        )
        want = "".join(sorted(_MAILDIR_LETTER[f] for f in flags))
        assert m.group("flags") == want, (
            f"{hits[0]}: marks {flags} must read `:2,{want}`"
        )
        assert info.date_time == time.gmtime(epoch)[:6], (
            f"{hits[0]}: the file's time must be the arrival date {date!r}; "
            f"it is {info.date_time}"
        )


def _assert_eml(members, root: str) -> None:
    files = {info.filename: data for info, data in members if not info.is_dir()}
    manifest_name = f"{root}/manifest.json"
    assert manifest_name in files, f"no manifest at {manifest_name}; files: {sorted(files)}"
    rows = json.loads(files[manifest_name])
    for mailbox, flags, date in _SOURCE:
        mine = [
            r for r in rows
            if _subject(mailbox).encode() in files.get(f"{root}/{r.get('filename')}", b"")
        ]
        assert len(mine) == 1, (
            f"the manifest must list the message filed in {mailbox!r} exactly once; rows: {rows}"
        )
        row = mine[0]
        assert row["source_mailbox"] == mailbox, (
            f"the manifest records the message filed in {mailbox!r} as from "
            f"{row['source_mailbox']!r}"
        )
        assert set(row["imap_flags"]) == set(flags), (
            f"manifest row for {mailbox!r}: marks {row['imap_flags']} are not {list(flags)}"
        )
        stamped = datetime.datetime.fromisoformat(row["internal_date"])
        assert int(stamped.timestamp()) == _epoch(date), (
            f"manifest row for {mailbox!r}: arrival date {row['internal_date']!r} is not {date!r}"
        )


def _utc_day() -> str:
    return datetime.datetime.now(datetime.timezone.utc).date().isoformat()


def _security_rows(app) -> list[str]:
    return [
        app.driver.get_text("notification-item", index=i)
        for i in range(app.notifications.notification_count())
    ]


@pytest.mark.feature("mailbox-export")
def test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates(
    app, dedicated_mail_nest, imap_source, request, tmp_path
):
    """One mailbox, exported in all three formats, each archive opened as the
    user opens it:

    * its **name** says whose mailbox, which format and which day (outcome 23);
    * every message keeps its **mailbox, arrival date and read / flagged /
      answered / draft marks**, in each format's own way (outcome 25) — mbox's
      `From ` line and `Status:`/`X-Status:`, Maildir++'s filename, EML zip's
      manifest — and every entry's time is the arrival date;
    * `Work` and `work`, and the device name `Aux`, **unpack as separate,
      usable folders** on any filesystem (outcome 26), each name decoding back
      to the mailbox it came from;
    * the `..`-named mailbox **never reaches outside** the folder the archive
      is unpacked into (outcome 27);
    * each download leaves the owner a **security notice naming its format and
      the address** it came from (outcome 36).
    """
    handle = dedicated_mail_nest
    _import_everything(app, handle, request, imap_source)

    for label, wire in _FORMATS:
        export = _export_to_done(app, label)
        days = {_utc_day()}
        name, data = _download(app, export)
        days.add(_utc_day())
        _assert_file_name(name, wire, days, handle, handle.nest)

        members = _open(name, data, tmp_path)
        names = [info.filename for info, _ in members]
        roots = {n.split("/", 1)[0] for n in names}
        assert len(roots) == 1, f"the {wire} archive must have one root directory: {sorted(roots)}"
        root = roots.pop()
        # Maildir++ needs `:` by its own definition (§ Archive paths → *Not
        # addressed*); mbox and EML zip are the formats for a Windows extraction.
        _assert_extracts_safely(names, windows_safe=wire != "maildir")
        {"mbox": _assert_mbox, "maildir": _assert_maildir, "eml-zip": _assert_eml}[wire](
            members, root
        )
        _discard(app, export)

    # ── The owner's notice, once per download, naming the format and address.
    app.notifications.navigate()
    for _label, wire in _FORMATS:
        before, after = S.notifications.row_security_mailbox_export_downloaded(
            format=wire, ip="\x00ip\x00"
        ).split("\x00ip\x00")
        row = wait_until(
            lambda: next((r for r in _security_rows(app) if before in r), None),
            budgets.PUSH_REFRESH_S,
            diagnose=lambda: (
                f"no security notice for the {wire} download; rows: "
                f"{_security_rows(app)!r}; page error: {app.error_text()!r}"
            ),
        )
        assert after in row, f"the {wire} notice must show in full: {row!r}"
        address = row[row.index(before) + len(before) : row.index(after)]
        assert re.fullmatch(r"[0-9a-fA-F:.]+", address), (
            f"the {wire} notice must name the address the download came from; "
            f"it reads {address!r} in {row!r}"
        )


@pytest.mark.feature("mailbox-export")
def test_the_same_export_twice_is_the_same_archive(
    app, dedicated_mail_nest, imap_source, request
):
    """Outcome 24 — the same mailbox, the same choices and the same format give
    the same archive, byte for byte (§ Goal; § Container shape's determinism
    contract: one total order, every entry time the message's own, nothing read
    from the clock, the host or a random source).

    The whole saved `.zip.zst` is compared, not the unpacked messages: a user
    checks one export against another with a checksum of the file they hold.
    Two **separate** export sessions — a fresh session key, fresh chunks, a
    fresh seal — so the comparison covers everything a second export could
    vary, not one blob read twice.

    **INBOX is left out of both, by the same choice.** Every download seals
    the owner a security notice into INBOX (`security_notify.rs`, the email
    channel of the `MailboxExportDownloaded` notice), so downloading the first
    export *changes the mailbox* the second would read — the two would differ
    because the source did, which is the bar working, not failing. Leaving
    INBOX out keeps "the same mailbox" literally true and still exports four
    mailboxes, their colliding names and every mark.
    """
    handle = dedicated_mail_nest
    _import_everything(app, handle, request, imap_source)

    archives = []
    for _ in range(2):
        export = _export_to_done(app, S.mail_export.format_mbox, leave_out=("INBOX",))
        archives.append(_download(app, export))
        _discard(app, export)

    (first_name, first), (second_name, second) = archives
    assert len(first) > 0, "the first archive is empty"
    assert first == second, (
        f"two exports of the same mailbox in the same format differ: "
        f"{first_name} is {len(first)} bytes, {second_name} {len(second)} bytes; "
        f"first difference at byte "
        f"{next((i for i, (a, b) in enumerate(zip(first, second)) if a != b), min(len(first), len(second)))}"
    )


def _recipient_keys(nest) -> frozenset[bytes]:
    """Every recipient key the nest seals mail to — on this dedicated nest, the
    one mail-enabled account's. Read-only, to prove the rotation was real."""
    import sqlite3

    conn = sqlite3.connect(nest["db_path"], timeout=10.0)
    try:
        rows = conn.execute("SELECT mls_pubkey FROM actor_mls_pubkeys").fetchall()
    finally:
        conn.close()
    return frozenset(r[0] for r in rows)


@pytest.mark.feature("mailbox-export")
def test_an_export_made_before_a_key_rotation_still_opens_after_it(
    app, dedicated_mail_nest, imap_source, request, tmp_path
):
    """Outcome 21 — § Key material (c): the export's session key is wrapped to
    the account's mail keys, and opens against the account's **complete
    standing key set**, never one generation. So the user exports, then
    rotates their mail keys, then — in a **restarted** app, whose custody holds
    only what the account kept — downloads the export made before the rotation,
    and it opens with every message in it.

    Non-vacuous: the rotation must change the key the nest seals mail to.
    """
    handle = dedicated_mail_nest
    _import_everything(app, handle, request, imap_source)
    _export_to_done(app, S.mail_export.format_mbox)

    keys_before = _recipient_keys(handle.nest)
    app.mail_settings.navigate()
    app.mail_settings.rotate_keys()
    assert app.mail_settings.wait_for_rotation_to_finish(budgets.ORCHESTRATION_STEP_S), (
        "the rotation never finished: progress "
        f"{app.driver.get_text('mail-rotate-keys-progress-indicator')!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=budgets.ORCHESTRATION_STEP_S), (
        f"the rotation must settle to up to date; status={app.mail_settings.status_text()!r}"
    )
    assert _recipient_keys(handle.nest) != keys_before, (
        "the rotation did not change the key mail is sealed to, so the export "
        "is not one generation back"
    )

    # A fresh process: nothing of the pre-rotation key survives in memory.
    app.driver.hard_reload()
    export = MailExportActions(app.driver)
    export.navigate()
    wait_until(
        lambda: app.driver.is_visible("mail-export-done-summary") or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "after the restart the wizard did not reopen on the finished export's "
            f"Done step; error-message: {export.error_text()!r}"
        ),
    )
    name, data = _download(app, export)
    assert export.error_text() == "", (
        f"the pre-rotation export was refused after the rotation: {export.error_text()!r}"
    )
    members = _open(name, data, tmp_path)
    blob = b"".join(d for _i, d in members)
    for mailbox, _flags, _date in _SOURCE:
        assert _subject(mailbox).encode() in blob, (
            f"the export made before the rotation opened without the message "
            f"from {mailbox!r}; entries: {[i.filename for i, _ in members]}"
        )
