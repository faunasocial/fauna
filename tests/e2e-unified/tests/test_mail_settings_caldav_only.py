"""tier_3: the mail-settings page's CalDAV-only render gate — a CalDAV-enabled,
email-never-enabled actor sees the shared credential-management section (the one
bridge credential that AUTHs IMAP+SMTP+CalDAV+CardDAV+WebDAV) and the CalDAV MUA
row, but NOT the email-specific IMAP/SMTP rows.

docs/goal/ui/mail-settings.md § CalDAV-only mailbox / § Element IDs /
§ Credential-management reachability: `credential_management_reachable` =
`enabled || caldav_enabled || carddav_enabled || serves_webdav_set`, computed once
in shared Rust. `mail-settings-add-credential-button` / `-rotate-keys-button` /
`-keys-info` gate on that disjunction, NOT on `enabled` alone (code-verified
2026-07-18 against macOS's `MailSettingsView.manageSection` and linux's
`mail.rs::manage_group` — both show the section whenever a mailbox exists, with
no separate `enabled` check; the doc's older wording said "when enabled" for these
three ids and was corrected in the same commit as this test). The
`mail-settings-mua-*` rows stay per-protocol: IMAP/SMTP on `enabled`, CalDAV host/
port on `caldav_enabled`.

How the credential arrives: the user's own **Add credential** gesture. On a
deployment serving CalDAV with mail off, an account holding no MSEK still sees
the credential-management section, and its first Add credential IS the enable —
the read recipe, no submission token (`mail-settings.md` § Layout & flow →
*CalDAV-only mailbox*; `mail-credentials.md` § Trigger taxonomy, *The
calendar-only credential*, path (3)). Until 2026-10-26 this file minted through
the `enable_caldav_mailbox` test-agent command
(`helpers/mail_dedicated_nest.py::mint_caldav_mailbox`), which stays green with
that production path broken; the raw-protocol CalDAV tests keep that seam, this
file drives the page.

Why this is worth a dedicated assertion even though the CalDAV-only flow is
proven end-to-end elsewhere: those tests drive a raw CalDAV MUA over the wire
and never read the mail-settings PAGE itself, so a regression in the page's
per-row visibility gate (e.g. a client re-introducing a stale `enabled`-only
check, the exact bug class `mail-settings.md`'s CalDAV-only lift fixed) would
pass every existing CalDAV test and go unnoticed. This file reads the page.

Two tests:

- `test_caldav_only_mail_settings_render_gate` — every app: the gesture on a
  domainless box, then the per-row gate (the CalDAV row's host is the nest's own
  locator, the email rows stay hidden, the credential section renders). Every
  app reads the one shared `MailSettingsMachine`, whose `AddCredential` mints
  the first credential (§ Implementation status today), so no app is scoped out.
- `test_a_bare_account_mints_its_first_credential_from_add_credential` — tui:
  the same gesture on a CalDAV-only box running a real MDA, with a chosen
  password a stock CalDAV client then signs in with — proof the first
  credential the page mints is a real one.
"""

from __future__ import annotations

import secrets
import time
from urllib.parse import urlparse

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [pytest.mark.tier_3]
# App marks live on each TEST: the two run on different columns, and a
# module-level app mark credits each test with a witness on every app listed.

# The first credential's display name. "Default" derives the credential id
# `default`, whose username is the bare `<handle>@<domain>`
# (`mail-credentials.md` § MUA-username).
_FIRST_CREDENTIAL = "Default"


def _add_first_credential(app, password: str) -> None:
    """The user's gesture: Add credential on an account holding no MSEK, with a
    chosen password — then wait for the row it mints."""
    app.mail_settings.add_credential_plain(_FIRST_CREDENTIAL, password)
    assert app.mail_settings.wait_for_credential_count(
        1, timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        "Add credential on an MSEK-less account of a CalDAV-serving, mail-off "
        "deployment must mint exactly one app password (mail-settings.md § Layout "
        f"& flow → CalDAV-only mailbox); got {app.mail_settings.credential_count()}. "
        f"mail-page error: {app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )


@pytest.mark.feature("turn-on-mail")
@pytest.mark.feature("calendar-in-standard-apps")
@pytest.mark.android
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.ios
def test_caldav_only_mail_settings_render_gate(dedicated_no_mail_app, dedicated_no_mail_nest):
    """Turn the deployment's CalDAV on with mail off, let a dedicated fresh actor
    (never enabled mail) mint its first credential from Add credential, then
    assert the mail-settings page's per-row visibility gate: the CalDAV MUA row +
    the shared credential-management controls show; the email-only IMAP/SMTP
    rows stay hidden.
    """
    app = dedicated_no_mail_app
    driver = app.driver
    admin = dedicated_no_mail_nest["admin"]

    # Fixture setup, not behaviour under test: the deployment's CalDAV toggle is
    # the admin's choice, made over the admin's own wire kind. Set BEFORE the
    # page mounts — its machine reads the deployment's toggles from
    # `fauna.setup.status` at hydrate.
    with WsRpcAdminClient(
        dedicated_no_mail_nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        assert client.call("fauna.bridges.set_caldav_enabled", {"enabled": True}).get("ok") is True

    app.mail_settings.navigate()
    # The way in exists before any credential does: the reachability predicate's
    # DAV arm (`mail-settings.md` § Credential-management reachability).
    driver.wait_for("mail-settings-add-credential-button", timeout=20.0)
    _add_first_credential(app, "CaldavOnlyRenderGatePw01Ab")

    # The CalDAV row is gated on the deployment's `caldav_enabled`, read at
    # hydrate; the credential above has landed, so the block is rendered.
    driver.wait_for("mail-settings-mua-caldav-host", timeout=20.0)

    # `is_visible_scrolled`: the port is the second line of the MUA block, which
    # can sit past the fold of a short window (windows reads UIA IsOffscreen). The
    # host row above was just waited for, so the grid is rendered and this
    # scroll targets a counted element.
    assert driver.is_visible_scrolled("mail-settings-mua-caldav-port"), (
        "mail-settings-mua-caldav-port must render alongside -caldav-host "
        f"(both gated on caldav_enabled); error: {app.error_text()!r}"
    )

    # The two VALUES a calendar app needs (calendar-in-standard-apps outcome 28):
    # on a nest with no domain — this one is reached at a bare loopback locator —
    # the host is that locator itself (no `mail.` prefix) and the port is the
    # admin-set CalDAV port the nest holds (`caldav-server.md` § Network
    # exposure; the shared `MuaInstructions::for_node_url` +
    # `apply_admin_caldav_port`).
    locator = urlparse(dedicated_no_mail_nest["url"]).hostname
    with WsRpcAdminClient(
        dedicated_no_mail_nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        caldav_port = int(client.call("fauna.bridges.get_mail_config", {})["caldav_port"])
    host_text = (driver.get_text("mail-settings-mua-caldav-host") or "").strip()
    assert host_text == locator, (
        f"on a domainless nest the CalDAV host must be the nest's own locator "
        f"{locator!r}, got {host_text!r}"
    )
    deadline = time.monotonic() + 20.0
    while (
        time.monotonic() < deadline
        and (driver.get_text("mail-settings-mua-caldav-port") or "").strip() != str(caldav_port)
    ):
        time.sleep(0.3)  # sleep-ok: poll interval of a deadline loop
    port_text = (driver.get_text("mail-settings-mua-caldav-port") or "").strip()
    assert port_text == str(caldav_port), (
        f"the CalDAV port must be the one the nest holds ({caldav_port}), got {port_text!r}"
    )

    # Email was never enabled on this dedicated actor — the email-specific MUA
    # rows must stay hidden (mail-settings.md § MUA setup instructions).
    assert driver.is_absent("mail-settings-mua-imap-host"), (
        "mail-settings-mua-imap-host is gated on `enabled` and must stay hidden "
        "on a CalDAV-only (email-disabled) mailbox"
    )
    assert driver.is_absent("mail-settings-mua-imap-port"), (
        "mail-settings-mua-imap-port must stay hidden on a CalDAV-only mailbox"
    )
    assert driver.is_absent("mail-settings-mua-smtp-host"), (
        "mail-settings-mua-smtp-host must stay hidden on a CalDAV-only mailbox"
    )
    assert driver.is_absent("mail-settings-mua-smtp-port"), (
        "mail-settings-mua-smtp-port must stay hidden on a CalDAV-only mailbox"
    )

    # The shared credential-management section renders on
    # `credential_management_reachable` (= caldav_enabled here), NOT on `enabled`
    # — a CalDAV-only actor still needs to see/rotate the one bridge credential
    # their calendar app authenticates with.
    assert driver.is_visible("mail-settings-add-credential-button"), (
        "mail-settings-add-credential-button gates on credential_management_"
        "reachable (caldav_enabled counts), not on enabled alone — see "
        "mail-settings.md § Element IDs"
    )
    assert driver.is_visible("mail-settings-keys-info"), (
        "mail-settings-keys-info must render — the credential-management "
        "section is reachable via caldav_enabled"
    )
    assert app.mail_settings.credential_count() == 1, (
        "the first Add credential mints exactly one shared `default` credential "
        f"(no separate CalDAV credential); got "
        f"{app.mail_settings.credential_count()} rows"
    )


_BARE_ACCOUNT_PASSWORD = "BareAccountFirstCredentialPw01Ab"


@pytest.mark.feature("calendar-in-standard-apps")
@pytest.mark.tui
def test_a_bare_account_mints_its_first_credential_from_add_credential(
    app, dedicated_caldav_only_nest, request
):
    """An account that onboarded before its deployment served CalDAV — signed
    in, no MSEK, never through a first setup on this box — gets its calendar
    credential from the page: Add credential, a chosen password, and a stock
    CalDAV client signs in with it against the real MDA (`mail-credentials.md`
    § Trigger taxonomy, *The calendar-only credential*, path (3)).

    The box is `dedicated_caldav_only_nest`: CalDAV on, mail never enabled, an
    MDA bound to CalDAV alone. The account is self-registered and signed in by
    session injection, so the wizard's first-setup auto-mint never ran for it —
    the bare account the path exists for.
    """
    from helpers.admin_wire import admit_adult
    from conftest import _login_app_as
    from helpers.caldav_client import sign_in_round_trip

    handle = dedicated_caldav_only_nest
    nest = handle.nest
    localpart = "bare" + secrets.token_hex(3)
    user = admit_adult(nest, localpart, domain=handle.domain, token=True)
    _login_app_as(app, request, nest, user)

    app.mail_settings.navigate()
    app.driver.wait_for("mail-settings-add-credential-button", timeout=20.0)
    assert app.mail_settings.credential_count() == 0, (
        "fixture setup failed: the bare account must start with no app password"
    )
    _add_first_credential(app, _BARE_ACCOUNT_PASSWORD)
    app.driver.wait_for("mail-settings-mua-caldav-host", timeout=20.0)
    assert app.mail_settings.enabled_toggle_state() == "off", (
        "a calendar-only credential is not an email enable: the toggle must stay "
        f"off, but reads {app.mail_settings.enabled_toggle_state()!r}"
    )

    sign_in_round_trip(
        f"https://127.0.0.1:{handle.caldav_port}",
        f"{localpart}@{handle.domain}",
        _BARE_ACCOUNT_PASSWORD,
    )
