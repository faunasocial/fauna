"""tier_3 e2e for the family-safety client surface — family-safety.md
§ App surface. Live wire-driving verification against a real nest, as
opposed to compile/unit-test proof.

Runs on every app that has landed the surface: windows (2026-07-10),
linux + web (2026-07-11), macos + ios (2026-07-12 — one shared FaunaKit
`FamilyView`, so both apple apps land together). android is skipped until
it lifts the surface (see `_skip_unless_family_surface`); it still carries a
`family-tab` debt line in ui.yaml `navigation.nav_exceptions`.

Two tests, split by confidence:

`test_family_admission_and_ward_view` (GREEN) drives the real client UI
through:
  (a) an admin admits a guardian, then admits a ward via the pending-request
      Approve flow with a guardian selected (`invite-request-row-guardian-select`)
      — `invite_requests_approve_handler` -> `create_user_with_handle` ->
      `insert_guardianship_tx`, all inside one admission transaction. This is
      the supervised-admission path whose UI every app can drive on this shared
      test nest (direct admission, `fauna.admin.users.create` with a
      `guardian_actor`, also mints a supervised account since 2026-08-30, but
      its guardian picker is built on tui alone): `nest_instance` is started registration-CLOSED by design (many other tests
      rely on that default), and `account_core::register_core` refuses a closed
      posture UNCONDITIONALLY, before it ever looks at `invite_code` — so
      `fauna.account.register` (the OOB-code redeem path, whether driven by the
      real UI or a correctly-signed raw WS call) is REGISTRATION-CLOSED on this
      nest regardless of a valid invite code.

      ⚠ The posture is the `RegistrationMode` enum (`open` / `invite_required` /
      `closed`), NOT the retired `registration.open` boolean this docstring used
      to name: `routes.rs` — "Replaces the old `registration.open` +
      `registration.invite_required`" — and `register_core` reads
      `state.registration_mode`, never `state.auth.registration.open` (zero hits).
      The `--registration-open` CLI flag is likewise gone (`common/nest.py`:
      registration policy is an admin choice, so it is never a flag — it rides a
      `[nest] registration_mode` seed). The conclusion above is unchanged, but only
      because the default resolves to `closed`: "regardless of a valid invite code"
      is true of `closed` ALONE. In `invite_required` a valid code is exactly what
      admits, and in `open` `register_core` validates a supplied code and accepts
      it. Do not generalize this paragraph into "codes never redeem on a test nest"
      — a nest whose registration posture an admin has opened redeems them
      (`test_bearer_cache_web.py` drives precisely that).
  (b) separately, the admin mints a guardian-scoped invite code
      (`admin-users-invite-guardian-select`) and a fresh joiner checks the
      out-of-band code, seeing `invite-code-supervised-notice` BEFORE
      redemption (a real, peek-only `fauna.account.invite_code.verify` call
      — proves the OTHER admin guardian-picker + the onboarding notice
      render, without needing `registration.open`). Not carried through to
      redemption for the reason in (a).
  (c) the ward's own session (logged in directly — a real, already-admitted
      account, not onboarding-under-test) shows the gated `family-tab` /
      global `supervised-indicator` / the family page's supervised section
      (`family-guardian-handle`, `family-policy-summary`), proving
      `fauna.family.status`'s `supervised_by`/`policy` fields round-trip
      correctly through the whole stack (shared Rust -> UniFFI on the native
      apps / wasm on web -> the client's own renderer).

`test_family_guardian_sees_ward` drives the SAME admission, then logs in as
the guardian and polls the family page's Wards list, edits + saves the ward's
reach policy, pre-approves a contact, and graduates the ward.

  It used to be xfail on windows only: the guardian's Wards list rendered
  empty, and the cause was open between "a narrow server defect on the
  `invite_requests.approve` guardian-threading path" and "a windows-side
  `Wards` ObservableCollection binding gap". `conformance_family.rs::
  approve_with_guardian_creates_link_and_policy` proved the server side
  correct, and the windows-side gap turned out to be machine-
  contention flakiness, not a deterministic client bug — it passes green on
  a quiet machine. Fixed along the way (all windows-only, all confirmed via
  quiet-machine reruns): the reach-policy toggles/combos went stale (never
  re-synced) on any load whose value happened to equal the control's current
  or compile-time-default value, since `[ObservableProperty]`'s equality
  guard skips `PropertyChanged` for a no-op set; and the graduate-confirm
  button, once revealed, failed `is_visible` (`IsOffscreen`) because
  `StartBringIntoView()` was called before the reveal's layout pass had run.

One physical app instance is driven through THREE sequential identities
(admin -> disposable notice-check joiner -> ward, and a fourth for the
guardian in the second test), via `app.driver.reset()` between
phases — the same "clear stores, return to onboarding" + re-login cycle
`test_factory_reset_calendar_reclaim.py` uses. Login via `_login_app_as`
(state-protocol `set_state`) is sanctioned fixture setup under the
UI-driven-mutation carve-out (arranging the world is not the action under
test) once an account already exists and onboarding itself isn't what's
being verified — the admin guardian-picker selections, the onboarding
notice check, and every family-page mutation are real UI actions.
"""

import json
import os
import time
from datetime import datetime

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.launch_harness import reached_authenticated_app
from helpers.app_surface import skip_unbuilt
from helpers.family_wire import (
    DEFAULT_WARD_POLICY,
    actor_ws,
    admit_adult_directly,
    admit_ward,
    blocked_dm_peers,
    decide_dm_peer,
    reset_ward_policy,
    set_ward_policy,
    submit_invite_request,
)
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.rpc_hold import (
    arm_rpc_hold,
    release_rpc_hold,
    rpc_hold_status,
    wait_for_held_rpc,
)
from helpers.waiting import (
    await_device_removal_ready,
    await_feed_reload_after,
    feed_reload_baseline,
    wait_until,
)
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The wire kind the supervision-status read travels as — the nest's own router
#: registration (`bins/fauna-nest/src/family_handlers.rs`,
#: `libs/fauna-protocol/src/kind.rs`), not any client-side method name.
#: `arm_rpc_hold` rejects an unregistered spelling.
FAMILY_STATUS_KIND = "fauna.family.status"

# `nest_instance` is SESSION-scoped (conftest.py:508), so every account this
# module admits lands on the SAME nest and handles are a shared namespace: two
# admissions of "family-guardian1" collide on `fauna.account.handle_taken`.
# (This bit once: the original blanket xfail(strict=False) on test 2 reported
# that collision as an "expected failure", hiding it -- which is very likely why
# the empty-Wards defect it was nominally tracking was never isolated.)
#
# The suffix therefore carries a RANDOM component, not a hand-written per-test
# one. A hand-written suffix keeps two *tests* apart but not two runs of the
# same test, and this module is app-parametrized against one session-scoped
# nest: `--app sweep` ran `_handles("4")` once per app and the second app's
# approval hit `fauna.admin.conflict` on the re-taken handle. Which tests share
# an account and which take their own is now decided by the fixtures below
# (`_shared_family` / `family_pair`), not by whether two literals happen to
# differ -- so the readable stem stays for diagnosis and uniqueness is
# mechanical.
def _unique_suffix() -> str:
    """A collision-free tail for a handle minted on the shared nest."""
    return os.urandom(3).hex()


def _handles(stem: str) -> tuple[str, str]:
    suffix = f"{stem}-{_unique_suffix()}"
    return f"family-guardian-{suffix}", f"family-ward-{suffix}"


@pytest.fixture(scope="module")
def _shared_family(nest_instance):
    """ONE guardian + ONE ward, admitted once for the whole module.

    This is the module's answer to the shipped 10/60 s invite-request budget
    (`submit_invite_request`): ten of the fifteen tests here do not care *how*
    their ward was admitted — they read or edit the family page — so they share
    one ward and the module pays ONE submit for all ten instead of two each.

    Module-scoped, so it is built once even under `--app sweep`: the nest is
    session-scoped, the accounts live on the nest, and one pair serving every
    app parametrization is both cheaper and the thing that stops this module
    from colliding with itself on `handle_taken`.

    The pair is deliberately NOT handed out raw — `family_pair` is the fixture
    tests take, and it resets the policy document first.
    """
    guardian_handle, ward_handle = _handles("shared")
    guardian_identity = admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = admit_ward(nest_instance, guardian_identity, ward_handle)
    return {
        "guardian_identity": guardian_identity,
        "ward_identity": ward_identity,
        "guardian_handle": guardian_handle,
        "ward_handle": ward_handle,
    }


@pytest.fixture
def family_pair(_shared_family, nest_instance):
    """The module's shared guardian/ward pair, with the ward's policy document
    reset to defaults before the test body runs.

    Take this fixture whenever the test only needs *a* supervised account to
    look at or set policy on. Do NOT take it when the test consumes the pairing
    (graduation, an accepted transfer) or leaves per-ward residue another test
    would see (a marked device that survives, a deleted device) — those admit
    their own ward and pay their own submit; there are three, each labelled at
    its admission.

    Returns `(guardian_identity, ward_identity, guardian_handle, ward_handle)`.
    """
    reset_ward_policy(
        nest_instance, _shared_family["guardian_identity"], _shared_family["ward_identity"]
    )
    return (
        _shared_family["guardian_identity"],
        _shared_family["ward_identity"],
        _shared_family["guardian_handle"],
        _shared_family["ward_handle"],
    )


def _approve_pending(app, handle: str, *, guardian: str | None, deadline_s: float = 15.0) -> None:
    """Poll the admin-users hub for a pending request with `handle`, optionally
    select a guardian on that row (`invite-request-row-guardian-select`), then
    approve at the "personal" tier. Assumes the seeded row is the only pending
    request at index 0 (matches test_admin_users_hub.py's own convention)."""
    app.admin.navigate_users()
    deadline = time.monotonic() + deadline_s
    while time.monotonic() < deadline and handle not in app.admin.invite_request_handles():
        time.sleep(0.4)
        app.admin.navigate_users()
    assert handle in app.admin.invite_request_handles(), (
        f"seeded request for {handle!r} not visible after {deadline_s}s. "
        f"{app.admin.pending_requests_diagnosis()}"
    )
    if guardian is not None:
        app.admin.set_request_guardian(guardian, index=0)
    app.admin.set_request_tier("personal", index=0)
    app.admin.approve_request(index=0)


def _admit_guardian_and_ward_through_the_admin_ui(
    app, nest_url: str, guardian_handle: str, ward_handle: str
) -> tuple[dict, dict, str]:
    """The full UI admission — admit the guardian, admit the ward with the
    guardian selected on its pending-request row, and mint a guardian-scoped
    invite code. Returns (guardian_identity, ward_identity, code).

    ⚠ **The module's most expensive fixture: TWO anonymous invite-request
    submits, a fifth of the shipped 10/60 s budget** (`submit_invite_request`).
    It has exactly ONE caller — `test_family_admission_and_ward_view`, whose
    subject IS this flow (the pending-request guardian picker, the tier, the
    Approve, and the guardian-scoped code mint are the mutations under test).
    Every other test takes `family_pair`, or `_admit_own_pair` when it needs a
    ward of its own; neither costs an admin-UI navigation either.
    """
    guardian_identity = submit_invite_request(nest_url, handle=guardian_handle)
    _approve_pending(app, guardian_handle, guardian=None)

    ward_identity = submit_invite_request(nest_url, handle=ward_handle)
    _approve_pending(app, ward_handle, guardian=guardian_handle)

    return guardian_identity, ward_identity, _mint_guardian_code(app, guardian_handle)


def _mint_guardian_code(app, guardian_handle: str, age_band: str | None = None) -> str:
    """Mint a guardian-scoped invite code through the admin UI
    (`admin-users-invite-guardian-select`, and `admin-users-invite-age-band-select`
    when `age_band` is given) — the out-of-band code `_check_notice_only` peeks
    at. A real UI mutation, and free of the anonymous budget: minting is an
    admin act."""
    app.admin.navigate_invite_codes()
    code = app.admin.create_invite_code(
        tier="free", uses=1, guardian=guardian_handle, age_band=age_band
    )
    assert code, f"mint (with guardian) returned no token. error: {app.error_text()!r}"
    return code


def _admit_own_pair(nest_instance, stem: str) -> tuple[dict, dict, str, str]:
    """A guardian + ward this test alone owns — ONE anonymous submit (the ward's;
    the guardian is direct-admitted).

    Only for the tests that cannot share `family_pair`, because what they do to
    the pair cannot be reset: an accepted transfer and a graduation both END the
    pairing; the device-marker test leaves a guardian-marked device behind on a
    page whose assertion is a whole-page badge count; and the Guardian-Notify,
    screen-time-budget and device-mark-toggle tests each grow a day-scoped or
    ever-growing counter on the shared ward (`guardian_content_notices`,
    `guardian_usage`, registered sync devices) that no wire surface can clear
    short of ending the pairing — `db/family.rs`'s graduation and
    ward-deletion drops are the only deletes those tables get — so reusing the
    shared ward across an app leg leaks that residue into the next leg's
    assertions under a multi-app invocation. Six
    tests share this shape now, each naming its reason at its call site.

    Returns (guardian_identity, ward_identity, guardian_handle, ward_handle).
    """
    guardian_handle, ward_handle = _handles(stem)
    guardian_identity = admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = admit_ward(nest_instance, guardian_identity, ward_handle)
    return guardian_identity, ward_identity, guardian_handle, ward_handle


def _check_notice_only(app, nest_url: str, code: str, guardian_handle: str) -> str:
    """Fresh disposable joiner checks the OOB code and confirms
    invite-code-supervised-notice renders BEFORE redemption. Peek-only
    (fauna.account.invite_code.verify does not decrement `uses`) — does not
    redeem (see module docstring: registration.open gates redemption on this
    nest regardless of a valid invite code). Returns the notice's text."""
    app.driver.reset()
    joiner_secret_hex = bytes(SigningKey.generate()).hex()
    app.onboarding.navigate_to_status()
    app.onboarding.import_key(joiner_secret_hex)
    app.driver.wait_for("handle-input", timeout=15)
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest",
        json.dumps([nest_url, "family-notice-check"]),
    )
    app.driver.wait_for("invite-code-input", timeout=20)
    app.driver.type_text("invite-code-input", code)
    app.driver.click("invite-code-check-button")

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not app.driver.is_visible("invite-code-supervised-notice"):
        time.sleep(0.4)
    assert app.driver.is_visible("invite-code-supervised-notice"), (
        "OOB code check did not surface invite-code-supervised-notice before "
        f"redemption. status={app.driver.get_text('invite-code-status')!r} "
        f"error: {app.error_text()!r}"
    )
    notice = app.driver.get_text("invite-code-supervised-notice")
    assert guardian_handle in notice, f"supervised notice missing guardian handle: {notice!r}"
    return notice


@pytest.mark.feature("family-safety")
def test_family_admission_and_ward_view(admin_app, request, nest_instance):
    app = admin_app

    nest_url = nest_instance["url"]
    # The one caller of the full UI admission, and the one test that spends two
    # of the module's six anonymous invite-request submits: the admin's pending-
    # request approval WITH a guardian selected is this test's subject, so it
    # cannot be shortcut to `family_pair` (see `submit_invite_request`).
    guardian_handle, ward_handle = _handles("admission")
    guardian_identity, ward_identity, code = (
        _admit_guardian_and_ward_through_the_admin_ui(
            app, nest_url, guardian_handle, ward_handle
        )
    )
    _check_notice_only(app, nest_url, code, guardian_handle)

    # The ward's own session (a real, already-admitted account, logged in
    # directly): the global supervised-indicator + family page supervised
    # section.
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)

    # supervised-indicator and family-tab are set by the SAME
    # CheckFamilyStatusAsync block; supervised-indicator (a page-level
    # overlay, not a NavigationView FooterMenuItem) is the reliable UIA
    # witness for the gate firing — a raw `family-tab` visibility/click check
    # hits a pre-existing, unrelated windows FlaUI gap shared by
    # `test_admin_tab_visible_for_admin[windows]` (tracked internally).
    app.driver.wait_for("supervised-indicator", timeout=20)
    indicator_text = app.driver.get_text("supervised-indicator")
    assert guardian_handle in indicator_text, f"supervised-indicator missing guardian: {indicator_text!r}"

    app.family.navigate()
    assert guardian_handle in app.family.guardian_handle_text(), (
        f"family page supervised section missing guardian: {app.family.guardian_handle_text()!r}"
    )
    assert app.family.policy_summary_text(), "supervised section should render a read-only policy summary"


def _require_a_relaunch_that_keeps_the_session(app) -> None:
    """Pin the CURRENT launch's client store for the next relaunch — or skip,
    declared as unbuilt, on an app whose relaunch cannot come back signed in as
    the identity `_login_app_as` injected; a held-status window across a
    relaunch needs the app to reach the read it holds.

    ⚠ Call it AFTER the login and right before the relaunch:
    `preserve_state_across_relaunch()` pins the store of the launch that is
    running now, and a later `driver.reset()` starts a fresh launch the pin
    does not cover — the relaunch then comes back signed out."""
    if not app.driver.preserve_state_across_relaunch():
        skip_unbuilt(
            app.driver,
            surface="a client store that can be pinned across a relaunch",
            detail=(
                "the driver hands the relaunched process a fresh store, so the "
                "app returns signed out and never issues the read this test holds"
            ),
            tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
        )
    if not app.driver.relaunch_preserves_injected_identity():
        skip_unbuilt(
            app.driver,
            surface="a login that survives a relaunch",
            detail=(
                "_login_app_as injects the session with set_state, and on this "
                "app the injection does not reach a store the relaunch keeps, so "
                "the app returns signed out and never reaches the surface under test"
            ),
            tracked="drivers/http_bridge.py::relaunch_preserves_injected_identity",
        )


@pytest.mark.feature("family-safety")
def test_family_supervised_indicator_shows_restored_guardian_while_status_is_pending(
    admin_app, request, nest_instance, family_pair
):
    """family-safety.md § the unfetched-policy ruling (ratified 2026-08-02),
    clause 2: a cold start must distinguish *unsupervised* from *supervised,
    last-known state restored* — the distinction clause 1 alone cannot supply
    for a client whose `fauna.family.status` re-read has not answered yet.

    This is `e2e-latency-independent-assertions.md`'s Pre-fetch windows class
    applied to family-safety (`test_inbox_mode_is_unknown_while_its_fetch_is_
    still_pending` is the worked template for the mechanism; the *correct*
    pending-window answer differs here because family-safety's own design
    already gives the client something honest to show — the persisted
    snapshot restored ahead of the read — where inbox-mode has nothing and
    must show blank).

    **The scenario this needs is a ward with a PRIOR successful read, not a
    fresh one.** A device that has never completed one successful status read
    has no snapshot to restore and "enforces nothing" — family-safety.md's own
    declared, ACCEPTED residual (clause 3), not a bug this test could prove
    wrong. The provable claim is narrower and real: once a snapshot exists,
    a SECOND read (fired again at every cold launch and WS reconnect) must not
    blank the restored state while it is in flight. linux's leg is
    `apps/fauna-linux/src/app.rs`'s `supervision_snapshot::load()` block, which
    runs — and sets `supervised-indicator` — BEFORE `check_family_status()`
    issues the live re-read; tui's twin is `restore_supervision_snapshot`
    (`apps/fauna-tui/src/family.rs`).

    Held windows must be COLD ones (see the load-bearing arm-before-relaunch
    ordering below) — an app already holding the guardian handle in memory is
    remembering, not guessing, and re-entering the page does not unlearn it.
    """
    app = admin_app
    port = nest_instance["port"]
    # Shares the module's one admitted pair: reads the supervised indicator and
    # sets one content floor — both reset by `family_pair`.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    # ── Guardian sets the ward's spam content floor (real UI mutation) ──────
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_content_floor("spam", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # ── The ward logs in — ONE successful fauna.family.status read, which is
    # what persists the supervision snapshot this test's pending window relies
    # on (clause 2: "written on every successful status read"). ──────────────
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    indicator_text = app.driver.get_text("supervised-indicator")
    assert guardian_handle in indicator_text, (
        "precondition: supervised-indicator must name the guardian from the "
        "FIRST (live) read, or the held-window assertion below cannot tell a "
        f"correct restore from an absent one: {indicator_text!r}"
    )

    _require_a_relaunch_that_keeps_the_session(app)

    # Armed BEFORE the relaunch — the load-bearing ordering
    # `test_inbox_mode_is_unknown_while_its_fetch_is_still_pending` paid for:
    # arming after the relaunch races the app's own start-up read.
    arm_rpc_hold(port, FAMILY_STATUS_KIND)
    try:
        assert app.driver.recover(), "the app did not come back up after the relaunch"
        reached_authenticated_app(app.driver, timeout=90)

        def _diagnose_arrival():
            return f"error={app.error_text()!r}"

        wait_for_held_rpc(port, FAMILY_STATUS_KIND, diagnose=_diagnose_arrival)

        seen = app.driver.get_text("supervised-indicator") or ""
        assert guardian_handle in seen, (
            "while fauna.family.status is still pending, supervised-indicator "
            f"must show the RESTORED last-known guardian, but it reports {seen!r}. "
            "The nest is holding the reply, so this is not a value read from "
            "anywhere live — it is either the persisted snapshot (correct) or a "
            "value the app made up by collapsing 'no answer yet' into "
            f"'unsupervised' (wrong). error={app.error_text()!r}"
        )
        assert rpc_hold_status(port, FAMILY_STATUS_KIND)["holding"] >= 1, (
            "the reply was released before the assertion above ran, so it "
            "proved nothing about the pending window"
        )
    finally:
        release_rpc_hold(port, FAMILY_STATUS_KIND)

    # And the other half: released, the live re-read still confirms the same
    # guardian — this is what separates "restores the last-known state" from
    # "never updates again".
    wait_until(
        lambda: guardian_handle in (app.driver.get_text("supervised-indicator") or ""),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "once the held reply is released the indicator must still name "
            f"the guardian, but it reports {app.driver.get_text('supervised-indicator')!r}"
        ),
    )


def _admit_adult(nest_instance, handle: str) -> dict:
    """Admit an ordinary (unsupervised, non-guardian) account — the proposed
    guardian of the transfer handshake tests.

    Direct-admitted (`admit_adult_directly`), so it costs no anonymous submit:
    what these tests need from this actor is that it EXISTS and is admissible as
    a guardian, and the admission path it arrived by is not part of any
    assertion here."""
    return admit_adult_directly(nest_instance, handle)


def _guardian_proposes(app, request, nest_instance, guardian_identity,
                       ward_handle: str, target_hex: str, target_handle: str) -> None:
    """Log in as the guardian, select the ward, and propose `target` as its
    new guardian through the transfer input (real UI mutation — point 8)."""
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"ward {ward_handle!r} not in guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    app.family.propose_transfer(target_hex)
    assert not app.has_error(), f"transfer proposal raised: {app.error_text()!r}"

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not app.family.transfer_pending_visible():
        time.sleep(0.5)
    assert app.family.transfer_pending_visible(), (
        f"family-transfer-pending did not render. error: {app.error_text()!r}"
    )
    pending = app.family.transfer_pending_text()
    assert target_handle in pending, (
        f"pending text should name the proposed guardian: {pending!r}"
    )


def _login_as_target_and_find_prompt(app, request, nest_instance, target_identity,
                                     guardian_handle: str, ward_handle: str) -> None:
    """Log in as the proposed guardian and reach the incoming prompt. The
    target has NO other family relationship, so reaching the page at all
    proves the widened family-tab gate (any relationship OR pending/incoming
    transfer — family-safety.md § Graduation & transfer, Visibility)."""
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, target_identity)

    # The widened gate is the load-bearing assertion: without it a target not
    # otherwise in a family relationship never sees the prompt. tab_visible()
    # handles the mobile Settings-nesting difference (family.py's own doc).
    # On desktop (macos/windows/linux) and web, `family-tab` sits in a
    # persistent sidebar/top-nav, so the widened gate is directly observable.
    # On mobile it is nested in Settings' `List` (apple's SwiftUI `List` is
    # lazily-rendered, and `/scroll` is a stub for the in-process driver — the
    # same documented class as `apple-e2e-automation.md` rule 6, and why
    # `test_admin_nav.py::test_admin_tab_visible_for_admin` isn't ios-marked
    # either), so a late list entry can never register without real scrolling
    # — a pre-existing SettingsView.swift structural gap, not this feature's.
    # The state-protocol nav + a real incoming-prompt render below is the
    # honest mobile substitute: it only succeeds if the caller's own
    # `fauna.family.status` genuinely carries the incoming transfer.
    if not app.driver.is_mobile():
        deadline = time.monotonic() + 20.0
        while time.monotonic() < deadline and not app.family.tab_visible():
            time.sleep(0.5)
        assert app.family.tab_visible(), (
            "family-tab must reveal for a proposed guardian with no other family "
            f"relationship (the widened gate). error: {app.error_text()!r} "
            f"{app.driver.diagnose('family-tab')} "
            # `settings-tab` is an always-present nav row on every app. If IT
            # reads absent here too, this client cannot observe nav rows from
            # this vantage point at all (a harness / nav-shell gap) — a wholly
            # different failure from the family-status gate never firing, and the
            # two are indistinguishable from the bare assertion above (e2e rule 6).
            f"reference={app.driver.diagnose('settings-tab')}"
        )

    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and app.family.incoming_transfer_count() == 0:
        time.sleep(0.5)
        app.family.reload()
    assert app.family.incoming_transfer_count() == 1, (
        f"expected one incoming transfer prompt. error: {app.error_text()!r}"
    )
    text = app.family.incoming_transfer_text(0)
    assert guardian_handle in text and ward_handle in text, (
        f"incoming prompt should name the current guardian and the ward: {text!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_age_band_readouts(admin_app, request, nest_instance, family_pair):
    """The two age-band readouts (family-safety.md § App surface → *Age-band
    surfaces*, #6 + #7): a ward admitted with a band shows it on the guardian's
    ward row (`family-ward-age-band`, scoped under that ward's item) and on the
    ward's own supervised section (`family-age-band-summary`), each naming how
    it was set; a ward admitted with NO band (the module's shared pair, a
    pre-band admission) shows neither — absent, never a placeholder.

    Its own pair (one anonymous submit): the shared ward has no band and must
    stay that way to be the control.
    """
    from conftest import _login_app_as

    app = admin_app
    guardian_handle, ward_handle = _handles("band")
    guardian_identity = admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = admit_ward(nest_instance, guardian_identity, ward_handle, age_band="13-15")

    # The guardian's view: the band on the ward's row.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    app.family.select_ward_by_handle(ward_handle)
    handles = app.family.ward_handles()
    assert ward_handle in handles, f"the guardian's ward list lacks {ward_handle!r}: {handles!r}"
    row = handles.index(ward_handle)
    scope = f"family-ward-item[{row}]"
    app.driver.wait_for("family-ward-age-band", timeout=10, scope=scope)
    ward_row_text = app.driver.get_text("family-ward-age-band", scope=scope)
    assert "13" in ward_row_text and "guardian" in ward_row_text, (
        f"the ward row should name the band and how it was set: {ward_row_text!r}"
    )

    # The ward's own view: the summary on the supervised section.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    app.family.navigate()
    app.driver.wait_for("family-age-band-summary", timeout=10)
    summary = app.driver.get_text("family-age-band-summary")
    assert "13" in summary and "guardian" in summary, (
        f"the ward's summary should name the band and how it was set: {summary!r}"
    )

    # The control: the shared pair's ward carries no band — neither surface.
    pair_guardian_identity, _pair_ward_identity, _pair_guardian_handle, pair_ward_handle = family_pair
    app.driver.reset()
    _login_app_as(app, request, nest_instance, pair_guardian_identity)
    app.family.navigate()
    app.family.select_ward_by_handle(pair_ward_handle)
    handles = app.family.ward_handles()
    pair_scope = f"family-ward-item[{handles.index(pair_ward_handle)}]"
    assert app.driver.is_absent("family-ward-age-band", scope=pair_scope), (
        "a ward admitted without a band must paint no band row (never a placeholder)"
    )


@pytest.mark.feature("family-safety")
def test_family_transfer_accept_journey(admin_app, request, nest_instance):
    """The consent handshake, accept arm (family-safety.md § Graduation &
    transfer): guardian A proposes adult B for a ward through the UI; B —
    who has no other family relationship — reaches the prompt via the
    widened family-tab gate and accepts; the ward moves to B's list, the
    policy intact, and A no longer guards it."""
    app = admin_app

    # Its OWN ward (one anonymous submit), not `family_pair`: the accepted
    # transfer re-points the link to another guardian, so the pairing does not
    # survive the test — there is nothing to reset back to.
    guardian_identity, _ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "transfer-accept"
    )
    target_handle = f"family-target-3-{_unique_suffix()}"
    target_identity = _admit_adult(nest_instance, target_handle)

    _guardian_proposes(app, request, nest_instance, guardian_identity,
                       ward_handle, target_identity["actor_id_hex"], target_handle)
    _login_as_target_and_find_prompt(app, request, nest_instance, target_identity,
                                     guardian_handle, ward_handle)

    app.family.accept_incoming_transfer(0)
    assert not app.has_error(), f"accept raised: {app.error_text()!r}"

    # The accept re-points the link: the ward lands in B's own Wards list.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"accepted ward should appear in the new guardian's list. error: {app.error_text()!r}"
    )
    assert app.family.incoming_transfer_count() == 0, "prompt should clear after accept"

    # …and the old guardian no longer lists it.
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle not in app.family.ward_handles(), (
        f"old guardian still lists the transferred ward. error: {app.error_text()!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_transfer_decline_journey(admin_app, request, nest_instance, family_pair):
    """The decline arm: B refuses, the prompt clears, and the link stands —
    A still guards the ward and the pending marker is gone (declining is
    always available and costs nothing)."""
    app = admin_app

    # Shares the module's one admitted pair: the DECLINE arm leaves the link
    # exactly as it found it, and the reset cancels any proposal a failed run
    # left pending.
    guardian_identity, _ward_identity, guardian_handle, ward_handle = family_pair
    target_handle = f"family-target-4-{_unique_suffix()}"
    target_identity = _admit_adult(nest_instance, target_handle)

    _guardian_proposes(app, request, nest_instance, guardian_identity,
                       ward_handle, target_identity["actor_id_hex"], target_handle)
    _login_as_target_and_find_prompt(app, request, nest_instance, target_identity,
                                     guardian_handle, ward_handle)

    app.family.decline_incoming_transfer(0)
    assert not app.has_error(), f"decline raised: {app.error_text()!r}"
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and app.family.incoming_transfer_count() > 0:
        time.sleep(0.5)
        app.family.reload()
    assert app.family.incoming_transfer_count() == 0, "prompt should clear after decline"

    # The link is unchanged: A still guards the ward, no pending marker.
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"declined transfer must leave the link intact. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert not app.family.transfer_pending_visible(), (
        "pending marker must clear after the decline"
    )


def _family_status_read_landed(app, port: int, login) -> None:
    """Run ``login`` with the nest HOLDING the app's ``fauna.family.status`` read,
    then let it through and drain the app — the causal anchor a NEGATIVE
    assertion about the family gate needs (convention 14).

    An unrelated account's `family-tab` reads "absent" both before the post-auth
    status read has answered and after it answered "no relationship", so a bare
    absence check right after login passes against an app that never asked.
    Holding the read proves the app issued it; the `released` counter proves the
    reply went out; `barrier()` drains the UI work the reply enqueued. Armed
    BEFORE the login, as every hold in this file is, so the app's own start-up
    read cannot race the arm."""
    before = rpc_hold_status(port, FAMILY_STATUS_KIND).get("released", 0)
    arm_rpc_hold(port, FAMILY_STATUS_KIND)
    try:
        login()
        wait_for_held_rpc(
            port, FAMILY_STATUS_KIND, diagnose=lambda: f"error={app.error_text()!r}"
        )
    finally:
        release_rpc_hold(port, FAMILY_STATUS_KIND)
    wait_until(
        lambda: rpc_hold_status(port, FAMILY_STATUS_KIND).get("released", 0) > before,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"status={rpc_hold_status(port, FAMILY_STATUS_KIND)!r}",
    )
    app.driver.barrier()


@pytest.mark.feature("family-safety")
def test_family_transfer_withdraw_journey(admin_app, request, nest_instance, family_pair):
    """The withdraw arm of the hand-over (family-safety.md § Graduation &
    transfer): guardian A proposes adult B, sees the proposal waiting with B's
    handle, and withdraws it through `family-transfer-cancel-button`; the
    pending marker clears, the link stands, and B — whose only tie to the
    family was that proposal — no longer has a Family page to open.

    Shares the module's one admitted pair: the withdraw leaves the link exactly
    as it found it, and `family_pair`'s reset cancels a proposal a failed run
    left pending.
    """
    from conftest import _login_app_as

    app = admin_app
    guardian_identity, _ward_identity, _guardian_handle, ward_handle = family_pair
    target_handle = f"family-target-withdraw-{_unique_suffix()}"
    target_identity = _admit_adult(nest_instance, target_handle)

    _guardian_proposes(app, request, nest_instance, guardian_identity,
                       ward_handle, target_identity["actor_id_hex"], target_handle)

    app.family.cancel_transfer()
    assert not app.has_error(), f"withdraw raised: {app.error_text()!r}"

    def _pending_cleared() -> bool:
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
        return not app.family.transfer_pending_visible()

    wait_until(
        _pending_cleared,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "family-transfer-pending still renders after the withdraw: "
            f"{app.driver.diagnose('family-transfer-pending')} error: {app.error_text()!r}"
        ),
    )
    assert ward_handle in app.family.ward_handles(), (
        f"a withdrawn proposal must leave the link intact. error: {app.error_text()!r}"
    )

    # B's side: the proposal was B's only relationship, so once the status read
    # has answered B has no Family page at all — the gate's "or a hand-over
    # waiting for them" arm closes with the proposal.
    app.driver.reset()
    _family_status_read_landed(
        app,
        nest_instance["port"],
        lambda: _login_app_as(app, request, nest_instance, target_identity),
    )
    assert not app.family.tab_visible(), (
        "family-tab must hide for an adult whose only family tie was a withdrawn "
        f"hand-over. {app.driver.diagnose('family-tab')} "
        f"reference={app.driver.diagnose('settings-tab')}"
    )


@pytest.mark.feature("family-safety")
def test_family_page_hidden_for_an_unrelated_account(admin_app, request, nest_instance, family_pair):
    """The Family page's gate (family-safety.md § App surface): it appears only
    for someone who supervises, is supervised, or has a hand-over waiting for
    them. The hand-over arm is proven by the transfer journeys above; the
    guardian and the ward reach it here as the control, and the negative is an
    ordinary adult with none of the three, who never sees `family-tab` —
    measured after the app's own status read has answered, never before it.
    """
    from conftest import _login_app_as

    app = admin_app
    guardian_identity, ward_identity, _guardian_handle, _ward_handle = family_pair
    stranger_identity = _admit_adult(nest_instance, f"family-unrelated-{_unique_suffix()}")

    for who, identity in (("guardian", guardian_identity), ("ward", ward_identity)):
        app.driver.reset()
        _login_app_as(app, request, nest_instance, identity)
        wait_until(
            app.family.tab_visible,
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            diagnose=lambda who=who: (
                f"control: the {who} must see family-tab. error: {app.error_text()!r} "
                f"{app.driver.diagnose('family-tab')}"
            ),
        )

    app.driver.reset()
    _family_status_read_landed(
        app,
        nest_instance["port"],
        lambda: _login_app_as(app, request, nest_instance, stranger_identity),
    )
    assert not app.family.tab_visible(), (
        "family-tab must stay hidden for an account that neither supervises, is "
        f"supervised, nor has a hand-over waiting. {app.driver.diagnose('family-tab')} "
        f"reference={app.driver.diagnose('settings-tab')}"
    )


@pytest.mark.feature("family-safety")
def test_family_supervised_notice_names_the_codes_age_band(admin_app, nest_instance, family_pair):
    """A supervised invite code minted with an age band tells the person joining
    that band BEFORE redemption, in the same `invite-code-supervised-notice`
    sentence that names the guardian ("… as a 13–15 account" — family-safety.md
    § App surface → *Age-band surfaces*: the band rides the existing notice,
    never a second one). The admin mints through the real form, the band select
    enabled once the guardian is picked; the joiner only peeks (verify does not
    spend the code), so the shared pair's guardian serves.
    """
    app = admin_app
    _guardian_identity, _ward_identity, guardian_handle, _ward_handle = family_pair

    code = _mint_guardian_code(app, guardian_handle, age_band="13-15")
    notice = _check_notice_only(app, nest_instance["url"], code, guardian_handle)
    assert "13–15" in notice, f"the supervised notice should name the code's age band: {notice!r}"


@pytest.mark.feature("family-safety")
def test_family_guardian_sees_ward(admin_app, request, nest_instance):
    app = admin_app

    nest_url = nest_instance["url"]
    # Its OWN ward (one anonymous submit), not `family_pair`: it GRADUATES the
    # ward at the end, deleting the link and the policy document — the pairing
    # is consumed, not merely dirtied.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "guardian-sees-ward"
    )
    code = _mint_guardian_code(app, guardian_handle)
    _check_notice_only(app, nest_url, code, guardian_handle)

    # ── The guardian's session: review the ward, edit + save its reach
    #    policy, exercise approvals/contact-add, graduate it ───────────────
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)

    app.family.navigate()
    # family-heading (a static landmark) renders before LoadAsync()'s async
    # fauna.family.status round trip resolves — poll for the ward to actually
    # land rather than reading Wards immediately after navigate().
    deadline = time.monotonic() + 15.0
    handles = app.family.ward_handles()
    while time.monotonic() < deadline and ward_handle not in handles:
        time.sleep(0.5)
        handles = app.family.ward_handles()
    assert ward_handle in handles, (
        f"expected ward {ward_handle!r} among {handles!r}. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load for the selected ward. error: {app.error_text()!r}; "
        # Separates the two ways this can fail, which the bare assertion conflates:
        # the editor panel never revealed (its children are then ABSENT from the tree,
        # so the toggle count is 0) vs. it revealed but the save button sits below the
        # fold (count 1, offscreen). Convention 6 — a failure must diagnose itself.
        f"save-button: {app.driver.diagnose('family-policy-save-button')}; "
        f"editor-top toggle count: {app.driver.count('family-policy-contact-approval-toggle')}"
    )

    # Flip every knob away from its default and save.
    app.family.set_contact_approval(True)
    app.family.set_unknown_sender("hold")
    app.family.set_federation(False)
    app.family.set_feed_sources("block")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # Reload and confirm the edit persisted server-side (not just optimistic UI).
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.contact_approval_state() == "on", "contact-approval did not persist"
    assert app.family.unknown_sender_label() == S.family.value_hold, (
        f"unknown-sender-mail did not persist: {app.family.unknown_sender_label()!r}"
    )
    assert app.family.federation_state() == "off", "federation-contact did not persist"
    assert app.family.feed_sources_label() == S.family.value_block, (
        f"feed-sources did not persist: {app.family.feed_sources_label()!r}"
    )

    # Approvals-queue smoke: renders without error (no real knock was staged).
    assert app.family.approval_count() >= 0
    assert not app.has_error()

    # Row 70 regression: the family page must satisfy BOTH obligations of
    # e2e-conventions.md convention 2's rider before the real contact-add
    # below exercises the happy path. (a) the shared `error-message` id is
    # ABSENT on a clean page — a permanently-present element makes the
    # negative assertion below (and `assert not app.has_error()` throughout
    # this file) unable to ever fail. (b) a real page error is readable
    # through the state protocol (`has_error()`/`error_text()`), not only
    # the raw UI element — windows was the one app of 24 error-bearing pages
    # that rendered its `ActionError` locally without publishing into
    # `App.CurrentErrorMessage`, so `has_error()` silently read False while
    # the on-screen text was correct. An invalid (non-hex) contact-add id
    # fails entirely client-side (`Convert.FromHexString` throws before any
    # RPC), so this is deterministic and needs no nest round trip.
    assert not app.driver.is_visible("error-message"), (
        "family page must start with no error surfaced"
    )
    assert not app.has_error(), f"clean family page reports an error: {app.error_text()!r}"
    app.family.add_contact("not-a-hex-actor-id")
    # A convention-14 deadline poll, NOT `driver.wait_for`: `wait_for` scrolls
    # the element into view itself (a general-purpose fallback), which would
    # mask a regression of the *app's own* scroll-into-view
    # (FamilyPage.xaml.cs's deferred `LayoutUpdated` -> `StartBringIntoView`,
    # `reference_windows_e2e_is_visible_offscreen`) behind the harness doing
    # the job for it. A bare `is_visible` poll only asserts what the app
    # itself brought on screen.
    wait_until(
        lambda: app.driver.is_visible("error-message"),
        UI_SETTLE_S,
        diagnose=lambda: (
            "an invalid contact-add actor id must surface on the shared "
            "error-message element; it stayed hidden. "
            f"error_text={app.error_text()!r}"
        ),
    )
    assert app.has_error(), (
        "the family page's error must be readable through the state protocol "
        "(has_error()/error_text()), not only the raw UI element — obligation (b)"
    )
    shown_element = app.driver.get_text("error-message")
    assert S.family.contact_add_invalid_actor_id in shown_element, (
        f"error-message text missing the expected reason (got {shown_element!r})"
    )
    assert S.family.contact_add_invalid_actor_id in app.error_text(), (
        f"error_text() (state protocol) missing the expected reason (got {app.error_text()!r})"
    )

    # Contact pre-approval smoke: a real fauna.family.contact.add call. Also
    # proves the error clears (both surfaces) once a valid add succeeds.
    peer_hex = SigningKey.generate().verify_key.encode().hex()
    app.family.add_contact(peer_hex)
    assert not app.has_error(), f"contact-add raised: {app.error_text()!r}"
    assert not app.driver.is_visible("error-message"), (
        "a successful contact-add must clear the prior invalid-input error"
    )

    # Real graduate round trip: the ward disappears from the guardian's list.
    before = app.family.ward_count()
    app.family.begin_graduate()
    assert app.family.graduate_confirm_visible(), (
        f"graduate confirm step did not reveal. error: {app.error_text()!r}"
    )
    confirm_text = app.family.graduate_confirm_text()
    assert ward_handle in confirm_text, f"graduate-confirm text missing ward handle: {confirm_text!r}"
    app.family.confirm_graduate()

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle not in app.family.ward_handles(), (
        f"ward still listed after graduate. error: {app.error_text()!r}"
    )
    assert app.family.ward_count() == before - 1


@pytest.mark.feature("family-safety")
def test_family_bridge_dm_knob_round_trips(admin_app, request, nest_instance, family_pair):
    """family-safety.md § The bridge-DM gate: the guardian sets the ward's
    `unknown_peer_dm` knob to `hold` through `family-policy-unknown-peer-dm-select`
    and it persists server-side — the live-wire proof that this knob is reachable
    at all, which it was not before tui shipped the select (2026-08-14).

    Two properties beyond "the select paints", each of which has its own way of
    silently not holding:

    1. **The value persists.** Asserted after a `reload()`, which re-reads
       `fauna.family.status` — an optimistic in-editor render would survive a
       naive re-read of the same widget but not this.
    2. **An untouched select does not rewrite the stored knob.** The knob rides
       `policy.update` as `Option<String>` where absent means "leave unchanged"
       (§ Policy-update compatibility), so the editor sends it only once the
       guardian has touched it. Saving an unrelated edit afterwards must leave
       `hold` standing. Without this, every future save of any other knob would
       echo back whatever this build rendered — including the fail-closed render
       of a value only a newer nest could have written.

    Its own test rather than a leg of `test_family_guardian_sees_ward` because
    the select is a per-app build gate (convention 7): folding it in would skip
    that whole test on the six apps still lifting it.
    """
    app = admin_app
    app.family.require_unknown_peer_dm_select()

    # Shares the module's one admitted pair: asserts the unknown-peer-dm
    # DEFAULT before editing it — `family_pair`'s reset is what makes that
    # default assertion true on every run.
    guardian_identity, _ward_identity, guardian_handle, ward_handle = family_pair

    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)

    app.family.navigate()
    deadline = time.monotonic() + 15.0
    handles = app.family.ward_handles()
    while time.monotonic() < deadline and ward_handle not in handles:
        time.sleep(0.5)
        handles = app.family.ward_handles()
    assert ward_handle in handles, (
        f"expected ward {ward_handle!r} among {handles!r}. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}; "
        f"select: {app.driver.diagnose('family-policy-unknown-peer-dm-select')}"
    )

    # The default is `allow` — and it arrives on the wire as ABSENT, so this
    # also pins that absence renders as the permissive default rather than the
    # fail-closed one (rendering `hold` here would show a policy stricter than
    # the one the nest enforces).
    assert app.family.unknown_peer_dm_label() == S.family.value_allow, (
        f"expected the allow default, got {app.family.unknown_peer_dm_label()!r}"
    )

    app.family.set_unknown_peer_dm("hold")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.unknown_peer_dm_label() == S.family.value_hold, (
        f"unknown-peer-dm did not persist: {app.family.unknown_peer_dm_label()!r}"
    )

    # Property 2: an unrelated edit, saved without touching this select, must
    # leave the stored `hold` alone.
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"second save_policy raised: {app.error_text()!r}"
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.contact_approval_state() == "on", "the unrelated edit did not persist"
    assert app.family.unknown_peer_dm_label() == S.family.value_hold, (
        "an untouched unknown-peer-dm select rewrote the stored knob: "
        f"{app.family.unknown_peer_dm_label()!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_ward_asks_guardian_for_a_contact(admin_app, request, nest_instance, family_pair):
    """family-safety.md § Child-initiated contact requests, end to end: the
    ward's send is refused, the ward asks in-app, and the ask lands in the
    guardian's queue as a row that actually says who it is about.

    The whole journey exists because the refusal used to be a dead end. Three
    things it pins that unit tests cannot:

    1. **The nest really refuses with the typed error**, and the app really
       recognises it (`RpcError::is_guardian_approval_required` over whichever
       namespace `fauna.inbox.send` happens to use) — a string match would pass
       a unit test and fail here the day the namespace changes.
    2. **The ask reaches the guardian's queue as a `contact_request` row whose
       text is the peer's handle.** This is the live proof of the shared
       display-rule fix: a `contact_request` carries a deliberately empty
       `summary`, so before 2026-08-14 this row rendered BLANK beside live
       Approve/Deny buttons on all 7 apps. Asserting the handle — not merely
       `approval_count() == 1` — is what makes the assertion able to fail.
    3. **The ward's pending state is durable**, read back from
       `status.contact_requests` after a page reload rather than a session flag.

    The peer asked about is the *guardian's own* account: guardianship is not a
    contact edge, so `(ward, guardian)` is a genuinely unrelated pair as far as
    `contact_approval` is concerned, and it needs no third admission.
    """
    app = admin_app
    app.contacts.require_contact_request_ask()

    # Shares the module's one admitted pair: sets contact-approval and leaves
    # an approval row; nothing here outlives the policy reset.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair
    guardian_actor_id = guardian_identity["actor_id_hex"]

    from conftest import _login_app_as

    # ── The guardian turns contact_approval on (the knob that moves acceptance
    #    authority to them — with it off the ward contacts freely and there is
    #    nothing to ask about). Driven through the UI per convention 8.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
    assert ward_handle in app.family.ward_handles(), (
        f"ward missing from the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"
    # The doorbell baseline (family-safety.md § Child-initiated contact requests
    # → *Doorbell*): the guardian's feed is session-long, so the assertion below
    # is "one more", never "one".
    doorbell = S.notifications.row_family_contact_request
    app.notifications.navigate()
    doorbells_before = sum(
        doorbell in t for t in app.driver.get_texts("notification-item")
    )

    # ── The ward's session: the send is refused, and the refusal is not a dead
    #    end — the ask is offered in its place.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.contacts.navigate()
    app.contacts.find_by_actor_id(guardian_actor_id)
    assert app.contacts.actor_id_result_text() == guardian_actor_id, (
        f"lookup did not resolve: {app.contacts.actor_id_result_text()!r}"
    )
    assert not app.contacts.guardian_ask_offered(), (
        "the ask must not be offered before anything was refused"
    )

    app.contacts.add_contact()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not app.contacts.guardian_ask_offered():
        time.sleep(0.5)
    assert app.contacts.guardian_ask_offered(), (
        "the refused send offered no ask — is the typed refusal being recognised? "
        f"error: {app.error_text()!r}"
    )

    app.contacts.ask_guardian()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and not app.contacts.contact_request_pending_text():
        time.sleep(0.5)
    assert app.contacts.contact_request_pending_text(), (
        f"no pending state after asking. error: {app.error_text()!r}"
    )
    assert not app.contacts.guardian_ask_offered(), (
        "the ask button must not survive a landed ask — clicking it again re-asks"
    )

    # Durable, not a session flag: reload the page and the state comes back from
    # the ward's own `status.contact_requests`.
    app.contacts.navigate()
    app.contacts.find_by_actor_id(guardian_actor_id)
    assert app.contacts.contact_request_pending_text(), (
        "the pending state did not survive a reload — it is a session flag, not "
        f"status.contact_requests. error: {app.error_text()!r}"
    )

    # ── The guardian's queue: one row, and it NAMES the peer.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and app.family.approval_count() == 0:
        time.sleep(0.5)
        app.family.reload()
    assert app.family.approval_count() >= 1, (
        f"the ask never reached the guardian's queue. error: {app.error_text()!r}"
    )
    texts = [app.family.approval_text(i) for i in range(app.family.approval_count())]
    assert any(guardian_handle in t for t in texts), (
        "the contact_request row does not name its peer — a blank row beside live "
        f"Approve/Deny buttons is the defect this asserts against. rows: {texts!r}"
    )

    # ── The doorbell: the ask rang the guardian once, and the notification
    #    opens the Family page where the decision is made.
    app.notifications.navigate()

    def _doorbell_rows() -> list[int] | None:
        texts = app.driver.get_texts("notification-item")
        rows = [i for i, t in enumerate(texts) if doorbell in t]
        if len(rows) > doorbells_before:
            return rows
        app.notifications.navigate()
        return None

    rows = wait_until(
        _doorbell_rows,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            f"the ward's ask never rang the guardian ({doorbells_before} before); rows="
            f"{app.driver.get_texts('notification-item')!r}"
        ),
    )
    assert len(rows) == doorbells_before + 1, (
        f"one ask must ring exactly once: {len(rows)} doorbells, {doorbells_before} before"
    )
    app.notifications.open_notification(rows[0])
    app.driver.wait_for("family-heading", timeout=UI_SETTLE_S)


@pytest.mark.feature("family-safety")
def test_family_guardian_un_denies_the_second_blocked_peer(
    admin_app, request, nest_instance, family_pair
):
    """family-safety.md § The bridge-DM gate → *The un-deny surface*: with TWO
    denied bridge-DM peers on one ward, the guardian taps the allow button in the
    SECOND row, and exactly that row's peer comes back from the nest un-denied
    while the first stays denied.

    The property is **each allow button addresses its own row's
    `(bridge_id, peer_id)`**. A button wired to the first row would un-deny the
    wrong person while the page still looked right — it shows one row fewer
    either way. That is why the tap goes to the second row: a tap on row 0
    cannot tell a correct button from one hard-wired to row 0.

    The denies are seeded as the guardian over the wire (fixture setup, point 8):
    a `dm_hold` decide is not queue-scoped, so no held conversation — and so no
    live bridge relay — is needed to produce a `block` verdict. The un-deny, the
    action under test, is a real tap.
    """
    app = admin_app
    app.family.require_blocked_peers_supported()

    # Shares the module's one admitted pair: `reset_ward_policy` un-denies any
    # peer a failed earlier run left behind, so exactly our two rows render.
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    ward_actor = bytes.fromhex(ward_identity["actor_id_hex"])
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    guardian_seed = bytes(guardian_identity["signing_key"])
    # Nostr pubkey-shaped ids, fresh per run so a row can only be ours.
    peers = {os.urandom(32).hex(), os.urandom(32).hex()}
    with WsRpcAdminClient(nest_instance["url"], guardian_actor, guardian_seed) as guardian:
        for peer in peers:
            decide_dm_peer(guardian, ward_actor, "nostr", peer, False)
        denied = {p["peer_id"] for p in blocked_dm_peers(guardian, ward_actor)}
    assert denied == peers, f"the seeded denies did not land: {denied!r}"

    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    app.family.select_ward_by_handle(ward_handle, timeout=15.0)
    rows = wait_until(
        lambda: (lambda r: r if len(r) == 2 else None)(app.family.blocked_peer_ids(peers)),
        15.0,
        diagnose=lambda: (
            f"rows {app.family.blocked_peer_rows()!r}; error {app.error_text()!r}; "
            f"{app.driver.diagnose('family-blocked-peer-item')}"
        ),
    )
    assert set(rows) == peers, (
        f"the un-deny rows do not name the denied peers: {app.family.blocked_peer_rows()!r} "
        f"vs {peers!r}"
    )
    target, bystander = rows[1], rows[0]

    app.family.allow_blocked_peer(1)

    # Wait for the nest's denied set to CHANGE, then say which peer left: a
    # button addressing the wrong row changes it too, and must fail on the
    # assertion below naming the peer, not on a bare timeout.
    with WsRpcAdminClient(nest_instance["url"], guardian_actor, guardian_seed) as guardian:
        # Wrapped in a 1-tuple so an EMPTY set (both un-denied) still ends the
        # poll and fails on the assertion instead of reading as "not yet".
        (still,) = wait_until(
            lambda: (lambda d: (d,) if len(d) < 2 else None)(
                {p["peer_id"] for p in blocked_dm_peers(guardian, ward_actor)} & peers
            ),
            15.0,
            diagnose=lambda: f"no un-deny reached the nest; error {app.error_text()!r}",
        )
    assert still == {bystander}, (
        "the second row's allow button did not un-deny the second row's peer: "
        f"the nest still denies {still!r}; expected exactly the first row's "
        f"{bystander!r} (tapped row 1 = {target!r})"
    )
    # The page re-reads after the flip: one row left, and it is the bystander.
    left = wait_until(
        lambda: (lambda r: r if len(r) == 1 else None)(app.family.blocked_peer_ids(peers)),
        15.0,
        diagnose=lambda: f"rows {app.family.blocked_peer_rows()!r}",
    )
    assert left == [bystander], f"the list kept the wrong row: {left!r}"


@pytest.mark.feature("status-quotas-and-limits")
def test_family_guardian_feature_limit_reaches_the_wards_status(
    admin_app, request, nest_instance, family_pair
):
    """`dynamic-features.md` § Transparency & auditability: a limit a guardian
    set is visible to the ward it binds, attributed to the guardian.

    The guardian tightens `p2p-share` to 3 operations/day through the policy
    document's `features` sub-document — an API write, legitimately: the
    subject here is the WARD's Status rendering, and the guardian-side feature
    editor is not what this asserts (point 8's fixture-setup carve-out, the
    same one `test_feature_limits.py::_set_admin_policy` uses for the admin
    tier). 3 is tighter than tier 1's bound, so the guardian document must win
    the meet; the ward's cell must then say "Your guardian", which a screen
    that hard-coded or registry-derived the tier cannot produce.

    The `family_pair` setup reset puts `features: {}` back for the next test.
    """
    app = admin_app
    guardian_identity, ward_identity, _guardian_handle, _ward_handle = family_pair
    guardian_limit = 3

    policy = dict(DEFAULT_WARD_POLICY)
    policy["features"] = {
        "p2p-share": {"availability": "limit", "operations": {"per_day": guardian_limit}}
    }
    with WsRpcAdminClient(
        nest_instance["url"],
        bytes.fromhex(guardian_identity["actor_id_hex"]),
        bytes(guardian_identity["signing_key"]),
    ) as guardian:
        guardian.call(
            "fauna.family.policy.update",
            {
                "supervised_actor_id": bytes.fromhex(ward_identity["actor_id_hex"]),
                "policy": policy,
            },
        )

    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.settings._navigate_subpage("status")
    try:
        app.settings.wait_for_feature_limits()
    except TimeoutError:
        raise AssertionError(
            "feature-limits-section never rendered for the ward: "
            f"{app.driver.diagnose('feature-limits-section')} error={app.error_text()!r}"
        ) from None

    expected_value = S.features.quota_value(
        remaining=str(guardian_limit), limit=str(guardian_limit)
    )
    # p2p-share is row 2; its first cell is operations/day.
    cell = app.settings.wait_for_feature_limit_quota_value(
        row=2, cell=0, expected_value=expected_value, timeout=15.0
    )
    assert cell["value"] == expected_value, (
        f"the guardian's bound must be the one the ward sees: {cell}"
    )
    assert cell["tier"] == S.features.tier_guardian, (
        f"the ward's Status must say the GUARDIAN set this limit, got {cell}"
    )


@pytest.mark.feature("family-safety")
def test_family_content_floor_blocks_flagged_feed_post(admin_app, request, nest_instance, family_pair):
    """family-safety.md § Content policy (Slice C): a guardian sets the ward's
    `spam` content floor to `block` through the Family editor (the mutation under
    test — UI-driven per e2e point 8), and the ward's own feed then renders
    `content-policy-blocked-notice` in place of a spam-labeled post's body while a
    clean post is untouched.

    The labeled post is a *precondition* injected via the feed test seam (fixture
    setup, not the action under test): the feed carries no live post-decrypt
    classify hook — feed labels ride the wire from the nest's `content_labels`
    projection, empty on an encrypted nest — so the only way to stage a
    spam-labeled feed post in tier_3 is the `TestPostSpec.labels` inject seam. The
    render enforcement itself (guardian floor → `render_verdict_entries` → block)
    is exercised for real by the ward's shipping feed read model.

    Linux is the Slice C reference leg; web + the other apps lift it (this gate
    widens as they land).
    """
    app = admin_app
    app.family.require_content_render_verdict_supported()

    # Shares the module's one admitted pair: sets one content floor — reset by
    # `family_pair`.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    # ── Guardian sets the ward's spam content floor to `block` (real UI mutation) ──
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_content_floor("spam", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # Reload-persist: the content floor round-trips server-side, and (transparency)
    # the guardian's editor re-renders it as Block.
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.content_floor_label("spam") == S.family.value_block, (
        f"spam content floor did not persist: {app.family.content_floor_label('spam')!r}"
    )

    # ── The ward logs in; a spam-labeled post is blocked, a clean one is not ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    # The post-auth fauna.family.status read is what loads the ward's own content
    # policy into the feed render (supervised-indicator is its witness — same
    # handler sets both). Waiting for it guarantees the floor is live before the
    # injected posts render.
    app.driver.wait_for("supervised-indicator", timeout=20)

    # posts[i] renders as post-card[i] (order preserved): [0] clean, [1] spam-labeled.
    count = app.feed.seed_posts([
        {"post_id": "cf-clean", "author": "afriend", "body": "an ordinary post from a friend"},
        {
            "post_id": "cf-spam",
            "author": "aspammer",
            "body": "this body must never render",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"

    # The spam-labeled post is blocked: the notice renders, the body does not.
    assert app.family.content_notice_blocked("post-card[1]"), (
        "spam-labeled post did not render the content-policy block notice "
        f"({app.driver.diagnose('content-policy-blocked-notice')}; "
        f"{app.driver.diagnose('nest-blocked-notice')}; engine sources: "
        f"{(app.driver.get_state() or {}).get('content_policy_sources')!r})"
    )
    assert app.driver.count("feed-post-text", scope="post-card[1]") == 0, (
        "a blocked post must not render its body text"
    )
    # The clean post is untouched: its body renders, no block notice.
    assert app.driver.is_absent("content-policy-blocked-notice", scope="post-card[0]"), (
        "the clean post must not be blocked"
    )
    assert app.driver.count("feed-post-text", scope="post-card[0]") >= 1, (
        "the clean post's body should render normally"
    )


# How long the ward's client gets, after the benign flip, to reconnect (its own
# backoff + the silent bearer re-mint), re-read `fauna.family.status`, and
# render the floor. A named budget, generous for a loaded box (convention 14);
# the loop below asserts state, never elapsed time.
RECONNECT_BIND_BUDGET_S = 120.0

# How long the ward's client gets, after the benign flip, to prove the reconnect
# itself landed — a feed re-query that BEGAN after the flip commits its verdict
# (`helpers.waiting.await_feed_reload_after`). Priced like
# `test_nest_flip_resilience.py`'s `REHYDRATE_WAIT_S`: the same chain (jittered
# backoff, silent bearer re-mint, the reconnect resync) on the same apps.
RECONNECT_REQUERY_BUDGET_S = 300.0


def _set_ward_content_floor(
    nest_instance, guardian_identity: dict, ward_identity: dict, category: str, floor: str
) -> None:
    """Set ONE content floor on the shared ward's policy document, as the
    GUARDIAN, over the wire — the same link-authorized mutation the guardian's
    editor sends (`fauna.family.policy.update`), off the reset defaults."""
    ward_actor = bytes.fromhex(ward_identity["actor_id_hex"])
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    guardian_seed = bytes(guardian_identity["signing_key"])
    policy = dict(DEFAULT_WARD_POLICY)
    # A floors-only write, as an app still painting the four selects sends it:
    # no `document` key, since a present document — even the reset's empty one —
    # clears the floors it rides beside (nest-rules.md § The shared rule editor (b)).
    content = {k: v for k, v in DEFAULT_WARD_POLICY["content_policy"].items() if k != "document"}
    policy["content_policy"] = {**content, category: floor}
    with WsRpcAdminClient(nest_instance["url"], guardian_actor, guardian_seed) as guardian:
        guardian.call(
            "fauna.family.policy.update",
            {"supervised_actor_id": ward_actor, "policy": policy},
        )


@pytest.mark.feature("family-safety")
def test_family_content_floor_binds_on_ws_reconnect_without_relaunch(
    admin_app, request, nest_instance, family_pair
):
    """family-client-enforcement.md § Content policy, the unfetched-policy
    ruling's clause 1 — "refresh fires at cold launch and on WS reconnect": a
    floor the guardian sets AFTER the ward's post-auth read binds on the ward's
    next WS reconnect, with no relaunch and no Family-page visit.

    The cold-launch trigger is proven by
    `test_family_content_floor_blocks_flagged_feed_post` (floor first, login
    second). This pins the OTHER trigger: the ward logs in FIRST against a
    default policy, so its one post-auth read loads no floor; the guardian then
    sets spam→block; a benign nest flip (`restart_nest`, the Watchtower-redeploy
    shape `test_nest_flip_resilience.py` guards) drops and re-establishes the
    ward's socket; and the ward's feed must then block a spam-labeled post.
    

    The guardian's edit is a wire mutation, not the editor (e2e point 8's cited
    exception): the editor path is proven by the sibling test above, and the
    mutation under test is the WARD's re-read, which the one app instance must
    stay logged in as the ward to witness. The pre-flip assertion — the spam
    post renders UN-blocked — is what proves the reconnect, and not the login,
    binds the floor: nothing on the feed page re-reads status between the two
    (both reference legs read it post-auth and on a Family-page visit, neither
    of which happens here).

    Two post-flip barriers, so a red names its own layer (convention 6). First
    the RECONNECT is observed directly: every app re-queries its feed on
    reconnect, so a re-query that began after the flip committing proves this
    client's socket came back. Only then the FLOOR: a red there can no longer
    mean "the flip never reached the client" — it means the status re-read did
    not fire, or fired and its reply never reached the render floor.

    Latency-independent (convention 14): the reconnect barrier is the
    `feed_reloads` causal anchor, and the post-flip wait re-seeds the spam post
    until it renders blocked, under `RECONNECT_BIND_BUDGET_S` — the floor either
    loaded or did not; no settle-sleep decides the verdict.
    """
    from common.nest import restart_nest
    from conftest import _login_app_as

    app = admin_app
    app.family.require_content_render_verdict_supported()

    # Shares the module's one admitted pair: sets one content floor — reset by
    # `family_pair`.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    spam_post = {
        "post_id": "cf-reconnect-spam",
        "author": "aspammer",
        "body": "this body must never render once the floor binds",
        "labels": [{"category": "spam", "confidence_per_mille": 900}],
    }

    # ── The ward logs in FIRST, while the policy is at its reset defaults ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    # The post-auth fauna.family.status read has landed once the indicator
    # names the guardian (same handler) — and it carried no floor.
    app.driver.wait_for("supervised-indicator", timeout=20)
    count = app.feed.seed_posts([spam_post])
    assert count == 1, f"expected 1 seeded post-card, got {count}. error: {app.error_text()!r}"
    # The block notice is the discriminator, not the body: with no guardian
    # floor the ward's OWN default spam threshold still COLLAPSES a spam-labeled
    # post (the every-user collapse, `moderation.md` § Categories & enforcement
    # item 1), so the body is hidden either way — only a `block` floor paints
    # the notice in state `blocked`, and only the guardian can set one.
    assert not app.family.content_notice_blocked("post-card[0]"), (
        "the spam post was blocked BEFORE the guardian set any floor — this test needs "
        "the ward's policy at its defaults (family_pair resets it); the pre-state is wrong"
    )

    # ── The guardian sets the ward's spam floor to `block` — after the ward's read ──
    _set_ward_content_floor(nest_instance, guardian_identity, ward_identity, "spam", "block")

    # The reconnect's causal baseline, read behind the app's own barrier as late
    # as possible before the flip (`feed_reload_baseline`'s docstring: a bare
    # read can predate the last action and release the barrier on nothing).
    reloads_before = feed_reload_baseline(app.driver)

    # ── The benign flip: the ward's WS drops; the client reconnects on its own ──
    restart_nest(nest_instance, graceful=True)

    # ── (1) The reconnect landed: a feed re-query begun after the flip committed ──
    # Fails here, naming the flip, if this client's socket never came back — a
    # transport or harness fault, kept apart from (2)'s reading.
    await_feed_reload_after(
        app.driver,
        reloads_before,
        budget_s=RECONNECT_REQUERY_BUDGET_S,
        what="the benign flip",
    )

    # ── (2) After the reconnect, the re-read binds the floor: the same post now blocks ──
    # `seed_posts` re-injects until the card count holds (its own bounded wait),
    # so each pass is a fresh render against whatever floor is loaded now.
    deadline = time.monotonic() + RECONNECT_BIND_BUDGET_S
    blocked = False
    while time.monotonic() < deadline:
        if app.feed.seed_posts([spam_post]) == 1 and app.family.content_notice_blocked(
            "post-card[0]"
        ):
            blocked = True
            break
    assert blocked, (
        "the guardian's spam→block floor never bound on the ward's client within "
        f"{RECONNECT_BIND_BUDGET_S:.0f}s of its PROVEN WS reconnect (barrier (1) passed) — "
        "the reconnect sweep either did not re-read fauna.family.status (clause 1's "
        "second trigger) or re-read it without moving the render floor, so the edit "
        f"would wait for the ward's next login. error: {app.error_text()!r}"
    )
    assert app.driver.count("feed-post-text", scope="post-card[0]") == 0, (
        "a blocked post must not render its body text"
    )


@pytest.mark.feature("family-safety")
def test_family_content_floor_blocks_flagged_conversation_message(
    admin_app, request, nest_instance, family_pair
):
    """family-safety.md § Content policy (Slice C): the guardian content floor
    also enforces on the CONVERSATIONS surface (the second social surface). A
    guardian sets the ward's `spam` floor to `block` through the Family editor,
    and the ward's own conversation then renders `content-policy-blocked-notice`
    in place of a spam-labeled inbound message's body while a clean message is
    untouched.

    The spam label is a *precondition* injected via the conversations test seam
    (fixture setup, not the action under test): the generic inject path does not
    classify — only the real MLS receive path's `observe_local_detection` does —
    so, exactly as the feed stages `TestPostSpec.labels`, the spam label is stamped
    directly through the `labels=` inject arm
    (`ConversationsManager::inject_inbound_with_labels_for_test`). The render
    enforcement itself (guardian floor -> `render_verdict_entries` -> block) is
    exercised for real by the ward's shipping conversation bubble.

    Linux is the Slice C reference leg; the other apps lift it (this gate
    widens as they land).
    """
    app = admin_app
    app.family.require_content_render_verdict_supported()

    # Shares the module's one admitted pair: sets one content floor — reset by
    # `family_pair`.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    # ── Guardian sets the ward's spam content floor to `block` (real UI mutation) ──
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_content_floor("spam", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # ── The ward logs in; a spam-labeled inbound DM is blocked, a clean one is not ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    # The post-auth fauna.family.status read loads the ward's content policy into
    # the render (supervised-indicator is its witness — the same handler sets both).
    app.driver.wait_for("supervised-indicator", timeout=20)

    # Inject a clean message + a spam-labeled message from the same sender into
    # one thread. The spam label (900 per-mille >> the 500 guardian-floor trigger)
    # is staged directly via the `labels=` inject arm; the clean message carries
    # no labels.
    app.conversations.navigate()
    app.conversations.inject_and_open_thread(
        rail="FaunaMls",
        sender="afriend-cfc@self-nest.test",
        body="an ordinary message from a friend",
    )
    app.conversations.inject_inbound_for_test(
        rail="FaunaMls",
        sender="afriend-cfc@self-nest.test",
        body="this spam body must never render",
        labels=[{"category": "spam", "confidence_per_mille": 900}],
    )

    # Wait for both bubbles to settle: the clean body (dm-message-text) plus the
    # blocked one's notice (content-policy-blocked-notice) = two rendered arms.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and (
        app.driver.count("dm-message-text")
        + app.driver.count("content-policy-blocked-notice")
        < 2
    ):
        time.sleep(0.3)

    # The spam-labeled message is blocked: exactly one notice, and the blocked
    # bubble carries no body text — so the only rendered body is the clean one.
    assert app.driver.count("content-policy-blocked-notice") == 1, (
        "the spam-labeled DM did not render the content-policy block notice "
        f"({app.driver.diagnose('content-policy-blocked-notice')}); "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count("dm-message-text") == 1, (
        "exactly the clean message body should render (a blocked message must "
        f"not render its body); dm-message-text count="
        f"{app.driver.count('dm-message-text')}"
    )
    assert (
        app.driver.get_text("dm-message-text", index=0).strip()
        == "an ordinary message from a friend"
    ), "the rendered body should be the clean message, not the blocked one"


@pytest.mark.feature("privacy-settings")
def test_own_spam_threshold_collapses_flagged_conversation_message(logged_in_app):
    """moderation.md § Categories & enforcement item 1 / family-safety.md § Content
    policy (the every-user, unsupervised half): a user's OWN spam threshold collapses
    a spam-labeled message in their own conversation view — no guardian involved. The
    default spam threshold (500 per-mille) is fetched at login into
    `content_policy` (the same shared engine the guardian floor composes into), and a
    900-per-mille spam-labeled message collapses (its body hidden behind a "show
    anyway" reveal) while a clean message shows.

    The content-policy collapse placeholder carries no test id in v1 — the feed leg
    established that "v1 e2e drives only the block case; the collapse reveal is a user
    affordance" — so the assertion is body-absence: exactly the clean body renders and
    the spam body never does. The composition + strictest-wins is unit-pinned in
    `content_policy::tests` and `fauna_core::obligation::tests`; this proves the linux
    login→cache→render wiring end-to-end.
    """
    conv = logged_in_app.conversations
    d = logged_in_app.driver
    logged_in_app.family.require_content_render_verdict_supported()

    sender = "afriend-ots@self-nest.test"
    # Stage a clean message (thread has 1) then a spam-labeled one (900 per-mille >>
    # the default 500 threshold), both from the same sender → one thread, two bubbles.
    thread_id = conv.inject_and_resolve_thread(
        rail="FaunaMls", sender=sender, body="an ordinary message from a friend"
    )
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=sender,
        body="this spam body must never render",
        labels=[{"category": "spam", "confidence_per_mille": 900}],
    )
    # Wait until BOTH messages are in the store before opening (the render is
    # deterministic on open) — the collapsed bubble adds no dm-message-text, so a
    # bubble-count poll can't see it; poll the thread's message_count instead.
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        t = next((t for t in conv.list_threads() if t.thread_id == thread_id), None)
        if t is not None and t.message_count >= 2:
            break
        time.sleep(0.2)
    conv.open_thread_by_id(thread_id)

    # The spam-labeled message collapses behind the viewer's own threshold — its body
    # is hidden — so exactly the clean message's body renders.
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and d.count("dm-message-text") < 1:
        time.sleep(0.3)
    assert d.count("dm-message-text") == 1, (
        "the spam-labeled message should collapse behind the viewer's own spam "
        f"threshold, hiding its body; dm-message-text count={d.count('dm-message-text')}, "
        f"error={logged_in_app.error_text()!r}"
    )
    assert (
        d.get_text("dm-message-text", index=0).strip()
        == "an ordinary message from a friend"
    ), "the only rendered body should be the clean message, not the collapsed spam one"


def _set_own_spam_threshold(nest_instance, user: dict, per_mille: int) -> None:
    """Set the user's OWN spam threshold over the wire, as that user — the same
    `fauna.spam.set_preferences` the Settings slider's save sends."""
    with WsRpcAdminClient(
        nest_instance["url"],
        bytes.fromhex(user["actor_id_hex"]),
        bytes(user["signing_key"]),
    ) as client:
        client.call("fauna.spam.set_preferences", {"spam_threshold": per_mille})


# The nest's default own spam threshold (`fauna.spam.get_preferences` on an
# account that never set one) — what the shared test user is restored to.
_DEFAULT_OWN_SPAM_THRESHOLD = 500


@pytest.mark.feature("privacy-settings")
def test_own_spam_threshold_binds_on_ws_reconnect_without_relaunch(
    logged_in_app, nest_instance, test_user
):
    """family-client-enforcement.md § Content policy — the viewer's own thresholds
    refresh on the same two triggers as the guardian floor: cold launch and WS
    reconnect. A threshold the user lowers AFTER this client's post-auth read
    (from another device — here, the wire) binds at this client's next WS
    reconnect, with no relaunch.

    

    The pre-flip assertion — the 300-per-mille message's body RENDERS under the
    default 500 threshold — is what proves the reconnect, not the login, binds
    the lowered one. Two post-flip barriers so a red names its layer
    (convention 6): the reconnect itself (`feed_reloads` causal anchor), then
    the collapse. Latency-independent (convention 14): the collapse is polled
    under `RECONNECT_BIND_BUDGET_S`; no settle-sleep decides the verdict.

    Restores the shared user's threshold to the default in `finally` — the
    identity backs every later `logged_in_app` test.
    """
    from common.nest import restart_nest

    conv = logged_in_app.conversations
    d = logged_in_app.driver
    logged_in_app.family.require_content_render_verdict_supported()

    sender = "aborderline-otr@self-nest.test"
    clean_body = "a plain note before the borderline one"
    borderline_body = "a borderline message the labeler scored low"
    thread_id = conv.inject_and_resolve_thread(rail="FaunaMls", sender=sender, body=clean_body)
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=sender,
        body=borderline_body,
        labels=[{"category": "spam", "confidence_per_mille": 300}],
    )
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        t = next((t for t in conv.list_threads() if t.thread_id == thread_id), None)
        if t is not None and t.message_count >= 2:
            break
        time.sleep(0.2)
    conv.open_thread_by_id(thread_id)

    # ── Pre-state: 300 < the default 500, so both bodies render ──
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and d.count("dm-message-text") < 2:
        time.sleep(0.3)
    assert d.count("dm-message-text") == 2, (
        "under the default own spam threshold (500) a 300-per-mille spam label must "
        f"not collapse; dm-message-text count={d.count('dm-message-text')} — the "
        "pre-state is wrong (was the shared user's threshold left lowered?), "
        f"error={logged_in_app.error_text()!r}"
    )

    try:
        # ── The user lowers their own threshold elsewhere — after this client's read ──
        _set_own_spam_threshold(nest_instance, test_user, 200)
        # Barrier (1) reads the feed's reload counters, and apple's reconnect
        # re-fetch of the feed is view-owned (`.onReconnect` on the feed view), so
        # with the Conversations page open no feed reload runs there. Park the app
        # on the feed before the flip — every app reloads the feed on reconnect
        # from there; the own-threshold re-read (app-level) is what (2) judges.
        d.navigate_to("feed")
        reloads_before = feed_reload_baseline(d)

        # ── The benign flip: the WS drops; the client reconnects on its own ──
        restart_nest(nest_instance, graceful=True)

        # ── (1) The reconnect landed ──
        await_feed_reload_after(
            d,
            reloads_before,
            budget_s=RECONNECT_REQUERY_BUDGET_S,
            what="the benign flip",
        )

        # ── (2) The re-read binds the lowered threshold: the borderline body collapses ──
        # Each tick re-opens the thread, so it is a fresh render against whatever
        # thresholds are loaded now.
        def _collapsed() -> bool:
            conv.open_thread_by_id(thread_id)
            return d.count("dm-message-text") == 1

        wait_until(
            _collapsed,
            RECONNECT_BIND_BUDGET_S,
            interval=1.0,
            diagnose=lambda: (
                "the lowered own spam threshold (200) never bound after a PROVEN WS "
                "reconnect (barrier (1) passed) — the reconnect sweep did not re-read "
                "fauna.spam.get_preferences, or re-read it without moving the render "
                "engine's own thresholds; dm-message-text "
                f"count={d.count('dm-message-text')}, error={logged_in_app.error_text()!r}"
            ),
        )
        assert d.get_text("dm-message-text", index=0).strip() == clean_body, (
            "the only rendered body should be the clean message, not the collapsed one"
        )
    finally:
        _set_own_spam_threshold(nest_instance, test_user, _DEFAULT_OWN_SPAM_THRESHOLD)


@pytest.mark.feature("family-safety")
def test_family_guardian_notify_surfaces_flagged_count(admin_app, request, nest_instance):
    """family-client-enforcement.md § Guardian Notify (Slice D-client): with the guardian's
    `content_notify` knob ON and the ward's `spam` floor at `block`, the ward's
    conforming client counts its own guardian-floor enforcement events and reports
    coarse per-category aggregates via `fauna.family.notify_report` (category +
    count, **never content, never an id**). The guardian's Family surface then
    renders the day's count for that ward (`family-ward-content-notices`) — the
    notification is the doorbell, the status read is the truth.

    The report is the behaviour under test, driven end-to-end through the real
    client (e2e point 8): the guardian enables Notify + sets the floor through the
    editor UI, and the ward's shipping feed read model both enforces the floor and
    emits the coarse report — no API shortcut stands in for either. The spam label
    is a precondition staged via the feed test seam (as in the Slice C floor
    tests), the only way to stage a labeled feed post on an encrypted tier_3 nest.

    linux + web are the reference legs (windows/apple/android lift as Slice G).
    """
    app = admin_app
    app.family.require_guardian_notify_client_supported()

    # Its OWN ward (one anonymous submit), not `family_pair`: the flagged count
    # this test produces lands in `guardian_content_notices(ward, day, category)`,
    # which nothing resets short of ending the pairing — a shared ward would
    # carry today's count into the next app leg's "nothing flagged yet"
    # assertion below.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "guardian-notify"
    )

    # ── Guardian: enable Notify + set the ward's spam floor to block (real UI) ──
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_content_floor("spam", "block")
    app.family.set_content_notify(True)
    app.family.save_policy()
    assert not app.has_error(), f"save_policy raised: {app.error_text()!r}"

    # Reload-persist: the Notify knob round-trips (transparency + it is what gates
    # the ward-side counting), and there is no readout yet — nothing flagged today.
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.content_notify_state() == "on", (
        f"content_notify did not persist: {app.family.content_notify_state()!r}"
    )
    assert app.family.ward_content_notices_text(0) == "", (
        f"expected no Notify readout before the ward flags anything: "
        f"{app.family.ward_content_notices_text(0)!r}"
    )

    # ── Ward: renders a spam-labeled post; its client counts + reports ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    # The post-auth fauna.family.status read loads the floor + the content_notify
    # knob into the render + counting path (supervised-indicator is its witness —
    # same handler) and starts the ward's Notify flush loop.
    app.driver.wait_for("supervised-indicator", timeout=20)

    count = app.feed.seed_posts([
        {"post_id": "gn-clean", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": "gn-spam",
            "author": "aspammer",
            "body": "this body must never render",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"
    # The floor fires — the enforcement event Guardian Notify counts.
    assert app.family.content_notice_blocked("post-card[1]"), (
        "spam-labeled post did not render the block notice — there is nothing to count"
    )
    # Let the ward's client batch + flush the coarse report to the nest BEFORE we
    # flip identity (reset wipes the ward's client and its flush loop). The first
    # report is eager (no prior flush).
    # Force the check now rather than waiting out the real flush-tick interval —
    # convention 14's run_now poke, on every app (contract:
    # `fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW`; web `#lib/family-notify-e2e`,
    # windows `GuardianNotifyCache.CheckNowAsync`, linux `flush_notify_report`,
    # tui `due_notify_report`). The real cadence gate
    # (notify_report_min_interval_secs) still applies; only the tick wait is
    # skipped.
    #
    # There is deliberately no per-app branch here any more. The old else-arm
    # slept 12 seconds for linux and tui, on the claim that neither had a poke —
    # stale for linux (it has had one since its tick stopped being armed under
    # e2e) and now closed for tui. That sleep was never a wait for anything on
    # either: linux does not arm its 5s tick under e2e at all, and tui's flush
    # rides a **60s** LOCK_TICK, so 12s could only ever catch it by luck — and
    # the window is closed by the very next line, which switches identity and
    # destroys the ward's client along with its counts.
    #
    # (Wording note: spell that removed call out in prose, never as source. The
    # sleep-ratchet gate greps raw file text, so writing the literal call form
    # in a comment counts as a sleep and silently holds this file's baseline up
    # — which is exactly what happened on the first draft of this comment.)
    app.driver.call_command("family_notify_check_now", timeout=20)

    # ── Guardian: the ward's readout now shows the spam count ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    spam_label = S.family.policy_content_spam_label
    count_text = S.family.ward_content_notice_count(count="1")
    deadline = time.monotonic() + 40.0
    readout = ""
    while time.monotonic() < deadline:
        app.family.reload()
        if ward_handle in app.family.ward_handles():
            readout = app.family.ward_content_notices_text(0)
            if spam_label in readout and count_text in readout:
                break
        time.sleep(1.0)
    assert spam_label in readout and count_text in readout, (
        f"the guardian's Notify readout is missing the spam count. got {readout!r}; "
        f"expected to contain {spam_label!r} and {count_text!r}. error: {app.error_text()!r}"
    )

    # ── The doorbell (family-client-enforcement.md § Guardian Notify): the
    # report rang this guardian — whose feed is their own, fresh with this pair
    # — exactly once for (ward, spam, today), and the row opens the Family page.
    # One report is all a run can make inside the real cadence gate, so the
    # once-per-day collapse of LATER reports is the nest's row dedup, proven
    # below the app tier; what this pins is that a report reaches the guardian
    # as one notification that leads where the decision is read.
    doorbell = S.notifications.row_family_content_notice
    app.notifications.navigate()

    def _doorbell_rows() -> list[int] | None:
        rows = [
            i for i, t in enumerate(app.driver.get_texts("notification-item")) if doorbell in t
        ]
        if rows:
            return rows
        app.notifications.navigate()
        return None

    rows = wait_until(
        _doorbell_rows,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"no Notify doorbell; rows={app.driver.get_texts('notification-item')!r}",
    )
    assert len(rows) == 1, f"one report for one category must ring once, got {len(rows)}"
    app.notifications.open_notification(rows[0])
    app.driver.wait_for("family-heading", timeout=UI_SETTLE_S)


# ── Slice F: the guardian-enrolled-device marker (family-safety.md § Full
# visibility for young children). TWO surfaces, one flag:
#   * the WARD-facing half — the `device-guardian-mark-badge` on their own
#     devices page + the delete refusal — driven by the first test below;
#   * the GUARDIAN-facing control — the Family-page `family-device-mark-item`
#     rows and their toggles — driven by the second. The nest ward-device-list
#     it needed (`FamilyWardInfo.devices`) landed 2026-07-19, and the control
#     itself landed on linux + web 2026-08-01.
# The first test stages its mark over the wire (precondition, point 8(b)); the
# second is what proves a guardian's real UI flip sets the very flag the ward
# then sees. ──


def _register_ward_device(nest_url: str, ward_identity: dict, device_id_hex: str, label: str) -> None:
    """Register a sync device on the WARD's account the way a client's sync
    daemon does (`fauna.sync.register`, as the device-owning actor). Fixture
    setup, not the action under test (e2e point 8(b)).

    The label seals under the **ward's own** root (S9 flip, 2026-08-02) — the
    registering actor is the ward, so that is the root a real daemon on their
    device would use, and it is what makes the label render on the ward's OWN
    devices page. ⚠ It does **not** make the label render on the *guardian's*
    Family page: a guardian holds no key for their ward's root, so the nest's
    projection substitutes the device's DISPLAY IDENTITY — the short device-id
    form, `_short_id` — instead (ruled 2026-08-02, family-safety.md § Full
    visibility for young children).
    """
    from fauna_ffi import seal_device_label

    actor_id = bytes.fromhex(ward_identity["actor_id_hex"])
    signing_key = bytes(ward_identity["signing_key"])
    label_sealed = seal_device_label(signing_key, bytes.fromhex(device_id_hex), label)
    payload = {"device_id": device_id_hex, "label": label, "capabilities": "read,write"}
    if label_sealed is not None:
        payload["label_sealed"] = label_sealed
    with WsRpcAdminClient(nest_url, actor_id, signing_key) as ward:
        ward.call("fauna.sync.register", payload)


def _guardian_set_device_mark(
    nest_url: str, guardian_identity: dict, ward_identity: dict, device_id_hex: str, marked: bool
) -> None:
    """The guardian sets/clears `guardian_marked` on one of the ward's devices
    (`fauna.family.device.mark`, link-authorized). Precondition setup here (point
    8(b)) — the mutation UNDER TEST in the test that calls this is the ward's own
    UI-driven devices-page render + delete gestures. The guardian's app-UI path
    to the same RPC is not shortcut away: it is itself the mutation under test in
    `test_family_guardian_device_mark_toggle` below, which is the citation point
    8 requires for this shortcut."""
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    guardian_seed = bytes(guardian_identity["signing_key"])
    ward_actor = bytes.fromhex(ward_identity["actor_id_hex"])
    with WsRpcAdminClient(nest_url, guardian_actor, guardian_seed) as guardian:
        guardian.call(
            "fauna.family.device.mark",
            {"supervised_actor_id": ward_actor, "device_id": device_id_hex, "marked": marked},
        )


def _short_id(hex_id: str) -> str:
    """The canonical short display form of a long hex id — first 12 chars + `…`
    (U+2026). Mirrors `fauna_core::format::short_id` (value-formatting.md
    § Short id), which is what the nest's guardian ward-device projection
    substitutes for a label the guardian cannot open (the 2026-08-02 display-
    identity ruling, family-safety.md § Full visibility for young children)."""
    return hex_id if len(hex_id) <= 12 else hex_id[:12] + "…"


def _device_names(driver) -> list[str]:
    """Every listed device's `device-name`, in card order."""
    return [driver.get_text("device-name", index=i) for i in range(driver.count("device-card"))]


def _device_card_index_by_name(driver, name: str) -> int:
    """Position of the `device-card` whose `device-name` reads `name` — order-
    independent, so the test never depends on the nest's device sort."""
    for i in range(driver.count("device-card")):
        if driver.get_text("device-name", index=i) == name:
            return i
    raise AssertionError(f"no device-card named {name!r}: {driver.diagnose('device-name')}")


@pytest.mark.feature("family-safety")
def test_family_device_marker_badge_and_ward_delete_refusal(admin_app, request, nest_instance):
    """Slice F (`family-safety.md` § Full visibility for young children): the
    ward's OWN devices page renders `device-guardian-mark-badge` on a
    guardian-marked device (transparency by construction), and the ward's attempt
    to delete a marked device surfaces the nest's typed `fauna.sync.guardian_marked`
    refusal on `error-message` (never a silent failure), while an UNmarked device
    stays freely removable. The UI-level twin of the nest conformance test
    `marked_device_refuses_ward_deletion_until_the_guardian_unmarks`. The
    guardian's mark is precondition setup (point 8(b)); the mutations under test
    are the ward's UI-driven page render + delete gestures."""
    app = admin_app

    nest_url = nest_instance["url"]
    # Its OWN ward (one anonymous submit), not `family_pair`: it leaves a
    # guardian-MARKED device behind and deletes another, and its central
    # assertion is a whole-page `device-guardian-mark-badge` count — a marked
    # device carried in from any earlier test would break it.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "device-marker"
    )

    # Precondition (point 8(b)): two devices on the ward's account — the
    # guardian's "enrolled" one (marked) and the child's own (unmarked). Both
    # authenticate as the ward; only the mark distinguishes them.
    marked_id = os.urandom(32).hex()
    plain_id = os.urandom(32).hex()
    _register_ward_device(nest_url, ward_identity, marked_id, "parents-tablet")
    _register_ward_device(nest_url, ward_identity, plain_id, "kids-phone")
    _guardian_set_device_mark(nest_url, guardian_identity, ward_identity, marked_id, True)

    # The ward's own session (a real admitted account, logged in directly).
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)

    d = app.driver
    app.backups.navigate_devices()
    d.wait_for("device-card", timeout=15)
    # Both fixture-registered devices must be listed — anchored BY NAME, never by
    # a total card count. A client whose own sync agent registers a device at
    # post-auth (tui direct-spawns `fauna-sync-agent` and registers a `fauna-tui`
    # device — `sync-agent.md` § Control plane split, the shared
    # `setup_renewal_grant` every native app calls) legitimately lists a third
    # card, which says nothing about the mechanism under test. `== 2` made the
    # roster's own membership a hidden precondition of a guardian-marker test.
    fixtured = {"parents-tablet", "kids-phone"}
    deadline = time.monotonic() + 15.0
    names = _device_names(d)
    while time.monotonic() < deadline and not fixtured <= set(names):
        time.sleep(0.5)
        app.backups.navigate_devices()
        names = _device_names(d)
    assert fixtured <= set(names), (
        f"the ward should see both registered devices; listed {names!r}: "
        f"{d.diagnose('device-card')}"
    )

    # (1) Transparency — the NEW mechanism: exactly the marked device renders the
    # badge; the unmarked device does not.
    assert d.count("device-guardian-mark-badge") == 1, (
        "exactly the guardian-marked device should render device-guardian-mark-badge "
        "(the ward sees which device their guardian enrolled): "
        f"{d.diagnose('device-guardian-mark-badge')}"
    )
    marked_index = _device_card_index_by_name(d, "parents-tablet")
    assert d.is_visible("device-guardian-mark-badge", scope=f"device-card[{marked_index}]"), (
        "the badge should render inside the marked device's card: "
        f"{d.diagnose('device-guardian-mark-badge')}"
    )
    plain_index = _device_card_index_by_name(d, "kids-phone")
    # A count, not a visibility read: an unmarked card can sit below the fold of the
    # devices list, and windows' is_visible is !IsOffscreen, so a badge painted there
    # would read "not visible" and pass vacuously (e2e-conventions.md convention 6).
    # GuardianBadgeVisibility collapses the badge when absent, so count is exact.
    assert d.count("device-guardian-mark-badge", scope=f"device-card[{plain_index}]") == 0, (
        "the unmarked device's card must NOT render the badge"
    )

    # (2) The core promise — the ward cannot unilaterally remove the guardian's
    # device: the delete refusal surfaces on error-message, and the device stays.
    # The barrier first: until the app's account runtime has assembled, ANY
    # removal is refused, and that refusal would satisfy the assertion below
    # without the nest's guardian-mark check ever having been asked.
    await_device_removal_ready(d)
    d.click("device-remove-button", index=marked_index)
    deadline = time.monotonic() + RPC_ROUNDTRIP_S + UI_SETTLE_S
    while time.monotonic() < deadline and not app.has_error():
        time.sleep(0.5)
    assert app.has_error(), (
        "removing a guardian-marked device must surface the nest's "
        "fauna.sync.guardian_marked refusal on error-message, not fail silently: "
        f"{d.diagnose('error-message')}"
    )
    assert "parents-tablet" in _device_names(d), (
        f"the marked device must NOT be removed by the ward's attempt: {d.diagnose('device-card')}"
    )

    # (3) No regression — an UNmarked device is still freely removable by its
    # owner (proves the refusal is specific to the mark, not global breakage).
    plain_index = _device_card_index_by_name(d, "kids-phone")
    d.click("device-remove-button", index=plain_index)
    deadline = time.monotonic() + RPC_ROUNDTRIP_S + UI_SETTLE_S
    while time.monotonic() < deadline and "kids-phone" in _device_names(d):
        time.sleep(0.5)
    names = _device_names(d)
    assert "kids-phone" not in names, (
        f"the ward's own (unmarked) device should delete freely; listed {names!r}: "
        f"{d.diagnose('device-card')}"
    )
    assert "parents-tablet" in names, (
        f"only the unmarked device should have gone; listed {names!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_guardian_device_mark_toggle(admin_app, request, nest_instance):
    """Slice F, guardian half (`family-safety.md` § Full visibility for young
    children): the guardian's Family page renders one `family-device-mark-item`
    per ward device, and its `family-device-mark-toggle` really drives
    `fauna.family.device.mark` — in BOTH directions, nest-confirmed across a
    refetch — with the ward's own device list as the independent witness that
    the flag the guardian's UI set is the same flag the transparency promise
    reads.

    This is the citation the sibling test's `_guardian_set_device_mark` wire
    shortcut needs (e2e point 8): every mutation here is a real UI gesture on
    the guardian's page, never an RPC standing in for the guardian.

    Order matters to the proof. Marking is asserted per row and SCOPED to that
    row: a client that paints the toggle outside its `family-device-mark-item`
    would otherwise read as "nothing on the marked row, nothing on the unmarked
    row" — a false pass on the negative half, exactly how tui's ward-side badge
    shipped broken (fixed). Rows are found by the device's
    DISPLAY IDENTITY, never by position, so the nest's device sort stays out
    of the contract.

    ⚠ The guardian's rows do NOT show the ward's user-chosen labels (ruled
    2026-08-02, family-safety.md § Full visibility for young children): those
    rest sealed under the ward's own root, which the guardian neither holds
    nor may be handed (no guardian key escrow). The nest substitutes the short
    device-id form (`_short_id`), so the guardian-side assertions here key on
    the ids the fixture generated; the ward-side leg still keys on the real
    labels, which the ward's own custody opens.

    linux + web + tui + apple (macos + ios) are the current legs; android's UI
    joined 2026-08-01 but stays gated pending the android e2e bridge
    attr-route (windows remains the UI Slice G lift —
    `require_device_mark_control_supported`).
    """
    app = admin_app
    app.family.require_device_mark_control_supported()

    nest_url = nest_instance["url"]
    # Its OWN ward (one anonymous submit), not `family_pair`: it registers two
    # more sync devices on the ward every time it runs and nothing ever removes
    # them, so a shared ward accumulates devices across app legs until a later
    # leg's registration trips `fauna.sync.device_limit_exceeded`
    #  — marks and then UNmarks its own
    # freshly-registered devices, and asserts only on those two rows.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "device-mark-toggle"
    )

    # Precondition (point 8(b)): two devices on the ward's account. Only the
    # guardian's mark will distinguish them — both authenticate as the ward.
    guardian_device = os.urandom(32).hex()
    child_device = os.urandom(32).hex()
    _register_ward_device(nest_url, ward_identity, guardian_device, "parents-tablet")
    _register_ward_device(nest_url, ward_identity, child_device, "kids-phone")

    # ── Guardian: the Family page lists the ward's devices ──
    from conftest import _login_app_as
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()

    # Generous budget, deadline poll — a green run pays only the real latency
    # (convention 14). The ward row must appear before its editor can load.
    WARD_VISIBLE_BUDGET_S = 20.0
    deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    assert ward_handle in app.family.ward_handles(), (
        f"expected ward {ward_handle!r} in the guardian's list. error: {app.error_text()!r}"
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )

    # The rows themselves — the pure render off `FamilyWardInfo.devices`. The
    # guardian sees each device's display identity (its short id — the ruling
    # above), NOT the sealed labels the fixture registered.
    DEVICE_ROWS_BUDGET_S = 20.0
    guardian_display = _short_id(guardian_device)
    child_display = _short_id(child_device)
    fixtured = {guardian_display, child_display}
    deadline = time.monotonic() + DEVICE_ROWS_BUDGET_S
    rows = app.family.device_mark_labels()
    while time.monotonic() < deadline and not all(
        any(name in (r or "") for r in rows) for name in fixtured
    ):
        time.sleep(0.5)
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
        rows = app.family.device_mark_labels()
    assert all(any(name in (r or "") for r in rows) for name in fixtured), (
        f"the guardian should see both of the ward's devices as device-mark rows, "
        f"each showing its short device id (the display-identity ruling); "
        f"rows read {rows!r}: {app.driver.diagnose('family-device-mark-item')}"
    )
    assert not any("parents-tablet" in (r or "") or "kids-phone" in (r or "") for r in rows), (
        f"the ward's user-chosen labels must NEVER reach the guardian's rows "
        f"(they rest sealed under the ward's root); rows read {rows!r}"
    )

    # Nothing is marked yet — the ward-side badge test's starting state, asserted
    # per row so an unscoped paint cannot fake it.
    guardian_row = app.family.device_mark_index_by_label(guardian_display)
    child_row = app.family.device_mark_index_by_label(child_display)
    assert app.family.device_mark_state(guardian_row) == "off", (
        f"no device should start marked: {app.driver.diagnose('family-device-mark-toggle')}"
    )
    assert app.family.device_mark_state(child_row) == "off", (
        f"no device should start marked: {app.driver.diagnose('family-device-mark-toggle')}"
    )

    # ── (1) The mutation under test: the guardian marks their enrolled device ──
    MARK_CONFIRMED_BUDGET_S = 25.0

    def _states_after_refetch() -> tuple[str, str]:
        """Both rows' toggle states re-read from a fresh `fauna.family.status`
        — the refetch is what makes every assertion nest-confirmed rather than
        a readback of local UI intent."""
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
        g = app.family.device_mark_index_by_label(guardian_display)
        c = app.family.device_mark_index_by_label(child_display)
        return app.family.device_mark_state(g), app.family.device_mark_state(c)

    app.family.set_device_mark(guardian_row, True)
    deadline = time.monotonic() + MARK_CONFIRMED_BUDGET_S
    states = ("", "")
    while time.monotonic() < deadline:
        states = _states_after_refetch()
        if states[0] == "on":
            break
        time.sleep(0.5)
    assert states[0] == "on", (
        f"flipping the guardian device's toggle must persist as guardian_marked "
        f"(states were {states!r}). error: {app.error_text()!r}: "
        f"{app.driver.diagnose('family-device-mark-toggle')}"
    )
    # The negative half, scoped to its own row: marking one device must not mark
    # the other. A flat read would return the marked row's value here.
    assert states[1] == "off", (
        f"marking one device must not mark the ward's own device (states {states!r}): "
        f"{app.driver.diagnose('family-device-mark-toggle')}"
    )

    # ── (2) The ward is the independent witness: their OWN device list now shows
    # the badge on exactly the device the guardian's UI marked ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    d = app.driver
    app.backups.navigate_devices()
    d.wait_for("device-card", timeout=15)
    BADGE_BUDGET_S = 20.0
    deadline = time.monotonic() + BADGE_BUDGET_S
    while time.monotonic() < deadline and "parents-tablet" not in _device_names(d):
        time.sleep(0.5)
        app.backups.navigate_devices()
    marked_index = _device_card_index_by_name(d, "parents-tablet")
    plain_index = _device_card_index_by_name(d, "kids-phone")
    assert d.is_visible("device-guardian-mark-badge", scope=f"device-card[{marked_index}]"), (
        "the ward must see the guardian's badge on the device the guardian's own UI "
        f"marked — the two ends of the transparency promise must agree: "
        f"{d.diagnose('device-guardian-mark-badge')}"
    )
    assert d.count("device-guardian-mark-badge", scope=f"device-card[{plain_index}]") == 0, (
        "the ward's own device must NOT carry the badge"
    )

    # ── (3) The un-enroll step is the same control, backwards: the guardian
    # clears the mark from their page (§ Full visibility — "the guardian removes
    # the device by unmarking it"), and the ward's badge goes with it ──
    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
    while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
        time.sleep(0.5)
        app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    guardian_row = app.family.device_mark_index_by_label(guardian_display)
    assert app.family.device_mark_state(guardian_row) == "on", (
        "the mark should still read on for a guardian returning to the page: "
        f"{app.driver.diagnose('family-device-mark-toggle')}"
    )
    app.family.set_device_mark(guardian_row, False)
    deadline = time.monotonic() + MARK_CONFIRMED_BUDGET_S
    while time.monotonic() < deadline:
        states = _states_after_refetch()
        if states[0] == "off":
            break
        time.sleep(0.5)
    assert states == ("off", "off"), (
        f"unmarking must clear guardian_marked and leave the other device alone "
        f"(states {states!r}). error: {app.error_text()!r}: "
        f"{app.driver.diagnose('family-device-mark-toggle')}"
    )

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.backups.navigate_devices()
    d.wait_for("device-card", timeout=15)
    deadline = time.monotonic() + BADGE_BUDGET_S
    while time.monotonic() < deadline and d.count("device-guardian-mark-badge") > 0:
        time.sleep(0.5)
        app.backups.navigate_devices()
    assert d.count("device-guardian-mark-badge") == 0, (
        "un-enrolling from the guardian's page must clear the ward's badge too — "
        f"the flag has one owner: {d.diagnose('device-guardian-mark-badge')}"
    )


@pytest.mark.feature("family-safety")
def test_family_screen_time_window_lock(admin_app, request, nest_instance, family_pair):
    """Slice E, the window half (`family-client-enforcement.md` § Screen time): a guardian
    sets a usage window through the real editor UI, and the ward's own client
    renders the full-screen `screen-time-lock` when the local clock is outside
    it — with the Family page still reachable read-only. (Clearing the window outright
    is not exercised here; the body moves the guardian from a locking window to
    an allowed one, and
    `test_family_screen_time_empty_field_clears_and_zero_budget_locks` proves
    the empty-field clear.)

    Every mutation is a real UI gesture on the guardian's page (e2e point 8):
    the window is typed into `family-policy-screen-window-*-input` and saved
    with `family-policy-save-button`, never an RPC standing in for the guardian.

    **Latency-independent by construction** (convention 14) — nothing here waits
    on wall-clock *timing*, even though the feature under test is about time.
    The window is computed RELATIVE to the ward device's current local minute
    with a multi-hour margin on both sides, so the verdict it produces is stable
    for hours and cannot flip mid-run:

      * the LOCKED window opens 3h from now and closes 4h from now, so "now" is
        outside it by ≥3h in either direction;
      * the ALLOWED window opens 2h ago and closes 2h ahead, so "now" is inside
        it by ≥2h either way.

    Both are asserted with generous deadline polls, so a green run pays only the
    real latency. Window enforcement is pure client-local clock and needs no
    heartbeat at all, which is exactly why § Screen time builds it first.
    """
    app = admin_app
    app.family.require_screen_time_supported()

    # Shares the module's one admitted pair: sets a screen-time window — reset
    # by `family_pair`.
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    # The window bounds are derived from the ward device's own clock — the same
    # machine runs the app under test, so "now" here is "now" there.
    now = time.localtime()
    now_minutes = now.tm_hour * 60 + now.tm_min

    def hhmm(minutes_from_midnight: int) -> str:
        m = minutes_from_midnight % 1440
        return f"{m // 60:02d}:{m % 60:02d}"

    # Locked: a window that starts in 3h and ends in 4h. Wrapping is legal, so
    # the modulo needs no special-casing near midnight.
    locked_start, locked_end = hhmm(now_minutes + 180), hhmm(now_minutes + 240)
    # Allowed: a window spanning now-2h .. now+2h.
    allowed_start, allowed_end = hhmm(now_minutes - 120), hhmm(now_minutes + 120)

    from conftest import _login_app_as

    WARD_VISIBLE_BUDGET_S = 20.0
    LOCK_BUDGET_S = 30.0

    def _guardian_sets_window(start: str, end: str) -> None:
        """Drive the guardian's real editor: select the ward, type both bounds,
        Save. Asserts the values survive the refetch, so a save the nest refused
        can never be mistaken for one it accepted."""
        app.driver.reset()
        _login_app_as(app, request, nest_instance, guardian_identity)
        app.family.navigate()
        deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
        while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
            time.sleep(0.5)
            app.family.reload()
        assert ward_handle in app.family.ward_handles(), (
            f"expected ward {ward_handle!r} in the guardian's list. "
            f"error: {app.error_text()!r}"
        )
        app.family.select_ward_by_handle(ward_handle)
        assert app.family.policy_editor_visible(), (
            f"policy editor did not load. error: {app.error_text()!r}"
        )
        app.family.set_screen_window(start, end)
        app.family.save_policy()

        # The reload is the round-trip proof: the editor refills from the nest,
        # so this asserts what was STORED, not what was typed.
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
        deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
        while time.monotonic() < deadline and app.family.screen_window_start() != start:
            time.sleep(0.5)
            app.family.reload()
            app.family.select_ward_by_handle(ward_handle)
        assert (app.family.screen_window_start(), app.family.screen_window_end()) == (start, end), (
            f"the guardian's window did not persist: got "
            f"{app.family.screen_window_start()!r}..{app.family.screen_window_end()!r}, "
            f"expected {start!r}..{end!r}. error: {app.error_text()!r}"
        )

    def _ward_lock_state() -> bool:
        """Log the ward in and read the lock off a NON-family page.

        The ward's Family-page visit is the causal trigger: it performs the
        `fauna.family.status` read that refreshes the lock's inputs (the same
        read that fills the read-only policy summary). Navigating away then
        shows the verdict — which is also the goal-doc invariant under test,
        that the Family page stays reachable read-only while locked.
        """
        app.driver.reset()
        _login_app_as(app, request, nest_instance, ward_identity)
        app.family.navigate()
        assert not app.family.screen_lock_visible(), (
            "the Family page must stay reachable read-only while locked "
            "(family-client-enforcement.md § Screen time) — the ward has to be able to see "
            "who supervises them and what the policy is: "
            f"{app.driver.diagnose('screen-time-lock')}"
        )
        app.driver.navigate_to("feed")
        return app.family.screen_lock_visible()

    # ── 1. Outside the window → the ward is locked, and the lock explains itself
    _guardian_sets_window(locked_start, locked_end)
    deadline = time.monotonic() + LOCK_BUDGET_S
    locked = _ward_lock_state()
    while time.monotonic() < deadline and not locked:
        time.sleep(0.5)
        locked = _ward_lock_state()
    assert locked, (
        f"a ward outside their usage window ({locked_start}..{locked_end}, now "
        f"{hhmm(now_minutes)}) must see screen-time-lock. "
        f"error: {app.error_text()!r}: {app.driver.diagnose('screen-time-lock')}"
    )
    message = app.family.screen_lock_message()
    assert guardian_handle in message, (
        "the lock must name the guardian who set the policy (family-client-enforcement.md "
        f"§ Screen time), got {message!r}"
    )
    assert locked_start in message, (
        f"the lock must name when screen time resumes ({locked_start}), got {message!r}"
    )

    # ── 2. Inside the window → no lock. The SAME ward, the same client, one
    # policy edit apart: only the window can explain the difference.
    _guardian_sets_window(allowed_start, allowed_end)
    deadline = time.monotonic() + LOCK_BUDGET_S
    still_locked = _ward_lock_state()
    while time.monotonic() < deadline and still_locked:
        time.sleep(0.5)
        still_locked = _ward_lock_state()
    assert not still_locked, (
        f"a ward INSIDE their usage window ({allowed_start}..{allowed_end}, now "
        f"{hhmm(now_minutes)}) must not be locked. "
        f"{app.driver.diagnose('screen-time-lock')}"
    )


@pytest.mark.feature("family-safety")
def test_family_screen_time_budget_and_usage_readouts(admin_app, request, nest_instance):
    """Slice E, the BUDGET half (`family-client-enforcement.md` § Screen time): a guardian
    sets a daily budget through the real editor UI; both surfaces render the
    ward's usage for the day; the ward's client heartbeats real foreground
    minutes to the nest; and exhausting the budget flips the already-built
    `screen-time-lock` to its `LockedBudget` message.

    Every mutation a *guardian* makes is a real UI gesture (e2e point 8): the
    budget is typed into `family-policy-screen-daily-minutes-input` and saved
    with `family-policy-save-button`, never an RPC standing in for them.

    **The two readouts must agree — that is the pillar's transparency rule.**
    § Screen time: *"The guardian's Family surface shows per-ward usage …; the
    ward's summary shows the same number"*. Both are asserted here, against the
    same figure, from the two different accounts that are supposed to see it.

    **Latency-independent by construction** (convention 14). Screen time is the
    one family pillar whose behavior genuinely is a function of elapsed time, so
    a test that waited for a real heartbeat would be DEFUNCT under testing.md
    § point 14 — minutes long and still untrustworthy under load. Instead the
    ward's clock is advanced through the `screen_time_heartbeat` poke (linux
    `main.rs`, web `#lib/screen-time-e2e.ts`), which runs the app's own
    production heartbeat step over the real `fauna.family.usage_report` wire.
    The cadence and accrual RULES are not faked anywhere — they live in shared
    Rust and are proven exhaustively at tier_1 in `fauna_core::screen_time`.

    No usage window is ever set here, so `in_window` is vacuously true and the
    ONLY thing that can lock this ward is the budget — which is what makes the
    final assertion a proof about the budget arm specifically.
    """
    app = admin_app
    app.family.require_screen_time_supported()

    # Its OWN ward (one anonymous submit), not `family_pair`: the minutes it
    # heartbeats accumulate in `guardian_usage(ward, day)`, which nothing resets
    # short of ending the pairing — a shared ward would carry today's usage
    # into the next app leg's "15 of 120 must not lock" assertion
    # below.
    guardian_identity, ward_identity, guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "screen-time-budget"
    )

    from conftest import _login_app_as

    BUDGET_MINUTES = 120
    WARD_VISIBLE_BUDGET_S = 20.0
    READOUT_BUDGET_S = 30.0
    LOCK_BUDGET_S = RPC_ROUNDTRIP_S + UI_SETTLE_S

    def _guardian_page() -> None:
        """Log in as the guardian and land on the Family page with the ward
        selected in the shared editor."""
        app.driver.reset()
        _login_app_as(app, request, nest_instance, guardian_identity)
        app.family.navigate()
        deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
        while time.monotonic() < deadline and ward_handle not in app.family.ward_handles():
            time.sleep(0.5)
            app.family.reload()
        assert ward_handle in app.family.ward_handles(), (
            f"expected ward {ward_handle!r} in the guardian's list. "
            f"error: {app.error_text()!r}"
        )
        app.family.select_ward_by_handle(ward_handle)

    # ── 1. No budget → NO accounting, and so no readout at all.
    # The negative comes first, while the policy is still untouched: it is the
    # goal doc's "no usage accounting without a declared policy" rule, and
    # asserting it after a budget existed could never distinguish "not rendered"
    # from "rendered stale".
    _guardian_page()
    assert app.family.ward_usage_today_text() == "", (
        "a ward with NO daily budget must show no usage readout at all "
        "(family-client-enforcement.md § Screen time — no accounting without a declared "
        f"policy), got {app.family.ward_usage_today_text()!r}"
    )

    # ── 2. The guardian sets a daily budget through the real editor.
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )
    app.family.set_screen_daily_minutes(str(BUDGET_MINUTES))
    app.family.save_policy()

    # The reload is the round-trip proof: the editor refills from the nest, so
    # this asserts what was STORED, not what was typed.
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    deadline = time.monotonic() + WARD_VISIBLE_BUDGET_S
    while (
        time.monotonic() < deadline
        and app.family.screen_daily_minutes() != str(BUDGET_MINUTES)
    ):
        time.sleep(0.5)
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
    assert app.family.screen_daily_minutes() == str(BUDGET_MINUTES), (
        f"the guardian's daily budget did not persist: got "
        f"{app.family.screen_daily_minutes()!r}, expected {BUDGET_MINUTES!r}. "
        f"error: {app.error_text()!r}"
    )

    # ── 3. …and the guardian's per-ward readout appears, at zero.
    deadline = time.monotonic() + READOUT_BUDGET_S
    while time.monotonic() < deadline and not app.family.ward_usage_today_text():
        time.sleep(0.5)
        app.family.reload()
    guardian_readout = app.family.ward_usage_today_text()
    assert guardian_readout, (
        "a ward WITH a daily budget must show the usage readout "
        f"(family-ward-usage-today): {app.driver.diagnose('family-ward-usage-today')}"
    )
    assert str(BUDGET_MINUTES) in guardian_readout, (
        "the guardian's readout must name the budget the usage is measured "
        f"against, got {guardian_readout!r}"
    )

    # ── 4. The ward sees the SAME number on their own read-only summary.
    # Folded into `family-policy-summary` (the supervised section's element set
    # is `family-guardian-handle` + `family-policy-summary`; usage is a line of
    # "the active policy, read-only"), and resolved through the same shared
    # `usage_today_line` the guardian's row uses.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.family.navigate()
    deadline = time.monotonic() + READOUT_BUDGET_S
    while (
        time.monotonic() < deadline
        and str(BUDGET_MINUTES) not in app.family.policy_summary_text()
    ):
        time.sleep(0.5)
        app.family.reload()
    ward_summary = app.family.policy_summary_text()
    assert str(BUDGET_MINUTES) in ward_summary, (
        "the ward's own summary must show the same screen-time figure their "
        "guardian sees (family-client-enforcement.md § Screen time — transparency), got "
        f"{ward_summary!r}: {app.driver.diagnose('family-policy-summary')}"
    )

    # ── 5. The ward uses the app: the heartbeat reports real foreground minutes
    # and the nest's cross-device total comes back changed. Well under the
    # budget, so nothing locks yet — this isolates the REPORTING path from the
    # enforcement one.
    USED_MINUTES = 15
    app.family.heartbeat(USED_MINUTES)
    deadline = time.monotonic() + READOUT_BUDGET_S
    while (
        time.monotonic() < deadline
        and str(USED_MINUTES) not in app.family.policy_summary_text()
    ):
        time.sleep(0.5)
        app.family.reload()
    ward_summary = app.family.policy_summary_text()
    assert str(USED_MINUTES) in ward_summary, (
        f"after {USED_MINUTES} foreground minutes the ward's summary must show "
        f"them — the heartbeat's whole job is to get that number to the nest "
        f"and back. got {ward_summary!r}"
    )
    app.driver.navigate_to("feed")
    assert not app.family.screen_lock_visible(), (
        f"{USED_MINUTES} of {BUDGET_MINUTES} minutes must NOT lock the ward: "
        f"{app.driver.diagnose('screen-time-lock')}"
    )

    # ── 6. The GUARDIAN sees those same reported minutes — the cross-device
    # accounting round trip, from the ward's device to the guardian's screen.
    _guardian_page()
    deadline = time.monotonic() + READOUT_BUDGET_S
    while (
        time.monotonic() < deadline
        and str(USED_MINUTES) not in app.family.ward_usage_today_text()
    ):
        time.sleep(0.5)
        app.family.reload()
    guardian_readout = app.family.ward_usage_today_text()
    assert str(USED_MINUTES) in guardian_readout, (
        "the guardian must see the minutes their ward's client reported "
        "(family-client-enforcement.md § Screen time — the nest sums heartbeats across "
        f"devices), got {guardian_readout!r}"
    )

    # ── 7. Exhaust the budget → the lock flips to its LockedBudget message.
    # The Family page stays reachable read-only, exactly as under a window lock.
    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.family.navigate()
    # Barrier: the heartbeat engine credits nothing until a status read has
    # told it a budget exists (`HeartbeatEngine::accrue`, `budget_set`), so a
    # poke that beats the post-auth read is silently lost — as in step 4.
    def _budget_read_landed() -> bool:
        if str(BUDGET_MINUTES) in app.family.policy_summary_text():
            return True
        app.family.reload()
        return False

    wait_until(
        _budget_read_landed,
        READOUT_BUDGET_S,
        diagnose=lambda: f"the ward's status read never landed: {app.driver.diagnose('family-policy-summary')}",
    )
    app.family.heartbeat(BUDGET_MINUTES)
    app.driver.navigate_to("feed")
    deadline = time.monotonic() + LOCK_BUDGET_S
    while time.monotonic() < deadline and not app.family.screen_lock_visible():
        time.sleep(0.5)
        app.driver.navigate_to("family")
        app.driver.navigate_to("feed")
    assert app.family.screen_lock_visible(), (
        f"a ward who has used their whole {BUDGET_MINUTES}-minute budget must "
        f"see screen-time-lock. error: {app.error_text()!r}: "
        f"{app.driver.diagnose('screen-time-lock')}"
    )
    message = app.family.screen_lock_message()
    assert guardian_handle in message, (
        "the lock must name the guardian who set the policy (family-client-enforcement.md "
        f"§ Screen time), got {message!r}"
    )
    assert str(BUDGET_MINUTES) in message, (
        "a BUDGET lock must name the budget that ran out — not a window time; "
        "no window was ever set on this ward, so a window message here would "
        f"mean the wrong arm fired. got {message!r}"
    )
    # Read only now that the lock is PROVEN engaged: straight after the
    # heartbeat the lock's inputs have not been refreshed yet (that takes the
    # next `fauna.family.status` read — the loop above), so an absence read there
    # passed whatever the Family page did (a lock-on-family mutant survived it).
    app.driver.navigate_to("family")
    assert not app.family.screen_lock_visible(), (
        "the Family page must stay reachable read-only while locked "
        "(family-client-enforcement.md § Screen time) — the ward has to be able to see who "
        f"supervises them and what the policy is: "
        f"{app.driver.diagnose('screen-time-lock')}"
    )


# ── The guardian's editor rules and the ward's read-only summary. Every
# mutation is a real gesture on the guardian's editor (e2e point 8); the
# readbacks are the editor refilled from the nest and the ward's own
# `family-policy-summary`, so each assertion is about what was STORED. ──


def _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle: str) -> None:
    """Log in as the guardian and load ``ward_handle`` into the shared editor."""
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()

    def _ward_listed() -> bool:
        if ward_handle in app.family.ward_handles():
            return True
        app.family.reload()
        return False

    wait_until(
        _ward_listed,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"ward {ward_handle!r} not in the guardian's list. error: {app.error_text()!r}",
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not load. error: {app.error_text()!r}"
    )


def _reload_editor(app, ward_handle: str) -> None:
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"policy editor did not reload. error: {app.error_text()!r}"
    )


def _ward_summary(app, request, nest_instance, ward_identity, expect) -> str:
    """Log in as the ward and return `family-policy-summary` once ``expect(text)``
    holds — the summary fills from the ward's own status read, so it is polled
    toward the positive, never read once."""
    from conftest import _login_app_as

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    app.family.navigate()

    def _read() -> str | None:
        text = app.family.policy_summary_text()
        if expect(text):
            return text
        app.family.reload()
        return None

    return wait_until(
        _read,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            f"the ward's family-policy-summary never matched: {app.family.policy_summary_text()!r} "
            f"error: {app.error_text()!r}"
        ),
    )


def _push_knock(nest_url: str, recipient_hex: str, sender_hex: str, summary: str) -> None:
    """Store an inbound contact knock — the nest's `test-hooks` door onto the
    production `db.push_knock` path, as `test_knock_sender_display.py` uses it.
    Arranging the world: the stranger is not the user under test."""
    import requests

    resp = requests.post(
        f"{nest_url}/api/v1/test/push/knock",
        json={"actor_id": recipient_hex, "sender_id": sender_hex, "summary": summary},
        timeout=10,
    )
    assert resp.status_code == 200 and resp.json().get("ok") is True, (
        f"test-hooks knock endpoint returned {resp.status_code}: {resp.text}"
    )


def _contact_status(nest_url: str, identity: dict, peer_hex: str) -> str | None:
    """The caller's edge toward ``peer_hex`` (`fauna.contacts.status`) — the API
    readback (convention 5) of what a guardian's decision did to the ward."""
    sk = identity["signing_key"]
    with WsRpcAdminClient(nest_url, bytes(sk.verify_key), bytes(sk)) as client:
        return client.call("fauna.contacts.status", {"peer_id": peer_hex}).get("status")


@pytest.mark.feature("family-safety")
def test_family_guardian_decides_a_strangers_contact_request(
    admin_app, request, nest_instance, family_pair
):
    """family-safety.md § Guardian policy — the enforcement split: with contact
    approval on, a stranger's request to contact the child waits in the
    guardian's queue (`family-approval-item`, showing the request's own text),
    and the guardian accepts or refuses it for the child — an accept makes the
    two contacts, a refusal blocks the stranger on the child's behalf.

    The guardian turns approval on through the editor (point 8); the two
    requests are stored through the nest's knock door, as the strangers'
    side is not under test. Shares the module's pair: both requests are
    decided, so the queue ends empty, and the two strangers are fresh.
    """
    app = admin_app
    url = nest_instance["url"]
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    suffix = _unique_suffix()
    accepted = _admit_adult(nest_instance, f"family-knock-yes-{suffix}")
    refused = _admit_adult(nest_instance, f"family-knock-no-{suffix}")
    yes_text, no_text = f"hello from yes {suffix}", f"hello from no {suffix}"
    _push_knock(url, ward_identity["actor_id_hex"], accepted["actor_id_hex"], yes_text)
    _push_knock(url, ward_identity["actor_id_hex"], refused["actor_id_hex"], no_text)

    def _row_of(text: str) -> int | None:
        rows = [app.family.approval_text(i) for i in range(app.family.approval_count())]
        return next((i for i, row in enumerate(rows) if text in row), None)

    def _both_waiting() -> bool:
        if _row_of(yes_text) is not None and _row_of(no_text) is not None:
            return True
        app.family.reload()
        return False

    wait_until(
        _both_waiting,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "both strangers' requests must wait in the guardian's queue; rows="
            f"{[app.family.approval_text(i) for i in range(app.family.approval_count())]!r} "
            f"error: {app.error_text()!r}"
        ),
    )

    app.family.approve(_row_of(yes_text))
    assert not app.has_error(), f"approve raised: {app.error_text()!r}"
    wait_until(
        lambda: _row_of(yes_text) is None or app.family.reload(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the accepted request must leave the queue. error: {app.error_text()!r}",
    )
    row = _row_of(no_text)
    assert row is not None, "the second request must still wait while the first is decided"
    app.family.deny(row)
    assert not app.has_error(), f"deny raised: {app.error_text()!r}"
    wait_until(
        lambda: _row_of(no_text) is None or app.family.reload(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the refused request must leave the queue. error: {app.error_text()!r}",
    )

    assert _contact_status(url, ward_identity, accepted["actor_id_hex"]) in ("accepted", "confirmed"), (
        "an accepted request must make the stranger the child's contact"
    )
    assert _contact_status(url, ward_identity, refused["actor_id_hex"]) == "blocked", (
        "a refused request must block the stranger on the child's behalf"
    )


@pytest.mark.feature("family-safety")
def test_family_ward_sees_a_strangers_request_waiting_for_the_guardian(
    admin_app, request, nest_instance, family_pair
):
    """family-safety.md § App surface — "pending approval states render on the
    blocked surfaces themselves (the knock row …)": while contact approval is on,
    a stranger's request on the child's own contacts page is marked
    `knock-guardian-pending` ("Waiting for your guardian's approval"), because
    acceptance is then the guardian's (§ Reach approvals). The mark is derived on
    the child's side from the policy it already holds
    (`fauna_client_family::knocks_await_guardian`), so it sits on EVERY knock row
    (convention 17: one mark per `knock-card`), and once the guardian turns
    approval off the same row carries none — the half that lets the first
    assertion fail.

    The guardian turns approval on through the editor (point 8); the request is
    stored through the nest's knock door, as the stranger's side is not under
    test; turning approval back off is the module's own policy reset over the
    wire (arranging the next step, not the action under test).
    """
    from conftest import _login_app_as

    app = admin_app
    app.contacts.require_knock_guardian_pending()
    url = nest_instance["url"]
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    suffix = _unique_suffix()
    stranger = _admit_adult(nest_instance, f"family-knock-wait-{suffix}")
    stranger_hex = stranger["actor_id_hex"]
    _push_knock(url, ward_identity["actor_id_hex"], stranger_hex, f"hello from {suffix}")

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    app.contacts.navigate()

    def _rows() -> tuple[int | None, list[str], int]:
        return (
            app.contacts.knock_index_for(stranger_hex),
            app.contacts.knock_guardian_pending_texts(),
            app.contacts.knock_count(),
        )

    def _marked():
        row, marks, cards = _rows()
        if row is not None and len(marks) == cards:
            return row, marks
        app.contacts.navigate()
        return None

    row, marks = wait_until(
        _marked,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "with contact approval on, every knock row must carry knock-guardian-pending; "
            f"(stranger's row, marks, knock-cards)={_rows()!r} "
            f"{app.driver.diagnose('knock-guardian-pending')} error: {app.error_text()!r}"
        ),
    )
    assert marks[row] == S.contacts.knock_guardian_pending, (
        f"the stranger's row must say it waits for the guardian, read {marks[row]!r}"
    )
    assert set(marks) == {S.contacts.knock_guardian_pending}, f"every mark the same text: {marks!r}"

    # The guardian turns approval off: the child decides again, and the mark goes.
    reset_ward_policy(nest_instance, guardian_identity, ward_identity)

    def _unmarked():
        app.family.navigate()
        app.family.reload()
        app.contacts.navigate()
        row, marks, _cards = _rows()
        # A tuple, never the bare index: row 0 is falsy to wait_until.
        return (row,) if row is not None and not marks else None

    (row,) = wait_until(
        _unmarked,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "with contact approval off the knock stays listed and carries no "
            f"knock-guardian-pending; (stranger's row, marks, knock-cards)={_rows()!r} "
            f"error: {app.error_text()!r}"
        ),
    )
    # Leave no knock behind for the module's later tests: the child, now free to
    # decide, dismisses it through the row.
    app.contacts.dismiss_knock(row)
    wait_until(
        lambda: app.contacts.knock_index_for(stranger_hex) is None or app.contacts.navigate(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"the dismissed knock must leave the list. error: {app.error_text()!r}",
    )


@pytest.mark.feature("family-safety")
def test_family_collapse_floor_folds_content_the_child_can_still_show(
    admin_app, request, nest_instance, family_pair
):
    """family-client-enforcement.md § Content policy — a category is inherit,
    collapse with a reveal, or block: a guardian's `collapse` floor folds a
    labeled post on the child's feed and a labeled message in the child's
    conversation behind `content-policy-blocked-notice` in state `collapsed`,
    with no body, and the child's `content-policy-collapsed-reveal-button`
    shows each one. The category is `commercial` because the child's OWN
    default thresholds cover spam and phishing and would collapse those with no
    guardian involved; nothing but the guardian's floor folds a commercial label.

    The labels are preconditions through the feed and conversation inject seams
    (fixture setup, as the block witnesses above); the floor is set through the
    guardian's editor and the reveal is the child's own tap (point 8).
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_content_render_verdict_supported()
    app.family.require_content_collapse_ids()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    d = app.driver

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_floor("commercial", "collapse")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    assert app.family.content_floor_label("commercial") == S.family.value_collapse, (
        f"the commercial floor did not persist: {app.family.content_floor_label('commercial')!r}"
    )

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    d.wait_for("supervised-indicator", timeout=20)

    # ── The feed: [0] clean, [1] commercial-labeled ──
    count = app.feed.seed_posts([
        {"post_id": "cc-clean", "author": "afriend", "body": "an ordinary post from a friend"},
        {
            "post_id": "cc-commercial",
            "author": "aseller",
            "body": "a commercial post folded away",
            "labels": [{"category": "commercial", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"
    assert d.is_visible("content-policy-blocked-notice", scope="post-card[1]"), (
        f"the labeled post must fold behind the notice. {d.diagnose('content-policy-blocked-notice')}"
        f"; engine sources: {(d.get_state() or {}).get('content_policy_sources')!r}"
    )
    assert d.get_attr("content-policy-blocked-notice", "state", scope="post-card[1]") == "collapsed"
    assert d.count("feed-post-text", scope="post-card[1]") == 0, "a folded post shows no body"
    assert d.is_absent("content-policy-blocked-notice", scope="post-card[0]"), "the clean post is untouched"

    d.click("content-policy-collapsed-reveal-button", scope="post-card[1]")
    wait_until(
        lambda: d.count("feed-post-text", scope="post-card[1]") >= 1,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the child's reveal must show the folded post's body; "
            f"{d.diagnose('content-policy-blocked-notice')} error: {app.error_text()!r}"
        ),
    )
    assert d.is_absent("content-policy-blocked-notice", scope="post-card[1]"), (
        "a revealed post carries no placeholder"
    )

    # ── A conversation: a clean message, then a commercial-labeled one ──
    conv = app.conversations
    sender = f"aseller-{_unique_suffix()}@self-nest.test"
    thread_id = conv.inject_and_resolve_thread(
        rail="FaunaMls", sender=sender, body="an ordinary message from a friend"
    )
    conv.inject_inbound_for_test(
        rail="FaunaMls",
        sender=sender,
        body="a commercial message folded away",
        labels=[{"category": "commercial", "confidence_per_mille": 900}],
    )
    wait_until(
        lambda: any(
            t.thread_id == thread_id and t.message_count >= 2 for t in conv.list_threads()
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "both messages must reach the thread before it is opened",
    )
    conv.open_thread_by_id(thread_id)
    wait_until(
        lambda: d.count("content-policy-blocked-notice") == 1 and d.count("dm-message-text") == 1,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the labeled message must fold behind the notice and the clean one show; "
            f"notices={d.count('content-policy-blocked-notice')}, "
            f"bodies={d.count('dm-message-text')}, error={app.error_text()!r}"
        ),
    )
    assert d.get_attr("content-policy-blocked-notice", "state") == "collapsed"
    d.click("content-policy-collapsed-reveal-button")
    wait_until(
        lambda: d.count("dm-message-text") == 2 and d.count("content-policy-blocked-notice") == 0,
        UI_SETTLE_S,
        diagnose=lambda: (
            "the child's reveal must show the folded message; "
            f"bodies={d.count('dm-message-text')}, error={app.error_text()!r}"
        ),
    )


@pytest.mark.feature("family-safety")
def test_family_editor_states_screen_time_is_enforced_on_the_childs_devices(
    admin_app, request, nest_instance, family_pair
):
    """family-client-enforcement.md § Screen time — "the conforming-client bound
    stated on the editor": the guardian's editor, beside the screen-time inputs,
    says the limits are enforced by the apps on the child's own devices
    (`family-policy-screen-hint`, the shared `family.policy_screen_caveat`)."""
    app = admin_app
    app.family.require_policy_hints()
    guardian_identity, _ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert app.family.screen_hint_text() == S.family.policy_screen_caveat, (
        f"the editor must state the screen-time bound; read {app.family.screen_hint_text()!r}. "
        f"{app.driver.diagnose('family-policy-screen-hint')}"
    )


@pytest.mark.feature("family-safety")
def test_family_editor_states_which_content_categories_are_not_detected_yet(
    admin_app, request, nest_instance, family_pair
):
    """family-client-enforcement.md § Content policy → *Where it binds* — the
    honest bound stated on the editor: the apps do not detect adult content or
    ads yet, so a limit on them waits until a post or message is labelled that
    way (`family-policy-content-hint`, the shared `family.policy_content_hint`)."""
    app = admin_app
    app.family.require_content_hint()
    guardian_identity, _ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert app.family.content_hint_text() == S.family.policy_content_hint, (
        f"the editor must state the content-category bound; read {app.family.content_hint_text()!r}. "
        f"{app.driver.diagnose('family-policy-content-hint')}"
    )


@pytest.mark.feature("family-safety")
def test_family_screen_time_editor_refuses_half_or_empty_window(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Screen time: a usage window whose bounds
    are only half filled in, or that starts and ends at the same minute, is
    refused with its reason on `error-message`, and nothing is stored — the
    editor refills from the nest with the window it had before.

    The reason text is the shared `fauna_core::screen_time` rule's own (every
    app refuses through it, client-side, before the nest would), so the
    assertion keys on its words. Shares the module's pair: nothing is stored.
    """
    app = admin_app
    app.family.require_screen_time_supported()
    guardian_identity, _ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert (app.family.screen_window_start(), app.family.screen_window_end()) == ("", ""), (
        "precondition: the reset pair carries no window"
    )

    for start, end, reason in (
        ("08:00", "", "pairs"),
        ("", "20:00", "pairs"),
        ("09:30", "09:30", "start == end"),
    ):
        app.family.set_screen_window(start, end)
        app.family.save_policy()
        error = wait_until(
            lambda: app.error_text(),
            UI_SETTLE_S,
            diagnose=lambda: f"no refusal on error-message for window {start!r}..{end!r}",
        )
        assert reason in error, (
            f"the refusal of window {start!r}..{end!r} must give its reason "
            f"(expected {reason!r} in it), got {error!r}"
        )
        _reload_editor(app, ward_handle)
        assert (app.family.screen_window_start(), app.family.screen_window_end()) == ("", ""), (
            f"a refused window {start!r}..{end!r} must store nothing, but the editor refilled "
            f"{app.family.screen_window_start()!r}..{app.family.screen_window_end()!r}"
        )


@pytest.mark.feature("family-safety")
def test_family_screen_time_empty_field_clears_and_zero_budget_locks(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Screen time: the guardian removes a limit
    by leaving its field empty, and a budget of zero is a deliberate full lock —
    the ward's app is locked all day with no minute used.

    Order is the proof: a window and a budget are stored (the editor refills
    both), both fields are emptied and saved (the editor refills empty — the
    limits are gone, not left unchanged), then a zero budget alone is saved and
    the ward, who has heartbeated nothing, is locked on the budget arm. Shares
    the module's pair: `family_pair` resets the policy, and a zero budget
    accrues no usage residue (nothing heartbeats).
    """
    app = admin_app
    app.family.require_screen_time_supported()
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_screen_window("07:00", "21:00")
    app.family.set_screen_daily_minutes("90")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    stored = (
        app.family.screen_window_start(),
        app.family.screen_window_end(),
        app.family.screen_daily_minutes(),
    )
    assert stored == ("07:00", "21:00", "90"), f"the limits did not store: {stored!r}"

    app.family.set_screen_window("", "")
    app.family.set_screen_daily_minutes("")
    app.family.save_policy()
    assert not app.has_error(), f"clearing raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    cleared = (
        app.family.screen_window_start(),
        app.family.screen_window_end(),
        app.family.screen_daily_minutes(),
    )
    assert cleared == ("", "", ""), (
        f"emptied fields must remove the limits, but the editor refilled {cleared!r}"
    )

    app.family.set_screen_daily_minutes("0")
    app.family.save_policy()
    assert not app.has_error(), f"zero budget raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    assert app.family.screen_daily_minutes() == "0", (
        f"a zero budget must store as zero, got {app.family.screen_daily_minutes()!r}"
    )

    from conftest import _login_app_as

    def _ward_locked() -> bool:
        app.driver.reset()
        _login_app_as(app, request, nest_instance, ward_identity)
        app.family.navigate()
        app.driver.navigate_to("feed")
        return app.family.screen_lock_visible()

    wait_until(
        _ward_locked,
        RPC_ROUNDTRIP_S * 3,
        interval=0.5,
        diagnose=lambda: (
            f"a zero budget must lock the ward with no minute used. error: {app.error_text()!r} "
            f"{app.driver.diagnose('screen-time-lock')}"
        ),
    )
    message = app.family.screen_lock_message()
    expected = S.family.screen_lock_budget(minutes="0", guardian=guardian_handle)
    assert expected in message, (
        f"the lock must be the budget arm naming the zero budget and the guardian: "
        f"expected {expected!r} in {message!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_ward_summary_lists_content_floors_and_notify(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Content policy + § Guardian Notify: the
    ward's own read-only `family-policy-summary` lists each content category the
    guardian set — with its setting — and shows whether Notify is on; a category
    left at "use my settings" adds no line, and turning Notify off takes its
    line away. Shares the module's pair: `family_pair` resets the floors.
    """
    app = admin_app
    app.family.require_content_render_verdict_supported()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    f = S.family
    notify_label = f.policy_content_notify_label

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_floor("nsfw", "block")
    app.family.set_content_floor("spam", "collapse")
    app.family.set_content_floor("phishing", "block")
    app.family.set_content_notify(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    summary = _ward_summary(
        app, request, nest_instance, ward_identity, lambda t: notify_label in (t or "")
    )
    lines = summary.splitlines() if "\n" in summary else [summary]
    for label, value in (
        (f.policy_content_nsfw_label, f.value_block),
        (f.policy_content_spam_label, f.value_collapse),
        (f.policy_content_phishing_label, f.value_block),
    ):
        assert any(label in line and value in line for line in lines), (
            f"the ward's summary must list {label!r} as {value!r}: {summary!r}"
        )
    assert f.policy_content_commercial_label not in summary, (
        f"a category left at the ward's own setting must add no line: {summary!r}"
    )

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_notify(False)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    summary = _ward_summary(
        app, request, nest_instance, ward_identity, lambda t: bool(t) and notify_label not in t
    )
    assert f.policy_content_nsfw_label in summary, (
        f"turning Notify off must leave the floors listed: {summary!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_guardian_hears_nothing_until_notify_is_on(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Guardian Notify: hidden content is counted
    for the guardian only once they turn Notify on. With the spam floor at block
    and Notify OFF, the ward's app hides a spam post and is made to run its
    report check now — and the guardian's per-ward readout stays empty.

    The negative is anchored on the ward's own completed check
    (`family_notify_check_now` returns after the client ran it), never on a wait.
    The ON arm is `test_family_guardian_notify_surfaces_flagged_count`.

    Shares the module's pair, deliberately not its own ward: the module is at
    nine of the shipped ten anonymous submits (`submit_invite_request`), and a
    CORRECT build writes nothing here — only a build that counts with Notify off
    would leave a `guardian_content_notices` row on the shared ward, and that
    build already fails this test.
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_guardian_notify_client_supported()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_floor("spam", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    assert app.family.content_notify_state() == "off", (
        f"precondition: Notify is off by default, got {app.family.content_notify_state()!r}"
    )

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    count = app.feed.seed_posts([
        {"post_id": "gn-off-clean", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": "gn-off-spam",
            "author": "aspammer",
            "body": "this body must never render",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"
    wait_until(
        lambda: app.family.content_notice_blocked("post-card[1]"),
        UI_SETTLE_S,
        diagnose=lambda: "precondition: the spam floor must hide the post — nothing to count otherwise",
    )
    app.driver.call_command("family_notify_check_now", timeout=20)

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert app.family.ward_content_notices_text(0) == "", (
        "with Notify off the guardian must hear nothing about hidden content, but "
        f"the readout shows {app.family.ward_content_notices_text(0)!r}"
    )


# ── The child's own filter beside the guardian's floor, Notify's scope, and
# offline enforcement (`family-client-enforcement.md` § Content policy, § Guardian
# Notify). Each shares the module's pair: `family_pair` resets the floors,
# Notify and screen time, and a test that moves the ward's OWN spam threshold
# puts it back in a `finally`. ──


def _save_own_spam_threshold_in_settings(app, nest_instance, user: dict, typed: str, per_mille: int) -> None:
    """The signed-in user moves their own `spam-threshold` and saves it — the
    real Settings gesture — then waits until the nest stores ``per_mille``
    (`fauna.spam.get_preferences`, convention 5's readback), so a save the app
    never sent cannot pass for one it did."""
    app.settings.navigate()
    app.settings.save_spam_preferences(spam_threshold=typed)

    def _stored() -> bool:
        with actor_ws(nest_instance, user) as ws:
            return ws.call("fauna.spam.get_preferences", {}).get("spam_threshold") == per_mille

    wait_until(
        _stored,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the own spam threshold {typed!r} never reached the nest as {per_mille} "
            f"per-mille; error: {app.error_text()!r}"
        ),
    )


@pytest.mark.feature("family-safety")
def test_family_ward_can_make_their_own_filter_stricter_than_the_guardians_never_looser(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Content policy: the guardian's floor and
    the child's own spam threshold compose strictest-wins — "a ward may set
    themselves stricter than the guardian floor, never looser".

    The guardian sets `spam` to `collapse`, which bites at the shared trigger
    (`fauna_core::obligation::GUARDIAN_FLOOR_TRIGGER_PERMILLE`, 500). One
    conversation holds three messages: a clean one, a spam label at 900 (over
    the trigger) and one at 300 (under it). The child then moves their own
    threshold through Settings, twice:

      * **looser** — 900 (a stop on the settings slider's 0.1 grid,
        `settings.md` § the spam threshold's `SPAM_THRESHOLD_STEP_PER_MILLE`): the
        300 message shows, but the 900 one stays folded away. The child's own
        looser setting does not undo the guardian's floor;
      * **stricter** — 200: now the 300 message folds away too, which the
        guardian's floor alone would never do.

    The witness is the set of message bodies that render, which reads the same
    on every app whether or not it tags the collapse arm. Each poll re-opens the
    thread, so it renders against the thresholds the app holds now.
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_content_render_verdict_supported()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_floor("spam", "collapse")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    assert app.family.content_floor_label("spam") == S.family.value_collapse, (
        f"the spam floor did not persist as collapse: {app.family.content_floor_label('spam')!r}"
    )

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)

    conv = app.conversations
    d = app.driver
    sender = "aneighbour-own@self-nest.test"
    clean = "a plain note from a neighbour"
    over_floor = "a message the labeler scored well over the floor"
    under_floor = "a borderline message the labeler scored low"
    thread_id = conv.inject_and_resolve_thread(rail="FaunaMls", sender=sender, body=clean)
    for body, confidence in ((over_floor, 900), (under_floor, 300)):
        conv.inject_inbound_for_test(
            rail="FaunaMls",
            sender=sender,
            body=body,
            labels=[{"category": "spam", "confidence_per_mille": confidence}],
        )

    def _all_three_stored() -> bool:
        t = next((t for t in conv.list_threads() if t.thread_id == thread_id), None)
        return t is not None and t.message_count >= 3

    wait_until(
        _all_three_stored,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the thread never held all three messages; error={app.error_text()!r}",
    )

    def _bodies() -> list[str]:
        conv.open_thread_by_id(thread_id)
        return sorted((t or "").strip() for t in d.get_texts("dm-message-text"))

    def _await_bodies(want: list[str], why: str) -> None:
        wait_until(
            lambda: _bodies() == sorted(want),
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            interval=1.0,
            diagnose=lambda: f"{why}; rendered bodies {_bodies()!r}, error={app.error_text()!r}",
        )

    try:
        # ── Looser: the child's own permissive setting cannot undo the floor ──
        _save_own_spam_threshold_in_settings(app, nest_instance, ward_identity, "0.9", 900)
        conv.navigate()
        _await_bodies(
            [clean, under_floor],
            "with the child's own threshold at 900 the 300 message must show, and the "
            "900 one must stay folded under the guardian's collapse floor",
        )

        # ── Stricter: the child's own aggressive setting folds what the floor lets through ──
        _save_own_spam_threshold_in_settings(app, nest_instance, ward_identity, "0.2", 200)
        conv.navigate()
        _await_bodies(
            [clean],
            "with the child's own threshold at 200 the 300 message must fold away too "
            "(stricter than the guardian's floor)",
        )
    finally:
        _set_own_spam_threshold(nest_instance, ward_identity, _DEFAULT_OWN_SPAM_THRESHOLD)


@pytest.mark.feature("family-safety")
def test_family_guardian_notify_counts_only_what_the_guardians_limits_hid(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Guardian Notify: with Notify ON, the
    ward's app counts what the GUARDIAN's floor hid, never what the child's own
    settings hid. The guardian turns Notify on and leaves the spam floor at
    "use my settings"; the child's own spam threshold (the default, 500) folds
    away a spam post at 900; the ward's app is made to run its report check
    now; and the guardian's per-ward readout stays empty.

    The OFF arm is `test_family_guardian_hears_nothing_until_notify_is_on`, the
    ON arm for a guardian-floor hide `test_family_guardian_notify_surfaces_flagged_count`.
    The negative is anchored on the ward's own completed check
    (`family_notify_check_now` returns after the app ran it), never on a wait.

    Shares the module's pair for that sibling's reason: a CORRECT build writes
    nothing here, so the shared ward carries no residue.
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_guardian_notify_client_supported()
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    # The child's own threshold is what hides the post — pinned, not assumed.
    _set_own_spam_threshold(nest_instance, ward_identity, _DEFAULT_OWN_SPAM_THRESHOLD)

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_notify(True)
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"
    _reload_editor(app, ward_handle)
    assert app.family.content_notify_state() == "on", (
        f"Notify did not persist: {app.family.content_notify_state()!r}"
    )
    assert app.family.content_floor_label("spam") == S.family.value_inherit, (
        "precondition: the guardian's spam floor must be left to the child's own "
        f"settings, got {app.family.content_floor_label('spam')!r}"
    )

    app.driver.reset()
    _login_app_as(app, request, nest_instance, ward_identity)
    app.driver.wait_for("supervised-indicator", timeout=20)
    count = app.feed.seed_posts([
        {"post_id": "gn-own-clean", "author": "afriend", "body": "an ordinary post"},
        {
            "post_id": "gn-own-spam",
            "author": "aspammer",
            "body": "this body is folded by the child's own setting",
            "labels": [{"category": "spam", "confidence_per_mille": 900}],
        },
    ])
    assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"
    d = app.driver
    wait_until(
        lambda: d.count("feed-post-text", scope="post-card[1]") == 0
        and d.count("feed-post-text", scope="post-card[0]") >= 1,
        UI_SETTLE_S,
        diagnose=lambda: (
            "precondition: the child's own threshold must fold the spam post away — "
            f"nothing is hidden otherwise; {d.diagnose('feed-post-text')}; engine sources: "
            f"{(d.get_state() or {}).get('content_policy_sources')!r}"
        ),
    )
    assert not app.family.content_notice_blocked("post-card[1]"), (
        "the spam post must be folded by the child's own setting, not blocked by a "
        "guardian floor — the floor was left at the child's settings"
    )
    app.driver.call_command("family_notify_check_now", timeout=20)

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    assert app.family.ward_content_notices_text(0) == "", (
        "what the child's own setting hid must never be reported to the guardian, but "
        f"the readout shows {app.family.ward_content_notices_text(0)!r}"
    )


@pytest.mark.feature("family-safety")
def test_family_limits_and_lock_hold_while_the_app_cannot_reach_the_nest(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Content policy, the unfetched-policy ruling
    (clauses 1–2), and § Screen time: a policy the app could not FETCH keeps the
    last-known state in force. The ward's app reads the policy once, live; it
    is then relaunched while the nest holds every `fauna.family.status` reply,
    so it starts without reaching the nest — and the content floor and the
    screen-time lock still bind, from the persisted snapshot.

    Two relaunches, one per pillar, because a lock hides the feed the floor
    renders on: (1) a spam `block` floor blocks a spam post; (2) a usage window
    that opens 3h from now and closes 4h from now locks the feed (the window
    arithmetic is `test_family_screen_time_window_lock`'s — hours of margin on
    both sides, so the verdict cannot flip mid-run).

    The hold is armed BEFORE each relaunch, and the reply is proven still held
    once each assertion has run — the pending-window pattern of
    `test_family_supervised_indicator_shows_restored_guardian_while_status_is_pending`.
    """
    from conftest import _login_app_as

    app = admin_app
    app.family.require_content_render_verdict_supported()
    app.family.require_screen_time_supported()
    port = nest_instance["port"]
    guardian_identity, ward_identity, guardian_handle, ward_handle = family_pair

    def _ward_reads_live_then_relaunches_unreachable(live_ready, while_unreachable) -> None:
        """Sign the ward in, wait for ``live_ready`` (the live read landed, and
        with it the snapshot), then relaunch with the status read held and run
        ``while_unreachable``."""
        app.driver.reset()
        _login_app_as(app, request, nest_instance, ward_identity)
        live_ready()
        _require_a_relaunch_that_keeps_the_session(app)
        arm_rpc_hold(port, FAMILY_STATUS_KIND)
        try:
            assert app.driver.recover(), "the app did not come back up after the relaunch"
            reached_authenticated_app(app.driver, timeout=90)
            wait_for_held_rpc(
                port, FAMILY_STATUS_KIND, diagnose=lambda: f"error={app.error_text()!r}"
            )
            while_unreachable()
            assert rpc_hold_status(port, FAMILY_STATUS_KIND)["holding"] >= 1, (
                "the status reply was released before the assertions ran, so they "
                "proved nothing about an app that cannot reach the nest"
            )
        finally:
            release_rpc_hold(port, FAMILY_STATUS_KIND)

    # ── 1. The content floor ──
    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_content_floor("spam", "block")
    app.family.save_policy()
    assert not app.has_error(), f"save raised: {app.error_text()!r}"

    def _indicator_names_guardian() -> None:
        # Gate on count(): linux mounts the indicator only once the live read
        # lands, and get_text() raises LookupError on an absent element.
        wait_until(
            lambda: app.driver.count("supervised-indicator") > 0
            and guardian_handle in (app.driver.get_text("supervised-indicator") or ""),
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            diagnose=lambda: f"the live read never landed; error={app.error_text()!r}",
        )

    def _floor_still_blocks() -> None:
        count = app.feed.seed_posts([
            {"post_id": "offline-clean", "author": "afriend", "body": "an ordinary post"},
            {
                "post_id": "offline-spam",
                "author": "aspammer",
                "body": "this body must never render",
                "labels": [{"category": "spam", "confidence_per_mille": 900}],
            },
        ])
        assert count == 2, f"expected 2 seeded post-cards, got {count}. error: {app.error_text()!r}"
        wait_until(
            lambda: app.family.content_notice_blocked("post-card[1]"),
            UI_SETTLE_S,
            diagnose=lambda: (
                "with fauna.family.status unanswered the restored spam floor must still "
                "block the spam post — an app that renders it treated 'no answer' as "
                f"'no floor'; {app.driver.diagnose('content-policy-blocked-notice')}"
            ),
        )
        assert app.driver.count("feed-post-text", scope="post-card[1]") == 0, (
            "a blocked post must not render its body text"
        )

    _ward_reads_live_then_relaunches_unreachable(_indicator_names_guardian, _floor_still_blocks)

    # ── 2. The screen-time lock ──
    now = time.localtime()
    now_minutes = now.tm_hour * 60 + now.tm_min

    def hhmm(minutes_from_midnight: int) -> str:
        m = minutes_from_midnight % 1440
        return f"{m // 60:02d}:{m % 60:02d}"

    locked_start, locked_end = hhmm(now_minutes + 180), hhmm(now_minutes + 240)
    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)
    app.family.set_screen_window(locked_start, locked_end)
    app.family.save_policy()
    _reload_editor(app, ward_handle)
    stored = (app.family.screen_window_start(), app.family.screen_window_end())
    assert stored == (locked_start, locked_end), (
        f"the guardian's window did not persist: got {stored!r}, expected "
        f"{(locked_start, locked_end)!r}. error: {app.error_text()!r}"
    )

    def _locked_live() -> None:
        def _locked() -> bool:
            app.family.navigate()
            app.driver.navigate_to("feed")
            return app.family.screen_lock_visible()

        wait_until(
            _locked,
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            interval=1.0,
            diagnose=lambda: (
                f"precondition: outside the window ({locked_start}..{locked_end}, now "
                f"{hhmm(now_minutes)}) the live read must lock the ward; "
                f"{app.driver.diagnose('screen-time-lock')}"
            ),
        )

    def _lock_still_up() -> None:
        app.driver.navigate_to("feed")
        wait_until(
            app.family.screen_lock_visible,
            UI_SETTLE_S,
            diagnose=lambda: (
                "with fauna.family.status unanswered the restored window must still lock "
                "the ward — airplane mode is not a bedtime-lock bypass; "
                f"{app.driver.diagnose('screen-time-lock')}"
            ),
        )
        message = app.family.screen_lock_message()
        assert guardian_handle in message and locked_start in message, (
            "the restored lock must still name the guardian and when screen time "
            f"resumes ({locked_start}), got {message!r}"
        )

    _ward_reads_live_then_relaunches_unreachable(_locked_live, _lock_still_up)


# ── Screen time across devices and days (`family-client-enforcement.md`
# § Screen time). The app under test is one of the ward's devices; the ward's
# OTHER device is the wire (`fauna.family.usage_report` signed as the ward — the
# nest accounts per account, never per device). Both tests share the module's
# pair and are residue-proof by construction: the minutes earlier tests and app
# legs left on the ward's day are READ first (a zero-minute report is a read),
# and the budget is set just above them, so no assertion depends on the ward
# starting the day at zero. The budget is arranged over the wire — the
# guardian's editor gesture for it is `test_family_screen_time_budget_and_usage_readouts`'s
# subject. ──


def _local_utc_offset_minutes() -> int:
    """This machine's UTC offset — the one the app under test reports, since it
    runs here (`fauna_core::caltime::local_utc_offset_seconds`)."""
    offset = datetime.now().astimezone().utcoffset()
    return int(offset.total_seconds() // 60) if offset is not None else 0


def _local_day(utc_offset_minutes: int) -> int:
    """`fauna_core::day_bucket::local_day_bucket` for now at ``utc_offset_minutes``."""
    return (int(time.time()) + utc_offset_minutes * 60) // 86_400


def _usage_report_from_another_device(
    nest_instance, ward_identity: dict, minutes: int, utc_offset_minutes: int
) -> dict:
    """One `fauna.family.usage_report` from another of the ward's devices; the
    reply is ``{day, day_total_minutes}`` for the day the nest stamped."""
    with actor_ws(nest_instance, ward_identity) as ward:
        return ward.call(
            "fauna.family.usage_report",
            {"minutes": minutes, "utc_offset_minutes": utc_offset_minutes},
        )


def _budget_above_todays_use(
    nest_instance, guardian_identity: dict, ward_identity: dict, headroom: int
) -> tuple[int, int]:
    """Set the ward's daily budget ``headroom`` minutes above what they have
    already used today; return ``(budget, used_today)``. The nest accounts
    only while a budget is set, so the read happens under a placeholder one."""
    set_ward_policy(
        nest_instance, guardian_identity, ward_identity, screen_time={"daily_minutes": 1440}
    )
    used = _usage_report_from_another_device(
        nest_instance, ward_identity, 0, _local_utc_offset_minutes()
    )["day_total_minutes"]
    budget = used + headroom
    assert budget <= 1440, (
        f"the shared ward has already used {used} minutes today, so no budget can sit "
        f"{headroom} above it — something left far more residue than this module makes"
    )
    set_ward_policy(
        nest_instance, guardian_identity, ward_identity, screen_time={"daily_minutes": budget}
    )
    return budget, used


def _minutes_used(readout: str, budget: int) -> int | None:
    """The used figure in a "{used} of {budget} minutes" readout, or ``None``."""
    import re

    marker = "§"
    pattern = re.escape(S.family.ward_usage_today_of_budget(used=marker, budget=str(budget)))
    match = re.search(pattern.replace(marker, r"(\d+)"), readout or "")
    return int(match.group(1)) if match else None


def _ward_signs_in_under_the_budget(app, request, nest_instance, ward_identity, budget: int, used: int) -> None:
    """Sign the ward in and wait until their own summary shows ``used`` of
    ``budget`` — the status read that loads the budget into the app's heartbeat,
    which reports nothing until it holds one."""
    _ward_summary(
        app,
        request,
        nest_instance,
        ward_identity,
        lambda text: _minutes_used(text or "", budget) == used,
    )


def _both_readouts_show(
    app, request, nest_instance, family, budget: int, at_least: int, at_most: int
) -> int:
    """The ward's own summary and the guardian's per-ward row both show one used
    figure in ``[at_least, at_most]`` against ``budget`` — the same number on
    both (§ Screen time: "the ward's summary shows the same number"). Returns it.

    A range, not one number: the app keeps crediting real foreground seconds
    between the pokes, so how many whole minutes it adds beyond the poked ones
    depends on how long the run took (convention 14) — ``at_most`` is the
    caller's bound on that, derived from the run's own elapsed time."""
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family

    def _in_range(text: str | None) -> bool:
        used = _minutes_used(text or "", budget)
        return used is not None and at_least <= used <= at_most

    ward_used = _minutes_used(
        _ward_summary(app, request, nest_instance, ward_identity, _in_range), budget
    )

    _guardian_opens_editor(app, request, nest_instance, guardian_identity, ward_handle)

    def _guardian_agrees() -> bool:
        if _minutes_used(app.family.ward_usage_today_text(), budget) == ward_used:
            return True
        app.family.reload()
        return False

    wait_until(
        _guardian_agrees,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"the guardian's readout must show the ward's own {ward_used} of {budget}, got "
            f"{app.family.ward_usage_today_text()!r}; error: {app.error_text()!r}"
        ),
    )
    return ward_used


def _real_minutes_since(started: float) -> int:
    """Whole minutes of real time since ``started`` (monotonic), rounded up, plus
    one for a partial minute the app may have carried in — the most real
    foreground time the app can have credited on top of the poked minutes."""
    return int((time.monotonic() - started) // 60) + 2


@pytest.mark.feature("family-safety")
def test_family_screen_time_budget_counts_use_on_every_device_of_the_ward(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Screen time: "Daily-budget enforcement
    needs cross-device accounting (a per-device budget would multiply by device
    count)". The ward uses the app under test for 10 minutes — well inside the
    budget, no lock; another of their devices then reports the 20 minutes that
    reach it; and at the app's next heartbeat, which returns the nest's
    cross-device total, the app locks though its own use is far under the
    budget. Both readouts then show the sum.

    Clock movement is the `screen_time_heartbeat` poke (convention 14's fake
    clock over the real `fauna.family.usage_report` wire), as in
    `test_family_screen_time_budget_and_usage_readouts`.
    """
    app = admin_app
    app.family.require_screen_time_supported()
    guardian_identity, ward_identity, guardian_handle, _ward_handle = family_pair
    started = time.monotonic()

    APP_MINUTES = 10
    OTHER_DEVICE_MINUTES = 20
    NEXT_HEARTBEAT_MINUTES = 6  # past the 5-minute report cadence, so it is due
    budget, used_before = _budget_above_todays_use(
        nest_instance, guardian_identity, ward_identity, APP_MINUTES + OTHER_DEVICE_MINUTES
    )

    # ── 1. The app alone: 10 minutes, under the budget, no lock ──
    _ward_signs_in_under_the_budget(app, request, nest_instance, ward_identity, budget, used_before)
    app.family.heartbeat(APP_MINUTES)
    app.driver.navigate_to("feed")
    assert not app.family.screen_lock_visible(), (
        f"{APP_MINUTES} minutes on this device, {used_before + APP_MINUTES} today of a "
        f"{budget}-minute budget, must not lock: {app.driver.diagnose('screen-time-lock')}"
    )

    # ── 2. Another device of the ward's uses the rest of the day's budget ──
    reply = _usage_report_from_another_device(
        nest_instance, ward_identity, OTHER_DEVICE_MINUTES, _local_utc_offset_minutes()
    )
    assert reply["day_total_minutes"] >= budget, (
        "the nest must sum the two devices' minutes into one day's total: after "
        f"{used_before} + {APP_MINUTES} + {OTHER_DEVICE_MINUTES} it reports {reply!r}"
    )

    # ── 3. The app's next heartbeat learns the total, and the ward is locked ──
    app.family.heartbeat(NEXT_HEARTBEAT_MINUTES)

    def _locked() -> bool:
        app.driver.navigate_to("family")
        app.driver.navigate_to("feed")
        return app.family.screen_lock_visible()

    wait_until(
        _locked,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        interval=1.0,
        diagnose=lambda: (
            f"the ward's devices together used {reply['day_total_minutes']} of a "
            f"{budget}-minute budget, so this app must lock though it used only "
            f"{APP_MINUTES + NEXT_HEARTBEAT_MINUTES} itself; "
            f"{app.driver.diagnose('screen-time-lock')}"
        ),
    )
    message = app.family.screen_lock_message()
    assert guardian_handle in message and str(budget) in message, (
        f"the budget lock must name the guardian and the {budget}-minute budget, got {message!r}"
    )

    # ── 4. Both readouts show the sum of every device ──
    floor = used_before + APP_MINUTES + OTHER_DEVICE_MINUTES + NEXT_HEARTBEAT_MINUTES
    _both_readouts_show(
        app,
        request,
        nest_instance,
        family_pair,
        budget,
        at_least=floor,
        at_most=floor + _real_minutes_since(started),
    )


@pytest.mark.feature("family-safety")
def test_family_screen_time_budget_starts_over_at_the_wards_local_midnight(
    admin_app, request, nest_instance, family_pair
):
    """`family-client-enforcement.md` § Screen time: "A daily budget must reset at
    the ward's local midnight" — the day bucket is the ward's LOCAL day, which
    the nest stamps from its own clock and the offset each report carries.

    Latency-independent (convention 14), never a wait for midnight: another of
    the ward's devices reports from a clock an hour on the far side of a local
    midnight — 23:00 yesterday where the offset clamp allows it, else 01:00
    tomorrow — and uses the WHOLE budget there. Those minutes land in another
    local day's bucket, so the app under test (reporting at this machine's
    offset) is not locked, and both readouts show only today's minutes. A nest
    that bucketed by UTC day, or ignored the offset, would put both reports in
    one day and lock the app.
    """
    app = admin_app
    app.family.require_screen_time_supported()
    guardian_identity, ward_identity, _guardian_handle, _ward_handle = family_pair
    started = time.monotonic()

    APP_MINUTES = 10
    NEXT_HEARTBEAT_MINUTES = 6
    budget, used_before = _budget_above_todays_use(
        nest_instance, guardian_identity, ward_identity, 30
    )

    # The other device's offset: its local clock an hour past yesterday's 23:00
    # mark or tomorrow's 01:00 mark relative to this machine's local day. The two
    # windows meet, so one always fits the clamp (`-720..=840`).
    here = _local_utc_offset_minutes()
    local = time.localtime()
    minutes_into_today = local.tm_hour * 60 + local.tm_min
    yesterday = here - minutes_into_today - 60
    tomorrow = here + (1440 - minutes_into_today) + 60
    other_offset = yesterday if yesterday >= -720 else tomorrow
    assert -720 <= other_offset <= 840, (here, minutes_into_today, other_offset)

    # ── 1. The app: 10 minutes today ──
    _ward_signs_in_under_the_budget(app, request, nest_instance, ward_identity, budget, used_before)
    app.family.heartbeat(APP_MINUTES)

    # ── 2. The other device uses the whole budget in its own local day ──
    reply = _usage_report_from_another_device(nest_instance, ward_identity, budget, other_offset)
    assert reply["day"] == _local_day(other_offset) and reply["day"] != _local_day(here), (
        f"the nest must stamp the other device's report into ITS local day "
        f"({_local_day(other_offset)}), not this machine's ({_local_day(here)}): {reply!r}"
    )
    assert reply["day_total_minutes"] >= budget, (
        f"precondition: the other local day must hold the whole {budget}-minute budget: {reply!r}"
    )

    # ── 3. The app's next heartbeat, at this machine's offset: today, not locked ──
    app.family.heartbeat(NEXT_HEARTBEAT_MINUTES)
    floor = used_before + APP_MINUTES + NEXT_HEARTBEAT_MINUTES
    used = _both_readouts_show(
        app,
        request,
        nest_instance,
        family_pair,
        budget,
        at_least=floor,
        at_most=floor + _real_minutes_since(started),
    )
    assert used < budget, (
        f"today's figure {used} must not include the other day's {budget} minutes"
    )
    assert _local_day(here) != reply["day"], (
        "the run crossed this machine's local midnight into the other device's day, "
        "so the assertions above did not separate two days — the offset choice "
        "above has the wrong margin"
    )

    _ward_signs_in_under_the_budget(app, request, nest_instance, ward_identity, budget, used)
    app.driver.navigate_to("feed")
    assert not app.family.screen_lock_visible(), (
        f"the whole {budget}-minute budget was used in ANOTHER local day; today's "
        f"{used} minutes must not lock the ward: {app.driver.diagnose('screen-time-lock')}"
    )
