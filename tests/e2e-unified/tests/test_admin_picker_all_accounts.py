"""tier_3 e2e: every admin actor picker offers EVERY account on the nest, not
just the newest ``fauna.admin.users.list`` page (``admin.md`` § 2 → *What
identifies a user in an admin picker*).

``fauna.admin.users.list`` answers one page, newest first. A picker that reads a
single page loses the OLDEST accounts once a nest holds more than a page — and
the oldest account is the box claimer, the likeliest front-page member and
catch-all target. Each test here picks that claimer on ``crowded_nest``
(conftest), a dedicated nest seeded until the claimer has left the nest's default
page, through one of the picker families ``admin.md`` § 2 names: the admin-web
front-page picker, the admin-dns catch-all and role-address pickers, and the
admin-users guardian picker.

A picker that never offers the claimer fails at the pick: ``select`` refuses an
option the picker did not paint (e2e-conventions.md convention 11), and where a
driver can list a picker's options the test first waits for the claimer to
appear, so a slow read is not mistaken for a missing account. Every pick is then
read back over the wire, so an option that painted but bound another account
fails too.
"""

import secrets

import pytest

from helpers.admin_wire import (
    admin_user,
    apex_actor,
    catch_all_actor_for,
    invite_code_guardian,
    picker_option,
    role_overrides_for,
    seed_local_domain,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
]


def _oldest_account(nest) -> tuple[str, bytes]:
    """``(option text, actor id)`` of ``crowded_nest``'s box claimer — its oldest
    account, off the nest's default ``users.list`` page by construction."""
    claimer = admin_user(nest)
    return picker_option(claimer), bytes(claimer["actor_id"])


def _wait_until_offered(driver, element_id: str, option: str, *, index: int = 0) -> None:
    """Wait for a picker's asynchronous account read to paint ``option``.

    A driver that cannot list a picker's options (``option_texts`` answers
    ``None``) returns at once: its ``select`` refuses an option the picker never
    offered, which is the same assertion made one step later."""
    if driver.option_texts(element_id, index=index) is None:
        return
    wait_until(
        lambda: option in (driver.option_texts(element_id, index=index) or []),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{element_id!r} never offered {option!r}, an account older than the "
            f"newest users.list page; it painted "
            f"{driver.option_texts(element_id, index=index)!r}"
        ),
    )


@pytest.mark.feature("admin-front-page")
def test_admin_web_apex_picker_offers_an_account_older_than_the_newest_page(
    crowded_admin_app, crowded_nest
):
    """The front-page picker offers the box claimer, and picking it designates
    that account over ``fauna.web.get_apex_actor``."""
    option, actor_id = _oldest_account(crowded_nest)
    app = crowded_admin_app
    app.admin.navigate_web()
    app.driver.wait_for("admin-web-apex-actor-select", timeout=15.0)
    _wait_until_offered(app.driver, "admin-web-apex-actor-select", option)

    app.admin.set_apex_actor(option)
    persisted = wait_until(
        lambda: apex_actor(crowded_nest),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"get_apex_actor never became non-empty after picking {option!r}",
    )
    assert bytes(persisted) == actor_id, (
        f"picking {option!r} in admin-web-apex-actor-select designated "
        f"{bytes(persisted).hex()}, expected the box claimer {actor_id.hex()}"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_pickers_offer_an_account_older_than_the_newest_page(
    crowded_admin_app, crowded_nest
):
    """A domain row's catch-all and role-address pickers both offer the box
    claimer, and each pick persists that account over
    ``fauna.bridges.list_local_domains``."""
    domain = f"crowded-{secrets.token_hex(4)}.test"
    seed_local_domain(crowded_nest, domain)
    option, actor_id = _oldest_account(crowded_nest)
    app = crowded_admin_app
    app.admin.navigate_dns()
    wait_until(
        lambda: domain in app.admin.dns_domain_names(),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {domain!r} not rendered on admin-dns; "
            f"got {app.admin.dns_domain_names()!r}"
        ),
    )
    idx = app.admin.dns_domain_names().index(domain)

    catch_all_select = "admin-dns-domain-catch-all-select"
    _wait_until_offered(app.driver, catch_all_select, option, index=idx)
    app.admin.set_catch_all(option, index=idx)
    catch_all = wait_until(
        lambda: catch_all_actor_for(crowded_nest, domain),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"catch_all_actor_id for {domain!r} never became non-empty after "
            f"picking {option!r}"
        ),
    )
    assert bytes(catch_all) == actor_id, (
        f"picking {option!r} in {catch_all_select} designated "
        f"{bytes(catch_all).hex()}, expected the box claimer {actor_id.hex()}"
    )

    role_select = "admin-dns-domain-role-address-abuse-select"
    _wait_until_offered(app.driver, role_select, option, index=idx)
    app.admin.set_role_address("abuse", option, index=idx)
    abuse = wait_until(
        lambda: role_overrides_for(crowded_nest, domain).get("abuse"),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"role_address_overrides['abuse'] for {domain!r} never became non-empty "
            f"after picking {option!r}"
        ),
    )
    assert abuse == actor_id.hex(), (
        f"picking {option!r} in {role_select} delegated abuse@ to {abuse!r}, "
        f"expected the box claimer {actor_id.hex()}"
    )


@pytest.mark.feature("admin-users")
def test_admin_users_guardian_picker_offers_an_account_older_than_the_newest_page(
    crowded_admin_app, crowded_nest
):
    """The invite form's guardian picker offers the box claimer, and a code minted
    under that pick names the claimer as its guardian over
    ``fauna.admin.invite_codes.list``."""
    option, actor_id = _oldest_account(crowded_nest)
    app = crowded_admin_app
    app.admin.navigate_users()

    code = app.admin.create_invite_code(guardian=option)
    assert code, f"minting an invite code under guardian {option!r} returned no code"
    guardian = invite_code_guardian(crowded_nest, code)
    assert guardian is not None and bytes(guardian) == actor_id, (
        f"invite code {code!r} minted with guardian {option!r} names guardian "
        f"{bytes(guardian).hex() if guardian else None}, expected the box claimer "
        f"{actor_id.hex()}"
    )
