"""Conftest for tests/e2e-unified/tests/.

Re-exports the `fake_cloud` fixture (and its loopback bind address / threaded
server override) so platform tests in
this directory can request it. Pytest only auto-discovers fixtures from
conftest.py files; the fixtures themselves live in `fakes/fake_cloud.py` so
non-test callers can import them directly. Mirrors `fakes/conftest.py` (which
scopes the same fixtures to the fakes/ self-tests).
"""
import os

import pytest

from fakes.fake_cloud import (  # noqa: F401
    fake_cloud,
    httpserver_listen_address,
    make_httpserver,
)


@pytest.fixture(autouse=True)
def _isolate_seed_ledger(request, monkeypatch, tmp_path):
    """The cargo-target seed tool's SEED_LOG is a module constant with no
    override. Any test module that loads
    it under the module-level name `seed` — test_cargo_target_seed{,_mac,_win}.py,
    each via its own `_load_seed_module()` — gets it redirected here
    automatically, so a test driving `main()` end-to-end can never append to
    the real `~/.cache/fauna-cargo-target-seeds.log` just because it didn't
    think to request a ledger fixture. One shared home rather than copying
    this into all three files."""
    seed_module = getattr(request.module, "seed", None)
    if seed_module is not None:
        monkeypatch.setattr(
            seed_module, "SEED_LOG", str(tmp_path / "seed-ledger.log")
        )


#: What a granted acquire_slot exports into its holder's environment and every
#: descendant inherits (build-slot-pool.md § Build/e2e slot locks): the held
#: marker, the slot index, the fenced cpu list, the core figure and win's job
#: name. Prefixes for the per-pool names, whole names for the rest.
_GRANT_ENV_PREFIXES = ("FAUNA_SLOT_HELD_", "FAUNA_SLOT_INDEX_", "FAUNA_SLOT_CPUSET_")
_GRANT_ENV_NAMES = ("FAUNA_SLOT_CORES", "FAUNA_SLOT_JOB")


@pytest.fixture
def no_inherited_slot(monkeypatch):
    """Start the test from an environment that holds no slot. A whole-suite e2e
    run takes `e2e_long` and its lane in-process, so every test it collects
    inherits `FAUNA_SLOT_HELD_E2E_LONG`, `_E2E_<LANE>` and the family's
    `_E2E` — and the slot code treats a held marker as its own ancestry, so
    reentrancy outranks the cross-pool order and the disk floor's refusal
    (build-slot-pool.md § Cross-pool acquisition order). A test of a refusal
    run there would see a re-entrant grant instead.
    Opt-in, never autouse: an app
    test's nested build recipe needs that same inherited marker to stay
    re-entrant. Subprocesses a test builds from `dict(os.environ)` inherit the
    scrub; monkeypatch restores everything afterwards."""
    for var in list(os.environ):
        if var.startswith(_GRANT_ENV_PREFIXES) or var in _GRANT_ENV_NAMES:
            monkeypatch.delenv(var)
