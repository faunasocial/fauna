"""The forward queue on the Nests page — a home nest's posts its relay refused
(docs/goal/ui/nests.md § Forward queue; the ruling — whose page, and why the
user's — is docs/goal/architecture/nest/private-mode.md § Post Forwarding).

A private home nest forwards each new post to its paired public relay through
an outbox that retries a refusal for ever. Before this surface, a refused post
was visible only in the home nest's log — a state only the user can fix (the
relay-side permission is their own link action), announced nowhere the user
looks. The Nests page now says, at the top, how many of their posts are waiting
to reach their relay and why the last attempt failed, and offers *Retry now*
and *Stop forwarding these*.

Topology: two locally-built nests, started DIRECTLY through `common.start_nest`
because both need boot seeds no per-nest start option carries — the relay's
`paired_only` submission policy (`config/test-public-paired-only.toml`) and the
home nest's pre-claim private NAT seed (`config/test-private-paired.toml`, the
same seed `tests/api/test_namespace_sync.py`'s `paired` fixture uses). Where
the home nest forwards, and whether it forwards at all, is NOT boot wiring: it
is the user's own pairing row on the home nest, carrying the relay's URL and
`post_forward` (`private-mode.md` § Implementation status today, ruled
2026-10-01). The fixture records that home-side half of the user's link with
`fauna.pair.add`; the relay-side half is what the journey's one-action link
adds. So this module is standalone-only: it requests `nest_binary`, which
docker and live refuse, and the mode axis' closure rule excludes it there.

The app is logged into the HOME nest — the nest whose queue this is — as a
user registered on both nests, so the one-action link from the Nests page can
authenticate to the relay too.

Every wait is a deadline poll on latent state (convention 14): the queue block
on a re-entered page (tui hydrates on the nav edge), and the relay's own local
feed for the delivered post. No sleep stands in for the outbox worker's pass.

tier_3 (two real `fauna-nest` binaries, the real federation channel between
them, the real app). tui leads; the other six apps paint the page but do not
yet render the block — gated in-body as `skip_unbuilt` (convention 7), never
silently omitted; each app's trickle-down flips its gate.
"""
from __future__ import annotations

import subprocess

import pytest

from common import register_user, start_nest
from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from tests.api import ws_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
]

# The apps whose Nests page renders `nests-forward-*`. Each trickle-down adds
# itself here.
_DRIVING_APPS = {"tui", "linux", "web", "android", "macos", "ios"}

# The outbox worker polls every 5 s when idle and backs off 30 s after the
# first refusal, so the first attempt — and with it the recorded reason — lands
# within seconds of the post; delivery after the link is a nudge — due at once,
# or at worst 60 s after the entry's last attempt (the re-arm's throttle) — plus
# one worker pass. Generous budgets, since the box is shared.
_FIRST_ATTEMPT_S = 60.0
_DELIVERY_S = 90.0


def _require_driving_app(driver):
    """Skip-with-a-reason on an app that paints the Nests page but not the
    forward-queue block yet."""
    if app_name(driver) not in _DRIVING_APPS:
        skip_unbuilt(
            driver,
            surface="nests forward-queue block",
            detail="the shared LinkedNestsSnapshot carries forward_queue and the "
            "two actions, but this app's Nests shell does not render "
            "nests-forward-* yet (ui/nests.md § Forward queue)",
        )


def _stop(nest):
    nest["proc"].terminate()
    try:
        nest["proc"].wait(timeout=10)
    except subprocess.TimeoutExpired:
        nest["proc"].kill()
        nest["proc"].wait()


@pytest.fixture
def home_behind_refusing_relay(nest_binary, tmp_path_factory):
    """A private HOME nest forwarding to a public RELAY that refuses it.

    The user's own row on the home nest names the relay and carries
    `post_forward` (the home-side half of a link — `fauna.pair.add` against the
    home nest, as a directional link from the app writes it), so the home nest
    forwards their posts there. The relay runs `paired_only`, and nobody has
    paired the home nest on it, so every forward is refused with
    `fauna.federation.forbidden` — the exact state the block exists for. The
    user is registered on both nests (the relay registration is what lets the
    page's one-action link authenticate there).
    """
    from conftest import _as_nest_handle, _make_user
    from drivers.port_util import find_free_port

    relay = start_nest(
        nest_binary, str(tmp_path_factory.mktemp("fq-relay")), find_free_port(),
        config_name="test-public-paired-only.toml",
    )
    home = None
    try:
        home = start_nest(
            nest_binary, str(tmp_path_factory.mktemp("fq-home")), find_free_port(),
            config_name="test-private-paired.toml",
        )
        home_h, relay_h = _as_nest_handle(home), _as_nest_handle(relay)
        user = _make_user(home_h)
        register_user(
            relay_h["port"], user["actor_id_hex"],
            admin_signing_key=relay_h["admin"]["signing_key"],
        )
        # The home-side half of the link: the user's own row on the home nest,
        # naming the relay's URL with the default self-sync set (`post_forward`
        # included). This is what makes the home nest forward their posts.
        ws_api.add_pairing(
            home_h["port"], user, bytes.fromhex(relay["nest_id"]),
            nest_url=relay_h["peer_url"],
        )
        yield {"home": home_h, "relay": relay_h, "user": user}
    finally:
        if home is not None:
            _stop(home)
        _stop(relay)


def _sign_in_to_home(app, request, world):
    from conftest import _login_app_as

    _login_app_as(app, request, world["home"], world["user"], verify_live_actor=True)


def _queue_on_reentered_page(app):
    """Re-enter the Nests page and read the block: (summary, reason).

    The page hydrates on the nav edge, so a poll that stays on the page would
    keep reading the frame it landed on; re-navigating each round is what makes
    the read latent-state rather than first-paint.
    """
    app.linked_nests.navigate()
    assert app.linked_nests.is_page_visible(), (
        f"Nests page unreachable. error: {app.error_text()!r}"
    )
    return app.linked_nests.forward_queue_text(), app.linked_nests.forward_queue_reason()


def _relay_holds_post(world, text) -> bool:
    posts = ws_api.local_feed_posts(world["relay"]["port"], world["user"], search=text)
    return any(text in (p.get("body") or p.get("text") or "") for p in posts)


@pytest.mark.feature("nests-and-trust")
def test_a_refused_forward_shows_on_the_nests_page_and_linking_the_relay_delivers_it(
    app, request, home_behind_refusing_relay
):
    """Post on the home nest → the relay refuses it → the Nests page says one
    post is waiting and why → Retry now round-trips without error and changes
    nothing (the relay still refuses) → linking the relay FROM THIS PAGE grants
    forwarding on the relay's side and nudges the queue → the post reaches the
    relay's own feed and the block disappears."""
    _require_driving_app(app.driver)
    world = home_behind_refusing_relay
    _sign_in_to_home(app, request, world)

    text = "forward me to the relay"
    app.feed.navigate()
    app.feed.create_post(text)

    # 1. The queue shows the post, and — once the worker has tried — the
    #    relay's own refusal as the reason. Both are read off a re-entered page.
    summary, reason = wait_until(
        lambda: (lambda sr: sr if sr[0] and sr[1] else None)(
            _queue_on_reentered_page(app)
        ),
        _FIRST_ATTEMPT_S,
        diagnose=lambda: (
            f"last read: queue={app.linked_nests.forward_queue_text()!r} "
            f"reason={app.linked_nests.forward_queue_reason()!r} "
            f"error={app.error_text()!r}"
        ),
    )
    assert "1" in summary, f"one post queued, page says {summary!r}"
    assert "forbidden" in reason.lower() or "not paired" in reason.lower(), (
        f"the reason must be the relay's own refusal, got {reason!r}"
    )

    # 2. Retry now: the action round-trips (no error), and the relay still
    #    refuses, so the post is still waiting on the next re-entered page.
    app.linked_nests.retry_forwards()
    assert not app.linked_nests.page_error_text(timeout=2.0), (
        "Retry now must not surface an error"
    )
    summary, _ = _queue_on_reentered_page(app)
    assert summary and "1" in summary, (
        f"retrying against a still-refusing relay keeps the post queued; page says {summary!r}"
    )

    # 3. Link the relay from here. The one-action link writes the reciprocal
    #    pairing rows with the full self-sync set — `post_forward` included —
    #    on both nests, and the home nest's `fauna.pair.add` re-arms this
    #    user's queue, so the very next worker pass delivers.
    app.linked_nests.link(world["relay"]["url"])
    assert app.linked_nests.wait_for_pairing_count(1, timeout=20.0), (
        f"linking the relay should add one pairing. error: "
        f"{app.linked_nests.page_error_text()!r}"
    )

    wait_until(
        lambda: _relay_holds_post(world, text),
        _DELIVERY_S,
        diagnose=lambda: (
            f"relay never received the post; home page reads "
            f"queue={_queue_on_reentered_page(app)!r}"
        ),
    )
    wait_until(
        lambda: _queue_on_reentered_page(app)[0] is None,
        _DELIVERY_S,
        diagnose=lambda: f"block still painted: {app.linked_nests.forward_queue_text()!r}",
    )


@pytest.mark.feature("nests-and-trust")
def test_stop_forwarding_drops_the_queue_and_keeps_the_post(
    app, request, home_behind_refusing_relay
):
    """Post → refused → *Stop forwarding these* → the block is gone on the
    next re-entered page, the relay never receives the post, and the post is
    still on the home nest's own feed (only its relay left)."""
    _require_driving_app(app.driver)
    world = home_behind_refusing_relay
    _sign_in_to_home(app, request, world)

    text = "keep me at home"
    app.feed.navigate()
    app.feed.create_post(text)

    wait_until(
        lambda: _queue_on_reentered_page(app)[0],
        _FIRST_ATTEMPT_S,
        diagnose=lambda: f"queue never painted; error={app.error_text()!r}",
    )
    app.linked_nests.discard_forwards()
    assert not app.linked_nests.page_error_text(timeout=2.0), (
        "Stop forwarding these must not surface an error"
    )
    wait_until(
        lambda: _queue_on_reentered_page(app)[0] is None,
        30.0,
        diagnose=lambda: f"block still painted: {app.linked_nests.forward_queue_text()!r}",
    )

    # The post itself stayed on the home nest — the relay copy is all that left.
    home_posts = ws_api.local_feed_posts(world["home"]["port"], world["user"], search=text)
    assert any(text in (p.get("body") or p.get("text") or "") for p in home_posts), (
        f"discarding the forward must not touch the post; home feed: {home_posts!r}"
    )
    assert not _relay_holds_post(world, text), "a discarded forward never reaches the relay"
