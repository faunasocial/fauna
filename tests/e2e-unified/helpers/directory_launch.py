"""Launch an app against a nest with the PLC-directory override wired the way
each app reads it — the one launcher the directory-driven custody journeys share
(``test_alert_sweep_directory_feeders_e2e.py``, ``test_audit_floor_nest_silence.py``).

Native apps read the override from ``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` at launch;
web has no process environment, so it is served through the SPA proxy and then
pointed at the directory by ``driver.enable_fake_plc_directory`` — which every
app also receives, so the running session's override is the directory's on all
of them. iOS needs its direct-launch fixture's ``udid`` beside the app path.

A driver factory in ``scripts/features_scan.py``'s sense: a test module that
imports it drives an app, so it is registered in ``DRIVER_FACTORY_IMPORTS``.
"""
from __future__ import annotations

from drivers import create_driver


def launch_app_with_directory(app_name: str, request, nest: dict, directory):
    """Launch ``app_name`` against ``nest`` with ``directory`` wired in.

    Returns ``(driver, spa_proxy_server)``; the second is ``None`` except on web,
    where the caller shuts it down after the driver.
    """
    from conftest import _seeded_environment

    spa_proxy_server = None
    if app_name == "web":
        from conftest import _serve_spa_proxy

        static_dir = request.getfixturevalue("static_dir")
        spa_url, spa_proxy_server = _serve_spa_proxy(static_dir, nest["url"])
        driver = create_driver("web")
        driver.launch({"url": spa_url.rstrip("/") + "/app/"})
    else:
        environment = {
            **_seeded_environment(request, nest),
            "FAUNA_ATPROTO_PLC_DIRECTORY_URL": directory.url,
            "FAUNA_DNS_PROVIDER_FAKE": "1",
        }
        if app_name == "linux":
            environment["GTK_A11Y"] = "none"
        driver = create_driver(app_name)
        launch_config = {"url": nest["url"], "environment": environment}
        if app_name == "ios":
            # No bare `ios_app_path` fixture exists — iOS's direct-launch fixture
            # (`ios_setup`) returns `{"udid", "app_path"}` together, because
            # `drivers/ios.py`'s `launch()` requires both.
            ios_setup = request.getfixturevalue("ios_setup")
            launch_config["app_path"] = ios_setup["app_path"]
            launch_config["udid"] = ios_setup["udid"]
        else:
            launch_config["app_path"] = request.getfixturevalue(f"{app_name}_app_path")
        driver.launch(launch_config)
    try:
        driver.wait_for_state(lambda s: s is not None, timeout=30)
    except (TimeoutError, Exception):
        pass
    driver.enable_fake_plc_directory(directory.url)
    return driver, spa_proxy_server
