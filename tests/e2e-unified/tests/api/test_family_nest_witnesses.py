"""The family-safety nest outcomes, observed from outside — full-stack (tier_3) API E2E.

Witnesses the `[nest]` outcomes of `docs/features/family-safety.md` that are
decided entirely by the nest and observable on the WS-RPC wire (the mail-gate
outcomes, which need a real mail peer, live beside the other mail-gate
witnesses in `tests/platform/docker/test_mail_family_gate_docker.py`):

* **14** — contact from other nests switched off: strangers there cannot start
  contact, accepted contacts there keep getting through
  (`docs/goal/behavior/family-safety.md` § Guardian policy — the enforcement split).
* **15** — with contact approval on, an invitation into a conversation from
  someone not approved does not reach the child, on the same nest and across
  nests (§ Implementation status today, *The reach pillar binds on every
  inbox-write path*).
* **24** — a supervised child cannot delete their own account (§ Lifecycle gates).
* **25** — a supervising account cannot be removed until each child is handed
  over or graduated, and the refusal names them (§ Lifecycle gates).
* **28** — admission at an age band starts the child on that band's suggested
  settings, which the guardian can then change (§ The account age band).
* **23** — a guardian's no to a contact the child asked for blocks nobody, and
  the child can ask again (§ Child-initiated contact requests).
* **27** — a supervised child can still change their own plan (§ Lifecycle gates).
* **37** — the guardian is told only the category and the count, never the
  content or an item id (`docs/goal/behavior/family-client-enforcement.md`
  § Guardian Notify).

Outcome 20 (the bridge-DM refusal) needs a consented bridge principal and lives
in `test_family_bridge_dm_gate.py`; outcome 26 (the link across a recovery)
needs a nest of its own and lives in `test_family_succession.py`.

Every refusal here is asserted as a two-way discriminator: the same call, with
the one fact the rule keys on changed, must go through — otherwise "refused"
cannot be told from "this call never works".

Account budget: wards can only be admitted through the anonymous
invite-request path on this nest (`helpers/family_wire.py::admit_ward`), which
the shipped build throttles per source. This module spends three submits — the
shared pair, the lifecycle test's own ward (it graduates it) and the age-band
ward (the band is set at admission). Every other account is admin-admitted.

Process safety: no ``pkill``/``killall``; both nests are session-scoped
fixtures torn down by their own fixtures.
"""

from __future__ import annotations

import os
import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError
from clients.ws_rpc_federation_client import FederationChannelClient
from common import build_email_inbox_payload
from helpers.admin_wire import admin_client
from helpers.family_wire import (
    actor_ws,
    admit_adult_directly,
    admit_ward,
    family_status,
    reset_ward_policy,
    set_ward_policy,
    ward_row,
)

pytestmark = pytest.mark.tier_3

#: `rpc_errors::forbidden_ns("conversations", …)` — the same-nest Welcome refusal.
CONVERSATIONS_FORBIDDEN = "fauna.conversations.forbidden"
#: `rpc_errors::forbidden_ns("federation", …)` — every cross-nest refusal.
FEDERATION_FORBIDDEN = "fauna.federation.forbidden"
#: A supervised account's self-delete (`account_handlers.rs`, the lifecycle gate).
ACCOUNT_GUARDIAN_APPROVAL = "fauna.account.guardian_approval_required"
#: A guardian's self-delete while links stand.
ACCOUNT_GUARDIANSHIPS_UNRESOLVED = "fauna.account.guardianships_unresolved"
#: An admin's evict/delete of a guardian while links stand
#: (`admin_ws_handlers.rs::require_no_guardianships`).
ADMIN_GUARDIANSHIPS_UNRESOLVED = "fauna.admin.guardianships_unresolved"


def _handle(stem: str) -> str:
    return f"famwit-{stem}-{os.urandom(3).hex()}"


@pytest.fixture(scope="module")
def _shared_family(nest_instance):
    """One guardian + one ward for the tests that only set policy and read —
    one anonymous submit for all of them."""
    guardian = admit_adult_directly(nest_instance, _handle("guardian"))
    ward = admit_ward(nest_instance, guardian, _handle("ward"))
    return guardian, ward


@pytest.fixture
def family_pair(_shared_family, nest_instance):
    """The shared pair with the ward's policy document reset to its defaults
    first, so no test inherits another's knobs. Returns ``(guardian, ward)``."""
    guardian, ward = _shared_family
    reset_ward_policy(nest_instance, guardian, ward)
    return guardian, ward


def _open_inbox(nest, identity: dict) -> None:
    """Set ``identity``'s own inbox mode to ``open``, so the inbox MODE never
    refuses and any refusal left is the guardian's reach policy."""
    with actor_ws(nest, identity) as ws:
        ws.call("fauna.inbox.mode.set", {"mode": "open"})


def _inbox_items(nest, identity: dict) -> list:
    with actor_ws(nest, identity) as ws:
        return ws.call("fauna.inbox.fetch", {"limit": 0}).get("items", [])


def _pending_approvals_for(nest, guardian: dict, ward: dict, peer_hex: str) -> list:
    """Every entry in the guardian's approval queue for ``ward`` naming ``peer_hex``."""
    want_ward = bytes.fromhex(ward["actor_id_hex"])
    want_peer = bytes.fromhex(peer_hex)
    with actor_ws(nest, guardian) as ws:
        approvals = ws.call("fauna.family.approvals.list", {}).get("approvals") or []
    return [
        a
        for a in approvals
        if bytes(a.get("supervised_actor_id") or b"") == want_ward
        and bytes(a.get("peer_actor_id") or b"") == want_peer
    ]


def _same_nest_welcome(nest, sender: dict, recipient: dict, channel_hex: str) -> dict:
    """One DM Welcome from ``sender`` to ``recipient`` on the same nest."""
    with actor_ws(nest, sender) as ws:
        return ws.call(
            "fauna.conversations.welcome.deliver",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "channel_id": channel_hex,
                "welcome_bytes": secrets.token_bytes(96),
                "kind": {"type": "dm"},
            },
        )


def _relayed_welcome(peer_nest, nest, recipient: dict, channel_hex: str) -> dict:
    """One DM Welcome a peer nest relays to ``recipient`` — the cross-nest door,
    whose wire carries no signed sender."""
    with FederationChannelClient(initiator=peer_nest, target=nest) as fed:
        return fed.call(
            "fauna.federation.welcome.deliver",
            {
                "recipient_actor_id": recipient["actor_id_hex"],
                "channel_id": channel_hex,
                "welcome_bytes": secrets.token_bytes(96),
                "channel_type": "dm",
                "group_id": None,
                "origin_nest_url": peer_nest["url"],
            },
        )


def _relayed_inbox_message(peer_nest, nest, sender_sk, recipient: dict, subject: str) -> dict:
    """One signed social-inbox message from ``sender_sk`` (an actor living on
    the peer nest) that the peer nest relays to ``recipient`` — the cross-nest
    door that DOES name its sender, by the payload's own signature."""
    payload, _ = build_email_inbox_payload(
        sender_sk, recipient["actor_id_hex"], subject, "body", node_url=nest["url"]
    )
    with FederationChannelClient(initiator=peer_nest, target=nest) as fed:
        return fed.call(
            "fauna.federation.inbox.deliver",
            {"recipient_actor_id": recipient["actor_id_hex"], "payload_bytes": payload},
        )


# ── 14: contact from other nests switched off ───────────────────────────────


@pytest.mark.feature("family-safety")
def test_federation_contact_off_turns_away_strangers_on_other_nests_but_not_accepted_contacts(
    nest_instance, second_nest, family_pair
):
    """Outcome 14, both clauses, through both cross-nest doors.

    A stranger on the peer nest tries the two ways another nest can start
    contact: a signed social-inbox message (`fauna.federation.inbox.deliver`)
    and a conversation invitation (`fauna.federation.welcome.deliver`). Both are
    refused, nothing lands in the ward's inbox, and — because
    `supervised_reach_verdict` checks `federation_contact` BEFORE
    `contact_approval` — the stranger never reaches the guardian's queue either.

    Then the guardian accepts that same remote person as the ward's contact
    (`fauna.family.contact.add`), and the identical relayed message arrives:
    "contacts already accepted there keep getting through". The ward's own inbox
    mode is `open` throughout, so the only thing that changes between the two
    arms is the contact edge the reach floor keys on.
    """
    guardian, ward = family_pair
    _open_inbox(nest_instance, ward)
    set_ward_policy(nest_instance, guardian, ward, federation_contact=False)
    # The ward is shared across this module, so its inbox is measured, never
    # assumed empty.
    before = len(_inbox_items(nest_instance, ward))

    from nacl.signing import SigningKey

    remote_sk = SigningKey.generate()
    remote_hex = bytes(remote_sk.verify_key).hex()

    with pytest.raises(RpcCallError) as refused_message:
        _relayed_inbox_message(second_nest, nest_instance, remote_sk, ward, "hello from afar")
    assert refused_message.value.code == FEDERATION_FORBIDDEN, (
        f"a stranger on another nest messaging a ward with contact from other "
        f"nests off must be refused with {FEDERATION_FORBIDDEN}: {refused_message.value!r}"
    )

    with pytest.raises(RpcCallError) as refused_welcome:
        _relayed_welcome(second_nest, nest_instance, ward, secrets.token_bytes(32).hex())
    assert refused_welcome.value.code == FEDERATION_FORBIDDEN, (
        f"a conversation invitation relayed from another nest must be refused "
        f"with {FEDERATION_FORBIDDEN}: {refused_welcome.value!r}"
    )

    assert len(_inbox_items(nest_instance, ward)) == before, (
        "nothing of the stranger's may land in the ward's inbox"
    )
    assert _pending_approvals_for(nest_instance, guardian, ward, remote_hex) == [], (
        "a stranger turned away by `federation_contact = off` must not even reach "
        "the guardian's queue — the knob suppresses before contact approval knocks"
    )

    with actor_ws(nest_instance, guardian) as guardian_ws:
        guardian_ws.call(
            "fauna.family.contact.add",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "peer_actor_id": bytes.fromhex(remote_hex),
            },
        )

    reply = _relayed_inbox_message(
        second_nest, nest_instance, remote_sk, ward, "hello again, now accepted"
    )
    assert (reply.get("inbox_id") or 0) >= 1, (
        f"once the remote person is an accepted contact, their relayed message "
        f"must be delivered with `federation_contact` still off: {reply!r}"
    )
    assert len(_inbox_items(nest_instance, ward)) == before + 1, (
        "exactly the accepted contact's message joined the ward's inbox"
    )


# ── 15: conversation invitations under contact approval ─────────────────────


@pytest.mark.feature("family-safety")
def test_contact_approval_keeps_an_unapproved_persons_conversation_invitation_from_the_child(
    nest_instance, second_nest, family_pair
):
    """Outcome 15, on the same nest and across nests.

    Same nest: a stranger's DM Welcome to a ward under `contact_approval` is
    refused with the opaque conversations refusal and leaves no inbox row; once
    the guardian approves that person (`fauna.family.contact.add`) the identical
    invitation is delivered — the knob acts on people not approved, nothing else.

    Across nests: the relayed Welcome names no signed sender, so nobody there
    can ever be the approved party (`unauthenticated_reach_verdict` suppresses
    under `contact_approval`). The discriminator is the knob itself: the same
    peer nest's identical relay is delivered once the guardian turns contact
    approval off. The ward's inbox mode is `open` throughout.
    """
    guardian, ward = family_pair
    _open_inbox(nest_instance, ward)
    set_ward_policy(nest_instance, guardian, ward, contact_approval=True)
    before = len(_inbox_items(nest_instance, ward))

    stranger = admit_adult_directly(nest_instance, _handle("stranger"))
    channel_hex = secrets.token_bytes(32).hex()

    with pytest.raises(RpcCallError) as same_nest:
        _same_nest_welcome(nest_instance, stranger, ward, channel_hex)
    assert same_nest.value.code == CONVERSATIONS_FORBIDDEN, (
        f"an unapproved person's conversation invitation to a ward under contact "
        f"approval must be refused with {CONVERSATIONS_FORBIDDEN}: {same_nest.value!r}"
    )

    remote_channel_hex = secrets.token_bytes(32).hex()
    with pytest.raises(RpcCallError) as cross_nest:
        _relayed_welcome(second_nest, nest_instance, ward, remote_channel_hex)
    assert cross_nest.value.code == FEDERATION_FORBIDDEN, (
        f"a relayed conversation invitation names nobody the guardian could have "
        f"approved, so it must be refused with {FEDERATION_FORBIDDEN}: {cross_nest.value!r}"
    )
    assert len(_inbox_items(nest_instance, ward)) == before, (
        "neither refused invitation may leave anything in the ward's inbox"
    )

    with actor_ws(nest_instance, guardian) as guardian_ws:
        guardian_ws.call(
            "fauna.family.contact.add",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "peer_actor_id": bytes.fromhex(stranger["actor_id_hex"]),
            },
        )
    delivered = _same_nest_welcome(nest_instance, stranger, ward, channel_hex)
    assert (delivered.get("inbox_id") or 0) >= 1, (
        f"the same invitation from the now-approved person must reach the ward: {delivered!r}"
    )

    set_ward_policy(nest_instance, guardian, ward, contact_approval=False)
    relayed = _relayed_welcome(second_nest, nest_instance, ward, remote_channel_hex)
    assert (relayed.get("inbox_id") or 0) >= 1, (
        f"the identical relay must deliver once contact approval is off — else "
        f"the refusal above is a relay that never delivers, not the knob: {relayed!r}"
    )


# ── 24 / 25: the lifecycle gates ────────────────────────────────────────────


@pytest.mark.feature("family-safety")
def test_a_supervised_child_cannot_delete_their_own_account(nest_instance, family_pair):
    """Outcome 24: the ward's own `fauna.account.delete` is refused, typed.

    Self-deletion would sever the guardianship link unilaterally, so it waits
    for the guardian to graduate the account. The positive arm — the SAME call
    by an unsupervised account schedules the deletion — is what shows the
    refusal is the supervision and not the call. The graduated-ward arm is the
    lifecycle test below, which owns a ward it may consume.
    """
    _guardian, ward = family_pair

    with pytest.raises(RpcCallError) as refusal:
        with actor_ws(nest_instance, ward) as ws:
            ws.call("fauna.account.delete", {})
    assert refusal.value.code == ACCOUNT_GUARDIAN_APPROVAL, (
        f"a supervised account's self-delete must be refused with "
        f"{ACCOUNT_GUARDIAN_APPROVAL}: {refusal.value!r}"
    )

    adult = admit_adult_directly(nest_instance, _handle("adult"))
    with actor_ws(nest_instance, adult) as ws:
        ws.call("fauna.account.delete", {})


@pytest.mark.feature("family-safety")
def test_a_supervising_account_cannot_be_removed_until_its_child_is_graduated(
    nest_instance,
):
    """Outcome 25 (and the second half of 24): every removal of a guardian is
    refused while a link stands, the admin's refusal names the child, and
    graduation lifts both gates.

    Three doors are tried while the link stands: the admin's evict, the admin's
    delete, and the guardian's own self-delete. The admin refusals carry the
    supervised account ids in their details — "the refusal names them" — which
    is what lets the admin resolve each link. Then the guardian graduates the
    child; the graduated account may now delete itself, and the admin's delete
    of the former guardian goes through.
    """
    guardian = admit_adult_directly(nest_instance, _handle("lifeguard"))
    ward = admit_ward(nest_instance, guardian, _handle("lifeward"))
    guardian_id = bytes.fromhex(guardian["actor_id_hex"])

    with admin_client(nest_instance) as admin:
        with pytest.raises(RpcCallError) as evict:
            admin.call(
                "fauna.admin.users.evict",
                {"actor_id": guardian_id, "reason": "cleanup", "category": "other"},
            )
        with pytest.raises(RpcCallError) as delete:
            admin.call("fauna.admin.users.delete", {"actor_id": guardian_id})
    for door, refusal in (("evict", evict), ("delete", delete)):
        assert refusal.value.code == ADMIN_GUARDIANSHIPS_UNRESOLVED, (
            f"the admin's {door} of a guardian must be refused with "
            f"{ADMIN_GUARDIANSHIPS_UNRESOLVED}: {refusal.value!r}"
        )
        assert ward["actor_id_hex"] in str(refusal.value.details), (
            f"the admin's {door} refusal must name the supervised account "
            f"{ward['actor_id_hex']}: details={refusal.value.details!r}"
        )

    with pytest.raises(RpcCallError) as self_delete:
        with actor_ws(nest_instance, guardian) as ws:
            ws.call("fauna.account.delete", {})
    assert self_delete.value.code == ACCOUNT_GUARDIANSHIPS_UNRESOLVED, (
        f"a guardian's own delete must be refused with "
        f"{ACCOUNT_GUARDIANSHIPS_UNRESOLVED}: {self_delete.value!r}"
    )

    with actor_ws(nest_instance, guardian) as ws:
        ws.call(
            "fauna.family.graduate",
            {"supervised_actor_id": bytes.fromhex(ward["actor_id_hex"])},
        )

    # The graduated account is an ordinary one now: its own delete schedules.
    with actor_ws(nest_instance, ward) as ws:
        ws.call("fauna.account.delete", {})
    with admin_client(nest_instance) as admin:
        admin.call("fauna.admin.users.delete", {"actor_id": guardian_id})


# ── 28: the age band's suggested settings ───────────────────────────────────


@pytest.mark.feature("family-safety")
def test_a_child_admitted_at_an_age_band_starts_on_that_bands_settings(
    nest_instance, family_pair
):
    """Outcome 28: admission at `u13` writes the under-13 catalog as the child's
    first policy document, which both sides read back, and which the guardian
    then edits knob by knob.

    The discriminator is the shared ward admitted with no band: its document is
    the unsupervised-equivalent one (every reach knob open), so the U13 values
    below are the band's doing, not the nest's default.
    """
    guardian, unbanded = family_pair
    ward = admit_ward(nest_instance, guardian, _handle("u13"), age_band="u13")

    unbanded_policy = ward_row(family_status(nest_instance, guardian), unbanded)["policy"]
    assert unbanded_policy["contact_approval"] is False
    assert unbanded_policy["federation_contact"] is True

    own = family_status(nest_instance, ward)
    policy = own["policy"]
    assert own["age_band"]["band"] == "u13", own["age_band"]
    assert policy["contact_approval"] is True, policy
    assert policy["unknown_sender_mail"] == "hold", policy
    assert policy["federation_contact"] is False, policy
    assert policy["feed_sources"] == "block", policy
    assert policy["unknown_peer_dm"] == "hold", policy
    assert policy["content_notify"] is True, policy
    assert policy["content_policy"]["nsfw"] == "block", policy

    guardian_view = ward_row(family_status(nest_instance, guardian), ward)
    assert guardian_view is not None, "the banded ward is missing from its guardian's status"
    assert guardian_view["policy"] == policy, (
        "the guardian must read the same starting document the child reads"
    )

    # The settings are a starting point: the guardian edits one knob and the
    # rest of the band's document stands.
    with actor_ws(nest_instance, guardian) as ws:
        ws.call(
            "fauna.family.policy.update",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "policy": {**policy, "federation_contact": True},
            },
        )
    after = family_status(nest_instance, ward)["policy"]
    assert after["federation_contact"] is True, after
    assert after["contact_approval"] is True, after
    assert after["unknown_sender_mail"] == "hold", after


# ── 23: a refused contact ask blocks nobody ─────────────────────────────────


@pytest.mark.feature("family-safety")
def test_a_guardians_no_to_a_contact_ask_blocks_nobody_and_the_child_can_ask_again(
    nest_instance, family_pair
):
    """Outcome 23: the guardian's refusal drops the ask and nothing else.

    The ward asks for a contact (`fauna.family.contact.request`), the guardian
    refuses it from the queue, and then two things are observed from the ward's
    side: the ward's contact status for that person carries no edge at all (a
    block would read `blocked`), and the ward's second ask is accepted and lands
    in the guardian's queue again — the same call the nest refuses with
    `permission_denied` when the person IS blocked.
    """
    guardian, ward = family_pair
    set_ward_policy(nest_instance, guardian, ward, contact_approval=True)
    peer = admit_adult_directly(nest_instance, _handle("asked"))
    peer_id = bytes.fromhex(peer["actor_id_hex"])

    def ask() -> None:
        with actor_ws(nest_instance, ward) as ws:
            ws.call("fauna.family.contact.request", {"peer_actor_id": peer_id})

    def queued() -> list:
        return [
            a
            for a in _pending_approvals_for(nest_instance, guardian, ward, peer["actor_id_hex"])
            if a.get("kind") == "contact_request"
        ]

    ask()
    assert len(queued()) == 1, "the child's ask must wait in the guardian's queue"

    with actor_ws(nest_instance, guardian) as ws:
        ws.call(
            "fauna.family.approvals.decide",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "kind": "contact_request",
                "peer_actor_id": peer_id,
                "approve": False,
            },
        )
    assert queued() == [], "a refused ask leaves the guardian's queue"

    with actor_ws(nest_instance, ward) as ws:
        status = ws.call("fauna.contacts.status", {"peer_id": peer["actor_id_hex"]})
    assert status.get("status") is None, (
        f"a guardian's no must leave no contact edge between the child and the "
        f"person — least of all `blocked`: {status!r}"
    )

    ask()
    assert len(queued()) == 1, (
        "the child may ask again after a no, and the new ask waits for the guardian"
    )


# ── 27: the child's own plan ────────────────────────────────────────────────


@pytest.mark.feature("family-safety")
def test_a_supervised_child_can_still_change_their_own_plan(nest_instance, family_pair):
    """Outcome 27: supervision does not gate the account's own plan change.

    The ward redeems an upgrade code itself (`fauna.account.upgrade`) and its
    own account read shows the new plan, while its guardian still lists it — the
    plan moved and the supervision did not.
    """
    guardian, ward = family_pair
    with admin_client(nest_instance) as admin:
        code = admin.call(
            "fauna.admin.invite_codes.create", {"tier": "community", "uses": 1}
        )["code"]

    with actor_ws(nest_instance, ward) as ws:
        before = ws.call("fauna.account.get", {})["tier"]
        assert before != "community", f"the ward must start below the target plan: {before!r}"
        ws.call("fauna.account.upgrade", {"tier": "community", "invite_code": code})
        after = ws.call("fauna.account.get", {})["tier"]
    assert after == "community", f"the child's own plan change must take: {after!r}"
    assert ward_row(family_status(nest_instance, guardian), ward) is not None, (
        "changing the plan must leave the child supervised by the same guardian"
    )


# ── 37: what the guardian is told ───────────────────────────────────────────


@pytest.mark.feature("family-safety")
def test_the_guardian_is_told_only_the_category_and_the_count(nest_instance, family_pair):
    """Outcome 37: the report the child's app sends, and everything the guardian
    can read back, carry a category and a count and nothing else.

    The ward's report is deliberately polluted with what a careless or hostile
    client might add — a content identifier, an item id, a snippet of the hidden
    text, at both the top level and inside the entry. The nest takes the
    category and the count, and none of the extras appears anywhere in the
    guardian's family status or in their notifications.
    """
    guardian, ward = family_pair
    set_ward_policy(nest_instance, guardian, ward, content_notify=True)
    marker = f"leak-{os.urandom(6).hex()}"

    with actor_ws(nest_instance, ward) as ws:
        ws.call(
            "fauna.family.notify_report",
            {
                "entries": [
                    {
                        "category": "nsfw",
                        "count": 3,
                        "content_id": f"{marker}-content",
                        "item_id": f"{marker}-item",
                        "text": f"{marker}-text",
                    }
                ],
                "utc_offset_minutes": 0,
                "post_id": f"{marker}-post",
            },
        )

    row = ward_row(family_status(nest_instance, guardian), ward)
    assert row is not None, "the ward is missing from its guardian's status read"
    notices = row.get("content_notices") or []
    nsfw = [n for n in notices if n.get("category") == "nsfw"]
    assert nsfw and nsfw[0]["count"] >= 3, (
        f"the guardian must be told the category and how many times: {notices!r}"
    )
    for notice in notices:
        assert set(notice) <= {"category", "count"}, (
            f"a content notice must carry only a category and a count: {notice!r}"
        )

    with actor_ws(nest_instance, guardian) as ws:
        status_reply = ws.call("fauna.family.status", {})
        notifications = ws.call("fauna.notifications.list", {"limit": 100})["notifications"]
    assert any("family.content_notice" in repr(n) for n in notifications), (
        f"the guardian must also get the content notice as a notification: {notifications!r}"
    )
    for surface, reply in (("family status", status_reply), ("notifications", notifications)):
        assert marker not in repr(reply), (
            f"nothing the child's report carried beyond the category and the count "
            f"may reach the guardian's {surface}: found {marker!r}"
        )
