"""An actor switch performed **while the conversations route is open** must rebind
that page to the incoming actor.

This is the conversations twin of the feed-page bug closed,
and it pins the same contract one layer up from the manager singleton:
``account-scoping.md`` § The scoping taxonomy → *The switch/sign-out isolation
contract* — "no account-scoped datum may be read or written by a session
authenticated as a different account; a switch that lets account B **render** or
modify account A's local state is a bug". In this SPA the account-scoped data that
survives a switch is not on disk — it is module singletons and, the part every
previous session missed, **component-local state on a route that does not remount**.

Why the conversations route is a blind spot, in the page's own words: its comment
states it "remounts on re-navigation to /app/conversations". That is true, and it is
exactly what hides the bug — an in-app switch is a **same-route ``goto``** (the test
agent's nav patch, ``web-bridge/agent.js``; production's switcher happens to
``location.assign``, so it is safe there only by luck), and a ``goto`` to the route
you are already on does *not* remount the component. So ``onMount``'s one-shot
``manager = await getConversationsManager()`` never runs again.

The resulting split is what this test drives at, and it is why the page fails in a
shape that reads like a product bug rather than a switch bug:

* the thread **list** renders from the ``conversationsSnapshot`` store, written by
  the module-level ``refreshConversations()`` — which follows the **module**
  manager, correctly rebuilt for the incoming actor. The list therefore looks right.
* the open thread's **messages** render from ``manager.threadDetail(selectedId)`` on
  the **component-local** manager — still the OUTGOING actor's instance. Every user
  action (select, compose, send) drives that same stale manager, so this is a
  write-side violation too, not only a stale read.

Pre-fix the observable is "actor B's own thread is listed but opens empty, with no
``error-message``" — indistinguishable from an empty thread, which is precisely how
this class has repeatedly been misread as a product bug (this class spent four
sessions on its feed-page sibling).

**Web-marked because the mechanism is web-shaped**, the same way this suite's
``test_second_login_rebinds_main_page_to_live_clients`` is ``@pytest.mark.windows``
for a windows-shaped one: the native shells tear down and rebuild their whole
authenticated shell on a switch, so their per-page state cannot outlive the actor.
The cross-app *contract* is pinned by
``test_second_login_live_clients.py::test_second_login_as_a_different_actor_switches_the_live_session``.
"""

import time

import pytest

from common.auth import create_actor_and_register

pytestmark = [pytest.mark.tier_3]

# Generous, latency-independent budget (convention 14): the switch resets the
# manager singletons and the page must rebuild against the incoming actor. Poll for
# the observable rather than sleeping a guess — a green run pays only what it needs.
ACTOR_SWITCH_WAIT_S = 45.0

ACTOR_A_SENDER = "alice-convswitch@self-nest.test"
ACTOR_A_BODY = "actor-A-thread-body-convswitch"
ACTOR_B_SENDER = "bob-convswitch@self-nest.test"
ACTOR_B_BODY = "actor-B-thread-body-convswitch"


def _switch_to(app, request, nest, user, *, handle: str) -> None:
    """``set_state`` login as ``user`` landing on the conversations view.

    The forcing function is the ``nav`` block naming the route the app is ALREADY
    on: SvelteKit resolves that to a same-route ``goto``, so the page does not
    remount and any actor-scoped state held on the component survives the switch.
    Navigating anywhere else (as every other switch test does, landing on ``feed``)
    unmounts this page and hides the bug entirely.

    Web's ``node_url`` must be the SPA proxy (``spa_url``), not the raw nest URL —
    the browser reaches the nest only through the CORS-adding proxy (conftest's
    ``_serve_spa_proxy``), mirroring ``test_second_login_live_clients._login_as``.
    """
    node_url = request.getfixturevalue("spa_url") if app.driver.is_web() else nest["url"]
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": bytes(user["signing_key"]).hex(),
            "handle": handle,
            "actor_id": user["actor_id_hex"],
            "device_id": "test-device-conv-switch",
        },
        "nav": {"stack": [{"view": "conversations"}]},
    })


def _wait_message_text(app, needle: str, timeout_s: float = ACTOR_SWITCH_WAIT_S) -> bool:
    """Deadline-poll until some ``dm-message-text`` bubble contains ``needle``."""
    d = app.driver
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            for i in range(d.count("dm-message-text")):
                if needle in d.get_text("dm-message-text", index=i):
                    return True
        except Exception:  # noqa: BLE001 — mid-render races are expected while polling
            pass
        time.sleep(0.25)
    return False


def _visible_message_texts(app) -> list[str]:
    """Every rendered bubble's text, for failure diagnosis (convention 6)."""
    d = app.driver
    try:
        return [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]
    except Exception:  # noqa: BLE001
        return []


@pytest.mark.web
@pytest.mark.feature("multiple-accounts")
def test_actor_switch_on_the_conversations_route_rebinds_the_page(
    logged_in_app, request, nest_instance, test_user
):
    """Switching actors while ON /app/conversations must rebind the page to actor B.

    Asserts latency-independent state (convention 14): the observable is *whose
    thread the page can open*, deadline-polled to a generous budget.
    """
    app = logged_in_app
    conv = app.conversations

    # ── Baseline: actor A's own thread opens and renders. ─────────────────────
    # A red here is a broken baseline, never the regression under test (convention 6).
    conv.navigate()
    conv.inject_and_open_thread(rail="FaunaMls", sender=ACTOR_A_SENDER, body=ACTOR_A_BODY)
    assert _wait_message_text(app, ACTOR_A_BODY, timeout_s=15.0), (
        f"baseline: actor A's own thread must open and render before the switch is "
        f"exercised. bubbles={_visible_message_texts(app)!r} "
        f"error={app.error_text()!r} {app.driver.diagnose('dm-message-text')}"
    )

    # ── Actor B: a second registered actor on the SAME nest, so the only thing
    # changing is the identity — not the nest, not the transport. ─────────────
    other = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    assert other["actor_id_hex"].lower() != test_user["actor_id_hex"].lower(), (
        "the two actors must differ"
    )

    # The forcing function: switch WITHOUT leaving the conversations route.
    _switch_to(app, request, nest_instance, other, handle="e2e-conv-switch-b")

    # ── Actor B's own thread must open and render. ────────────────────────────
    # The inject seam routes through `getConversationsManager()` (the module
    # singleton the agent reset, so it rebuilds as B) — so B's thread genuinely
    # exists and, pre-fix, is even LISTED. What fails pre-fix is opening it: the
    # component still holds A's manager, whose `threadDetail` does not know B's
    # thread id, so the detail pane renders nothing and no error is raised.
    conv.inject_and_open_thread(rail="FaunaMls", sender=ACTOR_B_SENDER, body=ACTOR_B_BODY)
    assert _wait_message_text(app, ACTOR_B_BODY), (
        f"actor B's own thread did not render after an actor switch performed while "
        f"the conversations route was open — the page is still bound to actor A's "
        f"ConversationsManager (the component-local `manager` built in a `onMount` "
        f"that never re-ran, because a switch is a same-route `goto` and the route "
        f"does not remount). bubbles={_visible_message_texts(app)!r} "
        f"error={app.error_text()!r} {app.driver.diagnose('dm-message-text')}"
    )

    # ── ...and actor A's content must be gone. ────────────────────────────────
    # The isolation contract's other direction: B may not RENDER A's local state.
    # Checked after B's own render lands, so this can never pass vacuously on a
    # page that simply has not painted yet.
    assert not any(ACTOR_A_BODY in text for text in _visible_message_texts(app)), (
        f"actor A's message is still rendered to actor B after the switch — the "
        f"switch/sign-out isolation contract (account-scoping.md) forbids account B "
        f"rendering account A's local state. bubbles={_visible_message_texts(app)!r}"
    )
