"""tier_3 e2e: the per-domain catch-all actor picker on admin-dns
(``admin-dns-domain-catch-all-select``).

The admin designates one actor as a domain's catch-all — unmatched inbound mail
on that domain routes to it (``mail-aliases.md`` § Kind 4). It is a **1-per-domain
setting**, so it lives on the ``admin-dns`` per-domain row alongside that domain's
mode/records (``admin.md`` § 4 / ``mail-multidomain.md`` § Per-domain catch-all),
**not** on ``admin-aliases`` (which hosts the N-per-domain external forwarders).

Drives the shared ``LocalDomainMachine``'s ``SetCatchAllActor`` over the
``fauna.bridges.set_catch_all_actor`` RPC and asserts the designation **persists**
by reading it back over ``fauna.bridges.list_local_domains`` (the authoritative
wire surface), then clears it. The RCPT-time resolver *consumption* of
``catch_all_actor_id`` is a separate track (``mail-multidomain.md`` § Impl status).

``nest_instance`` is session-scoped, so we seed a unique domain and assert on
*that* domain's row rather than an absolute count.
"""

import secrets

import pytest

from helpers.admin_wire import (
    admin_user,
    catch_all_actor_for,
    picker_option,
    seed_local_domain,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

_DOMAIN = f"catchall-{secrets.token_hex(4)}.test"


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_catch_all_designate_and_clear(admin_app, nest_instance):
    """Designate a domain's catch-all actor via the per-row picker → assert it
    persists over ``list_local_domains`` → clear it ("None") → assert cleared."""
    seed_local_domain(nest_instance, _DOMAIN)
    admin = admin_user(nest_instance)
    option, actor_id = picker_option(admin), admin["actor_id"]

    admin_app.admin.navigate_dns()

    # Wait for the seeded domain's row (which carries the catch-all picker).
    wait_until(
        lambda: admin_app.admin.dns_domain_names()
        if _DOMAIN in admin_app.admin.dns_domain_names()
        else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {_DOMAIN!r} not rendered on admin-dns; "
            f"got {admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )
    idx = admin_app.admin.dns_domain_names().index(_DOMAIN)

    # Sanity: a fresh domain has no catch-all designated.
    assert catch_all_actor_for(nest_instance, _DOMAIN) is None, (
        f"freshly-seeded {_DOMAIN!r} unexpectedly already has a catch-all"
    )

    # --- Designate the admin actor as the domain's catch-all ---
    admin_app.admin.set_catch_all(option, index=idx)
    persisted = wait_until(
        lambda: catch_all_actor_for(nest_instance, _DOMAIN),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"catch_all_actor_id for {_DOMAIN!r} never became non-empty after "
            f"designating {option!r}"
        ),
    )
    assert persisted == actor_id, (
        f"after designating {option!r} via admin-dns-domain-catch-all-select, "
        f"list_local_domains catch_all_actor_id for {_DOMAIN!r} = {persisted!r}, "
        f"expected {actor_id!r}"
    )

    # --- Clear it (the "None" option clears the designation) ---
    admin_app.admin.set_catch_all(S.admin.dns.catch_all_none, index=idx)
    wait_until(
        lambda: catch_all_actor_for(nest_instance, _DOMAIN) is None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"after selecting None, catch_all_actor_id for {_DOMAIN!r} not cleared; "
            f"still {catch_all_actor_for(nest_instance, _DOMAIN)!r}"
        ),
    )
