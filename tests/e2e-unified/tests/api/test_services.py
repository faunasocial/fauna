"""Tests for the service-management admin WS-RPC kinds (`fauna.admin.services.*`).

The HTTP twins (`GET /admin/api/services`, `PUT /admin/api/services/{name}`) were
ripped in the WS-RPC-everywhere cut; the canonical surface is now
`fauna.admin.services.{list,update}` (`bins/fauna-nest/src/admin_ws_handlers.rs`,
Admin-class). The on-disk `services.json` intent file is unchanged — the update
handler still writes it.
"""

import json
import os

from common import create_actor_and_register
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def services_nest(request, nest_mode, tmp_path_factory):
    """A fresh claimed nest for one service-management test.

    Zero start options: every one of these tests wants nothing but a nest that
    is up and claimed, which is why the family routes through the mode provider
    rather than compiling a binary of its own.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "services-nest")
    yield nest
    cleanup()


def _admin_client(nest) -> WsRpcAdminClient:
    """An Admin-class WS-RPC client for the nest's claimed admin."""
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


class TestServiceManagement:
    """Service intent file management via the admin WS-RPC data plane."""

    def test_get_services_returns_all_false_by_default(self, services_nest):
        nest = services_nest
        with _admin_client(nest) as admin:
            data = admin.call("fauna.admin.services.list", {})
        assert data["version"] == 1
        assert data["services"]["bridge"] is False
        assert "dns" not in data["services"]  # vestigial flag dropped
        # The retired algorithm flag left the wire with its sidecar (2026-10-01).
        assert "algorithm" not in data["services"]

    def test_enable_and_disable_bridge_service(self, services_nest):
        nest = services_nest
        with _admin_client(nest) as admin:
            # Enable bridge
            data = admin.call(
                "fauna.admin.services.update",
                {"name": "bridge", "enabled": True},
            )
            assert data["ok"] is True
            assert data["service"] == "bridge"
            assert data["enabled"] is True

            # Verify via list
            data = admin.call("fauna.admin.services.list", {})
            assert data["services"]["bridge"] is True

            # Disable bridge
            data = admin.call(
                "fauna.admin.services.update",
                {"name": "bridge", "enabled": False},
            )
            assert data["enabled"] is False

            data = admin.call("fauna.admin.services.list", {})
            assert data["services"]["bridge"] is False

    def test_unknown_service_is_rejected(self, services_nest):
        nest = services_nest
        with _admin_client(nest) as admin:
            with pytest.raises(RpcCallError) as excinfo:
                admin.call(
                    "fauna.admin.services.update",
                    {"name": "unknown", "enabled": True},
                )
        assert excinfo.value.code == "fauna.admin.invalid_params"
        assert "unknown service" in str(excinfo.value.details)

    def test_retired_algorithm_service_is_rejected(self, services_nest):
        """The `algorithm` name went with the sidecar it gated: it is refused
        like any unknown service (core-client-kind-catalog.md § Algorithm &
        Reputation)."""
        nest = services_nest
        with _admin_client(nest) as admin:
            with pytest.raises(RpcCallError) as excinfo:
                admin.call(
                    "fauna.admin.services.update",
                    {"name": "algorithm", "enabled": True},
                )
        assert excinfo.value.code == "fauna.admin.invalid_params"

    def test_non_admin_caller_is_denied(self, services_nest):
        """A registered non-admin User cannot drive the admin services kinds.

        WS-RPC successor to the twin's "unauthenticated → 401": there is no
        unauthenticated call (the WS handshake is signature-authed), so the
        meaningful negative path is the caller-class gate — a User-class actor
        invoking an Admin-only kind is rejected with `fauna.admin.permission_denied`
        (the central capability gate derives the family code, routes.rs gate 1d /
        `bridge_method_allowlist::class_refusal_namespace`, ruled 2026-08-17 —
        api-layers.md § Caller-class authorization → Refusal codes at the gate;
        `admin_ws_handlers::require_permission` remains the defense-in-depth twin).
        """
        nest = services_nest
        port = nest["port"]
        user = create_actor_and_register(
            port, admin_signing_key=nest["admin"]["signing_key"]
        )
        user_client = WsRpcAdminClient(
            nest["url"],
            actor_id=user["actor_id_bytes"],
            signing_key=bytes(user["signing_key"]),
        )
        with user_client as c:
            with pytest.raises(RpcCallError) as excinfo:
                c.call("fauna.admin.services.list", {})
        assert excinfo.value.code == "fauna.admin.permission_denied"

    def test_services_json_file_exists(self, services_nest):
        """The intent file is written to disk at boot."""
        nest = services_nest
        services_path = os.path.join(nest["tmp_dir"], "services.json")
        assert os.path.exists(services_path)
        with open(services_path) as f:
            data = json.load(f)
        assert "version" in data
        assert "services" in data
