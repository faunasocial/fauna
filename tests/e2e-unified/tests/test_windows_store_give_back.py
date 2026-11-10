"""tier_1: the Store witness's venue and give-back, with no Credential Manager or
CIM call.

A Store build names no credential namespace of its own, so its sign-in writes this
Windows user's production rows, which ``helpers/windows_credman.py::sweep``
refuses to walk. Convention 12's Windows arm approves one venue for them, a
virtual machine's own Windows user, and
``tests/artifact/test_windows_store_install.py::sign_in_venue`` holds the run to
it: it refuses a machine that does not report itself a virtual machine
(``machine_is_virtual``), and its give-back (``give_back``) deletes by exact target
only the rows a snapshot taken before the run did not hold (``rows_gained``).
These pin the machine reading over the strings Windows reports, that set over
``CredentialRow`` values, the delete loop over a stand-in store, and the fence
that keeps a give-back to whole namespaces.
"""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import windows_credman  # noqa: E402
from helpers.windows_credman import (  # noqa: E402
    CRED_PERSIST_LOCAL_MACHINE,
    CRED_TYPE_GENERIC,
    CredentialRow,
    give_back,
    rows_gained,
)
from helpers.windows_store_install import machine_is_virtual  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]

PREFIXES = ("fauna-windows/", "fauna-sync-agent/")


@pytest.mark.parametrize("manufacturer, model", [
    ("the virtualization host International GmbH.", "the virtualization host ARM Virtual Machine"),
    ("Microsoft Corporation", "Virtual Machine"),
    ("VMware, Inc.", "VMware20,1"),
    ("QEMU", "Standard PC (Q35 + ICH9, 2009)"),
    ("innotek GmbH", "VirtualBox"),
])
def test_a_virtual_machine_is_read_as_one(manufacturer, model):
    assert machine_is_virtual(manufacturer, model)


@pytest.mark.parametrize("manufacturer, model", [
    ("Microsoft Corporation", "Surface Laptop, 7th Edition"),
    ("Dell Inc.", "XPS 13 9345"),
    ("LENOVO", "21N1"),
    ("", ""),
])
def test_a_physical_or_unreadable_machine_is_not(manufacturer, model):
    """A person's own PC, or a reading that says nothing, is refused: the check
    fails closed."""
    assert not machine_is_virtual(manufacturer, model)


def _row(target: str, blob: bytes = b"x") -> CredentialRow:
    return CredentialRow(target, CRED_TYPE_GENERIC, CRED_PERSIST_LOCAL_MACHINE, blob)


def _targets(rows: list[CredentialRow]) -> list[str]:
    return [row.target for row in rows]


def test_from_an_empty_snapshot_every_row_after_is_given_back():
    after = [_row("fauna-windows/index"), _row("fauna-sync-agent/account/a")]
    assert _targets(rows_gained([], after)) == [
        "fauna-sync-agent/account/a", "fauna-windows/index",
    ]


def test_a_row_the_snapshot_held_is_never_given_back_even_when_changed():
    before = [_row("fauna-windows/index", b"old")]
    after = [_row("fauna-windows/index", b"new"), _row("fauna-windows/account/a")]
    assert _targets(rows_gained(before, after)) == ["fauna-windows/account/a"]


def test_a_run_that_added_nothing_owes_nothing():
    rows = [_row("fauna-windows/index")]
    assert rows_gained(rows, rows) == []
    assert rows_gained([], []) == []


def test_a_row_the_run_removed_is_not_written_back():
    assert rows_gained([_row("fauna-windows/index")], []) == []


class _StandInStore:
    """Credential Manager's two calls a give-back makes, over a dict."""

    def __init__(self, rows: list[CredentialRow], *, deletes_stick: bool = True):
        self.rows = {row.target: row for row in rows}
        self.deleted: list[str] = []
        self.deletes_stick = deletes_stick

    def rows_under(self, prefix: str) -> list[CredentialRow]:
        return [row for target, row in sorted(self.rows.items()) if target.startswith(prefix)]

    def CredDeleteW(self, target, type_, flags):  # noqa: N802 — the Win32 name
        self.deleted.append(target)
        if self.deletes_stick:
            self.rows.pop(target, None)
        return True


def _stand_in(monkeypatch, store: _StandInStore) -> None:
    monkeypatch.setattr(windows_credman, "rows_under", store.rows_under)
    monkeypatch.setattr(windows_credman, "_advapi32", lambda: store)


def test_give_back_deletes_only_what_the_run_added_under_its_namespaces(monkeypatch):
    held = _row("fauna-sync-agent/held")
    store = _StandInStore([
        held, _row("fauna-windows/index"), _row("fauna-windows/account/a"),
        _row("another-program/row"),
    ])
    _stand_in(monkeypatch, store)
    give_back([held], PREFIXES)
    assert sorted(store.deleted) == ["fauna-windows/account/a", "fauna-windows/index"]
    assert sorted(store.rows) == ["another-program/row", "fauna-sync-agent/held"]


def test_a_gained_row_that_will_not_stay_gone_fails_naming_it(monkeypatch):
    store = _StandInStore([_row("fauna-windows/index")], deletes_stick=False)
    _stand_in(monkeypatch, store)
    monkeypatch.setattr(windows_credman, "_SWEEP_BUDGET_S", 0.5)
    with pytest.raises(AssertionError, match="fauna-windows/index"):
        give_back([], PREFIXES)


@pytest.mark.parametrize("prefix", ["", "*", "/", "fauna-windows", "fauna-*/", "a/b/"])
def test_a_prefix_that_is_not_one_whole_namespace_is_refused_before_any_call(
    monkeypatch, prefix
):
    def reached():
        raise AssertionError("the refused give-back reached Credential Manager")

    monkeypatch.setattr(windows_credman, "_advapi32", reached)
    monkeypatch.setattr(windows_credman, "rows_under", lambda _prefix: reached())
    with pytest.raises(RuntimeError, match="refusing to give back"):
        give_back([], ["fauna-windows/", prefix])
