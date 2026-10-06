"""Drive the task-delegation advisory lease from outside an app — a stand-in
participant's heartbeat, and the nest-side age seam.

``docs/goal/behavior/participants.md`` § Coordination primitive: the lease is a
dumb per-actor last-writer-wins blackboard on the home nest. Any of the user's
own participants may heartbeat it (``fauna.delegation.heartbeat``) and read it
(``fauna.delegation.observe``), and freshness is the nest-computed ``age_ms``
compared against ``fauna_core::delegation::LEASE_STALE_MS`` (90 s).

Two things a Task-delegation page journey needs and an app cannot supply:

**A participant that is not this app.** The page's runner column is about *other*
devices as much as this one — the nest, a sibling desktop, a phone the policy
must never pick. :func:`heartbeat` is that participant: a real
``fauna.delegation.heartbeat`` from the user's own actor, self-reporting whatever
class the journey needs. It is a fixture-setup arrangement of the world (another
of the user's devices exists and is running something), never the mutation under
test — every assignment a journey *makes* still goes through a real app's picker
(convention 8).

**A lease that has gone stale.** The nest computes ``age_ms`` from its own
monotonic clock, so the only way across the 90 s boundary without a seam is to
wait 90 s of wall clock — the fixed-delay shape ``e2e-conventions.md``
convention 14 calls defunct. :func:`age_lease` is the seam
(``bins/fauna-nest/src/delegation_lease_test_hook.rs``, ``--features
test-hooks``): it back-dates the recorded heartbeat, so the very next
``observe`` reports the lease as old as the test asked for, and it **returns the
resulting age** so the caller asserts the boundary was crossed rather than
assuming it. Convention 15: the feature is the boundary, so this is a
standalone-nest surface — the docker/live artifacts do not compile it.

Stopping a stand-in is simply not calling :func:`heartbeat` again; there is no
release on the wire (``LeaseRegistry::release`` is nest-internal, for the nest's
own leases). :func:`age_lease` is therefore also how a journey says "this device
went away", which is the departure two ``task-delegation`` outcomes are about.
"""

from __future__ import annotations

import json
import urllib.request

from clients.ws_rpc_admin_client import WsRpcAdminClient

#: Comfortably past ``fauna_core::delegation::LEASE_STALE_MS`` (90 000). Any
#: value over the constant does; this one is far enough over that a reader never
#: has to check whether it is off by a rounding error.
PAST_STALE_MS = 5 * 60 * 1000

#: ``fauna_core::delegation::LEASE_STALE_MS`` — the freshness boundary the nest's
#: ``age_ms`` is compared against. Mirrored here so an assertion can name it.
LEASE_STALE_MS = 90_000


def _actor_client(nest_instance: dict, user: dict) -> WsRpcAdminClient:
    """A WS-RPC connection authenticated as ``user`` — the lease map is keyed by
    the authenticated actor, so a stand-in participant of *this* user must
    connect as this user."""
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(user["signing_key"].verify_key),
        signing_key=bytes(user["signing_key"]),
    )


def device_holder(device_id: str) -> dict:
    """``ParticipantRef::Device`` on the wire — a stand-in device of the user's,
    keyed by the same hex-encoded 32-byte id the Devices roster uses."""
    return {"Device": {"device_id": device_id}}


def nest_holder(nest_instance: dict) -> dict:
    """``ParticipantRef::Nest`` on the wire, naming **this** nest.

    The pubkey is ``nest.info``'s ``nest_id`` — byte-for-byte what the nest's own
    in-process lease runner claims with (``delegation_runner::nest_self_ref`` →
    ``state.nest_identity.public_key_bytes()``), so a row rendered from this
    stand-in names the real home nest rather than a fiction. The ``[u8; 32]``
    rides as a 32-byte CBOR byte string, like every fixed-width id, so it is
    sent as raw ``bytes``.
    """
    from tests.api import ws_api

    raw = ws_api.nest_info(nest_instance["port"])["nest_id"]
    raw = bytes.fromhex(raw.strip()) if isinstance(raw, str) else bytes(raw)
    assert len(raw) == 32, f"nest.info nest_id is not 32 bytes: {raw!r}"
    return {"Nest": {"actor_pubkey": raw}}


def heartbeat(
    nest_instance: dict,
    user: dict,
    task_kind: str,
    *,
    holder: dict,
    holder_class: str = "PluggedInDesktop",
) -> dict:
    """Claim or renew ``task_kind``'s lease for a stand-in participant of
    ``user``, and return the nest's post-write ``LeaseState``.

    ``holder`` is a wire ``ParticipantRef`` — :func:`device_holder` or
    :func:`nest_holder`. ``holder_class`` is the participant class the holder
    self-reports: ``"PluggedInDesktop"``, ``"BatteryMobile"`` or
    ``"AlwaysOnNest"``. The nest records both unconditionally (it is a dumb
    blackboard); what the class *means* is decided client-side by
    ``fauna_core::delegation::decide``, which is exactly what a journey
    asserting the policy order wants to exercise.

    **The reply is the causal barrier.** It carries the holder the nest recorded
    at that instant, so a journey about a takeover can assert the stand-in
    genuinely held the lease before asserting that the app took it back — rather
    than racing the app's own heartbeat and passing when nothing was ever
    contended.

    Last-writer-wins, so calling this while a real seat is also heartbeating the
    same kind is genuine contention — which is the point for a preemption
    journey, and the reason to pick a kind no client runs (``backup-upload``,
    ``content-rescore``) when the journey instead wants a stand-in nothing
    fights over.
    """
    with _actor_client(nest_instance, user) as client:
        return client.call(
            "fauna.delegation.heartbeat",
            {
                "task_kind": task_kind,
                "holder": holder,
                "holder_class": holder_class,
            },
        )


def observe(nest_instance: dict, user: dict, task_kind: str | None = None) -> list[dict]:
    """The user's current lease snapshot, as any of their own devices reads it.

    Diagnostic, not an assertion target: what a journey asserts is what the
    *page* renders. This is what puts the nest's own view in a failure message
    when the two disagree (convention 6).
    """
    with _actor_client(nest_instance, user) as client:
        reply = client.call(
            "fauna.delegation.observe",
            {"task_kinds": [task_kind] if task_kind else []},
        )
    return reply.get("leases", [])


def age_lease(
    nest_instance: dict,
    user: dict,
    *,
    task_kind: str | None = None,
    age_ms: int = PAST_STALE_MS,
) -> list[dict]:
    """Back-date ``user``'s lease heartbeats so the nest reports them ``age_ms``
    old — "that device went away", as a state rather than a 90 s wait.

    ``task_kind`` ``None`` ⇒ every kind this user holds a slot for. Returns the
    ``[{"task_kind", "age_ms"}, …]`` the nest reports *after* the write, which
    the caller should assert against :data:`LEASE_STALE_MS`: an empty list means
    nothing was holding anything, and a journey that assumed otherwise would
    otherwise pass for the wrong reason.
    """
    body: dict = {"actor": user["actor_id_hex"], "age_ms": age_ms}
    if task_kind is not None:
        body["task_kind"] = task_kind
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/delegation/age",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())["aged"]


def age_past_stale(nest_instance: dict, user: dict, task_kind: str) -> None:
    """:func:`age_lease` for one kind, plus the assertion that it landed on the
    stale side — the one-liner a journey wants when "that device went away" is a
    precondition rather than the thing under test."""
    aged = age_lease(nest_instance, user, task_kind=task_kind)
    kinds = {row["task_kind"]: row["age_ms"] for row in aged}
    assert task_kind in kinds, (
        f"nothing held the {task_kind!r} lease, so aging it changed nothing and "
        f"the journey's premise is unmet; the nest reports "
        f"{observe(nest_instance, user)!r}"
    )
    assert kinds[task_kind] >= LEASE_STALE_MS, (
        f"the {task_kind!r} lease reads {kinds[task_kind]} ms old, still inside "
        f"the {LEASE_STALE_MS} ms freshness window — the age seam did not move "
        f"it across the boundary"
    )
