"""The windows launch env keeps an exported `RUST_LOG` beside the configured one.

conftest sets `RUST_LOG` on most windows launches (its share-plane and
place-accepts targets). The windows driver took the exported value only when
the launch config set none, so a diagnosis run's own debug targets never
reached the app's `fauna.log` and nothing said so. linux and tui append the
exported directives to the configured ones (`test_linux_bridge_env.py`,
`test_tui_driver_env.py`); windows does the same.
"""

import pytest

pytestmark = pytest.mark.tier_1


def test_an_exported_rust_log_is_appended_to_the_configured_one(monkeypatch):
    from drivers import windows as win_driver

    monkeypatch.setenv("RUST_LOG", "fauna_archive_import_machine=debug")
    env = {"RUST_LOG": "info,fauna_sync_engine::share_glue=debug"}
    win_driver._merge_inherited_rust_log(env)
    assert env["RUST_LOG"] == (
        "info,fauna_sync_engine::share_glue=debug,fauna_archive_import_machine=debug"
    )


def test_an_exported_rust_log_is_used_when_none_is_configured(monkeypatch):
    from drivers import windows as win_driver

    monkeypatch.setenv("RUST_LOG", "info,fauna_archive_import_machine=debug")
    env = {}
    win_driver._merge_inherited_rust_log(env)
    assert env["RUST_LOG"] == "info,fauna_archive_import_machine=debug"


def test_the_configured_rust_log_stands_when_none_is_exported(monkeypatch):
    from drivers import windows as win_driver

    monkeypatch.delenv("RUST_LOG", raising=False)
    env = {"RUST_LOG": "info,fauna_conversations=debug"}
    win_driver._merge_inherited_rust_log(env)
    assert env["RUST_LOG"] == "info,fauna_conversations=debug"


def test_the_same_value_twice_is_not_doubled(monkeypatch):
    from drivers import windows as win_driver

    monkeypatch.setenv("RUST_LOG", "info,fauna_conversations=debug")
    env = {"RUST_LOG": "info,fauna_conversations=debug"}
    win_driver._merge_inherited_rust_log(env)
    assert env["RUST_LOG"] == "info,fauna_conversations=debug"
