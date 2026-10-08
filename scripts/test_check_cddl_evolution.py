"""Tests for scripts/check-cddl-evolution.py — the CDDL schema-evolution gate.

Run with: pytest scripts/test_check_cddl_evolution.py -v

Two layers:
  * Pure-function unit tests on `check_schema_pair(path, base, head)` — the gate's
    core diff logic — proving it CATCHES every blocked change and ALLOWS every
    permitted one. No git, no filesystem.
  * One end-to-end integration test that drives the script's `main()` against a
    real temp git repo with an `origin/main` merge-base, exercising the
    git-show / merge-base path the unit tests skip.

The blocked/allowed contract is `libs/fauna-protocol/schemas/README.md`
§ Schema-evolution gate and `docs/goal/architecture/transport.md` § Schema and
forward-compat discipline.
"""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest


def _load_gate():
    """Import the hyphenated script as a module."""
    path = Path(__file__).resolve().parent / "check-cddl-evolution.py"
    spec = importlib.util.spec_from_file_location("check_cddl_evolution", path)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gate = _load_gate()


# A small valid base schema reused across cases. Two fields, one optional.
BASE = textwrap.dedent(
    """\
    Foo = {
      id: tstr,
      ? note: tstr,
      * tstr => any,
    }
    """
)


def _errs(base: str, head: str) -> list[str]:
    return gate.check_schema_pair("foo.cddl", base, head)


# ── Blocked changes (each MUST produce an error) ────────────────────────────


def test_removed_field_is_blocked():
    head = textwrap.dedent(
        """\
        Foo = {
          id: tstr,
          * tstr => any,
        }
        """
    )
    errs = _errs(BASE, head)
    assert any("removed field `note`" in e for e in errs), errs


def test_removed_type_is_blocked():
    base = BASE + "\nBar = {\n  x: uint,\n}\n"
    errs = _errs(base, BASE)  # head dropped Bar entirely
    assert any("removed type definition `Bar`" in e for e in errs), errs


def test_optional_to_required_is_blocked():
    head = textwrap.dedent(
        """\
        Foo = {
          id: tstr,
          note: tstr,
          * tstr => any,
        }
        """
    )
    errs = _errs(BASE, head)
    assert any("`note` was optional, now required" in e for e in errs), errs


def test_type_change_is_blocked():
    head = textwrap.dedent(
        """\
        Foo = {
          id: uint,
          ? note: tstr,
          * tstr => any,
        }
        """
    )
    errs = _errs(BASE, head)
    assert any("`id` type changed" in e for e in errs), errs


def test_rename_is_blocked_as_removal():
    # Renaming a key reads as remove-old + add-new; the removal is the violation.
    head = textwrap.dedent(
        """\
        Foo = {
          identifier: tstr,
          ? note: tstr,
          * tstr => any,
        }
        """
    )
    errs = _errs(BASE, head)
    assert any("removed field `id`" in e for e in errs), errs


# ── Allowed changes (each MUST be clean) ────────────────────────────────────


def test_new_optional_field_is_allowed():
    head = textwrap.dedent(
        """\
        Foo = {
          id: tstr,
          ? note: tstr,
          ? added: uint,
          * tstr => any,
        }
        """
    )
    assert _errs(BASE, head) == []


def test_new_type_is_allowed():
    head = BASE + "\nBaz = {\n  y: bool,\n}\n"
    assert _errs(BASE, head) == []


def test_new_file_is_allowed():
    # base is None → brand-new schema file, nothing to diff against.
    assert gate.check_schema_pair("new.cddl", None, BASE) == []


def test_unchanged_is_clean():
    assert _errs(BASE, BASE) == []


# ── End-to-end integration through the real git merge-base path ─────────────


def _git(repo: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )


# ── Ratified in-place breaks (libs/fauna-protocol/schemas/ratified-breaks.txt) ──


REMOVED = "removed"
TIGHTENED = "optional→required"


def test_parse_ratified_breaks_takes_only_cddl_keys_with_their_transition():
    text = textwrap.dedent(
        """\
        # comment
        rust auth::CertBinding.sig   removed 2026-09-24 ratified elsewhere
        cddl EventRsvpPayload        removed 2026-09-24 dead kind  # trailing comment
        cddl Foo.note                optional→required 2026-09-24 tightened

        """
    )
    assert gate.parse_ratified_breaks(text) == frozenset(
        {("EventRsvpPayload", REMOVED), ("Foo.note", TIGHTENED)}
    )


@pytest.mark.parametrize(
    "line",
    [
        "cddl Foo.note 2026-09-24 the pre-transition grammar",  # no transition
        "cddl Foo.note retyped 2026-09-24 a retype names its target type",
        "cddl Foo.note retyped→ 2026-09-24 an empty target type",
        "cddl Foo retyped→uint 2026-09-24 a type is only ever removed",
        "cddl Foo optional→required 2026-09-24 a type is only ever removed",
        "cddl Foo.note removed",  # no ratified-on date
        "cddl",
        # Exactly 4 tokens — gate, key, transition, ratified-on, but no
        # ratification text — used to pass the old `len(parts) < 4` check.
        # The grammar's fifth field is not optional.
        "cddl Foo.note removed 2026-09-24",
    ],
)
def test_parse_ratified_breaks_refuses_a_malformed_entry(line: str):
    with pytest.raises(ValueError):
        gate.parse_ratified_breaks(line + "\n")


@pytest.mark.parametrize(
    "line",
    [
        "cdl Foo.note removed 2026-09-24 typo'd gate",
        "rsut Foo.note removed 2026-09-24 typo'd gate",
        "CDDL Foo.note removed 2026-09-24 wrong case",
    ],
)
def test_parse_ratified_breaks_refuses_an_unknown_gate_token(line: str):
    # An unrecognized gate token used to be silently skipped by BOTH parsers
    # (`parts[0] != "cddl"` treated `cdl`/`rsut` exactly like a `rust` line —
    # ignored, not validated), which meant a mistyped gate token on a
    # `removed` entry silently switched off that key's revival refusal. It
    # is now a parse error, same as any other malformed line.
    with pytest.raises(ValueError):
        gate.parse_ratified_breaks(line + "\n")


def test_parse_ratified_breaks_skips_rust_lines_without_validating_them():
    # A `rust` line malformed by the struct gate's OWN grammar (missing
    # ratification) is still skipped here — it belongs to that gate's
    # parser, which validates it instead (mirrored in `tools/check-additive-evolution`).
    assert gate.parse_ratified_breaks("rust auth::CertBinding.sig removed 2026-09-24\n") == frozenset()


def test_ratified_field_break_is_allowed_but_nothing_else():
    head = textwrap.dedent(
        """\
        Foo = {
          id: tstr,
          * tstr => any,
        }
        """
    )
    # Unlisted: still blocked.
    assert gate.check_schema_pair("foo.cddl", BASE, head)
    # Listed field removal: excused.
    assert gate.check_schema_pair("foo.cddl", BASE, head, frozenset({("Foo.note", REMOVED)})) == []
    # A listing for another field excuses nothing.
    errs = gate.check_schema_pair("foo.cddl", BASE, head, frozenset({("Foo.id", REMOVED)}))
    assert any("removed field `note`" in e for e in errs)


def test_optional_to_required_entry_excuses_only_the_tightening():
    tightened = BASE.replace("? note: tstr", "note: tstr")
    allow = frozenset({("Foo.note", TIGHTENED)})
    assert gate.check_schema_pair("foo.cddl", BASE, tightened, allow) == []
    # The same entry does NOT excuse a later removal of the field …
    removed = BASE.replace("  ? note: tstr,\n", "")
    errs = gate.check_schema_pair("foo.cddl", BASE, removed, allow)
    assert any("removed field `note`" in e for e in errs), errs
    # … nor a retype.
    retyped = BASE.replace("? note: tstr", "? note: uint")
    errs = gate.check_schema_pair("foo.cddl", BASE, retyped, allow)
    assert any("type changed" in e for e in errs), errs


def test_retyped_entry_excuses_only_the_retype_to_its_named_type():
    allow = frozenset({("Foo.note", "retyped→uint")})
    assert gate.parse_ratified_breaks(
        "cddl Foo.note retyped→uint 2026-09-24 ratified\n"
    ) == allow
    to_uint = BASE.replace("? note: tstr", "? note: uint")
    assert gate.check_schema_pair("foo.cddl", BASE, to_uint, allow) == []
    to_bstr = BASE.replace("? note: tstr", "? note: bstr")
    errs = gate.check_schema_pair("foo.cddl", BASE, to_bstr, allow)
    assert any("type changed" in e for e in errs), errs


def test_ratified_type_removal_is_allowed():
    head = "Bar = { x: tstr }\n"
    base = BASE + "\nBar = { x: tstr }\n"
    errs = gate.check_schema_pair("foo.cddl", base, head)
    assert any("removed type definition `Foo`" in e for e in errs)
    assert gate.check_schema_pair("foo.cddl", base, head, frozenset({("Foo", REMOVED)})) == []


def test_type_removal_entry_excuses_no_field_edit_of_a_same_named_type():
    # A `Type removed` entry excuses the type's removal and nothing else: a
    # same-named type still defined — here, or in another schema file — gets
    # no field excuse from it.
    removed = BASE.replace("  ? note: tstr,\n", "")
    errs = gate.check_schema_pair("other.cddl", BASE, removed, frozenset({("Foo", REMOVED)}))
    assert any("removed field `note`" in e for e in errs), errs


def test_a_ratified_removed_name_never_comes_back():
    # The list's permanence premise is enforced: a head defining a type, or a
    # field, the list records as removed is refused — in any schema file,
    # new files included.
    errs = gate.check_schema_pair("new.cddl", None, BASE, frozenset({("Foo", REMOVED)}))
    assert any("revives" in e and "`Foo`" in e for e in errs), errs
    errs = gate.check_schema_pair("foo.cddl", BASE, BASE, frozenset({("Foo.note", REMOVED)}))
    assert any("revives" in e and "`note`" in e for e in errs), errs


def _run_gate(repo: Path) -> subprocess.CompletedProcess:
    script = Path(__file__).resolve().parent / "check-cddl-evolution.py"
    # The gate resolves SCHEMAS_DIR relative to cwd and diffs HEAD vs
    # origin/main, so run it with the temp repo as cwd.
    return subprocess.run(
        [sys.executable, str(script)],
        cwd=str(repo),
        capture_output=True,
        text=True,
    )


@pytest.fixture()
def schema_repo(tmp_path: Path) -> Path:
    """A git repo whose origin/main has one schema; HEAD is a fresh branch."""
    repo = tmp_path / "repo"
    schemas = repo / gate.SCHEMAS_DIR
    schemas.mkdir(parents=True)
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "t@t")
    _git(repo, "config", "user.name", "t")
    (schemas / "foo.cddl").write_text(BASE)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "base")
    # The gate diffs against origin/main; a local branch of that name is a
    # valid merge-base target for `git merge-base HEAD origin/main`.
    _git(repo, "branch", "origin/main")
    return repo


def test_integration_clean_passes(schema_repo: Path):
    result = _run_gate(schema_repo)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "passed" in result.stdout


def test_integration_breaking_change_fails(schema_repo: Path):
    # Remove the optional field on HEAD only — a blocked change.
    (schema_repo / gate.SCHEMAS_DIR / "foo.cddl").write_text(
        textwrap.dedent(
            """\
            Foo = {
              id: tstr,
              * tstr => any,
            }
            """
        )
    )
    _git(schema_repo, "commit", "-aqm", "drop note")
    result = _run_gate(schema_repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "removed field `note`" in result.stdout
    assert "ratified-breaks.txt" in result.stdout


def test_integration_ratified_break_passes(schema_repo: Path):
    (schema_repo / gate.SCHEMAS_DIR / "foo.cddl").write_text(
        textwrap.dedent(
            """\
            Foo = {
              id: tstr,
              * tstr => any,
            }
            """
        )
    )
    (schema_repo / gate.RATIFIED_BREAKS_PATH).write_text(
        "cddl Foo.note removed 2026-09-24 ratified in the test\n"
    )
    _git(schema_repo, "add", "-A")
    _git(schema_repo, "commit", "-qm", "drop note, ratified")
    result = _run_gate(schema_repo)
    assert result.returncode == 0, result.stdout + result.stderr


def test_integration_malformed_allowlist_fails_loudly(schema_repo: Path):
    # A pre-transition entry (no transition token) is a parse error, never a
    # silently ignored — or silently honoured — line.
    (schema_repo / gate.RATIFIED_BREAKS_PATH).write_text(
        "cddl Foo.note 2026-09-24 the pre-transition grammar\n"
    )
    _git(schema_repo, "add", "-A")
    _git(schema_repo, "commit", "-qm", "malformed allowlist")
    result = _run_gate(schema_repo)
    assert result.returncode == 2, result.stdout + result.stderr
    assert "ratified-breaks.txt" in result.stderr


# ── The allowlist's own monotonic invariant ──────────────────────────────────
#
# "The list only grows" used to be enforced only as a side effect of the
# revival check above: deleting an entry and never reviving the name slipped
# past the gate entirely, since it read the allowlist from HEAD only. These
# drive the real `main()` path against a base that already carries a ratified
# `cddl Foo removed` entry.


@pytest.fixture()
def ratified_repo(tmp_path: Path) -> Path:
    """A git repo whose origin/main already carries a ratified `Foo removed`."""
    repo = tmp_path / "repo"
    schemas = repo / gate.SCHEMAS_DIR
    schemas.mkdir(parents=True)
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "t@t")
    _git(repo, "config", "user.name", "t")
    (schemas / "foo.cddl").write_text("Bar = {\n  x: tstr,\n}\n")
    (repo / gate.RATIFIED_BREAKS_PATH).write_text(
        "cddl Foo removed 2026-09-24 ratified in the test\n"
    )
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "base")
    _git(repo, "branch", "origin/main")
    return repo


def test_integration_dropped_ratified_entry_fails_even_without_revival(ratified_repo: Path):
    (ratified_repo / gate.RATIFIED_BREAKS_PATH).write_text("")
    _git(ratified_repo, "commit", "-aqm", "drop the ratified entry")
    result = _run_gate(ratified_repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "missing from HEAD" in result.stdout, result.stdout
    assert "only grows" in result.stdout, result.stdout


def test_integration_delete_and_revive_together_still_fails(ratified_repo: Path):
    # The named attack: delete the ratified entry AND revive the type it
    # excused, in the same change. The revival check alone reads only HEAD's
    # (already-shrunk) list, so it would find nothing — the monotonic check
    # catches the deletion regardless of the revival.
    (ratified_repo / gate.RATIFIED_BREAKS_PATH).write_text("")
    (ratified_repo / gate.SCHEMAS_DIR / "foo.cddl").write_text(
        "Bar = {\n  x: tstr,\n}\nFoo = {\n  y: tstr,\n}\n"
    )
    _git(ratified_repo, "commit", "-aqm", "delete ratified entry and revive Foo")
    result = _run_gate(ratified_repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "missing from HEAD" in result.stdout, result.stdout
    # Documents the gap this closes: the revival check alone (reading HEAD's
    # already-shrunk list) never fires here.
    assert "revives" not in result.stdout, result.stdout


def test_integration_growing_the_list_is_clean(ratified_repo: Path):
    (ratified_repo / gate.RATIFIED_BREAKS_PATH).write_text(
        "cddl Foo removed 2026-09-24 ratified in the test\n"
        "cddl Bar.x optional→required 2026-09-24 a second ratification\n"
    )
    _git(ratified_repo, "commit", "-aqm", "grow the list")
    result = _run_gate(ratified_repo)
    assert result.returncode == 0, result.stdout + result.stderr


# ── The 0.1.x compat-free window (version-compatibility.md § Dimension 2,
# the fifth ratified exception) ─────────────────────────────────────────


def test_product_version_reads_the_workspace_package_section_only():
    toml = (
        '[package]\nversion = "9.9.9"\n\n'
        '[workspace.package]\nedition = "2024"\nversion = "0.1.3"\n\n'
        '[workspace.dependencies]\nversion = "7"\n'
    )
    assert gate.product_version(toml) == "0.1.3"
    assert gate.product_version('[package]\nversion = "1.0.0"\n') is None


def test_the_compat_free_window_is_exactly_0_1_x():
    for v in ("0.1.0", "0.1.3", "0.1.99"):
        assert gate.in_compat_free_window(v), v
    for v in ("0.2.0", "0.10.0", "1.1.0", "0.0.9", "1.0.0"):
        assert not gate.in_compat_free_window(v), v


def _remove_bar_x(repo: Path, version: str) -> subprocess.CompletedProcess:
    (repo / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{version}"\n')
    (repo / gate.SCHEMAS_DIR / "foo.cddl").write_text("Bar = {\n}\n")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "remove Bar.x")
    return _run_gate(repo)


def test_integration_a_break_inside_0_1_x_is_reported_not_refused(ratified_repo: Path):
    result = _remove_bar_x(ratified_repo, "0.1.3")
    assert result.returncode == 0, result.stdout + result.stderr
    assert "compat-free window" in result.stdout, result.stdout
    assert "note:" in result.stdout, result.stdout


def test_integration_the_same_break_at_0_2_0_is_refused(ratified_repo: Path):
    result = _remove_bar_x(ratified_repo, "0.2.0")
    assert result.returncode == 1, result.stdout + result.stderr


def test_integration_the_list_still_only_grows_inside_the_window(ratified_repo: Path):
    (ratified_repo / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.3"\n')
    (ratified_repo / gate.RATIFIED_BREAKS_PATH).write_text("")
    _git(ratified_repo, "add", "-A")
    _git(ratified_repo, "commit", "-qm", "drop the ratified entry inside the window")
    result = _run_gate(ratified_repo)
    assert result.returncode == 1, result.stdout + result.stderr
    assert "missing from HEAD" in result.stdout, result.stdout
