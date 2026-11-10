"""What a pending join request does when its recheck reaches no verdict, and
when it reaches the one verdict no retry clears — `join-a-nest.md` outcome 15.

`onboarding.md` § The pending-invite surface (*Approval is detected as
admission*) and `transport-trust.md` § Pre-claim surfacing rule two arms of
`recheck_invite_status` that nothing exercised end to end before this module:

* **A nest the poll cannot reach is no verdict.** The request stays
  `PendingReview` and the poll keeps asking. Until 2026-11-07 a transient
  failure of the status call itself (the nest stopped) wrote a transient
  `Rechecking` error instead, and since the recheck proceeds only from
  `PendingReview`, the poll stopped for good on all 7 apps — the shared machine
  now restores the pending state (`machine_lifecycle.rs::
  pending_poll_transient_status_call_keeps_the_request_pending`).
* **A changed nest identity is terminal.** The typed identity verdict renders
  the terminal `Rechecking` error with no re-trust affordance: a consumer that
  retries a possible-MITM signal without a human must stop.

**Fixtures.** The unreachable arm stops a nest, so it rides the function-scoped
`stoppable_nest`, never the shared `nest_instance`. The identity arm needs a pin
the nest's live identity disagrees with, and native pins are consulted only over
TLS, so it rides `self_signed_nest` and seeds a mismatching pin through the
shared E2E bridge name `set_nest_identity_pin_for_test` — the idiom
`test_nest_identity_pin.py` uses. A pin is HELD at the recheck, so the verdict is
the identity one, not § Pre-claim surfacing rule 3's transient (trust trouble
with no pin held).

**The tick.** The app's own poll fires every `INVITE_RECHECK_POLL_MS`; these
tests drive the same `recheck_invite_status` through the bridge instead, whose
ack waits for the recheck to finish — a causal barrier, not a sleep
(convention 14). It stands in for the poll timer, not for a user's mutation
(convention 8): the user's own act on this page, the submit, goes through the UI.

**tier_3** — real `fauna-nest`, real wire, real app UI.
"""

import json
import secrets
import time

import pytest

from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_pending_invite_journey import (
    _drive_app_to_pending_review,
    _status_text,
)

pytestmark = pytest.mark.tier_3

# One driven recheck: one nest round trip (or one refused connect), plus the
# app's re-render. A ceiling a green run never spends.
RECHECK_BUDGET_S = 30.0

INVITE = S.onboarding.invite


def _tick(app) -> None:
    """One poll tick: the machine's own `recheck_invite_status`, acked only once
    it has returned (`OnboardingMachine::call_machine_method_async`)."""
    app.driver.call_machine_method("recheck_invite_status", "", timeout=RECHECK_BUDGET_S)


def _settled_status(app) -> str:
    """The status row once no recheck is in flight — the app's own poll may be
    mid-recheck when a test looks (`Checking status…` is a spinner, not a
    state this module asserts on)."""
    return wait_until(
        lambda: (lambda s: s if s != INVITE.rechecking else None)(_status_text(app)),
        RECHECK_BUDGET_S,
        diagnose=lambda: f"a recheck never finished; error={app.error_text()!r}",
    )


@pytest.mark.feature("join-a-nest")
def test_a_pending_request_stays_pending_while_the_nest_is_unreachable(app, stoppable_nest):
    """Submit → the nest goes away → a recheck tick → still pending, no error,
    the recheck still offered → a second tick still reaches for the nest."""
    app.driver.reset()
    handle = f"pendoffline-{int(time.time() * 1000)}"
    _drive_app_to_pending_review(app, stoppable_nest["url"], handle)
    assert _settled_status(app) == INVITE.pending_review

    # A causal barrier: when stop() returns the port is dead.
    stoppable_nest["stop"]()

    for tick in (1, 2):
        _tick(app)
        status = _settled_status(app)
        assert status == INVITE.pending_review, (
            f"tick {tick}: an unreachable nest must leave the request pending "
            "(onboarding.md § The pending-invite surface; join-a-nest.md "
            f"outcome 15), status reads {status!r}; error={app.error_text()!r}"
        )
        # The recheck affordance shows only in PendingReview, so it is the
        # honest witness that the poll can still run — an error state would
        # have hidden it and stopped the poll for good.
        assert app.driver.is_visible("invite-request-recheck-button"), (
            f"tick {tick}: the recheck must stay offered while the nest is "
            f"unreachable; status={status!r} error={app.error_text()!r}"
        )
        assert not app.has_error(), (
            f"tick {tick}: an unreachable nest is no error, got {app.error_text()!r}"
        )


@pytest.mark.feature("join-a-nest")
def test_a_changed_nest_identity_ends_the_pending_request_with_no_trust_offer(
    app, self_signed_nest
):
    """Submit over TLS → the pin the app holds stops matching the nest's live
    identity → a recheck tick → the terminal error, no recheck, no re-trust
    affordance, and a later tick does not resume the poll."""
    nest_url = self_signed_nest["url"]
    app.driver.reset()
    handle = f"pendidentity-{int(time.time() * 1000)}"
    _drive_app_to_pending_review(app, nest_url, handle)
    assert _settled_status(app) == INVITE.pending_review

    # The nest's identity, as far as this app now knows, is a different key:
    # the next connect's channel binding fails possession-verify against it
    # (`IdentityError::PinChanged`) — the identity-changed verdict.
    app.driver.call_machine_method(
        "set_nest_identity_pin_for_test",
        json.dumps({"nest_url": nest_url, "actor_id": secrets.token_hex(32)}),
    )

    terminal_prefix = INVITE.error.terminal(cause="")[: -len(".")]

    def _ended() -> str | None:
        status = _settled_status(app)
        if status == INVITE.pending_review:
            _tick(app)  # the app's own poll may not have fired yet
            status = _settled_status(app)
        return status if status != INVITE.pending_review else None

    status = wait_until(
        _ended,
        RECHECK_BUDGET_S,
        diagnose=lambda: f"status={_status_text(app)!r} error={app.error_text()!r}",
    )
    assert status.startswith(terminal_prefix), (
        "a changed nest identity must end the request on the terminal error "
        "(transport-trust.md § Pre-claim surfacing; join-a-nest.md outcome 15), "
        f"status reads {status!r}; error={app.error_text()!r}"
    )
    for absent in (
        "invite-request-recheck-button",
        "nest-identity-changed-trust-button",
        "trust-box-grant-button",
    ):
        assert app.driver.is_absent(absent), (
            f"{absent!r} must not render after the identity verdict — no retry, "
            f"no re-trust offer; status={status!r}"
        )

    # The poll does not resume: a further tick leaves the terminal state as it
    # was (`recheck_invite_status` proceeds only from PendingReview).
    _tick(app)
    assert _settled_status(app) == status, (
        f"a tick after the verdict changed the page: {_status_text(app)!r} "
        f"(was {status!r})"
    )
    assert app.driver.is_absent("invite-request-recheck-button")
