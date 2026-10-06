"""Bluesky/ATProto integration settings — the integration-depth selector and
the F1 login-plane surface it gates (docs/goal/ui/atproto.md).

The page's spine is the four-rung **depth selector** (Off / Linked / Hosted
visible / Hosted full). Selecting a *different* level stages the transition
card (except the one effect-free move, Off→Linked, which applies on select);
confirming it is the single `set_integration_level` call. Each level reveals
its own panel below: the Linked link surface, the hosted identity panel, and —
at `hosted_full` — the F1 login-plane (app credentials + connected apps + the
external-apps kill-switch).

tier_3 (full stack — real `fauna-nest` binary): the transition matrix, the
nest-computed real-domain gate, and the machine's nest-first/local-second
custody split (D3 — the app-credential secret never crosses the nest seam) are
all cross-binary behaviors a mocked backend can't catch.

**Three nests.** The effect-free Off↔Linked ladder is domain-independent and
runs on the shared `logged_in_app`. The selector's greying needs a genuinely
non-public handle domain — which the shared nest does NOT have: the autouse
`_session_primary_mail_domain` fixture pins `fauna.test` on `nest_instance` as
its primary mail domain, so `handle_domain()` resolves to that public name and
the hosted rungs are ENABLED. Greying is therefore tested on the dedicated
domainless `atproto_localhost_logged_in_app` (handle domain stays `localhost` →
hosted rungs gated). The hosted ladder + the full-PDS panel need a **public**
handle domain so the hosted rungs are selectable — `atproto_hosted_logged_in_app`
(`claim_domain=fauna.test`); reaching a hosted level over the wire needs no
real PLC (the DID is minted asynchronously by the bridge, absent here, so the
identity reads `pending`).

**Per-app.** The depth selector is built on linux + web + apple (macOS/iOS)
+ tui + windows (it landed per-app — ui/atproto.md § Migration) and android
(2026-07-30, Robolectric-verified); the F1 tests below drive to `hosted_full`
through the selector (`ensure_hosted_full` is a no-op on a client without
one).

**windows serves every case here, including `test_mint_reveal_and_revoke_round_trip`.** `atproto-app-credential-reveal`'s own
text now carries the minted/revealed secret and the button disables once revealed —
matching the F1 get_text contract (there is no separate secret element) that
linux/web/tui/macos/ios already implement. The previous interim behavior (a
page-error-banner announcement, button label static) is gone.

**The D10 delegation trio also runs on macos + ios (2026-07-31), linux and web
(both 2026-08-15).** It was tui-only because the shared gestures were
deliberately not exported while tui — which consumes the machine as direct Rust
— was the only shell rendering the row; apple became the first shell that needed
them, so `AtprotoSettingsMachine::{authorize,deauthorize}_external_apps` are
UniFFI-exported. linux consumes the machine as direct Rust like tui, so its leg
needed no export at all — only the row and the `atproto_delegation_advance_clock`
agent command the lapse case drives. web's leg landed the **wasm** half of that
same export in the commit that built its renderer (the withholding rule is "the
export lands with the first shell that needs it", not "never"), plus web's own
shape of the clock command: a `test-helpers`-gated
`atprotoDelegationSetClockOffsetForTest` and a page-installed rehydrate hook,
because a browser has no agent process to nudge. **windows joined 2026-08-24**,
the 6th and last trickle-down leg: it inherited apple's UniFFI gestures and
consumes the shared wire->user-voice maps through the two `value-format`-gated
exports (`delegation_capability_label` / `delegation_liveness_label`) rather than
a C# `switch`; its clock command is `TestAgent`'s `atproto_delegation_advance_clock`
arm, which pokes the same `AtprotoSettingsMachineHost` instance the page observes
through a page-installed rehydrate hook. android's leg reuses the same
`atproto_delegation_advance_clock` `TestAgent` arm as windows; the tests above
now carry its `android` marker  — the coverage-contract
mark and an actual device run are two different questions (the ruling):
android's e2e still needs the emulator-host-gated device run every android track owes,
but the UI it would exercise is already built and unit-proven, so the parity gap
drains now and the run's pass/fail cell fills later.
"""
import pytest

from i18n.strings import S

pytestmark = [pytest.mark.tier_3]


# ── The page renders (all F1 clients) ───────────────────────────────────────
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_atproto_settings_page_renders(logged_in_app):
    """The page is reachable and renders its landmark."""
    app = logged_in_app
    app.atproto_settings.navigate()
    assert app.atproto_settings.is_page_visible(), (
        f"atproto page not reachable. error: {app.error_text()!r}. "
        f"diagnose: {app.driver.diagnose('atproto-page')}."
    )


# ── Depth selector: renders at Off; hosted rungs greyed on a localhost nest ──
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_depth_selector_renders_at_off(atproto_localhost_logged_in_app):
    """Default level is Off; Off/Linked are selectable; the two hosted rungs are
    greyed WITH a reason on the domainless (localhost) nest — never hidden
    (§ Reveal/greying rules). The full-PDS panel does not render at Off.

    Uses the dedicated `atproto_localhost_logged_in_app`, NOT the shared
    `logged_in_app`: the latter's nest has a pinned public mail domain
    (`fauna.test`, via the autouse `_session_primary_mail_domain` fixture), so
    its hosted rungs would be enabled — see the module docstring."""
    app = atproto_localhost_logged_in_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    assert bs.has_depth_selector(), "linux renders the depth selector"
    assert bs.depth_level() == "off", (
        f"a fresh user defaults to Off, got {bs.depth_level()!r}. "
        f"error: {bs.page_error_text()!r}"
    )
    # Off + Linked are never gated.
    assert bs.is_depth_enabled("off"), "Off must be selectable"
    assert bs.is_depth_enabled("linked"), "Linked must be selectable"
    # Hosted rungs greyed-with-reason (never hidden) on a non-public domain.
    assert not bs.is_depth_enabled("hosted_visible"), (
        "hosted visible must be greyed on a localhost nest"
    )
    assert not bs.is_depth_enabled("hosted_full"), (
        "hosted full must be greyed on a localhost nest"
    )
    assert bs.depth_gate_marker("hosted_visible") == "gated", (
        "the greyed hosted rung must carry a gate reason"
    )
    # The full-PDS panel (mint button) does not render at Off.
    assert not bs.is_fullpds_visible(), (
        "the full-PDS panel renders only at level = hosted_full"
    )


# ── Off↔Linked: the effect-free ladder applies without a card ───────────────
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_off_linked_cardless_ladder(logged_in_app):
    """Off→Linked is the one effect-free move — it applies on select, no card.
    Linked→Off with no external account linked is likewise effect-free."""
    app = logged_in_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"

    bs.select_depth("linked")
    assert bs.wait_for_depth_level("linked"), (
        f"Off→Linked must apply immediately. level={bs.depth_level()!r} "
        f"error={bs.page_error_text()!r}"
    )
    assert not bs.is_card_visible(), "Off→Linked is effect-free — no transition card"

    bs.select_depth("off")
    assert bs.wait_for_depth_level("off"), (
        f"Linked→Off (no external link) must apply immediately. "
        f"level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )


# ── The Linked panel: the shared bridge surface, revealed by level ──────────
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_linked_panel_reuses_the_shared_bridge_surface(logged_in_app):
    """At level = Linked the page reveals the consume-side link surface — the
    shared `bridge-link-form`/`bridge-card` components embedded verbatim from
    the Bridges page, contributing zero new element IDs (ui/atproto.md § Layout
    & flow, § Element IDs). It is a *panel of the level*: absent at Off,
    present at Linked, gone again on stepping back down.

    This panel is what makes the Bridges page's Bluesky-card migration safe
    (§ Migration step 2): the card and the panel land together so linked
    management is never homeless. That the Bridges page itself stops listing
    Bluesky is not asserted here — the e2e nest is built without the `bluesky`
    provider feature, so `fauna.bridges.list` carries no Bluesky row to drop
    and the assertion would pass vacuously. The exclusion rule is pinned where
    it is real: `fauna_client_bridges::is_unified_bridges_page_bridge`'s own
    unit test."""
    app = logged_in_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    _reset_to_off(bs)

    assert not bs.is_linked_panel_visible(), (
        "the Linked panel is a panel of level=linked — it must not render at Off. "
        f"level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )

    bs.select_depth("linked")
    assert bs.wait_for_depth_level("linked"), (
        f"Off→Linked must apply immediately. level={bs.depth_level()!r} "
        f"error={bs.page_error_text()!r}"
    )
    assert bs.wait_for_linked_panel(True), (
        "level=linked must reveal the shared bridge link surface "
        f"(bridge-action-button). error={bs.page_error_text()!r} "
        f"diagnose: {app.driver.diagnose('bridge-action-button')}."
    )

    bs.select_depth("off")
    assert bs.wait_for_depth_level("off"), (
        f"Linked→Off (no external link) must apply immediately. "
        f"level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert bs.wait_for_linked_panel(False), (
        "stepping back to Off must hide the Linked panel again"
    )


# ── The hosted ladder through the transition card (public-domain nest) ──────
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_hosted_ladder_through_the_card(atproto_hosted_logged_in_app):
    """On a public-domain nest the hosted rungs are enabled. Drive the full
    ladder through the transition card: each hosted move opens a card whose copy
    is the machine's composed lines, confirm applies it, the hosted panel + the
    full-PDS panel reveal by level, and stepping back down is reversible (the
    identity is retained)."""
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    _reset_to_off(bs)

    # Hosted rungs are ENABLED on a public domain (once the first status refresh
    # reports hosted_allowed — the snapshot defaults to greyed until then).
    assert bs.wait_for_depth_enabled("hosted_visible"), (
        f"hosted visible must be selectable on a public domain. "
        f"gate={bs.depth_gate_marker('hosted_visible')!r} error={bs.page_error_text()!r}"
    )
    assert bs.is_depth_enabled("hosted_full"), "hosted full must be selectable on a public domain"
    assert bs.depth_gate_marker("hosted_visible") == "ok", "no gate reason on a public domain"

    # Off → Hosted visible: a card opens with the machine's composed lines; confirm.
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), (
        f"entering a hosted level must stage the transition card. error={bs.page_error_text()!r}"
    )
    assert bs.card_text().strip(), "the card must render the machine's composed effect lines"
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"confirm must apply the transition. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    # Post-mint: the identity summary renders (pending — no bridge here); the
    # full-PDS panel does NOT (that is hosted_full); delete-presence appears.
    assert bs.is_hosted_handle_visible(), (
        f"the hosted identity summary must render after minting. handle={bs.hosted_handle_text()!r}"
    )
    assert not bs.is_fullpds_visible(), "the full-PDS panel renders only at hosted_full"
    assert bs.is_delete_presence_visible(), (
        "delete-presence renders once a hosted identity exists"
    )

    # Hosted visible → full PDS: confirm the open-plane card; the full-PDS panel reveals.
    bs.select_depth("hosted_full")
    assert bs.wait_for_card(), f"stepping up to full PDS must stage a card. error={bs.page_error_text()!r}"
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_full"), (
        f"confirm must reach hosted_full. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert bs.wait_for_fullpds_visible(), "the full-PDS panel must render at hosted_full"

    # Full PDS → Hosted visible (down): the card renders the suspend-plane line
    # VERBATIM from the machine; confirm; the full-PDS panel hides.
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), "stepping down from full PDS must stage a card"
    assert S.atproto_settings.card_suspend_plane in bs.card_text(), (
        "the card must render the machine's pending_transition.lines verbatim "
        f"(suspend-plane copy on leaving full PDS). card={bs.card_text()!r}"
    )
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"confirm must step down. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert not bs.is_fullpds_visible(), "the full-PDS panel must hide below hosted_full"

    # Hosted visible → Off (down): deactivation. The identity is RETAINED — the
    # delete-presence action stays reachable so re-enabling is discoverable.
    bs.select_depth("off")
    assert bs.wait_for_card(), "stepping off a hosted level must stage a card"
    bs.confirm_transition()
    assert bs.wait_for_depth_level("off"), (
        f"confirm must step off. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert bs.is_delete_presence_visible(), (
        "a deactivated identity stays visible (delete-presence retained after stepping off)"
    )


# ── F1 login-plane: mint / reveal / revoke + the kill-switch (all F1 clients) ─
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_mint_reveal_and_revoke_round_trip(atproto_hosted_logged_in_app):
    """Mint → the row appears with the secret revealed inline (rule 1) → revoke
    → the row is gone. Exercises `fauna.bridges.atproto.{provision,list,revoke}_
    app_credential` end-to-end through the shared machine's nest-first custody
    split (D3). On the selector clients this first drives to hosted_full (where
    the full-PDS panel lives); elsewhere the panel is already present."""
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()
    assert bs.wait_for_fullpds_visible(), (
        f"the full-PDS panel must be present to mint. error: {bs.page_error_text()!r}"
    )
    before = bs.credential_count()

    bs.mint_credential()
    assert bs.wait_for_credential_count(before + 1), (
        f"minting should add one credential row. error: {bs.page_error_text()!r}"
    )

    # The newly minted row is the last one (rows render in nest-list order).
    new_index = before
    secret = bs.reveal_credential_secret(new_index)
    assert secret and secret != "Reveal", (
        f"mint must show the returned secret inline (rule 1), got {secret!r}. "
        f"error: {bs.page_error_text()!r}"
    )

    bs.revoke_credential(new_index)
    assert bs.wait_for_credential_count(before), (
        f"revoking should remove the credential row. error: {bs.page_error_text()!r}"
    )


@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_kill_switch_is_non_destructive(atproto_hosted_logged_in_app):
    """Flipping the external-apps kill-switch off and back on must never remove
    credential rows (rule 3: OFF suspends the plane, keeps every row listed and
    individually revocable)."""
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()
    assert bs.wait_for_fullpds_visible(), (
        f"the full-PDS panel (kill-switch) must be present. error: {bs.page_error_text()!r}"
    )

    # Ensure at least one row exists so "unchanged" is a meaningful assertion.
    before = bs.credential_count()
    if before == 0:
        bs.mint_credential()
        assert bs.wait_for_credential_count(1)
        before = 1

    # Start from ON (the nest column default; restore if a prior test left it off).
    bs.set_external_apps_enabled(True)
    assert bs.wait_for_external_apps_state("on"), (
        f"the kill-switch should default/return on. error: {bs.page_error_text()!r}"
    )

    bs.set_external_apps_enabled(False)
    assert bs.wait_for_external_apps_state("off"), (
        f"the kill-switch should flip off. error: {bs.page_error_text()!r}"
    )
    assert bs.credential_count() == before, (
        "the kill-switch must not remove credential rows while off"
    )

    bs.set_external_apps_enabled(True)
    assert bs.wait_for_external_apps_state("on"), (
        f"the kill-switch should flip back on. error: {bs.page_error_text()!r}"
    )
    assert bs.credential_count() == before, (
        "toggling the kill-switch back on must not change credential rows"
    )


# ── D10: the authoring-delegation row (tui is the lead app) ─────────────────
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_authorize_and_revoke_external_posting(atproto_hosted_logged_in_app):
    """The D10 round trip: an account at hosted_full starts with external apps
    able to sign in but NOT to post; authorizing renders a live delegation row;
    revoking removes it and returns the page to the empty state.

    This is the journey that makes the F2.2 write path reachable at all — until
    it lands, `ingest_external_write` refuses every external post with the D6
    *fauna-surface* sub-type naming this very control, and no app renders it.

    Drives the app UI only (convention 8): the mutations are the two buttons a
    user clicks, never a raw `provision_authoring_delegation` call. Asserts
    latency-independent state (convention 14) — the liveness *state* off the
    `state` attr, never elapsed time.
    """
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()
    assert bs.wait_for_fullpds_visible(), (
        f"the full-PDS panel must be present. error: {bs.page_error_text()!r}"
    )

    # Start from the empty state so "authorize created it" is meaningful. A
    # prior test in this module-scoped nest may have left one live.
    if bs.is_delegation_row_visible():
        bs.revoke_delegation()
        assert bs.wait_for_delegation(present=False), (
            f"could not reset to the un-authorized state. error: {bs.page_error_text()!r}"
        )
    assert not bs.is_delegation_row_visible(), (
        "an account that has never authorized must render no delegation row — "
        "a bare sub-key is NOT an authorization"
    )

    bs.authorize_delegation()
    assert bs.wait_for_delegation(present=True), (
        f"authorizing must render the delegation row. error: {bs.page_error_text()!r}. "
        f"diagnose: {app.driver.diagnose('atproto-delegation-row')}."
    )
    assert bs.wait_for_delegation_status("active"), (
        f"a freshly minted delegation is active, not {bs.delegation_status()!r}. "
        f"error: {bs.page_error_text()!r}"
    )
    # The row states what the user actually signed — capabilities and window
    # both come from the cert, re-verified client-side under the account's own
    # identity key before anything renders.
    assert bs.delegation_scope_text().strip(), "the row must name what was granted"
    assert bs.delegation_lasts_until_text().strip(), (
        "the row must state when it was authorized and when it lapses — the "
        "time-bound is the whole point of a capability grant"
    )

    bs.revoke_delegation()
    assert bs.wait_for_delegation(present=False), (
        f"revoking must remove the delegation row. error: {bs.page_error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_reauthorizing_needs_no_revoke_first(atproto_hosted_logged_in_app):
    """Re-authorizing an already-live delegation is the RENEWAL gesture: one
    control, no revoke first. Provisioning overwrites the stored cert, so a
    lapsed grant recovers in one click — the `nests.md` § Expiry / renewal bar
    that a lapse must read as "re-authorize here", never as silent feature loss.

    A revoke-then-re-mint flow would destroy the signing sub-key K and churn it
    for what is only an expiry refresh, so the absence of a required revoke is
    the actual product guarantee under test.
    """
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()

    if not bs.is_delegation_row_visible():
        bs.authorize_delegation()
        assert bs.wait_for_delegation(present=True), (
            f"could not establish a delegation to renew. error: {bs.page_error_text()!r}"
        )
    assert bs.wait_for_delegation_status("active")

    # The renewal: authorize again, with NO revoke in between. The control must
    # still BE there on a live row — that it doubles as renew is the whole
    # ruling, and a page that hid it once authorized would force the
    # revoke-then-re-mint flow this test exists to forbid.
    bs.authorize_delegation()
    assert bs.wait_for_delegation_status("active"), (
        f"re-authorizing must leave a live delegation, not {bs.delegation_status()!r}. "
        f"error: {bs.page_error_text()!r}"
    )
    assert bs.is_delegation_row_visible(), (
        "re-authorizing must never leave the account un-authorized — it is a "
        "renewal, not a revoke-and-re-mint"
    )
    # The sharp end: a nest that REFUSED to overwrite a live cert (the natural
    # wrong implementation — "already authorized") would surface here. Read the
    # error immediately rather than polling for a quiet window — the assertion
    # above already provided the causal barrier (same snapshot fold), and
    # waiting N seconds for an error NOT to appear is the settle-sleep that
    # turns a late real failure into a false green (convention 14).
    assert not bs.current_error_text(), (
        "re-authorizing a LIVE delegation must succeed, not be refused as "
        f"already-authorized. error: {bs.current_error_text()!r}"
    )

    bs.revoke_delegation()
    assert bs.wait_for_delegation(present=False)


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_lapse_reads_as_reauthorize_here(atproto_hosted_logged_in_app):
    """The lapse journey `nests.md` § Expiry / renewal requires: a delegation
    within ~14 days of its ~90-day window reads `expiring_soon`, one past it
    reads `expired`, and in BOTH states the row stays visible with the same
    `-authorize` control as the remedy — never silent feature loss. Recovery
    is the ordinary renewal gesture: re-authorize, no revoke first.

    Drives the app UI only (convention 8) for the mint/revoke mutations; the
    lapse itself is reached by advancing the row's RENDER clock
    (`advance_delegation_clock`, never the mint clock — convention 14's fake
    clock, never a sleep for the ~90-day window). Asserts the `state` attr,
    never prose (convention 14).
    """
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.ensure_hosted_full()

    if not bs.is_delegation_row_visible():
        bs.authorize_delegation()
        assert bs.wait_for_delegation(present=True), (
            f"could not establish a delegation to lapse. error: {bs.page_error_text()!r}"
        )
    assert bs.wait_for_delegation_status("active")

    try:
        # ~80 days: past the 76-day (90-14) expiring-soon boundary, short of
        # the 90-day expiry itself.
        bs.advance_delegation_clock(80 * 24 * 60 * 60)
        assert bs.wait_for_delegation_status("expiring_soon"), (
            f"a delegation 80 days into its 90-day window must read "
            f"expiring_soon, not {bs.delegation_status()!r}. "
            f"error: {bs.page_error_text()!r}"
        )
        assert bs.is_delegation_row_visible(), (
            "expiring_soon must not hide the row — the warning IS the point"
        )

        # Past the full 90-day window.
        bs.advance_delegation_clock(95 * 24 * 60 * 60)
        assert bs.wait_for_delegation_status("expired"), (
            f"a delegation 95 days into its 90-day window must read expired, "
            f"not {bs.delegation_status()!r}. error: {bs.page_error_text()!r}"
        )
        assert bs.is_delegation_row_visible(), (
            "a lapsed delegation stays visible so there is something to act "
            "on — silent feature loss is exactly what this journey forbids"
        )
        # The remedy: the SAME authorize control renews, no revoke needed.
    finally:
        # Reset before re-authorizing: `authorize_delegation` mints with the
        # real wall clock regardless, but a stale advanced offset would
        # re-lapse the fresh cert the instant the row next refreshes.
        bs.advance_delegation_clock(0)

    bs.authorize_delegation()
    assert bs.wait_for_delegation_status("active"), (
        f"re-authorizing a lapsed delegation must recover it to active, not "
        f"{bs.delegation_status()!r}. error: {bs.page_error_text()!r}"
    )
    assert bs.is_delegation_row_visible()

    bs.revoke_delegation()
    assert bs.wait_for_delegation(present=False)


# ── "Delete my Bluesky presence": the confirm ceremony (ui/atproto.md § User actions row 4) ──
#
# Placed LAST in the module deliberately. It is the one destructive gesture on
# this page and it runs against the module-scoped `atproto_hosted_nest`: the
# sweep records a tombstone on that nest's identity, which a sibling arriving
# afterwards would find in a state its own setup never produced. Ordering is the
# honest fix here rather than a fresh nest per test — a whole extra nest for one
# assertion is the wrong trade, and the dependency is real, not incidental.
#
# tui led; linux/web/android/macos/ios landed the trickle-down leg; windows is
# the 7th and last app.
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("atproto")
def test_delete_presence_ceremony(atproto_hosted_logged_in_app):
    """The destructive flow, end to end through the app UI: the button opens its
    OWN confirm card (never the depth selector's), cancelling deletes nothing,
    and confirming runs the sweep — after which the level has converged to `off`,
    the identity summary says the presence is gone, and the button withdraws
    because its only remaining outcome would be a no-op.

    The card's copy is asserted against the shared machine's own i18n strings, so
    this also pins the two promises `atproto-pds-bridge.md` § Disable & revocation
    layer 2 makes the ceremony responsible for: that the identity SURVIVES (the
    sweep is reversible in identity terms — a card reading "delete my account"
    would collect consent for something stronger than what runs), and the honest
    caveat that already-published copies cannot be recalled."""
    app = atproto_hosted_logged_in_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    _reset_to_off(bs)

    # Reach a hosted level so there is a presence to destroy.
    assert bs.wait_for_depth_enabled("hosted_visible"), (
        f"hosted visible must be selectable on a public domain. "
        f"gate={bs.depth_gate_marker('hosted_visible')!r} error={bs.page_error_text()!r}"
    )
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), f"entering a hosted level stages the card. error={bs.page_error_text()!r}"
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"could not reach a hosted level. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert bs.is_delete_presence_visible(), "a standing presence offers the delete action"

    # The button opens the ceremony's OWN card and destroys nothing.
    assert not bs.is_delete_card_visible(), "the ceremony is closed until the user opens it"
    bs.open_delete_confirm()
    assert bs.wait_for_delete_card(), (
        f"atproto-delete-presence must open the confirm ceremony. error={bs.page_error_text()!r} "
        f"diagnose: {app.driver.diagnose('atproto-delete-confirm-card')}."
    )
    assert not bs.is_card_visible(), (
        "the delete ceremony has its OWN card — the depth selector's transition "
        "card describes a level change and must never be what confirms a sweep"
    )
    card = bs.delete_card_text()
    assert S.atproto_settings.card_no_recall in card, (
        "the card must carry § Disable & revocation's honest caveat verbatim from "
        f"the machine's composed lines. card={card!r}"
    )
    # The identity-survives line is parameterized on the handle, so match the
    # copy AFTER the placeholder rather than re-deriving the handle this nest
    # happens to mint — that tail is where the promise actually lives.
    _SENTINEL = "HANDLE-SENTINEL"
    kept_tail = S.atproto_settings.delete_confirm_identity_kept(
        handle=_SENTINEL
    ).split(_SENTINEL, 1)[1].lstrip()
    assert kept_tail and kept_tail in card, (
        "the card must say the identity survives — without it the ceremony reads "
        f"as 'delete my account'. card={card!r}"
    )

    # Cancelling deletes nothing: the card closes, the level and the presence stand.
    bs.cancel_delete()
    assert bs.wait_for_no_delete_card(), f"cancel must close the card. error={bs.page_error_text()!r}"
    assert bs.depth_level() == "hosted_visible", (
        f"cancel must not change the level. level={bs.depth_level()!r}"
    )
    assert bs.is_delete_presence_visible(), "cancel must leave the presence standing"

    # Confirming runs the sweep.
    bs.open_delete_confirm()
    assert bs.wait_for_delete_card(), "the ceremony re-opens after a cancel"
    bs.confirm_delete()
    assert bs.wait_for_depth_level("off", timeout=20.0), (
        f"confirming the sweep must land the level on off — a selector parked on a "
        f"hosted rung would offer to restore a presence that no longer exists. "
        f"level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    assert bs.wait_for_no_delete_card(), "the ceremony is over"
    assert bs.wait_for_no_delete_presence(timeout=20.0), (
        "the delete action withdraws once the presence is destroyed — its only "
        "remaining outcome is a no-op"
    )
    # The identity SURVIVES the sweep and says so, at level off: this is the
    # § Errors & edge cases rule that the summary is not gated on the level.
    assert bs.is_hosted_handle_visible(), (
        "the identity summary must render at level off so the user can see what "
        f"became of it. diagnose: {app.driver.diagnose('atproto-hosted-handle')}."
    )
    assert S.atproto_settings.identity_status_deleted in bs.hosted_handle_text(), (
        f"the summary must report the deleted state, not the raw wire word. "
        f"summary={bs.hosted_handle_text()!r}"
    )
    assert bs.page_error_text() in ("", None), (
        f"a completed sweep is not an error. error={bs.page_error_text()!r}"
    )


def _reset_to_off(bs) -> None:
    """Drive the selector down to Off (confirming any card) so a module-scoped
    nest's prior state can't make the ladder test order-dependent."""
    if bs.depth_level() == "off":
        return
    bs.select_depth("off")
    if bs.wait_for_card(timeout=4.0):
        bs.confirm_transition()
    assert bs.wait_for_depth_level("off", timeout=15.0), (
        f"could not reset to Off. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
