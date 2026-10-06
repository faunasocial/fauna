"""win's host-side builds must join the SHARED host tree, not a private
`--target aarch64-pc-windows-msvc` one.

The win sibling of `test_mac_host_tree_is_shared.py`, and the same defect: Windows
is arm64, so `aarch64-pc-windows-msvc` **is** this machine's host triple, and
cargo hard-separates explicit-target units from implicit-host ones even when the
triple is identical. Measured 2026-08-23 with `cargo build -Z unstable-options
--unit-graph` against the dev inner loop's own graph (`cargo build -p fauna-nest
--features test-hooks,nostr`, 1020 units / 82 workspace-local):

    windows-ffi   explicit --target :  239/1055 units,  0/96 workspace-local
    windows-ffi   implicit host     :  819/1009 units, 50/96 workspace-local
    sync-service  explicit --target :  247/1032 units,  0/66 workspace-local
    sync-service  implicit host     :  883/988  units, 61/66 workspace-local

Zero workspace-local units shared on either path means every `libs/fauna-*` crate
compiled a second time in every win checkout, on every `libs/` edit.

**This file's real job is to pin the GUARD, not the flag.** Joining the shared
tree puts the dll in `target/<profile>/fauna_ffi.dll` — one slot for every host
build of fauna-ffi in the workspace. The live hazard on Windows is
`_windows-ffi-flavor`'s OWN two flavors: `production` and `test-helpers` are the
convention-15 security seam split and would share one path. Beyond that,
`ffi-store-safe-check` (`--no-default-features --features store-safe`) and
`android-host-test` (`--features test-helpers`) write the same dev-profile
slot, as does any bare `cargo build -p fauna-ffi` / `--workspace` — neither recipe
is on the Windows host's merge-gate path (which runs test-compile-check, dotnet test,
windows-debug, windows-release, feature-test-compile-check, and store-safe's
artifact probe is unix-only), so on this machine those are the hand-run case.

Either way the failure is silent: a foreign build leaves the shared dll newer
while `_windows-ffi-flavor`'s source-keyed `--stamp` still reads fresh, so cargo
is skipped and the bindgen gate regenerates the C# bindings from the WRONG flavor
and stages that dll into `runtimes/win-arm64/native/`. So the recipe copies its
dll to a flavor-private slot, and — the part that is easy to get wrong — the copy
is **part of the gated build command itself**. A separate `--source shared
--target private` gate would re-copy whenever another recipe touched the shared
slot, i.e. it would import exactly the dll it exists to keep out.

mac solved the same problem with `--artifact-dir`; that flag does **not** compose
with `cargo rustc`, and `_windows-ffi-flavor` needs `cargo rustc` for its
deliberate `--lib --crate-type cdylib` trim (§ UniFFI cdylib rebuild cost (win);
the unused staticlib alone is ~474 MB). Hence the copy.

Structural/text analysis only — the real-build verification is `just
windows-ffi-test dev` + `just windows-debug` on a Windows box, which no other dev
machine can run (so these assertions are machine-independent by construction and
run everywhere, the same reasoning as `test_windows_ffi_warm_takes_no_slot`).
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"
_NEST_HELPERS = _REPO / "tests" / "common" / "nest.py"
# The release workflow ships from the public-files tree (ported 2026-08-31 —
# release-integrity.md § Release signing → *When a release workflow publishes*).
# In the curated public tree that source directory is pruned and the file is
# injected at .github/workflows/, so resolve whichever location this tree has.
_SOURCE_RELEASE_WORKFLOW = (
    _REPO / "scripts" / "publish" / "public-files" / ".github" / "workflows" / "release.yml"
)
_RELEASE_WORKFLOW = (
    _SOURCE_RELEASE_WORKFLOW
    if _SOURCE_RELEASE_WORKFLOW.exists()
    else _REPO / ".github" / "workflows" / "release.yml"
)

_HOST_TRIPLE_FLAG = "--target aarch64-pc-windows-msvc"


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name arg *ARGS:` declaration alike. Duplicated from
    `test_mac_host_tree_is_shared.py` per this suite's
    no-cross-import-between-test-files convention (small helpers get duplicated,
    not imported)."""
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


def _code(body: str) -> str:
    """Recipe body with comment lines dropped and whitespace collapsed — the
    comments *explain* the removed flag at length, so a naive substring search
    over the raw body can never go red."""
    code_lines = [ln for ln in body.splitlines() if not ln.lstrip().startswith("#")]
    return " ".join(" ".join(code_lines).split())


def _justfile() -> str:
    return _JUSTFILE.read_text(encoding="utf-8")


# ── the FFI recipe: implicit host + a flavor-private artifact ────────────────

def assert_ffi_flavor_joins_the_shared_host_tree(text: str) -> None:
    """Split out as a plain function so the pin can be red-verified against a
    PAST justfile (`git show <sha>:justfile`) without checking that tree out."""
    code = _code(_recipe_body(text, "_windows-ffi-flavor"))
    assert code, "_windows-ffi-flavor recipe not found"
    assert "-p fauna-ffi" in code, "_windows-ffi-flavor is expected to build fauna-ffi"
    assert _HOST_TRIPLE_FLAG not in code, (
        f"_windows-ffi-flavor must build IMPLICIT-host: `{_HOST_TRIPLE_FLAG}` "
        f"names win's OWN triple, so it produces identical code in a SEPARATE "
        f"artifact tree — measured at 0/96 workspace-local units shared with the "
        f"dev inner loop, i.e. every libs/fauna-* crate compiled twice per win "
        f"checkout."
    )


def test_windows_ffi_flavor_builds_implicit_host():
    assert_ffi_flavor_joins_the_shared_host_tree(_justfile())


def assert_ffi_artifact_is_flavor_private(text: str) -> None:
    code = _code(_recipe_body(text, "_windows-ffi-flavor"))
    assert code, "_windows-ffi-flavor recipe not found"
    slot = re.search(r'FFI_DIR="([^"]*)"', code)
    assert slot, (
        "_windows-ffi-flavor builds into the shared host tree, so it must define "
        "an FFI_DIR — a flavor-private slot for its own dll. The plain "
        "`target/<profile>/fauna_ffi.dll` is one slot for BOTH this recipe's "
        "flavors (the convention-15 seam split) and for every other host build of "
        "fauna-ffi, and whichever built last would own the file."
    )
    path = slot.group(1)
    assert "windows-ffi/" in path, (
        f"FFI_DIR={path} must be a real subdirectory this recipe owns "
        f"(`windows-ffi/<flavor>/`); the bare profile dir IS the shared slot"
    )
    assert "FEATURES" in path, (
        f"FFI_DIR={path} must carry the FEATURES axis — production and "
        f"test-helpers are the security-relevant flavor split (testing.md "
        f"convention 15), and one path for both is the seam leak it exists to stop"
    )
    assert re.search(r'FFI_DLL="\$\{?FFI_DIR\}?/fauna_ffi\.dll"', code), (
        "FFI_DLL must be the dll inside that private slot"
    )


def test_the_ffi_artifact_slot_is_flavor_private():
    assert_ffi_artifact_is_flavor_private(_justfile())


def assert_the_private_copy_is_inside_the_gated_build(text: str) -> None:
    """The subtle half. The copy into the private slot must be part of the SAME
    command build-if-stale gates, so it can only ever run for a build this recipe
    just performed. Gating the copy separately on the shared slot's mtime would
    re-copy whatever another recipe left there — importing the exact dll the
    private slot exists to exclude."""
    code = _code(_recipe_body(text, "_windows-ffi-flavor"))
    assert code, "_windows-ffi-flavor recipe not found"
    gates = [seg for seg in code.split("build-if-stale.py")[1:]]
    assert gates, "_windows-ffi-flavor must gate its cargo step with build-if-stale"
    cargo_gate = next(
        (g for g in gates if "cargo-win.cmd rustc" in g and "-p fauna-ffi" in g), None
    )
    assert cargo_gate is not None, "the cargo-rustc step must be build-if-stale gated"
    run = cargo_gate.split(" -- ", 1)
    assert len(run) == 2, "build-if-stale needs its `--` command separator"
    command = run[1]
    # `CARGO_OUT_DIR` replaced the literal `target/$PROFILE_DIR` 2026-08-24 (the
    # x64 cross leg): `CARGO_OUT_DIR="target${TARGET:+/$TARGET}/$PROFILE_DIR"`
    # so an explicit --target build reads its own triple's output dir instead of
    # the implicit-host one.
    # The copy is `scripts/win-stage-dll.sh` (rename-aside, then copy) since
    # 2026-09-28: a bare `cp` over a dll some live process has mapped dies
    # `Device or resource busy` (test_windows_ffi_stage_over_mapped_dll.py).
    assert 'scripts/win-stage-dll.sh "$CARGO_OUT_DIR/fauna_ffi.dll" "$FFI_DLL"' in command, (
        "the copy into the flavor-private slot must live INSIDE the gated build "
        "command (after the cargo call, joined by &&), not as a gate of its own — "
        "a `--source <shared> --target <private>` gate would re-copy whenever any "
        "other host build of fauna-ffi touched the shared slot, which is precisely "
        "the wrong-flavor dll this guard exists to keep out"
    )


def test_the_private_copy_is_part_of_the_gated_build_command():
    assert_the_private_copy_is_inside_the_gated_build(_justfile())


def test_the_bindgen_reads_the_private_slot_not_the_shared_one():
    """`_windows-ffi-bindgen` runs uniffi-bindgen against a dll and stages that
    same dll into the app. Pointed at `target/<profile>/fauna_ffi.dll` it would
    read whatever feature set happened to build last."""
    code = _code(_recipe_body(_justfile(), "_windows-ffi-bindgen"))
    assert code, "_windows-ffi-bindgen recipe not found"
    assert "windows-ffi/" in code, (
        "_windows-ffi-bindgen must resolve the FLAVOR-PRIVATE dll "
        "(`target/<profile>/windows-ffi/<flavor>/fauna_ffi.dll`), never the "
        "shared `target/<profile>/fauna_ffi.dll` slot"
    )
    assert not re.search(r'--library\s+target/\{\{profile_dir\}\}/fauna_ffi\.dll', code), (
        "_windows-ffi-bindgen must not generate bindings from the shared slot"
    )


# ── the e2e agent build ──────────────────────────────────────────────────────

def assert_sync_agent_builds_implicit_host(text: str) -> None:
    # 2026-08-25: the build itself moved from an inline subprocess.run
    # in conftest.py's sync_agent_binary fixture into
    # common.nest.build_sync_service_win (gate-then-slot composition, mirroring
    # build_node()) — this asserts against THAT function's text now, not the
    # fixture, which just delegates to it via _ensure_sync_agent_built.
    # The package is `fauna-sync-agent`, named ALONE: the windows-only wrapper
    # crate that used to produce this exe is retired, and a second `-p` in the
    # same cargo invocation would unify its features into the agent's.
    assert '["build", "-p", "fauna-sync-agent"]' in text, (
        "common.nest.build_sync_service_win is expected to build the "
        "fauna-sync-agent package, and that package alone"
    )
    assert '"--target"' not in text, (
        "build_sync_service_win must build IMPLICIT-host — measured at "
        "0/66 workspace-local units shared with the dev inner loop, 61/66 after "
        "the drop. Its macOS twin `macos_sync_agent_binary` had always built this "
        "way; the two fixtures are now the same shape."
    )
    assert 'debug_dir / "fauna-sync-agent.exe"' in text, (
        "_sync_service_win_build_paths must point at the shared host tree's output"
    )


def test_e2e_sync_agent_builds_implicit_host():
    assert_sync_agent_builds_implicit_host(_NEST_HELPERS.read_text(encoding="utf-8"))


# ── the deliberate carve-outs ────────────────────────────────────────────────

def test_the_release_workflow_keeps_its_explicit_triple():
    """`release.yml` builds BOTH windows arches from one runner, so naming the
    triple is load-bearing there and always will be — an implicit-host build
    cannot produce the x86_64 artifact at all.

    Pinned so a future session applying the implicit-host rule does not "finish
    the job" across the release path. If it ever legitimately changes, delete
    this pin in the same commit that re-measures and updates the layout
    notes."""
    text = _RELEASE_WORKFLOW.read_text(encoding="utf-8")
    assert "rust_target: aarch64-pc-windows-msvc" in text, (
        "release.yml no longer names the arm64 windows triple — the release path "
        "is cross-arch by definition and must keep it"
    )


def test_the_gnullvm_slice_stays_explicit():
    """The Go mail bridge consumes a SEPARATE fauna_ffi built for
    `*-pc-windows-gnullvm` (llvm-mingw, because cgo cannot use MSVC). That one is
    genuinely cross-compiled — it is not the host triple and could not drop the
    flag even in principle."""
    text = _justfile()
    assert "aarch64-pc-windows-gnullvm" in text, (
        "the gnullvm slice is genuinely cross (cgo cannot link MSVC output) and "
        "must keep its explicit --target"
    )
