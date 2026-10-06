"""Nostr — the standalone Nostr settings page (docs/goal/ui/nostr.md).

A dedicated user-settings sub-page — the same treatment as mail
(`mail-settings.md`), NOT folded into the unified Bridges page (page structure
ratified 2026-06-13, nostr.md § Page structure). Renders the shared FaunaKit
`NostrSettingsView` over `NostrVM`; the control plane rides the unified
`fauna.bridges.*` WS-RPC keyed `bridge_id:"nostr"` (nostr.md § WS-RPC migration
contract). web/linux lead; macOS + iOS lifted the page 2026-06-14 (shared
FaunaKit `NostrSettingsView` — account linking + 5 content toggles + relay
management + follows). Same IDs + flows on all seven apps (priority #1).

tier_3 (full stack — real `fauna-nest` binary): the Nostr control plane
(`fauna.bridges.{link,unlink,set_settings,list,add_follow,remove_follow}` keyed
`bridge_id:"nostr"`) + the `NostrProvider::available` gate are nest-side, so a
mocked backend can't catch flow breaks between the shared `NostrVM` and the
handlers.

RESOLVED (was ⚠ KNOWN GAP): `NostrProvider::available` (`nostr_bridging_available`,
`bins/fauna-nest/src/nostr/mod.rs`) now gates on the RATIFIED target —
`any_nsec_deposited()`, not the legacy `nest_mode` row — so it is correctly
`false` on a fresh e2e nest (zero deposits) and stays that way until the first
nsec is ever deposited (S8.9). That is expected, by design — but every apple
app used to conflate this `available` bootstrap gate with "the bridge is
registered at all," hiding the link-account form (which is what bootstraps the
first deposit) behind it, so a fresh nest's account-link form never rendered
on macOS/iOS (client bug, fixed 2026-07-14: `NostrStatus.registered` — whether
the bridge appears in `fauna.bridges.list` at all — now gates the "unavailable"
notice; `available` gates nothing else in `NostrSettingsView`). web had the
same bug in `NostrSettingsSection.svelte`, fixed the same way, same day.

Covers the built v1 surface (nostr.md § Layout & flow / § Element IDs):
  * the page renders + the account-link form (`nostr-link-mode`/`-link-button`);
  * link (generate) → `nostr-pubkey-copy-btn` + the 5 toggles render → unlink;
  * the 5 content toggles round-trip via `set_settings`;
  * relay add/remove round-trip (incl. the `wss://` client-side validation);
  * follow add/remove round-trip.
DMs are not on this page — a Nostr DM is a bridged room on unified Conversations,
tested by `test_bridged_conversation.py` (nostr.md § Layout & flow note 5).

Authored greenfield 2026-06-14 against the built `NostrSettingsView` + ui.yaml.

e2e enablement (UNBLOCKED 2026-06-19, historical — `available`'s gate has since
migrated to the ratified nsec-deposit target, see the RESOLVED note above): the
e2e nest is now built `--features test-hooks,nostr` (`tests/common/nest.py`
`build_node`), so `NostrProvider` (`#[cfg(feature = "nostr")]`,
`bins/fauna-nest/src/lib.rs`) registers → `fauna.bridges.list` surfaces the
nostr bridge (this is what `registered` reads today) → back in 2026-06-19
`status.available` was true on the plaintext `logged_in_app` nest under the
then-current gate → these flows ran. (Before that the binary was
`--features test-hooks` only, so the bridge never registered and the page
rendered only the Phase-1 "unavailable" notice — that is why the green run was
"OWED": it was never runnable, not a per-app UI gap.) Enabling it also surfaced
two latent nostr-feature compile rots (the feature isn't in default/CI builds): a
grown `extra` wire field unswept at the `#[cfg(feature="nostr")]` construction
sites, and two axum `:param`→`{param}` route literals. nostr.md § Implementation
status today.

Client coverage: web GREEN (6/6, 2026-06-19) — migrated to `fauna.bridges.*`,
renders the canonical-id page on BOTH the `/app/nostr` route and the settings rail
(shared `NostrSettingsSection`). macOS GREEN (6/6, re-verified 2026-07-14 with
the `registered`/`available` fix above). iOS GREEN (6/6, re-verified
2026-07-18): the two follow tests
(`test_follow_add_remove_round_trip`, `test_relay_invalid_url_rejected`) used
to time out on `nostr-follow-pubkey-input` — the iOS `Form`-lazy-`List`
off-screen-registration gap documented fleet-wide for sibling `Form`-based
settings pages (`docs/goal/architecture/apps/apple-e2e-automation.md` §
limitation (b)); FIXED by apple's rule-6 eager-`ScrollView` conversion of
`NostrSettingsView`, unrelated to the `registered`/`available`
fix. Linux's page closed 2026-07-19 (nostr.md § Implementation status
today) — link-mode/nsec/link/unlink IDs, relay management, follows, and
the 2 missing publish-* toggles all landed. Linux GREEN (6/6, confirmed
2026-07-20 at low load — a 2026-07-19 run at 4/6 had found a REAL bug:
`settings/nostr_tab.rs` never actually gated the linked-vs-unlinked UI
(every row/group rendered unconditionally), so `nostr-pubkey-copy-btn`
(the e2e `is_linked()` signal) was always visible regardless of real
link state. That made `test_link_generate_and_unlink_round_trip` fail
(unlink never read as unlinked) and cascaded into
`test_follow_add_remove_round_trip` (`ensure_linked()` saw the stale
"already linked" signal and skipped the real link call, so `add_follow`
ran against an unlinked account and silently no-opped). Fixed via a new
`NostrLinkGate` (mirrors apple `NostrSettingsView`'s `if status.linked`
split); `cargo test -p fauna-linux --bins` 159/159, clippy clean on the
file. The first re-run attempt was blocked by an unrelated main-gate red
— a sibling's new `PushEvent::BridgeAtprotoSessionsChanged` wire variant
left `apps/fauna-linux/src/app.rs`'s dispatch non-exhaustive; fixed
forward by another session before this confirming run).
`pytest.mark.linux` added. android is feature-complete too (2026-07-16)
but was never added to `pytestmark` — still open, e2e blocked on the
primary dev VM by the android-emulator host-machine setup gap (not by
android readiness); a session with emulator access should verify
`--client android` and add its marker.
"""
import pytest

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.linux,
    pytest.mark.tui,
    pytest.mark.windows,
]

# A follow target. `NostrProvider::add_follow` (bridge_provider.rs) accepts an
# npub OR a raw hex pubkey and stores the hex path as-is (no bech32 validation),
# so a synthetic 64-hex key drives the add/list/remove round-trip. `list_follows`
# re-encodes it to an npub for display, so tests assert on row COUNT, not text.
FOLLOW_PUBKEY_HEX = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"

# A valid relay URL (the client requires a wss://|ws:// prefix; NostrVM.addRelay).
RELAY_URL = "wss://relay.example.com"
# A URL the client-side prefix check must reject (no row added, error surfaced).
BAD_RELAY_URL = "http://not-a-relay.example.com"
# A well-formed relay on a private-network literal — refused by the shared
# `relay_url_error` with its own message (network-exposure.md § Rulings F7).
PRIVATE_RELAY_URL = "wss://192.168.1.10:7777"


@pytest.mark.feature("nostr")
def test_nostr_page_renders(logged_in_app):
    """The Nostr page is reachable from user settings and, on a plaintext
    (available) nest, an unlinked account shows the account-link form."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page not reachable. error: {app.error_text()!r}"
    )
    # Establish the unlinked state so the assertion is deterministic (the session
    # nest is shared — a prior test may have linked it).
    if app.nostr.is_linked():
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked(), (
            f"unlink should return to the link form. error: {app.nostr.page_error_text()!r}"
        )
    # The unlinked account-link surface: the link-mode select + the link button.
    assert app.driver.is_visible("nostr-link-mode"), (
        f"link-mode select should render: {app.driver.diagnose('nostr-link-mode')}"
    )
    assert app.driver.is_visible("nostr-link-button"), (
        f"link button should render: {app.driver.diagnose('nostr-link-button')}"
    )


@pytest.mark.feature("nostr")
def test_link_generate_and_unlink_round_trip(logged_in_app):
    """Link via a generated keypair → the linked surface (pubkey-copy + the 5
    content toggles) renders → unlink → the link form returns. Exercises
    `fauna.bridges.{link,unlink}` (`bridge_id:"nostr"`) end-to-end."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    # Start unlinked.
    if app.nostr.is_linked():
        app.nostr.unlink()
        assert app.nostr.wait_for_unlinked()

    app.nostr.link_generate()
    assert app.nostr.wait_for_linked(), (
        f"generate-link should link the account. error: {app.nostr.page_error_text()!r}"
    )
    # Linked surface: pubkey copy + every content toggle renders.
    assert app.driver.is_visible("nostr-pubkey-copy-btn"), (
        f"pubkey copy should render once linked: {app.driver.diagnose('nostr-pubkey-copy-btn')}"
    )
    for toggle_id in app.nostr.CONTENT_TOGGLES:
        assert app.driver.is_visible(toggle_id), (
            f"{toggle_id} should render once linked: {app.driver.diagnose(toggle_id)}"
        )

    app.nostr.unlink()
    assert app.nostr.wait_for_unlinked(), (
        f"unlink should return to the link form. error: {app.nostr.page_error_text()!r}"
    )
    assert not app.nostr.is_linked(), "account should be unlinked"


@pytest.mark.feature("nostr")
def test_content_toggles_round_trip(logged_in_app):
    """Each content toggle flips and the new value persists across the
    `fauna.bridges.set_settings` → refresh round-trip (the switch reflects the
    persisted status, not an optimistic tap). Restores each to its prior value
    so the shared session nest is left as found."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )

    for toggle_id in app.nostr.CONTENT_TOGGLES:
        before = app.nostr.toggle_state(toggle_id)
        assert before in ("on", "off"), (
            f"{toggle_id} should expose an on/off state, got {before!r}"
        )
        want = "off" if before == "on" else "on"
        app.nostr.set_toggle(toggle_id, want == "on")
        assert app.nostr.wait_for_toggle_state(toggle_id, want), (
            f"{toggle_id} should round-trip to {want!r}. "
            f"error: {app.nostr.page_error_text(timeout=2.0)!r}"
        )
        # Restore.
        app.nostr.set_toggle(toggle_id, before == "on")
        assert app.nostr.wait_for_toggle_state(toggle_id, before), (
            f"{toggle_id} should restore to {before!r}"
        )


@pytest.mark.feature("nostr")
def test_relay_add_remove_round_trip(logged_in_app):
    """Add a wss:// relay → its row appears → remove it → the row drops.
    The client read-modify-writes the `relay_list` JSON array over
    `fauna.bridges.set_settings` (`bridge_id:"nostr"`); a refresh re-reads it."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    # Known-empty start (session nest is shared).
    app.nostr.clear_relays()
    assert app.nostr.wait_for_relay_count(0), "relay list should start empty"

    app.nostr.add_relay(RELAY_URL)
    assert app.nostr.wait_for_relay_count(1), (
        f"adding a relay should create one row. error: {app.nostr.page_error_text()!r}"
    )
    rendered = " ".join(app.nostr.relay_texts())
    assert "relay.example.com" in rendered, (
        f"the relay row should render the added URL, got {rendered!r}"
    )

    app.nostr.remove_relay(0)
    assert app.nostr.wait_for_relay_count(0), (
        f"removing the relay should empty the list. error: {app.nostr.page_error_text()!r}"
    )


@pytest.mark.feature("nostr")
def test_relay_invalid_url_rejected(logged_in_app):
    """A non-wss/ws relay URL is rejected client-side (NostrVM.addRelay): no row
    is added and an error surfaces — the optional negative assertion."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    app.nostr.clear_relays()
    assert app.nostr.wait_for_relay_count(0)

    app.nostr.add_relay(BAD_RELAY_URL)
    # No row should be added for an invalid URL.
    assert not app.nostr.wait_for_relay_count(1, timeout=4.0), (
        "an invalid (non-wss/ws) relay URL must not add a row"
    )
    assert app.nostr.relay_count() == 0
    assert app.nostr.page_error_text(timeout=6.0), (
        "an invalid relay URL should surface an error message"
    )


@pytest.mark.feature("nostr")
def test_relay_private_address_rejected(logged_in_app):
    """A private-network relay (an IP literal the nest may never dial) is
    refused client-side by the shared predicate — no row is added and the
    private-address message surfaces (`nostr.md` § Errors & edge cases; the
    rule is `nest/network-exposure.md` § Rulings F7). All seven apps render
    the shared `relay_url_error` message."""
    from i18n.strings import S

    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    app.nostr.clear_relays()
    assert app.nostr.wait_for_relay_count(0)

    app.nostr.add_relay(PRIVATE_RELAY_URL)
    assert not app.nostr.wait_for_relay_count(1, timeout=4.0), (
        "a private-network relay URL must not add a row"
    )
    assert app.nostr.relay_count() == 0
    error = app.nostr.page_error_text(timeout=6.0)
    assert error, "a private-network relay URL should surface an error message"
    assert S.nostr.relays.private_address in error, error


@pytest.mark.feature("nostr")
def test_follow_add_remove_round_trip(logged_in_app):
    """Add a follow (pubkey + petname) → its row appears → remove it → the row
    drops. Exercises `fauna.bridges.{add_follow,remove_follow,list_follows}`
    (`bridge_id:"nostr"`)."""
    app = logged_in_app
    app.nostr.navigate()
    assert app.nostr.is_page_visible(), (
        f"nostr page should be reachable from user settings. error: {app.error_text()!r}"
    )
    assert app.nostr.ensure_linked(), (
        f"could not link the account. error: {app.nostr.page_error_text()!r}"
    )
    # Known-empty start (session nest is shared).
    app.nostr.clear_follows()
    assert app.nostr.wait_for_follow_count(0), "follow list should start empty"

    app.nostr.add_follow(FOLLOW_PUBKEY_HEX, petname="alice")
    assert app.nostr.wait_for_follow_count(1), (
        f"adding a follow should create one row. error: {app.nostr.page_error_text()!r}"
    )

    app.nostr.remove_follow(0)
    assert app.nostr.wait_for_follow_count(0), (
        f"removing the follow should empty the list. error: {app.nostr.page_error_text()!r}"
    )
