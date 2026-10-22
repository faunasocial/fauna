"""tier_1: a discarded machine's replica vault goes with it.

`HttpBridgeDriver` keeps harvested account replicas in a vault dir so they
survive the machine's relaunches (`e2e-launch-isolation.md`). A teardown that
discards the machine has no further use for it, and left behind it is one more
stray dir in the shared temp dir (`testing.md` § Test temp-dir hygiene); a
machine a relaunch pin keeps is not discarded, so its vault must stay.
"""

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.http_bridge import HttpBridgeDriver  # noqa: E402

class _Driver(HttpBridgeDriver):
    def _retire_discarded_machine(self) -> None:
        pass

    def _stop_app(self, **kwargs) -> None:
        pass


def _driver_with_vault(tmp_path: Path, *, pinned: bool) -> tuple[_Driver, Path]:
    d = _Driver()
    vault = tmp_path / "fauna-e2e-replica-vault-x"
    (vault / "actor").mkdir(parents=True)
    d._replica_vault_dir = str(vault)
    d._replica_vault = {"actor": str(vault / "actor")}
    d._principal_slot_carry_live = True
    d._preserve_pinned_keys = ("xdg_base",) if pinned else None
    return d, vault


def test_teardown_of_a_discarded_machine_removes_its_replica_vault(tmp_path):
    d, vault = _driver_with_vault(tmp_path, pinned=False)
    d.teardown()
    assert not vault.exists()
    assert "_replica_vault" not in d.__dict__ and "_replica_vault_dir" not in d.__dict__


def test_teardown_of_a_pinned_machine_keeps_its_replica_vault(tmp_path):
    d, vault = _driver_with_vault(tmp_path, pinned=True)
    d.teardown()
    assert vault.is_dir(), "a pin keeps the machine, so its vault must survive"
