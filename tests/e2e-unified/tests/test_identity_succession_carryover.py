"""tier_3 e2e: what a recovery carries across that the user set up BEFORE it.

Three outcomes of ``docs/features/take-your-account-back.md`` that were ruled
and built nest-side with Rust pins but no journey:

- **39** — someone you had blocked stays blocked after the recovery
  (``docs/goal/behavior/succession-propagation.md`` § Propagation, the contacts
  collision keeps the stricter status; built 2026-08-31 as
  ``db/successions.rs::collapse_contact_edges``);
- **42** — subscriptions you pay for as a reader come with the account
  (``docs/goal/behavior/succession-repoint-axis.md`` § The declared re-point
  axis, the paying reader's side ruled 2026-08-13:
  ``rule_the_paying_readers_entitlements`` moves ``subscribers.subscriber_id``);
- **45** — you can delete posts you made before the recovery (the same
  section's content plane, ruled and built 2026-08-14 to close "the successor
  cannot delete its own legacy posts": ``content.author`` moves).

**Why one file, and why each journey stands alone.** All three have the same
shape — the user does something through the app, the account is taken back,
the same surface still says the same thing (or, for 45, still lets the user
act) — and none depends on another. Each journey runs its own ceremony over its
own dedicated actor (``succeedable_app``): a succession re-points the signed-in
account, so a shared actor would leave every later test in the run talking to a
retired identity.

⚠ **The pre-ceremony act is the whole test, every time** — the rule
``test_identity_succession_aftermath.py`` states for every leg. A block, a
subscription or a post that never landed would make the matching "it is still
there afterwards" read vacuous, so each journey asserts its precondition is
visible in the app BEFORE the ceremony starts.

Convention 8: the block, the delete and the tier are user gestures in the app.
The reader's subscribe is a headless API call because a subscribe to a
client-minted tier only ever QUEUES and the author's app must approve it
(``monetization.md`` § Pillar 1 → the canonical e2e seeding shape, which
``test_subscriptions.py`` follows too); the behaviour under test is what the
reader's app shows afterwards. Convention 14: every wait is a deadline poll on
state the app shows, never a settle.

tier_3: a real ``fauna-nest`` binary and the app under test.
"""
from __future__ import annotations

import uuid

import pytest

from actions.api_actor import ApiActor
from helpers.diagnostics import aftermath_log_lines as _aftermath_log
from helpers.other_profile import open_other_profile, seed_accepted_contact
from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    SUCCESSION_AND_RELAUNCH_S,
    require_stolen_gate,
    settled_actor_id,
    wait_for_successor_actor,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# What follows the ceremony is the successor's first session against a nest the
# app is already connected to — the contact roster, the subscriptions page and
# the feed each read their plane afresh. Sized like the aftermath journeys'
# budgets: a green run pays none of it (convention 14).
_CARRY_READ_S = 120.0


def _succeed(app) -> tuple[str, str]:
    """Mint a kit, run "my identity was stolen" with it, and return
    ``(old_actor, new_actor)`` once the app is signed in as the successor.

    The steps every succession journey drives (``helpers/inherited_corpus.py``
    ``seed_under_a_then_succeed`` and the aftermath file's ``_run_succession``
    take the same ones); anchored on the rendered actor id changing and the
    live session following it, never on elapsed time.
    """
    old_actor = settled_actor_id(app)
    assert len(old_actor) == SECRET_HEX_LEN, (
        "the Status page must render the signed-in actor id before the "
        f"ceremony, so the switch is observable; got {old_actor!r}, error "
        f"surface: {app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(app, old_actor)
    assert new_actor != old_actor, (
        "the account must belong to a freshly minted identity before anything "
        "it carried across means anything"
    )
    return old_actor, new_actor


@pytest.mark.feature("take-your-account-back")
def test_someone_you_blocked_stays_blocked_after_the_recovery(
    succeedable_app, nest_instance
):
    """block a contact → succeed → the contact is still blocked (outcome 39).

    ``succession-propagation.md`` § Propagation → Contacts: the ceremony moves
    every contact edge from the retired identity to the successor, and where
    the two already share a peer the merged edge keeps the STRICTER status, so
    a block is never lost to the move. The collision half — both identities
    holding an edge to one peer — is pinned at both legs in Rust
    (``tests/succession_propagation.rs::a_pushed_succession_collision_keeps_the_blocked_edge``);
    what no Rust pin reaches is the user's view of it: that the successor's own
    app reads the peer back as blocked, on the surface where the user blocked
    them, so the toggle still offers *Unblock* rather than *Block*.

    ⚠ The pre-ceremony block is asserted on the toggle's label: a block that
    never landed would leave the peer merely accepted, and "not blocked after"
    would then be no finding at all.
    """
    app, user = succeedable_app

    # ── Precondition: an accepted contact, blocked through their Profile ────
    # The accepted edge is arrangement (it is the only tap-through to another
    # actor's Profile — `helpers/other_profile.py`); the block is the user's.
    other = seed_accepted_contact(nest_instance, user)
    open_other_profile(app, other)
    assert app.profile.wait_for_block_label(S.profile.block), (
        "an accepted, unblocked contact's toggle reads Block before the "
        f"user acts; got {app.profile.block_label()!r}, "
        f"error={app.profile.error_text()!r}"
    )
    app.profile.block()
    assert app.profile.wait_for_block_label(S.profile.unblock), (
        "the block must land BEFORE the ceremony or this test proves nothing: "
        f"the toggle reads {app.profile.block_label()!r}, "
        f"error={app.profile.error_text()!r}"
    )

    _, new_actor = _succeed(app)

    # ── The successor's app still reads the peer as blocked ─────────────────
    # Each poll re-opens the Profile from the contact row: the toggle's label
    # is derived from the edge the open-time read returns, so a fresh open is
    # what makes the read a statement about the successor's edge at rest.
    last: dict[str, str] = {"label": "", "error": ""}

    def _still_blocked() -> bool:
        try:
            open_other_profile(app, other)
            app.driver.wait_until_enabled("profile-block-button", timeout=30.0)
            last["label"] = app.profile.block_label()
        except Exception as e:  # noqa: BLE001 — reported below
            last["error"] = f"{type(e).__name__}: {e}"
            return False
        last["error"] = ""
        return last["label"] == S.profile.unblock

    wait_until(
        _still_blocked,
        _CARRY_READ_S,
        diagnose=lambda: (
            "the successor's app must still read the contact the user blocked "
            "as blocked (succession-propagation.md § Propagation → Contacts: "
            "the stricter status survives the move), but the toggle reads "
            f"{last['label']!r} (expected {S.profile.unblock!r}); last re-open "
            f"{last['error'] or 'succeeded'}; successor={new_actor!r}; "
            f"error={app.error_text()!r}. A Block label means the edge moved "
            "without its status, or a fresh accepted edge won the merge; no row "
            "at all means the roster lost the peer in the move."
        ),
    )


@pytest.mark.feature("take-your-account-back")
def test_a_readers_subscriptions_come_with_the_account(
    request, succeedable_app, nest_instance
):
    """subscribe as a reader → succeed → the subscription is still listed,
    still active, still to the same creator (outcome 42).

    ``succession-repoint-axis.md`` § The declared re-point axis, the paying
    reader's side: ``subscribers.subscriber_id`` moves with the account in the
    ceremony (``rule_the_paying_readers_entitlements``). The loss it closed was
    user-visible on exactly this page: a reader who rotated their identity did
    not merely drop off ``fauna.subscriptions.mine.list`` — every tier they had
    paid for went dark, with no way back from any app.

    The app plays both people here, in turn: a tier on a client-minted plane
    can only be created and approved by its author's app (the canonical
    seeding shape, ``test_subscriptions.py``'s module docstring), so the app
    signs in as the creator for those two gestures and then as the reader,
    whose page is what the outcome is about and whose account is taken back.

    ⚠ The pre-ceremony read is the reader's own page listing the subscription
    as active — an unapproved request or a grant that never landed would leave
    nothing to carry, and "listed afterwards" would then be vacuous.
    """
    from conftest import _login_app_as, _make_user

    app, reader = succeedable_app
    author = _make_user(nest_instance)
    tier = f"gold-{uuid.uuid4().hex[:8]}"

    # ── Precondition part 1: the creator's tier, created in the creator's app ──
    _login_app_as(app, request, nest_instance, author, verify_live_actor=True)
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"the creator's tier {tier!r} must exist before the reader can "
        f"subscribe to it; error={subs.error_text()!r}"
    )

    # ── Precondition part 2: the reader subscribes; the creator approves ────
    reader_actor = ApiActor(
        nest_instance["url"], reader["token"], reader["actor_id_hex"],
        bytes(reader["signing_key"]),
    )
    reply = reader_actor.subscribe(author["actor_id_bytes"], tier)
    assert reply.get("outcome") == "queued", (
        "a subscribe to a fresh client-minted tier always queues for the "
        f"creator's app to approve (monetization.md § Pillar 1); got {reply!r}"
    )
    subs.refresh()
    assert subs.wait_for_pending_request(1), (
        "the reader's request must reach the creator's page before it can be "
        f"approved; error={subs.error_text()!r}"
    )
    subs.approve_first_request()
    assert subs.wait_for_subscriber(1), (
        "the approve must seat the reader on the creator's roster — that row "
        f"is what the ceremony moves; error={subs.error_text()!r}"
    )

    # ── Precondition part 3: the READER's page lists it as active ───────────
    _login_app_as(app, request, nest_instance, reader, verify_live_actor=True)
    subs = app.subscriptions
    subs.navigate_settings()
    assert subs.wait_for_mine_subscription(tier, timeout=_CARRY_READ_S), (
        "the reader's Subscriptions page must list the subscription BEFORE the "
        f"ceremony or this test proves nothing; error={subs.error_text()!r}"
    )
    at = subs.mine_tiers().index(tier)
    assert subs.mine_status(at) == "active", (
        f"the approved subscription reads active before the ceremony; got "
        f"{subs.mine_status(at)!r}"
    )
    creator_before = subs.mine_author(at)

    _, new_actor = _succeed(app)

    # ── The successor's page still lists it, active, to the same creator ────
    # A fresh page read on every poll (`refresh_settings` bounces through
    # another sub-page so the on-visible `mine.list` re-read fires): the
    # successor's first visit can land before its session has settled.
    def _listed() -> bool:
        try:
            subs.refresh_settings()
            return tier in subs.mine_tiers()
        except Exception:  # noqa: BLE001 — the diagnosis below reads the page
            return False

    wait_until(
        _listed,
        _CARRY_READ_S,
        diagnose=lambda: (
            "the successor's Subscriptions page must still list the tier the "
            "reader paid for (succession-repoint-axis.md § The declared "
            "re-point axis — the paying reader's side), but lists "
            f"{subs.mine_tiers()!r}; successor={new_actor!r}; "
            f"error={subs.error_text()!r}. An empty list means the roster row "
            "stayed with the retired identity, so every tier the reader paid "
            "for went dark."
        ),
    )
    at = subs.mine_tiers().index(tier)
    assert subs.mine_status(at) == "active", (
        "the carried subscription must still read active — a row that moved "
        "but lost its entitlement is the same loss with a row left behind; "
        f"got {subs.mine_status(at)!r}"
    )
    assert subs.mine_author(at) == creator_before, (
        "the carried subscription must still name the same creator; got "
        f"{subs.mine_author(at)!r}, before the ceremony {creator_before!r}"
    )


@pytest.mark.feature("take-your-account-back")
def test_posts_from_before_the_recovery_can_still_be_deleted(succeedable_app):
    """post → succeed → the successor deletes that post, and it is gone
    (outcome 45).

    ``succession-repoint-axis.md`` § The declared re-point axis, the content
    plane (ruled and built 2026-08-14): ``content.author`` moves with the
    account, because ``check_post_delete_authorization`` compares the caller to
    the stored author — left behind, the successor's own tombstone was refused
    in both storage shapes, a state no app could repair (``nest/common.md``
    § Client-state recoverability). The nest-side pins cover the refusal; what
    they cannot reach is the user's side of it: that the successor's app still
    treats the post as the user's own — offers the delete in the post's menu at
    all — and that the delete lands and stays landed.

    ⚠ The pre-ceremony post is asserted in the feed's own state with its id:
    the delete afterwards is keyed on that id, never on a card count, so a post
    that never landed cannot make the "gone" read pass.
    """
    app, _ = succeedable_app
    feed = app.feed

    # ── Precondition: a post of the user's own, made in the app ─────────────
    text = f"before-the-recovery-{uuid.uuid4().hex[:8]}"
    feed.create_post(text=text)
    row = feed.wait_for_post_state_by_text(text)
    assert row is not None and row.get("post_id"), (
        f"the user's own post {text!r} must be in the feed BEFORE the ceremony "
        f"or this test proves nothing; error={app.error_text()!r}"
    )
    post_id = row["post_id"]

    _, new_actor = _succeed(app)

    # ── The successor sees the post, and its menu offers the delete ─────────
    # The ceremony leaves the app on Settings, so each poll ENTERS the Feed
    # page — the entry is what re-queries the nest (`FeedActions.reenter`); the
    # successor's first read can land before its session has settled. Clicking
    # a feed row instead would find no row off the Feed page, and re-clicking a
    # row already selected starts no reload on every app.
    last_error: list[str] = []

    def _listed() -> bool:
        try:
            feed.reenter()
            return feed.post_state_by_id(post_id) is not None
        except Exception as exc:  # noqa: BLE001 — named in the diagnosis
            last_error[:] = [f"{type(exc).__name__}: {exc}"]
            return False

    def _feed_ids() -> list:
        try:
            return [p.get("post_id") for p in feed._feed_posts_from_state()]
        except Exception as exc:  # noqa: BLE001 — a diagnosis never raises
            return [f"<unreadable: {exc}>"]

    wait_until(
        _listed,
        _CARRY_READ_S,
        diagnose=lambda: (
            f"the successor's feed must still show the post {text!r} the user "
            f"made before the recovery (id {post_id}); successor={new_actor!r}; "
            f"feed state holds {_feed_ids()!r}; last poll error "
            f"{last_error!r}; error={app.error_text()!r}"
        ),
    )
    at = feed.card_index_by_id(post_id)
    feed.open_post_actions(index=at)
    assert feed.post_delete_visible(), (
        "the successor's app must still offer delete on a post the user made "
        "before the recovery: the nest moved its author to the successor "
        "(the content plane), so a menu without delete means the app still "
        "reads the post as someone else's and the user cannot remove their "
        f"own words. error={app.error_text()!r}; aftermath log "
        f"{_aftermath_log(app)}"
    )
    # The menu stays open: `delete_post` opens it idempotently, so it presses
    # the delete on the very menu just read.

    # ── The delete lands, and stays landed ──────────────────────────────────
    feed.delete_post(index=feed.card_index_by_id(post_id))
    assert feed.post_state_by_id(post_id) is None, (
        "the deleted post must leave the successor's feed"
    )

    def _still_gone() -> bool:
        feed.reenter()
        return feed.post_state_by_id(post_id) is None

    wait_until(
        _still_gone,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "a reload of the feed brought the deleted post back: the delete "
            "was local only, and the nest refused the successor's tombstone "
            f"(error={app.error_text()!r})"
        ),
    )
