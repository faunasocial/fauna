"""tier_1: the harness cdylib loader ALWAYS routes through `just e2e-ffi` on
linux/mac — never only when nothing exists on disk.

The regression: `_find_cdylib`
used to build ONLY when no file was found on disk at all, and otherwise
returned whichever `libfauna_ffi.*` happened to sit in the shared
`target/<profile>/` slot — the SAME slot `mail-bridge-ffi`'s
`--no-default-features --features labeler` build (and any other host build of
fauna-ffi) writes. A `just mail-bridge-ffi` build left behind there predated a
`libs/fauna-ffi/src/cabi.rs` change, so four folder e2e tests silently ran
against a stale library.

The fix routes every call through `just e2e-ffi` (build-if-stale-gated, so a
warm tree pays ~50ms) into its OWN private slot, the same way `static_dir`
calls `just web` on every call rather than only when the SPA bundle is
missing. This pins that "every call", not "only when absent": the just
invocation must happen twice in a row even though the target file already
exists (and is unchanged) after the first call.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

import fauna_ffi  # noqa: E402

pytestmark = pytest.mark.tier_1


class _FakeProc:
    def __init__(self, returncode: int, stdout: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout


def test_a_populated_slot_never_short_circuits_the_just_call(tmp_path, monkeypatch):
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    calls = []

    def fake_run(cmd, cwd, check, **_kw):
        calls.append(cmd)
        lib = fauna_ffi._e2e_ffi_target_root(tmp_path) / fauna_ffi._E2E_FFI_SLOT / "libfauna_ffi.so"
        lib.parent.mkdir(parents=True, exist_ok=True)
        lib.write_bytes(b"not a real cdylib")
        return _FakeProc(0)

    monkeypatch.setattr(fauna_ffi.subprocess, "run", fake_run)

    fauna_ffi._build_via_just_e2e_ffi(tmp_path, "libfauna_ffi.so")
    # The slot is already populated from the first call — a stale-or-not check
    # here would wrongly skip the second invocation.
    fauna_ffi._build_via_just_e2e_ffi(tmp_path, "libfauna_ffi.so")

    assert len(calls) == 2, (
        "a populated private slot must never short-circuit the freshness "
        f"gate — `just e2e-ffi` decides staleness, this loader must not: {calls!r}"
    )
    assert all(cmd[:2] == ["just", "e2e-ffi"] for cmd in calls)


def test_a_failed_just_e2e_ffi_raises_naming_the_recipe(tmp_path, monkeypatch):
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.setattr(fauna_ffi.subprocess, "run", lambda cmd, cwd, check, **_kw: _FakeProc(1))

    with pytest.raises(RuntimeError, match="just e2e-ffi"):
        fauna_ffi._build_via_just_e2e_ffi(tmp_path, "libfauna_ffi.so")


def test_a_successful_run_that_left_no_file_still_raises(tmp_path, monkeypatch):
    # Belt-and-braces: exit 0 alone must not be trusted if the private slot
    # genuinely has nothing in it (e.g. a mismatched profile/slot literal).
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.setattr(fauna_ffi.subprocess, "run", lambda cmd, cwd, check, **_kw: _FakeProc(0))

    with pytest.raises(RuntimeError, match="did not produce"):
        fauna_ffi._build_via_just_e2e_ffi(tmp_path, "libfauna_ffi.so")


def test_a_failed_build_carries_the_recipes_own_output_at_column_zero(tmp_path, monkeypatch):
    # The first build runs at IMPORT, inside pytest's collection capture, and a
    # failed build still imports — so the recipe's own diagnosis must ride the
    # exception or it is lost (the prebuild line then says only "exit 1").
    # Its lines stay unindented: the merge-gate check's INFRA_RE is anchored at
    # ^, and a slot timeout must read INFRA there, not RED.
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    output = "".join(f"noise {i}\n" for i in range(40)) + (
        "[build-slot] ERROR: no 'build' slot freed within 5400s — queue position 3 of 9\n"
    )
    monkeypatch.setattr(
        fauna_ffi.subprocess, "run", lambda cmd, cwd, check, **_kw: _FakeProc(1, output))

    with pytest.raises(RuntimeError) as exc:
        fauna_ffi._build_via_just_e2e_ffi(tmp_path, "libfauna_ffi.so")

    lines = str(exc.value).splitlines()
    assert "[build-slot] ERROR: no 'build' slot freed within 5400s — queue position 3 of 9" in lines
    assert "noise 0" not in lines, "only the tail rides the exception"
    assert len(lines) <= fauna_ffi._BUILD_TAIL_LINES + 2
