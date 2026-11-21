"""A repository script loaded as a FRESH module, its compiled code reused.

Many tier_1 suites test a script by loading it by path once per test —
``spec_from_file_location`` + ``exec_module`` — so that no test sees a global
another test changed. Loading the fleet core broker's script that way also loads
the three dozen scripts it loads by path itself, and most of the cost is reading and
unmarshalling their bytecode again: 236 ms a load on Windows, 59 ms with the code
reused (measured 2026-11-18 on a loaded box; ``build-slot.py`` alone 36 ms →
7 ms). A suite of 892 items pays it per item
(merge-gate-catalog.md § The heavy gate catalog, the 28th gate's *win arm*).

:func:`fresh_module` keeps the isolation and drops the cost: every call EXECUTES
the script (and every script it loads) into new module objects, so no test shares
a module, a global or a patched attribute with another; only the code objects —
immutable — are reused, keyed on the file's path and mtime, so an edit made under
a running process is compiled anew. The reuse is a scoped wrap of the source
loader's ``get_code``, installed for the duration of one load under a lock and
restored after it, so an import outside a load is untouched.
"""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import threading
from types import CodeType, ModuleType

_ORIGINAL_GET_CODE = importlib.machinery.SourceFileLoader.get_code
_CODE: dict[str, tuple[int, CodeType]] = {}
_LOCK = threading.RLock()


def _cached_get_code(self, fullname):
    path = self.get_filename(fullname)
    try:
        mtime = os.stat(path).st_mtime_ns
    except OSError:
        return _ORIGINAL_GET_CODE(self, fullname)
    hit = _CODE.get(path)
    if hit is None or hit[0] != mtime:
        hit = (mtime, _ORIGINAL_GET_CODE(self, fullname))
        _CODE[path] = hit
    return hit[1]


def fresh_module(path: str, name: str) -> ModuleType:
    """Execute the script at ``path`` as a new module named ``name`` — and every
    script it loads by path as new modules too — reusing only compiled code."""
    with _LOCK:
        before = importlib.machinery.SourceFileLoader.get_code
        importlib.machinery.SourceFileLoader.get_code = _cached_get_code
        try:
            spec = importlib.util.spec_from_file_location(name, path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            return module
        finally:
            importlib.machinery.SourceFileLoader.get_code = before
