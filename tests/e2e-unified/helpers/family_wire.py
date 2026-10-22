"""Family-safety (guardian/ward) wire setup shared by every suite that needs a
supervised account: the app journeys in ``tests/test_family.py`` and its
siblings, and the app-less nest witnesses under ``tests/api/``.

Fixture setup and read-back only — every mutation an app journey has under
test still goes through the app UI (e2e-conventions.md convention 8); these are
the doors that put the accounts and the policy document in place around it.

An *identity* here is ``{"signing_key": SigningKey, "actor_id_hex": str}`` —
the shape ``submit_invite_request`` and ``admit_adult_directly`` return.
"""

from __future__ import annotations

import time

from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from helpers.admin_wire import admin_client


def actor_ws(nest, identity: dict) -> WsRpcAdminClient:
    """A WS-RPC connection signing as ``identity`` (guardian, ward or adult)."""
    return WsRpcAdminClient(
        nest["url"],
        bytes.fromhex(identity["actor_id_hex"]),
        bytes(identity["signing_key"]),
    )


def submit_invite_request(nest_url: str, handle: str, message: str = "let me in") -> dict:
    """Seed a real Ed25519-signed pending invite-request -- fixture setup
    (not the action under test), mirrors test_admin_users_hub.py's helper.
    Returns the requester's identity so the caller can log back in as them
    once admitted.

    "This module" below is ``tests/test_family.py``, where these helpers lived
    until they were lifted here; its budget audit is the worked example. Any
    module calling ``admit_ward`` spends from the same per-source budget, so
    count its submits the same way.

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
    COUNT is. So: every non-ward account comes from `admit_adult_directly`
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
    `family_pair` (free) or `admit_adult_directly` (free).

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


def admit_adult_directly(nest_instance, handle: str) -> dict:
    """Mint an ordinary (unsupervised, non-guardian) account at a cost of ZERO
    anonymous submits.

    `fauna.admin.users.create` is the **third** account-creation path
    (`public-mode.md` § Registration & Identity — the ceremony, the admin claim,
    or an admin admitting a user directly), and it is *admin*-authenticated, so
    it never touches the anonymous `invite_request.submit` budget
    `submit_invite_request` documents. Every account this module needs that is
    not a ward is an ordinary adult — the guardians, and the transfer
    handshake's proposed guardians — so all of them come from here.

    ⚠ It cannot mint a WARD. `fauna.admin.users.create` passes
    `guardian: None` to `create_user_with_handle`, and family-safety.md
    § The guardianship link sets supervision **at admission** through an invite
    code or a request approval only ("v1 does not convert an existing full
    account into a supervised one"). So a supervised account still has to walk
    `admit_ward`'s throttled path — see that helper.

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


def admit_ward(
    nest_instance, guardian_identity: dict, ward_handle: str, *, age_band: str | None = None
) -> dict:
    """Admit a SUPERVISED account linked to `guardian_identity` — the one
    admission in this module that costs an anonymous submit (see
    `submit_invite_request`'s budget note).

    Supervision is only ever set at admission — by an invite code, a request
    approval, or a direct admission with a guardian (`fauna.admin.users.create`,
    since 2026-08-30; `family-safety.md` § The guardianship link). The code path
    cannot redeem on this nest — `nest_instance` starts registration-`closed`
    and `register_core` refuses a closed posture before it looks at any code
    (module docstring (a)) — and this helper takes the request path, with its
    one throttled `invite_request.submit`.

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
    ward_identity = submit_invite_request(nest_instance["url"], handle=ward_handle)
    guardian_actor = bytes.fromhex(guardian_identity["actor_id_hex"])
    with admin_client(nest_instance) as admin:
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
DEFAULT_WARD_POLICY = {
    "contact_approval": False,
    "unknown_sender_mail": "allow",
    "federation_contact": True,
    "feed_sources": "allow",
    "content_policy": {
        "nsfw": "inherit",
        "spam": "inherit",
        "phishing": "inherit",
        "commercial": "inherit",
        # The guardian's rule document (nest-rules.md § The shared rule editor
        # (b)): absent would leave the last test's rules standing; an EMPTY
        # document is how a write clears it. `version` is the grammar version
        # (`fauna_core::content_policy::GRAMMAR_VERSION`).
        "document": {"version": 2, "rules": [], "scorers": []},
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


def reset_ward_policy(nest_instance, guardian_identity: dict, ward_identity: dict) -> None:
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
            {"supervised_actor_id": ward_actor, "policy": dict(DEFAULT_WARD_POLICY)},
        )
        # Denied bridge-DM peers are not part of the policy document, so the
        # update above leaves them standing; a run that died between its denies
        # and its un-deny would hand the next one extra rows. Allowing is the
        # un-deny itself — the same guardian authority, idempotent.
        for peer in blocked_dm_peers(guardian, ward_actor):
            decide_dm_peer(guardian, ward_actor, peer["bridge_id"], peer["peer_id"], True)
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


def blocked_dm_peers(guardian: WsRpcAdminClient, ward_actor: bytes) -> list[dict]:
    """The ward's `block`-verdict bridge-DM peers, as the guardian's own
    `fauna.family.status` carries them (`FamilyWardInfo::blocked_dm_peers`) —
    the nest's truth the un-deny journey asserts against."""
    status = guardian.call("fauna.family.status", {})
    for ward in status.get("wards") or []:
        if bytes(ward["actor_id"]) == ward_actor:
            return list(ward.get("blocked_dm_peers") or [])
    raise AssertionError("the ward is missing from its guardian's status read")


def decide_dm_peer(
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


def set_ward_policy(nest, guardian_identity: dict, ward_identity: dict, **overrides) -> None:
    """Write the ward's whole policy document as the GUARDIAN: the defaults with
    ``overrides`` laid over them. Written out in full for the reason
    ``DEFAULT_WARD_POLICY`` gives — a partial write would carry earlier values
    of the absent-means-unchanged pillars forward."""
    policy = {**DEFAULT_WARD_POLICY, **overrides}
    with actor_ws(nest, guardian_identity) as guardian:
        guardian.call(
            "fauna.family.policy.update",
            {
                "supervised_actor_id": bytes.fromhex(ward_identity["actor_id_hex"]),
                "policy": policy,
            },
        )


def family_status(nest, identity: dict) -> dict:
    """``fauna.family.status`` as ``identity`` sees it — a guardian's ``wards``,
    a ward's own ``guardian``/``policy``/``age_band``."""
    with actor_ws(nest, identity) as ws:
        return ws.call("fauna.family.status", {})


def ward_row(status: dict, ward_identity: dict) -> dict | None:
    """The guardian-side ``wards`` row for ``ward_identity`` in a
    ``family_status`` reply, or ``None`` when the guardian no longer lists it."""
    want = bytes.fromhex(ward_identity["actor_id_hex"])
    for row in status.get("wards") or []:
        if bytes(row["actor_id"]) == want:
            return row
    return None
