"""tier_3 e2e: the app's on-device log file stays under its size cap (tui, linux, android,
macos, ios).

`docs/goal/architecture/apps/observability.md` § 1. Shared capture — the
`fauna-log` crate writes a daily-named, size-capped file under
`<data_dir>/logs/`, whose directory never holds more than the total cap plus
one line of overshoot on the live file (`libs/fauna-log/src/rolling.rs`).
`docs/features/app-log.md` outcome 7.

The headless pin (`a_runaway_writer_stays_under_the_total_cap_forever`) proves
the writer; this witnesses the bound on a REAL app's log directory: the app is
launched with the test-capable build's lowered cap
(`FAUNA_E2E_LOG_TOTAL_CAP_BYTES`, compiled out of release artifacts —
convention 15), made to log several times that cap through its own tracing
subscriber (the agent's `app_log_emit`, one shared body:
`fauna_log::rolling::emit_burst_for_test`), and its log directory is read
through the driver (`on_device_log_sizes` / `on_device_log_text`). The directory stays under the cap, the oldest lines of the burst have
been pruned away (so the cap really engaged, rather than the burst being too
small to reach it), and the Logs page still lists the newest line.

tier_3: a real `fauna-nest` (`nest_instance`) and a locally-built app. tui and
linux read the file from the host; android over its bridge (`GET /app-log`),
its app re-reading the lowered cap once the launch intent's extras are process
env (`fauna_log::rolling::reload_e2e_caps_for_test`, called by `TestAgent.start`,
because `FaunaApp` installs logging first). macos and ios read the file from the
host under the launch's own Application Support (`InProcessAgentDriver.
on_device_log_text`), their shared FaunaKit arm (`AppLogEmitTestCommand`) driving
the same FFI body; ios launches on the session's simulator, so its config is the
shared app config with this launch's seed and environment laid over it. web keeps no on-device log file
(`observability.md` § 1: the file layer is native), an absence only the owner
approves.
"""
from __future__ import annotations

import json
import uuid

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _build_app_config, _seeded_environment, get_available_apps
from drivers import create_driver
from helpers import budgets
from helpers.app_surface import require_surface
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

#: The apps this journey runs on.
_APPS = ("tui", "linux", "android", "macos", "ios")

#: The lowered total cap the launch asks for (the per-file cap follows at one
#: fifth, as in production): small enough that a short burst passes it several
#: times over, large enough that the app's own startup lines are a rounding error.
TOTAL_CAP = 200_000

#: The burst: LINES lines of about LINE_PAD bytes each — 3x the cap.
LINES = 600
LINE_PAD = 1_000

#: The overshoot the bound allows: one line on the live file. A burst line plus
#: its tracing prefix (time, level, target) is well under this.
ONE_LINE_SLACK = 4_096


@pytest.fixture(params=[a for a in _APPS if a in get_available_apps()])
def log_app(request):
    """``(app, app_path)``; the app's id lands in the test name, which is what
    conftest's ``--app`` filter reads. ios has no ``ios_app_path`` fixture: its
    path and simulator come from the shared app config (:func:`_launch_config`),
    so it answers ``None`` here."""
    if request.param == "ios":
        return request.param, None
    return request.param, request.getfixturevalue(f"{request.param}_app_path")


def _launch_config(app_name, app_path, nest_instance, request, seed) -> dict:
    """This launch's config: the app, the nest, the seed and the lowered cap —
    over the shared app config on ios, whose launch also names its simulator."""
    environment = {
        **_seeded_environment(request, nest_instance),
        "FAUNA_E2E_LOG_TOTAL_CAP_BYTES": str(TOTAL_CAP),
    }
    if app_name == "ios":
        base = _build_app_config("ios", nest_instance, request)
        return {
            **base,
            "seed_credentials": seed,
            "environment": {**base.get("environment", {}), **environment},
        }
    return {
        "app_path": app_path,
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "environment": environment,
    }


def _total_bytes(driver) -> int:
    return sum(driver.on_device_log_sizes().values())


def _listing(driver) -> str:
    sizes = driver.on_device_log_sizes()
    return ", ".join(f"{name}={size}" for name, size in sizes.items()) or "(no fauna.log.* files)"


@pytest.mark.feature("app-log")
def test_the_on_device_log_directory_stays_under_its_size_cap(
    nest_instance, log_app, request
):
    app_name, app_path = log_app
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    seed = build_registry_seed(
        [
            {"actor_id": user["actor_id_hex"],
             "secret_hex": bytes(user["signing_key"]).hex(),
             "nest_url": nest_instance["url"],
             "device_id": "log-cap-user", "handle": "user"},
        ],
        active=user["actor_id_hex"],
    )

    driver = create_driver(app_name)
    driver.launch(_launch_config(app_name, app_path, nest_instance, request, seed))
    try:
        require_surface(driver, "on-device-log-reader")
        driver.wait_for_state(
            lambda s: bool(s.get("session", {}).get("authenticated")), timeout=45
        )
        reader = driver.on_device_log_text

        tag = f"logcap-{uuid.uuid4().hex[:10]}"
        first = f"{tag} #0 "
        newest = f"{tag} #{LINES - 1} "
        result = driver.call_command(
            "app_log_emit", {"tag": tag, "count": LINES, "bytes": LINE_PAD}
        ) or {}
        if isinstance(result, str):
            result = json.loads(result)
        assert result.get("emitted") == LINES, (
            f"the agent's `app_log_emit` did not write the burst: {result!r}"
        )

        # The file writer is non-blocking: wait for the burst's last line to
        # reach the disk before measuring, so the reading is the bound's, not
        # the flush's.
        wait_until(
            lambda: newest in reader(),
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                f"the burst's newest line {newest!r} never reached the on-device "
                f"log file; files: {_listing(driver)}"
            ),
        )

        total = _total_bytes(driver)
        assert total <= TOTAL_CAP + ONE_LINE_SLACK, (
            f"after a {LINES * LINE_PAD}-byte burst the log directory holds {total} "
            f"bytes, over the {TOTAL_CAP}-byte cap plus one line — the bound "
            f"(observability.md § 1) did not hold. A total near the burst size "
            f"means the lowered cap never applied (FAUNA_E2E_LOG_TOTAL_CAP_BYTES "
            f"unread: a build without the test-capable seam). Files: {_listing(driver)}"
        )
        text = reader()
        assert first not in text, (
            "the burst's first line is still on disk, so nothing was pruned and "
            f"the cap never engaged; files: {_listing(driver)}"
        )
        assert len(driver.on_device_log_sizes()) > 1, (
            f"a burst of several caps must have rolled the file: {_listing(driver)}"
        )

        app = ActionLayer(driver)
        app.logs.navigate()
        assert app.logs.is_page_visible(), (
            f"the Logs page is not reachable. error: {app.error_text()!r}"
        )
        assert wait_until(
            lambda: app.logs.any_entry_contains(newest.strip()),
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                f"the Logs page does not list the burst's newest line {newest!r}; "
                f"first rows: {app.logs.entry_texts()[:5]!r}"
            ),
        )
    finally:
        driver.teardown()
