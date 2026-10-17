"""tier_3 — the mail-import CLIENT's promises, witnessed once and with no app.

`docs/goal/behavior/mailbox-migration.md` makes some claims of the client
rather than of any app's screen: the shared `MailImportMachine` is the one
place the source credentials live (§ Credential handling), and every app
runs that same machine. A claim like that is a `[nest]` outcome on the
feature page — proven app-independently, observed from outside — and a
witness that drives an app is refused for it (`feature-catalog.md` § The
catalog lint, rule 4).

So the import here is a real client import with no app driver:
`fauna_ffi.harness_mail_import` builds the machine the apps build
(`rpc_glue::build_mail_import_machine`), walks it Source → Done against a real
TLS IMAP source (`fakes/fake_imap_source.py`, the same server the app journeys
use) and a real nest, over the importer's own authenticated WS-RPC session.
Nothing here hand-builds an import request — a witness that wrote the wire
itself would only prove what the test chose to send.

The evidence is read where the nest keeps things, by the same helper the tui
journey reads it with (`helpers/mail_import_evidence.py`).
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_import_evidence import assert_the_password_never_reached_the_nest
from helpers.recipient_seal_key import provision_recipient_seal_key

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fakes"))
from fake_imap_source import (  # noqa: E402
    FakeImapSource,
    SourceMailbox,
    sample_message,
    session_ca_pem,
    session_cert,
)

pytestmark = pytest.mark.tier_3

# Distinctive on purpose: the evidence searches every byte the nest holds for
# it, and a short common word would be found by accident in some unrelated row.
SOURCE_PASSWORD = "0ld-provider-password-app-free-3b95d2"


@pytest.mark.feature("mailbox-import")
def test_the_old_providers_password_never_reaches_the_nest(dedicated_no_mail_nest):
    """Outcome 14: a whole import signs in to the old provider with its
    password, brings every message over — and afterwards the password's bytes
    are in no file the nest keeps and in its log, and the session row names
    the source by user and host only.

    The import must really have run for the absence to mean anything: the
    source's own record shows the client logged in with this password, and the
    import reached Done having brought every message across.
    """
    nest = dedicated_no_mail_nest
    user = nest["user"]
    # The import seals at ingest and fails closed without a recipient seal key
    # on file; an app's enable-mail flow publishes one, and a throwaway one
    # stands in for it here (the nest only ever seals to it).
    with WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])

    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message(s) for s in ("first old message", "second old message")],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("an archived message")]),
    ]
    with FakeImapSource(mailboxes, cert=session_cert(), password=SOURCE_PASSWORD) as source:
        result = fauna_ffi.harness_mail_import(
            nest["url"],
            bytes(user["signing_key"]),
            host="localhost",
            port=source.port,
            username=source.username,
            password=SOURCE_PASSWORD,
            ca_pem=session_ca_pem(),
        )
        assert result["step"] == "Done", f"the import did not finish: {result}"
        assert (result["imported"], result["skipped"], result["errored"]) == (3, 0, 0), (
            f"the import did not bring all three messages across: {result}"
        )
        assert (source.username, SOURCE_PASSWORD) in source.logins, (
            f"the client never signed in with the password; logins seen: {source.logins}"
        )
        assert sorted(source.fetched_uids) == [("Archive", 1), ("INBOX", 1), ("INBOX", 2)], (
            f"the source was not read in full: {source.fetched_uids}"
        )

        assert_the_password_never_reached_the_nest(nest, source)
