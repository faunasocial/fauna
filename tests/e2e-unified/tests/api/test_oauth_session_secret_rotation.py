"""tier_3 API: ending every saved sign-in shuts out each outside app
(`docs/goal/behavior/authorization-server.md` § The issuer (TP5) → *Two
rotation arms*).

`docs/features/admin-nest.md` outcome 28, driven with no app at all. The app
journey's twin, `test_atproto_pds_consent.py::test_a_forced_session_secret_
rotation_kills_every_live_refresh_token`, approves its consent card in a
launched app, which a `[nest]` outcome cannot cite; here alice approves over the
wire the way her app does (the typed-code start's `lookup_code` +
`resolve_consent`, `test_oauth_consent_starts.py`), so the assertion half is
the same and nothing is launched.

Two outside apps hold live grants. After the admin forces the session secret,
each one's refresh token is refused `invalid_grant` on its very next read, and
neither is listed among alice's connected apps any more. Asserted by effect on
returned state (convention 14). The module owns its consent nest: the forced
rotation ends every grant on it, which a shared module's later tests would
inherit.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from tests.api.test_oauth_consent_starts import _alice, _client, _grant_by_typed_code

pytestmark = pytest.mark.tier_3


def _connected(consent_nest) -> set[str]:
    with _alice(consent_nest) as ws:
        return {g.get("client_id") for g in ws.call("fauna.bridges.atproto.list_grants", {})["grants"]}


@pytest.mark.feature("admin-nest")
def test_ending_every_saved_sign_in_refuses_each_outside_apps_next_refresh(
    consent_bridge, consent_nest
):
    apps = [_client(consent_nest, port) for port in (17791, 17792)]
    refresh = {}
    for app in apps:
        tokens = _grant_by_typed_code(consent_nest, app)
        rotated = app.refresh(tokens["refresh_token"])
        assert rotated.get("refresh_token"), rotated
        refresh[app.client_id] = rotated["refresh_token"]
    assert {a.client_id for a in apps} <= _connected(consent_nest), (
        "precondition: both outside apps are listed as connected"
    )

    admin = consent_nest["admin"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as admin_ws:
        forced = admin_ws.call("fauna.oauth.force_rotate_session_secret", {})
    assert forced["rotated_at"] > 0, forced
    assert forced.get("grants_ended", 0) >= len(apps), (
        f"the rotation must end and count every grant it killed: {forced!r}"
    )

    for app in apps:
        status, refused = app.token_request_expecting_refusal(
            {"grant_type": "refresh_token", "refresh_token": refresh[app.client_id]}
        )
        assert (status, refused.get("error")) == (400, "invalid_grant"), (
            f"{app.client_id}'s next refresh must be refused: {status} {refused!r}"
        )

    still = _connected(consent_nest) & {a.client_id for a in apps}
    assert not still, f"the ended grants must leave alice's connected-apps list: {still!r}"
