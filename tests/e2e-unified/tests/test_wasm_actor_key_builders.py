"""Drift guard: every actor-keyed `localStorage` key web's wasm modules build is
in `account_scope::ACTOR_KEY_BUILDERS`, the one list a remove-account and a
sign-out erase.

Owner: `docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy →
*Erasure follows scope* (web's account-scope erase). The dispositions record
five apps whose hand-listed drops drifted from the keys they meant to drop; a
key a module mints without joining the list survives the account it belongs to.

`fauna-wasm` does not compile natively, so the guard is textual: in each source
file that writes `localStorage`, every `format!("fauna…{…}")` key builder must sit
in a function the list names. `sessionStorage` keys (succession's) are per-tab
and are deliberately not swept, so a file that never touches `local_storage()`
is out of scope.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SRC = _REPO / "libs/fauna-wasm/src"
_SCOPE = _SRC / "account_scope.rs"

_KEY_FORMAT = re.compile(r'format!\(\s*"fauna[^"]*\{')
_FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)")


def _listed_builders() -> set[str]:
    text = _SCOPE.read_text(encoding="utf-8")
    m = re.search(r"ACTOR_KEY_BUILDERS:[^=]*=\s*&\[(.*?)\];", text, re.S)
    assert m, f"no ACTOR_KEY_BUILDERS list in {_SCOPE}"
    return {p.strip().removeprefix("crate::") for p in m.group(1).split(",") if p.strip()}


def _local_storage_key_builders() -> set[str]:
    """`<module>::<fn>` for each actor-keyed `fauna…` key built in a file that
    writes `localStorage`; test modules and comments excluded."""
    found = set()
    for path in sorted(_SRC.rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        if "local_storage()" not in text:
            continue
        module = path.relative_to(_SRC).with_suffix("").as_posix().replace("/", "::")
        current_fn = None
        for line in text.splitlines():
            stripped = line.lstrip()
            if stripped.startswith("//"):
                continue
            if stripped.startswith("mod tests") or "#[cfg(test)]" in stripped:
                break
            if fn := _FN.match(line):
                current_fn = fn.group(1)
            if _KEY_FORMAT.search(line):
                found.add(f"{module}::{current_fn}")
    return found


def test_scanner_sees_the_backup_audit_builder():
    """The scan is not vacuous: the one builder known today is found."""
    assert "backup_audit::storage_key" in _local_storage_key_builders()


def test_every_actor_keyed_local_storage_builder_is_erased():
    missing = _local_storage_key_builders() - _listed_builders()
    assert not missing, (
        f"actor-keyed localStorage builder(s) {sorted(missing)} are not in "
        f"`account_scope::ACTOR_KEY_BUILDERS` ({_SCOPE.relative_to(_REPO)}): a "
        "remove-account or sign-out would leave the key behind. Add the builder "
        "to the list (or, for install-scoped state, do not key it by actor)."
    )
