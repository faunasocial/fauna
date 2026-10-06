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
import os
import subprocess
import sys
from pathlib import Path
from typing import Any, Optional

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import port_base_url


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


atexit.register(close_all_ws)


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
    returned ``{"count": N}``. ``actor`` is the (User-class) caller; the count
    is for ``target_actor_id_hex``.

    ``scheme``: pass ``"https"`` for a ``serve_tls=True`` ``port`` (see ``channel_send``).
    """
    client = _client_for(_base_url(port, scheme), actor)
    reply = client.call(
        "fauna.conversations.keypackage.count",
        {"actor_id": target_actor_id_hex},
    )
    return reply["count"]


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

_KEYPKG_BIN: Path | None = None


def _ensure_keypkg_bin() -> Path:
    """Build (once per process) + locate the ``mls-keypackage-gen`` e2e helper binary.

    The real ``FaunaMlsBackend`` *parses* each peer's key package during group
    bootstrap (``MlsEngine::key_package_from_bytes``), so API-tier peers can't use
    fake byte strings — a fake KP is consumed but fails to parse, yielding no
    group. Each peer publishes a **real** key package minted by this helper (a
    throwaway in-memory engine bound to the peer's identity; the private half is
    discarded, fine because the API-tier peer never processes the Welcome).
    """
    global _KEYPKG_BIN
    if _KEYPKG_BIN is not None:
        return _KEYPKG_BIN
    from common.nest import _cargo_cmd, get_repo_root
    repo_root = get_repo_root()
    cargo_target = os.environ.get("CARGO_TARGET_DIR")
    # `.exe` on Windows — without it the built binary is never found and the
    # rebuild below loops into the AssertionError. (Latent until the windows
    # app gained the `conversations_real_*` bridge commands: every caller of
    # this helper was linux/web-only.)
    exe = ".exe" if sys.platform == "win32" else ""
    name = f"mls-keypackage-gen{exe}"
    candidates = []
    if cargo_target:
        candidates.append(Path(cargo_target) / "debug" / name)
    candidates.append(repo_root / "target" / "debug" / name)
    # Build once per process, even when a binary exists — the same rule
    # `_ensure_group_gen_bin` below keeps. The key package's shape is the
    # ENGINE's, and it moves: a binary left over from before key packages
    # advertised the room-policy extension minted packages that the engine,
    # once it stopped forking policy-less groups (2026-09-25), refuses to seat
    # ("does not advertise the room-policy extension"). Found by a windows run
    # of test_thread_membership_real.py against an Aug 27 build of this helper.
    # Cargo no-ops when fresh. `_cargo_cmd` routes through
    # `scripts/cargo-win.cmd` on Windows, where a bare `cargo` picks up Git Bash's
    # `/usr/bin/link.exe` instead of the MSVC linker.
    subprocess.run(
        _cargo_cmd(repo_root) + ["build", "-p", "fauna-mls", "--bin", "mls-keypackage-gen"],
        cwd=repo_root,
        check=True,
    )
    for p in candidates:
        if p.exists():
            _KEYPKG_BIN = p
            return p
    raise AssertionError(f"{name} not found after build (looked in {candidates})")


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

_GROUP_GEN_BIN: Path | None = None


def _ensure_group_gen_bin() -> Path:
    """Build (once) + locate the ``mls-group-gen`` e2e helper binary.

    Mints a real MLS group + Welcome for a recipient's published key package — the
    sender side of a GUI *receive* proof (``test_fauna_mls_web_receive.py``). The
    GUI app is the only real engine; the sender is this throwaway engine, the
    same pattern :func:`_ensure_keypkg_bin` uses to give engine-less peers a real
    key package. See ``libs/fauna-mls/src/group_gen_main.rs``.
    """
    global _GROUP_GEN_BIN
    if _GROUP_GEN_BIN is not None:
        return _GROUP_GEN_BIN
    from common.nest import _cargo_cmd, get_repo_root
    repo_root = get_repo_root()
    cargo_target = os.environ.get("CARGO_TARGET_DIR")
    # `.exe` on Windows — without it the built binary is never found and the
    # rebuild below loops into the AssertionError (same latent gap
    # _ensure_keypkg_bin had until the windows app gained the
    # conversations_real_* bridge commands; this sibling was never exercised
    # on windows either until now).
    exe = ".exe" if sys.platform == "win32" else ""
    name = f"mls-group-gen{exe}"
    candidates = []
    if cargo_target:
        candidates.append(Path(cargo_target) / "debug" / name)
    candidates.append(repo_root / "target" / "debug" / name)
    # Build once per process, even when a binary exists: this helper grows
    # modes (`--scheduling`), and a binary left over from before one landed
    # would take the new mode's flag for a secret. Cargo no-ops when fresh.
    # `_cargo_cmd` routes through `scripts/cargo-win.cmd` on Windows, where a bare
    # `cargo` picks up Git Bash's `/usr/bin/link.exe` instead of the MSVC
    # linker.
    subprocess.run(
        _cargo_cmd(repo_root) + ["build", "-p", "fauna-mls", "--bin", "mls-group-gen"],
        cwd=repo_root,
        check=True,
    )
    for p in candidates:
        if p.exists():
            _GROUP_GEN_BIN = p
            return p
    raise AssertionError(f"{name} not found after build (looked in {candidates})")


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


def mint_group_welcome_with_messages(
    sender_secret_bytes: bytes, recipient_keypackage_bytes: bytes, bodies: list[str]
) -> tuple[str, bytes, list[bytes]]:
    """:func:`mint_group_welcome_with_message` for several messages of one sender.

    Returns ``(channel_id_hex, welcome_bytes, envelopes)`` — one sealed
    application envelope per body, in stream order, so a proof can post them at
    different moments (the read-state relaunch witness posts the second while
    the recipient's app is closed). Each is a distinct ``channel_send``; the
    nest assigns their channel ``seq`` in post order.
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
    return client.call(
        "fauna.folders.share",
        {"name": name, "group_id": group_id_hex},
    )


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
    payload = {"name": name, "actor_id": member_actor_id_hex, "access": access}
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
