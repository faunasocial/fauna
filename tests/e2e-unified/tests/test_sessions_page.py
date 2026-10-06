"""Settings → Sessions: list → revoke one → sign out everywhere else, through
the app UI, with the nest's own session list as the outside witness.

Owner: ``docs/goal/ui/sessions.md`` (§ Layout & flow, § User actions, § Done
definition box 2 — the first tier_3 journey) over
``docs/goal/behavior/devices.md`` § Session Management.

Arrangement: a dedicated actor signs the app in, and the test mints two more
sessions for that actor over the wire (``fauna.auth.*`` with the actor's own
key, ``WsRpcAdminClient``) — two stranger sign-ins the page must list and cut.
Every act goes through the UI (convention 8); every verdict is read back from
the nest's ``fauna.sessions.list`` (convention 5). The page's painted
token ids (the app's ``settings.session_token_ids`` state) bridge a card index
to the nest's ids — a plain read of the held snapshot, never a timing guess.

⚠ Each witness read opens its own client and so mints a session of its own;
those ids accumulate in ``harness_ids`` and are never mistaken for the app's
(the ``test_onboarding_launch_routing_smoke`` case M lesson).
"""
from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The page's act → re-read → re-fold, plus the nest applying the revoke.
#: Generous by design (convention 14); a green run pays only real latency.
ACT_FOLDED_S = 60.0


def _require_sessions_page(driver) -> None:
    """Convention 7: tui is the lead app (built first); the other six follow as
    one batched trickle-down row."""
    if app_name(driver) != "tui":
        skip_unbuilt(
            driver,
            surface="sessions page",
            detail="tui leads; this app renders the shared sessions_view fold in its trickle-down leg",
            tracked="docs/goal/ui/sessions.md § Implementation status today",
        )


def _client(nest_url: str, user: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest_url, bytes.fromhex(user["actor_id_hex"]), bytes(user["signing_key"])
    )


def _mint_stranger_session(nest_url: str, user: dict) -> str:
    """A second sign-in of this account from somewhere else — its session id."""
    with _client(nest_url, user) as client:
        return client.own_token_id


def _listed(nest_url: str, user: dict, harness_ids: set[str]) -> set[str]:
    """The actor's live session ids as the NEST lists them."""
    with _client(nest_url, user) as client:
        harness_ids.add(client.own_token_id)
        return {s["token_id"] for s in client.call("fauna.sessions.list", {})["sessions"]}


@pytest.mark.feature("sessions")
def test_list_revoke_one_then_sign_out_everywhere_else(app, request, nest_instance, test_user):
    from conftest import _login_app_as, _make_user

    driver = app.driver
    _require_sessions_page(driver)
    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    stranger_a = _mint_stranger_session(nest_url, user)
    stranger_b = _mint_stranger_session(nest_url, user)
    harness_ids: set[str] = set()

    # ── 1. The list ────────────────────────────────────────────────────────
    app.sessions.navigate()
    painted = app.sessions.painted_token_ids()
    assert painted, f"the page folded no rows (error: {app.sessions.error_text()!r})"
    assert app.sessions.card_count() == len(painted), (app.sessions.card_count(), painted)
    assert app.sessions.mark(0) == S.sessions.mark_this_app, (
        f"card 0 must be this app's own row; mark={app.sessions.mark(0)!r}"
    )
    assert not app.sessions.has_revoke_button(0), "this app's own row must carry no revoke"
    own_id = painted[0]
    for stranger in (stranger_a, stranger_b):
        assert stranger in painted[1:], (
            f"a stranger sign-in {stranger} is not listed; painted={painted}"
        )
        assert app.sessions.has_revoke_button(painted.index(stranger))
    assert app.sessions.kind(painted.index(stranger_a)) == S.sessions.kind_app

    # ── 2. Revoke one ─────────────────────────────────────────────────────
    app.sessions.revoke(painted.index(stranger_a))
    wait_until(
        lambda: stranger_a not in (app.sessions.painted_token_ids() or [stranger_a]),
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"the revoked card never left the page (painted={app.sessions.painted_token_ids()}, "
            f"error={app.sessions.error_text()!r})"
        ),
    )
    assert app.sessions.error_text() == ""
    listed = _listed(nest_url, user, harness_ids)
    assert stranger_a not in listed, "the nest still lists the revoked session"
    assert stranger_b in listed, "revoking one session must not touch another"
    assert own_id in listed, "revoking a stranger must not end this app's own session"

    # ── 3. Sign out everywhere else ───────────────────────────────────────
    before = _listed(nest_url, user, harness_ids)
    app.sessions.sign_out_everywhere_else()

    def others_gone() -> bool:
        return not ((before - {own_id}) & _listed(nest_url, user, harness_ids))

    wait_until(
        others_gone,
        ACT_FOLDED_S,
        diagnose=lambda: (
            f"sessions still listed after sign out everywhere else: "
            f"{sorted((before - {own_id}) & _listed(nest_url, user, harness_ids))} "
            f"(error={app.sessions.error_text()!r})"
        ),
    )
    assert own_id in _listed(nest_url, user, harness_ids), (
        "sign out everywhere else must keep this app's own session"
    )
    wait_until(
        lambda: stranger_b not in (app.sessions.painted_token_ids() or [stranger_b]),
        ACT_FOLDED_S,
        diagnose=lambda: f"the page never re-folded (painted={app.sessions.painted_token_ids()})",
    )
    assert app.sessions.error_text() == ""
    assert app.sessions.mark(0) == S.sessions.mark_this_app
    assert not app.sessions.has_revoke_button(0)
