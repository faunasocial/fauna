"""Tests for ``clients.ws_rpc_anon_client.WsRpcAnonClient``.

The anonymous client drives nest's pre-identity onboarding kinds over the
bearer-less ``GET /api/v1/ws`` connection — the first Python consumer of the
anonymous WS surface. It is the test-side analogue of the Rust
``libs/fauna-anon-client`` connector the onboarding/launch state machines ride,
and the keystone that lets the tier_3 e2e suite drive claim-admin /
invite-request / storage-mode *before a bearer exists* (the bearer-only
:class:`WsRpcAdminClient` cannot — its challenge/verify needs an already-claimed
admin). Tracked internally, § S4e0.

Two assertions:

* True round-trip with ``ok=true`` via ``fauna.setup.status`` (an allowlisted
  pre-identity kind needing no signature). Drives every layer of the client:
  bare-``fauna.v1`` WebSocket handshake → canonical DAG-CBOR Request encode →
  correlation-id-matched Reply decode → reply payload returned.
* The pre-identity allowlist gate — an *off*-allowlist kind
  (``fauna.protocol.echo``, open to bearer callers but not anonymous ones) is
  answered ``fauna.protocol.unauthenticated`` with the connection kept open,
  proving the connection is genuinely anonymous and the gate is wired
  (``bins/fauna-nest/src/pre_identity_allowlist.rs``).
"""

import re
import secrets

import pytest

pytestmark = [pytest.mark.tier_3]


def test_anon_client_round_trips_setup_status(nest_instance):
    """Smoke test — full round-trip with ok=true via fauna.setup.status.

    ``nest_instance`` is claimed by default (admin exists, claim-code
    consumed), so the reply must report ``claimed=True``. ``node_mode`` may be
    ``None`` (fixtures start Unconfigured) — we only assert the key is present
    and the call decoded, not a specific mode.
    """
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_instance["url"]) as anon:
        status = anon.call("fauna.setup.status", {})

    assert isinstance(status, dict), f"setup.status reply not a map: {status!r}"
    assert status.get("claimed") is True, (
        f"nest_instance is claimed; setup.status should report claimed=true, got {status!r}"
    )
    assert "node_mode" in status, f"setup.status must carry a node_mode key: {status!r}"
    assert status.get("version"), f"setup.status must carry a version: {status!r}"
    assert "domain" in status, f"setup.status must carry a domain key: {status!r}"


def test_anon_client_off_allowlist_kind_is_unauthenticated(nest_instance):
    """The anonymous connection routes only the fixed pre-identity allowlist.

    ``fauna.posts.get`` is reachable for an authenticated caller but is
    *not* on the anonymous allowlist, so the dispatcher answers
    ``fauna.protocol.unauthenticated`` (and keeps the connection open). This is
    the negative proof that the connection carries no bearer.

    A kind every build registers, never ``fauna.protocol.echo``: echo exists
    only in a ``test-hooks`` build, so naming it would deselect this proof from
    every run against a shipped artifact (exclusion class (6)), where the
    anonymous gate matters most.
    """
    from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

    with WsRpcAnonClient(nest_instance["url"]) as anon:
        with pytest.raises(RpcCallError) as excinfo:
            anon.call("fauna.posts.get", {"post_id": secrets.token_bytes(16)})
    assert excinfo.value.code == "fauna.protocol.unauthenticated", (
        f"off-allowlist kind on the anonymous WS should be unauthenticated, "
        f"got {excinfo.value.code}"
    )


def _allowlisted_kinds() -> list[str]:
    """Every kind ``is_pre_identity_kind`` names, read from its own source.

    Deliberately **shape-form, not a hand list** (the ratchet lesson from
    `merge-gate-check.md` § Feature scope): a list maintained here would be a second,
    silently-diverging copy of the allowlist, and the drift it exists to catch
    is exactly an entry appearing or vanishing. Parsing the function body means
    a kind cannot enter the allowlist without entering this test.
    """
    from pathlib import Path

    src = (
        Path(__file__).resolve().parents[3]
        / "bins/fauna-nest/src/pre_identity_allowlist.rs"
    ).read_text()
    start = src.index("pub fn is_pre_identity_kind")
    # The arms live in the single `matches!`/`match` body that opens the fn; the
    # unit tests below it also mention kind strings, so stop at the fn's end.
    end = src.index("\n}\n", start)
    body = src[start:end]
    kinds = re.findall(r'"(fauna\.[a-z0-9_.]+)"', body)
    assert len(kinds) >= 20, (
        "parsed implausibly few kinds from is_pre_identity_kind — the function's "
        f"shape changed and this parser needs updating; got {kinds!r}"
    )
    return sorted(set(kinds))


def test_no_allowlist_entry_names_a_kind_the_nest_does_not_serve(nest_instance):
    """Every pre-identity entry must name a kind the router actually serves.

    **Read the scope before trusting this test.** It derives its kind list from
    ``is_pre_identity_kind``'s own source, so it is *structurally incapable* of
    noticing an entry being deleted — the entry would leave the gate and the
    fixture in the same edit, which is the self-consistent-fixture blindness
    `docs/goal/architecture/e2e-conventions.md` § point 14 warns about. That
    direction (membership: *should* this kind be pre-identity) is covered
    elsewhere and deliberately not here: `pre_identity_allowlist.rs`'s own unit
    tests pin 13 kinds by name, and the tier_3 journeys pin the recovery ones by
    using them (`test_recovery_pre_identity_remedies.py`,
    `test_recovery_kit_restore.py`, `test_succession_fixture.py` — each reds if
    its kind leaves the list, mutation-proven 2026-08-09).

    What is left for this test is the drift nothing else sees: an entry that
    outlives its kind. The allowlist is a **security boundary**, and a stale
    name on it is a standing anonymous door held open for a kind that does not
    exist today and may be reintroduced under that name tomorrow — by someone
    who has no reason to look here. Renaming or retiring a kind without touching
    this file therefore has to fail somewhere, and this is the somewhere.

    The assertion is exactly one thing: the answer must not be
    ``unknown_kind``. Everything else is legitimate — an empty payload earns
    ``invalid_params`` from most handlers, the loopback-only bridge enrollment
    earns ``remote_enrollment_unsupported``, a throttled kind may earn a
    throttle refusal, and the read-only kinds simply succeed. ``unauthenticated``
    is checked too, for free, though by the note above it cannot realistically
    fire.
    """
    from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

    unserved, gate_refused = [], []
    for kind in _allowlisted_kinds():
        try:
            with WsRpcAnonClient(nest_instance["url"]) as anon:
                anon.call(kind, {})
        except RpcCallError as exc:
            if exc.code == "fauna.protocol.unknown_kind":
                unserved.append(kind)
            elif exc.code == "fauna.protocol.unauthenticated":
                gate_refused.append(kind)

    assert not unserved, (
        "these names are on the pre-identity allowlist but the nest serves no "
        "such kind — a stale entry is an anonymous door held open for a name "
        f"nothing implements; drop them from the allowlist: {unserved}"
    )
    assert not gate_refused, (
        "on the allowlist yet refused by the anonymous connection gate — the "
        f"parsed list and the compiled gate disagree: {gate_refused}"
    )
