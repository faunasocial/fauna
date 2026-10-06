"""tier_3 e2e: the per-domain role-address override pickers on admin-dns
(``admin-dns-domain-role-address-<role>-select``).

The admin can redirect a domain's RFC 2142 / RFC 5321 §4.5.1 operations role
addresses — ``postmaster@`` / ``abuse@`` / ``noc@`` / ``security@`` — to a chosen
actor, e.g. delegate ``abuse@community-domain`` to a community moderator while the
others stay on the admin (``mail-multidomain.md`` § Per-domain role-address
routing). Each role is a per-(domain, role) admin setting, so the four pickers live
on the ``admin-dns`` per-domain row alongside the catch-all picker — **not** on
``admin-aliases`` (which hosts the N-per-domain external forwarders), and the four
are *not* claimable as user aliases (they are reserved local-parts).

Drives the shared ``LocalDomainMachine``'s ``SetRoleAddress`` over the
``fauna.bridges.set_role_address`` RPC and asserts the override **persists** by
reading the ``role_address_overrides`` JSON map back over
``fauna.bridges.list_local_domains`` (the authoritative wire surface), that setting
one role **preserves** the others (the nest atomic-merges), then clears it. The
RCPT-time resolver *consumption* of the map is a separate, already-shipped track.

``nest_instance`` is session-scoped, so we seed a unique domain and assert on
*that* domain's row.
"""

import secrets

import pytest

from helpers.admin_wire import (
    admin_user,
    picker_option,
    role_overrides_for,
    seed_local_domain,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

# Verified apps: linux (lead) + windows + web + tui + macos + ios have the four
# `admin-dns-domain-role-address-<role>-select` pickers; android adds its marker
# when it lifts the uniform shape (mail-multidomain.md § Per-domain
# role-address routing — "the android lift remains").
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.web,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
]


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_role_address_designate_merge_and_clear(admin_app, nest_instance):
    """Delegate ``abuse@`` via the per-row picker → assert it persists in
    ``role_address_overrides`` over ``list_local_domains`` → set ``postmaster@`` too
    and assert the abuse override is preserved (atomic merge) → clear ``abuse@``
    ("Admin (default)") → assert only postmaster remains."""
    # A domain PER INVOCATION, not per module import. `nest_instance` is
    # `scope="session"` and shared by every app parametrization, so a
    # module-level constant is one domain for all three apps: the second app
    # re-seeds a domain the first already designated and the "freshly-seeded"
    # assert below fails on the leftover override — the whole of this test's
    # half (it passes on whichever app runs first).
    domain = f"roleaddr-{secrets.token_hex(4)}.test"
    seed_local_domain(nest_instance, domain)
    # The override map stores 64-char lowercase actor hex, so compare in hex.
    admin = admin_user(nest_instance)
    option, actor_hex = picker_option(admin), bytes(admin["actor_id"]).hex()

    admin_app.admin.navigate_dns()

    # Wait for the seeded domain's row (which carries the four role pickers).
    wait_until(
        lambda: admin_app.admin.dns_domain_names()
        if domain in admin_app.admin.dns_domain_names()
        else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {domain!r} not rendered on admin-dns; "
            f"got {admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )
    idx = admin_app.admin.dns_domain_names().index(domain)

    # Sanity: a fresh domain has no role-address overrides.
    assert role_overrides_for(nest_instance, domain) == {}, (
        f"freshly-seeded {domain!r} unexpectedly already has role overrides"
    )

    # --- Delegate abuse@ to the admin actor ---
    admin_app.admin.set_role_address("abuse", option, index=idx)
    persisted = wait_until(
        lambda: role_overrides_for(nest_instance, domain).get("abuse"),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"role_address_overrides['abuse'] for {domain!r} never became non-empty "
            f"after delegating to {option!r}"
        ),
    )
    assert persisted == actor_hex, (
        f"after delegating abuse@ via admin-dns-domain-role-address-abuse-select, "
        f"role_address_overrides['abuse'] for {domain!r} = {persisted!r}, "
        f"expected {actor_hex!r}"
    )

    # --- Set postmaster@ too; the nest atomic-merges, so abuse@ must survive ---
    admin_app.admin.set_role_address("postmaster", option, index=idx)
    merged = wait_until(
        lambda: role_overrides_for(nest_instance, domain)
        if "postmaster" in role_overrides_for(nest_instance, domain)
        else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"role_address_overrides for {domain!r} never gained 'postmaster' "
            f"after delegating to {option!r}"
        ),
    )
    assert merged.get("postmaster") == actor_hex and merged.get("abuse") == actor_hex, (
        f"setting postmaster@ did not preserve abuse@ (atomic merge broken): "
        f"role_address_overrides for {domain!r} = {merged!r}"
    )

    # --- Clear abuse@ ("Admin (default)"); postmaster@ stays ---
    admin_app.admin.set_role_address("abuse", S.admin.dns.role_address_admin_default, index=idx)
    cleared = wait_until(
        lambda: role_overrides_for(nest_instance, domain)
        if "abuse" not in role_overrides_for(nest_instance, domain)
        else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"role_address_overrides for {domain!r} never dropped 'abuse' after "
            f"selecting Admin (default); still "
            f"{role_overrides_for(nest_instance, domain)!r}"
        ),
    )
    assert cleared is not None and "abuse" not in cleared, (
        f"after selecting Admin (default), abuse@ override for {domain!r} not "
        f"cleared; map still {role_overrides_for(nest_instance, domain)!r}"
    )
    assert cleared.get("postmaster") == actor_hex, (
        f"clearing abuse@ unexpectedly dropped postmaster@: {cleared!r}"
    )
