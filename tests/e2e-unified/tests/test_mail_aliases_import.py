"""E2E coverage for the mail-aliases page's bulk paste-import sheet
(``mail-aliases-import-sheet``, opened by ``mail-aliases-import-button``) —
Plan 3 (recipient-whitelist client UI) of the recipient-whitelist
alias-import design (tracked internally).

The sheet is a multi-line textarea (one address per line) that dispatches
``MailAliasesAction::Import`` -> the shared
``fauna_client_mail_settings::MailAliasesMachine`` -> the User-class
``fauna.bridges.import_account_aliases`` RPC (best-effort per line, idempotent
-- Plan 1 of the same design doc), rendering a per-line outcome summary into
``mail-aliases-import-result`` (i18n ``mail_aliases.import_result`` =
``'{created} created · {existed} already existed · {invalid} invalid'``,
followed by one ``mail_aliases.import_invalid_line`` = ``'{address} — {reason}'``
row per invalid line -- ``mail-aliases.md:199-201`` requires the reason be
rendered, not just counted). The full outcome matrix (created /
skipped_duplicate / invalid + reasons) is already proven at the WS-RPC layer by
the tier_3 ``tests/e2e-unified/tests/api/test_alias_import.py`` -- this file
does NOT duplicate that ground truth; it is the render/UI-contract layer only:
does the sheet open, accept a mixed batch (one address that already exists,
one fresh address, one malformed line), submit, render the right
created-count plus the invalid line's own reason, and does the list refresh to
show the newly-created row.

tier_3: a real client driver against a real ``fauna-nest`` (``logged_in_app``
spins one). Same tier + same seeding idiom as ``test_mail_aliases.py``
(``_seed_exact`` + ``refresh_via_generate`` to establish ``default_domain``
before the add-sheet's Create can run) -- there is no tier_2 mocked-backend
pattern for the mail-aliases surface.

Import validates domain-ownership (unlike the single-alias
``create_account_alias``, which does not), so the test provisions a real
local domain via the admin ``add_local_domain`` RPC first, mirroring
``test_alias_import.py``'s seeding.

Implemented on windows (Task 9), linux (Task 8), macOS (Task 10 -- one shared
FaunaKit ``MailAliasImportSheet`` serves macOS/iOS, though only the macos
marker is registered here so far), web (Task 7), and android (Task 11,
``MailAliasesScreen.kt``'s ``ImportSheet``, Compose-content-tested in
``MailAliasesContentTest.kt::importSheetOpensAndSubmits`` +
``importResultRendersCountsAndInvalidReasons``). android's tier_3 e2e stays
host-emulator-gated like every other android e2e test.

**The sheet must stay open on submit.** ``read_import_result()`` polls
``is_visible("mail-aliases-import-result")``, and that element lives *inside*
the sheet -- a client that dismisses the sheet on success hides the very
element this test asserts on (the windows leg's latent bug). linux/apple keep
it open and render the outcome in place; Cancel closes it.
"""

import re
import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.mail_aliases import default_alias_domain

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    pytest.mark.tui,
    pytest.mark.android,
]

DOMAIN = "aliases-import-ux-e2e.test"


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _ensure_domain(admin) -> None:
    """Idempotently make the nest own ``DOMAIN`` (re-add is a no-op)."""
    admin.call(
        "fauna.bridges.add_local_domain",
        {
            "domain": DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        },
    )


def _seed_exact(client, pattern) -> None:
    """Seed a canonical exact alias on ``DOMAIN`` so ``default_domain`` resolves
    (the add-sheet's Create needs it — mirrors ``test_mail_aliases.py``'s
    ``_seed_exact``)."""
    client.call(
        "fauna.bridges.create_account_alias",
        {"kind": "exact", "local_domain": DOMAIN, "pattern": pattern, "controls": {"label": ""}},
    )


def _list(client):
    return client.call("fauna.bridges.list_account_aliases", {})["aliases"]


def _delete_all(client) -> None:
    """Leave the session-scoped actor as found (net-zero on the shared nest)."""
    for row in _list(client):
        try:
            client.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


@pytest.mark.feature("mail-aliases")
def test_import_addresses_reports_and_relists(logged_in_app, test_user, nest_instance):
    """Paste a mixed batch (one address that already exists, one fresh
    address, one malformed line), submit — the sheet renders a created-count
    of 1 and the list refreshes to include the new address."""
    with _admin_client(nest_instance) as admin:
        _ensure_domain(admin)

    handle = "seed" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        _seed_exact(api, handle)
        try:
            app = logged_in_app
            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible(), (
                "mail-aliases-add-button should be visible after navigating to "
                f"the mail-aliases page; error: {app.error_text()!r}"
            )
            # Re-list the seeded alias into the snapshot (sets default_domain,
            # the precondition add_exact's Create needs — same idiom as
            # test_mail_aliases.py).
            app.mail_aliases.refresh_via_generate()
            assert app.mail_aliases.wait_for_pattern(f"{handle}@{DOMAIN}"), (
                "the seeded canonical exact alias should render after the "
                f"mint's refresh; rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )

            # Create "me-netflix" through the add-sheet first, so re-pasting it
            # in the import batch is reported as already-existed.
            #
            # ⚠ The add-sheet takes a LOCALPART, so it mints on the snapshot's
            # `default_domain` — the CANONICAL row's domain, not the seeded
            # `DOMAIN` (`derive_default_domain` reads canonical → first exact →
            # first; mail-aliases.md:166, :444). The already-existed line below
            # must therefore carry that same domain, or it names a different
            # address and comes back "created", making the count 2.
            minted_on = default_alias_domain(api)
            app.mail_aliases.add_exact("me-netflix")
            assert app.mail_aliases.wait_for_pattern(f"me-netflix@{minted_on}"), (
                "add_exact should create me-netflix before the import batch; "
                f"rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )

            # An import line carries its OWN full address, so `me-amazon` stays
            # on the seeded domain — which makes this batch cover both: a fresh
            # address on a non-default domain, and a collision on the default one.
            lines = [
                f"me-amazon@{DOMAIN}",  # fresh -> created
                f"me-netflix@{minted_on}",  # already exists -> already existed
                "not-an-address",  # malformed -> invalid
            ]
            try:
                app.mail_aliases.import_addresses(lines)
            except TimeoutError as exc:
                pytest.fail(
                    "mail-aliases-import-sheet did not render its IDs "
                    f"(mail-aliases-import-button / -textarea / -submit-button): "
                    f"{exc}; error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
                )

            result_text = app.mail_aliases.read_import_result()
            assert result_text, (
                "mail-aliases-import-result should render after submitting the "
                f"import sheet; error: {app.mail_aliases.page_error_text(timeout=2.0)!r} "
                f"diagnose={app.driver.diagnose('mail-aliases-import-result')!r}"
            )
            created_match = re.search(r"(\d+)\s*created", result_text)
            assert created_match and created_match.group(1) == "1", (
                "mail-aliases-import-result should render a created count of 1 "
                f"(one fresh address, one already-existed, one invalid); got "
                f"{result_text!r}; error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            # mail-aliases.md:199-201 requires the invalid line's own reason be
            # rendered alongside the counts, not just tallied.
            assert "not-an-address" in result_text and "malformed address" in result_text, (
                "mail-aliases-import-result should render the invalid line's "
                f"address and reason ('not-an-address — malformed address'); got "
                f"{result_text!r}; error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )

            # The list refreshed to include the newly-created address.
            assert app.mail_aliases.wait_for_pattern(f"me-amazon@{DOMAIN}"), (
                "the import should relist the newly-created me-amazon address; "
                f"rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )

            # Ground truth over the same user WS-RPC surface: both the
            # already-existing and newly-created addresses are present; the
            # malformed line produced no alias.
            patterns = {r["pattern"] for r in _list(api)}
            assert "me-amazon" in patterns and "me-netflix" in patterns, (
                f"both me-amazon and me-netflix should exist on the nest; got {patterns!r}"
            )
        finally:
            _delete_all(api)
