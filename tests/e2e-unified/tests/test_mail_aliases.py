"""E2E coverage for the user-facing mail-aliases page.

Target state: docs/goal/behavior/mail-aliases.md § Aliases UX (the page a user
manages their own per-account mail addresses from); UX/IDs:
tests/e2e-unified/ui.yaml `mail-aliases` page + `mail-aliases-list` component.

linux is the **lead** client for the mail-UX seed (tracked internally, § the
12 per-app seed tracks — the linux row); the shared
`fauna_client_mail_settings::MailAliasesMachine` + the linux GTK page are the
prior art the other five apps lift. **web** is the first lift — it binds the
same shared machine through wasm (`MailAliasesSection.svelte`); the remaining
four apps deselect via --client until they implement the page.

tier_3: a real client driver (linux GTK / web Playwright) against a real
fauna-nest (logged_in_app spins one). The aliases backend is fully built
(tracked internally, § A2 — exercised at the WS-RPC layer by
tests/api/test_mail_aliases_user.py), so these drive the page end-to-end (UI →
shared machine → nest → re-render) and assert ground truth over the same user
WS-RPC surface.

Seeding note (mail-aliases.md § Kind 1 + the `default_domain` derivation): the
add-sheet's Create needs a *canonical exact alias* to derive the create domain,
and `generate_disposable_alias` derives <handle>+<domain> from it server-side.
In production that canonical alias is auto-created at mail-enable
(`ensure_canonical_handle_alias`), but only for a *handled* actor — and the
shared `logged_in_app` actor is registered handle-less via the admin API. So the
test seeds one exact alias over the user WS-RPC client first (the same arrange
the disposable API test does), then drives the UI.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import MAIL_PRIMARY_DOMAIN
from helpers.mail_aliases import default_alias_domain

# Implemented on all 7 apps now; the action class is platform-agnostic and
# every app drives the same gestures through the shared MailAliasesMachine.
# android: built with exact ID matches (`MailAliasesScreen.kt`) and
# Compose-content-tested (`MailAliasesContentTest.kt`); the tier_3 run stays
# host-emulator-gated like every other android e2e test.
#
# (The two-way "Active" toggle re-enable round-trip lives in the sibling
# `test_mail_aliases_toggle.py`.)
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios, pytest.mark.tui, pytest.mark.android]

DOMAIN = "aliases-ux-e2e.test"


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _seed_exact(client, pattern):
    """Seed a canonical exact alias so default_domain resolves + disposable mint
    has a <handle>+<domain> to derive from."""
    client.call(
        "fauna.bridges.create_account_alias",
        {"kind": "exact", "local_domain": DOMAIN, "pattern": pattern, "controls": {"label": ""}},
    )


def _list(client):
    return client.call("fauna.bridges.list_account_aliases", {})["aliases"]


def _delete_all(client):
    """Leave the session-scoped actor as found (net-zero on the shared nest)."""
    for row in _list(client):
        try:
            client.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


@pytest.mark.feature("mail-aliases")
def test_mail_aliases_page_reachable(logged_in_app):
    """Navigate to the mail-aliases page and confirm the add-button shows."""
    logged_in_app.mail_aliases.navigate()
    assert logged_in_app.mail_aliases.is_page_visible(), (
        "mail-aliases-add-button should be visible after navigating to the "
        f"mail-aliases page; error: {logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("mail-aliases")
def test_mail_aliases_generate_disposable_renders_row(logged_in_app, test_user, nest_instance):
    """Mint a disposable from the UI; it appears as a `-temp-` row.

    Proves the page renders rows from the shared machine's snapshot, the
    generate-disposable button dispatches GenerateDisposable, and the seeded
    canonical exact alias is picked up server-side (the mint derives
    <handle>+<domain> from it).
    """
    handle = "bob" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        _seed_exact(api, handle)
        try:
            app = logged_in_app
            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible()

            app.mail_aliases.generate_disposable()
            assert app.mail_aliases.wait_for_pattern(f"{handle}@{DOMAIN}"), (
                "the seeded canonical exact alias should render after the mint's "
                f"refresh; rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            assert app.mail_aliases.disposable_count() >= 1, (
                "generate-disposable should add a -temp- row; "
                f"rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            # Ground truth over the same user WS-RPC surface: a disposable row landed.
            assert any(r["kind"] == "disposable" for r in _list(api))
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_mail_aliases_add_exact_via_sheet(logged_in_app, test_user, nest_instance):
    """Add an exact alias through the add-sheet; it appears in the list + nest.

    The add-sheet's Create needs `default_domain`, so a generate-disposable mint
    re-lists into the snapshot first (the page hydrates at login, before this
    test seeds).

    ⚠ The Create lands on the **canonical** row's domain, NOT on the seeded
    alias's `DOMAIN` — `derive_default_domain` reads canonical → first exact →
    first, and the actor's `<handle>@<domain>` canonical always wins
    (mail-aliases.md:166, :444). The seeded alias's job is therefore the
    opposite of what this test used to assume: it is the OTHER domain the
    derivation must decline to pick.
    """
    handle = "carol" + secrets.token_hex(3)
    new_local = "shop" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        _seed_exact(api, handle)
        try:
            app = logged_in_app
            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible()
            # Re-list the seeded alias into the snapshot (sets default_domain).
            app.mail_aliases.refresh_via_generate()
            assert app.mail_aliases.wait_for_pattern(f"{handle}@{DOMAIN}")

            app.mail_aliases.add_exact(new_local, label="Shopping")
            new_addr = f"{new_local}@{default_alias_domain(api)}"
            assert app.mail_aliases.wait_for_pattern(new_addr), (
                f"add-sheet Create should add {new_addr!r}; "
                f"rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            # Ground truth: the nest has the new exact alias with its label.
            row = next(
                (r for r in _list(api) if r["kind"] == "exact" and r["pattern"] == new_local),
                None,
            )
            assert row is not None, "new exact alias should exist on the nest"
            assert row["label"] == "Shopping"
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_mail_aliases_revoke_then_delete(logged_in_app, test_user, nest_instance):
    """Revoke (soft-off, row preserved) then delete (gone) an alias from the UI.

    The UI drives both gestures; ground truth is asserted over the user WS-RPC
    list (disabled=true after revoke, row absent after delete) rather than via
    UI introspection.
    """
    handle = "dave" + secrets.token_hex(3)
    victim = "temp" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        _seed_exact(api, handle)
        try:
            app = logged_in_app
            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible()
            app.mail_aliases.refresh_via_generate()
            assert app.mail_aliases.wait_for_pattern(f"{handle}@{DOMAIN}")

            # Create the victim through the UI, then locate its row. Lands on
            # the canonical row's domain, not the seeded one — see
            # `default_alias_domain`.
            app.mail_aliases.add_exact(victim)
            victim_addr = f"{victim}@{default_alias_domain(api)}"
            assert app.mail_aliases.wait_for_pattern(victim_addr)

            idx = app.mail_aliases.index_of_pattern(victim_addr)
            app.mail_aliases.revoke(idx)
            # Row preserved; nest shows disabled=true.

            def _victim_row():
                return next(
                    (r for r in _list(api) if r["kind"] == "exact" and r["pattern"] == victim),
                    None,
                )

            import time

            deadline = time.monotonic() + 10.0
            while time.monotonic() < deadline:
                r = _victim_row()
                if r is not None and r["disabled"] is True:
                    break
                time.sleep(0.3)
            r = _victim_row()
            assert r is not None and r["disabled"] is True, (
                f"revoke should flip disabled=true while preserving the row; got {r!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            assert app.mail_aliases.has_pattern(victim_addr), "revoke must preserve the row"

            # Delete via the overflow-menu; the row goes away on the nest + UI.
            idx = app.mail_aliases.index_of_pattern(victim_addr)
            app.mail_aliases.delete(idx)
            assert app.mail_aliases.wait_for_pattern_gone(victim_addr), (
                f"delete should remove {victim_addr!r}; "
                f"rows={app.mail_aliases.patterns()!r}"
            )
            deadline = time.monotonic() + 10.0
            while time.monotonic() < deadline:
                if _victim_row() is None:
                    break
                time.sleep(0.3)
            assert _victim_row() is None, "delete should remove the row from the nest"
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_mail_aliases_revoke_and_delete_by_address_with_canonical_row_present(
    handled_logged_in_app,
):
    """Revoke then delete an ordinary alias by address while a canonical row
    precedes it in the list — regression for the row/control index-space bug.

    The canonical `<handle>@<domain>` row renders none of the four mutating
    controls (mail-aliases.md:249), so a control's own occurrence-index
    compresses relative to the row's pattern-index the moment a canonical row
    precedes the target: acting on "row index N" by clicking "control
    occurrence N" then hits the row *after* the intended one. `_seed_exact`
    seeds on a domain that is never the runtime mail domain, so the other
    tests in this file never produce a canonical row and never exercise this.

    A *handled* actor's mail-enable auto-mints the real canonical row
    (`handled_logged_in_app`, mirroring test_mail_aliases_canonical_readonly.py).
    `list_aliases_for_actor` orders `created_at DESC, rowid DESC` (newest
    first) — so the canonical row only precedes rows *older* than it. The
    victim is therefore seeded over WS-RPC **before** mail-enable mints the
    canonical, making the canonical the newer (and hence earlier-listed) row.
    """
    app = handled_logged_in_app
    handle = app.handled_actor["handle"]
    canonical_addr = f"{handle}@{MAIL_PRIMARY_DOMAIN}"
    victim = "shop" + secrets.token_hex(3)
    victim_addr = f"{victim}@{MAIL_PRIMARY_DOMAIN}"

    with WsRpcAdminClient(
        app.handled_nest["url"],
        actor_id=app.handled_actor["actor_id_bytes"],
        signing_key=bytes(app.handled_actor["signing_key"]),
    ) as api:

        def _row(pattern):
            return next(
                (r for r in _list(api) if r["pattern"] == pattern), None
            )

        api.call(
            "fauna.bridges.create_account_alias",
            {
                "kind": "exact",
                "local_domain": MAIL_PRIMARY_DOMAIN,
                "pattern": victim,
                "controls": {"label": ""},
            },
        )

        app.mail_settings.navigate()
        app.mail_settings.ensure_mail_enabled()

        app.mail_aliases.navigate()
        assert app.mail_aliases.is_page_visible()
        # Re-lists into the snapshot, pulling in both the pre-seeded victim
        # and the freshly-minted canonical (mints an extra disposable too,
        # immaterial here).
        app.mail_aliases.refresh_via_generate()
        assert app.mail_aliases.wait_for_pattern(canonical_addr), (
            "mail-enabling a handled actor should mint + render the canonical "
            f"row; rows={app.mail_aliases.patterns()!r}"
        )
        assert app.mail_aliases.wait_for_pattern(victim_addr), (
            f"the pre-seeded {victim_addr!r} should render after refresh; "
            f"rows={app.mail_aliases.patterns()!r}"
        )

        patterns = app.mail_aliases.patterns()
        canon_idx = patterns.index(canonical_addr)
        victim_idx = patterns.index(victim_addr)
        # This is the bug's actual precondition: the canonical row must
        # precede the victim so the compressed control-index drifts.
        assert canon_idx < victim_idx, (
            "test precondition: the canonical row must precede the victim row "
            f"to exercise the index-space bug; rows={patterns!r}"
        )

        app.mail_aliases.revoke(victim_idx)

        import time

        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            r = _row(victim)
            if r is not None and r["disabled"] is True:
                break
            time.sleep(0.3)
        r = _row(victim)
        assert r is not None and r["disabled"] is True, (
            f"revoke(victim's row index) should disable the VICTIM, not the "
            f"row after it; got {r!r}; error: "
            f"{app.mail_aliases.page_error_text(timeout=2.0)!r}"
        )
        canonical_row = _row(handle)
        assert canonical_row is not None and canonical_row["disabled"] is False, (
            "revoke must never touch the canonical row (the nest also rejects "
            f"it server-side); got {canonical_row!r}"
        )

        # Delete via the overflow-menu — same index-space bug, destructive.
        # Re-fetch the index rather than trusting it's unchanged since revoke.
        app.mail_aliases.delete(app.mail_aliases.index_of_pattern(victim_addr))
        assert app.mail_aliases.wait_for_pattern_gone(victim_addr), (
            f"delete should remove {victim_addr!r}; "
            f"rows={app.mail_aliases.patterns()!r}"
        )
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            if _row(victim) is None:
                break
            time.sleep(0.3)
        assert _row(victim) is None, "delete should remove the victim from the nest"
        canonical_row = _row(handle)
        assert canonical_row is not None, (
            "delete must never remove the canonical row; "
            f"canonical vanished: {_list(api)!r}"
        )
