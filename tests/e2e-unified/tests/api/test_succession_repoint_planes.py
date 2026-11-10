"""What a succession carries across, burns and cuts, plane by plane — full-stack
(tier_3) API E2E.

Witnesses the `take-your-account-back` outcomes owned by
`docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis —
each one ruled and built in the nest and pinned by in-process Rust tests the
feature catalog cannot cite (§ Implementation status today):

* **46** — *"A share link minted before the recovery but never registered
  stops working, and an export link from before is burned."* (the share-link
  serve gate; the grant/token-shaped long tail's `eviction_tokens` burn)
* **47** — *"Your mailing lists, custom feeds and labeler subscriptions stay
  yours, and the anti-abuse meters are not reset."* (the mailing-list plane;
  the curation cluster)
* **48** — *"Your fediverse identity carries on under your handle, while
  bridges to outside networks the account had set up are cut."* (the two
  feature-gated bridge planes)
* **49** — *"The calendars and contacts your standard apps used come with the
  account; their app passwords stop working."* (the MUA-facing bridge plane)

Every test runs the real ceremony (`helpers/succession.py`: a genuine
RecoveryKey and a genuine succession statement the nest verifies against its
own registration chain) and asserts each plane BOTH before it and after it —
an empty list afterwards is only evidence when the same list was full before.

**This module owns its nest.** A succession revokes every session of the old
identity and re-points the account; the session-scoped nest must never see it.

Latency-independent (convention 14): every call is a request with a reply and
every assertion is on returned state. Nothing sleeps and nothing polls.

Process safety: no ``pkill``/``killall``; the nest is started through the
framework's dedicated-nest seam and torn down by this module's fixture.
"""

from __future__ import annotations

import secrets
from dataclasses import dataclass

import pytest
import requests

import fauna_ffi
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from conftest import _make_user
from helpers.private_share import get, text_content
from helpers.succession import register_recovery_kit, succeed_identity
from tests.api.test_share_links_nest_promises import _plaintext_file

pytestmark = pytest.mark.tier_3

FILENAME = "notes.txt"
LIFETIME_S = 3600


@pytest.fixture(scope="module")
def repoint_nest(request, nest_mode, tmp_path_factory):
    """A nest this module owns outright (see the docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "succession-repoint-nest"
    )
    yield nest
    cleanup()


@dataclass
class Successor:
    actor_id_hex: str
    seed: bytes

    def client(self, nest) -> WsRpcAdminClient:
        """A WS-RPC client signing as the successor (its actor id IS its
        Ed25519 pubkey, so the seed the ceremony named is the key)."""
        from nacl.signing import SigningKey

        sk = SigningKey(self.seed)
        assert bytes(sk.verify_key).hex() == self.actor_id_hex, (
            "the successor seed must be the one the statement named"
        )
        return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _succeed(nest, actor) -> Successor:
    """Register a recovery kit for ``actor`` and take the account back."""
    old_seed_hex = bytes(actor["signing_key"]).hex()
    successor_seed = secrets.token_bytes(32)
    kit_secret = register_recovery_kit(
        nest["url"], actor_id_hex=actor["actor_id_hex"], identity_seed_hex=old_seed_hex
    )
    successor_id = succeed_identity(
        nest["url"],
        old_actor_id_hex=actor["actor_id_hex"],
        recovery_secret_hex=kit_secret,
        successor_seed_hex=successor_seed.hex(),
        old_seed_hex=old_seed_hex,
    )
    assert successor_id != actor["actor_id_hex"], "the ceremony must name a new key"
    return Successor(successor_id, successor_seed)


@pytest.mark.feature("take-your-account-back")
def test_an_unregistered_share_link_from_before_the_recovery_stops_working(repoint_nest):
    """Outcome 46, its share-link half: the serve-side succession gate.

    A token the old key signed and nobody ever registered — what a seed thief
    keeps for itself, or a client that crashed short of ``fauna.share.create``
    leaves behind — serves statelessly for a live author, because
    registration is not a serving gate. After the ceremony there is no row to
    move and the author's ``users`` row survives, so only the serve side can
    stop it: ``GET /share/{token}`` consults the author's succession state for
    an unregistered token and answers ``410 Gone``.

    The registered link minted at the same moment is the control on both
    sides of the ceremony: it still serves afterwards (its registry row moved
    to the successor), so the 410 is the gate, not the file, the token shape
    or the ceremony having broken serving altogether.
    """
    nest = repoint_nest
    owner = _make_user(nest)
    secret = bytes(owner["signing_key"])
    _, _, manifest_hash = _plaintext_file(nest, owner, text_content(200))
    manifest_hash = bytes.fromhex(manifest_hash)

    _, registered_url = fauna_ffi.harness_create_link(
        nest["url"], secret, manifest_hash, FILENAME, LIFETIME_S
    )
    kept_url = fauna_ffi.harness_mint_unregistered_link(
        nest["url"], secret, manifest_hash, f"kept-{FILENAME}", LIFETIME_S
    )
    assert kept_url != registered_url, "the two links must be two tokens"
    before = {"registered": get(registered_url)[0], "kept": get(kept_url)[0]}
    assert before == {"registered": 200, "kept": 200}, (
        f"both links must serve before the ceremony — an unregistered token "
        f"serves statelessly for a live author: {before!r}"
    )
    with _ws(nest, owner) as ws:
        listed = ws.call("fauna.share.list", {}).get("shares", [])
    assert len(listed) == 1, f"only the registered link has a registry row: {listed!r}"

    heir = _succeed(nest, owner)

    after = {"registered": get(registered_url)[0], "kept": get(kept_url)[0]}
    assert after == {"registered": 200, "kept": 410}, (
        f"after the ceremony the never-registered token must answer 410 while "
        f"the registered one, whose row moved, still serves: {after!r}"
    )
    with heir.client(nest) as heir_ws:
        heir_listed = heir_ws.call("fauna.share.list", {}).get("shares", [])
    assert [s["token_id"] for s in heir_listed] == [s["token_id"] for s in listed], (
        f"the registered link must list under the successor — its only kill "
        f"switch: {heir_listed!r}"
    )


def _admin_ws(nest) -> WsRpcAdminClient:
    admin_sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(admin_sk.verify_key), signing_key=bytes(admin_sk)
    )


def _pull_export(nest, token: str) -> int:
    """``GET /api/v1/export`` with ``token`` as the bearer — the export door
    takes the eviction export token *instead of* an identity. Returns the
    status (a live token answers a zip, a dead one 401)."""
    resp = requests.get(
        f"{nest['url']}/api/v1/export",
        headers={"Authorization": f"Bearer {token}"},
        timeout=120,
    )
    if resp.status_code == 200:
        assert resp.content[:2] == b"PK", (
            f"a live export token must be handed a zip, got {resp.content[:16]!r}"
        )
    return resp.status_code


@pytest.mark.feature("take-your-account-back")
def test_an_export_link_from_before_the_recovery_is_burned(repoint_nest):
    """Outcome 46, its export-link half: ``eviction_tokens`` BURNS.

    An evicted member's export token is the one credential that opens the
    whole account's archive without an identity, so its value follows the
    corpus: moved to the successor it would aim a thief-readable string at
    everything the successor will ever hold (the grant/token-shaped long
    tail's ruling). The token opens the archive before the ceremony — the
    control that makes the 401 after it the burn, not a dead door — and the
    successor holds no token of its own.
    """
    nest = repoint_nest
    member = _make_user(nest)
    with _admin_ws(nest) as admin:
        admin.call(
            "fauna.admin.users.evict",
            {
                "actor_id": bytes.fromhex(member["actor_id_hex"]),
                "reason": "moving on",
                "category": "capacity",
            },
        )
    with _ws(nest, member) as ws:
        eviction = ws.call("fauna.account.get", {}).get("eviction") or {}
    token = eviction.get("export_token")
    assert token, f"an evicted member must be handed an export token: {eviction!r}"
    assert _pull_export(nest, token) == 200, "the export link must open before the ceremony"

    heir = _succeed(nest, member)

    assert _pull_export(nest, token) == 401, (
        "the export link minted before the recovery must be burned by it"
    )
    with heir.client(nest) as heir_ws:
        heir_eviction = heir_ws.call("fauna.account.get", {}).get("eviction") or {}
    assert not heir_eviction.get("export_token"), (
        f"the successor must hold no export token carried from before: {heir_eviction!r}"
    )


# ── 47: lists, feeds, labeler subscriptions and the meters ──────────────────

LIST_DOMAIN = "lists-succession.test"
#: The per-account daily list-recipient ceiling this module's nest runs under:
#: one send to two members spends it exactly.
DAILY_CEILING = 2


def _set_mass_mailing_policy(nest, **overrides) -> None:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/mass-mailing/policy", json=overrides, timeout=15
    )
    assert resp.status_code == 200, (
        f"mass-mailing policy hook returned {resp.status_code}: {resp.text}"
    )


def _compose(from_addr: str, subject: str) -> bytes:
    lines = [
        f"From: {from_addr}",
        f"To: {from_addr}",
        f"Subject: {subject}",
        f"Message-ID: <{secrets.token_hex(6)}@{LIST_DOMAIN}>",
        "Date: Sun, 14 Jun 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "A list message.",
        "",
    ]
    return ("\r\n".join(lines) + "\r\n").encode()


def _owned_list(ws, list_id) -> dict:
    reply = ws.call("fauna.bridges.list_account_lists", {})
    row = next(
        (r for r in reply.get("lists", []) if bytes(r["list_id"]) == bytes(list_id)), None
    )
    return {"row": row, "account_recipients_today": reply.get("account_recipients_today")}


def _subscribed(ws, labeler_id: bytes) -> bool:
    rows = ws.call("fauna.labelers.list", {})["labelers"]
    return any(bytes(r["labeler_id"]) == labeler_id and r["subscribed"] for r in rows)


@pytest.mark.feature("take-your-account-back")
def test_lists_feeds_and_labeler_subscriptions_stay_yours_and_the_meter_is_not_reset(
    repoint_nest, run_seal_helper
):
    """Outcome 47: the mailing-list plane and the curation cluster MOVE.

    * **The list** — its address moves with the account's aliases and every
      management door is owner-scoped, so a list left behind is a live public
      address nobody can manage. The successor lists it, with its members.
    * **The meter** — ``mail_list_account_daily_counter`` moves because it
      *constrains* the account: a self-service ceremony must not be a reset
      button for a rate cap. The owner spends the day's ceiling before the
      ceremony; the successor reads it spent and its next send is refused.
    * **The feed** — ``fauna.feed.list`` is nest-wide and names each feed's
      owner, so a feed left behind sits in the public catalog under an
      identity nobody can ever remove it from. It lists under the successor.
    * **The labeler subscription** — read at arrival, and the reason a List
      labeler's nest-wide scores are withdrawn at the last unsubscribe. The
      successor is subscribed.
    """
    from helpers.labeler_publish import FIXTURES, publish_wasm_labeler

    nest = repoint_nest
    owner = _make_user(nest)
    with _admin_ws(nest) as admin:
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        admin.call(
            "fauna.bridges.add_local_domain",
            {"domain": LIST_DOMAIN, "mta_sts_cert_mode": "per_host"},
        )
    labeler_id = publish_wasm_labeler(run_seal_helper, nest, FIXTURES / "cat_labeler.wat")
    feed_name = f"succession-feed-{secrets.token_hex(3)}"
    local_part = "news-" + secrets.token_hex(4)
    list_addr = f"{local_part}@{LIST_DOMAIN}"

    _set_mass_mailing_policy(nest, list_recipients_per_account_per_day_ceiling=DAILY_CEILING)
    try:
        with _ws(nest, owner) as ws:
            list_id = ws.call(
                "fauna.bridges.create_account_list",
                {"local_part": local_part, "local_domain": LIST_DOMAIN},
            )["list_id"]
            for _ in range(DAILY_CEILING):
                ws.call(
                    "fauna.bridges.add_list_member",
                    {
                        "list_id": list_id,
                        "recipient_address": f"sub-{secrets.token_hex(4)}@external.test",
                    },
                )
            ws.call(
                "fauna.bridges.send_list_message",
                {"list_id": list_id, "message": _compose(list_addr, "first")},
            )
            feed_id = ws.call(
                "fauna.feed.create", {"name": feed_name, "rules": [], "combination": "all"}
            )["feed_id"]
            ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})

            before_list = _owned_list(ws, list_id)
            before_feed = next(
                f for f in ws.call("fauna.feed.list", {})["feeds"] if f["feed_id"] == feed_id
            )
            before_subscribed = _subscribed(ws, labeler_id)
        assert before_list["row"] is not None, "the list must be the owner's before the ceremony"
        assert before_list["row"]["member_count"] == DAILY_CEILING, before_list
        assert before_list["account_recipients_today"] == DAILY_CEILING, (
            f"the send must have spent the day's ceiling: {before_list!r}"
        )
        assert before_feed["owner"] == owner["actor_id_hex"], before_feed
        assert before_subscribed, "the owner must be subscribed before the ceremony"

        heir = _succeed(nest, owner)

        with heir.client(nest) as heir_ws:
            after_list = _owned_list(heir_ws, list_id)
            after_feeds = [
                f for f in heir_ws.call("fauna.feed.list", {})["feeds"] if f["feed_id"] == feed_id
            ]
            after_subscribed = _subscribed(heir_ws, labeler_id)
            with pytest.raises(RpcCallError) as capped:
                heir_ws.call(
                    "fauna.bridges.send_list_message",
                    {"list_id": list_id, "message": _compose(list_addr, "second")},
                )
    finally:
        _set_mass_mailing_policy(nest)

    assert after_list["row"] is not None, (
        f"the successor must manage the list the account owned: {after_list!r}"
    )
    assert bytes(after_list["row"]["owner_actor_id"]).hex() == heir.actor_id_hex, after_list
    assert after_list["row"]["member_count"] == DAILY_CEILING, after_list
    assert after_list["account_recipients_today"] == DAILY_CEILING, (
        f"the ceremony must not reset the account's daily list meter: {after_list!r}"
    )
    assert capped.value.code == "fauna.bridges.list_daily_cap_exceeded", capped.value.code
    assert [f["owner"] for f in after_feeds] == [heir.actor_id_hex], (
        f"the feed must list under the successor: {after_feeds!r}"
    )
    assert after_subscribed, "the labeler subscription must be the successor's"


# ── 49: the MUA-facing plane ────────────────────────────────────────────────

DAV_DOMAIN = "dav-succession.test"


def _spawn_dav_bridge(nest, mail_bridge_binary, tmp_path_factory):
    """A fresh real Go MDA (IMAP + CalDAV/CardDAV) against ``nest``.

    Fresh per phase on purpose: the DAV door caches each login's material for
    a short while (`davauth/auth_material_cache.go`), so the bridge that
    served the old password before the ceremony could still accept it after
    — a stale cache would make a refusal unobservable, and is not the
    property under test. A new bridge reads the nest as it is now."""
    from conftest import _spawn_mda_bridge

    return _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=nest,
        tmp=tmp_path_factory.mktemp("succession-dav"),
        bridge_id=f"succession-dav-{secrets.token_hex(3)}",
        domain=DAV_DOMAIN,
    )


def _dav_lists(ws, actor_id: bytes) -> tuple[list, list]:
    """The owner's calendar and address-book ids, through the nest's own
    owner-scoped list doors (the ones the bridge serves a PROPFIND from)."""
    cals = ws.call("fauna.bridges.list_calendars", {"actor_id": actor_id})["calendars"]
    books = ws.call("fauna.bridges.list_addressbooks", {"actor_id": actor_id})["addressbooks"]
    return (
        sorted(bytes(c["calendar_id"]).hex() for c in cals),
        sorted(bytes(b["addressbook_id"]).hex() for b in books),
    )


def _dav_status(port: int, username: str, password: str) -> int:
    """One authenticated PROPFIND at the DAV root; its status (401 refused)."""
    resp = requests.request(
        "PROPFIND",
        f"https://127.0.0.1:{port}/",
        auth=(username, password),
        headers={"Depth": "0"},
        verify=False,
        timeout=30,
    )
    return resp.status_code


@pytest.mark.feature("take-your-account-back")
def test_calendars_and_contacts_come_with_the_account_and_their_app_passwords_stop_working(
    repoint_nest, mail_bridge_binary, seal_helper_binary, tmp_path_factory
):
    """Outcome 49: the IMAP/CalDAV/CardDAV collection plane MOVES; the
    MSEK-derived app-password wraps BURN.

    A move of the wraps would let whatever mail password the thief set open
    the successor's mail, so the successor's calendar app signs in again with
    a new app password. The collections themselves are the user's data and
    cross whole.

    Before the ceremony a real calendar app (the Go MDA over HTTPS) signs in
    with the old app password and creates a calendar and the lazy Contacts
    book. After it, a fresh bridge refuses the old password (401) and admits
    the successor's new one, and the nest lists the same calendar and address
    book ids under the successor. The collections are asserted on ids, not
    names: their metadata stays sealed to the key it was written under.
    """
    from conftest import _provision_msek_recipient
    from helpers.caldav_client import CalDAVClient
    from helpers.carddav_client import CardDAVClient
    from nacl.signing import SigningKey

    nest = repoint_nest
    with _admin_ws(nest) as admin:
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        admin.call(
            "fauna.bridges.add_local_domain",
            {"domain": DAV_DOMAIN, "mta_sts_cert_mode": "per_host"},
        )
    local = f"dav-{secrets.token_hex(4)}"
    old_password = f"{local}-old-password"
    prov = _provision_msek_recipient(
        nest_instance=nest,
        seal_helper_binary=seal_helper_binary,
        domain=DAV_DOMAIN,
        local_part=local,
        password=old_password,
    )
    owner = prov.recipient
    username = prov.username

    bridge = _spawn_dav_bridge(nest, mail_bridge_binary, tmp_path_factory)
    try:
        base = f"https://127.0.0.1:{bridge.caldav_port}"
        caldav = CalDAVClient(base, username, old_password, verify=False)
        caldav.wait_until_serving()
        caldav.mkcalendar(f"work-{secrets.token_hex(3)}", displayname="Work")
        CardDAVClient(base, username, old_password, verify=False).contacts_addressbook()
        old_login_before = _dav_status(bridge.caldav_port, username, old_password)
    finally:
        bridge.cleanup()
    with _ws(nest, owner) as ws:
        cals_before, books_before = _dav_lists(ws, owner["actor_id_bytes"])
    assert old_login_before == 207, (
        f"the old app password must sign in before the ceremony: {old_login_before}"
    )
    assert len(cals_before) >= 2, (
        f"the lazy Personal calendar and the one made must exist: {cals_before!r}"
    )
    assert books_before, "the lazy Contacts book must exist before the ceremony"

    heir = _succeed(nest, owner)

    heir_sk = SigningKey(heir.seed)
    new_password = f"{local}-new-password"
    _provision_msek_recipient(
        nest_instance=nest,
        seal_helper_binary=seal_helper_binary,
        # The address moved with the account: no alias to add, only the
        # successor's own seal key and app password.
        domain=None,
        local_part=local,
        password=new_password,
        actor={
            "actor_id_bytes": bytes(heir_sk.verify_key),
            "actor_id_hex": heir.actor_id_hex,
            "signing_key": heir_sk,
        },
    )
    bridge = _spawn_dav_bridge(nest, mail_bridge_binary, tmp_path_factory)
    try:
        CalDAVClient(
            f"https://127.0.0.1:{bridge.caldav_port}", username, new_password, verify=False
        ).wait_until_serving()
        logins = {
            "old": _dav_status(bridge.caldav_port, username, old_password),
            "new": _dav_status(bridge.caldav_port, username, new_password),
        }
    finally:
        bridge.cleanup()
    with heir.client(nest) as heir_ws:
        cals_after, books_after = _dav_lists(heir_ws, bytes(heir_sk.verify_key))

    assert logins == {"old": 401, "new": 207}, (
        f"after the ceremony the old app password must be refused and the "
        f"successor's new one admitted: {logins!r}"
    )
    assert (cals_after, books_after) == (cals_before, books_before), (
        f"every calendar and address book must list under the successor: "
        f"before {(cals_before, books_before)!r}, after {(cals_after, books_after)!r}"
    )
