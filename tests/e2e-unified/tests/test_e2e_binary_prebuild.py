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
    cp_pos = joined.index('cp "$CARGO_LIB" "$LIB"')
    assert cargo_pos < cp_pos, "the private-slot copy must run after the cargo build, in the same gated command"


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
    binary, stamp = common_nest._recovery_fixture_build_paths(target_dir)
    binary.parent.mkdir(parents=True)
    binary.write_text("fake exe")
    stamp.write_text("stamp")
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    monkeypatch.setattr(common_nest, "get_repo_root", lambda: tmp_path)
    monkeypatch.setattr(common_nest, "_recovery_fixture_newest_source_mtime",
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
    assert common_nest._recovery_fixture_newest_source_mtime(tmp_path) <= stamp_mtime

    os.utime(shared, (3_000, 3_000))  # only the SHARED crate moved
    assert common_nest._recovery_fixture_newest_source_mtime(tmp_path) > stamp_mtime


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
    ("build_sync_service_win", "_sync_service_win_newest_source_mtime",
     common_nest._sync_service_win_build_paths, "fauna-sync-agent"),
    ("build_recovery_fixture", "_recovery_fixture_newest_source_mtime",
     common_nest._recovery_fixture_build_paths, "recovery_fixture"),
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
    built = tmp_path / stem  # cargo's JSON-reported exe (build_sync_service_win reads `binary`)
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

    def log_lane_hold(self, fd, apps, inhold_prebuild_s=None):
        # The real writer records a hold only for a lane fd it granted.
        pool = self._pool_of.get(fd)
        if pool not in (None, self.E2E_LONG_POOL):
            self.events.append(("hold", pool, sorted(apps), inhold_prebuild_s))

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
