"""tier_3 e2e: the mailbox an account already had comes back, with its app
passwords, after a factory reset and a re-claim through the app — the
``factory-reset`` feature page's outcome 17 (``docs/features/factory-reset.md``).

``docs/goal/behavior/mail-credentials.md`` § A nest that lost the mailbox
(factory reset, rebuilt box) owns the behaviour: the mail custody is the
account's and outlives the box, the account plane's bind leg pushes it to the
wiped nest, and the claim-time provisioning reads the lost mailbox (custody
holds the MSEK and a live credential, the nest holds no recipient
registration) and restores the nest-side half from the custody — the same
credentials, so the same app passwords — never minting over it. The claim that
chose email also flips the deployment mail switch back on. Nothing asks the
admin to enable or resume mail. ``nest/common.md`` § Factory reset owns the
reset and re-claim gestures themselves.

**Every state change is the app's act** (convention 8): the claim that turns
mail on (a domain-shaped handle derives email ON, ``onboarding.md`` § 3b), the
extra app password, the reset from the admin Danger zone, the re-claim through
the wizard with the code the reset pre-filled. The harness only plays the
supervisor that restarts the self-exited nest into its wipe
(``common.nest.resume_after_self_exit``) and reads back what the box and the
page say. The client process — and so its store, which holds the mail custody —
is the same one throughout, as for the person who resets their box and claims
it again from the app they are holding.

**Non-vacuous.** The roster and the added password's secret are read BEFORE the
reset and compared after it; the nest is witnessed wiped (unclaimed, mail off)
between the two; and the page's own enabled toggle, its status and the absence
of the pending-rotation banner are what say no enable or resume is owed. The
outcome-14 witness (``test_factory_reset_mail.py``) drives the nest over the
API and never carries an app's custody across the reset; this is the journey
that does.

What it does not assert: a delivery into the restored mailbox — the venue is a
local nest with no MTA bridge; the release gate's live mail tests hold that leg.

Runs on tui, linux, web, android, macos and ios (the real-domain claim needs the
``post-logged-in-dial-seam`` surface, which the fixture gates on); the other
apps follow in their trickle-down lifts.
"""

from __future__ import annotations

import contextlib
import secrets

import pytest

from actions.connected_apps import ConnectedAppsActions
from common.nest import _wait_nest_fresh, resume_after_self_exit
from conftest import MAIL_PRIMARY_DOMAIN
from helpers.crash_recovery import setup_status
from helpers.waiting import (
    await_serving_enablement_for,
    serving_enablement_completed_runs,
    wait_until,
)
from nacl.signing import SigningKey
from tests.test_factory_reset_journeys import (
    _await_prefilled_claim,
    _claim_through_the_wizard,
)
from tests.test_mail_enable_at_admin_claim import (
    _drive_admin_claim_to_logged_in,
    _real_domain_nest,
    _reinstall_nest_override,
    _wait_deployment_mail_enabled,
    _wait_for_auto_minted_credential,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.destructive,
]

#: A nest restarting into its wipe and answering fresh.
_NEST_FRESH_S = 120.0
#: The restore landing after the re-claim: the bind leg's push, the claim-time
#: provisioning's calls, the page's hydrate (convention 14 — a ceiling, not a
#: timing assertion).
_RESTORE_S = 120.0
#: The added password's name — distinct from the claim's auto-minted `Default`.
_EXTRA = "Laptop"

PENDING_BANNER = "mail-settings-pending-rotation-banner"
PENDING_RESUME = "mail-settings-pending-rotation-resume-button"


@pytest.fixture
def reclaim_mail_nest(request, app, nest_mode, tmp_path_factory):
    """A fresh unclaimed nest the wizard reaches by a domain-shaped handle
    (`<localpart>@fauna.test` over the `"nest"` override), so the claim derives
    email ON — the shared real-domain fixture body, which also clears the
    override on teardown."""
    yield from _real_domain_nest(
        request, app, nest_mode, tmp_path_factory, "reset-mail-reclaim-nest"
    )


def _roster(app) -> list[tuple[str, str]]:
    """The mail app-password rows as (name, login), in roster order — one visit
    to Connected apps, then back to the mail page. The login carries the
    credential id, so an equal list names the same credentials."""
    roster = ConnectedAppsActions(app.driver)
    try:
        assert roster.visit(), (
            "the Connected apps page never finished its read; "
            f"error: {roster.current_error_text()!r}"
        )
        return [
            (roster.mail_name(n), roster.mail_login(n))
            for n in range(roster.mail_row_count())
        ]
    finally:
        app.mail_settings.navigate()


def _secret_of(app, name: str) -> str:
    """The revealed secret of the mail row named `name`."""
    roster = ConnectedAppsActions(app.driver)
    try:
        assert roster.visit()
        for n in range(roster.mail_row_count()):
            if roster.mail_name(n) == name:
                return roster.reveal_mail_secret(n)
        raise AssertionError(f"no mail row named {name!r}")
    finally:
        app.mail_settings.navigate()


def _page_state(app) -> str:
    ms = app.mail_settings
    return (
        f"toggle={ms.enabled_toggle_state()!r} status={ms.status_text()!r} "
        f"pending banner shown={app.driver.count(PENDING_BANNER) > 0} "
        f"page error={ms.page_error_text(timeout=0.5)!r} app error={app.error_text()!r}"
    )


@pytest.mark.feature("factory-reset")
def test_a_reclaim_after_a_reset_brings_back_the_mailbox_and_its_app_passwords(
    app, reclaim_mail_nest
):
    """Outcome 17: an admin with mail on and two app passwords resets the nest
    from the app and claims it again with email chosen; the Mail page reads on,
    lists the same passwords (same names, same logins, the same secret), and
    shows no enable or resume prompt — and the deployment's mail switch is on
    again, from the claim's own choice."""
    nest = reclaim_mail_nest
    url = nest["url"]

    # ── Before: claim with email chosen, add a second app password ─────────
    localpart = "admin" + secrets.token_hex(3)
    secret_hex = secrets.token_hex(32)
    _drive_admin_claim_to_logged_in(
        app, nest, secret_hex, f"{localpart}@{MAIL_PRIMARY_DOMAIN}"
    )
    assert _wait_deployment_mail_enabled(url, timeout=90.0), (
        f"the domain-handle claim must turn mail on; app error={app.error_text()!r}"
    )
    assert _wait_for_auto_minted_credential(app, timeout=60.0), (
        "the claim turned mail on but minted no app password; "
        f"{_page_state(app)}"
    )
    app.mail_settings.add_credential_autogen(_EXTRA)
    assert app.mail_settings.wait_for_credential_count(2, timeout=60.0), (
        f"the added app password never listed; {_page_state(app)}"
    )
    before = _roster(app)
    assert {name for name, _ in before} >= {_EXTRA}, before
    extra_secret = _secret_of(app, _EXTRA)
    assert extra_secret, "the added app password's secret never revealed"

    # ── The reset, from the page, and the box coming back wiped ────────────
    code = app.admin.factory_reset_via_ui()
    # Whether the `"nest"` override outlives the app's reset is the app's
    # business; installing it again keeps the re-claim leg on this nest.
    _reinstall_nest_override(app, nest)
    resume_after_self_exit(nest)
    _wait_nest_fresh(url, timeout=_NEST_FRESH_S)
    wiped = setup_status(url)
    assert not wiped.get("claimed") and not wiped.get("email_enabled"), (
        f"the reset must leave an unclaimed box with mail off — reads {wiped!r}"
    )

    # ── The re-claim through the wizard, the same identity, email chosen ───
    actor_id_hex = bytes(SigningKey(bytes.fromhex(secret_hex)).verify_key).hex()
    prior_runs = serving_enablement_completed_runs(app.driver, actor_id_hex)
    _await_prefilled_claim(app, code, "the reset opens the claim step")
    _claim_through_the_wizard(app, nest, "the pre-filled code re-claims the box")
    # The causal anchor (convention 14): the re-claim's own serving-enablement
    # run has finished, so the deployment toggles are final for it.
    decided = await_serving_enablement_for(
        app.driver, actor_id_hex, what="the re-claim", prior_runs=prior_runs
    )
    assert _wait_deployment_mail_enabled(url, timeout=30.0), (
        "the re-claim chose email, so it must turn the deployment's mail on "
        f"again; the re-claim decided {decided!r}, box reads {setup_status(url)!r}, "
        f"app error={app.error_text()!r}"
    )

    # ── After: the Mail page reads on, the same passwords, nothing owed ────
    _await_restored(app)
    assert app.mail_settings.enabled_toggle_state() == "on", _page_state(app)
    assert app.driver.count(PENDING_BANNER) == 0, (
        f"a restored mailbox must owe no resume; {_page_state(app)}"
    )
    assert app.driver.count(PENDING_RESUME) == 0, _page_state(app)

    after = _roster(app)
    assert after == before, (
        "the re-claim must bring back the app passwords the account had — "
        f"before {before!r}, after {after!r}"
    )
    assert _secret_of(app, _EXTRA) == extra_secret, (
        "the restored app password must be the same password, not a new one"
    )


def _await_restored(app) -> None:
    """The page reads the mailbox on and up to date. Re-opens the page each
    poll: the restore lands after the claim, and the page hydrates on entry."""

    def restored() -> bool:
        with contextlib.suppress(Exception):
            app.mail_settings.navigate()
        return (
            app.mail_settings.enabled_toggle_state() == "on"
            and app.mail_settings.status_text() == app.mail_settings.STATUS_ENABLED
        )

    wait_until(
        restored,
        _RESTORE_S,
        interval=3.0,
        diagnose=lambda: f"the restored mailbox never read on and up to date; {_page_state(app)}",
    )
