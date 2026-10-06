r"""tier_1: both apple drivers answer where this launch's custodian store lives.

The orphaned-store witness
(`test_backups.py::test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store`)
reads the sealed store straight off disk, because that is the only observable that
tells "the page stopped offering the bytes" from "the space actually came back".
It finds the store through `custodian_store_roots(config_home, sync_agent_state_base)`
— and apple has **no `config_home` at all** (it is an XDG concept; both apple
launches pin HOME instead), so the answer has to come from the driver, and the
helper has to accept "no `config_home`" instead of raising `AttributeError` before
a single assertion runs.

These are tier_1 on purpose: the join is a pure function of two launch fields, so
it is pinned against a fabricated store tree in milliseconds, with no simulator, no
app build and no nest. What only a real run can show — that the agent / in-app
host really writes there — is the e2e's own `assert roots` / `assert blobs_before`,
which fail loudly rather than pass vacuously.

⚠ The two apple roots are DIFFERENT shapes, and the difference is the point:

* **macOS** — the external agent's user-domain root,
  `<launch HOME>/Library/Application Support/Fauna/sync`. Never the app-group
  container (moved out 2026-08-25).
* **iOS** — the in-app host's UNSCOPED base, `<container>/Library/Application
  Support/Fauna` (`AccountStateDir.base`). NOT the `Fauna/sync` root the
  iOS driver's `_resolved_store_root` publishes for the account store — the
  custodian never writes there, so answering that would search an empty
  directory and read as "the pass stored nothing".
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.ios import IosInProcessDriver  # noqa: E402
from drivers.macos import MacosInProcessDriver  # noqa: E402
from helpers.sync_agent_config import (  # noqa: E402
    agent_state_base,
    custodian_store_blobs,
    custodian_store_roots,
)

# tier_1 only, deliberately NO `macos`/`ios` app marker — see the sibling
# `test_ios_driver_app_log.py`: gating a pure-function contract behind `--app` would
# hide it behind the run nobody does by default.
pytestmark = pytest.mark.tier_1

ACTOR = "ab" * 32


def _seed_store(base: Path) -> Path:
    """One sealed blob in the store's own layout under `base`:
    `<base>/<actor-hex>/backup-custodian/blobs/<2-hex>/<64-hex>`."""
    blob = base / ACTOR / "backup-custodian" / "blobs" / "cd" / ("cd" * 32)
    blob.parent.mkdir(parents=True)
    blob.write_bytes(b"sealed")
    return blob


def _macos_launched(home: Path) -> MacosInProcessDriver:
    """A macOS driver as `launch()` leaves it: the `_resolved_store_root` line it
    publishes right after relocating HOME (`drivers/macos.py`)."""
    driver = MacosInProcessDriver.__new__(MacosInProcessDriver)
    driver._app_support = str(home / "Library" / "Application Support")
    driver._resolved_store_root = str(Path(driver._app_support) / "Fauna" / "sync")
    return driver


def _ios_launched(container_support: Path) -> IosInProcessDriver:
    driver = IosInProcessDriver()
    driver._app_support = str(container_support)
    driver._resolved_store_root = str(container_support / "Fauna" / "sync")
    return driver


def test_macos_answers_the_agents_user_domain_root(tmp_path):
    driver = _macos_launched(tmp_path)
    assert driver.sync_agent_state_base == str(
        tmp_path / "Library" / "Application Support" / "Fauna" / "sync"
    )
    assert "Group Containers" not in driver.sync_agent_state_base


def test_macos_before_launch_answers_none_not_a_guess():
    driver = MacosInProcessDriver.__new__(MacosInProcessDriver)
    assert driver.sync_agent_state_base is None


def test_ios_answers_the_unscoped_base_not_the_account_store_root(tmp_path):
    support = tmp_path / "Library" / "Application Support"
    driver = _ios_launched(support)
    assert driver.sync_agent_state_base == str(support / "Fauna")
    assert driver.sync_agent_state_base != driver._resolved_store_root


def test_ios_with_no_resolved_container_answers_none():
    driver = IosInProcessDriver()
    driver._app_support = None
    assert driver.sync_agent_state_base is None


@pytest.mark.parametrize("launched", ["macos", "ios"])
def test_the_orphan_witness_finds_the_store_with_no_config_home(tmp_path, launched):
    """The exact call the witness makes: `config_home` absent (apple has none),
    `sync_agent_state_base` from the driver — and the blob comes back."""
    if launched == "macos":
        driver = _macos_launched(tmp_path)
        store_base = Path(driver.sync_agent_state_base)
    else:
        driver = _ios_launched(tmp_path)
        store_base = Path(driver.sync_agent_state_base)
    blob = _seed_store(store_base)

    assert not hasattr(driver, "config_home"), (
        "apple drivers must not grow an XDG `config_home` — the witness's "
        "`getattr(..., None)` is what makes that safe, and a fake one would send "
        "the helper to a directory that never exists"
    )
    roots = custodian_store_roots(
        getattr(driver, "config_home", None),
        getattr(driver, "sync_agent_state_base", None),
    )
    assert roots == [store_base / ACTOR / "backup-custodian"]
    assert custodian_store_blobs(roots[0]) == [blob]


def test_no_config_home_and_no_base_is_a_named_error_not_a_type_error():
    """A driver that answers neither must fail loudly and say why, not raise a
    `TypeError` out of `Path(None)` three frames down."""
    with pytest.raises(RuntimeError, match="sync_agent_state_base"):
        agent_state_base(None, None)


def test_an_explicit_base_never_consults_config_home(tmp_path):
    assert agent_state_base(None, tmp_path) == tmp_path
    # The unix shape is unchanged: no base -> derived from the XDG config home.
    assert agent_state_base(tmp_path) == tmp_path / "fauna" / "sync"
