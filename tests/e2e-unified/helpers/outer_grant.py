"""The environment a build-slot test hands its own child, with whatever an
OUTER grant exported taken back out.

A tooling test runs the build-slot.py under test (or a shim beside it) as a
child, and builds the child's environment from `os.environ`. When the pytest
itself runs under a grant — the tier_1 gate's `slot_tier_1` prefix, the
background tester inside the verify drain's grant, a session's
`build-slot.py --pool focus -- pytest …` — that environment already carries the
grant's exports (`build-slot.py::_cap_jobs`, `_kache_shim_env`,
`_jobs_shim_env`): this checkout's `cargo-shim` and `jobs-shim` on PATH, the
front variables, the held/index/cpuset/cores variables, and every tool's jobs
knob at the outer k. The child then reads itself as nested — a shim already
on PATH is left where it is, behind the test's own fake driver, and a caller's
lower jobs cap is kept — and the test's assertion fails for a reason that is
not its subject — on 2026-11-06 the swift
`-j k` case failed every time under a focus grant and passed bare. A test that WANTS an outer grant sets its
variables on top of this. Owner: `docs/goal/architecture/e2e-latency-independent-assertions.md`
(a test reads only state it owns).
"""

import os

import pytest

#: The shim directories a grant puts first on PATH (build-slot.py's
#: _CARGO_SHIM_DIR_NAME, _JOBS_SHIM_DIR_NAME), of any copy: one beside a
#: build-slot.py.
_SHIM_DIRS = ("cargo-shim", "jobs-shim")

#: Variables a grant sets that name the grant itself.
_GRANT_VARS = (
    "FAUNA_CARGO_FRONT",
    "FAUNA_CARGO_FRONT_PYTHON",
    "FAUNA_SLOT_CORES",
    "FAUNA_SLOT_CORES_RANGE",
)
_GRANT_PREFIXES = ("FAUNA_SLOT_HELD_", "FAUNA_SLOT_INDEX_", "FAUNA_SLOT_CPUSET_")

#: The jobs knobs _cap_jobs writes at the grant's k (its _JOBS_ENV_KNOBS and
#: _MSBUILD_DAEMONLESS); MAKEFLAGS and GRADLE_OPTS lose only what it added.
_JOBS_KNOBS = (
    "CARGO_BUILD_JOBS", "GOMAXPROCS", "BINARYEN_CORES", "RUST_TEST_THREADS",
    "MSBUILDDISABLENODEREUSE", "UseSharedCompilation", "DOTNET_CLI_USE_MSBUILD_SERVER",
)


def is_shim_dir(entry: str) -> bool:
    norm = os.path.normpath(entry) if entry else ""
    return bool(norm) and os.path.basename(norm) in _SHIM_DIRS and os.path.isfile(
        os.path.join(os.path.dirname(norm), "build-slot.py"))


def without_outer_grant(environ=None) -> dict:
    """A copy of `environ` (default os.environ) as a shell outside every grant
    would have it."""
    env = dict(os.environ if environ is None else environ)
    for var in list(env):
        if var in _GRANT_VARS or var in _JOBS_KNOBS or var.startswith(_GRANT_PREFIXES):
            del env[var]
    if "PATH" in env:
        env["PATH"] = os.pathsep.join(
            e for e in env["PATH"].split(os.pathsep) if e and not is_shim_dir(e))
    make = [f for f in env.get("MAKEFLAGS", "").split() if not f.startswith(("-j", "--jobs"))]
    if make:
        env["MAKEFLAGS"] = " ".join(make)
    else:
        env.pop("MAKEFLAGS", None)
    gradle = [o for o in env.get("GRADLE_OPTS", "").split()
              if not o.startswith(("-Dorg.gradle.workers.max=", "-Dorg.gradle.daemon="))
              and "kotlin.compiler.execution.strategy=" not in o]
    if gradle:
        env["GRADLE_OPTS"] = " ".join(gradle)
    else:
        env.pop("GRADLE_OPTS", None)
    return env


@pytest.fixture(autouse=True)
def no_outer_grant(monkeypatch):
    """Autouse in a module that imports it: this test's os.environ as
    without_outer_grant has it, restored at teardown — every child it spawns
    from os.environ, and every in-process call that reads it, sees no outer
    grant."""
    clean = without_outer_grant()
    for var in list(os.environ):
        if var not in clean:
            monkeypatch.delenv(var)
    for var, value in clean.items():
        if os.environ.get(var) != value:
            monkeypatch.setenv(var, value)
