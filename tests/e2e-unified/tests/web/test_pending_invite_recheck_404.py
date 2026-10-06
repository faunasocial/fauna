"""Tier_3: relaunch → recheck → 404 clears the orphan pending-invite slot.

Scenario 3 of the pending-invite persistence suite (the sibling
``tests/web/test_pending_invite_persistence.py`` holds the rest: 2 and 4 are
snapshot-injection tier_2 tests, 1 a tier_3 real submit). This one is split
out because it genuinely needs a REAL nest:

  - The cleanup only fires when a real recheck reaches the nest's
    ``fauna.account.invite_request.status`` handler, which returns
    ``fauna.account.invite_request_not_found`` for an actor with no invite
    request (``bins/fauna-nest/src/invite_core.rs`` get_invite_request_status_core
    → ``InviteError::InviteRequestNotFound`` → ``invite_handlers.rs``).
  - The client maps that to ``InviteRequestError::NotFound`` →
    ``InviteRequestState::Error{transient:false, Rechecking, "invite.error.not_found"}``
    (``libs/fauna-onboarding-machine`` machine.rs recheck_invite_status).
  - ``+page.svelte`` ``persistInviteSlotIfActionable`` observes that exact
    shape and calls ``deletePendingInvite`` (per
    ``docs/goal/behavior/onboarding.md`` §3 "Persistence callouts").

A faked snapshot can't drive that end-to-end — the tier_2 sibling masked it
by injecting ``seed_identity`` + a fake Error snapshot, so its scenario-3
variant could only ``pytest.skip``. Only the full stack exercises the
launch-path identity seeding (Case 3) + anonymous WS-RPC recheck.

Per ``docs/goal/behavior/onboarding.md`` §"App-launch routing" Case 3
(identity + pending-invite, no nest_url → seed_identity + seed_pending_invite
→ wizard at InviteRequest).

Web-specific (other apps have their own per-platform store + regression
files). Lives in ``tests/web/`` so the cross-app conftest auto-deselects
for non-web apps.
"""

import json

from helpers import web_store
import secrets
import time

import pytest

pytestmark = [pytest.mark.web, pytest.mark.tier_3]


# Cosmetic: the nest looks the invite request up by the derived actor id, not
# this handle; it only has to round-trip through the persisted slot.
TEST_HANDLE = "alice@nest.example.test"
TEST_REQUEST_ID = "req-orphan-001"


def _read_pending_invite_keys(app, actor: str) -> dict:
    """The identity's per-actor `fauna/{actor}/pending_invite` record as its
    four fields, every one None when the slot is absent."""
    record = web_store.read_record(app.driver, actor, "pending_invite") or {}
    return {k: record.get(k) for k in ("nest_url", "handle", "request_id", "status_json")}


def test_relaunch_after_recheck_404_clears_orphan_and_lands_at_handle_entry(
    app, nest_instance, spa_url
):
    """A stale PendingReview slot whose actor has no invite request on the nest
    is cleared by a real recheck (404), and the next relaunch — now identity-only
    — lands the wizard at handle_entry."""
    # Real identity secret: its derived Ed25519 actor has NO invite request on
    # the fresh session nest, so recheck → 404.
    secret_hex = secrets.token_hex(32)

    # Pre-seed identity + a stale PendingReview slot. nest_url MUST be the SPA
    # proxy origin (same-origin): the wizard's anonymous WS-RPC opens
    # /api/v1/ws against it and the proxy splices that to the real nest. A
    # direct nest URL would be cross-origin-blocked in the browser.
    actor = web_store.seed_identity(app.driver, secret_hex)
    pending_status = json.dumps(
        {"PendingReview": {"request_id": TEST_REQUEST_ID, "last_checked_ms": 0}}
    )
    web_store.seed(
        app.driver,
        {
            f"fauna/{actor}/pending_invite": web_store.pending_invite_record(
                nest_url=spa_url,
                handle=TEST_HANDLE,
                request_id=TEST_REQUEST_ID,
                status_json=pending_status,
            )
        },
    )

    # Relaunch → launch-path Case 3 (identity + pending-invite, no node_url):
    # seed_identity + seed_pending_invite → wizard at InviteRequest with the
    # recheck button (PendingReview).
    app.driver.hard_reload()
    assert app.is_visible("invite-request-recheck-button"), (
        "wizard did not re-seed at InviteRequest/PendingReview after relaunch; "
        f"error={app.error_text()!r}"
    )

    # Real recheck: anonymous WS-RPC → nest invite-status handler → 404 for the
    # unknown actor → Error{Rechecking, not_found} → persistInviteSlotIfActionable
    # → deletePendingInvite. (Requires Case 3 to have seeded the identity — a
    # machine without a secret returns Error{"no identity"} and never clears
    # the slot.)
    app.click("invite-request-recheck-button")

    deadline = time.monotonic() + 15
    keys = _read_pending_invite_keys(app, actor)
    while time.monotonic() < deadline and not all(v is None for v in keys.values()):
        time.sleep(0.3)
        keys = _read_pending_invite_keys(app, actor)

    snap = app.driver.call_machine_method("inviteRequestSnapshot")
    assert all(v is None for v in keys.values()), (
        "orphan pending-invite slot not cleared after recheck-404; "
        f"keys={keys} invite_snapshot={snap}"
    )

    # Relaunch again: no slot, identity-only → Case 4 → wizard at handle_entry.
    app.driver.hard_reload()
    assert app.is_visible("handle-input"), (
        "expected handle_entry after the orphan slot was cleared; "
        f"error={app.error_text()!r}"
    )
