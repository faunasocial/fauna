"""The fleet-scope census leaves a stopped nest's database alone (tier_1).

`helpers/fleet_scope_census.observe` runs after every test's fixture
finalizers, and the last test's finalizers stop the session nest: its cleanup is
`terminate()` (TerminateProcess on Windows), so the WAL and -shm are left as the
killed process had them. Opening that db read-only crashed python.exe with an
access violation inside sqlite3 on Windows — no exception to catch, the run ended
with no verdict and the background tester lost the shard's last-green merge.
The census therefore never opens the db of a nest whose process has exited,
and still reads a running one.
"""

import pytest

pytestmark = pytest.mark.tier_1

from helpers import fleet_scope_census


class _Proc:
    def __init__(self, returncode):
        self._returncode = returncode

    def poll(self):
        return self._returncode


@pytest.fixture
def opened(monkeypatch):
    """Record every db the census opens, standing in for sqlite3."""
    seen: list[str] = []

    class _Conn:
        def close(self):
            pass

    def fake_open(db_path):
        seen.append(db_path)
        return _Conn()

    monkeypatch.setattr(fleet_scope_census, "_open", fake_open)
    monkeypatch.setattr(fleet_scope_census, "_read", lambda conn, actor: None)
    monkeypatch.setitem(fleet_scope_census._state, "disabled", None)
    from helpers import shared_identity

    monkeypatch.setattr(shared_identity, "shared_actors", lambda: {"ab" * 32})
    return seen


def test_a_stopped_nest_is_not_opened(opened):
    fleet_scope_census.observe({"db_path": "x/nest.db", "proc": _Proc(1)}, "t::a")
    assert opened == []


def test_a_running_nest_is_read(opened):
    fleet_scope_census.observe({"db_path": "x/nest.db", "proc": _Proc(None)}, "t::a")
    assert opened == ["x/nest.db"]


def test_a_nest_with_no_local_process_is_read(opened):
    """A docker or live nest carries no `proc`: the guard reads only what it can see."""
    fleet_scope_census.observe({"db_path": "x/nest.db"}, "t::a")
    assert opened == ["x/nest.db"]
