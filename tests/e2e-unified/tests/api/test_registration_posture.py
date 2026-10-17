"""Who gets in, and how — the registration posture witnessed at the wire
(`docs/goal/behavior/admin.md` § 2. Users, Sections 1–2;
`docs/goal/architecture/nest/public-mode.md` § Registration Modes).

The nest outcomes of `docs/features/admin-users.md` and `join-a-nest.md` that
the posture decides, each driven over the kinds a production caller uses — the
admin's choices through Admin-class WS-RPC (`testing.md` point 8's
fixture-setup carve-out: the admin authoring journeys are the app suites'),
the person through the pre-identity kinds the onboarding wizard rides:

* **admin-users 20** — a new nest lets nobody register themselves until its
  admin opens registration (`closed` is the default on a fresh nest).
* **admin-users 19** — closing registration never locks out a member who
  already has an account.
* **admin-users 21** — while registration is closed the admin still admits a
  person by approving their request.
* **admin-users 22** — an invite code admits under open and invite-only, is
  refused under closed, and a code minted while closed works once registration
  opens.
* **admin-users 23** — a member redeems an invite code to move to its tier,
  whatever the mode.
* **admin-users 24** — once the cap on free accounts is reached the next free
  sign-up is refused whatever the mode: a no-code sign-up under open, a
  free-tier code under invite-only (left unconsumed); a non-free code or a
  raised cap is what admits the next person.
* **admin-users 25** — with app-verified age required for sign-ups, the admin
  can still approve a request that carries no age check.
* **admin-users 17** — a refusal is cleared ninety days on and the person can
  ask again, with nobody doing anything. The 90-day sweep is the production
  one, reached through the `test-hooks` endpoint
  `POST /api/v1/test/invite_requests/prune_denied`, which ages the one
  person's refusal and runs the sweep now — never a wait (convention 14).
* **join-a-nest 11** — a sign-up whose app reports the person as a minor is
  refused and pointed to joining through a guardian.

Every refusal is asserted as a two-way discriminator: the same call with the
one fact the rule keys on changed goes through, so "refused" cannot be told
from "this call never works". Each test sets the registration mode it depends
on, on one module-shared nest, so order is never load-bearing — except outcome
20, which needs a nest nobody has touched and starts its own.
"""

from __future__ import annotations

import json
import secrets
import time
import urllib.request

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import (
    ensure_tier,
    mint_token_via_handshake,
    set_registration_mode,
)
from common.sig_domain import invite_submit_signed_message, register_signed_message

pytestmark = pytest.mark.tier_3

NEST_DOMAIN = "posture.example"
CODE_TIER = "member"
DAY = 24 * 3600

REGISTRATION_CLOSED = "fauna.account.registration_closed"
INVITE_REQUIRED = "fauna.account.invite_required"
AGE_VERIFICATION_REQUIRED = "fauna.account.age_verification_required"
GUARDIAN_ADMISSION_REQUIRED = "fauna.account.guardian_admission_required"
REQUEST_EXISTS = "fauna.account.invite_request_exists"
REQUEST_NOT_FOUND = "fauna.account.invite_request_not_found"
FREE_LIMIT_REACHED = "fauna.account.free_limit_reached"


def _as_signing_key(raw) -> SigningKey:
    """Nests return the admin signing key as either a `SigningKey` or hex."""
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


@pytest.fixture(scope="module")
def posture_nest(request, nest_mode, tmp_path_factory):
    """A nest claimed onto ``NEST_DOMAIN`` (the domain every registration
    signature is over) with one non-free tier for the invite codes to name."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "posture-nest", claim_domain=NEST_DOMAIN,
    )
    nest["admin_sk"] = _as_signing_key(nest["admin"]["signing_key"])
    ensure_tier(nest["port"], base_url=nest["url"], admin_signing_key=nest["admin_sk"],
                name=CODE_TIER)
    yield nest
    cleanup()


# ── wire helpers ─────────────────────────────────────────────────────────────


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin_sk"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _mode(nest, mode: str, *, max_free_users: int | None = None) -> None:
    """One admin write carries mode + the free-tier ceiling; a call that names
    no ceiling clears it, so every test starts uncapped."""
    set_registration_mode(nest["port"], base_url=nest["url"],
                          admin_signing_key=nest["admin_sk"], mode=mode,
                          max_free_users=max_free_users)


def _free_account_count(nest) -> int:
    """How many accounts sit at the free tier right now — the number the
    ceiling compares against (the admin's own row included)."""
    with _admin(nest) as admin:
        return sum(1 for u in admin.users_list_all() if u.get("tier") == "free")


def _handle(prefix: str) -> str:
    return f"{prefix}{secrets.token_hex(3)}"


def _register(nest, handle: str, *, sk: SigningKey | None = None,
              invite_code: str | None = None, age_band: str | None = None) -> SigningKey:
    """`fauna.account.register` for a fresh key (or ``sk``); raises
    `RpcCallError` on a refusal. Returns the key that registered."""
    sk = sk or SigningKey.generate()
    actor = bytes(sk.verify_key)
    ts = int(time.time() * 1000)
    body = {
        "actor_id": actor.hex(),
        "handle": handle,
        "timestamp": ts,
        "signature": sk.sign(register_signed_message(actor, handle, NEST_DOMAIN, ts)).signature.hex(),
    }
    if invite_code is not None:
        body["invite_code"] = invite_code
    if age_band is not None:
        body["age_claim"] = {"band": age_band}
    with WsRpcAnonClient(nest["url"]) as anon:
        anon.call("fauna.account.register", body)
    return sk


def _refusal(fn, *args, **kwargs) -> str:
    """The error code ``fn`` is refused with; fails if it goes through."""
    with pytest.raises(RpcCallError) as ei:
        fn(*args, **kwargs)
    return ei.value.code


def _account(nest, sk: SigningKey) -> dict:
    """Sign in as ``sk`` (a fresh handshake mint) and read its own account."""
    assert mint_token_via_handshake(nest["url"], sk), "the sign-in minted no token"
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as ws:
        return ws.call("fauna.account.get", {})


def _submit_request(nest, sk: SigningKey, handle: str) -> int:
    """`fauna.account.invite_request.submit` with no age claim; returns its id."""
    actor = bytes(sk.verify_key)
    ts = int(time.time() * 1000)
    sig = sk.sign(invite_submit_signed_message(actor, handle, "", ts)).signature.hex()
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.account.invite_request.submit", {
            "actor_id": actor.hex(), "handle": handle, "message": "",
            "timestamp": ts, "signature": sig,
        })["id"]


def _request_status(nest, sk: SigningKey) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.account.invite_request.status",
                         {"actor_id": bytes(sk.verify_key).hex()})


def _mint_code(nest, tier: str = CODE_TIER) -> str:
    with _admin(nest) as admin:
        return admin.call("fauna.admin.invite_codes.create", {"tier": tier, "uses": 1})["code"]


def _prune_denied(nest, sk: SigningKey, secs: int) -> dict:
    """Age ``sk``'s refusal by ``secs`` and run the production 90-day sweep."""
    body = json.dumps({"actor_id": bytes(sk.verify_key).hex(), "secs": secs}).encode()
    req = urllib.request.Request(f"{nest['url']}/api/v1/test/invite_requests/prune_denied",
                                 data=body, headers={"Content-Type": "application/json"},
                                 method="POST")
    with urllib.request.urlopen(req, timeout=15.0) as resp:
        assert resp.status == 200, f"prune hook returned {resp.status}"
        return json.loads(resp.read())


# ── 20: a new nest is closed ─────────────────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_a_new_nest_lets_nobody_register_until_its_admin_opens_it(
        request, nest_mode, tmp_path_factory):
    """Outcome 20: on a nest fresh from its claim, nobody registers themselves;
    the admin opening registration is what lets the same person in."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "fresh-posture-nest", claim_domain=NEST_DOMAIN,
    )
    try:
        nest["admin_sk"] = _as_signing_key(nest["admin"]["signing_key"])
        sk = SigningKey.generate()
        handle = _handle("first")
        assert _refusal(_register, nest, handle, sk=sk) == REGISTRATION_CLOSED, (
            "a fresh nest must refuse self-registration until its admin opens it"
        )
        _mode(nest, "open")
        _register(nest, handle, sk=sk)
        assert _account(nest, sk)["handle"] == handle
    finally:
        cleanup()


# ── 19: closing never locks a member out ─────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_closing_registration_never_locks_out_an_existing_member(posture_nest):
    """Outcome 19: the mode gates registration, never authentication."""
    nest = posture_nest
    _mode(nest, "open")
    handle = _handle("member")
    member = _register(nest, handle)

    _mode(nest, "closed")
    assert _refusal(_register, nest, _handle("stranger")) == REGISTRATION_CLOSED, (
        "closed must refuse a stranger, or this test proves nothing about closing"
    )
    account = _account(nest, member)
    assert account["handle"] == handle, (
        f"a member who registered while open must still sign in and read their "
        f"account once registration closes: {account!r}"
    )


# ── 21: the admin still admits while closed ──────────────────────────────────


@pytest.mark.feature("admin-users")
def test_while_closed_the_admin_still_admits_by_approving_a_request(posture_nest):
    """Outcome 21: approval never rides the registration endpoint."""
    nest = posture_nest
    _mode(nest, "closed")
    sk = SigningKey.generate()
    handle = _handle("asked")
    assert _refusal(_register, nest, handle, sk=sk) == REGISTRATION_CLOSED
    req_id = _submit_request(nest, sk, handle)
    with _admin(nest) as admin:
        admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
    assert _account(nest, sk)["handle"] == handle, (
        "the approved person must sign in on a closed nest"
    )


# ── 22: an invite code rides the mode ────────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_an_invite_code_admits_under_open_and_invite_only_and_waits_out_closed(posture_nest):
    """Outcome 22: a code admits under open and invite-only, is refused under
    closed, and a code minted while closed works once registration opens."""
    nest = posture_nest

    _mode(nest, "closed")
    minted_while_closed = _mint_code(nest)
    sk = SigningKey.generate()
    handle = _handle("coded")
    assert _refusal(_register, nest, handle, sk=sk,
                    invite_code=minted_while_closed) == REGISTRATION_CLOSED, (
        "closed means no self-service admission at all, a code included"
    )

    _mode(nest, "invite_required")
    assert _refusal(_register, nest, _handle("nocode")) == INVITE_REQUIRED, (
        "invite-only must refuse a sign-up with no code"
    )
    _register(nest, handle, sk=sk, invite_code=minted_while_closed)
    assert _account(nest, sk)["tier"] == CODE_TIER, (
        "the code minted while closed must admit at its tier once registration opens"
    )

    _mode(nest, "open")
    open_sk = _register(nest, _handle("opencode"), invite_code=_mint_code(nest))
    assert _account(nest, open_sk)["tier"] == CODE_TIER, "a code must admit under open too"


# ── 23: a member upgrades with a code in any mode ────────────────────────────


@pytest.mark.feature("admin-users")
@pytest.mark.parametrize("mode", ["open", "invite_required", "closed"])
def test_a_member_redeems_a_code_to_move_tier_whatever_the_mode(posture_nest, mode):
    """Outcome 23: `fauna.account.upgrade` is not registration, so the mode
    never gates it."""
    nest = posture_nest
    _mode(nest, "open")
    member = _register(nest, _handle("upgrader"))
    assert _account(nest, member)["tier"] != CODE_TIER

    _mode(nest, mode)
    code = _mint_code(nest)
    with WsRpcAdminClient(nest["url"], actor_id=bytes(member.verify_key),
                          signing_key=bytes(member)) as ws:
        ws.call("fauna.account.upgrade", {"tier": CODE_TIER, "invite_code": code})
        tier = ws.call("fauna.account.get", {})["tier"]
    assert tier == CODE_TIER, f"the upgrade must take under {mode!r}: {tier!r}"


# ── 25: the age knob binds self-service only ─────────────────────────────────


@pytest.mark.feature("admin-users")
def test_with_age_verification_required_the_admin_still_approves_an_unchecked_request(
        posture_nest):
    """Outcome 25: the knob refuses self-service sign-ups that carry no
    verified age claim, and leaves request approval to the admin's judgment."""
    nest = posture_nest
    _mode(nest, "open")
    with _admin(nest) as admin:
        admin.call("fauna.admin.set_age_verification_required", {"required": True})
    try:
        sk = SigningKey.generate()
        handle = _handle("unchecked")
        assert _refusal(_register, nest, handle, sk=sk) == AGE_VERIFICATION_REQUIRED, (
            "the knob must refuse a self-service sign-up with no age check"
        )
        req_id = _submit_request(nest, sk, handle)
        with _admin(nest) as admin:
            admin.call("fauna.admin.invite_requests.approve", {"id": req_id})
        assert _account(nest, sk)["handle"] == handle, (
            "the admin's approval must admit a request that carries no age check"
        )
    finally:
        with _admin(nest) as admin:
            admin.call("fauna.admin.set_age_verification_required", {"required": False})


# ── 17: a refusal is not for ever ────────────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_a_refusal_is_cleared_ninety_days_on_and_the_person_can_ask_again(posture_nest):
    """Outcome 17: day 89 the refusal still stands and blocks a new ask; past
    day 90 the sweep has cleared it and the same person asks again."""
    nest = posture_nest
    sk = SigningKey.generate()
    handle = _handle("refused")
    req_id = _submit_request(nest, sk, handle)
    with _admin(nest) as admin:
        admin.call("fauna.admin.invite_requests.deny", {"id": req_id, "reason": "not now"})

    swept = _prune_denied(nest, sk, 89 * DAY)
    assert swept["aged"] is True, swept
    assert _request_status(nest, sk)["status"] == "denied", (
        "at 89 days the refusal must still stand"
    )
    assert _refusal(_submit_request, nest, sk, handle) == REQUEST_EXISTS, (
        "while the refusal stands the person cannot ask again"
    )

    swept = _prune_denied(nest, sk, 2 * DAY)
    assert swept["pruned"] >= 1, swept
    assert _refusal(_request_status, nest, sk) == REQUEST_NOT_FOUND, (
        "past ninety days the refusal must be gone"
    )
    again = _submit_request(nest, sk, handle)
    assert _request_status(nest, sk)["status"] == "pending", (
        f"the person must be able to ask again, with nobody doing anything: request {again}"
    )


# ── join-a-nest 11: a minor is pointed to a guardian ─────────────────────────


@pytest.mark.feature("join-a-nest")
def test_a_sign_up_reported_as_a_minor_is_pointed_to_a_guardian(posture_nest):
    """join-a-nest outcome 11: an app's age report saying minor refuses the
    unsupervised sign-up toward guardian-mediated admission; the same sign-up
    reported adult goes through."""
    nest = posture_nest
    _mode(nest, "open")
    sk = SigningKey.generate()
    handle = _handle("young")
    assert _refusal(_register, nest, handle, sk=sk, age_band="13-15") == GUARDIAN_ADMISSION_REQUIRED
    _register(nest, handle, sk=sk, age_band="18+")
    assert _account(nest, sk)["handle"] == handle


# ── 24: the free ceiling binds whatever the mode ─────────────────────────────


@pytest.mark.feature("admin-users")
def test_once_the_free_cap_is_reached_the_next_free_sign_up_is_refused_whatever_the_mode(
        posture_nest):
    """Outcome 24: the ceiling is checked at the self-service door for every
    admission that would land at the free tier — a no-code sign-up under open,
    a free-tier invite code under invite-only — and a non-free code, or a
    raised cap, is what gets the next person in (`public-mode.md`
    § Registration Modes → *What "regardless of mode" binds*). The refused code
    is not consumed: the same code admits once the cap is raised."""
    nest = posture_nest
    try:
        # Open: the cap is set to exactly the free accounts the nest holds.
        free_now = _free_account_count(nest)
        _mode(nest, "open", max_free_users=free_now)
        assert _refusal(_register, nest, _handle("overcap")) == FREE_LIMIT_REACHED, (
            "at the ceiling, the next free sign-up under open must be refused"
        )
        # A code minted at a non-free tier creates no free account: the door is
        # open, only the free tier is full.
        paid = _register(nest, _handle("paidcode"), invite_code=_mint_code(nest))
        assert _account(nest, paid)["tier"] == CODE_TIER, "a non-free code is outside the cap"
        _mode(nest, "open", max_free_users=free_now + 1)
        roomed = _register(nest, _handle("roomed"))
        assert _account(nest, roomed)["tier"] == "free", (
            "raising the ceiling admits the next free account"
        )

        # Invite-only: a code is the only way in, and a free-tier code lands at
        # free — the cap bites on it too, without burning the code.
        free_now = _free_account_count(nest)
        _mode(nest, "invite_required", max_free_users=free_now)
        free_code = _mint_code(nest, tier="free")
        sk = SigningKey.generate()
        handle = _handle("freecode")
        assert _refusal(_register, nest, handle, sk=sk,
                        invite_code=free_code) == FREE_LIMIT_REACHED, (
            "under invite-only a free-tier code must not admit past the ceiling"
        )
        paid_too = _register(nest, _handle("paidcode"), invite_code=_mint_code(nest))
        assert _account(nest, paid_too)["tier"] == CODE_TIER, (
            "under invite-only a non-free code stays outside the cap"
        )
        _mode(nest, "invite_required", max_free_users=free_now + 1)
        _register(nest, handle, sk=sk, invite_code=free_code)
        assert _account(nest, sk)["tier"] == "free", (
            "the refused code was not consumed: it admits once the admin raises the ceiling"
        )
    finally:
        _mode(nest, "open")
