"""The web-app origin choice on admin-nest (admin.md § N Nest → *Web-app
origin*), driven through the app UI against a real nest.

The admin flips this nest's ``/app/`` to the central origin with the section's
radio + save, sees the status line name the **exact** address users are sent to
(the nest's own projection, folded by the shared
``fauna_client_admin::admin_web_app_origin_view``), and the nest's live router
agrees: ``GET /app/`` answers a 302 to that same address (convention 5 — the API
leg that tells a UI-side failure from a nest-side one). Then back to bundled,
through the UI again, and ``/app/`` stops redirecting.

**Why a dedicated nest.** The shared session nest runs with no handle domain,
and a domainless box keeps serving bundled whatever the choice
(``web-content-hosting.md`` § The nest-served `/app/` and the central origin) —
there would be no redirect to observe. ``web_hosting_nest`` claims a real
DNS-shaped domain. A tier_3 nest ships no SPA, so its bundled ``/app/`` answers
404 (still reserved, never user content); what the SPA serves under bundled is
pinned in Rust against the real ``mount_spa``.

tui is the lead app; the other six follow in their batched trickle-down.
"""

import time
import urllib.error
import urllib.request

import pytest

from conftest import _login_admin_as
from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The apps whose admin-nest renders the section. tui leads.
BUILT = {"tui"}


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def _get_app(url: str):
    """GET ``/app/`` WITHOUT following a redirect → (status, Location)."""
    try:
        resp = _OPENER.open(url.rstrip("/") + "/app/")
        return resp.status, resp.headers.get("Location")
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("Location")


def _wait_for(predicate, timeout: float = 20.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.3)
    return predicate()


@pytest.fixture
def origin_admin_app(request, app, web_hosting_nest):
    """``admin_app`` pointed at ``web_hosting_nest`` (only the nest differs)."""
    _login_admin_as(
        app,
        request,
        web_hosting_nest,
        spa_url_fixture="web_hosting_spa_url",
        fixture_name="origin_admin_app",
    )
    yield app


@pytest.mark.feature("admin-nest")
def test_admin_flips_app_to_the_central_origin_and_back(origin_admin_app, web_hosting_nest):
    app = origin_admin_app
    if app_name(app.driver) not in BUILT:
        skip_unbuilt(
            app.driver,
            surface="admin-nest-web-app-origin-section",
            detail="the web-app origin section's six-app trickle-down has not reached this app yet",
            tracked="",
        )
    nest_url = web_hosting_nest["url"]
    app.admin.navigate_nest()

    bundled_text = S.admin.nest_page.web_app_origin_status_bundled
    assert _wait_for(lambda: app.admin.web_app_origin_status_text() == bundled_text), (
        f"a fresh nest must read bundled; status {app.admin.web_app_origin_status_text()!r}, "
        f"error: {app.error_text()!r}"
    )
    status, location = _get_app(nest_url)
    assert status == 404 and location is None, (
        f"bundled with no shipped SPA must answer 404, not redirect: {status} {location!r}"
    )

    app.admin.select_web_app_origin("central")
    app.admin.save_web_app_origin()
    try:
        # The status line must carry the nest's exact projected target.
        assert _wait_for(
            lambda: app.admin.web_app_origin_status_text() != bundled_text
        ), f"status never left bundled after saving central. error: {app.error_text()!r}"
        status_text = app.admin.web_app_origin_status_text()
        status, location = _get_app(nest_url)
        assert status == 302 and location, (
            f"central must make /app/ answer 302 (status line {status_text!r}); "
            f"got {status} {location!r}"
        )
        assert location.startswith("https://app.fauna.social/app/"), location
        assert status_text == S.admin.nest_page.web_app_origin_status_central(target=location), (
            f"the status must name exactly where users are sent: {status_text!r} vs {location!r}"
        )
    finally:
        # Back to bundled through the UI, and wait for the nest to agree, so the
        # module-scoped nest is left as found.
        app.admin.select_web_app_origin("bundled")
        app.admin.save_web_app_origin()
        assert _wait_for(lambda: app.admin.web_app_origin_status_text() == bundled_text), (
            f"status did not return to bundled. error: {app.error_text()!r}"
        )
    status, location = _get_app(nest_url)
    assert status == 404 and location is None, (
        f"bundled again must stop redirecting: {status} {location!r}"
    )
