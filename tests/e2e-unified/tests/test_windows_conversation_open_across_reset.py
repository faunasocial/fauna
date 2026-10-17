"""Windows regression: a conversation left open across a sign-out must not crash the app.

Root cause this pins (2026-10-09). The windows
``ConversationsPage`` renders off ``ServiceClients.ConvSession``'s manager when the
login built a conversations session, and otherwise falls back to
``ConversationsManagerHost.Instance`` — the host's OWN wrapper of the process-wide
manager. An actor change (``ResetForActorChange``, reached from every sign-out, the
e2e ``reset`` and an account switch) disposes exactly that wrapper, on the agent's poll
thread, BEFORE the frame navigates away. The page's ``Page_Unloaded`` then runs
``FlushActiveComposeDraft`` — it writes the open thread's half-written reply back into
the manager — against the disposed wrapper, and the ``ObjectDisposedException`` reaches
``Application.UnhandledException`` on the UI thread. The app dies, and the next command
(the next test's sign-in) is never acknowledged.

When the fallback is taken: the e2e login awaits its session build for 10 s
(``App.AwaitE2eConvSessionAsync``) and production swallows a failed build — so a loaded
box (measured: the 2026-10-08 feature run, the host at 24–30 of 32 cores) or a build
failure hands the page the host's wrapper. On a quiet box the build finishes inside the
bound, the page gets the session's own wrapper (which nothing disposes), and the crash
cannot happen — which is why a solo rerun of the red module was green three times.

This test makes the fallback deterministic instead of waiting for load. It holds the
nest's ``fauna.auth.challenge`` — the WS handshake the session build's first step
(``NestRpcClient.ConnectedAsync``) cannot get past — across the sign-in, and releases it
only once the sign-in is ACKNOWLEDGED: the acknowledgement comes after the post-action's
bounded await, and with the handshake parked the build cannot have finished, so the ack
itself proves the bound expired and MainPage got no session. (A held read is not enough:
``GetIdentityAsync`` gives up on a parked ``fauna.account.get`` after ~5 s and the build
then completes inside the bound — measured, the first draft of this test was vacuous.)

Windows-only: the session-or-host fallback and its bounded await are windows' own
``App.xaml.cs`` shape, as in ``test_second_login_live_clients.py``.
"""

import pytest

from conftest import _E2E_LOGIN_DEVICE_ID, _login_app_as, _relaunch_trusting_nest
from helpers.connection import wait_until_online
from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, wait_for_held_rpc

pytestmark = [pytest.mark.tier_3]

AUTH_CHALLENGE = "fauna.auth.challenge"


def _sign_in_with_the_handshake_held(app, nest_instance, user) -> None:
    """``_login_app_as``'s own ``set_state``, split around the hold: the release has to
    land between the acknowledgement and the wait for the connection."""
    port = nest_instance["port"]
    _relaunch_trusting_nest(app.driver, nest_instance)
    arm_rpc_hold(port, AUTH_CHALLENGE)
    try:
        app.driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "handle": user.get("handle") or "e2e-user",
                "secret_hex": user["signing_key"].encode().hex(),
                "actor_id": user["actor_id_hex"],
                "device_id": _E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # The precondition, proven rather than assumed: the handshake was really
        # parked when the sign-in was acknowledged.
        wait_for_held_rpc(
            port, AUTH_CHALLENGE,
            diagnose=lambda: f"state={app.driver.get_state()!r}",
        )
    finally:
        release_rpc_hold(port, AUTH_CHALLENGE)
    wait_until_online(app.driver)


@pytest.mark.windows
def test_a_conversation_left_open_across_a_reset_never_crashes_the_app(
    app, request, nest_instance, test_user
):
    """Sign in with no conversations session, open a conversation, reset, sign in
    again: the second sign-in must be acknowledged by a live app."""
    conv = app.conversations

    _sign_in_with_the_handshake_held(app, nest_instance, test_user)

    # An OPEN thread is what makes the page's unload write back into the manager.
    conv.navigate()
    conv.seed_own_message(
        "left open across a reset", recipient="bob-reset-open@self-nest.test"
    )

    # The sign-out: disposes the host's wrapper, then navigates the open page away.
    app.driver.reset()

    # The next sign-in is the observable. Pre-fix the page's unload has killed the app,
    # so this is never acknowledged; the driver's ack-timeout diagnostics then carry the
    # app's `[fauna] FATAL (UI thread)` report whole.
    _login_app_as(app, request, nest_instance, test_user)

    # …and the app that acknowledged it is the one still serving the conversations page
    # (`navigate` waits for the page's own `new-conversation-button`).
    conv.navigate()
