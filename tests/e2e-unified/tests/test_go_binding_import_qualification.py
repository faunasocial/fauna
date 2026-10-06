"""tier_1: `qualify-go-binding-imports.py` keeps the generated Go bindings compilable.

The class: `uniffi-bindgen-go` writes a
BARE `import "fauna_core"` when an export in one UniFFI namespace returns a type
from another. That is not a resolvable path in our single-module
`libs/fauna-mail-go` tree, so the package fails `go build` with `package
fauna_core is not in std`. Two packages — `fauna_conversations` and
`fauna_onboarding_machine` — carried it undetected for weeks, skipped by name
from `mail-bridge-ffi-check`'s compile step, harmless only because nothing
imported them.

The fix rewrites the import into a module-qualified path instead of gating the
offending export one at a time, which closes the class: the package *name* is
the last path element either way, so every reference still resolves.

What these pin, and why each is load-bearing:

  * The rewrite fires, is idempotent, and leaves std / already-qualified /
    single-declaration (`import "C"`) imports alone.
  * The touched block stays SORTED. gofmt sorts within a group by path and the
    generated tree is gofmt-clean today; a qualified path sorts differently
    from the bare name it replaces, so without the re-sort this script would
    itself make the tree gofmt-dirty.
  * A bare import the rewriter does NOT recognize is a hard error, not a silent
    pass-through. This is the "a generator upgrade cannot silently reintroduce
    it" property the NEXT block asked for: the failure lands at generation
    time, not in a later session's compile.
  * The module path matches the real `go.mod`, and both generator paths (the
    staging bindgen AND the drift check's comparand) run the step — a tree
    qualified on one path and compared against a raw-generated other would red
    every run.
"""

import importlib.util
import os
import re
import subprocess
import sys

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_REPO = os.path.normpath(os.path.join(_HERE, "..", "..", ".."))
_SCRIPT = os.path.join(_REPO, "scripts", "qualify-go-binding-imports.py")
_BINDINGS = os.path.join(_REPO, "libs", "fauna-mail-go")
_JUSTFILE = os.path.join(_REPO, "justfile")

_MODULE = "github.com/faunasocial/fauna/libs/fauna-mail-go"


def _load_script():
    spec = importlib.util.spec_from_file_location("qualify_go_binding_imports", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _run(*args, expect_rc=0):
    proc = subprocess.run(
        [sys.executable, _SCRIPT, *map(str, args)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode == expect_rc, (
        f"rc={proc.returncode} (wanted {expect_rc})\n"
        f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    )
    return proc


def _pkg(tmp_path, name, body):
    d = tmp_path / name
    d.mkdir(parents=True, exist_ok=True)
    f = d / f"{name}.go"
    f.write_text(body)
    return f


_CROSS_NAMESPACE = """package fauna_conversations

// #include <fauna_conversations.h>
import "C"

import (
\t"bytes"
\t"encoding/binary"
\t"fauna_client_moderation"
\t"fauna_core"
\t"fmt"
\t"unsafe"
)

// A doc comment mentioning a bare `import "fauna_core"` must not be rewritten.
func X() {}
"""


def test_bare_cross_namespace_import_is_qualified(tmp_path):
    f = _pkg(tmp_path, "fauna_conversations", _CROSS_NAMESPACE)

    proc = _run(tmp_path)

    text = f.read_text()
    assert f'"{_MODULE}/fauna_core"' in text
    assert f'"{_MODULE}/fauna_client_moderation"' in text
    assert "4 import(s) qualified" in proc.stdout or "2 import(s) qualified" in proc.stdout


def test_touched_block_is_re_sorted_gofmt_style(tmp_path):
    """gofmt sorts within a group by path; a qualified path lands elsewhere.

    Without the re-sort the generated tree stops being gofmt-clean — which is
    how a formatting gate would start reporting the generator as the offender.
    """
    f = _pkg(tmp_path, "fauna_conversations", _CROSS_NAMESPACE)

    _run(tmp_path)

    block = re.search(r"^import \(\n(.*?)^\)$", f.read_text(), re.S | re.M).group(1)
    paths = re.findall(r'"([^"]+)"', block)
    assert paths == sorted(paths), f"import block is not sorted: {paths}"
    assert paths == [
        "bytes",
        "encoding/binary",
        "fmt",
        f"{_MODULE}/fauna_client_moderation",
        f"{_MODULE}/fauna_core",
        "unsafe",
    ]


def test_comment_and_cgo_and_std_imports_are_untouched(tmp_path):
    f = _pkg(tmp_path, "fauna_conversations", _CROSS_NAMESPACE)

    _run(tmp_path)
    text = f.read_text()

    # `import "C"` is the cgo preamble anchor — rewriting or moving it breaks the build.
    assert '// #include <fauna_conversations.h>\nimport "C"\n' in text
    # A doc comment quoting the bare name is prose, not an import.
    assert 'A doc comment mentioning a bare `import "fauna_core"`' in text
    assert '\t"bytes"\n' in text and '\t"unsafe"\n' in text


def test_second_run_is_a_no_op(tmp_path):
    """Idempotence, and no needless write: an already-qualified path has a
    slash, so it can never match the bare pattern again."""
    f = _pkg(tmp_path, "fauna_conversations", _CROSS_NAMESPACE)
    _run(tmp_path)
    after_first = f.read_text()
    old = 1_000_000_000
    os.utime(f, (old, old))

    proc = _run(tmp_path)

    assert f.read_text() == after_first
    assert f.stat().st_mtime == old, "an unchanged file was rewritten"
    assert "0 import(s) qualified across 0 file(s)" in proc.stdout


def test_package_without_cross_namespace_imports_is_left_alone(tmp_path):
    """Eight of the ten generated packages are in this shape. Untouched means
    untouched down to the mtime: the staging tree is mirrored by
    `sync-generated-tree.py`, whose whole job is the generator/mtime contract."""
    f = _pkg(
        tmp_path,
        "fauna_core",
        'package fauna_core\n\nimport (\n\t"bytes"\n\t"fmt"\n)\n',
    )
    before = f.read_text()
    old = 1_000_000_000
    os.utime(f, (old, old))

    proc = _run(tmp_path)

    assert f.read_text() == before
    assert f.stat().st_mtime == old, "a file with nothing to qualify was rewritten"
    assert "0 import(s) qualified" in proc.stdout


def test_a_doc_comment_quoting_a_bare_name_is_not_an_import(tmp_path):
    """The real tree contains one: `fauna_ffi.go` carries a Rust doc comment
    that quotes `import "fauna_core"` while explaining this very footgun.

    Both the rewrite and the verification are scoped to import declarations for
    this reason — a whole-file scan would rewrite prose, or fail the recipe over
    a comment, on a tree that is perfectly correct.
    """
    f = _pkg(
        tmp_path,
        "fauna_ffi",
        'package fauna_ffi\n\nimport (\n\t"bytes"\n)\n\n'
        '// Returns bytes on purpose: an export returning a `fauna_core` type makes\n'
        '// uniffi-bindgen-go emit an uncompilable bare `import "fauna_core"`.\n'
        "func MailToLabelerInputBare() {}\n",
    )
    before = f.read_text()

    proc = _run(tmp_path)

    assert f.read_text() == before
    assert "0 import(s) qualified" in proc.stdout


def test_unrecognized_import_shape_fails_loudly(tmp_path):
    """The pin the tracked tree cannot express: an import the rewriter does not
    match must NOT pass through silently.

    A generator upgrade that emits a different spec shape has to fail here, at
    generation time, rather than land an uncompilable binding for whichever
    session next imports the package (exactly how the two skipped packages
    survived for weeks).
    """
    _pkg(
        tmp_path,
        "fauna_conversations",
        'package fauna_conversations\n\nimport ( "fauna_core" )\n',
    )

    proc = _run(tmp_path, expect_rc=1)

    assert "bare cross-namespace import(s) survived qualification" in proc.stderr
    assert 'bare import "fauna_core"' in proc.stderr
    assert "do NOT re-add a skip list" not in proc.stderr  # that wording lives in the justfile
    assert "teach scripts/qualify-go-binding-imports.py" in proc.stderr


def test_module_path_matches_the_real_go_mod():
    """The constant is a copy of the skeleton `go.mod` the `mail-bridge-ffi`
    recipe writes; a divergence would qualify imports to a module that does not
    exist, and every package would fail to resolve."""
    mod = _load_script()
    go_mod = open(os.path.join(_BINDINGS, "go.mod")).read()
    declared = re.search(r"^module\s+(\S+)$", go_mod, re.M).group(1)
    assert mod.MODULE_PATH == declared


def test_tracked_bindings_carry_no_bare_cross_namespace_import():
    """Parse-only assertion over the real tree — catches a hand-edit or a sync
    that bypassed the qualification step without waiting for the heavy gate."""
    mod = _load_script()
    offenders = []
    for root, _dirs, files in os.walk(_BINDINGS):
        for name in sorted(files):
            if not name.endswith(".go"):
                continue
            path = os.path.join(root, name)
            lines = open(path).read().splitlines()
            for start, end in mod.import_regions(lines):
                for idx in range(start, end):
                    for found in re.findall(r'"([^"]*)"', lines[idx]):
                        if mod._BARE.match(found):
                            offenders.append(f"{path}:{idx + 1}: {found}")
    assert not offenders, (
        "tracked Go bindings carry bare cross-namespace imports (they will not "
        "compile) — run `just mail-bridge-ffi`:\n  " + "\n  ".join(offenders)
    )


def test_both_generator_paths_qualify_the_tree():
    """A tree qualified on one path and compared against a raw-generated other
    reds every run, so both invocations must exist: the staging bindgen
    (`_mail-bridge-ffi-bindgen`) and the drift check's comparand
    (`mail-bridge-ffi-check`)."""
    text = open(_JUSTFILE).read()
    # Interpreter-agnostic on purpose: what matters is that the script runs on
    # both paths, not how the interpreter is spelled. It was pinned to a literal
    # `python3 ` prefix until 2026-08-23, when the justfile moved to the `{{py}}`
    # variable (always-uv, so every machine gets the same interpreter) and this
    # assertion went red over a change that altered nothing it is about.
    invocations = re.findall(r"scripts/qualify-go-binding-imports\.py", text)
    assert len(invocations) >= 2, (
        "qualify-go-binding-imports.py must run on BOTH the staging bindgen and "
        f"the drift check's comparand; found {len(invocations)} invocation(s)"
    )


def test_the_check_compiles_every_generated_package():
    """The compile scope must stay `./...`. A by-name skip list is exactly how
    `fauna_conversations` and `fauna_onboarding_machine` stayed broken for
    weeks — the packages nothing compiles are the ones that rot."""
    text = open(_JUSTFILE).read()
    assert "go -C libs/fauna-mail-go build ./..." in text, (
        "mail-bridge-ffi-check must compile EVERY generated binding package"
    )
