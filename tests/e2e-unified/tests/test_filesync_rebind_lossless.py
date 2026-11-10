"""Re-binding a computer's folder to a different set, or binding one path to two
sets at once, loses no edit and skips no file.

Owner doc: ``docs/goal/behavior/conflicts.md`` § Implementation status today,
*The ENGINE's two state roots are scoped the same way* (2026-09-20): the
engine's merge base and causal frontier live under the watch directory, and the
watch directory is what gets re-bound — so before they were scoped per
``(device, set)`` a re-bind carried set A's frontier into set B, and receiver
rule 1 skipped every incoming B row whose ``seq`` sat at or below it, byte-free,
while the anchor advanced past them. The concurrent shape is the same hazard
with two live engines sharing one root — a state no app UI reaches, because
every app keys its bindings by path, so binding a bound path under a second set
re-points it in place (the second test). Tier_1 pin:
``anchor_accounting_test`` (sequential re-bind, concurrent double-bind); this
module is the app-level witness.

The flow every test here asserts::

    the harness writer records set B's history, then set A's (so A's frontier
      passes every B seq — the shape that used to be skipped)
      -> the app binds a folder (the Folders UI's own bind/unbind code path,
         `sync_add_location` / `sync_remove_location`)
      -> its agent's engine pulls and applies each set's rows
      -> the test reads the folder on disk, and each set's recorded changes on
         the nest for what the app published

Bodies differ per file, and every wait is a deadline poll on observed state
(convention 14). The writer is ``helpers.harness_writer`` — a returned
``sync()`` means the records are on the nest.
"""

from __future__ import annotations

import shutil
import tempfile
from pathlib import Path

import pytest

from common.auth import (
    sealed_path,
    sync_changes_list,
    user_create_folder,
    user_folder_ref,
)
from helpers.harness_writer import HarnessWriter
from helpers.waiting import wait_until

# The fixture: `logged_in_app` with each platform's agent serving before login;
# one owner, imported rather than copied.
from tests.test_sync_live_apply import _diagnose, sync_live_apply_app  # noqa: F401

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; linux drives the same shared bindings model through the same
    # two commands (`test_sync_live_apply.py`).
    pytest.mark.tui,
    pytest.mark.linux,
    # macos joined 2026-11-09 over the same two commands, which route through
    # `LocationsModel.add`/`.remove` there; its agent is the private child the
    # `real_sync_agent` launch builds, and `isolated_sync_agent` keeps a windows
    # app in the same invocation on its own pipe — `test_sync_live_apply.py`'s
    # macos note and markers.
    pytest.mark.macos,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
]

WINDOW = 180.0


@pytest.fixture
def rebind_app(app, sync_live_apply_app):  # noqa: F811
    """The signed-in app with its agent serving — ``sync_live_apply_app``,
    requested beside ``app`` so the catalog scan reads this module as driving
    the app through its own UI (the scan does not follow an imported fixture)."""
    return sync_live_apply_app


def _secret(test_user) -> str:
    return test_user["signing_key"].encode().hex()


def _new_set(nest, test_user, name: str) -> str:
    """Create ``name`` custody-first as the signed-in account; return its ref."""
    port, url, sk = nest["port"], nest["url"], _secret(test_user)
    user_create_folder(port, name, secret_key=sk, base_url=url)
    return user_folder_ref(port, name, secret_key=sk, base_url=url)


def _bind(app, path: Path, name: str, ref: str) -> None:
    app.driver.call_command("sync_add_location",
                            {"path": str(path), "folder": name, "folder_id": ref})


def _bound(app) -> set[tuple[str, str]]:
    sync = ((app.driver.get_state() or {}).get("data") or {}).get("sync") or {}
    return {(loc.get("path"), loc.get("folder")) for loc in sync.get("locations", [])}


def _await_bound(app, want: set, what: str) -> None:
    wait_until(lambda: _bound(app) == want, 120.0, interval=1.0,
               diagnose=lambda: f"{what}: bound {_bound(app)!r}, wanted {want!r}; "
                                f"{_diagnose(app)}")


def _await_files(app, root: Path, files: dict[str, bytes], what: str) -> None:
    def wrong() -> list[str]:
        out = []
        for rel, body in files.items():
            try:
                have = (root / rel).read_bytes()
            except OSError:
                have = None
            if have != body:
                out.append(rel)
        return out

    wait_until(lambda: not wrong(), WINDOW, interval=1.0,
               diagnose=lambda: f"{what}: never on disk as written: {wrong()!r}; "
                                f"{_diagnose(app)}")


def _recorded_sizes(nest, test_user, folder: str, rel: str) -> set[int]:
    """The sizes of every live (non-delete) change ``folder`` records for ``rel``."""
    sk = _secret(test_user)
    seal = sealed_path(sk, rel)
    rows = sync_changes_list(nest["port"], secret_key=sk, folder=folder,
                             base_url=nest["url"])["changes"]
    return {c.get("size_bytes") for c in rows
            if bytes(c.get("path_sealed") or b"") == seal and c.get("change_type") != "delete"}


def _await_recorded(app, nest, test_user, folder: str, rel: str, body: bytes, what: str) -> None:
    wait_until(lambda: len(body) in _recorded_sizes(nest, test_user, folder, rel),
               WINDOW, interval=1.0,
               diagnose=lambda: f"{what}: {folder!r} never recorded {rel} at "
                                f"{len(body)} bytes (have "
                                f"{sorted(_recorded_sizes(nest, test_user, folder, rel))}); "
                                f"{_diagnose(app)}")


def _histories(nest, test_user, tmp: Path, tag: str):
    """Two sets: B's history written FIRST (its seqs lowest), then A's, longer —
    so A's frontier passes every B seq whatever the nest numbers them by.
    Returns ``(set_a, ref_a, a_files, writer_a, set_b, ref_b, b_files, writer_b)``."""
    url, sk = nest["url"], _secret(test_user)
    set_a, set_b = f"rebind-{tag}-a", f"rebind-{tag}-b"
    ref_a, ref_b = _new_set(nest, test_user, set_a), _new_set(nest, test_user, set_b)
    b_files = {f"b{i}.txt": f"set b's file {i}\n".encode() * i for i in range(1, 4)}
    a_files = {f"a{i}.txt": f"set a's file {i}\n".encode() * i for i in range(1, 7)}
    writer_b = HarnessWriter(url, sk, ref_b, tmp / "writer-b")
    for rel, body in b_files.items():
        writer_b.write(rel, body)
    writer_b.sync(expect=list(b_files))
    writer_a = HarnessWriter(url, sk, ref_a, tmp / "writer-a")
    for rel, body in a_files.items():
        writer_a.write(rel, body)
    writer_a.sync(expect=list(a_files))
    return set_a, ref_a, a_files, writer_a, set_b, ref_b, b_files, writer_b


@pytest.mark.timeout(int(8 * WINDOW + 300))
@pytest.mark.feature("local-folder-sync")
def test_rebinding_a_folder_to_another_set_loses_no_edit_and_skips_no_file(
    rebind_app, nest_instance, test_user
):
    """Bind a folder to set A, edit in it, re-bind it to set B: every one of B's
    files arrives (none skipped below A's frontier), nothing local is lost, the
    edit reaches B, and B's later changes keep arriving."""
    app = rebind_app
    tmp = Path(tempfile.mkdtemp(prefix="fauna-rebind-"))
    try:
        (set_a, ref_a, a_files, _wa, set_b, ref_b, b_files, writer_b) = _histories(
            nest_instance, test_user, tmp, "seq")
        watch = tmp / "watch"
        watch.mkdir()

        _bind(app, watch, set_a, ref_a)
        _await_bound(app, {(str(watch), set_a)}, "bind to set A")
        _await_files(app, watch, a_files, "set A's history")

        # The user's edit while bound to A: a new file and a changed one.
        mine = b"written on this computer while the folder synced set a\n"
        (watch / "mine.txt").write_bytes(mine)
        edited = b"set a's file 1, edited on this computer\n"
        (watch / "a1.txt").write_bytes(edited)
        _await_recorded(app, nest_instance, test_user, set_a, "mine.txt", mine,
                        "the new file published to set A")
        _await_recorded(app, nest_instance, test_user, set_a, "a1.txt", edited,
                        "the edit published to set A")

        # ── Re-bind: the same folder, now set B. ──
        app.driver.call_command("sync_remove_location", {"folder": set_a})
        _await_bound(app, set(), "unbind from set A")
        _bind(app, watch, set_b, ref_b)
        _await_bound(app, {(str(watch), set_b)}, "re-bind to set B")

        # THE assertion: every B row lands, though each sits below A's frontier.
        _await_files(app, watch, b_files, "set B's history after the re-bind")
        # Nothing local is lost: the user's edit, the new file and A's files stay.
        _await_files(app, watch, {**a_files, "a1.txt": edited, "mine.txt": mine},
                     "the folder's own files after the re-bind")
        # The edit travels on into the set the folder now syncs.
        _await_recorded(app, nest_instance, test_user, set_b, "mine.txt", mine,
                        "the new file published to set B")
        _await_recorded(app, nest_instance, test_user, set_b, "a1.txt", edited,
                        "the edit published to set B")

        # And B's later changes keep arriving.
        later = b"a later change to set b, after the re-bind\n"
        writer_b.write("b-later.txt", later)
        writer_b.sync(expect=["b-later.txt"])
        _await_files(app, watch, {"b-later.txt": later}, "set B's later change")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


@pytest.mark.timeout(int(8 * WINDOW + 300))
@pytest.mark.feature("local-folder-sync")
def test_one_path_bound_to_two_sets_loses_no_edit_and_skips_no_file(
    rebind_app, nest_instance, test_user
):
    """Binding a folder already bound to set A under set B as well. Every app's
    bindings are keyed by the folder's path (`fauna_client_sync::agent::
    LocationBindingsModel::add` replaces any row for the same path), so the
    second bind RE-POINTS the folder at B in place — the ref-keyed
    ``SetLocationFolder`` the goal doc names as the hazard, with no unbind in
    between. Asserted: the folder ends bound to B alone, every one of B's files
    arrives though each sits below A's frontier, the folder's own files and
    edit stay, the edit reaches B, and B's later changes keep arriving.

    Two LIVE engines over one root (one path in two sets at once) is a state no
    app UI reaches, since the model holds one row per path; the engine half of
    it is pinned at tier_1 (``anchor_accounting_test``, the concurrent
    double-bind).
    """
    app = rebind_app
    tmp = Path(tempfile.mkdtemp(prefix="fauna-doublebind-"))
    try:
        (set_a, ref_a, a_files, _wa, set_b, ref_b, b_files, writer_b) = _histories(
            nest_instance, test_user, tmp, "two")
        watch = tmp / "watch"
        watch.mkdir()

        _bind(app, watch, set_a, ref_a)
        _await_bound(app, {(str(watch), set_a)}, "bind to set A")
        _await_files(app, watch, a_files, "set A's history")

        mine = b"written on this computer while the folder synced set a, in place\n"
        (watch / "mine.txt").write_bytes(mine)
        _await_recorded(app, nest_instance, test_user, set_a, "mine.txt", mine,
                        "the new file published to set A")

        # ── The same path, bound under set B too: re-pointed in place. ──
        _bind(app, watch, set_b, ref_b)
        _await_bound(app, {(str(watch), set_b)}, "the path re-pointed at set B")

        # THE assertion: A's frontier, carried in place, hides none of B's rows.
        _await_files(app, watch, b_files, "set B's history after the in-place re-point")
        _await_files(app, watch, {**a_files, "mine.txt": mine},
                     "the folder's own files after the in-place re-point")
        _await_recorded(app, nest_instance, test_user, set_b, "mine.txt", mine,
                        "the new file published to set B")

        later = b"a later change to set b, after the in-place re-point\n"
        writer_b.write("b-later.txt", later)
        writer_b.sync(expect=["b-later.txt"])
        _await_files(app, watch, {"b-later.txt": later}, "set B's later change")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
