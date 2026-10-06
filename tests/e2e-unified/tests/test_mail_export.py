"""E2E coverage for the user-facing mail-export wizard.

Target state: docs/goal/behavior/mail-export.md § UX shape (the five-step
wizard) / § Session row model / § Container shape / § Download flow; UX/IDs:
tests/e2e-unified/ui.yaml `mail-export` page + `mail-export-mailbox-progress-list`
component.

**tui is the lead app for this feature** (`docs/goal/architecture/testing.md`
§ Default app and nest mode — the TUI-first feature flow): it was the first app
whose glue gives the shared `MailExportMachine` key custody and spawns
`MailExportMachine::run_export`, so pressing `Start` produces a real archive;
linux, macos, ios, windows, android and web followed. Any apps not in `_DRIVING_APPS`
still build the seam-only machine (`rpc_glue::build_mail_export_machine_without_key_custody`) and answer
`Start` with an honest refusal — deliberately, since custody without a spawn would
open a session nothing drives. They are gated below as `skip_unbuilt`
(convention 7), not silently omitted, and each one's trickle-down flips its gate.

Test taxonomy:
- `tier_3` (mocking depth): every binary real — a real nest, a real Go MTA
  bridge, a real inbound SMTP delivery, and the real client drive loop sealing
  real chunks onto real nest disk.
"""

import os
import shutil
import time
import zipfile

import pytest

from helpers.app_surface import app_name, skip_environment, skip_unbuilt
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import route_inbound_mail_to_app
from helpers.mail_wire import _connect_smtp_starttls
from helpers.waiting import wait_until

# All 7 apps carry the marker: the wizard is painted everywhere and its
# client-side steps are the same shared machine, so `--client <app>` must select
# this file rather than deselect it. What differs is how far the journey gets,
# and that is decided in-body by `_require_driving_app` — a declared, tallied
# skip, never a silent omission.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    # The journey waits for the delivered message to surface decrypted in the
    # app before exporting it, and the launch-gated apps (windows, macOS, iOS,
    # android) keep their deterministic mock conversation backends unless the
    # consuming test carries this marker (conftest `_apply_real_conversations_env`).
    pytest.mark.real_conversations,
]

# The (external) sender's domain — no local-domain or loopback exemption, so the
# message arrives as genuine external inbound.
SENDER_DOMAIN = "external.test"

# The apps whose glue spawns `run_export` today. Every other app builds the
# custody-less machine, so `Start` refuses honestly and the journey below cannot
# be run — which is parity debt to declare, not a test to weaken.
_DRIVING_APPS = {"tui", "linux", "macos", "ios", "windows", "android", "web"}


def _require_driving_app(driver):
    """Skip-with-a-reason on an app that paints the wizard but does not drive it."""
    if app_name(driver) not in _DRIVING_APPS:
        skip_unbuilt(
            driver,
            surface="mail-export drive loop",
            detail="the app builds the seam-only MailExportMachine "
            "(build_mail_export_machine_without_key_custody), so Start refuses "
            "honestly rather than opening a session nothing drives",
            tracked="mail-export.md § Implementation status today",
        )


def _deliver_inbound(mx_port: int, server_name: str, recipient_addr: str,
                     raw_message: bytes, deadline: float) -> None:
    """One real inbound SMTP MAIL/RCPT/DATA through the MTA's port-25 STARTTLS
    listener. Returns after the `250` on `.`, which the MTA sends only once the
    WS-RPC ingest (seal + store) committed — so the message is in the store
    before this returns, with no wall-clock wait standing in for that fact."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{SENDER_DOMAIN}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _download_listing(driver) -> list[str]:
    """Everything in the app's download dir right now, sorted.

    Asks the driver for the directory at every call rather than holding a path
    across a wait: android's is a host mirror of the device's directory that
    only `download_dir()` itself refreshes (`drivers/base.py::download_dir`).
    """
    download_dir = driver.download_dir()
    if not download_dir or not os.path.isdir(download_dir):
        return []
    return sorted(os.listdir(download_dir))


def _saved_archives(driver) -> list[str]:
    """The export archives sitting in the app's download dir, newest last.

    § Compression wrapper pins the name as
    `fauna-export-<handle>-<format>-<iso-date>.zip.zst`, so the prefix is the
    contract and the rest (which handle, which day) is not this test's business.
    """
    names = [
        n for n in _download_listing(driver)
        if n.startswith("fauna-export-") and n.endswith(".zip.zst")
    ]
    return [os.path.join(driver.download_dir(), n) for n in names]


def _archive_entries(path: str, tmp_path) -> dict[str, bytes]:
    """Open a saved `.zip.zst` to `{entry name: bytes}`.

    § Container shape is a zip whose entries are all `Stored`, wrapped in one
    zstd stream, and ratifies that the **user** runs `zstd -d` once — so this
    does exactly that decompression, then opens the zip. Through the standard
    library's `compression.zstd` (Python 3.14, the version `.python-version`
    pins) rather than the `zstd` CLI, which not every dev box carries (macOS has
    none) — a skip there left the journey's one content assertion unasserted.
    """
    from compression import zstd

    unzstd = os.path.join(str(tmp_path), "export.zip")
    with zstd.open(path, "rb") as src, open(unzstd, "wb") as dst:
        shutil.copyfileobj(src, dst)
    with zipfile.ZipFile(unzstd) as zf:
        return {info.filename: zf.read(info) for info in zf.infolist()}


def _export_to_done(app, mail_bridge_mta, nest_instance, test_user, local_part):
    """Deliver one real inbound email to the app's user and walk the wizard to
    the Done step. Returns ``(export actions, nonce, subject)``.

    ``local_part`` is the test's own inbound address, so no two tests contend
    for one alias. Every step is the user's own gesture through the app UI
    (convention 8); the waits are state assertions, never clocks (convention 14).
    """
    from actions.mail_export import MailExportActions

    # ── 1. Enable mail on the logged-in user (mints the client-held MSEK and
    # registers its recipient pubkey, so the MTA can seal to a key only this app
    # opens — and so the export's record opener has something to open with), then
    # route a local part of this test's own to this actor.
    domain = mail_bridge_mta.domain
    recipient_addr = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, local_part
    )

    # ── 2. Deliver one real inbound email. CRLF line endings throughout, as the
    # wire requires and as the mbox serializer's normalization is written
    # against — a fixture with bare LF would exercise a shape no MTA produces.
    nonce = f"mailexport{int(time.time() * 1000)}qx"
    subject = f"Exported message {nonce}"
    raw_message = ("\r\n".join([
        f"From: External Sender <sender@{SENDER_DOMAIN}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: <{nonce}@{SENDER_DOMAIN}>",
        "Date: Mon, 21 Sep 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must survive the export unchanged.",
    ]) + "\r\n").encode()
    _deliver_inbound(
        mail_bridge_mta.mx_port, domain, recipient_addr, raw_message,
        time.monotonic() + 40.0,
    )

    # ── 3. Wait for the message to show decrypted in the app. Not a courtesy:
    # the export walks the same store the receive loop reads, so this is the
    # causal anchor that replaces "sleep and hope the ingest landed" — a state
    # assertion, not a clock one (convention 14).
    wait_until(
        lambda: next(
            (t for t in app.conversations.list_threads()
             if nonce in (t.label or "") or nonce in (t.snippet or "")),
            None,
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the inbound email tagged {nonce!r} never surfaced decrypted, so "
            f"there is nothing for the export to find.\n"
            f"  threads: {[(t.label, t.snippet) for t in app.conversations.list_threads()]}\n"
            f"  bridge log: {mail_bridge_mta.log_file}"
        ),
    )

    # ── 4. Run the wizard. First clear what earlier tests left on this
    # session-scoped account: the wizard adopts the newest export not yet
    # discarded on entry (§ Resume), which would open it on Done rather than
    # Format. Setup, not a step of this journey — hence through the nest.
    _clear_prior_exports(nest_instance, test_user)
    # Default format (mbox) and default scope — § UX shape
    # pre-selects every mailbox but Trash/Junk, and the delivered mail is in
    # INBOX. Deliberately no custom mailbox names: two names that fold to one
    # archive path would fail the whole export and would read here as a harness flake rather than a product bug.
    export = MailExportActions(app.driver)
    export.navigate()
    assert export.is_page_visible(), (
        "mail-export wizard not reachable on its Format step.\n"
        f"  showing Done instead: {app.driver.is_visible('mail-export-done-summary')}\n"
        f"  showing Progress instead: {app.driver.is_visible('mail-export-progress-summary')}\n"
        f"  error-message: {export.error_text()!r}\n"
        f"  sessions on the nest: "
        f"{[(s.get('state'), s.get('started_at')) for s in _list_sessions(nest_instance, test_user['signing_key'])]}"
    )
    export.next()   # Format → Scope
    export.next()   # Scope → Confirm
    export.start()  # the durable commit

    # ── 5. Poll the page to the Done step. `mail-export-done-summary` is painted
    # only there, so its visibility IS the state assertion — no sleep, no fixed
    # delay, nothing keyed on how fast the box happened to be (convention 14).
    wait_until(
        lambda: app.driver.is_visible("mail-export-done-summary") or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the export never reached the Done step.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  progress: {_progress_or_blank(export)!r}"
        ),
    )
    assert export.done_summary() != "", "the Done step must summarize the export"
    return export, nonce, subject


@pytest.mark.feature("mailbox-export")
def test_mail_export_wizard_reachable(logged_in_app):
    """The mail-export wizard is reachable and renders step 1 (format picker).

    Runs on every app: painting the wizard is not what the drive loop gates.
    """
    from actions.mail_export import MailExportActions

    export = MailExportActions(logged_in_app.driver)
    export.navigate()
    assert export.is_page_visible(), "mail-export wizard not reachable"


@pytest.mark.feature("mailbox-export")
def test_mail_export_runs_to_done(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """The whole journey: a real inbound email is delivered, the user exports
    their mailbox through the wizard, downloads the archive, and the message is
    **inside it**.

    This is the assertion the feature exists for. Reaching the Done step only
    proves the session row said `completed`; opening the saved archive and
    finding the delivered message proves the client really unsealed every
    record, serialized it, sealed the chunks, uploaded them, downloaded the
    blob, opened its frames and wrote out an archive a standard tool reads.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    export, nonce, subject = _export_to_done(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexport"
    )

    # ── 6. Download. The click drives § Download flow end to end: GET the
    # nest-minted URL, unwrap the row's session key under the account's standing
    # set, open the frames in order refusing an unterminated blob, and write the
    # recovered `.zip.zst` into the app's download dir (under e2e,
    # `FAUNA_E2E_DOWNLOAD_DIR`; on android the app's own download directory,
    # which the driver mirrors; on web, wherever the bridge saves the browser's
    # downloads, which it does on each `download_dir()` call).
    download_dir = app.driver.download_dir()
    assert download_dir, "the driver must expose an e2e download dir to observe the save"
    before = set(_saved_archives(app.driver))
    export.download()

    saved = wait_until(
        lambda: next((p for p in _saved_archives(app.driver) if p not in before), None),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"no export archive appeared in {download_dir}.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  dir now: {_download_listing(app.driver)}"
        ),
    )

    # ── 7. The assertion the whole feature is for: the delivered message is in
    # the archive, byte-for-byte enough that both its header and its body
    # survived. An empty or header-only archive passes every earlier step.
    entries = _archive_entries(saved, tmp_path)
    assert entries, f"the exported archive has no entries: {saved}"
    blob = b"".join(entries.values())
    assert nonce.encode() in blob, (
        f"the delivered message (tagged {nonce!r}) is not in the exported "
        f"archive.\n  entries: {sorted(entries)}\n  archive: {saved}"
    )
    assert f"Subject: {subject}".encode() in blob, (
        f"the exported message must carry its Subject header verbatim.\n"
        f"  entries: {sorted(entries)}"
    )
    assert f"The {nonce} body must survive the export unchanged.".encode() in blob, (
        f"the exported message must carry its body.\n  entries: {sorted(entries)}"
    )


def _list_sessions(nest_instance, signing_key) -> list[dict]:
    """The caller's own export sessions, as the nest projects them
    (`fauna.bridges.list_export_sessions`, the same User-class kind every app's
    wizard lists with). Read-only: an observation, not a step of the journey."""
    from common.auth import _authed_call

    reply = _authed_call(
        nest_instance["url"], signing_key, "fauna.bridges.list_export_sessions", {}
    )
    return list(reply.get("sessions") or [])


def _clear_prior_exports(nest_instance, user) -> None:
    """Dispose of every export the account still holds — a finished one is
    discarded, an unfinished one cancelled — so the wizard opens on its Format
    step. Test isolation for a session-scoped account, not a journey step."""
    from common.auth import _authed_call

    for s in _list_sessions(nest_instance, user["signing_key"]):
        state = s.get("state")
        if state == "completed":
            kind = "fauna.bridges.discard_export_blob"
        elif state in ("running", "paused"):
            kind = "fauna.bridges.cancel_export_session"
        else:
            continue
        _authed_call(
            nest_instance["url"], user["signing_key"], kind,
            {"session_id": s["session_id"]},
        )


def _newest_completed_session(nest_instance, user) -> dict:
    """The user's most recently started `completed` export — the one the wizard
    just walked to Done."""
    done = [s for s in _list_sessions(nest_instance, user["signing_key"])
            if s.get("state") == "completed"]
    assert done, (
        "the wizard reached Done but the nest lists no completed export for the "
        f"user: {_list_sessions(nest_instance, user['signing_key'])!r}"
    )
    return max(done, key=lambda s: s.get("started_at", 0))


def _get_blob(nest_instance, user, download_url: str):
    """GET the export's download URL as ``user``, with a bearer minted by the
    ordinary handshake — the request any of that user's clients would make."""
    import requests

    from common.auth import mint_token_via_handshake

    token = mint_token_via_handshake(nest_instance["url"], user["signing_key"])
    return requests.get(
        nest_instance["url"].rstrip("/") + download_url,
        headers={"Authorization": f"Bearer {token}"},
        timeout=120,
    )


def _files_containing(root: str, needle: bytes) -> list[str]:
    """Every file under ``root`` whose bytes contain ``needle``."""
    hits = []
    for dirpath, _dirs, files in os.walk(root):
        for name in files:
            path = os.path.join(dirpath, name)
            try:
                with open(path, "rb") as fh:
                    if needle in fh.read():
                        hits.append(path)
            except OSError:
                continue
    return hits


# `mail-export.md` § Blob shape on disk: the terminator is an empty-plaintext
# frame — a 4-byte length, a 24-byte nonce and the 16-byte Poly1305 tag.
_TERMINATOR_FRAME_BYTES = 4 + 24 + 16


@pytest.mark.feature("mailbox-export")
def test_mail_export_archive_is_sealed_to_its_owner(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """Nobody else can read or download your export, and the server never holds
    a key to the archive it stores for you (`mail-export.md` § Sealed-blob
    delivery, § Download flow step 2).

    The export is made the user's way, through the wizard. Then three observers
    the user does not control look at it: a **second account** on the same nest
    (its listing never shows the session, its GET of the download URL is
    refused), the **nest's own disk** (nothing it stores — database, WAL, the
    export blob — carries the exported message's body in the clear), and the
    **bytes the nest serves to the owner** (framed ciphertext under the
    § Blob shape preamble, not the archive — only the client's unwrap turns it
    into one, which `test_mail_export_runs_to_done` proves from the other side).
    """
    from conftest import _make_user

    app = logged_in_app
    _require_driving_app(app.driver)
    _export, nonce, _subject = _export_to_done(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportseal"
    )
    body_line = f"The {nonce} body must survive the export unchanged.".encode()
    session = _newest_completed_session(nest_instance, test_user)
    assert session.get("download_url"), f"a completed export must carry its download URL: {session!r}"

    # ── A second account on the same nest: blind to the session, refused at the
    # door. The 404 is deliberate (§ Cross-actor isolation) — the same answer a
    # session that does not exist gets, so the refusal leaks nothing either.
    stranger = _make_user(nest_instance)
    stranger_ids = {s.get("session_id") for s in _list_sessions(nest_instance, stranger["signing_key"])}
    assert session["session_id"] not in stranger_ids, (
        "another account's listing shows this user's export session"
    )
    foreign = _get_blob(nest_instance, stranger, session["download_url"])
    assert foreign.status_code == 404, (
        f"another account downloaded this user's export: HTTP {foreign.status_code}, "
        f"{len(foreign.content)} bytes"
    )

    # ── What the nest serves the owner is ciphertext: the § Blob shape preamble
    # and then frames — never the `.zip.zst`, never the message.
    own = _get_blob(nest_instance, test_user, session["download_url"])
    assert own.status_code == 200, f"the owner's download was refused: HTTP {own.status_code}"
    served = own.content
    assert served.startswith(b"FXPT"), (
        f"the served blob does not open with the sealed-blob preamble: {served[:16]!r}"
    )
    assert body_line not in served and nonce.encode() not in served, (
        "the blob the nest stores and serves carries the exported message in the clear"
    )

    # ── The key the nest holds is the wrapped one only (§ Key material): a
    # 256-bit session key would be 32 bytes; what rests on the row is the
    # X-Wing-sealed `ExportSessionKeyBlob`, which only the user's standing key
    # set opens.
    wrapped = bytes(session.get("wrapped_session_key") or b"")
    assert len(wrapped) > 32, (
        f"the nest's stored session key is {len(wrapped)} bytes — not a wrapped key"
    )

    # ── Nothing the nest keeps on disk holds the exported message's body.
    leaks = _files_containing(nest_instance["tmp_dir"], body_line)
    assert not leaks, f"the exported message's body rests in the clear on nest disk: {leaks}"


@pytest.mark.feature("mailbox-export")
def test_mail_export_refuses_a_truncated_or_unfinished_archive(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """A truncated or still-running archive is refused rather than saved as if
    it were your whole mailbox (`mail-export.md` § Download flow step 4,
    § Architectural rules' no-partial-download rule).

    **Truncated.** The export is made through the wizard; then its blob on nest
    disk loses its terminator frame — the shape a download cut short, or a blob
    damaged at rest, presents to the client. Pressing Download must say why it
    refused on `error-message`, and **nothing** may appear in the download dir:
    not the archive's name, not a partial file under another one. Every frame
    before the terminator still authenticates, so this is exactly the case only
    the terminator check catches.

    **Still running.** The download door refuses a session that has not
    completed, whoever asks — the owner included — so there is no URL at which
    a half-built archive can be fetched at all.
    """
    import glob

    from common.auth import _authed_call

    app = logged_in_app
    _require_driving_app(app.driver)
    export, _nonce, _subject = _export_to_done(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexporttrunc"
    )
    session = _newest_completed_session(nest_instance, test_user)

    # ── Cut the terminator off the blob the nest will serve. The nest takes its
    # Content-Length from the file, so the client receives a well-formed,
    # shorter body — every body frame intact, the commitment to its length gone.
    exports_dir = os.path.join(nest_instance["tmp_dir"], "exports")
    blobs = glob.glob(os.path.join(exports_dir, f"{session['session_id']}*.sealed"))
    assert len(blobs) == 1, (
        f"expected the session's one sealed blob under {exports_dir}, found {blobs}; "
        f"dir: {sorted(os.listdir(exports_dir)) if os.path.isdir(exports_dir) else 'absent'}"
    )
    size = os.path.getsize(blobs[0])
    assert size > len(b"FXPT") + 2 + _TERMINATOR_FRAME_BYTES, f"blob implausibly small: {size} bytes"
    try:
        os.truncate(blobs[0], size - _TERMINATOR_FRAME_BYTES)
    except PermissionError:
        skip_environment(
            "the nest's data dir is not writable by the harness in this nest mode, "
            "so the blob cannot be truncated"
        )

    download_dir = app.driver.download_dir()
    assert download_dir, "the driver must expose an e2e download dir to observe the save"
    before = _download_listing(app.driver)
    assert export.error_text() == "", (
        f"the Done step already shows an error before Download: {export.error_text()!r}"
    )
    export.download()

    # The refusal is a state the page paints — wait on it, not on a clock.
    refusal = wait_until(
        lambda: export.error_text() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            "Download of a truncated archive never surfaced a refusal.\n"
            f"  download dir: {_download_listing(app.driver)}"
        ),
    )
    after = _download_listing(app.driver)
    assert after == before, (
        f"a refused archive left files behind in the download dir: "
        f"{sorted(set(after) - set(before))} (refusal: {refusal!r})"
    )

    # ── Still running: open a session nothing drives and ask the door for it.
    # An observation of the door the app downloads through, not a step of the
    # user's journey — the wizard offers Download only on the Done step, so no
    # gesture can reach it for a running export (convention 8 is about journey
    # mutations; this one is cancelled again below).
    started = _authed_call(
        nest_instance["url"], test_user["signing_key"],
        "fauna.bridges.start_export_session",
        {
            "format": "mbox",
            # Opaque to the nest (§ Session row model): an empty CBOR map.
            "scope_descriptor": b"\xa0",
            # Never opened by anyone — nothing is uploaded under it.
            "wrapped_session_key": b"\x00" * 64,
            "total_count": 1,
        },
    )
    running_id = started["session_id"]
    try:
        resp = _get_blob(nest_instance, test_user, f"/api/v1/export/{running_id}")
        assert resp.status_code == 404, (
            f"the download door served a still-running export: HTTP {resp.status_code}, "
            f"{len(resp.content)} bytes"
        )
    finally:
        _authed_call(
            nest_instance["url"], test_user["signing_key"],
            "fauna.bridges.cancel_export_session", {"session_id": running_id},
        )


def _progress_or_blank(export) -> str:
    """The Progress screen's summary if it is still painted — diagnosis only."""
    try:
        return export.progress_summary()
    except Exception:
        return ""
