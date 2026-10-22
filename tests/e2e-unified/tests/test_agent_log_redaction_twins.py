"""Pin: the e2e helpers that read the sync agent's log match the set name in
the REDACTED form the agent writes, never the plaintext.

Since 2026-10-18 the agent routes every set-name log field through
``fauna_core::log_redact::log_folder_name``. A barrier that still matched the
plaintext name counted zero restarts for ever, and
``test_webdav_shared_set.py``'s member barrier went red at "restart count stuck
at 0" while the engine was restarting. The vector below is the one
``log_redact.rs``'s ``log_folder_name_hashes_a_user_chosen_name`` pins, so the
Python twin and the Rust original cannot drift apart unseen.

Test taxonomy: tier_1 (in-process, no binary or driver).
"""

from types import SimpleNamespace

import pytest

from helpers.folder_content import engine_restart_count, log_folder_name

pytestmark = pytest.mark.tier_1


def _app_with_log(text: str):
    return SimpleNamespace(driver=SimpleNamespace(app_stderr_text=lambda: text))


def test_log_folder_name_matches_the_rust_vector():
    assert log_folder_name("My Photos 2026") == "name~b8b858714318"


def test_engine_restart_count_counts_the_redacted_restart_lines_of_its_set_only():
    mine = log_folder_name("shared-dav-0a1b2c3d")
    other = log_folder_name("shared-dav-ffffffff")
    restart = "engine key material, folder or mode changed; restarting engine"
    log = "\n".join([
        f"2026-10-20T17:00:00Z  INFO fauna_sync_agent::engine_driver: {restart} folder={mine} mode=Mirror",
        f"2026-10-20T17:00:01Z  INFO fauna_sync_agent::engine_driver: serving folder folder={mine} mode=Mirror",
        f"2026-10-20T17:00:02Z  INFO fauna_sync_agent::engine_driver: {restart} folder={other} mode=Mirror",
        f"2026-10-20T17:00:03Z  INFO fauna_sync_agent::engine_driver: {restart} folder={mine} mode=Mirror",
    ])
    assert engine_restart_count(_app_with_log(log), "shared-dav-0a1b2c3d") == 2


def test_engine_restart_count_ignores_a_plaintext_name():
    # The agent never writes this; a line that did must not satisfy a barrier
    # keyed on what the agent actually writes.
    log = "INFO engine_driver: restarting engine folder=shared-dav-0a1b2c3d"
    assert engine_restart_count(_app_with_log(log), "shared-dav-0a1b2c3d") == 0


def test_engine_restart_count_is_zero_when_the_log_is_unreadable():
    def _boom():
        raise OSError("gone")

    app = SimpleNamespace(driver=SimpleNamespace(app_stderr_text=_boom))
    assert engine_restart_count(app, "x") == 0
