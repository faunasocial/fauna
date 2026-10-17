"""The account runtime's two e2e holds — the publish hold and the fleet-walk
hold (`libs/fauna-account-plane/src/account_driver/e2e_holds.rs`).

Each keeps back only the runtime's UNPROMPTED leg — the local-write wake's
publish and a pass's, a nudge's walk and a pass's — never the awaited one a
caller asks for and waits on (`publish_mail`, `walk_fleet`). So a hold can only
turn red a tree that leans on the unprompted leg's latency, never a correct
one: it stands a replica in the window a timing race would otherwise open only
sometimes (`docs/goal/behavior/mail-credentials.md` § Hard revoke → *Published
before the nest moves*, the witness paragraphs). Compiled out of release
builds (convention 15; `test_shared_crate_seam_gating.py` scans the gate).

The spellings below are pinned to the Rust constants by
`tests/test_account_runtime_e2e_holds.py`.
"""

from __future__ import annotations

#: While this device owes the nest an own fleet row of the named kind, an
#: unprompted publish of the fleet plane publishes nothing.
PUBLISH_HOLD_ENV = "FAUNA_E2E_HOLD_PUBLISH_KIND"
#: Once this process has listed the fleet scope, an unprompted walk of it walks
#: nothing.
WALK_HOLD_ENV = "FAUNA_E2E_HOLD_FLEET_WALK"
#: The mail custody's row kind (`fauna_protocol::merge_policy::KIND_MAIL`).
KIND_MAIL = "fauna.state.mail"


def relaunch_session_app_with(app, request, environment: dict) -> None:
    """Relaunch the session-scoped ``app`` with ``environment`` added to its
    launch env (``hard_reload`` replays the login it holds), and at this test's
    teardown take the variables back out and relaunch it bare — the app
    outlives the test, and a hold left in its process would reach whatever test
    drives it next.

    The restore is a finalizer, not a ``finally``: it runs after the call
    phase's report, so a failure's app log is the held process's own, not the
    bare relaunch's. ``relaunch_environment()`` is the live dict every later
    ``recover()`` / ``hard_reload()`` reads, so the test's own relaunches keep
    the hold."""
    env = app.driver.relaunch_environment()
    assert env is not None, (
        f"{type(app.driver).__name__} has no relaunchable launch environment — "
        "an account-runtime hold rides the process env at launch"
    )
    saved = {k: env.get(k) for k in environment}

    def restore() -> None:
        for key, value in saved.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        app.driver.recover()

    request.addfinalizer(restore)
    env.update(environment)
    app.driver.hard_reload()
