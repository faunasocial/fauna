"""Pin: every `alias.<name>` a test module reads off a sibling test module
(`from . import test_x as alias`) names something that sibling defines.

A helper that moves out of a test module (the zero-cheat live helpers went to
`helpers.live_admin`) leaves its other callers dying with `AttributeError` only
when they RUN — and the tier_4 live modules run against a shared box, rarely.
This reds at tier_1 instead. Static (AST) only: no module is imported.

Test taxonomy: tier_1 (in-process, no binary or driver).
"""

import ast
import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

TESTS_DIR = Path(__file__).parent


def _top_level_names(tree: ast.Module) -> set[str]:
    names: set[str] = set()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, (ast.Assign, ast.AnnAssign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for t in targets:
                for n in ast.walk(t):
                    if isinstance(n, ast.Name):
                        names.add(n.id)
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for a in node.names:
                names.add((a.asname or a.name).split(".")[0])
        elif isinstance(node, (ast.If, ast.Try)):
            # conditional definitions: count every name bound inside
            names |= _top_level_names(ast.Module(body=node.body, type_ignores=[]))
            for extra in getattr(node, "orelse", []):
                names |= _top_level_names(ast.Module(body=[extra], type_ignores=[]))
    return names


def _sibling_aliases(tree: ast.Module) -> dict[str, str]:
    """alias -> sibling module name, from `from . import mod [as alias]`."""
    out: dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level == 1 and node.module is None:
            for a in node.names:
                out[a.asname or a.name] = a.name
    return out


def dangling_refs(path: Path) -> list[str]:
    tree = ast.parse(path.read_text())
    aliases = _sibling_aliases(tree)
    if not aliases:
        return []
    defined: dict[str, set[str]] = {}
    for mod in set(aliases.values()):
        sibling = TESTS_DIR / f"{mod}.py"
        if sibling.exists():
            defined[mod] = _top_level_names(ast.parse(sibling.read_text()))
    bad = []
    for node in ast.walk(tree):
        if (
            isinstance(node, ast.Attribute)
            and isinstance(node.value, ast.Name)
            and node.value.id in aliases
        ):
            mod = aliases[node.value.id]
            if mod in defined and node.attr not in defined[mod]:
                bad.append(f"{path.name}:{node.lineno}: {node.value.id}.{node.attr} — {mod} has no `{node.attr}`")
    return bad


#: `from . import mod` in any spacing — the only import a sibling alias comes
#: from. A module whose text has none has no alias, so it is never parsed
#: (`helpers/source_scan.py`; a parse of every test module is most of a minute
#: on Windows).
_SIBLING_IMPORT = re.compile(r"\bfrom\s*\.\s*import\b")


def test_sibling_test_module_attribute_references_resolve():
    from helpers import source_scan

    bad: list[str] = []
    texts = source_scan.sources(TESTS_DIR.parent)
    for path, text in texts.items():
        if path.parent != TESTS_DIR or not path.name.startswith("test_"):
            continue
        if _SIBLING_IMPORT.search(text):
            bad += dangling_refs(path)
    assert not bad, "dangling cross-module helper references:\n" + "\n".join(bad)
