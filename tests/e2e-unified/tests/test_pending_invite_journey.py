"""The pending-invite journey, driven against a real nest — the two journeys
`onboarding.md` § Implementation status today records as still owed.

`onboarding.md` § The pending-invite surface (ratified 2026-08-11) makes two
promises that nothing exercised end to end before this module:

* **Approval is detected as admission.** The admin approve creates the account
  and *deletes* the request row, so the requester's poll sees `not_found`; the
  machine runs the registered-probe (`fauna.auth.challenge` + `fauna.auth.verify`
  on the identity it already holds) before rendering any error, and a successful
  verify **is** the login — `wizard_outcome() = LoggedIn` / `OnboardingStep::Done`.
  This is what makes `admin.md` § Architectural rules 5 ("approving must trigger
  the requester's onboarding flow to advance") true. Until 2026-08-12 the page
  rendered a terminal error on approval instead.
* **A denied requester can resubmit.** The nest refuses a submit while any row
  for the actor exists, so `wizard_submit_invite_request` sends the signed
  `invite_request.cancel` first when the state is `Denied`.

What existed before: `test_invite_request_states.py` *injects* snapshots (tier_2,
never drives the probe), and its `Approved` case pretended to cover approval
using a variant no live nest ever serves (the approve deletes the row).
`test_invite_request_submit_roundtrip.py` drives the real submit but stops at
`PendingReview`. So the approve→advance journey had never had a green e2e, which
is exactly what `onboarding.md` § Implementation status today → *Test coverage*
flags with a ⚠.

**tier_3** — real `fauna-nest`, real wire, real app UI. The requester's half is
driven entirely through the app (convention 8); the admin's approve/deny is the
counterparty precondition and rides a direct admin WS-RPC client, the same
arrangement `test_admin_users_action_error.py` uses (and the one row 236 names).

**Convention 14.** No settle-sleeps: every wait is a deadline poll on state the
app actually reaches, under a named budget that a green run never spends.
"""

import json
import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.launch_harness import reached_authenticated_app

pytestmark = pytest.mark.tier_3

# The wizard's own submit → PendingReview round trip: one anonymous WS call.
SUBMIT_BUDGET_S = 30.0

# The unattended-approval budget. A **ceiling, not a mirror** of
# `fauna_onboarding_machine::INVITE_RECHECK_POLL_MS` (30 s): sized so one whole
# missed tick plus nest + probe latency still passes, and spent only by a genuine
# failure — a green run exits the moment the app flips `session.authenticated`.
UNATTENDED_APPROVAL_BUDGET_S = 150.0

# A driven recheck (the user pressed the button) — one nest round trip.
RECHECK_BUDGET_S = 30.0


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _pending_request_id(admin_ws, handle: str) -> int:
    """The id of `handle`'s pending row, or raise naming what the nest holds.

    Convention 6 — the two ways this fails (the submit never reached the nest;
    the row is there but non-pending) need different fixes and a bare
    `StopIteration` tells them apart for nobody."""
    rows = admin_ws.call("fauna.admin.invite_requests.list", {})["invite_requests"]
    for row in rows:
        if row["handle"] == handle and row["status"] == "pending":
            return row["id"]
    raise AssertionError(
        f"no PENDING invite request for {handle!r} on the nest. "
        f"rows for this handle: {[r for r in rows if r['handle'] == handle]!r}; "
        f"all handles: {sorted(r['handle'] for r in rows)!r}"
    )


def _drive_app_to_pending_review(app, nest_url: str, handle: str) -> None:
    """Import a fresh identity through the real onboarding UI, jump the wizard to
    `invite_request` for this claimed nest, and drive the REAL submit — the same
    arrangement as `test_invite_request_submit_roundtrip.py`, which is where the
    `navigate_to_invite_request_for_known_nest` hop is explained (a test nest is
    not DNS-discoverable, so the wizard cannot find it by handle)."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(bytes(SigningKey.generate()).hex())
    app.driver.wait_for("handle-input", timeout=15)

    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_url, handle]),
    )
    app.driver.wait_for("invite-request-submit-button", timeout=20)
    app.driver.click("invite-request-submit-button")

    # PendingReview is the only state that reveals the recheck affordance
    # (`invite_request.rs` gates it on `snap.recheck_visible`), so it is the
    # honest witness that the real `invite_request.submit` landed.
    _wait_visible(
        app,
        "invite-request-recheck-button",
        SUBMIT_BUDGET_S,
        "the real invite_request.submit never reached PendingReview",
    )


def _wait_visible(app, element_id: str, budget_s: float, what: str) -> None:
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if app.driver.is_visible(element_id):
            return
        time.sleep(0.3)
    raise AssertionError(
        f"{what}: {element_id!r} never rendered within {budget_s}s. "
        f"{app.driver.diagnose(element_id)} "
        f"status={_status_text(app)!r} error={app.error_text()!r}"
    )


def _status_text(app) -> str:
    try:
        return app.driver.get_text("invite-request-status")
    except Exception as exc:  # a read failure is itself a reportable fact
        return f"<unreadable: {exc}>"


@pytest.mark.feature("join-a-nest")
def test_admin_approval_advances_the_requester_with_no_user_action(app, nest_instance):
    """Submit → admin approves out of band → the app advances the user off the
    pending surface **on its own**, with no click in between.

    The whole point is the *absence* of a user action between the approve and
    the advance, so this test deliberately touches nothing until the app has
    moved by itself. Since 2026-09-21 it moves to § 3b-ter's one-tap trust
    offer (an approved request is a join — see the inline note below on why
    that is not the interstitial § The pending-invite surface rules out); the
    offer is then answered and the authenticated shell asserted, so both
    halves of the journey stay witnessed here.

    The arrival half waits via `reached_authenticated_app`, which polls
    `GET /app/state` — the tui agent serves that from a published snapshot
    without waking the main loop (`automation.rs::start_if_enabled`'s
    `app_state` hook reads the shared mutex directly), so it is the cheapest
    honest witness available.

    This used to be load-bearing, not merely a preference: an element read
    goes through `dispatch` → `recv_agent`, which spins the `tokio::select!`
    loop, and before the fix the invite-poll arm rebuilt its `sleep()`
    fresh every spin — so polling an element here would have restarted the
    very timer under test and starved it indefinitely. `main.rs`'s
    `periodic_tick` now reads its deadline off a persistent `Interval` owned
    outside the loop, so a competing element read no longer resets it; state
    stays the cheaper wait, but an element-based wait would pass too now.
    """
    # tui, linux, web, android and windows drive `recheck_invite_status` on
    # INVITE_RECHECK_POLL_MS as of 2026-08-12 / 2026-08-15 (windows); macOS/iOS's SwiftUI task-loop timer
    # (`OnboardingVM.pollPendingInviteWhileNeeded`) landed
    # 2026-08-23 — all 7 apps poll now, so this test's own
    # `skip_unbuilt` gate is retired (apple was last; matches the
    # precedent of deleting rather than widening a now-vacuous guard).

    app.driver.reset()
    handle = f"pendapprove-{int(time.time() * 1000)}"
    _drive_app_to_pending_review(app, nest_instance["url"], handle)

    # The counterparty's action, out of band — the admin is a different actor and
    # this is the precondition, not the behavior under test (convention 8).
    with _admin_client(nest_instance) as admin_ws:
        req_id = _pending_request_id(admin_ws, handle)
        approved = admin_ws.call("fauna.admin.invite_requests.approve", {"id": req_id})
    assert approved["handle"] == handle, (
        f"the approve did not admit {handle!r}: {approved!r}"
    )

    # NOTHING between the approve and the advance is a user action. The app
    # must notice by itself.
    #
    # Where it advances TO, since 2026-09-21, is the one-tap trust offer
    # rather than the shell: an approved request is a JOIN, and
    # `onboarding.md` § 3b-ter offers a joining user the same grant a claiming
    # admin gets. That is *not* the "Approved, press Continue" interstitial
    # § The pending-invite surface rules out, and this test's promise is
    # untouched — the ruled-out one gates ADMISSION behind a press, while this
    # one appears after the user is already admitted and costs nothing to
    # decline. The unattended stretch under test is the approve → advance one,
    # and it still contains no user action.
    try:
        app.onboarding.finish_joiner_trust_prompt(
            grant=False, timeout=UNATTENDED_APPROVAL_BUDGET_S
        )
        reached_authenticated_app(app.driver, timeout=UNATTENDED_APPROVAL_BUDGET_S)
    except Exception as exc:
        # Self-diagnosing (convention 6): the candidate causes need different
        # fixes. Still on the invite page ⇒ the poll never fired, or fired and the
        # registered-probe did not resolve the admission. On the trust offer ⇒ the
        # admission worked and the offer's own exit stranded the join. Off both
        # with no session ⇒ the machine reached Done and the app's own hand-off
        # dropped the user.
        surfaces = {
            eid: app.driver.is_visible(eid)
            for eid in ("invite-request-submit-button", "invite-request-recheck-button",
                        "trust-box-grant-button", "handle-input", "feed-tab")
        }
        pytest.fail(
            "an approved invite request never advanced the requester to the "
            f"authenticated app within {UNATTENDED_APPROVAL_BUDGET_S}s with no user "
            "action (onboarding.md § The pending-invite surface — 'approval is "
            "detected as admission'; admin.md § Architectural rules 5). "
            f"barrier: {exc}; visible: {surfaces!r}; "
            f"invite status: {_status_text(app)!r}; app error: {app.error_text()!r}"
        )

    # The arrival flag and the rendered surface must agree — a session flag over a
    # still-mounted wizard page is the vacuous-green shape the launch-routing
    # smoke module had to close once already.
    assert app.driver.is_absent("invite-request-submit-button"), (
        "the app reports an authenticated session while the invite_request page "
        "is still mounted; do not trust the flag until this is explained"
    )


def _seed_invite_request(nest_url: str, handle: str) -> dict:
    """Seed a real Ed25519-signed pending invite request over the anonymous
    kind — fixture setup for the admin-side row under test (convention 8: the
    admin's approve is the mutation driven through the UI; the requester's
    submit is the precondition). Returns the requester's identity so the test
    can read the admitted account's own `fauna.family.status` afterwards.
    Carries NO age claim, as every app but android/ios submits (D3)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient
    from common.sig_domain import invite_submit_signed_message

    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    message = "let me in"
    ts = int(time.time() * 1000)
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
    return {"signing_key": sk, "actor_id_hex": actor_hex}


@pytest.mark.feature("family-safety")
def test_admin_admits_a_request_at_a_band_the_row_shows_the_claim(admin_app, nest_instance):
    """The pending-request row (family-safety.md § App surface → *Age-band
    surfaces*, #2 + #3): a request submitted by an app with no store age
    signal reads "No app age verification" on `invite-request-row-age-claim`
    (D6 — absence is the signal the admitting admin sees) and its band select
    starts at `not-set`; the admin picks a guardian, then the band `13-15`, and
    approves; the admitted ward's own `fauna.family.status` reports the band
    with provenance `guardian-asserted` (the admitting adult's judgment, D5).
    """
    from helpers.admin_wire import admit_adult

    nest_url = nest_instance["url"]
    guardian_handle = f"bandguardian-{int(time.time() * 1000)}"
    admit_adult(nest_instance, guardian_handle)
    handle = f"bandward-{int(time.time() * 1000)}"
    ward = _seed_invite_request(nest_url, handle)

    admin_app.admin.navigate_users()
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
    assert admin_app.admin.request_age_claim(index=row) == "No app age verification", (
        f"a claim-less request must say so: {admin_app.admin.request_age_claim(index=row)!r}"
    )
    assert admin_app.admin.request_age_band(index=row) == "not-set", (
        f"no claim → the band select starts unset: {admin_app.admin.request_age_band(index=row)!r}"
    )

    admin_app.admin.set_request_guardian(guardian_handle, index=row)
    admin_app.admin.set_request_age_band("13-15", index=row)
    assert admin_app.admin.request_age_band(index=row) == "13-15"
    admin_app.admin.set_request_tier("personal", index=row)
    admin_app.admin.approve_request(index=row)

    admin_app.admin.navigate_users()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and handle in admin_app.admin.invite_request_handles():
        time.sleep(0.4)
        admin_app.admin.navigate_users()
    assert handle not in admin_app.admin.invite_request_handles(), (
        f"the approved request should be gone. error: {admin_app.error_text()!r} "
        f"{admin_app.admin.pending_requests_diagnosis()}"
    )

    # The admitted account's own view: the band, established by the guardian.
    ward_ws = WsRpcAdminClient(
        nest_url,
        actor_id=bytes(ward["signing_key"].verify_key),
        signing_key=bytes(ward["signing_key"]),
    )
    with ward_ws:
        status = ward_ws.call("fauna.family.status", {})
    assert status.get("supervised_by"), f"the admitted account must be supervised: {status!r}"
    assert status.get("age_band") == {"band": "13-15", "provenance": "guardian-asserted"}, (
        f"the admitted account's band must be the one the admin picked: {status.get('age_band')!r}"
    )


@pytest.mark.feature("join-a-nest")
def test_a_denied_requester_reads_the_reason_and_can_resubmit(app, nest_instance):
    """Deny → the reason renders on the page → a resubmit succeeds.

    The load-bearing half is the **resubmit**: the nest refuses
    `invite_request.submit` while any row for the actor exists, so before this
    landed a denied requester was stuck forever. `wizard_submit_invite_request`
    now sends the signed `invite_request.cancel` first from `Denied`
    (`machine.rs`; `onboarding.md` § The pending-invite surface — "Deny is read
    directly"). Client sequencing only, no wire change — which is why this half
    runs on every app, unlike the unattended approval above.

    The deny is normally observed through the **manual recheck button**, which
    `onboarding.md` § 3 keeps as "the impatient user's affordance" on all 7
    apps — but the app's own automatic invite-recheck poll (same section,
    live on all 7 apps as of the test above) drives the identical
    `recheck_invite_status()` on the same cadence, and can occasionally win
    the race and flip the page out of `PendingReview` before this test's
    click fires. Both paths call
    the exact same machine method and produce the exact same transition, so
    either is valid evidence the deny+resubmit flow works — the click below
    is attempted only while the button is still there, never required.
    """
    app.driver.reset()
    handle = f"penddeny-{int(time.time() * 1000)}"
    _drive_app_to_pending_review(app, nest_instance["url"], handle)

    reason = f"not right now ({handle})"
    with _admin_client(nest_instance) as admin_ws:
        denied_id = _pending_request_id(admin_ws, handle)
        admin_ws.call(
            "fauna.admin.invite_requests.deny", {"id": denied_id, "reason": reason}
        )

    # The user presses Recheck; the denial and its reason must reach the page.
    # Attempt the click only while the button is visible, and tolerate it
    # vanishing mid-click too (the is_visible check and the click are not
    # atomic): the app's own automatic poll can beat this click to the same
    # recheck_invite_status() call and take the button out of the DOM first
    # (see the docstring). Either path lands the same transition, so a lost
    # race here is not a failure — the deadline-poll below is the real
    # assertion either way.
    if app.driver.is_visible("invite-request-recheck-button"):
        try:
            app.driver.click("invite-request-recheck-button")
        except Exception:
            pass
    deadline = time.monotonic() + RECHECK_BUDGET_S
    while time.monotonic() < deadline and reason not in _status_text(app):
        time.sleep(0.3)
    assert reason in _status_text(app), (
        "a denied request did not surface its reason on invite-request-status "
        f"within {RECHECK_BUDGET_S}s (i18n `onboarding.invite.denied` = "
        f"'Request denied: {{reason}}'). status={_status_text(app)!r}; "
        f"app error={app.error_text()!r}"
    )

    # Resubmit. Without the cancel-then-submit sequence the nest rejects this
    # (a row for the actor already exists) and the page never returns to
    # PendingReview — which is precisely the state this asserts.
    app.driver.click("invite-request-submit-button")
    _wait_visible(
        app,
        "invite-request-recheck-button",
        SUBMIT_BUDGET_S,
        "a denied requester could not resubmit (the cancel-then-submit sequence "
        "did not clear the denied row, so the nest refused the new submit)",
    )
    assert not app.has_error(), (
        f"the resubmit raised an app error: {app.error_text()!r}"
    )

    # Nest-side proof, not just a UI state: a *new pending* row exists, and it is
    # not the denied one. A UI-only assertion would also pass if the page merely
    # re-rendered its old PendingReview.
    with _admin_client(nest_instance) as admin_ws:
        resubmitted_id = _pending_request_id(admin_ws, handle)
    assert resubmitted_id != denied_id, (
        f"the resubmit reused the denied row id {denied_id} — the cancel did not "
        "delete it, so this is not the ratified cancel-then-submit sequence"
    )
