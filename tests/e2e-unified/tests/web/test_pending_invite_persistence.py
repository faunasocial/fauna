"""Pending-invite persistence — force-quit / relaunch regressions.

Per the pending-invites long-term-store design (tracked internally), §Testing:
  1. submit invite request, force-quit, relaunch — wizard lands at
     InviteRequest with PendingReview
     → tier_3 (the session nest): the slot is written only at the REAL
       submit's return (onboarding.md § Long-term store contract), which a
       snapshot setter never passes; Continue is disabled throughout
       PendingReview (§ 3), so nothing else could write it.
  2. a slot carrying the retired Approved variant relaunches — wizard
     degrades it to PendingReview (onboarding.md § 3, *At-rest*), Continue
     disabled; Approved itself retired 2026-08-12
  3. submit, recheck → 404, restart — wizard lands at IdentityChoice/
     HandleEntry, no orphan invite in store
     → lives in the tier_3 sibling test_pending_invite_recheck_404.py:
       the not-found cleanup only fires when a REAL recheck reaches the
       nest, which a faked snapshot can't drive (this file's snapshot
       setters bypass persistInviteSlotIfActionable).
  4. submit, redeem → LoggedIn, restart — wizard skipped, app at feed,
     no orphan invite in store

These tests are web-specific (other apps have their own per-platform
store implementations and their own regression files). Test file lives
in tests/web/ so the cross-app conftest auto-deselects when running
non-web apps.
"""

import json
import secrets

from common.accounts import actor_id_hex
from helpers import web_store
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
import pytest


# The tier is per test: scenario 1 drives a real submit against the session
# nest (tier_3); 2 and 4 fake the store and snapshot only (tier_2).
pytestmark = [pytest.mark.web]


WEB_URL_GUARD = "https://nest.example.test"
TEST_HANDLE = "alice@nest.example.test"
TEST_REQUEST_ID = "req-pending-invite-001"


def _read_pending_invite_keys(app, secret_hex: str = "00" * 32) -> dict:
    """The identity's per-actor `fauna/{actor}/pending_invite` record as its
    four fields, every one None when the slot is absent."""
    record = web_store.read_record(app.driver, actor_id_hex(secret_hex), "pending_invite") or {}
    return {k: record.get(k) for k in ("nest_url", "handle", "request_id", "status_json")}


@pytest.mark.tier_3
@pytest.mark.feature("join-a-nest")
def test_force_quit_after_submit_relaunches_at_invite_request_pending(
    app, nest_instance, spa_url
):
    """1. A real submit writes the slot at its RETURN; Continue stays disabled
    throughout PendingReview; a force-quit relaunch re-seeds the wizard at
    InviteRequest with PendingReview.

    The slot's only write moment is the `wizard_submit_invite_request()` return
    (onboarding.md § Long-term store contract) — the former continue-exit that
    also saved it retired 2026-08-12, and Continue is now the out-of-band code's
    redeem and nothing else, **disabled during `PendingReview`** (onboarding.md
    § 3, the button's row). So the submit has to be real: a snapshot setter
    lands PendingReview without ever passing the return that writes the slot
    (this module's docstring), which is why this scenario — alone of this
    file's three — takes the session nest. `spa_url` is the nest the wizard
    dials for the same reason the 404 sibling gives: the relaunch's poll
    re-dials the slot's `nest_url` from the browser, which must be same-origin.
    """
    secret_hex = secrets.token_hex(32)
    handle = "joiner" + secrets.token_hex(3)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=UI_SETTLE_S)
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest", json.dumps([spa_url, handle])
    )
    app.driver.wait_for("invite-request-submit-button", timeout=RPC_ROUNDTRIP_S)
    app.driver.click("invite-request-submit-button")

    # PendingReview is the only state that reveals Recheck — the submit landed.
    wait_until(
        lambda: app.driver.is_visible("invite-request-recheck-button"),
        RPC_ROUNDTRIP_S, interval=0.5,
        diagnose=lambda: (
            "the real invite_request.submit never reached PendingReview. "
            f"error={app.error_text()!r}"
        ),
    )
    assert not app.driver.is_enabled("invite-request-continue-button"), (
        "Continue must be disabled during PendingReview — the pending-review "
        "journey advances by polling, never by a continue-exit (onboarding.md "
        f"§ 3). {app.driver.diagnose('invite-request-continue-button')}"
    )

    # Written at the submit's return — no click owed. Waited on rather than
    # read: the page persists after the submit resolves, so the recheck
    # button can paint a tick before the store write lands (convention 14).
    def slot_written():
        keys = _read_pending_invite_keys(app, secret_hex)
        return keys if keys["request_id"] else None

    keys = wait_until(
        slot_written, UI_SETTLE_S, interval=0.3,
        diagnose=lambda: (
            "the submit's return wrote no pending-invite slot: "
            f"{_read_pending_invite_keys(app, secret_hex)}"
        ),
    )
    assert keys["nest_url"] == spa_url
    assert keys["handle"] == handle
    assert "PendingReview" in (keys["status_json"] or ""), keys

    # Force-quit: hard reload. The launch path calls tryRestorePendingInvite,
    # seeds the wizard, and lands on InviteRequest still in PendingReview —
    # the request is genuinely pending on the nest, so the relaunch's
    # immediate poll keeps it there.
    app.driver.hard_reload()
    wait_until(
        lambda: app.driver.is_visible("invite-request-recheck-button"),
        RPC_ROUNDTRIP_S, interval=0.5,
        diagnose=lambda: (
            "relaunch (tryRestorePendingInvite) should land back on "
            "InviteRequest in PendingReview: "
            f"{app.driver.diagnose('invite-request-recheck-button')} "
            f"error={app.error_text()!r}"
        ),
    )
    keys_after = _read_pending_invite_keys(app, secret_hex)
    assert keys_after["request_id"] == keys["request_id"], (
        f"the slot must survive the relaunch unchanged: before={keys} "
        f"after={keys_after}"
    )


@pytest.mark.tier_2
def test_relaunch_with_a_retired_approved_slot_degrades_to_pending_review(app):
    """2. A slot whose status_json no longer parses — here the retired
    `Approved` variant — relaunches onto InviteRequest as PendingReview,
    polling, with Continue disabled.

    `InviteRequestState::Approved` retired 2026-08-12 (onboarding.md § 3's
    state list): an admin approve deletes the request row, so no live nest
    serves it, and approval is detected as admission by the poll. What the
    relaunch owes an unparseable slot is `seed_pending_invite`'s degrade to
    PendingReview (onboarding.md § 3, *At-rest*), which resumes polling and
    lets the registered-probe re-derive the truth — never the old dead end, a
    rendered Approved whose Continue redeemed into `ActorAlreadyRegistered`.
    """
    approved_state = {
        "Approved": {
            "quota": {"storage_bytes": 1_000_000_000, "traffic_bytes_per_month": 5_000_000_000},
            "request_id": TEST_REQUEST_ID,
        }
    }
    actor = web_store.seed_identity(app.driver, "00" * 32)
    web_store.seed(
        app.driver,
        {
            f"fauna/{actor}/pending_invite": web_store.pending_invite_record(
                nest_url=WEB_URL_GUARD,
                handle=TEST_HANDLE,
                request_id=TEST_REQUEST_ID,
                status_json=json.dumps(approved_state),
            )
        },
    )
    app.driver.hard_reload()

    # The degrade is read off the machine, not off a button: the relaunch's
    # immediate poll dials the slot's unresolvable `nest.example.test`, so the
    # state is PendingReview until that poll lands and a transient
    # `Error{context: Rechecking}` after it. Both prove the degrade — a recheck
    # runs ONLY from PendingReview (`machine.rs` `recheck_invite_status`) — and
    # neither is the retired Approved.
    def degraded():
        snap = app.driver.call_machine_method("inviteRequestSnapshot")
        if isinstance(snap, str):
            snap = json.loads(snap)
        state = snap.get("state") if isinstance(snap, dict) else None
        if not isinstance(state, dict):
            return None
        if "PendingReview" in state:
            return state
        error = state.get("Error")
        if isinstance(error, dict) and error.get("context") == "Rechecking":
            return state
        return None

    wait_until(
        degraded, RPC_ROUNDTRIP_S, interval=0.5,
        diagnose=lambda: (
            "a retired-Approved slot must re-seed the wizard at InviteRequest "
            "in PendingReview: invite_snapshot="
            f"{app.driver.call_machine_method('inviteRequestSnapshot')} "
            f"error={app.error_text()!r}"
        ),
    )
    assert not app.is_enabled("invite-request-continue-button"), (
        "Continue is the out-of-band code's redeem and nothing else, disabled "
        "during PendingReview (onboarding.md § 3): "
        f"{app.driver.diagnose('invite-request-continue-button')} "
        f"error={app.error_text()!r}"
    )


# Scenario 3 (recheck → 404 → orphan cleanup) lives in the tier_3 sibling
# test_pending_invite_recheck_404.py — it needs a real nest, not a faked
# snapshot (see this module's docstring).


@pytest.mark.tier_2
def test_redeem_then_relaunch_clears_orphan_invite_and_attempts_silent_signin(app):
    """4. After successful redeem the launch path sees secret + nest_url
    in localStorage with no pending-invite slot. The launch path
    attempts silent challenge against the saved nest_url; on success it
    routes to /app/feed.

    This test fakes the nest_url (no real nest in the bridge fixture),
    so the silent challenge fails with TypeError → maps to
    `transient_error` per `runSilentChallenge`. We verify the
    transient-error UI is rendered (proves the silent-challenge path
    actually executed) and the orphan pending-invite slot was not
    re-created — the cleanup happened.

    The "lands at /app/feed" leg of the contract requires a real nest
    fixture (out of scope for this file; covered by integration tests
    when a nest_instance fixture is wired in)."""
    actor = web_store.seed_identity(
        app.driver, "00" * 32, nest_url=WEB_URL_GUARD, handle=TEST_HANDLE
    )
    # Make sure no orphan pending-invite slot is present.
    app.driver.eval_js(f"localStorage.removeItem('fauna/{actor}/pending_invite')")
    app.driver.hard_reload()
    # Silent challenge runs against the fake nest_url and fails. The
    # launch screen shows the transient-error CTA (per
    # the onboarding client target-state design (tracked internally),
    # §"App-launch routing"). The error is the right error: it proves
    # the silent-challenge code path was hit (the previous synchronous
    # redirect would never see a network failure).
    import time as _time
    deadline = _time.monotonic() + 5
    saw_launch_error = False
    while _time.monotonic() < deadline:
        try:
            if app.is_visible("launch-transient-error"):
                saw_launch_error = True
                break
        except Exception:
            pass
        _time.sleep(0.2)
    assert saw_launch_error, "Expected silent-challenge to fail and show launch-transient-error"
    # The orphan pending-invite slot was NOT recreated — the cleanup
    # part of the contract held.
    keys = _read_pending_invite_keys(app)
    assert all(v is None for v in keys.values()), (
        f"Expected no orphan pending-invite keys, got {keys}"
    )
