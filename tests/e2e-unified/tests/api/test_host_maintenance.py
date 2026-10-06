"""Host-OS maintenance over WS-RPC — the nest-side contract.

installers/vps.md § Host OS Maintenance §§ 3–4: an onboarded VPS host runs a
`fauna-reboot-coordinator` that publishes apt/reboot state via `host-status` into
the root-owned `:ro` `/data/maintenance-host` mount, and the nest surfaces it on
`fauna.setup.status` as the `os_*` fields so the admin client can render a
patch/reboot indicator. The admin's "restart now" affordance
(`fauna.admin.request_host_restart`) writes a `restart-requested` flag into the rw
uid-1000 `/data/maintenance` mount the coordinator picks up. The channel is split
by trust direction so a compromised nest can't forge the host status (HM-1/HM-2).

This isolates the **nest side** of that flow: it injects a
`host-status` file into the real nest's host-state dir and drives the kinds
directly over `WsRpcAdminClient`, so an indicator-empty or restart-now failure can
be triaged nest-side vs client-side. Green here means the nest reads the channel
and serves the contract; the UI render is `tests/test_host_maintenance.py`.

tier_3: a real `fauna-nest` binary. The session nest has no host maintenance
channel by default (only a VPS cloud-init creates the two mount dirs), so the
test creates + tears down them around each assertion.
"""
import shutil
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = pytest.mark.tier_3


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _maintenance_dir(nest_instance) -> Path:
    """`<data_dir>/maintenance` (rw, uid-1000) — the nest writes `nest-readiness`
    and `restart-requested` here; `<data_dir>` is the dir holding the SQLite file."""
    return Path(nest_instance["db_path"]).parent / "maintenance"


def _host_state_dir(nest_instance) -> Path:
    """`<data_dir>/maintenance-host` (`:ro`, root-owned on a real VPS) — the host
    coordinator writes `host-status` here and the nest only reads it. Split from
    the rw `maintenance` dir by trust direction (the HM-1/HM-2 fix)."""
    return Path(nest_instance["db_path"]).parent / "maintenance-host"


@pytest.mark.feature("admin-nest")
def test_host_maintenance_status_and_restart(nest_instance):
    """End-to-end nest contract for both directions of the (trust-split) channel:

    1. With **no** channel (the default session nest), `setup.status` reports the
       os_* fields at their "nothing pending" defaults (the version-skew-safe
       state) and `request_host_restart` is rejected `no_host` (no host to reboot).
    2. With a `host-status` file injected into the **host-state** (`:ro`) dir,
       `setup.status` reflects the parsed os_* values.
    3. With the rw `maintenance` dir present, `request_host_restart` writes the
       `restart-requested` flag there for the host coordinator to consume.
    """
    maint = _maintenance_dir(nest_instance)
    host_state = _host_state_dir(nest_instance)
    # Clean slate regardless of any prior test on the shared session nest.
    shutil.rmtree(maint, ignore_errors=True)
    shutil.rmtree(host_state, ignore_errors=True)
    try:
        with _admin_client(nest_instance) as admin:
            # (1) No channel → defaults + restart-now rejected.
            status = admin.call("fauna.setup.status", {})
            assert status["os_security_updates_pending"] == 0
            assert status["os_reboot_pending"] is False
            # Optionals stay unset when no reboot has been deferred and nothing has
            # been patched (no update channel).
            assert not status.get("os_reboot_deferred_since")
            assert not status.get("os_last_patched_at")
            with pytest.raises(RpcCallError) as excinfo:
                admin.call("fauna.admin.request_host_restart", {})
            assert excinfo.value.code == "fauna.host_maintenance.no_host"

            # (2) Inject host-status into the host-state (:ro) dir → setup.status
            # reflects it. A host-status planted in the rw `maintenance` dir would
            # NOT be trusted (the nest only reads the host-state dir).
            host_state.mkdir(parents=True, exist_ok=True)
            (host_state / "host-status").write_text(
                "security_updates_pending=3\n"
                "reboot_pending=true\n"
                "reboot_deferred_since=1719500000\n"
                "last_patched_at=1719400000\n"
            )
            status = admin.call("fauna.setup.status", {})
            assert status["os_security_updates_pending"] == 3
            assert status["os_reboot_pending"] is True
            assert status["os_reboot_deferred_since"] == 1719500000
            assert status["os_last_patched_at"] == 1719400000

            # (3) With the rw maintenance dir present, restart-now writes the flag
            # the coordinator consumes (into the rw dir, not the host-state dir).
            maint.mkdir(parents=True, exist_ok=True)
            reply = admin.call("fauna.admin.request_host_restart", {})
            assert reply["ok"] is True
            assert (maint / "restart-requested").exists(), (
                "request_host_restart must write the restart-requested flag into "
                "the rw maintenance mount"
            )
    finally:
        shutil.rmtree(maint, ignore_errors=True)
        shutil.rmtree(host_state, ignore_errors=True)


@pytest.mark.feature("admin-nest")
def test_os_fields_admin_gated_off_anonymous(nest_instance):
    """OS-LEAK (2026-06-28 second-pass review): the host-OS `os_*` patch-state
    fields are admin-only. `fauna.setup.status` is a fully-anonymous, unthrottled
    pre-identity discovery kind, so serving host patch/reboot posture on it makes
    every box a fleet-harvestable targeting oracle (an unpatched-kernel-with-reboot-
    pending list, each paired with its `domain`). The fix gates the four `os_*`
    fields on an authenticated admin caller; an anonymous (no-bearer) caller must
    get the default "nothing pending" values even when the host coordinator has
    published real state. The wire fields stay additive — only their population is
    gated (`installers/vps.md` § Host OS Maintenance; `nest/common.md` §
    `fauna.setup.status`).
    """
    host_state = _host_state_dir(nest_instance)
    shutil.rmtree(host_state, ignore_errors=True)
    try:
        # Publish real host-OS state (a reboot pending with security updates) into
        # the :ro host-state dir the nest reads.
        host_state.mkdir(parents=True, exist_ok=True)
        (host_state / "host-status").write_text(
            "security_updates_pending=7\n"
            "reboot_pending=true\n"
            "reboot_deferred_since=1719500000\n"
            "last_patched_at=1719400000\n"
        )

        # Admin (authenticated) sees the real values.
        with _admin_client(nest_instance) as admin:
            status = admin.call("fauna.setup.status", {})
            assert status["os_security_updates_pending"] == 7
            assert status["os_reboot_pending"] is True
            assert status["os_reboot_deferred_since"] == 1719500000
            assert status["os_last_patched_at"] == 1719400000

        # Anonymous (no bearer) gets the default "nothing pending" — the host
        # posture is NOT disclosed pre-auth.
        with WsRpcAnonClient(nest_instance["url"]) as anon:
            status = anon.call("fauna.setup.status", {})
            assert status["os_security_updates_pending"] == 0, (
                "anonymous setup.status must not disclose host security-update counts"
            )
            assert status["os_reboot_pending"] is False, (
                "anonymous setup.status must not disclose a pending host reboot"
            )
            assert not status.get("os_reboot_deferred_since"), (
                "anonymous setup.status must not disclose reboot-deferral age"
            )
            assert not status.get("os_last_patched_at"), (
                "anonymous setup.status must not disclose patch cadence"
            )
    finally:
        shutil.rmtree(host_state, ignore_errors=True)
