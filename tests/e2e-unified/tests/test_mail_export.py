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

import datetime
import glob
import os
import re
import time
import uuid
from pathlib import Path

import pytest

from helpers.app_surface import app_name, skip_environment, skip_unbuilt
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.mail_client_ui import route_inbound_mail_to_app
from helpers.mail_export_ui import (
    archive_entries,
    clear_prior_exports,
    download_listing,
    list_sessions,
    saved_archives,
)
from helpers.mail_wire import _connect_smtp_starttls
from helpers.relaunch import relaunch, require_relaunchable
from helpers.rpc_hold import (
    arm_rpc_hold,
    refuse_rpc,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_held_rpc,
)
from helpers.waiting import wait_until
from i18n.strings import S

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

# The day the SENDER says it wrote every message (its `Date:` header) — a fixed
# day in the past, never the day the message arrives. § Wizard steps step 2
# ranges by INTERNALDATE, the arrival, so a test can tell the two apart only
# because they differ.
_SENDER_DATE_HEADER = "Mon, 21 Sep 2026 12:00:00 +0000"
_SENDER_DAY = "2026-09-21"

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
                     raw_message: bytes, deadline: float, *,
                     sender_domain: str = SENDER_DOMAIN) -> None:
    """One real inbound SMTP MAIL/RCPT/DATA through the MTA's port-25 STARTTLS
    listener. Returns after the `250` on `.`, which the MTA sends only once the
    WS-RPC ingest (seal + store) committed — so the message is in the store
    before this returns, with no wall-clock wait standing in for that fact."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<sender@{sender_domain}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def _export_to_done(app, mail_bridge_mta, nest_instance, test_user, local_part):
    """Deliver one real inbound email to the app's user and walk the wizard to
    the Done step. Returns ``(export actions, nonce, subject)``."""
    export, nonce, subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, local_part
    )
    export.start()  # the durable commit
    _wait_done(app, export)
    return export, nonce, subject


def _wait_done(app, export) -> str:
    """Poll the page to the Done step and return its summary.
    `mail-export-done-summary` is painted only there, so its visibility IS the
    state assertion — no sleep, no fixed delay, nothing keyed on how fast the
    box happened to be (convention 14)."""
    wait_until(
        lambda: app.driver.is_visible("mail-export-done-summary") or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the export never reached the Done step.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  progress: {_progress_or_blank(export)!r}"
        ),
    )
    summary = export.done_summary()
    assert summary != "", "the Done step must summarize the export"
    return summary


def _deliver_and_open_confirm(app, mail_bridge_mta, nest_instance, test_user, local_part):
    """Deliver one real inbound email to the app's user and walk the wizard to
    its Confirm step, committing nothing. Returns ``(export actions, nonce,
    subject)``.

    ``local_part`` is the test's own inbound address, so no two tests contend
    for one alias. Every step is the user's own gesture through the app UI
    (convention 8); the waits are state assertions, never clocks (convention 14).
    """
    recipient_addr = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, local_part
    )
    nonce, subject = _deliver_tagged(app, mail_bridge_mta, recipient_addr)
    export = _open_wizard(app, nest_instance, test_user)
    # Default format (mbox) and default scope — § UX shape pre-selects every
    # mailbox but Trash/Junk, and the delivered mail is in INBOX.
    export.next()   # Format → Scope
    export.next()   # Scope → Confirm
    return export, nonce, subject


def _deliver_tagged(app, mail_bridge_mta, recipient_addr, *,
                    sender_domain=SENDER_DOMAIN, extra_headers=(), surfaces=True):
    """Deliver one real inbound email, tagged with a fresh nonce, to
    ``recipient_addr`` (an address ``route_inbound_mail_to_app`` routed to the
    app's user). Returns ``(nonce, subject)``.

    ``extra_headers`` are prepended to the header section verbatim, as a sender
    or an upstream relay would have written them. ``surfaces`` waits for the
    message to show decrypted in the app's Conversations — the causal anchor
    that it is in the store the export walks; a message a filter files out of
    INBOX never shows there, and its anchor is the MTA's own `250`, which it
    sends only once the WS-RPC ingest (seal + store) committed.
    """
    # The nonce is unique per call, so two messages of one test never share one.
    nonce = f"mailexport{time.time_ns()}qx"
    subject = f"Exported message {nonce}"
    # CRLF line endings throughout, as the wire requires and as the mbox
    # serializer's normalization is written against — a fixture with bare LF
    # would exercise a shape no MTA produces.
    raw_message = ("\r\n".join([
        *extra_headers,
        f"From: External Sender <sender@{sender_domain}>",
        f"To: {recipient_addr}",
        f"Subject: {subject}",
        f"Message-ID: <{nonce}@{sender_domain}>",
        f"Date: {_SENDER_DATE_HEADER}",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must survive the export unchanged.",
    ]) + "\r\n").encode()
    _deliver_inbound(
        mail_bridge_mta.mx_port, mail_bridge_mta.domain, recipient_addr, raw_message,
        time.monotonic() + 40.0, sender_domain=sender_domain,
    )
    if surfaces:
        # Not a courtesy: the export walks the same store the receive loop
        # reads, so this replaces "sleep and hope the ingest landed" — a state
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
    return nonce, subject


def _open_wizard(app, nest_instance, user):
    """Open the wizard on its Format step and return its actions.

    First clears what earlier tests left on this session-scoped account: the
    wizard adopts the newest export not yet discarded on entry (§ Resume), which
    would open it on Done rather than Format. Setup, not a step of the journey —
    hence through the nest. Deliberately no custom mailbox names anywhere in
    this file: two names that fold to one archive path would fail the whole
    export and would read here as a
    harness flake rather than a product bug.
    """
    from actions.mail_export import MailExportActions

    clear_prior_exports(nest_instance, user)
    export = MailExportActions(app.driver)
    export.navigate()
    # A test that stopped short of Start leaves this app's wizard on Scope or
    # Confirm with no session behind it; walk back to Format through the
    # wizard's own Back, as a user would.
    for _ in range(2):
        if export.is_page_visible(timeout=2.0) or not app.driver.is_visible("wizard-back-button"):
            break
        export.back()
    assert export.is_page_visible(), (
        "mail-export wizard not reachable on its Format step.\n"
        f"  showing Done instead: {app.driver.is_visible('mail-export-done-summary')}\n"
        f"  showing Progress instead: {app.driver.is_visible('mail-export-progress-summary')}\n"
        f"  error-message: {export.error_text()!r}\n"
        f"  sessions on the nest: "
        f"{[(s.get('state'), s.get('started_at')) for s in list_sessions(nest_instance, user['signing_key'])]}"
    )
    return export


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
def test_mail_export_offers_three_formats_with_mbox_chosen(
    logged_in_app, nest_instance, test_user
):
    """The Format step offers mbox, Maildir++ and EML zip, with mbox chosen
    until the user picks another (`mail-export.md` § Wizard steps step 1).

    Runs on every app: the picker is the shared machine's client-side step,
    which no drive loop gates. That each choice really produces its own shape
    of archive is `test_mail_export_strip_removes_transit_headers_and_keeps_the_senders`'s
    job, which exports in all three.
    """
    export = _open_wizard(logged_in_app, nest_instance, test_user)
    labels = [S.mail_export.format_mbox, S.mail_export.format_maildir,
              S.mail_export.format_eml]
    assert export.selected_format() == S.mail_export.format_mbox, (
        f"a fresh wizard must have mbox chosen, shows {export.selected_format()!r}"
    )
    options = export.format_options()
    if options is not None:
        assert sorted(options) == sorted(labels), (
            f"the picker must offer exactly the three formats: {options!r}"
        )
    # Each is a real choice the picker takes — `select` refuses an option the
    # picker does not paint (convention 11) — and the last pick is mbox again,
    # so the wizard leaves this test as it found it.
    for label in [*labels[1:], labels[0]]:
        export.select_format(label)
        assert export.selected_format() == label, (
            f"picked {label!r}, the picker shows {export.selected_format()!r}"
        )


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
    before = set(saved_archives(app.driver))
    export.download()

    saved = wait_until(
        lambda: next((p for p in saved_archives(app.driver) if p not in before), None),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"no export archive appeared in {download_dir}.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  dir now: {download_listing(app.driver)}"
        ),
    )

    # ── 7. The assertion the whole feature is for: the delivered message is in
    # the archive, byte-for-byte enough that both its header and its body
    # survived. An empty or header-only archive passes every earlier step.
    entries = archive_entries(saved, tmp_path)
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


# The mailboxes the scope step leaves unselected by default (§ UX shape).
_DESELECTED_BY_DEFAULT = {"Trash", "Junk"}

# Session states that hold an export someone started (§ Quota composition's
# "live"); a cancelled or errored row left by an earlier test is not one.
_LIVE_STATES = {"running", "paused", "completed"}


def _template_pattern(render, **groups) -> "re.Pattern[str]":
    """A regex matching ``render``'s output with each named placeholder free:
    the template is rendered with sentinels, escaped, and each sentinel becomes
    a named group — so the assertion follows `en.yaml`, never a copied string."""
    rendered = re.escape(render(**{k: f"@@{k}@@" for k in groups}))
    for k, body in groups.items():
        rendered = rendered.replace(re.escape(f"@@{k}@@"), f"(?P<{k}>{body})")
    return re.compile(rf"^{rendered}$")


def _byte_size_value(text: str) -> tuple[float, int]:
    """Undo `fauna_core::format::byte_size` (1024 units, one decimal):
    ``(value in bytes, the unit's size)``."""
    units = {"B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30, "TB": 1 << 40}
    m = re.fullmatch(r"([0-9]+(?:\.[0-9])?) (B|KB|MB|GB|TB)", text)
    assert m, f"not a shared-format byte size: {text!r}"
    return float(m.group(1)) * units[m.group(2)], units[m.group(2)]


@pytest.mark.feature("mailbox-export")
def test_mail_export_confirm_estimates_size_and_time_before_anything_starts(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """Before anything starts, the Confirm step says how many messages are about
    to be exported, roughly how large the archive will be and roughly how long
    it will take — and nothing has started (`mail-export.md` § Wizard steps
    step 3).

    The figures are checked against the nest's own listing
    (`fauna.bridges.list_own_mailboxes`, read-only, as the user): the message
    total is the selected mailboxes' counts, the estimated size is their
    `sealed_bytes` through the shared byte format, and the time is that figure
    through the shared 1 MiB/s rate (`export_time_estimate_secs`) — so a size
    or time the app invented, or summed over unselected mailboxes, goes red.
    """
    from common.auth import _authed_call

    app = logged_in_app
    _require_driving_app(app.driver)
    export, _nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportconfirm"
    )
    summary = export.confirm_summary()

    listing = _authed_call(
        nest_instance["url"], test_user["signing_key"],
        "fauna.bridges.list_own_mailboxes", {},
    )
    selected = [m for m in listing.get("mailboxes") or []
                if m.get("name") not in _DESELECTED_BY_DEFAULT]
    messages = sum(int(m.get("exists") or 0) for m in selected)
    sealed = sum(int(m.get("sealed_bytes") or 0) for m in selected)
    assert messages >= 1 and sealed > 0, (
        f"the delivered message must be listed with its size: {listing!r}"
    )

    m = _template_pattern(
        lambda summary, bytes, duration: S.mail_export.confirm_summary_estimated_fmt(
            summary=summary, bytes=bytes, duration=duration),
        summary=".+", bytes=".+?", duration=".+?",
    ).match(summary)
    assert m, (
        f"the Confirm summary carries no size and time estimate.\n"
        f"  summary: {summary!r}\n"
        f"  listing: {listing!r}\n"
        f"  error-message: {export.error_text()!r}"
    )
    counts = _template_pattern(
        lambda format, mailboxes, messages: S.mail_export.confirm_summary_count_fmt(
            format=format, mailboxes=mailboxes, messages=messages),
        format=".+", mailboxes="[0-9]+", messages="[0-9]+",
    ).match(m.group("summary"))
    assert counts, f"the counts half of the summary is malformed: {summary!r}"
    assert int(counts.group("messages")) == messages, (
        f"total messages {counts.group('messages')} is not the selected "
        f"mailboxes' {messages}: {summary!r}"
    )
    assert int(counts.group("mailboxes")) == len(selected), summary

    shown, unit = _byte_size_value(m.group("bytes"))
    assert abs(shown - sealed) <= unit * 0.05 + 1, (
        f"estimated size {m.group('bytes')!r} is not the selected mailboxes' "
        f"{sealed} sealed bytes"
    )
    secs = -(-sealed // (1 << 20))  # ceil over the shared 1 MiB/s rate
    assert secs < 3600, f"a handful of messages cannot estimate an hour: {sealed} B"
    assert m.group("duration") == S.time.uptime_m(mins=str(secs // 60)), (
        f"estimated time {m.group('duration')!r} is not {secs} s in the shared "
        f"duration format"
    )

    # Nothing starts until the user confirms: no live session exists yet, and
    # the wizard is still on Confirm.
    live = [s for s in list_sessions(nest_instance, test_user["signing_key"])
            if s.get("state") in _LIVE_STATES]
    assert live == [], f"a session opened before Start: {live!r}"
    assert app.driver.is_visible("mail-export-start-button"), (
        "the wizard left Confirm without the user pressing Start"
    )


def _newest_completed_session(nest_instance, user) -> dict:
    """The user's most recently started `completed` export — the one the wizard
    just walked to Done."""
    done = [s for s in list_sessions(nest_instance, user["signing_key"])
            if s.get("state") == "completed"]
    assert done, (
        "the wizard reached Done but the nest lists no completed export for the "
        f"user: {list_sessions(nest_instance, user['signing_key'])!r}"
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
    stranger_ids = {s.get("session_id") for s in list_sessions(nest_instance, stranger["signing_key"])}
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
    before = download_listing(app.driver)
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
            f"  download dir: {download_listing(app.driver)}"
        ),
    )
    after = download_listing(app.driver)
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


# ── The wizard's controls and lifecycle (mailbox-export outcomes 5-9, 11-19, 40) ──
#
# Every mutation below is the user's own gesture in the app (convention 8). The
# nest is touched for three things only, none of them a step of the journey:
# observing what it holds (the session listing, the mailbox listing, the blob
# on its disk), arranging the mail the export walks (a real SMTP delivery, and
# a filter that files one sender's mail into Archive — a rule the user could
# make themselves), and holding one RPC kind's dispatch (`helpers/rpc_hold.py`)
# so that a state part way through an export is one the test can stand in
# rather than an interval it races (convention 14). A one-message export
# otherwise runs to Done before the test's next line: there is no "while it
# runs" to look at without the hold.

# The drive loop's one read per page (`MailExportMachine::run_export_inner`):
# held, the export stays `running` between two pages for as long as the test
# needs it to.
_FETCH_KIND = "fauna.bridges.fetch_export_chunk_ciphertext"
# The call that turns a fully uploaded blob into a `completed` session: held,
# the archive's bytes rest on the nest while the session is still unfinished.
_FINALIZE_KIND = "fauna.bridges.finalize_export_session"


def _mailboxes(nest_instance, user) -> list[dict]:
    """The user's mailboxes as the nest lists them to the wizard
    (`fauna.bridges.list_own_mailboxes`), in its order — the order the drive
    loop walks them in. Read-only."""
    from common.auth import _authed_call

    reply = _authed_call(
        nest_instance["url"], user["signing_key"], "fauna.bridges.list_own_mailboxes", {}
    )
    return list(reply.get("mailboxes") or [])


def _session(nest_instance, user, session_id: str) -> dict | None:
    return next(
        (s for s in list_sessions(nest_instance, user["signing_key"])
         if s.get("session_id") == session_id),
        None,
    )


def _the_live_session(nest_instance, user) -> dict:
    """The one `running`/`paused` export the user has — the one this test's
    Start just opened (`_open_wizard` cleared every earlier one)."""
    live = [s for s in list_sessions(nest_instance, user["signing_key"])
            if s.get("state") in ("running", "paused")]
    assert len(live) == 1, f"expected exactly one live export, the nest lists {live!r}"
    return live[0]


def _wait_session_state(nest_instance, user, session_id: str, state: str, what: str,
                        export=None) -> dict:
    def _diag() -> str:
        extra = f"\n  error-message: {export.error_text()!r}" if export else ""
        return f"session now: {_session(nest_instance, user, session_id)!r}{extra}"

    return wait_until(
        lambda: (s := _session(nest_instance, user, session_id))
        and s.get("state") == state and s or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"{what}: the export never read {state!r}.\n  {_diag()}",
    )


def _blob_files(nest_instance, session_id: str) -> list[str]:
    """The sealed blob files the nest holds for ``session_id`` on its disk
    (§ Blob shape on disk names them after the session)."""
    return glob.glob(os.path.join(nest_instance["tmp_dir"], "exports", f"{session_id}*.sealed"))


def _download_entries(app, export, tmp_path) -> dict[str, bytes]:
    """Press Download on the Done step and open the archive it saves.

    The archive's name is the handle, the format and the DAY, so a second
    export of a day saves under the name an earlier one already took — and the
    sink renames its finished `.part` file into place over it. A new file is
    therefore told by its identity (inode, mtime), never by a name not seen
    before.
    """
    def _identities() -> dict[str, tuple[int, int]]:
        out = {}
        for p in saved_archives(app.driver):
            try:
                st = os.stat(p)
            except OSError:
                continue
            out[p] = (st.st_ino, st.st_mtime_ns)
        return out

    before = _identities()
    export.download()
    saved = wait_until(
        lambda: next((p for p, ident in _identities().items() if before.get(p) != ident), None),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"no export archive appeared in {app.driver.download_dir()}.\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  dir now: {download_listing(app.driver)}"
        ),
    )
    out = tmp_path / uuid.uuid4().hex
    out.mkdir()
    return archive_entries(saved, out)


def _file_sender_into(nest_instance, user, sender_domain: str, mailbox: str) -> int:
    """A mail filter of the user's own — the rule a user writes in mail
    settings — filing ``sender_domain``'s mail into ``mailbox``. Returns its id.
    Arrangement of the mail the export walks, not a step of the journey."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_instance["url"], actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as account:
        reply = account.call(
            "fauna.email.filters.create",
            {
                "name": f"file {sender_domain} into {mailbox}",
                "rules": [{"SenderDomain": {"domain": sender_domain}}],
                "combination": "all",
                "action": {"FileInto": {"mailbox": mailbox}},
                "priority": 0,
            },
        )
    return int(reply["id"])


def _drop_filter(nest_instance, user, filter_id: int) -> None:
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        nest_instance["url"], actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as account:
        account.call("fauna.email.filters.delete", {"id": filter_id})


def _deliver_into_archive(app, mail_bridge_mta, nest_instance, user, recipient) -> str:
    """Deliver one message the user's own filter files into Archive, and return
    its nonce. Archive sorts ahead of INBOX in the nest's listing, so it is the
    first mailbox with mail the drive loop walks."""
    domain = f"archived{uuid.uuid4().hex[:8]}.test"
    before = {m["name"]: int(m.get("exists") or 0) for m in _mailboxes(nest_instance, user)}
    filter_id = _file_sender_into(nest_instance, user, domain, "Archive")
    try:
        nonce, _subject = _deliver_tagged(
            app, mail_bridge_mta, recipient, sender_domain=domain, surfaces=False
        )
    finally:
        _drop_filter(nest_instance, user, filter_id)
    wait_until(
        lambda: next((int(m.get("exists") or 0) for m in _mailboxes(nest_instance, user)
                      if m["name"] == "Archive"), 0) > before.get("Archive", 0) or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the filed message never reached Archive: {_mailboxes(nest_instance, user)!r}",
    )
    return nonce


def _messages_in(entries: dict[str, bytes]) -> list[bytes]:
    """Every message in an opened archive, whatever its format: an mbox file
    holds several, split on its `From ` separator lines (the separator itself
    dropped); a Maildir++ or `.eml` entry is one message. Index files — the EML
    manifest, Maildir++'s `subscriptions` — are not messages."""
    out = []
    for name, data in entries.items():
        if (name.endswith("/") or name.endswith(".json")
                or name.rsplit("/", 1)[-1] == "subscriptions"):
            continue
        if name.endswith(".mbox"):
            parts = re.split(rb"(?m)^From ", data)
            out.extend(p.split(b"\n", 1)[1] if b"\n" in p else b"" for p in parts[1:])
        else:
            out.append(data)
    return out


def _the_message(entries: dict[str, bytes], nonce: str) -> bytes:
    hits = [m for m in _messages_in(entries) if nonce.encode() in m]
    assert len(hits) == 1, (
        f"expected the one message tagged {nonce!r} in the archive, found {len(hits)}; "
        f"entries: {sorted(entries)}"
    )
    return hits[0]


def _header_fields(message: bytes) -> list[tuple[str, bytes]]:
    """``[(lower-cased field name, the field's raw bytes, continuations
    included)]`` of a message's header section — up to the first empty line,
    LF or CRLF framed, as § Wizard steps step 2's strip frames it."""
    fields: list[tuple[str, bytes]] = []
    for line in re.split(rb"\r?\n", message):
        if line == b"":
            break
        if line[:1] in (b" ", b"\t") and fields:
            name, raw = fields[-1]
            fields[-1] = (name, raw + b"\n" + line)
            continue
        name, _, _ = line.partition(b":")
        fields.append((name.strip().decode("latin-1").lower(), line))
    return fields


def _strip_set() -> set[str]:
    """§ Wizard steps step 2's **Strip set**, read from the goal doc itself —
    the list is the authority (the Rust constant is pinned to it the same way),
    so a header added there is asserted here with no second copy to forget."""
    doc = Path(__file__).resolve().parents[3] / "docs" / "goal" / "behavior" / "mail-export.md"
    text = doc.read_text(encoding="utf-8")
    segment = text.split("**Strip set:**", 1)[1].split("**Why", 1)[0]
    names = {n.lower() for n in re.findall(r"`([A-Za-z0-9-]+)`", segment)}
    assert {"received", "authentication-results", "x-originating-ip"} <= names, (
        f"the goal doc's strip set did not parse: {sorted(names)}"
    )
    return names


def _utc_day() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")


@pytest.mark.feature("mailbox-export")
def test_mail_export_scope_starts_without_trash_and_junk_and_leaves_out_a_deselected_mailbox(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """You choose which mailboxes to export: every mailbox but Trash and Junk
    starts selected, and one you leave out is not in the archive
    (`mail-export.md` § Wizard steps step 2).

    Two messages in two mailboxes — one in INBOX, one the user's own filter
    filed into Archive — so leaving Archive out is checked against a message
    that WOULD be there: the archive keeps the INBOX one and holds neither the
    Archive one nor any Archive entry at all.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportscope"
    )
    kept, _ = _deliver_tagged(app, mail_bridge_mta, recipient)
    left_out = _deliver_into_archive(app, mail_bridge_mta, nest_instance, test_user, recipient)

    export = _open_wizard(app, nest_instance, test_user)
    export.next()  # Format → Scope
    offered = export.mailbox_names()
    listed = [m["name"] for m in _mailboxes(nest_instance, test_user)]
    assert offered == listed, f"the Scope step must offer every mailbox: {offered} vs {listed}"
    assert _DESELECTED_BY_DEFAULT <= set(offered), offered
    wrong = {n: export.mailbox_selected(n) for n in offered
             if export.mailbox_selected(n) == (n in _DESELECTED_BY_DEFAULT)}
    assert not wrong, (
        f"every mailbox but Trash and Junk must start selected; these do not: {wrong}"
    )

    export.toggle_mailbox("Archive")
    assert not export.mailbox_selected("Archive"), "Archive did not deselect"
    export.next()  # Scope → Confirm
    export.start()
    _wait_done(app, export)
    entries = _download_entries(app, export, tmp_path)
    blob = b"".join(entries.values())
    assert kept.encode() in blob, (
        f"the INBOX message (tagged {kept!r}) is missing — the export dropped a "
        f"selected mailbox; entries: {sorted(entries)}"
    )
    assert left_out.encode() not in blob, (
        f"the Archive message (tagged {left_out!r}) is in the archive although "
        f"Archive was left out; entries: {sorted(entries)}"
    )
    assert not [n for n in entries if "Archive" in n], (
        f"the archive carries an entry for the deselected Archive: {sorted(entries)}"
    )

    # Leave the wizard as the next test expects it: the shared machine keeps a
    # mailbox's checkbox across sessions (`refresh` keeps any selection already
    # made), so re-select Archive through the Scope step, as a user would.
    export.discard()
    assert export.is_page_visible(), f"Discard did not return to Format: {export.error_text()!r}"
    export.next()
    export.toggle_mailbox("Archive")
    assert export.mailbox_selected("Archive")
    export.back()


@pytest.mark.feature("mailbox-export")
def test_mail_export_date_range_takes_whole_days_by_arrival_and_discard_removes_the_export(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """A date range takes in each end's whole day, and a message is placed by
    when it ARRIVED, not by the date its sender wrote (`mail-export.md`
    § Wizard steps step 2); Discard removes a finished export from the nest at
    once (step 5).

    The delivered message says it was written on 2026-09-21 and arrives today.
    A range of today alone takes it — so both ends span the whole day, or a
    message that arrived after midnight would fall outside — and a range of
    2026-09-21 alone does not, though that is the day its `Date:` names.
    Between the two exports, the first is discarded: its blob leaves the nest's
    disk and its download URL stops answering, while the wizard is back on its
    Format step.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportrange"
    )
    day_before = _utc_day()
    nonce, _ = _deliver_tagged(app, mail_bridge_mta, recipient)
    day_after = _utc_day()  # the same day, unless the delivery crossed midnight UTC

    # ── The arrival day: in.
    export = _open_wizard(app, nest_instance, test_user)
    export.next()
    export.set_date_from(day_before)
    export.set_date_to(day_after)
    export.next()
    export.start()
    _wait_done(app, export)
    entries = _download_entries(app, export, tmp_path)
    assert nonce.encode() in b"".join(entries.values()), (
        f"a range of the arrival day ({day_before} … {day_after}) left out a message "
        f"that arrived that day; entries: {sorted(entries)}"
    )

    # ── Discard: gone from the nest now, not in thirty days.
    first = _newest_completed_session(nest_instance, test_user)
    assert _blob_files(nest_instance, first["session_id"]), (
        "precondition: the finished export's blob must be on the nest's disk"
    )
    export.discard()
    assert export.is_page_visible(), (
        f"Discard did not return the wizard to Format: {export.error_text()!r}"
    )
    assert not _blob_files(nest_instance, first["session_id"]), (
        "Discard left the export's blob on the nest's disk"
    )
    gone = _get_blob(nest_instance, test_user, first["download_url"])
    assert gone.status_code == 404, (
        f"a discarded export still downloads: HTTP {gone.status_code}"
    )

    # ── The day the sender wrote: out.
    export.next()
    export.set_date_from(_SENDER_DAY)
    export.set_date_to(_SENDER_DAY)
    export.next()
    export.start()
    _wait_done(app, export)
    entries = _download_entries(app, export, tmp_path)
    assert nonce.encode() not in b"".join(entries.values()), (
        f"a range of {_SENDER_DAY} — the day the message's Date: header names, not "
        f"the day it arrived — took it in; the range must go by arrival"
    )


@pytest.mark.feature("mailbox-export")
def test_mail_export_refuses_a_date_that_is_not_a_day_or_a_range_that_runs_backwards(
    logged_in_app, nest_instance, test_user
):
    """A date that is not a real day, or a start date after the end date, is
    refused before the export starts — never quietly treated as no range at all
    (`mail-export.md` § Wizard steps step 2: "refuses `Start` before any session
    exists").

    For each bad range the user presses Start: the page says why, it is still
    on the Confirm step, and the nest holds no export from it.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    export = _open_wizard(app, nest_instance, test_user)
    export.next()
    bad = [
        ("2026-02-30", ""),            # not a real day
        ("", "2026-13-01"),            # not a real month
        ("21/09/2026", ""),            # not written YYYY-MM-DD
        ("2026-10-02", "2026-10-01"),  # since after until
    ]
    try:
        for since, until in bad:
            export.set_date_from(since)
            export.set_date_to(until)
            export.next()  # Scope → Confirm
            export.start()
            refusal = wait_until(
                lambda: export.error_text() or None,
                MLS_HANDSHAKE_S,
                diagnose=lambda: f"Start with {since!r} … {until!r} surfaced no refusal",
            )
            sessions = [s for s in list_sessions(nest_instance, test_user["signing_key"])
                        if s.get("state") in _LIVE_STATES]
            assert sessions == [], (
                f"Start with {since!r} … {until!r} opened an export ({refusal!r}): {sessions!r}"
            )
            assert app.driver.is_visible("mail-export-start-button"), (
                f"Start with {since!r} … {until!r} left the Confirm step ({refusal!r})"
            )
            export.back()  # Confirm → Scope
    finally:
        # Leave no bad range behind for the next test: the machine keeps both
        # fields across a visit.
        export.set_date_from("")
        export.set_date_to("")
        export.back()


@pytest.mark.feature("mailbox-export")
@pytest.mark.parametrize("fmt", ["mbox", "maildir", "eml"])
def test_mail_export_strip_removes_transit_headers_and_keeps_the_senders(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path, fmt
):
    """Transit headers are kept unless you ask to strip them; stripping removes
    every header a relaying or receiving server stamped to record the path the
    message took, in whichever format you chose, and keeps everything the sender
    wrote — the sender's own DKIM signature included (`mail-export.md`
    § Wizard steps step 2, the strip set and its kept list).

    The message carries a stamp from several strip-set families as it reaches
    the nest, plus whatever the nest's own MTA adds on receipt, and a
    DKIM-Signature. The strip set the archive is checked against is read from
    the goal doc, so no header there escapes the assertion. Each format also
    shows its own archive shape here (§ Format choices): `.mbox` files, a
    Maildir++ `cur/` tree, or `.eml` files with their `manifest.json`.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    label = {"mbox": S.mail_export.format_mbox, "maildir": S.mail_export.format_maildir,
             "eml": S.mail_export.format_eml}[fmt]
    dkim = ("DKIM-Signature: v=1; a=rsa-sha256; d=external.test; s=sel; "
            "h=from:to:subject:date; bh=YmFyYmF6; b=Zm9vYmFyYmF6")
    transit = [
        "Received: from relay.external.test (relay.external.test [192.0.2.10]) "
        "by mx.external.test; Mon, 21 Sep 2026 11:59:00 +0000",
        "X-Originating-IP: [192.0.2.44]",
        "X-Forwarded-For: 192.0.2.45",
        "Authentication-Results: mx.external.test; spf=pass smtp.mailfrom=external.test",
        "ARC-Seal: i=1; a=rsa-sha256; d=external.test; s=arc; cv=none; b=Zm9v",
        "X-Virus-Scanned: by scanner.external.test",
    ]
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, f"e2eexportstrip{fmt}"
    )
    nonce, subject = _deliver_tagged(
        app, mail_bridge_mta, recipient, extra_headers=[*transit, dkim]
    )

    export = _open_wizard(app, nest_instance, test_user)
    export.select_format(label)
    export.next()  # Format → Scope
    assert not export.strip_headers_on(), "the transit-header strip must start off"

    if fmt == "mbox":
        # ── Off: the transit trail is kept.
        export.next()
        export.start()
        _wait_done(app, export)
        kept = {n for n, _ in _header_fields(
            _the_message(_download_entries(app, export, tmp_path), nonce))}
        assert {"received", "x-originating-ip", "authentication-results"} <= kept, (
            f"with the strip off the transit headers must be kept; the message has {sorted(kept)}"
        )
        export.discard()
        assert export.is_page_visible(), f"Discard did not return to Format: {export.error_text()!r}"
        export.select_format(label)
        export.next()

    # ── On: every strip-set header is gone, everything the sender wrote stays.
    export.toggle_strip_headers()
    assert export.strip_headers_on(), "the strip toggle did not turn on"
    export.next()
    export.start()
    _wait_done(app, export)
    entries = _download_entries(app, export, tmp_path)
    names = sorted(entries)
    if fmt == "mbox":
        assert any(n.endswith(".mbox") for n in names), f"no .mbox file in an mbox export: {names}"
    elif fmt == "maildir":
        assert any("/cur/" in n for n in names), f"no Maildir++ cur/ tree: {names}"
    else:
        assert (any(n.rsplit("/", 1)[-1] == "manifest.json" for n in names)
                and any(n.endswith(".eml") for n in names)), (
            f"an EML export must hold .eml files and manifest.json: {names}"
        )

    message = _the_message(entries, nonce)
    fields = _header_fields(message)
    present = {n for n, _ in fields}
    leaked = sorted(present & _strip_set())
    assert not leaked, (
        f"stripping left transit headers in the {fmt} archive: {leaked}\n"
        f"  header section: {[raw for _, raw in fields]}"
    )
    assert {"from", "to", "subject", "message-id", "date", "mime-version",
            "content-type", "dkim-signature"} <= present, (
        f"stripping removed a header the sender wrote: {sorted(present)}"
    )
    assert any(raw.replace(b"\r", b"") == dkim.encode() for n, raw in fields
               if n == "dkim-signature"), (
        "the sender's DKIM-Signature must survive the strip byte for byte, or the "
        "shared archive can no longer be checked as unaltered"
    )
    assert f"Subject: {subject}".encode() in message
    assert f"The {nonce} body must survive the export unchanged.".encode() in message


@pytest.mark.feature("mailbox-export")
def test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """While an export runs you see how many messages are exported, skipped and
    failed, the time elapsed and the time remaining, and each mailbox's own
    progress; you can pause it and resume it later; and when it finishes you see
    how many messages were exported, how long it took and how large the archive
    is (`mail-export.md` § Wizard steps steps 4 and 5).

    The run is held at its second page read (`pass_first=1`), so the first
    mailbox with mail — Archive, which sorts ahead of INBOX — is exported and
    the export is still `running`: the one state in which a time remaining
    exists. Pause is pressed there; the hold is released, and the loop finishes
    its page and pauses the session itself. Resume continues the SAME stream
    (generation 0 — nothing was restarted) to Done, and the archive holds both
    messages.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    port = nest_instance["port"]
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportprogress"
    )
    in_archive = _deliver_into_archive(app, mail_bridge_mta, nest_instance, test_user, recipient)
    in_inbox, _ = _deliver_tagged(app, mail_bridge_mta, recipient)

    export = _open_wizard(app, nest_instance, test_user)
    export.next()
    export.next()
    selected = [m for m in _mailboxes(nest_instance, test_user)
                if m["name"] not in _DESELECTED_BY_DEFAULT]
    total = sum(int(m.get("exists") or 0) for m in selected)
    assert selected[0]["name"] == "Archive" and int(selected[0]["exists"]) >= 1, (
        f"precondition: the walk must start in Archive, holding mail: {selected!r}"
    )

    running = _template_pattern(
        lambda summary, elapsed, remaining: S.mail_export.progress_remaining_fmt(
            summary=summary, elapsed=elapsed, remaining=remaining),
        summary=".+", elapsed=".+?", remaining=".+?",
    )
    counts = _template_pattern(
        lambda exported, total, skipped, errored: S.mail_export.progress_summary_fmt(
            exported=exported, total=total, skipped=skipped, errored=errored),
        exported="[0-9]+", total="[0-9]+", skipped="[0-9]+", errored="[0-9]+",
    )

    arm_rpc_hold(port, _FETCH_KIND, pass_first=1)
    try:
        export.start()
        wait_for_held_rpc(
            port, _FETCH_KIND,
            diagnose=lambda: f"error-message: {export.error_text()!r}; "
                             f"progress: {_progress_or_blank(export)!r}",
        )
        session = _the_live_session(nest_instance, test_user)
        assert session["state"] == "running", session

        # ── Counts, elapsed and remaining. The Progress screen repaints from
        # the drive loop's own snapshot, so wait for the first page to show.
        summary = wait_until(
            lambda: (s := _progress_or_blank(export)) and running.match(s) and s or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the running export never showed elapsed and remaining time: "
                f"{_progress_or_blank(export)!r}"
            ),
        )
        c = counts.match(running.match(summary).group("summary"))
        assert c, f"the counts half of the progress summary is malformed: {summary!r}"
        exported = int(c.group("exported"))
        assert int(c.group("total")) == total, (
            f"total is not the selected mailboxes' {total}: {summary!r}"
        )
        assert 1 <= exported < total, f"one page in, the count must be part way: {summary!r}"
        assert (c.group("skipped"), c.group("errored")) == ("0", "0"), summary

        # ── Each mailbox's own progress: one row per selected mailbox, in the
        # walk's order, Archive's showing what has been exported of it.
        rows = export.mailbox_progress()
        assert [n for n, _ in rows] == [m["name"] for m in selected], rows
        assert rows[0][1] == f"{exported}/{selected[0]['exists']}", rows
        for (name, progress), m in zip(rows[1:], selected[1:]):
            assert progress == f"0/{m['exists']}", (name, progress, rows)
        assert export.error_log() == "", f"a healthy run logs no error: {export.error_log()!r}"

        # ── Pause, pressed while the loop is between pages.
        export.pause()
    finally:
        release_rpc_hold(port, _FETCH_KIND)
    paused = _wait_session_state(
        nest_instance, test_user, session["session_id"], "paused",
        "Pause was pressed", export,
    )
    assert int(paused.get("stream_generation") or 0) == 0, paused
    wait_until(
        lambda: (s := _progress_or_blank(export)) and not running.match(s) and s or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"a paused export still counts down: {_progress_or_blank(export)!r}",
    )

    # ── Resume: the same stream, carried on to the end.
    export.resume()
    done_summary = _wait_done(app, export)
    done = _newest_completed_session(nest_instance, test_user)
    assert done["session_id"] == session["session_id"], (
        f"Resume finished a different export: {done!r} vs {session!r}"
    )
    assert int(done.get("stream_generation") or 0) == 0, (
        f"a warm Resume must continue the stream, not restart it: {done!r}"
    )

    # ── The Done card: exported count, the archive's size, the time it took.
    m = _template_pattern(
        lambda exported, format, bytes, elapsed: S.mail_export.done_summary_timed_fmt(
            exported=exported, format=format, bytes=bytes, elapsed=elapsed),
        exported="[0-9]+", format=".+?", bytes="[0-9]+", elapsed=".+?",
    ).match(done_summary)
    assert m, f"the Done card does not say count, size and time: {done_summary!r}"
    assert int(m.group("exported")) == total, (done_summary, total)
    assert m.group("format") == S.mail_export.format_mbox, done_summary
    assert int(m.group("bytes")) == int(done["blob_bytes"]), (done_summary, done)

    blob = b"".join(_download_entries(app, export, tmp_path).values())
    assert in_archive.encode() in blob and in_inbox.encode() in blob, (
        "the paused and resumed export must hold every message — the one exported "
        "before the pause and the one after it"
    )


@pytest.mark.feature("mailbox-export")
def test_mail_export_failure_is_shown_with_its_reason_in_the_error_log(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """While an export runs you see the reason for every message that could not
    be exported (`mail-export.md` § Wizard steps step 4's error log). Under
    § An unopenable record fails the session there is no per-message skip to
    list: a message that cannot be taken fails the whole export, and its one
    reason — the one the nest records on the session — is the error log's
    line.

    The nest turns the loop's first read away (`refuse_rpc`), the failure no
    ordinary mailbox provokes on demand.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    port = nest_instance["port"]
    export, _nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportfail"
    )
    before = {s.get("session_id") for s in list_sessions(nest_instance, test_user["signing_key"])}
    refuse_rpc(port, _FETCH_KIND)
    try:
        export.start()
        session = wait_until(
            lambda: next((s for s in list_sessions(nest_instance, test_user["signing_key"])
                          if s.get("session_id") not in before
                          and s.get("state") == "errored"), None),
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the export never failed. sessions: "
                f"{list_sessions(nest_instance, test_user['signing_key'])!r}\n"
                f"  error-message: {export.error_text()!r}"
            ),
        )
    finally:
        release_rpc_hold(port, _FETCH_KIND)
    reason = session.get("error_reason") or ""
    assert reason, f"a failed export must record why: {session!r}"
    log = wait_until(
        lambda: export.error_log() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the error log stayed empty; the nest recorded {reason!r}",
    )
    assert log == reason, (
        f"the error log must be the failure's one reason, as recorded:\n"
        f"  shown:    {log!r}\n  recorded: {reason!r}"
    )


@pytest.mark.feature("mailbox-export")
def test_mail_export_an_unopenable_message_fails_the_whole_export_naming_its_mailbox_and_message(
    dedicated_actor_app, mail_bridge_mta, nest_instance
):
    """If a message cannot be opened, the whole export fails and says which
    mailbox and which message — never an archive quietly missing it
    (`mail-export.md` § An unopenable record fails the session).

    Two messages arrive through the real MTA and both surface decrypted; then
    the second one's sealed body is damaged where it rests on nest disk
    (`helpers.segment_at_rest.unopenable_mail_record` — a tampered envelope,
    the one cause the goal names that the product can still reach, now that an
    account keeps every mail key it ever retired). The nest still serves the
    record; only the app, opening it, can tell. The export the user then starts
    must stop at that message: the session fails with a reason naming the
    mailbox and the UID, the error log shows exactly that reason, the wizard
    never reaches Done, and nothing lands in the download dir. Skipping the
    record would reach Done with a one-message archive, and this would go red.

    A DEDICATED actor, never the shared ``test_user``: the damaged record stays
    in its owner's INBOX for good, and every later export of that mailbox fails
    on it by design.
    """
    from helpers.segment_at_rest import unopenable_mail_record

    app, user = dedicated_actor_app
    _require_driving_app(app.driver)
    key = user["signing_key"]
    recipient = route_inbound_mail_to_app(
        app, mail_bridge_mta, nest_instance, user,
        f"e2eexportunopenable{user['actor_id_hex'][:10]}",
    )
    _deliver_tagged(app, mail_bridge_mta, recipient)
    _deliver_tagged(app, mail_bridge_mta, recipient)
    try:
        mailbox, uid = unopenable_mail_record(nest_instance, bytes.fromhex(user["actor_id_hex"]))
    except PermissionError:
        skip_environment(
            "the nest's data dir is not writable by the harness in this nest mode, "
            "so no record can be damaged at rest"
        )

    export = _open_wizard(app, nest_instance, user)
    export.next()   # Format → Scope
    export.next()   # Scope → Confirm
    before = {s.get("session_id") for s in list_sessions(nest_instance, key)}
    downloads = download_listing(app.driver)
    export.start()
    failed = wait_until(
        lambda: next((s for s in list_sessions(nest_instance, key)
                      if s.get("session_id") not in before
                      and s.get("state") in ("errored", "completed")), None),
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the export neither failed nor finished. sessions: "
            f"{list_sessions(nest_instance, key)!r}\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  progress: {_progress_or_blank(export)!r}"
        ),
    )
    assert failed["state"] == "errored", (
        f"an export over a message this device cannot open must fail, never "
        f"finish without it: {failed!r}"
    )

    # ── The reason names the mailbox and the message — the opener's branch,
    # not a fetch the nest refused.
    reason = failed.get("error_reason") or ""
    assert reason.startswith(f"{mailbox}: uid {uid} "), (
        f"the failure must name {mailbox} and uid {uid}, the message that could "
        f"not be opened: {reason!r}"
    )
    log = wait_until(
        lambda: export.error_log() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the error log stayed empty; the nest recorded {reason!r}",
    )
    assert log == reason, (
        f"the error log must show the failure's reason, as recorded:\n"
        f"  shown:    {log!r}\n  recorded: {reason!r}"
    )

    # ── Nothing that could pass for the mailbox: no Done step, no file.
    assert app.driver.is_absent("mail-export-done-summary"), (
        "a failed export must not reach the Done step"
    )
    assert download_listing(app.driver) == downloads, (
        f"a failed export left files in the download dir: "
        f"{sorted(set(download_listing(app.driver)) - set(downloads))}"
    )


def _relaunch_onto_the_export(app):
    """Close the app and open it again (the driver's own cold relaunch), then
    go back to the export page. Returns the new page's actions."""
    from actions.mail_export import MailExportActions

    relaunch(app.driver)
    export = MailExportActions(app.driver)
    export.navigate()
    return export


@pytest.mark.feature("mailbox-export")
def test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here(
    logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """Closing the app mid-export and opening it again brings you back to the
    same export's progress; an app not running the export does not pause it;
    and resuming it there runs it again from the first message, as the same
    export (`mail-export.md` § Wizard steps step 4, § Resume — warm continues
    the stream, cold restarts it).

    The app is closed while its export is held between pages, so the session is
    still `running` with nobody left to drive it. Back in the app, the export
    page opens on that export's Progress; Pause is refused with a reason and
    the session stays `running` (pausing it would stall whichever device is
    driving it); Resume restarts it on the same session under a new stream
    generation, and it runs to Done holding the message.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    require_relaunchable(app.driver)
    port = nest_instance["port"]
    export, nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportrestart"
    )

    arm_rpc_hold(port, _FETCH_KIND)
    try:
        export.start()
        wait_for_held_rpc(port, _FETCH_KIND,
                          diagnose=lambda: f"error-message: {export.error_text()!r}")
        session = _the_live_session(nest_instance, test_user)
        export = _relaunch_onto_the_export(app)
    finally:
        # The closed app's parked read answers nobody.
        release_rpc_hold(port, _FETCH_KIND)

    # ── Back on the same export's progress.
    wait_until(
        lambda: app.driver.is_visible("mail-export-progress-summary") or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the reopened app did not come back to the export's progress.\n"
            f"  Format step showing: {app.driver.is_visible('mail-export-format-picker')}\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  session: {_session(nest_instance, test_user, session['session_id'])!r}"
        ),
    )
    assert _session(nest_instance, test_user, session["session_id"])["state"] == "running"

    # ── Pause from an app that is not running it: refused, nothing paused.
    assert export.error_text() == "", f"an error before Pause: {export.error_text()!r}"
    export.pause()
    refusal = wait_until(
        lambda: export.error_text() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"Pause in an app not running the export said nothing; session "
            f"{_session(nest_instance, test_user, session['session_id'])!r}"
        ),
    )
    still = _session(nest_instance, test_user, session["session_id"])
    assert still["state"] == "running", (
        f"Pause from an app not running the export paused it ({refusal!r}): {still!r}"
    )

    # ── Resume here: the same export, run again from its first message.
    export.resume()
    _wait_done(app, export)
    done = _newest_completed_session(nest_instance, test_user)
    assert done["session_id"] == session["session_id"], (
        f"the restarted export is not the same export: {done!r} vs {session!r}"
    )
    assert int(done.get("stream_generation") or 0) == 1, (
        f"a Resume where the export was not running must restart it on a new "
        f"stream: {done!r}"
    )
    blob = b"".join(_download_entries(app, export, tmp_path).values())
    assert nonce.encode() in blob, "the restarted export is missing the message"


@pytest.mark.feature("mailbox-export")
def test_mail_export_a_failed_export_shows_as_failed_after_a_restart_and_dismiss_clears_it(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """A failed export is still there when you come back: closing the app and
    opening it again lands on that export's Progress step with its reason in the
    error log, offering none of Pause, Resume or Cancel — only **Dismiss**, which
    removes the record and returns the wizard to its first step
    (`mail-export.md` § A failed export is a record the wizard shows until the
    user moves on).

    The failure is provoked as `test_mail_export_failure_is_shown_with_its_reason_in_the_error_log`
    provokes it: the nest turns the loop's first read away (`refuse_rpc`).
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    require_relaunchable(app.driver)
    port = nest_instance["port"]
    key = test_user["signing_key"]
    export, _nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportfailrelaunch"
    )
    before = {s.get("session_id") for s in list_sessions(nest_instance, key)}
    refuse_rpc(port, _FETCH_KIND)
    try:
        export.start()
        failed = wait_until(
            lambda: next((s for s in list_sessions(nest_instance, key)
                          if s.get("session_id") not in before
                          and s.get("state") == "errored"), None),
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the export never failed. sessions: {list_sessions(nest_instance, key)!r}\n"
                f"  error-message: {export.error_text()!r}"
            ),
        )
    finally:
        release_rpc_hold(port, _FETCH_KIND)
    reason = failed.get("error_reason") or ""
    assert reason, f"a failed export must record why: {failed!r}"

    # ── After a restart: the failure, on its Progress step, with its reason.
    export = _relaunch_onto_the_export(app)
    log = wait_until(
        lambda: export.error_log() or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the reopened app does not show the failed export.\n"
            f"  Format step showing: {app.driver.is_visible('mail-export-format-picker')}\n"
            f"  Progress step showing: {app.driver.is_visible('mail-export-progress-summary')}\n"
            f"  error-message: {export.error_text()!r}\n"
            f"  recorded reason: {reason!r}"
        ),
    )
    assert log == reason, (
        f"the error log must be the failure's one reason, as recorded:\n"
        f"  shown:    {log!r}\n  recorded: {reason!r}"
    )
    assert app.driver.is_visible("mail-export-progress-summary"), (
        "a failed export is shown on its Progress step"
    )
    offered = [i for i in ("mail-export-pause-button", "mail-export-resume-button",
                           "mail-export-cancel-button") if app.driver.is_visible(i)]
    assert not offered, f"a failed export offers none of the three controls: {offered}"
    dismiss = app.driver.get_text("mail-export-discard-button")
    assert dismiss == S.mail_export.dismiss_failed_button, (
        f"a failed export's one control is Dismiss, labelled as such: {dismiss!r}"
    )

    # ── Dismiss: the record goes, the wizard starts again.
    export.discard()
    assert export.is_page_visible(), (
        f"Dismiss did not return the wizard to Format: {export.error_text()!r}"
    )
    left = [s for s in list_sessions(nest_instance, key)
            if s.get("session_id") == failed["session_id"]]
    assert not left, f"Dismiss left the failed export listed: {left!r}"


@pytest.mark.feature("mailbox-export")
def test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """Cancelling an export asks you to confirm, and a cancelled export leaves
    no partial archive behind on your nest — from any app of yours, the one
    driving it or not (`mail-export.md` § Wizard steps step 4, § Resume: the
    user's own Cancel "applies unconditionally — disposing of an export from any
    device stays the user's right").

    The export is held after its last upload and before it is finalized, so
    every byte of the archive rests on the nest under a session still
    `running` — the partial archive a cancel must remove. The app that started
    it is then closed and reopened, so the Cancel comes from an app that is not
    driving it. The first press only asks; the second cancels, and the blob is
    gone. Releasing the closed app's parked finalize afterwards does not bring
    the export back.
    """
    from actions.mail_export import MailExportActions

    app = logged_in_app
    _require_driving_app(app.driver)
    require_relaunchable(app.driver)
    port = nest_instance["port"]
    export, _nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportcancel"
    )

    arm_rpc_hold(port, _FINALIZE_KIND)
    try:
        export.start()
        wait_for_held_rpc(port, _FINALIZE_KIND,
                          diagnose=lambda: f"error-message: {export.error_text()!r}")
        session = _the_live_session(nest_instance, test_user)
        sid = session["session_id"]
        assert session["state"] == "running", session
        assert _blob_files(nest_instance, sid), (
            "precondition: the unfinished export's archive must be on the nest's disk"
        )

        export = _relaunch_onto_the_export(app)
        wait_until(
            lambda: app.driver.is_visible("mail-export-cancel-button") or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: f"the reopened app shows no running export: {export.error_text()!r}",
        )

        # ── The first press asks; nothing is cancelled yet.
        export.arm_cancel()
        assert export.cancel_button_text() == S.common.confirm_q, (
            f"the first Cancel press must ask for confirmation, the button reads "
            f"{export.cancel_button_text()!r}"
        )
        assert _session(nest_instance, test_user, sid)["state"] == "running"
        assert _blob_files(nest_instance, sid), "the unconfirmed Cancel removed the archive"

        # ── The second press cancels, and the partial archive goes with it.
        export.confirm_cancel()
        assert MailExportActions(app.driver).is_page_visible(), (
            f"a cancelled export must return the wizard to Format: {export.error_text()!r}"
        )
        _wait_session_state(nest_instance, test_user, sid, "cancelled",
                            "Cancel was confirmed", export)
        assert not _blob_files(nest_instance, sid), (
            f"the cancelled export left its partial archive on the nest: "
            f"{_blob_files(nest_instance, sid)}"
        )
    finally:
        release_rpc_hold(port, _FINALIZE_KIND)

    # The closed app's finalize, parked all along, now reaches the nest: it
    # must not complete what the user cancelled.
    wait_until(
        lambda: rpc_hold_status(port, _FINALIZE_KIND).get("holding", 0) == 0 or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the parked finalize never woke: {rpc_hold_status(port, _FINALIZE_KIND)!r}",
    )
    after = _session(nest_instance, test_user, sid)
    assert after and after["state"] == "cancelled", f"the cancelled export came back: {after!r}"
    assert not _blob_files(nest_instance, sid)


# ── Another of your devices (mailbox-export outcomes 16, 17 and 20) ──
#
# The relaunch journeys above reach the machine state a second device does — no
# parked run in this app — but on the same device's store. These run on
# `alice_second_device`: the same identity in a second app process, signed in
# the way `devices.md` § Adding a Second Device adds one (the same secret key, a
# distinct device id) with a fresh, empty client store, so whatever it opens it
# opens with its own key custody, never a store the first device wrote.

# The apps `alice_second_device` launches a second seat of (its own docstring);
# the web second device is a different fixture, and the phones have none.
_SECOND_DEVICE_APPS = {"tui", "linux", "macos", "windows"}


def _second_device(request, app):
    """Sign in a second device of the session user and open its export page.
    Requested only after the first device routed mail, so the mailbox — and the
    key the export is sealed to — exists when it signs in. Returns
    ``(device_b, export actions on device B)``."""
    from actions.mail_export import MailExportActions

    if app_name(app.driver) not in _SECOND_DEVICE_APPS:
        skip_unbuilt(
            app.driver,
            surface="a second signed-in device of the same user",
            detail="alice_second_device launches a second seat on tui, linux, "
                   "macOS and windows only",
            tracked="tests/e2e-unified/conftest.py::alice_second_device",
        )
    device_b, _driver_b = request.getfixturevalue("alice_second_device")
    export_b = MailExportActions(device_b.driver)
    export_b.navigate()
    return device_b, export_b


@pytest.mark.feature("mailbox-export")
def test_mail_export_finished_on_one_device_downloads_and_opens_on_another(
    request, logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """A finished export can be downloaded and opened from any of your devices,
    not only the one that ran it (`mail-export.md` § Wizard steps step 5, § Key
    material: the wrapped session key is stored "so *any* of the user's clients
    (not just the one that started the session) can fetch it, unwrap under the
    actor key, and decrypt the download").

    Device A runs the export to Done. Device B — signed in afterwards, with a
    store A never wrote — opens the export page, which adopts the newest export
    not yet discarded and so opens on that export's Done step; Download there
    saves an archive B unsealed with its own key custody, holding the message.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    _export, nonce, _subject = _export_to_done(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportotherdevice"
    )
    done = _newest_completed_session(nest_instance, test_user)

    device_b, export_b = _second_device(request, app)
    wait_until(
        lambda: device_b.driver.is_visible("mail-export-done-summary") or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: (
            f"the second device's export page did not open on the finished export.\n"
            f"  Format step showing: {device_b.driver.is_visible('mail-export-format-picker')}\n"
            f"  error-message: {export_b.error_text()!r}\n"
            f"  newest completed session: {done!r}"
        ),
    )
    assert export_b.done_summary() != "", "the second device's Done step must summarize it"

    blob = b"".join(_download_entries(device_b, export_b, tmp_path).values())
    assert nonce.encode() in blob, (
        "the archive the second device downloaded does not hold the exported message"
    )
    assert export_b.error_text() == "", (
        f"the second device's download reported an error: {export_b.error_text()!r}"
    )


@pytest.mark.feature("mailbox-export")
def test_mail_export_another_device_cannot_pause_it_and_resuming_there_restarts_it(
    request, logged_in_app, mail_bridge_mta, nest_instance, test_user, tmp_path
):
    """On another of your devices, an export the first device is running does
    not pause — pausing it would stall the device driving it — and resuming it
    there runs it again from the first message, as the same export, while the
    first device's run is shut out (`mail-export.md` § Resume — warm continues
    the stream, cold restarts it: a restart "is always honoured from `running`
    or `paused`, and the generation is what shuts the previous driver out").

    Device A's export is held between pages, so the session is `running` with A
    still driving it. Device B opens the export page on that export's Progress;
    its Pause is refused with a reason and the session stays `running`; its
    Resume restarts the session under stream generation 1. Only then is the
    hold released, so A's parked read and B's restarted one both proceed — and
    the export B finishes holds the message under the one key B minted: a
    chunk A sealed under the superseded key would fail the download.
    """
    app = logged_in_app
    _require_driving_app(app.driver)
    port = nest_instance["port"]
    export, nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportotherresume"
    )

    arm_rpc_hold(port, _FETCH_KIND)
    try:
        export.start()
        wait_for_held_rpc(port, _FETCH_KIND,
                          diagnose=lambda: f"error-message: {export.error_text()!r}")
        session = _the_live_session(nest_instance, test_user)
        sid = session["session_id"]

        device_b, export_b = _second_device(request, app)
        wait_until(
            lambda: device_b.driver.is_visible("mail-export-progress-summary") or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the second device did not open on the running export's progress.\n"
                f"  Format step showing: {device_b.driver.is_visible('mail-export-format-picker')}\n"
                f"  error-message: {export_b.error_text()!r}\n"
                f"  session: {_session(nest_instance, test_user, sid)!r}"
            ),
        )

        # ── Pause from a device that is not running it: refused, nothing paused.
        assert export_b.error_text() == "", f"an error before Pause: {export_b.error_text()!r}"
        export_b.pause()
        refusal = wait_until(
            lambda: export_b.error_text() or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"Pause on a device not running the export said nothing; session "
                f"{_session(nest_instance, test_user, sid)!r}"
            ),
        )
        still = _session(nest_instance, test_user, sid)
        assert still["state"] == "running", (
            f"Pause on a device not running the export paused it ({refusal!r}): {still!r}"
        )
        assert int(still.get("stream_generation") or 0) == 0, still

        # ── Resume there: the same export restarted on a new stream, while
        # device A's read is still parked.
        export_b.resume()
        wait_until(
            lambda: (s := _session(nest_instance, test_user, sid))
            and int(s.get("stream_generation") or 0) == 1 and s or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"Resume on the second device never restarted the export.\n"
                f"  session: {_session(nest_instance, test_user, sid)!r}\n"
                f"  error-message: {export_b.error_text()!r}"
            ),
        )
    finally:
        release_rpc_hold(port, _FETCH_KIND)

    _wait_done(device_b, export_b)
    done = _newest_completed_session(nest_instance, test_user)
    assert done["session_id"] == sid, (
        f"the second device finished a different export: {done!r} vs {session!r}"
    )
    assert int(done.get("stream_generation") or 0) == 1, (
        f"the first device's superseded run must not move the export on: {done!r}"
    )
    blob = b"".join(_download_entries(device_b, export_b, tmp_path).values())
    assert nonce.encode() in blob, "the restarted export is missing the message"


@pytest.mark.feature("mailbox-export")
def test_mail_export_another_device_can_cancel_it_and_no_partial_archive_remains(
    request, logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """Any of your devices can cancel an export another one is running, and the
    cancelled export leaves no partial archive behind (`mail-export.md`
    § Resume: the user's own Cancel "applies unconditionally — disposing of an
    export from any device stays the user's right").

    Device A's export is held after its last upload and before it is finalized,
    so the whole archive rests on the nest under a session still `running`.
    Device B opens on that export, and its Cancel — asked, then confirmed —
    cancels it and removes the blob. Releasing A's parked finalize afterwards
    does not bring the export back.
    """
    from actions.mail_export import MailExportActions

    app = logged_in_app
    _require_driving_app(app.driver)
    port = nest_instance["port"]
    export, _nonce, _subject = _deliver_and_open_confirm(
        app, mail_bridge_mta, nest_instance, test_user, "e2eexportothercancel"
    )

    arm_rpc_hold(port, _FINALIZE_KIND)
    try:
        export.start()
        wait_for_held_rpc(port, _FINALIZE_KIND,
                          diagnose=lambda: f"error-message: {export.error_text()!r}")
        session = _the_live_session(nest_instance, test_user)
        sid = session["session_id"]
        assert _blob_files(nest_instance, sid), (
            "precondition: the unfinished export's archive must be on the nest's disk"
        )

        device_b, export_b = _second_device(request, app)
        wait_until(
            lambda: device_b.driver.is_visible("mail-export-cancel-button") or None,
            MLS_HANDSHAKE_S,
            diagnose=lambda: (
                f"the second device shows no running export: {export_b.error_text()!r}; "
                f"session {_session(nest_instance, test_user, sid)!r}"
            ),
        )

        export_b.arm_cancel()
        assert export_b.cancel_button_text() == S.common.confirm_q, (
            f"the first Cancel press must ask for confirmation, the button reads "
            f"{export_b.cancel_button_text()!r}"
        )
        assert _session(nest_instance, test_user, sid)["state"] == "running"

        export_b.confirm_cancel()
        assert MailExportActions(device_b.driver).is_page_visible(), (
            f"a cancelled export must return the second device's wizard to Format: "
            f"{export_b.error_text()!r}"
        )
        _wait_session_state(nest_instance, test_user, sid, "cancelled",
                            "Cancel was confirmed on the second device", export_b)
        assert not _blob_files(nest_instance, sid), (
            f"the cancelled export left its partial archive on the nest: "
            f"{_blob_files(nest_instance, sid)}"
        )
    finally:
        release_rpc_hold(port, _FINALIZE_KIND)

    # Device A's finalize, parked all along, now reaches the nest: it must not
    # complete what the user cancelled on the other device.
    wait_until(
        lambda: rpc_hold_status(port, _FINALIZE_KIND).get("holding", 0) == 0 or None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the parked finalize never woke: {rpc_hold_status(port, _FINALIZE_KIND)!r}",
    )
    after = _session(nest_instance, test_user, sid)
    assert after and after["state"] == "cancelled", f"the cancelled export came back: {after!r}"
    assert not _blob_files(nest_instance, sid)
