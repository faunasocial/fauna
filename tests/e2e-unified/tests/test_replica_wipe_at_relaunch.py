"""tier_1: `wipe_replica_at_next_relaunch` withholds one replica restore, once.

A relaunch carries the signed-in actor's writer key and its replica together
(`e2e-launch-isolation.md`, the principal carry). The hook makes exactly the
next restore lay down no replica — a machine whose local store was wiped —
and every later restore carries it again; a driver with no live carry says so
rather than pretending (`test_offline_preference_edits.py` is its consumer).
"""

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.http_bridge import HttpBridgeDriver  # noqa: E402

ACTOR = "ab" * 32


def _driver(tmp_path: Path, *, carry_live: bool = True) -> HttpBridgeDriver:
    d = HttpBridgeDriver()
    src = tmp_path / "vault" / ACTOR
    src.mkdir(parents=True)
    (src / "journal").write_text("rows")
    d._replica_vault = {ACTOR: str(src)}
    d._principal_slot_carry_live = carry_live
    d._resolved_store_root = str(tmp_path / "root")
    return d


def _replica(tmp_path: Path) -> Path:
    return tmp_path / "root" / ACTOR / "account-store"


def test_the_next_restore_lays_down_no_replica_and_the_one_after_does(tmp_path):
    d = _driver(tmp_path)
    assert d.wipe_replica_at_next_relaunch(ACTOR)
    d._restore_replica(ACTOR)
    assert not _replica(tmp_path).exists()
    d._restore_replica(ACTOR)
    assert (_replica(tmp_path) / "journal").read_text() == "rows"


def test_a_driver_with_no_live_carry_refuses(tmp_path):
    d = _driver(tmp_path, carry_live=False)
    assert not d.wipe_replica_at_next_relaunch(ACTOR)


def _launch(d: HttpBridgeDriver, tmp_path: Path, config: dict) -> None:
    """One `launch()`'s carry half, as linux and tui run it: a fresh
    file-backed credential dir in the env, the config the driver re-uses on
    every relaunch."""
    d._launch_config = config
    d._begin_principal_slot_carry(config, {"FAUNA_E2E_CREDENTIAL_DIR": str(tmp_path / "creds")})


def test_a_pin_cleared_by_reset_leaves_the_next_relaunch_carrying(tmp_path):
    """The leak `test_offline_preference_edits.py`'s first-sync witness met on
    tui: test 1 pinned the store and relaunched, so that launch decided "no
    carry" (its store was the pin's). `reset()` at the next test's boundary
    drops the pin, so the NEXT relaunch carries again — and the hook, which is
    about that relaunch, must say so rather than read the pinned launch's
    decision."""
    d = HttpBridgeDriver()
    config: dict = {"app_path": "/nonexistent"}
    _launch(d, tmp_path, config)
    config.update(credential_dir=str(tmp_path / "creds"), keyring_app="fauna-e2e-agent-1")
    d._record_relaunch_pin(config, ("credential_dir", "keyring_app"))
    _launch(d, tmp_path, config)  # the pinned relaunch
    assert not d.wipe_replica_at_next_relaunch(ACTOR), "a pinned relaunch carries nothing"
    assert not d._discards_machine_at_teardown()

    d._clear_relaunch_pin()  # the next test's reset()

    assert d.wipe_replica_at_next_relaunch(ACTOR)
    assert d._discards_machine_at_teardown()


def test_a_store_the_caller_supplied_never_carries_pin_or_not(tmp_path):
    d = HttpBridgeDriver()
    _launch(d, tmp_path, {"app_path": "/nonexistent", "credential_dir": str(tmp_path / "theirs")})
    d._clear_relaunch_pin()
    assert not d.wipe_replica_at_next_relaunch(ACTOR)
    assert not d._discards_machine_at_teardown()


# ── android's arm: the replica rests on the device, so the wipe is a delete ──

from drivers.android import AndroidBridgeDriver  # noqa: E402


class _Done:
    def __init__(self, stdout: str):
        self.stdout, self.stderr, self.returncode = stdout, "", 0


def _android(monkeypatch, config: dict | None, *, device_says: str = "FAUNA_WIPED\n"):
    """An android driver whose bridge and adb are recorded, not reached."""
    d = AndroidBridgeDriver()
    if config is not None:
        d._launch_config = config
    calls: list = []
    monkeypatch.setattr(d, "_read_exit_before_close", lambda close: None)
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(d, "_delete", lambda path, *a, **k: calls.append(("DELETE", path)) or {})
    monkeypatch.setattr(d, "_post", lambda path, body=None, **k: calls.append(("POST", path)) or {})
    monkeypatch.setattr(d, "_mark_log_baseline", lambda: None)
    monkeypatch.setattr(d, "_mark_app_launch", lambda: None)
    monkeypatch.setattr(d, "_vault_slots_before_erase", lambda: None)
    monkeypatch.setattr(d, "_adb_cmd", lambda: ["adb"])

    def run(argv, **kwargs):
        calls.append(("adb", argv[-1]))
        return _Done(device_says)

    monkeypatch.setattr("drivers.android.subprocess.run", run)
    return d, calls


def test_android_removes_the_replica_between_the_stop_and_the_start_once(monkeypatch):
    d, calls = _android(monkeypatch, {"app_path": "/x.apk"})
    assert d.wipe_replica_at_next_relaunch(ACTOR.upper())
    assert d.recover()
    assert [c[0] for c in calls] == ["DELETE", "adb", "POST"], calls
    assert f"rm -rf files/account-store/{ACTOR}/account-store" in calls[1][1]

    calls.clear()
    assert d.recover()
    assert [c[0] for c in calls] == ["DELETE", "POST"], "the wipe is consumed by one relaunch"


def test_android_refuses_before_a_launch_under_a_pin_and_on_the_production_store(monkeypatch):
    d, _ = _android(monkeypatch, None)
    assert not d.wipe_replica_at_next_relaunch(ACTOR)

    d, _ = _android(monkeypatch, {"production_store": True})
    assert not d.wipe_replica_at_next_relaunch(ACTOR)

    d, _ = _android(monkeypatch, {"app_path": "/x.apk"})
    monkeypatch.setattr(d, "supports_cold_relaunch", lambda: True)
    assert d.preserve_state_across_relaunch()
    assert not d.wipe_replica_at_next_relaunch(ACTOR), "a pinned store survives by design"
    d._clear_relaunch_pin()
    assert d.wipe_replica_at_next_relaunch(ACTOR)


def test_android_a_refused_device_delete_fails_the_relaunch_loudly(monkeypatch):
    d, _ = _android(
        monkeypatch, {"app_path": "/x.apk"}, device_says="run-as: package not debuggable\n"
    )
    assert d.wipe_replica_at_next_relaunch(ACTOR)
    with pytest.raises(RuntimeError, match="refused on the device"):
        d.recover()
