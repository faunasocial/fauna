"""E2E: the mail-aliases page is USABLE after a mid-session mail enable.

Target state: ``docs/goal/behavior/mail-aliases.md:82`` § Kind 5 — Disposable —
"the user clicks ``mail-aliases-generate-disposable-button`` → client calls
``fauna.bridges.generate_disposable_alias(...)`` → nest generates a 6-character
base32 token ... constructs the address ``<handle>-temp-<token>@<our-domain>``,
returns it". The nest derives the address from the actor's canonical exact alias
**server-side**; the goal doc names *no* client-side precondition for the mint.

The deadlock this closes, found by the convention 11
permissive actuation sweep. Every app gates the page's three mutating controls
on ``default_domain`` being present in the client snapshot — linux
``settings/mail_aliases.rs``, windows ``MailAliasesViewModel.CanManage``, tui,
web ``MailAliasesSection.svelte``, apple ``MailAliasesView``, android
``MailAliasesScreen``. But ``default_domain`` has exactly one writer: the shared
``MailAliasesMachine::refresh`` (``libs/fauna-client-mail-settings/src/aliases.rs``),
which derives it from the *listed rows*. So a client whose snapshot lacks the
domain has every control that could refresh it disabled — a dead page.

It is reachable by the ordinary journey this test drives: **enable mail, then
open Aliases.** The page hydrated at login, before the enable. Apps that
re-hydrate on the navigation edge (tui, web, windows, android) recover on their
own; linux hydrated once at settings-shell build and never again, so it stayed
dead — measured 2026-08-28 at 60 s, and hidden all along because the e2e action
clicked the disabled button directly. ``refresh_via_generate`` now waits for
*enabled* (convention 8: drive it the way a user would), and this test states
the invariant on its own.

Why ``handled_logged_in_app``: ``default_domain`` is derived from listed rows,
so a handle-less actor legitimately has none — no canonical alias is ever
minted and the controls are *correctly* disabled. A handled actor's mail-enable
mints ``<handle>@<domain>`` (``ensure_canonical_handle_alias``;
``mail-aliases.md:34``), so a live snapshot must show a domain and the controls
must come alive.

tier_3: a real client driver drives the real ``fauna-nest`` binary end-to-end
(UI → shared ``MailAliasesMachine`` → WS-RPC → nest → re-render).

android's tier_3 run stays host-emulator-gated like every other android e2e
test; its render is proven at the Compose-content
level (``MailAliasesContentTest.kt``, ``MailSettingsContentTest.kt``).
"""

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
]

# The three controls every app gates on `default_domain`. Fix (1) — re-hydrate
# on the navigation edge — brings all three back, which is why the test asserts
# all three rather than only the one the mint needs.
_GATED_CONTROLS = [
    "mail-aliases-generate-disposable-button",
    "mail-aliases-add-button",
    "mail-aliases-import-button",
]


@pytest.mark.feature("mail-aliases")
def test_aliases_page_is_usable_after_enabling_mail_midsession(handled_logged_in_app):
    """Enable mail, then open Aliases: the page's controls must be usable
    without restarting the app, and the disposable mint must actually work
    through the enabled control."""
    app = handled_logged_in_app

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    app.mail_aliases.navigate()
    assert app.mail_aliases.is_page_visible(), (
        f"mail-aliases page should be reachable; error: {app.error_text()!r}"
    )

    # The deadlock assertion. A control still disabled here means the page has
    # no way forward from the UI: `default_domain` is unset and every control
    # that could set it is off. `wait_until_enabled` raises with the element's
    # own diagnosis, so the failure names itself (convention 6).
    for control in _GATED_CONTROLS:
        app.driver.wait_until_enabled(control, timeout=15.0)

    # …and the enabled control genuinely works: minting through it adds a
    # `-temp-` row, so the assertion above is about a live page, not merely a
    # sensitive widget.
    before = app.mail_aliases.disposable_count()
    app.mail_aliases.refresh_via_generate()
    assert app.mail_aliases.wait_for_disposable_count(before + 1), (
        f"minting through the enabled generate button should add a disposable "
        f"row (had {before} before); rows={app.mail_aliases.patterns()!r}; "
        f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
    )
