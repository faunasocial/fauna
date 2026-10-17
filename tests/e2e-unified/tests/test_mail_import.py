"""E2E coverage for the user-facing mail-import wizard: a person moves their
mail off a foreign IMAP server and into their own nest.

Target state: `docs/goal/behavior/mailbox-migration.md` § UX shape / § Wizard
steps / § Credential handling / § Client-driven streaming model; UX/IDs:
`tests/e2e-unified/ui.yaml` `mail-import` page. tui is the lead app for this
page (`mailbox-migration.md` § Implementation status today).

**Why this file could not exist until now.** § The two TLS modes leaves a source
IMAP session no plaintext variant — the user's foreign-mailbox password crosses
it — and no harness can mint a publicly-chained certificate for `localhost`, so
the compiled app had no way to verify a harness-run source server. Every other
layer of the wizard was built and green while this walk was simply unreachable.
The source-IMAP trust seed closes exactly that
(`e2e-automation-surface-gating.md` § The source-IMAP trust seed); the
`mail_import_source` marker on this module is what turns it on.

**tier_3, and it earns the tier.** Real tui, real shared-Rust IMAP client, real
TLS, real nest, real `fauna.bridges.import_message` over WS-RPC. The only stub
is the third-party server at the far end — which is not a mock of anything of
ours, and is the same reason the scan-gate tests stay tier_3 behind `fake_clamd`.

**Every mutation goes through the app UI** (convention 8): the point of the file
is that a *user* can complete this journey, so nothing here reaches for the
import RPCs directly. The one thing read outside the UI is the fake server's own
record of what was asked of it — external black-box verification, which the
convention explicitly leaves outside the rule, and the only way to tell "the
wizard says 2 imported" apart from "the wizard read 2 messages off the source".
"""

from __future__ import annotations

import os
import socket
import sys
from pathlib import Path

import pytest

from actions.mail_import import MailImportActions
from common.nest import start_nest_in_place, stop_nest
from conftest import _apply_mail_import_source_trust_env, _seeded_environment
from drivers import create_driver
from helpers import budgets
from helpers.app_surface import skip_unbuilt
from helpers.connection import wait_until_online
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_import_evidence import (
    assert_the_password_never_reached_the_nest,
    imap_internaldate,
    import_session_counts,
    mailboxes_over_imap,
    stored_message_count,
)
from helpers.mail_import_ui import (
    MAIL_PASSWORD,
    enable_mail_so_the_nest_can_seal,
    fill_generic_source,
    type_until_it_takes,
)
from helpers.relaunch import relaunch, require_relaunchable
from helpers.waiting import wait_until
from i18n.strings import S

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_imap_source import (  # noqa: E402
    FakeImapSource,
    SourceMailbox,
    sample_message,
    session_cert,
)

pytestmark = [
    pytest.mark.tier_3,
    # The apps that render the wizard, widened one leg at a time as the
    # trickle-down lands (mailbox-migration.md § Implementation status today).
    # tui led 2026-08-28; linux is the first trickle-down leg and windows the
    # second native one (both 2026-08-31); macos + ios are the last two
    # (2026-09-01, one shared FaunaKit `MailImportView`).
    # web carries a per-test `@pytest.mark.web` on the reachability arm ONLY —
    # see the comment on that test for why the other two cannot run there.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # Seeds FAUNA_E2E_IMAP_EXTRA_CA_PEM into every app launch in the session, so
    # the app can verify the certificate `imap_source` serves. Without it the
    # Source step fails at the handshake and every test below reports a TLS
    # error rather than a wizard verdict.
    pytest.mark.mail_import_source,
]

# The source mailbox the wizard imports FROM. Two messages in INBOX and one in
# Archive: enough that "imported everything" and "imported only what was
# scoped" are different observable outcomes, and small enough that the walk is
# about the wizard rather than about throughput.
INBOX_SUBJECTS = ("first old message", "second old message")
ARCHIVE_SUBJECTS = ("an archived message",)

# The old provider's password. Distinctive on purpose: the walk below searches
# everything the nest holds for these bytes (outcome 14), and a short common
# word would be found by accident in some unrelated row.
SOURCE_PASSWORD = "0ld-provider-password-7c41e9"


def _mailboxes() -> list[SourceMailbox]:
    return [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message(s) for s in INBOX_SUBJECTS],
        ),
        SourceMailbox(
            name="Archive",
            messages=[sample_message(s) for s in ARCHIVE_SUBJECTS],
        ),
    ]


def _sized_tail(mailboxes: int, subjects: tuple[str, ...]) -> str:
    """The Confirm summary after its source label, for `subjects`' messages —
    the template the app renders (`mail_import.confirm_summary_sized_fmt`),
    sized as the fake serves them (each well under a KiB, so in bytes)."""
    size = sum(len(sample_message(s)) for s in subjects)
    return S.mail_import.confirm_summary_sized_fmt(
        source="",
        mailboxes=str(mailboxes),
        messages=str(len(subjects)),
        size=S.size.bytes(value=str(size)),
    )


@pytest.fixture
def imap_source():
    """A fresh source server per test.

    Per-test rather than per-module because the assertions read what the client
    *asked* the server for (`logins`, `fetched_uids`), and a shared instance
    would carry a previous test's requests into the next one's evidence.

    The certificate is the session's, not this instance's: the app was launched
    trusting one CA before the first test ran (`_apply_mail_import_source_trust_env`),
    and app drivers are session-scoped, so a per-instance mint would be
    untrusted by the very app that has to connect to it.
    """
    with FakeImapSource(
        _mailboxes(), cert=session_cert(), password=SOURCE_PASSWORD
    ) as source:
        yield source


# What reading a mailbox may ask of a server (`fake_imap_source`'s command
# list, which is `session.rs`'s). Anything else — `SELECT`, `STORE`, `COPY`,
# `MOVE`, `EXPUNGE`, `APPEND`, `CREATE`, `DELETE`, `RENAME` — changes the
# source or could.
_READ_ONLY_VERBS = {"CAPABILITY", "LOGIN", "AUTHENTICATE", "LIST", "EXAMINE", "STATUS", "UID", "LOGOUT"}


def _assert_the_source_was_only_read(source: FakeImapSource) -> None:
    """Outcome 31 (`mailbox-migration.md` § Where the IMAP client runs): the
    import opens every mailbox with `EXAMINE`, never `SELECT`, and reads every
    body with `BODY.PEEK`, which sets no `\\Seen` flag — from the fake's own
    record of every command the client sent, so a command nobody expected is
    caught too."""
    verbs = [c.split(" ", 1)[0] for c in source.commands]
    changing = [c for c in source.commands if c.split(" ", 1)[0] not in _READ_ONLY_VERBS]
    assert not changing, f"the import sent the source {changing}; every command: {source.commands}"
    assert "EXAMINE" in verbs, f"no mailbox was opened read-only: {source.commands}"
    uid_commands = [c for c in source.commands if c.startswith("UID ")]
    assert all(
        c.split(" ", 2)[1].upper() == "FETCH" for c in uid_commands
    ), f"a UID command other than FETCH reached the source: {uid_commands}"
    body_reads = [c for c in uid_commands if "BODY" in c.upper()]
    assert body_reads, f"no body was read at all: {source.commands}"
    assert all("BODY.PEEK[" in c.upper() for c in body_reads), (
        f"a body was read without BODY.PEEK, which marks it read: {body_reads}"
    )


def _connect_journey(app, handle, request, source: FakeImapSource) -> MailImportActions:
    """Mail on, the wizard opened and connected to `source` — the Scope step
    listing every mailbox the source has, ready for the user's choices."""
    login_as_nest_admin(app, handle.nest, dedicated_node_url(app, handle, request))
    enable_mail_so_the_nest_can_seal(app, handle)
    wizard = MailImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()
    fill_generic_source(wizard, source, password=source.password)
    wizard.connect()
    assert wait_until(
        lambda: wizard.mailbox_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the Scope step never listed the source mailboxes; "
        f"error was {wizard.error_text()!r}",
    ) == list(source.mailboxes)
    return wizard


def _walk_to_a_started_import(app, handle, request, source: FakeImapSource) -> MailImportActions:
    """Mail on, then the wizard from Source to Start over every mailbox the
    source lists — the preamble every run-time failure below shares."""
    wizard = _connect_journey(app, handle, request, source)
    wizard.scope_next()
    wait_until(
        wizard.confirm_summary,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"no Confirm summary; error was {wizard.error_text()!r}",
    )
    wizard.start()
    return wizard


# web runs THIS arm and no other. Its `ImportSourceNest` is the honest `Rejected`
# stub until the relay IMAP transport lands (mailbox-migration.md § Implementation
# status today), so `Connect` cannot succeed there and the two walks below are
# genuinely unreachable — not skipped, deselected: the page itself is real on web
# and this test proves it renders, routes, and opens on step 1. Widen the module
# marker to include web (and delete this one) when the transport lands.
@pytest.mark.web
@pytest.mark.feature("mailbox-import")
def test_the_import_wizard_is_reachable_and_opens_on_the_source_step(logged_in_app):
    """The page exists, is routable from Settings, and renders step 1."""
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible(), (
        "the mail-import wizard did not render its source picker; "
        f"page error was {wizard.error_text()!r}"
    )


# `SelectSourceKind` is a cheap, client-only dispatch (module docs on every
# app's mail_import — no RPC), real everywhere `MailImportNest` is, which is
# every app including web: `Connect` is the only stub. So this walk carries its
# own `@pytest.mark.web`, independent of the module's Connect-dependent ones.
@pytest.mark.web
@pytest.mark.feature("mailbox-import")
def test_selecting_a_source_kind_seeds_the_host_and_port_drafts(logged_in_app):
    """Nothing re-seeded the Source-step host/port drafts from the
    snapshot a `SelectSourceKind` dispatch just wrote, so picking Outlook left
    the host field empty and Connect dialed an empty host. Every app now seeds
    them unconditionally on a kind change (apple's `syncDraftsFromSnapshot`
    shape) — including the accepted quirk that Generic, which has no preset of
    its own (§ Wizard steps step 1), keeps whatever the previous kind painted
    rather than blanking the field.
    """
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    wizard.select_source("Outlook / Hotmail / Office365")
    assert wait_until(
        lambda: wizard.host_value() == "outlook.office365.com",
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"host draft={wizard.host_value()!r} port draft={wizard.port_value()!r}",
    ), "the host draft was not seeded from the Outlook preset"
    assert wizard.port_value() == "993"

    wizard.select_source("Generic IMAP")
    assert wait_until(
        lambda: wizard.host_value() == "outlook.office365.com",
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"host draft={wizard.host_value()!r} port draft={wizard.port_value()!r}",
    ), "Generic has no preset — the prior kind's value must stay painted, not blank"
    assert wizard.port_value() == "993"


@pytest.mark.feature("mailbox-import")
def test_a_status_size_source_is_estimated_without_reading_its_mailboxes(
    logged_in_app,
):
    """§ Wizard steps step 4 (outcome 24) against a source that advertises
    RFC 8438's STATUS=SIZE, as a Dovecot does: the Confirm step's message
    total and size come from one STATUS per selected mailbox, and no mailbox
    is EXAMINEd or enumerated to get them — the outside witness is the fake's
    own record of what it was asked. The happy-path walk below is the other
    branch: a source without STATUS=SIZE, sized by enumeration.
    """
    with FakeImapSource(_mailboxes(), cert=session_cert(), status_size=True) as source:
        wizard = MailImportActions(logged_in_app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        fill_generic_source(wizard, source, password=source.password)
        wizard.connect()
        assert wait_until(
            lambda: wizard.mailbox_names(),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "the Scope step never listed the source mailboxes; "
            f"error was {wizard.error_text()!r}",
        ) == ["INBOX", "Archive"]

        wizard.scope_next()
        summary = wait_until(
            wizard.confirm_summary,
            budgets.UI_SETTLE_S,
            diagnose=lambda: "the Confirm step rendered no summary; error was "
            f"{wizard.error_text()!r}",
        )
        assert summary.endswith(_sized_tail(2, INBOX_SUBJECTS + ARCHIVE_SUBJECTS)), (
            f"the confirm summary {summary!r} does not say both mailboxes' "
            "messages and their size are coming"
        )
        assert sorted(source.status_queries) == [
            ("Archive", "MESSAGES SIZE"),
            ("INBOX", "MESSAGES SIZE"),
        ], f"the source was asked STATUS {source.status_queries}"
        assert source.fetched_uids == [], "no body is read to estimate"
        # Back to Source: the app is session-scoped, and the next test starts
        # its walk there.
        wizard.back()
        assert wait_until(lambda: wizard.mailbox_names(), budgets.UI_SETTLE_S)
        wizard.back()
        assert wizard.is_page_visible(), "Back twice did not return to Source"


@pytest.mark.feature("mailbox-import")
def test_a_rejected_password_is_surfaced_and_the_user_retries_in_place(
    logged_in_app, imap_source
):
    """§ Wizard steps 2: a failed connection surfaces the source server's own
    error, and the user retries **without re-entering the connection details**.

    The retry half is the part worth a test — a wizard that resets to a blank
    Source step on every typo is technically correct and unusable, and the
    credential is the one field § Credential handling says lives only in client
    memory, so it is also the only one that *should* have to be retyped.
    """
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    fill_generic_source(wizard, imap_source, password="not-the-password")
    wizard.connect()

    assert wait_until(
        lambda: wizard.error_text() != "",
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "connect with a bad password reported no error at all — "
        "the wizard either accepted it or swallowed the rejection",
    )
    # The source really refused it, rather than the client refusing to dial.
    assert (imap_source.username, "not-the-password") in imap_source.logins, (
        f"the app never reached the source server with those credentials; "
        f"logins seen: {imap_source.logins}"
    )

    # Retype only the password — everything else must have survived the failure.
    wizard.set_password(imap_source.password)
    wizard.connect()

    assert wait_until(
        lambda: wizard.mailbox_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "after a corrected password the wizard never reached the "
        f"Scope step; error was {wizard.error_text()!r}",
    ) == ["INBOX", "Archive"]


@pytest.mark.feature("mailbox-import")
def test_a_failed_connection_is_explained_in_plain_words_never_the_servers_reply(
    logged_in_app, imap_source
):
    """§ Wizard steps step 2 (outcome 51): a connection that fails is told in
    one plain sentence for what went wrong — the server refused the sign-in,
    or nothing answered at all — and never in the source server's own reply,
    which goes to the app's log only.

    Witnessed with Generic IMAP, the one kind whose host and port the user
    types and so the only one a harness server can stand behind. The
    provider-hint half of the outcome (Gmail, Outlook and iCloud add their
    own hint after the sentence, `connect_failure_sentence`) has no app
    witness, and a seed redirecting a preset's host to a harness server was
    considered and REFUSED (`e2e-automation-surface-gating.md` § Seeds ruled
    out, 2026-10-13): the shared function's unit tests pin sentence + hint per
    provider through the same connect path, the field they write is the one
    this test proves tui paints, and outcome 16's test witnesses tui's picker
    writing the machine's kind — so this arm is the outcome's honest app proof.
    """
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    # The server answers and refuses the password.
    fill_generic_source(wizard, imap_source, password="not-the-password")
    wizard.connect()
    refused = wait_until(
        lambda: wizard.error_text() or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "connect with a bad password reported no error at all",
    )
    assert (imap_source.username, "not-the-password") in imap_source.logins, (
        f"the app never reached the source server; logins seen: {imap_source.logins}"
    )
    assert refused == S.mail_import.connect_failed_sign_in, (
        f"a refused sign-in reads {refused!r}, not the plain sentence"
    )
    for raw in ("AUTHENTICATIONFAILED", "Invalid credentials"):
        assert raw not in refused, f"the server's own reply reached the user: {refused!r}"

    # Nothing answers: a port held bound but never listening refuses every
    # connection, and holding it keeps another process from taking it mid-test.
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as closed:
        closed.bind(("127.0.0.1", 0))
        type_until_it_takes(wizard.set_port, wizard.port_value, str(closed.getsockname()[1]))
        wizard.set_password(imap_source.password)
        wizard.connect()
        assert wait_until(
            lambda: (lambda e: e if e and e != refused else None)(wizard.error_text()),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"error still reads {wizard.error_text()!r}",
        ) == S.mail_import.connect_failed_reach


@pytest.mark.feature("mailbox-import")
def test_discard_and_duplicate_mailboxes_start_unticked_and_are_never_hidden(
    logged_in_app,
):
    """§ Wizard steps step 3 (outcome 52): every mailbox the source lists is
    offered, and all start ticked except the ones holding discards (Trash,
    Junk/Spam) or duplicates of mail another mailbox holds (Gmail's All Mail,
    Important, Starred) — recognised by the role the server advertises first,
    so a localized Gmail that names none of them in English is still read
    right, and by the well-known names when no role is advertised. They start
    unticked, never hidden: the user ticks one to take it.
    """
    # name → (advertised role, ticked by default)
    offered = {
        "INBOX": (None, True),
        "Archive": (None, True),
        "Gesendet": ("\\Sent", True),
        "Papierkorb": ("\\Trash", False),
        "Werbung": ("\\Junk", False),
        "[Gmail]/Alle Nachrichten": ("\\All", False),
        "[Gmail]/Markiert": ("\\Flagged", False),
        "[Gmail]/Wichtig": ("\\Important", False),
        "[Gmail]/Starred": (None, False),
        "[Google Mail]/Bin": (None, False),
        "Junk": (None, False),
    }
    mailboxes = [
        SourceMailbox(name=name, messages=[sample_message(f"in {name}")], special_use=role)
        for name, (role, _ticked) in offered.items()
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = MailImportActions(logged_in_app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        fill_generic_source(wizard, source, password=source.password)
        wizard.connect()
        assert wait_until(
            lambda: wizard.mailbox_names(),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "the Scope step never listed the source mailboxes; "
            f"error was {wizard.error_text()!r}",
        ) == list(offered), "a mailbox the source lists is not offered"

        ticked = {name: wizard.mailbox_selected(name) for name in offered}
        assert ticked == {name: t for name, (_role, t) in offered.items()}, (
            f"the default selection is {ticked}"
        )

        wizard.toggle_mailbox("Papierkorb")
        assert wait_until(
            lambda: wizard.mailbox_selected("Papierkorb"),
            budgets.UI_SETTLE_S,
        ), "an unticked Trash cannot be ticked"
        # Back to Source: the app is session-scoped, and the next test starts
        # its walk there.
        wizard.back()
        assert wizard.is_page_visible(), "Back did not return to Source"


@pytest.mark.feature("mailbox-import")
def test_an_import_walks_source_to_done_and_really_reads_the_source(
    app, dedicated_mail_nest, imap_source, request
):
    """The whole journey, as a user runs it: connect, choose what to bring,
    confirm, watch it run, land on Done.

    Two independent verdicts, because either alone can be satisfied by a wizard
    that does not work:

      1. The UI reaches Done and reports the messages it was scoped to import.
      2. The **source server** was actually read — and read only for the
         mailbox the user kept. A Done screen reporting counts it computed from
         the scope selection would satisfy (1) on its own.

    Uses `dedicated_mail_nest` + `login_as_nest_admin` (not `logged_in_app`'s
    shared nest): `enable_mail_plain`'s "add credential" step never renders
    `mail-add-credential-type-selector` against a nest with no mail domain
    configured, the same precondition gap `enable_mail_so_the_nest_can_seal`'s
    own docstring already named (`test_mail_sent_feed.py` is the pattern this
    follows).
    """
    handle = dedicated_mail_nest
    login_as_nest_admin(app, handle.nest, dedicated_node_url(app, handle, request))
    enable_mail_so_the_nest_can_seal(app, handle)

    wizard = MailImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    fill_generic_source(wizard, imap_source, password=imap_source.password)
    wizard.connect()

    assert wait_until(
        lambda: wizard.mailbox_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the Scope step never listed the source mailboxes; "
        f"error was {wizard.error_text()!r}",
    ) == ["INBOX", "Archive"]

    # § Wizard steps 3: mailboxes default to ALL, and the user deselects.
    assert wizard.mailbox_selected("INBOX"), "mailboxes must default to selected"
    assert wizard.mailbox_selected("Archive"), "mailboxes must default to selected"
    wizard.toggle_mailbox("Archive")
    assert not wizard.mailbox_selected("Archive"), (
        "deselecting a mailbox did not take — the Scope step's central control "
        "does nothing"
    )

    wizard.scope_next()
    summary = wait_until(
        wizard.confirm_summary,
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the Confirm step rendered no summary, so the user is "
        "asked to commit to an import they cannot see the size of",
    )
    # The summary names the MAILBOX count, which is what the user's deselection
    # just changed.
    assert "1 mailbox(es)" in summary, (
        f"the confirm summary {summary!r} does not report the single mailbox left "
        "in scope after the user deselected Archive"
    )
    # § Wizard steps step 4 (outcome 24): how many messages and roughly how
    # much data are coming — INBOX's, never Archive's. This source does not
    # advertise STATUS=SIZE, so the estimate is the import's own enumeration
    # summed (the fake's record shows no STATUS asked).
    assert summary.endswith(_sized_tail(1, INBOX_SUBJECTS)), (
        f"the confirm summary {summary!r} does not say the {len(INBOX_SUBJECTS)} "
        "INBOX messages and their size are coming"
    )
    assert imap_source.status_queries == [], (
        "a server without STATUS=SIZE was asked STATUS: "
        f"{imap_source.status_queries}"
    )
    # § Wizard steps step 4: Start is the commit point — nothing is read off
    # the source, let alone imported, while the user is still deciding.
    assert imap_source.fetched_uids == [], (
        "message bodies were read off the source before the user confirmed: "
        f"{imap_source.fetched_uids}"
    )

    wizard.start()

    done = wait_until(
        lambda: wizard.done_summary() or None,
        budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: "the import never reached Done; progress was "
        f"{wizard.progress_summary()!r}, errors {wizard.error_log()!r}",
    )
    assert f"{len(INBOX_SUBJECTS)} imported" in done, (
        f"Done summary {done!r} does not report the {len(INBOX_SUBJECTS)} messages "
        "that were in scope (`mail_import.done_summary_fmt`)"
    )

    # Verdict 2: the source was really read, and only where the user allowed.
    assert imap_source.fetched_uids == [("INBOX", 1), ("INBOX", 2)], (
        "the import did not fetch exactly the scoped mailbox's messages from the "
        f"source; the server was asked for {imap_source.fetched_uids}"
    )

    # Verdict 3 (outcome 31): the source is left exactly as it was.
    _assert_the_source_was_only_read(imap_source)
    # Verdict 4 (outcome 14): the old provider's password never reached the nest.
    assert_the_password_never_reached_the_nest(handle.nest, imap_source)


@pytest.mark.feature("mailbox-import")
def test_a_message_over_the_users_size_limit_is_skipped_as_too_large(
    app, dedicated_mail_nest, request
):
    """§ Wizard steps step 3 and § Per-message flow step 1: the size limit
    starts at 50 MB, the user lowers it, and a message over it is never
    fetched — it is listed as too large and counted as skipped — while the
    rest of the mailbox arrives; the Done card then says how long it took
    (step 6).

    Three verdicts the UI alone could fake, so each has an outside witness: the
    fake source's own record says the large message's body was never asked
    for, and the Done card's skip count is the session row's (it survives in
    the nest's `skipped_count`, reported back through the app), not a number
    the wizard derived from the limit.
    """
    handle = dedicated_mail_nest
    login_as_nest_admin(app, handle.nest, dedicated_node_url(app, handle, request))
    enable_mail_so_the_nest_can_seal(app, handle)

    # Over a 1 MB limit by half a megabyte; the two small ones are far under.
    large = sample_message("a large message", body="x" * (1536 * 1024))
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[
                sample_message("a small message"),
                large,
                sample_message("another small message"),
            ],
        )
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = MailImportActions(app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        fill_generic_source(wizard, source, password=source.password)
        wizard.connect()
        assert wait_until(
            lambda: wizard.mailbox_names(),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "the Scope step never listed the source mailboxes; "
            f"error was {wizard.error_text()!r}",
        ) == ["INBOX"]

        assert wizard.max_size_value() == "50", (
            "the max message size must start at 50 MB (§ Wizard steps step 3); "
            f"the field shows {wizard.max_size_value()!r}"
        )
        type_until_it_takes(wizard.set_max_size_mb, wizard.max_size_value, "1")
        wizard.scope_next()
        wait_until(wizard.confirm_summary, budgets.UI_SETTLE_S)
        wizard.start()

        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: "the import never reached Done; progress was "
            f"{wizard.progress_summary()!r}, errors {wizard.error_log()!r}",
        )
        assert "2 imported" in done and "1 skipped" in done, (
            f"Done summary {done!r} must report the two small messages imported "
            "and the large one skipped"
        )
        # § Wizard steps step 6: the finished import says how long it took.
        assert " · took " in done, (
            f"Done summary {done!r} does not say how long the import took "
            "(`mail_import.done_summary_timed_fmt`)"
        )
        assert source.fetched_uids == [("INBOX", 1), ("INBOX", 3)], (
            "the large message's body was fetched although it is over the "
            f"limit; the server was asked for {source.fetched_uids}"
        )


# ── the source connection (§ The two TLS modes) ───────────────────────────


@pytest.mark.feature("mailbox-import")
def test_the_app_never_offers_an_unencrypted_connection(logged_in_app):
    """Outcome 19 (§ The two TLS modes): the connection-security picker offers
    implicit TLS and STARTTLS and nothing else — read from every option the app
    paints, so a plaintext option added anywhere in the list is caught."""
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()
    wizard.select_source("Generic IMAP")
    options = wait_until(
        lambda: logged_in_app.driver.option_texts("mail-import-source-tls-mode"),
        budgets.UI_SETTLE_S,
        diagnose=lambda: "the Generic source painted no connection-security picker; "
        f"error was {wizard.error_text()!r}",
    )
    assert options == [S.mail_import.tls_implicit, S.mail_import.tls_starttls], (
        f"the connection-security picker offers {options!r}"
    )


@pytest.mark.feature("mailbox-import")
def test_an_unverifiable_certificate_stops_before_the_password_is_sent(logged_in_app):
    """Outcome 20, the certificate half (§ The two TLS modes): a source whose
    certificate does not chain to anything the app trusts is refused at the
    handshake, the wizard says it could not secure the connection, and the
    source's own record shows no command at all — no `LOGIN`, so no password."""
    # No `cert=`: the instance mints its own chain, which no app was launched
    # trusting.
    with FakeImapSource(_mailboxes(), password=SOURCE_PASSWORD) as source:
        wizard = MailImportActions(logged_in_app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        fill_generic_source(wizard, source, password=source.password)
        wizard.connect()
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"no error after Connect; Scope lists {wizard.mailbox_names()!r}",
        ) == S.mail_import.connect_failed_secure
        assert source.logins == [], f"the password was sent: {source.logins}"
        assert source.commands == [], f"the source was sent {source.commands}"


@pytest.mark.feature("mailbox-import")
def test_a_refused_starttls_stops_before_the_password_is_sent(logged_in_app):
    """Outcome 20, the STARTTLS half (§ The two TLS modes): a source that
    answers `STARTTLS` with `NO` is never spoken to in the clear — a MITM can
    always manufacture that `NO` — so the wizard says it could not secure the
    connection and the source, which stays on the line listening, never
    receives a `LOGIN`."""
    with FakeImapSource(
        _mailboxes(),
        cert=session_cert(),
        password=SOURCE_PASSWORD,
        tls_mode="starttls",
        refuse_starttls=True,
    ) as source:
        wizard = MailImportActions(logged_in_app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        fill_generic_source(wizard, source, password=source.password)
        wizard.select_tls_mode(S.mail_import.tls_starttls)
        wizard.connect()
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"no error after Connect; Scope lists {wizard.mailbox_names()!r}",
        ) == S.mail_import.connect_failed_secure
        assert source.starttls_refusals == 1, "the app never asked to secure the connection"
        assert source.logins == [], f"the password was sent in the clear: {source.logins}"


# ── failures part-way through (§ Failure handling, § Per-message error budget) ──


@pytest.mark.feature("mailbox-import")
def test_a_password_refused_part_way_through_stops_the_import_and_asks_for_new_credentials(
    app, dedicated_mail_nest, request
):
    """Outcome 32 (§ Failure handling → *Source-side errors*, the `LOGIN` row):
    the old provider changes the password and drops the connection after the
    first mailbox; the app signs in again with the password it holds, is
    refused, and stops the import — saying why and offering new credentials —
    rather than retrying a password that will never work again or skipping on
    to the next mailbox. The first mailbox's mail stays imported."""
    mailboxes = [
        SourceMailbox(name="INBOX", messages=[sample_message("before the change")]),
        SourceMailbox(
            name="Later",
            messages=[sample_message("after the change"), sample_message("also after")],
        ),
    ]
    with FakeImapSource(
        mailboxes, cert=session_cert(), password=SOURCE_PASSWORD, revoke_password_after=1
    ) as source:
        wizard = _walk_to_a_started_import(app, dedicated_mail_nest, request, source)
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: "the import never stopped; progress was "
            f"{wizard.progress_summary()!r}, done {wizard.done_summary()!r}, "
            f"logins {source.logins}",
        ) == S.mail_import.password_session_sign_in_again
        assert source.password_revoked, "the source never changed its password"
        assert source.logins[-1] == (source.username, SOURCE_PASSWORD), (
            f"the app did not sign in again with the password it held: {source.logins}"
        )
        assert wizard.done_summary() == "", "the import claims to have finished"
        assert wizard.progress_summary().startswith("1 of 3"), (
            f"the first mailbox's message is not counted imported: {wizard.progress_summary()!r}"
        )
        assert source.fetched_uids == [("INBOX", 1)]


@pytest.mark.feature("mailbox-import")
def test_a_mailbox_that_vanishes_part_way_through_is_noted_and_the_next_one_continues(
    app, dedicated_mail_nest, request
):
    """Outcome 33 (§ Failure handling → *Source-side errors*, the `EXAMINE`
    row): a mailbox deleted on the source after the import began is written to
    the skip list, and the import goes on to the next mailbox and finishes.

    The fake holds the last mailbox's bodies until the skip list has been read,
    so the note is read on the Progress screen while the run is still going —
    no race against the run reaching Done.
    """
    mailboxes = [
        SourceMailbox(name="INBOX", messages=[sample_message("an inbox message")]),
        SourceMailbox(name="Archive", messages=[sample_message("deleted mid-run")]),
        SourceMailbox(name="Later", messages=[sample_message("a later message")]),
    ]
    with FakeImapSource(
        mailboxes,
        cert=session_cert(),
        password=SOURCE_PASSWORD,
        vanish_on_fetch=("Archive",),
        hold_fetch_in="Later",
    ) as source:
        wizard = _walk_to_a_started_import(app, dedicated_mail_nest, request, source)
        assert wait_until(
            lambda: source.holding.is_set() and "Archive" in wizard.error_log(),
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"skip list {wizard.error_log()!r}, examines {source.examines}, "
            f"progress {wizard.progress_summary()!r}, error {wizard.error_text()!r}",
        )
        # The run reached Archive and was refused it; `holding` above already
        # proves it went on to Later — a body fetch there is waiting.
        assert ("Archive", "NO") in source.examines
        source.release_hold()
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"no Done; progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}",
        )
        assert done.startswith("2 imported"), f"Done summary {done!r}"
        assert source.fetched_uids == [("INBOX", 1), ("Later", 1)]


@pytest.mark.feature("mailbox-import")
def test_review_skipped_on_the_done_screen_lists_every_skipped_message_and_mailbox(
    app, dedicated_mail_nest, request
):
    """Outcomes 11 and 33 after the run ends (§ Wizard steps step 6, "Review
    skipped" deep-links into the skip log): an import with one message over the
    user's size limit and one mailbox deleted on the source mid-run reaches
    Done, the user presses Review skipped, and both reasons are listed there —
    the note no longer disappears with the Progress screen.

    The card keeps the log out of sight until the button is pressed, so a
    clean import's Done card stays a summary; the test reads it hidden first.
    """
    if not (app.driver.is_tui() or app.driver.is_linux() or app.driver.is_android()):
        skip_unbuilt(
            app.driver,
            surface="mail-import-review-skipped-button (the skip log on the Done screen)",
            detail="witnessed on tui, linux and android; the rest follow",
            tracked="mailbox-migration.md § Implementation status today; ",
        )
    large = sample_message("a large message", body="x" * (1536 * 1024))
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message("a small message"), large],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("deleted mid-run")]),
        SourceMailbox(name="Later", messages=[sample_message("a later message")]),
    ]
    with FakeImapSource(
        mailboxes,
        cert=session_cert(),
        password=SOURCE_PASSWORD,
        vanish_on_fetch=("Archive",),
    ) as source:
        wizard = _connect_journey(app, dedicated_mail_nest, request, source)
        type_until_it_takes(wizard.set_max_size_mb, wizard.max_size_value, "1")
        wizard.scope_next()
        wait_until(
            wizard.confirm_summary,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"no Confirm summary; error was {wizard.error_text()!r}",
        )
        wizard.start()
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"no Done; progress {wizard.progress_summary()!r}, "
            f"skip list {wizard.error_log()!r}, error {wizard.error_text()!r}",
        )
        assert done.startswith("2 imported") and "1 skipped" in done, f"Done summary {done!r}"
        assert ("Archive", "NO") in source.examines
        assert wizard.error_log() == "", (
            f"the skip log shows on the Done card before Review skipped: {wizard.error_log()!r}"
        )

        wizard.review_skipped()
        log = wait_until(
            lambda: wizard.error_log() or None,
            budgets.UI_SETTLE_S,
            diagnose=lambda: "Review skipped showed no skip log on the Done screen; "
            f"done {wizard.done_summary()!r}, error {wizard.error_text()!r}",
        )
        assert "INBOX uid 2: too large" in log, f"the too-large message is not listed: {log!r}"
        assert "Archive" in log, f"the vanished mailbox is not listed: {log!r}"
        assert wizard.done_summary() == done, "Review skipped left the Done screen"


@pytest.mark.feature("mailbox-import")
def test_an_unreadable_message_is_counted_failed_and_the_rest_arrives(
    app, dedicated_mail_nest, request
):
    """Outcome 34 (§ Failure handling → *Source-side errors*, the `FETCH`
    row): a message the source refuses to hand over is counted as failed —
    on the session row the Done card reads back — and the messages either side
    of it arrive."""
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message(s) for s in ("one", "two cannot be read", "three")],
        )
    ]
    with FakeImapSource(
        mailboxes, cert=session_cert(), password=SOURCE_PASSWORD, unreadable={("INBOX", 2)}
    ) as source:
        wizard = _walk_to_a_started_import(app, dedicated_mail_nest, request, source)
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"no Done; progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}, log {wizard.error_log()!r}",
        )
        assert done.startswith(
            S.mail_import.done_summary_fmt(imported="2", skipped="0", errored="1")
        ), f"Done summary {done!r}"
        assert source.refused_fetches == [("INBOX", 2)]
        assert source.fetched_uids == [("INBOX", 1), ("INBOX", 3)]


@pytest.mark.feature("mailbox-import")
def test_an_import_that_keeps_failing_stops_itself_with_the_reason(
    app, dedicated_mail_nest, request
):
    """Outcome 37 (§ Per-message error budget): a source that cannot hand over
    a single message stops the import once the budget is spent — saying why —
    long before it has asked for every message."""
    total = 100
    mailboxes = [
        SourceMailbox(name="INBOX", messages=[sample_message(f"broken {i}") for i in range(total)])
    ]
    with FakeImapSource(
        mailboxes,
        cert=session_cert(),
        password=SOURCE_PASSWORD,
        unreadable={("INBOX", uid) for uid in range(1, total + 1)},
    ) as source:
        wizard = _walk_to_a_started_import(app, dedicated_mail_nest, request, source)
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"the import never stopped; progress {wizard.progress_summary()!r}, "
            f"done {wizard.done_summary()!r}, refused {len(source.refused_fetches)}",
        ) == S.mail_import.error_budget_exceeded
        assert wizard.done_summary() == "", "the import claims to have finished"
        assert "INBOX uid" in wizard.error_log(), "the per-message reasons are not listed"
        assert len(source.refused_fetches) < total, (
            f"the import asked for all {total} messages before stopping"
        )
@pytest.mark.feature("mailbox-import")
def test_a_source_that_drops_and_stays_down_pauses_the_import_and_resume_carries_on(
    app, dedicated_mail_nest, request
):
    """Outcome 35 (§ Failure handling → *Source-side errors*, the TCP/TLS row):
    the old provider drops the connection part-way through and does not come
    back; the app tries again three times, then pauses the import — paused,
    never failed — saying why. Once the provider is back, Resume carries on
    from where it stopped: the first mailbox is not read again.

    **It runs the production ladder, 5 s / 30 s / 2 min, about 155 s.** The
    wait is on the pause itself (`MAIL_IMPORT_RETRY_LADDER_S`), never on a
    clock, so nothing here races the ladder (convention 14); shortening it
    would take an e2e-gated backoff seed for nothing a faithful run cannot
    show. The three retries are counted outside the app: the fake records
    each reconnect it refused while down.
    """
    _tui_first(app)
    mailboxes = [
        SourceMailbox(name="INBOX", messages=[sample_message("before the outage")]),
        SourceMailbox(
            name="Later",
            messages=[sample_message("after the outage"), sample_message("also after")],
        ),
    ]
    with FakeImapSource(
        mailboxes, cert=session_cert(), password=SOURCE_PASSWORD, drop_after=1
    ) as source:
        wizard = _walk_to_a_started_import(app, dedicated_mail_nest, request, source)
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.MAIL_IMPORT_RETRY_LADDER_S,
            diagnose=lambda: "the import never paused; progress was "
            f"{wizard.progress_summary()!r}, done {wizard.done_summary()!r}, "
            f"refused reconnects {source.refused_connections}",
        ) == S.mail_import.paused_connection_lost
        assert source.down.is_set(), "the source never dropped"
        assert source.refused_connections == 3, (
            "the app did not try again exactly three times before pausing: "
            f"{source.refused_connections} reconnects"
        )
        assert wizard.done_summary() == "", "the import claims to have finished"
        assert wizard.progress_summary().startswith("1 of 3"), (
            f"the first mailbox's message is not counted imported: {wizard.progress_summary()!r}"
        )
        assert [s[0] for s in import_session_counts(dedicated_mail_nest.nest)] == ["paused"], (
            "the nest does not hold the import paused"
        )

        source.come_back()
        wizard.resume()
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"no Done after Resume; progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}",
        )
        assert done.startswith("3 imported"), f"Done summary {done!r}"
        assert source.fetched_uids == [("INBOX", 1), ("Later", 1), ("Later", 2)], (
            f"Resume did not carry on from where it stopped: {source.fetched_uids}"
        )


# ── the run itself: since date, pause, resume, cancel (outcomes 6, 8, 9, 25-27)
#
# Each journey parks the import on a held source fetch (`FakeImapSource.hold`)
# so the step under test happens at a known point of the run, never in a race
# with it (convention 14): the client has asked for that message and is waiting
# on it; the test acts, then releases it.


def _tui_first(app, *, also_android: bool = False) -> None:
    """The run journeys witness the progress step's behaviour on tui, the lead
    app, and linux (mailbox-migration.md § Implementation status today); the
    others follow in the trickle-down. `also_android` admits android to one
    journey whose app work has landed there ahead of the rest."""
    admitted = app.driver.is_tui() or app.driver.is_linux()
    if also_android:
        admitted = admitted or app.driver.is_android()
    if not admitted:
        skip_unbuilt(
            app.driver,
            surface="mail-import run journeys (since date, pause/resume/cancel, app-password help)",
            detail="witnessed on tui first",
            tracked="mailbox-migration.md § Implementation status today",
        )


def _dated(day: str) -> str:
    """An IMAP INTERNALDATE for `day` (``DD-Mon-YYYY``), mid-morning UTC."""
    return f"{day} 09:00:00 +0000"


def _wait_at(source: FakeImapSource, mailbox: str, uid: int, wizard) -> None:
    assert wait_until(
        lambda: source.is_waiting_at(mailbox, uid),
        budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: f"the import never asked for {mailbox} uid {uid}; "
        f"progress {wizard.progress_summary()!r}, error {wizard.error_text()!r}, "
        f"fetched {source.fetched_uids}",
    ), f"the import did not reach {mailbox} uid {uid}"


def _progress(imported: int, total: int, skipped: int, errored: int = 0) -> str:
    return S.mail_import.progress_summary_fmt(
        imported=str(imported),
        total=str(total),
        skipped=str(skipped),
        errored=str(errored),
    )


def _running_timing_frame() -> tuple[str, str]:
    """The fixed words around a running import's two durations
    (`mail_import.progress_remaining_fmt`): the durations are the clock's, so
    only their frame is asserted."""
    marked = S.mail_import.progress_remaining_fmt(summary="", elapsed="\0", remaining="\1")
    between, after = marked.split("\0", 1)[1].split("\1", 1)
    return between, after


@pytest.mark.feature("mailbox-import")
def test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date(
    app, dedicated_mail_nest, request
):
    """§ Wizard steps steps 3 and 5, § Resume protocol step 3 — one run,
    paused and resumed, under a since date:

    - **The since date** (outcome 6): an older message is neither imported
      nor counted — the Confirm total is the dated range's own, the skipped
      count holds only what the import wanted and could not take (the one
      message over the size limit), and the total never changes. The
      exclusion is decided on the source's own INTERNALDATE.
    - **Pause** (outcome 9): pausing while a mailbox is mid-fetch lets that
      fetch finish and then stops — the next mailbox is not touched until
      Resume — and Resume finishes the run.
    - **The date survives the pause** (outcome 26): the mailbox imported after
      Resume leaves its own older message out too.
    - **Progress** (outcomes 8 and 25): the bar moves, the running summary
      says how long it has run and about how long is left, the paused one no
      longer estimates, each mailbox's row counts its own messages, and the
      over-limit message is listed with its reason.
    """
    _tui_first(app)
    large = sample_message("a new but large message", body="x" * (1536 * 1024))
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[
                sample_message("a new inbox message"),
                sample_message("an old inbox message"),
                large,
                sample_message("another new inbox message"),
            ],
            internal_dates=[
                _dated("01-Mar-2024"),
                _dated("01-Jun-2019"),
                _dated("02-Mar-2024"),
                _dated("03-Mar-2024"),
            ],
        ),
        SourceMailbox(
            name="Archive",
            messages=[
                sample_message("a new archived message"),
                sample_message("an old archived message"),
                sample_message("another new archived message"),
            ],
            internal_dates=[_dated("04-Mar-2024"), _dated("02-Jun-2019"), _dated("05-Mar-2024")],
        ),
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = _connect_journey(app, dedicated_mail_nest, request, source)
        assert wizard.mailbox_names() == ["INBOX", "Archive"]

        type_until_it_takes(wizard.set_date_from, wizard.date_from_value, "2023-01-01")
        type_until_it_takes(wizard.set_max_size_mb, wizard.max_size_value, "1")
        wizard.scope_next()
        summary = wait_until(
            wizard.confirm_summary,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"no Confirm summary; error was {wizard.error_text()!r}",
        )
        # Five in range: three in INBOX (the large one is in range — it is
        # skipped later for its size, never for its date) and two in Archive.
        in_range = S.mail_import.confirm_summary_fmt(source="", mailboxes="2", messages="5")
        assert in_range in summary, (
            f"the Confirm summary {summary!r} does not count exactly the five "
            "messages on or after the since date"
        )

        source.hold("INBOX", 4)
        wizard.start()
        _wait_at(source, "INBOX", 4, wizard)

        wizard.pause()
        source.release("INBOX", 4)
        # The fetch in flight finishes: INBOX's two in-range messages land,
        # its large one is skipped as too large, its old one is neither.
        assert wait_until(
            lambda: wizard.progress_summary().startswith(_progress(2, 5, 1)),
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}, log {wizard.error_log()!r}",
        ), "the paused run did not settle on INBOX's in-range messages"
        assert wizard.error_text() == "", (
            f"pausing mid-fetch surfaced an error: {wizard.error_text()!r}"
        )
        assert "INBOX uid 3: too large" in wizard.error_log(), (
            "the progress step does not list the large message with its reason; "
            f"the log reads {wizard.error_log()!r}"
        )
        paused = wizard.progress_summary()
        between, after = _running_timing_frame()
        assert between.split(" · ")[0] in paused and after not in paused, (
            f"a paused import should say how long it ran and estimate nothing: {paused!r}"
        )
        assert not [f for f in source.fetched_uids if f[0] == "Archive"], (
            f"the import kept going after Pause: {source.fetched_uids}"
        )
        # Per mailbox: INBOX's row carries its own two and its one skip;
        # Archive's has taken nothing yet.
        row = S.mail_import.progress_row_counts_fmt
        expected_rows = {
            "INBOX": row(imported="2", count="3", skipped="1", errored="0"),
            "Archive": row(imported="0", count="2", skipped="0", errored="0"),
        }
        assert wait_until(
            lambda: wizard.mailbox_progress() == expected_rows,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"rows {wizard.mailbox_progress()!r}",
        ), f"the paused run's per-mailbox rows never read {expected_rows!r}"

        source.hold("Archive", 3)
        wizard.resume()
        _wait_at(source, "Archive", 3, wizard)
        running = wait_until(
            lambda: (lambda s: s if s.endswith(after) else None)(wizard.progress_summary()),
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"progress {wizard.progress_summary()!r}",
        )
        assert running.startswith(_progress(2, 5, 1)), (
            f"the resumed run's summary {running!r} lost INBOX's counts or "
            "revised its total"
        )
        assert between in running, (
            f"a running import should say how long it has run and about how "
            f"long is left: {running!r}"
        )
        assert 0.0 < wizard.progress_fraction() < 1.0, (
            "the progress bar does not show a run part-way through"
        )
        source.release("Archive", 3)

        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"the import never reached Done; progress "
            f"{wizard.progress_summary()!r}, error {wizard.error_text()!r}",
        )
        # Four imported (two per mailbox); one skipped, the large one. Had
        # the date been lost on Resume, Archive's old message would make five.
        expected = S.mail_import.done_summary_fmt(imported="4", skipped="1", errored="0")
        assert done.startswith(expected), (
            f"Done reads {done!r}; expected it to start {expected!r}"
        )


@pytest.mark.feature("mailbox-import")
def test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on(
    app, dedicated_mail_nest, request
):
    """§ Resume protocol steps 1–3, § Wizard steps step 5 ("the screen survives
    app restart"), § Credential handling — the app is closed mid-import, with
    no chance to pause it, and opened again:

    - **Back where it was** (outcome 10): the wizard opens on the same import's
      Progress, its counts the session's own.
    - **The password is asked for again** (outcome 28): it lived only in the
      closed app's memory, so Resume without it is refused with a sentence and
      the source is not even dialled — the fake's own record of sign-ins is the
      witness.
    - **It carries on where it stopped** (outcome 10): with the password, the
      run asks the source only for what lies past each mailbox's recorded
      cursor — the mailbox finished before the close is not read again.
    - **The since date still holds** (outcome 26): the mailbox imported after
      the reopen leaves its older message out.
    """
    _tui_first(app, also_android=True)
    require_relaunchable(app.driver)
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[
                sample_message("a new inbox message"),
                sample_message("an old inbox message"),
                sample_message("another new inbox message"),
            ],
            internal_dates=[_dated("01-Mar-2024"), _dated("01-Jun-2019"), _dated("02-Mar-2024")],
        ),
        SourceMailbox(
            name="Archive",
            messages=[
                sample_message("a new archived message"),
                sample_message("an old archived message"),
                sample_message("another new archived message"),
            ],
            internal_dates=[_dated("03-Mar-2024"), _dated("02-Jun-2019"), _dated("04-Mar-2024")],
        ),
    ]
    with FakeImapSource(mailboxes, cert=session_cert(), password=SOURCE_PASSWORD) as source:
        wizard = _connect_journey(app, dedicated_mail_nest, request, source)
        type_until_it_takes(wizard.set_date_from, wizard.date_from_value, "2023-01-01")
        wizard.scope_next()
        wait_until(
            wizard.confirm_summary,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"no Confirm summary; error was {wizard.error_text()!r}",
        )

        # INBOX is handed over whole before Archive is opened; the close lands
        # while Archive's last body is on its way.
        source.hold("Archive", 3)
        wizard.start()
        _wait_at(source, "Archive", 3, wizard)
        assert wait_until(
            lambda: wizard.progress_summary().startswith(_progress(2, 4, 0)),
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}",
        ), "INBOX's two in-range messages are not in before the close"

        relaunch(app.driver, crash=True)
        # The closed app's parked fetch answers nobody; let it go so the
        # source's record settles before the reopened app's run is read.
        source.release("Archive", 3)
        assert wait_until(lambda: ("Archive", 3) in source.fetched_uids, budgets.UI_SETTLE_S)
        fetched_before = len(source.fetched_uids)
        logins_before = len(source.logins)

        # ── Back on the same import's Progress.
        wizard = MailImportActions(app.driver)
        wizard.navigate()
        assert wait_until(
            lambda: wizard.progress_summary().startswith(_progress(2, 4, 0)) or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: "the reopened app did not come back to the import's "
            f"progress: summary {wizard.progress_summary()!r}, "
            f"source step showing {app.driver.is_visible('mail-import-source-picker')}, "
            f"error {wizard.error_text()!r}",
        ), "the reopened app is not on the same import's Progress"

        # ── Resume without the password: refused, the source not dialled.
        wizard.resume()
        assert wait_until(
            lambda: wizard.error_text() or None,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"Resume without a password said nothing; progress "
            f"{wizard.progress_summary()!r}, logins {source.logins}",
        ) == S.mail_import.resume_needs_password
        assert len(source.logins) == logins_before, (
            f"a Resume with no password reached the source: {source.logins}"
        )

        # ── With the password: signed in again, and only what is left is read.
        # A Generic source's port is not on the session row, so the person
        # checks it beside the password (it starts at 993; this source listens
        # elsewhere).
        assert wizard.port_value() == "993", (
            f"the port beside the password starts at {wizard.port_value()!r}"
        )
        type_until_it_takes(wizard.set_port, wizard.port_value, str(source.port))
        wizard.set_password(SOURCE_PASSWORD)
        wizard.resume()
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"the reopened import never reached Done; progress "
            f"{wizard.progress_summary()!r}, error {wizard.error_text()!r}, "
            f"fetched {source.fetched_uids[fetched_before:]}",
        )
        assert source.logins[logins_before:] == [(source.username, SOURCE_PASSWORD)], (
            f"the reopened app did not sign in with the password just typed: "
            f"{source.logins[logins_before:]}"
        )
        assert source.fetched_uids[fetched_before:] == [
            ("Archive", 1),
            ("Archive", 2),
            ("Archive", 3),
        ], (
            "the resumed run did not read exactly what lay past the cursors — "
            f"INBOX was finished before the close: {source.fetched_uids[fetched_before:]}"
        )
        # Four imported, two per mailbox. Had the reopened run lost the since
        # date, Archive's old message would make five.
        expected = S.mail_import.done_summary_fmt(imported="4", skipped="0", errored="0")
        assert done.startswith(expected), f"Done reads {done!r}; expected it to start {expected!r}"


@pytest.mark.feature("mailbox-import")
def test_cancel_asks_first_and_keeps_what_already_arrived(
    app, dedicated_mail_nest, request
):
    """§ Wizard steps step 5: "Cancel asks for confirmation; on confirm the
    import is aborted but already-imported messages are kept".

    - **One press is not a cancel** (outcome 27): the button turns into the
      confirm question and the run carries on — the next mailbox is still
      fetched and imported.
    - **The confirm cancels and keeps** (outcome 9): the message the source
      was handing over at that moment is never imported, and the count of
      what arrived before stays as it was.
    """
    _tui_first(app)
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message("inbox one"), sample_message("inbox two")],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("archived one")]),
        SourceMailbox(name="Later", messages=[sample_message("a later one")]),
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = _connect_journey(app, dedicated_mail_nest, request, source)
        wizard.scope_next()
        wait_until(wizard.confirm_summary, budgets.UI_SETTLE_S)

        source.hold("Archive", 1)
        source.hold("Later", 1)
        wizard.start()
        _wait_at(source, "Archive", 1, wizard)

        wizard.press_cancel()
        assert wait_until(
            lambda: wizard.cancel_label() == S.common.confirm_q,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"Cancel reads {wizard.cancel_label()!r}",
        ), "the first press of Cancel did not ask to confirm"

        # Not cancelled: the run takes Archive and goes on to Later.
        source.release("Archive", 1)
        _wait_at(source, "Later", 1, wizard)
        assert wait_until(
            lambda: wizard.progress_summary().startswith(_progress(3, 4, 0)),
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"progress {wizard.progress_summary()!r}, "
            f"error {wizard.error_text()!r}",
        ), "one press of Cancel stopped the import"

        # The confirm lands while Later's message is still in flight: the
        # session stops running (no estimate any more) and its counts are the
        # nest's — the three messages that arrived, kept.
        wizard.press_cancel()
        _, left = _running_timing_frame()
        kept = wait_until(
            lambda: (lambda s: s if not s.endswith(left) else None)(wizard.progress_summary()),
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: f"progress {wizard.progress_summary()!r}, "
            f"cancel {wizard.cancel_label()!r}, error {wizard.error_text()!r}",
        )
        assert kept.startswith(_progress(3, 4, 0)), (
            f"the cancelled import does not keep what arrived: {kept!r}"
        )
        # The message in flight is handed over after the cancel, and never
        # imported: a run that ignored the confirm would count it and reach
        # Done.
        source.release("Later", 1)
        assert wait_until(
            lambda: ("Later", 1) in source.fetched_uids,
            budgets.MAIL_AGENT_WRITEBACK_S,
        ), "the held fetch was never served after its release"
        app.driver.barrier()
        assert wizard.done_summary() == "" and wizard.progress_summary().startswith(
            _progress(3, 4, 0)
        ), (
            "the import went on after the confirmed cancel; progress "
            f"{wizard.progress_summary()!r}, done {wizard.done_summary()!r}"
        )


@pytest.mark.feature("mailbox-import")
def test_losing_the_nest_connection_mid_import_loses_nothing(
    app, dedicated_mail_nest, request
):
    """§ Failure handling → *Nest-side errors*, the WS-RPC row (outcome 36):
    the app's connection to its nest drops while an import is part-way through
    — and the import still finishes with every message, each stored once.

    The drop is the real thing, with no seam: the nest process is stopped
    abruptly (no close frame) while the source is handing over INBOX's second
    message, so the app is holding fetched mail it has not yet sent; the
    source then finishes handing it over to an app with no nest, and the same
    nest comes back on the same port and data. The import is never touched by
    the test after Start. Whether the app's send waits for the reconnect or
    meets the dead socket and retries (5 s / 30 s / 2 min), and whether a
    resend lands on mail the nest already took (its dedup skips it), the end
    state is the same: Done, nothing failed, and the nest holds exactly the
    five source messages — none lost, none twice.
    """
    _tui_first(app)
    handle = dedicated_mail_nest
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message(s) for s in ("inbox one", "inbox two", "inbox three")],
        ),
        SourceMailbox(
            name="Archive",
            messages=[sample_message(s) for s in ("archived one", "archived two")],
        ),
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = _connect_journey(app, handle, request, source)
        wizard.scope_next()
        wait_until(wizard.confirm_summary, budgets.UI_SETTLE_S)
        stored_before = stored_message_count(handle.nest)

        source.hold("INBOX", 2)
        wizard.start()
        _wait_at(source, "INBOX", 2, wizard)

        # The drop: an abrupt stop, the way a crashed or killed nest goes —
        # no WS close frame tells the app it is coming.
        stop_nest(handle.nest, graceful=False)
        source.release("INBOX", 2)
        assert wait_until(
            lambda: ("INBOX", 3) in source.fetched_uids,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"fetched {source.fetched_uids}",
        ), "the source never finished handing INBOX over"
        start_nest_in_place(handle.nest)

        done = wait_until(
            lambda: wizard.done_summary() or None,
            # The send's retry ladder may be on its 30 s rung when the nest
            # comes back; the budget covers the ladder's first two rungs.
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: "the import never reached Done after the nest came "
            f"back; progress {wizard.progress_summary()!r}, error "
            f"{wizard.error_text()!r}, log {wizard.error_log()!r}",
        )
        assert wizard.error_text() == "", (
            f"the dropped connection surfaced an error: {wizard.error_text()!r}"
        )

    sessions = import_session_counts(handle.nest)
    assert len(sessions) == 1, f"expected one import session, the nest holds {sessions}"
    state, total, imported, skipped, errored = sessions[0]
    assert (state, total, errored) == ("completed", 5, 0), (
        f"the nest's record of the import is {sessions[0]}; Done read {done!r}"
    )
    # A resend the nest's dedup absorbed is a skip, never a second copy: what
    # was imported and what was skipped as already there add up to the five
    # messages exactly — one more would be a message stored twice.
    assert imported + skipped == 5, (
        f"the nest counted {imported} imported + {skipped} skipped for five "
        f"messages; Done read {done!r}"
    )
    assert stored_message_count(handle.nest) - stored_before == 5, (
        "the nest does not hold all five source messages after the drop: "
        f"{stored_message_count(handle.nest) - stored_before} arrived"
    )


@pytest.mark.feature("mailbox-import")
def test_a_provider_that_needs_an_app_password_says_so_and_where_to_make_one(
    logged_in_app,
):
    """§ Wizard steps step 1 (outcome 16): Gmail and iCloud take an app
    password, and choosing either explains that it needs one — with two-step
    sign-in on — and names the provider's page where it is made. Generic IMAP
    takes the server's own password and shows no app-password field."""
    _tui_first(logged_in_app)
    wizard = MailImportActions(logged_in_app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    for label, page in (
        ("Gmail", "myaccount.google.com/apppasswords"),
        ("iCloud", "appleid.apple.com"),
    ):
        wizard.select_source(label)
        help_text = wait_until(
            lambda: (lambda h: h if page in h else None)(wizard.app_password_help()),
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"the app-password field explains {wizard.app_password_help()!r}",
        )
        assert help_text and "2FA" in help_text, (
            f"choosing {label} does not explain the app password and where to "
            f"make it: {help_text!r}"
        )

    wizard.select_source("Generic IMAP")
    assert wait_until(
        lambda: logged_in_app.driver.is_absent("mail-import-source-app-password"),
        budgets.UI_SETTLE_S,
    ), "Generic IMAP still shows the app-password field"


# ── what landed, read from outside the app (outcomes 12, 23) and a second
#    device on the same source (outcome 30)


def _shaped_source() -> list[SourceMailbox]:
    """A source whose every message differs from the defaults the fake would
    otherwise serve — its own marks, its own arrival date — across a standard
    mailbox (INBOX), a second standard one (Sent) and one no standard mailbox
    matches (Projects). An unread message stays unread, so "the marks came
    across" cannot be met by an import that marks everything read."""
    return [
        SourceMailbox(
            name="INBOX",
            messages=[
                sample_message("a read inbox message"),
                sample_message("a flagged inbox message"),
                sample_message("an unread inbox message"),
            ],
            flags=[("\\Seen",), ("\\Flagged",), ()],
            internal_dates=[_dated("01-Mar-2024"), _dated("02-Mar-2024"), _dated("03-Mar-2024")],
        ),
        SourceMailbox(
            name="Sent",
            messages=[sample_message("a reply that was sent")],
            flags=[("\\Seen", "\\Answered")],
            internal_dates=[_dated("04-Mar-2024")],
        ),
        SourceMailbox(
            name="Projects",
            messages=[
                sample_message("a project note"),
                sample_message("an old project thread"),
            ],
            flags=[("\\Seen", "\\Flagged", "\\Answered"), ("\\Answered",)],
            internal_dates=[_dated("05-Mar-2024"), _dated("06-Jun-2019")],
        ),
    ]


@pytest.mark.feature("mailbox-import")
def test_imported_mail_keeps_its_shape_and_an_unmatched_mailbox_arrives_under_its_own_name(
    app, dedicated_mail_nest, request
):
    """§ Goal ("without losing any structure (per-mailbox layout, dates,
    flags)") and § Wizard steps step 3 (a source mailbox matching no standard
    one "lands as a user-created mailbox named after the source"):

    - **Its shape** (outcome 12): every message lands in the mailbox of the
      source's name, with the source's INTERNALDATE and exactly its read,
      flagged and answered marks — an unread message stays unread.
    - **An unmatched mailbox** (outcome 23): ``Projects`` exists afterwards,
      holding exactly Projects' messages.

    The reader is a standard mail app's own: IMAP against this nest's mail
    bridge, signed in with the credential the mail-enable step minted. It sits
    outside the importing app, so the app's Done screen cannot vouch for what
    the nest kept, and it is where a mailbox, a date and all three marks are
    observable as one answer.
    """
    _tui_first(app)
    handle = dedicated_mail_nest
    mailboxes = _shaped_source()
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = _connect_journey(app, handle, request, source)
        wizard.scope_next()
        wait_until(
            wizard.confirm_summary,
            budgets.UI_SETTLE_S,
            diagnose=lambda: f"no Confirm summary; error was {wizard.error_text()!r}",
        )
        wizard.start()
        done = wait_until(
            lambda: wizard.done_summary() or None,
            budgets.MAIL_AGENT_WRITEBACK_S,
            diagnose=lambda: f"the import never reached Done; progress "
            f"{wizard.progress_summary()!r}, error {wizard.error_text()!r}, "
            f"log {wizard.error_log()!r}",
        )
        assert done.startswith(
            S.mail_import.done_summary_fmt(imported="6", skipped="0", errored="0")
        ), f"Done reads {done!r}; every one of the six messages should have arrived"

    address = alias_admin_to_address(handle.nest, handle.domain)
    landed = mailboxes_over_imap(handle, address, MAIL_PASSWORD)

    assert "Projects" in landed, (
        "the source's Projects mailbox, which matches no standard mailbox, was "
        f"not created; a mail app sees {sorted(landed)}"
    )
    for mailbox in mailboxes:
        want = {
            sample_subject: (frozenset(flags), imap_internaldate(date))
            for sample_subject, flags, date in zip(
                (_subject_of(m) for m in mailbox.messages),
                mailbox.flags,
                mailbox.internal_dates,
            )
        }
        got = {m.subject: (m.flags, m.internal_date) for m in landed.get(mailbox.name, [])}
        assert got == want, (
            f"{mailbox.name} does not hold what the source's {mailbox.name} did.\n"
            f"  source (subject: flags, date): {want}\n"
            f"  nest, read over IMAP:          {got}\n"
            f"  every mailbox a mail app sees: "
            f"{ {name: [m.subject for m in msgs] for name, msgs in landed.items()} }"
        )


def _subject_of(message: bytes) -> str:
    """The Subject a `sample_message` was minted with."""
    for line in message.decode().split("\r\n"):
        if line.startswith("Subject: "):
            return line[len("Subject: "):]
    raise AssertionError(f"no Subject in {message!r}")


def _second_device(handle, request) -> MailImportActions:
    """The same user signed in on a second install of the app — another device
    in `mailbox-migration.md` § Architectural rules' sense: its own launch world
    (convention 10), the same account key, a device id of its own. Trusts this
    nest and the session's source CA exactly as the `app` fixture's launch does.
    The caller tears its driver down."""
    driver = create_driver("tui")
    environment = _seeded_environment(request, handle.nest)
    _apply_mail_import_source_trust_env(environment, request)
    driver.launch({
        "app_path": request.getfixturevalue("tui_app_path"),
        "url": handle.nest["url"],
        "environment": environment,
    })
    try:
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": handle.nest["url"],
                "secret_hex": handle.nest["admin"]["signing_key"].encode().hex(),
                "device_id": os.urandom(32).hex(),
            },
            "nav": {"stack": [{"view": "admin"}]},
        })
        wait_until_online(driver)
    except BaseException:
        driver.teardown()
        raise
    return MailImportActions(driver)


@pytest.mark.feature("mailbox-import")
def test_a_second_import_of_the_same_account_on_another_device_is_refused(
    app, dedicated_mail_nest, request
):
    """§ Architectural rules: "Multi-device parallel imports of the same source
    are blocked by a per-source lock in `import_sessions`" (outcome 30).

    The order is the only one in which a person can meet the lock. A device
    that opens the wizard while an import is live shows that import, not the
    Source step (§ A failed import is a record the wizard shows until the user
    moves on: "after a restart or on another device the wizard shows the newest
    live session"), so it never offers Start on that source. The refusal is for
    two devices already in the wizard at once: both connect to the same old
    account and reach Confirm; the first presses Start and then Pause, so its
    session holds the source at rest (a paused session holds the lock as a
    running one does — § RPC surface, `start_import_session`); the second then
    presses Start and is refused with `error.bridges.import_source_locked`'s
    sentence on `error-message`. The nest still holds exactly the one, paused
    session, and the source is read no further.
    """
    _tui_first(app)
    handle = dedicated_mail_nest
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message("first of two"), sample_message("second of two")],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("archived")]),
    ]
    with FakeImapSource(mailboxes, cert=session_cert()) as source:
        wizard = _connect_journey(app, handle, request, source)
        wizard.scope_next()
        wait_until(wizard.confirm_summary, budgets.UI_SETTLE_S)

        second = _second_device(handle, request)
        try:
            second.navigate()
            assert second.is_page_visible(), (
                "the second device never showed the import wizard on its Source "
                f"step; its state {second.driver.get_state()!r}\nits log, tail:\n"
                f"{second.driver.app_stderr_text()[-6000:]}"
            )
            fill_generic_source(second, source, password=source.password)
            second.connect()
            assert wait_until(
                second.mailbox_names,
                budgets.RPC_ROUNDTRIP_S,
                diagnose=lambda: f"the second device never listed the source; "
                f"error was {second.error_text()!r}",
            ) == ["INBOX", "Archive"]
            second.scope_next()
            wait_until(
                second.confirm_summary,
                budgets.UI_SETTLE_S,
                diagnose=lambda: f"no Confirm summary; error was {second.error_text()!r}",
            )

            source.hold("INBOX", 2)
            wizard.start()
            _wait_at(source, "INBOX", 2, wizard)
            wizard.pause()
            source.release("INBOX", 2)
            assert wait_until(
                lambda: [s[0] for s in import_session_counts(handle.nest)] == ["paused"],
                budgets.MAIL_AGENT_WRITEBACK_S,
                diagnose=lambda: f"sessions {import_session_counts(handle.nest)}, "
                f"progress {wizard.progress_summary()!r}, error {wizard.error_text()!r}",
            ), "the first device's import never rested paused"

            second.start()
            refused = S.error.bridges.import_source_locked
            # The bare sentence, as mail settings shows its refusals — never
            # behind the seam's own "nest rejected: " wrapper
            # (`DispatchError::refused_control`).
            assert wait_until(
                lambda: second.error_text() == refused,
                budgets.RPC_ROUNDTRIP_S,
                diagnose=lambda: f"error-message reads {second.error_text()!r}, "
                f"confirm {second.confirm_summary()!r}, progress "
                f"{second.progress_summary()!r}, done {second.done_summary()!r}",
            ), "the second device's Start was not refused as already being imported"
            assert second.done_summary() == "", "the refused import ran to Done anyway"
        finally:
            second.driver.teardown()

        sessions = import_session_counts(handle.nest)
        assert [s[0] for s in sessions] == ["paused"], (
            f"the refused Start changed the nest's sessions: {sessions}"
        )
        assert ("Archive", 1) not in source.fetched_uids, (
            f"the paused import went on: {source.fetched_uids}"
        )
