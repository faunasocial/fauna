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

Why this is worth a dedicated assertion even though the CalDAV-only flow is
already proven end-to-end (8/8 tier_3 `dedicated_mail_nest`-family
files green on macOS, including CalDAV round-trips minted via this same
`enable_caldav_mailbox` command): those tests drive a raw CalDAV MUA over the
wire and never read the mail-settings PAGE itself, so a regression in the
page's per-row visibility gate (e.g. a client re-introducing a stale
`enabled`-only check, the exact bug class `mail-settings.md`'s CalDAV-only
lift fixed) would pass every existing CalDAV test and go unnoticed. This test
reads the page directly.

Scoped to the apps whose test agent implements `enable_caldav_mailbox`
(`helpers/mail_dedicated_nest.py::mint_caldav_mailbox`): linux, macos, tui,
web (`$lib/mail-caldav-e2e`), windows — and **ios, which joined 2026-09-21**.
iOS was never short of anything here: its shell dispatches the very same shared
`CaldavMailboxTestCommand` macOS does (`Fauna-iOS/App/FaunaApp.swift`), and the
page under test is the shared FaunaKit `MailSettingsView`, so both apple targets
render one set of ids off one `credential_management_reachable` disjunction.
Nothing here speaks a mail protocol — the mint is a test-agent command and the
assertions read the settings PAGE — so this outcome sits on the near side of the
Go-bridge build wall that holds the rest of the CalDAV family on macOS.
"""

from __future__ import annotations

import pytest

from helpers.mail_dedicated_nest import mint_caldav_mailbox

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.ios,
]


@pytest.mark.feature("turn-on-mail")
def test_caldav_only_mail_settings_render_gate(dedicated_no_mail_app):
    """Seed caldav-on + email-off on a dedicated fresh actor (never enabled mail),
    then assert the mail-settings page's per-row visibility gate: the CalDAV MUA
    row + the shared credential-management controls show; the email-only IMAP/SMTP
    rows stay hidden.
    """
    app = dedicated_no_mail_app
    driver = app.driver

    # Seed BEFORE navigating: the mail-settings page's MailSettingsMachine snapshot
    # is built once when the page first mounts (no live resubscription to a
    # mutation from a separate machine instance — that's the same "throwaway VM"
    # snapshot-staleness class documented elsewhere in this suite), so minting
    # after navigating would race the already-mounted page's stale snapshot rather
    # than exercising the render gate itself. Sibling CalDAV tests
    # (test_caldav_admin_port_rebind.py, test_caldav_autoschedule_mailbox_less.py)
    # mint before touching any client UI for the same reason.
    mint_caldav_mailbox(driver, password=None)
    app.mail_settings.navigate()

    # Settle signal: the CalDAV row is gated on `caldav_enabled`, which the mint
    # just flipped — wait for it rather than the credential list (which may take
    # an extra snapshot round-trip to reflect the freshly-minted `default` row).
    driver.wait_for("mail-settings-mua-caldav-host", timeout=20.0)

    # `is_visible_scrolled`: the port is the second line of the MUA block, which
    # can sit past the fold of a short window (windows reads UIA IsOffscreen). The
    # host row above was just waited for, so the grid is rendered and this
    # scroll targets a counted element.
    assert driver.is_visible_scrolled("mail-settings-mua-caldav-port"), (
        "mail-settings-mua-caldav-port must render alongside -caldav-host "
        f"(both gated on caldav_enabled); error: {app.error_text()!r}"
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
        "enable_caldav_mailbox mints exactly one shared `default` credential "
        f"(no separate CalDAV credential); got "
        f"{app.mail_settings.credential_count()} rows"
    )
