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

import sys
from pathlib import Path

import pytest

from actions.mail_import import MailImportActions
from helpers import budgets
from helpers.mail_dedicated_nest import dedicated_node_url, login_as_nest_admin
from helpers.waiting import wait_until

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
    with FakeImapSource(_mailboxes(), cert=session_cert()) as source:
        yield source


def _type_until_it_takes(setter, getter, value: str, *, timeout: float = 5.0) -> None:
    """Call ``setter(value)``, retyping on a bounded poll until ``getter()``
    reads back exactly ``value``.

    Guards a real race: `select_source` dispatches `SelectSourceKind`
    asynchronously, and its snapshot callback re-seeds the host/port drafts
    once it lands (`seed_source_drafts`,
    apps/fauna-linux/src/settings/mail_import.rs) — carrying over whatever
    host was already there for Generic IMAP (no preset of its own to
    overwrite it with, a documented "accepted quirk"). `clear_and_type` is
    two separate bridge round trips (`/element/clear` then `/element/type`);
    if that re-seed lands in the gap between them, the type half inserts at
    the CURRENT text's end (`type_text`,
    apps/fauna-linux/src/automation/agent.rs — `insert_text` at the read-back
    cursor position, not a full replace), silently concatenating the typed
    value onto the stale re-seeded one instead of replacing it.
    """
    import time

    deadline = time.monotonic() + timeout
    setter(value)
    while getter() != value and time.monotonic() < deadline:
        time.sleep(0.2)
        setter(value)


def _fill_generic_source(
    wizard: MailImportActions, source: FakeImapSource, *, password: str
) -> None:
    """Step 1 as a user fills it in for a self-hosted server.

    Generic IMAP is the provider whose fields are all user-entered
    (§ Wizard steps step 1); the three presets hard-code host/port/TLS, so they
    could not be pointed at a harness server at all.
    """
    wizard.select_source("Generic IMAP")
    _type_until_it_takes(wizard.set_host, wizard.host_value, "localhost")
    _type_until_it_takes(wizard.set_port, wizard.port_value, str(source.port))
    # Exact i18n labels (`i18n/strings/en.yaml` `mail_import:`): the select
    # route checks membership against what the app painted and 409s on a
    # near-miss, so an abbreviated label fails as 'option not offered'.
    wizard.select_tls_mode("Implicit TLS (993)")
    wizard.set_username(source.username)
    wizard.set_password(password)


# The credential the mail-enable precondition mints. Nothing authenticates with
# it here — an import rides the app's own WS-RPC session, not a MUA login — but
# `enable_mail` is the flow that provisions the actor's recipient key, and it
# mints a credential on the way.
_MAIL_PASSWORD = "import-journey-mua-password"


def _enable_mail_so_the_nest_can_seal(app, handle) -> None:
    """Precondition: the importing actor must have a recipient seal key on file.

    NOT incidental setup — without it the nest refuses the whole call with
    ``recipient has no encryption key on file``
    (`bins/fauna-nest/src/bridge_import_handlers.rs`, the fail-closed
    `get_recipient_mail_seal_key`). § Per-message flow step 4 seals imported mail
    to the recipient's registered key at ingest, and `enable_mail` is the flow
    that registers it — the same provisioning `test_mail_sent_feed.py` relies on
    so the bridge can seal a Sent copy.

    Driving it through the app UI keeps this honest: convention 8 permits an API
    shortcut for fixture setup, but the user-facing enable path exists and works,
    so there is no reason to reach past it.
    """
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_MAIL_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=30.0), (
        "mail did not report enabled, so the actor has no recipient seal key and "
        "the nest will refuse every imported message with 'recipient has no "
        f"encryption key on file'. status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}. If enabling "
        "mail turns out to need a domain this shared nest does not have, switch "
        "this module to the `dedicated_mail_nest` fixture + `login_as_nest_admin` "
        "preamble that every other enable_mail_plain caller uses "
        "(test_mail_sent_feed.py is the smallest example)."
    )
    # The client's enable opened the deployment gates (driven as nest admin), so
    # each idling bridge has exited 0 for the supervisor to restart it bound
    # (mail-bridge-lifecycle.md § Default-off). The binaries e2e has no s6, so
    # play supervisor here (test_dedicated_mail_nest_consumers_rebind.py).
    handle.rebind_after_enable()


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

    _fill_generic_source(wizard, imap_source, password="not-the-password")
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
    configured, the same precondition gap `_enable_mail_so_the_nest_can_seal`'s
    own docstring already named (`test_mail_sent_feed.py` is the pattern this
    follows).
    """
    handle = dedicated_mail_nest
    login_as_nest_admin(app, handle.nest, dedicated_node_url(app, handle, request))
    _enable_mail_so_the_nest_can_seal(app, handle)

    wizard = MailImportActions(app.driver)
    wizard.navigate()
    assert wizard.is_page_visible()

    _fill_generic_source(wizard, imap_source, password=imap_source.password)
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
    # just changed. Its message count reads 0 here by design and asserting on it
    # would pin a bug: `LIST` carries no per-mailbox count (only `EXAMINE`
    # does), so `rpc_glue`'s `list_source_mailboxes` reports 0 and `run_import`
    # refines the estimate once it EXAMINEs each selected mailbox.
    assert "1 mailbox(es)" in summary, (
        f"the confirm summary {summary!r} does not report the single mailbox left "
        "in scope after the user deselected Archive"
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
