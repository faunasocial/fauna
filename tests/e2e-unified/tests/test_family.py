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
      the ONLY supervised-admission path this shared test nest can exercise:
      `nest_instance` is started registration-CLOSED by design (many other tests
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

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.launch_harness import reached_authenticated_app
from helpers.app_surface import skip_unbuilt
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


def _admin_client(nest_instance):
    """A WS-RPC connection signing as the nest ADMIN — the fixture-setup door for
    the admissions below (`WsRpcAdminClient` is actor-agnostic; it authenticates
    as whatever key it is handed)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest_instance["url"], bytes(admin_sk.verify_key), bytes(admin_sk)
    )


def _submit_invite_request(nest_url: str, handle: str, message: str = "let me in") -> dict:
    """Seed a real Ed25519-signed pending invite-request -- fixture setup
    (not the action under test), mirrors test_admin_users_hub.py's helper.
    Returns the requester's identity so the caller can log back in as them
    once admitted.

    Submits ONCE. This helper used to ride out the nest's anti-flood throttle
    (`anonymous_rate_limit.rs::invite_request_config`) with a retry loop and a
    five-second wait against a 90 s deadline, because 10 submits / 60 s per
    source IP is a budget every test in this session-scoped module shares from
    127.0.0.1 -- it "surfaced as a bogus red on tui while slower clients slipped
    under it". That workaround was the wall-clock-dependent class the project's
    testing conventions rule DEFUNCT, and that row 303 names as the one fix to
    avoid; it also had the worse effect of HIDING the budget, since the throttle
    was firing in real runs while the row that owned it read as unmeasured.
    The budget is raised at compile time in a `test-hooks` build
    (`INVITE_REQUEST_MAX_EVENTS`, the same split `register_config` already had),
    so a rate-limited reply here is a genuine finding again -- let it raise.

    ⚠ **THE `test-hooks` LIFT IS NOT AVAILABLE IN EVERY MODE, AND THIS MODULE
    FITS THE SHIPPED BUDGET.** `conftest.py`'s `build_node` compiles the
    standalone nest `--features test-hooks`, but the Docker image is a RELEASE
    build (`Dockerfile:185`) and convention 15 forbids putting the automation
    surface in a release artifact -- so under `--nest docker` this surface binds
    at the shipped **10 submits / 60 s / source**, and every actor in a
    container run arrives from the ONE bridge-gateway address. That mode is the
    release-candidate vehicle (`testing.md` § Default app and nest mode), so a
    family-safety cell blanked by the budget is a coverage hole, not a
    classification: this module's subject is the family PAGE, never the budget,
    so it MUST fit (`testing.md`'s fit-or-classify rule).

    Measured 2026-08-29: at 2 submits per test the module spent exactly the 10
    it is allowed by its 6th test and every later test raised
    `fauna.protocol.rate_limited` here. Pacing the calls is not the fix (that is
    the deleted five-second retry loop above all over again -- convention 14, and
    naming it here in code would itself trip the sleep ratchet); REDUCING THE
    COUNT is. So: every non-ward account comes from `_admit_adult_directly`
    (0 submits), and the read/policy tests share ONE ward through
    `family_pair` (1 submit for the module). Only these callers pay:

      * `test_family_admission_and_ward_view` -- 2, and they are the point: the
        admin's UI approval of a real pending request IS its subject.
      * the six tests that consume or dirty the pairing (transfer-accept,
        guardian-sees-ward, device-marker, guardian-notify,
        screen-time-budget, device-mark-toggle) -- 1 each, for their own ward.
        The last three moved here off `family_pair` 2026-09-15: each grows a
        day-scoped or ever-growing counter on the ward that no wire surface
        can clear short of ending the pairing, so the shared ward carried it
        from one app leg into the next under a multi-app
        invocation.
      * `_shared_family` -- 1, once per module for everyone else.

    Nine in total, against a budget of ten. **Adding a submit here is a budget
    decision**: count the module's total before you add one, and prefer
    `family_pair` (free) or `_admit_adult_directly` (free).

    ⚠ The count above is PER APP LEG, not per invocation: every caller here
    except `_shared_family` (module-scoped, built once) rides `admin_app`/
    `app`, which is itself parametrized once per app in `--app`
    (`conftest.py::app`), so each pays its submit again for every app leg. A
    single-app run stays at nine; a multi-app run is unaudited against the
    60 s window (`testing.md:160` -- "audited in ONE module", one app).
    """
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    from common.sig_domain import invite_submit_signed_message

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


def _admit_adult_directly(nest_instance, handle: str) -> dict:
    """Mint an ordinary (unsupervised, non-guardian) account at a cost of ZERO
    anonymous submits.

    `fauna.admin.users.create` is the **third** account-creation path
    (`public-mode.md` § Registration & Identity — the ceremony, the admin claim,
    or an admin admitting a user directly), and it is *admin*-authenticated, so
    it never touches the anonymous `invite_request.submit` budget
    `_submit_invite_request` documents. Every account this module needs that is
    not a ward is an ordinary adult — the guardians, and the transfer
    handshake's proposed guardians — so all of them come from here.

    ⚠ It cannot mint a WARD. `fauna.admin.users.create` passes
    `guardian: None` to `create_user_with_handle`, and family-safety.md
    § The guardianship link sets supervision **at admission** through an invite
    code or a request approval only ("v1 does not convert an existing full
    account into a supervised one"). So a supervised account still has to walk
    `_admit_ward`'s throttled path — see that helper.

    `label=handle` below matches what the nest itself writes, not a picker
    workaround anymore. The admin guardian pickers are now HANDLE (else
    full-actor-hex) pickers everywhere (admin.md § 2 → *What identifies a user
    in an admin picker*, ratified 2026-08-30, all 7 arms landed by
    2026-09-09 — `apps/fauna-tui/src/admin/users.rs::guardian_picker` →
    `super::picker_option`, and its linux/android/apple/web/windows twins), NOT
    label pickers, so `register_user`'s label no longer decides picker
    distinctness (the 2026-08-30 "picker painted `e2e-test` five times over"
    label collision can't reproduce today — each caller here already passes
    its own distinct `handle`). `label=handle` stays because
    `create_user_with_handle` (`bins/fauna-nest/src/db/admin.rs:1272`) always
    defaults a handle-registered user's `label` to its `handle` — this just
    restores that production shape.

    Fixture setup, not the action under test (e2e point 8): the direct-admission
    UI path is itself the mutation under test in `test_admin_users_admit.py`,
    which is this shortcut's point-8 citation.
    """
    from common.auth import register_user

    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    register_user(
        nest_instance["port"],
        actor_hex,
        base_url=nest_instance["url"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
        handle=handle,
        label=handle,
    )
    return {"signing_key": sk, "actor_id_hex": actor_hex}


def _admit_ward(
    nest_instance, guardian_identity: dict, ward_handle: str, *, age_band: str | None = None
) -> dict:
    """Admit a SUPERVISED account linked to `guardian_identity` — the one
    admission in this module that costs an anonymous submit (see
    `_submit_invite_request`'s budget note).

    Supervision is only ever set at admission, and only by an invite code or a
    request approval (`family-safety.md` § The guardianship link). The code path
    cannot redeem on this nest — `nest_instance` starts registration-`closed`
    and `register_core` refuses a closed posture before it looks at any code
    (module docstring (a)) — which leaves the request path and its one throttled
    `invite_request.submit`.

    The APPROVAL rides the admin RPC rather than the admin UI on purpose: for
    every test whose subject is the family page this is pure fixture setup, and
    routing it through `admin_app` would also pin these admissions to a
    function-scoped fixture, which is exactly what `_shared_family` must not be.
    The admin's real UI approval — pending-request row, guardian picker, tier,
    Approve — is the mutation under test in
    `test_family_admission_and_ward_view`, which is the point-8 citation here.

    `age_band` (a wire token) admits the ward under that band beside the
    guardian — the age-band readouts' fixture (`family-safety.md` § The
    account age band); the admin UI's own band picker is
    `test_pending_invite_journey.py`'s subject.
    """
    ward_identity = _submit_invite_request(nest_instance["url"], handle=ward_handle)
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    with _admin_client(nest_instance) as admin:
        rows = admin.call("fauna.admin.invite_requests.list", {})["invite_requests"]
        pending = [r for r in rows if r["handle"] == ward_handle]
        assert pending, (
            f"the seeded invite request for {ward_handle!r} is not listed as pending; "
            f"listed handles: {[r['handle'] for r in rows]!r}"
        )
        params = {
            "id": pending[0]["id"],
            "tier": "personal",
            "guardian_actor": guardian_actor,
        }
        if age_band is not None:
            params["age_band"] = age_band
        admin.call("fauna.admin.invite_requests.approve", params)
    return ward_identity


#: The guardian-policy document with every knob at its default. `family-safety.md`
#: § Wire & data shape: "Every default is the unsupervised-equivalent value" — a
#: fresh link with an untouched policy changes nothing until the guardian
#: tightens it.
#:
#: Written out in full rather than left partial because `policy.update` has two
#: different absence rules (§ Policy-update compatibility): the four v1 reach
#: knobs REPLACE, while the v1.x pillars (`content_policy`, `screen_time`,
#: `content_notify`, `unknown_peer_dm`, `features`) are absent-means-UNCHANGED.
#: A reset that omitted a pillar would silently carry the previous test's value
#: forward — the order-dependence this whole fixture exists to prevent.
_DEFAULT_WARD_POLICY = {
    "contact_approval": False,
    "unknown_sender_mail": "allow",
    "federation_contact": True,
    "feed_sources": "allow",
    "content_policy": {
        "nsfw": "inherit",
        "spam": "inherit",
        "phishing": "inherit",
        "commercial": "inherit",
    },
    # `{}`, not three explicit nulls: the sub-document is PRESENT (so the write
    # replaces the columns) and every field takes its `serde(default)` — which
    # for `ScreenTimePolicy` is all-`None`, "no screen-time limit"
    # (`fauna_core::screen_time`). Present-and-empty is how a reset clears a
    # pillar; absent would leave it as the last test set it.
    "screen_time": {},
    "content_notify": False,
    "unknown_peer_dm": "allow",
    "features": {},
}


def _reset_ward_policy(nest_instance, guardian_identity: dict, ward_identity: dict) -> None:
    """Put the shared ward's policy document back to its defaults, and clear any
    pending transfer, as the GUARDIAN.

    Both calls are link-authorized guardian mutations, so this is the same
    authority the tests themselves drive through the UI — fixture setup for a
    test whose subject is something else (point 8).

    Runs at SETUP, never teardown: a test that fails half-way through a policy
    edit must not leave the next one to inherit it, and a teardown reset does
    not run when the test process dies. Setup-reset is also what keeps the
    module order-INDEPENDENT — every sharing test starts from the same document
    no matter what ran before it.
    """
    ward_actor = bytes.fromhex(ward_identity["actor_id_hex"])
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    guardian_seed = bytes(guardian_identity["signing_key"])
    with WsRpcAdminClient(nest_instance["url"], guardian_actor, guardian_seed) as guardian:
        guardian.call(
            "fauna.family.policy.update",
            {"supervised_actor_id": ward_actor, "policy": dict(_DEFAULT_WARD_POLICY)},
        )
        # Denied bridge-DM peers are not part of the policy document, so the
        # update above leaves them standing; a run that died between its denies
        # and its un-deny would hand the next one extra rows. Allowing is the
        # un-deny itself — the same guardian authority, idempotent.
        for peer in _blocked_dm_peers(guardian, ward_actor):
            _decide_dm_peer(guardian, ward_actor, peer["bridge_id"], peer["peer_id"], True)
        # A ward's pending contact asks are not in the policy document either.
        # `test_family_ward_asks_guardian_for_a_contact` leaves one on the shared
        # ward, so under `--app linux,web` the second leg read it as the DURABLE
        # pending state and was never offered the ask it asserts. A
        # `contact_request` deny drops the ask and never blocks the peer
        # (family-safety.md § Child-initiated contact requests).
        for entry in guardian.call("fauna.family.approvals.list", {}).get("approvals") or []:
            if entry["kind"] == "contact_request" and bytes(entry["supervised_actor_id"]) == ward_actor:
                guardian.call(
                    "fauna.family.approvals.decide",
                    {
                        "supervised_actor_id": ward_actor,
                        "kind": "contact_request",
                        "peer_actor_id": bytes(entry["peer_actor_id"]),
                        "approve": False,
                    },
                )
        # A proposal left pending by a previous test would render on the
        # guardian's editor (`transfer-pending`) and be accepted/declined by the
        # wrong test. Cancel is idempotent-shaped for our purpose: no pending
        # proposal is a typed refusal, not a failure of the reset.
        try:
            guardian.call(
                "fauna.family.transfer.cancel", {"supervised_actor_id": ward_actor}
            )
        except Exception:
            pass


def _blocked_dm_peers(guardian: WsRpcAdminClient, ward_actor: bytes) -> list[dict]:
    """The ward's `block`-verdict bridge-DM peers, as the guardian's own
    `fauna.family.status` carries them (`FamilyWardInfo::blocked_dm_peers`) —
    the nest's truth the un-deny journey asserts against."""
    status = guardian.call("fauna.family.status", {})
    for ward in status.get("wards") or []:
        if bytes(ward["actor_id"]) == ward_actor:
            return list(ward.get("blocked_dm_peers") or [])
    raise AssertionError("the ward is missing from its guardian's status read")


def _decide_dm_peer(
    guardian: WsRpcAdminClient, ward_actor: bytes, bridge_id: str, peer_id: str, approve: bool
) -> None:
    """`approvals_decide { kind: "dm_hold" }` — the guardian's verdict on one
    bridge-DM peer. Not queue-scoped (family-safety.md § The bridge-DM gate →
    *The un-deny surface*), so a deny needs no held conversation first."""
    guardian.call(
        "fauna.family.approvals.decide",
        {
            "supervised_actor_id": ward_actor,
            "kind": "dm_hold",
            "peer_actor_id": b"",
            "bridge_id": bridge_id,
            "peer_address": peer_id,
            "approve": approve,
        },
    )


@pytest.fixture(scope="module")
def _shared_family(nest_instance):
    """ONE guardian + ONE ward, admitted once for the whole module.

    This is the module's answer to the shipped 10/60 s invite-request budget
    (`_submit_invite_request`): ten of the fifteen tests here do not care *how*
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
    guardian_identity = _admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = _admit_ward(nest_instance, guardian_identity, ward_handle)
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
    _reset_ward_policy(
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
    submits, a fifth of the shipped 10/60 s budget** (`_submit_invite_request`).
    It has exactly ONE caller — `test_family_admission_and_ward_view`, whose
    subject IS this flow (the pending-request guardian picker, the tier, the
    Approve, and the guardian-scoped code mint are the mutations under test).
    Every other test takes `family_pair`, or `_admit_own_pair` when it needs a
    ward of its own; neither costs an admin-UI navigation either.
    """
    guardian_identity = _submit_invite_request(nest_url, handle=guardian_handle)
    _approve_pending(app, guardian_handle, guardian=None)

    ward_identity = _submit_invite_request(nest_url, handle=ward_handle)
    _approve_pending(app, ward_handle, guardian=guardian_handle)

    return guardian_identity, ward_identity, _mint_guardian_code(app, guardian_handle)


def _mint_guardian_code(app, guardian_handle: str) -> str:
    """Mint a guardian-scoped invite code through the admin UI
    (`admin-users-invite-guardian-select`) — the out-of-band code
    `_check_notice_only` peeks at. A real UI mutation, and free of the anonymous
    budget: minting is an admin act."""
    app.admin.navigate_invite_codes()
    code = app.admin.create_invite_code(tier="free", uses=1, guardian=guardian_handle)
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
    guardian_identity = _admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = _admit_ward(nest_instance, guardian_identity, ward_handle)
    return guardian_identity, ward_identity, guardian_handle, ward_handle


def _check_notice_only(app, nest_url: str, code: str, guardian_handle: str) -> None:
    """Fresh disposable joiner checks the OOB code and confirms
    invite-code-supervised-notice renders BEFORE redemption. Peek-only
    (fauna.account.invite_code.verify does not decrement `uses`) — does not
    redeem (see module docstring: registration.open gates redemption on this
    nest regardless of a valid invite code)."""
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


@pytest.mark.feature("family-safety")
def test_family_admission_and_ward_view(admin_app, request, nest_instance):
    app = admin_app

    nest_url = nest_instance["url"]
    # The one caller of the full UI admission, and the one test that spends two
    # of the module's six anonymous invite-request submits: the admin's pending-
    # request approval WITH a guardian selected is this test's subject, so it
    # cannot be shortcut to `family_pair` (see `_submit_invite_request`).
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

    Direct-admitted (`_admit_adult_directly`), so it costs no anonymous submit:
    what these tests need from this actor is that it EXISTS and is admissible as
    a guardian, and the admission path it arrived by is not part of any
    assertion here."""
    return _admit_adult_directly(nest_instance, handle)


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
    guardian_identity = _admit_adult_directly(nest_instance, guardian_handle)
    ward_identity = _admit_ward(nest_instance, guardian_identity, ward_handle, age_band="13-15")

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

    # Shares the module's one admitted pair: `_reset_ward_policy` un-denies any
    # peer a failed earlier run left behind, so exactly our two rows render.
    guardian_identity, ward_identity, _guardian_handle, ward_handle = family_pair
    ward_actor = bytes.fromhex(ward_identity["actor_id_hex"])
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    guardian_seed = bytes(guardian_identity["signing_key"])
    # Nostr pubkey-shaped ids, fresh per run so a row can only be ours.
    peers = {os.urandom(32).hex(), os.urandom(32).hex()}
    with WsRpcAdminClient(nest_instance["url"], guardian_actor, guardian_seed) as guardian:
        for peer in peers:
            _decide_dm_peer(guardian, ward_actor, "nostr", peer, False)
        denied = {p["peer_id"] for p in _blocked_dm_peers(guardian, ward_actor)}
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
                {p["peer_id"] for p in _blocked_dm_peers(guardian, ward_actor)} & peers
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

    policy = dict(_DEFAULT_WARD_POLICY)
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
    assert app.driver.is_visible("content-policy-blocked-notice", scope="post-card[1]"), (
        "spam-labeled post did not render the content-policy block notice"
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
    policy = dict(_DEFAULT_WARD_POLICY)
    policy["content_policy"] = {**_DEFAULT_WARD_POLICY["content_policy"], category: floor}
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
    # the notice, and only the guardian can set one.
    assert app.driver.is_absent("content-policy-blocked-notice", scope="post-card[0]"), (
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
        if app.feed.seed_posts([spam_post]) == 1 and app.driver.is_visible(
            "content-policy-blocked-notice", scope="post-card[0]"
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
    assert app.driver.is_visible("content-policy-blocked-notice", scope="post-card[1]"), (
        "spam-labeled post did not render the block notice — there is nothing to count"
    )
    # Let the ward's client batch + flush the coarse report to the nest BEFORE we
    # flip identity (reset wipes the ward's client and its flush loop). The first
    # report is eager (no prior flush).
    # Force the check now rather than waiting out the real flush-tick interval —
    # convention 14's run_now poke, on every app (contract:
    # `fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW`; web `$lib/family-notify-e2e`,
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
    deadline = time.monotonic() + 10.0
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
    deadline = time.monotonic() + 10.0
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
    it — with the Family page still reachable read-only, and the lock gone once
    the guardian clears the window.

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
        "the lock must name the guardian who set the policy (family-safety.md "
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
    `main.rs`, web `$lib/screen-time-e2e.ts`), which runs the app's own
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
    LOCK_BUDGET_S = 30.0

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
        "the lock must name the guardian who set the policy (family-safety.md "
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
