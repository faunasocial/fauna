"""tier_1 — every app that runs `local-folder-sync` outcome 15's journey turns
on the engine's held-pass line in its launch environment.

The journey (`test_folder_place_accepts_hold.py`) asserts the hold through the
seat's own account of it. Each time B's place declines a pull, B's engine logs
one line (`PLACE_ACCEPTS_HELD_LINE`). That line is debug, on a target of its
own (`fauna_sync_engine::place_accepts`), so it reaches the log only when the
app's branch of `_build_app_config` calls `_apply_place_accepts_log_env`. The
agent inherits the env it sets: tui and linux spawn it directly, and windows'
detached spawner passes the app's env down.

What this guards against was measured 2026-11-16. The windows branch never
made the call, so the windows arm went red three times with the hold in force.
A missing debug target and a seat that never declined look identical from the
journey, and the red was read as a
seat-read defect. Building nothing and spawning
nothing, this test catches the dropped call in milliseconds, before a 30-minute
GUI run reads it as product behaviour.

The apps it holds to are the journey's own: those the
`folder-place-accepts-hold` surface lists as built (`surfaces.toml`).
"""

import pytest

import conftest
from conftest import PLACE_ACCEPTS_LOG_TARGET, _apply_place_accepts_log_env
from helpers.conftest_branches import branch_calls

# The registry's one reader (the one `helpers.app_surface` wraps), imported
# directly. `conftest` has put `scripts/` on the path already. `app_surface`
# would reach `fauna_ffi` through `feature_ledger`, and a run of this
# source-only file would then pre-build the cdylib.
import features_catalog

pytestmark = pytest.mark.tier_1


def test_every_app_the_hold_journey_runs_on_logs_its_held_line():
    built = set(features_catalog.load_surface_registry()["folder-place-accepts-hold"].built)
    wired = branch_calls(conftest._build_app_config, "_apply_place_accepts_log_env")
    missing = built - wired
    assert not missing, (
        f"_build_app_config branches {sorted(missing)} are built for the "
        "`folder-place-accepts-hold` journey but never call "
        "`_apply_place_accepts_log_env`, so their agents never log the held "
        "line the journey counts, and the journey reds with the hold in force. "
        "Add the call to the branch (see linux, tui and macos)."
    )


def test_the_target_is_merged_into_an_existing_rust_log():
    env = {"RUST_LOG": "info,fauna_share=debug"}
    _apply_place_accepts_log_env(env)
    assert env["RUST_LOG"] == f"info,fauna_share=debug,{PLACE_ACCEPTS_LOG_TARGET}=debug"


def test_the_target_rides_on_info_when_nothing_was_set():
    env = {}
    _apply_place_accepts_log_env(env)
    assert env["RUST_LOG"] == f"info,{PLACE_ACCEPTS_LOG_TARGET}=debug"
