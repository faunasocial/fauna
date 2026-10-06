"""tier_3 e2e: the admin Aliases page (`admin-aliases`) — external forwarders
(`mail-aliases.md` § Kind 7 / `admin.md` § 4).

The admin maps an address on a hosted local domain to an external destination
with no local mailbox (`info@<domain>` → `oldaccount@elsewhere`). This drives the
full client→nest path through the shared ``ForwarderMachine``
(``libs/fauna-client-mail-settings::forwarders``) over the Admin-class
``fauna.bridges.{create,list,delete}_forwarder`` WS-RPC kinds — no HTTP twin.

A local domain is seeded over the admin WS-RPC control plane first (a forwarder
must live on a hosted domain, and the add-form domain picker only offers hosted
domains). The forward *target* must NOT be a hosted domain (``validate_forward_
target``), so we use an off-deployment address.

``nest_instance`` is session-scoped (driver cached across tests), so we seed a
unique domain + local-part and assert *that* forwarder is present/absent rather
than an absolute count.
"""

import secrets
import time

import pytest

pytestmark = [pytest.mark.tier_3]

_DOMAIN = f"fwd-{secrets.token_hex(4)}.test"
_LOCAL_PART = "info"
_TARGET = f"yourname-{secrets.token_hex(3)}@elsewhere-example.test"
_ADDRESS = f"{_LOCAL_PART}@{_DOMAIN}"


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane so the add-form
    domain picker offers it. Mirrors ``test_admin_dns.py::_seed_local_domain``."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": domain,
                "mta_sts_cert_mode": "expand_primary",
            },
        )


@pytest.mark.feature("admin-forwarders")
def test_admin_aliases_forwarder_create_and_delete(admin_app, nest_instance):
    """Create an external forwarder on a hosted domain, see it render in the
    forwarder list with the right address + target, then delete it and see it
    drop out — the full create→list→delete round-trip over the shared
    ``ForwarderMachine`` (`fauna.bridges.{create,list,delete}_forwarder`)."""
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_aliases()

    # add_forwarder retries the domain select internally until the add-form
    # offers it — the hosted-domain picker's own AdminForwardersLoaded fetch
    # (list_forwarders + list_local_domains) can still be in flight.
    admin_app.admin.add_forwarder(_DOMAIN, _LOCAL_PART, _TARGET)

    # Poll for the new forwarder row (the create dispatch is an async WS-RPC
    # round-trip + a Refresh).
    deadline = time.time() + 12.0
    addresses: list[str] = []
    while time.time() < deadline:
        addresses = admin_app.admin.forwarder_addresses()
        if _ADDRESS in addresses:
            break
        time.sleep(0.5)

    err = admin_app.admin.forwarders_action_error_text()
    assert _ADDRESS in addresses, (
        f"forwarder {_ADDRESS!r} not found among rows {addresses!r} "
        f"(action error: {err!r})"
    )
    # The paired target renders on the same row.
    targets = admin_app.admin.forwarder_targets()
    idx = addresses.index(_ADDRESS)
    assert idx < len(targets) and targets[idx] == _TARGET, (
        f"forwarder target mismatch: row {idx} targets={targets!r}, want {_TARGET!r}"
    )

    # Delete it; the row drops out of the list.
    admin_app.admin.delete_forwarder(index=idx)
    deadline = time.time() + 12.0
    while time.time() < deadline:
        if _ADDRESS not in admin_app.admin.forwarder_addresses():
            break
        time.sleep(0.5)
    assert _ADDRESS not in admin_app.admin.forwarder_addresses(), (
        f"forwarder {_ADDRESS!r} still present after delete "
        f"({admin_app.admin.forwarder_addresses()!r})"
    )


@pytest.mark.feature("admin-forwarders")
def test_admin_aliases_forwarder_to_hosted_domain_rejected(admin_app, nest_instance):
    """A forward *target* on a hosted domain is rejected (`validate_forward_
    target` — that's an alias, not a forward); the failure surfaces on
    `admin-aliases-action-error`, not the app-wide banner."""
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_aliases()

    # Target on the SAME hosted domain — a same-deployment alias, not a forward.
    # add_forwarder retries the domain select internally (see the other test).
    admin_app.admin.add_forwarder(_DOMAIN, "sales", f"someone@{_DOMAIN}")

    deadline = time.time() + 12.0
    err = ""
    while time.time() < deadline:
        err = admin_app.admin.forwarders_action_error_text()
        if err:
            break
        time.sleep(0.5)
    assert err, "expected an action error for a hosted-domain forward target"
    assert f"sales@{_DOMAIN}" not in admin_app.admin.forwarder_addresses(), (
        "the rejected forwarder must not appear in the list"
    )
