"""E2E witness for the deployment's own Microsoft app registration: an admin
sets it on `admin-mail`, and the Outlook import signs in with it.

Target state: `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in —
the device-code grant, paragraph *An admin-supplied registration*: the admin's
pair persists in nest state, reaches every app on `fauna.setup.status`, and
`MicrosoftRegistration::effective` makes it the client ID the sign-in sends —
live even while the project's own constant is still empty. UX/IDs: ui.yaml
`admin-mail` (`admin-mail-microsoft-*`) and `mail-import`
(`mail-import-source-oauth-button`).

**tier_3.** Real tui, real nest, real shared-Rust device flow. The only stub is
the identity platform's far end — the session's `fake_microsoft_identity`,
which the `mail_import_source` marker points every app launch at
(`FAUNA_E2E_MICROSOFT_AUTHORITY`, `e2e-gated-seeds.md`). The seed replaces the
whole authority and ignores the tenant, so the fake's record of the client ID
it was asked with is the witness of the precedence.

**Every mutation goes through the app UI** (convention 8): the admin types the
registration and presses Save; the user picks Outlook and presses the button.
The one read outside the UI is the fake's own record of the request.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

from actions.mail_import import MailImportActions
from helpers import budgets
from helpers.mail_dedicated_nest import login_as_nest_admin
from helpers.mail_import_ui import save_microsoft_registration
from i18n.strings import S

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "fakes"))
from fake_microsoft_identity import session_microsoft_identity  # noqa: E402

pytestmark = [
    pytest.mark.tier_3,
    # The dedicated-nest witness stays tui-only; linux and android walk the same
    # sign-in through test_mail_import.py::test_outlook_signs_in_with_microsoft_and_imports,
    # and the admin section itself is witnessed on every app that renders it by
    # test_admin_mail.py::test_microsoft_registration_save_round_trips.
    pytest.mark.tui,
    # Seeds the identity fake's authority (and the CA its certificate chains to)
    # into every app launch.
    pytest.mark.mail_import_source,
]

# A registration of the admin's own, distinctive so it cannot be the project's.
ADMIN_CLIENT_ID = "0b5c1a2e-3f4d-4e5a-8b6c-7d8e9f0a1b2c"
ADMIN_TENANT = "contoso.example"


@pytest.mark.feature("mailbox-import")
def test_an_admin_set_microsoft_registration_is_the_client_id_the_sign_in_sends(
    app, dedicated_no_mail_nest
):
    # A plain dedicated nest: the registration is nest-wide setup state and the
    # sign-in goes to the identity fake, so no bridge is ever dialled and a mail
    # venue (whose idling bridges the rebind sweep makes a test restart) buys
    # nothing. tui-only today, so the raw nest URL is reachable.
    nest = dedicated_no_mail_nest
    login_as_nest_admin(app, nest, nest["url"])
    fake = session_microsoft_identity()

    try:
        # The admin enters their own registration; the field reads back what
        # the nest stored (normalised — the GUID lowercased) once the save
        # re-reads fauna.setup.status.
        save_microsoft_registration(app, ADMIN_CLIENT_ID.upper(), ADMIN_TENANT)

        # The user's Outlook import signs in with it.
        wizard = MailImportActions(app.driver)
        wizard.navigate()
        assert wizard.is_page_visible()
        wizard.select_source(S.mail_import.source_outlook)
        wizard.set_username("person@contoso.example")
        n = len(fake.issued_codes) + 1
        app.driver.click("mail-import-source-oauth-button")
        fake.wait_for_code(count=n, timeout=budgets.RPC_ROUNDTRIP_S)
        asked = fake.device_code_requests[-1]
        assert asked.get("client_id") == ADMIN_CLIENT_ID, (
            "the sign-in asked with the project's registration, not the admin's: "
            f"{asked!r}; wizard error {wizard.error_text()!r}"
        )
    finally:
        # Leave the nest on the project's registration for whatever runs next.
        save_microsoft_registration(app, "", "")
