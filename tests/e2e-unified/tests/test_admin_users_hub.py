"""Consolidated `admin-users` hub — the three sections over the shared
`fauna-client-admin` `AdminClient` (admin.md § Users; tracked internally).

"Who's on / who wants on / let someone on", all framed around assigning a TIER:
  - Users:            change a user's tier (`fauna.admin.users.update`).
  - Invite:           mint a code → copy token (`fauna.admin.invite_codes.create`).
  - Pending requests: approve a request at a chosen tier (`fauna.admin.invite_requests.approve`).

tier_3: drives the real linux UI against a real nest; the tier pickers are
tree-safe cycle buttons (a GtkDropDown value can't be set headless — AT-SPI's
`select()` is a headless no-op on the Linux dev VM).
"""

import secrets
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _fresh_handle(prefix: str) -> str:
    """A per-submission handle — NEVER a literal.

    `nest_instance` is ``scope="session"`` (`conftest.py`) and is shared by every
    app parametrization, while the nest refuses a handle already assigned to an
    actor: `submit_invite_request_core`'s early rejection, surfaced as
    `fauna.account.handle_taken` (`bins/fauna-nest/src/invite_core.rs`). A
    hard-coded handle therefore admits on whichever app happens to run first and
    makes the SAME test unrunnable on every app after it — the refusal is raised
    inside `_submit_invite_request`, before a single assertion, so it reads as a
    product bug in the app rather than as this module colliding with itself.

    Measured 2026-08-17: 9/9 green on one app, 7-of-9
    red *per app* across a three-app sweep, entirely from that collision. The
    literal handles are why the module looked broken on all three apps at once,
    which in turn is what made an all-app failure look like nest-side evidence.
    """
    return f"{prefix}{secrets.token_hex(3)}"


def _submit_invite_request(nest_url: str, handle: str, message: str = "let me in") -> str:
    """Submit a real Ed25519-signed invite request (the requester's onboarding
    path), so the admin hub has a pending row to approve. Mirrors the signing in
    tests/api/test_invite_requests.py.

    Rides the anonymous (pre-identity) WS-RPC kind `fauna.account.invite_request.submit`
    — the HTTP twin `POST /api/v1/invite-requests` was retired in the no-HTTP
    migration (`bins/fauna-nest/src/lib.rs` § retired user-side invite twins), so a
    raw POST now hits the SPA fallback (200, nothing created). `anon.call` raises
    `RpcCallError` on a rejection.

    Returns the requester's full actor hex, so the caller can address the row
    that requester becomes by IDENTITY rather than by position
    (`admin.user_row_index`)."""
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    ts = int(time.time() * 1000)
    from common.sig_domain import invite_submit_signed_message

    msg = invite_submit_signed_message(bytes.fromhex(actor_hex), handle, message, ts)
    sig = sk.sign(msg).signature.hex()
    with WsRpcAnonClient(nest_url) as anon:
        anon.call(
            "fauna.account.invite_request.submit",
            {
                "actor_id": actor_hex,
                "handle": handle,
                "message": message,
                "timestamp": ts,
                "signature": sig,
            },
        )
    return actor_hex


@pytest.mark.feature("admin-users")
def test_change_user_tier(admin_app):
    """Users section: changing a user's tier via the row picker persists.

    The admin is always a registered user, so user-row[0] exists. Cycling its
    `admin-users-tier-select` applies the new tier through `fauna.admin.users.update`
    and the refetched row reflects it.
    """
    admin_app.admin.navigate_users()
    assert admin_app.admin.user_count() >= 1

    before = admin_app.admin.user_tier(0)
    # Pick a target distinct from the current tier (default tiers: free/personal/community).
    target = "personal" if before != "personal" else "community"
    admin_app.admin.set_user_tier(target, index=0)

    # The row rebuilt from the refetch shows the applied tier — proves the
    # update round-tripped (not just an optimistic label flip).
    assert admin_app.admin.user_tier(0) == target, (
        f"tier did not persist: wanted {target}, got {admin_app.admin.user_tier(0)!r}. "
        f"error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-users")
def test_mint_code_and_copy(admin_app):
    """Invite section: mint a code at a chosen tier and surface it copyable."""
    admin_app.admin.navigate_invite_codes()
    initial = admin_app.admin.invite_code_count()
    code = admin_app.admin.create_invite_code(tier="personal", uses=3)
    assert len(code) > 0, f"mint returned no token. error: {admin_app.error_text()!r}"
    assert admin_app.admin.invite_code_count() == initial + 1
    assert admin_app.admin.copy_button_visible()
    admin_app.admin.copy_minted_code()


@pytest.mark.feature("admin-users")
def test_mint_code_with_guardian_and_age_band(admin_app, nest_instance):
    """Invite section: the age-band select (`admin-users-invite-age-band-select`)
    is un-pickable until a guardian is chosen, then a mint carries the picked
    band beside the guardian — the code's row echoes it and the nest recorded it
    (family-safety.md § App surface → *Age-band surfaces*, D2: a band presupposes
    a guardianship link, so the picker gates exactly as the nest refuses).
    """
    from helpers.admin_wire import admit_adult, invite_code_age_band

    guardian_handle = _fresh_handle("bandguardian")
    admit_adult(nest_instance, guardian_handle)

    admin_app.admin.navigate_invite_codes()
    admin_app.admin.open_invite_form()
    assert admin_app.admin.invite_age_band() == "not-set", (
        f"a fresh form starts with no band: {admin_app.admin.invite_age_band()!r}"
    )
    assert not admin_app.admin.invite_age_band_enabled(), (
        "the band select must be disabled while no guardian is selected. "
        f"error: {admin_app.error_text()!r}"
    )
    admin_app.admin.set_invite_guardian(guardian_handle)
    assert admin_app.admin.invite_age_band_enabled(), (
        "choosing a guardian must enable the band select. "
        f"error: {admin_app.error_text()!r}"
    )

    code = admin_app.admin.mint_invite_code(tier="free", uses=1, age_band="u13")
    assert code, f"mint (with guardian + band) returned no token. error: {admin_app.error_text()!r}"
    assert invite_code_age_band(nest_instance, code) == "u13", (
        "the nest must record the picked band on the minted code"
    )
    row = admin_app.admin.invite_code_item_text(code)
    assert "Under 13" in row, f"the code row should echo the band label: {row!r}"


@pytest.mark.feature("admin-users")
def test_approve_invite_request_at_tier(admin_app, nest_instance):
    """Pending requests section: a seeded request can be approved at a tier.

    Approving admits the requester (creating the account at the chosen tier) and
    removes the row; the requester then appears in the Users section.
    """
    handle = _fresh_handle("joiner")
    actor_hex = _submit_invite_request(nest_instance["url"], handle=handle)

    admin_app.admin.navigate_users()  # the hub renders all three sections
    # Poll for THIS request to surface — never for a bare count. The section is
    # shared state (sibling tests and the other app parametrizations seed rows
    # into it), so `count < 1` is already false when someone else's row is
    # sitting there and the loop exits before this request has arrived.
    deadline = time.monotonic() + 15.0
    while (time.monotonic() < deadline
           and handle not in admin_app.admin.invite_request_handles()):
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    assert handle in admin_app.admin.invite_request_handles(), (
        f"seeded request {handle!r} not visible after 15s. "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )

    row = admin_app.admin.invite_request_row_index(handle)
    admin_app.admin.set_request_tier("personal", index=row)
    admin_app.admin.approve_request(index=row)

    # Refetch the hub; the request is gone and THIS requester has a row. Polled
    # by identity, never by a row count: the list shows one page, so once the
    # session nest holds a full page of accounts the count stops growing.
    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if admin_app.admin.has_user_row(actor_hex):
            break
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    assert admin_app.admin.has_user_row(actor_hex), (
        f"approved requester {actor_hex[:12]}… did not appear in the user list. "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )
    admin_app.admin.navigate_users()
    assert handle not in admin_app.admin.invite_request_handles()


def _admit_user(admin_app, nest_instance, prefix: str) -> str:
    """Admit a fresh user through the real admission flow (invite request →
    admin approves at a tier), then land back on the Users section. Returns the
    admitted actor's full hex id.

    Callers address that user's row by IDENTITY (`admin.user_row_index`), never
    by position: the list is ``created_at DESC``, but `created_at` is
    second-granular, so a user admitted in the same second as the claim ties with
    the admin and SQLite leaves the order among ties unspecified. Session-scoped
    sibling tests — and the other app parametrizations, which share this one nest
    — put rows in this list too, so "the non-admin row" is not a well-defined
    thing to reach for.
    """
    handle = _fresh_handle(prefix)
    actor_hex = _submit_invite_request(nest_instance["url"], handle=handle)

    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    while (time.monotonic() < deadline
           and handle not in admin_app.admin.invite_request_handles()):
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    assert handle in admin_app.admin.invite_request_handles(), (
        f"seeded request {handle!r} never surfaced. "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )

    # Approve THIS request's row. A failed approval leaves its request `pending`
    # (`admin.md` § 2 Section 1), so index 0 is the earliest stuck row, not this
    # one — approving that instead re-runs an old failure once per test, which is
    # exactly how a single stuck row reads as a whole broken module.
    row = admin_app.admin.invite_request_row_index(handle)
    admin_app.admin.set_request_tier("personal", index=row)
    admin_app.admin.approve_request(index=row)

    # Poll for THIS actor's row, not for a bare count: the list already carries
    # every earlier test's and every other app's admissions, so `count >= 2` is
    # true before this approval lands and would let a failed approve pass.
    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if admin_app.admin.has_user_row(actor_hex):
            return actor_hex
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    raise AssertionError(
        f"admitted {handle!r} ({actor_hex[:12]}…) did not appear in the user "
        f"list. error: {admin_app.error_text()!r} "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )


@pytest.mark.feature("admin-users")
def test_evict_and_cancel_eviction(admin_app, nest_instance):
    """Users section: a user's eviction can be started and cancelled.

    Eviction is a warn→suspend→delete timeline, so the row persists after evict,
    now showing the cancel control.
    """
    # The row this test admitted, by identity — not "some non-admin row". The
    # admin can tie into any slot and evicting the admin is refused
    # (fauna.admin.conflict), which would read as a product bug rather than the
    # ordering luck it is; and a row a sibling test left mid-eviction would fail
    # the pre-condition below for reasons that have nothing to do with evicting.
    actor_hex = _admit_user(admin_app, nest_instance, "evictee")
    row = admin_app.admin.user_row_index(actor_hex)
    assert not admin_app.admin.user_eviction_active(row), "the row should start un-evicting"

    # Evict → the row flips to the cancel-eviction control (user not deleted).
    admin_app.admin.evict_user(row)
    assert admin_app.admin.user_eviction_active(row), (
        f"eviction did not start: the row still shows the evict control. "
        f"error: {admin_app.error_text()!r}"
    )

    # Cancel → the row flips back to the evict control.
    admin_app.admin.cancel_user_eviction(row)
    assert not admin_app.admin.user_eviction_active(row), (
        f"cancel did not clear the eviction: the row still shows cancel. "
        f"error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-users")
def test_a_row_whose_deletion_has_begun_offers_no_restore(admin_app, nest_instance):
    """Once an evicted account's deletion has begun it can no longer be restored,
    and its row offers no restore (`admin.md` § Cutting a user off — Restore "is
    refused once the ladder has reached `deleting`… The row then offers no
    restore control").

    `deleting` is reached 28 days into the ladder and is normally finalized in
    the same tick, so the row is made to stay there the way production holds
    one: the evictee guards a ward, and the finalize refuses to strand a
    supervised account. The link is admitted AFTER the eviction starts — evict
    itself refuses a guardian whose links stand — and the 28 days pass through
    the `test-hooks` endpoint `/api/v1/test/eviction/advance`, which moves this
    one account's clock and runs the production eviction tick. The evict is the
    admin's UI gesture; what is asserted is the row the app paints.
    """
    from helpers.admin_wire import (
        admit_adult,
        admit_ward_directly,
        eviction_state,
        graduate_ward,
        post_nest_hook,
    )

    day = 24 * 3600
    evictee = admit_adult(nest_instance, _fresh_handle("deletingguardian"))["actor_id_hex"]
    admin_app.admin.navigate_users()
    row = admin_app.admin.user_row_index(evictee)
    admin_app.admin.evict_user(row)
    assert admin_app.admin.user_eviction_active(row), (
        f"eviction did not start. error: {admin_app.error_text()!r}"
    )

    ward = admit_ward_directly(nest_instance, evictee, _fresh_handle("deletingward"))["actor_id_hex"]
    try:
        assert post_nest_hook(nest_instance, "eviction/advance",
                              {"actor_id": evictee, "secs": 14 * day + 60})["shifted"]
        post_nest_hook(nest_instance, "eviction/advance", {"actor_id": evictee, "secs": 14 * day + 60})
        assert eviction_state(nest_instance, evictee) == "deleting", (
            "the ladder should stand at deleting with the finalize held by the ward; "
            f"it reports {eviction_state(nest_instance, evictee)!r}"
        )

        # The refetched row: no restore, and — the account being on its way out —
        # no other cut-off entry point either.
        admin_app.admin.navigate_users()
        row = admin_app.admin.user_row_index(evictee)
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline and admin_app.admin.user_eviction_active(row):
            time.sleep(0.4)
            admin_app.admin.navigate_users()
            row = admin_app.admin.user_row_index(evictee)
        assert not admin_app.admin.user_eviction_active(row), (
            "a row whose deletion has begun must not offer restore "
            f"(admin-users-cancel-eviction-button). error: {admin_app.error_text()!r}"
        )
        assert not admin_app.admin.user_evict_available(row), (
            "a row whose deletion has begun must not offer evict"
        )
        assert not admin_app.admin.user_suspend_available(row), (
            "a row whose deletion has begun must not offer suspend"
        )
    finally:
        # Release the hold so the shared session nest finishes the deletion
        # rather than retrying a held finalize for the rest of the run.
        graduate_ward(nest_instance, ward)
        post_nest_hook(nest_instance, "eviction/advance", {"actor_id": evictee, "secs": 0})


@pytest.mark.feature("admin-users")
def test_suspend_and_restore_user(admin_app, nest_instance):
    """Users section: a user can be cut off *immediately* and restored.

    `admin.md` § 2 → *Cutting a user off*: Suspend enters the eviction machine's
    `suspended` state at once with **no** delete timeline — the "stop this account
    now" path the timed ladder cannot serve. Restore is the *same*
    `admin-users-cancel-eviction-button` the evict path uses: one cut-off state,
    one exit, from either entry point.

    That the suspended user then loses all dispatch is enforced (and proven)
    nest-side at `caller_class_for_actor` — `conformance_suspension.rs`. What this
    test pins is the client leg the nest cannot: the button renders, hits
    `fauna.admin.users.suspend`, and the row round-trips through the real UI.
    """
    # The row this test admitted, by identity — suspending the admin is refused
    # (fauna.admin.conflict), the admin can tie into any slot, and a row a
    # sibling test left suspended would fail the pre-condition below.
    actor_hex = _admit_user(admin_app, nest_instance, "mallory")
    row = admin_app.admin.user_row_index(actor_hex)
    assert admin_app.admin.user_suspend_available(row), (
        f"the row should offer Suspend before any cut-off. error: {admin_app.error_text()!r}"
    )
    assert not admin_app.admin.user_eviction_active(row), "the row should start un-suspended"

    # Suspend → the row flips to the restore control (shared with evict).
    admin_app.admin.suspend_user(row)
    assert admin_app.admin.user_eviction_active(row), (
        f"suspend did not take: the row shows no restore control. "
        f"error: {admin_app.error_text()!r}"
    )
    # A suspended row offers no second suspend — the nest's `suspend_user_now`
    # matches `eviction_status IN ('', 'warning')`, so it would be a no-op.
    assert not admin_app.admin.user_suspend_available(row), (
        "a suspended row must not offer Suspend again"
    )

    # Restore → the row is a normal user again, offering both entry points.
    admin_app.admin.cancel_user_eviction(row)
    assert not admin_app.admin.user_eviction_active(row), (
        f"restore did not clear the suspension. error: {admin_app.error_text()!r}"
    )
    assert admin_app.admin.user_suspend_available(row), (
        "a restored row offers Suspend again"
    )


@pytest.mark.feature("admin-users")
def test_admin_row_offers_no_cut_off_controls(admin_app, nest_instance):
    """An admin can be neither suspended nor evicted (`fauna.admin.conflict`) —
    a suspended *sole* admin could never be restored by anyone, so `admin.md` § 2
    makes the state unrepresentable: remove the admin role first.

    The row must therefore not *offer* either control. Pins the whole chain the
    `AdminUser.is_admin` projection exists for: nest `admin_actor_ids` → the wire
    flag → the shared `admin_user_row_controls` decision → the rendered row.

    Both rows are located by ACTOR ID, never by position — see
    `admin.admin_row_index`. (This test was fleet-wide red on linux and web until
    2026-07-16 for exactly that reason: it took the admin as the *last* row, but
    same-second `created_at` ties leave the order unspecified, so it was
    ordering-luck flaky rather than pinning a real bug — the shared decision itself
    was always correct.)
    """
    admin_hex = bytes(nest_instance["admin"]["signing_key"].verify_key).hex()
    plain_hex = _admit_user(admin_app, nest_instance, "plainuser")
    # Each row is read straight after its own lookup: the two accounts may sit on
    # different pages of the list, and a lookup leaves the list on its row's page.
    admin_row = admin_app.admin.admin_row_index(admin_hex)

    assert not admin_app.admin.user_suspend_available(admin_row), (
        "the admin's own row must not offer Suspend — the nest refuses it with "
        "fauna.admin.conflict, so the button would be dead"
    )
    assert not admin_app.admin.user_evict_available(admin_row), (
        "the admin's own row must not offer Evict either"
    )
    # ...and the guard is specific to the admin, not a blanket hiding of controls.
    plain_row = admin_app.admin.user_row_index(plain_hex)
    assert admin_app.admin.user_suspend_available(plain_row), (
        f"the plain user's row still offers Suspend. error: {admin_app.error_text()!r}"
    )
    assert admin_app.admin.user_evict_available(plain_row), (
        "the plain user's row still offers Evict"
    )


@pytest.mark.feature("admin-users")
def test_make_admin_offers_and_schedules_without_error(admin_app, nest_instance):
    """Users section: the roster surface's grant instrument
    (`admin.md` § Admin continuity and succession, instrument 1).

    `fauna.admin.admins.add` schedules an `AdminAdd` pending action with a 24h
    delay (`bins/fauna-nest/src/pending_actions.rs`; the API-layer contract is
    pinned in `tests/api/test_admin_auth.py::test_admin_management`) — the
    target is NOT immediately admin. So this test pins what the UI leg
    actually owns: a plain row offers `admin-users-make-admin-button` (and not
    `-remove-admin-button`), clicking it schedules the grant with no error, and
    the row still reads as a plain (non-admin) row immediately after — the
    24h execution itself is out of e2e's reach and is the Rust test's job.
    """
    # The row this test admitted, by identity — an admin row offers
    # remove-admin, not make-admin, so picking "some non-admin row" is the
    # difference between testing the grant and testing the ordering.
    actor_hex = _admit_user(admin_app, nest_instance, "futureadmin")
    row = admin_app.admin.user_row_index(actor_hex)

    assert admin_app.admin.user_make_admin_available(row), (
        f"a plain row should offer make-admin. error: {admin_app.error_text()!r}"
    )
    assert not admin_app.admin.user_remove_admin_available(row), (
        "a plain row must not offer remove-admin"
    )

    admin_app.admin.make_admin(row)

    assert not admin_app.admin.users_action_error_text(), (
        f"granting admin should schedule cleanly, not error: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )
    # The grant is a scheduled pending action (24h delay) — the row is still a
    # plain row right after the click, not yet promoted.
    assert admin_app.admin.user_make_admin_available(row), (
        "the row should still offer make-admin — the grant has not executed yet"
    )


@pytest.mark.feature("admin-users")
def test_remove_admin_refused_at_last_superadmin(admin_app, nest_instance):
    """Users section: the roster surface's revoke instrument refuses to strand
    the deployment at zero superadmins.

    Unlike the grant, `fauna.admin.admins.remove` enforces the roster floor
    **synchronously** — `admin_ws_handlers::admins_remove_handler` calls
    the target-aware `can_remove_admin` before scheduling anything (and the
    writer re-refuses at execution — the floor's authoritative line), so with the single
    superadmin a fresh nest starts with (the only count reachable from e2e —
    a second admin only joins after a 24h grant executes), the refusal is
    immediate and testable through the real UI, unlike the grant's eventual
    effect above.
    """
    admin_hex = bytes(nest_instance["admin"]["signing_key"].verify_key).hex()
    admin_app.admin.navigate_users()
    admin_row = admin_app.admin.admin_row_index(admin_hex)

    assert admin_app.admin.user_remove_admin_available(admin_row), (
        f"the admin's own row should offer remove-admin. error: {admin_app.error_text()!r}"
    )
    assert not admin_app.admin.user_make_admin_available(admin_row), (
        "an admin row must not offer make-admin"
    )

    admin_app.admin.remove_admin(admin_row)

    err = admin_app.admin.users_action_error_text()
    assert err, (
        "expected the last-superadmin removal to be refused via "
        f"admin-users-action-error, but it was empty. global error-message: "
        f"{admin_app.error_text()!r}"
    )
    assert not admin_app.has_error(), (
        f"the refusal leaked into the global error-message banner: {admin_app.error_text()!r}"
    )
    # Nothing changed — the admin's own row still offers remove-admin.
    assert admin_app.admin.user_remove_admin_available(admin_row), (
        "a refused removal must not alter the row's controls"
    )


def _wait_for_request(admin_app, handle: str) -> int:
    """Re-open the hub until the pending request for ``handle`` paints; return
    its row. Polls for THIS request, never a bare count (the section is shared
    state — see `test_approve_invite_request_at_tier`)."""
    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    while (time.monotonic() < deadline
           and handle not in admin_app.admin.invite_request_handles()):
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    return admin_app.admin.invite_request_row_index(handle)


def _requester_status(nest_url: str, actor_hex: str) -> dict:
    """The requester's own view of their request (`invite_request.status`, the
    anonymous read the onboarding wizard polls) — what the admin's decision
    tells the person who asked."""
    with WsRpcAnonClient(nest_url) as anon:
        return anon.call("fauna.account.invite_request.status", {"actor_id": actor_hex})


@pytest.mark.feature("admin-users")
def test_deny_invite_request_with_a_reason(admin_app, nest_instance):
    """Pending requests section: the admin turns a request down from the page,
    with a reason (`admin.md` § 2. Users — deny attaches the optional reason).

    The row leaves the pending list, and the requester's own status read says
    denied and carries the reason the admin typed — the reason crossed the UI,
    the wire and the nest intact."""
    handle = _fresh_handle("turneddown")
    actor_hex = _submit_invite_request(nest_instance["url"], handle=handle)
    row = _wait_for_request(admin_app, handle)

    reason = f"not this time {secrets.token_hex(3)}"
    admin_app.admin.deny_request(index=row, reason=reason)

    deadline = time.monotonic() + 15.0
    status = _requester_status(nest_instance["url"], actor_hex)
    while time.monotonic() < deadline and status.get("status") != "denied":
        time.sleep(0.4)
        status = _requester_status(nest_instance["url"], actor_hex)
    assert status.get("status") == "denied", (
        f"the request was not denied: {status!r}. page error: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )
    assert status.get("denial_reason") == reason, (
        f"the requester must see the admin's reason {reason!r}: {status!r}"
    )
    admin_app.admin.navigate_users()
    assert handle not in admin_app.admin.invite_request_handles(), (
        "a denied request must leave the pending list"
    )


@pytest.mark.feature("admin-users")
def test_approving_a_request_whose_handle_was_taken_since_fails_on_the_page(
        admin_app, nest_instance):
    """Pending requests section: a request whose handle someone else took after
    it was made fails to approve — the page's own error line says so, and the
    request stays waiting (`admin.md` § 2. Users — approval re-validates the
    handle).

    The handle is taken by a direct admission over the admin control plane:
    the arrangement, not the act under test (convention 8)."""
    from helpers.admin_wire import admit_adult

    handle = _fresh_handle("snapped")
    actor_hex = _submit_invite_request(nest_instance["url"], handle=handle)
    admit_adult(nest_instance, handle)

    row = _wait_for_request(admin_app, handle)
    admin_app.admin.approve_request(index=row)

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not admin_app.admin.users_action_error_text():
        time.sleep(0.3)
    assert admin_app.admin.users_action_error_text(), (
        f"approving a request whose handle is taken must fail on "
        f"admin-users-action-error; global banner: {admin_app.error_text()!r}"
    )
    assert not admin_app.has_error(), (
        f"the failure belongs on the Users page's own line: {admin_app.error_text()!r}"
    )
    assert _requester_status(nest_instance["url"], actor_hex).get("status") == "pending", (
        "the refused approval must leave the request waiting"
    )
    admin_app.admin.navigate_users()
    assert handle in admin_app.admin.invite_request_handles(), (
        f"the request must stay on the pending list. "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )


@pytest.mark.feature("admin-users")
def test_delete_invite_code(admin_app, nest_instance):
    """Invite section: a code the admin minted is deleted from its row and
    leaves the list — on the page and on the nest (`admin.md` § 2. Users)."""
    from helpers.admin_wire import admin_client

    admin_app.admin.navigate_invite_codes()
    code = admin_app.admin.create_invite_code(tier="free", uses=1)
    assert code, f"mint returned no token. error: {admin_app.error_text()!r}"

    admin_app.admin.delete_invite_code(code)

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and code in admin_app.admin.invite_code_values():
        time.sleep(0.3)
    assert code not in admin_app.admin.invite_code_values(), (
        f"the deleted code still lists. page error: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )
    with admin_client(nest_instance) as client:
        listed = [c.get("code") for c in
                  client.call("fauna.admin.invite_codes.list", {}).get("invite_codes", [])]
    assert code not in listed, f"the nest still holds the deleted code: {listed!r}"


@pytest.mark.feature("admin-users")
def test_a_suspended_member_is_never_offered_as_a_guardian(admin_app, nest_instance):
    """The guardian picker never offers a suspended member (`admin.md` § 2.
    Users — a picker offers every NON-suspended account).

    Two fresh members; the admin suspends one from its row, then opens the
    invite form. The picker is read once it paints the other member — proof it
    reflects the post-suspension roster — and the suspended one is absent."""
    from helpers.admin_wire import admit_adult

    kept = _fresh_handle("keptguardian")
    cut = _fresh_handle("cutguardian")
    admit_adult(nest_instance, kept)
    cut_hex = admit_adult(nest_instance, cut)["actor_id_hex"]

    admin_app.admin.navigate_users()
    row = admin_app.admin.user_row_index(cut_hex)
    admin_app.admin.suspend_user(row)
    assert admin_app.admin.user_eviction_active(row), (
        f"the suspension did not take. error: {admin_app.admin.users_action_error_text()!r}"
    )

    admin_app.admin.navigate_invite_codes()
    admin_app.admin.open_invite_form()
    deadline = time.monotonic() + 15.0
    options = admin_app.admin.invite_guardian_options() or []
    while time.monotonic() < deadline and kept not in options:
        time.sleep(0.3)
        options = admin_app.admin.invite_guardian_options() or []
    assert kept in options, f"the picker never painted {kept!r}: {options!r}"
    assert cut not in options, (
        f"a suspended member must never be offered as a guardian: {options!r}"
    )


@pytest.mark.feature("admin-users")
def test_users_pagination_controls(admin_app):
    """Users section: the pagination controls render, are guarded at the lower
    bound, and round-trip.

    What this test owns is the WIRING — that the two buttons reach the shared
    offset math and the list comes back — not the bound arithmetic itself, which
    is pinned at tier_1 on `fauna_core::format::{next_page_offset,
    prev_page_offset}` (`libs/fauna-core/src/format.rs`).

    Deliberately page-count agnostic. The older form asserted "the user set never
    changes", which silently assumed the nest holds a single page — a premise
    `nest_instance` stops honouring the moment the session admits more than
    `USERS_PAGE_SIZE` users, and it is session-scoped and shared by every app
    parametrization, so the count only ever grows. `next` → `prev` returning to
    the same page is the invariant that holds either way.

    ⚠ **The bound is asserted, not driven through** (2026-08-28). This test used
    to click `prev` at offset 0 and call the result "a guarded no-op", relying on
    the click handler's own bound check because "a reliable disabled-state read
    isn't available on the Linux AT-SPI bridge". That premise is dead: the app
    reflects the bound with `set_sensitive` and `is_enabled` reads it faithfully
    (convention 11's gate is built on exactly that read). So clicking at the
    bound was the harness doing what no user can — the permissive actuation sweep
    of 2026-08-28 caught it, three `DISABLED-ACTUATION` markers in this one test.
    Asserting the disabled state is also strictly better coverage: it pins the
    guard the user actually meets, not the fallback behind it.
    """
    admin_app.admin.navigate_users()
    assert admin_app.admin.pagination_present(), (
        f"pagination container missing. error: {admin_app.error_text()!r}"
    )
    first_page = admin_app.admin.user_actor_ids()

    # Lower bound: at offset 0 the app DISABLES prev. That is the guard a user
    # meets, so assert it rather than clicking through it.
    assert not admin_app.admin.prev_page_enabled(), (
        f"prev must be disabled at offset 0 — that is how the lower bound is "
        f"expressed to the user. "
        f"{admin_app.driver.diagnose('admin-users-prev-page', attrs=('enabled',))}"
    )

    if not admin_app.admin.next_page_enabled():
        # Single page: both bounds are disabled and there is no round trip to
        # make. Asserting that is the whole of this run's wiring claim.
        assert admin_app.admin.user_actor_ids() == first_page, (
            f"the page must not change when neither control is live. "
            f"error: {admin_app.error_text()!r}"
        )
        assert not admin_app.has_error(), (
            f"pagination raised: {admin_app.error_text()!r}"
        )
        return

    # Several pages: forward and back lands on the page we started from, and
    # prev must have come alive once we left offset 0.
    admin_app.admin.next_page()
    assert admin_app.admin.prev_page_enabled(), (
        f"prev must be enabled once we are past offset 0. "
        f"{admin_app.driver.diagnose('admin-users-prev-page', attrs=('enabled',))}"
    )
    admin_app.admin.prev_page()
    assert admin_app.admin.user_actor_ids() == first_page, (
        f"next→prev did not return to the first page. "
        f"error: {admin_app.error_text()!r}"
    )
    assert not admin_app.has_error(), f"pagination raised: {admin_app.error_text()!r}"
