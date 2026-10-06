"""tier_3 api: a replayed idempotency key across a RECONNECT returns the
ORIGINAL reply — the nest's durable idempotency tier (W4 (account-data-plane.md § Workstreams) phase 3).

``docs/goal/architecture/account-data-plane.md`` § The offline-mutation
contract → *Nest-side durable idempotency*: the per-connection
``IdempotencyCache`` is structurally useless for offline replay (empty on
every reconnect), so the nest gains a **durable** idempotency table — a
replay of an already-applied intent key returns the first outcome, never a
second effect. ``transport.md`` § Idempotency and reconnect-with-resume owns
the wire semantics, including the rule this module deliberately leans on:
the server replays **regardless of the kind's ``forbid_replay`` flag** (the
original op succeeded once; the question is just "what did it return?" —
the forbid is about the *client* not auto-retrying).

The witness kind is ``fauna.admin.invite_codes.create`` precisely because it
is the sharpest possible discriminator on both axes: its handler mints a
fresh **random** code per execution (so a re-run is visibly a different
reply, and a second row), and it is ``forbid_replay = true`` (so a green run
also pins the serve-regardless rule). Before the durable tier, the second
connection's empty cache let the handler run again for real — two codes,
two rows.

Latency-independent (convention 14): no waits at all — every step is a
synchronous request/reply on a live socket.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3

_CREATE = "fauna.admin.invite_codes.create"
_LIST = "fauna.admin.invite_codes.list"
_DELETE = "fauna.admin.invite_codes.delete"


def _admin_client(nest):
    admin = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin.verify_key),
        signing_key=bytes(admin),
    )


def test_a_replayed_key_across_a_reconnect_returns_the_original_reply(nest_instance):
    """Same key, fresh connection → the original Reply, and exactly one row.

    Two separate client contexts = two WS connections, so the second call
    arrives with the per-connection cache guaranteed empty — the exact shape
    of an outbox drain re-presenting its stored key after a reconnect
    (`fauna_sync_engine::outbox` re-sends the SAME 16-byte envelope key on
    every attempt).
    """
    key = secrets.token_bytes(16)
    payload = {"tier": "free", "uses": 1}
    minted = []
    try:
        with _admin_client(nest_instance) as ws:
            reply1 = ws.call_with_key(_CREATE, payload, key)
        code1 = reply1["code"]
        assert code1, f"first create returned no code: {reply1!r}"
        minted.append(code1)

        with _admin_client(nest_instance) as ws:
            reply2 = ws.call_with_key(_CREATE, payload, key)
        code2 = reply2.get("code")
        if code2 and code2 != code1:
            minted.append(code2)

        assert code2 == code1, (
            "a replayed idempotency key on a fresh connection must return the "
            f"ORIGINAL reply (first code {code1!r}), not re-run the handler "
            f"(got {code2!r}) — the durable tier exists because the "
            "per-connection cache is empty on every reconnect"
        )

        # The effect happened once: exactly one of the codes this test minted
        # exists on the nest (the shared session nest may hold others).
        with _admin_client(nest_instance) as ws:
            listed = ws.call(_LIST, {})
        ours = [
            c["code"]
            for c in listed.get("invite_codes", [])
            if c.get("code") in {code1, code2}
        ]
        assert ours == [code1], (
            f"exactly the first call's row should exist, got {ours!r} — a "
            "second row means the replay re-ran the handler"
        )
    finally:
        # Leave the shared nest as found, whichever way the asserts went.
        with _admin_client(nest_instance) as ws:
            for code in minted:
                try:
                    ws.call(_DELETE, {"code": code})
                except Exception:  # noqa: BLE001 — best-effort cleanup
                    pass
