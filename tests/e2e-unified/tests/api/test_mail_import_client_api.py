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
import threading
from pathlib import Path

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_import_evidence import (
    assert_never_reached_the_nest,
    assert_the_password_never_reached_the_nest,
)
from helpers.recipient_seal_key import provision_recipient_seal_key

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fakes"))
from fake_imap_source import (  # noqa: E402
    OUTLOOK_IMAP_HOST,
    FakeImapSource,
    SourceMailbox,
    sample_message,
    session_ca_pem,
    session_cert,
    session_cert_for,
    session_preset_redirect,
)
from fake_microsoft_identity import session_microsoft_identity  # noqa: E402

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


# The project's own client ID is still empty (`oauth_device.rs`'s `MICROSOFT`
# constant), so the deployment carries a registration of the admin's own — set
# here on the admin's WS-RPC session, as `admin-mail`'s Save sends it.
OUTLOOK_CLIENT_ID = "3a9e6c1d-5b7f-4e2a-8d0c-1f2e3d4c5b6a"
OUTLOOK_ADDRESS = "person@outlook.example"


@pytest.mark.feature("mailbox-import")
def test_the_microsoft_token_never_reaches_the_nest(dedicated_no_mail_nest):
    """Outcome 18: an Outlook import signs in with Microsoft — the device-code
    grant against the harness's fake authority — imports every message over
    `AUTHENTICATE XOAUTH2`, and afterwards neither the access token nor the
    refresh token the authority granted is in any file the nest keeps or in
    its log (`mailbox-migration.md` § Microsoft sign-in — the device-code
    grant, the "Witnessing outcomes 17 and 18" paragraph).

    The import must really have run on that token for the absence to mean
    anything: the Outlook server's own record shows one XOAUTH2 sign-in
    carrying it, and the import reached Done with every message across. The
    Outlook host is reached through the preset-host redirect seed
    (`e2e-gated-seeds.md` § The preset-host redirect seed), the authority
    through the Microsoft authority seed (§ The Microsoft authority seed).
    That the token is thrown away when the import ends is the machine's own
    state, pinned at tier 1 (`import.rs`'s token-state tests).
    """
    nest = dedicated_no_mail_nest
    user = nest["user"]
    admin = nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin.verify_key), signing_key=bytes(admin)
    ) as ws:
        ws.call(
            "fauna.bridges.set_microsoft_registration",
            {"client_id": OUTLOOK_CLIENT_ID, "tenant": "common"},
        )
    with WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as ws:
        provision_recipient_seal_key(ws, user["actor_id_bytes"])

    fake = session_microsoft_identity()
    host, port = session_preset_redirect()
    assert host == OUTLOOK_IMAP_HOST
    mailboxes = [
        SourceMailbox(
            name="INBOX",
            messages=[sample_message(s) for s in ("first outlook message", "second outlook message")],
        ),
        SourceMailbox(name="Archive", messages=[sample_message("an archived outlook message")]),
    ]
    with FakeImapSource(
        mailboxes,
        cert=session_cert_for(OUTLOOK_IMAP_HOST),
        port=port,
        username=OUTLOOK_ADDRESS,
        xoauth2_token=fake.access_token,
    ) as source:
        n = len(fake.issued_codes) + 1
        result: dict = {}

        def import_it() -> None:
            result.update(fauna_ffi.harness_mail_import_outlook(
                nest["url"],
                bytes(user["signing_key"]),
                username=OUTLOOK_ADDRESS,
                authority=fake.authority,
                host_map=f"{OUTLOOK_IMAP_HOST}=127.0.0.1:{port}",
                ca_pem=session_ca_pem(),
            ))

        importer = threading.Thread(target=import_it, daemon=True)
        importer.start()
        try:
            code = fake.wait_for_code(count=n, timeout=60.0)
        except TimeoutError:
            importer.join(timeout=60.0)
            raise AssertionError(f"the sign-in asked Microsoft for no code: {result}")
        assert fake.device_code_requests[-1].get("client_id") == OUTLOOK_CLIENT_ID, (
            f"the sign-in did not ask with the deployment's registration: "
            f"{fake.device_code_requests[-1]!r}"
        )
        fake.approve(code)
        importer.join(timeout=120.0)
        assert not importer.is_alive(), "the Outlook import never returned"

        assert result["step"] == "Done", f"the import did not finish: {result}"
        assert (result["imported"], result["skipped"], result["errored"]) == (3, 0, 0), (
            f"the import did not bring all three messages across: {result}"
        )
        assert source.xoauth2_auths == [(OUTLOOK_ADDRESS, fake.access_token)], (
            f"the import did not sign in with the granted token: {source.xoauth2_auths!r}"
        )
        assert source.logins == [], f"a password sign-in reached Outlook: {source.logins!r}"

        assert_never_reached_the_nest(nest, fake.access_token.encode(), "Microsoft access token")
        assert_never_reached_the_nest(nest, fake.refresh_token.encode(), "Microsoft refresh token")
