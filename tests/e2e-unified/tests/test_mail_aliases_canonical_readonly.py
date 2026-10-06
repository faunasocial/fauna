"""E2E: the canonical ``<handle>@<domain>`` alias row renders READ-ONLY (linux).

Target state: ``docs/goal/behavior/mail-aliases.md:249`` § Aliases UX — "The
canonical row is read-only ... the client renders it without the disabled-toggle,
edit, revoke, or delete controls and shows a 'primary address' marker instead,
because the nest rejects disabling / renaming / deleting it
(``canonical_alias_protected``)". The nest flags the row ``is_canonical = true``
(``:319``, computed from the runtime mail domain — never stored); the linux
``build_alias_row`` (`apps/fauna-linux/src/settings/mail_aliases.rs`) conditionally
omits the four mutating controls for that row. **No new ui.yaml id** — the
read-only row reuses the per-row element set, conditionally omitting the mutating
controls (``:249``).

Why this needs a *handled* login (the gap this closes): the shared
``logged_in_app`` actor is registered handle-less, so
``canonical_address_for_actor`` returns ``None`` and **no** row is ever flagged
canonical — which is why the canonical read-only render previously had only nest
conformance (``conformance_account_aliases::list_account_aliases_marks_canonical_row``)
+ shared-unit (``alias_view_projects_is_canonical``) coverage, never a client e2e.
``handled_logged_in_app`` is the handled counterpart: a nest whose primary mail
domain == handle domain == ``fauna.test`` and an actor with a claimed handle, so
enabling mail auto-mints the canonical ``<handle>@fauna.test``
(``ensure_canonical_handle_alias`` via ``provision_recipient_mls_pubkey``;
``mail-aliases.md:34``) and the page can render it.

tier_3: a real client driver drives the real ``fauna-nest`` binary end-to-end
(UI → shared ``MailAliasesMachine`` → WS-RPC → nest → re-render). Runs on **linux**
(lead, ``build_alias_row``), **web** (``MailAliasesSection.svelte`` § ``is_canonical``
branch), and **windows** (``MailAliasesPanel.xaml`` —
``BoolToVisibilityInverse(IsCanonical)`` collapses the mutating-controls group);
all omit the four mutating controls for the canonical row off the same shared
``AliasView.is_canonical`` flag. The proof of the read-only render is that **exactly
one row — the canonical — omits every mutating control** while every ordinary row
keeps them: with ``n`` rows, each per-row control marker (which is conditionally
rendered) has exactly ``n - 1`` instances.
"""

import pytest

from conftest import MAIL_PRIMARY_DOMAIN

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.tui,
    # apple joined in its catalog trickle-down pass: the shared FaunaKit
    # `MailAliasesView`'s `if alias.isCanonical` branch renders the "primary
    # address" marker and omits all four mutating controls off the same shared
    # `AliasView.is_canonical` flag — one implementation for both targets.
    pytest.mark.macos,
    pytest.mark.ios,
]

# The four per-row controls `build_alias_row` renders for an ordinary alias and
# omits for the canonical row (mail-aliases.md:249).
_MUTATING_CONTROLS = [
    "mail-aliases-list-item-disabled-toggle",
    "mail-aliases-list-item-edit-button",
    "mail-aliases-list-item-revoke-button",
    "mail-aliases-list-item-overflow-menu",
]


@pytest.mark.feature("mail-aliases")
def test_canonical_alias_row_is_read_only(handled_logged_in_app):
    """Enabling mail for a handled actor mints the canonical ``<handle>@<domain>``
    alias; the page renders that row read-only (no toggle/edit/revoke/delete)
    while an ordinary alias row keeps all four controls."""
    app = handled_logged_in_app
    handle = app.handled_actor["handle"]
    canonical_addr = f"{handle}@{MAIL_PRIMARY_DOMAIN}"

    # Enable mail → the nest writes the canonical <handle>@<domain> exact alias
    # for this handled actor (provision_recipient_mls_pubkey →
    # ensure_canonical_handle_alias, which runs before the credential lands in the
    # snapshot — so ensure_mail_enabled() returning guarantees it's written).
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # The aliases page hydrated at login (before enable), so the freshly-minted
    # canonical isn't in the client snapshot yet. refresh_via_generate() mints a
    # disposable (deriving <handle>+<domain> from the canonical server-side) and
    # re-lists every alias — pulling the canonical into the snapshot AND adding an
    # ORDINARY row, so we can prove the read-only-ness is canonical-specific
    # rather than a global "every row is read-only".
    app.mail_aliases.navigate()
    assert app.mail_aliases.is_page_visible(), (
        f"mail-aliases page should be reachable; error: {app.error_text()!r}"
    )
    app.mail_aliases.refresh_via_generate()
    assert app.mail_aliases.wait_for_pattern(canonical_addr), (
        "the canonical <handle>@<domain> alias should render after mail-enable + "
        f"refresh; rows={app.mail_aliases.patterns()!r}; "
        f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_aliases.disposable_count() >= 1, (
        "refresh_via_generate should add an ordinary (disposable) row alongside "
        f"the canonical; rows={app.mail_aliases.patterns()!r}"
    )

    patterns = app.mail_aliases.patterns()
    n = len(patterns)
    assert canonical_addr in patterns, (
        f"the canonical {canonical_addr!r} should be one of the rows; rows={patterns!r}"
    )
    assert n >= 2, (
        f"expected the canonical row + ≥1 ordinary row so the n-1 control count is "
        f"meaningful; rows={patterns!r}"
    )

    # The canonical row omits ALL FOUR mutating controls; every other (ordinary)
    # row keeps them — so each control's (conditionally-rendered, hence compressed)
    # count is exactly n-1. Combined with `canonical_addr in patterns`, this proves
    # the read-only row is the canonical one. mail-aliases.md:249.
    for control in _MUTATING_CONTROLS:
        got = app.driver.count(control)
        assert got == n - 1, (
            f"exactly one row (the canonical {canonical_addr!r}) must omit {control!r}: "
            f"with {n} rows expected {n - 1} controls, got {got}; rows={patterns!r}"
        )
