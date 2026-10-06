"""What the home nest enforces after a succession — full-stack (tier_3) API E2E.

Witnesses the `take-your-account-back` outcomes that are the *nest's* half of
`docs/goal/behavior/identity-succession.md` § Enforcement on the home nest and
`docs/goal/behavior/succession-aftermath.md` § Re-key scope:

* **13** — *"Your account comes back under the new key at the same handle,
  keeping your tier and any admin role."* (§ Enforcement, step 2)
* **19** — *"Messages and posts still signed with the stolen key are refused,
  so whoever holds it can no longer send anything as you."* (step 4)
* **20** — *"Someone who only knows your old key, or your handle, is pointed at
  the account's new key."* (step 5)
* **25** — *"Apps you had authorized to act as you are disconnected by the
  recovery and must be authorized again."* (§ Re-key scope, the Nostr row)

Every one of them was implemented and pinned only by in-process Rust tests the
feature catalog cannot cite; `tests/api/test_succession_fixture.py` asserted
that a succession *lands* and nothing about what it then enforces.

**The ceremony is real throughout.** `helpers/succession.py` + the Rust
`recovery_fixture` helper register a genuine RecoveryKey and sign a genuine
succession statement, which the nest verifies against its own stored
registration chain — a fabricated row would be refused at submit, so reaching
any assertion below means the nest applied this statement.

**This module owns its nest.** A succession revokes every session of the old
identity and re-points the account, and outcome 13 drives a pending action
through the nest-wide `pending_actions/run_due` hook; both would break the
session-scoped nest for every other test.

Latency-independent (convention 14): every call is a request with a reply, and
every assertion is on returned state. Nothing sleeps and nothing polls.

Process safety: no ``pkill``/``killall``; the nest is started through the
framework's dedicated-nest seam and torn down by this module's fixture.
"""

from __future__ import annotations

import secrets

import pytest
import requests

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from clients.ws_rpc_federation_client import FederationChannelClient
from common import build_email_inbox_payload
from common.auth import create_actor_and_register
from helpers.succession import (
    register_recovery_kit,
    succeed_identity,
    succession_statements,
)

pytestmark = pytest.mark.tier_3

#: `rpc_errors::invalid_params_ns("federation", …)` — how a `BadPayload`
#: rejection of a relayed inbox payload surfaces to the peer nest.
FEDERATION_INVALID_PARAMS = "fauna.federation.invalid_params"
#: The refusal `deliver_inbox_payload_core`'s supersession consult produces.
SUPERSEDED_TEXT = "sender identity has been superseded"


@pytest.fixture(scope="module")
def succession_nest(request, nest_mode, tmp_path_factory):
    """A nest this module owns outright (see the docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-enforcement-nest"
    )
    yield nest
    cleanup()


@pytest.fixture()
def admin_succession_nest(request, nest_mode, tmp_path_factory):
    """A nest whose own claimed ADMIN is the identity that gets succeeded.

    Its own nest, function-scoped, because the ceremony retires the very key
    the harness registers users with — nothing else may still need it.

    Succeeding the claimed admin rather than promoting a second actor is not a
    shortcut, it is the only door: `admin.add` is a quorum-1 pending action and
    the nest refuses self-approval (`fauna.pending_actions.permission_denied`,
    "self-approval is not allowed"), so a nest with one admin cannot mint a
    second. The claimed admin already carries all three things outcome 13 is
    about — a handle (`claim_admin` requires one), a tier, and the role.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "admin-succession-nest"
    )
    yield nest
    cleanup()


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _admin_ws(nest) -> WsRpcAdminClient:
    admin_sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    )


def _fresh_actor(nest) -> dict:
    """A registered actor with a handle, nobody else's test shares."""
    return create_actor_and_register(
        nest["port"],
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )


def _succeed(nest, actor) -> str:
    """Register a kit for `actor` and take the account back. Returns the
    successor's actor id (hex)."""
    seed_hex = bytes(actor["signing_key"]).hex()
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=actor["actor_id_hex"], identity_seed_hex=seed_hex
    )
    return succeed_identity(
        nest["url"],
        old_actor_id_hex=actor["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        old_seed_hex=seed_hex,
    )


def _successor_client(nest, successor_id_hex: str, successor_seed_hex: str):
    """A WS-RPC client signing as the successor. The successor's actor id IS
    its Ed25519 pubkey, so the seed the ceremony was given is the key."""
    from nacl.signing import SigningKey

    sk = SigningKey(bytes.fromhex(successor_seed_hex))
    assert bytes(sk.verify_key).hex() == successor_id_hex, (
        "the successor seed must be the one the statement named"
    )
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    )


def _run_due(nest) -> dict:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/pending_actions/run_due", json={}, timeout=60
    )
    assert resp.status_code == 200, (
        f"test-hooks pending_actions run_due returned {resp.status_code}: {resp.text}"
    )
    return resp.json()


@pytest.mark.feature("take-your-account-back")
def test_the_account_comes_back_at_the_same_handle_with_tier_and_admin_role(
    admin_succession_nest,
):
    """Outcome 13: handle, tier and admin role all cross the ceremony.

    All three are asserted, because the succession transaction writes them
    three different ways and each has its own failure mode: the handle through
    a **two-step move** (the old row must release the UNIQUE handle before the
    new row can take it), the tier as an inherited column on the successor's
    new `users` row, and the admin role as a delete+insert that must carry the
    ROLE with it (naming only `(actor_id, added_at)` would let the column
    default silently promote a moderator to superadmin).

    The admin half is asserted through the **door**, not the table: the
    successor is served an Admin-class kind — indeed every read below is made
    *by the successor*, since the old key's sessions are revoked and its
    handshake refused — while a control actor who never held the role is
    refused. A row-level check would pass just as happily against a role the
    permission layer does not honour.
    """
    succession_nest = admin_succession_nest
    admin_sk = succession_nest["admin"]["signing_key"]
    user = {
        "signing_key": admin_sk,
        "actor_id_hex": bytes(admin_sk.verify_key).hex(),
        "actor_id_bytes": bytes(admin_sk.verify_key),
        "handle": succession_nest["admin"].get("handle") or "admin",
    }
    handle = user["handle"]
    # The control actor is registered BEFORE the ceremony: registering needs
    # the admin key, which this test is about to retire.
    stranger = _fresh_actor(succession_nest)
    # `users.tier` carries a foreign key onto `tiers`, so the tier this test
    # moves has to be a real one — minted here rather than borrowing whatever
    # the deployment happens to ship, so the assertion cannot pass by the
    # successor merely inheriting the default.
    tier = f"pro-{secrets.token_hex(3)}"

    with _admin_ws(succession_nest) as admin_ws:
        admin_ws.call(
            "fauna.admin.tiers.create",
            {
                "name": tier,
                "max_inbox_bytes": 1 << 30,
                "max_storage_bytes": 1 << 30,
                "max_devices": 8,
                "max_blob_size": 1 << 24,
                "max_feeds": 16,
            },
        )
        admin_ws.call(
            "fauna.admin.users.update",
            {"actor_id": user["actor_id_bytes"], "tier": tier, "label": "before"},
        )
        roster = admin_ws.call("fauna.admin.admins.list", {})
    assert any(
        bytes(a["actor_id"]).hex() == user["actor_id_hex"] for a in roster["admins"]
    ), (
        f"the subject must hold the admin role before the ceremony, or the "
        f"move this test asserts has nothing to move: {roster!r}"
    )

    successor_seed = secrets.token_bytes(32).hex()
    kit_secret = register_recovery_kit(
        succession_nest["url"],
        actor_id_hex=user["actor_id_hex"],
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )
    successor_id = succeed_identity(
        succession_nest["url"],
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=bytes(user["signing_key"]).hex(),
    )

    # Every read below is the SUCCESSOR's: the old key's sessions are revoked
    # and its handshake refused, so being served these Admin-class kinds at all
    # is the role assertion, made before the row assertions it carries.
    with _successor_client(succession_nest, successor_id, successor_seed) as heir_ws:
        new_row = heir_ws.call(
            "fauna.admin.users.get", {"actor_id": bytes.fromhex(successor_id)}
        )["user"]
        old_row = heir_ws.call(
            "fauna.admin.users.get", {"actor_id": user["actor_id_bytes"]}
        )["user"]
        heir_roster = heir_ws.call("fauna.admin.admins.list", {})

    assert new_row["handle"] == handle, (
        f"the account must come back at the SAME handle; successor carries "
        f"{new_row.get('handle')!r}, want {handle!r}"
    )
    assert new_row["tier"] == tier, (
        f"the tier must ride along — it is the account's quota, not the key's; "
        f"successor carries {new_row['tier']!r}, want {tier!r}"
    )
    assert not old_row.get("handle"), (
        "the retired identity must RELEASE the handle — the UNIQUE index means "
        "both rows holding it is not even expressible, so a successor that "
        f"appears to have it while this row still does is a read error: {old_row!r}"
    )

    assert any(
        bytes(a["actor_id"]).hex() == successor_id for a in heir_roster["admins"]
    ), f"the successor must hold the moved admin role: {heir_roster!r}"
    assert not any(
        bytes(a["actor_id"]).hex() == user["actor_id_hex"] for a in heir_roster["admins"]
    ), (
        f"the retired identity must not still be on the roster — the role "
        f"MOVES, it is not copied: {heir_roster!r}"
    )

    # Without this the successor being served proves only that the kind is
    # open, not that the role is what opened it.
    with _ws(succession_nest, stranger) as stranger_ws:
        with pytest.raises(RpcCallError):
            stranger_ws.call("fauna.admin.admins.list", {})


@pytest.mark.feature("take-your-account-back")
def test_an_old_key_or_handle_is_pointed_at_the_successor(succession_nest):
    """Outcome 20: both discovery doors resolve forward, pre-identity.

    Both are deliberately **pre-identity** kinds: peers hold OLD actor ids and
    a party catching up holds no account on this nest, so discovery that
    required a session would be discovery that never runs
    (`pre_identity_allowlist.rs:124-128`).

    The never-succeeded control is the half that makes the lookup honest: an
    empty answer must mean *this identity was never succeeded*, not *the nest
    withheld it*, and a lookup that errored on the ordinary case could not be
    told from one that errored on a withheld one.
    """
    user = _fresh_actor(succession_nest)
    handle = user["handle"]
    successor_id = _succeed(succession_nest, user)

    statements = succession_statements(succession_nest["url"], user["actor_id_hex"])
    assert len(statements) == 1, (
        f"the nest must serve the succession back to anyone holding the OLD "
        f"actor id, got {len(statements)} statement(s)"
    )
    assert statements[0], "a served statement must be the verbatim bytes, not empty"

    with WsRpcAnonClient(succession_nest["url"]) as anon:
        resolved = anon.call("fauna.actor.by_handle", {"handle": handle})
    assert resolved["actor_id"] == successor_id, (
        f"the handle must resolve to the account's NEW key; got "
        f"{resolved['actor_id']}, want {successor_id}"
    )

    never = _fresh_actor(succession_nest)
    assert succession_statements(succession_nest["url"], never["actor_id_hex"]) == [], (
        "an identity that was never succeeded must answer with an EMPTY chain "
        "rather than an error — that is what lets a consumer tell 'never "
        "succeeded' from 'the nest withheld it'"
    )


@pytest.mark.feature("take-your-account-back")
def test_content_still_signed_with_the_superseded_key_is_refused(
    succession_nest, second_nest
):
    """Outcome 19: the arrival is refused on the door with no authenticated caller.

    This is the path that needs the check most, and the one a self-describing
    verification cannot cover: `fauna.federation.inbox.deliver` carries no
    authenticated sender at all — a peer nest relays bytes, and the payload's
    own signatures establish only that the *signature* is good. A thief holding
    the stolen key still produces perfectly valid signatures forever, so the
    refusal is the succession-table consult and nothing else
    (`routes.rs:1319-1343`).

    The old key's *authenticated* doors are shut by a different mechanism
    (step 3 revokes every session, step 4 refuses the handshake), which is why
    this test drives the unauthenticated one rather than `fauna.inbox.send`.

    Two-way: an identically-shaped payload from a sender who was never
    succeeded, relayed by the same peer on the same channel in the same test,
    must land — otherwise "refused" is indistinguishable from a relay that
    delivers nothing.
    """
    recipient = _fresh_actor(succession_nest)
    with _ws(succession_nest, recipient) as recipient_ws:
        recipient_ws.call("fauna.inbox.mode.set", {"mode": "open"})

    thief_held = _fresh_actor(succession_nest)
    innocent = _fresh_actor(succession_nest)

    # Compose BOTH payloads before the ceremony: the bytes are identical in
    # shape and differ only in which key signed them.
    superseded_payload, _ = build_email_inbox_payload(
        thief_held["signing_key"],
        recipient["actor_id_hex"],
        "still signed with the old key",
        "body",
        node_url=succession_nest["url"],
    )
    innocent_payload, _ = build_email_inbox_payload(
        innocent["signing_key"],
        recipient["actor_id_hex"],
        "signed with a key that was never succeeded",
        "body",
        node_url=succession_nest["url"],
    )

    _succeed(succession_nest, thief_held)

    def relay(payload: bytes):
        with FederationChannelClient(
            initiator=second_nest, target=succession_nest
        ) as fed:
            return fed.call(
                "fauna.federation.inbox.deliver",
                {
                    "recipient_actor_id": recipient["actor_id_hex"],
                    "payload_bytes": payload,
                },
            )

    with pytest.raises(RpcCallError) as refusal:
        relay(superseded_payload)
    assert FEDERATION_INVALID_PARAMS in str(refusal.value), (
        f"content authored by a superseded key must be refused with "
        f"{FEDERATION_INVALID_PARAMS}, got {refusal.value!r}"
    )
    assert SUPERSEDED_TEXT in str(refusal.value), (
        f"the refusal must name the supersession — any other reason would mean "
        f"some unrelated gate stopped this, and the consult is untested: "
        f"{refusal.value!r}"
    )

    reply = relay(innocent_payload)
    assert reply["inbox_id"] >= 1, (
        f"the same relay, the same recipient, a key that was never succeeded — "
        f"this must land, or the refusal above proves nothing: {reply!r}"
    )

    with _ws(succession_nest, recipient) as recipient_ws:
        items = recipient_ws.call("fauna.inbox.fetch", {"limit": 0}).get("items", [])
    assert len(items) == 1, (
        f"exactly the innocent sender's message may be in the inbox — the "
        f"superseded key's must have landed nowhere: {len(items)} item(s)"
    )


@pytest.mark.feature("take-your-account-back")
def test_the_recovery_disconnects_the_apps_you_had_authorized(succession_nest):
    """Outcome 25: bunker connections and zap-signer designations both die.

    Two different succession shapes, asserted together because the outcome is
    one sentence about both: `nostr_bunker_apps` **moves and revokes** in one
    statement (`actor_tables.rs`), while `nostr_zap_signers` **burns**
    outright. Either way the successor holds neither, which is the promise —
    "must be authorized again".

    Both are standing third-party authority rather than user data, which is
    why they die while the npub itself survives: the deposited key rests under
    the *nest* identity key and crosses no wire, so a seed thief gained use,
    never possession (`docs/goal/ui/nostr.md` § Key succession and rotation).

    The pre-ceremony assertions are not scaffolding — without them an empty
    list afterwards would be equally consistent with the designations never
    having been made.
    """
    user = _fresh_actor(succession_nest)
    signer_pubkey = secrets.token_bytes(32).hex()

    with _ws(succession_nest, user) as user_ws:
        # A deposited key is the precondition for a bunker invite: custodial
        # signing is the only mode that can answer NIP-46 requests at all.
        user_ws.call(
            "fauna.bridges.link",
            {"bridge_id": "nostr", "mode": "generate", "params": {}},
        )
        invite = user_ws.call("fauna.nostr.bunker.create_invite", {})
        assert invite["connection_id"] >= 1, invite
        user_ws.call(
            "fauna.nostr.zap_signers.add",
            {"signer_pubkey": signer_pubkey, "label": "a wallet provider"},
        )

        apps_before = user_ws.call("fauna.nostr.bunker.list", {})["apps"]
        signers_before = user_ws.call("fauna.nostr.zap_signers.list", {})["signers"]
    assert len(apps_before) == 1, (
        f"the connected app must be on the roster before the ceremony: {apps_before!r}"
    )
    assert [s["signer_pubkey"] for s in signers_before] == [signer_pubkey], (
        f"the zap-signer designation must be on the roster before the "
        f"ceremony: {signers_before!r}"
    )

    successor_seed = secrets.token_bytes(32).hex()
    kit_secret = register_recovery_kit(
        succession_nest["url"],
        actor_id_hex=user["actor_id_hex"],
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )
    successor_id = succeed_identity(
        succession_nest["url"],
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=bytes(user["signing_key"]).hex(),
    )

    with _successor_client(succession_nest, successor_id, successor_seed) as heir_ws:
        apps_after = heir_ws.call("fauna.nostr.bunker.list", {})["apps"]
        signers_after = heir_ws.call("fauna.nostr.zap_signers.list", {})["signers"]

    assert apps_after == [], (
        f"every bunker connection must be disconnected by the recovery — a "
        f"thief-minted one signs as the npub silently, forever: {apps_after!r}"
    )
    assert signers_after == [], (
        f"every zap-signer designation must die with them: this nest BELIEVES "
        f"receipts a designated signer mints for the user's pubkey, and a "
        f"believed receipt meeting a tier's price buys the post: {signers_after!r}"
    )


def test_the_subscription_tier_plane_moves_with_the_account(succession_nest):
    """The tier plane re-points: tiers, the subscriber roster, the entitlement.

    ⚠ **Still deliberately carries no `feature` marker, for a reason that
    CHANGED on 2026-09-22 — read it before adding one.** That outcome is two
    claims —
    *"Your subscription tiers, your subscribers and the posts they paid for
    come with the account, **and what you publish afterwards cannot be read by
    whoever held the old key**"*. This test witnesses the **move** and says
    nothing whatever about the key: the ceremony deliberately carries the
    tier's key material across (those keys seal the author's back catalogue, so
    stranding them would be user-irrecoverable), which means a green run here
    is equally consistent with the successor publishing under the exact key a
    seed thief read out of the account planes.

    The sealing half is the client-side aftermath leg, built 2026-09-22 and
    witnessed by
    `tests/e2e-unified/tests/test_identity_succession_aftermath.py::test_the_successors_subscriber_tier_is_re_keyed`,
    which unwraps the tier's live `KeyBlob` before and after the ceremony and
    compares the keys inside. **That witness moved outcome 24 from `[nest]` to
    `[app]`, which is what keeps this test uncited** — the rotation is the
    successor's CLIENT's act and cannot be proven app-independently at all, so
    an app-surface outcome may not cite an app-independent test (feature-
    catalog.md § The two surfaces; the lint's rule 4). The change is a
    tightening, not a softening: the outcome's words are untouched and it now
    has to be witnessed per column instead of counting for all seven at once.
    The nest holds no period key and rotates none, so it is not the sealing
    half either.

    **Never make this test outcome 24's witness.** Its own scope is API-tier
    and key-blind by construction: an API-only harness cannot approve a
    subscription on a client-minted tier at all (`requests.approve` refuses
    without a client-minted `encrypted_upload`), which is why the roster it
    follows is a *pending* request rather than a granted one. It stays for what
    it does prove — the move half, real, user-visible and otherwise unwitnessed
    end to end, plus the retired key's refusal at the handshake below.
    """
    author = _fresh_actor(succession_nest)
    reader = _fresh_actor(succession_nest)

    # `followers` is the one tier the nest provisions itself, on the first
    # follow — every other tier needs the author's own client to mint its key
    # material, which no API-only test can stand in for.
    with _ws(succession_nest, reader) as reader_ws:
        reader_ws.call(
            "fauna.subscriptions.subscribe",
            {"author_id": bytes(author["actor_id_bytes"]), "tier": "followers"},
        )

    # The encrypted-mode path always ENQUEUES, and approving is the author's
    # CLIENT's job: `requests.approve` refuses without an `encrypted_upload`
    # ("this tier's key is client-minted, so the nest cannot wrap it"), which
    # is the same no-nest-held-key fact the docstring above turns on. So what
    # an API-only harness can follow across the ceremony is the tier row and
    # the pending request — both real, both user-visible, neither needing key
    # material.
    with _ws(succession_nest, author) as author_ws:
        tiers_before = author_ws.call("fauna.subscriptions.tiers.list", {})["tiers"]
        pending_before = author_ws.call("fauna.subscriptions.requests.list", {})[
            "requests"
        ]
    assert any(t["name"] == "followers" for t in tiers_before), tiers_before
    assert any(
        bytes(r["subscriber_id"]).hex() == reader["actor_id_hex"]
        for r in pending_before
    ), f"the follow must be pending on the author before the ceremony: {pending_before!r}"

    successor_seed = secrets.token_bytes(32).hex()
    kit_secret = register_recovery_kit(
        succession_nest["url"],
        actor_id_hex=author["actor_id_hex"],
        identity_seed_hex=bytes(author["signing_key"]).hex(),
    )
    successor_id = succeed_identity(
        succession_nest["url"],
        old_actor_id_hex=author["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed,
        old_seed_hex=bytes(author["signing_key"]).hex(),
    )

    with _successor_client(succession_nest, successor_id, successor_seed) as heir_ws:
        tiers_after = heir_ws.call("fauna.subscriptions.tiers.list", {})["tiers"]
        pending_after = heir_ws.call("fauna.subscriptions.requests.list", {})[
            "requests"
        ]

    assert any(t["name"] == "followers" for t in tiers_after), (
        f"the author's own tiers must come with the account — left behind, the "
        f"plane freezes under an identity that can never serve, rotate or "
        f"revoke any of it: {tiers_after!r}"
    )
    assert any(
        bytes(r["subscriber_id"]).hex() == reader["actor_id_hex"]
        for r in pending_after
    ), (
        f"a pending subscribe request must move with the tier it names — left "
        f"behind it is dead for the reader (the redemption path's tier "
        f"pre-check fails) and invisible to the successor, whose own list is "
        f"owner-keyed: {pending_after!r}"
    )

    # The retired identity cannot even reach the plane any more — and the
    # refusal comes at the HANDSHAKE, not at the call: step 4 refuses
    # `fauna.auth.handshake` outright and the refusal names where to fetch the
    # statement, so a peer holding only the old key is pointed forward rather
    # than left guessing (outcome 20's mechanism, observed here from the
    # credential side).
    with pytest.raises(RpcCallError) as refused:
        with _ws(succession_nest, author) as old_ws:
            old_ws.call("fauna.subscriptions.tiers.list", {})
    assert "superseded" in str(refused.value), refused.value
    assert successor_id in str(refused.value), (
        f"the refusal must name the successor, which is what makes it a "
        f"redirection rather than a dead end: {refused.value!r}"
    )
