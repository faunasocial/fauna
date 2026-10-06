"""tier_3: the audience picked for a half-written post is still picked after an
app restart — a sale with its price and teaser, a tier, a room —
``docs/goal/ui/feed.md`` § Persistence: "``text``, ``tags``,
``attached_file``, ``gate_tier``, ``gate_preview``, ``sell`` and ``gate_room``
survive" (only user-authored input rests, enumerated by
``fauna_feed::drafts::PostDrafts``).

``test_feed_draft_persistence.py`` proves a post draft's TEXT survives, and
``test_feed_draft_attachment_restore.py`` its file. Neither picks an audience,
so an app that restored the words and dropped "who can read this" — handing the
author back a Public post they had addressed to a tier, a room or a sale — would
pass both. That is the direction worth fearing: the next submit would widen the
post silently, the same harm § User actions' sticky-audience rule exists to
prevent on the post-submit clear.

**Three journeys, one per kind of audience**, because each comes back through a
different door. A sale's answer and its fields render straight off the restored
draft. A tier's name is the draft's own string. A room is the one that can
fail on the way back: the composer names it by its label, which it looks up in
the rooms the app has re-read for this account (``own_rooms``), so a relaunched
app shows the room only once its conversations state is back — the room journey
waits for exactly that, on the owner's own seat.

**Ordering is what makes the sealed draft blob assertable.** The blob is opaque
to the test, so "a save landed" is observed as "the blob changed", and each wait
follows exactly one change to the draft (the text, the answer, then each field
the answer brings), so the blob in hand at the restart is the draft's final
state rather than one the debounce caught half-way. The restart is the DEFAULT
fresh-store relaunch, never ``preserve_state_across_relaunch()``: only what
reached the nest's ``__drafts`` plane may come back.

**Dedicated accounts** — a fresh one for the sale and the tier, the room's own
three seats for the room — so no earlier test's resting draft is restored into
these composers, and the tier created here never appears in another test's
audience list.

Latency-independent throughout (e2e convention 14): every wait polls the
observable that IS the contract — the nest's draft blob, the restored field,
the picked audience — under a ceiling a green run never pays.
"""

import time
import uuid

import pytest

from helpers.room_seats import (  # noqa: F401 — `room_app`/`room_seats` are fixtures
    MESSAGE_BUDGET_S,
    bootstrap_room,
    room_app,
    room_seats,
    seat_story,
    thread_by_id,
)
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    # tui leads. An app joins once its composer forwards each audience answer
    # to the manager as it is picked and paints a restored one back
    # (`ui/feed.md` § Implementation status today) — the drafts-survive
    # cross-app lift.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    # android: `FeedComposeScreen` forwards each answer as it is picked
    # (`FeedVM.setComposeGate`/`setComposeRoom`/`setComposeSell`, the teaser
    # through `setComposePreview` — the same `updateCompose*` manager calls
    # every other app makes) and paints the select, teaser and sale fields
    # from `snapshot.compose`, 2026-09-29. The marker went on 2026-09-25 on a
    # `FeedVM.stageComposeGate` that never existed; until the forwarding
    # landed, android re-staged a page-local Public audience at submit.
    # `compose_body_text`'s android arm (`actions/feed.py`) reads the composer
    # back, and `ROOM_APPS` carries android for the room journey's seats
    # (`helpers/room_seats.py`). The e2e run is host-emulator-gated; the
    # runnable pin is `FeedVMComposeAudienceTest` (`just android-host-test`).
    pytest.mark.android,
    # The room journey's seats need the real conversations session (tui always
    # runs it; the marker keeps the session-wide env consistent cross-app).
    pytest.mark.real_conversations,
    # The debounce itself must land each draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]

#: The draft rail this test drives (``fauna_protocol::drafts::DRAFT_RAILS``).
RAIL = "posts"


def _poll(read, done, timeout: float = 15.0):
    """Read until ``done(value)`` or the budget runs out; returns the last value
    read, for the failure message."""
    deadline = time.monotonic() + timeout
    value = read()
    while not done(value) and time.monotonic() < deadline:
        time.sleep(0.2)
        value = read()
    return value


def _text_if_visible(driver, element_id: str) -> str:
    return driver.get_text(element_id) if driver.is_visible(element_id) else ""


class _Rail:
    """The account's ``posts`` rail as the nest holds it, read by a fresh
    side-channel device for the same actor, plus the last blob seen — so each
    ``saved`` call means "the one change just made has landed"."""

    def __init__(self, node_url: str, actor: dict):
        self._node_url = node_url
        self._actor_id = actor["actor_id_bytes"]
        self._signing_key = bytes(actor["signing_key"])
        self.last = self._read()

    def _read(self):
        from clients.ws_rpc_admin_client import WsRpcAdminClient

        with WsRpcAdminClient(
            self._node_url, actor_id=self._actor_id, signing_key=self._signing_key
        ) as dev:
            return dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")

    def saved(self, what: str, timeout: float = 20.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            blob = self._read()
            if blob is not None and blob != self.last:
                self.last = blob
                return
            time.sleep(0.5)
        raise AssertionError(
            f"precondition: {what} must re-save the post draft to the nest __drafts "
            f"plane at path={RAIL!r} before the restart — a change to the draft that "
            f"never reaches the rail cannot survive one"
        )


def _type_body(app, rail: _Rail, body: str) -> None:
    app.feed.navigate()
    app.feed.open_composer()
    app.driver.type_text("compose-text-field", body)
    typed = _poll(app.feed.compose_body_text, lambda t: t == body)
    assert typed == body, f"precondition: the post text must be in the composer, got {typed!r}"
    rail.saved("typing the post")


def _restart_to_composer(app, body: str) -> None:
    app.driver.hard_reload()
    app.feed.navigate()
    app.feed.open_composer()
    restored = _poll(app.feed.compose_body_text, lambda t: t == body)
    assert restored == body, (
        f"the post draft did not survive the restart (expected {body!r}, got "
        f"{restored!r}); error={app.error_text()!r}"
    )


def _login_fresh(app, request, nest_instance) -> dict:
    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    return user


@pytest.mark.feature("drafts-survive")
def test_a_post_drafts_sale_with_its_price_and_teaser_survives_a_restart(
    app, request, nest_instance
):
    user = _login_fresh(app, request, nest_instance)
    driver = app.driver
    rail = _Rail(nest_instance["url"], user)
    tag = uuid.uuid4().hex[:8]
    body = f"a post I mean to sell, not finished yet {tag}"
    teaser = f"what a buyer reads before paying {tag}"
    price = "$3"
    asking_sats = "3000"

    _type_body(app, rail, body)

    driver.select("compose-gate-tier-select", S.feed.post.gate_sell)
    driver.wait_for("compose-sell-price")
    rail.saved("choosing to sell the post")
    driver.type_text("compose-sell-price", price)
    rail.saved("typing the price")
    driver.type_text("compose-sell-asking-price", asking_sats)
    rail.saved("typing the asking price")
    # Subscribers get a sold post free by default; turning that OFF is the one
    # sale setting whose survival is not indistinguishable from a fresh default.
    driver.click("compose-sell-subscribers-free")
    rail.saved("turning off the subscribers-get-it-free default")
    driver.type_text("compose-gate-preview-field", teaser)
    rail.saved("typing the teaser")

    _restart_to_composer(app, body)

    audience = _poll(app.feed.compose_audience, lambda a: a == S.feed.post.gate_sell)
    assert audience == S.feed.post.gate_sell, (
        f"the sale picked for the half-written post must still be picked after the "
        f"restart, got audience {audience!r}; error={app.error_text()!r}"
    )
    assert _text_if_visible(driver, "compose-sell-price") == price, (
        f"the sale's price must survive the restart: {driver.diagnose('compose-sell-price')}"
    )
    assert _text_if_visible(driver, "compose-sell-asking-price") == asking_sats, (
        "the sale's asking price must survive the restart: "
        f"{driver.diagnose('compose-sell-asking-price')}"
    )
    assert driver.get_attr("compose-sell-subscribers-free", "checked") == "false", (
        "the sale's subscribers-get-it-free answer must survive the restart as the "
        f"author left it (off): {driver.diagnose('compose-sell-subscribers-free', attrs=('checked',))}"
    )
    assert _text_if_visible(driver, "compose-gate-preview-field") == teaser, (
        "the sale's teaser must survive the restart: "
        f"{driver.diagnose('compose-gate-preview-field')}"
    )


@pytest.mark.feature("drafts-survive")
def test_a_post_drafts_tier_and_teaser_survive_a_restart(app, request, nest_instance):
    user = _login_fresh(app, request, nest_instance)
    driver = app.driver
    tag = uuid.uuid4().hex[:8]
    tier = f"gold-{tag}"
    body = f"a post for my tier, not finished yet {tag}"
    teaser = f"what everyone else reads {tag}"

    # Setup: a tier to address the post to, created where an author creates one.
    subs = app.subscriptions
    subs.navigate()
    subs.open_tiers_tab()
    subs.create_tier(tier, rank=1, price_hint="$5/mo")
    assert subs.wait_for_tier(tier), (
        f"precondition: the tier {tier!r} must be created; error={subs.error_text()!r}"
    )

    rail = _Rail(nest_instance["url"], user)
    _type_body(app, rail, body)
    assert app.feed.wait_for_audience_option(tier, 15.0), (
        f"precondition: compose-gate-tier-select must offer the tier {tier!r}; "
        f"error={app.error_text()!r}"
    )
    rail.saved("addressing the post to the tier")
    driver.wait_for("compose-gate-preview-field")
    driver.type_text("compose-gate-preview-field", teaser)
    rail.saved("typing the teaser")

    _restart_to_composer(app, body)

    audience = _poll(app.feed.compose_audience, lambda a: a == tier)
    assert audience == tier, (
        f"the tier picked for the half-written post must still be picked after the "
        f"restart, got audience {audience!r}; error={app.error_text()!r}"
    )
    assert _text_if_visible(driver, "compose-gate-preview-field") == teaser, (
        "the tier post's teaser must survive the restart: "
        f"{driver.diagnose('compose-gate-preview-field')}"
    )


@pytest.mark.feature("drafts-survive")
def test_a_post_drafts_room_and_teaser_survive_a_restart(
    room_seats, nest_instance, room_app, request
):
    owner, member, third = room_seats
    app = owner.app
    driver = app.driver
    tag = uuid.uuid4().hex[:8]
    body = f"a post for the room, not finished yet {tag}"
    teaser = f"what someone outside the room reads {tag}"

    # Setup: a room the author sits on the floor of (the real-wire bootstrap
    # every room journey shares — convention 8's carve-out (b)).
    group_id, _channel_hex = bootstrap_room(owner, member, third)
    option = S.feed.post.gate_room(room=thread_by_id(app, group_id).label)

    rail = _Rail(nest_instance["url"], owner.actor)
    _type_body(app, rail, body)
    assert app.feed.wait_for_audience_option(option, MESSAGE_BUDGET_S), (
        f"precondition: compose-gate-tier-select must offer {option!r} once the room "
        f"is bound; error={app.error_text()!r}\n{seat_story(owner)}"
    )
    rail.saved("addressing the post to the room")
    driver.wait_for("compose-gate-preview-field")
    driver.type_text("compose-gate-preview-field", teaser)
    rail.saved("typing the teaser")

    _restart_to_composer(app, body)
    # The relaunched seat's conversations state has to come back before the
    # composer can name the room at all; this is the same readiness poll the
    # seat's first launch ended on.
    app.conversations.enable_real_faunamls()

    audience = _poll(app.feed.compose_audience, lambda a: a == option, MESSAGE_BUDGET_S)
    assert audience == option, (
        f"the room picked for the half-written post must still be picked after the "
        f"restart, got audience {audience!r} — a Public here means the next submit "
        f"would publish a post the author addressed to a room; "
        f"error={app.error_text()!r}\n{seat_story(owner)}"
    )
    assert _text_if_visible(driver, "compose-gate-preview-field") == teaser, (
        "the room post's teaser must survive the restart: "
        f"{driver.diagnose('compose-gate-preview-field')}"
    )
