"""scripts/cargo-win.cmd sets up the MSVC environment for the host it runs on and the
target it is handed, either arch either way round.

The same wrapper runs on the arm64 dev box and on the x64 hosted release runner
(`docs/goal/architecture/release-integrity.md` § What a release ships → *Every
release build runs the `just` recipe*). Until the release legs went through the
recipes it assumed the arm64 box: `bin\\Hostarm64\\` tools, the target picked by
`CARGO_WIN_ARCH` alone. So two things are pinned here, through the wrapper's
`CARGO_WIN_DEBUG_TOOLCHAIN=1` report (it prints what it resolved and exits before
cargo runs, so nothing compiles):

1. The HOST is rustc's own host triple, never PROCESSOR_ARCHITECTURE, which an
   emulated parent rewrites (Windows's Git Bash is an x64 process on the arm64 box).
   A stub `rustc` under a scratch USERPROFILE stands in for the x64 runner's.
2. The TARGET is the command line's `--target` triple when it names a windows MSVC
   one, else CARGO_WIN_ARCH, else the host — `--target` winning, so a variable
   leaked from a caller cannot give a target build the other arch's libraries.
"""

import os
import re
import subprocess
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.tier_1,
    pytest.mark.skipif(os.name != "nt", reason="cargo-win.cmd is a cmd.exe script"),
]

_REPO = Path(__file__).resolve().parents[5]


def _resolve(tmp_path: Path, host: str | None, *args: str, arch: str = "") -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if k.upper() != "CARGO_WIN_ARCH"}
    env["CARGO_WIN_DEBUG_TOOLCHAIN"] = "1"
    if arch:
        env["CARGO_WIN_ARCH"] = arch
    if host is not None:
        stub = tmp_path / ".cargo" / "bin"
        stub.mkdir(parents=True, exist_ok=True)
        (stub / "rustc.cmd").write_text(f"@echo rustc 1.0.0\r\n@echo host: {host}\r\n", encoding="ascii")
        env["USERPROFILE"] = str(tmp_path)
    out = subprocess.run(["cmd", "/c", r"scripts\cargo-win.cmd", *args], cwd=_REPO, env=env,
                         capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=120)
    assert out.returncode == 0, out.stdout + out.stderr
    return dict(re.findall(r"^DEBUG (\w+)=(.*?)\s*$", out.stdout, re.M))


@pytest.mark.parametrize("host,arch", [("x86_64-pc-windows-msvc", "x64"), ("aarch64-pc-windows-msvc", "arm64")])
def test_the_host_is_rustcs_and_the_default_target(tmp_path, host, arch):
    got = _resolve(tmp_path, host, "tree")
    assert (got["HOST_ARCH"], got["TARGET_ARCH"]) == (arch, arch), got


@pytest.mark.parametrize("host,args,want", [
    ("x86_64-pc-windows-msvc", ("tree", "--target", "aarch64-pc-windows-msvc"), "arm64"),
    ("x86_64-pc-windows-msvc", ("tree", "--target=aarch64-pc-windows-msvc"), "arm64"),
    ("aarch64-pc-windows-msvc", ("tree", "--locked", "--target", "x86_64-pc-windows-msvc", "-p", "x"), "x64"),
    ("aarch64-pc-windows-msvc", ("tree", "--features", "store-safe,p2p-share", "--target", "x86_64-pc-windows-msvc"), "x64"),
])
def test_the_target_is_the_command_lines_triple(tmp_path, host, args, want):
    assert _resolve(tmp_path, host, *args)["TARGET_ARCH"] == want


def test_the_command_line_wins_over_a_leaked_variable(tmp_path):
    got = _resolve(tmp_path, "aarch64-pc-windows-msvc", "tree", "--target", "aarch64-pc-windows-msvc", arch="x64")
    assert got["TARGET_ARCH"] == "arm64", got
    got = _resolve(tmp_path, "aarch64-pc-windows-msvc", "tree", arch="x64")
    assert got["TARGET_ARCH"] == "x64", "with no --target the variable still selects the target"


@pytest.mark.parametrize("recipe", ["line", "body"])
def test_the_justfiles_cargo_reaches_the_wrapper(tmp_path, recipe):
    """`{{cargo}}` — the cargo a recipe line that builds on every OS runs — reaches
    cmd as the wrapper's path from a linewise recipe and from a `#!` body alike. A
    linewise recipe reaches Git's sh through the MSYS runtime's parse of its Windows
    command line, which takes a doubled backslash to one first: the first win run of
    `tui-release` under `cmd //c scripts\\\\cargo-win.cmd` died on
    `'scriptscargo-win.cmd' is not recognized` (2026-10-28), while the same value in
    a `#!` body ran. `--version` compiles nothing."""
    justfile = (_REPO / "justfile").read_text(encoding="utf-8")
    (shell,) = re.findall(r"^set windows-shell := .*$", justfile, re.M)
    value = subprocess.run(["just", "--evaluate", "cargo"], cwd=_REPO, capture_output=True, text=True,
                           stdin=subprocess.DEVNULL, timeout=120).stdout
    assert "cargo-win.cmd" in value, value
    scratch = tmp_path / "cargo.just"
    scratch.write_text(f"{shell}\n\nline:\n    {value} --version\n\n"
                       f"body:\n    #!/usr/bin/env bash\n    set -euo pipefail\n    {value} --version\n",
                       encoding="utf-8", newline="\n")
    out = subprocess.run(["just", "--justfile", str(scratch), "--working-directory", str(_REPO), recipe],
                         capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=300)
    assert out.returncode == 0 and re.search(r"^cargo \d", out.stdout, re.M), out.stdout + out.stderr


def test_the_real_host_and_its_sdk_resolve(tmp_path):
    """On this machine's own rustc: a host the wrapper knows, and an SDK version
    directory, never the Driver Kit's `wdf` sibling."""
    got = _resolve(tmp_path, None)
    assert got["HOST_ARCH"] in ("arm64", "x64") and got["TARGET_ARCH"] == got["HOST_ARCH"], got
    assert re.fullmatch(r"10\.\d+\.\d+\.\d+", got["SDK_VER"]), got
    assert re.fullmatch(r"\d+\.\d+\.\d+", got["MSVC_VER"]), got
