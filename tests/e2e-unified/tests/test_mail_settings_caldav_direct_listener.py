"""tier_3: the mail-settings connection block paints the nest's DIRECT DAV
listener — the nest's own address at the admin-set port — not the router-fronted
`mail.<domain>` on 443.

docs/goal/behavior/caldav-server.md § Network exposure → *Any-locator serving* /
§ Addressing; docs/goal/ui/mail-settings.md § MUA setup instructions
(`mail-settings-mua-caldav-host`, `mail-settings-mua-caldav-port`). The shared
`MuaInstructions::apply_dav_listener` threads `fauna.bridges.get_caldav_port`
(`port`, `direct_listener`) into the page on hydrate; libs/fauna-client-mail-settings
unit tests and the nest-level docker test `test_caldav_bare_ip_serving.py` pin the
logic and the reply, but nothing drove an app to prove the PAGE paints it.

The harness nest is reached at a bare loopback locator (a local-target, so direct). The existing
`test_mail_settings_caldav_only.py` reads the same two ids but at the DEFAULT port
(8443), which equals the pre-hydrate default `for_node_url` builds from the node URL
alone — so it passes even if the hydrate never applies the nest's reply. This test
therefore changes the admin CalDAV port to a NON-default value BEFORE the page
mounts; only the `get_caldav_port` -> `apply_dav_listener` path can make the page show
it. The port is seeded over the admin RPC as test setup (the same seed the sibling
`test_caldav_admin_port_rebind.py` uses); the assertion is on the rendered page.
"""

from __future__ import annotations

from urllib.parse import urlparse

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from drivers.port_util import find_free_port
from helpers.mail_dedicated_nest import mint_caldav_mailbox
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]


@pytest.mark.feature("calendar-in-standard-apps")
def test_mail_settings_paints_direct_listener_host_and_admin_port(
    dedicated_no_mail_app, dedicated_no_mail_nest
):
    app = dedicated_no_mail_app
    driver = app.driver
    nest = dedicated_no_mail_nest
    locator = urlparse(nest["url"]).hostname

    admin = nest["admin"]
    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        default_port = int(client.call("fauna.bridges.get_caldav_port", {})["port"])
        new_port = find_free_port()
        while new_port == default_port:
            new_port = find_free_port()
        # The CalDAV MUA row is gated on the deployment's `caldav_enabled`, read at
        # hydrate (see test_mail_settings_caldav_only.py): the admin's own toggle.
        assert client.call("fauna.bridges.set_caldav_enabled", {"enabled": True}).get("ok") is True
        client.call("fauna.bridges.set_caldav_port", {"port": new_port})
        reply = client.call("fauna.bridges.get_caldav_port", {})
    # The harness nest declares no `direct_listener` (no operator hatch); its bare
    # loopback locator is a local-target, which `apply_dav_listener` treats as
    # direct on its own. The port override is what only the hydrate can supply.
    assert int(reply["port"]) == new_port, (
        f"set_caldav_port({new_port}) must be what get_caldav_port reports; got {reply!r}"
    )

    # Seed before the page mounts (its snapshot is built once at mount — see
    # test_mail_settings_caldav_only.py), then open the page.
    mint_caldav_mailbox(driver, password=None)
    app.mail_settings.navigate()
    driver.wait_for("mail-settings-mua-caldav-host", timeout=20.0)
    assert driver.is_visible_scrolled("mail-settings-mua-caldav-port"), (
        "mail-settings-mua-caldav-port must render alongside -caldav-host; "
        f"error: {app.error_text()!r}"
    )

    # Poll to the expected port: the hydrate applies the listener after the first
    # paint, and the first paint shows the default.
    def _port_text() -> str:
        return (driver.get_text("mail-settings-mua-caldav-port") or "").strip()

    wait_until(
        lambda: _port_text() == str(new_port),
        20.0,
        diagnose=lambda: (
            f"the CalDAV port must be the admin-set {new_port} (default was "
            f"{default_port}), got {_port_text()!r}"
        ),
    )
    host_text = (driver.get_text("mail-settings-mua-caldav-host") or "").strip()
    assert host_text == locator, (
        f"a direct listener is reached at the nest's own address {locator!r}, "
        f"not mail.<domain>; got {host_text!r}"
    )
    assert not host_text.startswith("mail."), f"unexpected mail. prefix: {host_text!r}"
