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
    build_pos = joined.index("prev_build.py --build", path_pos)
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
    target_dir = tmp_path / "cargo-target"
    binary, stamp = common_nest._example_build_paths(target_dir, "recovery_fixture")
    binary.parent.mkdir(parents=True)
    binary.write_text("fake exe")
    stamp.write_text("stamp")
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    monkeypatch.setattr(common_nest, "_example_newest_source_mtime",
                        lambda repo: stamp.stat().st_mtime - 3600)

    def _no_subprocess(*a, **k):
        raise AssertionError("a warm build_recovery_fixture must spawn nothing")

    monkeypatch.setattr(common_nest.subprocess, "run", _no_subprocess)
    assert common_nest.build_recovery_fixture() == str(binary)


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

# (builder, source-mtime hook, build paths → (binary, stamp), cargo exe stem)
_STAMP_WRITERS = [
    ("build_node", "_nest_newest_source_mtime",
     lambda td: common_nest._nest_variant_paths(td, "test-hooks,nostr", "debug"), "fauna-nest"),
    ("build_sync_agent", "_sync_agent_newest_source_mtime",
     common_nest._sync_agent_build_paths, "fauna-sync-agent"),
    ("build_recovery_fixture", "_example_newest_source_mtime",
     lambda td: common_nest._example_build_paths(td, "recovery_fixture"), "recovery_fixture"),
    ("build_render_cloud_init", "_example_newest_source_mtime",
     lambda td: common_nest._example_build_paths(td, "render_cloud_init"), "render_cloud_init"),
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
    fn = getattr(common_nest, builder)
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

    def __init__(self):
        self.events = []
        self._next_fd = 100
        self._pool_of = {}

    @staticmethod
    def e2e_app_pool(apps):
        return "e2e_other"

    @staticmethod
    def e2e_lane(heavy_tests):
        return "short"

    def acquire_slot(self, pool, cmd=None):
        import os

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

    def release_slot(self, fd, pool=None):
        import os

        self._pool_of.pop(fd, None)
        self.events.append(("release", pool))
        if pool is not None:
            os.environ.pop(f"FAUNA_SLOT_HELD_{pool.upper()}", None)
            if pool != "e2e_long":
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
    monkeypatch.setattr(conftest, "_E2E_SLOT", None)
    yield mod, tree
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


def test_a_prewarm_only_run_writes_no_last_green_record(monkeypatch, capsys):
    from helpers import last_green

    monkeypatch.setenv(conftest.PREWARM_ONLY_ENV, "1")
    monkeypatch.setattr(last_green, "write", lambda *a, **k: pytest.fail("wrote the directory"))
    conftest._write_last_green(None, pytest.ExitCode.OK)
    assert "NOT written — a prewarm-only run" in capsys.readouterr().out


def test_an_interrupted_session_writes_no_last_green_record(monkeypatch, capsys):
    """The yield's other half (delayed-testing.md § The background tester): a shard
    the tester stops, or a warm-only refusal, ends INTERRUPTED and the directory's
    writer declines it — the stopped tests stay neither red nor green."""
    from helpers import last_green

    monkeypatch.setattr(last_green, "write", lambda *a, **k: pytest.fail("wrote the directory"))
    conftest._write_last_green(None, pytest.ExitCode.INTERRUPTED)
    assert "NOT written — the session was interrupted" in capsys.readouterr().out


# ── The yield at a test boundary: the tester's flag ends the session after the current test, the
# completed tests recorded; a Ctrl-C still writes nothing.

def test_a_yield_stopped_session_writes_its_completed_tests(monkeypatch, capsys):
    from helpers import last_green

    wrote = []
    monkeypatch.setattr(last_green, "write", lambda *a, **k: wrote.append(1))
    monkeypatch.setattr(conftest, "_yield_stop", {"reason": "a waiter in 'build'"})
    conftest._write_last_green(None, pytest.ExitCode.INTERRUPTED)
    assert wrote == [1], "the boundary stop's completed tests are the tester's to keep"
    monkeypatch.setattr(conftest, "_yield_stop", {})
    conftest._write_last_green(None, pytest.ExitCode.INTERRUPTED)
    assert wrote == [1] and "NOT written — the session was interrupted" in capsys.readouterr().out


_YIELD_PROJECT_CONFTEST = '''
import importlib.util, json, os, sys
import pytest

sys.path.insert(0, {e2e!r})
_spec = importlib.util.spec_from_file_location("e2e_conftest_for_yield", {conftest!r})
real = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(real)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_teardown(item, nextitem):
    yield
    real._yield_at_boundary(item, nextitem)


def pytest_sessionfinish(session, exitstatus):
    with open(os.environ["FAUNA_E2E_YIELD_FILE"] + ".session", "w") as fh:
        json.dump({{"exit": int(exitstatus), "stop": real._yield_stop}}, fh)
'''

_YIELD_PROJECT_TESTS = '''
import os

def test_one():
    with open(os.environ["FAUNA_E2E_YIELD_FILE"], "w") as fh:
        fh.write("a waiter in 'build' (pid 20)")

def test_two():
    pass

def test_three():
    pass
'''


def test_a_raised_flag_ends_a_real_session_after_the_current_test(tmp_path):
    """Real pytest, the real hook: the flag raised during the first test stops the
    session at its end — exit INTERRUPTED, the two tests behind it never run and
    are named in the stop file the tester reads."""
    import json
    import os
    import subprocess

    e2e = Path(conftest.__file__).resolve().parent
    project = tmp_path / "project"
    project.mkdir()
    (project / "conftest.py").write_text(_YIELD_PROJECT_CONFTEST.format(
        e2e=str(e2e), conftest=str(e2e / "conftest.py")))
    (project / "test_yield.py").write_text(_YIELD_PROJECT_TESTS)
    flag = tmp_path / "yield"
    env = {k: v for k, v in os.environ.items() if not k.startswith("PYTEST_")}
    env[conftest.YIELD_FILE_ENV] = str(flag)
    out = subprocess.run([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider",
                          "--rootdir", str(project), str(project)],
                         cwd=project, env=env, capture_output=True, text=True, timeout=120)
    assert out.returncode == pytest.ExitCode.INTERRUPTED, out.stdout + out.stderr
    assert "1 passed" in out.stdout, out.stdout
    stopped = (tmp_path / "yield.stopped").read_text().splitlines()
    assert stopped[0] == "the tester yields: a waiter in 'build' (pid 20)"
    assert [line.rsplit("::", 1)[1] for line in stopped[1:]] == ["test_two", "test_three"]
    session = json.loads((tmp_path / "yield.session").read_text())
    assert session == {"exit": 2, "stop": {"reason": "a waiter in 'build' (pid 20)"}}


def test_no_flag_or_no_next_test_stops_nothing(tmp_path, monkeypatch):
    class _Session:
        shouldstop = False
        items = []

    class _Item:
        session = _Session()
        nodeid = "t::a"

    monkeypatch.setattr(conftest, "_yield_stop", {})
    flag = tmp_path / "yield"
    monkeypatch.setenv(conftest.YIELD_FILE_ENV, str(flag))
    conftest._yield_at_boundary(_Item(), _Item())
    assert _Item.session.shouldstop is False, "no flag raised"
    flag.write_text("the fence")
    conftest._yield_at_boundary(_Item(), None)
    assert _Item.session.shouldstop is False, "the last test: the session ends on its own"
    assert not (tmp_path / "yield.stopped").exists()


# ── The lane usage record: every lane release logs its
# hold — the refuse loop's and the session end's alike — before the lock goes.


def _holds(mod):
    return [e for e in mod.events if isinstance(e, tuple) and e[0] == "hold"]


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
    conftest.pytest_collection_finish(_FakeSession())
    conftest._release_e2e_slot()
    tail = [e for e in mod.events if isinstance(e, tuple) and e[0] in ("hold", "release")]
    assert [e[:2] for e in tail] == [
        ("hold", "e2e_other"), ("release", "e2e_other"), ("release", "e2e_long"),
    ], mod.events
    import inspect

    assert "_release_e2e_slot()" in inspect.getsource(conftest.pytest_sessionfinish)


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
    jobs = inspect.getsource(conftest._prebuild_binaries)
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
    """`helpers.succession` builds `recovery_fixture` on first call, from inside
    a test body, so `_prebuild_binaries` keys the hoist on the module's import
    (measured 2026-09-26: a cold build inside the test's 900 s budget killed the
    whole pytest process on Windows). Its siblings `succession_ceremony` /
    `succession_retry` never build it and must not trigger a cold cargo build."""
    cases = {
        "from helpers.succession import register_recovery_kit\n": True,
        "    from helpers.succession import succeed_identity\n": True,
        "from helpers import succession\n": True,
        "import helpers.succession\n": True,
        "from helpers.succession_ceremony import settled_actor_id\n": False,
        "from helpers.succession_retry import x\n": False,
        "from helpers import succession_ceremony\n": False,
        "# from helpers.succession import nothing\n": False,
    }
    for i, (text, expected) in enumerate(cases.items()):
        path = tmp_path / f"test_case_{i}.py"
        path.write_text(text)
        item = type("Item", (), {"path": path})()
        assert conftest._item_reaches_recovery_fixture(item) is expected, text


def test_prebuild_binaries_hoists_the_recovery_fixture_build(monkeypatch, tmp_path):
    """An item whose module imports `helpers.succession` makes the collection-
    time prebuild run `_ensure_recovery_fixture_built`; an unrelated item does
    not (laziness preserved)."""
    from helpers import nest_mode as nest_mode_mod

    monkeypatch.setattr(nest_mode_mod, "builds_local_nest", lambda mode: True)
    ran = []
    monkeypatch.setattr(
        conftest, "_ensure_recovery_fixture_built", lambda: ran.append("rf")
    )
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
    assert ran == ["rf"]


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
    slot = common_nest._build_slot_cmd(t.repo)
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
    full xcframework restore, the iOS `xcodebuild`, then the simulator — and the
    warm pass skips only the simulator, so the restore precedes the iOS build
    there too."""
    ran = []
    for name in ("_ensure_app_built",):
        monkeypatch.setattr(conftest, name, lambda app: ran.append(f"app[{app}]"))
    for name in ("_restore_full_apple_ffi", "_ensure_ios_app_built", "_get_ios_setup"):
        monkeypatch.setattr(conftest, name, (lambda n: lambda: ran.append(n))(name))
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: {"ios", "macos"})
    session = type("S", (), {"items": []})()
    conftest._prebuild_binaries(session)
    assert ran == ["app[ios]", "app[macos]", "_restore_full_apple_ffi",
                   "_ensure_ios_app_built", "_get_ios_setup"], ran
    ran.clear()
    conftest._prebuild_binaries(session, prewarm=True)
    assert ran == ["app[ios]", "app[macos]", "_restore_full_apple_ffi",
                   "_ensure_ios_app_built"], ran


def test_the_ios_simulator_boot_is_reported_as_setup_not_prebuild(monkeypatch):
    """The simulator's create-and-finish-booting waits for no `build`, so the
    hold charges it to `setup_s` (the usage line's `inhold_setup_s`) and only the
    rest of the pass to `prebuild_s`."""
    monkeypatch.setattr(conftest, "_ensure_app_built", lambda app: None)
    monkeypatch.setattr(conftest, "_ensure_ios_app_built", lambda: None)
    monkeypatch.setattr(conftest, "_get_ios_setup", lambda: time.sleep(0.3))
    monkeypatch.setattr(conftest, "_real_fixture_closure", lambda item: set())
    monkeypatch.setattr(conftest, "_prebuild_app_set", lambda s, r: {"ios"})
    hold = conftest._new_hold({"ios"})
    conftest._timed_prebuild(hold, lambda: conftest._prebuild_binaries(type("S", (), {"items": []})()))
    assert hold["setup_s"] >= 0.3
    assert 0 <= hold["prebuild_s"] < 0.3, hold


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
