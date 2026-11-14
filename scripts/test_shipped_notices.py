"""Tests for scripts/shipped-notices.py — the notices generator.

Every leg runs over a FIXTURE closure: the resolver output (`cargo tree`, `cargo
metadata`, `go version -m`, a deno lockfile, `project.assets.json`, a Gradle
dependency report) is canned, and the package caches are temporary directories
holding licence files the test wrote itself. Nothing here runs a resolver or
opens a real package cache. The assertions read the generator's verdict and
the generated file's STRUCTURE (its entry labels, the dedup references, the
closing section), which is what a session may read
(docs/goal/architecture/shipped-notices.md § How the notices file is produced).
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tomllib
import zipfile
from pathlib import Path

import pytest

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("shipped_notices", _HERE / "shipped-notices.py")
sn = importlib.util.module_from_spec(_spec)
sys.modules["shipped_notices"] = sn
_spec.loader.exec_module(sn)

ALLOW = {"MIT", "Apache-2.0", "BSD-3-Clause", "MPL-2.0", "LGPL-2.1-or-later", "Apache-2.0 WITH LLVM-exception", "ISC", "BSD-2-Clause"}


# --- licence expressions ---------------------------------------------------------


@pytest.mark.parametrize(
    "expr,ok",
    [
        ("MIT", True),
        ("MIT OR Apache-2.0", True),
        ("MIT/Apache-2.0", True),
        ("GPL-3.0 OR MIT", True),
        ("GPL-3.0", False),
        ("MIT AND GPL-3.0", False),
        ("(MIT OR Apache-2.0) AND BSD-3-Clause", True),
        ("Apache-2.0 WITH LLVM-exception", True),
        ("Apache-2.0 WITH Classpath-exception-2.0", False),
        ("LGPL-2.1+", True),
        ("", False),
        ("MIT AND", False),
    ],
)
def test_licence_allowed(expr, ok):
    assert sn.licence_allowed(expr, ALLOW) is ok


def test_fallback_ids_follow_the_satisfying_branches():
    assert sn.fallback_ids("GPL-3.0 OR MIT", ALLOW) == ["MIT"]
    assert sn.fallback_ids("(MIT OR Apache-2.0) AND ISC", ALLOW) == ["MIT", "Apache-2.0", "ISC"]
    assert sn.fallback_ids("Apache-2.0 WITH LLVM-exception", ALLOW) == ["Apache-2.0 WITH LLVM-exception"]


def test_text_file_name_for_a_with_expression():
    assert sn.text_file_name("Apache-2.0 WITH LLVM-exception") == "Apache-2.0-WITH-LLVM-exception.txt"
    assert sn.text_file_name("MIT") == "MIT.txt"


# --- fixtures ----------------------------------------------------------------------


def _texts(tmp: Path) -> Path:
    d = tmp / "licence-texts"
    d.mkdir()
    for i in sorted(ALLOW):
        (d / sn.text_file_name(i)).write_text(f"canonical text of {i}\n")
    return d


def _pkg(root: Path, name: str, files: dict[str, bytes]) -> Path:
    d = root / name
    d.mkdir(parents=True)
    for fname, data in files.items():
        p = d / fname
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
    return d


DPKG_LIBC6_DEV = "ii \t2.43-2ubuntu2.4\tglibc\t2.43-2ubuntu2.4"
RUSTC_VV = "\n".join([
    "rustc 1.101.0-nightly (2026-10-19)",
    "binary: rustc",
    "commit-hash: 0000000000000000000000000000000000000abc",
    "commit-date: 2026-10-19",
    "host: aarch64-unknown-linux-gnu",
    "release: 1.101.0-nightly",
    "LLVM version: 23.1.4",
    "",
])
TOOLCHAIN: dict[str, Path] = {}


@pytest.fixture(autouse=True)
def _fixture_toolchain(tmp_path_factory):
    """A fixture Rust sysroot and GOROOT holding the toolchain files the
    generator copies, which every Runner answers `rustc`/`go env` with."""
    root = tmp_path_factory.mktemp("toolchain")
    sysroot = root / "sysroot"
    _pkg(sysroot, "share/doc/rust", {"COPYRIGHT-library.html": b"<html>fixture std notice</html>\n"})
    goroot = _pkg(root, "goroot", {"LICENSE": b"fixture go licence\n"})
    libc = root / "libc"
    _pkg(libc, "usr/share/doc/libc6-dev", {"copyright": b"fixture libc6-dev record\n"})
    _pkg(libc, "usr/share/common-licenses", {"LGPL-2.1": b"fixture LGPL-2.1 text\n"})
    _pkg(libc, "etc", {"os-release": b'NAME="Ubuntu"\nID=ubuntu\n'})
    TOOLCHAIN.update(sysroot=sysroot, goroot=goroot, libc=libc)
    old_root, sn.LIBC_ROOT = sn.LIBC_ROOT, libc
    yield
    sn.LIBC_ROOT = old_root


class Runner:
    """A canned resolver: argv prefix → stdout. A toolchain query no answer
    names gets the fixture toolchain: `rustc -vV`, the sysroot, and the Go
    version the first `go version -m` answer recorded with its GOROOT."""

    def __init__(self, answers):
        self.answers = answers
        self.calls = []
        self.go_version = None

    def __call__(self, argv, env=None):
        self.calls.append((list(argv), dict(env or {})))
        if list(argv[:2]) == ["go", "-C"] and argv[3:4] == ["env"]:
            # `go -C <module> env X`: answered as `go env X`, the module recorded
            self.env_modules = getattr(self, "env_modules", []) + [argv[2]]
            argv = ["go", *argv[3:]]
        for prefix, out in self.answers:
            if list(argv[: len(prefix)]) == list(prefix):
                if list(argv[:3]) == ["go", "version", "-m"] and self.go_version is None and out.strip():
                    self.go_version = out.splitlines()[0].rpartition(": ")[2].split()[0]
                return out
        tool = {
            ("rustc", "-vV"): RUSTC_VV,
            ("rustc", "--print", "sysroot"): f"{TOOLCHAIN['sysroot']}\n",
            ("go", "env", "GOVERSION"): f"{self.go_version}\n",
            ("go", "env", "GOROOT"): f"{TOOLCHAIN['goroot']}\n",
        }.get(tuple(argv))
        if tool is None and list(argv[:1]) == ["dpkg-query"]:
            tool = DPKG_LIBC6_DEV
        if tool is not None:
            return tool
        raise AssertionError(f"unexpected command {argv}")


def _third_party(res):
    return [c for c in res.components if c.ecosystem != sn.TOOLCHAIN]


def _rust_fixture(tmp: Path):
    cache = tmp / "registry"
    repo = tmp / "repo"
    serde = _pkg(cache, "serde-1.0.0", {"Cargo.toml": b"", "LICENSE-MIT": b"MIT text, serde authors\n", "LICENSE-APACHE": b"apache text\n"})
    itoa = _pkg(cache, "itoa-1.0.0", {"Cargo.toml": b"", "LICENSE-APACHE": b"apache text\n"})
    bare = _pkg(cache, "bare-0.2.0", {"Cargo.toml": b""})
    mpl = _pkg(cache, "webpki-roots-0.26.0", {"Cargo.toml": b"", "LICENSE": b"MPL text\n"})
    fork = _pkg(repo / "vendor", "openmls", {"Cargo.toml": b"", "LICENSE": b"openmls MIT text\n"})
    own = _pkg(repo / "apps", "fauna-tui", {"Cargo.toml": b""})
    reg = "registry+https://github.com/rust-lang/crates.io-index"
    pkgs = [
        {"id": "own", "name": "fauna-tui", "version": "0.1.6", "source": None, "license": "MIT OR Apache-2.0", "license_file": None, "manifest_path": str(own / "Cargo.toml")},
        {"id": "serde", "name": "serde", "version": "1.0.0", "source": reg, "license": "MIT OR Apache-2.0", "license_file": None, "manifest_path": str(serde / "Cargo.toml")},
        {"id": "itoa", "name": "itoa", "version": "1.0.0", "source": reg, "license": "MIT OR Apache-2.0", "license_file": None, "manifest_path": str(itoa / "Cargo.toml")},
        {"id": "bare", "name": "bare", "version": "0.2.0", "source": reg, "license": "MIT", "license_file": None, "manifest_path": str(bare / "Cargo.toml")},
        {"id": "mpl", "name": "webpki-roots", "version": "0.26.0", "source": reg, "license": "MPL-2.0", "license_file": None, "manifest_path": str(mpl / "Cargo.toml")},
        {"id": "fork", "name": "openmls", "version": "0.9.0", "source": None, "license": "MIT", "license_file": None, "manifest_path": str(fork / "Cargo.toml")},
    ]
    meta = json.dumps({"packages": pkgs, "workspace_members": ["own"]})
    tree = "\n".join(
        [
            f"fauna-tui v0.1.6 ({own})",
            "serde v1.0.0",
            "itoa v1.0.0",
            "serde v1.0.0 (*)",
            "bare v0.2.0",
            "webpki-roots v0.26.0",
            f"openmls v0.9.0 ({fork})",
            "",
        ]
    )
    runner = Runner([(["cargo", "fetch"], ""), (["cargo", "tree"], tree), (["cargo", "metadata"], meta)])
    return runner


def _gen(tmp, manifest_artifact, runner, exceptions=(), allow=ALLOW, **kw):
    manifest = {"artifact": {"a": manifest_artifact}, "exception": list(exceptions)}
    return sn.generate(
        "a",
        manifest,
        allow=allow,
        texts_dir=_texts(tmp) if not (tmp / "licence-texts").exists() else tmp / "licence-texts",
        version="0.1.6",
        run=runner,
        go_ids={},
        repo=tmp / "repo",
        **kw,
    )


# --- the Rust leg -------------------------------------------------------------------


def test_rust_leg_closure_dedup_fallback_and_closing_section(tmp_path):
    runner = _rust_fixture(tmp_path)
    res = _gen(tmp_path, {"name": "the terminal archive", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, runner)
    assert res.ok, res.verdicts
    names = [c.name for c in _third_party(res)]
    # workspace members out, the vendored fork in, the (*) repeat once
    assert names == ["bare", "itoa", "openmls", "serde", "webpki-roots"]
    text = res.text
    # the five, the Rust standard library, and the C library a host (linux-gnu) link carries
    assert text.count("Component: ") == 7
    # the shared apache text printed once and referred to by the second entry
    assert text.count("apache text\n") == 1
    assert "same text as" in text
    # a component with no file of its own gets the canonical text, marked
    assert "No licence file was copied" in text
    assert "canonical text of MIT" in text
    # the closing section lists the MPL component with its source address
    closing = text.split(sn.CLOSING_HEADING, 1)[1]
    assert "webpki-roots 0.26.0" in closing
    assert "https://crates.io/crates/webpki-roots/0.26.0" in closing
    assert "serde" not in closing
    v = "\n".join(res.verdicts)
    assert "rust: components=5 files-copied=" in v and "fallbacks=1" in v and "refused=0" in v


def test_rust_leg_resolves_per_root_with_selected_unification_and_no_proc_macros(tmp_path):
    runner = _rust_fixture(tmp_path)
    _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "features": ["full"], "target": "x86_64-pc-windows-msvc"}}, runner)
    argv, env = next(c for c in runner.calls if c[0][:2] == ["cargo", "tree"])
    assert "normal,no-proc-macro" in argv
    assert argv[argv.index("--target") + 1] == "x86_64-pc-windows-msvc"
    assert argv[argv.index("--features") + 1] == "full"
    assert "--locked" in argv and "--offline" in argv
    assert env.get("CARGO_RESOLVER_FEATURE_UNIFICATION") == "selected"


def test_rust_leg_asks_cargo_tree_for_plain_output(tmp_path):
    """A runner that forces colour (CARGO_TERM_COLOR=always) styles the tree's ` (*)` marker;
    the public CI's macOS job read `generic-array v0.14.7 <ESC>[33m…(*)…` as unparsed (0.1.7)."""
    runner = _rust_fixture(tmp_path)
    _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, runner)
    argv, env = next(c for c in runner.calls if c[0][:2] == ["cargo", "tree"])
    assert argv[argv.index("--color") + 1] == "never"
    assert env.get("CARGO_TERM_COLOR") == "never"


def test_rust_leg_fetches_the_locked_set_once_before_resolving_offline(tmp_path):
    # `cargo metadata` reads the whole lock, and a build fetches only its own
    # targets' packages: without the fetch, the offline resolve fails on any
    # package no build on this machine downloaded. The fetch takes the locked,
    # checksummed bytes and moves no version.
    runner = _rust_fixture(tmp_path)
    groups = [{"packages": ["fauna-tui"], "targets": ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]}, {"packages": ["fauna-tui"]}]
    _gen(tmp_path, {"name": "n", "channel": "x", "rust": groups}, runner)
    argvs = [c[0] for c in runner.calls]
    assert argvs[0] == ["cargo", "fetch", "--locked"]
    assert sum(a[:2] == ["cargo", "fetch"] for a in argvs) == 1
    assert all("--offline" in a for a in argvs if a[:2] in (["cargo", "tree"], ["cargo", "metadata"]))


def test_refused_licence_fails_unless_excepted(tmp_path):
    runner = _rust_fixture(tmp_path)
    allow = ALLOW - {"MPL-2.0"}
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, runner, allow=allow)
    assert not res.ok
    assert any("REFUSED rust webpki-roots@0.26.0" in v for v in res.verdicts)
    res2 = sn.generate(
        "a",
        {"artifact": {"a": {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}},
         "exception": [{"ecosystem": "rust", "name": "webpki-roots", "licence": "MPL-2.0", "reading": "test"}]},
        allow=allow, texts_dir=tmp_path / "licence-texts", version="0.1.6", run=runner, go_ids={}, repo=tmp_path / "repo",
    )
    assert res2.ok, res2.verdicts


def test_copy_refusals_leave_out_and_report(tmp_path):
    cache = tmp_path / "registry"
    reg = "registry+https://github.com/rust-lang/crates.io-index"
    big = _pkg(cache, "big-1.0.0", {"Cargo.toml": b"", "LICENSE": b"a" * (sn.MAX_FILE + 1), "NOTICE": b"ok notice\n"})
    nul = _pkg(cache, "nul-1.0.0", {"Cargo.toml": b"", "LICENSE": b"bad\x00text"})
    latin = _pkg(cache, "latin-1.0.0", {"Cargo.toml": b"", "COPYING": b"caf\xe9\n"})
    pkgs = [
        {"id": n, "name": n, "version": "1.0.0", "source": reg, "license": "MIT", "license_file": None, "manifest_path": str(d / "Cargo.toml")}
        for n, d in (("big", big), ("nul", nul), ("latin", latin))
    ]
    runner = Runner([(["cargo", "fetch"], ""), (["cargo", "tree"], "big v1.0.0\nnul v1.0.0\nlatin v1.0.0\n"), (["cargo", "metadata"], json.dumps({"packages": pkgs, "workspace_members": []}))])
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["big"]}}, runner)
    assert res.ok
    v = "\n".join(res.verdicts)
    assert "left out rust big@1.0.0 LICENSE: over 256 KiB" in v
    assert "left out rust nul@1.0.0 LICENSE: carries a NUL" in v
    assert "left out rust latin@1.0.0 COPYING: not valid UTF-8" in v
    assert "left-out=3" in v
    # big kept its NOTICE, which is not its licence: all three fell back to
    # the canonical text
    assert "fallbacks=3" in v
    assert "ok notice" in res.text


def test_missing_fallback_text_fails(tmp_path):
    runner = _rust_fixture(tmp_path)
    texts = _texts(tmp_path)
    (texts / "MIT.txt").unlink()
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, runner)
    assert not res.ok
    assert any("no fallback text for MIT" in v for v in res.verdicts)


# --- the Go leg ---------------------------------------------------------------------


def test_go_leg_reads_the_binary_module_list_and_the_repository_table(tmp_path):
    modcache = tmp_path / "gomod"
    _pkg(modcache, "github.com/!burnt!sushi/toml@v1.3.0", {"COPYING": b"toml mit\n"})
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    _pkg(modcache, "example.com/fork@v0.0.1", {"LICENSE": b"fork text\n"})
    out = "\n".join(
        [
            "/x/fauna-mail-bridge: go1.22.0",
            # `go version -m` lines are tab-separated, a leading tab first
            "\t".join(["", "path", "fauna/bridges"]),
            "\t".join(["", "mod", "fauna/bridges", "(devel)", ""]),
            "\t".join(["", "dep", "github.com/BurntSushi/toml", "v1.3.0", "h1:abc="]),
            "\t".join(["", "dep", "golang.org/x/net", "v0.19.0", "h1:abc="]),
            "\t".join(["", "=>", "golang.org/x/net", "v0.20.0", "h1:def="]),
            "\t".join(["", "dep", "example.com/fork", "v0.0.1", "h1:abc="]),
            "\t".join(["", "build", "-compiler=gc"]),
            "",
        ]
    )
    runner = Runner([(["go", "version", "-m"], out)])
    (tmp_path / "bridge").write_bytes(b"\x7fELF")  # the built binary must exist
    go_ids = {"github.com/BurntSushi/toml": "MIT", "golang.org/x/net": "BSD-3-Clause"}
    (tmp_path / "bridge").write_bytes(b"")  # the leg refuses a binary that is not there
    res = sn.generate(
        "a",
        {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binary": "bridge", "modcache": str(modcache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=runner, go_ids=go_ids, repo=tmp_path,
    )
    assert not res.ok  # example.com/fork has no id in the table
    assert any("REFUSED go example.com/fork@v0.0.1: licence none" in v for v in res.verdicts)
    names = {(c.name, c.version) for c in _third_party(res)}
    assert ("golang.org/x/net", "v0.20.0") in names  # the replacement wins
    assert ("fauna/bridges", "(devel)") not in names  # the main module is Fauna's own
    assert "toml mit" in res.text


def test_go_table_parser_reads_only_the_go_section(tmp_path):
    doc = tmp_path / "N.md"
    doc.write_text(
        "## Vendored\n\n| Path | Licence |\n|---|---|\n| `x/y` | MIT |\n\n"
        "## Go modules (`bins/fauna-bridges`)\n\n| Module path | Licence |\n|---|---|\n"
        "| `golang.org/x/net` | BSD-3-Clause |\n| `cloud.google.com/go` | Apache-2.0 OR BSD-3-Clause |\n| `z/u` | unverified |\n\n## Web\n| `a/b` | MIT |\n"
    )
    ids = sn.parse_go_table(doc)
    assert ids == {"golang.org/x/net": "BSD-3-Clause", "cloud.google.com/go": "Apache-2.0 OR BSD-3-Clause"}


def test_vendored_table_parser_reads_each_paths_licence_id(tmp_path):
    doc = tmp_path / "N.md"
    doc.write_text(
        "## Vendored source\n\n| Path | Origin | Licence | Notes |\n|---|---|---|---|\n"
        "| `libs/ksni` | a fork | Unlicense (`libs/ksni/UNLICENSE`) | kept |\n"
        "| `bins/b/third_party/go-imap` | `github.com/emersion/go-imap/v2`, forked | MIT (`bins/b/third_party/go-imap/LICENSE`) | seams |\n\n"
        "## Go modules (`bins/b`)\n\n| Module path | Licence |\n|---|---|\n| `golang.org/x/net` | BSD-3-Clause |\n"
    )
    ids = sn.parse_go_table(doc)
    assert ids["libs/ksni"] == "Unlicense" and ids["bins/b/third_party/go-imap"] == "MIT"


def _go_version_m(*mods: tuple) -> str:
    """`go version -m` output: each mod is (path, version) or (path, version, to, to_version)."""
    lines = ["/x/fauna-mail-bridge.exe: go1.22.0", "\t".join(["", "path", "github.com/faunasocial/fauna/bins/b"]),
             "\t".join(["", "mod", "github.com/faunasocial/fauna/bins/b", "(devel)", ""])]
    for m in mods:
        lines.append("\t".join(["", "dep", m[0], m[1], "h1:abc=" if len(m) == 2 else ""]))
        if len(m) == 4:
            lines.append("\t".join(["", "=>", m[2], m[3], ""]))
    return "\n".join(lines + [""])


def test_go_leg_resolves_a_local_fork_against_its_module_and_leaves_faunas_own_out(tmp_path):
    # The windows build stages the bridge in target/, away from its module: a
    # local `replace` path is relative to the module's directory, never the
    # binary's; a module under Fauna's own path is Fauna's, like a workspace member.
    repo = tmp_path / "repo"
    _pkg(repo / "bins" / "b" / "third_party", "go-imap", {"LICENSE": b"go-imap fork text\n"})
    _pkg(repo / "libs", "fauna-mail-go", {"go.mod": b""})
    (repo / "target").mkdir()
    (repo / "target" / "bridge.exe").write_bytes(b"MZ")
    modcache = tmp_path / "gomod"
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    out = _go_version_m(
        ("github.com/emersion/go-imap/v2", "v2.0.0-beta.5", "./third_party/go-imap", "(devel)"),
        ("github.com/faunasocial/fauna/libs/fauna-mail-go", "v0.0.0", "../../libs/fauna-mail-go", "(devel)"),
        ("golang.org/x/net", "v0.20.0"),
    )
    ids = {"bins/b/third_party/go-imap": "MIT", "golang.org/x/net": "BSD-3-Clause"}
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binaries": ["target/bridge.exe"], "module": "bins/b", "modcache": str(modcache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([(["go", "version", "-m"], out)]), go_ids=ids, repo=repo,
    )
    assert res.ok, res.verdicts
    got = {(c.name, c.version, c.origin) for c in _third_party(res)}
    assert got == {
        ("github.com/emersion/go-imap/v2", "v2.0.0-beta.5", "the Fauna source repository, bins/b/third_party/go-imap"),
        ("golang.org/x/net", "v0.20.0", "golang.org/x/net"),
    }
    assert "go-imap fork text" in res.text


def test_go_leg_downloads_only_the_linked_modules_the_cache_lacks(tmp_path):
    # A release job that did not build the binary has an empty module cache: the
    # leg fetches exactly the missing linked modules, inside the main module (so
    # go.sum checks them), and nothing it already holds.
    modcache = tmp_path / "gomod"
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    (tmp_path / "b").mkdir()
    (tmp_path / "bridge").write_bytes(b"MZ")
    out = _go_version_m(("golang.org/x/net", "v0.20.0"), ("golang.org/x/text", "v0.14.0"))

    class Fetch(Runner):
        def __call__(self, argv, env=None):
            if argv[:2] == ["go", "-C"] and argv[3:5] == ["mod", "download"]:
                _pkg(modcache, "golang.org/x/text@v0.14.0", {"LICENSE": b"text licence\n"})
            return super().__call__(argv, env)

    runner = Fetch([(["go", "version", "-m"], out), (["go", "-C"], "")])
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binary": "bridge", "module": "b", "modcache": str(modcache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=runner,
        go_ids={"golang.org/x/net": "BSD-3-Clause", "golang.org/x/text": "BSD-3-Clause"}, repo=tmp_path,
    )
    assert res.ok, res.verdicts
    fetches = [c[0] for c in runner.calls if c[0][:2] == ["go", "-C"] and "download" in c[0]]
    assert fetches == [["go", "-C", str(tmp_path / "b"), "mod", "download", "golang.org/x/text@v0.14.0"]]
    assert "text licence" in res.text


def test_go_leg_refuses_a_module_the_download_did_not_bring(tmp_path):
    # An absent module would otherwise read as one with no licence file of its
    # own and print the canonical text in place of its copyright lines.
    (tmp_path / "bridge").write_bytes(b"MZ")
    out = _go_version_m(("golang.org/x/net", "v0.20.0"))
    with pytest.raises(FileNotFoundError):
        sn.generate(
            "a", {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binary": "bridge", "modcache": str(tmp_path / "gomod")}}}},
            allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([(["go", "version", "-m"], out), (["go", "-C"], "")]),
            go_ids={"golang.org/x/net": "BSD-3-Clause"}, repo=tmp_path,
        )


def test_a_per_run_binary_stands_in_for_the_manifest_list(tmp_path):
    # `--set go.binary=<path>` names where this run's build put the bridge; the
    # manifest's default path need not exist then.
    modcache = tmp_path / "gomod"
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    (tmp_path / "parts").mkdir()
    (tmp_path / "parts" / "bridge.exe").write_bytes(b"MZ")
    runner = Runner([(["go", "version", "-m"], _go_version_m(("golang.org/x/net", "v0.20.0")))])
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binaries": ["target/absent.exe"], "binary": "parts/bridge.exe", "modcache": str(modcache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=runner, go_ids={"golang.org/x/net": "BSD-3-Clause"}, repo=tmp_path,
    )
    assert res.ok, res.verdicts
    assert [c[0][-1] for c in runner.calls if c[0][:3] == ["go", "version", "-m"]] == [str(tmp_path / "parts" / "bridge.exe")]


# --- the toolchain's runtime code ---------------------------------------------------


def _stdlib(res, name):
    return [c for c in res.components if c.ecosystem == sn.TOOLCHAIN and c.name == name]


def test_a_rust_artifact_gains_one_standard_library_entry_from_the_toolchain(tmp_path):
    runner = _rust_fixture(tmp_path)
    groups = [{"packages": ["fauna-tui"], "targets": ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]}, {"packages": ["fauna-tui"]}]
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": groups}, runner)
    assert res.ok, res.verdicts
    (std,) = _stdlib(res, sn.RUST_STDLIB)
    assert std.version == "1.101.0-nightly (commit 0000000000000000000000000000000000000abc)"
    assert std.licence == "MIT OR Apache-2.0"
    entry = res.text.split(f"Component: {sn.RUST_STDLIB}", 1)[1]
    assert "Licence: MIT OR Apache-2.0" in entry
    assert "--- COPYRIGHT-library.html [text " in entry and "fixture std notice" in entry
    assert sum(a[:2] == ["rustc", "-vV"] for a, _ in runner.calls) == 1
    v = "\n".join(res.verdicts)
    # the host group links for the fixture's aarch64-unknown-linux-gnu: std and the C library
    assert "a toolchain: components=2 files-copied=3 fallbacks=0 left-out=0 refused=0" in v
    assert not _stdlib(res, sn.GO_STDLIB)


def test_the_rust_notice_passes_under_its_own_ceiling_and_no_other_file_does(tmp_path):
    big = b"<p>" + b"a" * (sn.MAX_FILE + 10) + b"</p>\n"
    (TOOLCHAIN["sysroot"] / sn.RUST_STDLIB_FILE).write_bytes(big)
    cache = tmp_path / "registry"
    reg = "registry+https://github.com/rust-lang/crates.io-index"
    d = _pkg(cache, "big-1.0.0", {"Cargo.toml": b"", "LICENSE": big})
    pkgs = [{"id": "big", "name": "big", "version": "1.0.0", "source": reg, "license": "MIT", "license_file": None, "manifest_path": str(d / "Cargo.toml")}]
    runner = Runner([(["cargo", "fetch"], ""), (["cargo", "tree"], "big v1.0.0\n"), (["cargo", "metadata"], json.dumps({"packages": pkgs, "workspace_members": []}))])
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["big"]}}, runner)
    assert res.ok, res.verdicts
    v = "\n".join(res.verdicts)
    assert "left out rust big@1.0.0 LICENSE: over 256 KiB" in v
    assert "toolchain: components=2 files-copied=3 fallbacks=0 left-out=0" in v
    assert res.text.count("a" * 1000) >= 1  # the notice was copied
    # ... and the larger ceiling still holds a limit
    assert sn.vet_bytes(sn.STDLIB_MAX_FILE, lambda: b"", sn.STDLIB_MAX_FILE) == (None, "over 4 MiB")


def test_a_toolchain_without_the_rust_notice_fails_the_run(tmp_path):
    (TOOLCHAIN["sysroot"] / sn.RUST_STDLIB_FILE).unlink()
    with pytest.raises(FileNotFoundError):
        _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, _rust_fixture(tmp_path))


# --- the GNU C Library's statically linked part (the toolchain bullet, Owed (f)) ---


def _linux_art(triples):
    return {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "targets": list(triples)}}


def test_a_linux_gnu_artifact_gains_the_c_library_entry(tmp_path):
    res = _gen(tmp_path, _linux_art(["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]), _rust_fixture(tmp_path))
    assert res.ok, res.verdicts
    (libc,) = _stdlib(res, sn.GLIBC)
    assert (libc.version, libc.licence) == ("2.43-2ubuntu2.4", "LGPL-2.1-or-later")
    entry = res.text.split(f"Component: {sn.GLIBC}", 1)[1]
    assert "--- copyright [text " in entry and "fixture libc6-dev record" in entry
    assert "--- LGPL-2.1 [text " in entry and "fixture LGPL-2.1 text" in entry
    # LGPL-2.1 asks for the source: the closing section names the distribution's source package
    closing = res.text.split(sn.CLOSING_HEADING, 1)[1]
    assert f"{sn.GLIBC} 2.43-2ubuntu2.4" in closing
    assert "https://launchpad.net/ubuntu/+source/glibc/2.43-2ubuntu2.4" in closing
    assert "toolchain: components=2 files-copied=3 fallbacks=0 left-out=0 refused=0" in "\n".join(res.verdicts)


def test_a_host_group_on_a_linux_gnu_host_target_gains_it(tmp_path):
    res = _gen(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"]}}, _rust_fixture(tmp_path), host_target="x86_64-unknown-linux-gnu")
    assert res.ok and len(_stdlib(res, sn.GLIBC)) == 1


@pytest.mark.parametrize("triple", ["aarch64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-linux-android", "wasm32-unknown-unknown"])
def test_no_other_triple_gains_the_c_library_entry(tmp_path, triple):
    runner = _rust_fixture(tmp_path)
    res = _gen(tmp_path, _linux_art([triple]), runner, which=_ndk(tmp_path))
    assert res.ok, res.verdicts
    assert not _stdlib(res, sn.GLIBC)
    assert not any(a[:1] == ["dpkg-query"] for a, _ in runner.calls)


def test_a_debian_job_names_the_debian_source_package(tmp_path):
    (TOOLCHAIN["libc"] / "etc" / "os-release").write_bytes(b'ID=debian\nVERSION_CODENAME=bookworm\n')
    runner = _rust_fixture(tmp_path)
    runner.answers.append((["dpkg-query"], "ii \t2.36-9+deb12u10\tglibc\t2.36-9+deb12u10"))
    res = _gen(tmp_path, _linux_art(["aarch64-unknown-linux-gnu"]), runner)
    (libc,) = _stdlib(res, sn.GLIBC)
    assert libc.source == "https://snapshot.debian.org/package/glibc/2.36-9+deb12u10/"


@pytest.mark.parametrize("missing", ["record", "licence", "package", "distribution"])
def test_a_job_without_the_c_library_record_fails_the_run(tmp_path, missing):
    libc = TOOLCHAIN["libc"]
    runner = _rust_fixture(tmp_path)
    if missing == "record":
        (libc / sn.GLIBC_RECORD).unlink()
    elif missing == "licence":
        (libc / sn.GLIBC_LICENCE_FILE).unlink()
    elif missing == "package":
        runner.answers.append((["dpkg-query"], "un \t\t\t"))
    else:
        (libc / "etc" / "os-release").write_bytes(b"ID=gentoo\n")
    with pytest.raises((FileNotFoundError, RuntimeError)):
        _gen(tmp_path, _linux_art(["aarch64-unknown-linux-gnu"]), runner)


@pytest.mark.parametrize("cgo,goos,want", [("1", "linux", 1), ("0", "linux", 0), ("1", "darwin", 0)])
def test_a_linux_cgo_go_binary_gains_the_c_library_entry_and_no_other_go_binary_does(tmp_path, cgo, goos, want):
    art, answers = _go_art(tmp_path, ["go1.26.11"])
    answers = [(pre, out + f"\tbuild\tCGO_ENABLED={cgo}\n\tbuild\tGOOS={goos}\n") if pre[:3] == ["go", "version", "-m"] else (pre, out) for pre, out in answers]
    res, _ = _gen_go(tmp_path, art, answers)
    assert res.ok, res.verdicts
    assert len(_stdlib(res, sn.GLIBC)) == want


ANDROID_TRIPLES = ["aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android", "i686-linux-android"]


def _ndk(tmp: Path, name="ndk", notice=b"fixture bionic notice\n", props=b"Pkg.Desc = Android NDK\nPkg.Revision = 28.2.13676358\nPkg.ReleaseName = r28c\n"):
    """A fixture NDK: its compiler drivers, `source.properties` and sysroot
    notice; returns the `which` that finds its drivers."""
    root = tmp / name
    prebuilt = root / "toolchains" / "llvm" / "prebuilt" / "linux-x86_64"
    (prebuilt / "bin").mkdir(parents=True)
    for tool in sn.NDK_CLANG.values():
        (prebuilt / "bin" / tool).write_bytes(b"#!/bin/sh\n")
    if props is not None:
        (root / "source.properties").write_bytes(props)
    if notice is not None:
        (prebuilt / "sysroot").mkdir()
        (prebuilt / "sysroot" / "NOTICE").write_bytes(notice)
    return lambda tool: str(prebuilt / "bin" / tool)


def _android_art(triples=ANDROID_TRIPLES):
    return {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "targets": list(triples)}}


def test_an_android_artifact_gains_the_ndk_sysroot_entry(tmp_path):
    res = _gen(tmp_path, _android_art(), _rust_fixture(tmp_path), which=_ndk(tmp_path))
    assert res.ok, res.verdicts
    (ndk,) = _stdlib(res, sn.NDK_SYSROOT)
    assert (ndk.version, ndk.licence) == ("r28c (28.2.13676358)", "BSD-2-Clause")
    entry = res.text.split(f"Component: {sn.NDK_SYSROOT}", 1)[1]
    assert "--- NOTICE [text " in entry and "fixture bionic notice" in entry
    assert "toolchain: components=2 files-copied=2 fallbacks=0 left-out=0 refused=0" in "\n".join(res.verdicts)


def test_one_android_triple_still_gains_the_entry(tmp_path):
    res = _gen(tmp_path, _android_art(), _rust_fixture(tmp_path), which=_ndk(tmp_path), targets=["aarch64-linux-android"])
    assert res.ok, res.verdicts
    assert len(_stdlib(res, sn.NDK_SYSROOT)) == 1


def test_no_other_triple_looks_for_an_ndk(tmp_path):
    art ={"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "targets": ["aarch64-unknown-linux-gnu"]}}
    res = _gen(tmp_path, art, _rust_fixture(tmp_path), which=lambda tool: pytest.fail(f"looked for {tool}"))
    assert res.ok and not _stdlib(res, sn.NDK_SYSROOT)


def test_the_ndk_notice_passes_under_its_own_ceiling(tmp_path):
    big = b"a" * (sn.MAX_FILE + 10) + b"\n"
    res = _gen(tmp_path, _android_art(), _rust_fixture(tmp_path), which=_ndk(tmp_path, notice=big))
    assert res.ok, res.verdicts
    assert "toolchain: components=2 files-copied=2 fallbacks=0 left-out=0" in "\n".join(res.verdicts)
    assert sn.vet_bytes(sn.NDK_SYSROOT_MAX_FILE, lambda: b"", sn.NDK_SYSROOT_MAX_FILE) == (None, "over 1 MiB")


@pytest.mark.parametrize("missing", ["driver", "notice", "properties"])
def test_an_ndk_without_its_driver_notice_or_release_fails_the_run(tmp_path, missing):
    which = _ndk(tmp_path, notice=None if missing == "notice" else b"n\n", props=None if missing == "properties" else b"Pkg.Revision = 28.2.13676358\n")
    if missing == "driver":
        which = lambda tool: None  # noqa: E731
    with pytest.raises(FileNotFoundError):
        _gen(tmp_path, _android_art(), _rust_fixture(tmp_path), which=which)


def test_triples_linking_two_ndks_fail_the_run(tmp_path):
    one, two = _ndk(tmp_path, "ndk-a"), _ndk(tmp_path, "ndk-b")
    which = lambda tool: (one if tool.startswith("aarch64") else two)(tool)  # noqa: E731
    with pytest.raises(RuntimeError, match="2 NDK installations"):
        _gen(tmp_path, _android_art(), _rust_fixture(tmp_path), which=which)


def test_the_five_android_artifacts_resolve_android_triples():
    """The manifest's every Android artifact names Android triples, so each
    gains the NDK entry."""
    manifest = tomllib.loads((sn.REPO / "installer" / "shipped-notices.toml").read_text(encoding="utf-8"))
    android = {k: v for k, v in manifest["artifact"].items() if k.startswith("android-")}
    assert sorted(android) == ["android-foss", "android-kids", "android-release", "android-store-no-payments", "android-store-safe"]
    for name, spec in android.items():
        triples = {t for _, ts in sn.narrow_groups(sn.rust_groups(spec["rust"]), None) for t in ts}
        assert triples and all(t in sn.NDK_CLANG for t in triples), name


def _go_art(tmp, versions):
    modcache = tmp / "gomod"
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    answers, bins = [], []
    for i, v in enumerate(versions):
        b = tmp / f"bridge{i}"
        b.write_bytes(b"MZ")
        bins.append(str(b))
        answers.append((["go", "version", "-m", str(b)], _go_version_m(("golang.org/x/net", "v0.20.0")).replace("go1.22.0", v, 1)))
    return {"name": "n", "channel": "x", "go": {"binaries": bins, "modcache": str(modcache)}}, answers


def _gen_go(tmp, art, answers, goversion=None):
    if goversion is not None:
        answers = answers + [(["go", "env", "GOVERSION"], goversion + "\n")]
    runner = Runner(answers)
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp), version="0.1.6", run=runner,
                      go_ids={"golang.org/x/net": "BSD-3-Clause"}, repo=tmp)
    return res, runner


def test_go_binaries_of_one_version_give_one_standard_library_entry(tmp_path):
    art, answers = _go_art(tmp_path, ["go1.26.11", "go1.26.11"])
    res, _ = _gen_go(tmp_path, art, answers)
    assert res.ok, res.verdicts
    (std,) = _stdlib(res, sn.GO_STDLIB)
    assert (std.version, std.licence) == ("1.26.11", "BSD-3-Clause")
    entry = res.text.split(f"Component: {sn.GO_STDLIB}", 1)[1]
    assert "Licence: BSD-3-Clause" in entry and "fixture go licence" in entry
    assert "toolchain: components=1 files-copied=1" in "\n".join(res.verdicts)
    assert not _stdlib(res, sn.RUST_STDLIB)  # no Rust root, no Rust entry


def test_go_binaries_of_two_versions_give_two_entries_and_a_job_of_another_version_fails(tmp_path):
    art, answers = _go_art(tmp_path, ["go1.26.11", "go1.26.10"])
    # the job's Go matches only one of them: refused, never a licence for the wrong release
    with pytest.raises(RuntimeError, match="records go1.26.10"):
        _gen_go(tmp_path, art, answers, goversion="go1.26.11")


def test_two_go_versions_are_two_entries_each_checked_in_its_own_module(tmp_path):
    # Each distinct version is recorded once, with the main module it was
    # built in; the job's Go is asked inside that module (its go.mod picks the
    # toolchain), so binaries of two modules on two Go versions list two entries.
    modcache = tmp_path / "gomod"
    _pkg(modcache, "golang.org/x/net@v0.20.0", {"LICENSE": b"bsd text\n"})
    answers, bins, by_module = [], [], {}
    for i, v in enumerate(["go1.26.11", "go1.26.10", "go1.26.11"]):
        mod = tmp_path / f"m{i % 2}"
        mod.mkdir(exist_ok=True)
        b = mod / f"bridge{i}"
        b.write_bytes(b"MZ")
        bins.append(str(b))
        by_module[str(mod)] = v
        answers.append((["go", "version", "-m", str(b)], _go_version_m(("golang.org/x/net", "v0.20.0")).replace("go1.22.0", v, 1)))

    class PerModule(Runner):
        def __call__(self, argv, env=None):
            if list(argv[:2]) == ["go", "-C"] and argv[3:] == ["env", "GOVERSION"]:
                self.calls.append((list(argv), {}))
                return by_module[argv[2]] + "\n"
            return super().__call__(argv, env)

    runner = PerModule(answers)
    art = {"name": "n", "channel": "x", "go": {"binaries": bins, "modcache": str(modcache)}}
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=runner,
                      go_ids={"golang.org/x/net": "BSD-3-Clause"}, repo=tmp_path)
    assert res.ok, res.verdicts
    assert [c.version for c in _stdlib(res, sn.GO_STDLIB)] == ["1.26.11", "1.26.10"]
    asked = [a[2] for a, _ in runner.calls if a[:2] == ["go", "-C"] and a[-1] == "GOVERSION"]
    assert asked == [str(tmp_path / "m0"), str(tmp_path / "m1")]
    assert "toolchain: components=2 files-copied=2" in "\n".join(res.verdicts)
    assert sn.go_stdlib_components([], None) == []


def test_a_go_version_mismatch_fails_the_run(tmp_path):
    art, answers = _go_art(tmp_path, ["go1.26.11"])
    with pytest.raises(RuntimeError, match="this job's Go is go1.27.0"):
        _gen_go(tmp_path, art, answers, goversion="go1.27.0")


def test_a_binary_recording_no_go_version_fails_the_run(tmp_path):
    art, answers = _go_art(tmp_path, ["go1.26.11"])
    answers = [(a, out.replace(": go1.26.11", ":", 1)) for a, out in answers]
    with pytest.raises(RuntimeError, match="records no Go version"):
        _gen_go(tmp_path, art, answers)


# --- the npm leg --------------------------------------------------------------------


def test_npm_leg_walks_runtime_dependencies_plus_named_bundled_packages(tmp_path):
    web = tmp_path / "web"
    nm = web / "node_modules" / ".deno"
    own_deps = {"@codemirror+state@6.6.0": {"@marijn/find-cluster-break": "^1"}, "svelte@5.57.1": {}}
    for key, lic in (("@codemirror+state@6.6.0", "MIT"), ("@marijn+find-cluster-break@1.0.2", "MIT"), ("svelte@5.57.1", "MIT"), ("vite@8.3.3", "MIT")):
        name = key.rsplit("@", 1)[0].replace("+", "/")
        meta = {"license": lic, "dependencies": own_deps.get(key, {})}
        _pkg(nm / key / "node_modules", name, {"package.json": json.dumps(meta).encode(), "LICENSE": f"{name} licence\n".encode()})
    (web / "package.json").write_text(json.dumps({"dependencies": {"@codemirror/state": "6.6.0"}, "devDependencies": {"svelte": "^5.57.1", "vite": "8.3.3"}}))
    (web / "deno.lock").write_text(json.dumps({
        "version": "5",
        "specifiers": {"npm:@codemirror/state@6.6.0": "6.6.0", "npm:svelte@5": "5.57.1", "npm:vite@8.3.3": "8.3.3"},
        "npm": {
            "@codemirror/state@6.6.0": {"dependencies": ["@marijn/find-cluster-break"]},
            "@marijn/find-cluster-break@1.0.2": {},
            "svelte@5.57.1": {"dependencies": ["vite"]},  # a peer the lockfile lists
            "vite@8.3.3": {"optionalDependencies": ["@rolldown/binding-x"]},
        },
    }))
    res = sn.generate(
        "a",
        {"artifact": {"a": {"name": "n", "channel": "x", "npm": {"dir": str(web), "bundled": ["svelte"]}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert res.ok, res.verdicts
    assert sorted(c.name for c in res.components) == ["@codemirror/state", "@marijn/find-cluster-break", "svelte"]


def test_excepted_component_with_no_id_and_no_file_gets_the_excepted_ids_standard_text(tmp_path):
    # A package that declares no licence and carries no file of its own, its id
    # recorded by a manifest exception: the excepted id is shown, and its
    # canonical text stands in like any other fallback.
    web = tmp_path / "web"
    nm = web / "node_modules" / ".deno"
    _pkg(nm / "@x+types@0.7.5" / "node_modules", "@x/types", {"package.json": json.dumps({}).encode()})
    (web / "package.json").write_text(json.dumps({"dependencies": {"@x/types": "0.7.5"}}))
    (web / "deno.lock").write_text(json.dumps({"version": "5", "specifiers": {"npm:@x/types@0.7.5": "0.7.5"}, "npm": {"@x/types@0.7.5": {}}}))
    art = {"name": "n", "channel": "x", "npm": {"dir": str(web)}}
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path)
    assert not res.ok
    assert any("REFUSED npm @x/types@0.7.5" in v for v in res.verdicts)
    res = sn.generate(
        "a", {"artifact": {"a": art}, "exception": [{"ecosystem": "npm", "name": "@x/types", "licence": "MIT", "reading": "its source repository is MIT"}]},
        allow=ALLOW, texts_dir=tmp_path / "licence-texts", version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert res.ok, res.verdicts
    assert "Licence: MIT" in res.text
    assert "canonical text of MIT" in res.text
    assert "No licence file was copied" in res.text
    assert any("npm: components=1 files-copied=0 fallbacks=1" in v for v in res.verdicts)


# --- the NuGet leg ------------------------------------------------------------------


def test_nuget_leg_reads_assets_and_nuspec_expressions(tmp_path):
    pkgs = tmp_path / "nuget"
    _pkg(pkgs, "communitytoolkit.mvvm/8.2.2", {
        "communitytoolkit.mvvm.nuspec": b'<?xml version="1.0"?><package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata><id>CommunityToolkit.Mvvm</id><license type="expression">MIT</license></metadata></package>',
        "License.md": b"mvvm licence\n",
    })
    _pkg(pkgs, "microsoft.windowsappsdk/1.6.0", {
        "microsoft.windowsappsdk.nuspec": b'<?xml version="1.0"?><package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata><id>Microsoft.WindowsAppSDK</id><license type="file">license.txt</license></metadata></package>',
        "license.txt": b"microsoft terms\n",
    })
    assets = tmp_path / "project.assets.json"
    assets.write_text(json.dumps({
        "packageFolders": {str(pkgs) + "/": {}},
        "libraries": {
            "CommunityToolkit.Mvvm/8.2.2": {"type": "package", "path": "communitytoolkit.mvvm/8.2.2", "files": ["communitytoolkit.mvvm.nuspec", "License.md", "lib/x.dll"]},
            "Microsoft.WindowsAppSDK/1.6.0": {"type": "package", "path": "microsoft.windowsappsdk/1.6.0", "files": ["microsoft.windowsappsdk.nuspec", "license.txt"]},
            "Microsoft.Windows.SDK.BuildTools/10.0.1": {"type": "package", "path": "microsoft.windows.sdk.buildtools/10.0.1", "files": []},
            "Fauna.Core/1.0.0": {"type": "project", "path": "../x.csproj"},
        },
        # What reaches the published output: a package with a runtime, native,
        # runtimeTargets or resource asset under some target; build tooling
        # (build assets alone, `_._` placeholders) does not.
        "targets": {
            "net10.0": {
                "CommunityToolkit.Mvvm/8.2.2": {"type": "package", "runtime": {"lib/x.dll": {}}, "build": {"b.targets": {}}},
                "Microsoft.WindowsAppSDK/1.6.0": {"type": "package", "compile": {"lib/_._": {}}, "runtime": {"lib/_._": {}}},
                "Microsoft.Windows.SDK.BuildTools/10.0.1": {"type": "package", "build": {"build/x.props": {}}},
            },
            "net10.0/win-arm64": {
                "Microsoft.WindowsAppSDK/1.6.0": {"type": "package", "native": {"runtimes/win-arm64/native/a.dll": {}}},
            },
        },
    }))
    art = {"name": "n", "channel": "x", "nuget": {"assets": str(assets)}}
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path)
    assert not res.ok
    # build tooling is not a component of the published app (and is not refused)
    assert not any("BuildTools" in v and "REFUSED" in v for v in res.verdicts)
    assert any("not shipped nuget Microsoft.Windows.SDK.BuildTools@10.0.1" in v for v in res.verdicts)
    assert any("REFUSED nuget Microsoft.WindowsAppSDK@1.6.0" in v for v in res.verdicts)
    res = sn.generate(
        "a", {"artifact": {"a": art}, "exception": [{"ecosystem": "nuget", "name": "Microsoft.WindowsApp*", "licence": "LicenseRef-Microsoft-WindowsAppSDK", "reading": "Microsoft's own terms"}]},
        allow=ALLOW, texts_dir=tmp_path / "licence-texts", version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert res.ok, res.verdicts
    assert sorted(c.name for c in res.components) == ["CommunityToolkit.Mvvm", "Microsoft.WindowsAppSDK"]
    assert "microsoft terms" in res.text
    assert "Licence: LicenseRef-Microsoft-WindowsAppSDK" in res.text


def _nuspec(pkg_id: str) -> bytes:
    return (b'<?xml version="1.0"?><package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd"><metadata><id>'
            + pkg_id.encode() + b'</id><license type="expression">MIT</license></metadata></package>')


def _runtime_pack_fixture(tmp: Path, *, packs=True, host="restore"):
    """A self-contained publish's inputs: project.assets.json (whose libraries
    never name a runtime pack — a pack is a download dependency) and the
    published app's deps.json per runtime, whose `runtimepack` libraries are
    the packs the publish copied into the output.

    `host` is where the build took the app host (the app's .exe) from: the
    restore's download list (`restore`, the pack in the package folders with
    the files the .NET release puts in each of its packs), or the .NET SDK's
    own packs folder (`sdk`: the restore downloads only another runtime's,
    and the SDK's copy carries no nuspec and no licence file)."""
    pkgs = tmp / "nuget"
    pkgs.mkdir(parents=True)
    release_files = {"LICENSE.TXT": b"runtime licence\n", "THIRD-PARTY-NOTICES.TXT": b"runtime third-party notices\n"}
    if packs:
        _pkg(pkgs, "microsoft.netcore.app.runtime.win-arm64/10.0.8", {
            "microsoft.netcore.app.runtime.win-arm64.nuspec": _nuspec("Microsoft.NETCore.App.Runtime.win-arm64"),
            **release_files,
            "PACKAGE.md": b"not a licence\n",
        })
        _pkg(pkgs, "microsoft.windows.sdk.net.ref/10.0.26100.57", {
            "microsoft.windows.sdk.net.ref.nuspec": _nuspec("Microsoft.Windows.SDK.NET.Ref"),
        })
    downloaded = {"restore": "win-arm64", "sdk": "win-x64"}.get(host)
    if downloaded:
        _pkg(pkgs, f"microsoft.netcore.app.host.{downloaded}/10.0.8", {
            f"microsoft.netcore.app.host.{downloaded}.nuspec": _nuspec(f"Microsoft.NETCore.App.Host.{downloaded}"),
            **release_files,
            f"runtimes/{downloaded}/native/apphost.exe": b"MZ\x00host",
        })
    if host == "sdk":
        _pkg(tmp / "dotnet", "packs/Microsoft.NETCore.App.Host.win-arm64/10.0.8", {
            "runtimes/win-arm64/native/apphost.exe": b"MZ\x00host",
        })
        (tmp / "dotnet" / "dotnet.exe").write_bytes(b"MZ\x00dotnet")
    assets = tmp / "project.assets.json"
    assets.write_text(json.dumps({
        "packageFolders": {str(pkgs) + "/": {}},
        "project": {"frameworks": {"net10.0": {"downloadDependencies": [
            {"name": "Microsoft.NETCore.App.Runtime.win-arm64", "version": "[10.0.8, 10.0.8]"},
            *([{"name": f"Microsoft.NETCore.App.Host.{downloaded}", "version": "[10.0.8, 10.0.8]"}] if downloaded else []),
        ]}}},
        "libraries": {"Fauna.Core/1.0.0": {"type": "project", "path": "../x.csproj"}},
        "targets": {"net10.0": {}, "net10.0/win-arm64": {}},
    }))
    publish = tmp / "bin" / "win-arm64" / "publish"
    publish.mkdir(parents=True)
    (publish / "App.deps.json").write_text(json.dumps({
        "runtimeTarget": {"name": ".NETCoreApp,Version=v10.0/win-arm64"},
        "libraries": {
            "App/1.0.0": {"type": "project"},
            "runtimepack.Microsoft.NETCore.App.Runtime.win-arm64/10.0.8": {"type": "runtimepack", "serviceable": False, "sha512": ""},
            "runtimepack.Microsoft.Windows.SDK.NET.Ref/10.0.26100.57": {"type": "runtimepack", "serviceable": False, "sha512": ""},
            "Fauna.Core/1.0.0": {"type": "project"},
        },
    }))
    deps = str(tmp / "bin" / "{rid}" / "publish" / "App.deps.json")
    return {"name": "n", "channel": "x", "nuget": {"assets": str(assets), "deps": deps}}


def _gen_nuget(tmp, art, targets):
    # `dotnet` resolves to the fixture SDK where one was laid out, else to none
    sdk = tmp / "dotnet" / "dotnet.exe"
    texts = tmp / "licence-texts" if (tmp / "licence-texts").exists() else _texts(tmp)
    return sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=texts, version="0.1.6",
                       run=Runner([]), go_ids={}, repo=tmp, targets=targets,
                       which=lambda tool: str(sdk) if tool == "dotnet" and sdk.is_file() else None)


def _host(res):
    return [c for c in res.components if c.name.startswith("Microsoft.NETCore.App.Host.")]


def test_nuget_leg_lists_the_runtime_packs_a_self_contained_publish_copies(tmp_path):
    # shipped-notices.md § How the notices file is produced, the NuGet bullet: a
    # self-contained publish copies the .NET runtime (and the Windows SDK
    # projection) into the output from runtime packs, which the restore lists
    # as downloads, never as libraries; the published app's deps.json names
    # them. `{rid}` is the runtime of the run's Windows target.
    art = _runtime_pack_fixture(tmp_path)
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc", "aarch64-pc-windows-gnullvm"])
    assert res.ok, res.verdicts
    assert sorted(c.name for c in res.components) == [
        "Microsoft.NETCore.App.Host.win-arm64", "Microsoft.NETCore.App.Runtime.win-arm64", "Microsoft.Windows.SDK.NET.Ref"]
    assert any("nuget: components=3" in v for v in res.verdicts)
    assert any("runtime packs from App.deps.json (.NETCoreApp?Version?v10.0/win-arm64): 2" in v for v in res.verdicts)
    # the pack's own licence AND its third-party notices file are copied
    assert "runtime licence" in res.text and "runtime third-party notices" in res.text
    assert "--- THIRD-PARTY-NOTICES.TXT [text " in res.text
    assert "not a licence" not in res.text
    # a pack carrying no file of its own takes its id's standard text
    assert "the standard text of MIT stands in" in res.text


def test_a_per_runtime_deps_path_needs_a_windows_target(tmp_path):
    # `{rid}` names one runtime's publish: a run that names no Windows target
    # cannot say which, and is refused rather than reading some other one.
    art = _runtime_pack_fixture(tmp_path)
    with pytest.raises(RuntimeError, match="Windows target"):
        _gen_nuget(tmp_path, art, None)


def test_a_runtime_whose_app_was_not_published_fails(tmp_path):
    # the x64 run over an arm64-only publish: no deps.json for win-x64
    art = _runtime_pack_fixture(tmp_path)
    with pytest.raises(FileNotFoundError):
        _gen_nuget(tmp_path, art, ["x86_64-pc-windows-msvc"])


def test_a_runtime_pack_absent_from_the_package_folders_fails(tmp_path):
    art = _runtime_pack_fixture(tmp_path, packs=False)
    with pytest.raises(FileNotFoundError):
        _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])


# --- the .NET app host: shipped-notices.md § How the notices file is produced,
# the toolchain bullet, Owed (c). The app's .exe is the apphost launcher of the
# Microsoft.NETCore.App.Host.<rid> pack with the app's name written into it.


def test_the_app_host_the_restore_downloaded_is_an_entry_with_its_own_files(tmp_path):
    art = _runtime_pack_fixture(tmp_path, host="restore")
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc", "aarch64-pc-windows-gnullvm"])
    assert res.ok, res.verdicts
    assert [(c.name, c.version, c.licence) for c in _host(res)] == [("Microsoft.NETCore.App.Host.win-arm64", "10.0.8", "MIT")]
    assert sorted(label for label, _, _ in _host(res)[0].files) == ["LICENSE.TXT", "THIRD-PARTY-NOTICES.TXT"]
    assert "Component: Microsoft.NETCore.App.Host.win-arm64" in res.text
    # the .NET release's files, byte-identical in its packs, are printed once
    assert res.text.count("--- THIRD-PARTY-NOTICES.TXT [text ") == 1
    assert any(v == "shipped-notices: nuget app host Microsoft.NETCore.App.Host.win-arm64@10.0.8 (downloaded by the restore)" for v in res.verdicts), res.verdicts


def test_the_app_host_the_sdk_supplied_takes_the_runtime_packs_version_and_files(tmp_path):
    # The SDK keeps its host's app host pack in its own packs folder, so the
    # restore downloads none for that runtime (on an arm64 machine, arm64's;
    # on the x64 release runner, x64's) — and that copy carries no nuspec and
    # no licence file. Its version is the .NET runtime pack's the same publish
    # copied, and its id and files are that pack's: the same .NET release.
    art = _runtime_pack_fixture(tmp_path, host="sdk")
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])
    assert res.ok, res.verdicts
    assert [(c.name, c.version, c.licence) for c in _host(res)] == [("Microsoft.NETCore.App.Host.win-arm64", "10.0.8", "MIT")]
    assert sorted(label for label, _, _ in _host(res)[0].files) == ["LICENSE.TXT", "THIRD-PARTY-NOTICES.TXT"]
    assert any(v == ("shipped-notices: nuget app host Microsoft.NETCore.App.Host.win-arm64@10.0.8 (the .NET SDK's own copy, "
                     "with the id and files of Microsoft.NETCore.App.Runtime.win-arm64 10.0.8)") for v in res.verdicts), res.verdicts
    # another runtime's download is not this run's app host
    assert not any("Host.win-x64" in c.name for c in res.components)


def test_an_app_host_the_sdk_does_not_hold_at_the_runtimes_version_fails(tmp_path):
    art = _runtime_pack_fixture(tmp_path, host="sdk")
    pack = tmp_path / "dotnet" / "packs" / "Microsoft.NETCore.App.Host.win-arm64"
    (pack / "10.0.8").rename(pack / "10.0.5")
    with pytest.raises(FileNotFoundError, match="app host"):
        _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])
    # and no dotnet on the job at all
    (tmp_path / "dotnet" / "dotnet.exe").unlink()
    with pytest.raises(FileNotFoundError, match="app host"):
        _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])


def test_an_app_host_neither_downloaded_nor_placed_by_a_runtime_pack_fails(tmp_path):
    # a framework-dependent publish copies no .NET runtime pack, so the version
    # of an app host the restore did not download cannot be read
    art = _runtime_pack_fixture(tmp_path, host="sdk")
    deps = tmp_path / "bin" / "win-arm64" / "publish" / "App.deps.json"
    doc = json.loads(deps.read_text())
    del doc["libraries"]["runtimepack.Microsoft.NETCore.App.Runtime.win-arm64/10.0.8"]
    deps.write_text(json.dumps(doc))
    with pytest.raises(RuntimeError, match="app host"):
        _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])


def test_a_publish_naming_no_runtime_cannot_place_its_app_host(tmp_path):
    art = _runtime_pack_fixture(tmp_path)
    deps = tmp_path / "bin" / "win-arm64" / "publish" / "App.deps.json"
    doc = json.loads(deps.read_text())
    doc["runtimeTarget"] = {"name": ".NETCoreApp,Version=v10.0"}
    deps.write_text(json.dumps(doc))
    with pytest.raises(RuntimeError, match="names no runtime"):
        _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])


def test_a_third_party_notices_file_never_stands_in_for_a_packages_licence(tmp_path):
    # A package whose only file is its THIRD-PARTY-NOTICES (System.Formats.Cbor
    # in the app's restore) has it copied, but keeps its licence's standard
    # text: that file attributes code the package bundles, not the package's
    # own terms.
    d = _pkg(tmp_path, "p", {"THIRD-PARTY-NOTICES.TXT": b"a\n", "README.md": b"c\n"})
    copied = [f[0] for f in sn.dir_files(d)]
    assert copied == ["THIRD-PARTY-NOTICES.TXT"]
    assert not any(sn.stands_for_licence(label) for label in copied)


# --- notices files: shipped-notices.md § How the notices file is produced ---
# A package's third-party notices file is copied in every leg, and the
# standard-text fallback is decided on licence files alone — a NOTICE or a
# third-party notices file is copied, never in place of the licence.


def test_a_third_party_notices_file_is_copied_in_every_leg(tmp_path):
    d = _pkg(tmp_path, "p", {"THIRD-PARTY-NOTICES.TXT": b"a\n", "ThirdPartyNotices.txt": b"b\n", "License.md": b"l\n", "README.md": b"c\n"})
    assert sorted(f[0] for f in sn.dir_files(d)) == ["License.md", "THIRD-PARTY-NOTICES.TXT", "ThirdPartyNotices.txt"]
    jar = tmp_path / "x.jar"
    with zipfile.ZipFile(jar, "w") as z:
        z.writestr("META-INF/THIRD-PARTY-NOTICES.txt", "n\n")
        z.writestr("META-INF/LICENSE", "l\n")
    assert sorted(f[0] for f in sn.zip_files(jar)) == ["META-INF/LICENSE", "META-INF/THIRD-PARTY-NOTICES.txt"]


def test_only_a_licence_file_stands_for_the_licence():
    assert sn.stands_for_licence("LICENSE.TXT") and sn.stands_for_licence("COPYING")
    assert sn.stands_for_licence("LICENSES/MIT.txt") and sn.stands_for_licence("UNLICENSE")
    assert not sn.stands_for_licence("NOTICE") and not sn.stands_for_licence("META-INF/NOTICE.txt")
    assert not sn.stands_for_licence("THIRD-PARTY-NOTICES.TXT") and not sn.stands_for_licence("ThirdPartyNotices.txt")
    # a named leg's file is its component's licence whatever it is called
    assert sn.stands_for_licence("NOTICE", sn.TOOLCHAIN) and sn.stands_for_licence("NOTICE", "bundled")
    assert not sn.stands_for_licence("NOTICE", "rust")


def test_a_notices_only_package_keeps_its_standard_text(tmp_path):
    # System.Formats.Cbor in the app's restore: THIRD-PARTY-NOTICES.TXT is its
    # only file. It is copied, and its MIT standard text stands in beside it.
    art = _runtime_pack_fixture(tmp_path)
    (tmp_path / "nuget" / "microsoft.netcore.app.runtime.win-arm64" / "10.0.8" / "LICENSE.TXT").unlink()
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])
    assert res.ok, res.verdicts
    assert "runtime third-party notices" in res.text
    assert res.text.count(sn.FALLBACK_LINE.format(id="MIT")) == 2


def test_a_notice_file_alone_does_not_suppress_the_standard_text(tmp_path):
    art = _runtime_pack_fixture(tmp_path)
    pdir = tmp_path / "nuget" / "microsoft.netcore.app.runtime.win-arm64" / "10.0.8"
    (pdir / "LICENSE.TXT").unlink()
    (pdir / "THIRD-PARTY-NOTICES.TXT").unlink()
    (pdir / "NOTICE.txt").write_bytes(b"attribution notice\n")
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])
    assert res.ok, res.verdicts
    assert "attribution notice" in res.text
    assert res.text.count(sn.FALLBACK_LINE.format(id="MIT")) == 2


def test_a_package_with_a_licence_and_a_notices_file_copies_both_and_no_fallback(tmp_path):
    art = _runtime_pack_fixture(tmp_path)
    res = _gen_nuget(tmp_path, art, ["aarch64-pc-windows-msvc"])
    assert res.ok, res.verdicts
    assert "runtime licence" in res.text and "runtime third-party notices" in res.text
    assert res.text.count("--- THIRD-PARTY-NOTICES.TXT [text ") == 1
    # the one fallback is the SDK projection's, which carries no file at all
    assert res.text.count(sn.FALLBACK_LINE.format(id="MIT")) == 1

# --- the bundled leg ------------------------------------------------------------------


def _bundled(tmp: Path, *, files=("llvm-mingw/LICENSE.TXT",), stage_licence=True, stage_binary=True, **over):
    stage = tmp / "target"
    stage.mkdir(parents=True, exist_ok=True)
    if stage_binary:
        (stage / "libunwind.dll").write_bytes(b"MZ\x00binary")
    if stage_licence:
        (stage / "llvm-mingw").mkdir(exist_ok=True)
        (stage / "llvm-mingw" / "LICENSE.TXT").write_bytes(b"toolchain licence\n")
    comp = {"name": "libunwind", "version": "22.1.8", "origin": "the llvm-mingw toolchain, release 20260616",
            "licence": "Apache-2.0 WITH LLVM-exception", "source": "https://example.invalid/20260616",
            "binary": "libunwind.dll", "files": list(files)}
    comp.update(over)
    return {"name": "n", "channel": "x", "bundled": {"dir": "target", "components": [comp]}}


def _gen_bundled(tmp, art, exceptions=()):
    return sn.generate("a", {"artifact": {"a": art}, "exception": list(exceptions)}, allow=ALLOW,
                       texts_dir=_texts(tmp), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp)


def test_bundled_leg_copies_the_licence_file_the_build_staged_beside_the_binary(tmp_path):
    # shipped-notices.md § How the notices file is produced, the bundled bullet:
    # a binary no resolver lists (the mail bridge's libunwind.dll, from the
    # llvm-mingw toolchain) is named in the manifest with its licence id and the
    # licence file its build staged; the binary itself must be staged.
    res = _gen_bundled(tmp_path, _bundled(tmp_path))
    assert res.ok, res.verdicts
    assert [(c.ecosystem, c.name, c.version) for c in res.components] == [("bundled", "libunwind", "22.1.8")]
    assert "Component: libunwind" in res.text
    assert "From: the llvm-mingw toolchain, release 20260616" in res.text
    assert "Licence: Apache-2.0 WITH LLVM-exception" in res.text
    assert "--- llvm-mingw/LICENSE.TXT [text 1] ---" in res.text and "toolchain licence" in res.text
    assert "No licence file was copied" not in res.text
    assert any("bundled: components=1 files-copied=1 fallbacks=0" in v for v in res.verdicts)


def test_bundled_leg_refuses_a_binary_the_build_did_not_stage(tmp_path):
    with pytest.raises(FileNotFoundError):
        _gen_bundled(tmp_path, _bundled(tmp_path, stage_binary=False))


def test_bundled_leg_refuses_a_named_licence_file_the_build_did_not_stage(tmp_path):
    # never a silent fall back to the standard text, which would drop the
    # component's own copyright lines unnoticed
    with pytest.raises(FileNotFoundError):
        _gen_bundled(tmp_path, _bundled(tmp_path, stage_licence=False))


def test_bundled_leg_without_a_named_file_takes_the_standard_text(tmp_path):
    res = _gen_bundled(tmp_path, _bundled(tmp_path, files=()))
    assert res.ok, res.verdicts
    assert "the standard text of Apache-2.0 WITH LLVM-exception stands in" in res.text
    assert "canonical text of Apache-2.0 WITH LLVM-exception" in res.text


def test_bundled_leg_checks_the_licence_against_the_allow_list(tmp_path):
    res = _gen_bundled(tmp_path, _bundled(tmp_path, licence="GPL-3.0"))
    assert not res.ok
    assert any("REFUSED bundled libunwind@22.1.8" in v for v in res.verdicts)


def test_a_bundled_component_outside_the_allow_list_passes_on_its_exception(tmp_path):
    # the mingw-w64 runtime (Owed (d)): its id comes from a structured record and
    # is outside the allow list, so its exception carries the reading; its own
    # licence files are copied and no standard text stands in
    art = _bundled(tmp_path, name="mingw-w64 runtime", version="20260616", licence="LicenseRef-X AND ZPL-2.1")
    exc = [{"ecosystem": "bundled", "name": "mingw-w64 runtime", "licence": "LicenseRef-X AND ZPL-2.1", "reading": "a structured record"}]
    assert not _gen_bundled(tmp_path, art).ok
    res = _gen_bundled(tmp_path / "with", _bundled(tmp_path / "with", name="mingw-w64 runtime", version="20260616", licence="LicenseRef-X AND ZPL-2.1"), exc)
    assert res.ok, res.verdicts
    assert "Licence: LicenseRef-X AND ZPL-2.1" in res.text
    assert "toolchain licence" in res.text and "No licence file was copied" not in res.text
    assert any("bundled: components=1 files-copied=1 fallbacks=0 left-out=0 refused=0" in v for v in res.verdicts)


def test_the_cli_overrides_the_bundled_directory(monkeypatch, tmp_path):
    # the release assembly job reads the mail bridge's part, not target/
    seen = {}

    def fake(artifact, manifest, **kw):
        seen["bundled"] = manifest["artifact"][artifact]["bundled"]
        raise RuntimeError("stop")

    monkeypatch.setattr(sn, "generate", fake)
    assert sn.main(["windows-msi", str(tmp_path / "out"), "--set", "bundled.dir=parts/mail-bridge"]) == 1
    assert seen["bundled"]["dir"] == "parts/mail-bridge"
    assert seen["bundled"]["components"], "the override replaced the manifest's components"


# --- the Gradle leg -----------------------------------------------------------------


def test_gradle_leg_parses_the_report_and_reads_pom_and_archive_members(tmp_path):
    cache = tmp_path / "files-2.1"
    d = cache / "androidx.core" / "core" / "1.13.0" / "aaa"
    d.mkdir(parents=True)
    (d / "core-1.13.0.pom").write_text(
        "<project><licenses><license><name>The Apache Software License, Version 2.0</name></license></licenses></project>"
    )
    d2 = cache / "androidx.core" / "core" / "1.13.0" / "bbb"
    d2.mkdir(parents=True)
    with zipfile.ZipFile(d2 / "core-1.13.0.aar", "w") as z:
        z.writestr("META-INF/LICENSE.txt", "aar licence\n")
        z.writestr("classes.jar", "x")
    d3 = cache / "net.java.dev.jna" / "jna" / "5.14.0" / "ccc"
    d3.mkdir(parents=True)
    (d3 / "jna-5.14.0.pom").write_text("<project><licenses><license><name>Weird Licence</name></license></licenses></project>")
    report = tmp_path / "deps.txt"
    report.write_text(
        "releaseRuntimeClasspath - Runtime classpath of /release.\n"
        "+--- project :ffi\n"
        "+--- androidx.core:core:1.12.0 -> 1.13.0\n"
        "|    \\--- androidx.core:core:1.13.0 (*)\n"
        "+--- androidx.core:core-bom:1.13.0 (c)\n"
        "\\--- net.java.dev.jna:jna:5.14.0\n"
    )
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "gradle": {"report": str(report), "cache": str(cache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert sorted((c.name, c.version) for c in res.components) == [("androidx.core:core", "1.13.0"), ("net.java.dev.jna:jna", "5.14.0")]
    assert any("REFUSED gradle net.java.dev.jna:jna@5.14.0" in v for v in res.verdicts)
    core = next(c for c in res.components if c.name == "androidx.core:core")
    assert core.licence == "Apache-2.0"


def test_gradle_leg_reads_a_module_substitution_as_its_target(tmp_path):
    # `a:b:1 -> c:d:2` is a dependency substitution (the build's one-Tink rule):
    # the component on the classpath is c:d at 2, never "a:b" at "c:d:2".
    cache = tmp_path / "files-2.1"
    d = cache / "com.google.crypto.tink" / "tink-android" / "1.17.0" / "aaa"
    d.mkdir(parents=True)
    (d / "tink-android-1.17.0.pom").write_text(
        "<project><licenses><license><name>Apache License, Version 2.0</name></license></licenses></project>"
    )
    report = tmp_path / "deps.txt"
    report.write_text("+--- com.google.crypto.tink:tink:1.13.0 -> com.google.crypto.tink:tink-android:1.17.0\n")
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "gradle": {"report": str(report), "cache": str(cache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert [(c.name, c.version, c.licence) for c in res.components] == [("com.google.crypto.tink:tink-android", "1.17.0", "Apache-2.0")]
    assert res.ok, res.verdicts


def test_gradle_leg_takes_the_licence_a_pom_inherits_from_its_parent(tmp_path):
    # Guava's POMs name no licence of their own: guava-parent does, and Maven
    # inherits it. The parent is in the same cache, named by the <parent> element.
    cache = tmp_path / "files-2.1"
    d = cache / "com.google.guava" / "guava" / "31.0.1-jre" / "aaa"
    d.mkdir(parents=True)
    (d / "guava-31.0.1-jre.pom").write_text(
        '<project xmlns="http://maven.apache.org/POM/4.0.0"><parent><groupId>com.google.guava</groupId>'
        "<artifactId>guava-parent</artifactId><version>31.0.1-jre</version></parent></project>"
    )
    p = cache / "com.google.guava" / "guava-parent" / "31.0.1-jre" / "bbb"
    p.mkdir(parents=True)
    (p / "guava-parent-31.0.1-jre.pom").write_text(
        '<project xmlns="http://maven.apache.org/POM/4.0.0"><licenses><license>'
        "<name>The Apache Software License, Version 2.0</name></license></licenses></project>"
    )
    report = tmp_path / "deps.txt"
    report.write_text("\\--- com.google.guava:guava:31.0.1-jre\n")
    res = sn.generate(
        "a", {"artifact": {"a": {"name": "n", "channel": "x", "gradle": {"report": str(report), "cache": str(cache)}}}},
        allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
    )
    assert [(c.name, c.licence) for c in res.components] == [("com.google.guava:guava", "Apache-2.0")]
    assert res.ok, res.verdicts


# --- the cheap gate -----------------------------------------------------------------


def _goal(tmp: Path, channels=("Windows MSI", "Terminal archives (`.tar.gz`, `.zip`)")) -> Path:
    rows = "\n".join(f"| {c} | x | y |" for c in channels)
    p = tmp / "goal.md"
    p.write_text(f"# t\n\n## Per channel\n\n| Channel | Where | Closure |\n|---|---|---|\n{rows}\n\n**No screen.**\n\n## How\n")
    return p


def test_gate_green_on_conformant_tree(tmp_path):
    texts = _texts(tmp_path)
    manifest = {"artifact": {"a": {"name": "n", "channel": "Windows MSI", "rust": {"packages": ["x"]}}}}
    assert sn.check(manifest, ALLOW, texts, _goal(tmp_path)) == []


def test_gate_red_on_missing_fallback_text_and_unknown_channel(tmp_path):
    texts = _texts(tmp_path)
    (texts / "ISC.txt").unlink()
    manifest = {
        "artifact": {
            "a": {"name": "n", "channel": "Windows MSI", "rust": {"packages": ["x"]}},
            "b": {"name": "n", "channel": "Smart fridge", "rust": {"packages": ["x"]}},
            "c": {"name": "n", "channel": "Windows MSI"},
        },
        "exception": [{"ecosystem": "nuget", "name": "X"}],
    }
    errs = "\n".join(sn.check(manifest, ALLOW, texts, _goal(tmp_path)))
    assert "ISC" in errs
    assert "Smart fridge" in errs
    assert "artifact c names no closure" in errs
    assert "reading" in errs


def test_gate_red_on_a_bundled_component_missing_a_field(tmp_path):
    texts = _texts(tmp_path)
    manifest = {"artifact": {"a": {"name": "n", "channel": "Windows MSI", "bundled": {"components": [
        {"name": "libunwind", "version": "1", "licence": "MIT", "origin": "o", "source": "s"},
    ]}}}}
    errs = "\n".join(sn.check(manifest, ALLOW, texts, _goal(tmp_path)))
    assert "artifact a: bundled component libunwind names no `binary`" in errs
    manifest["artifact"]["a"]["bundled"]["components"][0]["binary"] = "libunwind.dll"
    assert sn.check(manifest, ALLOW, texts, _goal(tmp_path)) == []


def test_gate_green_on_the_real_tree():
    assert sn.main(["--check"]) == 0


def test_manifest_artifact_lookup_refuses_unknown(tmp_path, capsys):
    with pytest.raises(SystemExit):
        sn.generate("nope", {"artifact": {}}, allow=ALLOW, texts_dir=tmp_path, version="0", run=Runner([]), go_ids={}, repo=tmp_path)


def test_go_leg_refuses_a_binary_that_was_not_built(tmp_path):
    with pytest.raises(FileNotFoundError):
        sn.generate(
            "a", {"artifact": {"a": {"name": "n", "channel": "x", "go": {"binary": "missing", "modcache": str(tmp_path)}}}},
            allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=Runner([]), go_ids={}, repo=tmp_path,
        )


def test_go_leg_leaves_out_faunas_own_modules_and_reads_a_vendored_forks_id(tmp_path):
    # A `replace` to a path inside the repository: Fauna's own module (its path
    # under the repository's module prefix) is left out like a workspace member;
    # a vendored fork of a third-party module takes the id the notices file's
    # `## Vendored source` table records for its directory. An `unverified …`
    # cell in the Go table is no id.
    repo = tmp_path / "repo"
    bridges = repo / "bins" / "fauna-bridges"
    _pkg(bridges, "third_party/go-imap", {"LICENSE": b"go-imap mit\n"})
    _pkg(repo, "libs/fauna-mail-go", {"go.mod": b""})
    (bridges / "bridge").write_bytes(b"")
    notices = tmp_path / "NOTICES.md"
    notices.write_text("\n".join([
        "## Vendored source", "",
        "| Path | Origin | Licence | Notes |", "|---|---|---|---|",
        "| `bins/fauna-bridges/third_party/go-imap` | `github.com/emersion/go-imap/v2`, forked | MIT (`bins/fauna-bridges/third_party/go-imap/LICENSE`) | x |", "",
        "## Go modules (`bins/fauna-bridges`)", "",
        "| Module | Licence |", "|---|---|",
        "| `github.com/ipfs/bbloom` | unverified — deps.dev: no licence data |", "",
    ]))
    out = "\n".join([
        "bridge: go1.26.0",
        "\t".join(["", "mod", "fauna/bridges", "(devel)", ""]),
        "\t".join(["", "dep", "github.com/faunasocial/fauna/libs/fauna-mail-go", "v0.0.0"]),
        "\t".join(["", "=>", "../../libs/fauna-mail-go", ""]),
        "\t".join(["", "dep", "github.com/emersion/go-imap/v2", "v2.0.0-beta.8"]),
        "\t".join(["", "=>", "./third_party/go-imap", ""]),
        "\t".join(["", "dep", "github.com/ipfs/bbloom", "v0.0.4", "h1:abc="]),
        "",
    ])
    _pkg(tmp_path / "gomod", "github.com/ipfs/bbloom@v0.0.4", {"LICENSE": b"bbloom\n"})
    ids = sn.parse_go_table(notices)
    assert "github.com/ipfs/bbloom" not in ids
    assert ids["bins/fauna-bridges/third_party/go-imap"] == "MIT"
    art = {"name": "n", "channel": "x", "go": {"binary": "bins/fauna-bridges/bridge", "modcache": str(tmp_path / "gomod")}}
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6",
                      run=Runner([(["go", "version", "-m"], out)]), go_ids=ids, repo=repo)
    names = sorted(c.name for c in _third_party(res))
    assert names == ["github.com/emersion/go-imap/v2", "github.com/ipfs/bbloom"], names
    # The fork: named by the upstream module it forks, at the release its
    # `dep` line requires, found in the repository at its repository path.
    (fork,) = [c for c in res.components if c.name == "github.com/emersion/go-imap/v2"]
    assert fork.version == "v2.0.0-beta.8"
    assert fork.origin == fork.source == "the Fauna source repository, bins/fauna-bridges/third_party/go-imap"
    assert "Version: v2.0.0-beta.8" in res.text.split("Component: github.com/emersion/go-imap/v2", 1)[1].split("\n\n", 1)[0]
    assert any("REFUSED go github.com/ipfs/bbloom@v0.0.4: licence none" in v for v in res.verdicts)
    assert not any("go-imap" in v and "REFUSED" in v for v in res.verdicts)
    assert "go-imap mit" in res.text


def test_host_target_fills_a_group_that_names_no_target(tmp_path):
    # The image's notices stage runs at the build platform; the native groups
    # name no target (the image builds for its own host), so the stage passes
    # the image's triple, and a group naming its own target keeps it.
    runner = _rust_fixture(tmp_path)
    art = {"name": "n", "channel": "x", "rust": [{"packages": ["fauna-tui"]}, {"packages": ["fauna-tui"], "target": "wasm32-unknown-unknown"}]}
    sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6", run=runner, go_ids={}, repo=tmp_path / "repo", host_target="aarch64-unknown-linux-gnu")
    trees = [argv for argv, _ in runner.calls if argv[:2] == ["cargo", "tree"]]
    assert [a[a.index("--target") + 1] for a in trees] == ["aarch64-unknown-linux-gnu", "wasm32-unknown-unknown"]


# --- per-run target narrowing ------------------------------------------------------


def _trees(runner):
    return [a[a.index("--target") + 1] if "--target" in a else "" for a, _ in runner.calls if a[:2] == ["cargo", "tree"]]


def _narrow(tmp_path, art, targets):
    runner = _rust_fixture(tmp_path)
    res = sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6",
                      run=runner, go_ids={}, repo=tmp_path / "repo", targets=targets)
    return res, _trees(runner)


_TWO = ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]


def test_without_a_target_the_closure_spans_every_triple_the_entry_names(tmp_path):
    # One package carrying several triples (an Android app holds four ABIs)
    # lists the union: its caller names no target.
    _, trees = _narrow(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "targets": _TWO}}, None)
    assert trees == _TWO


def test_a_target_narrows_a_two_triple_entry_to_that_triple(tmp_path):
    # shipped-notices.md § Per channel: a bare binary's notices cover "that
    # binary" — the macOS nest binary's file resolves for its own triple only.
    res, trees = _narrow(tmp_path, {"name": "n", "channel": "x", "rust": {"packages": ["fauna-tui"], "targets": _TWO}},
                         ["aarch64-apple-darwin"])
    assert res.ok, res.verdicts
    assert trees == ["aarch64-apple-darwin"]


def test_a_target_set_narrows_every_group_of_a_multi_group_entry(tmp_path):
    # The Windows installer per arch: its msvc groups and its gnullvm group
    # (the mail bridge's FFI) each keep the triple of the arch named.
    msvc = ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]
    art = {"name": "n", "channel": "x", "rust": [
        {"packages": ["fauna-tui"], "targets": msvc},
        {"packages": ["fauna-tui"], "targets": msvc},
        {"packages": ["fauna-tui"], "targets": ["x86_64-pc-windows-gnullvm", "aarch64-pc-windows-gnullvm"]},
    ]}
    _, trees = _narrow(tmp_path, art, ["aarch64-pc-windows-msvc", "aarch64-pc-windows-gnullvm"])
    assert trees == ["aarch64-pc-windows-msvc", "aarch64-pc-windows-msvc", "aarch64-pc-windows-gnullvm"]


def test_a_target_leaves_a_group_naming_none_to_the_host(tmp_path):
    art = {"name": "n", "channel": "x", "rust": [{"packages": ["fauna-tui"]}, {"packages": ["fauna-tui"], "target": "x86_64-unknown-linux-gnu"}]}
    _, trees = _narrow(tmp_path, art, ["x86_64-unknown-linux-gnu"])
    assert trees == ["", "x86_64-unknown-linux-gnu"]


@pytest.mark.parametrize("targets", [["riscv64gc-unknown-linux-gnu"], ["aarch64-unknown-linux-gnu"],
                                     ["x86_64-unknown-linux-gnu", "aarch64-pc-windows-msvc"]])
def test_a_target_a_group_or_the_entry_does_not_name_is_refused(tmp_path, targets):
    # A group left with no triple, or a triple no group names, is a caller and
    # a manifest that disagree: refused before any resolver runs, never a
    # silently shorter file.
    art = {"name": "n", "channel": "x", "rust": [
        {"packages": ["fauna-tui"], "targets": ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]},
        {"packages": ["fauna-tui"], "target": "x86_64-unknown-linux-gnu"},
    ]}
    runner = _rust_fixture(tmp_path)
    with pytest.raises(RuntimeError):
        sn.generate("a", {"artifact": {"a": art}}, allow=ALLOW, texts_dir=_texts(tmp_path), version="0.1.6",
                    run=runner, go_ids={}, repo=tmp_path / "repo", targets=targets)
    assert _trees(runner) == []


def test_the_cli_passes_repeated_targets_to_the_generator(monkeypatch, tmp_path):
    seen = {}

    def fake(artifact, manifest, **kw):
        seen.update(kw)
        raise RuntimeError("stop")

    monkeypatch.setattr(sn, "generate", fake)
    rc = sn.main(["release-nest", str(tmp_path / "out"), "--target", "x86_64-pc-windows-msvc", "--target", "x86_64-pc-windows-gnullvm"])
    assert rc == 1
    assert seen["targets"] == ["x86_64-pc-windows-msvc", "x86_64-pc-windows-gnullvm"]


def test_texts_record_refuses_a_text_the_record_does_not_hold(tmp_path):
    """A text whose id left the allowlist must leave the directory with it."""
    import hashlib

    d = tmp_path / "licence-texts"
    d.mkdir()
    (d / "MIT.txt").write_bytes(b"kept\n")
    sha = hashlib.sha256(b"kept\n").hexdigest()
    (d / "SOURCE.toml").write_text(f'[text."MIT.txt"]\nid = "MIT"\nsha256 = "{sha}"\n', encoding="utf-8")
    assert sn.check_texts_record(d) == []
    (d / "Stale.txt").write_bytes(b"left behind\n")
    errs = sn.check_texts_record(d)
    assert len(errs) == 1 and errs[0].startswith("Stale.txt: a text SOURCE.toml does not record")
