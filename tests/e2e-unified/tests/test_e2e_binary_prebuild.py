"""The warm-takes-no-slot half of the e2e binary-build fix.

Two independent halves closed the fixture build-slot timeout inversion
(build-system.md § Build/e2e slot locks, the two ✅ bullets), landed the same
day and merged:

* **Collection-time prebuild**: cold builds happen in
  `pytest_collection_finish`, outside every per-test timeout, through a
  memoized ensure layer. Pinned in `test_harness_self_termination.py` § 5
  (memoization, failure replay, selection laziness, the bound-inversion pin).
* **Warm trees take no slot** (THIS file):
  the heavy cargo steps are freshness-gated OUTSIDE their `{{slot_build}}`, so
  a warm invocation runs no cargo and queues for nothing — pre-building
  removes the wait, not just the compile. Before this, `{{slot_build}}`
  wrapped cargo unconditionally and a fully warm tree still queued (up to
  90 min under contention).

The `--stamp` freshness semantics themselves are pinned in
`test_build_if_stale.py`; this file pins the composition (justfile) and the
`build_node` gate/slot/variant-pin.
"""

import os
import re
import sys
import time
import types
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

import conftest  # noqa: E402  (tests/e2e-unified is on sys.path via conftest)
from common import nest as common_nest  # noqa: E402


# ── The justfile composition: gate OUTSIDE the slot ───────────────────────────


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name *ARGS:` declaration alike."""
    lines = text.splitlines()
    header = re.compile(rf"^{re.escape(name)}(\s+\S+)*:")
    for i, ln in enumerate(lines):
        if header.match(ln):
            body = []
            for bl in lines[i + 1:]:
                if bl and not bl[:1].isspace():
                    break
                body.append(bl)
            return "\n".join(body)
    return ""


def test_mail_bridge_ffi_cargo_step_is_freshness_gated_outside_the_slot():
    """The fixture-feeding heavy step (`mail-bridge-build` / `seal-helper-build`
    both dep on `mail-bridge-ffi`) must decide "is there work?" BEFORE queueing:
    build-if-stale outermost, {{slot_build}} inside it, cargo innermost. And it
    must use --stamp: a cargo no-op leaves the .so untouched, so an
    artifact-keyed gate would go permanently stale on rebase mtime churn and
    queue on every warm run anyway."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "mail-bridge-ffi")
    assert body, "mail-bridge-ffi recipe not found"
    cargo_lines = [
        ln for ln in body.splitlines()
        if "cargo build" in ln and "-p fauna-ffi" in ln and not ln.lstrip().startswith("#")
    ]
    assert cargo_lines, "mail-bridge-ffi must build fauna-ffi"
    joined = " ".join(body.split())
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "mail-bridge-ffi's cargo step must be gated by build-if-stale --stamp "
        "(a warm tree must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    # The gated command line: gate → slot → cargo, in that order.
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    cargo_pos = joined.index("cargo build --locked -p fauna-ffi", gate_pos)
    assert gate_pos < slot_pos < cargo_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} OUTSIDE "
        "cargo — a slot acquired before the freshness verdict queues a warm tree"
    )


def test_e2e_ffi_cargo_step_is_freshness_gated_outside_the_slot():
    """The e2e harness's own cdylib recipe (`tests/e2e-unified/fauna_ffi.py`'s
    `_find_cdylib` calls it on every call) must follow
    the same shape `mail-bridge-ffi` does, for the same reason: build-if-stale
    outermost (so a warm tree queues no slot), {{slot_build}} inside it, cargo
    innermost — and gated by --stamp, since a cargo no-op leaves the .so
    untouched and an artifact-keyed gate would go permanently stale on rebase
    mtime churn."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "e2e-ffi")
    assert body, "e2e-ffi recipe not found"
    cargo_lines = [
        ln for ln in body.splitlines()
        if "cargo build" in ln and "-p fauna-ffi" in ln and not ln.lstrip().startswith("#")
    ]
    assert cargo_lines, "e2e-ffi must build fauna-ffi"
    joined = " ".join(body.split())
    assert "build-if-stale.py" in joined and "--stamp" in joined, (
        "e2e-ffi's cargo step must be gated by build-if-stale --stamp "
        "(a warm tree must take no build slot)"
    )
    gate_pos = joined.index("build-if-stale.py")
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    cargo_pos = joined.index("cargo build --locked -p fauna-ffi", gate_pos)
    assert gate_pos < slot_pos < cargo_pos, (
        "order must be build-if-stale (freshness) OUTSIDE {{slot_build}} OUTSIDE "
        "cargo — a slot acquired before the freshness verdict queues a warm tree"
    )
    # The copy into the harness-private slot must live INSIDE the gated
    # command — a separately-gated copy would re-import whichever foreign
    # flavor last touched the shared `target/<profile>/` slot, the exact
    # collision this recipe exists to avoid (`e2e-ffi-slot`'s comment).
    # (It lands by rename — `test_ffi_slot_atomic_replace.py` pins why.)
    cp_pos = joined.index('mv -f "$LIB.$$.tmp" "$LIB"')
    assert cargo_pos < cp_pos, "the private-slot copy must run after the cargo build, in the same gated command"


#: The Unix Go/cgo recipes the collection-time prebuild calls
#: (`_ensure_mail_bridge_built`, `_ensure_seal_helper_built`, the two atproto
#: ensurers) → the binary each writes into bins/fauna-bridges/.
_GO_CGO_RECIPES = {
    "mail-bridge-build": "fauna-mail-bridge",
    "seal-helper-build": "seal-helper-testonly",
    "atproto-bridge-build": "fauna-atproto-bridge",
    "atproto-bridge-build-e2e": "fauna-atproto-bridge-e2e",
}


@pytest.mark.parametrize("recipe,out_name", sorted(_GO_CGO_RECIPES.items()))
def test_go_cgo_recipes_build_through_the_gated_helper(recipe, out_name):
    """Each Unix Go/cgo recipe builds through `_go-cgo-build` and runs no
    `{{slot_build}}` of its own: an ungated `{{slot_build}} go build` reached
    the slot on a warm tree too, so the in-hold re-check (refuse mode) refused
    `seal-helper-build` on every cycle however often the warm pass had just
    built it."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), recipe)
    assert body, f"{recipe} recipe not found"
    code = [ln for ln in body.splitlines() if not ln.lstrip().startswith("#")]
    assert not any("{{slot_build}}" in ln for ln in code), (
        f"{recipe} takes a build slot outside the freshness gate"
    )
    assert any(
        "just _go-cgo-build" in ln and f'"{out_name}"' in ln for ln in code
    ), f"{recipe} must build {out_name} through `_go-cgo-build`"


def _go_cgo_helper_joined() -> str:
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_go-cgo-build")
    assert body, "_go-cgo-build recipe not found"
    return " ".join(
        ln for ln in body.splitlines() if not ln.lstrip().startswith("#")
    ).replace("\\", " ")


def test_go_cgo_helper_is_freshness_gated_outside_the_slot():
    """build-if-stale --stamp outermost, {{slot_build}} inside it, `go build`
    innermost — the shape every cargo step in the chain already has."""
    joined = " ".join(_go_cgo_helper_joined().split())
    gate_pos = joined.index("build-if-stale.py")
    assert "--stamp" in joined[gate_pos:], "the go build must be gated by --stamp"
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    go_pos = joined.index("go -C bins/fauna-bridges build", gate_pos)
    assert gate_pos < slot_pos < go_pos, (
        "order must be build-if-stale OUTSIDE {{slot_build}} OUTSIDE go build"
    )


def test_windows_go_cgo_build_is_freshness_gated_outside_the_slot():
    """The windows twin of the helper test above: `_windows-go-cgo-build` gated only its cargo step, so its
    `{{slot_build}} go build` took a slot on a warm tree and refuse mode refused
    it on every in-hold re-check. Gate outermost, slot inside, `go build`
    innermost; the build drops every OTHER stamp for its out_name (all targets
    and profiles write the same target/<out_name>, so a surviving arm64 stamp
    would read fresh over an amd64 exe), the prefix ends in the `.exe` so the
    prod and e2e atproto stamps never match each other's glob; the DLL copies
    stay outside the gate (shared by all four exes and both architectures)."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "_windows-go-cgo-build")
    assert body, "_windows-go-cgo-build recipe not found"
    code = [ln for ln in body.splitlines() if not ln.lstrip().startswith("#")]
    joined = " ".join(" ".join(code).replace("\\", " ").split())
    gate_pos = joined.index("build-if-stale.py")
    assert "--stamp" in joined[gate_pos:], "the go build must be gated by --stamp"
    slot_pos = joined.index("{{slot_build}}", gate_pos)
    go_pos = joined.index("go -C bins/fauna-bridges build", gate_pos)
    assert gate_pos < slot_pos < go_pos
    assert joined.count("{{slot_build}}") == 1, "only the gated go build may take a slot"
    assert "windows-go-cgo-{{out_name}}" in joined
    assert 'rm -f "$STAMP_PREFIX"-*.stamp' in joined, (
        "the build must drop every other stamp of this out_name (other targets too)"
    )
    # The prefix must end at the out_name (which carries `.exe`) so the glob
    # `<prefix>-*.stamp` cannot reach a sibling exe's stamp.
    prefix = re.search(r'STAMP_PREFIX="[^"]*windows-go-cgo-\{\{out_name\}\}"', joined)
    assert prefix, "STAMP_PREFIX must end at {{out_name}}"
    for recipe, out_name in (
        ("windows-atproto-bridge-build", "fauna-atproto-bridge.exe"),
        ("windows-atproto-bridge-build-e2e", "fauna-atproto-bridge-e2e.exe"),
    ):
        rbody = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), recipe)
        assert f'"{out_name}"' in rbody, f"{recipe} must pass {out_name}"
    # The DLL staging is unconditional: after the gate, not inside its command.
    assert joined.index('cp "$REL/fauna_ffi.dll"') > joined.index("-o \"$OUT\" {{cmd}}'")


def test_a_freshly_built_go_binary_reads_warm(tmp_path):
    """The gate's own output must not count as its source: the binaries land
    INSIDE `bins/fauna-bridges`, a `--source`, so without the helper's excludes
    a build's own output is newer than the stamp it just wrote and the next
    check (the in-hold re-check) reads stale for ever. Runs build-if-stale's
    --check with the helper's exact exclude list, after a build wrote every one
    of the four binaries."""
    excludes = re.findall(r"--exclude '([^']+)'", _go_cgo_helper_joined())
    assert excludes, "_go-cgo-build must exclude its built binaries from its sources"
    module = tmp_path / "bins" / "fauna-bridges"
    (module / "cmd" / "seal-helper-testonly").mkdir(parents=True)
    src = module / "cmd" / "seal-helper-testonly" / "seal.go"
    src.write_text("package main\n")
    old = time.time() - 600
    os.utime(src, (old, old))
    stamp = tmp_path / "go-cgo.stamp"
    stamp.write_text("")
    os.utime(stamp, (old + 60, old + 60))
    for out_name in _GO_CGO_RECIPES.values():  # the build's outputs, after the stamp
        (module / out_name).write_text("binary")
    out = module / "seal-helper-testonly"

    def check(*extra: str) -> int:
        import subprocess

        return subprocess.run(
            [sys.executable, str(_REPO / "scripts" / "build-if-stale.py"), "--check", "-q",
             "--stamp", str(stamp), "--target", "bins/fauna-bridges/seal-helper-testonly",
             "--source", "bins/fauna-bridges", *extra],
            cwd=tmp_path,
        ).returncode

    assert out.exists()
    assert check() == 1, "sanity: without the excludes the output reads as a newer source"
    assert check(*[a for p in excludes for a in ("--exclude", p)]) == 0, (
        "a freshly built Go binary must read warm under the helper's excludes"
    )
    src.write_text("package main // edited\n")
    assert check(*[a for p in excludes for a in ("--exclude", p)]) == 1, (
        "a real source edit must still read stale"
    )


def test_e2e_prev_build_recipe_checks_warm_cache_outside_the_slot():
    """`e2e-prev-build` used to run `{{slot_build}} ... prev_build.py --build`
    unconditionally — a fully warm previous-build cache still queued for a
    build slot just so `prev_build.py --build` could conclude, via its own
    `cached_binaries()`, that there was nothing to do (build-system.md §
    Build/e2e slot locks, same-class residue). The recipe must probe warmth
    via `prev_build.py --path` (cheap: file-existence + dependency checks, no
    cargo) OUTSIDE `{{slot_build}}`, and only take the slot when that probe
    reports cold."""
    body = _recipe_body(_JUSTFILE.read_text(encoding="utf-8"), "e2e-prev-build")
    assert body, "e2e-prev-build recipe not found"
    joined = " ".join(body.split())
    assert "prev_build.py --path" in joined, (
        "e2e-prev-build must probe the pinned-build cache via `prev_build.py "
        "--path` before deciding whether to build"
    )
    path_pos = joined.index("prev_build.py --path")
    slot_pos = joined.index("{{slot_build}}", path_pos)
    # The web branch's own `--build` (no outer slot, it slots its own cargo steps)
    # precedes the slot-wrapped one; the order that matters is slot -> its --build.
    build_pos = joined.index("prev_build.py --build", slot_pos)
    assert path_pos < slot_pos < build_pos, (
        "order must be the --path warmth probe OUTSIDE {{slot_build}} OUTSIDE "
        "the --build work — a slot acquired before the probe queues a warm "
        "cache for nothing"
    )


# ── build_node: warm path takes no slot, runs no cargo ────────────────────────


def test_build_node_warm_path_runs_no_subprocess(tmp_path, monkeypatch):
    """THE fixture-side warm pin: with a fresh variant stamp + pinned exe,
    build_node must return without invoking anything — no cargo, no build-slot
    queue. (Its subprocess call is routed through the build pool, so 'no
    subprocess' IS 'no slot'.)"""
    target_dir = tmp_path / "cargo-target"
    pinned, stamp = common_nest._nest_variant_paths(target_dir, "test-hooks,nostr", "debug")
    pinned.parent.mkdir(parents=True)
    pinned.write_text("fake exe")
    stamp.write_text("stamp")
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    # Sources far older than the stamp — the warm case.
    monkeypatch.setattr(common_nest, "_nest_newest_source_mtime",
                        lambda repo: stamp.stat().st_mtime - 3600)

    def _no_subprocess(*a, **k):
        raise AssertionError(
            "a warm build_node must not spawn ANY subprocess — spawning means "
            "queueing for a build slot with nothing to do"
        )

    monkeypatch.setattr(common_nest.subprocess, "run", _no_subprocess)
    assert common_nest.build_node() == str(pinned)


def test_build_node_routes_cargo_through_the_build_pool(tmp_path, monkeypatch):
    """A stale tree's cargo run must hold a machine-wide `build` slot —
    `cargo build -p fauna-nest` compiles most of the workspace, and before
    2026-07-29 it ran completely unslotted (the inverse defect of the fixture
    slot-wait inversion: unbounded load instead of an unbounded-feeling wait)."""
    target_dir = tmp_path / "cargo-target"
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    monkeypatch.setattr(common_nest, "_nest_newest_source_mtime", lambda repo: 0.0)
    # `_build_slot_cmd` degrades to an empty prefix when `<repo>/scripts/
    # build-slot.py` is absent (a solo checkout with no sibling builds has
    # nothing to arbitrate against) — this fake repo needs the file present
    # to exercise the slot-wrapped path this test is actually pinning.
    build_slot = tmp_path / "scripts" / "build-slot.py"
    build_slot.parent.mkdir(parents=True, exist_ok=True)
    build_slot.write_text("")
    assert build_slot.exists()

    built = tmp_path / "fauna-nest"  # stem must match what build_node looks for
    built.write_text("fake built exe")
    seen = {}

    class _Result:
        returncode = 0
        stderr = ""
        stdout = '{"executable": "%s"}' % str(built).replace("\\", "\\\\")

    def _capture(argv, **k):
        seen["argv"] = argv
        return _Result()

    monkeypatch.setattr(common_nest.subprocess, "run", _capture)
    out = common_nest.build_node()
    argv = seen["argv"]
    assert "build-slot.py" in str(argv[1]), f"cargo must be slot-wrapped; got argv {argv}"
    assert argv[argv.index("--pool") + 1] == "build"
    assert "cargo" in argv, f"the wrapped command must be cargo; got {argv}"
    # And the returned path is the per-variant pinned copy, not cargo's shared path.
    pinned, stamp = common_nest._nest_variant_paths(target_dir, "test-hooks,nostr", "debug")
    assert out == str(pinned)
    assert pinned.read_text() == "fake built exe"
    assert stamp.exists(), "a successful build must commit the variant stamp"


def test_nest_variant_stamp_is_per_variant():
    """Two feature sets (or profiles) must never share a stamp or a pinned copy —
    the shared cargo output path is exactly what made variants clobber each
    other (the bluesky 'unknown kind' trap)."""
    td = Path("/tmp-unused")
    a = common_nest._nest_variant_paths(td, "test-hooks,nostr", "debug")
    b = common_nest._nest_variant_paths(td, "test-hooks,nostr,bluesky", "debug")
    c = common_nest._nest_variant_paths(td, "test-hooks,nostr", "release")
    assert len({a[0], b[0], c[0]}) == 3, "pinned exe paths must be distinct per variant"
    assert len({a[1], b[1], c[1]}) == 3, "stamps must be distinct per variant"


def test_build_recovery_fixture_warm_path_runs_no_subprocess(tmp_path, monkeypatch):
    """A fresh stamp + existing binary returns with no cargo and no slot."""
    from helpers import helper_bins

    entry = helper_bins.REGISTRY["recovery_fixture"]
    target_dir = tmp_path / "cargo-target"
    binary, stamp = helper_bins.binary_paths(entry, target_dir)
    binary.parent.mkdir(parents=True)
    binary.write_text("fake exe")
    stamp.write_text(helper_bins.stamp_text(entry, binary))
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    monkeypatch.setattr(common_nest, "_example_newest_source_mtime",
                        lambda repo: stamp.stat().st_mtime - 3600)

    def _no_subprocess(*a, **k):
        raise AssertionError("a warm helper-binary build must spawn nothing")

    monkeypatch.setattr(common_nest.subprocess, "run", _no_subprocess)
    assert helper_bins.build(entry) == str(binary)

    # A binary another cargo run rewrote (other features) is not the stamped one.
    binary.write_text("rebuilt elsewhere")
    import os
    os.utime(binary, ns=(binary.stat().st_mtime_ns + 10**9,) * 2)
    with pytest.raises(AssertionError, match="must spawn nothing"):
        helper_bins.build(entry)


def test_refused_build_of_a_stale_helper_writes_no_stamp(tmp_path, monkeypatch):
    """A refused (exit 75) or failed build must never mark a present-but-stale
    exe fresh: on win32 the old rule read 'exe exists' as 'built', stamped it,
    and every later pass read warm and never rebuilt it."""
    import os
    import subprocess

    from helpers import helper_bins

    entry = helper_bins.REGISTRY["recovery_fixture"]
    target_dir = tmp_path / "cargo-target"
    binary, stamp = helper_bins.binary_paths(entry, target_dir)
    binary.parent.mkdir(parents=True)
    binary.write_text("old exe")
    old = binary.stat().st_mtime - 7200
    os.utime(binary, (old, old))
    stamp.write_text(f"stamp\nbinary-mtime-ns: {binary.stat().st_mtime_ns}\n")
    os.utime(stamp, (old, old))
    before = stamp.read_text()
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    # A source edit newer than the stamp makes the helper stale.
    monkeypatch.setattr(common_nest, "_example_newest_source_mtime",
                        lambda repo: old + 3600)

    def _refused(cmd, **kwargs):
        return subprocess.CompletedProcess(cmd, 75, stdout="", stderr="refused")

    monkeypatch.setattr(common_nest.subprocess, "run", _refused)
    with pytest.raises(RuntimeError, match="failed"):
        helper_bins.build(entry)
    assert stamp.read_text() == before
    assert stamp.stat().st_mtime == old, "a refused build must leave the stamp untouched"


def test_recovery_fixture_is_stale_when_a_shared_crate_changes(tmp_path):
    """The regression this builder exists for (measured 2026-09-29): the fixture
    links `fauna-protocol` and the client crates, so an edit to ANY libs/ file —
    not just `recovery_fixture.rs` — must read newer than its stamp. The old rule
    compared the binary against its own source only, and a binary built before a
    wire-shape change signed a profile the nest's strict decode refused
    (`fauna.profile.invalid_request … SchemaMismatch`)."""
    import os

    (tmp_path / "Cargo.lock").write_text("")
    (tmp_path / "rust-toolchain.toml").write_text("")
    example = tmp_path / "libs" / "fauna-client-recovery" / "examples" / "recovery_fixture.rs"
    shared = tmp_path / "libs" / "fauna-protocol" / "src" / "email.rs"
    for path in (example, shared):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("")
        os.utime(path, (1_000, 1_000))
    for path in (tmp_path / "Cargo.lock", tmp_path / "rust-toolchain.toml"):
        os.utime(path, (1_000, 1_000))
    stamp_mtime = 2_000.0
    assert common_nest._example_newest_source_mtime(tmp_path) <= stamp_mtime

    os.utime(shared, (3_000, 3_000))  # only the SHARED crate moved
    assert common_nest._example_newest_source_mtime(tmp_path) > stamp_mtime


# ── conftest wiring: the prebuild map covers the nest-build aliases ───────────


def test_prebuild_map_covers_node_binary_alias():
    """`tests/platform/conftest.py::node_binary` is the platform tree's name for
    the same nest build — the prebuild map must cover it, or the tier_4/platform
    suites' first nest-requesting test pays the build inside its own timeout."""
    covered = {name for name, _builder, _args in conftest._PREBUILD_BY_FIXTURE}
    for required in ("nest_binary", "node_binary", "bluesky_nest_binary",
                     "mail_bridge_binary", "seal_helper_binary"):
        assert required in covered, (
            f"_PREBUILD_BY_FIXTURE must cover {required!r} — a binary-building "
            f"session fixture outside the map builds inside the first "
            f"requesting test's 900s budget"
        )


def test_prebuild_map_covers_the_bridges_nest_fixture():
    """`test_bridges.py::bridges_nest_binary` (a dedicated-provider-set nest
    build) is a file-local variant fixture that used to build in-test — same
    residue class as the node_binary aliases above (build-system.md § Build/e2e
    slot locks, same-class residue list)."""
    covered = {name for name, _builder, _args in conftest._PREBUILD_BY_FIXTURE}
    for required in ("bridges_nest_binary",):
        assert required in covered, (
            f"_PREBUILD_BY_FIXTURE must cover {required!r} — a binary-building "
            f"session fixture outside the map builds inside the first "
            f"requesting test's 900s budget"
        )


# ── The stamp is dated at the build-slot GRANT, not at the request ────────────
#
# the warm pass builds outside every
# slot, then the in-hold pass re-checks and must find the tree warm. A stamp
# dated at the REQUEST called the tree stale whenever an input was edited
# while the build waited in the `build` queue — though cargo, starting at the
# grant, compiled that edit — so the in-hold re-check queued for `build` a
# second time, inside the e2e hold (measured 17.1 min, 2026-09-26). The slot
# script hands the grant instant back through $FAUNA_SLOT_GRANT_FILE.

def _helper_paths(name, target_dir):
    from helpers import helper_bins

    return helper_bins.binary_paths(helper_bins.REGISTRY[name], target_dir)


def _builder_fn(builder):
    """A `common.nest` builder by name, or a registry build (`helper_bin:<name>`)."""
    if builder.startswith("helper_bin:"):
        from helpers import helper_bins

        entry = helper_bins.REGISTRY[builder.split(":", 1)[1]]
        return lambda: helper_bins.build(entry)
    return getattr(common_nest, builder)


# (builder, source-mtime hook, build paths → (binary, stamp), cargo exe stem)
_STAMP_WRITERS = [
    ("build_node", "_nest_newest_source_mtime",
     lambda td: common_nest._nest_variant_paths(td, "test-hooks,nostr", "debug"), "fauna-nest"),
    ("build_sync_agent", "_sync_agent_newest_source_mtime",
     common_nest._sync_agent_build_paths, "fauna-sync-agent"),
    ("helper_bin:recovery_fixture", "_example_newest_source_mtime",
     lambda td: _helper_paths("recovery_fixture", td), "recovery_fixture"),
    ("helper_bin:mls-keypackage-gen", "_example_newest_source_mtime",
     lambda td: _helper_paths("mls-keypackage-gen", td), "mls-keypackage-gen"),
]
_STAMP_WRITER_IDS = [w[0] for w in _STAMP_WRITERS]


def _queued_build(tmp_path, monkeypatch, builder, mtime_hook, paths, stem, *, edit_after_grant):
    """Drive one stale build through a stub slot wrapper that queues: the
    source is edited while the build waits, the slot is granted later, then
    (optionally) the source is edited again while "cargo" runs. Returns the
    builder so the caller can re-check."""
    target_dir = tmp_path / "cargo-target"
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    build_slot = tmp_path / "scripts" / "build-slot.py"
    build_slot.parent.mkdir(parents=True, exist_ok=True)
    build_slot.write_text("")
    binary, _stamp = paths(target_dir)
    built = tmp_path / stem  # cargo's JSON-reported exe (build_sync_agent reads `binary`)
    built.write_text("fake built exe")
    source = {"mtime": 0.0}
    monkeypatch.setattr(common_nest, mtime_hook, lambda repo: source["mtime"])

    class _Result:
        returncode = 0
        stderr = ""
        stdout = '{"executable": "%s"}' % str(built).replace("\\", "\\\\")

    def _slot_wrapped_cargo(argv, **kw):
        assert "build-slot.py" in str(argv[1]), f"not slot-wrapped: {argv}"
        grant_file = (kw.get("env") or {}).get("FAUNA_SLOT_GRANT_FILE")
        assert grant_file, "the slot-wrapped run must name a grant file for the slot script"
        now = time.time()
        source["mtime"] = now + 50  # edited while queued (after the request)
        Path(grant_file).write_text(repr(now + 100))  # the slot is granted
        if edit_after_grant:
            source["mtime"] = now + 150  # edited while cargo runs
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text("fake built exe")
        return _Result()

    monkeypatch.setattr(common_nest.subprocess, "run", _slot_wrapped_cargo)
    fn = _builder_fn(builder)
    fn()
    return fn


@pytest.mark.parametrize("builder,mtime_hook,paths,stem", _STAMP_WRITERS, ids=_STAMP_WRITER_IDS)
def test_edit_while_queued_leaves_the_tree_warm(tmp_path, monkeypatch, builder, mtime_hook,
                                                paths, stem):
    """An input edited while the build waits for its slot is compiled by the
    build, so the in-hold re-check must be a stamp read: no second slot run."""
    fn = _queued_build(tmp_path, monkeypatch, builder, mtime_hook, paths, stem,
                       edit_after_grant=False)

    def _no_subprocess(*a, **k):
        raise AssertionError(
            f"{builder}'s re-check queued for a build slot again: its stamp was "
            "dated before the grant, so an edit the build compiled looks newer"
        )

    monkeypatch.setattr(common_nest.subprocess, "run", _no_subprocess)
    fn()


@pytest.mark.parametrize("builder,mtime_hook,paths,stem", _STAMP_WRITERS, ids=_STAMP_WRITER_IDS)
def test_edit_after_the_grant_still_rebuilds(tmp_path, monkeypatch, builder, mtime_hook,
                                             paths, stem):
    """The twin: an edit made after the grant (while cargo runs) may have
    missed the compile, so the tree must stay stale and the next call rebuild."""
    fn = _queued_build(tmp_path, monkeypatch, builder, mtime_hook, paths, stem,
                       edit_after_grant=True)

    def _rebuild(*a, **k):
        raise RuntimeError("rebuild attempted")  # enough: the re-check went stale

    monkeypatch.setattr(common_nest.subprocess, "run", _rebuild)
    with pytest.raises(RuntimeError, match="rebuild attempted"):
        fn()


@pytest.mark.parametrize("builder,mtime_hook,paths,stem", _STAMP_WRITERS[:2], ids=_STAMP_WRITER_IDS[:2])
def test_a_refused_build_over_an_older_binary_commits_no_stamp(
    tmp_path, monkeypatch, builder, mtime_hook, paths, stem
):
    """A build the slot refused (exit 75 — the preflight's plan probe refuses
    every compile on purpose) built nothing, so an older binary on disk must
    not be stamped fresh: on windows the agent builder once judged success by
    the exe existing alone, stamped a two-day-old agent at the plan probe, and
    the real warm pass then skipped the build (2026-11-04)."""
    target_dir = tmp_path / "cargo-target"
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    build_slot = tmp_path / "scripts" / "build-slot.py"
    build_slot.parent.mkdir(parents=True, exist_ok=True)
    build_slot.write_text("")
    binary, stamp = paths(target_dir)
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text("an older build")
    monkeypatch.setattr(common_nest, mtime_hook, lambda repo: time.time() + 1000)

    class _Refused:
        returncode = 75
        stderr = "[build-slot] REFUSED: refused a 'build' slot. Nothing was built."
        stdout = ""

    monkeypatch.setattr(common_nest.subprocess, "run", lambda argv, **kw: _Refused())
    with pytest.raises(RuntimeError):
        _builder_fn(builder)()
    assert not stamp.exists(), f"{builder} stamped a build that never ran"


# ── No compile under a held lane: the refuse-mode loop ────────────────────────
#
# the in-hold re-check runs with
# `build` acquires REFUSED, so a tree found cold after all (the warm pass
# failed, an input moved during the lane wait, the target was evicted) never
# compiles with the lane idle behind the build queue. conftest releases the
# lane, warms holding nothing, re-queues, re-checks — bounded, then builds in
# the hold as before. Measured before: 1 h 53 min of a 2 h 24 min `e2e_other`
# hold spent queued for `build` (2026-09-29).


class _FakeSlotModule:
    """The slice of build-slot.py conftest touches, with env markers kept the
    way the real module keeps them (so the reentrancy short-circuit the
    pool-aware release exists to defeat is exercised, not assumed)."""

    E2E_LONG_POOL = "e2e_long"
    LONG_LANE_MIN_HEAVY_TESTS = 100
    REFUSE_BUILD_ENV = "FAUNA_SLOT_REFUSE_BUILD"

    class SlotRefusedError(RuntimeError):
        pass

    class SlotOrderError(RuntimeError):
        pass

    SIM_BOOT_POOL = "sim_boot"
    SIM_BOOT_CORES = 12

    def __init__(self):
        self.events = []
        self._next_fd = 100
        self._pool_of = {}
        #: False: the bound never leaves the boot its room (acquire times out).
        self.sim_boot_room = True

    @staticmethod
    def e2e_app_pool(apps):
        return "e2e_other"

    @staticmethod
    def e2e_lane(heavy_tests):
        return "short"

    @staticmethod
    def sim_boot_enabled(platform=None):
        return True

    def acquire_slot(self, pool, cmd=None, timeout_s=None):
        import os

        if pool == self.SIM_BOOT_POOL:
            # The real one: no count, admitted by the bound at its figure.
            self.events.append(("acquire", pool, timeout_s))
            if not self.sim_boot_room:
                raise TimeoutError("grant HELD by the core bound: 9.0 busy of 14, needs 12")
            self._next_fd += 1
            self._pool_of[self._next_fd] = pool
            return self._next_fd
        marker = f"FAUNA_SLOT_HELD_{pool.upper()}"
        if os.environ.get(marker) or (pool != "e2e_long" and os.environ.get("FAUNA_SLOT_HELD_E2E")):
            self.events.append(("reentrant", pool))
            return None
        os.environ[marker] = "1"
        if pool != "e2e_long":
            os.environ["FAUNA_SLOT_HELD_E2E"] = "1"
        self._next_fd += 1
        self._pool_of[self._next_fd] = pool
        self.events.append(("acquire", pool))
        return self._next_fd

    def log_lane_hold(self, fd, apps, inhold_prebuild_s=None, inhold_setup_s=None):
        # The real writer records a hold only for a lane fd it granted.
        pool = self._pool_of.get(fd)
        if pool not in (None, self.E2E_LONG_POOL):
            self.events.append(("hold", pool, sorted(apps), inhold_prebuild_s, inhold_setup_s))

    def annotate_note(self, fd, fields):
        # The real one merges `fields` into the holder note of a live grant.
        self.events.append(("note", self._pool_of.get(fd), dict(fields)))

    def release_slot(self, fd, pool=None):
        import os

        self._pool_of.pop(fd, None)
        self.events.append(("release", pool))
        if pool is not None:
            os.environ.pop(f"FAUNA_SLOT_HELD_{pool.upper()}", None)
            if pool not in ("e2e_long", self.SIM_BOOT_POOL):
                os.environ.pop("FAUNA_SLOT_HELD_E2E", None)


class _FakeItem:
    def __init__(self, nodeid="tests/test_x.py::test_y[linux]"):
        self.nodeid = nodeid
        self.fixturenames = []

    @staticmethod
    def iter_markers():
        class _M:
            name = "tier_3"

        return [_M()]


class _FakeSession:
    def __init__(self):
        self.items = [_FakeItem()]

        class _Opt:
            collectonly = False

        class _Cfg:
            option = _Opt()

        self.config = _Cfg()


@pytest.fixture
def refuse_loop(monkeypatch):
    """`pytest_collection_finish` wired to a fake slot module and fake
    prebuilds. `tree["cold"]` decides whether the in-hold re-check would need
    a build; the fake in-hold prebuild refuses exactly the way the real slot
    script does — by appending the command to the marker file the env names —
    and the fake warm pass makes the tree warm (`warms=True`) or leaves it cold."""
    import os

    mod = _FakeSlotModule()
    tree = {"cold": True, "warms": True}
    events = mod.events

    def fake_prebuild_binaries(session, *, prewarm=False):
        if prewarm:
            events.append("warm-build")
            if tree["warms"]:
                tree["cold"] = False
            return
        marker = os.environ.get(getattr(mod, "REFUSE_BUILD_ENV", "FAUNA_SLOT_REFUSE_BUILD"))
        if tree["cold"] and marker:
            events.append("in-hold-refused")
            with open(marker, "a", encoding="utf-8") as fh:
                fh.write("cargo build -p fauna-nest --features test-hooks,nostr\n")
            return
        events.append("in-hold-built" if tree["cold"] else "in-hold-warm")

    def fake_prewarm(session):
        events.append("warm-pass")
        fake_prebuild_binaries(session, prewarm=True)
        if tree.pop("moves_after_warm", False):
            tree["cold"] = True  # an input edited while the run queued for its lane

    for name in ("FAUNA_SLOT_HELD_E2E", "FAUNA_SLOT_HELD_E2E_OTHER", "FAUNA_SLOT_HELD_E2E_LONG",
                 "FAUNA_SLOT_HELD_BUILD", mod.REFUSE_BUILD_ENV):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(conftest, "_build_slot_module", lambda: mod)
    monkeypatch.setattr(conftest, "_refuse_drafts_window_collision", lambda session: None)
    monkeypatch.setattr(conftest, "_run_app_set", lambda session: {"linux"})
    monkeypatch.setattr(conftest, "_prewarm_before_slot", lambda mod: True)
    monkeypatch.setattr(conftest, "_prewarm_prebuilds", fake_prewarm)
    monkeypatch.setattr(conftest, "_prebuild_web_spa", lambda session: events.append("spa"))
    monkeypatch.setattr(conftest, "_prebuild_binaries", fake_prebuild_binaries)
    # The plan header's job lists (empty unless a test names jobs).
    tree["jobs"] = []
    monkeypatch.setattr(conftest, "_web_spa_jobs", lambda session: [])
    monkeypatch.setattr(conftest, "_prebuild_jobs", lambda session: list(tree["jobs"]))
    monkeypatch.delenv(conftest.PLAN_ONLY_ENV, raising=False)
    monkeypatch.setattr(conftest, "_E2E_SLOT", None)
    yield mod, tree
    # A warm-only run on a warm tree arms the guard through the real path; left
    # armed, it would put the run hosting this test into refuse mode.
    conftest._disarm_warm_only_guard()
    if conftest._E2E_SLOT is not None:
        conftest._E2E_SLOT = None
    for name in ("FAUNA_SLOT_HELD_E2E", "FAUNA_SLOT_HELD_E2E_OTHER", "FAUNA_SLOT_HELD_E2E_LONG"):
        os.environ.pop(name, None)


def test_a_cold_in_hold_recheck_releases_the_lane_warms_and_requeues(refuse_loop):
    """The tree is cold when the lane is granted (it moved during the wait):
    the in-hold re-check is REFUSED its build, the lane is released — markers
    cleared, so the re-acquire really queues instead of short-circuiting as
    reentrant — the warm pass compiles holding nothing, the lane is taken again
    and the second re-check is a warm no-op. Nothing is ever built in the hold."""
    import os

    mod, tree = refuse_loop
    tree["moves_after_warm"] = True
    conftest.pytest_collection_finish(_FakeSession())
    lane_only = [e for e in mod.events if e != "spa" and not (isinstance(e, tuple) and e[0] == "hold")]
    assert lane_only == [
        "warm-pass", "warm-build",          # the warm pass — but the tree moves after it
        ("acquire", "e2e_other"),
        "in-hold-refused",                   # refused: nothing compiled with the lane held
        ("release", "e2e_other"),            # pool-aware release clears the markers
        "warm-pass", "warm-build",          # compiled holding no lane
        ("acquire", "e2e_other"),            # a REAL re-acquire, not a reentrant None
        "in-hold-warm",                      # the re-check is a stamp read
    ], mod.events
    assert "in-hold-built" not in mod.events
    assert conftest._E2E_SLOT is not None and conftest._E2E_SLOT[1], "the lane is held for the tests"
    assert os.environ.get("FAUNA_SLOT_HELD_E2E") == "1"
    assert mod.REFUSE_BUILD_ENV not in os.environ, "refuse mode is off before the first test runs"


def test_a_warm_in_hold_recheck_keeps_the_lane_without_a_cycle(refuse_loop):
    """The common case: the warm pass held, the in-hold re-check takes no slot,
    and the lane is neither released nor re-queued."""
    mod, tree = refuse_loop
    tree["cold"] = False
    conftest.pytest_collection_finish(_FakeSession())
    acquires = [e for e in mod.events if isinstance(e, tuple) and e[0] == "acquire"]
    releases = [e for e in mod.events if isinstance(e, tuple) and e[0] == "release"]
    assert len(acquires) == 1 and not releases, mod.events
    assert "in-hold-refused" not in mod.events


def test_the_cycle_is_bounded_and_then_builds_in_the_hold_as_before(refuse_loop):
    """A path that takes `build` regardless of freshness would cycle forever;
    after `_INHOLD_REFUSE_RETRIES` release-and-warm cycles the run builds inside
    the hold, as every run did before 2026-09-29 — the lock never makes work
    impossible."""
    mod, tree = refuse_loop
    tree["warms"] = False
    conftest.pytest_collection_finish(_FakeSession())
    assert mod.events.count("in-hold-refused") == conftest._INHOLD_REFUSE_RETRIES
    assert mod.events.count(("release", "e2e_other")) == conftest._INHOLD_REFUSE_RETRIES
    assert mod.events[-1] == "in-hold-built", mod.events
    assert conftest._E2E_SLOT is not None and conftest._E2E_SLOT[1]


def test_a_slot_module_predating_refuse_mode_runs_the_in_hold_pass_as_before(refuse_loop):
    """The machine's policy copy may predate refuse mode: no `REFUSE_BUILD_ENV`
    means no marker is ever set, the in-hold pass runs unconditionally and the
    lane is released exactly once, at session finish."""
    mod, tree = refuse_loop
    tree["moves_after_warm"] = True
    del _FakeSlotModule.REFUSE_BUILD_ENV
    try:
        conftest.pytest_collection_finish(_FakeSession())
    finally:
        _FakeSlotModule.REFUSE_BUILD_ENV = "FAUNA_SLOT_REFUSE_BUILD"
    assert mod.events[-1] == "in-hold-built", mod.events
    assert ("release", "e2e_other") not in mod.events


# ── The background tester's warm-only shard (delayed-testing.md § The background
# tester: the tester never compiles; a tree whose binaries are not warm is a shard
# not run).


def test_a_warm_only_run_on_a_cold_tree_compiles_nothing_and_ends_interrupted(
    refuse_loop, monkeypatch, capsys
):
    """No warm pass, one refusing re-check under the lane, then an INTERRUPTED exit
    — the status the last-green writer declines, so the shard records nothing —
    printing the line the merge-gate check's tester greps for."""
    import os

    mod, tree = refuse_loop
    monkeypatch.setenv(conftest.WARM_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception) as exited:
        conftest.pytest_collection_finish(_FakeSession())
    assert exited.value.returncode == pytest.ExitCode.INTERRUPTED
    assert "warm-pass" not in mod.events and "warm-build" not in mod.events, mod.events
    assert "in-hold-refused" in mod.events and "in-hold-built" not in mod.events, mod.events
    assert ("release", "e2e_other") not in mod.events, "session finish releases the lane"
    assert conftest.WARM_ONLY_REFUSAL in capsys.readouterr().err
    assert mod.REFUSE_BUILD_ENV not in os.environ


def test_a_warm_only_run_on_a_warm_tree_proceeds_on_stamp_reads(refuse_loop, monkeypatch):
    mod, tree = refuse_loop
    tree["cold"] = False
    monkeypatch.setenv(conftest.WARM_ONLY_ENV, "1")
    conftest.pytest_collection_finish(_FakeSession())
    assert "warm-pass" not in mod.events and "in-hold-refused" not in mod.events, mod.events
    assert [e for e in mod.events if isinstance(e, tuple) and e[0] == "acquire"] == [
        ("acquire", "e2e_other")]
    assert conftest._E2E_SLOT is not None and conftest._E2E_SLOT[1]


class _HeavyItem:
    def __init__(self, tier):
        self.tier = tier

    def iter_markers(self):
        return [types.SimpleNamespace(name=self.tier)]


def test_a_warm_only_run_keeps_refuse_mode_on_around_each_heavy_test(refuse_loop, monkeypatch):
    """A warm tree's shard keeps refuse mode for its tests, not just its re-check:
    measured 2026-11-03 on Windows, a verify shard took its lane warm and its first
    windows test compiled the FlaUI bridge cold under it (delayed-testing.md § The
    warm step: a shard never compiles). Set around a tier_2/3/4 test only — a
    tier_1 test is in-process and may drive `build-slot.py` as its subject."""
    import os

    mod, tree = refuse_loop
    tree["cold"] = False
    monkeypatch.setenv(conftest.WARM_ONLY_ENV, "1")
    conftest.pytest_collection_finish(_FakeSession())
    try:
        assert conftest._WARM_ONLY_GUARD is not None, "a warm tree's run arms the guard"
        assert mod.REFUSE_BUILD_ENV not in os.environ, "off between tests"
        for tier, armed in (("tier_3", True), ("tier_1", False)):
            gen = conftest.pytest_runtest_protocol(_HeavyItem(tier), None)
            next(gen)
            assert (os.environ.get(mod.REFUSE_BUILD_ENV) == conftest._WARM_ONLY_GUARD[1]) is armed, tier
            with pytest.raises(StopIteration):
                gen.send(None)
            assert mod.REFUSE_BUILD_ENV not in os.environ, f"{tier}: off again after the test"
    finally:
        conftest._disarm_warm_only_guard()
    assert conftest._WARM_ONLY_GUARD is None


def test_an_ordinary_run_never_arms_the_guard(refuse_loop):
    mod, tree = refuse_loop
    tree["cold"] = False
    conftest.pytest_collection_finish(_FakeSession())
    assert conftest._WARM_ONLY_GUARD is None, "a session's run builds in-test as before"


def test_a_build_a_warm_only_test_started_ends_the_run_before_its_report_is_noted(
    monkeypatch, tmp_path, capsys
):
    """The refusal surfaces as the line the tester greps for (`notwarm`, the shard
    passed over), and the run ends INTERRUPTED before the refused test is noted —
    so it is recorded neither red nor green, and the last-green directory declines
    the interrupted run whole."""
    marker = tmp_path / "refused.txt"
    marker.write_text("", encoding="utf-8")
    monkeypatch.setattr(conftest, "_WARM_ONLY_GUARD", ("FAUNA_SLOT_REFUSE_BUILD", str(marker)))
    noted = []
    monkeypatch.setattr(conftest, "_last_green_note", lambda report, outcome: noted.append(report.nodeid))
    monkeypatch.setattr(conftest, "_feature_ledger_note", lambda report, outcome: noted.append(report.nodeid))
    report = types.SimpleNamespace(nodeid="tests/test_x.py::test_x[windows]", when="setup",
                                   outcome="failed", duration=0.1, longrepr="refused")
    monkeypatch.setattr(conftest, "_print_live_reason", lambda report, label: None)
    conftest.pytest_runtest_logreport(report)
    assert noted, "an empty marker: the report is noted as ever"
    noted.clear()
    marker.write_text("just windows-flaui-bridge: dotnet build tests/e2e-unified/flaui-bridge -c Debug\n",
                      encoding="utf-8")
    try:
        with pytest.raises(pytest.exit.Exception) as exited:
            conftest.pytest_runtest_logreport(report)
    finally:
        # Disarmed before this test's own reports reach the hook of the real run.
        conftest._WARM_ONLY_GUARD = None
    assert exited.value.returncode == pytest.ExitCode.INTERRUPTED
    assert noted == [], "the refused test is not recorded"
    err = capsys.readouterr().err
    assert conftest.WARM_ONLY_REFUSAL in err and "test_x[windows] (setup)" in err
    assert "dotnet build tests/e2e-unified/flaui-bridge" in err


def test_a_windows_run_warms_the_flaui_bridge_before_its_lane(monkeypatch):
    """The driver builds the bridge at each launch; the warm pass builds it first,
    through the driver's own call, so the in-lane launch is a stamp read."""
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda session, requested: {"windows"})
    jobs = conftest._prebuild_jobs(types.SimpleNamespace(items=[]))
    assert ("windows flaui bridge", "_ensure_windows_flaui_bridge_built", ()) in jobs
    assert "_ensure_windows_flaui_bridge_built" not in conftest._NOT_PREWARMED_BUILDERS
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda session, requested: {"tui"})
    assert not any(b == "_ensure_windows_flaui_bridge_built"
                   for _l, b, _a in conftest._prebuild_jobs(types.SimpleNamespace(items=[])))
    from drivers import windows as windows_driver

    calls = []
    monkeypatch.setattr(windows_driver, "_ensure_bridge_built", lambda: calls.append(1))
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    conftest._ensure_windows_flaui_bridge_built()
    conftest._ensure_windows_flaui_bridge_built()
    assert calls == [1], "memoized: the warm pass's build is replayed in the hold"


_PRIORITY_PROBE = """
import ctypes, subprocess, sys
from ctypes import wintypes

k32 = ctypes.WinDLL("kernel32")
k32.GetCurrentProcess.restype = wintypes.HANDLE
k32.GetPriorityClass.argtypes = [wintypes.HANDLE]


def own():
    return hex(k32.GetPriorityClass(k32.GetCurrentProcess()))


if sys.argv[1] == "child":
    print(own())
    raise SystemExit(0)
sys.path.insert(0, sys.argv[1])
from helpers.shard_priority import raise_to_normal

before = own()
assert raise_to_normal()
child = subprocess.run([sys.executable, __file__, "child"], capture_output=True, text=True)
print(before, own(), child.stdout.strip())
"""


@pytest.mark.skipif(sys.platform != "win32", reason="Windows priority classes")
def test_a_shard_leaves_the_checks_below_normal_priority_and_its_children_follow(tmp_path):
    """The check's root is below normal and Windows hands that class to every child
    a below-normal process creates, so without the raise a shard's nest and app would
    run below every session on the box (delayed-testing.md: a shard is not niced)."""
    import subprocess

    probe = tmp_path / "probe.py"
    probe.write_text(_PRIORITY_PROBE, encoding="utf-8")
    out = subprocess.run([sys.executable, str(probe), str(Path(conftest.__file__).parent)],
                         capture_output=True, text=True, timeout=120,
                         creationflags=subprocess.BELOW_NORMAL_PRIORITY_CLASS)
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["0x4000", "0x20", "0x20"], out.stdout


@pytest.mark.parametrize("warm_only,calls", [(True, 1), (False, 0)])
def test_only_a_shard_raises_its_priority_at_session_start(monkeypatch, warm_only, calls):
    from helpers import shard_priority

    raised = []
    monkeypatch.setattr(shard_priority, "raise_to_normal", lambda: raised.append(1) or True)
    monkeypatch.setattr(conftest.sys, "platform", "win32")
    monkeypatch.delenv(conftest.WARM_ONLY_ENV, raising=False)
    if warm_only:
        monkeypatch.setenv(conftest.WARM_ONLY_ENV, "1")
    conftest.pytest_sessionstart(None)
    assert len(raised) == calls


def test_a_prewarm_only_run_warms_takes_no_lane_and_exits_ok(refuse_loop, monkeypatch):
    """The tester's warm step (delayed-testing.md § The background tester, *The
    warm step*): the warm pass runs — whatever `_prewarm_before_slot` says, so on
    macOS too — no lane is acquired, no in-hold re-check runs, and the session
    exits OK."""
    mod, tree = refuse_loop
    monkeypatch.setattr(conftest, "_prewarm_before_slot", lambda mod: False)
    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception) as exited:
        conftest.pytest_collection_finish(_FakeSession())
    assert exited.value.returncode == pytest.ExitCode.OK
    assert mod.events[:2] == ["warm-pass", "warm-build"], mod.events
    assert not [e for e in mod.events if isinstance(e, tuple) and e[0] == "acquire"], mod.events
    assert not [e for e in mod.events if str(e).startswith("in-hold")], mod.events
    assert conftest._E2E_SLOT is None


def test_a_prewarm_only_run_whose_build_fails_exits_non_zero(refuse_loop, monkeypatch, capsys):
    mod, tree = refuse_loop
    monkeypatch.setattr(conftest, "_prewarm_prebuilds",
                        lambda session: ["nest_binary: RuntimeError('cargo exited 101')"])
    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception) as exited:
        conftest.pytest_collection_finish(_FakeSession())
    assert exited.value.returncode != pytest.ExitCode.OK
    err = capsys.readouterr().err
    assert conftest.PREWARM_ONLY_FAILED in err and "cargo exited 101" in err
    assert conftest._E2E_SLOT is None


@pytest.fixture
def plan_world(refuse_loop, monkeypatch):
    """The preflight's fixture world: one warm job and one stale job, behind a
    slot module that knows the plan probe. The stale builder refuses the way
    the real slot script does under `PLAN_PROBE_ENV` — the command appended to
    the marker, the recipe failing — and every builder records whether it ran
    under the probe (`calls`)."""
    import os

    mod, tree = refuse_loop
    mod.PLAN_PROBE_ENV = "FAUNA_SLOT_PLAN_PROBE"
    mod._status_lines = lambda pool: [
        "[build-slot] status (slot dir /x):",
        f"  {pool}-0.lock: BUSY — pid 1 (alive)",
        f"  {pool}-1.lock: FREE",
    ]
    mod._disk_free_gb = lambda: 80.0
    mod._disk_floor_gb = lambda pool: 40.0
    monkeypatch.delenv(mod.PLAN_PROBE_ENV, raising=False)
    calls = []

    def warm():
        calls.append(("warm", bool(os.environ.get(mod.PLAN_PROBE_ENV))))

    def stale():
        probing = bool(os.environ.get(mod.PLAN_PROBE_ENV))
        calls.append(("stale", probing))
        if probing:
            with open(os.environ[mod.REFUSE_BUILD_ENV], "a", encoding="utf-8") as fh:
                fh.write("cargo build -p fauna-tui\n")
            raise RuntimeError("just exited 75")

    def setup():
        calls.append(("setup", True))

    monkeypatch.setattr(conftest, "_plan_fake_warm", warm, raising=False)
    monkeypatch.setattr(conftest, "_plan_fake_stale", stale, raising=False)
    monkeypatch.setattr(conftest, "_get_ios_setup", setup)
    tree["jobs"] = [
        ("nest_binary", "_plan_fake_warm", ()),
        ("app[tui]", "_plan_fake_stale", ()),
        ("ios simulator (create + finish booting)", "_get_ios_setup", ()),
    ]
    from helpers import prebuild_donor

    monkeypatch.setattr(prebuild_donor, "donor_line", lambda: "donor: (the fixture's)")
    yield mod, tree, calls


def _plan_lines(out: str) -> list[str]:
    return [ln for ln in out.splitlines() if ln.startswith(conftest.PLAN_PREFIX)]


def test_the_plan_header_names_the_lane_each_job_warm_or_stale_and_the_floor(
        plan_world, monkeypatch, capsys):
    """`just e2e-preflight` (build-slot-pool.md § Cross-pool acquisition order →
    *The preflight*): before the warm pass builds anything, one `[e2e-plan]` line
    per item — the lane's own status, each job from its freshness gate (the warm
    job returns, the stale one is refused at the slot and named by its command),
    session setup named but never run, the disk floor — and then the warm pass."""
    mod, tree, calls = plan_world
    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception) as exited:
        conftest.pytest_collection_finish(_FakeSession())
    assert exited.value.returncode == pytest.ExitCode.OK
    lines = _plan_lines(capsys.readouterr().out)
    assert lines[0].startswith("[e2e-plan] run: 1 tier_2/3/4 test(s), apps ['linux']"), lines
    assert "[e2e-plan] lane e2e_other: e2e_other-0.lock: BUSY — pid 1 (alive)" in lines, lines
    assert "[e2e-plan] lane e2e_other: e2e_other-1.lock: FREE" in lines, lines
    assert "[e2e-plan] job nest_binary: warm" in lines, lines
    assert ("[e2e-plan] job app[tui]: stale (would compile: cargo build -p fauna-tui) — "
            "cold cost: no record yet") in lines, lines
    assert ("[e2e-plan] job ios simulator (create + finish booting): would run "
            "(session setup — the plan does not run it)") in lines, lines
    assert "[e2e-plan] disk: 80G free, build floor 40G — ok" in lines, lines
    # Every probe ran under the probe, session setup not at all; then the warm pass.
    assert calls == [("warm", True), ("stale", True)], calls
    assert mod.events[:2] == ["warm-pass", "warm-build"], mod.events
    assert not [e for e in mod.events if isinstance(e, tuple) and e[0] == "acquire"], mod.events


def test_a_plan_only_run_prints_the_header_and_builds_nothing(plan_world, monkeypatch, capsys):
    """`--plan` (`PLAN_ONLY_ENV`): the header, then exit 0 — no warm pass, no lane,
    no slot, and no builder called except under the probe, where every `build`
    acquire is refused."""
    mod, tree, calls = plan_world
    monkeypatch.setenv(conftest.PLAN_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception) as exited:
        conftest.pytest_collection_finish(_FakeSession())
    assert exited.value.returncode == pytest.ExitCode.OK
    assert "[e2e-plan] job app[tui]: stale (would compile: cargo build -p fauna-tui) — "\
        "cold cost: no record yet" in \
        _plan_lines(capsys.readouterr().out)
    assert all(probing for _, probing in calls) and ("setup", True) not in calls, calls
    assert mod.events == [], mod.events
    assert conftest._E2E_SLOT is None


def test_a_slot_copy_without_the_probe_calls_no_builder(plan_world, monkeypatch, capsys):
    """A machine copy predating the plan probe would let a probe compile: the
    header names every job `unknown`, with the reason, and calls none of them."""
    mod, tree, calls = plan_world
    del mod.PLAN_PROBE_ENV
    monkeypatch.setenv(conftest.PLAN_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception):
        conftest.pytest_collection_finish(_FakeSession())
    lines = _plan_lines(capsys.readouterr().out)
    assert ("[e2e-plan] job nest_binary: unknown (the machine's build-slot copy has "
            "no plan probe yet)") in lines, lines
    assert calls == [], calls



def test_the_plan_header_carries_the_donor_line_and_a_stale_jobs_cold_cost(
        plan_world, monkeypatch, capsys):
    """Point 8 of the standing donor (build-target-layout-macos.md): the header
    says what a cold target would seed from, and a stale job carries its cold
    cost from the timings record — `no record yet` while it is empty."""
    from helpers import prebuild_donor

    monkeypatch.setattr(prebuild_donor, "donor_line", lambda: "no donor: the fixture's")
    monkeypatch.setenv(conftest.PLAN_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception):
        conftest.pytest_collection_finish(_FakeSession())
    lines = _plan_lines(capsys.readouterr().out)
    assert "[e2e-plan] no donor: the fixture's" in lines, lines
    assert ("[e2e-plan] job app[tui]: stale (would compile: cargo build -p fauna-tui) — "
            "cold cost: no record yet") in lines, lines
    assert "[e2e-plan] job nest_binary: warm" in lines, lines


def test_the_warm_pass_returns_what_failed_and_forgets_what_built(monkeypatch):
    """`_prewarm_prebuilds` reports its failures (the prewarm-only run's exit
    reads them) and keeps them memoized, while every success is forgotten for the
    in-hold re-check."""
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})

    def fake_binaries(session, *, prewarm=False):
        conftest._BINARY_BUILD_MEMO["nest_binary"] = ("ok", "/bin/nest")
        conftest._BINARY_BUILD_MEMO["app:tui"] = ("failed", RuntimeError("boom"))

    def fake_spa(session):
        raise RuntimeError("deno exited 1")

    monkeypatch.setattr(conftest, "_prebuild_web_spa", fake_spa)
    monkeypatch.setattr(conftest, "_prebuild_binaries", fake_binaries)
    failed = conftest._prewarm_prebuilds(_FakeSession())
    assert failed == ["web SPA: deno exited 1", "app:tui: boom"]
    assert list(conftest._BINARY_BUILD_MEMO) == ["app:tui"]


# ── The phase line (e2e-conventions.md point 9 → *A run names its phase while it
# runs*): one `[e2e-phase]` line per boundary, in order, which detached-run.py's
# grade reads.
_REAL_PREWARM_PREBUILDS = conftest._prewarm_prebuilds
_PHASE_LINE = re.compile(r"\[e2e-phase\] (.+?) — (.*) @(\S+)$")


def _phases(out: str) -> list[tuple[str, str]]:
    found = []
    for line in out.splitlines():
        m = _PHASE_LINE.search(line)
        if m:
            found.append((m.group(1), m.group(2)))
    return found


def test_a_prewarm_only_run_emits_its_boundaries_in_order(refuse_loop, monkeypatch, capsys):
    """collect → warm pass → one `prebuild <label>` per job the warm pass runs —
    and no lane phase, since the run takes no lane. The real warm pass and the
    real job loop, over a fake builder."""
    mod, tree = refuse_loop
    monkeypatch.setattr(conftest, "_PHASE_CONFIG", None)
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    monkeypatch.setattr(conftest, "_prewarm_prebuilds", _REAL_PREWARM_PREBUILDS)
    monkeypatch.setattr(conftest, "_fake_phase_builder", lambda: None, raising=False)
    monkeypatch.setattr(conftest, "_prebuild_binaries", lambda session, *, prewarm=False:
                        conftest._run_prebuild_jobs(
                            [("nest binary", "_fake_phase_builder", ()),
                             ("app tui", "_fake_phase_builder", ())], prewarm=prewarm))
    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    with pytest.raises(pytest.exit.Exception):
        conftest.pytest_collection_finish(_FakeSession())
    assert _phases(capsys.readouterr().out) == [
        ("collect", "1 items collected"),
        ("warm pass", "building before the e2e lane, holding no e2e slot"),
        ("prebuild", "nest binary"),
        ("prebuild", "app tui"),
    ]


def test_a_warm_only_run_emits_its_boundaries_in_order(refuse_loop, monkeypatch, capsys):
    """collect → lane wait → lane held → in-hold re-check (refuse mode), then the
    test loop's `tests` and each item's `test <node id>` — and no warm pass."""
    mod, tree = refuse_loop
    tree["cold"] = False
    monkeypatch.setattr(conftest, "_PHASE_CONFIG", None)
    monkeypatch.setenv(conftest.WARM_ONLY_ENV, "1")
    session = _FakeSession()
    conftest.pytest_collection_finish(session)
    conftest.pytest_runtestloop(session)
    conftest.pytest_runtest_logstart(session.items[0].nodeid, None)
    assert _phases(capsys.readouterr().out) == [
        ("collect", "1 items collected"),
        ("lane wait", "e2e_other"),
        ("lane held", "e2e_other"),
        ("in-hold re-check", "refuse mode — a cold tree is refused its build"),
        ("tests", "1 selected"),
        ("test", "tests/test_x.py::test_y[linux]"),
    ]


def test_a_prewarm_only_run_writes_no_last_green_record(monkeypatch, capsys):
    from helpers import last_green

    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    monkeypatch.setattr(last_green, "write", lambda *a, **k: pytest.fail("wrote the directory"))
    conftest._write_last_green(None, pytest.ExitCode.OK)
    assert "NOT written — a prewarm-only run" in capsys.readouterr().out


def test_an_interrupted_session_writes_no_last_green_record(monkeypatch, capsys):
    """A session that ends INTERRUPTED (a Ctrl-C, a warm-only refusal) is declined
    by the directory's writer — its tests stay neither red nor green."""
    from helpers import last_green

    monkeypatch.setattr(last_green, "write", lambda *a, **k: pytest.fail("wrote the directory"))
    conftest._write_last_green(None, pytest.ExitCode.INTERRUPTED)
    assert "NOT written — the session was interrupted" in capsys.readouterr().out


# ── The tester runs to the end: the boundary stop of the
# retired yield is gone — no flag is read, and no interrupted session is written.

def test_no_boundary_stop_is_left_in_the_harness(monkeypatch, capsys):
    from helpers import last_green

    for name in ("YIELD_FILE_ENV", "YIELD_STOPPED_SUFFIX", "_yield_stop", "_yield_at_boundary"):
        assert not hasattr(conftest, name), f"conftest still carries the retired yield's {name}"
    monkeypatch.setenv("FAUNA_E2E_YIELD_FILE", "/nonexistent/yield")
    monkeypatch.setattr(last_green, "write", lambda *a, **k: pytest.fail("wrote the directory"))
    conftest._write_last_green(None, pytest.ExitCode.INTERRUPTED)
    assert "NOT written — the session was interrupted" in capsys.readouterr().out


# ── The lane usage record: every lane release logs its
# hold — the refuse loop's and the session end's alike — before the lock goes.


def _holds(mod):
    return [e for e in mod.events if isinstance(e, tuple) and e[0] == "hold"]


def _notes(mod):
    return [e for e in mod.events if isinstance(e, tuple) and e[0] == "note"]


def test_a_simulator_registered_by_a_test_is_named_in_the_held_lanes_note(refuse_loop, monkeypatch):
    """A device a test boots with its own `simctl create` (the File Provider
    eviction test) is registered like the conftest's own throwaway devices:
    appended to the run's list and named in every lane note the run holds
    (`sims`), so the agent charges its boot to the run. No `atexit`: deleting
    the device stays the test's."""
    mod, tree = refuse_loop
    tree["cold"] = False
    monkeypatch.setattr(conftest, "_E2E_SIMULATORS", [])
    conftest.pytest_collection_finish(_FakeSession())
    conftest.register_simulator("UDID-1")
    conftest.register_simulator("UDID-2")
    assert conftest._E2E_SIMULATORS == ["UDID-1", "UDID-2"]
    assert _notes(mod)[-2:] == [("note", "e2e_other", {"sims": ["UDID-1"]}),
                                ("note", "e2e_other", {"sims": ["UDID-1", "UDID-2"]})], mod.events


def test_a_lane_taken_after_a_device_exists_is_noted_with_it(refuse_loop, monkeypatch):
    """The refuse loop hands the lane back and takes a fresh one: the new note
    names the run's simulators too, else the run's whole iOS load reads as the
    desktop's for the rest of the run. Every acquire is followed by the note."""
    mod, tree = refuse_loop
    tree["moves_after_warm"] = True
    monkeypatch.setattr(conftest, "_E2E_SIMULATORS", ["UDID-1"])
    conftest.pytest_collection_finish(_FakeSession())
    acquires = [i for i, e in enumerate(mod.events) if e == ("acquire", "e2e_other")]
    notes = [i for i, e in enumerate(mod.events) if e == ("note", "e2e_other", {"sims": ["UDID-1"]})]
    assert len(acquires) == 2 and len(notes) == 2, mod.events
    assert notes[0] == acquires[0] + 1 and notes[1] == acquires[1] + 1, mod.events


def test_a_refused_hold_and_its_requeued_hold_are_two_usage_lines(refuse_loop):
    """A run that handed its lane back shows as two short holds, not one long
    one: the refuse loop logs the first hold at its release, the session end
    logs the second. Each is logged BEFORE its release (the writer keys on the
    live grant) and carries the run's app set and its own in-hold prebuild time."""
    mod, tree = refuse_loop
    tree["moves_after_warm"] = True
    conftest.pytest_collection_finish(_FakeSession())
    conftest._release_e2e_slot()
    holds = _holds(mod)
    assert [h[:3] for h in holds] == [("hold", "e2e_other", ["linux"])] * 2, mod.events
    assert all(isinstance(h[3], float) and h[3] >= 0 for h in holds), holds
    first_hold = mod.events.index(holds[0])
    assert mod.events[first_hold + 1] == ("release", "e2e_other"), "logged, then released"
    assert mod.events[-2:] == [holds[1], ("release", "e2e_other")], mod.events
    assert conftest._E2E_SLOT is None


def test_the_session_end_release_logs_the_hold_and_the_long_mutex_after_the_lane(
    refuse_loop, monkeypatch
):
    """`pytest_sessionfinish` releases through the same path as the refuse loop:
    the lane (logged) first, then the long-lane mutex (not a lane — the writer
    records nothing for it)."""
    mod, tree = refuse_loop
    tree["cold"] = False
    monkeypatch.setattr(_FakeSlotModule, "e2e_lane", staticmethod(lambda heavy: "long"))
    # A long run is admitted only with a warrant (build-slot-pool.md § the
    # `e2e` pool bullet → *Who may hold the long lane*); without one the
    # refusal's pytest.exit ends the real session running this test.
    monkeypatch.setattr(conftest, "_long_run_warrant", lambda session: "release-order test")
    conftest.pytest_collection_finish(_FakeSession())
    conftest._release_e2e_slot()
    tail = [e for e in mod.events if isinstance(e, tuple) and e[0] in ("hold", "release")]
    assert [e[:2] for e in tail] == [
        ("hold", "e2e_other"), ("release", "e2e_other"), ("release", "e2e_long"),
    ], mod.events
    import inspect

    # The call, whatever it passes: since the grant's end record it hands the
    # run's exit status on (`_release_e2e_slot(getattr(session, "exitstatus", ...))`).
    assert "_release_e2e_slot(" in inspect.getsource(conftest.pytest_sessionfinish)


def test_a_slot_module_without_the_hold_writer_releases_as_before(refuse_loop, monkeypatch):
    """The machine's policy copy may predate the usage record: conftest reaches
    the writer through getattr, so an older copy just writes nothing."""
    mod, tree = refuse_loop
    tree["moves_after_warm"] = True
    monkeypatch.delattr(_FakeSlotModule, "log_lane_hold")
    conftest.pytest_collection_finish(_FakeSession())
    conftest._release_e2e_slot()
    assert not _holds(mod)
    assert mod.events.count(("release", "e2e_other")) == 2, mod.events


def test_the_ffi_cdylib_is_warmed_with_the_loaders_profile():
    """The compile half of the fauna-ffi cdylib runs in the warm pass through
    the same recipe and profile the loader uses at import, so the loader's own
    in-hold call is a stamp read (the cdylib queued inside every hold before —
    64 min on 2026-09-29). Two literals, one meaning: pinned together here."""
    import inspect

    loader = (_REPO / "tests" / "e2e-unified" / "fauna_ffi.py").read_text(encoding="utf-8")
    m = re.search(r'^_E2E_FFI_PROFILE = "(\w+)"$', loader, re.MULTILINE)
    assert m, "fauna_ffi._E2E_FFI_PROFILE must stay a literal this pin can read"
    src = inspect.getsource(conftest._ensure_fauna_ffi_built)
    assert f'["just", "e2e-ffi", "{m.group(1)}"]' in src
    jobs = inspect.getsource(conftest._prebuild_jobs)
    build_at = jobs.find('"_ensure_fauna_ffi_built"')
    load_at = jobs.find('"_ensure_fauna_ffi_loaded"')
    assert -1 < build_at < load_at, "build before load, both under `load_ffi`"
    assert "_ensure_fauna_ffi_built" not in conftest._NOT_PREWARMED_BUILDERS
    assert "_ensure_fauna_ffi_loaded" in conftest._NOT_PREWARMED_BUILDERS


@pytest.mark.parametrize(
    ("machine", "expected"),
    [
        ("ARM64", ["just", "windows-ffi-test", "dev"]),
        ("AMD64", ["just", "windows-ffi-test", "dev", "x86_64-pc-windows-msvc"]),
    ],
)
def test_the_windows_ffi_cdylib_is_warmed_with_the_loaders_recipe(monkeypatch, machine, expected):
    """On windows the loader builds `just windows-ffi-test dev` at import (the
    host's RID), so the warm pass must compile that flavor too. Skipping it left
    the loader's in-hold build to the refuse mode, which refuses it: a run whose
    modules did not import `fauna_ffi` at collection lost the cdylib for the
    whole session (measured 2026-10-09: `fauna-ffi unavailable … exit 75` on a
    tier_3 run that seeds nothing through it). The profile and the x64 triple are
    the loader's literals, pinned to it here."""
    loader = (_REPO / "tests" / "e2e-unified" / "fauna_ffi.py").read_text(encoding="utf-8")
    profile = re.search(r'^_WINDOWS_FFI_PROFILE = "(\w+)"$', loader, re.MULTILINE)
    assert profile, "fauna_ffi._WINDOWS_FFI_PROFILE must stay a literal this pin can read"
    assert profile.group(1) == expected[2]
    assert '"x86_64-pc-windows-msvc"' in loader

    ran: list = []
    monkeypatch.setattr(conftest, "_IS_WINDOWS", True)
    monkeypatch.setattr(conftest.platform, "machine", lambda: machine)
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    monkeypatch.setattr(
        conftest.subprocess, "run", lambda cmd, **kw: ran.append((cmd, kw)),
    )
    conftest._ensure_fauna_ffi_built()
    assert [cmd for cmd, _ in ran] == [expected]
    assert ran[0][1].get("check") is True


# ── recovery_fixture: reached through a helper call, hoisted by module import ─


def test_recovery_fixture_import_scan_matches_only_the_building_helper(tmp_path):
    """`helpers.succession` runs `recovery_fixture` from inside a test body, so
    the helper-binary registry keys its prebuild on the module's import
    (measured 2026-09-26: a cold build inside the test's 900 s budget killed the
    whole pytest process on Windows). Its siblings `succession_ceremony` /
    `succession_retry` never run it and must not trigger a cold cargo build."""
    from helpers import helper_bins

    cases = {
        "from helpers.succession import register_recovery_kit\n": True,
        "def test_x():\n    from helpers.succession import succeed_identity\n": True,
        "from helpers import succession\n": True,
        "import helpers.succession\n": True,
        "from helpers.succession_ceremony import settled_actor_id\n": False,
        "from helpers.succession_retry import x\n": False,
        "from helpers import succession_ceremony\n": False,
        "# from helpers.succession import nothing\n": False,
    }
    entry = helper_bins.REGISTRY["recovery_fixture"]
    for i, (text, expected) in enumerate(cases.items()):
        path = tmp_path / f"test_case_{i}.py"
        path.write_text(text)
        item = type("Item", (), {"path": path})()
        reached = entry in helper_bins.reached_entries([item], set())
        assert reached is expected, text


def test_prebuild_binaries_hoists_the_recovery_fixture_build(monkeypatch, tmp_path):
    """An item whose module imports `helpers.succession` makes the collection-
    time prebuild run the registry's `recovery_fixture` job; an unrelated item
    does not (laziness preserved)."""
    from helpers import nest_mode as nest_mode_mod

    monkeypatch.setattr(nest_mode_mod, "builds_local_nest", lambda mode: True)
    ran = []
    monkeypatch.setattr(conftest, "_ensure_helper_bin_built", ran.append)
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: set())

    def item_for(text, name):
        path = tmp_path / name
        path.write_text(text)
        return type("Item", (), {"path": path})()

    user = item_for("from helpers.succession import register_recovery_kit\n", "a.py")
    other = item_for("import os\n", "b.py")
    conftest._prebuild_binaries(type("S", (), {"items": [other]})())
    assert ran == []
    conftest._prebuild_binaries(type("S", (), {"items": [other, user]})())
    assert ran == ["recovery_fixture"]


# ── The iOS app build: its `xcodebuild` is a prewarmed job of its
# own, freshness-gated OUTSIDE a `build` slot and compiled under one — never a
# plain slot-wrap, which the in-hold refuse mode would refuse on every run.


@pytest.fixture
def ios_build_tree(tmp_path, monkeypatch):
    """A fake checkout for `_build_ios_app`: an apple package with a staged
    3-slice xcframework, the slot script present (so `_build_slot_cmd` prefixes
    it), a per-test derived-data dir, and `subprocess.run` replaced by a fake
    `xcodebuild` that writes the executable and records each command."""
    import types

    repo = tmp_path / "repo"
    pkg = repo / "apps" / "fauna-apple"
    for rel in ("FaunaKit/View.swift", "FaunaFFISwift/Sources/FaunaFFI.swift",
                "FaunaFFI.xcframework/ios-arm64-simulator/libfauna_ffi.a",
                "FaunaFFI.xcframework/ios-arm64/libfauna_ffi.a",
                "FaunaFFI.xcframework/macos-arm64/libfauna_ffi.a"):
        (pkg / rel).parent.mkdir(parents=True, exist_ok=True)
        (pkg / rel).write_text(rel)
    old = time.time() - 600
    for f in pkg.rglob("*"):
        if f.is_file():
            os.utime(f, (old, old))
    (repo / "scripts").mkdir()
    (repo / "scripts" / "build-slot.py").write_text("")
    derived = tmp_path / "derived"
    calls = []

    def fake_run(cmd, *a, **k):
        calls.append(list(cmd))
        exe = derived / "Build" / "Products" / "Debug-iphonesimulator" / "FaunaiOS"
        exe.parent.mkdir(parents=True, exist_ok=True)
        exe.write_text("bin")
        return types.SimpleNamespace(returncode=0, stdout="", stderr=None)

    monkeypatch.setattr(conftest, "_repo_root", repo)
    monkeypatch.setattr(conftest, "_ios_derived_data_dir", lambda: derived)
    monkeypatch.setattr(conftest, "_ios_package_workspace", lambda package_dir: tmp_path / "ws")
    monkeypatch.setattr(conftest.subprocess, "run", fake_run)
    return types.SimpleNamespace(repo=repo, pkg=pkg, derived=derived, calls=calls)


def test_the_ios_app_compiles_under_a_build_slot_and_a_warm_tree_runs_no_xcodebuild(ios_build_tree):
    t = ios_build_tree
    app = conftest._build_ios_app()
    assert app == str(t.derived / "FaunaiOS.app")
    assert len(t.calls) == 1, t.calls
    slot = common_nest._build_slot_cmd(t.repo, common_nest.DISK_GB_XCODE)
    assert slot, "the fake checkout carries the slot script"
    assert t.calls[0][: len(slot)] == slot and t.calls[0][len(slot)] == "xcodebuild", (
        "the compile path takes the `build` slot — the bare xcodebuild was mac's "
        "zero-slot session-tick class"
    )
    assert (t.derived / ".fauna-ios-app.stamp").exists()
    assert conftest._build_ios_app() == app
    assert len(t.calls) == 1, "a warm tree is a stamp read: no xcodebuild, no slot"


def test_the_ios_app_rebuilds_when_a_package_source_moves(ios_build_tree):
    t = ios_build_tree
    conftest._build_ios_app()
    (t.pkg / "FaunaKit" / "View.swift").write_text("edited")
    conftest._build_ios_app()
    assert len(t.calls) == 2, t.calls


def test_the_ios_app_rebuilds_when_another_ffi_generation_is_restored_with_older_mtimes(ios_build_tree):
    """The full flavor's restores keep mtimes (`cp -p`), so a DIFFERENT
    generation can arrive with mtimes older than the app's stamp — the gate
    reads the generation's fingerprint, not a newer-than test."""
    t = ios_build_tree
    conftest._build_ios_app()
    lib = t.pkg / "FaunaFFI.xcframework" / "ios-arm64-simulator" / "libfauna_ffi.a"
    lib.write_text("another generation, another size")
    older = time.time() - 3600
    os.utime(lib, (older, older))
    conftest._build_ios_app()
    assert len(t.calls) == 2, t.calls


def test_outputs_other_recipes_write_into_the_package_leave_the_ios_app_warm(ios_build_tree):
    """`mac-debug` rewrites its stamp and `.build/` on every macOS build, the
    flavor recipes rewrite `.ffi-flavor`: none is an input of the iOS app."""
    t = ios_build_tree
    conftest._build_ios_app()
    for rel in (".mac-debug.stamp", ".ffi-flavor", ".ffi-cache.stamp", ".build/debug/x.o",
                "generated/FaunaFFI.swift"):
        (t.pkg / rel).parent.mkdir(parents=True, exist_ok=True)
        (t.pkg / rel).write_text("new output")
    conftest._build_ios_app()
    assert len(t.calls) == 1, t.calls


def test_a_failed_ios_compile_writes_no_stamp(ios_build_tree, monkeypatch):
    import types

    t = ios_build_tree
    monkeypatch.setattr(conftest.subprocess, "run",
                        lambda cmd, *a, **k: types.SimpleNamespace(returncode=75, stdout="refused"))
    with pytest.raises(RuntimeError, match="building the iOS app failed"):
        conftest._build_ios_app()
    assert not (t.derived / ".fauna-ios-app.stamp").exists()


def test_the_ios_app_is_a_prewarmed_job_after_the_restore_and_before_the_simulator(monkeypatch):
    """Order in both passes: the iOS app's FFI (`app[ios]`), the macOS app, the
    full xcframework restore, the iOS `xcodebuild`, then the simulator (its
    boot held, in the warm pass since 2026-10-27), then — in the hold only —
    the look at the devices the warm pass booted; so the restore precedes the
    iOS build in the warm pass too."""
    ran = []
    for name in ("_ensure_app_built",):
        monkeypatch.setattr(conftest, name, lambda app: ran.append(f"app[{app}]"))
    for name in ("_restore_full_apple_ffi", "_ensure_ios_app_built", "_get_ios_setup",
                 "_recheck_ios_simulators"):
        monkeypatch.setattr(conftest, name, (lambda n: lambda: ran.append(n))(name))
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: {"ios", "macos"})
    session = type("S", (), {"items": []})()
    conftest._prebuild_binaries(session)
    assert ran == ["app[ios]", "app[macos]", "_restore_full_apple_ffi",
                   "_ensure_ios_app_built", "_get_ios_setup", "_recheck_ios_simulators"], ran
    ran.clear()
    conftest._prebuild_binaries(session, prewarm=True)
    assert ran == ["app[ios]", "app[macos]", "_restore_full_apple_ffi",
                   "_ensure_ios_app_built", "_get_ios_setup"], ran


# ── The file-provider apps' `xcodebuild`s: prerequisites of the tests that drive the built
# extension, so each is a prewarmed job keyed on its fixture's name — the
# deposit-adoption suites adopt the same fixtures by import — sharing the iOS
# app's freshness gate. Only the compile moved: installing, signing and
# registering stay in the fixtures, machine-wide steps no warm pass may run.

_FP_BUILDS = [
    ("_build_ios_fp_app", "Debug-iphonesimulator/Fauna-iOS.app", ".fauna-ios-fp-app.stamp", "Fauna-iOS"),
    ("_build_fp_host_app", "Debug/Fauna.app", ".fauna-fp-host-app.stamp", "Fauna"),
]


@pytest.fixture
def fp_build_tree(ios_build_tree, monkeypatch):
    """`ios_build_tree` with a fake `xcodebuild` that writes the product the
    command's scheme names into the checkout's `.xcode-build`."""
    import types

    t = ios_build_tree
    derived = t.pkg / ".xcode-build"

    def fake_run(cmd, *a, **k):
        t.calls.append(list(cmd))
        scheme = cmd[cmd.index("-scheme") + 1]
        rel = next(rel for _, rel, _, s in _FP_BUILDS if s == scheme)
        (derived / "Build" / "Products" / rel).mkdir(parents=True, exist_ok=True)
        return types.SimpleNamespace(returncode=0, stdout="", stderr=None)

    monkeypatch.setattr(conftest.subprocess, "run", fake_run)
    t.xcode_build = derived
    return t


@pytest.mark.parametrize("builder,rel,stamp,scheme", _FP_BUILDS)
def test_a_file_provider_app_compiles_under_a_build_slot_and_a_warm_tree_runs_no_xcodebuild(
    fp_build_tree, builder, rel, stamp, scheme
):
    t = fp_build_tree
    built = getattr(conftest, builder)()
    assert built == str(t.xcode_build / "Build" / "Products" / rel)
    assert len(t.calls) == 1, t.calls
    slot = common_nest._build_slot_cmd(t.repo, common_nest.DISK_GB_XCODE)
    assert slot and t.calls[0][: len(slot)] == slot and t.calls[0][len(slot)] == "xcodebuild"
    assert t.calls[0][t.calls[0].index("-scheme") + 1] == scheme
    assert t.calls[0][t.calls[0].index("-derivedDataPath") + 1] == str(t.xcode_build)
    assert (t.xcode_build / stamp).exists()
    assert getattr(conftest, builder)() == built
    assert len(t.calls) == 1, "a warm tree is a stamp read: no xcodebuild, no slot"


def test_each_file_provider_app_keeps_a_stamp_of_its_own(fp_build_tree):
    """Both build into one derived-data tree; one being warm says nothing of the other."""
    t = fp_build_tree
    conftest._build_ios_fp_app()
    conftest._build_fp_host_app()
    assert len(t.calls) == 2, t.calls


@pytest.mark.parametrize("builder,rel,stamp,scheme", _FP_BUILDS)
def test_a_file_provider_app_rebuilds_when_the_ffi_generation_moves(
    fp_build_tree, builder, rel, stamp, scheme
):
    t = fp_build_tree
    getattr(conftest, builder)()
    lib = t.pkg / "FaunaFFI.xcframework" / "ios-arm64-simulator" / "libfauna_ffi.a"
    lib.write_text("another generation, another size")
    older = time.time() - 3600
    os.utime(lib, (older, older))
    getattr(conftest, builder)()
    assert len(t.calls) == 2, t.calls


@pytest.mark.parametrize("builder,rel,stamp,scheme", _FP_BUILDS)
def test_a_failed_file_provider_compile_writes_no_stamp_and_names_its_build(
    fp_build_tree, monkeypatch, builder, rel, stamp, scheme
):
    import types

    t = fp_build_tree
    monkeypatch.setattr(conftest.subprocess, "run",
                        lambda cmd, *a, **k: types.SimpleNamespace(returncode=75, stdout="refused"))
    with pytest.raises(RuntimeError, match=scheme):
        getattr(conftest, builder)()
    assert not (t.xcode_build / stamp).exists()


def test_the_file_provider_ensurers_are_no_ops_without_their_ffi(fp_build_tree, monkeypatch):
    """A selection on a tree that cannot link must not cost a failed compile: the
    fixture's own guidance skip stays the one reader of the absence."""
    t = fp_build_tree
    monkeypatch.setattr(conftest, "detect_platform", lambda: "macos")
    monkeypatch.setattr(conftest.shutil, "which", lambda name: "/usr/bin/xcodebuild")
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    sim_slice = t.pkg / "FaunaFFI.xcframework" / "ios-arm64-simulator"
    (sim_slice / "libfauna_ffi.a").unlink()
    sim_slice.rmdir()
    assert conftest._ensure_ios_fp_app_built() is None
    conftest.shutil.rmtree(t.pkg / "FaunaFFI.xcframework")
    assert conftest._ensure_fp_host_app_built() is None
    assert t.calls == []

    monkeypatch.setattr(conftest, "detect_platform", lambda: "linux")
    assert conftest._ensure_ios_fp_app_built() is None
    assert conftest._ensure_fp_host_app_built() is None
    assert t.calls == []


def test_the_file_provider_apps_are_keyed_on_their_fixtures_after_the_restore(monkeypatch):
    """Keyed on the fixture, never the module: a deposit-adoption suite adopts it
    by import. After the restore, so a macOS app build's FFI flavor swap cannot
    stale their stamps between the warm pass and the hold."""
    monkeypatch.setattr(conftest, "_real_fixture_closure",
                        lambda item: {"fauna_ios_fp_app", "fauna_fp_host_app"})
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: {"ios", "macos"})
    monkeypatch.setattr(conftest, "_helper_bin_jobs", lambda s, r: [])
    monkeypatch.setattr(conftest, "_macos_spawns_a_real_sync_agent", lambda items: False)
    monkeypatch.setattr(conftest, "_ios_seats_needed", lambda s: 1)
    session = type("S", (), {"items": [object()]})()
    builders = [builder for _, builder, _ in conftest._prebuild_jobs(session)]
    restore = builders.index("_restore_full_apple_ffi")
    assert restore < builders.index("_ensure_ios_fp_app_built") < builders.index("_get_ios_setup")
    assert restore < builders.index("_ensure_fp_host_app_built") < builders.index("_get_ios_setup")

    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    builders = [builder for _, builder, _ in conftest._prebuild_jobs(session)]
    assert "_ensure_ios_fp_app_built" not in builders
    assert "_ensure_fp_host_app_built" not in builders


def test_the_ios_simulator_boot_is_reported_as_setup_not_prebuild(monkeypatch):
    """The simulator's create-and-finish-booting waits for no `build`, so the
    hold charges it to `setup_s` (the usage line's `inhold_setup_s`) and only the
    rest of the pass to `prebuild_s`."""
    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_ensure_ios_app_built", lambda: None)
    monkeypatch.setattr(conftest, "_get_ios_setup", lambda: time.sleep(0.3))
    monkeypatch.setattr(conftest, "_E2E_SIMULATORS", [])
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: {"ios"})
    hold = conftest._new_hold({"ios"})
    conftest._timed_prebuild(hold, lambda: conftest._prebuild_binaries(type("S", (), {"items": []})()))
    assert hold["setup_s"] >= 0.3
    assert 0 <= hold["prebuild_s"] < 0.3, hold


def test_the_host_flavor_clears_the_full_fast_path_stamp_before_it_wipes_the_framework():
    """`_apple-ffi-host-flavor` replaces a full xcframework with the host-only
    one at its TOP, so it must drop the full flavor's `.ffi-cache.stamp` there,
    not only at its end: a refused cargo step in between (exit 75 in a refused
    e2e hold) left the stamp vouching for a host-only framework, and the next
    `apple-ffi-test-sim` fast-pathed "artifacts already current" with no
    ios-arm64-simulator slice (the 2026-10-24 baseline run's first hold)."""
    body = _recipe_body(_JUSTFILE.read_text(), "_apple-ffi-host-flavor")
    wipe = body.index("rm -rf apps/fauna-apple/FaunaFFI.xcframework")
    clear = body.find("rm -f apps/fauna-apple/.ffi-cache.stamp")
    assert 0 <= clear < wipe, "the stamp must go before the first framework wipe"


@pytest.fixture
def ios_setup_env(monkeypatch):
    """`_get_ios_setup` on a fake mac: an empty memo, no real simctl, the
    simulator probe and the app build as switches a test flips."""
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    monkeypatch.setattr(conftest, "detect_platform", lambda: "macos")
    monkeypatch.setattr(conftest.shutil, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(conftest.subprocess, "run", lambda *a, **k: None)
    import drivers.ios

    monkeypatch.setattr(drivers.ios, "boot_and_wait_until_usable", lambda udid: None)
    # The held boot's grant is the fake slot module's business, tested below.
    monkeypatch.setattr(conftest, "_boot_simulator_held", lambda udid, in_test=False: None)
    env = {"udid": "SIM-1", "build_fails": True}
    monkeypatch.setattr(conftest, "_create_ephemeral_simulator", lambda **_kw: env["udid"])
    monkeypatch.setattr(conftest, "_find_simulator_udid", lambda: None)

    def build():
        if env["build_fails"]:
            raise RuntimeError("FaunaFFI.xcframework has no 'ios-arm64-simulator' slice")
        return "/apps/Fauna.app"

    monkeypatch.setattr(conftest, "_build_ios_app", build)
    return env


def test_an_ios_setup_that_failed_in_a_refused_hold_is_retried_in_the_next(ios_setup_env):
    """A release-and-warm cycle forgets what a refused hold memoized as failed,
    the simulator setup included: the next hold boots it afresh. It used to
    keep a one-shot flag of its own that the cycle never cleared, so the
    retried hold read the stale ``None`` and the whole ios leg skipped as
    unbuilt (the 2026-10-24 baseline run: 1497 ios tests skipped)."""
    with pytest.raises(RuntimeError, match="ios-arm64-simulator"):
        conftest._get_ios_setup()
    conftest._forget_prebuilds_for_retry()
    ios_setup_env["build_fails"] = False
    assert conftest._get_ios_setup() == {"udid": "SIM-1", "app_path": "/apps/Fauna.app"}


def test_an_ios_setup_failure_replays_as_an_error_never_a_none(ios_setup_env):
    with pytest.raises(RuntimeError):
        conftest._get_ios_setup()
    with pytest.raises(RuntimeError, match="already failed"):
        conftest._get_ios_setup()


def test_no_simulator_on_a_mac_with_xcode_raises_instead_of_skipping(ios_setup_env):
    ios_setup_env["udid"] = None
    ios_setup_env["build_fails"] = False
    with pytest.raises(RuntimeError, match="simulator"):
        conftest._get_ios_setup()


def test_ios_setup_is_absent_only_off_a_mac_with_xcode(ios_setup_env, monkeypatch):
    monkeypatch.setattr(conftest, "detect_platform", lambda: "linux")
    assert conftest._get_ios_setup() is None


def test_the_app_legs_report_each_apps_session_setup(monkeypatch):
    """The baseline payload's ``app_legs``: an app is ``ok`` only when every one
    of its setup entries built; ios needs both its app and its simulator."""
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {
        "app:macos": ("ok", None),
        "app:ios": ("ok", None),
        "ios-setup": ("failed", RuntimeError("no slice")),
        "nest-binary": ("ok", "/bin/nest"),
    })
    legs = conftest._baseline_app_legs()
    assert legs["macos"] == "ok"
    assert legs["ios"].startswith("failed") and "no slice" in legs["ios"]
    assert set(legs) == {"macos", "ios"}


def test_a_hold_logs_setup_apart_and_an_older_writer_gets_it_inside_prebuild(monkeypatch):
    mod = _FakeSlotModule()
    for name in ("FAUNA_SLOT_HELD_E2E", "FAUNA_SLOT_HELD_E2E_OTHER"):
        monkeypatch.delenv(name, raising=False)
    hold = {"apps": ["ios"], "prebuild_s": 6.0, "setup_s": 48.0}
    fd = mod.acquire_slot("e2e_other")
    conftest._release_lanes(mod, [(fd, "e2e_other")], hold)
    assert _holds(mod) == [("hold", "e2e_other", ["ios"], 6.0, 48.0)]

    class _Older(_FakeSlotModule):
        def log_lane_hold(self, fd, apps, inhold_prebuild_s=None):
            pool = self._pool_of.get(fd)
            self.events.append(("hold", pool, sorted(apps), inhold_prebuild_s))

    old = _Older()
    fd = old.acquire_slot("e2e_other")
    conftest._release_lanes(old, [(fd, "e2e_other")], hold)
    assert _holds(old) == [("hold", "e2e_other", ["ios"], 54.0)], "a predating copy: setup counted as before"


# ── The marker scan and the iOS device archive: a non-parametrized suite's app
# marker names the app it launches, EXCEPT a tier_4 `ios` item — the device
# archive, which `xcodebuild archive`s the shipping target itself and launches
# no simulator app.


class _MarkedItem:
    def __init__(self, *markers):
        self.nodeid = "tests/artifact/test_x.py::test_y"
        self.path = Path(self.nodeid.split("::")[0])
        self.fixturenames = []
        self._markers = markers

    def iter_markers(self):
        return [type("_M", (), {"name": m})() for m in self._markers]

    def get_closest_marker(self, name):
        return next((m for m in self.iter_markers() if m.name == name), None)


def _marker_app_set(monkeypatch, *items):
    monkeypatch.setattr(conftest, "get_available_apps", lambda: ["ios", "macos", "tui"])
    monkeypatch.setattr(conftest, "sweep_apps", lambda: ["ios", "macos", "tui"])
    session = type("_S", (), {"items": list(items)})()
    return conftest._prebuild_app_set(session, set())


def test_the_ios_device_archive_never_prebuilds_the_simulator_app(monkeypatch):
    """Building the simulator app stages the TEST FFI flavor, which the archive's
    `ios_device_ffi` guard refuses — so a prebuild here fails the very run it
    serves (`just e2e-macos-artifact-test` relies on it never happening)."""
    assert _marker_app_set(monkeypatch, _MarkedItem("tier_4", "ios")) == set()


def test_a_tier_4_macos_suite_and_a_non_artifact_ios_suite_keep_their_marker_build(monkeypatch):
    # The macOS artifact suite's marker build IS its artifact under
    # `--macos-artifact` (`mac-app debug`), and an ordinary ios-marked suite may
    # reach the simulator app through `getfixturevalue`.
    assert _marker_app_set(monkeypatch, _MarkedItem("tier_4", "macos")) == {"macos"}
    assert _marker_app_set(monkeypatch, _MarkedItem("tier_3", "ios")) == {"ios"}


# ── the simulator boot held before it starts (mac-core-hold.md § Per-core
#    tokens, the macOS bullet, *The e2e lane's simulator boot*: the user's
#    answer (a) of 2026-10-27; build-slot.py § Pools → sim_boot) ──────────────


@pytest.fixture
def held_boot_env(monkeypatch):
    """`_boot_simulator_held` against the fake slot module: the boot itself a
    recorded no-op, the run's device list empty."""
    import drivers.ios

    mod = _FakeSlotModule()
    monkeypatch.setattr(conftest, "_build_slot_module", lambda: mod)
    monkeypatch.setattr(conftest, "_E2E_SIMULATORS", [])
    monkeypatch.setattr(drivers.ios, "boot_and_wait_until_usable",
                        lambda udid: mod.events.append(("boot", udid)))
    monkeypatch.setattr(conftest, "_wait_until_simulator_quiet",
                        lambda udid: (mod.events.append(("quiet", udid)) or (0.5, 10.0, False)))
    monkeypatch.delenv("FAUNA_SLOT_HELD_SIM_BOOT", raising=False)
    return mod


def test_a_boot_is_held_under_a_sim_boot_grant_that_names_the_device(held_boot_env):
    """The grant is taken BEFORE `simctl boot`, its note names the run's
    devices (the fleet agent charges the booting tree to the run before any
    lane note exists), the boot runs, the device is waited quiet, and only
    then the grant goes — with no bound on the wait outside a test."""
    mod = held_boot_env
    conftest._E2E_SIMULATORS.append("SIM-1")
    conftest._boot_simulator_held("SIM-1")
    assert mod.events == [
        ("acquire", "sim_boot", None),
        ("note", "sim_boot", {"sims": ["SIM-1"]}),
        ("boot", "SIM-1"),
        ("quiet", "SIM-1"),
        ("release", "sim_boot"),
    ], mod.events


def test_a_boot_inside_a_test_waits_boundedly_then_boots_unheld(held_boot_env, capsys):
    """A seat first asked for inside a test (`in_test`) waits for the grant
    at most `_SIM_BOOT_IN_TEST_WAIT_S`; past it the device boots UNHELD with a
    loud line, so the run's own lane load can never make the test's work
    impossible. With room it is the ordinary held boot, under that bound."""
    mod = held_boot_env
    mod.sim_boot_room = False
    conftest._boot_simulator_held("SIM-2", in_test=True)
    assert mod.events == [("acquire", "sim_boot", conftest._SIM_BOOT_IN_TEST_WAIT_S),
                          ("boot", "SIM-2")], mod.events
    assert "booting UNHELD" in capsys.readouterr().err
    mod.events.clear()
    mod.sim_boot_room = True
    conftest._boot_simulator_held("SIM-2", in_test=True)
    assert [e[0] for e in mod.events] == ["acquire", "note", "boot", "quiet", "release"], mod.events
    assert mod.events[0][2] == conftest._SIM_BOOT_IN_TEST_WAIT_S


def test_a_slot_copy_without_the_pool_or_a_machine_listing_none_boots_as_before(
    held_boot_env, monkeypatch
):
    mod = held_boot_env
    mod.sim_boot_enabled = lambda platform=None: False
    conftest._boot_simulator_held("SIM-3")
    assert mod.events == [("boot", "SIM-3")], mod.events
    mod.events.clear()
    del mod.sim_boot_enabled
    monkeypatch.delattr(_FakeSlotModule, "SIM_BOOT_POOL")
    conftest._boot_simulator_held("SIM-3")
    assert mod.events == [("boot", "SIM-3")], mod.events
    mod.events.clear()
    monkeypatch.setattr(conftest, "_build_slot_module", lambda: None)
    conftest._boot_simulator_held("SIM-3")
    assert mod.events == [("boot", "SIM-3")], mod.events


def test_the_first_seat_boots_held_in_the_warm_pass_and_its_memo_survives_the_forget(
    held_boot_env, monkeypatch
):
    """`_get_ios_setup` creates the device, builds the app (a memo replay in
    practice) and boots HELD; the warm pass's forget keeps its memo
    (`_WARM_KEPT_MEMO_KEYS`), so the hold's replay boots nothing and creates
    no second device, while every build memo is forgotten as before."""
    mod = held_boot_env
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", {})
    monkeypatch.setattr(conftest, "detect_platform", lambda: "macos")
    monkeypatch.setattr(conftest.shutil, "which", lambda name: f"/usr/bin/{name}")
    created = []

    def create(**_kw):
        created.append("SIM-1")
        conftest.register_simulator("SIM-1")
        return "SIM-1"

    monkeypatch.setattr(conftest, "_create_ephemeral_simulator", create)
    monkeypatch.setattr(conftest, "_ensure_ios_app_built", lambda: "/apps/Fauna.app")
    assert conftest._get_ios_setup() == {"udid": "SIM-1", "app_path": "/apps/Fauna.app"}
    assert mod.events == [
        ("acquire", "sim_boot", None), ("note", "sim_boot", {"sims": ["SIM-1"]}),
        ("boot", "SIM-1"), ("quiet", "SIM-1"), ("release", "sim_boot"),
    ], mod.events
    conftest._BINARY_BUILD_MEMO["nest-binary"] = ("ok", "/nest")
    monkeypatch.setattr(conftest, "_prebuild_web_spa", lambda session: None)
    monkeypatch.setattr(conftest, "_prebuild_binaries", lambda session, prewarm=False: None)
    assert conftest._prewarm_prebuilds(_FakeSession()) == []
    assert "ios-setup" in conftest._BINARY_BUILD_MEMO
    assert "nest-binary" not in conftest._BINARY_BUILD_MEMO
    mod.events.clear()
    assert conftest._get_ios_setup() == {"udid": "SIM-1", "app_path": "/apps/Fauna.app"}
    assert mod.events == [] and created == ["SIM-1"]


def test_later_seats_boot_held_too_unbounded_in_the_warm_pass_bounded_in_a_test(
    held_boot_env, monkeypatch
):
    mod = held_boot_env
    monkeypatch.setattr(conftest, "_ios_second_seat_udid_cache", None)
    monkeypatch.setattr(conftest, "_ios_second_seat_setup_done", False)
    monkeypatch.setattr(conftest, "_ios_extra_seat_udids", {})
    udids = iter(["SEAT-1", "SEAT-2"])
    monkeypatch.setattr(conftest, "_create_ephemeral_simulator", lambda **_kw: next(udids))
    conftest._prewarm_ios_seat(1)
    assert mod.events[0] == ("acquire", "sim_boot", None) and ("boot", "SEAT-1") in mod.events
    mod.events.clear()
    assert conftest._get_ios_seat_udid(1) == "SEAT-1" and mod.events == []  # the cache
    assert conftest._get_ios_seat_udid(2) == "SEAT-2"
    assert mod.events[0] == ("acquire", "sim_boot", conftest._SIM_BOOT_IN_TEST_WAIT_S)
    assert ("boot", "SEAT-2") in mod.events


def test_the_in_hold_look_reboots_a_shut_down_device_under_its_grant(held_boot_env, monkeypatch):
    """Devices the warm pass booted are looked at once in the hold: one no
    longer Booted is booted again held; a listing that cannot be read, or a
    run with no device, boots nothing (and runs no `simctl`)."""
    mod = held_boot_env
    calls = []
    monkeypatch.setattr(conftest, "_booted_simulators", lambda: calls.append(1) or {"SIM-1"})
    conftest._recheck_ios_simulators()
    assert calls == [] and mod.events == []
    conftest._E2E_SIMULATORS.extend(["SIM-1", "SIM-2"])
    conftest._recheck_ios_simulators()
    assert calls == [1] and [e for e in mod.events if e[0] == "boot"] == [("boot", "SIM-2")]
    mod.events.clear()
    monkeypatch.setattr(conftest, "_booted_simulators", lambda: None)
    conftest._recheck_ios_simulators()
    assert mod.events == []


def _ios_item(nodeid, closure=(), path=None):
    item = _FakeItem(nodeid)
    item.fixturenames = list(closure)
    item.fspath = path
    return item


def test_the_warm_pass_boots_every_seat_the_items_foreseeably_need(tmp_path, monkeypatch):
    """1 seat always; 2 when an iOS item's closure names a second-seat fixture;
    the room journeys' 3 when an iOS item's module launches room seats — and
    a linux item's fixtures or module count for nothing."""
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set(item.fixturenames))
    monkeypatch.setattr(conftest, "_ROOM_SEAT_MODULES", {})
    plain = tmp_path / "test_plain.py"
    plain.write_text("def test_x(app): pass\n")
    rooms = tmp_path / "test_rooms.py"
    rooms.write_text("from helpers.room_seats import launch_seat\n")

    def needed(*items):
        return conftest._ios_seats_needed(type("S", (), {"items": list(items)})())

    assert needed() == 1
    assert needed(_ios_item("t.py::test_a[ios]", ("app",), plain)) == 1
    assert needed(_ios_item("t.py::test_a[ios]", ("app", "second_real_faunamls_app"), plain)) == 2
    assert needed(_ios_item("t.py::test_a[linux]", ("second_real_faunamls_app",), plain)) == 1
    assert needed(_ios_item("t.py::test_a[ios-native]", ("app",), rooms)) == 3
    assert needed(_ios_item("t.py::test_a[macos]", ("app",), rooms)) == 1
    assert needed(_ios_item("t.py::test_a[ios]", ("subs_author_app",), plain),
                  _ios_item("t.py::test_b[ios]", (), rooms)) == 3


def test_every_fixture_that_boots_a_second_ios_seat_is_prewarmed():
    """`_IOS_SECOND_SEAT_FIXTURES` is pinned against the source: every pytest
    fixture — conftest's and the test modules' — from which
    `_get_ios_second_seat_udid` is reachable through this file's own helpers
    is in the set, so a new second-seat fixture cannot silently boot its
    device inside the asking test."""
    import ast

    target = "_get_ios_second_seat_udid"
    conftest_path = _REPO / "tests" / "e2e-unified" / "conftest.py"
    tests_dir = _REPO / "tests" / "e2e-unified" / "tests"

    def module_graph(path):
        """({function: callees}, {fixture names}) over a module's top-level defs."""
        tree = ast.parse(path.read_text(encoding="utf-8"))
        calls, fixtures = {}, set()
        for node in tree.body:
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            calls[node.name] = {
                c.func.id for c in ast.walk(node)
                if isinstance(c, ast.Call) and isinstance(c.func, ast.Name)
            }
            for deco in node.decorator_list:
                text = ast.unparse(deco)
                if text.startswith("pytest.fixture") or text == "fixture":
                    fixtures.add(node.name)
        return calls, fixtures

    def reaching(calls, goal):
        found, frontier = set(), {goal}
        while frontier:
            goal_now = frontier.pop()
            for name, callees in calls.items():
                if goal_now in callees and name not in found:
                    found.add(name)
                    frontier.add(name)
        return found

    calls, fixtures = module_graph(conftest_path)
    assert target in calls
    conftest_helpers = reaching(calls, target)
    expected = {f for f in conftest_helpers if f in fixtures}
    for path in sorted(tests_dir.glob("test_*.py")):
        if target not in path.read_text(encoding="utf-8"):
            continue
        t_calls, t_fixtures = module_graph(path)
        reach = set()
        for goal in (target, *conftest_helpers):
            reach |= reaching(t_calls, goal)
        expected |= {f for f in reach if f in t_fixtures}
    assert expected, "the scan found no second-seat fixture at all — it is broken"
    assert expected <= conftest._IOS_SECOND_SEAT_FIXTURES, sorted(
        expected - conftest._IOS_SECOND_SEAT_FIXTURES
    )


_PS_LISTING = """\
    1     0   4:46.03 /sbin/launchd
  100     1   1:30.44 /usr/libexec/logd
  900     1   0:10.00 /Library/Developer/CoreSimulator/Volumes/iOS_26/…/launchd_sim --root /Users/u/Library/Developer/CoreSimulator/Devices/D26ABD7C-8F4A-4D37-BAA4-83013F1F2839/data/…
  901   900   0:20.50 /usr/libexec/diagnosticd
  902   901   0:05.25 /usr/libexec/trustd
  903     1   0:08.00 /Library/Developer/CoreSimulator/Volumes/iOS_26/…/launchd_sim --root /Users/u/Library/Developer/CoreSimulator/Devices/other-udid/data/…
  904   903   1:00.00 /usr/libexec/diagnosticd
  905     1   0:00.10 /usr/libexec/mediaanalysisd
"""


def test_ps_time_parses_minutes_hours_and_days():
    assert conftest._ps_time_s("0:00.12") == 0.12
    assert conftest._ps_time_s("7:46.52") == 7 * 60 + 46.52
    assert conftest._ps_time_s("1:02:03") == 3723.0
    assert conftest._ps_time_s("2-01:00:00") == 2 * 86400 + 3600.0
    assert conftest._ps_time_s("garbage") == 0.0


def test_the_tree_cpu_reading_sums_the_launchd_sim_root_and_its_descendants():
    """The device's tree is the `launchd_sim` whose command line names the
    device's directory plus every descendant — not another device's, not the
    host's `mediaanalysisd`; no such root reads None."""
    s = conftest._tree_cpu_s_from_ps
    assert s(_PS_LISTING, "d26abd7c-8f4a-4d37-baa4-83013f1f2839") == 10.0 + 20.5 + 5.25
    assert s(_PS_LISTING, "other-udid") == 8.0 + 60.0
    assert s(_PS_LISTING, "no-such-device") is None
    assert s("", "D26ABD7C-8F4A-4D37-BAA4-83013F1F2839") is None


def test_the_quiet_wait_releases_under_two_cores_or_at_the_cap():
    """Two readings a window apart give the tree's cores: a tree falling
    under _SIM_QUIET_CORES ends the wait; one never falling ends it at the
    cap; an unreadable tree ends it at once (nothing to wait on)."""
    sample = conftest._SIM_QUIET_SAMPLE_S

    def run(cpu_readings):
        readings = iter(cpu_readings)
        clock = {"t": 100.0}

        def sleep(s):
            clock["t"] += s

        return conftest._wait_until_simulator_quiet(
            "SIM", tree_cpu_s=lambda udid: next(readings), sleep=sleep, clock=lambda: clock["t"]
        )

    # 14 cores, then 12, then 1.2: quiet after three windows.
    cores, waited, capped = run([0.0, 14 * sample, 26 * sample, 27.2 * sample])
    assert (round(cores, 2), waited, capped) == (1.2, 3 * sample, False)
    # Never under: the cap ends it, the last reading reported.
    steady = [i * 13 * sample for i in range(100)]
    cores, waited, capped = run(steady)
    assert cores == 13.0 and capped and waited >= conftest._SIM_QUIET_MAX_S
    # Unreadable from the start, or mid-way: no wait, None.
    assert run([None]) == (None, 0.0, False)
    cores, waited, capped = run([0.0, None])
    assert cores is None and waited == sample and not capped


def _sim_ps(*rows):
    root = ("1 0 {t} /Library/Developer/CoreSimulator/Volumes/iOS/launchd_sim "
            "/Users/u/Library/Developer/CoreSimulator/Devices/SIM-U/data/var/run/launchd_bootstrap.plist")
    lines = [root.format(t=rows[0])]
    for pid, t in rows[1:]:
        lines.append(f"{pid} 1 {t} /usr/libexec/child")
    return "\n".join(lines)


def test_the_tree_meter_keeps_the_time_of_processes_that_exited():
    """A booting device's short-lived processes exit inside a window; a sum
    over the live ones alone read two boots quiet while the tree ran 7.5
    cores (2026-11-04). The meter keeps each pid's largest time seen."""
    meter = conftest._SimTreeMeter()
    assert meter.update("SIM-U", _sim_ps("0:01.00", (2, "0:10.00"), (3, "0:05.00"))) == 16.0
    # pid 3 exited, pid 2 grew, pid 4 is new: 3's 5 s stay counted.
    assert meter.update("SIM-U", _sim_ps("0:02.00", (2, "0:20.00"), (4, "0:01.00"))) == 2 + 20 + 5 + 1
    # The live-only reading of the same listing loses pid 3.
    assert conftest._tree_cpu_s_from_ps(_sim_ps("0:02.00", (2, "0:20.00"), (4, "0:01.00")), "SIM-U") == 23.0
    assert meter.update("OTHER", _sim_ps("0:01.00")) is None


def test_the_waits_read_the_tree_inside_each_window():
    slept, ticks = [], []
    conftest._sleep_window(slept.append, 5.0, ticks.append, "SIM-U")
    assert sum(slept) == 5.0 and len(slept) == round(5.0 / conftest._SIM_TREE_SUBSAMPLE_S)
    assert len(ticks) == len(slept) - 1
    slept.clear()
    conftest._sleep_window(slept.append, 5.0, None, "SIM-U")
    assert slept == [5.0]


# ── A tier_1-only run warms the fauna-ffi cdylib BEFORE it takes `tier_1` ─────
# 


class _Tier1Item:
    def __init__(self, path):
        self.nodeid = f"{path.name}::test_y"
        self.fixturenames = []
        self.path = path

    @staticmethod
    def iter_markers():
        class _M:
            name = "tier_1"

        return [_M()]


class _Tier1Session:
    def __init__(self, paths):
        self.items = [_Tier1Item(p) for p in paths]

        class _Opt:
            collectonly = False

        class _Cfg:
            option = _Opt()

        self.config = _Cfg()


@pytest.fixture
def tier_1_order(monkeypatch):
    events: list = []
    monkeypatch.setattr(conftest, "_refuse_drafts_window_collision", lambda s: None)
    monkeypatch.setattr(conftest, "_ensure_fauna_ffi_built", lambda: events.append("ffi-build"))
    monkeypatch.setattr(conftest, "_admit_tier_1_run", lambda: events.append("take-tier_1"))
    monkeypatch.delenv(conftest.PREWARM_ONLY_ENV, raising=False)
    monkeypatch.setattr(conftest, "_FFI_REACH_MEMO", {})
    return events


def test_a_tier_1_run_whose_item_reaches_fauna_ffi_builds_it_before_the_take(tier_1_order, tmp_path):
    """The loader imports lazily from a test body, so the build would land in the
    `tier_1` hold; the warm step must run first and hold no slot."""
    mod = tmp_path / "test_lazy.py"
    mod.write_text("def test_y():\n    import fauna_ffi\n", encoding="utf-8")
    conftest.pytest_collection_finish(_Tier1Session([mod]))
    assert tier_1_order == ["ffi-build", "take-tier_1"]


def test_a_tier_1_run_reaching_fauna_ffi_through_a_helper_builds_it_too(tier_1_order, tmp_path):
    mod = tmp_path / "test_via_helper.py"
    mod.write_text("from helpers.enrollment import enroll\n", encoding="utf-8")
    conftest.pytest_collection_finish(_Tier1Session([mod]))
    assert tier_1_order == ["ffi-build", "take-tier_1"]


def test_a_tier_1_run_that_never_reaches_fauna_ffi_builds_nothing(tier_1_order, tmp_path):
    mod = tmp_path / "test_plain.py"
    mod.write_text("def test_y():\n    assert True\n", encoding="utf-8")
    conftest.pytest_collection_finish(_Tier1Session([mod]))
    assert tier_1_order == ["take-tier_1"]


def test_a_failed_tier_1_warm_build_does_not_stop_the_run(tier_1_order, tmp_path, monkeypatch):
    def boom():
        raise RuntimeError("cargo failed")

    monkeypatch.setattr(conftest, "_ensure_fauna_ffi_built", boom)
    mod = tmp_path / "test_lazy.py"
    mod.write_text("import fauna_ffi\n", encoding="utf-8")
    conftest.pytest_collection_finish(_Tier1Session([mod]))
    assert tier_1_order == ["take-tier_1"]


class _DraftsMarker:
    def __init__(self, name, args=()):
        self.name = name
        self.args = args


class _DraftsItem:
    def __init__(self, nodeid, markers):
        self.nodeid = nodeid
        self._markers = markers

    def iter_markers(self, name=None):
        return [m for m in self._markers if name is None or m.name == name]

    def get_closest_marker(self, name):
        found = self.iter_markers(name)
        return found[0] if found else None


class _DraftsSession:
    items = [
        _DraftsItem("tests/test_retimed.py::test_a[macos]",
                    [_DraftsMarker("drafts_autosave_window_ms", (60000,))]),
        _DraftsItem("tests/test_production.py::test_b[macos]",
                    [_DraftsMarker("drafts_production_window")]),
    ]


def test_drafts_window_collision_refuses_a_run_that_executes_tests(monkeypatch):
    """A selection that both re-times the drafts debounce and needs the
    production window is refused when its tests would run."""
    monkeypatch.delenv(conftest.PREWARM_ONLY_ENV, raising=False)
    monkeypatch.delenv(conftest.PLAN_ONLY_ENV, raising=False)
    with pytest.raises(pytest.UsageError, match="drafts_autosave_window_ms re-times"):
        conftest._refuse_drafts_window_collision(_DraftsSession())


@pytest.mark.parametrize("env", ["PREWARM_ONLY_ENV", "PLAN_ONLY_ENV"])
def test_drafts_window_collision_lets_a_run_that_executes_nothing_through(monkeypatch, env):
    """A prewarm-only or plan-only run executes no test, so the session-wide
    re-timing can turn nothing red: the whole-suite warm step (the mac seed
    donor's tail, the background tester's warm step) is not refused. Measured
    2026-11-04: two live mac gate passes' donor warm steps exited 4 on it."""
    monkeypatch.delenv(conftest.PREWARM_ONLY_ENV, raising=False)
    monkeypatch.delenv(conftest.PLAN_ONLY_ENV, raising=False)
    monkeypatch.setenv(getattr(conftest, env), "1")
    conftest._refuse_drafts_window_collision(_DraftsSession())
