"""The per-app direct-launch keys — the one home every test that builds its own
``create_driver(app).launch({...})`` / ``make_launch_harness(app, ...)`` call
reads them from (convention 10, ``docs/goal/architecture/e2e-launch-isolation.md``).

Not a driver factory: it returns a dict of launch keys and creates no driver.
"""
from __future__ import annotations

from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[3]


def app_launch_config(app_name: str, request) -> dict:
    """App-specific keys ``create_driver(app_name).launch()`` needs.

    Most apps want only ``app_path`` (their ``<app>_app_path`` fixture). iOS has
    no such fixture: its direct-launch fixture ``ios_setup`` returns
    ``{"udid", "app_path"}`` together, because ``drivers/ios.py``'s ``launch()``
    requires both. android needs the APK plus the run's bridge APK and device
    (``helpers/android_device.run_launch_facts``, the one home conftest's own
    android launch reads too).

    Fetched via ``request.getfixturevalue`` (never a plain fixture parameter) so
    a ``--app macos`` run never triggers the iOS build, and vice versa. The dict
    spreads into both call shapes (``launch({...})`` and
    ``make_launch_harness(app, **cfg)``)."""
    if app_name == "ios":
        setup = request.getfixturevalue("ios_setup")
        return {"app_path": setup["app_path"], "udid": setup["udid"]}
    if app_name == "android":
        from helpers import android_device

        return {
            "app_path": request.getfixturevalue("android_app_path"),
            **android_device.run_launch_facts(_REPO_ROOT),
        }
    return {"app_path": request.getfixturevalue(f"{app_name}_app_path")}
