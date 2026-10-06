"""tier_3 API: the deployment-seed rotation chain, fetched with NO SESSION.

Goal doc: ``docs/goal/architecture/nest/box-recovery.md`` § Deployment-seed
rotation → *Client acceptance — re-pin on a verified chain* (the chain is
fetched over a pre-identity ``fauna.auth.rotation_chain`` kind on the same
connection) and *Chain, not statement* (the log serves the full ordered chain,
seq 1..head).

**Why this exists when the nest's own suites already cover the ceremony.**
``bins/fauna-nest``'s conformance tests call ``(meta.handler)(state, actor,
bytes)`` directly, inventing the caller's actor id — a harness that *cannot
observe* whether a genuinely anonymous connection is allowed to reach a kind at
all. That verdict is made a layer up, at ``routes.rs``'s connection gate
(``if conn.anonymous && !is_pre_identity_kind(&kind)``) against
``bins/fauna-nest/src/pre_identity_allowlist.rs``. This is the same lesson rows
53 and 60 paid for, and it bites *harder* here than anywhere else in the tree:
a client reaches this kind precisely when the identity it was presented does not
match its pin, which is the one moment its stored bearer is worthless — the
token was minted by, and the channel bound to, an identity the box no longer
serves. Drop the allowlist entry and every nest-side test stays green while the
bridge that makes rotation silent is unreachable by the only callers who need
it, i.e. the ceremony silently reverts to the outage it was designed to end.

So the nest suites pin the *decision*; this pins the *reachability*, over a real
nest binary and a real anonymous WebSocket — and, in the second test, over a box
whose identity genuinely changed under it.

Latency-independent by construction (convention 14): every assertion is on
returned state, and the one wait in this file — the running box adopting the
successor — is the sanctioned positive-wait shape: a named generous budget +
a deadline poll (``_poll_until_serves``). Adoption is an in-process
serving-generation restart (``box-recovery.md`` § Adoption by the running
process): the rotate handler fires ``serve_restart`` after its reply flushes,
the serve loop tears the generation down (connections included) and re-enters
``start_server``, so a short window of refused connections is part of the
mechanism, never a defect — green runs pay only the actual teardown.
"""

from __future__ import annotations

import secrets
import time

import pytest

pytestmark = pytest.mark.tier_3

_CHAIN_KIND = "fauna.auth.rotation_chain"
_ROTATE_KIND = "fauna.admin.deployment_seed.rotate"

# Generous ceiling for the in-process generation restart (reply-flush delay +
# WS 1001 drain + worker cancel + rebind). Sized far above any non-pathological
# teardown so load cannot flake it; a green run pays only the real duration.
_ADOPTION_BUDGET_S = 60.0


def _poll_until_serves(nest_url: str, expected: bytes, tag: str) -> None:
    """Deadline-poll ``fauna.nest.info`` until the box serves ``expected``.

    Tolerates the re-enter window's connection errors (refused / handshake
    EOF — the listener is down between generations) AND the reply-flush window
    in which the *old* generation still answers with the ancestor.
    """
    deadline = time.monotonic() + _ADOPTION_BUDGET_S
    last: object = None
    while time.monotonic() < deadline:
        try:
            served = _nest_id(nest_url)
        except Exception as e:  # noqa: BLE001 — the window's error shape varies
            last = e
        else:
            if served == expected:
                return
            last = f"still serving {served.hex()}"
        time.sleep(0.25)
    raise AssertionError(
        f"{tag}: box did not serve the successor within {_ADOPTION_BUDGET_S}s "
        f"(last: {last!r}) — the committed rotation never reached the running "
        f"process (box-recovery.md § No dual serving)"
    )


def _chain_after_adoption(nest_url: str) -> list:
    """The chain, read once the box is answering again after a rotation."""
    deadline = time.monotonic() + _ADOPTION_BUDGET_S
    last: object = None
    while time.monotonic() < deadline:
        try:
            return _chain(nest_url)
        except Exception as e:  # noqa: BLE001 — re-enter window
            last = e
            time.sleep(0.25)
    raise AssertionError(
        f"chain unreadable within {_ADOPTION_BUDGET_S}s of the rotation: {last!r}"
    )


def _chain(nest_url: str) -> list:
    """The box's rotation chain, oldest hop first, over an anonymous socket."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        reply = anon.call(_CHAIN_KIND, {})
    assert isinstance(reply, dict), f"rotation_chain reply not a map: {reply!r}"
    return list(reply.get("chain", []))


def _nest_id(nest_url: str) -> bytes:
    """The identity the box serves right now, as raw bytes.

    Read anonymously from ``fauna.nest.info`` (hex there) so the test never
    needs to derive an Ed25519 public key from a seed in Python.
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        info = anon.call("fauna.nest.info", {})
    return bytes.fromhex(info["nest_id"])


def test_a_box_that_never_rotated_serves_an_empty_chain(nest_instance):
    """Empty is the honest answer, not an error — and the shared nest proves it.

    This runs against the ordinary session nest deliberately: the overwhelming
    majority of boxes never rotate, so the reply they serve is the one almost
    every client will ever see, and a client must read it as "nothing to verify,
    fall through to the identity-changed surface" rather than as a refusal. An
    implementation that errored here would turn every un-rotated box into a
    failure at exactly the moment a client is already suspicious.

    Reaching the kind at all is the other half of the assertion: on an anonymous
    connection an off-allowlist kind comes back ``fauna.protocol.unauthenticated``
    (see ``test_anon_ws_rpc_client.py``), so a successful call *is* the
    allowlist-entry proof.
    """
    assert _chain(nest_instance["url"]) == [], (
        "a nest that never rotated must serve an empty chain, not an error"
    )


@pytest.mark.feature("admin-nest")
def test_the_chain_carries_the_hop_a_real_rotation_wrote(rotatable_nest):
    """Rotate a live box, then read the bridge back with no session at all.

    The end-to-end property the ceremony rests on: after the identity changes,
    an anonymous caller can obtain a statement that links the identity it used
    to hold to the one the box now serves. Asserted on returned state only —
    the *old* id comes from ``nest.info`` before the rotation, the *new* id from
    the rotate reply itself, so the test never derives a key.

    Its own nest, because rotation is deployment-destructive by design: every
    enrolled client's pin breaks until it walks this chain. Sharing the session
    nest would change the identity under every later test in the run.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    url = rotatable_nest["url"]
    admin = rotatable_nest["admin"]

    before = _nest_id(url)
    assert _chain(url) == [], "fixture nest should start un-rotated"

    successor_seed = secrets.token_bytes(32).hex()
    with WsRpcAdminClient(
        url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as ws:
        reply = ws.call(_ROTATE_KIND, {"new_seed": successor_seed})

    assert reply.get("already_rotated") is False, (
        f"a fresh successor seed must be a real rotation, not an ack: {reply!r}"
    )
    assert reply.get("seq") == 1, f"first rotation should be seq 1: {reply!r}"
    after = bytes(reply["nest_actor_id"])
    assert after != before, "the box must actually be serving a new identity"

    chain = _chain_after_adoption(url)
    assert len(chain) == 1, f"exactly one hop should have been written: {chain!r}"
    hop = chain[0]["statement"]
    assert bytes(hop["old_nest_actor_id"]) == before, (
        "the hop must start at the identity clients are pinned to"
    )
    assert bytes(hop["new_nest_actor_id"]) == after, (
        "the hop must end at the identity the box now serves"
    )
    assert hop["seq"] == 1
    # Both signatures are mandatory — `old_sig` is the continuity licence and
    # `new_sig` the possession proof. Their *verification* is pinned in Rust
    # (`fauna_protocol::nest_rotation`); what this asserts is that the box
    # actually served them, since a chain missing either is unusable by every
    # client and no nest-side test would notice it going out empty.
    assert len(bytes(chain[0]["old_sig"])) == 64, "old_sig must be on the wire"
    assert len(bytes(chain[0]["new_sig"])) == 64, "new_sig must be on the wire"


def test_the_running_box_serves_the_successor_immediately(rotatable_nest):
    """After the commit the box presents the successor — with no restart by us.

    This is the `box-recovery.md` § No dual serving rule reduced to its
    smallest observable: rotate, then ask the box who it is. The mechanism is
    the in-process serving-generation restart (§ Adoption by the running
    process): the rotate handler fires `serve_restart` once its reply flushes,
    the serve loop tears the generation down and re-enters `start_server`,
    which rebuilds `nest_identity`/`nest_signing_key` — and every worker
    holding cloned key material — from the rotated DB.

    The supervisor-less proof rides the same run: this fixture's nest is a
    bare child process with NOTHING supervising it, the test performs no
    restart, and the assertion that the ORIGINAL process is still alive
    afterwards (`proc.poll() is None`) is what distinguishes the in-process
    re-enter from an exit-and-hope-something-restarts-us remedy, which would
    leave this box dead (`nest/common.md` § Client-state recoverability).
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    url = rotatable_nest["url"]
    admin = rotatable_nest["admin"]
    try:
        proc = rotatable_nest["proc"]
    except Exception:  # noqa: BLE001 — a non-standalone provider owns the process
        proc = None

    with WsRpcAdminClient(
        url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as ws:
        reply = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
    after = bytes(reply["nest_actor_id"])

    _poll_until_serves(url, after, "post-rotation adoption")

    if proc is not None:
        assert proc.poll() is None, (
            "the nest process exited during rotation adoption — the ratified "
            "mechanism is an IN-PROCESS generation restart precisely so a "
            "supervisor-less box survives it (box-recovery.md § Adoption by "
            "the running process)"
        )


def test_a_second_rotation_in_the_same_process_extends_the_chain(rotatable_nest):
    """The graph-rebuild proof: rotate twice, restart nothing, get seq 1 then 2.

    `nest.info` alone can't distinguish a full graph rebuild from a two-field
    patch. The rotate handler can: it reads the predecessor seed it signs the
    statement with from ITS OWN `AppState.nest_signing_key`, so a second
    rotation in the same process succeeds only if the re-enter genuinely
    rebuilt the state — a stale field would offer the evicted ancestor as
    `old_seed` and the transaction's verify arm would refuse it. The two-hop
    chain read back at the end pins the ordering end-to-end.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    url = rotatable_nest["url"]
    admin = rotatable_nest["admin"]
    actor = bytes(admin["signing_key"].verify_key)
    key = bytes(admin["signing_key"])

    id0 = _nest_id(url)

    with WsRpcAdminClient(url, actor_id=actor, signing_key=key) as ws:
        r1 = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
    assert r1.get("seq") == 1, f"first rotation should be seq 1: {r1!r}"
    id1 = bytes(r1["nest_actor_id"])
    _poll_until_serves(url, id1, "first rotation")

    # A fresh connection: the old one died with its generation, and the admin's
    # Ed25519 auth is actor-keyed, so it authenticates against the successor
    # identity without ceremony.
    with WsRpcAdminClient(url, actor_id=actor, signing_key=key) as ws:
        r2 = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
    assert r2.get("already_rotated") is False, (
        f"a second fresh seed must be a real rotation: {r2!r}"
    )
    assert r2.get("seq") == 2, (
        f"second rotation should extend the chain at seq 2: {r2!r} — a stale "
        f"`old_seed` read means the generation re-enter did not rebuild AppState"
    )
    id2 = bytes(r2["nest_actor_id"])
    _poll_until_serves(url, id2, "second rotation")

    chain = _chain_after_adoption(url)
    hops = [
        (
            bytes(h["statement"]["old_nest_actor_id"]),
            bytes(h["statement"]["new_nest_actor_id"]),
            h["statement"]["seq"],
        )
        for h in chain
    ]
    assert hops == [(id0, id1, 1), (id1, id2, 2)], (
        f"the chain must record both hops in order: {hops!r}"
    )


@pytest.mark.feature("admin-nest")
def test_a_rotation_evicts_every_bearer_minted_before_it(rotatable_nest):
    """A committed rotation clears the bearer token store — the convergence
    forcing function (``box-recovery.md`` § Client acceptance → *Live-session
    convergence*, the ruling).

    The token store is in-memory and deliberately created OUTSIDE the serve
    loop, so the in-process serving-generation restart the rotation fires does
    NOT wipe it (an unrelated restart — the serving-port change — must not
    sign everyone out). Before the ruling landed, that survival was exactly
    the measured defect: the post-rotation reconnect re-authenticated with
    the held bearer, ran no graduation, and the live session's identity pin
    stayed on the predecessor until its next launch — an unbounded eviction
    window. The rotate handler now clears the store in the same decision that
    retires the key, so the teardown's forced reconnect must re-mint — and
    the mint is where graduation runs the rotation bridge
    (``fauna_anon_client::mint_bearer_over_handshake`` →
    ``graduate_handshake`` → ``try_rotation_bridge``).

    Two halves, deliberately one test: the **eviction** (an upgrade
    presenting the pre-rotation bearer is refused ``401``) and the
    **recovery control** (the same actor's fresh mint + upgrade works) —
    eviction without recovery would be a lockout, not convergence. The
    refusal is asserted only after ``_poll_until_serves`` confirms the box
    is answering again, so a 401 here is the token gate, never the re-enter
    window's connection refusal.
    """
    import websocket

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import ws_sslopt

    url = rotatable_nest["url"]
    admin = rotatable_nest["admin"]
    actor = bytes(admin["signing_key"].verify_key)
    key = bytes(admin["signing_key"])

    with WsRpcAdminClient(url, actor_id=actor, signing_key=key) as ws:
        # The upgrade that just succeeded validated this bearer; hold the raw
        # token the way a live app process holds it across the rotation.
        held = ws._bearer.token  # noqa: SLF001 — the raw credential IS the subject
        reply = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
    after = bytes(reply["nest_actor_id"])

    _poll_until_serves(url, after, "bearer-eviction rotation")

    scheme_swapped = url.replace("https://", "wss://").replace("http://", "ws://")
    ws_url = f"{scheme_swapped.rstrip('/')}/api/v1/ws/{actor.hex()}"
    try:
        sock = websocket.create_connection(
            ws_url,
            subprotocols=["fauna.v1", f"bearer.{held}"],
            timeout=10,
            # The raw dial the floor-cert opener cannot reach: that opener is a
            # urllib mechanism, so under `--nest docker` (a `wss://` nest on its
            # self-signed floor cert) this line met the cert bare. Same rule,
            # same file, other transport — `common.auth.ws_sslopt`.
            sslopt=ws_sslopt(ws_url),
        )
    except websocket.WebSocketBadStatusException as e:
        assert e.status_code == 401, (
            f"the pre-rotation bearer must be refused UNAUTHORIZED (401), got "
            f"{e.status_code}"
        )
    else:
        sock.close()
        raise AssertionError(
            "a bearer minted before the rotation still authenticated after it "
            "— the rotation must evict the credential class in the same "
            "decision that retires the key (box-recovery.md § Client "
            "acceptance → Live-session convergence)"
        )

    # Recovery control: a fresh mint for the same actor converges instead of
    # locking out — this is the exact path the evicted reconnect takes.
    with WsRpcAdminClient(url, actor_id=actor, signing_key=key) as ws2:
        info = ws2.call("fauna.nest.info", {})
    assert bytes.fromhex(info["nest_id"]) == after, (
        "the re-minted session must land on the successor identity"
    )


def test_the_chain_is_still_reachable_without_a_bearer_after_the_rotation(
    rotatable_nest,
):
    """The reachability that matters is reachability *after* the identity moved.

    A client whose pin just failed has no usable session — its bearer was minted
    by the superseded identity. This drives that exact order: rotate first, then
    fetch anonymously. It is a distinct assertion from the test above, which
    reads the chain over a connection opened after the rotation but is written
    to prove the *content*; here the subject is that the anonymous door is still
    open on a box that has already moved on.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    url = rotatable_nest["url"]
    admin = rotatable_nest["admin"]

    with WsRpcAdminClient(
        url,
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as ws:
        ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})

    # No bearer anywhere in this call, on a box that has rotated.
    assert len(_chain_after_adoption(url)) >= 1, (
        "the rotation chain must stay reachable anonymously after a rotation — "
        "that is the only moment a client ever needs it"
    )
