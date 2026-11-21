"""Pins for `helpers/source_scan.py`: the e2e tree's sources are read once per
process, the walk prunes what is never a test source, and a pin keyed on names
gets exactly the files that spell one of them.

Test taxonomy: tier_1 (in-process, no binary or driver).
"""

from pathlib import Path

import pytest

from helpers import source_scan

pytestmark = pytest.mark.tier_1


def _tree(root: Path, files: dict[str, str]) -> Path:
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


def test_the_tree_is_read_once_per_process():
    """Every pin asking of the same root gets the same mapping: a second read of
    the tree is the cost this helper exists to remove (on a loaded Windows one
    whole-tree read-and-parse is about a minute)."""
    assert source_scan.sources() is source_scan.sources()
    assert source_scan.sources(source_scan.E2E_ROOT) is source_scan.sources()


def test_the_walk_reads_python_only_and_prunes_venv_node_modules_and_pycache(tmp_path):
    root = _tree(tmp_path, {
        "conftest.py": "x = 1\n", "tests/test_a.py": "y = 2\n",
        "node_modules/pkg/x.py": "", "web/node_modules/y.py": "",
        "tools/.venv/lib/z.py": "", "tests/__pycache__/w.py": "", "notes.txt": "",
    })
    assert dict(source_scan.sources(root)) == {
        root / "conftest.py": "x = 1\n", root / "tests" / "test_a.py": "y = 2\n"}


def test_spelling_returns_only_the_files_that_spell_a_name_in_path_order(tmp_path):
    root = _tree(tmp_path, {
        "conftest.py": "def _make_nest(): ...\n",
        "helpers/h.py": "from x import provision_recipient_seal_key\n",
        "tests/test_b.py": "provision_recipient_seal_key()\n",
        "tests/test_a.py": "nothing_here()\n",
        "tests/sub/test_c.py": "derive_recipient_seal_key()\n",
    })
    names = {"provision_recipient_seal_key", "derive_recipient_seal_key"}
    assert [p.relative_to(root).as_posix() for p, _ in source_scan.spelling(names, root=root)] == [
        "helpers/h.py", "tests/sub/test_c.py", "tests/test_b.py"]


def test_spelling_honours_within_and_the_file_name_pattern(tmp_path):
    root = _tree(tmp_path, {
        "conftest.py": "needle\n", "helpers/h.py": "needle\n",
        "tests/test_a.py": "needle\n", "tests/util.py": "needle\n",
        "tests/sub/conftest.py": "needle\n",
    })
    rel = lambda found: [p.relative_to(root).as_posix() for p, _ in found]  # noqa: E731
    assert rel(source_scan.spelling({"needle"}, root=root, within=("conftest.py", "tests"))) == [
        "conftest.py", "tests/sub/conftest.py", "tests/test_a.py", "tests/util.py"]
    assert rel(source_scan.spelling({"needle"}, root=root, pattern="test_*.py")) == ["tests/test_a.py"]
    assert rel(source_scan.spelling({"needle"}, root=root, pattern="conftest.py")) == [
        "conftest.py", "tests/sub/conftest.py"]


def test_requiring_keeps_only_the_files_that_also_spell_every_required_word(tmp_path):
    root = _tree(tmp_path, {
        "a.py": "@pytest.fixture\ndef f(): build_node()\n",
        "b.py": "def g(): build_node()\n",
        "c.py": "@pytest.fixture\ndef h(): pass\n",
    })
    found = source_scan.spelling({"build_node"}, root=root, requiring={"fixture"})
    assert [p.name for p, _ in found] == ["a.py"]


def test_parsed_shares_one_tree_per_file_however_the_path_is_spelled(tmp_path):
    path = _tree(tmp_path, {"m.py": "def f():\n    return 1\n"}) / "m.py"
    tree = source_scan.parsed(path)
    assert source_scan.parsed(str(path)) is tree
    assert [n.name for n in tree.body] == ["f"]


def test_an_empty_name_set_is_refused_rather_than_matching_nothing():
    """A pin that computed its names and got none would otherwise scan no file
    and pass for nothing."""
    with pytest.raises(ValueError):
        source_scan.spells_any("anything", ())
