"""The e2e tree's own Python sources, read once per process, and the subset a
source-scanning pin must parse.

Many tier_1 pins read the e2e tree itself — ``conftest.py``, ``helpers/``, every
module under ``tests/`` — and walk it with ``ast``: 1,876 files and 33 MB of
source on 2026-11-18. One read-and-parse of all of it is about a minute of wall
on Windows (measured 2026-11-18 on a loaded box, normal class: 1.0 s to read,
43 s to parse, 15 s to walk every node), so a whole-directory run that repeats it
pin after pin spends a large share of its length there
(merge-gate-catalog.md § The heavy gate catalog, the 28th gate's *win arm*).
Two levers, and neither changes an answer:

* the tree's text is read ONCE per process and cached on its root
  (:func:`sources`) — a pin that used to walk and read the tree again reads the
  cached mapping;
* a pin parses only the files whose TEXT spells one of the names its question
  keys on (:func:`spelling`). A function can only call, define, decorate with or
  import a name its file spells, so a file spelling none of them adds nothing to
  the answer, and skipping its parse is exact. It is the share that matters: the
  files that spell a given helper's name are usually a few dozen of the 1,876.

Lifted from ``tests/test_nest_mode_axis.py``'s ``_walkable_sources`` and
``_names_any`` (2026-11-09), which measured the same cost — 30–80 s per whole-tree
parse on a loaded Windows host — for the start-option derivation alone.

The cached text is the tree as it was when first read: a run does not see an edit
made under it, which is what a pinned gate tree wants and what a hand run with
the tree held still gets anyway. Parsed trees are not cached, with ONE exception:
:func:`parsed`, for the few files many pins parse whole on their own — the root
``conftest.py`` (1.17 MB, 2.5 s a parse on a loaded Windows host) is parsed alone by
about thirty pins.
"""

from __future__ import annotations

import ast
import fnmatch
import functools
import os
import re
import types
from pathlib import Path
from typing import Iterable, Mapping

#: The e2e tree's root (``tests/e2e-unified``).
E2E_ROOT = Path(__file__).resolve().parents[1]

#: Directories no walk below descends into: never test sources, and a
#: ``node_modules`` or ``.venv`` can hold more files than the tree itself.
UNWALKED_DIRS = frozenset({".venv", "__pycache__", "node_modules"})


def sources(root: Path = E2E_ROOT) -> Mapping[Path, str]:
    """``{path: text}`` for every ``.py`` file under ``root``, in path order,
    read-only and cached on ``root`` for the life of the process — one entry
    per root however the call spells it (a bare ``sources()`` and
    ``sources(E2E_ROOT)`` share it).

    The skipped directories are pruned as the walk meets them, not filtered
    after an ``rglob`` has listed everything under them. A file that cannot be
    read (gone between the listing and the read) is left out; one that is not
    UTF-8 raises, because a pin must not silently lose a file."""
    return _sources(Path(root))


@functools.cache
def _sources(root: Path) -> Mapping[Path, str]:
    paths = []
    for here, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in UNWALKED_DIRS]
        paths.extend(Path(here, f) for f in files if f.endswith(".py"))
    texts = {}
    for path in sorted(paths):
        try:
            texts[path] = path.read_text(encoding="utf-8")
        except OSError:
            continue
    return types.MappingProxyType(texts)


def parsed(path: Path) -> ast.Module:
    """The parsed tree of one file, SHARED by every caller for the life of the
    process (the sixteen most recent files): for a file many pins parse whole on
    their own, like the root ``conftest.py``. The tree is read-only by contract —
    no caller may change a node or set an attribute on one, since the next
    caller gets the same object; a pin that annotates nodes parses its own copy
    with ``ast.parse``. Read as UTF-8 off the disk at first call."""
    return _parsed(Path(path).resolve())


@functools.lru_cache(maxsize=16)
def _parsed(path: Path) -> ast.Module:
    return ast.parse(path.read_text(encoding="utf-8"), filename=str(path))


@functools.cache
def _name_pattern(names: frozenset) -> re.Pattern:
    return re.compile("|".join(re.escape(n) for n in sorted(names)))


def spells_any(source: str, names: Iterable[str]) -> bool:
    """Whether ``source``'s text holds any of ``names`` verbatim — the test a
    file must pass before a pin parses it."""
    names = frozenset(names)
    if not names:
        raise ValueError("spells_any needs at least one name: an empty set matches no file")
    return _name_pattern(names).search(source) is not None


def spelling(
    names: Iterable[str],
    *,
    root: Path = E2E_ROOT,
    within: Iterable[str] = (),
    pattern: str = "*.py",
    requiring: Iterable[str] = (),
) -> list[tuple[Path, str]]:
    """``[(path, text)]``, in path order, of every source under ``root`` that a
    pin keyed on ``names`` must parse: its file name matches ``pattern`` (a
    glob on the name alone — ``"test_*.py"``, ``"conftest.py"``), it lies under
    one of ``within`` (paths relative to ``root``, each a directory or a file;
    empty = the whole tree), its text spells at least one of ``names``, and it
    spells EVERY one of ``requiring`` (for a match that needs two things at
    once — a fixture that calls a builder: ``requiring={"fixture"}``).

    The answer over the files returned equals the answer over every file the
    pin used to read, as long as every AST match the pin counts needs one of
    ``names`` (and all of ``requiring``) spelled in the file — a call,
    definition, decorator, attribute, argument or import of that name. A string
    constant is found by its literal spelling, so one split by implicit
    concatenation (``"fauna-" "nest"``) or written with escapes is not; a pin
    keyed on a constant says so where it calls this. A pin asking about an
    ABSENCE across every file (each module has a marker, no file lacks a
    header) cannot use this; it reads :func:`sources` whole."""
    names = frozenset(names)
    requiring = tuple(requiring)
    prefixes = [root / rel for rel in within]
    out = []
    for path, text in sources(root).items():
        if not fnmatch.fnmatchcase(path.name, pattern):
            continue
        if prefixes and not any(path.is_relative_to(p) for p in prefixes):
            continue
        if not spells_any(text, names):
            continue
        if all(word in text for word in requiring):
            out.append((path, text))
    return out
