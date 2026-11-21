"""The web-app origin choice on admin-nest (admin.md § N Nest → *Web-app
origin*), read through the app UI against a real nest.

While the central origin is not live the choice is not offered
(``web-content-hosting.md`` § Same-origin security model → *While the central
origin is not live, the choice is not offered*): the nest's projection says
``central_live`` false, the shared ``fauna_client_admin::admin_web_app_origin_view``
answers ``section_possibility`` absent for a nest holding bundled, and the app
paints no ``admin-nest-web-app-origin-*`` element at all. So on a real nest of
today's builds the admin Nest page shows no Web app section, and the nest's live
router keeps answering ``/app/`` with what it ships (convention 5 — the API leg
that tells a UI-side failure from a nest-side one). The flip-to-central journey
(radio + save, the status naming the exact ``Location``, and back) returns in the
commit that flips ``CENTRAL_APP_ORIGIN_LIVE``, as that ruling's test paragraph
says; until then the redirect is pinned at tier_1 with the fact passed as true.

**Why a dedicated nest.** ``web_hosting_nest`` claims a real DNS-shaped domain,
so the absence observed here is the not-live rule and not the domainless one (a
domainless box serves bundled whatever the choice). A tier_3 nest ships no SPA,
so its bundled ``/app/`` answers 404 (still reserved, never user content), while
an image nest answers it with its bundled SPA (200) — the handle's ``serves_spa``
says which.

tui is the lead app; the other six follow in their batched trickle-down.
"""

import time
import urllib.error

import pytest

from common.auth import no_redirect_opener
from conftest import _login_admin_as
from i18n.strings import S

pytestmark = pytest.mark.tier_3


# A redirect would be the failure here, so it is handed back rather than
# followed — under the floor-cert rule a TLS-serving nest needs
# (`common.auth.no_redirect_opener`).
_OPENER = no_redirect_opener()

_SECTION_IDS = (
    "admin-nest-web-app-origin-section",
    "admin-nest-web-app-origin-bundled-radio",
    "admin-nest-web-app-origin-central-radio",
    "admin-nest-web-app-origin-save-button",
    "admin-nest-web-app-origin-status",
)


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
def test_admin_nest_offers_no_web_app_origin_while_central_is_not_live(
    origin_admin_app, web_hosting_nest
):
    app = origin_admin_app
    nest_url = web_hosting_nest["url"]
    app.admin.navigate_nest()

    # The section's existence is decided by the page's read, so first wait for
    # that read to land: the update channel rides the same nest snapshot, and
    # its status leaves its loading sentence once the snapshot is in.
    loading = S.admin.nest_page.update_channel_loading
    assert _wait_for(
        lambda: app.admin.update_channel_status_text() not in ("", loading)
    ), (
        f"the Nest page's read never landed; update channel status "
        f"{app.admin.update_channel_status_text()!r}, error: {app.error_text()!r}"
    )

    shown = [i for i in _SECTION_IDS if not app.is_absent(i)]
    assert not shown, (
        f"the central origin is not live, so the Web app section must not be "
        f"offered; painted: {shown}, status {app.admin.web_app_origin_status_text()!r}"
    )

    bundled_status = 200 if web_hosting_nest["serves_spa"] else 404
    status, location = _get_app(nest_url)
    assert status == bundled_status and location is None, (
        f"/app/ must keep answering what the nest ships ({bundled_status}), "
        f"not redirect: {status} {location!r}"
    )
