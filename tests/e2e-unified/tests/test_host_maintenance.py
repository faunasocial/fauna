"""admin-nest host-OS-maintenance indicator + restart-now (installers/vps.md
§ Host OS Maintenance § 4) — the client UI half.

The admin/nest page renders a passive host-OS status line (`nest-os-maintenance-status`)
from the `os_*` fields on `fauna.setup.status` — "OS up to date" by default, or a
pending-updates count badge (`nest-os-updates-count`) / a "restart now" button
(`nest-os-restart-now-button`) when the host reports pending work. The button
dispatches `fauna.admin.request_host_restart`, which writes a `restart-requested`
flag the host coordinator picks up.

The state→line decision is shared Rust (`fauna_core::format::os_maintenance_status_label`,
over wasm for web / UniFFI for native), so all seven apps render the identical
line. The nest-side contract is `tests/api/test_host_maintenance.py`.

tier_3: a real client driver against a real `fauna-nest` binary. We inject a
`host-status` file into the nest's data dir to drive the non-default states (the
session nest has no host channel by default), then assert ground truth over the
same `fauna.setup.status` the line hydrates from. Files are torn down so the
shared session nest is left clean.
"""
import shutil
import time
from pathlib import Path

import pytest

from i18n.strings import S

# `tui` added 2026-07-29: the surface was already built (admin/nest.rs, 2026-07-18 —
# `admin.md` § N Nest: "All seven apps now render the host-OS-maintenance indicator
# + restart-now"); only this marker list predated it. The three ids the docstring
# names are UI elements; `host-status` / `maintenance-host` / `restart-requested`
# are the *filesystem* fixture paths below, not app ids.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.tui,
]


def _maintenance_dir(nest_instance) -> Path:
    """rw uid-1000 dir — the nest writes `restart-requested` here."""
    return Path(nest_instance["db_path"]).parent / "maintenance"


def _host_state_dir(nest_instance) -> Path:
    """`:ro` root-owned dir — the host coordinator writes `host-status` here; the
    nest only reads it (the HM-1/HM-2 trust split)."""
    return Path(nest_instance["db_path"]).parent / "maintenance-host"


def _write_host_status(nest_instance, body: str) -> Path:
    host_state = _host_state_dir(nest_instance)
    host_state.mkdir(parents=True, exist_ok=True)
    (host_state / "host-status").write_text(body)
    return host_state


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


@pytest.mark.feature("admin-nest")
def test_os_maintenance_status_up_to_date(admin_app, nest_instance):
    """With no host maintenance channel (the default), the admin-nest page shows
    the always-present status line reading 'OS up to date', and neither the count
    badge nor the restart-now button render (nothing is pending)."""
    shutil.rmtree(_maintenance_dir(nest_instance), ignore_errors=True)
    shutil.rmtree(_host_state_dir(nest_instance), ignore_errors=True)
    admin_app.admin.navigate_nest()
    assert _wait(admin_app.admin.os_maintenance_status_present), (
        "nest-os-maintenance-status missing on admin-nest. "
        f"error: {admin_app.error_text()!r}"
    )
    assert _wait(
        lambda: admin_app.admin.os_maintenance_status_text().strip()
        == S.admin.nest_page.os_up_to_date
    ), (
        "status line should read 'OS up to date' with no host channel. got: "
        f"{admin_app.admin.os_maintenance_status_text()!r}"
    )
    assert not admin_app.admin.os_updates_count_present(), (
        "the count badge must be absent when no updates are pending"
    )
    assert not admin_app.admin.os_restart_now_present(), (
        "the restart-now button must be absent when no reboot is pending"
    )


@pytest.mark.feature("admin-nest")
def test_os_restart_pending_shows_button_and_triggers(admin_app, nest_instance):
    """With a host-status reporting a pending reboot + security updates injected,
    the status line shows 'Restart pending …', the count badge shows the count,
    and the restart-now button appears. Clicking it dispatches
    `fauna.admin.request_host_restart`, proven by the `restart-requested` flag the
    coordinator consumes appearing in the rw maintenance mount."""
    host_state = _write_host_status(
        nest_instance,
        "security_updates_pending=3\nreboot_pending=true\n",
    )
    # The nest writes restart-requested into the rw `maintenance` dir; ensure it
    # exists so request_host_restart isn't rejected `no_host`.
    maint = _maintenance_dir(nest_instance)
    maint.mkdir(parents=True, exist_ok=True)
    try:
        admin_app.admin.navigate_nest()
        assert _wait(
            lambda: admin_app.admin.os_maintenance_status_text().strip()
            == S.admin.nest_page.os_restart_pending
        ), (
            "status line should read the restart-pending string. got: "
            f"{admin_app.admin.os_maintenance_status_text()!r} "
            f"error: {admin_app.error_text()!r}"
        )
        assert _wait(admin_app.admin.os_updates_count_present), (
            "the pending-updates count badge should render when updates are pending"
        )
        assert admin_app.admin.os_updates_count_text().strip() == "3", (
            f"count badge should read 3. got: {admin_app.admin.os_updates_count_text()!r}"
        )
        assert _wait(admin_app.admin.os_restart_now_present), (
            "the restart-now button should render when a reboot is pending"
        )

        # Click → fauna.admin.request_host_restart → the nest writes the flag the
        # coordinator consumes.
        admin_app.admin.os_restart_now()
        assert _wait(lambda: (maint / "restart-requested").exists()), (
            "clicking restart-now must write the restart-requested flag. "
            f"error: {admin_app.error_text()!r}"
        )
    finally:
        shutil.rmtree(maint, ignore_errors=True)
        shutil.rmtree(host_state, ignore_errors=True)
