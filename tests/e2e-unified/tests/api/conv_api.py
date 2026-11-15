"""WS-RPC seeding/query helpers for the conversations (MLS) surface.

The local-user MLS-channel HTTP twins (``POST /api/v1/channel/{id}``,
``GET /api/v1/channel/{id}``, ``POST /api/v1/keypackage/{actor_id}``,
``GET /api/v1/keypackage/{actor_id}/count``) were deleted by the WS-RPC
conversations migration (T8) and
replaced with the ``fauna.conversations.*`` kinds. These helpers drive the
replacement kinds over the canonical WS-RPC wire so API-tier tests keep
exercising the real nest channel-ingest / keypackage path.

The same-nest KP-fetch / Welcome-deliver planes are
``fauna.conversations.keypackage.fetch`` / ``fauna.conversations.welcome.deliver``
(``keypackage_fetch`` / ``welcome_deliver`` below). Their HTTP twins
(``GET /api/v1/keypackage/{actor_id}`` / ``POST /api/v1/welcome/{actor_id}``)
were deleted in the WS-RPC-everywhere rip-out — the surviving cross-nest plane
is the federation channel (``fauna.federation.{keypackage.fetch,welcome.deliver}``,
driven by ``clients.ws_rpc_federation_client``), not an HTTP route. The inbox
**read** also rides WS-RPC now — ``fauna.inbox.fetch`` (a pure peek; see
``inbox``) — its ``GET /api/v1/inbox/{actor_id}`` drain was deleted in the same
rip.

Wire contracts (verified against
``bins/fauna-nest/src/conversations_handlers.rs`` +
``libs/fauna-protocol/src/conversations.rs``, 2026-05-24):

* ``fauna.conversations.channel.send`` — ``{"channel_id": hex, "envelope":
  <bytes>}`` → ``{"seq": i64}``. Auto-registers the sender on the channel.
  ``envelope`` rides as a CBOR ``bstr`` (raw bytes, not hex).
* ``fauna.conversations.channel.fetch`` — ``{"channel_id": hex, "after":
  i64, "limit"?: i64}`` → ``{"messages": [{"seq": i64, "envelope":
  <bytes>}]}``. Returns messages with ``seq > after``; auto-registers the
  reader. ``envelope`` comes back as raw ``bytes``.
* ``fauna.conversations.keypackage.upload`` — ``{"packages": [<bytes>,
  ...]}`` → ``{"stored": u64}``. Self-upload (caller is implicit); each
  package is a CBOR ``bstr``.
* ``fauna.conversations.keypackage.count`` — ``{"actor_id": hex}`` →
  ``{"count": u64}``. Non-destructive.

Caller class: all ``fauna.conversations.*`` kinds are ``User``-class
(``bridge_method_allowlist.rs``); pass an actor created via
``common.auth.create_actor_and_register(port, admin_signing_key=...)``.

Connection reuse mirrors ``ws_api.py``: one open ``WsRpcAdminClient`` per
``(base_url, actor_id)`` cached and closed at process exit.
"""

from __future__ import annotations

import atexit
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import port_base_url
from common.nest import register_stop_listener


# (base_url, actor_id_hex) → open client. The WsRpcAdminClient is the generic
# WS-RPC client (the "Admin" name is historical); a User-class actor's keypair
# makes it a User-class client, which is what the conversations kinds require.
_CLIENTS: dict[tuple[str, str], WsRpcAdminClient] = {}


def _base_url(port: int, scheme: str | None = None) -> str:
    """Base URL for a nest ``port``, scheme READ from the one place the
    port→scheme fact lives (``common.auth.port_base_url``) rather than spelled
    here — the same shape ``ws_api._base_url`` already has.

    This defaulted to a literal ``"http"`` until 2026-08-30, which made every
    caller responsible for remembering ``scheme="https"`` against a TLS-serving
    nest. A forgotten keyword does not fail loudly: the client dials ``ws://`` at
    a TLS listener, which drops the connection with no HTTP response — below any
    application logging, so the nest's log stays clean — and the test sees a bare
    ``WebSocketConnectionClosedException: Connection to remote host was lost.``
    raised out of the WS handshake, naming nothing. That is exactly how
    ``test_custody_ceremony_two_accounts`` and
    ``test_fauna_mls_two_client_inbox_drain`` ERRORed at setup under
    ``--nest docker`` while passing in standalone: their shared fixture calls
    ``accept_contact(nest_instance["port"], ...)`` with no ``scheme=``, and the
    docker image serves TLS. Pinned by
    ``test_nest_mode_axis.py::test_every_port_keyed_api_helper_resolves_its_scheme_from_the_one_place``.

    ``scheme`` stays available for the cross-nest callers that dial a FOREIGN
    nest's port explicitly; passing it pins the scheme instead of deriving it.
    """
    if scheme is None:
        return port_base_url(port)
    return f"{scheme}://127.0.0.1:{port}"


def _client_for(base_url: str, actor: dict) -> WsRpcAdminClient:
    """Return an open, cached WS-RPC client for ``actor`` against ``base_url``.

    ``actor`` is a ``common.auth.create_actor_and_register`` dict — it carries
    ``actor_id_bytes`` (32-byte Ed25519 pubkey) and ``signing_key`` (PyNaCl
    ``SigningKey``).

    ⚠ Returns an OPEN client — see the twin of this cache in
    ``tests/api/ws_api.py`` for why that needs checking rather than assuming
    (a `_reconnect()` whose `_connect()` raised leaves a dead client cached).
    """
    actor_id_bytes = actor["actor_id_bytes"]
    key = (base_url.rstrip("/"), actor_id_bytes.hex())
    client = _CLIENTS.get(key)
    if client is None:
        client = WsRpcAdminClient(
            base_url,
            actor_id=actor_id_bytes,
            signing_key=bytes(actor["signing_key"]),
        )
        client.__enter__()
        _CLIENTS[key] = client
        return client
    try:
        return client.ensure_open()
    except Exception:
        _CLIENTS.pop(key, None)
        raise


def close_all_ws() -> None:
    """Close every cached WS-RPC connection. Safe to call repeatedly."""
    for client in list(_CLIENTS.values()):
        try:
            client.close()
        except Exception:
            pass
    _CLIENTS.clear()



def forget_port(port: int) -> None:
    """Close and drop every cached connection to the nest on ``port`` — called by
    ``common.nest.stop_nest`` when that nest goes down, so a restart never hands
    the next caller a socket to the process that is gone."""
    suffix = f":{port}"
    for key in [k for k in _CLIENTS if k[0].endswith(suffix)]:
        client = _CLIENTS.pop(key)
        try:
            client.close()
        except Exception:
            pass


atexit.register(close_all_ws)
register_stop_listener(forget_port)


# ── channels ─────────────────────────────────────────────────────────────────


def channel_send(
    port: int, actor: dict, channel_id_hex: str, envelope: bytes, *, scheme: str | None = None
) -> int:
    """``fauna.conversations.channel.send`` — returns the assigned ``seq``.

    Replacement for the old ``POST /api/v1/channel/{id}`` (which returned
    ``{"seq": ...}`` with a 201). The sender is auto-registered on the channel.

    ``scheme``: pass ``"https"`` when ``port`` belongs to a ``serve_tls=True``
    nest (e.g. ``cross_nest_foreign``/``caldav_cross_nest_peer``) — it serves
    ONLY https, so the default plain-http base would fail to connect.
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.channel.send",
        {"channel_id": channel_id_hex, "envelope": bytes(envelope)},
    )
    return reply["seq"]


def room_list_roster(
    port: int, actor: dict, room_id_hex: str, *, scheme: str | None = None
) -> dict:
    """``fauna.conversations.room.list_roster`` as ``actor`` — the room's live
    floor roster on this nest (`conversation-rooms.md` § The floor roster).

    Raises on a refusal rather than returning it: the nest answers
    ``permission_denied`` ("not a member of this room") both when ``actor`` is
    absent from a stored roster and when the nest holds NO roster for the room
    at all, and a caller telling those apart needs the error text, which the
    exception carries.
    """
    client = _client_for(_base_url(port, scheme), actor)
    return client.call("fauna.conversations.room.list_roster", {"room_id": room_id_hex})


def room_generations(
    port: int, actor: dict, room_id_hex: str, *, scheme: str | None = None
) -> list[dict]:
    """``fauna.conversations.room.generations`` as ``actor`` — the room's
    generations wrapped to ``actor``'s own floor entry, exactly one marked
    ``is_tip`` (`community-rooms.md` § The three classes → *Community*). A
    rotation is observable here as the tip's ``generation_id`` moving. Raises
    on a refusal (``actor`` off the floor)."""
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.conversations.room.generations", {"room_id": room_id_hex})
    return list(reply.get("generations", []))


def room_search(
    port: int, actor: dict, room_id_hex: str, query: str, *, scheme: str | None = None
) -> list[dict]:
    """``fauna.conversations.room.search`` as ``actor`` — the community room's
    home-nest search, the one search a member cannot run over its own slice
    (`community-rooms.md` § What the read covers). Returns the hits, each a
    log position and a rank, never text. An empty list is a real answer and
    covers three states on purpose: nothing matched, the room was never keyed
    to the nest, or the members rotated the nest's read out and the revoke
    deleted what it built. Raises on a refusal (a non-member, or a room that is
    not a community room)."""
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.room.search", {"room_id": room_id_hex, "query": query}
    )
    return list(reply.get("hits", []))


def channel_fetch(
    port: int,
    actor: dict,
    channel_id_hex: str,
    after: int = 0,
    limit: Optional[int] = None,
    nest_url: Optional[str] = None,
    *,
    scheme: str | None = None,
) -> list[dict]:
    """``fauna.conversations.channel.fetch`` — returns the inner ``messages`` list.

    Each entry is ``{"seq": int, "envelope": bytes}`` (the envelope is a CBOR
    ``bstr`` → Python ``bytes``). Replacement for ``GET /api/v1/channel/{id}?
    after=N``, which returned ``{"messages": [{"seq", "envelope": hex}]}``.

    ``nest_url`` (cross-nest): the channel's **home** nest URL (the group creator's
    nest). When set, ``port``'s nest relays the pull there over the membership-gated
    ``fauna.federation.channel.fetch`` — the way a recipient on a *different* nest
    reads a channel whose message log lives on the creator's nest
    (``ChannelFetchRequest.nest_url``). Absent ⇒ a same-nest fetch from the local log.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    payload: dict[str, Any] = {"channel_id": channel_id_hex, "after": after}
    if limit is not None:
        payload["limit"] = limit
    if nest_url:
        payload["nest_url"] = nest_url
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.conversations.channel.fetch", payload)
    return reply.get("messages", [])


def envelope_class(envelope: bytes) -> str:
    """The ``ChannelEnvelope`` variant a fetched log entry carries —
    ``"Application"``, ``"Commit"``, ``"RoomSealed"``, … (the Rust enum in
    ``libs/fauna-protocol/src/conversations.rs``, externally tagged in CBOR).

    The class is cleartext by design: the nest classifies envelopes without an
    MLS engine. A journey that counts *messages* counts ``"Application"``
    entries, never the raw log length — every device's first application send
    in an epoch rides behind a self-``Update`` takeover Commit (`devices.md`
    § Cross-device MLS group-state sync, the per-launch takeover), so the log
    grows by more than the messages sent."""
    import cbor2

    decoded = cbor2.loads(envelope)
    if isinstance(decoded, dict) and len(decoded) == 1:
        return str(next(iter(decoded)))
    if isinstance(decoded, str):
        return decoded
    raise AssertionError(f"not a ChannelEnvelope: {decoded!r:.120}")


def application_count(
    port: int, actor: dict, channel_id_hex: str, *, scheme: str | None = None
) -> int:
    """How many application messages ``channel_id_hex``'s log holds — the
    :func:`channel_fetch` entries whose :func:`envelope_class` is
    ``"Application"``."""
    return sum(
        envelope_class(m["envelope"]) == "Application"
        for m in channel_fetch(port, actor, channel_id_hex, scheme=scheme)
    )


def channel_actors(
    port: int, actor: dict, channel_id_hex: str, *, scheme: str | None = None
) -> list[str]:
    """``fauna.conversations.channel.actors`` as ``actor`` — the channel's
    routing roster on this nest, as lower-case actor-id hex.

    The nest writes a member's row there when their Welcome is delivered, so
    an MLS member missing from it is one whose Welcome never landed
    (`mls-group-key-material.md` § M2 *Admitting a member*). Member-scoped:
    ``actor`` must be on the roster itself, and the read never registers the
    caller — unlike ``channel_fetch``, which auto-registers a local caller on
    an unclaimed channel and so must never be called as the member whose
    roster row is under test. Raises on a refusal.
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.conversations.channel.actors", {"channel_id": channel_id_hex})
    return [a.lower() for a in reply.get("actors", [])]


# ── key packages (publish + count + fetch; all on fauna.conversations.*) ─────


def keypackage_upload(
    port: int, actor: dict, packages: list[bytes], *, scheme: str | None = None
) -> int:
    """``fauna.conversations.keypackage.upload`` — returns ``stored`` count.

    Self-upload: the caller (``actor``) is the owner; there is no target
    actor_id on the wire (it is implicit in the connection). Replacement for
    ``POST /api/v1/keypackage/{actor_id}``, which took ``[{data: hex}]`` and
    returned ``{"stored": N}``. Each package rides as a CBOR ``bstr``.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.keypackage.upload",
        {"packages": [bytes(p) for p in packages]},
    )
    return reply["stored"]


def keypackage_count(
    port: int, actor: dict, target_actor_id_hex: str, *, scheme: str | None = None
) -> int:
    """``fauna.conversations.keypackage.count`` — non-destructive count.

    Replacement for ``GET /api/v1/keypackage/{actor_id}/count``, which
    returned ``{"count": N}``. **Owner-only:** ``target_actor_id_hex`` must be
    ``actor``'s own id — the nest refuses any other target with
    ``fauna.conversations.permission_denied`` (``federation.md`` § Key packages
    — privacy & exhaustion). Another account's readiness is
    :func:`actor_addressable`'s boolean.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.keypackage.count",
        {"actor_id": target_actor_id_hex},
    )
    return reply["count"]


def actor_addressable(
    port: int, actor: dict, target_actor_id_hex: str, *, scheme: str | None = None
) -> bool:
    """``fauna.conversations.keypackage.addressable`` — whether ``target`` can be reached.

    ``True`` while the target holds ≥1 usable key package, one-time **or**
    last-resort (the same predicate as ``fauna.actor.by_handle``'s
    ``addressable`` bit). A pure read: consumes nothing, discloses no count.
    ``actor`` is the (User-class) caller.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.keypackage.addressable",
        {"actor_id": target_actor_id_hex},
    )
    return bool(reply["addressable"])


def keypackage_fetch(
    port: int,
    actor: dict,
    target_actor_id_hex: str,
    *,
    nest_url: Optional[str] = None,
    scheme: str | None = None,
) -> Optional[bytes]:
    """``fauna.conversations.keypackage.fetch`` — consume one of the target's KPs.

    Replacement for the deleted ``GET /api/v1/keypackage/{actor_id}`` (which
    returned ``{"key_package": hex}``). Destructive: takes one non-expired key
    package for ``target_actor_id_hex`` (decrementing its count), or ``None`` when
    the pool is empty. ``actor`` is the (User-class) caller; ``key_package`` rides
    back as a CBOR ``bstr`` → Python ``bytes`` (``serde_bytes``). Omit ``nest_url``
    for the same-nest fetch (the cross-nest plane is the federation channel).

    ``nest_url`` (cross-nest): the URL of the nest the TARGET actor lives on. The
    caller's own nest then relays the fetch there over the long-lived federation
    channel (``keypackage_fetch_handler`` → ``originate_keypackage_fetch``,
    ``conversations_handlers.rs``) instead of reading its own KP table. A loopback
    ``http://127.0.0.1:<port>`` peer is an explicit test-only affordance of
    ``federation_channel::validate_peer_url``, so a plain-HTTP tier_3 nest is a
    valid target.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    payload: dict = {"actor_id": target_actor_id_hex}
    if nest_url:
        payload["nest_url"] = nest_url
    reply = client.call("fauna.conversations.keypackage.fetch", payload)
    return reply.get("key_package")


def actor_by_handle(
    port: int, actor: dict, handle: str, *, scheme: str | None = None
) -> dict:
    """``fauna.actor.by_handle`` as ``actor`` — the same-nest handle lookup the
    FaunaMls rail's resolve makes. The reply's ``domain`` is this nest's handle
    domain (what a typed ``handle@domain`` must match to resolve same-nest) and
    ``addressable`` says whether the handle has key packages to bootstrap with.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    return client.call("fauna.actor.by_handle", {"handle": handle})


# ── welcome delivery (same-nest plane, and the cross-nest relay) ─────────────


def accept_contact(port: int, actor: dict, peer_id_hex: str, *, scheme: str | None = None) -> None:
    """``fauna.knocks.accept`` as ``actor`` — upserts an ``accepted`` contact row
    toward ``peer_id_hex``, so that peer's DM Welcome flows under the default
    inbox mode (direct-messages.md § Reach policy). Per-pair, so arranging a
    session-scoped recipient this way never mutates their own mode."""
    client = _client_for(_base_url(port, scheme), actor)
    client.call("fauna.knocks.accept", {"peer_id": peer_id_hex})


def set_inbox_mode(port: int, actor: dict, mode: str, *, scheme: str | None = None) -> None:
    """``fauna.inbox.mode.set`` as ``actor`` — e.g. ``"open"`` so a stranger's
    DM Welcome delivers (direct-messages.md § Reach policy: under the default
    ``allow_knock`` a non-contact's Dm/Group Welcome is refused)."""
    client = _client_for(_base_url(port, scheme), actor)
    client.call("fauna.inbox.mode.set", {"mode": mode})


def reachable_peer(
    port: int,
    admin_signing_key,
    sender_actor_id_hex: str,
    *,
    key_packages: int = 3,
    scheme: str | None = None,
) -> dict:
    """Register a same-nest peer a real-wire DM bootstrap can actually reach.

    The arrangement every real-backend 1:1 test needs, in one call — because it
    is *three* facts, and forgetting the third is silent:

    1. the peer exists (``create_actor_and_register``),
    2. they have one-time key packages for the sender's bootstrap to fetch, and
    3. **they accept the sender** — without which the nest refuses the Dm/Group
       Welcome with the opaque ``fauna.conversations.forbidden``
       (``conversations_handlers.rs`` ``welcome_deliver_core``: a recipient not
       yet on the channel falls to ``dm_initiation_mode_verdict``, and a fresh
       actor's stored mode is the ``allow_knock`` default ⇒ ``Knock`` ⇒ refused —
       ``direct-messages.md`` § Reach policy).

    Fact 3 is the one that rots. It became load-bearing on 2026-08-02
    (the DM plane's same-nest ``inbox_mode`` enforcement); that
    commit arranged it in the four modules it touched, and every *other*
    real-wire module kept the pre-ruling two-step and went red — presenting not
    as a refusal but as a missing Welcome / an unconsumed key package two
    assertions downstream, which cost multiple triage passes. Registering a peer and reaching them are now the same
    call so a future test cannot arrange two of the three and look correct.

    Use ``accept_contact`` directly instead when the peer is *not* fresh (a
    session-scoped actor, or one the test registered itself for other reasons).
    """
    from common import create_actor_and_register

    peer = create_actor_and_register(port, admin_signing_key=admin_signing_key)
    keypackage_upload(
        port, peer, mint_key_packages(bytes(peer["signing_key"]), key_packages),
        scheme=scheme,
    )
    accept_contact(port, peer, sender_actor_id_hex, scheme=scheme)
    return peer


def welcome_deliver(
    port: int,
    actor: dict,
    recipient_actor_id_hex: str,
    channel_id_hex: str,
    welcome_bytes: bytes,
    *,
    kind: Optional[dict] = None,
    nest_url: Optional[str] = None,
    scheme: str | None = None,
) -> int:
    """``fauna.conversations.welcome.deliver`` — deliver an MLS Welcome.

    Replacement for the deleted ``POST /api/v1/welcome/{actor_id}``. Stores the
    raw Welcome bytes in the recipient's inbox (observe via :func:`inbox`) and
    auto-registers them on ``channel_id_hex``. ``welcome_bytes`` rides as a CBOR
    ``bstr`` (``serde_bytes``). ``kind`` is the internally-tagged ``WelcomeKind``
    (tag key ``"type"``) — defaults to a DM (``{"type": "dm"}``); a group is
    ``{"type": "group", "group_id": hex}``; a shared folder is
    ``{"type": "folder", "group_id": hex}`` (the RAW MLS group id, not the
    channel id).

    ``nest_url`` (cross-nest): the URL of the nest the RECIPIENT lives on. This
    (the sharer's home) nest then relays the Welcome there over the federation
    channel rather than writing a local inbox row — ``welcome_deliver_core`` →
    ``originate_welcome_deliver`` (``conversations_handlers.rs``), which stamps
    the relayed envelope with this nest's own ``origin_nest_url`` (from
    ``handle_domain_if_set()``). That stamp is what the recipient client reads as
    ``home_nest_url`` and durably records as a ``ForeignFolder``, so a
    handle-domain-less sharer nest yields NO foreign record. The relayed reply
    carries ``inbox_id = 0`` — the peer's own row id is its local detail.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    payload: dict = {
        "recipient_actor_id": recipient_actor_id_hex,
        "channel_id": channel_id_hex,
        "welcome_bytes": bytes(welcome_bytes),
        "kind": kind if kind is not None else {"type": "dm"},
    }
    if nest_url:
        payload["nest_url"] = nest_url
    reply = client.call("fauna.conversations.welcome.deliver", payload)
    return reply["inbox_id"]


# ── real MLS key-package minting (the mls-keypackage-gen helper binary) ───────

def _ensure_keypkg_bin() -> Path:
    """The ``mls-keypackage-gen`` e2e helper binary, prebuilt at collection time.

    The real ``FaunaMlsBackend`` *parses* each peer's key package during group
    bootstrap (``MlsEngine::key_package_from_bytes``), so API-tier peers can't use
    fake byte strings — a fake KP is consumed but fails to parse, yielding no
    group. Each peer publishes a **real** key package minted by this helper (a
    throwaway in-memory engine bound to the peer's identity; the private half is
    discarded, fine because the API-tier peer never processes the Welcome).

    Never built here: the helper-binary registry (``helpers/helper_bins.py``)
    declares this module's reach, so the collection-time prebuild builds it
    outside every test's clock, stamp-gated on the whole workspace — a binary
    older than the engine it links (the key package's shape moves with it) is
    rebuilt there (e2e-conventions.md convention 9).
    """
    from helpers.helper_bins import helper_bin

    return helper_bin("mls-keypackage-gen")


def mint_key_packages(secret_bytes: bytes, count: int) -> list[bytes]:
    """Mint ``count`` real MLS key packages for the identity owning
    ``secret_bytes`` (the 32-byte Ed25519 seed)."""
    out = subprocess.run(
        [str(_ensure_keypkg_bin()), secret_bytes.hex(), str(count)],
        capture_output=True,
        text=True,
        check=True,
    )
    lines = [ln for ln in out.stdout.splitlines() if ln.strip()]
    assert len(lines) == count, f"expected {count} key packages, got {len(lines)}"
    return [bytes.fromhex(ln.strip()) for ln in lines]


# ── real MLS group + Welcome minting (the mls-group-gen helper binary) ────────

def _ensure_group_gen_bin() -> Path:
    """The ``mls-group-gen`` e2e helper binary, prebuilt at collection time.

    Mints a real MLS group + Welcome for a recipient's published key package — the
    sender side of a GUI *receive* proof (``test_fauna_mls_web_receive.py``). The
    GUI app is the only real engine; the sender is this throwaway engine, the
    same pattern :func:`_ensure_keypkg_bin` uses to give engine-less peers a real
    key package. See ``libs/fauna-mls/src/group_gen_main.rs``. Built with
    ``test-helpers`` (its ``--future-body`` mode's raw sealer exists only there)
    by the registry entry, never here.
    """
    from helpers.helper_bins import helper_bin

    return helper_bin("mls-group-gen")


def mint_group_welcome(
    sender_secret_bytes: bytes, recipient_keypackage_bytes: bytes
) -> tuple[str, bytes, str]:
    """Mint a real 1:1 MLS group as ``sender`` against ``recipient``'s key package.

    Returns ``(channel_id_hex, welcome_bytes, group_id_hex)`` — the channel id (the
    ``ChannelId`` Display/hex form the nest's ``welcome.deliver`` expects), the
    TLS-serialized Welcome the recipient's GUI engine can join, and the **raw**
    (variable-length) MLS group id backing the group. The group id is needed to
    bind a shared folder (``fauna.folders.share`` re-derives the identical
    ``ChannelId`` from it — the ``ChannelId`` is a one-way BLAKE3 hash, so it
    cannot be recovered from the channel id alone). The sender's group state lives
    only in the throwaway engine and is discarded (the proof drives the
    *recipient*, never the sender, again).
    """
    out = subprocess.run(
        [
            str(_ensure_group_gen_bin()),
            sender_secret_bytes.hex(),
            recipient_keypackage_bytes.hex(),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    lines = [ln for ln in out.stdout.splitlines() if ln.strip()]
    assert len(lines) == 3, (
        f"expected channel_id + welcome + group_id, got {len(lines)} line(s)"
    )
    return lines[0].strip(), bytes.fromhex(lines[1].strip()), lines[2].strip()


def mint_group_welcome_with_message(
    sender_secret_bytes: bytes, recipient_keypackage_bytes: bytes, body: str
) -> tuple[str, bytes, bytes]:
    """:func:`mint_group_welcome` plus a sealed **application message** carrying ``body``.

    Returns ``(channel_id_hex, welcome_bytes, app_envelope_bytes)``. The recipient is a
    member from group creation, so once their GUI joins the Welcome they can *decrypt*
    the envelope — post it with :func:`channel_send`. This is what lets a GUI-receive
    proof drive a real **decrypt**, not merely a join: only a decrypted body is
    classified, so the moderation queue's post-decrypt local half cannot be proven
    without one (``test_moderation_local_detection.py``'s web leg).
    """
    channel_id_hex, welcome_bytes, (envelope,) = mint_group_welcome_with_messages(
        sender_secret_bytes, recipient_keypackage_bytes, [body]
    )
    return channel_id_hex, welcome_bytes, envelope


#: A ``bodies`` entry for :func:`mint_group_welcome_with_messages` that seals,
#: at its place in the stream, a message of a kind a LATER build sends (a
#: ``ChannelMessageBody`` variant this build has no arm for) instead of text.
FUTURE_BODY = "--future-body"

#: A prefix for a :func:`mint_group_welcome_with_messages` body: the text after
#: it is sealed with a sender timestamp one day in the past (``mls-group-gen``'s
#: ``--backdated:`` form). The nest still assigns the record's ``seq`` at arrival.
BACKDATED = "--backdated:"


def mint_group_welcome_with_messages(
    sender_secret_bytes: bytes, recipient_keypackage_bytes: bytes, bodies: list[str]
) -> tuple[str, bytes, list[bytes]]:
    """:func:`mint_group_welcome_with_message` for several messages of one sender.

    Returns ``(channel_id_hex, welcome_bytes, envelopes)`` — one sealed
    application envelope per body, in stream order, so a proof can post them at
    different moments (the read-state relaunch witness posts the second while
    the recipient's app is closed). Each is a distinct ``channel_send``; the
    nest assigns their channel ``seq`` in post order. A body equal to
    :data:`FUTURE_BODY` is sealed as a later build's unknown message kind.
    """
    out = subprocess.run(
        [
            str(_ensure_group_gen_bin()),
            sender_secret_bytes.hex(),
            recipient_keypackage_bytes.hex(),
            *bodies,
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    lines = [ln for ln in out.stdout.splitlines() if ln.strip()]
    assert len(lines) == 3 + len(bodies), (
        f"expected channel_id + welcome + group_id + {len(bodies)} app envelope(s), "
        f"got {len(lines)} line(s)"
    )
    return (
        lines[0].strip(),
        bytes.fromhex(lines[1].strip()),
        [bytes.fromhex(ln.strip()) for ln in lines[3:]],
    )


@dataclass(frozen=True)
class RejoinStream:
    """Every piece of a recipient's removal and re-admission, minted by one
    sender engine (:func:`mint_group_rejoin_stream`), ready to post one at a
    time. Application envelopes and commits go to :func:`channel_send`, the two
    Welcomes to :func:`welcome_deliver`."""

    channel_id_hex: str
    welcome: bytes
    before: list[bytes]
    remove_commit: bytes
    add_commit: bytes
    second_welcome: bytes
    after: list[bytes]


def mint_group_rejoin_stream(
    sender_secret_bytes: bytes,
    recipient_keypackage_bytes: bytes,
    recipient_second_keypackage_bytes: bytes,
    before: list[str],
    after: list[str],
) -> RejoinStream:
    """A 1:1 the recipient is removed from and brought back into, whole.

    ``mls-group-gen --rejoin``: the sender creates the group against the
    recipient's first key package, seals ``before``, removes the recipient,
    re-adds them from their second key package (a spent key package cannot
    seat anyone twice) and seals ``after`` in the re-joined epoch. The one-shot
    engine's state is gone on exit, which is why the whole stream is minted at
    once; the proof decides when each piece reaches the nest.
    """
    out = subprocess.run(
        [
            str(_ensure_group_gen_bin()),
            "--rejoin",
            sender_secret_bytes.hex(),
            recipient_keypackage_bytes.hex(),
            recipient_second_keypackage_bytes.hex(),
            *before,
            "--",
            *after,
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    lines = [bytes.fromhex(ln.strip()) for ln in out.stdout.splitlines() if ln.strip()]
    want = 3 + len(before) + 3 + len(after)
    assert len(lines) == want, f"expected {want} line(s) from --rejoin, got {len(lines)}"
    channel = out.stdout.splitlines()[0].strip()
    n = len(before)
    return RejoinStream(
        channel_id_hex=channel,
        welcome=lines[1],
        before=lines[3 : 3 + n],
        remove_commit=lines[3 + n],
        add_commit=lines[4 + n],
        second_welcome=lines[5 + n],
        after=lines[6 + n :],
    )


def mint_scheduling_delivery(
    sender_secret_bytes: bytes, recipient_keypackage_bytes: bytes, imip_rfc5322: bytes
) -> tuple[str, bytes, bytes]:
    """Seal ``imip_rfc5322`` as a one-off **scheduling** delivery from ``sender``.

    Returns ``(channel_id_hex, welcome_bytes, app_envelope_bytes)`` — deliver the
    Welcome with :func:`welcome_deliver` (``kind={"type": "scheduling"}``), then
    post the envelope with :func:`channel_send` as the same sender, exactly the
    two calls a Fauna app's ``deliver_scheduling_imip`` makes. Sealed by
    ``MlsEngine::build_scheduling_delivery`` — the one builder the app rail and the
    MDA gateway share — so the recipient's drain cannot tell it from theirs; what
    the nest attests as the record's author is the ``channel_send`` caller.
    """
    out = subprocess.run(
        [
            str(_ensure_group_gen_bin()),
            "--scheduling",
            sender_secret_bytes.hex(),
            recipient_keypackage_bytes.hex(),
            bytes(imip_rfc5322).hex(),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    lines = [ln for ln in out.stdout.splitlines() if ln.strip()]
    assert len(lines) == 3, (
        f"expected channel_id + welcome + app envelope, got {len(lines)} line(s)"
    )
    return lines[0].strip(), bytes.fromhex(lines[1].strip()), bytes.fromhex(lines[2].strip())


def folder_create(
    port: int, actor: dict, name: str, *, mode: str = "sync", scheme: str | None = None
) -> dict:
    """``fauna.folders.create`` — create an owner-owned folder for ``actor``.

    The headless counterpart of the wizard's create; used to seed a *sharer's* set
    before binding it to an MLS group (``folder_share``). Returns the reply
    ``{id, name, mode, ...}``.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    return client.call(
        "fauna.folders.create",
        {
            "name": name,
            "mode": mode,
            "retention_policy": None,
            "include_paths": None,
            "exclude_paths": None,
        },
    )


def folder_share(
    port: int, actor: dict, name: str, group_id_hex: str, *, scheme: str | None = None
) -> dict:
    """``fauna.folders.share`` — bind ``actor``'s owner-owned set ``name`` to the
    raw MLS ``group_id_hex``.

    The nest BLAKE3-derives the 32-byte ``ChannelId`` from the raw group id, claims
    the ``group_id -> ChannelId`` namespace, registers the **owner** on the derived
    roster, and stores the group id in ``folders.mls_group_id`` (the shared flag).
    The *recipient* is rostered separately by ``welcome.deliver``. Returns the reply
    ``{ok, folder, channel_id}``.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    # By hash, so an app-created (sealed) set is found too (schema 114).
    from helpers.set_names import addressed

    return client.call("fauna.folders.share", addressed(name, group_id=group_id_hex))


def folder_set_access(
    port: int, actor: dict, name: str, member_actor_id_hex: str, access: str,
    *, byte_cap: int | None = None, scheme: str | None = None,
) -> dict:
    """``fauna.folders.members.set_access`` — grant ``member_actor_id_hex``
    ``"reader"``/``"writer"`` access on ``actor``'s owner-owned shared set
    ``name``. Owner-scoped AND claimant-gated (``folders.rs::MemberSetAccessRequest``);
    the headless counterpart of the UI-driven ``ActionLayer.backups.set_member_access``
    (used to seed a writer-access member without a second real GUI instance — the
    member row's OWN UI is what a test then drives, per e2e rule 8). Returns the
    reply ``{ok}``.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    # By hash, so an app-created (sealed) set is found too (schema 114).
    from helpers.set_names import addressed

    payload = addressed(name, actor_id=member_actor_id_hex, access=access)
    if byte_cap is not None:
        payload["byte_cap"] = byte_cap
    return client.call("fauna.folders.members.set_access", payload)


def folder_member_actors(
    port: int, actor: dict, name: str, *, scheme: str | None = None
) -> list[str]:
    """``fauna.folders.members.list_actors`` — the hex ActorIds on ``actor``'s
    owner-owned set ``name``, i.e. the owner-visible "Shared with" roster.

    The projection over the set's derived-``ChannelId`` ``actor_channels`` roster:
    a recipient lands on it at ``welcome.deliver`` and leaves it on a decline, a
    suppression, a voluntary leave, or an owner evict (``ui/folders.md`` § Sharing).
    Owner/member-gated, so call it as the *sharer*. Returns every listed actor,
    ``role`` included — filter on the caller's side when you mean members only.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    # By hash, so an app-created (sealed) set is found too (schema 114).
    from helpers.set_names import addressed

    reply = client.call("fauna.folders.members.list_actors", addressed(name))
    return [m["actor_id"] for m in reply.get("members", [])]


def folder_group_id_hex(
    port: int, actor: dict, name: str, *, scheme: str | None = None
) -> str | None:
    """The raw MLS group id (hex) binding ``actor``'s set ``name`` to its
    cross-user group, off ``fauna.folders.list`` — ``None`` while the set is
    unshared. The share plane's set id is ``ChannelId::from_group_id`` of it,
    derived app-side; this reads the stored fact, never re-derives it.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.folders.list", {})
    # By hash: a sealed set's row rests no plaintext name (schema 114).
    from helpers.set_names import find_set

    row = find_set(reply.get("folders", []), name)
    return row.get("mls_group_id") if row else None


def folder_row_id(port: int, actor: dict, name: str, *, scheme: str | None = None) -> int | None:
    """The control-plane row id of ``actor``'s own set ``name``, off
    ``fauna.folders.list`` — ``None`` while no such set exists. The identity a
    member's Media page addresses that set by (``local:<id>``, ``media.md``
    § Encryption at rest → *The filter and the upload target address the
    row*); read here as the stored fact, never derived."""
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.folders.list", {})
    from helpers.set_names import find_set

    row = find_set(reply.get("folders", []), name)
    return int(row["id"]) if row and row.get("id") is not None else None


def folder_rows(port: int, actor: dict, *, scheme: str | None = None) -> list[dict]:
    """``actor``'s ``fauna.folders.list`` rows WITH the member-visible
    projection (``include_shared_with_me``), each cut to the fields a red needs
    to read: id, role, access, owner handle, whether a name and a group id
    are present. A diagnosis read — the stored facts, never a stand-in for the
    app's own view."""
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call("fauna.folders.list", {"include_shared_with_me": True})
    return [
        {
            "id": r.get("id"),
            "role": r.get("role"),
            "access": r.get("access"),
            "owner_handle": r.get("owner_handle"),
            "name": r.get("name") or ("<sealed>" if r.get("name_sealed") else ""),
            "grouped": bool(r.get("mls_group_id")),
        }
        for r in reply.get("folders", [])
    ]


# ── inbox (Welcome delivery observation) ─────────────────────────────────────


def inbox(base_url: str, actor: dict) -> list:
    """Peek an actor's inbox over the WS-RPC kind ``fauna.inbox.fetch``.

    The WS-RPC successor of the deleted ``GET /api/v1/inbox/{actor}`` drain
    (removed in the WS-RPC-everywhere rip). ``fetch`` is a **pure peek**: it
    returns undelivered items without marking them delivered — the old HTTP
    twin marked-on-read, a data-loss bug the fetch/ack split fixed, and an
    observation helper wants the peek, never the consume. Caller-scoped by
    construction: the connection authenticates *as* ``actor`` (challenge/verify
    over its signing key), so it drains ``actor``'s own queue — works same-nest
    and on a foreign nest where ``actor`` is registered (the cross-nest Welcome
    case, e.g. ``test_cross_nest_welcome_delivery``).

    Used to observe MLS Welcome delivery. Returns the list of ``{id, payload}``
    items with ``payload`` **hex-encoded**, preserving the old HTTP shape (the
    kind rides ``payload`` as a CBOR ``bstr`` → Python ``bytes``). ``base_url``
    is the *recipient's* nest base (the foreign nest for a cross-nest Welcome)."""
    client = _client_for(base_url, actor)
    reply = client.call("fauna.inbox.fetch", {"limit": 0})
    return [{"id": it["id"], "payload": it["payload"].hex()} for it in reply.get("items", [])]


def inbox_row_pending(
    port: int, recipient: dict, inbox_id: int, *, scheme: str | None = None
) -> bool:
    """Whether ``recipient``'s inbox still holds row ``inbox_id`` un-acked (a
    :func:`inbox` peek, never a consume). The nest-side half of a pending-share
    diagnosis: a staged folder knock IS its un-acked Welcome row, so a page that
    paints no ``folder-pending-share`` while this reads True failed to show a knock
    the nest holds, and one that reads False had its knock acked (joined, declined
    or dropped) before the page looked (convention 6)."""
    return any(item["id"] == inbox_id for item in inbox(_base_url(port, scheme), recipient))


def await_welcome_applied(
    port: int,
    recipient: dict,
    inbox_id: int,
    budget_s: float,
    *,
    who: str = "the app",
    scheme: str | None = None,
) -> None:
    """Wait until ``recipient``'s app has JOINED the group a Welcome seated it
    in — the Welcome's inbox row (``inbox_id``, :func:`welcome_deliver`'s
    return) is gone from the recipient's peek.

    The join witness every real-MLS receive test waits on before posting to the
    channel. It is nest-side on purpose: the shared drain is the only surface
    that acks an inbox row, and it acks a Welcome only once ``ingest_welcome``
    durably applied it (``libs/fauna-client-inbox`` — the ratified ack policy),
    so the row's disappearance IS the join, on every app alike. The app's thread
    list is not a join witness for a 1:1: a direct thread whose channel has
    never carried a renderable message is listed on no rail
    (``archive-import.md`` § Linking and merging → *Phase two — the rulings*,
    ruling 1), so a Welcome-only DM is joined and invisible until its first
    message lands."""
    from helpers.waiting import wait_until

    def pending() -> bool:
        return inbox_row_pending(port, recipient, inbox_id, scheme=scheme)

    wait_until(
        lambda: not pending(),
        budget_s,
        interval=0.5,
        diagnose=lambda: (
            f"waiting for {who} to drain its inbox and join the Welcome's group "
            f"(inbox row {inbox_id} still un-acked: the app has not applied the "
            "Welcome — its receive loop never drained, or ingest_welcome failed "
            "and left the row for a retry)"
        ),
    )
