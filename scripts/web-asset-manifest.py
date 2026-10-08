"""The served SPA tree's hash manifest — write it, or check a tree against it.

release-integrity.md § Release signing → Web-app verifiability, piece 2: every
release of the hosted web app publishes a manifest naming each served file and
its SHA-256, produced by the same workflow that builds the tree, so anyone can
rebuild the tagged commit and compare. This script is that manifest's one
producer and one checker:

  write  <tree> [--commit SHA] [--repo DIR] [--static DIR]
      Hash every file under <tree> (the built SPA, `apps/fauna-web/build`) and
      write `<tree>/asset-manifest.json` — so the manifest ships, and is served,
      beside the files it names (it never lists itself). The commit defaults to
      the repo's HEAD; the toolchain pins are read from the repo and the tools
      on PATH, recorded so a mismatch is explainable, not graded. With
      --static (the SPA's `static/`, copied verbatim into the tree) it first
      REFUSES when that directory holds a git-ignored file `just wasm` does not
      produce — a test recipe's output or a retired chunk's, which a long-lived
      checkout keeps and a deploy's fresh checkout never has.
  check  <manifest> <tree> [--commit SHA]
      Rebuild-and-compare's comparison half (`just web-verify` drives the
      rebuild): exit 1 naming every changed, missing and extra file, and a
      commit mismatch; toolchain differences print as notes.
  commit <manifest>
      Print the commit a manifest was built from.

Stdlib only; run as `uv run --no-project python scripts/web-asset-manifest.py`.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

MANIFEST_NAME = "asset-manifest.json"
SCHEMA = 1

# The wasm chunks `just wasm` writes into the SPA's `static/` — the only
# git-ignored files a fresh checkout's `just web` puts there — and the four
# files wasm-pack emits per chunk (`sync-wasm-static.py`'s `_SUFFIXES`).
# Pinned to the justfile's `[linux] wasm:` recipe by test_web_asset_manifest.py.
SPA_WASM_STEMS = (
    "fauna_wasm",
    "fauna_wasm_atproto_settings",
    "fauna_wasm_backups",
    "fauna_wasm_connected_apps",
    "fauna_wasm_folders",
    "fauna_wasm_labeler_catalog",
    "fauna_wasm_launch",
    "fauna_wasm_media",
    "fauna_wasm_onboarding",
    "fauna_wasm_share",
)
WASM_SUFFIXES = ("_bg.wasm", ".js", ".d.ts", "_bg.wasm.d.ts")


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def build_manifest(tree: Path, commit: str, toolchains: dict) -> dict:
    files = {}
    for path in sorted(tree.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(tree).as_posix()
        if rel == MANIFEST_NAME:
            continue
        files[rel] = _sha256(path)
    return {"schema": SCHEMA, "commit": commit, "toolchains": toolchains, "files": files}


def write_manifest(tree: Path, commit: str, toolchains: dict) -> Path:
    out = tree / MANIFEST_NAME
    manifest = build_manifest(tree, commit, toolchains)
    out.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return out


def stray_static(static: Path) -> list:
    """Git-ignored files under `static` that `just wasm` does not produce."""
    static = static.resolve()
    listed = subprocess.run(
        ["git", "-C", str(static), "ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--", "."],
        capture_output=True, text=True, check=True,
    ).stdout
    produced = {f"{stem}{suffix}" for stem in SPA_WASM_STEMS for suffix in WASM_SUFFIXES}
    return sorted(rel for rel in listed.split("\0") if rel and rel not in produced)


def compare(expected: dict, actual: dict) -> list:
    """Every way `actual` fails to reproduce `expected`; empty means it does."""
    problems = []
    if expected.get("commit") != actual.get("commit"):
        problems.append(f"commit: manifest {expected.get('commit')} vs rebuild {actual.get('commit')}")
    want, got = expected.get("files", {}), actual.get("files", {})
    for rel in sorted(want.keys() | got.keys()):
        if rel not in got:
            problems.append(f"missing: {rel}")
        elif rel not in want:
            problems.append(f"extra: {rel}")
        elif want[rel] != got[rel]:
            problems.append(f"changed: {rel}")
    return problems


def repo_pins(repo: Path) -> dict:
    """Toolchain pins the repo itself records: the rustc channel and wasm-bindgen."""
    pins = {}
    toolchain = repo / "rust-toolchain.toml"
    if toolchain.is_file():
        channel = tomllib.loads(toolchain.read_text()).get("toolchain", {}).get("channel")
        if channel:
            pins["rust"] = channel
    lock = repo / "Cargo.lock"
    if lock.is_file():
        for pkg in tomllib.loads(lock.read_text()).get("package", []):
            if pkg.get("name") == "wasm-bindgen":
                pins["wasm-bindgen"] = pkg["version"]
                break
    return pins


def _tool_version(argv: list) -> str | None:
    try:
        out = subprocess.run(argv, capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    first = out.strip().splitlines()
    return first[0] if first else None


def toolchains(repo: Path) -> dict:
    pins = repo_pins(repo)
    # wasm-pack picks the binaryen (wasm-opt) release, and deno the bundler —
    # neither is pinned by a repo file, so the build records what it ran.
    # The wasm32 C compiler too: the chunks' one C dependency (zstd-sys, via
    # fauna-core's chunk compression and fauna-mail's export wrapper) is compiled
    # by it, and its version string lands in every chunk's `producers` section
    # (`scripts/wasm-sections.py` prints it). release-integrity.md piece 1 rules
    # that every builder selects ONE pinned LLVM release through this variable.
    cc = os.environ.get("CC_wasm32_unknown_unknown") or "clang"
    for name, argv in (
        ("wasm-pack", ["wasm-pack", "--version"]),
        ("deno", ["deno", "--version"]),
        ("wasm32-cc", [cc, "--version"]),
    ):
        version = _tool_version(argv)
        if version:
            pins[name] = version
    return pins


def _head(repo: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("write")
    w.add_argument("tree", type=Path)
    w.add_argument("--commit")
    w.add_argument("--repo", type=Path, default=Path("."))
    w.add_argument("--static", type=Path)
    c = sub.add_parser("check")
    c.add_argument("manifest", type=Path)
    c.add_argument("tree", type=Path)
    c.add_argument("--commit")
    c.add_argument("--repo", type=Path, default=Path("."))
    p = sub.add_parser("commit")
    p.add_argument("manifest", type=Path)
    args = parser.parse_args(argv)

    if args.cmd == "commit":
        print(json.loads(args.manifest.read_text(encoding="utf-8"))["commit"])
        return 0
    if args.cmd == "write":
        strays = stray_static(args.static) if args.static else []
        if strays:
            print(
                f"web-asset-manifest: REFUSED — {args.static} holds {len(strays)} git-ignored file(s) "
                "`just wasm` does not produce; a fresh checkout's build has none of them, so a manifest "
                "listing them never reproduces. Remove them, then re-run:",
                file=sys.stderr,
            )
            for rel in strays:
                print(f"  {rel}", file=sys.stderr)
            return 1
        out = write_manifest(args.tree, args.commit or _head(args.repo), toolchains(args.repo))
        print(f"web-asset-manifest: wrote {out}")
        return 0

    expected = json.loads(args.manifest.read_text(encoding="utf-8"))
    commit = args.commit or _head(args.repo)
    actual = build_manifest(args.tree, commit, toolchains(args.repo))
    for name, pin in sorted(expected.get("toolchains", {}).items()):
        mine = actual["toolchains"].get(name)
        if mine is not None and mine != pin:
            print(f"note: toolchain {name}: manifest {pin!r}, this rebuild {mine!r}")
    problems = compare(expected, actual)
    if problems:
        print(f"web-asset-manifest: MISMATCH — {len(problems)} difference(s) against {args.manifest}")
        for line in problems:
            print(f"  {line}")
        return 1
    print(f"web-asset-manifest: MATCH — {len(actual['files'])} files, commit {commit}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
