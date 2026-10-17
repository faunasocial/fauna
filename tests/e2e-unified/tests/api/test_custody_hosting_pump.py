"""What a nest holds for people elsewhere, once its hosting pump has really
pulled it — on a real ``fauna-nest`` binary, through the doors a host's app
and the owner's app ride.

Owner doc: ``docs/goal/architecture/account-replica-posture.md`` § The custody
grant + ceremony — the *two-sided bounds* bullet (the row's own allowance,
clamped; the host's tier bound, in a counter of its own the host's own files
never feel; the pump squeezing a later row to the headroom its earlier rows
leave), and the *reclaim* bullet (stop is a pause, remove is the reclaim and
drops the store only with the `(host, owner)` pair's last row, an expired row
is never dialled and its pair's store is reclaimed one 30-day grace window
later while the rows stay), and § Revocation (a revoked custody is no longer
fetched).

Every byte here was pulled for real. Each test's owner mints the custody
capability row on its own nest through the harness's
``fauna_ffi.harness_mint_custody`` (the ceremony's record → publish →
deposit order, over the account runtime's ledger door) and signs the witness
naming this nest's identity; the host deposits the hosting row over
``fauna.custody.hosting.register``; the NEST's pump then pulls the owner's
sealed planes as itself. Owner and host are two accounts of ONE private nest —
the pump dials the owner's nest at its own plaintext loopback URL, the shape
``test_custody_ceremony_journey.py::test_custody_ceremony_nest_anchored``
runs, and the reason this file uses ``domainless_nest``: a public deployment
refuses that URL at the register door.

Ports ``bins/fauna-nest/tests/conformance_custody_hosting_pump_client.rs``
(the pull with no host device, revoke severs) and the remove / stop / budget
scenarios of ``conformance_custody_hosting_client.rs`` onto the binary; the
tier squeeze and the host's-own-files half are new here. The pump is driven
only through its test hooks — ``run-now`` for a pass, ``hold`` so no scheduled
pass interleaves, ``clock`` to stand a month past an expiry — and every
assertion is on state, never timing (convention 14).
"""

import os
import secrets
import time

import pytest

import fauna_ffi
from clients._ws_rpc_core import RpcCallError
from common.auth import (
    create_actor_and_register,
    ensure_tier,
    set_tier_caps,
    set_user_tier,
    sync_changes_record,
    sync_register,
    user_create_folder,
)
from helpers.custody_hosting import Host, custody_hosting_hook, pump

pytestmark = pytest.mark.tier_3

#: `fauna_core::custody_ceremony::HOSTING_EXPIRED_STORE_GC_GRACE_SECS`.
GRACE_SECS = 30 * 24 * 3600


@pytest.fixture(scope="module")
def pump_nest(domainless_nest):
    """``domainless_nest`` with its periodic pump held for the module, so the
    only passes are this file's own ``run-now`` pokes, and the pump's clock
    restored to the wall clock at the end."""
    url = domainless_nest["url"]
    custody_hosting_hook(url, "hold", {"held": True})
    yield domainless_nest
    custody_hosting_hook(url, "clock", {"offset_secs": 0})
    custody_hosting_hook(url, "hold", {"held": False})


def _account(pump_nest) -> dict:
    return create_actor_and_register(
        pump_nest["port"], admin_signing_key=pump_nest["admin"]["signing_key"], base_url=pump_nest["url"])


def _on_own_tier(pump_nest, actor, **caps) -> str:
    """Move ``actor`` onto a fresh tier carrying ``caps``. Returns its name."""
    sk = pump_nest["admin"]["signing_key"]
    tier = f"e2e-custody-{secrets.token_hex(4)}"
    ensure_tier(pump_nest["port"], admin_signing_key=sk, name=tier, base_url=pump_nest["url"], **caps)
    set_user_tier(pump_nest["port"], actor["actor_id_hex"], tier, admin_signing_key=sk,
                  base_url=pump_nest["url"])
    return tier


def _pulled(pump_nest, host: Host, expect: int) -> dict[bytes, dict]:
    report = pump(pump_nest)
    assert report["pulled"] == expect and report["failed"] == 0, (
        f"every row of the pass must pull: {report!r}")
    rows = host.rows()
    assert all(r["held_bytes"] > 0 for r in rows.values()), (
        f"a pulled custody meters what it holds: {rows!r}")
    return rows


@pytest.mark.feature("admin-held-custody")
def test_removing_the_last_copy_for_a_person_drops_their_store_and_one_of_several_keeps_it(pump_nest):
    """Two copies held for one owner share one store: removing the first keeps
    it (the other copy still reads it and the next pass has nothing to fetch
    again), removing the last deletes it."""
    owner = _account(pump_nest)
    with Host(pump_nest, _account(pump_nest)) as host:
        first = host.custody(owner)
        last = host.custody(owner)
        held = _pulled(pump_nest, host, 2)[last]["held_bytes"]

        assert host.remove(first) == {"removed": True, "store_dropped": False}
        report = pump(pump_nest)
        assert report["pulled"] == 1 and report["recorded"] == 0, (
            f"the kept store still holds everything — nothing is fetched again: {report!r}")
        assert host.rows()[last]["held_bytes"] == held, "the remaining copy keeps its space"

        assert host.remove(last) == {"removed": True, "store_dropped": True}, (
            "removing the pair's last copy deletes the stored copy")
        assert host.rows() == {}


@pytest.mark.feature("admin-held-custody")
def test_a_paused_copy_keeps_its_space_until_it_is_removed(pump_nest):
    """Stop is a pause: the pump stops fetching, the metered bytes and the
    store stay; only remove frees them."""
    owner = _account(pump_nest)
    with Host(pump_nest, _account(pump_nest)) as host:
        g = host.custody(owner)
        held = _pulled(pump_nest, host, 1)[g]["held_bytes"]

        assert host.register(owner, g, stopped=True).get("ok") is True
        report = pump(pump_nest)
        assert report["stopped"] == 1 and report["pulled"] == 0, report
        row = host.rows()[g]
        assert row["stopped"] is True and row["held_bytes"] == held, (
            f"a paused copy keeps its space: {row!r}")

        assert host.remove(g) == {"removed": True, "store_dropped": True}, (
            "the paused copy's store was still there for remove to free")


@pytest.mark.feature("admin-held-custody")
def test_held_copies_are_bounded_by_the_hosts_tier_and_never_refuse_its_own_files(pump_nest):
    """What a member's held copies use counts against that member's tier
    storage allowance in a counter of its own: once it meets the bound a new
    copy is refused, and the member's own file writes still pass, because
    nothing held for others is charged to them."""
    owner, other_owner, member = _account(pump_nest), _account(pump_nest), _account(pump_nest)
    tier = _on_own_tier(pump_nest, member, max_storage_bytes=1024 * 1024 * 1024)
    with Host(pump_nest, member) as host:
        g = host.custody(owner)
        held = _pulled(pump_nest, host, 1)[g]["held_bytes"]

        # The admin lowers the member's allowance under what is already held.
        bound = held // 2
        set_tier_caps(pump_nest["port"], admin_signing_key=pump_nest["admin"]["signing_key"],
                      tier=tier, base_url=pump_nest["url"], max_storage_bytes=bound)
        with pytest.raises(RpcCallError) as refused:
            host.custody(other_owner)
        assert "custody-hosting storage bound reached" in str(refused.value.details), (
            refused.value.details)
        host.grants.pop()  # the refused deposit rests no row
        assert list(host.rows()) == [g], "the refused copy must rest no row"

        # The member's own files: a write well inside the allowance passes
        # though the held copies already exceed it.
        secret = bytes(member["signing_key"]).hex()
        device = secrets.token_hex(32)
        sync_register(pump_nest["port"], secret_key=secret, device_id=device, base_url=pump_nest["url"])
        folder = f"own-{secrets.token_hex(4)}"
        user_create_folder(pump_nest["port"], folder, secret_key=secret, base_url=pump_nest["url"])
        recorded = sync_changes_record(
            pump_nest["port"], secret_key=secret, folder=folder, device_id=device, path="mine.bin",
            manifest_hash=secrets.token_hex(32), size_bytes=min(100, bound // 4),
            base_url=pump_nest["url"])
        assert recorded.get("seq") is not None, recorded


@pytest.mark.feature("admin-held-custody")
def test_what_the_nest_holds_never_grows_past_the_allowance_set_for_it(pump_nest):
    """The row's own allowance binds what the pump keeps — lowered under what
    is held, the next pass evicts down to it — and the host's tier headroom
    binds a later copy: the first-registered copy keeps its space and the next
    is squeezed to what the tier leaves."""
    with Host(pump_nest, _account(pump_nest)) as host:
        owner = _account(pump_nest)
        g = host.custody(owner)
        held = _pulled(pump_nest, host, 1)[g]["held_bytes"]
        allowance = held // 2
        assert host.register(owner, g, cap=allowance).get("ok") is True
        report = pump(pump_nest)
        assert report["pulled"] == 1, report
        row = host.rows()[g]
        assert 0 < row["held_bytes"] <= allowance, (
            f"held {row['held_bytes']} past the allowance {allowance}: {row!r}")

    member = _account(pump_nest)
    tier = _on_own_tier(pump_nest, member, max_storage_bytes=1024 * 1024 * 1024)
    with Host(pump_nest, member) as host:
        # Grant ids ordered so the pass's (updated_at, grant_id) order is the
        # registration order even inside one second.
        first = host.custody(_account(pump_nest), grant_id=b"\x01" + os.urandom(15))
        first_held = _pulled(pump_nest, host, 1)[first]["held_bytes"]
        headroom = first_held // 2
        set_tier_caps(pump_nest["port"], admin_signing_key=pump_nest["admin"]["signing_key"],
                      tier=tier, base_url=pump_nest["url"], max_storage_bytes=first_held + headroom)
        later = host.custody(_account(pump_nest), grant_id=b"\xff" + os.urandom(15))
        report = pump(pump_nest)
        assert report["pulled"] == 2, report
        rows = host.rows()
        assert rows[first]["held_bytes"] == first_held, (
            f"the first-registered copy keeps its space: {rows!r}")
        assert rows[later]["held_bytes"] <= headroom, (
            f"the later copy must be squeezed to the {headroom} bytes the tier leaves: {rows!r}")


@pytest.mark.feature("admin-held-custody")
def test_an_expired_copy_is_no_longer_fetched_and_a_month_on_its_bytes_are_reclaimed(pump_nest):
    """Past the witness's expiry the pump never dials the row, and the store
    holds through one 30-day grace window; past that it is reclaimed while the
    row stays on the host's list."""
    url = pump_nest["url"]
    owner = _account(pump_nest)
    expires_in = 3600
    try:
        with Host(pump_nest, _account(pump_nest)) as host:
            g = host.custody(owner, expires_at_us=(int(time.time()) + expires_in) * 1_000_000)
            held = _pulled(pump_nest, host, 1)[g]["held_bytes"]

            custody_hosting_hook(url, "clock", {"offset_secs": 2 * expires_in})
            report = pump(pump_nest)
            assert report["expired"] == 1 and report["pulled"] == 0, report
            assert report["stores_reclaimed"] == 0, f"inside the grace the bytes stay: {report!r}"
            assert host.rows()[g]["held_bytes"] == held

            custody_hosting_hook(url, "clock", {"offset_secs": 2 * expires_in + GRACE_SECS})
            report = pump(pump_nest)
            assert report["expired"] == 1 and report["stores_reclaimed"] == 1, report
            assert g in host.rows(), "the expired row stays on the list"
            assert host.remove(g) == {"removed": True, "store_dropped": False}, (
                "the store was already reclaimed, so remove has nothing left to drop")
    finally:
        custody_hosting_hook(url, "clock", {"offset_secs": 0})


@pytest.mark.feature("admin-held-custody")
def test_when_the_owner_takes_the_permission_back_the_nest_stops_fetching(pump_nest):
    """The owner revokes through the shared revoke act: the next pass's fresh
    handshake is refused at the owner's nest, the row fails honestly, and
    nothing more is fetched into the store."""
    owner = _account(pump_nest)
    with Host(pump_nest, _account(pump_nest)) as host:
        g = host.custody(owner)
        held = _pulled(pump_nest, host, 1)[g]["held_bytes"]

        fauna_ffi.harness_revoke_custody(pump_nest["url"], bytes(owner["signing_key"]), g,
                                         host.custodian)
        report = pump(pump_nest)
        assert report["pulled"] == 0 and report["failed"] == 1, (
            f"a revoked custody is refused at connect and pulls nothing: {report!r}")
        assert host.rows()[g]["held_bytes"] == held, "the store gains nothing"
