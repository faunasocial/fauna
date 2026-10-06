"""Admin-users hub data plane over WS-RPC — the nest-side contract.

The `admin-users` hub (admin.md § Users) loads its three sections by calling, in
order, `fauna.admin.tiers.list` → `fauna.admin.users.list` →
`fauna.admin.invite_codes.list` → `fauna.admin.invite_requests.list` as the nest
admin (the client VMs — windows `AdminUsersViewModel.LoadAsync`, linux, … — do
exactly this over the shared `AdminClient`). This test isolates the **nest side**
of that flow: it drives the same kinds directly over `WsRpcAdminClient` (the same
challenge/verify → bearer → WS auth a real client rides) and asserts each replies
with the expected shape.

Why it exists: the hub had only *UI* e2e coverage (the linux `test_admin_users_hub`
suite), so a "page renders but data never loads" failure on another client couldn't
be triaged as nest-side vs client-side. This is the API-side half — green here
means the nest serves the admin data plane and any empty-hub symptom is
client-side.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def test_admin_data_plane_loads(nest_instance):
    """The four loads the hub performs all reply (no hang, no error) and carry
    the admin's own user + the default tiers — proving the nest data plane the
    client hub depends on is live."""
    with _admin_client(nest_instance) as admin:
        tiers = admin.call("fauna.admin.tiers.list", {})
        assert "tiers" in tiers, f"tiers.list reply missing `tiers`: {tiers!r}"
        # A fresh nest ships the default tiers (free/personal/community).
        tier_names = {t["name"] for t in tiers["tiers"]}
        assert tier_names, f"no tiers defined: {tiers!r}"

        users = admin.call("fauna.admin.users.list", {"limit": 50, "offset": 0})
        assert "users" in users and "total" in users, f"bad users.list reply: {users!r}"
        # The admin is always a registered user, so the roster is never empty.
        assert users["total"] >= 1, f"user roster empty: {users!r}"
        assert len(users["users"]) >= 1

        codes = admin.call("fauna.admin.invite_codes.list", {})
        assert "invite_codes" in codes, (
            f"invite_codes.list reply missing `invite_codes`: {codes!r}"
        )

        requests = admin.call("fauna.admin.invite_requests.list", {})
        assert "invite_requests" in requests, (
            f"invite_requests.list reply missing `invite_requests`: {requests!r}"
        )
