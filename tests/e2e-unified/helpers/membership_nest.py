"""A nest whose admin sells membership — the shared seam of the api-tier
paid-membership suites (`tests/api/test_membership_admission.py`,
`tests/api/test_membership_lapse.py`).

`docs/goal/behavior/monetization.md` § Pillar 4 → *Rail C*: a membership tier
is an ordinary subscription tier the nest's admin owns plus an Admin-class
designation linking it to a quota tier and a lapse tier
(`fauna.admin.membership_tiers.set`), and a verified payment for it admits or
moves a member. Everything here rides the wire a production caller uses: the
admin's choices through Admin-class WS-RPC (`testing.md` point 8's
fixture-setup carve-out), the payment through the public webhook a provider's
delivery system POSTs, signed with the fake provider's scheme.

Each suite starts its OWN nest through :func:`start_member_nest` (a
module-scoped fixture per file), so a suite that re-points the designation or
caps the free tier never reaches another file's assertions.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import urllib.error
import urllib.request
import uuid

from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import ensure_tier, set_registration_mode

NEST_DOMAIN = "members.example"
#: The admin's own subscription tier that is sold as membership.
MEMBERSHIP_TIER = "supporter"
#: The quota tier a paying member is put on.
QUOTA_TIER = "member"
FAKE_SECRET = "whsec_e2e_membership"


def _as_signing_key(raw) -> SigningKey:
    """Nests return the admin signing key as either a `SigningKey` or hex."""
    return raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))


def start_member_nest(
    request,
    nest_mode,
    tmp_path_factory,
    label: str,
    *,
    lapse_tier: str | None = None,
    quota_caps: dict | None = None,
    lapse_caps: dict | None = None,
) -> tuple[dict, callable]:
    """Start a nest whose admin sells membership: a `supporter` subscription
    tier designated to admit at the `member` quota tier (dropping to
    ``lapse_tier`` — the designation's own `free` default when None — when a
    window runs out), and a fake payment provider entitling it.

    ``quota_caps`` / ``lapse_caps`` are the `member` and lapse tiers' caps
    where they must differ from `free`'s; a ``lapse_tier`` other than `free` is
    created here (from `free`'s caps) before it is designated.

    Returns ``(handle, cleanup)``; the handle carries ``url``, ``port``,
    ``admin_sk``, ``admin_id`` and ``admin_actor``."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, label, claim_domain=NEST_DOMAIN,
    )
    admin_sk = _as_signing_key(nest["admin"]["signing_key"])
    admin_id = bytes(admin_sk.verify_key)
    admin_actor = ApiActor(nest["url"], "", admin_id.hex(), bytes(admin_sk))

    ensure_tier(nest["port"], base_url=nest["url"], admin_signing_key=admin_sk,
                name=QUOTA_TIER, **(quota_caps or {}))
    if lapse_tier is not None:
        ensure_tier(nest["port"], base_url=nest["url"], admin_signing_key=admin_sk,
                    name=lapse_tier, **(lapse_caps or {}))
    created = admin_actor.subscription_create_tier(MEMBERSHIP_TIER, rank=1, price_hint="$3/mo")
    assert created.get("created") is True, f"membership tier create failed: {created!r}"
    designate(admin_actor, QUOTA_TIER, lapse_tier)
    saved = admin_actor._ws_call("fauna.payments.providers.set", {
        "kind": "fake", "webhook_secret": FAKE_SECRET, "tier": MEMBERSHIP_TIER,
    })
    assert saved.get("saved") is True, f"providers.set failed: {saved!r}"

    return {
        "nest": nest,
        "url": nest["url"],
        "port": nest["port"],
        "admin_sk": admin_sk,
        "admin_id": admin_id,
        "admin_actor": admin_actor,
    }, cleanup


def designate(admin_actor: ApiActor, admin_tier: str, lapse_tier: str | None = None) -> None:
    """Point the membership at ``admin_tier`` (and ``lapse_tier``; omitted =
    the designation's `free` default) — `fauna.admin.membership_tiers.set`, an
    idempotent upsert that re-points."""
    body = {"tier_name": MEMBERSHIP_TIER, "admin_tier": admin_tier}
    if lapse_tier is not None:
        body["lapse_tier"] = lapse_tier
    reply = admin_actor._ws_call("fauna.admin.membership_tiers.set", body)
    assert reply.get("ok") is True, f"membership_tiers.set failed: {reply!r}"


def admin_client(member_nest) -> WsRpcAdminClient:
    sk = member_nest["admin_sk"]
    return WsRpcAdminClient(
        member_nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def set_mode(member_nest, mode: str, max_free_users: int | None = None) -> None:
    set_registration_mode(
        member_nest["port"], base_url=member_nest["url"],
        admin_signing_key=member_nest["admin_sk"], mode=mode,
        max_free_users=max_free_users)


def user_row(member_nest, actor_id: bytes) -> dict | None:
    """The admin's view of one account, or None when no account exists."""
    with admin_client(member_nest) as admin:
        try:
            return admin.call("fauna.admin.users.get", {"actor_id": actor_id})["user"]
        except RpcCallError:
            return None


def mint_membership_code(member_nest) -> str:
    minted = member_nest["admin_actor"]._ws_call(
        "fauna.payments.claims.mint", {"tier": MEMBERSHIP_TIER})
    assert minted.get("code"), f"claims.mint must return a code: {minted!r}"
    return minted["code"]


def claim_row(member_nest, code: str) -> dict:
    listed = member_nest["admin_actor"]._ws_call("fauna.payments.claims.list", {})
    rows = [c for c in listed.get("claims", []) if c.get("code") == code]
    assert len(rows) == 1, f"the minted code must be in the admin's audit list: {listed!r}"
    return rows[0]


def handle(prefix: str) -> str:
    return f"{prefix}{uuid.uuid4().hex[:8]}"


def pay_membership(member_nest, buyer_hex: str, valid_until: int) -> tuple[int, dict]:
    """POST a signed fake-provider `payment` event for the membership, naming
    ``buyer_hex`` as the buyer and ``valid_until`` (epoch seconds) as the end
    of the paid window. Returns ``(status, reply)``."""
    body = json.dumps({
        "id": f"pay_{uuid.uuid4().hex[:10]}",
        "event": "payment",
        "reference": buyer_hex,
        "valid_until": valid_until,
    }).encode()
    url = (f"{member_nest['url']}/api/v1/payments/webhook/"
           f"{member_nest['admin_id'].hex()}/fake")
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("X-Fauna-Signature",
                   hmac.new(FAKE_SECRET.encode(), body, hashlib.sha256).hexdigest())
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, {}
