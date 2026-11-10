"""The declared re-point axis, as the home nest applies it — full-stack (tier_3) API E2E.

Witnesses four `take-your-account-back` outcomes that are the *nest's* half of
`docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis:

* **40** — *"Anything the thief scheduled under your account — a handle
  change, a deletion — is cancelled by the recovery."* (the `pending_actions`
  timer: rows stay, written to their own terminal state)
* **41** — *"Mail and requests addressed to your old identity reach you as its
  successor instead of going nowhere."* (the delivery path's recipient
  re-point, before the inbox-mode routing)
* **43** — *"Your payment-provider connections, device authorizations and
  unredeemed membership codes from before the recovery are burned."* (the
  entitlement plane's burn, and the claim ledger voided as it travels)
* **44** — *"The devices that held the stolen key stop receiving your
  notifications, and the device you recover on signs up for them again."*
  (`push_subscriptions` burns in the reach leg; a successor seat's subscribe
  registers)

Each was ruled and built in the nest and pinned only by in-process Rust tests
in `bins/fauna-nest/src/db/successions.rs`, which the feature catalog cannot
cite.

**The ceremony is real throughout** (`helpers/succession.py` + the Rust
`recovery_fixture`): a genuine RecoveryKey registration and a genuine signed
succession statement the nest verifies against its own chain.

**Every assertion has its before.** A burned plane read empty afterwards is
equally consistent with the row never having been written, so each test reads
what it armed before the ceremony, and outcome 40 and 41 each carry a control
that the ceremony did not touch.

Latency-independent (convention 14): every call is a request with a reply and
every assertion is on returned state. Nothing sleeps and nothing polls.

Process safety: no ``pkill``/``killall``; the nest is started through the
framework's dedicated-nest seam and torn down by this module's fixture.
"""

from __future__ import annotations

import secrets
import uuid

import pytest
import requests
from nacl.signing import SigningKey

from actions.api_actor import ApiActor
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.push_dispatch import send_inbox_mail
from helpers.succession import register_recovery_kit, succeed_identity

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def repoint_nest(request, nest_mode, tmp_path_factory):
    """A nest this module owns: a succession revokes the old identity's
    sessions and re-points its planes, and outcome 40 reads the nest-wide
    pending-action list, which must hold only this module's rows."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-repoint-axis-nest"
    )
    yield nest
    cleanup()


def _fresh_actor(nest) -> dict:
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _admin_ws(nest) -> WsRpcAdminClient:
    admin_sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )


def _succeed(nest, actor) -> dict:
    """Take `actor`'s account back. Returns the successor as an actor dict
    (`actor_id_hex`, `actor_id_bytes`, `signing_key`) — its actor id IS the
    Ed25519 pubkey of the seed the statement named."""
    seed_hex = bytes(actor["signing_key"]).hex()
    successor_seed = secrets.token_bytes(32)
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=actor["actor_id_hex"], identity_seed_hex=seed_hex
    )
    successor_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=actor["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed.hex(),
        old_seed_hex=seed_hex,
    )
    sk = SigningKey(successor_seed)
    assert bytes(sk.verify_key).hex() == successor_id, (
        "the successor seed must be the one the statement named"
    )
    return {
        "actor_id_hex": successor_id,
        "actor_id_bytes": bytes(sk.verify_key),
        "signing_key": sk,
    }


def _admin_status(nest, action_id: int) -> str:
    """An action's status over the cross-account admin list — the retired
    identity cannot sign in to read `fauna.pending_actions.get`, and that kind
    answers its creator only."""
    with _admin_ws(nest) as admin:
        actions = admin.call("fauna.admin.pending_actions.list", {})["actions"]
    row = next((a for a in actions if a["id"] == action_id), None)
    assert row is not None, f"action {action_id} missing from the admin list: {actions!r}"
    return row["status"]


@pytest.mark.feature("take-your-account-back")
def test_what_the_thief_scheduled_is_cancelled_by_the_recovery(repoint_nest):
    """Outcome 40: a queued handle change and a queued deletion both read cancelled.

    `pending_actions` is the plane where the dangerous thing is a **timer**:
    the executor selects on `status = 'pending' AND execute_after <= now` and
    authenticates nobody, so a row the ceremony left armed would fire on the
    account the ceremony just rescued. The delays (6 h for a handle change,
    14 d for a deletion) put a racing succession inside the window by
    construction. The ruling keeps the rows — a thief's scheduled destruction
    is worth leaving legible — and writes their own terminal state.

    The control: an identically scheduled handle change by an actor who was
    never succeeded stays pending, so "cancelled" is the ceremony's act and
    not some nest-wide sweep.
    """
    nest = repoint_nest
    victim = _fresh_actor(nest)
    bystander = _fresh_actor(nest)
    suffix = secrets.token_hex(3)

    with _ws(nest, victim) as ws:
        rename = ws.call(
            "fauna.profile.handle.change", {"handle": f"taken-{suffix}"}
        )
        deletion = ws.call("fauna.account.delete", {})
    with _ws(nest, bystander) as ws:
        control = ws.call(
            "fauna.profile.handle.change", {"handle": f"kept-{suffix}"}
        )
    for reply in (rename, deletion, control):
        assert reply["status"] == "pending", f"must be scheduled, not immediate: {reply!r}"
        assert _admin_status(nest, reply["pending_action_id"]) == "pending", reply

    _succeed(nest, victim)

    assert _admin_status(nest, rename["pending_action_id"]) == "cancelled", (
        "the handle change the thief queued must be disarmed by the recovery — "
        "left pending, the executor runs it on the rescued account"
    )
    assert _admin_status(nest, deletion["pending_action_id"]) == "cancelled", (
        "the account deletion the thief queued must be disarmed by the recovery"
    )
    assert _admin_status(nest, control["pending_action_id"]) == "pending", (
        "an actor who was never succeeded keeps their scheduled action — the "
        "disarm is the ceremony's, scoped to the retired identity"
    )


@pytest.mark.feature("take-your-account-back")
def test_mail_and_requests_to_the_old_identity_reach_the_successor(repoint_nest):
    """Outcome 41: a knock and a mail addressed to the retired id land on the successor.

    A peer that has not propagated the statement goes on addressing the retired
    id, and refusing it would black-hole mail in flight, so the arrival is
    accepted — and resolved through the succession chain's terminal hop BEFORE
    the inbox-mode routing (`routes.rs`, the recipient consult). Two arrivals,
    because they take the two routes that ordering decides between:

    * **the knock** — the owner kept the default `allow_knock`, so a
      stranger's first mail becomes a pending request the successor must see
      on their own knock list;
    * **the mail** — the owner had opened their inbox; the mode moved with the
      account, and the arrival must be judged against the SUCCESSOR's mode and
      delivered to their inbox. Without the re-point, the retired id holds no
      mode row any more and the mail falls back to a knock nobody can read.

    Both are composed and sent after the ceremony, addressed to the OLD id —
    exactly what a not-yet-propagated peer does.
    """
    nest = repoint_nest
    url = nest["url"]

    knocked = _fresh_actor(nest)
    mailed = _fresh_actor(nest)
    with _ws(nest, mailed) as ws:
        ws.call("fauna.inbox.mode.set", {"mode": "open"})

    knocked_heir = _succeed(nest, knocked)
    mailed_heir = _succeed(nest, mailed)

    stranger = _fresh_actor(nest)
    writer = _fresh_actor(nest)

    knock_reply = send_inbox_mail(url, stranger, knocked, "addressed to the old id")
    assert knock_reply.get("inbox_id") is None, (
        f"under the successor's inherited allow_knock a stranger's first mail "
        f"is held as a knock: {knock_reply!r}"
    )
    with _ws(nest, knocked_heir) as ws:
        knocks = ws.call("fauna.knocks.list", {}).get("knocks", [])
    assert [k["sender"] for k in knocks] == [stranger["actor_id_hex"]], (
        f"the request addressed to the retired identity must wait on the "
        f"SUCCESSOR's knock list, the only one anyone can sign in to read: {knocks!r}"
    )

    mail_reply = send_inbox_mail(url, writer, mailed, "addressed to the old id")
    assert mail_reply.get("inbox_id") is not None, (
        f"the successor's inherited open mode must deliver the mail, not hold "
        f"it — a knock here means the arrival was judged against the retired "
        f"identity's (now empty) mode: {mail_reply!r}"
    )
    with _ws(nest, mailed_heir) as ws:
        items = ws.call("fauna.inbox.fetch", {"limit": 0}).get("items", [])
    assert len(items) == 1, (
        f"the mail addressed to the retired identity must land in the "
        f"successor's inbox: {len(items)} item(s)"
    )
    assert items[0]["id"] == mail_reply["inbox_id"], (
        f"the reply's inbox_id must name the successor's row: {items!r} vs {mail_reply!r}"
    )


@pytest.mark.feature("take-your-account-back")
def test_payment_connections_device_authorizations_and_unredeemed_codes_are_burned(
    repoint_nest,
):
    """Outcome 43: all three entitlement authorities die at the ceremony.

    Each is a standing authority a seed thief could have planted, consulted by
    a path that never asks who is authenticating:

    * **the provider connection** — its `webhook_secret` verifies events on a
      public webhook URL that names the actor; a thief-chosen secret moved to
      the successor would let the thief forge verified payments against the
      successor's own tiers. It burns: nothing on either identity.
    * **the device authorization** — the stored `ManageSubscribers` delegation
      the public federation bootstrap read serves. It is signed by and names
      the predecessor, so it can authorize nothing of the successor's. It
      burns: neither identity serves one.
    * **the unredeemed claim code** — a bearer credential redeemed by code
      alone. The ledger MOVES (it is the payment audit trail and names the
      tiers that moved), and every unredeemed code is stamped voided as it
      travels: the successor sees the row, and nobody can redeem it.
    """
    nest = repoint_nest
    url = nest["url"]
    creator = _fresh_actor(nest)
    creator_api = ApiActor(
        url, creator["token"], creator["actor_id_hex"], bytes(creator["signing_key"])
    )
    tier = f"gold-{secrets.token_hex(3)}"
    reply = creator_api.subscription_create_tier(tier, rank=1, auto_approve=False)
    assert reply.get("created") is True, f"tier create failed: {reply!r}"

    nest_info = requests.get(f"{url}/api/v1/nest/info", timeout=15)
    assert nest_info.status_code == 200, nest_info.text
    nest_pubkey = bytes.fromhex(nest_info.json()["public_key"])

    from fauna_ffi import build_manage_subscribers_delegation

    delegation = build_manage_subscribers_delegation(
        bytes(creator["signing_key"]), nest_pubkey
    )
    with _ws(nest, creator) as ws:
        saved = ws.call(
            "fauna.payments.providers.set",
            {"kind": "fake", "webhook_secret": "whsec_the_thief_chose_this", "tier": tier},
        )
        assert saved.get("saved") is True, f"providers.set failed: {saved!r}"
        uploaded = ws.call(
            "fauna.subscriptions.delegate.upload", {"authorization": delegation}
        )
        assert uploaded.get("uploaded") is True, uploaded
        code = ws.call("fauna.payments.claims.mint", {"tier": tier})["code"]
        providers_before = ws.call("fauna.payments.providers.list", {})["providers"]
        claims_before = ws.call("fauna.payments.claims.list", {})["claims"]

    def delegation_status(actor_hex: str) -> int:
        return requests.get(
            f"{url}/api/v1/subscriptions/delegate/{actor_hex}", timeout=15
        ).status_code

    assert [p["kind"] for p in providers_before] == ["fake"], providers_before
    assert delegation_status(creator["actor_id_hex"]) == 200, (
        "the uploaded delegation must be served before the ceremony"
    )
    before = [c for c in claims_before if c["code"] == code]
    assert len(before) == 1 and before[0].get("voided_at") is None, claims_before

    heir = _succeed(nest, creator)

    with _ws(nest, heir) as ws:
        providers_after = ws.call("fauna.payments.providers.list", {})["providers"]
        claims_after = ws.call("fauna.payments.claims.list", {})["claims"]

    assert providers_after == [], (
        f"a provider secret the thief may have chosen must not verify events "
        f"against the successor's tiers: {providers_after!r}"
    )
    assert delegation_status(creator["actor_id_hex"]) == 404, (
        "the retired identity's delegation must stop being served — the "
        "bootstrap read is public and consults no refusal plane"
    )
    assert delegation_status(heir["actor_id_hex"]) == 404, (
        "the predecessor-signed delegation must not be handed to the successor"
    )

    after = [c for c in claims_after if c["code"] == code]
    assert len(after) == 1, (
        f"the claim ledger is the payment audit trail and must travel with the "
        f"tiers it names, so the successor still sees the code: {claims_after!r}"
    )
    assert after[0].get("voided_at") is not None, (
        f"an unredeemed code is a bearer credential the thief could have "
        f"minted, so it must arrive voided: {after[0]!r}"
    )

    buyer = _fresh_actor(nest)
    with _ws(nest, buyer) as ws:
        with pytest.raises(RpcCallError) as refused:
            ws.call("fauna.payments.claims.redeem", {"code": code})
    assert refused.value.code, (
        f"a voided code must not be redeemable by whoever holds it: {refused.value!r}"
    )


@pytest.mark.feature("take-your-account-back")
def test_the_stolen_keys_push_subscriptions_are_burned_and_the_successor_subscribes(
    repoint_nest,
):
    """Outcome 44: the predecessor's push rows are gone; the successor's registers.

    A push subscription is a caller-supplied endpoint plus the caller's own
    keys, armed by an ordinary User-class subscribe — exactly what a seed thief
    holds — and the dispatcher POSTs to it authenticating nobody. Nothing can
    list them and unsubscribe needs the device id, so a row left on the retired
    identity would be a live feed nobody can stop, and a row moved to the
    successor would hand the thief the rescued account's notifications. They
    burn; what dies is one re-registration, which the device the owner
    recovers on makes (its client re-subscribes as the successor).

    Read through the nest's own push registry (the `test-hooks` read the
    dispatcher's table answers), because no production kind lists the rows —
    which is the point of the ruling.
    """
    nest = repoint_nest
    url = nest["url"]
    owner = _fresh_actor(nest)
    stolen_device = f"stolen-{uuid.uuid4().hex[:8]}"
    recovered_device = f"recovered-{uuid.uuid4().hex[:8]}"

    def subscribe(actor: dict, device_id: str) -> None:
        with _ws(nest, actor) as ws:
            reply = ws.call(
                "fauna.push.subscribe",
                {
                    "device_id": device_id,
                    "transport": "web-push",
                    "endpoint": f"https://push.example.invalid/{device_id}",
                    # Never dialled: this test reads the registry, not a push.
                    "key_p256dh": secrets.token_urlsafe(48),
                    "key_auth": secrets.token_urlsafe(12),
                },
            )
        assert reply["ok"] is True, reply

    def registered(actor_hex: str) -> list[str]:
        resp = requests.get(
            f"{url}/api/v1/test/push/subscriptions",
            params={"actor_id": actor_hex},
            timeout=15,
        )
        assert resp.status_code == 200, resp.text
        return sorted(r["device_id"] for r in resp.json())

    subscribe(owner, stolen_device)
    assert registered(owner["actor_id_hex"]) == [stolen_device], (
        "the stolen key's subscription must be registered before the ceremony"
    )

    heir = _succeed(nest, owner)

    assert registered(owner["actor_id_hex"]) == [], (
        "the subscriptions the stolen key armed must be burned — left on the "
        "retired identity they are a feed nobody can authenticate to stop"
    )
    assert registered(heir["actor_id_hex"]) == [], (
        "they must not be moved to the successor either: a thief-planted "
        "endpoint would then receive the rescued account's notifications"
    )

    subscribe(heir, recovered_device)
    assert registered(heir["actor_id_hex"]) == [recovered_device], (
        "the device the owner recovers on must be able to sign up again as the "
        "successor"
    )
