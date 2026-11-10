"""tier_3 API: a refresh token used twice ends every session that app held
(`docs/goal/behavior/authorization-server.md` § The issuer (TP5) → *Rotation
rides the NEST's registry*).

`docs/features/connected-apps.md` outcome 54, driven with no app at all. The
reuse family-kill is pinned in process on the shared registry
(`bridge_atproto_handlers::rotate_session`, which the token endpoint's refresh
grant calls); what only a live nest shows is that the OAuth plane really rides
it: alice approves an outside app over the wire the way her app does (the
typed-code start, `test_oauth_consent_starts.py`), the app rotates its refresh
token once, and then the SUPERSEDED token is presented again — the signature of
a stolen token racing the honest client. The replay is refused, and so is the
honest client's own current token on its next rotation: the whole family is
dead, not just the replayed token. Asserted by effect on returned state
(convention 14).

The forced session-secret rotation, which ends every grant on a nest, is
`test_oauth_session_secret_rotation.py`'s and owns its nest for that reason;
this replay ends one family only, so it shares the module-scoped consent nest.
"""

import pytest

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from tests.api.test_oauth_consent_starts import _client, _grant_by_typed_code

pytestmark = pytest.mark.tier_3


def _refused(app, refresh_token):
    status, body = app.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": refresh_token}
    )
    return status, body.get("error")


@pytest.mark.feature("connected-apps")
def test_a_refresh_token_used_twice_ends_the_apps_whole_session_family(
    consent_bridge, consent_nest
):
    app = _client(consent_nest, 17811)
    first = _grant_by_typed_code(consent_nest, app)["refresh_token"]
    assert first, "precondition: the grant carries a refresh token"

    # The honest rotation: a new refresh token, and the old one superseded.
    second = app.refresh(first)["refresh_token"]
    assert second and second != first, "a rotation must issue a NEW refresh token"

    # The replay: the superseded token, presented again under the app's own key.
    assert _refused(app, first) == (400, "invalid_grant"), (
        "a refresh token presented a second time must be refused"
    )

    # The family-kill: the CURRENT token — never replayed, held only by the
    # honest client — is refused too, because the replay ended the family.
    assert _refused(app, second) == (400, "invalid_grant"), (
        "a replayed refresh token must end every session of the family, the "
        "current token included — not only refuse the replayed one"
    )
