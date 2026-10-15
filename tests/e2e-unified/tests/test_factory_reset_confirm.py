"""tier_3 e2e: factory reset asks the admin to confirm before anything is
wiped (`admin.md` § N. Nest — `admin-factory-reset-button` opens a destructive
confirm with `admin-factory-reset-confirm-button` /
`admin-factory-reset-cancel-button`; the wipe itself is
`nest/common.md` § Factory reset).

Every other reset journey clicks the button and then the confirm in one breath,
so none of them can tell a confirm that guards the wipe from one that only
decorates it. This one stops after the FIRST click and reads the nest: it is
still claimed, the admin's own key still authenticates, and the app still holds
its session; cancelling closes the confirm and leaves all of that as it was.

Runs on its own dedicated nest (`factory_reset_confirm_nest`), never the session
`nest_instance`: if the defect this test exists to catch were present, the first
click would wipe whatever nest it pointed at.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
]


@pytest.fixture
def factory_reset_confirm_nest(request, nest_mode, tmp_path_factory):
    """A dedicated, function-scoped, claimed nest (module docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "factory-reset-confirm"
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def factory_reset_confirm_spa_url(static_dir, factory_reset_confirm_nest):
    """Function-scoped SPA proxy → ``factory_reset_confirm_nest`` (the session
    ``spa_url`` only proxies ``nest_instance``)."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, factory_reset_confirm_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def factory_reset_confirm_admin_app(request, app, factory_reset_confirm_nest):
    """``admin_app``, pointed at ``factory_reset_confirm_nest``."""
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        factory_reset_confirm_nest,
        spa_url_fixture="factory_reset_confirm_spa_url",
        fixture_name="factory_reset_confirm_admin_app",
    )
    yield app


def _wait(predicate, budget_s: float = budgets.RPC_ROUNDTRIP_S) -> bool:
    try:
        return bool(wait_until(predicate, budget_s))
    except AssertionError:
        return bool(predicate())


def _nest_untouched(nest) -> None:
    """The nest is still claimed and still knows its admin."""
    with WsRpcAnonClient(nest["url"]) as anon:
        status = anon.call("fauna.setup.status", {})
    assert status.get("claimed") is True, (
        f"the nest is no longer claimed — something was wiped: {status!r}"
    )
    admin = nest["admin"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        client.call("fauna.bridges.get_mail_config", {})


@pytest.mark.feature("factory-reset")
def test_factory_reset_first_click_wipes_nothing(
    factory_reset_confirm_admin_app, factory_reset_confirm_nest
):
    """The reset button only opens the confirm: the nest is still claimed and
    its admin still authenticates, and the app is still on the Nest page, not
    the claim step. Cancelling closes the confirm and changes nothing."""
    admin_app = factory_reset_confirm_admin_app
    nest = factory_reset_confirm_nest

    admin_app.admin.navigate_nest()
    admin_app.driver.wait_until_enabled("admin-factory-reset-button", timeout=15.0)
    admin_app.driver.click("admin-factory-reset-button")

    assert _wait(lambda: admin_app.driver.is_visible("admin-factory-reset-confirm-button")), (
        "the reset button must open a confirm; error: "
        f"{admin_app.error_text()!r}"
    )
    assert admin_app.driver.is_visible("admin-factory-reset-cancel-button"), (
        "the confirm must offer a way out"
    )
    _nest_untouched(nest)
    assert admin_app.driver.is_absent("claim-code-input"), (
        "the first click must not tear the session down to the claim step"
    )

    admin_app.driver.click("admin-factory-reset-cancel-button")
    assert _wait(lambda: admin_app.driver.is_absent("admin-factory-reset-confirm-button")), (
        "cancel must close the confirm"
    )
    assert admin_app.driver.count("admin-factory-reset-button") > 0, (
        "after cancelling, the Nest page still offers the reset button"
    )
    _nest_untouched(nest)
