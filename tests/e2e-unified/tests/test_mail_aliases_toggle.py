"""E2E: the per-row "Active" toggle is two-way — disable then **re-enable**.

Manual testing surfaced that disabling an alias was a one-way trap (no
un-revoke RPC), which on the canonical address bounced all inbound mail with no
client-side recovery. The fix added `fauna.bridges.enable_account_alias` (the
reverse of `revoke`) and a two-way "Active" toggle (mail-aliases.md § Disable:
"the user can re-enable later"). This test proves the round-trip end-to-end
against a real nest (UI toggle → shared MailAliasesMachine → nest → re-render),
asserting ground truth over the user WS-RPC list.

Marked **linux + web + windows + tui + android**: all render the two-way
"Active" toggle (web lifted it from the linux reference; windows landed it on
2026-06-08 — "mail-aliases canonical row read-only + two-way
Active toggle": `MailAliasesPanel.xaml.cs` `DisabledToggle_Toggled` calls
`EnableAsync` ON / `RevokeAsync` OFF, both dispatching through the shared
`MailAliasesMachine`; android's `AliasRow` Switch does the same via
`onEnable`/`onRevoke`, Compose-content-tested in
`MailAliasesContentTest.kt::disabledToggleReEnablesViaOnEnable`). The
shared-machine round-trip is also unit-tested
(`fauna-client-mail-settings::aliases::revoke_then_enable_round_trips_disabled`)
and the nest guard has an entrusted conformance test. android's tier_3
run stays host-emulator-gated like every other android e2e test.

tier_3: a real client driver (linux GTK / web / windows WinUI) against a real
fauna-nest (`logged_in_app`).
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.tui, pytest.mark.android]

DOMAIN = "aliases-toggle-e2e.test"


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _list(client):
    return client.call("fauna.bridges.list_account_aliases", {})["aliases"]


def _seed_exact(client, pattern):
    client.call(
        "fauna.bridges.create_account_alias",
        {"kind": "exact", "local_domain": DOMAIN, "pattern": pattern, "controls": {"label": ""}},
    )


def _delete_all(client):
    for row in _list(client):
        try:
            client.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


def _wait_disabled(api, pattern, want, timeout=10.0):
    """Wait until the exact alias `pattern` has disabled==want on the nest; return
    the row (or None)."""
    deadline = time.monotonic() + timeout
    row = None
    while time.monotonic() < deadline:
        row = next((r for r in _list(api) if r["kind"] == "exact" and r["pattern"] == pattern), None)
        if row is not None and row["disabled"] is want:
            return row
        time.sleep(0.3)
    return row


@pytest.mark.feature("mail-aliases")
def test_active_toggle_disable_then_reenable_round_trips(logged_in_app, test_user, nest_instance):
    """Toggle an alias off (disable) then on (re-enable); the nest reflects both.

    Seeds two exact aliases (the first acts as the canonical so default_domain
    resolves; the second is the victim we toggle — keeping the victim
    non-canonical so the nest's canonical-protection guard doesn't intercept).
    """
    handle = "bob" + secrets.token_hex(3)
    victim = "shop" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        _seed_exact(api, handle)
        _seed_exact(api, victim)
        try:
            app = logged_in_app
            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible()
            app.mail_aliases.refresh_via_generate()
            victim_addr = f"{victim}@{DOMAIN}"
            assert app.mail_aliases.wait_for_pattern(victim_addr), (
                f"seeded victim should render; rows={app.mail_aliases.patterns()!r}"
            )
            # Sanity: starts enabled.
            assert _wait_disabled(api, victim, False) is not None

            # Flip OFF → Revoke → disabled=true on the nest.
            idx = app.mail_aliases.index_of_pattern(victim_addr)
            app.mail_aliases.toggle_active(idx)
            row = _wait_disabled(api, victim, True)
            assert row is not None and row["disabled"] is True, (
                "toggling the Active switch off should disable the alias "
                f"(revoke); got {row!r}; error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )

            # Flip ON → Enable (the new RPC) → disabled=false again.
            idx = app.mail_aliases.index_of_pattern(victim_addr)
            app.mail_aliases.toggle_active(idx)
            row = _wait_disabled(api, victim, False)
            assert row is not None and row["disabled"] is False, (
                "toggling the Active switch back on should RE-ENABLE the alias "
                "(enable_account_alias) — disable is not a one-way trap; "
                f"got {row!r}; error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
        finally:
            _delete_all(api)
