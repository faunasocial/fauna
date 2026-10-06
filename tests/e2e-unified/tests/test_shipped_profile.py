"""The shipped-profile overflow ruling: release artifacts carry overflow checks.

`docs/goal/architecture/build-system.md` § Shipped-profile overflow checks. Every
shipped artifact builds `release` (or `dist`, which inherits it): the nest Docker
image, the Windows installer binaries, the apple/android production FFI, the Go
mail bridge's staticlib, wasm-pack's web output. `cargo test` builds dev, where
`overflow-checks` is on by default — so before the ruling, every red-verify of an
"overflow is refused" guard was measuring a profile we do not ship, and the
shipped profile silently wrapped instead (a wrap past a quota ceiling
flipped `users.storage_bytes_used` to SQLite REAL and took out `list_users` for
the whole nest — client-causable unrecoverable state).

This file pins the manifest facts the ruling rests on:

1. `[profile.release]` sets `overflow-checks = true`, and no inheriting shipped
   profile switches it back off — debug and release agree on overflow, so a
   debug test's verdict transfers to the artifact.
2. No profile sets `panic = "abort"`. The ruling's blast-radius argument is that
   an overflow panic UNWINDS into a documented container — a nest request task,
   `engine_host.rs`'s per-engine `catch_unwind`, UniFFI's scaffolding — and
   every one of those containers requires unwinding to exist. (`Cargo.toml`'s
   `dist` comment has said "do NOT add `panic = 'abort'`" since the profile was
   added; this turns the comment into a gate.)
3. `[profile.release]` does not set `debug-assertions = true`, and no
   inheriting shipped profile switches it on. Convention 15's shared-crate
   witnesses (`test_shared_crate_seam_gating.py`'s `_SAFE_GATE_CLAUSES`)
   accept a bare `debug_assertions` `#[cfg(...)]` as a safe gate on over 150
   automation seams across `libs/`+`bins/` — safe only because this manifest
   fact holds. Flip it and every one of those seams resolves `true` in a
   release build (`docs/goal/architecture/e2e-automation-surface-gating.md`
   § Implementation status today, the fifth-witness blind-spot-(2) this test
   pins).

Pure manifest parse — no build, no toolchain, so it runs on any machine.
"""

import tomllib
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]


def _profiles() -> dict:
    manifest = tomllib.loads((_REPO / "Cargo.toml").read_text(encoding="utf-8"))
    profiles = manifest.get("profile", {})
    assert profiles, "root Cargo.toml declares no [profile.*] sections at all"
    return profiles


def test_release_profile_carries_overflow_checks():
    profiles = _profiles()
    release = profiles.get("release", {})
    assert release.get("overflow-checks") is True, (
        "[profile.release] must set `overflow-checks = true` "
        "(build-system.md § Shipped-profile overflow checks): without it every "
        "shipped artifact silently wraps on the arithmetic every test panics on."
    )


def test_no_shipped_profile_switches_overflow_checks_back_off():
    for name, profile in _profiles().items():
        # `dev` may configure itself freely (its default is already on); the
        # ruling is about what ships. Anything inheriting release — dist today,
        # any future shipped profile — must not undo the release setting.
        if name == "dev":
            continue
        assert profile.get("overflow-checks") is not False, (
            f"[profile.{name}] switches overflow-checks back OFF, undoing the "
            "shipped-profile ruling (build-system.md § Shipped-profile overflow "
            "checks). Site-local `wrapping_*` is the sanctioned escape hatch, "
            "never a profile-wide revert."
        )


def test_release_profile_does_not_enable_debug_assertions():
    profiles = _profiles()
    release = profiles.get("release", {})
    assert release.get("debug-assertions") is not True, (
        "[profile.release] must not set `debug-assertions = true` "
        "(e2e-automation-surface-gating.md's fifth-witness blind spot (2)): "
        "every `debug_assertions`-gated automation seam in the tree — over 150 "
        "of them, `_SAFE_GATE_CLAUSES` in test_shared_crate_seam_gating.py — "
        "resolves true in a release build the moment this flips, compiling the "
        "automation surface into the nest Docker image, the installer "
        "binaries, and the apple/android/wasm/Go-bridge production artifacts."
    )


def test_no_shipped_profile_switches_debug_assertions_on():
    for name, profile in _profiles().items():
        # `dev` may configure itself freely (its default is already on); the
        # ruling is about what ships.
        if name == "dev":
            continue
        assert profile.get("debug-assertions") is not True, (
            f"[profile.{name}] switches debug-assertions ON, silently un-gating "
            "every automation seam that trusts `debug_assertions` alone as a "
            "safe `#[cfg(...)]` clause (e2e-automation-surface-gating.md's "
            "fifth-witness blind spot (2))."
        )


def test_no_shipped_profile_package_override_switches_debug_assertions_on():
    for name, profile in _profiles().items():
        # `dev` may configure itself freely (its default is already on); the
        # ruling is about what ships.
        if name == "dev":
            continue
        # A `[profile.<name>.package.<crate>]` table parses under a `package`
        # key whose own keys are crate names (or the literal "*" wildcard,
        # which Cargo applies to every crate in the profile not named more
        # specifically) — a sibling table to the profile's top-level keys,
        # invisible to the top-level check above.
        for pkg_name, pkg_table in profile.get("package", {}).items():
            assert pkg_table.get("debug-assertions") is not True, (
                f'[profile.{name}.package."{pkg_name}"] switches debug-assertions ON '
                "for that crate's compilation unit, silently un-gating every "
                "automation seam in its sources that trusts `debug_assertions` alone "
                "as a safe `#[cfg(...)]` clause (e2e-automation-surface-gating.md's "
                "fifth-witness blind spot (2)). No legitimate use case sanctions a "
                "package-level `debug-assertions` override — unlike `overflow-checks`, "
                "which build-system.md:750-754's escape hatch explicitly covers "
                "(the dev-only ruling: build-system.md:747-748)."
            )


def test_no_profile_sets_panic_abort():
    for name, profile in _profiles().items():
        assert profile.get("panic") != "abort", (
            f"[profile.{name}] sets `panic = \"abort\"` — UniFFI's FFI "
            "scaffolding relies on `catch_unwind`, and the overflow ruling's "
            "containment story (one request task / one engine, not the process) "
            "requires unwinding (build-system.md § Shipped-profile overflow "
            "checks; release-integrity.md's measured request-task witness)."
        )
