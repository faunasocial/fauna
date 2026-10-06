"""tier_3 e2e for the primary-domain-rename admin WS-RPC surface
(``fauna.bridges.{start,get_status,list,complete,abort,extend}_primary_domain_rename``).

Mission (``mail-primary-domain-rename.md``): the deployment's primary domain is
renameable entirely from a Fauna app over WS-RPC, no HTTP. This file proves the
wire path for the rename kinds — HTTP challenge/verify → bearer → WS subprotocol →
canonical DAG-CBOR Request encode → kind-routed dispatch → handler → Reply decode —
plus the Admin caller-class gate. The full state-machine business logic (flip,
grace watcher, abort inverse) is covered by the Rust handler + storage tests
(``bins/fauna-nest``); this file owns only the over-the-wire integration.

``nest_instance`` is session-scoped and **shared** with every other test, so these
assertions are deliberately **side-effect-free**: they read status/list, and prove
``start`` *reaches validation and refuses* a bogus target — they never actually
begin a rename (which would flip the deployment primary and pollute the shared
nest). The happy-path start→banner→abort flow is exercised UI-side in
``test_admin_dns.py`` against the driver, which aborts pre-flip.
"""

import secrets

import pytest

pytestmark = [pytest.mark.tier_3]


def _admin_client(nest_instance):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def test_rename_status_and_list_read_paths(nest_instance):
    """``get_primary_domain_rename_status`` + ``list_primary_domain_renames`` are
    registered, Admin-gated, and round-trip their Reply shapes. Pure reads — no
    rename is started, so the shared nest is untouched."""
    client = _admin_client(nest_instance)
    with client:
        status = client.call("fauna.bridges.get_primary_domain_rename_status", {})
        # The reply carries an optional ``rename`` (None when none is in flight).
        assert "rename" in status, f"status reply shape: {status!r}"

        listed = client.call("fauna.bridges.list_primary_domain_renames", {})
        assert isinstance(listed.get("renames"), list), (
            f"list reply must carry a ``renames`` array; got {listed!r}"
        )


@pytest.mark.feature("admin-dns-and-certificates")
def test_start_refuses_bogus_target_over_wire(nest_instance):
    """``start_primary_domain_rename`` reaches validation and refuses a
    ``new_primary_domain_id`` that matches no active additional — proving the kind
    is registered + the handler runs, WITHOUT beginning a rename (a bogus id can
    never flip the primary). Side-effect-free on the shared nest."""
    from clients.ws_rpc_admin_client import RpcCallError

    client = _admin_client(nest_instance)
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call(
                "fauna.bridges.start_primary_domain_rename",
                {
                    # 16 random bytes — no such domain_id exists.
                    "new_primary_domain_id": secrets.token_bytes(16),
                    "grace_days": 7,
                },
            )
    # The nest's two-step rule refuses a target that isn't a current additional
    # (mail-primary-domain-rename.md § RPC refusal codes). Match by substring so a
    # ``fauna.bridges.`` prefix (as on ``permission_denied``) still passes.
    assert "new_primary_must_be_additional" in excinfo.value.code, (
        f"bogus target must be refused as not-an-additional; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_non_admin_denied_on_rename_kind(nest_instance, test_user):
    """A User-class actor is rejected by the allowlist before any rename work."""
    from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.get_primary_domain_rename_status", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User-class actor on an Admin kind must be denied; got {excinfo.value.code!r}"
    )
