r"""tier_1: the windows driver answers where this launch's custodian store lives.

The orphaned-store witness
(`test_backups.py::test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store`)
reads the sealed store straight off disk, because that is the only observable that
tells "the page stopped offering the bytes" from "the space actually came back".
It finds the store through `custodian_store_roots(config_home, sync_agent_state_base)`.
windows has **no `config_home`** — that is an XDG concept, and the windows launch
relocates `%LOCALAPPDATA%` instead — so the answer has to come from the driver, and
the test's `getattr(..., "config_home", None)` is what makes its absence safe.

The join is a pure function of launch fields, so it is pinned here in milliseconds
against a fabricated store tree, with no FlaUI bridge, no app build and no nest.
What only a real run can show — that the agent really writes there — is the e2e's
own `assert roots` / `assert blobs_before`, which fail loudly rather than pass
vacuously.

⚠ A windows launch has three plausible-looking roots and only one is right:

* **the agent's `--data-dir`** — flat: the agent re-scopes under
  `<data-dir>/<actor-hex>/` itself, so the custodian store is
  `<data-dir>/<actor-hex>/backup-custodian`. THE ANSWER.
* `_resolved_store_root` (`<LOCALAPPDATA>\Fauna\sync`) — the unified ACCOUNT
  store's root, a declared sibling of the flat base. The custodian never writes
  there, so answering it would search an empty tree and read as "the pass stored
  nothing".
* `_data_dir` (`<LOCALAPPDATA>\Fauna`) — the APP's own flat base. Not the
  agent's either.

And there is ONE agent dir per launch: the SESSION-scoped `--data-dir` the launch
pins (`isolated_sync_agent_data_dir`). `custodian_pull_app` once spawned a second
agent on a per-test dir and pinned the driver to it; that agent exits as a
duplicate whenever the launch's own already serves the pipe, so the witness read
an empty dir. The fixture now adopts or starts the
launch's agent on the launch's dir (`helpers/windows_sync_agent.serving_agent`),
and the driver has nothing to override.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.windows import WindowsBridgeDriver  # noqa: E402
from helpers.sync_agent_config import (  # noqa: E402
    custodian_store_blobs,
    custodian_store_roots,
)

# tier_1 only, deliberately NO `windows` app marker — see the sibling
# `test_apple_custodian_store_root.py`: gating a pure-function contract behind
# `--app` would hide it behind the run nobody does by default.
pytestmark = pytest.mark.tier_1

ACTOR = "ab" * 32


def _seed_store(base: Path) -> Path:
    """One sealed blob in the store's own layout under `base`:
    `<base>/<actor-hex>/backup-custodian/blobs/<2-hex>/<64-hex>`."""
    blob = base / ACTOR / "backup-custodian" / "blobs" / "cd" / ("cd" * 32)
    blob.parent.mkdir(parents=True)
    blob.write_bytes(b"sealed")
    return blob


def _launched(root: Path) -> WindowsBridgeDriver:
    """A windows driver as `launch()` leaves it: the session body it POSTs to the
    bridge (whose `environment` is what the app process inherits — including the
    agent pin `_apply_isolated_sync_agent_env` sets) and the account-store root it
    publishes right after relocating `%LOCALAPPDATA%`."""
    driver = WindowsBridgeDriver()
    local = root / "localappdata"
    driver._resolved_store_root = str(local / "Fauna" / "sync")
    driver._session_body = {
        "app": "FaunaApp.exe",
        "args": [],
        "environment": {
            "LOCALAPPDATA": str(local),
            "FAUNA_E2E_SYNC_AGENT_DATA_DIR": str(root / "isolated-sync-agent-data"),
        },
    }
    return driver


def test_before_launch_answers_none_not_a_guess():
    assert WindowsBridgeDriver().sync_agent_state_base is None


def test_answers_the_agent_data_dir_the_launch_pinned(tmp_path):
    driver = _launched(tmp_path)
    assert driver.sync_agent_state_base == str(tmp_path / "isolated-sync-agent-data")


def test_a_launch_that_pinned_no_agent_answers_none(tmp_path):
    """No `FAUNA_E2E_SYNC_AGENT_DATA_DIR` -> the agent (if any) is the box's own,
    whose root this launch does not know. `None` makes `agent_state_base` name
    that loudly, where a guess would search the wrong tree and read as "the pass
    stored nothing".

    It asserts on the ENV rather than on a marker because which posture leaves
    the pin absent moved once already: since isolation became the windows
    default (`conftest._WINDOWS_ISOLATED_AGENT_IS_DEFAULT`, 2026-09-22), an
    unpinned launch is the deliberate `no_sync_agent` opt-out or a
    `real_sync_agent`-only installer journey rather than every ordinary run —
    and this behaviour held across that change untouched."""
    driver = _launched(tmp_path)
    driver._session_body["environment"].pop("FAUNA_E2E_SYNC_AGENT_DATA_DIR")
    assert driver.sync_agent_state_base is None


def test_never_answers_the_account_store_root_or_the_apps_flat_base(tmp_path):
    driver = _launched(tmp_path)
    driver._data_dir = str(tmp_path / "localappdata" / "Fauna")
    answer = driver.sync_agent_state_base
    assert answer != driver._resolved_store_root
    assert answer != driver._data_dir

    driver._session_body["environment"].pop("FAUNA_E2E_SYNC_AGENT_DATA_DIR")
    assert driver.sync_agent_state_base not in (
        driver._resolved_store_root,
        driver._data_dir,
    )


def test_the_agent_log_diagnostic_reads_the_launch_agents_dir(tmp_path):
    """`app_stderr_text` stitches the agent's own log in so a failure diagnoses
    itself (convention 6) — from the same dir `sync_agent_state_base` answers,
    read once rather than derived a second time."""
    driver = _launched(tmp_path)
    agent_dir = Path(driver.sync_agent_state_base)
    (agent_dir / "logs").mkdir(parents=True)
    (agent_dir / "logs" / "fauna.log.2026-09-21").write_text("custodian stint began")

    assert "custodian stint began" in driver.app_stderr_text()


def test_the_orphan_witness_finds_the_store_with_no_config_home(tmp_path):
    """The exact call the witness makes: `config_home` absent (windows has none),
    `sync_agent_state_base` from the driver — and the blob comes back."""
    driver = _launched(tmp_path)
    agent_dir = tmp_path / "isolated-sync-agent-data"
    blob = _seed_store(agent_dir)

    assert not hasattr(driver, "config_home"), (
        "the windows driver must not grow an XDG `config_home` — the witness's "
        "`getattr(..., None)` is what makes that safe, and a fake one would send "
        "the helper to a directory that never exists"
    )
    roots = custodian_store_roots(
        getattr(driver, "config_home", None),
        getattr(driver, "sync_agent_state_base", None),
    )
    assert roots == [agent_dir / ACTOR / "backup-custodian"]
    assert custodian_store_blobs(roots[0]) == [blob]

