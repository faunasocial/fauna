"""tier_3 baseline guard — a connected Fauna app survives a BENIGN nest flip.

A *benign flip* is the Watchtower redeploy case: the nest process is replaced
(new image) on the SAME ``/data`` volume, so the nest identity
(``nest_deployment.key``), SQLite DB, blobs, storage mode, and the admin claim all
persist; only in-memory state — the bearer ``token_store`` and the per-connection
push ``seq``/registry — is wiped. This is the seamless path the reconnect
machinery is supposed to make invisible, as opposed to a *factory reset* (identity
rotation, deliberately NOT seamless — ``test_factory_reset_*``).

There was no standalone e2e guarding the "What already works" baseline that the
graceful-redeploy work *assumes*: auto-reconnect forever, silent bearer re-mint,
in-gap requests wait (don't error), live pushes resume, and never bouncing back
to onboarding. The only nest-restart-with-a-client tests were factory-reset /
re-onboard (identity rotation). This is that missing guard — and the shared
restart harness the graceful-shutdown (clean-1001) and reconnect re-hydrate tracks
EXTEND with their own red-until-fixed assertions (don't fork a second harness).

The flip is driven by ``common.nest.restart_nest``, which SIGTERMs the fixture's
own nest proc (→ the graceful WS-1001 + drain path) and re-spawns the same binary
on the same data dir — only the fixture's own ``proc`` handle is signalled, never
``pkill``/name-match (an internal safety rule — signalling by process name risks
killing unrelated processes sharing the machine).

Authority — ``docs/goal/architecture/transport.md``:
  * § Close codes — ``1000`` stops the reconnect loop, ``1001`` reconnects with backoff.
  * § Graceful shutdown — SIGTERM → broadcast 1001 → drain → flush → exit (never 1000).
  * § Connection lifecycle — token_store is in-memory, wiped on restart; the cached
    bearer is 401'd at the next WS upgrade → ``connect_error_is_auth_rejection`` →
    silent ``clear_token()`` → ``ensure_auth()`` re-mint from the persisted key.
  * § Request lifecycle step 3 — a request issued while the WS is down *waits*
    (bounded by the deadline) for reconnect, ``was_in_flight:false``, rather than
    erroring — safe for every kind.
  * § Push events — reconnect resets ``seq`` to 0; observers re-pull via their
    snapshot-refresh path; the long-lived PushBroker re-bridges onto the new socket.
The benign-flip-vs-factory-reset distinction is in
``docs/goal/architecture/nest/common.md`` § Client-state recoverability (the
WONTFIX is *factory reset* only; a benign redeploy IS seamless and this asserts it).

Coverage note: the in-gap *read*-waits property at the client-crate level is
additionally proven against a mock by
``libs/fauna-client/tests/reconnect_resume.rs::request_issued_while_disconnected_waits_for_reconnect``;
here it is exercised end-to-end as the in-gap *write* (the post composed
immediately after the flip).

Client coverage: GREEN on native (linux) AND web. Web reaching green required a
shared-Rust fix (Track W): the wasm ``run_reconnect_loop``
(``libs/fauna-rpc-wasm``) now re-mints the bearer on every post-connection
reconnect, because the browser WebSocket API hides the WS-upgrade 401 that
native's ``connect_error_is_auth_rejection`` keys on — without it a benign flip
(which wipes the nest's in-memory ``token_store``) left the web app looping
forever on the stale bearer.
"""
from __future__ import annotations

import secrets
import time

import pytest
import requests

from common.nest import restart_nest, start_nest_in_place, stop_nest
from helpers.waiting import await_feed_reload_after, feed_reload_baseline

# NOT `standalone_only` any more (2026-08-29). The flip helpers used to re-spawn
# the handle's own `node_binary` against its `config_path` — both declared absent in
# docker mode — so the whole module opted out. Now the provider that knows how to
# START a nest also answers how to start it AGAIN (`start_in_place`), and a docker
# flip is `docker stop` + `docker start` on the same container: same port, same
# `/data` bind-mount, fresh process tree under the image's own s6. That is a
# STRICTLY better witness of the benign flip than standalone's re-spawn, because it
# is the supervision production actually runs.
#
# What stays excluded is left to the INFERRED classifiers rather than restated here
# (testing.md § Default app and nest mode — eligibility is classified exclusion, not
# opt-in): `test_nest_flip_resilience` drives `/api/v1/test/push/notify` and so is
# class (6) — the test-hooks surface a release artifact does not compile — and the
# call-graph scan sees that by itself. A module-level marker would have hidden the
# two tests that CAN witness the flip on the real artifact behind the one that
# cannot.
pytestmark = [pytest.mark.tier_3]

# --- Load-robust wait ceilings (deliberately generous — do NOT tighten) -------
# Every assertion below checks a *latency-independent* property: "eventually
# returns to Connected", "the write issued during the reconnect gap LANDS
# instead of erroring", "a nest push re-bridges onto the new socket". A real
# regression — bounces to onboarding, never re-mints the bearer, the reconnect
# arm never sweeps the rail — fails at ANY ceiling; only load-induced *slowness*
# can blow a tight one. And a false red here is byte-for-byte indistinguishable
# from a real reconnect regression: it is the exact "load flakiness vs. real
# transport bug" misattribution this track burned four wrong root-cause theories
# on.
#
# So the ceilings are sized well above any non-pathological reconnect, which
# lets this test give a TRUSTWORTHY verdict UNDER machine load rather than
# needing a scarce ~solo window (holding up the fleet is a worse trade than a
# slightly slower failure). The extra time is spent ONLY on a genuine failure
# (it waits longer before failing, still bounded by the 900s pytest-timeout);
# the happy path returns the instant the condition holds, so a passing run stays
# fast. Latency itself is deliberately NOT asserted here (the reconnect
# backoff is full-jittered by design, so "latency under a flap" is a
# distribution, not a bound — the weakest possible instrument on this track).
CONNECT_WAIT_S = 90.0      # return to 'Connected' after a benign flip
DISCONNECT_WAIT_S = 60.0   # leave 'Connected' while the nest is held down
INGAP_WRITE_WAIT_S = 90.0  # a post composed during the reconnect gap lands
PUSH_WAIT_S = 45.0         # a nest push re-bridges onto the new connection
REHYDRATE_WAIT_S = 300.0   # the reconnect re-fetch COMMITS a verdict.
# Sized for the CHAIN it rides, not one event: the jittered reconnect backoff +
# silent re-mint come first, and an app's reconnect resync may run several
# sequential prerequisite RPCs before the feed re-query itself starts (tui:
# refresh_feeds + own-tiers, then the reload) — each link load-priced.
#
# The ceiling is derived from the chain's OWN bound rather than guessed: every
# link is an RPC carrying `fauna_rpc_wasm::DEFAULT_DEADLINE` (30 s), and one
# reload is `load_sealed_scorers` (one sequential `model_fetch` per composition
# entry, `fauna-feed/src/manager.rs:1113`) followed by `fetch_page` — so a
# healthy-but-fully-load-priced reload can legitimately span several of those
# 30 s links back to back, and the barrier waits for a COMMIT, which a
# superseded reload never reaches (the next one's commit is what lands).
#
# Two measurements walked it up, both the same shape — the mechanism running,
# the budget expiring mid-chain — never a stuck trigger:
#   * tui  @ 90 s: reloads (4, 3) against baseline 3 (2026-08-22, double-digit load)
#   * web  @ 180 s: reloads (5, 3) against baseline 3 (2026-08-22, build queue 9 deep)
# web's chain is the longer one, so it prices above tui's. A THIRD failure at
# this ceiling should NOT walk the number again: `await_feed_reload_after` now
# reports how long the newest reload has been in flight, which separates "the
# budget expired mid-chain" (raise) from "a reload is parked far past the 30 s
# link bound" (a stall to root-cause). Read that line before touching this one.
RENDER_WAIT_S = 90.0       # ...and the committed result RENDERS the missed post.
# Deliberately its own, smaller ceiling: it starts only after the barrier above
# proved a post-flip reload committed, so the server round trip is already paid
# and what remains is a snapshot read + a frame. Sharing REHYDRATE_WAIT_S made
# the worst case two full chain budgets for no reason, against a 900 s
# pytest-timeout that also has to cover launch, login and the nest restart.


def _connection_status(app) -> str | None:
    """Linux's global ``connection-status`` indicator text, or ``None`` where the
    element isn't implemented yet (tracked internally, covers
    the lift to the other 5 clients). Callers fall back to functional proof of
    reconnection (a round-tripping post) where the indicator is absent."""
    if app.is_visible("connection-status"):
        return app.get_text("connection-status")
    return None


def _wait_connected(app, timeout: float = CONNECT_WAIT_S) -> str | None:
    """Poll the connection indicator until it reads 'Connected'. Returns the final
    text, or ``None`` where the indicator isn't implemented."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _connection_status(app)
        if last is None or last == "Connected":
            return last
        time.sleep(0.3)
    return last


def _wait_not_connected(app, timeout: float = DISCONNECT_WAIT_S) -> str | None:
    """Poll the connection indicator until it reads something OTHER than
    'Connected' (the flip while the nest is down), or time out. Returns the final
    text, or ``None`` where the indicator isn't implemented."""
    deadline = time.monotonic() + timeout
    last = _connection_status(app)
    while time.monotonic() < deadline:
        last = _connection_status(app)
        if last is None or last != "Connected":
            return last
        time.sleep(0.2)
    return last


def _feed_bodies(app) -> list[str]:
    """All visible post bodies via the cross-app feed accessors (the web state
    shape differs from native, so read through the FeedActions abstraction rather
    than the native state protocol)."""
    return [app.feed.post_text(i) for i in range(app.feed.post_count())]


@pytest.mark.feature("connect-and-sign-in", "notifications")
def test_nest_flip_resilience(logged_in_app, nest_instance, test_user):
    """A connected client survives a benign nest flip with no re-onboarding."""
    app = logged_in_app

    # --- Baseline (pre-flip): logged in, on the feed, connection live. "Logged
    #     in" is asserted via cross-app UI surfaces (feed present, onboarding
    #     absent), NOT the native state protocol — the web bridge's state shape
    #     differs and has no `session.authenticated`. ---
    assert app.feed.is_visible(), "should start logged in on the feed"
    assert app.is_absent("create-identity-button"), "should start past onboarding"
    pre_status = _wait_connected(app)
    assert pre_status in (None, "Connected"), f"expected Connected pre-flip, got {pre_status!r}"

    before_text = f"flip-baseline-{int(time.monotonic() * 1000)}"
    # Budget threaded into the action call (convention 14's INGAP rider): the
    # default 20 s has repeatedly expired on this file's BASELINE composes
    # under load, with no flip involved (2026-08-22 web: submit accepted,
    # 0 posts rendered at 20 s on a saturated box).
    app.feed.create_post(before_text, timeout=INGAP_WRITE_WAIT_S)
    assert before_text in app.feed.first_post_text(), "baseline post did not land pre-flip"

    # --- The benign flip: SIGTERM the nest (graceful 1001 + drain), respawn on the
    #     SAME data dir. `restart_nest` blocks for seconds (graceful drain + new
    #     boot), so the client has already received the 1001 and is reconnecting by
    #     the time it returns; the new nest is healthy but the CLIENT is still
    #     mid-reconnect — exactly the in-gap window under test. ---
    restart_nest(nest_instance, graceful=True)

    # --- (2)+(3) Silent re-mint + in-gap WRITE waits-then-succeeds. Issue a
    #     nest-bound write right after the flip, taking no manual reconnect action.
    #     A write needs a re-authenticated connection, so its success proves the
    #     bearer was silently re-minted (token_store wiped → 401 at the upgrade →
    #     re-mint from the persisted key) AND that a request issued during the gap
    #     waited for reconnect rather than erroring (Request lifecycle step 3). ---
    after_text = f"flip-after-{int(time.monotonic() * 1000)}"
    app.feed.create_post(after_text, timeout=INGAP_WRITE_WAIT_S)
    assert after_text in app.feed.first_post_text(), (
        "a post composed right after the flip never landed — the client did not "
        "silently re-mint + reconnect, or the in-gap write errored instead of waiting"
    )

    # --- (2) The pre-flip post persisted across the swap: it lives in the SQLite
    #     DB on the data dir, and the post-flip feed reload re-pulled it. ---
    assert any(before_text in b for b in _feed_bodies(app)), (
        "the pre-flip post was lost across the flip — the data dir did not persist "
        "or the feed never re-hydrated from the restarted nest"
    )

    # --- (1) The client reconnected to 'Connected' (where the indicator exists;
    #     elsewhere the round-tripping post above is the functional proof). ---
    final_status = _wait_connected(app)
    assert final_status in (None, "Connected"), (
        f"client did not return to Connected after the flip (indicator={final_status!r})"
    )

    # --- (5) Never bounced back to onboarding/claim: still on the feed, no
    #     create-identity surface (cross-app UI proof the session survived). ---
    assert app.feed.is_visible(), "feed surface lost after the flip"
    assert app.is_absent("create-identity-button"), (
        "client fell back to the onboarding surface after the flip"
    )

    # --- (4) Live pushes resume: the long-lived PushBroker re-bridges onto the new
    #     connection. Fire a nest-side push AFTER reconnect and assert it is
    #     delivered live (no manual refresh). The app sits on the FEED, so this
    #     also asserts the unread count is app-global, not page-scoped
    #     (notifications.md § Architectural rules, rule 4). Gated on the app
    #     exposing `data.notifications` — where absent, the post round trips
    #     above already prove the live socket resumed. ---
    state = app.driver.get_state() or {}
    notifications = (state.get("data") or {}).get("notifications")
    if notifications is not None:
        before_unread = notifications.get("unread_count", 0)
        resp = requests.post(
            f"{nest_instance['url']}/api/v1/test/push/notify",
            json={
                "actor_id": test_user["actor_id_hex"],
                "summary": "post-flip push probe",
                "notif_type": "test",
            },
            # Generous (load-robust): this fires the push via a nest test-hook;
            # a healthy nest answers in <1s, but under machine load a fixed-10s
            # HTTP ceiling is one more avoidable false-error point.
            timeout=45,
        )
        assert resp.status_code == 200, (
            f"test-hook notify failed after the flip: {resp.status_code} {resp.text}"
        )
        deadline = time.monotonic() + PUSH_WAIT_S
        seen = before_unread
        while time.monotonic() < deadline:
            data = (app.driver.get_state() or {}).get("data") or {}
            seen = (data.get("notifications") or {}).get("unread_count", before_unread)
            if seen > before_unread:
                break
            time.sleep(0.2)
        assert seen > before_unread, (
            f"a nest push fired after the flip never reached the client "
            f"(unread {before_unread}→{seen}) — either the PushBroker did not re-bridge "
            f"on reconnect, or the app's unread count is page-scoped rather than "
            f"app-global (notifications.md § Architectural rules, rule 4)"
        )


def _inject_post_as(nest, user, body_text) -> str:
    """Create a post as ``user`` over WS-RPC, server-side — NOT via the GTK client.

    Mirrors a post that arrived on another device while this client was
    disconnected. There is no feed-post push event (transport.md § Push events
    lists none), so the only way this reaches the client is a feed re-fetch.
    Returns the hex post_id. Uses the same `sign_and_encode_post` + `fauna.posts.create`
    path as the api/* tests."""
    import time as _t
    from tests.api import ws_api
    from tests.api.bare import sign_and_encode_post

    now_us = int(_t.time() * 1_000_000)
    post_bytes = sign_and_encode_post(
        user["signing_key"], now_us, body_text, tags=[]
    )
    return ws_api.create_post(nest["port"], user, post_bytes)


@pytest.mark.feature("connect-and-sign-in")
@pytest.mark.feature("feed-read")
def test_nest_flip_feed_rehydrate(logged_in_app, nest_instance, test_user):
    """The feed RE-HYDRATES on reconnect: a post that arrived while the client was
    disconnected appears after reconnect with NO manual refresh.

    This is the Track-2 property `transport.md` § Push events mandates —
    "application observers re-pull through their snapshot-refresh path" on
    reconnect — for the one surface that has neither a poll backstop nor (before
    Track 2) a reconnect re-fetch: the feed. The baseline guard above only proves
    a client-*composed* post survives (composing re-fetches anyway); this proves a
    post the client never composed shows up.

    Mechanism attribution rides the shared `feed_reloads` triple
    (`fauna_e2e_agent::FEED_RELOADS_KEY`): `started` is read before the flip,
    and afterwards the test waits for `committed_gen` to pass that baseline —
    generations are claimed at the reload's first statement, so a committed
    generation past the baseline is exactly "a re-query that BEGAN after the
    flip has landed its verdict", observed directly.

    ⚠ It waited on `completed` until 2026-08-23, which was UNSOUND and made
    this test unable to pass on web at all: a superseded reload never commits,
    the reconnect fires two overlapping reloads by construction, and the
    resulting permanent `started - completed` gap put the old release condition
    permanently out of reach. Web spent two sessions and a 300 s budget on it
    while the feature under test worked the whole time.
    Do not reintroduce a count-based wait here. The
    original design instead asserted the probe was NOT visible after a 2 s
    settle-sleep, to prove no other delivery path could explain its later
    appearance — an absence premise any legitimate concurrent refresh could
    invalidate (on web, the baseline compose's own async refresh landing after
    the injection did exactly that in the 2026-07-24 solo sweep), and the
    settle-sleep negative-assert shape convention 14 bans outright
    (`e2e-latency-independent-assertions.md` § The convention).

    Lead app: linux. All 7 apps have since landed the Track-2 fan-out
    (windows, web, macos/ios, android) — GREEN on every app, no more xfail. tui landed it last
    (`session.rs` subscribe_reconnects pump + `app.rs` `Resync::on_reconnect`).
    `feed_reloads` legs are landed on all 7 apps — the last three followed the
    first three as one-line reads of the shared derivation (android 2026-08-22,
    apple 2026-08-25, windows 2026-08-29). An app that ever loses
    the leg refuses loudly at the barrier (convention 11) rather than reading as
    "no re-query landed", and the refusal names the fix.
    """
    app = logged_in_app

    assert app.feed.is_visible(), "should start on the feed"
    # Let the connection settle before composing (mirrors test_nest_flip_resilience's
    # baseline): on a cold-started browser the SPA's launch-machine / wasm-establish
    # sequence can still be settling even though feed-view already renders, and the
    # baseline create_post's default 20s budget has nothing absorbing that latency —
    # this bit the test twice under load with no reconnect involved at all (the
    # timeout landed on THIS line, before any flip). _wait_connected has its own
    # generous ceiling, so this only costs time on a genuinely slow boot.
    _wait_connected(app)
    # Populate the feed once so the re-fetch updates an already-rendered view
    # (`create_post` returns only once the post is visible). Budget threaded in
    # (convention 14's INGAP rider): the 20 s default expired here on web under
    # load with the submit accepted and 0 posts rendered (2026-08-22) — the
    # docstring's "bit the test twice" latency class, no flip involved.
    app.feed.create_post(
        f"rehydrate-baseline-{int(time.monotonic() * 1000)}",
        timeout=INGAP_WRITE_WAIT_S,
    )

    # Inject a post as test_user via the server-side WS-RPC path — the app under
    # test never composes it, mirroring a post that arrived from another device
    # while this client wasn't looking. Confirm it landed server-side so a later
    # absence can ONLY mean "the client never re-fetched", not "the injection
    # failed". (With no feed-post push and no feed poll it ordinarily sits
    # nest-side unseen until the flip — but the test no longer DEPENDS on that:
    # attribution is the reload counter below, not the absence of every other
    # delivery path.)
    probe = f"rehydrate-probe-{int(time.monotonic() * 1000)}"
    post_id = _inject_post_as(nest_instance, test_user, probe)
    assert post_id, "server-side post injection did not return a post_id"

    # The probe's pre-flip visibility is READ (never asserted) purely as
    # diagnostics — a legitimate in-flight refresh may deliver it early, which
    # invalidates nothing below.
    preflip_visible = any(probe in body for body in _feed_bodies(app))

    # The causal baseline: the newest reload generation as of RIGHT HERE, read
    # behind the app's own barrier so it cannot be a snapshot published before
    # the compose above (`feed_reload_baseline`'s docstring carries the windows
    # measurement that made this necessary — an un-anchored baseline let the
    # release condition be satisfied by the PRE-flip commit). Taken as late as
    # possible, after the last pre-flip UI read, so the window in which a
    # legitimate non-reconnect reload could satisfy the barrier is as small as
    # the test can make it.
    reloads_before = feed_reload_baseline(app.driver)

    # Flip the nest. The client drops (1001), reconnects against the fresh process,
    # and (with Track 2) re-fetches the feed — picking up the probe. No manual
    # refresh / compose / navigation. RED before Track 2, GREEN after.
    restart_nest(nest_instance, graceful=True)

    # Spend the reconnect under its own named ceiling first (where the
    # indicator exists; elsewhere this returns at once and the barrier's own
    # budget absorbs the backoff) — the re-query chain below can only begin
    # once the client is back on a live socket, and folding the jittered
    # backoff into the barrier's budget is how the 90 s ceiling expired
    # mid-chain on a healthy mechanism (the 2026-08-22 tui measurement above).
    _wait_connected(app)

    # (1) The MECHANISM ran: a feed re-query that began after the pre-flip
    # baseline committed its result — fails here, naming the trigger wiring,
    # if an app's reconnect-triggered re-fetch is gone.
    try:
        await_feed_reload_after(
            app.driver,
            reloads_before,
            budget_s=REHYDRATE_WAIT_S,
            what="the benign flip",
        )
    except AssertionError as barrier_failure:
        # The counters say no post-flip re-query committed. Two readings, and
        # the counters CANNOT separate them — but the rendered feed can, and it
        # is one read away (convention 6: diagnose at the failure, not on a
        # re-run):
        #   * probe ABSENT  -> the feed really did not re-hydrate. Combined with
        #     the in-flight age above, a reload parked past the chain's own RPC
        #     deadlines is a product stall to root-cause.
        #   * probe PRESENT -> the feed DID re-hydrate while its counters stood
        #     still, so the barrier is reading a DIFFERENT FeedManager than the
        #     one that acted (web's manager survives navigation). That is an
        #     instrumentation defect, and hunting the product for it would be a
        #     wasted cycle.
        #
        # This branch fired on web 2026-08-22 and again 2026-08-23, and BOTH
        # times the answer was PRESENT — but the cause was neither reading: the
        # barrier's own arithmetic could not be satisfied once a reload had been
        # superseded (fixed by `committed_gen`). That
        # confound is gone, so a PRESENT here now really does mean the
        # wrong-instance reading. Kept because the read costs one DOM query and
        # a wrong hypothesis costs a 300 s run.
        try:
            probe_rendered = any(probe in body for body in _feed_bodies(app))
            feed_error = app.error_text()
        except Exception as exc:  # never mask the barrier's own failure
            probe_rendered, feed_error = f"<unavailable: {exc}>", "<unavailable>"
        raise AssertionError(
            f"{barrier_failure}\n\nDISCRIMINATOR (read at the failure): the "
            f"probe is rendered: {probe_rendered}; feed error surface: "
            f"{feed_error!r}. Probe PRESENT with the counters standing still "
            "means the barrier is watching a different FeedManager instance "
            "than the one that re-queried — fix the instrumentation, not the "
            "product. Probe ABSENT means the re-hydrate genuinely did not "
            "happen."
        ) from barrier_failure

    # (2) The END-STATE converged: the missed post is rendered, no manual
    # action. (If a legitimate pre-flip refresh already delivered it, this
    # returns at once — the mechanism proof above carries the attribution.)
    deadline = time.monotonic() + RENDER_WAIT_S
    seen = False
    while time.monotonic() < deadline:
        if any(probe in body for body in _feed_bodies(app)):
            seen = True
            break
        time.sleep(0.5)
    if not seen:
        # Convention 6: the barrier above admits an ERRORED commit — `reload`
        # counts Ok and Err alike, because the pair's job is to prove the
        # mechanism RAN, not that it worked. So the single likeliest reading of
        # "committed, but the probe is not rendered" is a re-query that landed a
        # failure verdict, and the feed's own error surface says which
        # (`FeedManager::reload`'s Err arm stamps `feed.error_load`). Read it
        # here rather than leaving the next session to guess between "the fetch
        # failed" and "the fetch succeeded but dropped the post".
        try:
            feed_error = app.error_text()
        except Exception as exc:  # an app without the surface must not mask the real failure
            feed_error = f"<unavailable: {exc}>"
        # Convention 6, second read: an empty error surface still leaves TWO
        # readings the element view cannot separate — the fetch succeeded but its
        # result never carried the probe, or it carried it and the view never
        # painted it. The app's own published post list is the model the view
        # binds to, so reading it here splits them in one state query instead of
        # leaving the next session to instrument a 300 s run for the answer.
        try:
            model_bodies = app.feed.model_post_bodies()
            if model_bodies is None:
                model_note = (
                    "this app publishes no `data.feed.posts`, so the two cannot be "
                    "separated from here — add that key (convention 11) before hunting"
                )
            elif any(probe in body for body in model_bodies):
                model_note = (
                    f"PRESENT in the model ({len(model_bodies)} post(s)) but absent from "
                    "the rendered elements — the re-query DID deliver it and the VIEW "
                    "never painted it. Hunt the render (a virtualized row that never "
                    "realized, a reconcile key, a template), not the fetch"
                )
            else:
                model_note = (
                    f"ABSENT from the model too ({len(model_bodies)} post(s)) — the "
                    "committed re-query's own result never carried the probe. Hunt "
                    "upstream (what the fetch asked for and what came back), not the view"
                )
        except Exception as exc:  # diagnostics must never mask the real failure
            model_note = f"<unavailable: {exc}>"
        assert seen, (
            "a post the client had not yet seen never appeared after reconnect — "
            "the feed did not re-hydrate. A post-flip re-query COMMITTED (the "
            "barrier above proved it) but its result never rendered the probe. "
            f"Feed error surface: {feed_error!r} — a `feed.error_load` here means "
            "the re-query committed an ERROR verdict (the fetch failed; waiting "
            "longer would not have helped, so do not raise RENDER_WAIT_S for it). "
            "An empty surface means the fetch succeeded and the probe was absent "
            "from its result or dropped on the way to the view. "
            f"DISCRIMINATOR (the app's own post list): {model_note}. "
            f"Probe visible pre-flip: {preflip_visible}"
        )


@pytest.mark.feature("offline-aware-controls")
def test_connection_status_flips_while_nest_down(logged_in_app, nest_instance):
    """The global ``connection-status`` indicator is DRIVEN by the live transport
    state: it reads 'Connected' after login, FLIPS off "Connected" while the nest
    is down, and returns to 'Connected' once it is back — with no manual action.

    This is the *visible* half of the reconnect machinery the resilience guards
    above prove functionally. Every app renders this indicator off its transport
    ``ConnectionState`` (linux/web/native via the shared
    ``NestClient::connection_state()`` watch — pumped to JS via
    ``setOnConnectionStateChanged`` and to the native apps via
    ``subscribe_connection_state``); a transient swap shows as "Connecting…",
    deliberately NOT as an error banner (transport.md § Connection lifecycle, and
    the user intent: a global indicator,
    never ``rpc disconnected`` error text).

    Unlike ``test_nest_flip_resilience`` (which only asserts the Connected
    endpoints, tolerating an absent indicator), this asserts the indicator
    actually LEAVES Connected during the gap — so it fails if the indicator is
    static / not wired to the connection-state subscription. Uses ``stop_nest`` +
    ``start_nest_in_place`` (NOT ``restart_nest``) so the nest stays DOWN long
    enough to observe the flip deterministically, rather than racing the client's
    reconnect. Structurally skipped on clients that don't render the indicator yet."""
    app = logged_in_app
    if not app.is_visible("connection-status"):
        pytest.skip(
            "client has no global connection-status indicator yet "
            ""
        )

    # --- Connected after login. ---
    assert _wait_connected(app) == "Connected", (
        "connection-status should read 'Connected' once logged in"
    )

    # --- Take the nest DOWN (graceful WS 1001) and leave it down: the client's
    #     reconnect loop now has nothing to connect to, so the indicator must
    #     leave 'Connected' (→ "Connecting…"/"Disconnected"). ---
    stop_nest(nest_instance, graceful=True)
    flipped = _wait_not_connected(app)
    assert flipped not in (None, "Connected"), (
        f"connection-status stayed 'Connected' while the nest was down (read "
        f"{flipped!r}) — the indicator is not driven by the live connection-state "
        "subscription"
    )

    # --- Bring the nest back on the SAME port + data dir: the client reconnects
    #     (silent bearer re-mint) and the indicator returns to 'Connected' with no
    #     manual action. ---
    start_nest_in_place(nest_instance)
    assert _wait_connected(app) == "Connected", (
        "connection-status did not return to 'Connected' after the nest came back "
        "— the reconnect transition is not reaching the indicator"
    )


def _ws_connections(admin_client) -> int:
    """The nest's live authenticated WS-RPC socket count (``fauna.admin.stats``
    ``ws_connections``), read through one admin socket of our own — counted
    once, consistently, on every read made through the same client. Mirrors
    ``tests/api/test_push_dispatch.py``'s ``_ws_connections``."""
    return int(admin_client.call("fauna.admin.stats", {})["ws_connections"])


def _admin_ws(nest):
    from nacl.signing import SigningKey
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    raw = nest["admin"]["signing_key"]
    sk = raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _land_on_media_loaded(app) -> None:
    """Enter the Media page and wait until its first nest read has LANDED —
    ``media-empty-state`` is painted only once the page has loaded and no set
    holds media, and a ``media-item`` row only once it has loaded and one does
    (ui.yaml `pages.media`: three states off one ID, no wall-clock inference)."""
    from helpers.budgets import RPC_ROUNDTRIP_S
    from helpers.waiting import wait_until

    app.media.navigate()
    wait_until(
        lambda: app.driver.is_visible("media-empty-state") or app.driver.count("media-item") > 0,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the Media page never finished its first read: "
            f"{app.driver.diagnose('media-empty-state')} error={app.error_text()!r}"
        ),
    )


def _build_the_page_machines(app) -> None:
    """Visit the pages whose machines used to dial sockets of their own on web
    (Folders/Devices, Media, Backups, the labeler catalog under
    Personalization), each to a state that proves its first nest read LANDED —
    so each machine's transport is provably live when the caller reads."""
    app.backups.navigate_folders()
    # Painted once the list load has settled (`test_folders_load_surfaces_no_error`).
    app.driver.wait_for("folder-add-button")
    _land_on_media_loaded(app)
    app.backups.navigate()
    app.driver.wait_for("backup-folder-selector")
    app.personalization.navigate()
    app.driver.wait_for("personalization-trained-factor-name-input")


# Web-only: the nest-side socket count is attributable to the APP's page
# machines only where the app process is the sole holder of the actor's
# sockets. Every native app runs a companion sync agent that holds sockets for
# the same actor on its own schedule, by design (transport.md § Implementation
# status today, the web one-socket entry) — measured 2026-09-25 on tui: 3 → 4
# with no set created, 3 → 7 with one. Deselected under non-web `--app` by the
# conftest marker-platform filter; the in-body `is_web()` declaration guards a
# no-`--app` run.
@pytest.mark.web
@pytest.mark.feature("connect-and-sign-in")
def test_page_machines_open_no_socket_of_their_own(logged_in_app, nest_instance):
    """Web's page machines ride the app's ONE authenticated socket —
    ``transport.md`` § Goal (*a single WebSocket per actor*), the web shape
    ``apps/web.md`` § Transport (the shared rpc port).

    Latency-independent (convention 14), read off the nest's own registry
    (``fauna.admin.stats`` → ``ws_connections``, the count
    ``test_push_dispatch.py`` reads the same way) rather than off a clock: the
    count with the app on the feed and the count after Folders, Media, Backups
    and Personalization have each LOADED must be EQUAL. Before 2026-09-25 web
    opened one ``WsRpcClient`` PER WASM CHUNK — each of those pages dialled its
    own socket the moment it built its machine, six extra sockets for one
    actor, each with its own jittered reconnect loop — so the two counts
    differed by the number of chunk machines built. No set is created between
    the reads: a create is not a page-machine act and (on the daemon-running
    apps) opens sockets of the daemon's.
    """
    from helpers.app_surface import declared_absence
    from helpers.connection import wait_until_online

    app = logged_in_app
    if not app.driver.is_web():
        declared_absence(
            app.driver,
            capability="a nest-side socket count attributable to the app's page machines",
            doc="transport.md § Implementation status today, the web one-socket "
            "entry (every native app runs a companion sync agent holding sockets "
            "for the same actor by design; their one-client shape is structural — "
            "one NestClient per session, apps/common.md § Nest Connection)",
        )
    assert app.feed.is_visible(), "should start logged in on the feed"
    wait_until_online(app.driver)

    with _admin_ws(nest_instance) as admin:
        # The baseline: the app's own socket with only the feed built, plus
        # this admin socket — counted once, on every read through `admin`.
        on_feed = _ws_connections(admin)
        _build_the_page_machines(app)
        with_pages = _ws_connections(admin)
        assert with_pages == on_feed, (
            f"the nest counts {with_pages} live socket(s) with Folders, Media, "
            f"Backups and Personalization built against {on_feed} on the feed "
            "alone — a page machine opened a socket of its own instead of riding "
            "the app's one connection (a wasm chunk built its WsRpcClient with "
            "WsRpcClient::connect instead of over the shared rpc port)"
        )


@pytest.mark.feature("connect-and-sign-in")
def test_a_folders_gesture_lands_the_moment_the_app_reads_online_after_a_flip(
    logged_in_app, nest_instance
):
    """A Folders gesture issued the instant the app reads online after a benign
    flip lands inside the gesture's ordinary budget, never ``not connected`` —
    the page's machine rides the socket the ``connection`` observable reports
    on (``transport-connection.md`` § Connection lifecycle, the one-loop-per-
    actor paragraph).

    With a chunk-private socket (web before 2026-09-25) the app's observable
    read online — the core singleton was back — while the Folders machine was
    still asleep in ITS backoff, up to the 60 s ceiling, and the create either
    stalled past the kind deadline or answered ``not connected``
    (). Over the shared port the request
    waits out nothing but that one socket's own reconnect, which
    ``wait_until_online`` has already seen complete. The gesture keeps its own
    budget (``FOLDER_CREATE_WAIT_S``): the assertion is that it SUCCEEDS with
    no error surfaced, not how fast (convention 14).

    Portable: every app has the pages, and the natives' one-client shape makes
    this hold structurally; an app whose leg does not publish the observable
    takes the gesture as its whole proof, as ``helpers/connection.py``
    documents.
    """
    from helpers.connection import wait_until_online

    app = logged_in_app
    assert app.feed.is_visible(), "should start logged in on the feed"
    wait_until_online(app.driver)
    # The machines exist BEFORE the flip, so it is their reconnect the
    # gesture below meets — the shape that failed, not a fresh build.
    _build_the_page_machines(app)

    # The benign flip; the app reconnects on its own. `restart_nest` returns
    # with the new nest healthy and the client mid-reconnect.
    restart_nest(nest_instance, graceful=True)
    wait_until_online(app.driver, timeout=CONNECT_WAIT_S)

    # The gesture, the moment the app reads online — no wait of its own beyond
    # the wizard's ordinary budget. `create_folder_via_wizard` polls for the
    # row under FOLDER_CREATE_WAIT_S and raises, naming every row it saw, if
    # the create never lands.
    app.backups.navigate_folders()
    app.driver.wait_for("folder-add-button")
    after_name = f"shared-socket-after-{secrets.token_hex(4)}"
    try:
        app.backups.create_folder_via_wizard(after_name)
    except (AssertionError, TimeoutError) as exc:
        raise AssertionError(
            f"a Folders create issued as soon as the app read online after the "
            f"flip did not land: {exc}. error={app.error_text()!r} — the Folders "
            "machine's transport was not the socket the `connection` observable "
            "reports on (a chunk-private socket still asleep in its own backoff)"
        ) from exc
    assert not app.has_error(), (
        f"the post-flip Folders create surfaced an error: {app.error_text()!r}"
    )
