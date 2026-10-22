"""The mail-import wizard's step-1 fill and its mail-enable precondition, as a
user drives them — shared by every module that imports from the harness's
foreign IMAP server (`fakes/fake_imap_source.py`): the import journeys
themselves, and the export journeys that need a mailbox shaped like a real
one (custom mailboxes, the source's own marks and arrival dates).

Authority: `docs/goal/behavior/mailbox-migration.md` § Wizard steps.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from actions.mail_import import MailImportActions
from helpers import budgets
from helpers.waiting import wait_until

if TYPE_CHECKING:
    from fake_imap_source import FakeImapSource


def type_until_it_takes(setter, getter, value: str, *, timeout: float = 5.0) -> None:
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


def set_message_allowance(app, nest: dict, count: int) -> None:
    """The admin sets how many messages a mailbox may hold (`admin-mail`'s
    `admin-mail-imap-message-count-input`) and presses Save; returns once the
    nest has stored it. The precondition a full-mailbox journey needs, set
    the way an admin sets it — through the app."""
    from helpers.mail_import_evidence import message_allowance

    field = "admin-mail-imap-message-count-input"
    app.admin.navigate_mail()
    assert wait_until(
        lambda: app.admin.mail_field_present(field),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "admin-mail never rendered the IMAP group's message allowance",
    )
    app.admin.set_mail_field(field, str(count))
    app.admin.save_mail_imap()
    assert wait_until(
        lambda: message_allowance(nest) == count,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the nest stored allowance {message_allowance(nest)!r}, not {count}; "
        f"the page shows {app.admin.mail_field_text(field)!r}",
    )


def fill_generic_source(
    wizard: MailImportActions, source: FakeImapSource, *, password: str
) -> None:
    """Step 1 as a user fills it in for a self-hosted server.

    Generic IMAP is the provider whose fields are all user-entered
    (§ Wizard steps step 1); the three presets hard-code host/port/TLS, so they
    could not be pointed at a harness server at all.
    """
    wizard.select_source("Generic IMAP")
    type_until_it_takes(wizard.set_host, wizard.host_value, "localhost")
    type_until_it_takes(wizard.set_port, wizard.port_value, str(source.port))
    # Exact i18n labels (`i18n/strings/en.yaml` `mail_import:`): the select
    # route checks membership against what the app painted and 409s on a
    # near-miss, so an abbreviated label fails as 'option not offered'.
    wizard.select_tls_mode("Implicit TLS (993)")
    wizard.set_username(source.username)
    wizard.set_password(password)


def save_microsoft_registration(app, client_id: str, tenant: str) -> None:
    """The admin enters the deployment's own Microsoft app registration on
    `admin-mail` and presses Save; returns once the client-ID field reads back
    what the nest stored (the GUID lowercased) — both empty clears it.

    Precondition for every Outlook sign-in a suite walks: the project's own
    client ID is still empty (`oauth_device.rs`'s `MICROSOFT` constant), so
    without an admin registration the button stays inert with "not available
    yet" (`mailbox-migration.md` § Microsoft sign-in — the device-code grant,
    *An admin-supplied registration*).
    """
    app.admin.navigate_mail()
    assert wait_until(
        lambda: app.admin.mail_field_present("admin-mail-microsoft-client-id-input"),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "admin-mail never rendered the Microsoft registration section",
    )
    app.admin.save_mail_microsoft_registration(client_id, tenant)
    assert wait_until(
        lambda: app.admin.mail_field_text("admin-mail-microsoft-client-id-input")
        == client_id.lower(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: "the saved registration never read back: field shows "
        f"{app.admin.mail_field_text('admin-mail-microsoft-client-id-input')!r}",
    )


# The credential the mail-enable precondition mints. Nothing authenticates with
# it here — an import rides the app's own WS-RPC session, not a MUA login — but
# `enable_mail` is the flow that provisions the actor's recipient key, and it
# mints a credential on the way.
MAIL_PASSWORD = "import-journey-mua-password"


def enable_mail_so_the_nest_can_seal(app, handle) -> None:
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
    app.mail_settings.enable_mail_plain(MAIL_PASSWORD)
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
