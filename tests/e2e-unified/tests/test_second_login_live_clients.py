"""Windows regression: a SECOND ``set_state`` login in one app process must leave
``MainPage`` bound to LIVE nest clients.

Root cause this pins (``App.xaml.cs``): the ``session`` block of a ``set_state`` calls
``DisposeNestClients()`` and builds a fresh ``ServiceClients``, queueing a ``MainPage``
hand-off that carries it. A sibling ``nav`` block in the SAME ``set_state`` used to
**overwrite** that queued post-action outright, and its own navigation is guarded by
``if (rootFrame.Content is not Views.MainPage)`` — false whenever MainPage is already in
the frame. So the fresh clients were never handed over: ``MainPage._clients`` (set once in
``OnNavigatedTo``) kept the very objects ``DisposeNestClients`` had just disposed, and the
next call through them died with::

    HTTP error: Cannot access a disposed object.
    Object name: 'System.Threading.SemaphoreSlim'   # NestRpcClient._connectGate

Every login call site sends ``session`` + ``nav`` in one ``set_state`` (conftest's
``_login_app_as``, ``admin_app``, ``login_as_nest_admin``, this file), so the overwrite
always won. What made the defect *look* rare is that ``driver.reset()`` navigates the frame
back to ``OnboardingPage`` — after a reset the guard is true and MainPage is rebuilt
anyway. It is a second login with **no intervening reset** that strands the disposed
clients, which is why the only suite that hit it in the wild was
``test_gated_post_compose.py`` (its subscriber leg re-logs in mid-test).

This test therefore reproduces it DETERMINISTICALLY with nothing injected: log in, prove
the feed works, log in a second time with no ``reset()``, and require the feed to still
load. Pre-fix the second feed load throws on the disposed client and the post never
renders; post-fix the ``session`` block's hand-off survives (the blocks now compose via
``PostActionChain.Then`` instead of clobbering) and MainPage rebinds to live clients.

The first test's *mechanism* is windows-specific (the composition of one ``set_state``'s
deferred post-actions is the windows ``TestAgent``/``App.xaml.cs`` shape), so it stays
``@pytest.mark.windows``.

⚠ This docstring used to end with "Linux/web rebuild their client on reassignment (Rust drops
the old one), so they have no equivalent hand-off to lose." **That claim was false**, and it
hid a worse bug for weeks. Linux never rebuilt anything on a second login: the session
patch's shell-build branch is gated on ``current_stack.borrow().is_none()``
(``main.rs``'s ``handle_test_command``), which is false whenever an authenticated window is
already mounted — so a second ``set_state`` naming a *different* actor updated the override,
the keyring and the account cache while the live ``FaunaClient``/``NestClient``/
``feed::host`` manager all stayed the PREVIOUS actor's. The command was
silently half-applied — exactly what e2e convention 11 forbids — and ``get_state`` then
reported the new actor while the app drove the old one, so
``test_gated_post_compose.py``'s subscriber leg read the AUTHOR's own custody and its
"subscriber sees only the teaser" assertion failed by *leaking the full body*, a
silently-wrong-data outcome rather than an honest error.

The second test below pins the missing half — a second login as a DIFFERENT actor must
switch the app's live session, not just its reported one — and is the cross-app contract
that claim wrongly waived.

⚠ A second claim here was also wrong and is corrected: this docstring used to add "Web had
the same hole one layer up (``getFeedManager()`` is a dumb singleton with no actor
comparison)." That singleton IS a real latent hole, but it was **not** web's cause — the
reset that closes it landed while web stayed red. Web's actual cause was one layer *further*
down: the session patch moved the legacy ``fauna_secret`` slot without moving the account
registry, so ``accountsBoot()``'s ``mirrorActiveToLegacy()`` healed the switch away and
reverted the identity, which then retired the incoming actor's WS client under a correctly
rebuilt feed manager (fixed; e2e convention 11's rule (c)). Naming a
plausible half-cause is how the web arm stayed open for three sessions — do not treat the
singleton note as the explanation.

**Web arm added 2026-07-30**, once ``test_settings.py::test_actor_id_visible[web]`` went
green: web's Status rail entry mapped to an empty-string sub-id where every other app
(linux/windows/macos/ios/android/tui) accepts the explicit id ``"status"``, so
``_navigate_subpage("status")`` never reached the Status sub-page and ``_live_actor_id``
read back ``''`` — unrelated to the actor-switch class this file pins (`settings.md` §
ui.yaml mapping; `actions/settings.py::_navigate_subpage`). Web's ``node_url`` must be the
SPA proxy (``spa_url`` — the session ``nest_instance`` this test's fixtures already use),
not the raw nest URL, mirroring ``test_gated_post_compose.py::_login_as``.

**Windows arm added 2026-08-03**. Windows is
architecturally immune to the registry-half-apply bug class linux/web hit: its ``session``
``set_state`` handler (``App.xaml.cs``) never consults the account registry to build the live
clients — it disposes the old ``NestRpcClient``/crypto and builds fresh ones directly from the
``secret_hex`` the command supplies (``_cryptoService.LoadFromSecret`` → ``new
NestRpcClient(nodeUrl, _cryptoService)``), and ``NestRpcClient.ConnectedAsync`` mints its
``FfiNestClient`` bearer from that same crypto's ``SecretBytes`` — never from
``RegistryLaunchPersistence``/``registry.Active()``. So there is no stale-registry pointer to
mint the wrong actor's bearer with. (A separate, narrower gap survives: the raw ``session``
patch also never updates ``App.ActiveActorHex`` — normally set only by ``StartMainAppAsync``,
the production login path — so a per-actor scoped reader keyed off it, e.g. ``App.Drafts``,
would stay pointed at the previous actor after a *test-only* second login. Out of scope for
this test, which asserts the live transport, not draft scoping.)
"""

import time

import pytest

from common.auth import create_actor_and_register
from helpers.e2e_session import (
    LIVE_ACTOR_SWITCH_WAIT_S,
    live_actor_id,
    wait_live_actor_id,
)

pytestmark = [pytest.mark.tier_3]

# Re-exported for this module's own readers; the budget (and the reasoning for
# it) now lives beside the barrier it paces, in `helpers/e2e_session.py`.
__all__ = ["LIVE_ACTOR_SWITCH_WAIT_S"]


def _login_as(app, request, nest, user, *, handle: str) -> None:
    """set_state login landing on the feed view — ``session`` + ``nav`` in ONE call,
    exactly as every real login call site sends it (that pairing is the trigger).

    For web the ``node_url`` must be the SPA proxy (``spa_url``), not the raw nest
    URL — the browser reaches the nest only through a CORS-adding proxy (conftest's
    ``_serve_spa_proxy`` note); native apps use the raw URL directly."""
    if app.driver.is_web():
        node_url = request.getfixturevalue("spa_url")
    else:
        node_url = nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-relogin",
        },
        "nav": {"stack": [{"view": "feed"}]},
    })
    time.sleep(1.5)


@pytest.mark.windows
def test_second_login_rebinds_main_page_to_live_clients(logged_in_app, request, nest_instance, test_user):
    """A second login with no intervening reset() must not strand disposed clients."""
    app = logged_in_app
    marker = f"relogin-guard-{int(time.time())}"

    # Login #1 (via the fixture) works — seed a post so the second login has something
    # nest-backed to read back. This also fails loudly if the baseline is already broken,
    # so a red here is never mistaken for the regression under test.
    app.feed.create_post(marker)
    assert app.feed.wait_for_post_text(marker), (
        f"baseline: the post authored under the FIRST login must render before the "
        f"re-login is exercised. error={app.error_text()!r}"
    )

    # The forcing function: a second set_state login into the SAME app process, with NO
    # driver.reset() in between, so MainPage is still the frame content.
    _login_as(app, request, nest_instance, test_user, handle="e2e-user")

    # MainPage must now hold live clients. Pre-fix its _clients.Rpc is disposed, so this
    # feed load throws (ObjectDisposedException on _connectGate) and nothing renders.
    loaded = app.feed.wait_for_post_text(marker, timeout_s=20.0)
    err = app.error_text() or ""
    assert "disposed" not in err.lower(), (
        f"a second login left MainPage bound to disposed nest clients — the "
        f"session block's ServiceClients hand-off was dropped: {err!r}"
    )
    assert loaded, (
        f"the feed must still load after a second login in the same app process "
        f"(MainPage must be rebound to the freshly built clients). "
        f"error={err!r} {app.driver.diagnose('post-card')}"
    )


# The live-actor read + its deadline poll moved to `helpers/e2e_session.py`
# 2026-08-18: `login_as` needs exactly this barrier (its own was reading the
# agent's `session_override` and so could never fail), and two copies of the
# "which surface actually knows the live identity" reasoning is precisely the
# drift that let the vacuous one survive. This module keeps the aliases so its
# call sites read unchanged.
_live_actor_id = live_actor_id
_wait_live_actor_id = wait_live_actor_id


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
# tui joined 2026-08-21 (the tui-excluding-marker audit): this is the SAME
# actor-switch class `test_family_notify_actor_switch.py` just caught on tui —
# `session.rs::apply_session_patch` called `establish()` straight from an
# authenticated->authenticated patch without `drop_authenticated_state()` first
# (fixed this session, `git log --grep 'the agent.s in-place session-patch
# never reset per-actor UI state'`) — so this test is now expected to pass.
@pytest.mark.tui
def test_second_login_as_a_different_actor_switches_the_live_session(
    logged_in_app, request, nest_instance, test_user
):
    """A second `set_state` login naming a DIFFERENT actor must switch the live session.

    The contract e2e convention 11 imposes: honour the command or fail loudly on
    `error-message` — never half-apply it. Pre-fix linux half-applied it silently (the
    override/keyring/account-cache became actor B while every live object stayed actor
    A), which is why `test_gated_post_compose.py`'s subscriber leg could "pass" the
    login and then leak the author's own unsealed body into the subscriber's card.

    Asserts latency-independent state, not timing: the observable is *which actor the
    live session reports*, polled to a generous budget.
    """
    app = logged_in_app

    # Baseline: the fixture's login is live and reports actor A. A red here is a broken
    # baseline, never the regression under test (e2e rule 6 — failures diagnose
    # themselves).
    first_hex = bytes(test_user["signing_key"].verify_key).hex()
    first_seen = _wait_live_actor_id(app, first_hex)
    assert first_hex.lower() in first_seen.lower(), (
        f"baseline: the FIRST login's actor must be live before the switch is "
        f"exercised; account-actor-id={first_seen!r} expected={first_hex!r} "
        f"error={app.error_text()!r}"
    )

    # Actor B: a second registered actor on the SAME nest, so the only thing changing
    # is the identity — not the nest, not the transport.
    other = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    other_hex = other["actor_id_hex"]
    assert other_hex.lower() != first_hex.lower(), "the two actors must differ"

    # The forcing function: a second login, no driver.reset() in between.
    _login_as(app, request, nest_instance, other, handle="e2e-second-actor")

    seen = _wait_live_actor_id(app, other_hex)
    assert other_hex.lower() in seen.lower(), (
        f"a second login as a different actor must switch the LIVE session, not only "
        f"the reported one: account-actor-id={seen!r} expected actor B={other_hex!r} "
        f"(actor A was {first_hex!r} — if the app is still showing A, the session "
        f"patch was silently half-applied). error={app.error_text()!r}"
    )

    # ── The identity flip is not enough: prove the rebuilt session's TRANSPORT works ──
    # `account-actor-id` above is populated from `DataMessage::AuthSuccess`, which fires
    # *before* `start_ws_rpc()`. So the assertion above proves the identity switched, not
    # that actor B can actually talk to the nest — and that gap hid a real second bug for a
    # full session: the account registry's `active` pointer stayed on actor A, so the
    # rebuilt `LaunchMachine` (fed by `RegistryLaunchPersistence::load_identity`, which
    # resolves `registry.active()`) minted A's bearer while `launch_authenticated` built the
    # client around B's keypair. The nest refuses that pairing at the WS handshake
    # (`security.md` § Cross-connection binding) and the reconnect supervisor retries the
    # `403 Forbidden` forever, so every WS-RPC call HANGS instead of failing. The only UI
    # symptom is "0 posts, error-message=''" — which reads as a product bug in whatever
    # feature test happens to hit it (it cost `test_gated_post_compose.py` exactly that).
    #
    # A full round trip is the observable, because B is a freshly registered actor: "the
    # feed is empty" is CORRECT for B, so only a write-then-read proves the transport.
    # Latency-independent (convention 14): a generous budget, deadline-polled, so a green
    # run pays only what it needs.
    # Back to the feed first: `_live_actor_id` above reads `account-actor-id` off the
    # Status sub-page, so the app is sitting in Settings right now and composing would
    # fail on a missing feed page rather than on the transport under test.
    marker = f"actor-b-live-{int(time.time())}"
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    try:
        app.feed.create_post(marker, timeout=LIVE_ACTOR_SWITCH_WAIT_S)
    except Exception as exc:  # noqa: BLE001 — re-raised with the diagnosis attached
        pytest.fail(
            f"actor B could not author after the switch, so the rebuilt session's WS-RPC "
            f"is not working: {exc!r}. error-message={app.error_text()!r}. Grep the app's "
            f"stderr for `403 Forbidden` on `ws connect failed` — that is the "
            f"bearer/keypair mismatch this assertion exists to catch."
        )
    assert app.feed.wait_for_post_text(marker, timeout_s=LIVE_ACTOR_SWITCH_WAIT_S), (
        f"actor B's own post never rendered after the switch, so the rebuilt session "
        f"cannot READ from the nest even though `account-actor-id` reports B "
        f"(the 403-forever handshake signature). error={app.error_text()!r} "
        f"{app.driver.diagnose('post-card')}"
    )
