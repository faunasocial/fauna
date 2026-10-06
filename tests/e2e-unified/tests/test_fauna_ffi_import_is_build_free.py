"""tier_1: importing `fauna_ffi` must never need a built cdylib.

The class: `tests/e2e-unified/fauna_ffi.py` loaded its native
library with a bare module-level `ctypes.CDLL(str(_find_cdylib()))`, and
`_find_cdylib` raises when the cdylib is absent — unconditionally on Windows,
where the build needs `just windows-ffi` and is deliberately not attempted.
Ten shipped test modules import `fauna_ffi` at module level, so on any tree
that had not already built fauna-ffi that raise turned `pytest
tests/e2e-unified/tests/` into `Interrupted: 10 errors during collection`,
exit 2, ZERO of ~3100 tests run — the whole suite dark behind one missing
build artifact. A fresh clone of the public repository is exactly such a tree,
which is how `suite_collect_check` found it.

The fix is the file's own per-symbol deferral pattern, one level up: where
that one turns a *stale* library into one actionable error in the one test
needing the missing symbol, this turns an *absent* library into one actionable
error in each test that actually calls through it. Collection stops depending
on build state; the diagnostic the developer needs is unchanged, only later.

What this pins:

  * `import fauna_ffi` SUCCEEDS with no cdylib anywhere the loader looks.
  * Calling a builder then raises, and the error still names the library, so
    the "run `just windows-ffi`" diagnostic is not swallowed into a silent
    no-op.
"""

import os
import shutil
import subprocess
import sys
import textwrap

import pytest

pytestmark = pytest.mark.tier_1

_HERE = os.path.dirname(__file__)
_E2E = os.path.normpath(os.path.join(_HERE, ".."))
_MODULE = os.path.join(_E2E, "fauna_ffi.py")


def _fake_checkout(tmp_path):
    """A directory `_find_repo_root` accepts, carrying no build output.

    The two markers it walks for are a `Cargo.toml` and a `libs/fauna-ffi`
    directory. The manifest is deliberately not a valid one: on the platforms
    whose loader auto-builds, cargo must fail immediately rather than start a
    real workspace build inside a throwaway tree.
    """
    root = tmp_path / "checkout"
    (root / "libs" / "fauna-ffi").mkdir(parents=True)
    (root / "Cargo.toml").write_text("this is not a manifest\n", encoding="utf-8")
    shutil.copy(_MODULE, root / "fauna_ffi.py")
    return root


def _run(root, body, tmp_path, extra_env=None):
    """Run `body` with `root` as both cwd and the only import path.

    `CARGO_TARGET_DIR` points at an empty directory so the loader's first
    candidate root is real but barren, and its fallback `<root>/target` does
    not exist at all.
    """
    empty_target = tmp_path / "empty-target"
    empty_target.mkdir()
    env = dict(os.environ)
    env.pop("FAUNA_E2E_FFI_NO_BUILD", None)
    env["CARGO_TARGET_DIR"] = str(empty_target)
    env["PYTHONPATH"] = str(root)
    env.update(extra_env or {})
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(body)],
        cwd=str(root), env=env, capture_output=True, text=True, timeout=300,
    )


def test_import_succeeds_with_no_cdylib(tmp_path):
    """The import itself is build-free — the property collection depends on."""
    root = _fake_checkout(tmp_path)
    proc = _run(root, """
        import fauna_ffi
        print("IMPORTED", fauna_ffi.build_post is not None)
    """, tmp_path)
    assert proc.returncode == 0, (
        "importing fauna_ffi without a built cdylib must not raise — it is a "
        "module-level import in ten shipped test modules, so a raise here "
        f"takes the whole suite's collection down.\nstdout:\n{proc.stdout}\n"
        f"stderr:\n{proc.stderr}"
    )
    assert "IMPORTED True" in proc.stdout, proc.stdout


_SPY_BUILD_RECIPE = """
    import subprocess
    calls = []
    def _spy(cmd, *args, **kwargs):
        calls.append(list(cmd))
        raise FileNotFoundError("build recipe spy: no build runs here")
    subprocess.run = _spy
    import fauna_ffi
    print("BUILDS", len(calls))
    try:
        fauna_ffi.build_post(b"\\x00" * 32, "hello")
    except Exception as exc:
        print("RAISED", str(exc)[:400])
"""


def test_the_no_build_opt_out_skips_the_import_time_build(tmp_path):
    """`FAUNA_E2E_FFI_NO_BUILD` is how a caller that only COLLECTS (the publish
    gate's `suite_collect_check`, in a fresh scratch copy where every build is
    cold) keeps the import from running a cdylib build it never uses — measured
    at 935 s on Windows, past the gate's 900 s collect ceiling. The import still
    succeeds and the deferred error still names the library at first use."""
    root = _fake_checkout(tmp_path)
    proc = _run(root, _SPY_BUILD_RECIPE, tmp_path, extra_env={"FAUNA_E2E_FFI_NO_BUILD": "1"})
    assert proc.returncode == 0, f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    assert "BUILDS 0" in proc.stdout, (
        f"the opt-out must run no build recipe at import: {proc.stdout!r}"
    )
    assert "RAISED" in proc.stdout and "fauna_ffi" in proc.stdout, proc.stdout


def test_without_the_opt_out_the_import_still_builds(tmp_path):
    """The opt-out is opt-IN: every ordinary run keeps the build-at-import that
    the stale-dll and mapped-dll fixes depend on."""
    root = _fake_checkout(tmp_path)
    proc = _run(root, _SPY_BUILD_RECIPE, tmp_path)
    assert proc.returncode == 0, f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    assert "BUILDS 0" not in proc.stdout, (
        f"an unset opt-out must still attempt the build: {proc.stdout!r}"
    )


def test_calling_a_builder_without_a_cdylib_raises_and_names_the_library(tmp_path):
    """Deferred, not swallowed: the diagnostic survives to first use."""
    root = _fake_checkout(tmp_path)
    proc = _run(root, """
        import fauna_ffi
        try:
            fauna_ffi.build_post(b"\\x00" * 32, "hello")
        except Exception as exc:
            print("RAISED", type(exc).__name__, str(exc)[:400])
        else:
            print("NO-RAISE")
    """, tmp_path)
    assert proc.returncode == 0, f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
    # The loader's own auto-build attempt legitimately prints before the
    # deferred error on every platform that attempts it, and the raised message
    # itself carries the failed recipe's multi-line output tail, so the outcome
    # is the `RAISED` line -- neither the first nor the last line of stdout.
    lines = proc.stdout.strip().splitlines()
    assert any(line.startswith("RAISED") for line in lines), (
        "a builder called against an absent cdylib must raise, not silently "
        f"return: {proc.stdout!r}"
    )
    assert "fauna_ffi" in proc.stdout, (
        "the deferred error must still name the library the developer has to "
        f"build: {proc.stdout!r}"
    )
