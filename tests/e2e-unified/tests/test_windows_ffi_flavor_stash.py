"""tier_1: a warm windows FFI flavor switch is a restage, never a build.

`_windows-ffi-flavor` stages every flavor into ONE fixed pair of paths, so a run
that needs two flavors switches between them. The in-hold re-check refuses every
`build` acquire, and an installer run reaches the dev flavor (the seeding
cdylib) and the release one (the MSI's input): each switch has to come back
without a slot, or the run gives its lane back and compiles under the refusal
(build-slot-pool.md § Cross-pool acquisition order → the warm pass).

The cargo half needs no help: a flavor's stamp and dll sit in its private slot,
which nothing else writes. The bindgen half does: `_windows-ffi-bindgen` builds
its tool under a slot, unconditionally. So each flavor keeps a stash of what its
bindgen staged, and `scripts/win-ffi-stash.sh` restores it — apple's mechanism
(`_apple-ffi-flavor`'s local fast path).

Real state, no mocks and no build: the shipped script runs against a real tree
laid out at the recipe's own paths.
"""

import os
import subprocess
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SCRIPT = _REPO / "scripts" / "win-ffi-stash.sh"

_BINDINGS = "apps/fauna-windows/FaunaApp/FaunaApp.Core/Generated/uniffi/fauna_ffi.cs"
_FAKE_BASES = "apps/fauna-windows/FaunaApp/FaunaApp.Tests/Generated/FakeBases.g.cs"
_STAGED_DLL = "apps/fauna-windows/FaunaApp/FaunaApp/runtimes/win-arm64/native/fauna_ffi.dll"


def _slot(flavor: str) -> str:
    return f"target/debug/windows-ffi/win-arm64/{flavor}"


def _run(tree: Path, action: str, flavor: str) -> subprocess.CompletedProcess:
    slot = _slot(flavor)
    return subprocess.run(
        ["bash", _SCRIPT.as_posix(), action, f"{slot}/staged", f"{slot}/bindgen.stamp",
         f"{slot}/fauna_ffi.dll", "win-arm64"],
        cwd=tree, capture_output=True, text=True, timeout=60,
    )


def _write(tree: Path, rel: str, text: str, mtime: float = 1_000.0) -> None:
    path = tree / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    os.utime(path, (mtime, mtime))


def _bindgen(tree: Path, flavor: str, *, mtime: float = 1_000.0) -> None:
    """What `_windows-ffi-bindgen` leaves: the pair staged, the flavor's own dll
    and its stamp."""
    _write(tree, _BINDINGS, f"bindings of {flavor}", mtime)
    _write(tree, _FAKE_BASES, f"fake bases of {flavor}", mtime)
    _write(tree, _STAGED_DLL, f"dll of {flavor}", mtime)
    _write(tree, f"{_slot(flavor)}/fauna_ffi.dll", f"dll of {flavor}", mtime)
    _write(tree, f"{_slot(flavor)}/bindgen.stamp", "stamp", mtime)


@pytest.fixture
def two_flavors(tmp_path):
    """Both flavors bindgen'd and stashed, the second one staged."""
    _bindgen(tmp_path, "dev")
    assert _run(tmp_path, "save", "dev").returncode == 0
    _bindgen(tmp_path, "release")
    assert _run(tmp_path, "save", "release").returncode == 0
    return tmp_path


def test_a_switch_back_restores_exactly_what_that_flavor_staged(two_flavors):
    tree = two_flavors
    (tree / _STAGED_DLL).unlink()  # the recipe's flavor guard

    proc = _run(tree, "restore", "dev")

    assert proc.returncode == 0, proc.stderr
    assert (tree / _BINDINGS).read_text() == "bindings of dev"
    assert (tree / _FAKE_BASES).read_text() == "fake bases of dev"
    assert (tree / _STAGED_DLL).read_text() == "dll of dev"


def test_a_restore_is_newer_than_anything_the_other_flavor_built(two_flavors):
    """The opposite of apple's `cp -p`, and the reason this is a test. Every
    flavor shares one `bin/` and `obj/`, and FaunaApp.csproj takes the dll
    `PreserveNewest`: a restored dll keeping its old mtime loses to the other
    flavor's copy already in `bin/`, a generated `.cs` keeping its old mtime
    reads up to date to the compiler, and the app ships the wrong core with no
    error anywhere. A restore has to look like the regeneration it replaces."""
    tree = two_flavors
    (tree / _STAGED_DLL).unlink()
    other_flavors_output = 2_000.0  # what a build of the other flavor left in bin/

    assert _run(tree, "restore", "dev").returncode == 0

    for rel in (_BINDINGS, _FAKE_BASES, _STAGED_DLL):
        assert (tree / rel).stat().st_mtime > other_flavors_output, rel


def test_a_restore_drops_a_generated_file_the_other_flavor_had(two_flavors):
    """The bindings of a test flavor must not survive into a production stage:
    the reason `_windows-ffi-bindgen` wipes the tree before it generates."""
    tree = two_flavors
    seam = tree / Path(_BINDINGS).parent / "seams_for_test.cs"
    seam.write_text("public void InjectForTest()")
    (tree / _STAGED_DLL).unlink()

    assert _run(tree, "restore", "dev").returncode == 0

    assert not seam.exists()


def test_a_stash_older_than_the_stamp_is_refused_and_the_tree_left_alone(two_flavors):
    """A bindgen run the stash never saw: its generation is the only one the
    stamp vouches for, so the recipe falls through to the slotted bindgen."""
    tree = two_flavors
    (tree / _STAGED_DLL).unlink()
    stamp = tree / _slot("dev") / "bindgen.stamp"
    stashed = tree / _slot("dev") / "staged" / ".stashed"
    newer = stashed.stat().st_mtime + 60
    os.utime(stamp, (newer, newer))

    proc = _run(tree, "restore", "dev")

    assert proc.returncode != 0
    assert (tree / _BINDINGS).read_text() == "bindings of release"


def test_no_stash_is_refused(tmp_path):
    _bindgen(tmp_path, "dev")
    (tmp_path / _STAGED_DLL).unlink()

    proc = _run(tmp_path, "restore", "dev")

    assert proc.returncode != 0
    assert (tmp_path / _BINDINGS).read_text() == "bindings of dev"


def test_a_stash_torn_mid_restore_leaves_no_staged_dll(two_flavors):
    """The dll lands last: the bindgen gate's `--target` existence check on it
    is what turns a half-restored tree into a regeneration."""
    tree = two_flavors
    (tree / _slot("dev") / "staged" / "FakeBases.g.cs").unlink()
    (tree / _STAGED_DLL).unlink()

    proc = _run(tree, "restore", "dev")

    assert proc.returncode != 0
    assert not (tree / _STAGED_DLL).exists()


def test_save_is_a_no_op_while_the_stash_is_current(two_flavors):
    tree = two_flavors
    stashed = tree / _slot("release") / "staged" / ".stashed"
    before = stashed.stat().st_mtime_ns
    _write(tree, _BINDINGS, "bindings of release")  # unchanged generation

    assert _run(tree, "save", "release").returncode == 0

    assert stashed.stat().st_mtime_ns == before


def test_a_newer_bindgen_replaces_the_stash(two_flavors):
    tree = two_flavors
    stashed = tree / _slot("release") / "staged" / ".stashed"
    newer = stashed.stat().st_mtime + 60
    _write(tree, _BINDINGS, "newer bindings of release")
    _write(tree, f"{_slot('release')}/bindgen.stamp", "stamp", newer)

    assert _run(tree, "save", "release").returncode == 0

    stash = tree / _slot("release") / "staged" / "uniffi" / "fauna_ffi.cs"
    assert stash.read_text() == "newer bindings of release"
    assert not (tree / f"{_slot('release')}/staged.tmp").exists()
