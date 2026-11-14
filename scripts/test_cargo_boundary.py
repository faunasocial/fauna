"""The cargo boundary's pure tests: `scripts/cargo-boundary.py`, the one home of
the boundary every compiling cargo runs behind (docs/goal/architecture/
build-compile-cache.md § The boundary's scope and what OFF means, decisions (1),
(5) and (6)).

pytest + stdlib, no machine read: the plan per subcommand, the `cargo fetch`
prelude's flags, the line's shape on Linux and macOS, the compiling set, the
runner reading by class and the refusal by name, the inside test, the boundary
line, the record a runner's witness reads, and the program's entry points. The
tests that need a machine's compile cache or its installed launcher live with
the dev fleet's front.
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import sysconfig
import tempfile
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve().parent
_HELPER = _HERE / "cargo-boundary.py"
_SHIM = _HERE / "cargo-shim" / "cargo"


def _load():
    spec = importlib.util.spec_from_file_location("cargo_boundary_under_test", _HELPER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cb = _load()

_BWRAP = ["/usr/bin/bwrap", "--unshare-user", "--unshare-ipc", "--unshare-net", "--unshare-uts", "--unshare-cgroup-try",
          "--die-with-parent", "--tmpfs", "/",
          "--dev-bind", "/dev", "/dev", "--proc", "/proc", "--tmpfs", "/run", "--tmpfs", "/tmp",
          "--tmpfs", "/dev/shm", "--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc",
          "--ro-bind", "/sys", "/sys", "--setenv", "TMPDIR", "/tmp", "--seccomp", "9",
          "--remount-ro", "/", "--"]
_UNSET = "unset GITHUB_ENV GITHUB_PATH GITHUB_OUTPUT GITHUB_STATE GITHUB_STEP_SUMMARY"
_TOKEN_SET = ["/bin/bash", "-c", 'export FAUNA_KACHE_INSIDE="$(readlink /proc/self/ns/net)"; exec "$@"',
              "kache-netns"]
#: the plan's own list, pinned: the compiling subcommands and their aliases
_COMPILING = ("build", "b", "check", "c", "clippy", "fix", "test", "t", "bench", "run", "r",
              "doc", "d", "rustc", "rustdoc")
_NOT_COMPILING = ("fetch", "install", "update", "add", "metadata", "vendor", "tree", "audit", "vet",
                  "deny", "generate-lockfile", "locate-project", "pkgid", "search", "publish", "fmt")
_HOSTED = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
_SELF_HOSTED = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "self-hosted"}


#: the darwin profile's writable roles and the path an unused one names, pinned
_MAC_ROLES = ("FAUNA_TARGET", "FAUNA_SCRATCH", "FAUNA_STORE", "FAUNA_KACHE_STATE",
              "FAUNA_KACHE_LOG", "FAUNA_WORKSPACE")
_UNUSED = "/dev/null/fauna-unused-role"


def _mac(profile: str, roles: dict | None = None) -> list[str]:
    """The darwin prefix: every role a `-D` parameter (an unused one `_UNUSED`),
    then inside the token, `TMPDIR` the scratch when there is one, and every role
    again as a variable — what the launcher's mac tail re-applies."""
    pairs = [(r, (roles or {}).get(r) or _UNUSED) for r in _MAC_ROLES]
    scratch = dict(pairs)["FAUNA_SCRATCH"]
    return ["/usr/bin/sandbox-exec", "-f", profile, *[w for r, p in pairs for w in ("-D", f"{r}={p}")],
            "/usr/bin/env", "FAUNA_KACHE_INSIDE=1", f"FAUNA_KACHE_PROFILE={profile}",
            *([f"TMPDIR={scratch}"] if scratch != _UNUSED else []),
            *[f"{r}={p}" for r, p in pairs]]


@pytest.fixture(autouse=True)
def _a_linux_machine_name_off_linux(monkeypatch):
    """The linux line's filter is chosen by `platform.machine()`, which names
    the arm64 build host `arm64` on macOS and `ARM64` on Windows, never Linux's
    `aarch64`: a non-Linux host builds the linux line only in these tests, so
    there it reads the Linux name of its own arch (on Linux the host's own
    answer stands, refusals included)."""
    if not sys.platform.startswith("linux"):
        name = {"arm64": "aarch64", "amd64": "x86_64"}.get(cb.platform.machine().lower(),
                                                             cb.platform.machine())
        monkeypatch.setattr(cb.platform, "machine", lambda: name)


@pytest.fixture
def outside(monkeypatch):
    """This process is not inside a boundary, whatever the test host is — and the
    writable set and the read list resolve to nothing and the host's `/bin`-style
    links are not read, so no `cargo metadata` runs and the linux line is its
    constant part alone (their own cases are below)."""
    monkeypatch.setattr(cb, "inside", lambda env: False)
    monkeypatch.setattr(cb, "writable_set", lambda cmd, env=None: [])
    monkeypatch.setattr(cb, "readable_set", lambda cmd, env=None: [])
    monkeypatch.setattr(cb, "usr_links", lambda: [])


# --- the plan --------------------------------------------------------------------

@pytest.mark.parametrize("cmd,fetch,inside,outside", [
    ("cargo build -p fauna-nest --features test-hooks,nostr --lib",
     "cargo fetch", "cargo build -p fauna-nest --features test-hooks,nostr --lib", None),
    ("cargo +nightly -Zunstable-options check --locked --target wasm32-unknown-unknown -p x",
     "cargo +nightly fetch -Zunstable-options --locked --target wasm32-unknown-unknown",
     "cargo +nightly -Zunstable-options check --locked --target wasm32-unknown-unknown -p x", None),
    ("cargo build --manifest-path=a/Cargo.toml --target=x86_64-unknown-linux-gnu --offline -Z build-std",
     "cargo fetch --manifest-path=a/Cargo.toml --target=x86_64-unknown-linux-gnu --offline -Z build-std",
     "cargo build --manifest-path=a/Cargo.toml --target=x86_64-unknown-linux-gnu --offline -Z build-std",
     None),
    ("cargo run -p fauna-tui --release -- --nest http://x --frozen",
     "cargo fetch", "cargo build -p fauna-tui --release",
     "cargo run -p fauna-tui --release -- --nest http://x --frozen"),
    ("cargo r --bin fauna-nest", "cargo fetch", "cargo build --bin fauna-nest", "cargo r --bin fauna-nest"),
    # the program's arguments without a `--` (cargo run takes them from the first
    # positional on — the apple-ffi bindgen line): never handed to the build half
    ("cargo run --locked -p fauna-ffi --features a,b --bin uniffi-bindgen generate --library x.a --out-dir g",
     "cargo fetch --locked", "cargo build --locked -p fauna-ffi --features a,b --bin uniffi-bindgen",
     "cargo run --locked -p fauna-ffi --features a,b --bin uniffi-bindgen generate --library x.a --out-dir g"),
    ("cargo run -pfoo --profile=dev -j 4 serve -- --port 1", "cargo fetch", "cargo build -pfoo --profile=dev -j 4",
     "cargo run -pfoo --profile=dev -j 4 serve -- --port 1"),
    ("cargo test -p fauna-core --lib -- some::test --nocapture",
     "cargo fetch", "cargo test -p fauna-core --lib --no-run",
     "cargo test -p fauna-core --lib -- some::test --nocapture"),
    ("cargo t --frozen", "cargo fetch --frozen", "cargo t --frozen --no-run", "cargo t --frozen"),
    ("cargo bench -p x", "cargo fetch", "cargo bench -p x --no-run", "cargo bench -p x"),
    ("cargo test --no-run -p x", "cargo fetch", "cargo test --no-run -p x", None),
    # a doctest run compiles its examples at run time, so cargo refuses `--doc --no-run`:
    # the whole line runs inside
    ("cargo test --locked --workspace --doc --features a/b",
     "cargo fetch --locked", "cargo test --locked --workspace --doc --features a/b", None),
    ("cargo doc -p x --no-deps", "cargo fetch", "cargo doc -p x --no-deps", None),
    ("cargo metadata --format-version 1", "cargo fetch", "cargo metadata --format-version 1", None),
])
def test_the_plan_is_prelude_inside_and_outside_per_subcommand(cmd, fetch, inside, outside):
    plan = cb.plan(cmd.split())
    assert plan == (fetch.split(), inside.split(), outside.split() if outside else None), plan


@pytest.mark.parametrize("args,own", [
    (["-p", "x", "--release", "serve", "--port", "1"], ["-p", "x", "--release"]),
    (["--bin", "a", "-F", "f", "go"], ["--bin", "a", "-F", "f"]),
    (["-pfoo", "--profile=dev", "x"], ["-pfoo", "--profile=dev"]),
    ([], []),
])
def test_cargo_runs_own_arguments_end_at_the_first_positional(args, own):
    assert cb.run_own_args(args) == own


@pytest.mark.parametrize("cmd,at", [
    ("cargo build", 1), ("cargo +nightly build", 2), ("cargo -Z x --config a=b build", 5),
    ("cargo --locked test", 2), ("cargo", 1), ("cargo --version", 2),
])
def test_the_subcommand_is_found_past_the_toolchain_and_global_options(cmd, at):
    assert cb.subcommand_at(cmd.split()) == at


# --- the compiling set and the line ----------------------------------------------

def test_the_compiling_set_is_pinned():
    assert cb.COMPILING_SUBCOMMANDS == frozenset(_COMPILING)
    assert cb.BOUNDARY_ON_COMPILE == frozenset({"linux", "darwin"})


@pytest.mark.parametrize("plat", ["linux", "darwin"])
def test_the_line_is_prelude_then_the_boundary_with_the_token_then_the_outer_half(outside, plat):
    prefix = [*cb.seccomp_fd_wrapper(), *_BWRAP, *_TOKEN_SET] if plat == "linux" else _mac(cb.TREE_MAC_PROFILE)
    argv = cb.argv(["cargo", "test", "-p", "x", "--", "t"], None, {}, plat)
    assert argv[:2] == ["/bin/bash", "-c"] and argv[3:] == ["kache-netns"], argv
    assert argv[2].splitlines() == [
        "set -e",
        _UNSET,
        "cargo fetch",
        'stale="$(mktemp "${TMPDIR:-/tmp}/cargo-boundary-stale.XXXXXX")"',
        """trap 'rm -f -- "$stale"' EXIT""",
        "for attempt in 1 2 3; do",
        "    " + shlex.join([*prefix, "cargo", "test", "-p", "x", "--no-run"]),
        "    rc=0",
        f'    FAUNA_CARGO_BOUNDARY_STALE="$stale" RUSTC_WRAPPER={shlex.quote(cb.REFUSE_RUSTC)} '
        "cargo test -p x -- t || rc=$?",
        '    if [ ! -s "$stale" ]; then exit "$rc"; fi',
        '    : > "$stale"',
        "done",
        "echo '[cargo-boundary] `cargo test -p x…`: a source changed during each of 3 compiles"
        " — giving up' >&2",
        'exit "$rc"',
    ], argv[2]
    assert Path(cb.REFUSE_RUSTC) == _HERE / "cargo-boundary-refuse-rustc"
    assert os.access(cb.REFUSE_RUSTC, os.X_OK), "the refuser is RUSTC_WRAPPER: it needs the bit"
    argv = cb.argv(["cargo", "build", "-p", "x"], None, {}, plat)
    assert argv[2].splitlines()[-1] == "exec " + shlex.join([*prefix, "cargo", "build", "-p", "x"])


def test_the_darwin_profile_is_the_trees_unless_the_caller_names_one(outside):
    assert Path(cb.TREE_MAC_PROFILE) == _HERE / "kache-mac.sb"
    assert (_HERE / "kache-mac.sb").is_file(), "the macOS leg reads the tree's profile: it ships"
    assert cb.boundary_prefix("darwin")[2] == cb.TREE_MAC_PROFILE
    assert cb.boundary_prefix("darwin", "/opt/x/kache-mac.sb")[2] == "/opt/x/kache-mac.sb"
    argv = cb.argv(["cargo", "build"], None, {}, "darwin", "/opt/x/kache-mac.sb")
    assert shlex.join(_mac("/opt/x/kache-mac.sb")) in argv[2]
    assert cb.boundary_prefix("win32") is None


def test_a_bounded_compile_applies_the_profiles_immutable_snapshot_and_names_it_inside(tmp_path):
    """A reinstall of the profile mid-build must not split the profile the compile
    runs under from the one the launcher's tail re-applies inside it: macOS refuses
    a nested `sandbox-exec` whose profile differs (`sandbox_apply: Operation not
    permitted` on every unit started after the swap). The compile applies the
    snapshot named by the profile's hash, which no install rewrites, and names it
    inside, where the launcher reads it."""
    import hashlib
    profile = tmp_path / "kache-mac.sb"
    profile.write_text("(version 1)\n; A\n")
    # an install from before the snapshots, or a missing file: the copy itself
    assert cb.mac_profile_snapshot(str(profile)) == str(profile)
    assert cb.mac_profile_snapshot(str(tmp_path / "absent.sb")) == str(tmp_path / "absent.sb")
    snaps = tmp_path / cb.MAC_PROFILE_SNAPSHOTS
    snaps.mkdir()
    old = snaps / f"kache-mac-{hashlib.sha256(profile.read_bytes()).hexdigest()}.sb"
    old.write_bytes(profile.read_bytes())
    applied = cb.mac_profile_snapshot(str(profile))
    assert applied == str(old)
    prefix = cb.boundary_prefix("darwin", applied)
    assert prefix[2] == str(old) and f"{cb.PROFILE_VAR}={old}" in prefix[prefix.index("/usr/bin/env"):]
    # the reinstall: a new snapshot first, then the copy swapped — the running
    # compile's snapshot is untouched, a new compile takes the new one
    profile.write_text("(version 1)\n; B\n")
    new = snaps / f"kache-mac-{hashlib.sha256(profile.read_bytes()).hexdigest()}.sb"
    new.write_bytes(profile.read_bytes())
    assert old.read_text().endswith("; A\n")
    assert cb.mac_profile_snapshot(str(profile)) == str(new)


@pytest.mark.parametrize("plat", ["linux", "darwin"])
@pytest.mark.parametrize("sub", _COMPILING + _NOT_COMPILING)
def test_every_compiling_cargo_runs_behind_the_boundary_and_no_other(outside, plat, sub):
    prefix = [*cb.seccomp_fd_wrapper(), *_BWRAP, *_TOKEN_SET] if plat == "linux" else _mac(cb.TREE_MAC_PROFILE)
    cmd = ["/home/u/.cargo/bin/cargo", "+nightly", "--locked", sub, "-p", "x"]
    argv = cb.argv(cmd, None, {}, plat)
    if sub not in _COMPILING:
        assert argv is None, f"`cargo {sub}` compiles nothing — it runs outside as it is"
        return
    assert argv is not None, f"`cargo {sub}` compiles — it runs behind the boundary"
    lines = argv[2].splitlines()
    assert lines[2] == "/home/u/.cargo/bin/cargo +nightly fetch --locked", lines
    assert any(shlex.join(prefix) in ln for ln in lines[2:]), lines


@pytest.mark.parametrize("cmd", ["cargo", "cargo --version", "cargo -Zhelp", "just build", "rustc x.rs",
                                 "wasm-pack build libs/fauna-wasm", "xcodebuild -scheme Fauna build"])
@pytest.mark.parametrize("plat", ["linux", "darwin"])
def test_no_subcommand_or_no_cargo_takes_no_boundary(outside, plat, cmd):
    assert cb.argv(cmd.split(), None, {}, plat) is None


def test_no_boundary_on_windows_or_already_inside(monkeypatch):
    monkeypatch.setattr(cb, "inside", lambda env: False)
    cmd = ["cargo", "build", "-p", "x"]
    assert cb.argv(cmd, None, {}, "win32") is None
    monkeypatch.setattr(cb, "inside", lambda env: True)
    for plat in ("linux", "darwin"):
        assert cb.argv(cmd, None, {}, plat) is None, \
            "a cargo reached again inside (a build script's `cargo metadata`) takes no second boundary"


def test_a_windows_program_name_is_folded():
    assert cb.cmd_name("C:\\Users\\u\\.cargo\\bin\\Cargo.EXE", "win32") == "cargo"
    assert cb.cmd_name("/usr/bin/Cargo", "linux") == "Cargo"


# --- the runners, by class (decision (5)) ----------------------------------------

@pytest.mark.parametrize("sub", _COMPILING)
@pytest.mark.parametrize("runner", [_HOSTED, _SELF_HOSTED], ids=["hosted", "self-hosted"])
def test_a_linux_runner_with_bwrap_compiles_behind_the_boundary(outside, monkeypatch, runner, sub):
    monkeypatch.setattr(cb, "bwrap_present", lambda: True)
    cmd = ["cargo", sub, "-p", "x"]
    bounded = cb.argv(cmd, None, dict(runner), "linux")
    assert bounded is not None and bounded == cb.argv(cmd, None, {}, "linux")


@pytest.mark.parametrize("sub", _COMPILING)
def test_a_hosted_linux_runner_without_bwrap_is_refused_by_name(outside, monkeypatch, sub):
    """Loud, never a silent compile outside; a runner that does not say it is
    self-hosted is read as hosted."""
    monkeypatch.setattr(cb, "bwrap_present", lambda: False)
    for env in (_HOSTED, {"GITHUB_ACTIONS": "true"}):
        with pytest.raises(cb.BoundaryRefused, match="Install bubblewrap") as refused:
            cb.argv(["cargo", sub, "-p", "x"], None, dict(env), "linux")
        assert str(refused.value).startswith("[cargo-boundary] REFUSED")


@pytest.mark.parametrize("sub", _COMPILING)
def test_a_self_hosted_linux_runner_without_bwrap_compiles_outside_with_a_warning(outside, monkeypatch,
                                                                                    capsys, sub):
    monkeypatch.setattr(cb, "bwrap_present", lambda: False)
    assert cb.argv(["cargo", sub, "-p", "x"], None, dict(_SELF_HOSTED), "linux") is None
    err = capsys.readouterr().err
    assert len(err.splitlines()) == 1 and err.startswith("[cargo-boundary] WARNING") \
        and "decision (5)" in err, err


def test_a_runner_never_refuses_a_command_that_compiles_nothing(outside, monkeypatch):
    monkeypatch.setattr(cb, "bwrap_present", lambda: False)
    for cmd in (["cargo", "fetch"], ["cargo", "metadata"], ["go", "build"]):
        assert cb.argv(cmd, None, dict(_HOSTED), "linux") is None


@pytest.mark.parametrize("sub", _COMPILING)
def test_a_hosted_macos_runner_compiles_behind_the_trees_profile(outside, sub):
    argv = cb.argv(["cargo", sub, "-p", "x"], None, dict(_HOSTED), "darwin")
    assert argv is not None
    lines = argv[2].splitlines()
    assert lines[2] == "cargo fetch", lines
    assert any(shlex.join(_mac(cb.TREE_MAC_PROFILE)) in ln for ln in lines[3:]), lines


def test_off_a_runner_the_reading_is_always_the_boundary(monkeypatch):
    monkeypatch.setattr(cb, "bwrap_present", lambda: False)
    for env in ({}, {"RUNNER_ENVIRONMENT": "github-hosted"}):
        assert cb.runner_reading(env, "linux") == "boundary"
    assert cb.runner_reading(dict(_HOSTED), "darwin") == "boundary"


# --- the inside test --------------------------------------------------------------

def test_no_token_or_a_forged_one_is_never_inside():
    assert not cb.inside({})
    assert not cb.inside({"FAUNA_KACHE_INSIDE": "net:[1]"})
    if sys.platform.startswith("linux"):
        # a token forged to this (host) namespace still has a network: not inside
        assert not cb.inside({"FAUNA_KACHE_INSIDE": os.readlink("/proc/self/ns/net")})


_INSIDE_PROBE = ("import importlib.util,os,sys\n"
                 "s=importlib.util.spec_from_file_location('cb',sys.argv[1]);m=importlib.util.module_from_spec(s)\n"
                 "s.loader.exec_module(m);print(m.inside(os.environ))\n")


def _bwrap_works() -> bool:
    if not sys.platform.startswith("linux") or not os.path.exists(_BWRAP[0]):
        return False
    try:
        return subprocess.run([*cb.seccomp_fd_wrapper(), *cb.bwrap_line(), "/usr/bin/true"], capture_output=True,
                              timeout=30).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_inside_means_the_token_matches_and_the_namespace_has_only_lo():
    python, reads = _python_reads()
    line = cb.bwrap_line(readable=[*reads, str(_HELPER)])
    got = subprocess.run([*cb.seccomp_fd_wrapper(), *line, *_TOKEN_SET, python, "-I", "-c",
                          _INSIDE_PROBE, str(_HELPER)],
                         capture_output=True, text=True, timeout=60)
    assert got.returncode == 0 and got.stdout.strip() == "True", (got.returncode, got.stderr[-2000:])


# --- the boundary line and the record --------------------------------------------

def test_a_labelled_bound_compile_prints_the_one_line_and_is_recorded(outside, tmp_path, capsys):
    rec = tmp_path / "record"
    env = {cb.RECORD_ENV: str(rec)}
    cmd = ["cargo", "build", "--locked", "-p", "x", "--lib"]
    assert cb.argv(cmd, "runner", env, "linux") is not None
    assert capsys.readouterr().err.splitlines() == [
        "[cargo-boundary] `cargo build --locked -p…` compiles with no network (runner)"]
    assert cb.argv(["cargo", "test", "-p", "y"], "unwrapped", env, "linux") is not None
    assert cb.argv(["cargo", "fetch"], "runner", env, "linux") is None, "not a compile: no record"
    assert cb.argv(cmd, None, env, "linux") is not None, "no label: neither printed nor recorded"
    assert rec.read_text().splitlines() == [shlex.join(cmd), "cargo test -p y"]
    assert "(unwrapped)" in capsys.readouterr().err


def test_no_record_file_named_records_nothing(outside, tmp_path, capsys):
    assert cb.argv(["cargo", "build"], "runner", {}, "linux") is not None
    assert list(tmp_path.iterdir()) == []


# --- the program ------------------------------------------------------------------

def _run(*args, env=None, timeout=60):
    return subprocess.run([sys.executable, "-I", str(_HELPER), *args], capture_output=True, text=True,
                          timeout=timeout, env={**os.environ, **(env or {})})


# --- the runner's two steps (decision (6), parts (iv)–(v)) ------------------------

def _gh(tmp_path: Path) -> dict[str, str]:
    """A fake GitHub Actions step environment in a tmp dir."""
    (tmp_path / "runner-temp").mkdir()
    for f in ("github_path", "github_env"):
        (tmp_path / f).write_text("")
    return {"GITHUB_PATH": str(tmp_path / "github_path"), "GITHUB_ENV": str(tmp_path / "github_env"),
            "RUNNER_TEMP": str(tmp_path / "runner-temp")}


@pytest.mark.parametrize("plat", ["linux", "darwin"])
def test_runner_setup_puts_the_shim_first_and_names_the_front_and_the_record(tmp_path, monkeypatch, plat,
                                                                            capsys):
    monkeypatch.setattr(cb, "bwrap_present", lambda: True)
    monkeypatch.setattr(cb, "sandbox_exec_present", lambda: True)
    env = _gh(tmp_path)
    probed = []
    assert cb.runner_setup(env, plat, probe=lambda a: probed.append(a) or 0) == 0
    assert (tmp_path / "github_path").read_text().splitlines() == [str(_SHIM.parent)]
    lines = dict(ln.split("=", 1) for ln in (tmp_path / "github_env").read_text().splitlines())
    assert lines == {"FAUNA_CARGO_FRONT": str(_HELPER), "FAUNA_CARGO_FRONT_PYTHON": sys.executable,
                     cb.RECORD_ENV: str(tmp_path / "runner-temp" / "cargo-boundary.log")}
    assert probed == ([list(cb.BWRAP_PROBE)] if plat == "linux" else [])
    assert str(_SHIM.parent) in capsys.readouterr().out


def test_the_linux_probe_is_the_bubblewrap_steps_own_line():
    assert shlex.join(cb.BWRAP_PROBE) == "/usr/bin/bwrap --unshare-all --ro-bind / / true"


@pytest.mark.parametrize("plat,present,probe_rc,needle", [
    ("linux", False, 0, "Install bubblewrap"),
    ("linux", True, 1, "unprivileged namespace"),
    ("darwin", False, 0, "sandbox-exec"),
    ("win32", True, 0, "no boundary primitive"),
])
def test_runner_setup_refuses_a_runner_without_a_primitive_and_writes_nothing(tmp_path, monkeypatch, capsys,
                                                                              plat, present, probe_rc,
                                                                              needle):
    monkeypatch.setattr(cb, "bwrap_present", lambda: present)
    monkeypatch.setattr(cb, "sandbox_exec_present", lambda: present)
    env = _gh(tmp_path)
    assert cb.runner_setup(env, plat, probe=lambda a: probe_rc) == 1
    assert needle in capsys.readouterr().err
    assert (tmp_path / "github_path").read_text() == "" and (tmp_path / "github_env").read_text() == ""


@pytest.mark.parametrize("drop", ["GITHUB_PATH", "GITHUB_ENV", "RUNNER_TEMP"])
def test_runner_setup_refuses_off_github_actions(tmp_path, capsys, drop):
    env = _gh(tmp_path)
    del env[drop]
    assert cb.runner_setup(env, "linux", probe=lambda a: 0) == 1
    assert "not a GitHub Actions step" in capsys.readouterr().err


def test_runner_witness_reds_an_empty_or_absent_record(tmp_path, capsys):
    rec = tmp_path / "record"
    for env in ({}, {cb.RECORD_ENV: str(rec)}):
        assert cb.runner_witness(env) == 1
    rec.write_text("\n")
    assert cb.runner_witness({cb.RECORD_ENV: str(rec)}) == 1
    assert capsys.readouterr().err.count("no cargo reached the boundary") == 3


def _set_up(tmp_path, monkeypatch) -> dict[str, str]:
    """runner-setup in a fake job, and the environment a later step sees: the
    variables as written, the shim's directory first on PATH."""
    monkeypatch.setattr(cb, "bwrap_present", lambda: True)
    gh = _gh(tmp_path)
    cargo = _fake_cargo_dir(tmp_path) / "cargo"
    assert cb.runner_setup(gh, "linux", probe=lambda a: 0, which=lambda n, path=None: str(cargo)) == 0
    step = dict(gh)
    step.update(ln.split("=", 1) for ln in Path(gh["GITHUB_ENV"]).read_text().splitlines())
    step["PATH"] = os.pathsep.join([*Path(gh["GITHUB_PATH"]).read_text().split(), str(cargo.parent), "/usr/bin"])
    Path(step[cb.RECORD_ENV]).write_text("cargo build -p x\ncargo test --locked\n")
    return step


def test_runner_setup_records_the_variables_the_shims_place_and_four_digests(tmp_path, monkeypatch):
    step = _set_up(tmp_path, monkeypatch)
    manifest = json.loads((Path(step["RUNNER_TEMP"]) / cb.MANIFEST_NAME).read_text())
    assert manifest["shim_dir"] == str(_SHIM.parent)
    assert manifest["env"] == {k: step[k] for k in ("FAUNA_CARGO_FRONT", "FAUNA_CARGO_FRONT_PYTHON",
                                                     cb.RECORD_ENV)}
    assert sorted(manifest["digests"]) == sorted([str(_SHIM), str(_HELPER), cb.TREE_MAC_PROFILE,
                                                  str(tmp_path / "bin" / "cargo")])


def test_runner_witness_prints_the_count_and_every_command(tmp_path, monkeypatch, capsys):
    step = _set_up(tmp_path, monkeypatch)
    assert cb.runner_witness(step) == 0, capsys.readouterr().err
    out = capsys.readouterr().out
    assert "2 cargo compile(s)" in out and "cargo build -p x" in out and "cargo test --locked" in out


@pytest.mark.parametrize("bypass", ["digest", "variable", "path", "manifest"])
def test_runner_witness_reds_what_a_bypass_cannot_keep_green(tmp_path, monkeypatch, capsys, bypass):
    """Decision (7), part (b): a non-empty record is not enough — a compile that
    rewrote the cargo proxy, dropped a variable through `$GITHUB_ENV`, or put a
    directory before the shim through `$GITHUB_PATH` reds the job by name."""
    step = _set_up(tmp_path, monkeypatch)
    if bypass == "digest":
        cargo = tmp_path / "bin" / "cargo"
        cargo.write_text(cargo.read_text() + "# planted\n")
        needle = "changed since runner-setup"
    elif bypass == "variable":
        step["FAUNA_CARGO_FRONT"] = ""
        needle = "FAUNA_CARGO_FRONT is ''"
    elif bypass == "path":
        step["PATH"] = os.pathsep.join([str(tmp_path / "planted"), step["PATH"]])
        needle = "not the shim's directory"
    else:
        (Path(step["RUNNER_TEMP"]) / cb.MANIFEST_NAME).unlink()
        needle = "no runner-setup manifest"
    assert cb.runner_witness(step) == 1
    assert needle in capsys.readouterr().err


def test_runner_witness_reads_past_the_entry_uv_run_puts_first(tmp_path, monkeypatch, capsys):
    """The witness step is `uv run --no-project python …`, and `uv run` puts the
    interpreter's scripts directory first on the PATH it hands python
    (`/usr/local/bin` under a Debian python, `/opt/homebrew/bin` under
    Homebrew's): the 0.1.6 sync PR's first public run redded every compiling job
    on it with the boundary intact. That one entry is uv's, not the job's."""
    step = _set_up(tmp_path, monkeypatch)
    step["UV"] = "/usr/local/bin/uv"
    step["PATH"] = os.pathsep.join([sysconfig.get_path("scripts"), step["PATH"]])
    assert cb.runner_witness(step) == 0, capsys.readouterr().err


def test_runner_witness_reads_past_one_uv_entry_never_two(tmp_path, monkeypatch, capsys):
    """Only the one entry `uv run` adds is passed over: the same directory
    planted before the shim through `$GITHUB_PATH` still reds the job, and so
    does the entry alone when the witness is not under `uv run`."""
    step = _set_up(tmp_path, monkeypatch)
    step["UV"] = "/usr/local/bin/uv"
    scripts = sysconfig.get_path("scripts")
    step["PATH"] = os.pathsep.join([scripts, scripts, step["PATH"]])
    assert cb.runner_witness(step) == 1
    assert "not the shim's directory" in capsys.readouterr().err
    del step["UV"]
    step["PATH"] = os.pathsep.join(step["PATH"].split(os.pathsep)[1:])
    assert cb.runner_witness(step) == 1


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_setup_then_a_shimmed_cargo_then_the_witness_end_to_end(tmp_path):
    """The three steps as a job runs them: setup writes the files, a later step's
    cargo resolves to the shim on that PATH, compiles inside, is recorded, and the
    witness reads it green."""
    gh = _gh(tmp_path)
    got = _run("runner-setup", env=gh)
    assert got.returncode == 0, got.stderr
    bin_dir = _fake_cargo_dir(tmp_path)
    log = tmp_path / "log"
    step_env = dict(os.environ)
    step_env.update(ln.split("=", 1) for ln in Path(gh["GITHUB_ENV"]).read_text().splitlines())
    shim_dirs = Path(gh["GITHUB_PATH"]).read_text().split()
    step_env.update({"PATH": os.pathsep.join([*shim_dirs, str(bin_dir), os.environ["PATH"]]),
                     "FAKE_CARGO_LOG": str(log), "FAUNA_KACHE_INSIDE": "",
                     "RUNNER_TEMP": gh["RUNNER_TEMP"]})
    built = subprocess.run(["cargo", "build", "-p", "x"], env=step_env, capture_output=True, text=True,
                           timeout=60)
    assert built.returncode == 0, built.stderr
    assert log.read_text().splitlines() == ["CARGO fetch", "CARGO build -p x"]
    witness = subprocess.run([sys.executable, "-I", str(_HELPER), "runner-witness"], env=step_env,
                             capture_output=True, text=True, timeout=60)
    assert witness.returncode == 0 and "cargo build -p x" in witness.stdout, witness.stderr


def test_the_runner_entry_points_take_no_arguments():
    got = _run("runner-witness", "extra")
    assert got.returncode == 2 and "usage:" in got.stderr, got.stderr


@pytest.mark.parametrize("args", [[], ["--bogus", "--", "cargo"], ["--"]])
def test_a_malformed_call_is_a_usage_error(args):
    got = _run(*args)
    assert got.returncode == 2 and "usage:" in got.stderr, got.stderr


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="the refusal is a Linux runner's")
@pytest.mark.skipif(os.path.exists("/usr/bin/bwrap"), reason="needs a host without /usr/bin/bwrap")
def test_the_program_turns_a_refusal_into_a_named_red():
    got = _run("--", "cargo", "build", env={**_HOSTED, "FAUNA_KACHE_INSIDE": ""})
    assert got.returncode == 1 and "[cargo-boundary] REFUSED" in got.stderr, got.stderr


#: A fake cargo's answer to the helper's `cargo metadata --no-deps` (the writable
#: set's resolution): the log's directory is its target, so the fake's log write
#: inside the read-only root lands in the bound set. Not logged.
_FAKE_METADATA = ('if [ "$1" = metadata ] && [[ " $* " == *" --no-deps "* ]]; then '
                  'printf \'{"target_directory":"%s","workspace_root":"%s"}\\n\' '
                  '"$(dirname "$FAKE_CARGO_LOG")" "$(dirname "$FAKE_CARGO_LOG")"; exit 0; fi\n')


def _fake_cargo_dir(tmp_path: Path) -> Path:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "cargo"
    fake.write_text('#!/bin/bash\n' + _FAKE_METADATA + 'echo "CARGO $*" >> "$FAKE_CARGO_LOG"\n')
    fake.chmod(0o755)
    return bin_dir


@pytest.mark.skipif(os.name != "posix", reason="a bash fake cargo")
@pytest.mark.parametrize("held", [False, True], ids=["direct", "held"])
def test_a_cargo_that_compiles_nothing_runs_as_it_is(tmp_path, held):
    bin_dir = _fake_cargo_dir(tmp_path)
    log = tmp_path / "log"
    got = _run(*(["--held"] if held else []), "--", "cargo", "metadata", "--format-version", "1",
               env={"PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}", "FAKE_CARGO_LOG": str(log)})
    assert got.returncode == 0, got.stderr
    assert log.read_text().splitlines() == ["CARGO metadata --format-version 1"]
    assert "[cargo-boundary]" not in got.stderr


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_the_program_runs_prelude_outside_the_compile_inside_and_records_it(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    (bin_dir / "cargo").write_text(
        "#!/bin/bash\n" + _FAKE_METADATA + "ifaces=$(awk 'NR>2{sub(\":.*\",\"\",$1); print $1}' /proc/self/net/dev | paste -sd, -)\n"
        'echo "CARGO $* | ifaces=$ifaces" >> "$FAKE_CARGO_LOG"\n')
    (bin_dir / "cargo").chmod(0o755)
    log, rec = tmp_path / "log", tmp_path / "record"
    got = _run("--", "cargo", "test", "-p", "x", "--", "t",
               env={"PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_CARGO_LOG": str(log),
                    cb.RECORD_ENV: str(rec), "FAUNA_KACHE_INSIDE": ""})
    assert got.returncode == 0, got.stderr
    calls = log.read_text().splitlines()
    assert len(calls) == 3, calls
    ifaces = [c.split(" | ifaces=")[1] for c in calls]
    assert calls[0].startswith("CARGO fetch |") and ifaces[0] != "lo", calls
    assert calls[1] == "CARGO test -p x --no-run | ifaces=lo", calls
    assert calls[2].startswith("CARGO test -p x -- t |") and ifaces[2] != "lo", calls
    assert "[cargo-boundary] `cargo test -p x…` compiles with no network (runner)" in got.stderr
    assert rec.read_text().splitlines() == ["cargo test -p x -- t"]


# --- the shim ---------------------------------------------------------------------

@pytest.mark.skipif(os.name != "posix" or shutil.which("bash") is None, reason="a bash shim")
def test_the_shim_with_no_front_named_runs_cargo_plain_once_noted(tmp_path):
    bin_dir = _fake_cargo_dir(tmp_path)
    log = tmp_path / "log"
    env = {k: v for k, v in os.environ.items() if k not in ("FAUNA_CARGO_FRONT", "FAUNA_CARGO_FRONT_PYTHON")}
    env.update({"PATH": f"{_SHIM.parent}:{bin_dir}:{env['PATH']}", "FAKE_CARGO_LOG": str(log)})
    got = subprocess.run(["cargo", "build", "-p", "x"], env=env, capture_output=True, text=True, timeout=60)
    assert got.returncode == 0, got.stderr
    assert log.read_text().splitlines() == ["CARGO build -p x"]
    assert got.stderr.count("[cargo-shim]") == 1 and "plain" in got.stderr, got.stderr


@pytest.mark.skipif(os.name != "posix" or shutil.which("bash") is None, reason="a bash shim")
def test_the_shim_calls_back_the_named_front_with_held_and_its_directory_off_path(tmp_path):
    front_dir = tmp_path / "a front with a space"
    front_dir.mkdir()
    front = front_dir / "front.py"
    front.write_text("import os,sys\nprint(repr(sys.argv[1:]))\nprint(os.environ['PATH'])\n")
    env = dict(os.environ)
    env.update({"PATH": f"{_SHIM.parent}:{env['PATH']}", "FAUNA_CARGO_FRONT": str(front),
                "FAUNA_CARGO_FRONT_PYTHON": sys.executable})
    got = subprocess.run(["cargo", "test", "-p", "x y"], env=env, capture_output=True, text=True, timeout=60)
    assert got.returncode == 0, got.stderr
    args, path = got.stdout.splitlines()
    assert args == repr(["--held", "--", "cargo", "test", "-p", "x y"])
    assert str(_SHIM.parent) not in path.split(":")


# --- decision (7): what a bounded compile may write --------------------------------

def test_the_linux_line_is_an_empty_root_with_its_reads_then_its_writes_before_its_tail(monkeypatch):
    """The constant part (the launcher's Rust copy pins it equal): every namespace
    unshared, the line dying with its parent, an empty tmpfs root made read-only
    last, `/usr`, `/etc` and `/sys` read-only, `/run`, `/tmp` and `/dev/shm` empty
    tmpfs, TMPDIR the private `/tmp`, the system-call filter from descriptor 9.
    The dynamic part sits before the last three words: the host's `/bin`-style
    links, `$HOME` made, each read path `--ro-bind-try p p`, then each writable
    path `--bind p p` — never a parent; the prefix opens the filter on the
    descriptor first (one `bash -c` outside `bwrap`), so the prefix is still one
    argv a caller runs as it is. The launcher's unit test pins the same line for
    the same inputs."""
    assert cb.BWRAP == tuple(_BWRAP) and cb.BWRAP_TAIL == 3
    assert "--bind" not in cb.BWRAP and ("--ro-bind", "/", "/") not in zip(cb.BWRAP, cb.BWRAP[1:], cb.BWRAP[2:])
    assert cb.BWRAP[-5:] == ("--seccomp", str(cb.SECCOMP_FD), "--remount-ro", "/", "--")
    assert cb.USR_LINKS == ("/bin", "/lib", "/lib64", "/sbin")
    links = cb.usr_links(readlink=lambda p: {"/bin": "usr/bin", "/lib": "usr/lib", "/sbin": "usr/sbin"}[p]
                         if p != "/lib64" else (_ for _ in ()).throw(OSError()), isdir=lambda p: False)
    assert links == ["--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/sbin", "/sbin"]
    line = cb.bwrap_line(["$CARGO_TARGET_DIR/kache/store"], ["/home/u/.cargo", "/home/u/.rustup"], "/home/u", links)
    assert line == [*_BWRAP[:-3], *links, "--dir", "/home/u",
                    "--ro-bind-try", "/home/u/.cargo", "/home/u/.cargo",
                    "--ro-bind-try", "/home/u/.rustup", "/home/u/.rustup",
                    "--bind", "$CARGO_TARGET_DIR/kache/store", "$CARGO_TARGET_DIR/kache/store",
                    "--remount-ro", "/", "--"]
    # a host directory in place of a link is bound read-only; an absent one is left out
    def no_link(p):
        raise OSError(p)
    assert cb.usr_links(readlink=no_link, isdir=lambda p: p == "/bin") == ["--ro-bind", "/bin", "/bin"]
    assert cb.topmost(["/k/bin", "/k", "/k/real/bin", "/kx", "/k/"]) == ["/k", "/kx"]
    assert cb.bwrap_line(links=[]) == list(cb.BWRAP)
    monkeypatch.setattr(cb, "usr_links", lambda: [])
    prefix = cb.boundary_prefix("linux", writable=["/t"])
    assert prefix == [*cb.seccomp_fd_wrapper(), *cb.bwrap_line(["/t"]), *_TOKEN_SET]
    assert prefix[:2] == ["/bin/bash", "-c"] and prefix[3] == "kache-seccomp"
    assert prefix[2].startswith(f"exec {cb.SECCOMP_FD}< <(printf '") and prefix[2].endswith("'); exec \"$@\"")
    # darwin: the roles ride as `-D` parameters, every one on every line
    assert cb.MAC_ROLES == _MAC_ROLES and cb.MAC_UNUSED_ROLE == _UNUSED
    roles = {"FAUNA_TARGET": "/t", "FAUNA_SCRATCH": "/t/.fauna-scratch/x"}
    assert cb.boundary_prefix("darwin", "/p.sb", ["/t"], roles) == _mac("/p.sb", roles)
    assert cb.boundary_prefix("darwin", "/p.sb") == _mac("/p.sb")


@pytest.mark.parametrize("machine,arch,nrs", [
    ("x86_64", 0xC000003E, (41, 53, 425)),
    ("aarch64", 0xC00000B7, (198, 199, 425)),
])
def test_the_seccomp_program_is_the_family_allowlist_over_socket_and_socketpair(machine, arch, nrs):
    """Decision (1)'s filter, word by word: a foreign arch or an x32 number is
    killed, `io_uring_setup` answers EPERM, `socket` and `socketpair` answer
    EAFNOSUPPORT for every family but unix, inet, inet6 and netlink, everything
    else is allowed; every jump lands inside the program."""
    import struct
    program = cb.seccomp_program(machine)
    words = [struct.unpack("<HBBI", program[i:i + 8]) for i in range(0, len(program), 8)]
    assert len(words) == 17
    kill, errno_, allow = 0x80000000, 0x00050000, 0x7FFF0000
    assert words[0:3] == [(0x20, 0, 0, 4), (0x15, 1, 0, arch), (0x06, 0, 0, kill)]
    assert words[3:6] == [(0x20, 0, 0, 0), (0x45, 0, 1, 0x40000000), (0x06, 0, 0, kill)]
    assert words[6:8] == [(0x15, 0, 1, nrs[2]), (0x06, 0, 0, errno_ | 1)]
    assert words[8:11] == [(0x15, 1, 0, nrs[0]), (0x15, 0, 6, nrs[1]), (0x20, 0, 0, 16)]
    assert [(w[1], w[3]) for w in words[11:15]] == [(4, 1), (3, 2), (2, 10), (1, 16)]
    assert words[15:17] == [(0x06, 0, 0, errno_ | 97), (0x06, 0, 0, allow)]
    for i, (code, jt, jf, _) in enumerate(words):
        if code != 0x06:
            assert i + 1 + jt < len(words) and i + 1 + jf < len(words), i
    assert cb.SOCKET_FAMILIES == (1, 2, 10, 16)


def test_a_machine_with_no_filter_is_refused_never_bounded_without_one():
    with pytest.raises(cb.BoundaryRefused, match="riscv64"):
        cb.seccomp_program("riscv64")
    with pytest.raises(cb.BoundaryRefused):
        cb.seccomp_fd_wrapper("riscv64")


@pytest.mark.skipif(not os.path.exists("/bin/bash"), reason="the wrapper is one bash line")
def test_the_fd_wrapper_opens_the_program_on_the_descriptor():
    """The `printf` literal round-trips through bash: the command the wrapper
    runs reads the program's exact bytes from descriptor 9."""
    wrapper = cb.seccomp_fd_wrapper("aarch64")
    got = subprocess.run([*wrapper, sys.executable, "-c",
                          f"import os, sys; sys.stdout.buffer.write(os.read({cb.SECCOMP_FD}, 4096))"],
                         capture_output=True, timeout=60)
    assert got.returncode == 0, got.stderr
    assert got.stdout == cb.seccomp_program("aarch64")


_FAMILIES_PROBE = r'''
import ctypes, errno, json, os, socket
res = {}
def attempt(name, fn):
    try:
        fn(); res[name] = "OK"
    except OSError as e:
        res[name] = errno.errorcode.get(e.errno, str(e.errno))
attempt("vsock", lambda: socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM).close())
attempt("alg", lambda: socket.socket(socket.AF_ALG, socket.SOCK_SEQPACKET).close())
for name, fam in (("unix", socket.AF_UNIX), ("inet", socket.AF_INET), ("inet6", socket.AF_INET6)):
    attempt(name, lambda fam=fam: socket.socket(fam, socket.SOCK_STREAM).close())
attempt("netlink", lambda: socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0).close())
attempt("socketpair", lambda: [s.close() for s in socket.socketpair()])
libc = ctypes.CDLL(None, use_errno=True)
def io_uring():
    params = ctypes.create_string_buffer(120)
    fd = libc.syscall(425, 4, params)
    if fd < 0:
        raise OSError(ctypes.get_errno(), "io_uring_setup")
    os.close(fd)
attempt("io_uring_setup", io_uring)
def fork():
    pid = os.fork()
    if pid == 0:
        os._exit(0)
    os.waitpid(pid, 0)
attempt("fork", fork)
res["seccomp"] = next(l.split()[1] for l in open("/proc/self/status") if l.startswith("Seccomp:"))
print(json.dumps(res))
'''


def _probe_json_env(argv: list[str], env: dict) -> dict:
    out = subprocess.run(argv, capture_output=True, text=True, timeout=60, env=env)
    assert out.returncode == 0, out.stderr[-2000:]
    return json.loads(out.stdout.strip().splitlines()[-1])


def _probe_json(argv: list[str]) -> dict:
    got = subprocess.run(argv, capture_output=True, text=True, timeout=120)
    assert got.returncode == 0, got.stderr[-2000:]
    return json.loads(got.stdout.strip().splitlines()[-1])


@pytest.mark.skipif(
    not sys.platform.startswith("linux") or not os.path.exists("/usr/bin/bwrap") or cb.inside(os.environ),
    reason="needs /usr/bin/bwrap, outside any boundary (Linux)")
def test_a_socket_family_the_namespace_lacks_is_refused_inside_the_line_and_the_mutation_reads_the_hole(tmp_path):
    """Decision (1)'s pin: inside the helper's own line `socket(AF_VSOCK)` and
    `socket(AF_ALG)` answer EAFNOSUPPORT and `io_uring_setup` EPERM, while the
    four families the namespace contains, a socketpair and a fork still work;
    the same line with its `--seccomp 9` dropped gives the host's own answers
    (on the primary dev VM a vsock socket that reaches the hypervisor) — so this test is known
    to see the hole it guards."""
    probe = tmp_path / "families-probe.py"
    probe.write_text(_FAMILIES_PROBE)
    outside = _probe_json([sys.executable, str(probe)])
    python, reads = _python_reads()
    line = cb.boundary_prefix("linux", readable=[*reads, str(tmp_path)])
    inside = _probe_json([*line, python, "-I", str(probe)])
    assert inside["seccomp"] == "2", inside
    assert {k: inside[k] for k in ("vsock", "alg", "io_uring_setup")} == {
        "vsock": "EAFNOSUPPORT", "alg": "EAFNOSUPPORT", "io_uring_setup": "EPERM"}, inside
    assert {k: inside[k] for k in ("unix", "inet", "inet6", "netlink", "socketpair", "fork")} == {
        "unix": "OK", "inet": "OK", "inet6": "OK", "netlink": "OK", "socketpair": "OK", "fork": "OK"}, inside
    at = line.index("--seccomp")
    assert line[at + 1] == str(cb.SECCOMP_FD)
    hole = _probe_json([*line[:at], *line[at + 2:], python, "-I", str(probe)])
    assert hole["seccomp"] == "0", hole
    for k in ("vsock", "alg", "io_uring_setup"):
        assert hole[k] == outside[k], (k, hole, outside)
    assert any(hole[k] != inside[k] for k in ("vsock", "alg", "io_uring_setup")), (hole, inside)


def _python_reads() -> tuple[str, list[str]]:
    """This test's interpreter, resolved to its installation, and the tree it
    reads — what a probe needs on the read list of a line over an empty root."""
    python = os.path.realpath(sys.executable)
    return python, [os.path.dirname(os.path.dirname(python))]


#: Decision (8)'s probe: a connect to each path named, then what the root shows.
_SOCKET_PROBE = r"""
import errno, json, os, socket, sys
out = {}
for path in sys.argv[1:]:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(path)
        out[path] = "CONNECTED"
    except OSError as e:
        out[path] = errno.errorcode.get(e.errno, str(e.errno))
    finally:
        s.close()
home = os.environ.get("HOME", "")
out["home"] = sorted(os.listdir(home)) if os.path.isdir(home) else None
out["present"] = [p for p in ("/var", "/opt", "/srv", "/media", "/mnt") if os.path.lexists(p)]
try:
    os.listdir("/proc/" + os.environ.get("PROBE_OUTER_PID", "0") + "/root")
    out["outer_root"] = "LISTED"
except OSError as e:
    out["outer_root"] = errno.errorcode.get(e.errno, str(e.errno))
print(json.dumps(out))
"""


def _session_host_shape(tmp_root: str) -> tuple[dict, str, str, list]:
    """A scratch HOME holding a listener at the session host's path shape and a
    listener under a store the line binds writable (both this test's own; the
    real session-host directory is never touched): (env, foreign, store_sock,
    sockets to close)."""
    home = os.path.join(tmp_root, "home")
    host_dir = os.path.join(home, ".cache", "fauna", "session-host")
    for d in (host_dir, os.path.join(home, ".cargo"), os.path.join(home, ".rustup"),
              os.path.join(tmp_root, "store")):
        os.makedirs(d, exist_ok=True)
    foreign = os.path.join(host_dir, "t.sock")
    store_sock = os.path.join(tmp_root, "store", "daemon.sock")
    socks = []
    for path in (foreign, store_sock):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.bind(path)
        s.listen(4)
        socks.append(s)
    env = {k: v for k, v in os.environ.items() if k not in ("FAUNA_KACHE_INSIDE", "CARGO_HOME", "RUSTUP_HOME")}
    env.update(HOME=home, PROBE_OUTER_PID=str(os.getpid()))
    return env, foreign, store_sock, socks


def _scratch_root() -> str:
    """A short scratch directory (a unix socket path is at most 107 bytes) that is
    not under `/tmp` or `/run`, which every line replaces with an empty tmpfs."""
    base = "/work/tmp" if os.path.isdir("/work/tmp") else os.path.expanduser("~")
    return tempfile.mkdtemp(prefix="cb8-", dir=base)


_linux_bwrap_outside = pytest.mark.skipif(
    not sys.platform.startswith("linux") or not os.path.exists("/usr/bin/bwrap") or cb.inside(os.environ),
    reason="needs /usr/bin/bwrap, outside any boundary (Linux)")


@_linux_bwrap_outside
def test_a_socket_outside_the_compiles_trees_is_absent_inside_the_line_and_the_mutation_reads_the_hole():
    """Decision (8)'s pin: inside the helper's own line a connect to a listener at
    the session host's path shape under HOME answers ENOENT — the path does not
    exist over the empty root — while a listener under the writable set (the
    store's socket) connects; the same line with `--ro-bind / /` in place of the
    empty root connects to both, so this test sees the hole it guards."""
    root = _scratch_root()
    socks = []
    try:
        env, foreign, store_sock, socks = _session_host_shape(root)
        probe = os.path.join(root, "probe.py")
        Path(probe).write_text(_SOCKET_PROBE)
        python, reads = _python_reads()
        home = env["HOME"]
        line = cb.boundary_prefix("linux", writable=[os.path.join(root, "store")],
                                  readable=[*reads, os.path.join(home, ".cargo"),
                                            os.path.join(home, ".rustup"), probe], home=home)
        inside = _probe_json_env([*line, python, "-I", probe, foreign, store_sock], env)
        assert inside[foreign] == "ENOENT", inside
        assert inside[store_sock] == "CONNECTED", inside
        at = line.index("--tmpfs")
        assert line[at:at + 2] == ["--tmpfs", "/"]
        mutated = [*line[:at], "--ro-bind", "/", "/", *line[at + 2:]]
        hole = _probe_json_env([*mutated, python, "-I", probe, foreign, store_sock], env)
        assert hole[foreign] == "CONNECTED" and hole[store_sock] == "CONNECTED", hole
    finally:
        for s in socks:
            s.close()
        shutil.rmtree(root, ignore_errors=True)


@_linux_bwrap_outside
def test_inside_the_line_home_holds_only_the_named_trees_and_no_host_root_or_system_tree_is_seen():
    """Decision (8)'s inventory pin: `$HOME` inside lists exactly the trees the
    read list names under it (the session host's `.cache` is not one), `/var`,
    `/opt`, `/srv`, `/media` and `/mnt` are absent, and this test's own
    process's root answers EACCES under `/proc` — the line's user namespace is
    not the host's (the pid namespace stays shared: kache's daemon needs it)."""
    root = _scratch_root()
    socks = []
    try:
        env, foreign, _, socks = _session_host_shape(root)
        probe = os.path.join(root, "probe.py")
        Path(probe).write_text(_SOCKET_PROBE)
        python, reads = _python_reads()
        home = env["HOME"]
        line = cb.boundary_prefix("linux", readable=[*reads, os.path.join(home, ".cargo"),
                                                      os.path.join(home, ".rustup"), probe], home=home)
        inside = _probe_json_env([*line, python, "-I", probe], env)
        assert inside["home"] == [".cargo", ".rustup"], inside
        assert inside["present"] == [], inside
        assert inside["outer_root"] == "EACCES", inside
        assert os.listdir(f"/proc/{os.getpid()}/root")
    finally:
        for s in socks:
            s.close()
        shutil.rmtree(root, ignore_errors=True)


def test_the_read_list_is_the_homes_the_workspace_and_each_addition_by_name(tmp_path, monkeypatch):
    """Decision (8): `CARGO_HOME` and `RUSTUP_HOME` (their defaults under HOME),
    the workspace root cargo names, the `.cargo` directory of the cwd and each
    ancestor, the git common directory, each cross linker's toolchain root on
    PATH, the grant's jobserver FIFO and the front's directory and interpreter
    root; a workspace cargo cannot name is refused, never guessed."""
    ws = tmp_path / "ws"
    (ws / ".cargo").mkdir(parents=True)
    (tmp_path / ".cargo").mkdir()
    ndk = tmp_path / "ndk" / "prebuilt"
    (ndk / "bin").mkdir(parents=True)
    linker = ndk / "bin" / cb.CROSS_LINKERS[0]
    linker.write_text("#!/bin/sh\n")
    linker.chmod(0o755)
    monkeypatch.chdir(ws)
    meta = json.dumps({"workspace_root": str(ws), "target_directory": str(ws / "target")})

    def run(argv):
        if argv[0] == "git":
            return 0, f"{tmp_path}/repo.git\n", ""
        return 0, meta, ""
    env = {"HOME": "/home/u", "PATH": str(ndk / "bin"),
           "CARGO_MAKEFLAGS": "-j4 --jobserver-auth=fifo:/slots/jobserver-g.fifo",
           "FAUNA_CARGO_FRONT": "/main/scripts/build-slot.py",
           "FAUNA_CARGO_FRONT_PYTHON": str(tmp_path / "venv" / "bin" / "python")}
    got = cb.readable_set(["cargo", "build"], env, run=run)
    # the shim execs the interpreter as named: a venv's own root is on the list
    assert str(tmp_path / "venv") in got
    assert got[:3] == ["/home/u/.cargo", "/home/u/.rustup", str(ws)]
    assert got[3:5] == [str(ws / ".cargo"), str(tmp_path / ".cargo")]
    assert f"{tmp_path}/repo.git" in got and str(ndk) in got
    assert "/slots/jobserver-g.fifo" in got and "/main/scripts" in got
    env2 = {"CARGO_HOME": "/c", "RUSTUP_HOME": "/r", "PATH": ""}
    assert cb.readable_set(["cargo", "build"], env2, run=run)[:2] == ["/c", "/r"]
    with pytest.raises(cb.BoundaryRefused, match="workspace"):
        cb.readable_set(["cargo", "build"], env, run=lambda a: (101, "", "error: no Cargo.toml"))
    assert cb.jobserver_fifo({"CARGO_MAKEFLAGS": "--jobserver-auth=3,4"}) is None


def test_the_read_list_holds_the_pinned_wasm32_compiler_root(tmp_path, monkeypatch):
    """The justfile exports the pinned wasm32 C compiler (`CC_wasm32_unknown_unknown`,
    `AR_wasm32_unknown_unknown`, `supply-chain/wasm32-cc.toml`) and `zstd-sys`'s
    build script runs it inside the boundary: its install root (above `bin`, the
    resource headers under `lib`) is on the read list, else every web wasm build
    fails with the tool not found. A relative or unset value adds nothing."""
    ws = tmp_path / "ws"
    ws.mkdir()
    root = tmp_path / "wasm32-cc"
    (root / "bin").mkdir(parents=True)
    (root / "bin" / "clang-23").write_text("")
    (root / "bin" / "clang").symlink_to("clang-23")
    monkeypatch.chdir(ws)
    meta = json.dumps({"workspace_root": str(ws)})

    def run(argv):
        return (1, "", "") if argv[0] == "git" else (0, meta, "")
    env = {"HOME": "/home/u", "PATH": "",
           "CC_wasm32_unknown_unknown": str(root / "bin" / "clang"),
           "AR_wasm32_unknown_unknown": str(root / "bin" / "llvm-ar")}
    got = cb.readable_set(["cargo", "build"], env, run=run)
    assert str(root) in got
    bare = cb.readable_set(["cargo", "build"], {"HOME": "/home/u", "PATH": "",
                                                "CC_wasm32_unknown_unknown": "clang"}, run=run)
    assert str(root) not in bare and "." not in bare and "" not in bare


def test_the_file_command_variables_are_unset_before_anything_runs(outside):
    assert cb.FILE_COMMAND_VARS == ("GITHUB_ENV", "GITHUB_PATH", "GITHUB_OUTPUT", "GITHUB_STATE",
                                    "GITHUB_STEP_SUMMARY")
    for plat in ("linux", "darwin"):
        lines = cb.argv(["cargo", "build"], None, {}, plat)[2].splitlines()
        assert lines[:2] == ["set -e", _UNSET], lines


def test_the_writable_set_is_bound_made_first_and_joined_by_the_callers(monkeypatch):
    monkeypatch.setattr(cb, "inside", lambda env: False)
    monkeypatch.setattr(cb, "writable_set", lambda cmd, env=None: ["/w/target"])
    monkeypatch.setattr(cb, "readable_set", lambda cmd, env=None: [])
    monkeypatch.setattr(cb, "usr_links", lambda: [])
    lines = cb.argv(["cargo", "build"], None, {}, "linux",
                    also_writable={"FAUNA_STORE": "/store", "FAUNA_KACHE_STATE": "/w/target"})[2].splitlines()
    assert lines[3] == "mkdir -p -- /w/target /store", lines
    # the resolved target is tagged outside: an untagged target is re-created by cargo
    # through a temporary sibling in its parent, which the read-only root refuses
    assert lines[4] == cb.tag_line("/w/target") and cb.CACHEDIR_TAG.split("\n")[0] in lines[4]
    assert lines[5] == "exec " + shlex.join([*cb.boundary_prefix("linux", writable=["/w/target", "/store"]),
                                             "cargo", "build"]), lines
    # a caller naming the set whole: nothing resolved
    monkeypatch.setattr(cb, "writable_set", lambda cmd, env=None: pytest.fail("resolved"))
    lines = cb.argv(["cargo", "build"], None, {}, "linux", writable=["/x"])[2].splitlines()
    assert cb.bwrap_line(["/x"])[-6:] == ["--bind", "/x", "/x", "--remount-ro", "/", "--"]
    assert shlex.join(cb.bwrap_line(["/x"])) in lines[-1]


def test_darwin_maps_the_set_to_its_roles_with_a_scratch_under_the_target(monkeypatch):
    """Decision (7) on darwin: the resolved target is `FAUNA_TARGET`, a fresh
    scratch under it `FAUNA_SCRATCH` and `TMPDIR`, the caller's store and state
    their roles; every path made and the target tagged first; the scratch
    removed when the command ends, so the inside half is not exec'd."""
    monkeypatch.setattr(cb, "inside", lambda env: False)
    monkeypatch.setattr(cb, "writable_set", lambda cmd, env=None: ["/w/target"])
    monkeypatch.setattr(cb.secrets, "token_hex", lambda n: "abc")
    also = {"FAUNA_STORE": "/store", "FAUNA_KACHE_STATE": "/state"}
    lines = cb.argv(["cargo", "build"], None, {}, "darwin", "/p.sb", also_writable=also)[2].splitlines()
    scratch = "/w/target/.fauna-scratch/abc"
    roles = {**also, "FAUNA_TARGET": "/w/target", "FAUNA_SCRATCH": scratch}
    assert lines[3] == shlex.join(["mkdir", "-p", "--", "/w/target", "/store", "/state", scratch]), lines
    assert lines[4] == cb.tag_line("/w/target")
    assert lines[5] == "trap " + shlex.quote(f"rm -rf -- {scratch}") + " EXIT"
    assert lines[6] == shlex.join([*_mac("/p.sb", roles), "cargo", "build"]), lines
    # an outer half: the scratch is made before each inside half and goes before
    # the program runs outside, and again when the command ends
    lines = cb.argv(["cargo", "run"], None, {}, "darwin", "/p.sb")[2].splitlines()
    lines = lines[lines.index('stale="$(mktemp "${TMPDIR:-/tmp}/cargo-boundary-stale.XXXXXX")"'):]
    assert lines[1] == "trap " + shlex.quote(f'rm -rf -- {scratch}; rm -f -- "$stale"') + " EXIT", lines
    assert lines[3:6] == [f"    mkdir -p -- {scratch}",
                           "    " + shlex.join([*_mac("/p.sb", {"FAUNA_TARGET": "/w/target",
                                                                "FAUNA_SCRATCH": scratch}), "cargo", "build"]),
                           f"    rm -rf -- {scratch}"], lines
    assert "RUSTC_WRAPPER=" in lines[7] and lines[7].endswith(" cargo run || rc=$?"), lines
    # `fix`: the workspace root is the workspace role
    monkeypatch.setattr(cb, "writable_set", lambda cmd, env=None: ["/w/target", "/w"])
    line = cb.argv(["cargo", "fix"], None, {}, "darwin", "/p.sb")[2]
    assert "-D FAUNA_WORKSPACE=/w " in line and f"-D FAUNA_STORE={_UNUSED} " in line, line
    # a set with no role for a path is refused, never widened
    with pytest.raises(cb.BoundaryRefused, match="no role"):
        cb.argv(["cargo", "build"], None, {}, "darwin", writable=["/a", "/b", "/c"])


def test_the_trees_darwin_profile_denies_writes_but_its_roles_and_named_devices():
    """The profile's text (decisions (1) and (7), part (c)): the network an
    allowlist of the store's unix sockets alone, the mDNSResponder socket denied after the
    unix allow, socket creation an allowlist of AF_UNIX, every mach lookup refused
    but the kept literal names with the nsurlsessiond belt after them and the
    preference write refused (`_MAC_MACH_RULES`), the write deny, one
    allow per role, and the measured device writes by name — never a parent
    directory."""
    text = (_HERE / "kache-mac.sb").read_text()
    rules = [ln.strip() for ln in text.splitlines() if ln.strip() and not ln.strip().startswith(";")]
    assert rules[:2] == ["(version 1)", "(allow default)"], rules
    assert rules[2:2 + len(_MAC_NETWORK_RULES)] == list(_MAC_NETWORK_RULES), rules
    at = 2 + len(_MAC_NETWORK_RULES)
    assert rules[at:at + len(_MAC_MACH_RULES)] == list(_MAC_MACH_RULES), rules
    assert rules[at + len(_MAC_MACH_RULES)] == "(deny file-write*)", rules
    # literal names only: no pattern, no prefix, no local name, no second mach rule
    assert sum("mach-lookup" in r for r in rules) == 3, rules
    assert "-regex" not in text and "local-name" not in text and "global-name-prefix" not in text
    allowed = re.findall(r'\(subpath \(param "([A-Z_]+)"\)\)', text.split("(deny file-write*)", 1)[1])
    assert tuple(allowed) == _MAC_ROLES, allowed
    assert re.findall(r'\(literal "([^"]+)"\)', text.split("(deny file-write*)", 1)[1]) == [
        "/dev/null", "/dev/dtracehelper"]
    assert '(subpath "' not in text, "a literal subpath would widen past a role"
    # the jobserver: data into an existing FIFO, the profile's last rule, and the
    # one write granted by vnode type — never a file-write* by type
    assert rules[-1] == "(allow file-write-data (vnode-type FIFO))", rules
    assert len(re.findall(r"\(vnode-type ", text)) == 1, text


def _meta(target="/m/target", root="/m"):
    calls = []

    def run(argv_):
        calls.append(argv_)
        return 0, json.dumps({"target_directory": target, "workspace_root": root}), ""
    return run, calls


@pytest.mark.parametrize("cmd,want,asks", [
    ("cargo build -p x", ["/m/target"], True),
    ("cargo +nightly --locked check --manifest-path a/Cargo.toml", ["/m/target"], True),
    ("cargo build --target-dir /abs/t", ["/abs/t"], False),
    ("cargo build --target-dir=/abs/t2 -p x", ["/abs/t2"], False),
    ("cargo fix --allow-dirty -p x", ["/m/target", "/m"], True),
    ("cargo clippy --fix -p x", ["/m/target", "/m"], True),
    ("cargo clippy -p x -- --fix", ["/m/target"], True),
    ("cargo fix --target-dir /abs/t", ["/abs/t", "/m"], True),
])
def test_the_writable_set_is_the_target_cargo_resolves_and_the_workspace_for_fix(cmd, want, asks):
    run, calls = _meta()
    assert cb.writable_set(cmd.split(), {}, run) == want
    assert bool(calls) == asks
    if calls:
        assert calls[0][-3:] == ["--no-deps", "--format-version", "1"] or "--manifest-path" in calls[0]


def test_the_metadata_call_keeps_the_toolchain_globals_and_manifest():
    assert cb.metadata_argv("cargo +nightly --locked check --manifest-path a/Cargo.toml -p x".split()) == \
        "cargo +nightly --locked metadata --no-deps --format-version 1 --manifest-path a/Cargo.toml".split()


@pytest.mark.parametrize("answer", [(101, "", "error: could not find `Cargo.toml`"), (0, "{}", ""),
                                    (0, "not json", "")])
def test_a_target_cargo_cannot_name_is_refused_never_guessed(answer):
    with pytest.raises(cb.BoundaryRefused, match="could not name the target directory"):
        cb.writable_set(["cargo", "build"], {}, lambda a: answer)


# --- decision (7) end to end: a build script's writes, and the mutation ------------

_PROBE_BUILD_RS = r'''
use std::io::Write;
fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let targets = format!("{};out_dir={}/boundary-probe", std::env::var("BOUNDARY_PROBE_TARGETS").unwrap(), out);
    let mut report = String::new();
    for item in targets.split(';').filter(|s| !s.is_empty()) {
        let (label, path) = item.split_once('=').unwrap();
        let r = std::fs::OpenOptions::new().create(true).append(true).open(path)
            .and_then(|mut f| f.write_all(b"planted\n"));
        report.push_str(&format!("{label} {}\n", if r.is_ok() { "OK" } else { "REFUSED" }));
    }
    let set = if std::env::var_os("GITHUB_ENV").is_some() { "SET" } else { "UNSET" };
    report.push_str(&format!("GITHUB_ENV-variable {set}\n"));
    // decision (1): a socket of a family the namespace does not contain
    extern "C" { fn socket(domain: i32, ty: i32, protocol: i32) -> i32; fn close(fd: i32) -> i32; }
    let vsock = unsafe { socket(40, 1, 0) };
    let answer = if vsock >= 0 { unsafe { close(vsock) }; "OK".to_owned() }
                 else { std::io::Error::last_os_error().raw_os_error().unwrap_or(-1).to_string() };
    report.push_str(&format!("vsock-socket {answer}\n"));
    std::fs::write(format!("{out}/boundary-probe-report.txt"), report).unwrap();
}
'''


#: The darwin profile's network rules, in order (decisions (1) and (8), darwin): every
#: outbound connection refused but to a unix socket under the store, mDNSResponder's refused after
#: the allow (the last matching rule wins), socket creation refused but for AF_UNIX.
_MAC_NETWORK_RULES = ("(deny network-outbound)",
                      '(allow network-outbound (remote unix-socket (subpath (param "FAUNA_STORE"))))',
                      '(deny network-outbound (literal "/private/var/run/mDNSResponder"))',
                      "(deny system-socket)", "(allow system-socket (socket-domain AF_UNIX))")
#: The rules they replaced — IP and the resolver denied by name, every other family
#: open: the mutation the darwin socket-family pins read the hole through.
_MAC_OLD_NETWORK_RULES = ("(deny network-outbound (remote ip))",
                          '(deny network-outbound (literal "/private/var/run/mDNSResponder"))')
#: The unix allow it narrowed (decision (8)): every unix socket reachable — the
#: mutation the darwin reach pin reads the hole through.
_MAC_OLD_UNIX_ALLOW = "(allow network-outbound (remote unix-socket))"
#: The literal global names a bounded process may look up (decision (1), the darwin
#: hole): every other `mach-lookup` is refused. The reason for each is in the profile.
_MAC_MACH_KEPT = ("com.apple.logd", "com.apple.system.notification_center")
#: The names a compile's tools were measured asking for that the deny-run dropped
#: (refused, nothing fails); the lookup pin reads the first refused.
_MAC_MACH_DROPPED = ("com.apple.cfprefsd.agent", "com.apple.system.opendirectoryd.libinfo",
                     "com.apple.system.opendirectoryd.membership", "com.apple.cfprefsd.daemon",
                     "com.apple.distributed_notifications@Uv3", "com.apple.SystemConfiguration.configd",
                     "com.apple.CoreServices.coreservicesd", "com.apple.metadata.mds", "com.apple.FSEvents",
                     "com.apple.lsd.mapdb", "com.apple.analyticsd", "com.apple.security.syspolicy")
#: The profile's mach rules, in order, as the file's lines read stripped: the bare
#: deny, the allow of the kept names, the `nsurlsessiond` belt after it (the last
#: matching rule wins), and cfprefsd's write on a caller's behalf refused.
_MAC_MACH_RULES = ("(deny mach-lookup)", "(allow mach-lookup",
                   *[f'(global-name "{n}")' for n in _MAC_MACH_KEPT[:-1]],
                   f'(global-name "{_MAC_MACH_KEPT[-1]}"))',
                   '(deny mach-lookup (global-name "com.apple.nsurlsessiond"))',
                   "(deny user-preference-write)")
#: The kernel's answer to a sandboxed `bootstrap_look_up` it refuses
#: (BOOTSTRAP_NOT_PRIVILEGED, measured 2026-11-06), as against an unknown name's 1102.
_BOOTSTRAP_REFUSED = 1100


def _mac_mach_block() -> str:
    return "\n".join(ln if ln.startswith("(deny") or ln.startswith("(allow") else "  " + ln
                     for ln in _MAC_MACH_RULES) + "\n"


def _mac_profile_text(mutate: str | None = None) -> str:
    """The tree's darwin profile, mutated: `"root"` — its `(deny file-write*)`
    line removed; `"network"` — its network rules turned back into the old
    `(remote ip)` denies; `"system-socket"` — its two socket-creation rules
    removed, the outbound allowlist left alone; `"unix"` — its unix allow
    widened back to every unix socket, `_MAC_OLD_UNIX_ALLOW`; `"mach"` — the whole
    mach block removed (`_MAC_MACH_RULES`, the preference-write deny with it):
    the profile as the darwin hole was found, every lookup open."""
    text = (_HERE / "kache-mac.sb").read_text()
    block = "\n".join(_MAC_NETWORK_RULES) + "\n"
    assert block in text
    if mutate == "mach":
        assert _mac_mach_block() in text
        return text.replace(_mac_mach_block(), "")
    if mutate == "root":
        assert "\n(deny file-write*)\n" in text
        return text.replace("\n(deny file-write*)\n", "\n")
    if mutate == "network":
        return text.replace(block, "\n".join(_MAC_OLD_NETWORK_RULES) + "\n")
    if mutate == "system-socket":
        return text.replace(block, "\n".join(_MAC_NETWORK_RULES[:3]) + "\n")
    if mutate == "unix":
        return text.replace(block, "\n".join((_MAC_NETWORK_RULES[0], _MAC_OLD_UNIX_ALLOW,
                                               *_MAC_NETWORK_RULES[2:])) + "\n")
    assert mutate is None, mutate
    return text


def _toolchain() -> str:
    for ln in (_HERE.parent / "rust-toolchain.toml").read_text().splitlines():
        if ln.startswith("channel"):
            return ln.split("=", 1)[1].strip().strip('"')
    raise AssertionError("no channel in rust-toolchain.toml")


#: The directories the Linux boundary mounts an empty tmpfs over: a crate under
#: one is invisible to the compile inside (pytest's `tmp_path` is under /tmp on a
#: hosted runner, where the 0.1.6 sync PR's shipped-suites job found exactly that).
_MASKED_INSIDE = ("/tmp", "/run", "/dev/shm")


def _masked(path: str) -> bool:
    real = os.path.realpath(path)
    return any(real == m or real.startswith(m + "/") for m in _MASKED_INSIDE)


def _visible_inside(tmp_path: Path) -> Path:
    """`tmp_path`, or — when the boundary would mask it — a fresh directory the
    compile inside reads like any path: under the runner's `RUNNER_TEMP`, else
    the user's cache. Never under the checkout, whose `.cargo` config would hand
    the probe the checkout's own target."""
    if not _masked(str(tmp_path)):
        return tmp_path
    runner_temp = os.environ.get("RUNNER_TEMP")
    if runner_temp and not _masked(runner_temp):
        base = Path(runner_temp) / "boundary-probe"
    else:
        base = Path.home() / ".cache" / "fauna-boundary-probe"
    base.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(dir=base))


@pytest.fixture
def probe_root(tmp_path):
    """The Linux probe's base: `tmp_path` where the boundary leaves it visible,
    else a directory `_visible_inside` makes, removed after the test."""
    base = _visible_inside(tmp_path)
    yield base
    if base != tmp_path:
        shutil.rmtree(base, ignore_errors=True)


def _probe_compile(tmp_path: Path, name: str, mutate: str | None, plat: str = "linux") -> dict[str, str]:
    """The ruling's probe, lifted: a scratch crate whose build script appends to
    the cargo proxy's directory, the registry, the checkout, `$GITHUB_ENV`,
    `$GITHUB_PATH`, a sibling directory and its own OUT_DIR, and opens a vsock
    socket, compiled through the helper's line on `plat` — `mutate` `"root"`:
    on linux with the empty root turned into `--bind / /` and every read bind
    writable, on darwin
    under a copy of the tree's profile with its `(deny file-write*)` line
    removed; `"seccomp"` (linux): the line's `--seccomp 9` dropped;
    `"network"` (darwin): the profile's network rules turned back into the old
    `(remote ip)` denies (`_mac_profile_text`). Returns
    each write's reading, the variable's and the socket's (`OK`, or the errno)."""
    root = tmp_path / name
    home = root / "cargo-home"
    crate = root / "crate"
    # the target exists EMPTY, as a fresh checkout's target dataset does: cargo would re-create
    # an untagged one through a sibling in the read-only parent
    for d in (home / "bin", home / "registry", crate / "src", root / "sibling", root / "target"):
        d.mkdir(parents=True)
    (crate / "Cargo.toml").write_text('[package]\nname = "boundary-probe"\nversion = "0.0.0"\n'
                                      'edition = "2021"\n[workspace]\n')
    (crate / "build.rs").write_text(_PROBE_BUILD_RS)
    (crate / "src" / "lib.rs").write_text("")
    gh_env, gh_path = root / "github_env", root / "github_path"
    gh_env.write_text("")
    gh_path.write_text("")
    targets = {"cargo_bin": home / "bin" / "boundary-probe", "registry": home / "registry" / "boundary-probe",
               "checkout": crate / "boundary-probe", "github_env": gh_env, "github_path": gh_path,
               "sibling": root / "sibling" / "boundary-probe"}
    # the real cargo, never a cargo shim a grant put first on PATH (it would hand the
    # compile to another front): the shim's directories leave PATH, its variables go.
    # BASH_ENV goes too, as on a runner: the bounded line is a `bash -c`, which sources
    # it first, and a dev shell's (or the public-CI replay's wrapper) re-exports its own
    # CARGO_TARGET_DIR over the probe's — the 0.1.6 grade's replay compiled into its
    # read-only public-ci target that way.
    env = {k: v for k, v in os.environ.items()
           if k not in ("FAUNA_KACHE_INSIDE", "RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER",
                        "FAUNA_CARGO_FRONT", "FAUNA_CARGO_FRONT_PYTHON", "BASH_ENV")}
    env["PATH"] = os.pathsep.join(p for p in env.get("PATH", "").split(os.pathsep)
                                  if os.path.basename(p.rstrip("/")) != "cargo-shim")
    env.update({"CARGO_HOME": str(home), "CARGO_TARGET_DIR": str(root / "target"),
                "RUSTUP_TOOLCHAIN": _toolchain(), "GITHUB_ENV": str(gh_env), "GITHUB_PATH": str(gh_path),
                cb.RECORD_ENV: str(root / "record"),
                "BOUNDARY_PROBE_TARGETS": ";".join(f"{k}={v}" for k, v in targets.items())})
    cargo = shutil.which("cargo", path=env.get("PATH"))
    cmd = [cargo, "build", "--manifest-path", str(crate / "Cargo.toml")]
    profile = None
    if plat == "darwin":
        assert mutate in (None, "root", "network"), mutate
        profile = root / "kache-mac.sb"
        profile.write_text(_mac_profile_text(mutate))
        profile = str(profile)
    bounded = cb.argv(cmd, "runner", env, plat, profile)
    assert bounded is not None
    if mutate == "root" and plat == "linux":
        # the empty root made the host's root, writable: the read-only remount goes
        # too, and the read list's binds turn writable (the cargo home and the
        # checkout are on it)
        assert " --tmpfs / " in bounded[2] and " --remount-ro / -- " in bounded[2]
        line = (bounded[2].replace(" --tmpfs / ", " --bind / / ", 1).replace(" --remount-ro / -- ", " -- ", 1)
                .replace(" --ro-bind-try ", " --bind-try "))
        bounded = [*bounded[:2], line, *bounded[3:]]
    if mutate == "seccomp":
        assert plat == "linux" and f" --seccomp {cb.SECCOMP_FD} " in bounded[2]
        bounded = [*bounded[:2], bounded[2].replace(f" --seccomp {cb.SECCOMP_FD} ", " "), *bounded[3:]]
    got = subprocess.run(bounded, env=env, cwd=crate, capture_output=True, text=True, timeout=900)
    assert got.returncode == 0, got.stderr[-3000:]
    assert (root / "record").read_text().splitlines() == [shlex.join(cmd)]
    # the build directory's layout moves between toolchains (`boundary-probe-<hash>/out`,
    # `boundary-probe/<hash>/out`): the report's own name is what is searched for
    reports = list((root / "target" / "debug" / "build").rglob("boundary-probe-report.txt"))
    assert len(reports) == 1, reports
    return dict(ln.split(" ", 1) for ln in reports[0].read_text().splitlines())


_NEEDS_REAL_COMPILE = pytest.mark.skipif(
    not _bwrap_works() or shutil.which("cargo") is None or cb.inside(os.environ),
    reason="needs a working /usr/bin/bwrap and cargo, outside any boundary (Linux)")


@_NEEDS_REAL_COMPILE
def test_a_build_script_writes_only_its_target_and_the_mutation_reads_the_hole(probe_root):
    """Decision (7)'s pin: under the helper's line every write outside the
    target is refused, OUT_DIR's lands, and the file-command variable is gone;
    the same run with the bind mutated back to `--bind / /` reads the writes OK
    — so this test is known to see the hole it guards."""
    got = _probe_compile(probe_root, "bounded", mutate=None)
    assert got == {"cargo_bin": "REFUSED", "registry": "REFUSED", "checkout": "REFUSED",
                   "github_env": "REFUSED", "github_path": "REFUSED", "sibling": "REFUSED",
                   "out_dir": "OK", "GITHUB_ENV-variable": "UNSET", "vsock-socket": "97"}, got
    hole = _probe_compile(probe_root, "mutated", mutate="root")
    assert all(hole[k] == "OK" for k in ("cargo_bin", "registry", "checkout", "github_path", "sibling",
                                         "out_dir")), hole
    assert hole["vsock-socket"] == "97", "the root's mutation never opens the socket family"
    assert (probe_root / "mutated" / "cargo-home" / "bin" / "boundary-probe").read_text() == "planted\n"


@_NEEDS_REAL_COMPILE
def test_a_build_scripts_vsock_socket_is_refused_and_the_filters_mutation_reads_the_hole(probe_root):
    """Decision (1)'s pin on a real compile: a build script's `socket(AF_VSOCK)`
    answers EAFNOSUPPORT (97) under the helper's line, and the same compile with
    the line's `--seccomp 9` dropped gets this host's own answer — on the primary dev VM an
    open socket to the hypervisor (`OK`) — so this test is known to see the
    hole it guards."""
    import socket
    try:
        socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM).close()
        host = "OK"
    except OSError as e:
        host = str(e.errno)
    got = _probe_compile(probe_root, "bounded", mutate=None)
    assert got["vsock-socket"] == "97", got
    hole = _probe_compile(probe_root, "mutated", mutate="seccomp")
    assert hole["vsock-socket"] == host, (hole, host)
    assert hole["out_dir"] == "OK" and hole["cargo_bin"] == "REFUSED", hole


_NEEDS_DARWIN_COMPILE = pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or shutil.which("cargo") is None
    or cb.inside(os.environ),
    reason="needs /usr/bin/sandbox-exec and cargo, outside any boundary (macOS)")


@_NEEDS_DARWIN_COMPILE
def test_a_build_script_writes_only_its_target_on_darwin_and_the_mutation_reads_the_hole(tmp_path):
    """Decision (7)'s darwin pin: under the tree's profile with the helper's role
    parameters every write outside the target is refused, OUT_DIR's lands, the
    file-command variable is gone and the scratch is removed after; the same
    run under the profile with its `(deny file-write*)` line removed reads the
    writes OK — so this test is known to see the hole it guards."""
    got = _probe_compile(tmp_path, "bounded", mutate=None, plat="darwin")
    assert got == {"cargo_bin": "REFUSED", "registry": "REFUSED", "checkout": "REFUSED",
                   "github_env": "REFUSED", "github_path": "REFUSED", "sibling": "REFUSED",
                   "out_dir": "OK", "GITHUB_ENV-variable": "UNSET", "vsock-socket": "1"}, got
    assert not list((tmp_path / "bounded" / "target" / cb.MAC_SCRATCH_DIR).iterdir()), \
        "the scratch outlived its compile"
    hole = _probe_compile(tmp_path, "mutated", mutate="root", plat="darwin")
    assert all(hole[k] == "OK" for k in ("cargo_bin", "registry", "checkout", "github_path", "sibling",
                                         "out_dir")), hole
    assert hole["vsock-socket"] == "1", "the write deny's mutation never opens the socket family"
    assert (tmp_path / "mutated" / "cargo-home" / "bin" / "boundary-probe").read_text() == "planted\n"


def _host_vsock_socket() -> str:
    """This host's own answer to `socket(AF_VSOCK, SOCK_STREAM)` (family 40 on
    both kernels): `OK`, or the errno."""
    import socket
    try:
        socket.socket(40, socket.SOCK_STREAM).close()
        return "OK"
    except OSError as e:
        return str(e.errno)


@_NEEDS_DARWIN_COMPILE
def test_a_build_scripts_vsock_socket_is_refused_on_darwin_and_the_network_mutation_reads_the_hole(tmp_path):
    """Decision (1)'s darwin pin on a real compile: a build script's
    `socket(AF_VSOCK)` answers EPERM (1) under the tree's profile, and the same
    compile under the profile with its network rules turned back into the old
    `(remote ip)` denies gets this host's own answer — on a macOS the virtualization host
    guest, an open socket (`OK`) — so this test is known to see the hole it
    guards. (A host with no vsock family answers its own errno both ways; the
    families pin below reads the same hole through the connect.)"""
    host = _host_vsock_socket()
    got = _probe_compile(tmp_path, "bounded", mutate=None, plat="darwin")
    assert got["vsock-socket"] == "1", got
    hole = _probe_compile(tmp_path, "mutated", mutate="network", plat="darwin")
    assert hole["vsock-socket"] == host, (hole, host)
    assert hole["out_dir"] == "OK" and hole["cargo_bin"] == "REFUSED", hole


_MAC_FAMILIES_PROBE = r'''
import ctypes, errno, json, socket, struct
res = {}
libc = ctypes.CDLL(None, use_errno=True)
def attempt(name, fn):
    try:
        fn(); res[name] = "OK"
    except OSError as e:
        res[name] = errno.errorcode.get(e.errno, str(e.errno))
def make(fam, ty):
    return lambda: socket.socket(fam, ty).close()
def vsock_connect():
    s = socket.socket(40, socket.SOCK_STREAM)
    # struct sockaddr_vm: len, family, reserved, port, cid — the host is CID 2
    addr = struct.pack("BBHII", 12, 40, 0, 22, 2)
    r = libc.connect(s.fileno(), addr, len(addr))
    e = ctypes.get_errno()
    s.close()
    if r != 0:
        raise OSError(e, "connect")
def resolve():
    try:
        socket.getaddrinfo("example.com", 443)
    except socket.gaierror:
        raise OSError(errno.ENOENT, "unresolved")
def tcp():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(5)
    try:
        s.connect(("192.0.2.1", 443))
    finally:
        s.close()
attempt("vsock", make(40, socket.SOCK_STREAM))
attempt("vsock_connect", vsock_connect)
attempt("route", make(socket.AF_ROUTE, socket.SOCK_RAW))
attempt("system", make(32, socket.SOCK_DGRAM))
for name, fam in (("unix", socket.AF_UNIX), ("inet", socket.AF_INET), ("inet6", socket.AF_INET6)):
    attempt(name, make(fam, socket.SOCK_STREAM))
attempt("socketpair", lambda: [s.close() for s in socket.socketpair()])
attempt("resolve", resolve)
attempt("tcp_connect", tcp)
print(json.dumps(res))
'''


@pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or cb.inside(os.environ),
    reason="needs /usr/bin/sandbox-exec, outside any boundary (macOS)")
def test_a_socket_family_but_unix_is_refused_inside_the_darwin_profile_and_each_mutation_reads_its_hole(tmp_path):
    """Decision (1)'s darwin pin, layer by layer: under the tree's profile a
    vsock, route or system socket is refused at creation (EPERM), every
    connect but a unix one is refused (a vsock connect to the host, an IPv4
    connect) and no name resolves, while unix, inet and inet6 sockets and a
    socketpair still open. With the socket-creation rules removed the vsock
    socket opens and its connect is still refused — the outbound allowlist
    holds alone; with the network rules turned back into the old `(remote ip)`
    denies the vsock answers are the host's own (on a the virtualization host guest a socket whose connect
    reaches the hypervisor's transport) — so this test is known to see the
    hole it guards."""
    probe = tmp_path / "families-probe.py"
    probe.write_text(_MAC_FAMILIES_PROBE)
    outside = _probe_json([sys.executable, str(probe)])

    def under(mutate):
        profile = tmp_path / f"kache-mac-{mutate or 'tree'}.sb"
        profile.write_text(_mac_profile_text(mutate))
        return _probe_json([*cb.boundary_prefix("darwin", str(profile)), sys.executable, str(probe)])
    inside = under(None)
    assert {k: inside[k] for k in ("vsock", "vsock_connect", "route", "system", "resolve", "tcp_connect")} == {
        "vsock": "EPERM", "vsock_connect": "EPERM", "route": "EPERM", "system": "EPERM",
        "resolve": "ENOENT", "tcp_connect": "EPERM"}, inside
    assert {k: inside[k] for k in ("unix", "inet", "inet6", "socketpair")} == {
        "unix": "OK", "inet": "OK", "inet6": "OK", "socketpair": "OK"}, inside
    alone = under("system-socket")
    assert {k: alone[k] for k in ("vsock", "route", "system")} == {
        k: outside[k] for k in ("vsock", "route", "system")}, (alone, outside)
    if alone["vsock"] == "OK":
        assert alone["vsock_connect"] == "EPERM", alone
    assert alone["resolve"] == "ENOENT" and alone["tcp_connect"] == "EPERM", alone
    hole = under("network")
    for k in ("vsock", "vsock_connect", "route", "system"):
        assert hole[k] == outside[k], (k, hole, outside)
    assert any(hole[k] != inside[k] for k in ("vsock", "vsock_connect")), (hole, inside)
    assert hole["resolve"] == "ENOENT" and hole["tcp_connect"] == "EPERM", hole


@pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or cb.inside(os.environ),
    reason="needs /usr/bin/sandbox-exec, outside any boundary (macOS)")
def test_a_profile_reinstalled_mid_build_leaves_the_nested_apply_on_the_compiles_own_snapshot(tmp_path):
    """The kernel's half of the snapshot: a compile bounded under the installed
    profile's snapshot, with the profile then reinstalled (a new snapshot written,
    the copy swapped — `just kache-launcher-install`, as a session ran it at
    01:01:24 on 2026-11-06 while the mac gate check's `apple-swift-build-check`
    compiled). Inside, re-applying the swapped copy is refused (`sandbox_apply:
    Operation not permitted` — the red's three units) and re-applying the snapshot
    the boundary named in `PROFILE_VAR` — what the launcher's tail takes inside —
    is not."""
    import hashlib

    def install(text):
        snaps = tmp_path / cb.MAC_PROFILE_SNAPSHOTS
        snaps.mkdir(exist_ok=True)
        (snaps / f"kache-mac-{hashlib.sha256(text.encode()).hexdigest()}.sb").write_text(text)
        (tmp_path / cb.MAC_PROFILE_NAME).write_text(text)
    beside = tmp_path / cb.MAC_PROFILE_NAME
    install(_mac_profile_text())
    prefix = cb.boundary_prefix("darwin", cb.mac_profile_snapshot(str(beside)))
    # a rule change the kernel sees (a comment, or a read the profile already allows,
    # compiles to the same profile and nests — measured): here a narrower one, which is
    # refused all the same
    install(_mac_profile_text() + '(deny file-read* (literal "/nonexistent-reinstalled-rule"))\n')
    params = prefix[3:prefix.index("/usr/bin/env")]
    rest = shlex.join([*params, "/usr/bin/true"])
    script = (f'/usr/bin/sandbox-exec -f "${cb.PROFILE_VAR}" {rest}; echo "snapshot=$?"\n'
              f'/usr/bin/sandbox-exec -f {shlex.quote(str(beside))} {rest}; echo "swapped=$?"\n')
    got = subprocess.run([*prefix, "/bin/bash", "-c", script], capture_output=True, text=True, timeout=60)
    assert got.returncode == 0, (got.returncode, got.stderr)
    lines = got.stdout.split()
    assert lines[0] == "snapshot=0" and lines[1] != "swapped=0", (got.stdout, got.stderr)
    assert "sandbox_apply: Operation not permitted" in got.stderr, got.stderr


_MAC_CONNECT_PROBE = r'''
import errno, json, socket, sys
res = {}
pairs = iter(sys.argv[1:])
for name, path in zip(pairs, pairs):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(path); res[name] = "OK"
    except OSError as e:
        res[name] = errno.errorcode.get(e.errno, str(e.errno))
    finally:
        s.close()
print(json.dumps(res))
'''


@pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or cb.inside(os.environ),
    reason="needs /usr/bin/sandbox-exec, outside any boundary (macOS)")
def test_a_unix_connect_outside_the_store_is_refused_inside_the_darwin_profile_and_the_mutation_reads_the_hole(
        tmp_path):
    """Decision (8)'s darwin pin: under the tree's profile with a store role, a
    connect to a listener at the session host's socket shape under a scratch
    `HOME` (`~/.cache/fauna/session-host/<session>.sock` — never a live
    host's) is refused (EPERM) while a listener under the store connects (the
    compile cache's `daemon.sock`); with the unix allow widened back to every
    unix socket the session-host-shaped connect succeeds — so this test is
    known to see the hole it guards."""
    import socket
    import tempfile
    probe = tmp_path / "connect-probe.py"
    probe.write_text(_MAC_CONNECT_PROBE)
    # AF_UNIX paths are short on darwin (104 bytes): a scratch root under /private/tmp
    root = Path(tempfile.mkdtemp(dir="/private/tmp", prefix="cb8-"))
    host_dir = root / "home" / ".cache" / "fauna" / "session-host"
    store = root / "store"
    host_dir.mkdir(parents=True)
    store.mkdir()
    listeners, paths = [], {}
    try:
        for name, path in (("session_host", host_dir / "boundary-test.sock"), ("store", store / "daemon.sock")):
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.bind(str(path))
            s.listen(4)
            listeners.append(s)
            paths[name] = str(path)

        def under(mutate):
            profile = tmp_path / f"kache-mac-{mutate or 'tree'}.sb"
            profile.write_text(_mac_profile_text(mutate))
            return _probe_json([*cb.boundary_prefix("darwin", str(profile), roles={"FAUNA_STORE": str(store)}),
                                sys.executable, str(probe), *[w for kv in paths.items() for w in kv]])
        assert under(None) == {"session_host": "EPERM", "store": "OK"}
        assert under("unix") == {"session_host": "OK", "store": "OK"}
    finally:
        for s in listeners:
            s.close()
        shutil.rmtree(root, ignore_errors=True)


_MAC_LOOKUP_PROBE = r'''
import ctypes, json, sys
libc = ctypes.CDLL(None)
bootstrap = ctypes.c_uint.in_dll(libc, "bootstrap_port").value
task = ctypes.c_uint.in_dll(libc, "mach_task_self_").value
res = {}
for name in sys.argv[1:]:
    # a lookup alone: it hands back a send right and starts no job — no message is
    # ever sent, so nothing is launched and no dialog can be raised
    port = ctypes.c_uint(0)
    res[name] = libc.bootstrap_look_up(bootstrap, name.encode(), ctypes.byref(port))
    if res[name] == 0:
        libc.mach_port_deallocate(task, port)
print(json.dumps(res))
'''

#: The services a bounded lookup must never reach: the one measured transfer service,
#: and the launching half of LaunchServices (opening a URL or registering a handler
#: through an app outside the profile).
_MAC_MACH_NEVER = ("com.apple.nsurlsessiond", "com.apple.coreservices.launchservicesd", "com.apple.lsd.open")


@pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or cb.inside(os.environ),
    reason="needs /usr/bin/sandbox-exec, outside any boundary (macOS)")
def test_a_lookup_off_the_list_is_refused_inside_the_darwin_profile_and_the_mutation_reads_the_hole(tmp_path):
    """Decision (1)'s darwin hole, by lookup: under the tree's profile a kept
    name answers KERN_SUCCESS and the transfer service, the launching half of
    LaunchServices and every name the deny-run dropped answer the kernel's refusal
    (`_BOOTSTRAP_REFUSED`); with the mach block removed every one of them
    answers KERN_SUCCESS — so this test is known to see the hole it guards.
    `bootstrap_look_up` alone, never a message: nothing is launched."""
    probe = tmp_path / "lookup-probe.py"
    probe.write_text(_MAC_LOOKUP_PROBE)
    names = (*_MAC_MACH_KEPT, *_MAC_MACH_NEVER, *_MAC_MACH_DROPPED)

    def under(mutate):
        profile = tmp_path / f"kache-mac-{mutate or 'tree'}.sb"
        profile.write_text(_mac_profile_text(mutate))
        return _probe_json([*cb.boundary_prefix("darwin", str(profile)), sys.executable, str(probe), *names])
    assert under(None) == {n: 0 if n in _MAC_MACH_KEPT else _BOOTSTRAP_REFUSED for n in names}
    assert under("mach") == {n: 0 for n in names}


_MAC_URLSESSION_PROBE = r'''
// a background URLSession's data task: the transfer is nsurlsessiond's, outside
// the caller's sandbox — the darwin hole as it was found
import Foundation

let url = URL(string: CommandLine.arguments[1])!
let timeout = Double(CommandLine.arguments[2])!

final class D: NSObject, URLSessionDataDelegate {
    var result = "TIMEOUT"
    var data = Data()
    let done = DispatchSemaphore(value: 0)
    func urlSession(_ s: URLSession, dataTask: URLSessionDataTask, didReceive d: Data) {
        data.append(d)
        result = "BODY " + String(decoding: data, as: UTF8.self)
    }
    func urlSession(_ s: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let e = error as NSError? { result = "ERROR \(e.domain) \(e.code)" }
        done.signal()
    }
}

let d = D()
let cfg = URLSessionConfiguration.background(withIdentifier: "fauna-boundary-probe-\(getpid())")
cfg.timeoutIntervalForResource = timeout
let session = URLSession(configuration: cfg, delegate: d, delegateQueue: nil)
session.dataTask(with: url).resume()
print(d.done.wait(timeout: .now() + timeout + 5) == .timedOut ? "TIMEOUT" : d.result)
session.invalidateAndCancel()
'''


@pytest.mark.skipif(
    sys.platform != "darwin" or not os.path.exists("/usr/bin/sandbox-exec") or cb.inside(os.environ)
    or shutil.which("swiftc") is None,
    reason="needs /usr/bin/sandbox-exec and swiftc, outside any boundary (macOS)")
def test_a_background_urlsession_reaches_nothing_inside_the_darwin_profile_and_the_mutation_reads_the_hole(
        tmp_path):
    """Decision (1)'s darwin hole end to end, read the way it was found: a
    background `URLSession` data task from inside, against a loopback server
    this test owns outside the profile. Under the tree's profile the server
    logs no request and the probe reports no body; with the mach block removed
    the server logs the request and the body comes back in — `nsurlsessiond`
    carrying it both ways — so this test is known to see the hole it guards."""
    import http.server
    import threading
    binary = tmp_path / "urlsession-probe"
    (tmp_path / "urlsession-probe.swift").write_text(_MAC_URLSESSION_PROBE)
    subprocess.run(["swiftc", "-O", "-o", str(binary), str(tmp_path / "urlsession-probe.swift")],
                   check=True, capture_output=True, timeout=300)
    hits: list[str] = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            hits.append(self.path)
            body = b"boundary-probe-body"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        def under(mutate):
            profile = tmp_path / f"kache-mac-{mutate or 'tree'}.sb"
            profile.write_text(_mac_profile_text(mutate))
            path = f"/boundary-{mutate or 'tree'}"
            before = len(hits)
            out = subprocess.run([*cb.boundary_prefix("darwin", str(profile)), str(binary),
                                  f"http://127.0.0.1:{server.server_address[1]}{path}", "15"],
                                 capture_output=True, text=True, timeout=120).stdout.strip()
            return out, hits[before:]
        out, got = under(None)
        assert got == [] and not out.startswith("BODY"), (out, got)
        out, got = under("mach")
        assert got == ["/boundary-mach"] and out == "BODY boundary-probe-body", (out, got)
    finally:
        server.shutdown()


# --- the run/test split: the outside half never compiles -------------------------

#: A fake cargo for the split: logged as `_fake_cargo_dir`'s, and its outside
#: `test` half (no `--no-run`) "compiles" through RUSTC_WRAPPER — as a real cargo
#: does for a unit a mid-build edit made stale — the first FAKE_STALE_TIMES
#: times, else exits FAKE_TEST_EXIT (a test's own result).
_FAKE_SPLIT = ('if [ "$1" = test ] && [[ " $* " != *" --no-run "* ]]; then\n'
               '  n=$(grep -c "^CARGO test .* -- t$" "$FAKE_CARGO_LOG")\n'
               '  if [ "$n" -le "${FAKE_STALE_TIMES:-0}" ]; then\n'
               '    "$RUSTC_WRAPPER" rustc --crate-name fauna_nest --edition=2021 src/lib.rs || exit 101\n'
               '  fi\n'
               '  exit "${FAKE_TEST_EXIT:-0}"\n'
               'fi\n')


def _split_run(root: Path, **env) -> tuple[subprocess.CompletedProcess, list[str]]:
    bin_dir = root / "bin"
    bin_dir.mkdir()
    (bin_dir / "cargo").write_text('#!/bin/bash\n' + _FAKE_METADATA + 'echo "CARGO $*" >> "$FAKE_CARGO_LOG"\n'
                                   + _FAKE_SPLIT)
    (bin_dir / "cargo").chmod(0o755)
    log = root / "target" / "log"
    log.parent.mkdir()
    log.touch()
    got = _run("--", "cargo", "test", "-p", "x", "--", "t",
               env={"PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_CARGO_LOG": str(log),
                    "FAUNA_KACHE_INSIDE": "", **{k: str(v) for k, v in env.items()}})
    return got, log.read_text().splitlines()


_INSIDE, _OUTSIDE = "CARGO test -p x --no-run", "CARGO test -p x -- t"


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_a_unit_stale_outside_is_refused_and_compiled_again_inside(probe_root):
    """The fault of 2026-11-11 (`couldn't find file bins/fauna-nest/src/lib.rs`):
    the outside half compiling a unit a mid-build edit made stale. The refuser
    stops it with one line naming the unit, and the line runs the inside half
    again, then the outside half, green."""
    got, calls = _split_run(probe_root, FAKE_STALE_TIMES=1)
    assert got.returncode == 0, got.stderr
    assert calls == ["CARGO fetch", _INSIDE, _OUTSIDE, _INSIDE, _OUTSIDE], calls
    assert got.stderr.count("[cargo-boundary] fauna_nest went stale between the compile and the run") == 1, \
        got.stderr


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_a_test_failure_outside_is_the_lines_result_never_a_second_compile(probe_root):
    got, calls = _split_run(probe_root, FAKE_TEST_EXIT=101)
    assert got.returncode == 101, got.stderr
    assert calls == ["CARGO fetch", _INSIDE, _OUTSIDE], calls
    assert "went stale" not in got.stderr


@pytest.mark.skipif(not _bwrap_works(), reason="needs a working /usr/bin/bwrap (Linux)")
def test_sources_that_keep_changing_give_up_after_three_compiles(probe_root):
    got, calls = _split_run(probe_root, FAKE_STALE_TIMES=9)
    assert got.returncode != 0
    assert calls == ["CARGO fetch", *[_INSIDE, _OUTSIDE] * cb.STALE_ATTEMPTS], calls
    assert "a source changed during each of 3 compiles — giving up" in got.stderr, got.stderr


@pytest.mark.skipif(os.name != "posix", reason="a bash refuser")
def test_the_refuser_passes_the_probes_and_refuses_a_compile_into_the_marker(tmp_path):
    marker = tmp_path / "stale"
    env = {**os.environ, cb.STALE_ENV: str(marker)}
    for probe in (["-vV"], ["-", "--crate-name", "___", "--print=file-names"], ["--print", "sysroot"]):
        got = subprocess.run([cb.REFUSE_RUSTC, "/bin/echo", *probe], env=env, capture_output=True, text=True)
        assert got.returncode == 0 and got.stdout.split() == probe, (probe, got)
    assert not marker.exists()
    got = subprocess.run([cb.REFUSE_RUSTC, "/bin/echo", "--crate-name", "fauna_ffi_core", "src/lib.rs"],
                         env=env, capture_output=True, text=True)
    assert got.returncode != 0 and got.stdout == ""
    assert marker.read_text() == "fauna_ffi_core\n"
    assert got.stderr.startswith("[cargo-boundary] fauna_ffi_core went stale"), got.stderr


#: A cargo in front of the real one, for a real compile: its first outside
#: `test` half (no `--no-run`) edits the crate's source before handing over —
#: a source changed between the inside compile and the run.
_EDIT_BETWEEN_HALVES = ('#!/bin/bash\n'
                        'if [ "$1" = test ] && [[ " $* " != *" --no-run "* ]] && [ ! -e "$EDIT_DONE" ]; then\n'
                        '  touch "$EDIT_DONE"; sleep 1; echo "// edited between the halves" >> "$EDIT_SRC"\n'
                        'fi\n'
                        'exec "$REAL_CARGO" "$@"\n')


def _edited_between_halves(root: Path, mutate_loop: bool) -> subprocess.CompletedProcess:
    crate, bin_dir = root / "crate", root / "bin"
    for d in (crate / "src", bin_dir, root / "target"):
        d.mkdir(parents=True)
    (crate / "Cargo.toml").write_text('[package]\nname = "stale-probe"\nversion = "0.0.0"\n'
                                      'edition = "2021"\n[workspace]\n')
    (crate / "src" / "lib.rs").write_text("#[test]\nfn passes() { assert_eq!(1 + 1, 2); }\n")
    (bin_dir / "cargo").write_text(_EDIT_BETWEEN_HALVES)
    (bin_dir / "cargo").chmod(0o755)
    env = {k: v for k, v in os.environ.items()
           if k not in ("FAUNA_KACHE_INSIDE", "RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER",
                        "RUSTC_WORKSPACE_WRAPPER", "FAUNA_CARGO_FRONT", "FAUNA_CARGO_FRONT_PYTHON", "BASH_ENV")}
    env["PATH"] = os.pathsep.join(p for p in env.get("PATH", "").split(os.pathsep)
                                  if os.path.basename(p.rstrip("/")) != "cargo-shim")
    real = shutil.which("cargo", path=env["PATH"])
    env.update({"CARGO_TARGET_DIR": str(root / "target"), "RUSTUP_TOOLCHAIN": _toolchain(),
                "REAL_CARGO": real, "EDIT_DONE": str(root / "edited"), "EDIT_SRC": str(crate / "src" / "lib.rs")})
    cmd = [str(bin_dir / "cargo"), "test", "--manifest-path", str(crate / "Cargo.toml")]
    old_cwd = os.getcwd()
    os.chdir(crate)  # the read list's `.cargo` walk starts at the cwd: the crate's, as the run's
    try:
        bounded = cb.argv(cmd, None, env, "linux")
    finally:
        os.chdir(old_cwd)
    assert bounded is not None and "for attempt in 1 2 3; do" in bounded[2]
    if mutate_loop:
        bounded = [*bounded[:2], bounded[2].replace("for attempt in 1 2 3; do", "for attempt in 1; do"),
                   *bounded[3:]]
    return subprocess.run(bounded, env=env, cwd=crate, capture_output=True, text=True, timeout=900)


@_NEEDS_REAL_COMPILE
def test_a_source_edited_between_the_halves_compiles_again_inside_and_the_mutation_reads_the_fault(probe_root):
    """The row's pin, on a real compile: the outside
    half meets a stale unit, is refused, and the inside half compiles it again —
    the test's own result (green), never `couldn't find file`; with the loop cut
    to one pass the same run reds on the refusal — so this test is known to see
    the fault."""
    got = _edited_between_halves(probe_root / "looped", mutate_loop=False)
    assert got.returncode == 0, got.stderr[-3000:]
    assert "stale_probe went stale between the compile and the run" in got.stderr, got.stderr[-3000:]
    assert "test passes ... ok" in got.stdout, got.stdout[-3000:]
    cut = _edited_between_halves(probe_root / "cut", mutate_loop=True)
    assert cut.returncode != 0, cut.stdout[-2000:]
    assert "stale_probe went stale" in cut.stderr and "test passes ... ok" not in cut.stdout
