"""The phrase-only restore, as the kit journeys drive it — shared steps.

Shared by ``tests/test_recovery_kit_restore.py`` and
``tests/test_recovery_kit_ceremonies.py``: every journey that ends "…and the
phrase brings the account back" takes the same steps — name the account, lose
every device, open ``recovery_entry``, wait for the ceremony to land, prove the
seed is the account's — and two copies of them would drift apart.

Goal docs: ``docs/goal/behavior/onboarding.md`` § 1 Identity (the screen and its
refusals) and ``docs/goal/behavior/identity-succession.md`` § Seed escrow →
*Restore path*.
"""
from __future__ import annotations

from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until

#: How long a restore may take to land or refuse: challenge, sign, fetch, unseal
#: and the handle probe behind ``handle_entry``, over a fresh pre-identity
#: connection. A generous ceiling paid only on red (convention 14).
RESTORE_OUTCOME_S = 45.0


def skip_unless_restore_is_built(app) -> None:
    """Temporary debt gate: has this app's restore leg landed yet?

    Not a declared absence — every app owes this screen. ``skip_unbuilt``
    fails under ``--strict-app`` and is tallied every run, so the gap stays
    visible instead of reading as a pass.
    """
    if app.is_visible("restore-from-recovery-kit-button"):
        return
    skip_unbuilt(
        app.driver,
        surface="restore-from-recovery-kit-button",
        detail=(
            "onboarding.md § 1 Identity; tui leads and the other six follow "
            "in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/identity-succession.md "
            "§ Implementation status today"
        ),
    )


def qualified_account(app, nest_instance) -> str:
    """The signed-in account as a user would type it on a device that has none.

    Qualified with the nest's own address, because a handle's ``@`` part is the
    only thing a pre-identity ceremony can find a nest from (the harness nest
    has no domain, so this is the direct-address locator of last resort,
    ``onboarding.md`` § 1 Identity). Read while a session still exists.
    """
    bare_handle = app.driver.get_state("session.handle")
    assert bare_handle, "the fixture must be signed in with a real nest handle"
    return f"{bare_handle}@{nest_instance['url'].split('//', 1)[1]}"


def point_the_restore_at(app, nest_instance) -> None:
    """Point the pre-identity restore connection at the plain-HTTP harness nest
    (the tier_3 ``nest`` provider override) — and, on web, let the browser dial
    it at all.

    The override hands the browser the nest's RAW url, so the handle check's
    ``/api/v1/health`` probe and the restore's pre-identity socket cross an
    origin boundary, and the nest answers with its own CORS decision. The
    session nest allows only the default hosted origin, so without this every
    web restore reads the blocked probe as ``registered_no_nest``. The grant is
    the admin's own choice surface (``fauna.admin.set_cors_origins``), naming
    this run's SPA origin the way production's allow-list names the hosted
    one — a real exact-match CORS decision still happens, nothing is proxied.
    Native apps see no CORS, so it is web-only.
    """
    if app.driver.is_web():
        from urllib.parse import urlparse

        from common.auth import set_cors_origins

        spa = urlparse(app.driver._spa_url or "")
        set_cors_origins(
            nest_instance["port"],
            admin_signing_key=nest_instance["admin"]["signing_key"],
            origins=[f"{spa.scheme}://{spa.netloc}"],
            base_url=nest_instance["url"],
        )
    app.driver.set_provider_base_urls({"nest": nest_instance["url"]})


def lose_every_device_and_open_restore(app, nest_instance) -> None:
    """Factory-reset the app and land on ``recovery_entry``, or skip if unbuilt.

    ``driver.reset()`` clears the credential namespace and returns to
    onboarding without a relaunch — from here the app holds nothing but what
    the user types. The provider override is the tier_3 nest seam
    ``test_onboarding_localhost.py`` also uses: a handle resolves to ``https``
    while the harness nest serves plain HTTP, so the pre-identity connection is
    pointed at it; the resolution under test still runs.
    """
    app.driver.reset()
    app.onboarding.navigate_to_status()
    skip_unless_restore_is_built(app)
    point_the_restore_at(app, nest_instance)
    app.onboarding.open_recovery_entry()


def wait_for_restore_landing(app) -> None:
    """Wait for a restore to land on ``handle_entry``, the import landing.

    The seed IS recovered at that point, so everything downstream is an import.
    Fails naming where the screen is instead, and what the error surface says.
    """
    wait_until(
        lambda: app.is_visible("handle-input"),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "the restore did not land on handle_entry; error surface says "
            f"{app.error_text()!r}, still on recovery_entry="
            f"{app.is_visible('recovery-entry-phrase-field')}, "
            f"on identity_import={app.is_visible('paste-secret-field')}"
        ),
    )
    assert_landing_left_no_error(app)


def assert_landing_left_no_error(app) -> None:
    """A landed restore leaves no error behind — read as a converged state.

    The landing is observed through the live element tree, the error through
    the state protocol, and the two need not advance together: windows PUSHES
    its state snapshot on a ~1 s cadence, so the read right after the page
    swap can still carry the refusal an earlier attempt left — measured on the
    replace journey, where the app's own log shows the error cleared before
    the restore settled while the one-shot read returned the old text
    (convention 14). So wait, bounded, for the cleared state; an error that
    genuinely stays behind still fails, naming its text.
    """
    wait_until(
        lambda: not app.has_error(),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "a landed restore leaves no error behind: " f"{app.error_text()!r}"
        ),
    )


def assert_restored_the_same_account(app) -> None:
    """Prove the restored seed is the account's own, not merely a valid seed.

    The handle check runs a silent challenge signed with whatever the restore
    handed back, and the nest verifies it against the registered actor — only
    the real seed earns the already-registered answer.
    """
    app.onboarding.run_handle_check(timeout=RESTORE_OUTCOME_S)
    message = app.driver.get_text("handle-message-area")
    assert "already_on_nest" in message or "Welcome back" in message, (
        "the restored seed must silently authenticate as the account that "
        f"minted the kit; handle-message-area reads {message!r} "
        f"(error surface: {app.error_text()!r})"
    )


def sign_in_after_restore(app) -> None:
    """handle_entry (already checked) → Continue → the signed-in main app.

    A restore lands where an import does, so signing in is the ordinary
    already-on-nest continuation (``onboarding.md`` § 1 Identity), not box
    recovery: that branch needs the user to have asked for it, which a kit
    restore never does. The wizard's ``LoggedIn`` handoff is where a phrase-only
    restore persists the predecessor seeds it recovered (tui
    ``session::persist_restored_predecessors``), so a journey that stopped at the
    handle check would never reach the device state a corpus read depends on.
    """
    app.onboarding.submit_handle()
    wait_until(
        lambda: app.is_visible("feed-tab") or app.is_visible("feed-view"),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "the restored account never reached the main app after Continue; "
            f"error surface: {app.error_text()!r}, still on handle_entry="
            f"{app.is_visible('handle-input')}"
        ),
    )
