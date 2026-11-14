#!/usr/bin/env python3
"""The notices generator: one artifact's THIRD-PARTY-NOTICES from its own closure.

    just shipped-notices <artifact> <out>      write the file, print verdict lines
    just shipped-notices-check                 the cheap gate (parse-only, stdlib)

Owner: docs/goal/architecture/shipped-notices.md § How the notices file is
produced. `installer/shipped-notices.toml` names each artifact of that doc's
§ Per channel table and the roots of its closure, per ecosystem; this script
resolves the closure with the resolvers each build already runs (`cargo tree`
and `cargo metadata`, `go version -m`, the deno lockfile, NuGet's
`project.assets.json` and the published app's `deps.json`, a Gradle dependency
report) plus the binaries the manifest names as bundled, checks every component's
licence id against `deny.toml`'s `[licenses] allow` (the manifest's exception
table excepted), and copies each component's own licence and notice files byte
for byte out of the package cache, the canonical text from
`supply-chain/licence-texts/` standing in where a package carries none. The
toolchain's own runtime code (the Rust standard library, the Go standard
library and runtime, the Android NDK's sysroot, the GNU C Library's statically
linked part) is added with no manifest line,
its text the toolchain's own file.

The generated file and the cached licence files are DEPENDENCY-AUTHORED TEXT:
this script copies them and never interprets them, and no session opens them
(release-integrity.md § Reviewing untrusted source without being subverted). What a session or a test reads is the
verdict lines on stdout — per ecosystem the component count, the files copied,
the fallbacks, the files left out and the ids refused — and the file's
structure. A component name or licence id echoed in a verdict line is first
reduced to a plain character set.

Standard library only. Exit 0 = written; 1 = refused (nothing written);
2 = usage.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "installer" / "shipped-notices.toml"
DENY = REPO / "deny.toml"
TEXTS = REPO / "supply-chain" / "licence-texts"
GOAL_DOC = REPO / "docs" / "goal" / "architecture" / "shipped-notices.md"


def repo_notices() -> Path:
    """The source repository's own notices file: kept under the publish tree's
    injected files in development, at the root of the public repository."""
    dev = REPO / "scripts" / "publish" / "public-files" / "THIRD-PARTY-NOTICES.md"
    if dev.exists():
        return dev
    return REPO / "THIRD-PARTY-NOTICES.md"

WORKSPACE_TOML = REPO / "Cargo.toml"

MAX_FILE = 256 * 1024
# The Rust standard library's notice (`share/doc/rust/COPYRIGHT-library.html`,
# the Rust project's generated notice for std and everything built into it) is
# HTML of about 1.5 MB; that one named file passes under this ceiling instead.
STDLIB_MAX_FILE = 4 * 1024 * 1024
# The Android NDK's notice for its sysroot (`<prebuilt>/sysroot/NOTICE`, which
# carries the notices of bionic's startup objects with the rest of the
# sysroot's) is about 480 KiB in NDK r28; that one named file passes under this
# ceiling instead.
NDK_SYSROOT_MAX_FILE = 1024 * 1024
LICENCE_PREFIXES = ("license", "licence", "copying", "notice", "unlicense")
# A third-party notices file (THIRD-PARTY-NOTICES.TXT, ThirdPartyNotices.txt)
# attributes the code a package bundles; it is not the package's own licence.
THIRD_PARTY_NOTICE_PREFIXES = ("third-party-notice", "thirdpartynotice", "third_party_notice")
# What a copied file must not be named to count as the component's licence
# when the standard-text fallback is decided: an attribution notice (Apache's
# NOTICE) or a third-party notices file is copied, never in place of a licence.
NOT_A_LICENCE_PREFIXES = ("notice",) + THIRD_PARTY_NOTICE_PREFIXES
# Licences in the allowlist that ask for more than attribution: their
# components are listed again with where the exact source is obtained.
SOURCE_ASKING = frozenset({"MPL-2.0", "LGPL-2.1-or-later"})
# `bundled`: binaries an artifact ships that no package resolver lists (a
# toolchain's runtime library), each named in the manifest with its licence.
ECOSYSTEMS = ("rust", "go", "npm", "nuget", "gradle", "bundled")
# The toolchain's own runtime code a build links into every binary, which no
# resolver lists: the Rust standard library for an artifact with Rust roots,
# the Go standard library and runtime for one with Go binaries, the Android
# NDK's sysroot for one with an Android Rust triple, the GNU C Library's
# statically linked part for one with a linux-gnu Rust triple or a Linux cgo
# Go binary. Written by the
# generator with no manifest line (shipped-notices.md, the toolchain bullet).
TOOLCHAIN = "toolchain"
LEGS = ECOSYSTEMS + (TOOLCHAIN,)
RUST_STDLIB = "Rust standard library"
RUST_STDLIB_LICENCE = "MIT OR Apache-2.0"
RUST_STDLIB_FILE = "share/doc/rust/COPYRIGHT-library.html"
GO_STDLIB = "Go standard library and runtime"
GO_STDLIB_LICENCE = "BSD-3-Clause"
GLIBC = "GNU C Library (startup objects and static part)"
GLIBC_LICENCE = "LGPL-2.1-or-later"
# The distribution's structured record for the C library's development package
# (its DEP-5 `copyright`) and the licence text it points to, read on the job
# that writes the notices; LIBC_ROOT is where that job's system is mounted.
GLIBC_PACKAGE = "libc6-dev"
GLIBC_RECORD = "usr/share/doc/libc6-dev/copyright"
GLIBC_LICENCE_FILE = "usr/share/common-licenses/LGPL-2.1"
LIBC_ROOT = Path("/")
# Where a distribution publishes the source package of a version: LGPL-2.1
# asks for the source, so the closing section lists it.
DISTRO_SOURCE = {
    "ubuntu": "https://launchpad.net/ubuntu/+source/{pkg}/{ver}",
    "debian": "https://snapshot.debian.org/package/{pkg}/{ver}/",
}
NDK_SYSROOT = "Android NDK sysroot (bionic startup objects)"
NDK_SYSROOT_LICENCE = "BSD-2-Clause"
# The compiler driver each Android triple's link runs, as `android-ffi` names
# them (justfile NDK_CLANG): the NDK whose sysroot the link used is the one
# that driver sits in.
NDK_CLANG = {
    "aarch64-linux-android": "aarch64-linux-android26-clang",
    "armv7-linux-androideabi": "armv7a-linux-androideabi26-clang",
    "x86_64-linux-android": "x86_64-linux-android26-clang",
    "i686-linux-android": "i686-linux-android26-clang",
}

# --- The generator's own wording: a public text --------------------------------
# The wording the public-texts process landed for it (round 8, package
# shipped-notices; the header's last sentence and
# the fallback line round 9, package shipped-notices-fallback); it changes only through a request.
# public-text: shipped-notices-wording
HEADER = """Third-party notices for {name} {version}

Fauna is free, open-source software made by Fauna Social. It is provided under your choice of the MIT licence or the Apache License, Version 2.0. The full text of both licences is beside this file (LICENSE-MIT, LICENSE-APACHE).

This software includes the third-party components listed below, across the systems it is published for; a copy built for one system may not contain every one of them. Each remains under its own licence, named in its entry with the licence and notice files copied from its package; a text several components share is printed once and referred to after that. A text marked as a licence's standard text stands in for a licence file that could not be copied from the component's package.
"""
LABELS = ("Component", "Version", "From", "Licence")
FALLBACK_LINE = "No licence file was copied from this component's package; the standard text of {id} stands in for it."
SAME_TEXT_LINE = "{label}: same text as [text {n}], printed above with {first}."
CLOSING_HEADING = "Components with a licence that asks that their source be available"
CLOSING_SENTENCE = "The exact source of each version listed here is published at the place shown."
# public-text-end: shipped-notices-wording
RULE = "=" * 78


# --- Licence expressions ------------------------------------------------------------

_TOKEN = re.compile(r"\s*(\(|\)|/|[A-Za-z0-9.+\-:]+)")


class ExprError(ValueError):
    pass


def _tokens(expr: str) -> list[str]:
    out, pos = [], 0
    expr = expr.strip()
    while pos < len(expr):
        m = _TOKEN.match(expr, pos)
        if not m:
            raise ExprError(expr)
        out.append(m.group(1))
        pos = m.end()
        while pos < len(expr) and expr[pos].isspace():
            pos += 1
    return out


def _parse(expr: str):
    """SPDX expression → tree. ('or', [..]) / ('and', [..]) / ('id', 'X') with
    WITH folded into the id ('Apache-2.0 WITH LLVM-exception'), cargo's legacy
    '/' read as OR and a trailing '+' as '-or-later'."""
    toks = _tokens(expr)
    if not toks:
        raise ExprError("empty")
    pos = 0

    def peek():
        return toks[pos] if pos < len(toks) else None

    def take():
        nonlocal pos
        if pos >= len(toks):
            raise ExprError(expr)
        pos += 1
        return toks[pos - 1]

    def atom():
        t = take()
        if t == "(":
            node = disj()
            if take() != ")":
                raise ExprError(expr)
            return node
        if t in (")", "/") or t.upper() in ("AND", "OR", "WITH"):
            raise ExprError(expr)
        if t.endswith("+"):
            t = t[:-1] + "-or-later"
        if peek() is not None and peek().upper() == "WITH":
            take()
            exc = take()
            t = f"{t} WITH {exc}"
        return ("id", t)

    def conj():
        items = [atom()]
        while peek() is not None and peek().upper() == "AND":
            take()
            items.append(atom())
        return items[0] if len(items) == 1 else ("and", items)

    def disj():
        items = [conj()]
        while peek() is not None and (peek().upper() == "OR" or peek() == "/"):
            take()
            items.append(conj())
        return items[0] if len(items) == 1 else ("or", items)

    node = disj()
    if pos != len(toks):
        raise ExprError(expr)
    return node


def _allowed(node, allow) -> bool:
    kind, val = node
    if kind == "id":
        return val in allow
    if kind == "and":
        return all(_allowed(n, allow) for n in val)
    return any(_allowed(n, allow) for n in val)


def licence_allowed(expr: str | None, allow) -> bool:
    if not expr:
        return False
    try:
        return _allowed(_parse(expr), allow)
    except ExprError:
        return False


def fallback_ids(expr: str, allow) -> list[str]:
    """The allowed ids a satisfying reading of `expr` uses, in order: every
    allowed branch of an OR (the recipient's choice), every part of an AND."""
    out: list[str] = []

    def walk(node):
        kind, val = node
        if kind == "id":
            if val in allow and val not in out:
                out.append(val)
        elif kind == "and":
            for n in val:
                walk(n)
        else:
            for n in val:
                if _allowed(n, allow):
                    walk(n)

    walk(_parse(expr))
    return out


def text_file_name(licence_id: str) -> str:
    return licence_id.replace(" WITH ", "-WITH-") + ".txt"


# --- Components and the copy step ------------------------------------------------------


@dataclass
class Component:
    ecosystem: str
    name: str
    version: str
    origin: str
    licence: str | None
    source: str
    files: list[tuple[str, int, Callable[[], bytes]]] = field(default_factory=list)
    ceiling: int = MAX_FILE  # the copy limit for this component's files


_SAFE = re.compile(r"[^A-Za-z0-9@._+/:() !-]")


def safe(s: str | None) -> str:
    if s is None:
        return "none"
    return _SAFE.sub("?", s)[:120]


def _is_licence_name(name: str) -> bool:
    """Whether a package's file is one the generator copies: a licence, an
    attribution notice or a third-party notices file."""
    return name.lower().startswith(LICENCE_PREFIXES + THIRD_PARTY_NOTICE_PREFIXES)


# The legs whose every file is named for its component (the toolchain's
# notices, a bundled binary's licence file): a named file is the component's
# licence whatever it is called — the NDK sysroot's NOTICE holds bionic's
# licences.
NAMED_LEGS = (TOOLCHAIN, "bundled")


def stands_for_licence(label: str, ecosystem: str | None = None) -> bool:
    """Whether a copied file is the component's licence for the standard-text
    fallback: every file of a named leg, else anything copied but a NOTICE or
    third-party notices file (a LICENSES/ entry counts)."""
    if ecosystem in NAMED_LEGS:
        return True
    return not label.rsplit("/", 1)[-1].lower().startswith(NOT_A_LICENCE_PREFIXES)


def dir_files(d: Path, extra: list[str] = ()) -> list[tuple[str, int, Callable[[], bytes]]]:
    """A package directory's own licence and notice files: top-level names that
    start LICENSE/LICENCE/COPYING/NOTICE/UNLICENSE or a third-party notices
    file's (THIRD-PARTY-NOTICES, ThirdPartyNotices), the files in a top-level
    LICENSES/ directory, and any file the package metadata names."""
    found: dict[str, Path] = {}
    if d.is_dir():
        for p in sorted(d.iterdir()):
            if p.is_file() and _is_licence_name(p.name):
                found[p.name] = p
            elif p.is_dir() and p.name.lower() in ("licenses", "licences"):
                for q in sorted(p.iterdir()):
                    if q.is_file():
                        found[f"{p.name}/{q.name}"] = q
    for rel in extra:
        p = d / rel
        if p.is_file() and rel not in found:
            found[rel] = p
    return [(label, p.stat().st_size, (lambda p=p: p.read_bytes())) for label, p in found.items()]


def zip_files(archive: Path) -> list[tuple[str, int, Callable[[], bytes]]]:
    out = []
    try:
        with zipfile.ZipFile(archive) as z:
            for info in z.infolist():
                n = info.filename
                base = n.split("/")[-1]
                top = n.count("/") == 0 or (n.startswith("META-INF/") and n.count("/") == 1)
                if top and base and _is_licence_name(base):
                    out.append((n, info.file_size, (lambda a=archive, n=n: zipfile.ZipFile(a).read(n))))
    except (zipfile.BadZipFile, OSError):
        pass
    return out


def _size_words(n: int) -> str:
    return f"{n // (1024 * 1024)} MiB" if n % (1024 * 1024) == 0 else f"{n // 1024} KiB"


def vet_bytes(size: int, load: Callable[[], bytes], ceiling: int = MAX_FILE) -> tuple[bytes | None, str | None]:
    if size >= ceiling:
        return None, f"over {_size_words(ceiling)}"
    data = load()
    if len(data) >= ceiling:
        return None, f"over {_size_words(ceiling)}"
    if b"\x00" in data:
        return None, "carries a NUL"
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return None, "not valid UTF-8"
    return data, None


# --- Resolvers ---------------------------------------------------------------------------


def default_run(argv, env=None) -> str:
    e = dict(os.environ)
    e.update(env or {})
    r = subprocess.run(argv, cwd=REPO, env=e, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"{argv[0]} {argv[1]} exited {r.returncode}")
    return r.stdout


_TREE_LINE = re.compile(r"^(\S+) v(\S+)(?: \((.+)\))?$")
CRATES_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"


def rust_groups(spec) -> list[dict]:
    """A `rust` entry is one root group or a list of them — one per cargo
    invocation the artifact's build makes (its own features and targets)."""
    return spec if isinstance(spec, list) else [spec]


def rust_components(spec, run, repo: Path, host_target: str | None = None, targets: list[str] | None = None) -> list[Component]:
    plan = narrow_groups(rust_groups(spec), targets, host_target)
    # `cargo metadata` reads every package of the lock, and a build fetches only
    # the packages of the targets it built, so the offline resolve below fails on
    # a machine (a fresh release runner above all) where no build downloaded some
    # other target's package. One fetch puts the whole lock in the cache: the
    # locked, checksummed bytes the review covered, no version moved.
    run(["cargo", "fetch", "--locked"])
    seen: dict[tuple[str, str, str | None], None] = {}
    for group, triples in plan:
        _rust_tree(group, triples, run, seen)
    return _rust_resolve(seen, run, repo)


def narrow_groups(groups: list[dict], targets: list[str] | None, host_target: str | None = None) -> list[tuple[dict, list[str]]]:
    """Each root group with the triples this run resolves it for.

    A group naming no target builds for the host; `host_target` names that host
    when the generator runs elsewhere (the image's notices stage runs at the
    build platform and resolves for the image's own). With no `targets` a group
    resolves for every triple it names — one file covering an artifact that
    holds several (an Android package's four ABIs). With `targets` (the triples
    the file's own binary is built for) a group naming triples keeps the ones
    in that set, so a per-triple file lists what its binary links; a group left
    with none, or a requested triple no group names, means the caller and the
    manifest disagree and is refused before any resolver runs."""
    plan: list[tuple[dict, list[str]]] = []
    used: set[str] = set()
    for g in groups:
        named = g.get("targets") or ([g["target"]] if g.get("target") else [])
        if not named:
            plan.append((g, [host_target or ""]))
            continue
        if targets is None:
            plan.append((g, list(named)))
            continue
        kept = [t for t in named if t in targets]
        if not kept:
            raise RuntimeError(f"no requested target ({', '.join(safe(t) for t in targets)}) is one the group of {', '.join(safe(p) for p in g['packages'])} names")
        used.update(kept)
        plan.append((g, kept))
    if targets is not None:
        unused = [t for t in targets if t not in used]
        if unused:
            raise RuntimeError(f"no root group names the requested target {', '.join(safe(t) for t in unused)}")
    return plan


def _rust_tree(spec: dict, triples: list[str], run, seen: dict) -> None:
    for target in triples:
        # Plain output whatever the caller's terminal settings: a runner that forces colour
        # styles the ` (*)` marker, and the line no longer parses.
        argv = ["cargo", "tree", "--color", "never", "--locked", "--offline", "-e", "normal,no-proc-macro",
                "--prefix", "none", "--format", "{p}"]
        for p in spec["packages"]:
            argv += ["-p", p]
        if target:
            argv += ["--target", target]
        if spec.get("features"):
            argv += ["--features", ",".join(spec["features"])]
        if spec.get("no_default_features"):
            argv.append("--no-default-features")
        out = run(argv, env={"CARGO_RESOLVER_FEATURE_UNIFICATION": "selected", "CARGO_TERM_COLOR": "never"})
        for line in out.splitlines():
            line = line.strip().removesuffix(" (*)")
            if not line:
                continue
            m = _TREE_LINE.match(line)
            if not m:
                raise RuntimeError(f"unparsed cargo tree line: {safe(line)}")
            seen.setdefault((m.group(1), m.group(2), m.group(3)), None)


def _rust_resolve(seen: dict, run, repo: Path) -> list[Component]:
    meta = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked", "--offline"]))
    members = set(meta.get("workspace_members", []))
    by_nv: dict[tuple[str, str], list[dict]] = {}
    for pkg in meta["packages"]:
        by_nv.setdefault((pkg["name"], pkg["version"]), []).append(pkg)
    comps = []
    for name, version, suffix in seen:
        cands = by_nv.get((name, version), [])
        pkg = _pick_cargo(cands, suffix)
        if pkg is None:
            raise RuntimeError(f"cargo metadata has no package {safe(name)} {safe(version)}")
        if pkg["id"] in members:
            continue
        pdir = Path(pkg["manifest_path"]).parent
        src = pkg.get("source")
        if src is None:
            try:
                rel = pdir.resolve().relative_to(repo.resolve()).as_posix()
            except ValueError:
                rel = pdir.name
            origin = source = f"the Fauna source repository, {rel}"
        elif src.startswith("registry+"):
            origin = "crates.io"
            source = f"https://crates.io/crates/{name}/{version}"
        else:
            origin = source = src.removeprefix("git+")
        extra = [pkg["license_file"]] if pkg.get("license_file") else []
        comps.append(Component("rust", name, version, origin, pkg.get("license"), source, dir_files(pdir, extra)))
    return comps


def _pick_cargo(cands: list[dict], suffix: str | None) -> dict | None:
    if len(cands) <= 1:
        return cands[0] if cands else None
    if suffix is None:
        reg = [c for c in cands if (c.get("source") or "").startswith("registry+")]
        return reg[0] if reg else None
    if suffix.startswith("/") or re.match(r"^[A-Za-z]:\\", suffix):
        for c in cands:
            if c.get("source") is None and str(Path(c["manifest_path"]).parent) == suffix:
                return c
    for c in cands:
        if (c.get("source") or "").startswith("git+") and suffix.split("#")[0] in c["source"]:
            return c
    return None


def go_escape(path: str) -> str:
    return re.sub(r"[A-Z]", lambda m: "!" + m.group(0).lower(), path)


# Go modules under this prefix are Fauna's own (an in-repository `replace`,
# like a Rust workspace member) and are left out of the notices.
FAUNA_GO_PREFIX = "github.com/faunasocial/fauna/"


def parse_go_table(doc: Path) -> dict[str, str]:
    """Module path → licence id, from the source repository's notices file's
    `## Go modules` table — the one home of that fact; an `unverified` cell is
    no id. Its `## Vendored source` table adds each vendored fork's id keyed by
    the fork's repository path (the leading id of the Licence cell), which is
    what a Go `replace` to that directory resolves to."""
    ids: dict[str, str] = {}
    section = ""
    for line in doc.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            section = line
            continue
        if section.startswith("## Go modules"):
            m = re.match(r"^\| `([^`]+)` \| ([^|]+?) \|", line)
            if m and not m.group(2).strip().lower().startswith("unverified"):
                ids[m.group(1)] = m.group(2).strip()
        elif section.startswith("## Vendored source"):
            m = re.match(r"^\| `([^`]+)` \| [^|]+ \| ([^|(]+?)(?: \(|\s*\|)", line)
            if m and not m.group(2).strip().lower().startswith("unverified"):
                ids[m.group(1)] = m.group(2).strip()
    return ids


def go_components(spec: dict, run, repo: Path, go_ids: dict[str, str], versions: list[tuple[str, Path]] | None = None, cgo_linux: list[str] | None = None) -> list[Component]:
    """The linked modules of the artifact's Go binaries. `versions` gathers
    each distinct Go version they record with the main module it was built in,
    whose `go.mod` decides which Go the job's own `go` runs there; `cgo_linux`
    the binaries built for Linux with cgo, which link the C library."""
    # A per-run `binary` (`--set go.binary=<path>`) names where this run's build
    # put the binary, in place of the manifest's list.
    binaries = [spec["binary"]] if spec.get("binary") else list(spec.get("binaries", []))
    modcache = spec.get("modcache") or os.environ.get("GOMODCACHE") or run(["go", "env", "GOMODCACHE"]).strip()
    comps: dict[tuple[str, str], Component] = {}
    for binary in binaries:
        if not Path(binary).is_absolute():
            binary = str(repo / binary)
        if not Path(binary).is_file():
            raise FileNotFoundError(2, "no such file", binary)
        # A local `replace` path is relative to the main module's directory,
        # which is not the binary's when a build stages it elsewhere.
        module = repo / spec["module"] if spec.get("module") else Path(binary).parent
        got, go_version, cgo = _go_binary(binary, run, modcache, go_ids, module, repo)
        if cgo and cgo_linux is not None:
            cgo_linux.append(Path(binary).name)
        if versions is not None and go_version not in [v for v, _ in versions]:
            versions.append((go_version, module))
        for c in got:
            comps.setdefault((c.name, c.version), c)
    return list(comps.values())


def _go_binary(binary: str, run, modcache: str, go_ids: dict[str, str], module: Path, repo: Path = REPO) -> tuple[list[Component], str, bool]:
    """The binary's linked modules, the Go version it records (the first line
    of `go version -m` is `<path>: go<version>`), and whether its build
    settings record a Linux build with cgo (`CGO_ENABLED=1`, `GOOS=linux`)."""
    out = run(["go", "version", "-m", binary])
    settings = {}
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3 and parts[1] == "build":
            k, _, v = parts[2].partition("=")
            settings[k] = v
    cgo = settings.get("CGO_ENABLED") == "1" and settings.get("GOOS") == "linux"
    first = out.splitlines()[0] if out.strip() else ""
    words = first.rpartition(": ")[2].split()
    if not words or not re.fullmatch(r"go\d[\w.+-]*", words[0]):
        raise RuntimeError(f"go version -m records no Go version for {safe(Path(binary).name)}")
    go_version = words[0]
    mods: list[dict] = []
    for line in out.splitlines():
        parts = line.strip("\n").split("\t")
        if len(parts) >= 4 and parts[1] == "dep":
            mods.append({"path": parts[2], "version": parts[3], "to": None, "to_version": ""})
        elif len(parts) >= 3 and parts[1] == "=>" and mods:
            mods[-1]["to"] = parts[2]
            mods[-1]["to_version"] = parts[3] if len(parts) > 3 else ""
    entries = []
    for m in mods:
        declared, to = m["path"], m["to"]
        if declared.startswith(FAUNA_GO_PREFIX) or (to or "").startswith(FAUNA_GO_PREFIX):
            continue
        if to and (to.startswith((".", "/")) or re.match(r"^[A-Za-z]:[\\/]", to)):
            # A vendored fork (a `replace` to a directory in the repository):
            # named by the upstream module it forks, at the version its `dep`
            # line requires (the release the fork is based on), found at its
            # repository path — resolved against the main module's directory,
            # never the binary's — and its id read by that path.
            pdir = (module / to).resolve()
            try:
                rel = pdir.relative_to(repo.resolve()).as_posix()
            except ValueError:
                rel = to
            origin = source = f"the Fauna source repository, {rel}"
            entries.append((declared, m["version"], origin, go_ids.get(rel) or go_ids.get(declared), source, pdir, False))
        else:
            name, version = (to, m["to_version"]) if to else (declared, m["version"])
            pdir = Path(modcache) / f"{go_escape(name)}@{version}"
            source = f"https://proxy.golang.org/{go_escape(name)}/@v/{version}.zip"
            entries.append((name, version, name, go_ids.get(name) or go_ids.get(declared), source, pdir, True))
    # A machine whose module cache lacks a linked module (a release job that did
    # not build the binary) fetches exactly those, inside the main module so
    # go.sum checks them; an absent one would read as a module carrying no
    # licence file of its own and print the canonical text for its copyright.
    missing = [f"{n}@{v}" for n, v, _, _, _, pdir, cached in entries if cached and not pdir.is_dir()]
    if missing:
        run(["go", "-C", str(module), "mod", "download", *missing])
    comps = []
    for name, version, origin, lic, source, pdir, _ in entries:
        if not pdir.is_dir():
            raise FileNotFoundError(2, "no such module directory", str(pdir))
        comps.append(Component("go", name, version, origin, lic, source, dir_files(pdir)))
    return comps, go_version, cgo


def rustc_host(vv: str) -> str:
    """The host triple in `rustc -vV`'s report: what a group naming no target
    builds for when the run names no `--host-target`."""
    for line in vv.splitlines():
        k, sep, v = line.partition(": ")
        if sep and k.strip() == "host":
            return v.strip()
    raise RuntimeError("rustc -vV reports no host line")


def rust_stdlib_component(run, vv: str | None = None) -> Component:
    """The Rust standard library of the pinned toolchain the run resolves with:
    named from `rustc -vV`, its text the toolchain's own generated notice for
    std and everything built into it, copied under the larger ceiling. A
    toolchain without the file fails the run."""
    fields = {}
    for line in (vv if vv is not None else run(["rustc", "-vV"])).splitlines():
        k, sep, v = line.partition(": ")
        if sep:
            fields[k.strip()] = v.strip()
    release, commit = fields.get("release"), fields.get("commit-hash")
    if not release or not commit:
        raise RuntimeError("rustc -vV reports no release or commit-hash line")
    sysroot = Path(run(["rustc", "--print", "sysroot"]).strip())
    p = sysroot / RUST_STDLIB_FILE
    if not p.is_file():
        raise FileNotFoundError(2, "no such toolchain notice file", str(p))
    files = [(p.name, p.stat().st_size, (lambda p=p: p.read_bytes()))]
    return Component(TOOLCHAIN, RUST_STDLIB, f"{release} (commit {commit})", "the Rust project, rust-lang.org", RUST_STDLIB_LICENCE,
                     f"https://github.com/rust-lang/rust/tree/{commit}/library", files, ceiling=STDLIB_MAX_FILE)


def ndk_sysroot_component(triples: list[str], which: Callable[[str], str | None] = shutil.which) -> Component:
    """The Android NDK's sysroot, whose startup objects every Android link puts
    into the library: found through the compiler driver the build's link runs
    (on PATH, as `android-ffi` requires it), named with the NDK's
    `source.properties` release, its text the NDK's own sysroot notice, copied
    under its named ceiling. Every triple must resolve to one NDK; a driver or
    a file missing fails the run."""
    roots: set[Path] = set()
    for t in triples:
        tool = NDK_CLANG.get(t)
        if tool is None:
            raise RuntimeError(f"no NDK compiler driver is known for {safe(t)}")
        found = which(tool)
        if not found:
            raise FileNotFoundError(2, "no NDK compiler driver on PATH", tool)
        # <ndk>/toolchains/llvm/prebuilt/<host>/bin/<driver>
        roots.add(Path(os.path.realpath(found)).parents[1])
    if len(roots) != 1:
        raise RuntimeError(f"the Android triples link against {len(roots)} NDK installations: {', '.join(safe(str(r)) for r in sorted(roots))}")
    (prebuilt,) = roots
    ndk = prebuilt.parents[3]  # <ndk>/toolchains/llvm/prebuilt/<host>
    props = ndk / "source.properties"
    if not props.is_file():
        raise FileNotFoundError(2, "no such NDK file", str(props))
    fields = {}
    for line in props.read_text(encoding="utf-8").splitlines():
        k, sep, v = line.partition("=")
        if sep:
            fields[k.strip()] = v.strip()
    revision = fields.get("Pkg.Revision")
    if not revision:
        raise RuntimeError(f"{safe(str(props))} names no Pkg.Revision")
    release = fields.get("Pkg.ReleaseName")
    version = f"{release} ({revision})" if release else revision
    p = prebuilt / "sysroot" / "NOTICE"
    if not p.is_file():
        raise FileNotFoundError(2, "no such NDK notice file", str(p))
    files = [("NOTICE", p.stat().st_size, (lambda p=p: p.read_bytes()))]
    return Component(TOOLCHAIN, NDK_SYSROOT, version, "the Android Open Source Project, source.android.com", NDK_SYSROOT_LICENCE,
                     "https://android.googlesource.com/platform/bionic", files, ceiling=NDK_SYSROOT_MAX_FILE)


def glibc_component(run, root: Path | None = None) -> Component:
    """The GNU C Library's statically linked part (its startup objects and its
    non-shared archive), which every `*-linux-gnu` link puts into the program:
    named with the version of the C library's development package the job has
    installed (`dpkg-query`), its texts the distribution's own record for that
    package and the licence text the record points to, copied byte for byte,
    its source the distribution's source package of that version. A job with
    no package, record, licence text or known distribution fails the run."""
    root = root if root is not None else LIBC_ROOT
    out = run(["dpkg-query", "-W", "-f", "${db:Status-Abbrev}\t${Version}\t${source:Package}\t${source:Version}", GLIBC_PACKAGE])
    fields = out.strip().split("\t")
    if len(fields) != 4 or not fields[0].startswith("ii") or not all(fields[1:]):
        raise RuntimeError(f"{GLIBC_PACKAGE} is not installed on this job: the notices are written where the C library is linked")
    _, version, src_pkg, src_ver = fields
    distro = ""
    osr = root / "etc" / "os-release"
    if osr.is_file():
        for line in osr.read_text(encoding="utf-8").splitlines():
            k, sep, v = line.partition("=")
            if sep and k.strip() == "ID":
                distro = v.strip().strip('"')
    if distro not in DISTRO_SOURCE:
        raise RuntimeError(f"no source location is known for the distribution {safe(distro) if distro else '(none)'}")
    files = []
    for rel in (GLIBC_RECORD, GLIBC_LICENCE_FILE):
        p = root / rel
        if not p.is_file():
            raise FileNotFoundError(2, "no such C library record", str(p))
        files.append((p.name, p.stat().st_size, (lambda p=p: p.read_bytes())))
    return Component(TOOLCHAIN, GLIBC, version, f"the GNU C Library project, as packaged by {distro}", GLIBC_LICENCE,
                     DISTRO_SOURCE[distro].format(pkg=src_pkg, ver=src_ver), files)


def go_stdlib_components(versions: list[tuple[str, Path]], run) -> list[Component]:
    """One Go standard library and runtime entry per Go version the artifact's
    binaries record, its text `LICENSE` at `go env GOROOT` — copied only when
    the job's own Go, asked inside the binary's main module (whose `go.mod`
    picks the toolchain, as the build's did), is that version; a different one
    fails the run."""
    comps = []
    for v, module in versions:
        words = run(["go", "-C", str(module), "env", "GOVERSION"]).split()
        have = words[0] if words else ""
        if v != have:
            raise RuntimeError(f"a Go binary records {safe(v)} but this job's Go is {safe(have) if have else 'none'}: run with the Go the binary was built with")
        p = Path(run(["go", "-C", str(module), "env", "GOROOT"]).strip()) / "LICENSE"
        if not p.is_file():
            raise FileNotFoundError(2, "no such toolchain licence file", str(p))
        files = [("LICENSE", p.stat().st_size, (lambda p=p: p.read_bytes()))]
        comps.append(Component(TOOLCHAIN, GO_STDLIB, v.removeprefix("go"), "the Go project, go.dev", GO_STDLIB_LICENCE,
                               f"https://go.dev/dl/{v}.src.tar.gz", files))
    return comps


def _npm_split(key: str) -> tuple[str, str]:
    """'@scope/name@1.2.3_peer@4' → ('@scope/name', '1.2.3_peer@4')."""
    at = key.index("@", 1)
    return key[:at], key[at + 1:]


def _npm_dir(d: Path, key: str) -> Path:
    name, ver = _npm_split(key)
    plain = ver.split("_", 1)[0]  # deno names the directory without the peer suffix
    return d / "node_modules" / ".deno" / f"{name.replace('/', '+')}@{plain}" / "node_modules" / name


def npm_components(spec: dict, repo: Path) -> list[Component]:
    d = Path(spec["dir"])
    if not d.is_absolute():
        d = repo / d
    pkg = json.loads((d / "package.json").read_text(encoding="utf-8"))
    lock = json.loads((d / spec.get("lockfile", "deno.lock")).read_text(encoding="utf-8"))
    npm = lock.get("npm", {})
    specs = lock.get("specifiers", {})
    all_ranges = {**pkg.get("dependencies", {}), **pkg.get("devDependencies", {})}
    roots = []
    for name in list(pkg.get("dependencies", {})) + list(spec.get("bundled", [])):
        rng = all_ranges.get(name)
        if rng is None:
            raise RuntimeError(f"npm root {safe(name)} is in no dependency table of package.json")
        ver = specs.get(f"npm:{name}@{rng}")
        if ver is None:
            # deno normalises a range in its specifier key (`^3.0.0` → `3`)
            hits = [v for k, v in specs.items() if k.startswith(f"npm:{name}@")]
            ver = hits[0] if len(hits) == 1 else None
        if ver is None:
            raise RuntimeError(f"npm root {safe(name)}@{safe(rng)} has no lockfile specifier")
        roots.append(f"{name}@{ver}")

    def resolve(dep: str) -> str:
        if dep in npm:
            return dep
        hits = [k for k in npm if _npm_split(k)[0] == dep]
        if len(hits) != 1:
            raise RuntimeError(f"npm dependency {safe(dep)} resolves to {len(hits)} lockfile entries")
        return hits[0]

    seen: list[str] = []
    stack = [resolve(r) for r in roots]
    while stack:
        key = stack.pop()
        if key in seen:
            continue
        seen.append(key)
        # The lockfile lists a package's peers and optional platform binaries
        # beside its dependencies; the runtime closure follows only the names
        # the package's own manifest puts under `dependencies`.
        pj = _npm_dir(d, key) / "package.json"
        own = set(json.loads(pj.read_text(encoding="utf-8")).get("dependencies", {})) if pj.is_file() else None
        for dep in npm.get(key, {}).get("dependencies", []):
            dep_name = _npm_split(dep)[0] if "@" in dep[1:] else dep
            if own is None or dep_name in own:
                stack.append(resolve(dep))
    comps = []
    for key in seen:
        name, ver = _npm_split(key)
        plain = ver.split("_", 1)[0]
        pdir = _npm_dir(d, key)
        lic = None
        pj = pdir / "package.json"
        if pj.is_file():
            meta = json.loads(pj.read_text(encoding="utf-8"))
            lic = meta.get("license")
            if isinstance(lic, dict):
                lic = lic.get("type")
            if lic is None and isinstance(meta.get("licenses"), list):
                lic = " OR ".join(x.get("type", "") for x in meta["licenses"] if isinstance(x, dict)) or None
        source = f"https://registry.npmjs.org/{name}/-/{name.split('/')[-1]}-{plain}.tgz"
        comps.append(Component("npm", name, plain, "registry.npmjs.org", lic if isinstance(lic, str) else None, source, dir_files(pdir)))
    return comps


# The asset groups of a project.assets.json target entry whose files reach the
# published output; `compile` and `build` (reference assemblies, MSBuild props
# and targets) do not, and `_._` is NuGet's empty-folder placeholder.
NUGET_SHIPPED_GROUPS = ("runtime", "native", "runtimeTargets", "resource")


def _nuspec_licence(pdir: Path, name: str) -> tuple[str | None, list[str]]:
    """A package's licence expression and the licence file it names, from its
    `.nuspec` (structured metadata, never a text read for its meaning)."""
    lic = None
    extra: list[str] = []
    nuspec = pdir / f"{name.lower()}.nuspec"
    if nuspec.is_file():
        try:
            root = ET.parse(nuspec).getroot()
            for el in root.iter():
                if el.tag.rsplit("}", 1)[-1] == "license":
                    if el.get("type") == "expression":
                        lic = (el.text or "").strip() or None
                    elif el.get("type") == "file" and el.text:
                        extra.append(el.text.strip())
        except ET.ParseError:
            pass
    return lic, extra


def nuget_components(spec: dict, repo: Path, notes: list[str] | None = None, targets: list[str] | None = None,
                     which: Callable[[str], str | None] = shutil.which) -> list[Component]:
    assets_path = Path(spec["assets"])
    if not assets_path.is_absolute():
        assets_path = repo / assets_path
    assets = json.loads(assets_path.read_text(encoding="utf-8-sig"))
    folders = [Path(f) for f in assets.get("packageFolders", {})]
    if not assets.get("targets"):
        raise RuntimeError(f"{safe(assets_path.name)} names no restore target, so what reaches the published output is unknown")
    # A package is in the app when some target gives it an asset that is
    # published; one with build assets alone (build tooling) is not.
    shipped = {key for entries in assets["targets"].values() for key, entry in entries.items()
               if any(f.rsplit("/", 1)[-1] != "_._" for g in NUGET_SHIPPED_GROUPS for f in (entry.get(g) or {}))}
    comps = []
    for key, lib in assets.get("libraries", {}).items():
        if lib.get("type") != "package":
            continue
        if key not in shipped:
            if notes is not None:
                notes.append(f"shipped-notices: not shipped nuget {safe(key.replace('/', '@', 1))}: no asset of it reaches the published output")
            continue
        name, version = key.split("/", 1)
        pdir = next((f / lib["path"] for f in folders if (f / lib["path"]).is_dir()), folders[0] / lib["path"] if folders else Path(lib["path"]))
        lic, extra = _nuspec_licence(pdir, name)
        files = [f for f in lib.get("files", []) if "/" not in f and _is_licence_name(f)]
        comps.append(Component("nuget", name, version, "nuget.org", lic, f"https://www.nuget.org/packages/{name}/{version}", dir_files(pdir, files + extra)))
    if spec.get("deps"):
        seen = {(c.name, c.version) for c in comps}
        for c in nuget_runtime_packs(spec["deps"], folders, repo, targets, notes, nuget_downloads(assets), which):
            if (c.name, c.version) not in seen:
                seen.add((c.name, c.version))
                comps.append(c)
    return comps


# A Windows target triple's .NET runtime identifier: `{rid}` in a deps path.
_WINDOWS_RID = {"aarch64": "win-arm64", "x86_64": "win-x64", "i686": "win-x86"}


def windows_rids(targets: list[str] | None) -> list[str]:
    rids: list[str] = []
    for t in targets or []:
        cpu, _, rest = t.partition("-")
        if rest.startswith("pc-windows-") and cpu in _WINDOWS_RID and _WINDOWS_RID[cpu] not in rids:
            rids.append(_WINDOWS_RID[cpu])
    return rids


def nuget_downloads(assets: dict) -> dict[str, str]:
    """The restore's download list (`project` → `frameworks` →
    `downloadDependencies`): each pack it fetched, lower-cased id → its exact
    version (`[V, V]`)."""
    out: dict[str, str] = {}
    for fw in ((assets.get("project") or {}).get("frameworks") or {}).values():
        for d in fw.get("downloadDependencies") or []:
            lo, _, hi = (d.get("version") or "").strip().strip("[]").partition(",")
            if d.get("name") and lo.strip() and (not hi.strip() or hi.strip() == lo.strip()):
                out[d["name"].lower()] = lo.strip()
    return out


APP_HOST = "Microsoft.NETCore.App.Host.{rid}"
DOTNET_RUNTIME_PACK = "Microsoft.NETCore.App.Runtime.{rid}"


def nuget_app_host(rid: str, downloads: dict[str, str], resolved: dict[str, tuple[str, Path]], folders: list[Path],
                   which: Callable[[str], str | None]) -> tuple[Component, str]:
    """The .NET app host of a publish for `rid`: the published app's .exe is the
    `apphost` launcher of the Microsoft.NETCore.App.Host.<rid> pack with the
    app's name written into it, and no library list names it. Where the
    restore downloaded the pack (the publish's runtime is not the SDK's own
    host's), it is that package, its version the download list's, its id and
    files its own in the package folders. Otherwise the .NET SDK supplied it
    from its own packs folder, a copy that carries no nuspec and no licence
    file: its version is the .NET runtime pack's the same publish copied (the
    SDK resolves both from its one bundled runtime version), the SDK's copy
    must be there at that version, and its id and files are that runtime
    pack's — the same .NET release's, whose packs carry the same licence and
    notices files. Returns the entry and where it came from."""
    name = APP_HOST.format(rid=rid)
    url = lambda v: f"https://www.nuget.org/packages/{name}/{v}"  # noqa: E731
    version = downloads.get(name.lower())
    if version is not None:
        rel = Path(name.lower()) / version
        pdir = next((f / rel for f in folders if (f / rel).is_dir()), None)
        if pdir is None:
            raise FileNotFoundError(2, "no such app host pack directory in the package folders", f"{name}/{version}")
        lic, extra = _nuspec_licence(pdir, name)
        return Component("nuget", name, version, "nuget.org", lic, url(version), dir_files(pdir, extra)), "downloaded by the restore"
    runtime = DOTNET_RUNTIME_PACK.format(rid=rid)
    if runtime.lower() not in resolved:
        raise RuntimeError(f"the app host of the {rid} publish cannot be placed: the restore downloaded no {name} "
                           f"and the publish copied no {runtime} to read its version from")
    version, rdir = resolved[runtime.lower()]
    dotnet = which("dotnet")
    sdk = Path(dotnet).resolve().parent / "packs" / name / version if dotnet else None
    if sdk is None or not (sdk / "runtimes" / rid / "native" / "apphost.exe").is_file():
        raise FileNotFoundError(2, "no such app host: the restore downloaded none and the .NET SDK's packs folder holds none at the runtime pack's version",
                                str(sdk) if sdk else f"dotnet/packs/{name}/{version}")
    lic, extra = _nuspec_licence(sdk, name)
    files = dir_files(sdk, extra)
    where = "the .NET SDK's own copy"
    if lic is None and not files:
        lic, extra = _nuspec_licence(rdir, runtime)
        files = dir_files(rdir, extra)
        where += f", with the id and files of {safe(runtime)} {safe(version)}"
    return Component("nuget", name, version, "nuget.org", lic, url(version), files), where


def nuget_runtime_packs(deps: str, folders: list[Path], repo: Path, targets: list[str] | None, notes: list[str] | None,
                        downloads: dict[str, str] | None = None, which: Callable[[str], str | None] = shutil.which) -> list[Component]:
    """The runtime packs a self-contained publish copied into the output — the
    .NET runtime, the Windows SDK projection — read from the published app's
    `<app>.deps.json` (its `runtimepack` libraries), and the app host its .exe
    is (`nuget_app_host`, for the runtime the deps.json's `runtimeTarget`
    names). The restore lists a pack as a download dependency, never as a
    library, so project.assets.json alone does not name it. A path holding
    `{rid}` is one runtime's publish, filled from the run's Windows targets."""
    if "{rid}" in deps:
        rids = windows_rids(targets)
        if not rids:
            raise RuntimeError("the published app's deps.json is per runtime ({rid}): run with the Windows target the artifact is built for (--target)")
        paths = [deps.replace("{rid}", r) for r in rids]
    else:
        paths = [deps]
    comps = []
    for p in paths:
        path = Path(p) if Path(p).is_absolute() else repo / p
        doc = json.loads(path.read_text(encoding="utf-8-sig"))
        packs = [k for k, lib in doc.get("libraries", {}).items() if lib.get("type") == "runtimepack"]
        resolved: dict[str, tuple[str, Path]] = {}
        for key in packs:
            name, _, version = key.removeprefix("runtimepack.").partition("/")
            rel = Path(name.lower()) / version
            pdir = next((f / rel for f in folders if (f / rel).is_dir()), None)
            if pdir is None:
                raise FileNotFoundError(2, "no such runtime pack directory in the package folders", f"{name}/{version}")
            resolved[name.lower()] = (version, pdir)
            lic, extra = _nuspec_licence(pdir, name)
            # dir_files copies the runtime's third-party notices with its licence.
            files = dir_files(pdir, extra)
            comps.append(Component("nuget", name, version, "nuget.org", lic, f"https://www.nuget.org/packages/{name}/{version}", files))
        runtime_target = (doc.get("runtimeTarget") or {}).get("name") or ""
        if notes is not None:
            notes.append(f"shipped-notices: nuget runtime packs from {safe(path.name)} ({safe(runtime_target)}): {len(packs)}")
        # `.NETCoreApp,Version=v10.0/win-arm64`: the publish's own record of its runtime
        rid = runtime_target.partition("/")[2]
        if not rid:
            raise RuntimeError(f"{safe(path.name)} names no runtime ({safe(runtime_target)}), so which app host the published .exe is cannot be read")
        host, where = nuget_app_host(rid, downloads or {}, resolved, folders, which)
        comps.append(host)
        if notes is not None:
            # `where` is this file's wording, its names already made safe
            notes.append(f"shipped-notices: nuget app host {safe(host.name)}@{safe(host.version)} ({where})")
    return comps


def bundled_components(spec: dict, repo: Path) -> list[Component]:
    """Binaries the artifact ships that no package resolver lists (the mail
    bridge's libunwind.dll, from the llvm-mingw toolchain), each named in the
    manifest with its version, origin and licence id. `dir` is where the build
    staged it (`--set bundled.dir=<dir>` per run); the binary must be there,
    and so must every licence file the entry names, which its build staged
    beside it — a missing one fails the run rather than letting the standard
    text stand in for the component's own copyright lines unnoticed."""
    stage = Path(spec.get("dir") or ".")
    if not stage.is_absolute():
        stage = repo / stage
    comps = []
    for c in spec.get("components", []):
        binary = stage / c["binary"]
        if not binary.is_file():
            raise FileNotFoundError(2, "no such bundled binary", str(binary))
        files = []
        for rel in c.get("files", []):
            p = stage / rel
            if not p.is_file():
                raise FileNotFoundError(2, "no such staged licence file", str(p))
            files.append((rel, p.stat().st_size, (lambda p=p: p.read_bytes())))
        comps.append(Component("bundled", c["name"], c["version"], c["origin"], c.get("licence"), c.get("source", c["origin"]), files))
    return comps


# Maven POM licence names → SPDX ids (a POM records a name, not an id). An
# unmapped name is no id, so the component is refused until mapped or excepted.
POM_LICENCE_NAMES = {
    "the apache software license, version 2.0": "Apache-2.0",
    "the apache license, version 2.0": "Apache-2.0",
    "apache license, version 2.0": "Apache-2.0",
    "apache license 2.0": "Apache-2.0",
    "apache-2.0": "Apache-2.0",
    "apache 2.0": "Apache-2.0",
    "mit": "MIT",
    "mit license": "MIT",
    "the mit license": "MIT",
    "bsd-3-clause": "BSD-3-Clause",
    "the bsd 3-clause license": "BSD-3-Clause",
    "new bsd license": "BSD-3-Clause",
    "bsd 2-clause license": "BSD-2-Clause",
    "lgpl-2.1-or-later": "LGPL-2.1-or-later",
}

_GRADLE_LINE = re.compile(r"^[| ]*[+\\]--- ([^:\s]+):([^:\s]+):(\S+)(?: -> (\S+))?(.*)$")


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def pom_licence_names(cache: Path, group: str, artifact: str, version: str, depth: int = 0) -> list[str]:
    """The licence names a POM declares, or — declaring none — the ones it
    inherits from its `<parent>` POM, as Maven does (Guava's POMs name theirs
    only in guava-parent). The parent is read from the same cache, never fetched."""
    names: list[str] = []
    parent: tuple[str, str, str] | None = None
    for pom in sorted((cache / group / artifact / version).glob("*/*.pom")):
        try:
            root = ET.parse(pom).getroot()
        except ET.ParseError:
            continue
        for el in root:
            tag = _local(el.tag)
            if tag == "licenses":
                for lic in el:
                    for child in lic:
                        if _local(child.tag) == "name" and child.text:
                            names.append(child.text.strip())
            elif tag == "parent":
                coords = {_local(c.tag): (c.text or "").strip() for c in el}
                if all(coords.get(k) for k in ("groupId", "artifactId", "version")):
                    parent = (coords["groupId"], coords["artifactId"], coords["version"])
    if not names and parent and depth < 8:
        return pom_licence_names(cache, *parent, depth=depth + 1)
    return names


def gradle_components(spec: dict, repo: Path) -> list[Component]:
    report = Path(spec["report"])
    if not report.is_absolute():
        report = repo / report
    cache = Path(spec.get("cache") or Path.home() / ".gradle" / "caches" / "modules-2" / "files-2.1")
    seen: dict[tuple[str, str, str], None] = {}
    for line in report.read_text(encoding="utf-8").splitlines():
        m = _GRADLE_LINE.match(line)
        if not m:
            continue
        tail = m.group(5).strip()
        if tail in ("(c)", "(n)"):
            continue
        target = m.group(4)
        if target and target.count(":") == 2:
            # `a:b:1 -> c:d:2`: a dependency substitution, the target is the component.
            seen.setdefault(tuple(target.split(":")), None)
        else:
            seen.setdefault((m.group(1), m.group(2), target or m.group(3)), None)
    comps = []
    for group, artifact, version in seen:
        vdir = cache / group / artifact / version
        lic_names = pom_licence_names(cache, group, artifact, version)
        files: list = []
        for arc in sorted(list(vdir.glob("*/*.jar")) + list(vdir.glob("*/*.aar"))):
            files += zip_files(arc)
        ids = [POM_LICENCE_NAMES.get(n.lower()) for n in lic_names]
        lic = " OR ".join(ids) if ids and all(ids) else None
        name = f"{group}:{artifact}"
        source = f"https://repo1.maven.org/maven2/{group.replace('.', '/')}/{artifact}/{version}/"
        comps.append(Component("gradle", name, version, f"Maven {name}", lic, source, files))
    return comps


# --- Generation ------------------------------------------------------------------------


@dataclass
class Result:
    ok: bool
    text: str
    verdicts: list[str]
    components: list[Component]


def load_allow(deny: Path = DENY) -> set[str]:
    return set(tomllib.loads(deny.read_text(encoding="utf-8"))["licenses"]["allow"])


def workspace_version(path: Path = WORKSPACE_TOML) -> str:
    return tomllib.loads(path.read_text(encoding="utf-8"))["workspace"]["package"]["version"]


def generate(artifact: str, manifest: dict, *, allow, texts_dir: Path, version: str, run=default_run, go_ids=None, repo: Path = REPO, host_target: str | None = None, targets: list[str] | None = None, which: Callable[[str], str | None] = shutil.which, libc_root: Path | None = None) -> Result:
    arts = manifest.get("artifact", {})
    if artifact not in arts:
        print(f"shipped-notices: no artifact {safe(artifact)} in the manifest (known: {', '.join(sorted(arts))})", file=sys.stderr)
        raise SystemExit(2)
    spec = arts[artifact]
    exception_list = manifest.get("exception", [])

    def exception_for(eco: str, name: str):
        for e in exception_list:
            pat = e.get("name", "")
            if e.get("ecosystem") == eco and (name == pat or (pat.endswith("*") and name.startswith(pat[:-1]))):
                return e
        return None

    verdicts: list[str] = []
    comps: list[Component] = []
    notes: list[str] = []  # what a resolver set aside as not shipped
    go_versions: list[tuple[str, Path]] = []  # the Go versions the artifact's binaries record
    cgo_linux: list[str] = []  # the artifact's Go binaries built for Linux with cgo
    for eco in ECOSYSTEMS:
        if eco not in spec:
            continue
        s = spec[eco]
        if eco == "rust":
            got = rust_components(s, run, repo, host_target, targets)
        elif eco == "go":
            got = go_components(s, run, repo, go_ids if go_ids is not None else parse_go_table(repo_notices()), go_versions, cgo_linux)
        elif eco == "npm":
            got = npm_components(s, repo)
        elif eco == "nuget":
            got = nuget_components(s, repo, notes, targets, which)
        elif eco == "gradle":
            got = gradle_components(s, repo)
        else:
            got = bundled_components(s, repo)
        comps += sorted(got, key=lambda c: (c.name, c.version))
    toolchain = "rust" in spec or "go" in spec
    linux_gnu = bool(cgo_linux)
    if "rust" in spec:
        vv = run(["rustc", "-vV"])
        comps.append(rust_stdlib_component(run, vv))
        triples = {t for _, ts in narrow_groups(rust_groups(spec["rust"]), targets, host_target) for t in ts}
        if "" in triples:  # a group naming no target builds for the host
            triples.discard("")
            triples.add(rustc_host(vv))
        android = sorted(t for t in triples if "-linux-android" in t)
        if android:
            comps.append(ndk_sysroot_component(android, which))
        linux_gnu = linux_gnu or any(t.endswith("-linux-gnu") for t in triples)
    comps += go_stdlib_components(go_versions, run)
    if linux_gnu:
        comps.append(glibc_component(run, libc_root))

    texts: dict[str, int] = {}        # sha256 → text number
    first_with: dict[int, str] = {}   # text number → the entry that printed it
    body: list[str] = []
    closing: list[str] = []
    stats = {eco: {"components": 0, "files-copied": 0, "fallbacks": 0, "left-out": 0, "refused": 0} for eco in LEGS}
    detail: list[str] = []
    failed = False

    def emit(label: str, data: bytes, who: str):
        h = hashlib.sha256(data).hexdigest()
        if h in texts:
            body.append(SAME_TEXT_LINE.format(label=label, n=texts[h], first=first_with[texts[h]]))
            return
        n = len(texts) + 1
        texts[h] = n
        first_with[n] = who
        body.append(f"--- {label} [text {n}] ---")
        body.append(data.decode("utf-8").rstrip("\n"))
        body.append("")

    for c in comps:
        st = stats[c.ecosystem]
        st["components"] += 1
        exc = exception_for(c.ecosystem, c.name)
        tag = f"{c.ecosystem} {safe(c.name)}@{safe(c.version)}"
        allowed = licence_allowed(c.licence, allow)
        if not allowed and exc is None:
            st["refused"] += 1
            failed = True
            detail.append(f"shipped-notices: REFUSED {tag}: licence {safe(c.licence)} not in deny.toml's allow list and no manifest exception")
            continue
        shown = c.licence if allowed else (c.licence or exc.get("licence", "see the component's own terms"))
        # The id the fallback text and the closing section go by: the
        # component's own, or — where it declares none — the id its exception
        # records, when that id is in the allow list.
        effective = c.licence if allowed else (None if c.licence else exc.get("licence"))
        effective_ok = licence_allowed(effective, allow)
        who = f"{c.name} {c.version}"
        body.append(RULE)
        body.append(f"{LABELS[0]}: {c.name}")
        body.append(f"{LABELS[1]}: {c.version}")
        body.append(f"{LABELS[2]}: {c.origin}")
        body.append(f"{LABELS[3]}: {shown}")
        body.append("")
        copied = 0
        licences = 0
        for label, size, load in c.files:
            data, why = vet_bytes(size, load, c.ceiling)
            if why:
                st["left-out"] += 1
                detail.append(f"shipped-notices: left out {tag} {safe(label)}: {why}")
                continue
            emit(label, data, who)
            copied += 1
            licences += stands_for_licence(label, c.ecosystem)
        st["files-copied"] += copied
        if licences == 0:
            ids = fallback_ids(effective, allow) if effective_ok else []
            if not ids:
                st["left-out"] += 0
                detail.append(f"shipped-notices: no text {tag}: no licence file of its own and no canonical text for its licence")
                if exc is None:
                    failed = True
            for i in ids:
                p = texts_dir / text_file_name(i)
                if not p.is_file():
                    failed = True
                    detail.append(f"shipped-notices: no fallback text for {safe(i)} ({tag})")
                    continue
                body.append(FALLBACK_LINE.format(id=i))
                emit(f"{i} (standard text)", p.read_bytes(), who)
            if ids:
                st["fallbacks"] += 1
        if effective_ok and SOURCE_ASKING.intersection(fallback_ids(effective, allow)):
            closing.append(f"{c.name} {c.version} ({effective}): {c.source}")

    out = [HEADER.format(name=spec.get("name", artifact), version=version)]
    out += body
    if closing:
        out += [RULE, CLOSING_HEADING, "", CLOSING_SENTENCE, ""] + closing
    text = "\n".join(out).rstrip("\n") + "\n"
    for eco in LEGS:
        if eco in spec or (eco == TOOLCHAIN and toolchain):
            verdicts.append(f"shipped-notices: {artifact} {eco}: " + " ".join(f"{k}={v}" for k, v in stats[eco].items()))
    verdicts += notes + detail
    total = sum(s["components"] for s in stats.values())
    if failed:
        verdicts.append(f"shipped-notices: {artifact} FAILED — {total} components, see the lines above; nothing written")
    else:
        verdicts.append(f"shipped-notices: {artifact} OK — {total} components, {len(texts)} distinct texts")
    return Result(not failed, text, verdicts, comps)


# --- The cheap gate ------------------------------------------------------------------------


def goal_channels(doc: Path = GOAL_DOC) -> set[str]:
    """First-column cells of the goal doc's § Per channel table."""
    out: set[str] = set()
    in_sec = False
    for line in doc.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            in_sec = line.strip() == "## Per channel"
            continue
        if in_sec and line.startswith("|") and not line.startswith("|---"):
            cell = line.split("|")[1].strip()
            if cell and cell != "Channel":
                out.add(cell)
    return out


def check(manifest: dict, allow, texts_dir: Path, goal_doc: Path) -> list[str]:
    errs = []
    for i in sorted(allow):
        if not (texts_dir / text_file_name(i)).is_file():
            errs.append(f"no fallback text for allowlist id {i}: supply-chain/licence-texts/{text_file_name(i)} (run `just licence-texts-fetch`)")
    channels = goal_channels(goal_doc)
    for name, spec in sorted(manifest.get("artifact", {}).items()):
        if spec.get("channel") not in channels:
            errs.append(f"artifact {name}: channel {spec.get('channel')!r} is not a row of {goal_doc.name} § Per channel")
        if not any(eco in spec for eco in ECOSYSTEMS) and not spec.get("pending"):
            errs.append(f"artifact {name} names no closure (one of {', '.join(ECOSYSTEMS)}, or `pending` with why)")
        if "rust" in spec and any(not g.get("packages") for g in rust_groups(spec["rust"])):
            errs.append(f"artifact {name}: a rust root group names no packages")
        if "go" in spec and not (spec["go"].get("binaries") or spec["go"].get("binary")):
            errs.append(f"artifact {name}: go names no binary")
        if "bundled" in spec:
            comps = spec["bundled"].get("components") or []
            if not comps:
                errs.append(f"artifact {name}: bundled names no component")
            for c in comps:
                for k in ("name", "version", "origin", "licence", "binary"):
                    if not c.get(k):
                        errs.append(f"artifact {name}: bundled component {c.get('name', '?')} names no `{k}`")
    for e in manifest.get("exception", []):
        for k in ("ecosystem", "name", "licence", "reading"):
            if not e.get(k):
                errs.append(f"exception {e.get('name', '?')}: missing `{k}` (every exception carries a reading)")
        if e.get("ecosystem") not in ECOSYSTEMS:
            errs.append(f"exception {e.get('name', '?')}: unknown ecosystem {e.get('ecosystem')!r}")
    return errs


def check_texts_record(texts_dir: Path = TEXTS) -> list[str]:
    """Each fallback text matches the checksum the fetch recorded."""
    rec_path = texts_dir / "SOURCE.toml"
    if not rec_path.is_file():
        return [f"{rec_path.relative_to(REPO)} missing (run `just licence-texts-fetch`)"]
    rec = tomllib.loads(rec_path.read_text(encoding="utf-8"))
    errs = []
    for fname, entry in sorted(rec.get("text", {}).items()):
        p = texts_dir / fname
        if not p.is_file():
            errs.append(f"{fname}: recorded but missing")
        elif hashlib.sha256(p.read_bytes()).hexdigest() != entry.get("sha256"):
            errs.append(f"{fname}: sha256 differs from SOURCE.toml (a fallback text changed by hand)")
    for p in sorted(texts_dir.glob("*.txt")):
        if p.name not in rec.get("text", {}):
            errs.append(f"{p.name}: a text SOURCE.toml does not record (run `just licence-texts-fetch`)")
    return errs


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("artifact", nargs="?")
    ap.add_argument("out", nargs="?")
    ap.add_argument("--check", action="store_true", help="the cheap gate")
    ap.add_argument("--set", action="append", default=[], metavar="ECO.KEY=VALUE",
                    help="override a manifest input for this run (e.g. go.binary=/path/to/bridge)")
    ap.add_argument("--host-target", metavar="TRIPLE",
                    help="the target a root group naming none resolves for (default: the host's)")
    ap.add_argument("--target", action="append", dest="targets", metavar="TRIPLE",
                    help="resolve only for this triple (repeatable): the file is for the binary built for it "
                         "(default: every triple the manifest entry names)")
    a = ap.parse_args(argv)
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    allow = load_allow()
    if a.check:
        errs = check(manifest, allow, TEXTS, GOAL_DOC) + check_texts_record()
        for e in errs:
            print(f"shipped-notices-check: {e}")
        print(f"shipped-notices-check: {'FAIL' if errs else 'OK'} — {len(manifest.get('artifact', {}))} artifacts, {len(allow)} allowlist ids")
        return 1 if errs else 0
    if not a.artifact or not a.out:
        ap.print_usage(sys.stderr)
        return 2
    for kv in a.set:
        key, _, value = kv.partition("=")
        eco, _, field_ = key.partition(".")
        manifest.setdefault("artifact", {}).setdefault(a.artifact, {}).setdefault(eco, {})[field_] = value
    try:
        res = generate(a.artifact, manifest, allow=allow, texts_dir=TEXTS, version=workspace_version(), host_target=a.host_target, targets=a.targets)
    except RuntimeError as e:
        print(f"shipped-notices: {a.artifact} FAILED — {e}")
        return 1
    except FileNotFoundError as e:
        print(f"shipped-notices: {a.artifact} FAILED — input missing: {safe(str(e.filename))} (build the artifact first, or name the input with --set)")
        return 1
    for v in res.verdicts:
        print(v)
    if not res.ok:
        return 1
    out = Path(a.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(res.text, encoding="utf-8")
    print(f"shipped-notices: wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
