"""E2E: the mail sub-pages are gated at their entry on whether mail is on.

Target state: ``docs/goal/ui/mail-settings.md`` § Errors & edge cases, *The
mail-usable gate* — every mail sub-page (Mail aliases, Mail spam, Export
mailbox, Import mailbox, Mail lists, Mail list members) and its settings rail
row is gated at its ENTRY on the one shared answer
``MailSettingsSnapshot::mail_usable()``: blocked, naming the Mail & Calendar
step (``mail_settings.usable_needs_mail_on``), while mail is off; possible once
it is on, read from the machine's LIVE snapshot so turning mail on lights the
rows at once; and a sub-page reached by another door while blocked shows only
its heading, the reason and Back. The rules are ``docs/goal/ui/README.md`` §
Offer only what is possible (rule 2: disabled with its reason within eyeshot;
rule 4: the gate is at the entry, never at the submit).

This journey witnesses ``docs/features/turn-on-mail.md`` outcome 29. The
in-process tui tests (``apps/fauna-tui/src/settings/mod.rs``,
``the_rail_gates_every_mail_subpage_row_on_mail_usable`` and
``a_mail_subpage_entered_while_mail_is_off_opens_on_its_reason_only``) prove
the paint against a seeded machine; this proves it against a real nest's answer,
with mail turned on through the app's own page (convention 8).

**The reason is read as painted** (``PlatformDriver.blocked_reason``) and
compared with the ``S.`` string, never a literal.

The blocked sub-page's own reason line is read by its id,
``mail-subpage-gate-reason``, beside its heading and Back and with none of the
page's controls; it is gone once mail is on.

**What this journey does not reach.** Before the machine's first hydrate the
rows read *waiting* (``mail_settings.usable_reading``); that window closes on
its own as the read lands, and a journey could only race it (convention 14).
The paint of that state is the in-process test's.

``dedicated_no_mail_app``: a freshly-registered actor on its own nest, so
"mail has never been turned on" holds by construction, whatever other modules
did to the shared session actor — and turning mail on here touches no one
else's.

tier_3: a real tui driver against a real fauna-nest (rail → shared
``MailSettingsMachine`` snapshot → ``mail_usable()`` → possibility seam →
automation registry).

tui leads; the other apps join by adding their marker once their gate is built
and their run is green. macos (``MailUsableGate.swift``) has joined, and ios
(the same shared view) has joined on its verify request. linux
(``settings/mail_gate.rs``'s ``GatedPage``; its rail rows publish the painted
reason as the ``reason`` attr ``blocked_reason`` reads, as ``possible::paint``
does) and web (``#lib/mail-gate``'s ``subpageGate``) have joined on their
verify requests. android has joined: its Settings landing paints the six rows
(``MailSubpageRows``, read off the session's ``MailUsableVM``) and each
sub-page opens behind ``MailSubpageGate``; its first run is owed on the host
emulator. windows has not built it yet.
"""

from __future__ import annotations

import pytest

from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
]

_UI_S = 30.0

# The six gated rows, in rail order (`mail-settings.md` § the mail-usable gate).
_MAIL_SUBPAGE_ROWS = (
    "mail-aliases",
    "mail-spam",
    "mail-export",
    "mail-import",
    "mail-lists",
    "mail-list-members",
)

# The Mail aliases page's own controls: none may be painted while the gate is
# blocked (no form whose submit could not succeed).
_ALIASES_CONTROLS = (
    "mail-aliases-add-button",
    "mail-aliases-generate-disposable-button",
    "mail-aliases-import-button",
)


def _row(key: str) -> str:
    return f"settings-nav-row[{key}]"


def _open_settings_root(driver) -> None:
    """The Settings shell with no sub-page open — where tui paints the rail."""
    driver.set_state({"nav": {"stack": [{"view": "settings"}]}})


def _rows_blocked_with(driver, reason: str) -> list[str]:
    """Every gated row that is NOT shown, disabled, with ``reason`` painted —
    empty when all six are."""
    wrong = []
    for key in _MAIL_SUBPAGE_ROWS:
        row = _row(key)
        if (
            driver.is_absent(row)
            or driver.is_enabled(row)
            or driver.blocked_reason(row) != reason
        ):
            wrong.append(key)
    return wrong


def _rows_live(driver) -> list[str]:
    """Every gated row that is NOT shown and live — empty when all six are."""
    return [
        key
        for key in _MAIL_SUBPAGE_ROWS
        if driver.is_absent(_row(key)) or not driver.is_enabled(_row(key))
    ]


def _diagnose_rows(driver) -> str:
    return "; ".join(
        driver.diagnose(_row(key), attrs=("enabled", "reason")) for key in _MAIL_SUBPAGE_ROWS
    ) + f"; {driver.diagnose('settings-nav-row[mail-settings]', attrs=('enabled',))}"


@pytest.mark.feature("turn-on-mail")
def test_mail_subpage_rows_wait_on_mail_and_light_up_once_it_is_on(dedicated_no_mail_app):
    app = dedicated_no_mail_app
    driver = app.driver
    needs_mail_on = S.mail_settings.usable_needs_mail_on

    # 1. Mail off: the six rows are shown, disabled, naming the one step — and
    #    that step's own row stays live. The reason is the causal barrier: the
    #    rows read *waiting* until the first hydrate lands, then the nest's answer.
    _open_settings_root(driver)
    wait_until(
        lambda: not _rows_blocked_with(driver, needs_mail_on),
        _UI_S,
        diagnose=lambda: (
            f"rows not blocked with {needs_mail_on!r}: "
            f"{_rows_blocked_with(driver, needs_mail_on)}; {_diagnose_rows(driver)}"
        ),
    )
    assert driver.is_enabled("settings-nav-row[mail-settings]"), (
        "the Mail & Calendar row — the one step that unblocks the rest — must stay "
        f"live while mail is off: {_diagnose_rows(driver)}"
    )

    # 2. A sub-page reached by another door (a nav patch, not the disabled row)
    #    shows only its heading, the reason and Back — none of its controls. The page's
    #    heading is the barrier that the sub-page, not the root, is up.
    app.mail_aliases.navigate()
    wait_until(
        lambda: driver.is_visible("page-heading")
        and driver.get_text("page-heading") == S.mail_aliases.title,
        _UI_S,
        diagnose=lambda: f"the Mail aliases page never opened: {driver.diagnose('page-heading')}",
    )
    assert driver.get_text("mail-subpage-gate-reason") == needs_mail_on, (
        f"the blocked sub-page's reason line must name the one step ({needs_mail_on!r}): "
        f"{driver.diagnose('mail-subpage-gate-reason')}"
    )
    # Back is the shell's own. On the desktop shells it is `settings-nav-back`;
    # on mobile the pushed sub-page keeps the platform's navigation-stack back,
    # which carries no id (`settings-nav-back` is a declared optional absence
    # in ui-actual-ios.yaml) — convention 3, keyed on the form factor.
    if not driver.is_mobile():
        assert driver.is_visible("settings-nav-back"), (
            f"the blocked sub-page keeps its way back: {driver.diagnose('settings-nav-back')}"
        )
    records = driver.registry_snapshot() or []
    page_ids = sorted(
        str(rec.get("id") or "")
        for rec in records
        if str(rec.get("id") or "").startswith("mail-aliases-")
    )
    assert not page_ids, (
        "the Mail aliases page painted its own controls while mail is off — the gate "
        f"is at the entry, never at the submit (ui/README.md rule 4): {page_ids!r}"
    )
    for control in _ALIASES_CONTROLS:
        assert driver.is_absent(control), (
            f"{control} is shown on a sub-page whose gate is blocked: "
            f"{driver.diagnose(control)}"
        )

    # 3. Turn mail on through the Mail & Calendar page itself. The toggle is
    #    live only once its read has landed (§ Controls wait for their reads).
    app.mail_settings.navigate()
    driver.wait_until_enabled(app.mail_settings.ENABLED_TOGGLE_ID, timeout=_UI_S)
    app.mail_settings.enable_mail("Default")
    assert app.mail_settings.wait_for_enabled_toggle_state(
        "on", timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        "mail must read on after the enable; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # 4. The rail reads the live snapshot: the six rows are live, no relaunch.
    _open_settings_root(driver)
    wait_until(
        lambda: not _rows_live(driver),
        _UI_S,
        diagnose=lambda: f"rows still not live after mail on: {_rows_live(driver)}; "
                         f"{_diagnose_rows(driver)}",
    )

    # …and the sub-page that showed only its reason now shows its page.
    app.mail_aliases.navigate()
    assert app.mail_aliases.is_page_visible(timeout=_UI_S), (
        "the Mail aliases page should open whole once mail is on; "
        f"{driver.diagnose('mail-aliases-add-button')}; error: {app.error_text()!r}"
    )
    assert driver.is_absent("mail-subpage-gate-reason"), (
        "the gate's reason line must go once mail is usable: "
        f"{driver.diagnose('mail-subpage-gate-reason')}"
    )
