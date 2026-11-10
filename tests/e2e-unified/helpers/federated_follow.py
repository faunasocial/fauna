"""The federated follow, driven through the app UI — shared by the tier_3
journeys that need a reader following an author on another nest.

``docs/goal/behavior/federated-follow.md`` is the owner. :func:`follow_across_nests`
is the journey's door and grant: the follower finds the author by
``handle@authority``, opens their Profile through the Find User result (which
carries the author's home), follows at that home, and waits until the author's
tui pump has granted it and the approval has crossed back — every mutation a
gesture in a seat (convention 8), every wait a deadline poll over projected
state (convention 14).
"""

from __future__ import annotations

import secrets
import time

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.room_seats import (
    MESSAGE_BUDGET_S,
    WELCOME_BUDGET_S,
    Seat,
    seat_log,
    seat_story,
    wait_for,
)

FOLLOWERS_TIER = "followers"

CROSSING_BUDGET_S = WELCOME_BUDGET_S
"""For a nest → nest round trip: the follow crossing to the home and the
approval crossing back, or a post pushed through the home's outbox. This is
the same order of crossing as a Welcome."""


def call(nest: dict, actor: dict, kind: str, payload: dict) -> dict:
    """One WS-RPC read as ``actor`` on the nest that holds their account."""
    with WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"])
    ) as client:
        return client.call(kind, payload)


def follow_status(away: dict, follower: Seat, author: Seat) -> dict:
    """The follower's own nest's view of the follow (``status.get`` reads the
    ``remote_follows`` row: ``follow_pending`` until the home answers, the
    tier once its ``follow.state`` push says approved)."""
    return call(
        away,
        follower.actor,
        "fauna.subscriptions.status.get",
        {"author_id": bytes.fromhex(author.actor_hex)},
    )


def feed_row(seat: Seat, text: str):
    """The seat's feed row whose body holds ``text``. It re-enters the page
    first, because a feed re-pulls only on a page entry
    (``FeedActions.reenter``)."""
    seat.app.feed.reenter()
    return seat.app.feed.post_state_by_text(text)


def follow_across_nests(follower: Seat, author: Seat, home: dict, away: dict) -> None:
    """``follower`` (on ``away``) follows ``author`` (on ``home``) through the
    UI, and returns once the follower's own nest holds the follow as approved
    in the followers tier."""
    author_address = f"{author.handle}@{home['authority']}"

    # ── The door: Find User across nests → the author's profile ──────────
    contacts = follower.app.contacts
    contacts.navigate()
    contacts.find_by_handle(author_address)
    wait_for(
        "the follower's Find User resolving the author on their home nest",
        contacts.actor_id_result_text,
        lambda text: text == author.actor_hex,
        CROSSING_BUDGET_S,
        diagnose=lambda: f"find-error={contacts.find_error_text()!r}\n{seat_story(follower)}",
    )
    contacts.open_found_profile()
    profile = follower.app.profile
    assert profile.wait_for_viewed_actor(author.actor_hex) == author.actor_hex, (
        f"the door opens the found author: error={profile.error_text()!r}"
    )
    carried = wait_for(
        "the door carrying the author's home into the profile",
        lambda: ((follower.app.driver.get_state() or {}).get("data", {}).get("profile") or {}).get(
            "home_nest_url"
        ),
        bool,
        MESSAGE_BUDGET_S,
    )
    assert home["authority"] in carried, (
        f"the carried home names the author's nest: profile.home_nest_url={carried!r}, "
        f"authority={home['authority']!r}"
    )
    assert profile.wait_for_follow_button(), seat_story(follower)

    # ── Follow at the home ────────────────────────────────────────────────
    profile.follow()
    assert profile.wait_for_follow_label("Following", timeout=MESSAGE_BUDGET_S), (
        f"the follow at the author's home should land: label={profile.follow_label()!r} "
        f"error={profile.error_text()!r}\n{seat_log(follower, 'follow', 'subscribe')}"
    )

    # ── The author's pump grants it; the approval crosses back ────────────
    status = wait_for(
        "the follow being approved at the home and the state crossing back",
        lambda: follow_status(away, follower, author),
        lambda s: s.get("tier") == FOLLOWERS_TIER and not s.get("follow_pending"),
        CROSSING_BUDGET_S,
        diagnose=lambda: (
            f"{seat_story(author)}\n{seat_log(author, 'approv', 'drain', 'subscri')}"
        ),
    )
    assert status.get("tier") == FOLLOWERS_TIER, status


# ── The app-less halves ────────────────────────────────────────────────────
#
# The ``[nest]`` witnesses in ``tests/api/`` drive a follow across nests with
# no app: the subscriber's gesture and the author's pump run through the shared
# Rust every app calls (``fauna_ffi.harness_follow`` / ``harness_redeem`` /
# ``harness_author_reconcile``), and every read is a WS-RPC call on the nest
# that holds the reader's account. Actors here are ``admit_adult`` dicts
# (``signing_key``, ``actor_id_bytes``); nests are ``cross_nest_foreign``-shaped
# handles.


def seed_of(actor: dict) -> bytes:
    """The actor's 32-byte identity seed — what the harness signs with."""
    return bytes(actor["signing_key"])


def hex_of(actor: dict) -> str:
    return bytes(actor["actor_id_bytes"]).hex()


def peer_of(nest: dict) -> str:
    """The authority a nest is asked to dial for ``nest`` (``peer_url`` where
    the handle carries one — the same string as ``url`` outside docker)."""
    return nest.get("peer_url") or nest["url"]


def new_reader(away: dict) -> dict:
    """A fresh handled account on the readers' nest ``away``."""
    from helpers.admin_wire import admit_adult

    return admit_adult(away, f"rdr{secrets.token_hex(4)}", domain=away["authority"], token=True)


def status_of(away: dict, reader: dict, author_hex: str) -> dict:
    """The reader's own nest's view of their follow of the author
    (``status.get`` reads the ``remote_follows`` row)."""
    return call(
        away, reader, "fauna.subscriptions.status.get", {"author_id": bytes.fromhex(author_hex)}
    )


def pump_until(home: dict, author: dict, what: str, read, done):
    """Run the author's pump, then poll ``read`` until ``done``: the pump commits
    whatever the home has queued (a follow, a paid request, a leave or a
    removal), and the state crosses back on its own. One more pump per poll
    round only while nothing moved — a queued request can arrive at the home
    after a pump ran."""
    import fauna_ffi

    deadline = time.monotonic() + CROSSING_BUDGET_S
    last = None
    while time.monotonic() < deadline:
        fauna_ffi.harness_author_reconcile(home["url"], seed_of(author))
        inner = time.monotonic() + 10
        while time.monotonic() < min(inner, deadline):
            last = read()
            if done(last):
                return last
            time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {what}; last read: {last!r}")


def post_as(home: dict, author: dict, text: str) -> str:
    """The author posts ``text`` on their home; returns the post id."""
    from tests.api.bare import sign_and_encode_post

    body = sign_and_encode_post(author["signing_key"], int(time.time() * 1_000_000), text)
    return call(home, author, "fauna.posts.create", {"body": body})["post_id"]


def listed_to(nest: dict, reader: dict, author_hex: str) -> dict[str, dict]:
    """Every post ``reader``'s own nest lists to them from ``author_hex``, by
    post id: their timeline read and the author read alike."""
    timeline = call(nest, reader, "fauna.feed.local.posts", {"limit": 200}).get("posts", [])
    by_author = call(
        nest, reader, "fauna.feed.author.posts", {"author": author_hex}
    ).get("posts", [])
    return {p["post_id"]: p for p in timeline + by_author}


def wait_listed(nest: dict, reader: dict, author_hex: str, post_id: str, what: str) -> dict:
    return wait_for(
        what,
        lambda: listed_to(nest, reader, author_hex).get(post_id),
        lambda row: row is not None,
        CROSSING_BUDGET_S,
    )
