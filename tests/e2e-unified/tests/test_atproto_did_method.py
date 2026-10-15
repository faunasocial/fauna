"""The first hosting offers a choice of identity — portable (`did:plc`,
recommended and pre-selected) or tied to the domain (`did:web`) — and the
handle is the same either way (docs/goal/behavior/atproto-pds-bridge.md
§ Enable UX; catalog `atproto` 22).

tier_3 (full stack — real `fauna-nest` binary). The choice renders only before
an account's FIRST mint, so each case needs an account that has never hosted:
the module-scoped `atproto_hosted_nest` holds an identity once any sibling has
minted on it, so every case here starts its own nest. Parametrized over the
two methods, each case asserts the handle the identity is minted with is the
nest's own derivation for this account — the same assertion for both methods
is what "the same either way" means. The "same handle either way" line under
the radios is untagged chrome, so the test reads the identity summary the mint
produces, never that line.

tui first; the other apps gain their markers in the cross-app lift (web's leg
also needs a proxy onto this dedicated nest — `_login_app_as`'s `spa_url`).
"""
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

HANDLE_DOMAIN = "fauna.test"


@pytest.fixture
def never_hosted_app(request, app, nest_mode, tmp_path_factory):
    """``app`` logged in to a fresh public-domain nest as an account that has
    never minted an identity. Waits for the account runtime, because a did:plc
    mint writes its senior rotation key into the account plane."""
    from conftest import _login_app_as, _make_user, _start_dedicated_nest
    from helpers.waiting import account_pump_role, await_account_runtime_assembled

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "atproto-never-hosted-nest",
        claim_domain=HANDLE_DOMAIN,
    )
    try:
        user = _make_user(nest)
        _login_app_as(app, request, nest, user)
        if account_pump_role(app.driver) is not None:
            await_account_runtime_assembled(app.driver)
        yield app, nest, user
    finally:
        cleanup()


def _handle_preview(nest, user) -> str:
    """The nest's own derivation of this account's Bluesky handle, read over
    the wire as the account (`get_integration_status`) — a read, not a
    mutation, so it may bypass the UI (convention 5)."""
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes.fromhex(user["actor_id_hex"]),
        signing_key=bytes(user["signing_key"]),
    ) as ws:
        return ws.call("fauna.bridges.atproto.get_integration_status", {})["handle_preview"]


@pytest.mark.tui
@pytest.mark.feature("atproto")
@pytest.mark.parametrize("method", ["plc", "web"])
def test_either_identity_choice_mints_the_same_handle(method, never_hosted_app):
    app, nest, user = never_hosted_app
    bs = app.atproto_settings
    bs.navigate()
    assert bs.is_page_visible(), f"atproto page not reachable. error: {app.error_text()!r}"
    assert bs.wait_for_depth_enabled("hosted_visible"), (
        f"hosted visible must be selectable on a public domain. "
        f"gate={bs.depth_gate_marker('hosted_visible')!r} error={bs.page_error_text()!r}"
    )

    expected = _handle_preview(nest, user)
    assert expected.endswith(f".{HANDLE_DOMAIN}"), (
        f"the nest must derive a handle under {HANDLE_DOMAIN!r}, got {expected!r}"
    )

    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), f"entering hosting must stage the card. error={bs.page_error_text()!r}"
    app.driver.wait_for("atproto-did-method-plc", timeout=8.0)
    assert (bs.did_method_state("plc"), bs.did_method_state("web")) == ("on", "off"), (
        "the portable identity is the recommended default and must be pre-selected; "
        f"plc={bs.did_method_state('plc')!r} web={bs.did_method_state('web')!r}"
    )
    if method != "plc":
        bs.select_did_method(method)
        wait_until(
            lambda: bs.did_method_state(method) == "on",
            8.0,
            diagnose=lambda: (
                f"choosing {method} must select it; plc={bs.did_method_state('plc')!r} "
                f"web={bs.did_method_state('web')!r} error={bs.current_error_text()!r}"
            ),
        )

    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"confirm must apply the transition. level={bs.depth_level()!r} "
        f"error={bs.page_error_text()!r}"
    )
    summary = wait_until(
        lambda: bs.hosted_handle_text() if bs.is_hosted_handle_visible() else "",
        15.0,
        diagnose=lambda: f"no identity summary after minting; error={bs.current_error_text()!r}",
    )
    assert S.atproto_settings.hosted_handle_prefix(handle=expected) in summary, (
        f"a {method} identity must carry the handle the nest derives for this "
        f"account ({expected!r}), the same as the other method; got {summary!r}"
    )
    assert S.atproto_settings.hosted_method_prefix(method=method) in summary, (
        f"the identity must be the {method} one the user chose; got {summary!r}"
    )
