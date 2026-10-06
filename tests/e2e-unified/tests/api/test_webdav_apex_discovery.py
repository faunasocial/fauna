"""tier_3 API e2e: a file app pointed at the domain's standard WebDAV address is
sent on to the files — and is told the service is unavailable, never misled,
while it is not live.

`docs/goal/behavior/webdav-server.md` § Network exposure & discovery: the apex
`/.well-known/webdav` answers `301` → `https://mail.<domain>/webdav/`, and `503`
when WebDAV is explicitly disabled or no primary mail domain is registered. The
nest's own Rust tests pin the handler; this drives the real binary over HTTP
from a file app's seat, through the three states a deployment actually passes
through — no domain yet, live, switched off — with the admin's own wire calls
moving it between them (a `[nest]` witness for
`docs/features/files-in-standard-apps.md`).
"""
from __future__ import annotations

import urllib.error
import urllib.request

import pytest

from helpers import budgets

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3

DOMAIN = "files-apex.test"


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def _apex(nest) -> tuple[int, str | None]:
    """GET the apex well-known the way a file app's first probe does; return
    (status, Location)."""
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        resp = opener.open(f"{nest['url']}/.well-known/webdav", timeout=budgets.RPC_ROUNDTRIP_S)
        return resp.status, resp.headers.get("Location")
    except urllib.error.HTTPError as err:
        return err.code, err.headers.get("Location")


def _admin(nest) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.mark.feature("files-in-standard-apps", "admin-calendar-contacts-files")
def test_the_standard_webdav_address_sends_a_file_app_on_to_the_files(dedicated_no_mail_nest):
    nest = dedicated_no_mail_nest

    # No primary mail domain yet: there is no host to send the app to, so the
    # apex says the service is unavailable rather than redirecting nowhere.
    status, location = _apex(nest)
    assert status == 503, f"with no mail domain the apex must 503; got {status} → {location}"

    # Live: a primary domain and mail on (WebDAV unset follows mail).
    with _admin(nest) as admin:
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        admin.call("fauna.bridges.add_local_domain", {
            "domain": DOMAIN, "mta_sts_cert_mode": "per_host",
        })
    status, location = _apex(nest)
    assert status == 301, f"a live WebDAV deployment's apex must 301; got {status}"
    assert location == f"https://mail.{DOMAIN}/webdav/", (
        f"the apex must send a file app to the files root on the mail host; got {location}"
    )

    # Switched off by the admin: unavailable again, even with mail still on.
    with _admin(nest) as admin:
        admin.call("fauna.bridges.set_webdav_enabled", {"enabled": False})
    status, location = _apex(nest)
    assert status == 503, (
        f"an explicitly disabled WebDAV must 503 at the apex; got {status} → {location}"
    )
