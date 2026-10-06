"""The live half of the connection barrier: the app really publishes it.

``test_connection_barrier.py`` pins what the barrier *waits for* against a
scripted driver. This pins that the observable it waits on is real — that the
app under test publishes ``fauna_e2e_agent::CONNECTION_KEY`` at the shared depth
and that it has reached online by the time ``logged_in_app`` hands the app over.

Without this, the barrier could quietly become vacuous in the one way that
matters: an app's state provider stops publishing the key, the login barrier
starts returning immediately on every test, and the whole suite goes back to
racing the WS handshake with nothing red to show for it.

`account-offline-mutation.md` § The offline-mutation contract, class 3 owns
the desensitizing rule (partitioned out of `account-data-plane.md`
2026-09-06); `e2e-conventions.md` convention 14 owns the barrier shape.
"""

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from helpers.connection import (
    APPS_PUBLISHING_CONNECTION,
    CONNECTION_KEY,
    connection_observable,
)

pytestmark = pytest.mark.tier_3


# No `@pytest.mark.feature`: this witnesses the HARNESS's own precondition, not a
# user-facing outcome. `test_agent_barrier.py` — the barrier command's self-test,
# the closest sibling — carries none for the same reason.
def test_connection_observable_is_online_after_login(logged_in_app):
    """``logged_in_app`` has already crossed the barrier, so the app must report
    an online transport here — if it does not, the barrier let a test through on
    a desensitized app and every ``OnlineOnly`` assertion below it is racing."""
    driver = logged_in_app.driver
    name = app_name(driver)

    if name not in APPS_PUBLISHING_CONNECTION:
        skip_unbuilt(
            driver,
            surface=f"the `{CONNECTION_KEY}` state observable",
            detail=(
                "this app publishes no connection state, so the shared "
                "connection barrier is a no-op on it and every OnlineOnly "
                "affordance it drives after launch is still racing the "
                "handshake"
            ),
            tracked="",
        )

    observed = connection_observable(driver)
    assert observed is not None, (
        f"{name} is declared to publish `{CONNECTION_KEY}` "
        f"(helpers/connection.py:APPS_PUBLISHING_CONNECTION) but /app/state "
        f"carried no such key — its state provider regressed, and the login "
        f"barrier has been a silent no-op on every test since"
    )
    assert isinstance(observed.get("state"), str) and observed["state"], (
        f"`{CONNECTION_KEY}.state` must be the lowercase transport word "
        f"(`ConnectionState::as_wire_word`), got {observed.get('state')!r}"
    )
    assert observed.get("online") is True, (
        f"{name} reports {observed['state']!r} AFTER the login barrier, so the "
        f"barrier is not doing its job: every OnlineOnly control this suite "
        f"drives from here is desensitized and refuses with a named 409"
    )
