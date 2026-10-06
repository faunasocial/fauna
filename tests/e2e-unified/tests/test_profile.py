"""E2E for the profile page — the text-only **edit form** (SELF publish/edit) and
the **OTHER-profile offers** subscriber-browse (item 7).

`test_profile_edit_publishes_and_renders_display_name` proves the linux edit form
end-to-end: open the form, edit the display name, Save (the client
`build_profile`-signs and calls `fauna.profile.set`), and the header
`profile-handle` re-renders the published `display_name` (read back via
`fauna.profile.get` → the shared `decode_profile`).

`test_other_profile_offers_render_and_subscribe` proves the subscriber-browse
surface: the viewer taps another creator's contact row → that creator's profile
opens → the Tiers tab's offers section renders the creator's offered tier →
Subscribe flips the row status to "Subscribed".

`test_other_profile_start_dm_opens_compose` + `test_other_profile_block` prove the
OTHER-profile **secondary relationship actions** (profile.md § Layout & flow,
ratified 2026-06-21): start-DM seeds the Conversations new-thread composer with
the viewed actor (pure nav glue), and `profile-block-button` is the Block⇄Unblock
toggle — block via the shared `fauna_client_contacts::knocks_block` over
`fauna.knocks.block`, unblock via `knocks_unblock` over `fauna.knocks.unblock` (the
guarded clear-the-edge, `ContactStatus` → `None`); the label flips on the viewed
actor's `contact_status`. Linux leads the flip; clients still on the block-only
interim assert the terminal "Blocked" label (`_is_block_toggle_client`). The third
secondary action, `profile-request-contact-button`, is
`test_other_profile_request_contact_lands_a_knock` — tui leads it; the other apps
skip as unbuilt until their render legs land.

profile.md § Where logic lives → Profile publish/edit + § Layout & flow → Another's
profile; monetization.md § Pillar 1 (surface 2). tier_3: real nest binary + real
driver + real `fauna.profile.*` / `fauna.subscriptions.*` / `fauna.knocks.*` over
the wire.
"""

from __future__ import annotations

import uuid
from pathlib import Path

import pytest

from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.other_profile import open_new_contact_profile
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


@pytest.mark.feature("profile")
def test_profile_edit_publishes_and_renders_display_name(
    logged_in_app, nest_instance, test_user
):
    profile = logged_in_app.profile
    profile.navigate()

    # Open the edit form (shown async after the read-modify-write profile fetch).
    profile.open_edit_form()

    new_name = "Ada Lovelace"
    profile.set_display_name(new_name)
    profile.set_bio("Mathematician and first programmer")
    profile.save()

    # The header re-renders the published display_name. This round-trips the
    # whole path: build_profile (sign) → fauna.profile.set → fauna.profile.get →
    # decode_profile → the header label (async, so poll).
    assert profile.wait_for_handle(new_name), (
        f"header profile-handle did not render the published display_name; "
        f"got {profile.handle_text()!r}, error={profile.error_text()!r}"
    )

    # Server-side: the publish landed and the row is readable via fauna.profile.get
    # (a non-empty signed body; the typed decode is client-side).
    actor = ApiActor(
        nest_instance["url"],
        test_user["token"],
        test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    reply = actor.profile_get()
    assert reply.get("body"), "fauna.profile.get returned no body after a publish"


@pytest.mark.feature("profile")
def test_profile_edit_sets_and_clears_avatar_and_banner(
    logged_in_app, nest_instance, test_user
):
    """`profile-edit-avatar` / `profile-edit-banner`: picking a local image
    stages it (a typed path on tui, which has no OS file picker — `tui.md`
    § Declared platform absences 4; a real OS `<input type="file">` on web
    and future native legs), Save uploads it through the ordinary public-post
    blob path and records the resulting `ContentHash` on the signed `Profile`
    (`profile.md` § Where logic lives → Field ownership). The non-UI
    build→sign→publish chain is already proven end to end by the shared-Rust
    tier_3 `conformance_profile.rs::avatar_survives_a_text_only_edit_and_can_be_cleared`
    (drives the real client builders + the real nest handlers with a
    pre-made hash); this test is the missing other half — the real UI picker
    driving a real local-file read through a real HTTP blob upload — which
    that test does not touch.

    `fauna.profile.get`'s body is a signed `EmbedAsBytes` wire whose typed
    decode is client-side only (`ApiActor.profile_get`'s own docstring), so
    this asserts the way that helper's other caller does: presence, plus the
    body's byte length, which visibly grows when `avatar`/`banner` go from
    absent to two 32-byte `ContentHash` refs and shrinks back to the
    text-only baseline once both are cleared.
    """
    app = logged_in_app
    app.profile.require_avatar_banner_upload_supported()
    if not TEST_IMAGE.exists():
        pytest.skip(f"test fixture missing: {TEST_IMAGE}")

    profile = app.profile
    profile.navigate()

    # A text-only baseline first, isolating the avatar/banner byte delta from
    # the first-publish defaults.
    profile.open_edit_form()
    profile.set_display_name("Ada Lovelace")
    profile.save()
    assert profile.wait_for_save(), (
        f"baseline text-only save failed: {profile.error_text()!r}"
    )

    actor = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    baseline_len = len(actor.profile_get()["body"])

    # Set both pictures.
    profile.open_edit_form()
    profile.set_avatar(str(TEST_IMAGE))
    profile.set_banner(str(TEST_IMAGE))
    assert profile.avatar_path_text() == profile.expected_staged_path_text(str(TEST_IMAGE))
    assert profile.banner_path_text() == profile.expected_staged_path_text(str(TEST_IMAGE))
    profile.save()
    assert profile.wait_for_save(), (
        f"save with a picked avatar+banner failed: {profile.error_text()!r}"
    )

    with_pictures = actor.profile_get()["body"]
    # Each ContentHash ref is a 32-byte digest; two newly-populated refs must
    # grow the signed body by well over that (CBOR framing + the raw-codec
    # CID prefix on top) — a generous floor that only a real upload landing
    # on both fields can clear.
    assert len(with_pictures) > baseline_len + 40, (
        f"body did not grow after setting avatar+banner: baseline="
        f"{baseline_len}, with_pictures={len(with_pictures)}"
    )

    # Clear both — the body should shrink back toward the text-only baseline.
    profile.open_edit_form()
    profile.remove_avatar()
    profile.remove_banner()
    profile.save()
    assert profile.wait_for_save(), (
        f"save after removing avatar+banner failed: {profile.error_text()!r}"
    )
    cleared_len = len(actor.profile_get()["body"])
    assert cleared_len < len(with_pictures), (
        f"body did not shrink after clearing avatar+banner: with_pictures="
        f"{len(with_pictures)}, cleared={cleared_len}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_other_profile_offers_render_and_subscribe(
    logged_in_app, nest_instance, test_user
):
    """Item 7 (OTHER-profile subscriber-browse): the viewer opens another
    creator's profile via a contact-row tap-through, sees the creator's offered
    tier in the Tiers-tab offers section, and subscribes — the row status flips
    to "Subscribed".

    Seeding needs no headless profile signing: `offers_list` reads a creator's
    tiers regardless of any published `Profile`, and the OTHER-profile header
    falls back to the actor id. So a headless AUTHOR registers + seeds an
    auto_approve tier, and the VIEWER (the UI user) accepts the author as a
    contact so they surface in the contacts list — the only wired tap-through to
    another actor's profile. The Subscribe click ends at `Pending approval`: the
    tier is client-minted, so the nest queues the request for the author rather
    than granting inline (see the assertion's own comment for why that is the
    ratified behavior and where the approve arm is tracked).
    """
    app = logged_in_app
    app.profile.require_other_profile_offers_browse_supported()

    # Headless author: register + seed an auto_approve paid tier.
    author = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    tier = f"gold-{uuid.uuid4().hex[:8]}"
    author_actor = ApiActor(
        nest_instance["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    author_actor.subscription_create_tier(
        tier, rank=1, auto_approve=True, price_hint="$5/mo"
    )

    # The viewer (the UI user) accepts the author as a contact so the author's row
    # appears in the contacts list (the wired nav path to their profile).
    viewer_actor = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    viewer_actor.accept_knock(author["actor_id_hex"])

    # Tap the author's contact row → their profile opens (open_profile(Some(hex))).
    app.contacts.open_contact_profile(author["actor_id_hex"])
    app.driver.wait_for("profile-view", timeout=10.0)

    # OTHER header (assert BEFORE the Tiers tab, which scrolls the header out of
    # view on some clients): the follow button is shown, the SELF edit button is
    # not (profile.md § Layout & flow → Another's profile; is_self=false branch).
    assert app.profile.wait_for_follow_button(), (
        "OTHER profile should show profile-follow-button; "
        f"edit_visible={app.profile.is_edit_button_visible()}, "
        f"error={app.profile.error_text()!r}"
    )
    assert not app.profile.is_edit_button_visible(), (
        "OTHER profile must not show profile-edit-button"
    )

    # The Tiers tab hosts the offers section; the seeded tier renders as a row.
    app.profile.open_tiers_tab()
    assert app.profile.wait_for_offer(tier), (
        f"author's offered tier {tier!r} should render in the offers section; "
        f"count={app.profile.offer_count()}, error={app.profile.error_text()!r}"
    )
    assert app.profile.has_offers_section(), "the offers section should render"
    assert app.profile.offer_count() == 1, "exactly the one seeded paid tier"
    assert app.profile.offer_price(0) == "$5/mo", (
        f"the offer row should render the seeded price_hint; got "
        f"{app.profile.offer_price(0)!r}"
    )
    # i18n subscriptions.offer_status_none.
    assert app.profile.offer_status(0) == S.subscriptions.offer_status_none, (
        f"before subscribing the row should read not-subscribed, got "
        f"{app.profile.offer_status(0)!r}"
    )

    # Subscribe via the row button → the request is QUEUED for the author, so the
    # status flips to "Pending approval" (i18n subscriptions.offer_status_pending).
    #
    # This is the ratified product behavior, not a degraded assertion. The nest
    # approves inline ONLY when it holds the tier's period key. The nest holds no
    # period key (every tier is client-minted), so `subscribe_handler` always
    # falls through to enqueue a `kind='subscribe'` row and returns `Queued`
    # (`bins/fauna-nest/src/subscription_handlers.rs`, the handler's own doc
    # comment; `monetization.md`). The real "auto" in auto_approve is client-side:
    # the AUTHOR's client drains it via
    # `SubscriptionsAuthorOrchestrator::drain_auto_approvals()`.
    #
    # This test's author is a headless `ApiActor` with no client, and there is no
    # headless approve path -- committing an approval needs the `encrypted_upload`
    # envelope only the author's client can mint. So the Queued->(pump)->Approved
    # arm needs an author-side GUI and is tracked separately;
    # asserting `Queued` here is the honest end of the flow this test drives, and
    # the test's own subject -- browsing ANOTHER actor's offers and subscribing --
    # is fully covered by it.
    #
    # Until 2026-07-31 this asserted `offer_status_active` on a stale
    # "nest is plaintext => grants inline" premise and was a STANDING RED that
    # three separate sessions had to notice and explain away.
    app.profile.subscribe_offer(0)
    assert app.profile.wait_for_offer_status(S.subscriptions.offer_status_pending), (
        f"after subscribing, a client-minted tier's offer status should flip to "
        f"Pending approval (the nest queues it for the author); got "
        f"{app.profile.offer_status(0)!r}, error={app.profile.error_text()!r}"
    )


@pytest.mark.feature("subscriptions-and-tiers")
def test_activating_the_tiers_tab_rereads_the_offers_section(
    logged_in_app, nest_instance, test_user
):
    """Activating `profile-tiers-tab` re-reads the OTHER-profile offers section,
    on every app — `monetization.md` § Pillar 1 → *The Tiers-tab re-read door*.

    **Why this test exists.** There is no push kind for a subscribe grant, so a
    subscriber sitting on an author's offers section has nothing to re-render
    from: whatever changes off-box — the author's client draining the approval
    queue, the author publishing another tier — is invisible until the app
    re-reads. Every app had a re-read door and they were four different ones
    (tui: every Tiers-tab activation; android: only when the tab branch
    re-composes, i.e. after a detour through Posts; linux/web/windows/apple:
    page granularity only, their tab click inert). So "did the author approve me
    yet?" — asked the obvious way, by re-clicking the tab — was answered on tui,
    sometimes on android, and never on the other four. This pins the ruled
    uniform door on whichever app runs it.

    **Why an added tier rather than a grant.** The door is "an off-box change to
    the offers section becomes visible on tab re-activation"; the *kind* of
    change is immaterial to it. A grant needs an author GUI (no headless approve
    exists — the `encrypted_upload` envelope is client-only), which would gate
    this to the two apps that can host a second native launch. A headless tier
    add is the same door, deterministically, on every app — and the pump test
    (`test_subscriptions.py`) still drives the grant arm through it.

    Latency-independent (convention 14): no wall-clock assumption anywhere — the
    helper deadline-polls a state, and a green run pays only the real re-read.
    """
    app = logged_in_app
    app.profile.require_other_profile_offers_browse_supported()

    author = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    author_actor = ApiActor(
        nest_instance["url"], author["token"], author["actor_id_hex"],
        bytes(author["signing_key"]),
    )
    # Render-only seeding, which is all this test needs (no grant materializes
    # here) — the orphan-tier caveat on subscription_create_tier does not bite.
    first = f"gold-{uuid.uuid4().hex[:8]}"
    author_actor.subscription_create_tier(first, rank=1, auto_approve=True)

    viewer_actor = ApiActor(
        nest_instance["url"], test_user["token"], test_user["actor_id_hex"],
        bytes(test_user["signing_key"]),
    )
    viewer_actor.accept_knock(author["actor_id_hex"])

    app.contacts.open_contact_profile(author["actor_id_hex"])
    app.driver.wait_for("profile-view", timeout=10.0)
    app.profile.open_tiers_tab()
    assert app.profile.wait_for_offer(first), (
        f"the author's first tier {first!r} should render before the external "
        f"change; count={app.profile.offer_count()}, "
        f"error={app.profile.error_text()!r}"
    )
    assert app.profile.offer_count() == 1, (
        f"exactly the one seeded tier should render before the external change; "
        f"got {app.profile.offer_count()}"
    )

    # ── The external change: the author publishes a second tier. The completed ──
    # round trip is the causal barrier — the nest holds both tiers from here on,
    # so anything the section still fails to show is the app's door, not a race.
    second = f"silver-{uuid.uuid4().hex[:8]}"
    author_actor.subscription_create_tier(second, rank=2, auto_approve=True)
    assert len(author_actor.subscription_tiers_list()) == 2, (
        "the author should hold both tiers nest-side before the re-read is "
        "asserted — the barrier this assertion exists to establish"
    )

    # ── The door: re-activating the tab must surface it. Before the 2026-08-12 ──
    # ruling this failed on linux, web and android (each stuck at 1) and passed
    # only on tui, which is the divergence the ruling closed.
    seen = app.profile.wait_for_offer_count_after_external_change(2)
    assert seen == 2, (
        f"activating profile-tiers-tab must re-read the offers section, so the "
        f"author's newly published tier {second!r} renders — the ruled uniform "
        f"door (monetization.md § Pillar 1 → The Tiers-tab re-read door). The "
        f"section still shows {seen} row(s); error={app.profile.error_text()!r}"
    )


# --- OTHER-profile secondary relationship actions (profile.md § Layout & flow) ----

# Lead = linux (profile lead); lifted to windows (linux-entrusted — ProfilePage
# start-DM seeds the shared ConversationsManager + NavigateToView; block over
# ProfileViewModel.ToggleBlockAsync → KnocksBlockAsync / KnocksUnblockAsync, the
# contact_status-driven Block⇄Unblock toggle, 2026-06-22) + macos + ios (shared
# FaunaKit `ProfileView`) + android (ProfileSecondaryActionsRow — start-DM seeds the
# shared ConversationsVM.startConversationWith + nav to conversation_compose; block
# over ProfileVM.toggleBlock → ApiClient.blockKnock / unblockKnock, the full
# contact_status-driven toggle, 2026-06-23). start-DM (nav glue) + block are built;
# profile-request-contact-button is parked on the knock-send federation gap
# (tracked internally), so it is not rendered and not asserted here.
def _is_block_toggle_client(driver) -> bool:
    """Clients that have flipped `profile-block-button` to the `contact_status`-
    driven Block⇄Unblock toggle (lead = linux). The others still ship the
    block-only interim (terminal "Blocked" label) until they lift the toggle off
    the linux reference — `profile.md` § Implementation status. A flipping client
    adds itself here and the toggle path covers it; when all six have flipped this
    helper and the block-only branch both go away. This is a transitional rollout
    gate, not a permanent platform difference."""
    return (
        driver.is_linux()
        or driver.is_web()
        or driver.is_windows()
        or driver.is_macos()
        or driver.is_ios()
        or driver.is_android()
        or driver.is_tui()
    )


@pytest.mark.feature("profile")
def test_other_profile_start_dm_opens_compose(logged_in_app, nest_instance, test_user):
    """`profile-start-dm-button` (OTHER profile): tapping it seeds the
    Conversations new-thread composer with the viewed actor and switches to the
    Conversations page — the recipient chip carries that actor. Pure nav glue:
    no new persistence, no new kind (profile.md § Where logic lives → Start DM)."""
    app = logged_in_app
    # All 7 apps. The earlier iOS-only gap is FIXED: iOS previously did not push
    # .newThread when the composer seed arrived from start-DM (it only mounted the
    # composer via the list's new-conversation-button), so profile→start_dm() seeded
    # the shared compose state but the NewThreadComposeView never appeared (macOS
    # shows it reactively in the detail pane, no push).
    # A change (2026-06-24) added a reactive push — onAppear + onChange(of:
    # vm.newThreadCompose != nil) in ConversationsListView, the iOS mirror of macOS's
    # reactive detail-pane mount — so start-DM now surfaces the composer on iOS too.
    other_actor_id = open_new_contact_profile(app, nest_instance, test_user)

    # The start-DM button is shown on the OTHER profile only.
    assert app.profile.wait_for_start_dm_button(), (
        "OTHER profile should show profile-start-dm-button; "
        f"error={app.profile.error_text()!r}"
    )

    # Tap it → the Conversations new-thread composer opens with the actor seeded
    # as a recipient chip (the chip display is the actor_id hex — the OTHER header
    # has no cached handle, same fallback as the header label).
    app.profile.start_dm()
    try:
        app.driver.wait_for("recipient-picker-chip", timeout=10.0)
    except TimeoutError:
        # Self-diagnose (e2e rule 6) instead of a bare timeout. The suspect on
        # iOS is the cancel→reopen NavigationStack race apple flagged: a stale
        # nav state could drop the reactive `.newThread` re-push so the composer
        # never surfaces. Report whether the whole composer is missing
        # (input/cancel also count=0 → not pushed) vs. just the chip
        # (composer pushed but the seed didn't promote to a chip) — the datum
        # we were asked to hand back to apple.
        raise AssertionError(
            "start-DM never surfaced the recipient chip; "
            f"chip={app.driver.diagnose('recipient-picker-chip')} "
            f"input={app.driver.diagnose('recipient-picker-input')} "
            f"cancel={app.driver.diagnose('new-conversation-cancel')}"
        ) from None
    assert app.driver.count("recipient-picker-chip") >= 1, (
        "the new-thread composer should carry the seeded recipient chip: "
        f"{app.driver.diagnose('recipient-picker-chip')}"
    )
    chip = app.driver.get_text("recipient-picker-chip", index=0)
    assert other_actor_id in chip, (
        f"the recipient chip should carry the viewed actor; got {chip!r}, "
        f"expected to contain {other_actor_id!r}"
    )


@pytest.mark.feature("profile")
def test_other_profile_block(logged_in_app, nest_instance, test_user):
    """`profile-block-button` (OTHER profile) is the Block⇄Unblock toggle whose
    label flips on the viewed actor's `contact_status` (profile.md § User actions;
    contacts.md § Where logic lives → Unblock).

    On a flipped client (lead = linux): the not-blocked edge reads "Block"; tapping
    blocks via the shared `fauna_client_contacts::knocks_block` over
    `fauna.knocks.block` (label → "Unblock"); tapping again unblocks via
    `knocks_unblock` over `fauna.knocks.unblock` — the guarded clear-the-edge,
    `ContactStatus` → `None` — and the label flips back to "Block".

    A client still on the block-only interim instead shows the terminal "Blocked"
    label after the single block (it has not lifted the toggle yet — see
    `_is_block_toggle_client`)."""
    app = logged_in_app
    # The viewer accepts the OTHER actor as a contact (status `accepted`), so the
    # edge starts not-blocked → the toggle reads "Block".
    open_new_contact_profile(app, nest_instance, test_user)

    assert app.profile.is_block_button_visible(), (
        "OTHER profile should show profile-block-button; "
        f"error={app.profile.error_text()!r}"
    )
    assert app.profile.block_label() == S.profile.block, (
        f"the block button should read 'Block' before tapping; got "
        f"{app.profile.block_label()!r}"
    )

    # Tap → knocks_block round-trips over the real wire (the label flips only when
    # the call returns ok).
    app.profile.block()

    if _is_block_toggle_client(app.driver):
        # Flipped client: the edge is now `blocked`, so the toggle reads "Unblock".
        assert app.profile.wait_for_block_label(S.profile.unblock), (
            f"after blocking, the toggle should flip to 'Unblock'; got "
            f"{app.profile.block_label()!r}, error={app.profile.error_text()!r}"
        )
        assert not app.profile.error_text(), (
            f"blocking should not surface an error; got {app.profile.error_text()!r}"
        )

        # Tap again → knocks_unblock clears the edge (guarded on `blocked`,
        # `ContactStatus` → `None`); the toggle flips back to "Block".
        app.profile.block()
        assert app.profile.wait_for_block_label(S.profile.block), (
            f"after unblocking, the toggle should flip back to 'Block'; got "
            f"{app.profile.block_label()!r}, error={app.profile.error_text()!r}"
        )
        assert not app.profile.error_text(), (
            f"unblocking should not surface an error; got "
            f"{app.profile.error_text()!r}"
        )
    else:
        # Block-only interim: the single block lands a terminal "Blocked" label.
        assert app.profile.wait_for_block_label(S.profile.blocked), (
            f"after blocking, the button label should flip to 'Blocked'; got "
            f"{app.profile.block_label()!r}, error={app.profile.error_text()!r}"
        )
        assert not app.profile.error_text(), (
            f"blocking should not surface an error; got {app.profile.error_text()!r}"
        )


@pytest.mark.feature("profile")
def test_other_profile_request_contact_lands_a_knock(logged_in_app, nest_instance, test_user):
    """`profile-request-contact-button` (OTHER profile) sends the same signed knock
    `contacts-add-button` does, to the VIEWED actor — proven on the recipient's
    side: their `fauna.knocks.list` now holds a pending knock from the viewer, and
    the button reads "Request sent" (profile.md § Where logic lives → *Request
    contact routing*).

    The viewed actor is a stranger (no contact edge — a knock is how one starts),
    opened by the state-protocol actor nav, and lives on the viewer's nest: it has
    published no profile, so the shared route rule answers "this nest".

    tier_3: real nest binary + real driver + real `fauna.inbox.send` over the wire.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    app = logged_in_app
    app.profile.require_request_contact_supported()
    app.profile.require_state_protocol_actor_nav_supported()

    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )

    def knocks_held_by_other() -> list:
        with WsRpcAdminClient(
            nest_instance["url"],
            actor_id=other["actor_id_bytes"],
            signing_key=bytes(other["signing_key"]),
        ) as client:
            return client.call("fauna.knocks.list", {}).get("knocks", [])

    assert not any(k["sender"] == test_user["actor_id_hex"] for k in knocks_held_by_other()), (
        "precondition: the other actor must not already hold a knock from the viewer"
    )

    app.profile.navigate_to_actor(other["actor_id_hex"])
    assert app.profile.wait_for_follow_button(), (
        f"the OTHER profile should open; error={app.profile.error_text()!r}"
    )

    app.profile.request_contact()
    assert app.profile.wait_for_request_contact_label(S.profile.request_contact_sent), (
        "after the knock lands the button should read 'Request sent'; "
        f"error={app.profile.error_text()!r}"
    )
    assert not app.profile.error_text(), (
        f"sending the knock should not surface an error; got {app.profile.error_text()!r}"
    )

    senders = [k["sender"] for k in knocks_held_by_other()]
    assert test_user["actor_id_hex"] in senders, (
        "the viewed actor should hold a pending knock from the viewer; "
        f"knocks from {senders}"
    )


@pytest.mark.feature("profile")
def test_profile_copy_button_copies_the_viewed_actors_id(logged_in_app, nest_instance, test_user):
    """`profile-actor-id-copy-btn` copies the actor id of the profile on screen —
    another person's on theirs, the viewer's own on their own (profile.md § User
    actions; `user-header` carries the button on both).

    The value is read from the button's `copied` attribute, written from the
    exact string that reached the clipboard: no driver reads the OS clipboard, and
    a visibility check would pass on a button that copied nothing, or the wrong
    id. Asserting both directions is what pins the button to the *viewed* actor
    rather than to whichever id the page happened to hold first."""
    app = logged_in_app
    app.profile.require_actor_id_copy_readback()

    other_actor_id = open_new_contact_profile(app, nest_instance, test_user)
    copied = app.profile.copy_actor_id()
    assert copied == other_actor_id, (
        f"on another person's profile the copy button must copy THEIR actor id "
        f"{other_actor_id!r}; it copied {copied!r}"
    )

    # The viewer's own profile: a fresh open, so nothing carries over from the
    # copy above, and the button now names the viewer. Opened by the viewer's own
    # id, which every app normalizes back to the SELF page.
    app.contacts.navigate()
    app.profile.navigate_to_actor(test_user["actor_id_hex"])
    assert app.profile.wait_for_edit_button(), (
        f"the viewer's own profile never opened; error={app.profile.error_text()!r}"
    )
    copied = app.profile.copy_actor_id()
    assert copied == test_user["actor_id_hex"], (
        f"on the viewer's own profile the copy button must copy the viewer's actor "
        f"id {test_user['actor_id_hex']!r}; it copied {copied!r}"
    )


@pytest.mark.feature("profile")
def test_state_protocol_actor_nav_opens_that_actor_then_normalizes_self(
    logged_in_app, nest_instance, test_user
):
    """The state-protocol nav (`{"view":"profile","actor_id":<hex>}`) reaches the
    SAME per-target profile page the contacts tap-through builds — and an
    `actor_id` naming the VIEWER normalizes back to the SELF page.

    Regression pin for the linux silent no-op: the nav patch used to read only
    the stack entry's `view`, so `ProfileActions.navigate_to_actor` switched the
    content stack to a profile page that had been *built* for SELF and therefore
    always rendered the SELF header — a dropped command by testing.md convention
    11's definition (no error, no effect, and a downstream read that looks like a
    product bug). Pre-fix this test fails on the first assertion with
    `profile-edit-button` showing and `error-message` empty.

    Both directions matter and only one of them is the obvious one:
      * OTHER — the named actor's page renders (`profile-follow-button`).
      * SELF  — an `actor_id` equal to the viewer's own must resolve back to the
        SELF page (`profile-edit-button`), because the page derives
        `is_self = target.is_none()`; passing `Some(own_hex)` would render the
        viewer's own profile in OTHER shape. That normalization is the pure half
        (`test_agent::profile_nav_target`, unit-pinned) and this is its
        end-to-end witness.

    No headless profile signing is needed: the OTHER header falls back to the
    actor id when no `Profile` is published, and unlike
    `test_other_profile_offers_render_and_subscribe` this journey needs no
    contact edge either — driving nav by actor id is the whole point.

    profile.md § Layout & flow → Another's profile. tier_3: real nest binary +
    real driver + the real per-target page build.
    """
    app = logged_in_app
    app.profile.require_state_protocol_actor_nav_supported()

    # A second registered actor to view. No contact edge, no published profile —
    # the nav is by actor id alone.
    other = create_actor_and_register(
        nest_instance["port"],
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )

    # OTHER: the named actor's page, in OTHER shape.
    app.profile.navigate_to_actor(other["actor_id_hex"])
    assert app.profile.wait_for_follow_button(), (
        "state-protocol nav to another actor should build that actor's profile "
        "(profile-follow-button); still on the SELF page means the nav patch "
        f"dropped the actor_id. edit_visible={app.profile.is_edit_button_visible()}, "
        f"error={app.profile.error_text()!r}"
    )
    assert not app.profile.is_edit_button_visible(), (
        "the OTHER profile must not show profile-edit-button"
    )

    # SELF: the viewer's own actor_id resolves back to the SELF page. Navigating
    # away first makes this a real transition rather than a no-op re-assert.
    app.contacts.navigate()
    app.profile.navigate_to_actor(test_user["actor_id_hex"])
    assert app.profile.wait_for_edit_button(), (
        "an actor_id naming the viewer should resolve to the SELF profile "
        "(profile-edit-button); rendering the follow button means the viewer's "
        "own id was passed through as an OTHER target. "
        f"follow_visible={app.profile.is_follow_button_visible()}, "
        f"error={app.profile.error_text()!r}"
    )
    assert not app.profile.is_follow_button_visible(), (
        "the SELF profile must not show profile-follow-button"
    )
