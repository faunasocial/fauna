"""The wasm32 C compiler pin — select the pinned LLVM install, or refuse.

release-integrity.md § Release signing → Web-app verifiability, piece 1, the
paragraph "The wasm32 C compiler": every builder of the served web tree compiles
the wasm chunks' C (zstd-sys) with ONE checksum-pinned upstream LLVM release,
named by `supply-chain/wasm32-cc.toml`. The build SELECTS that install, never
finds one: the justfile's wasm32 export block points `CC_wasm32_unknown_unknown`
and `AR_wasm32_unknown_unknown` at the fixed root below, and every wasm32 build
recipe runs `check` first, so a build with any other compiler REFUSES rather
than falling back to the `clang` on PATH.

  path clang|ar   Print the pinned install's clang / llvm-ar.
  check           Exit 0 when the installed clang's `--version` first line is
                  the pin's; otherwise exit 1 printing why and the install steps.
                  Until the pin records that line (`active`), a machine with no
                  install passes with a note, and one WITH an install refuses
                  until the line it reports is recorded.
  install-steps   Print the checksum-verified download + unpack commands for
                  this host's architecture, for the USER to run. Never runs them:
                  the compiler is a dependency, and the
                  bump round that moves the pin is the only way it changes.
  pin             Print the pinned `clang --version` first line (empty while
                  the pin has not recorded it).

The root is fixed per machine — `~/.local/share/fauna/wasm32-cc` — with no
override: a build-machine path is wiring, not a choice (principles.md § One
configuration surface). It is user-writable, so installing
needs no sudo. Recorded in the primary dev VM's setup doc.

Stdlib only; run as `uv run --no-project python scripts/wasm32-cc.py`.
"""

import os
import platform
import shlex
import subprocess
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PIN_FILE = REPO / "supply-chain" / "wasm32-cc.toml"
ROOT = Path.home() / ".local" / "share" / "fauna" / "wasm32-cc"
DOWNLOAD_DIR = Path.home() / ".cache" / "fauna" / "wasm32-cc"
TOOLS = {"clang": "clang", "ar": "llvm-ar"}

# `platform.machine()` spellings → the pin file's `[arch.*]` keys.
_ARCH = {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}


def load_pin(path: Path = PIN_FILE) -> dict:
    pin = tomllib.loads(path.read_text(encoding="utf-8"))
    for key in ("release", "clang_version_line", "arch"):
        if key not in pin:
            raise ValueError(f"{path}: missing `{key}`")
    for arch, entry in pin["arch"].items():
        if arch not in _ARCH.values():
            raise ValueError(f"{path}: unknown architecture `{arch}`")
        for key in ("url", "sha256"):
            if not entry.get(key):
                raise ValueError(f"{path}: [arch.{arch}] missing `{key}`")
        if len(entry["sha256"]) != 64 or any(c not in "0123456789abcdef" for c in entry["sha256"]):
            raise ValueError(f"{path}: [arch.{arch}] sha256 is not 64 lowercase hex digits")
    return pin


def host_arch(machine: str | None = None) -> str:
    machine = (machine or platform.machine()).lower()
    if machine not in _ARCH:
        raise ValueError(f"no wasm32-cc pin for host architecture `{machine}`")
    return _ARCH[machine]


def tool_path(tool: str, root: Path = ROOT) -> Path:
    return root / "bin" / TOOLS[tool]


def installed_version_line(root: Path = ROOT) -> str | None:
    clang = tool_path("clang", root)
    try:
        out = subprocess.run(
            [str(clang), "--version"], capture_output=True, text=True, check=True
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    lines = out.strip().splitlines()
    return lines[0] if lines else None


def install_steps(pin: dict, arch: str, root: Path = ROOT, downloads: Path = DOWNLOAD_DIR) -> list[str]:
    entry = pin["arch"][arch]
    tarball = downloads / entry["url"].rsplit("/", 1)[1]
    q = shlex.quote
    return [
        f"mkdir -p {q(str(downloads))}",
        f"curl -fL --proto '=https' -o {q(str(tarball))} {q(entry['url'])}",
        f"echo '{entry['sha256']}  {tarball}' | sha256sum -c -",
        f"rm -rf {q(str(root))} && mkdir -p {q(str(root))}",
        f"tar -xJf {q(str(tarball))} -C {q(str(root))} --strip-components=1",
        f"{q(str(tool_path('clang', root)))} --version | head -n 1",
    ]


def active(pin: dict) -> bool:
    """The pin is ACTIVE once it records its `clang --version` line.

    The line is the first install's measurement (the pin file's own comment:
    "recorded from the first install, before any builder selects this pin"), so
    recording it is the commit that switches every builder onto the pin. Before
    it, a machine with no install builds with the clang on PATH as it always
    did — the one transitional state, which ends at that commit.
    """
    return bool(pin["clang_version_line"])


ENV_VARS = {"clang": "CC_wasm32_unknown_unknown", "ar": "AR_wasm32_unknown_unknown"}


def check(pin: dict, observed: str | None, env: dict | None = None) -> tuple[bool, str]:
    """(ok, message) for the installed compiler against the pin — pure.

    `env` is the build's environment: an active pin also needs the justfile's
    two exports to name the pinned install, so cc-rs SELECTS it rather than
    resolving a compiler of its own.
    """
    want = pin["clang_version_line"]
    if active(pin) and env is not None:
        wrong = [
            f"{var}={env.get(var)!r} (want {str(tool_path(tool))!r})"
            for tool, var in ENV_VARS.items()
            if env.get(var) != str(tool_path(tool))
        ]
        if wrong:
            return False, (
                "the pin is active but the build does not select it — the justfile's wasm32 "
                "export block must set " + "; ".join(wrong)
            )
    if not active(pin):
        if observed is None:
            return True, (
                f"wasm32-cc: pin LLVM {pin['release']} not yet active (no `clang_version_line` "
                "recorded) — building with the clang on PATH until the first install records it"
            )
        return False, (
            f"the pin records no `clang_version_line` yet; the install at {ROOT} reports "
            f"{observed!r} — record that line in {PIN_FILE.relative_to(REPO)} (it is the "
            "first install's measurement, and the commit that activates the pin)."
        )
    if observed is None:
        return False, f"no pinned LLVM {pin['release']} at {ROOT} — install it:"
    if observed != want:
        return False, (
            f"{ROOT} holds {observed!r}, the pin is {want!r} (LLVM {pin['release']}) — reinstall:"
        )
    return True, f"wasm32-cc: OK — {observed}"


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] not in ("path", "check", "install-steps", "pin"):
        print(__doc__, file=sys.stderr)
        return 2
    cmd = argv[0]
    if cmd == "path":
        if len(argv) != 2 or argv[1] not in TOOLS:
            print("usage: wasm32-cc.py path clang|ar", file=sys.stderr)
            return 2
        print(tool_path(argv[1]))
        return 0
    pin = load_pin()
    if cmd == "pin":
        print(pin["clang_version_line"])
        return 0
    if cmd == "install-steps":
        print("\n".join(install_steps(pin, host_arch())))
        return 0
    observed = installed_version_line()
    ok, message = check(pin, observed, dict(os.environ))
    if ok:
        print(message)
        return 0
    print(f"wasm32-cc: REFUSED — {message}", file=sys.stderr)
    if active(pin):
        print("\n".join(f"  {s}" for s in install_steps(pin, host_arch())), file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
