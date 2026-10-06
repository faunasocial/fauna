"""E2E API test (tier_3): the task-delegation heartbeat lease
`fauna.delegation.{heartbeat,observe}`.

Drives the advisory lease plane directly against a real `fauna-nest` binary (no
browser). The transport half of the coordination primitive
(`docs/goal/behavior/participants.md` § Coordination primitive; design
tracked internally): the nest is a **dumb,
per-actor, last-writer-wins blackboard** — a `heartbeat` records `holder =
caller` unconditionally, `observe` returns the current snapshot with a
nest-computed `age_ms`, and all convergence logic is client-side. The
staleness→takeover *decision* is proven at tier_1
(`fauna_core::delegation::decide`); this test proves the **wire + LWW store +
observe snapshot** end-to-end (the e2e WS client can't receive Push frames, so
`observe` is request/reply — the `lease_changed` push is a best-effort nudge
covered by the nest-side handler unit test).

This is the API-contract carve-out (E2E rule 8a — a tier_3 `tests/api/` test
whose purpose *is* the wire contract, like `test_mls_replica_sync.py`).

Wire (verified against `bins/fauna-nest/src/delegation_handlers.rs` +
`libs/fauna-protocol/src/delegation.rs`):

* ``fauna.delegation.heartbeat`` — ``{"task_kind": str, "holder": ParticipantRef,
  "holder_class": str}`` → ``{"lease": LeaseState}``
* ``fauna.delegation.observe`` — ``{"task_kinds": [str]}`` (``[]`` ⇒ all) →
  ``{"leases": [LeaseState]}``

where ``ParticipantRef`` is the externally-tagged serde enum
``{"Device": {"device_id": str}}`` (holders are always Devices here —
backup-upload's runner is always a client), ``holder_class`` is the unit-variant
string ``"PluggedInDesktop"``, and ``LeaseState`` carries
``task_kind``/``holder``/``holder_class``/``age_ms``.

Both kinds are ``User``-class; the connection actor is the owner (no actor_id on
the wire), so a caller reads/writes only their own leases.
"""

import time

import pytest

from common import create_actor_and_register
from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3

DESKTOP = "PluggedInDesktop"


def _client(node_url: str, actor: dict) -> WsRpcAdminClient:
    """A fresh User-class WS-RPC client for ``actor`` — two clients for the same
    actor model two distinct *devices* (distinct connections, one identity)."""
    return WsRpcAdminClient(
        node_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _device(device_id: str) -> dict:
    """The externally-tagged `ParticipantRef::Device` wire shape."""
    return {"Device": {"device_id": device_id}}


def _heartbeat(client, task_kind: str, device_id: str, cls: str = DESKTOP) -> dict:
    return client.call(
        "fauna.delegation.heartbeat",
        {"task_kind": task_kind, "holder": _device(device_id), "holder_class": cls},
    )


def _observe(client, kinds: list | None = None) -> list:
    return client.call("fauna.delegation.observe", {"task_kinds": kinds or []})["leases"]


@pytest.mark.feature("task-delegation")
def test_lease_acquire_observe_takeover(nest_instance):
    """Device A acquires; device B (same actor, distinct connection) observes A
    holding it, then takes over last-writer-wins — the mechanics a stale-takeover
    drives (the *when* is the tier_1 `decide` logic)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _client(node_url, actor) as device_a:
        lease = _heartbeat(device_a, "backup-upload", "dev-a")["lease"]
        assert lease["holder"] == _device("dev-a")
        assert lease["task_kind"] == "backup-upload"
        assert lease["age_ms"] == 0, "a just-written lease is age 0"

    with _client(node_url, actor) as device_b:
        leases = _observe(device_b, ["backup-upload"])
        assert len(leases) == 1
        assert leases[0]["holder"] == _device("dev-a"), "B observes A holding the lease"
        assert leases[0]["holder_class"] == DESKTOP

        # B takes over — the nest is a dumb LWW store, so the write always lands.
        _heartbeat(device_b, "backup-upload", "dev-b")
        leases = _observe(device_b, ["backup-upload"])
        assert leases[0]["holder"] == _device("dev-b"), "last writer wins"


def test_observe_age_grows_and_kinds_isolated(nest_instance):
    """`age_ms` is nest-computed and grows with elapsed time; observe-all returns
    every kind, a filter narrows, and a kind with no lease is simply absent."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _client(node_url, actor) as dev:
        _heartbeat(dev, "backup-upload", "dev-a")
        _heartbeat(dev, "index", "dev-a")

        time.sleep(1.0)
        leases = {lease["task_kind"]: lease for lease in _observe(dev)}
        assert set(leases) == {"backup-upload", "index"}, "observe-all returns both kinds"
        assert leases["backup-upload"]["age_ms"] >= 900, (
            f"age tracks elapsed time, got {leases['backup-upload']['age_ms']}ms"
        )

        only = _observe(dev, ["index"])
        assert len(only) == 1 and only[0]["task_kind"] == "index", "filter narrows to one kind"
        assert _observe(dev, ["content-rescore"]) == [], "a kind with no lease is absent"


@pytest.mark.feature("task-delegation")
def test_renew_resets_age_keeping_a_live_holder_fresh(nest_instance):
    """A holder's periodic heartbeat resets ``age_ms`` toward 0, so a live runner
    never crosses the staleness threshold and is never preempted — the anti-churn
    half of the sticky rule (`fauna_core::delegation::decide`: a fresh self-lease
    ⇒ `Renew`). This is the wire mechanic the shared `LeaseCoordinator` loop
    relies on: it re-heartbeats every ``HEARTBEAT_PERIOD_MS`` so its own lease
    stays well under ``LEASE_STALE_MS``. Proves the LWW store's ``age_ms`` is
    per-*write*, not per-first-claim."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _client(node_url, actor) as dev:
        _heartbeat(dev, "backup-upload", "dev-a")
        time.sleep(1.0)
        aged = _observe(dev, ["backup-upload"])[0]["age_ms"]
        assert aged >= 900, f"age accrues between heartbeats, got {aged}ms"

        # A renew heartbeat resets the clock — the holder stays fresh, so a
        # co-tier peer keeps yielding (sticky) instead of taking over.
        _heartbeat(dev, "backup-upload", "dev-a")
        renewed = _observe(dev, ["backup-upload"])[0]["age_ms"]
        assert renewed < 500, f"a renew resets age toward 0, got {renewed}ms"


@pytest.mark.feature("task-delegation")
def test_leases_are_actor_scoped(nest_instance):
    """A caller sees only its own actor's leases (self-scoped from the
    connection; no actor_id on the wire)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    actor_1 = create_actor_and_register(port, admin_signing_key=admin_sk)
    actor_2 = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _client(node_url, actor_1) as c1:
        _heartbeat(c1, "backup-upload", "dev-a")
    with _client(node_url, actor_2) as c2:
        assert _observe(c2) == [], "leases are per-actor — actor 2 sees none of actor 1's"
