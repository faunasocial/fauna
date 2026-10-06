"""Nostr — Connected apps (the nest as the user's NIP-46 signer / bunker).

The *Connected apps* section of the standalone Nostr page (docs/goal/ui/nostr.md
§ The nest as the user's NIP-46 signer, ratified 2026-07-19; nest side built
2026-07-20). Third-party Nostr apps sign via the user's own box over NIP-46; the
user authorizes each app with a one-time-revealed, revocable connect string —
the mail-credentials interaction shape. The roster rides the User-class,
caller-scoped `fauna.nostr.bunker.{create_invite,list,revoke,set_label}` WS-RPC
kinds (nostr.md § WS-RPC migration contract).

tier_3 (full stack — real `fauna-nest` binary built `--features test-hooks,
nostr`): the bunker control plane (signer-key mint, one-time-secret lifecycle,
per-request authorization, connect-string composition) is nest-side, so a mocked
backend can't catch flow breaks between the shared `NostrBunkerClient` (via the
`nostrBunker*` wasm faces) and the handlers.

web is the lead app for the *Connected apps* fan-out (nostr.md § Layout &
flow item 6). Linux GREEN (2/2, 2026-07-20) — the second client, consuming
the shared `fauna_client_nostr::NostrBunkerClient` directly (native Rust, no
FFI hop). tui GREEN (2026-07-22, direct Rust too). android's client-side
render landed 2026-07-22 (`FfiNostrBunkerClient`, the first UniFFI face for
this control plane) — verified via Robolectric only; the marker is added
per the marker-IS-the-opt-in convention, but a real run stays
host-emulator-gated fleet-wide (standing gap, not this leg's to close).
macOS/iOS landed 2026-07-24 (shared FaunaKit `NostrSettingsView`'s new
`connectedAppsSection`, apple's second UniFFI face for this control plane
after android) — macOS GREEN live; iOS build-verified only (swift-test +
`FaunaiOS` target compile), a live run entrusted elsewhere per the same-day Bluesky leg's precedent.
windows landed 2026-07-30 (`Views/NostrPage.xaml`'s `ConnectedAppsSection`
over the testable `FaunaApp.Core` `NostrViewModel`; the third UniFFI-consuming
client for this plane, routing its roster labels through the shared
`bunker_app_label`/`bunker_last_used_label` faces apple added) — GREEN live,
which completes the fan-out: **all 7 apps now render this section.**

Covers (nostr.md § Layout & flow item 6 / § Element IDs):
  * the section renders only for a linked custodial account
    (`generated`/`imported`) — `nostr-bunker-connect-btn`, absent while unlinked;
  * mint an invite → the one-time `bunker://…` connect string reveals
    (`nostr-bunker-connect-string`, carrying relay + secret) and a pending
    roster row appears (`nostr-bunker-app-item`);
  * revoke the row (`nostr-bunker-app-revoke`) → the roster empties.
"""
import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
]


@pytest.mark.feature("nostr")
def test_bunker_section_renders_for_custodial_account(logged_in_app):
    """The Connected apps section renders only for a linked custodial account:
    absent while unlinked, present after a generate-link."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    # Unlinked → no Connected apps section.
    if app.nostr.is_linked():
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked()
    assert not app.nostr.is_connect_section_visible(), (
        "Connected apps must not render for an unlinked account"
    )
    # Link (generate → a custodial account) → the section renders.
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_connect_section(), (
        f"Connected apps section should render for a custodial account: "
        f"{app.driver.diagnose('nostr-bunker-connect-btn')}"
    )


@pytest.mark.feature("nostr")
def test_bunker_connect_and_disconnect_round_trip(logged_in_app):
    """Mint an invite → the one-time bunker:// connect string reveals and a
    pending roster row appears → revoke it → the roster empties. Exercises
    `fauna.nostr.bunker.{create_invite,list,revoke}` end-to-end."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    assert app.nostr.wait_for_connect_section(), (
        f"Connected apps section should render: "
        f"{app.driver.diagnose('nostr-bunker-connect-btn')}"
    )
    # Known-empty roster start (session nest is shared).
    app.nostr.clear_bunker_apps()
    assert app.nostr.wait_for_bunker_app_count(0), "roster should start empty"

    app.nostr.connect_app()
    assert app.nostr.wait_for_connect_string(), (
        f"minting an invite should reveal the connect string. "
        f"error: {app.nostr.page_error_text()!r}"
    )
    connect = app.nostr.connect_string()
    assert connect.startswith("bunker://"), (
        f"the reveal should be a bunker:// connect string, got {connect!r}"
    )
    assert "relay=" in connect and "secret=" in connect, (
        f"the connect string should carry the relay + one-time secret, got {connect!r}"
    )
    # A pending roster row now exists.
    assert app.nostr.wait_for_bunker_app_count(1), (
        f"minting an invite should create one pending roster row. "
        f"error: {app.nostr.page_error_text()!r}"
    )

    app.nostr.disconnect_app(0)
    assert app.nostr.wait_for_bunker_app_count(0), (
        f"revoking should empty the roster. error: {app.nostr.page_error_text()!r}"
    )
