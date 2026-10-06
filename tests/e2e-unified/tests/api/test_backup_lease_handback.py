"""A stalled nest hands the backup job back — full-stack (tier_3) API E2E.

Witnesses `task-delegation` outcome 11, *"A nest whose attempts keep failing
hands the job back to your devices"*
(``docs/goal/behavior/participants.md`` § Coordination primitive (Q-C)). Its
sibling outcome 10 — granting makes the nest the runner, revoking frees the
job at once — is witnessed by ``test_capability_rescore_drain.py``; the
*failing* half had no e2e at all, and it is the half that exists because the
other one is unsafe alone.

**Why the outcome exists.** Sufficiency for the ``backup-upload`` lease used
to be purely configuration-shaped: a granted ``NestBackupKey`` plus a
registered destination. A nest whose every sweep fails keeps satisfying that
and heartbeats the lease forever — and a client observing a fresh
``AlwaysOnNest`` holder stands down. So the owner's segments stop being backed
up by anyone while the nest's own row still says it runs the job. The third
conjunct (`BackupPassHealth`, ``bins/fauna-nest/src/segment_backup.rs``) makes
the predicate progress-shaped: after
``MAX_CONSECUTIVE_FAILED_PASSES`` (3) wholly-failed sweeps the nest releases
the lease and a device can take over.

**Two-way discriminator.** A single failed sweep must NOT hand the job back —
that is not a nicety, it is why the threshold is 3: the nest never preempts a
fresh foreign holder, so releasing on one blip parks the kind on whichever
client grabbed it until *that* holder goes stale (the constant carries a
compile-time assert saying so). So the test asserts the lease is still
nest-held after one failure and free only after the third.

**Latency-independent** (e2e-conventions.md convention 14). The sweeps are
causal barriers: ``POST /api/v1/test/backup/run-now`` runs exactly one sweep
*synchronously* and only then replies, so when it returns the pass has
definitively failed. The lease runner re-evaluates on its own
``HEARTBEAT_PERIOD_MS`` tick **or** when poked, and registering a destination
pokes it — so each observation is preceded by a poke and then a deadline poll
on *state*, never a fixed wait.

**This test owns its nest.** Its owner is enrolled with a destination that can
never be reached, so every sweep on this nest fails for as long as it lives —
which is exactly what must not be true of the shared session nest.

Process safety: no ``pkill``/``killall``; the nest is started through the
framework's dedicated-nest seam and torn down by this fixture.
"""

from __future__ import annotations

import secrets
import time

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from drivers.port_util import find_free_port

pytestmark = pytest.mark.tier_3

#: `segment_backup::MAX_CONSECUTIVE_FAILED_PASSES`.
MAX_CONSECUTIVE_FAILED_PASSES = 3
#: Generous budget for the runner's own re-evaluation after a poke. The runner
#: wakes on `delegation_runner_wake` (immediately) or its 30 s heartbeat, so
#: this is a failure bound, never a settle time.
LEASE_SETTLE_S = 45.0
#: `fauna_core::delegation::KIND_BACKUP_UPLOAD`.
BACKUP_UPLOAD = "backup-upload"


@pytest.fixture()
def stalling_nest(request, nest_mode, tmp_path_factory):
    """A fresh nest, exclusive to this module (see the docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "backup-handback-nest"
    )
    yield nest
    cleanup()


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _observe_backup_lease(nest, actor):
    """The owner's `backup-upload` lease as any of their own devices sees it
    (`fauna.delegation.observe`, self-scoped) — or None when free."""
    with _ws(nest, actor) as ws:
        reply = ws.call("fauna.delegation.observe", {"task_kinds": [BACKUP_UPLOAD]})
    leases = reply.get("leases", [])
    return leases[0] if leases else None


def _register_unreachable_destination(nest, actor, destination_id: str) -> None:
    """Register (or re-register) a peer-nest destination that can
    never be dialled. Re-registering is idempotent AND pokes
    `delegation_runner_wake`, which is how each observation below gets a prompt
    runner re-evaluation instead of waiting out a heartbeat."""
    with _ws(nest, actor) as ws:
        reply = ws.call(
            "fauna.backup.destination.register",
            {
                "destination_id": destination_id,
                # A port nothing is listening on. `find_free_port` picks one
                # the OS has free right now, so the dial fails to CONNECT
                # rather than reaching some other test's nest.
                "destination_nest_url": f"http://127.0.0.1:{find_free_port()}",
                "destination_nest_id": secrets.token_bytes(32).hex(),
                "kind": "nest",
            },
        )
    assert reply["ok"] is True, reply


def _sweep(nest) -> dict:
    """One synchronous nest-side backup pass — the causal barrier."""
    resp = requests.post(
        f"{nest['url']}/api/v1/test/backup/run-now", json={}, timeout=120
    )
    assert resp.status_code == 200, (
        f"test-hooks backup run-now returned {resp.status_code}: {resp.text}"
    )
    payload = resp.json()
    assert payload.get("ok") is True, payload
    assert payload.get("owners_run", 0) >= 1, (
        f"the sweep ran no owners ({payload!r}): this owner lacks either a "
        f"granted NestBackupKey or a registered destination, so no pass "
        f"happened and nothing could have failed."
    )
    return payload


def _poll_lease(nest, actor, *, want_held: bool, budget_s: float = LEASE_SETTLE_S):
    """Deadline poll on lease STATE (convention 14): returns the last observed
    lease once it matches `want_held`, else the last one seen."""
    deadline = time.monotonic() + budget_s
    last = None
    while True:
        last = _observe_backup_lease(nest, actor)
        if (last is not None) == want_held:
            return last
        if time.monotonic() >= deadline:
            return last
        time.sleep(0.5)


@pytest.mark.feature("task-delegation")
def test_repeated_failures_hand_the_backup_job_back(stalling_nest):
    """Enrol → the nest runs the job → three failed sweeps → it hands it back.

    The owner grants a ``NestBackupKey`` and registers a destination that can
    never be reached. That completes configuration-shaped sufficiency, so the
    nest claims the ``backup-upload`` lease and every device of the owner's
    sees an ``AlwaysOnNest`` holder and stands down. Each sweep then fails
    wholly (every ``(destination, kind)`` tuple errors on the dial), and on the
    third the nest stops claiming — the lease goes free and a device may take
    over.
    """
    owner = create_actor_and_register(
        stalling_nest["port"], admin_signing_key=stalling_nest["admin"]["signing_key"]
    )
    destination_id = f"handback-{secrets.token_hex(4)}"

    # ── Sufficiency: the seal grant + somewhere to send to ────────────
    with _ws(stalling_nest, owner) as ws:
        grant = ws.call(
            "fauna.backup.nest_key.grant", {"nest_backup_key": secrets.token_bytes(32)}
        )
    assert grant["ok"] is True, grant
    _register_unreachable_destination(stalling_nest, owner, destination_id)

    lease = _poll_lease(stalling_nest, owner, want_held=True)
    assert lease is not None, (
        "a nest holding the seal grant and a registered destination must claim "
        "the backup-upload lease — without that claim the rest of this test "
        "asserts nothing about handing anything back"
    )
    assert "Nest" in lease["holder"], (
        f"the granted nest must be the holder, got {lease!r}"
    )
    assert lease["holder_class"] == "AlwaysOnNest", lease

    # ── One failure is a blip, not a stall ────────────────────────────
    _sweep(stalling_nest)
    _register_unreachable_destination(stalling_nest, owner, destination_id)
    lease = _poll_lease(stalling_nest, owner, want_held=True, budget_s=5.0)
    assert lease is not None and "Nest" in lease["holder"], (
        "a single failed sweep must NOT hand the job back: the nest cannot "
        "preempt a fresh foreign holder, so releasing on one transient blip "
        f"parks the kind on whichever client takes it. Got {lease!r}"
    )

    # ── Sustained failure is a stall: the job goes back ───────────────
    for _ in range(MAX_CONSECUTIVE_FAILED_PASSES - 1):
        _sweep(stalling_nest)
    _register_unreachable_destination(stalling_nest, owner, destination_id)

    lease = _poll_lease(stalling_nest, owner, want_held=False)
    assert lease is None, (
        f"after {MAX_CONSECUTIVE_FAILED_PASSES} wholly-failed sweeps the nest "
        f"must hand the backup job back to the owner's devices, but it is "
        f"still claiming the lease: {lease!r}. A client observing this fresh "
        f"AlwaysOnNest holder stands down, so nobody would be backing this "
        f"owner up."
    )
