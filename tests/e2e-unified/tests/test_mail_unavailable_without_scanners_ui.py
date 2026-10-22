"""tier_3 on tui: a box that cannot enable mail renders it on both Mail pages.

A bare nest whose data dir carries a seeded hatch declaring no content scanners
(the seam ``tests/api/test_mail_unavailable_without_scanners.py`` built: the
test writes ``operator-hatch.toml`` and restarts the nest in place, since the
nest reads it once at boot) is public-axis and reports ``content_scanners:
absent``. The shared machines read that through
``SetupStatusReply::mail_unavailable_here``, and tui — a renderer of the
snapshot field, never a re-deriver — shows, on the user's Mail & Calendar page
and on the admin Mail section alike: the enable toggle disabled with its
``state`` reading ``unavailable``, and the notice beside it. No gesture turns
mail on, and the nest's own mail flag stays off.

The hatch-less twin is every existing enable test on the session nest
(``unknown`` keeps today's behaviour). Owners: ``docs/goal/ui/mail-settings.md``
§ Layout & flow, ``docs/goal/behavior/admin.md`` § 6 Mail; the fact:
``docs/goal/behavior/mail-bridge-lifecycle.md`` § Default-off on first claim →
*A box with no content scanners cannot enable mail* (its tier_3 observable).
"""

import os

import pytest

from i18n.strings import S
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import port_base_url

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.feature("turn-on-mail"),
]

_MAIL_TOGGLE = "mail-settings-enabled-toggle"
_MAIL_NOTICE = "mail-settings-unavailable-notice"
_ADMIN_TOGGLE = "admin-mail-enabled-toggle"
_ADMIN_NOTICE = "admin-mail-unavailable-notice"


@pytest.fixture()
def no_scanner_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest
    from helpers import nest_mode as nest_mode_mod
    from helpers.app_surface import skip_environment

    if not nest_mode_mod.builds_local_nest(nest_mode):
        skip_environment(
            "the hatch is seeded into a locally-spawned nest's data dir; an image "
            "nest's entrypoint regenerates it on every start (the tier_4 twin "
            "covers that path)"
        )
    from common.nest import start_nest_in_place, stop_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "no-scanner-ui-nest"
    )
    try:
        stop_nest(nest, graceful=True)
        with open(os.path.join(nest["tmp_dir"], "operator-hatch.toml"), "w") as f:
            f.write('content_scanners = "absent"\n')
        start_nest_in_place(nest)
        yield nest
    finally:
        cleanup()


@pytest.fixture
def no_scanner_spa_url(static_dir, no_scanner_nest):
    """Function-scoped SPA proxy → ``no_scanner_nest`` (the web lift's door)."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, no_scanner_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def no_scanner_admin_app(request, app, no_scanner_nest):
    """``admin_app``, pointed at ``no_scanner_nest`` (`_login_admin_as`)."""
    from conftest import _login_admin_as

    _login_admin_as(app, request, no_scanner_nest,
                    spa_url_fixture="no_scanner_spa_url",
                    fixture_name="no_scanner_admin_app")
    yield app


def _status(nest) -> dict:
    with WsRpcAnonClient(port_base_url(nest["port"])) as anon:
        return anon.call("fauna.setup.status", {})


def _wait_for_attr(driver, element_id: str, expected: str, timeout: float = 30.0) -> str:
    """Poll the element's ``state`` attr until it reads ``expected`` — the page
    hydrates asynchronously, so the end state is awaited, never timed."""
    import time

    deadline = time.monotonic() + timeout
    seen = None
    while time.monotonic() < deadline:
        if driver.count(element_id) > 0:
            seen = driver.get_attr(element_id, "state")
            if seen == expected:
                return seen
        time.sleep(0.3)
    return seen


def _assert_is_the_notice(text: str) -> None:
    """The notice paints its own constant, never the refusal's error sentence
    ("Mail wasn't turned on: …" reads as a failed attempt before any try)."""
    assert text.strip() == S.mail_settings.unavailable_notice, text
    assert text.strip() != S.error.bridges.mail_unavailable_here


def _refused_gesture(driver, element_id: str) -> None:
    """The toggle is visible but not actionable: tui refuses the click on a
    disabled element, so the enable gesture cannot reach the machine."""
    assert not driver.is_enabled(element_id), f"{element_id} is actionable"
    with pytest.raises(RuntimeError, match="disabled"):
        driver.click(element_id)


def test_both_mail_pages_render_a_box_that_cannot_enable_mail(
        no_scanner_admin_app, no_scanner_nest):
    app, nest = no_scanner_admin_app, no_scanner_nest
    status = _status(nest)
    assert status["node_mode"] == "public", status
    assert status.get("content_scanners") == "absent", status

    # The user's Mail & Calendar page (the admin is a user too).
    app.mail_settings.navigate()
    state = _wait_for_attr(app.driver, _MAIL_TOGGLE, "unavailable")
    error = app.driver.get_text("error-message") if app.driver.count("error-message") else ""
    assert state == "unavailable", (
        f"mail-settings toggle state={state!r}; error-message={error!r}"
    )
    assert app.driver.count(_MAIL_NOTICE) > 0, "the unavailable notice is shown"
    _assert_is_the_notice(app.driver.get_text(_MAIL_NOTICE))
    _refused_gesture(app.driver, _MAIL_TOGGLE)
    assert app.mail_settings.enabled_toggle_state() == "unavailable"

    # The admin Mail section.
    app.admin.navigate_mail()
    state = _wait_for_attr(app.driver, _ADMIN_TOGGLE, "unavailable")
    error = app.driver.get_text("error-message") if app.driver.count("error-message") else ""
    assert state == "unavailable", (
        f"admin-mail toggle state={state!r}; error-message={error!r}"
    )
    assert app.driver.count(_ADMIN_NOTICE) > 0, "the admin notice is shown"
    _assert_is_the_notice(app.driver.get_text(_ADMIN_NOTICE))
    _refused_gesture(app.driver, _ADMIN_TOGGLE)

    # Ground truth on the nest: deployment mail never turned on.
    assert _status(nest)["email_enabled"] is False
    assert not os.path.exists(os.path.join(nest["tmp_dir"], "imap-enabled"))
