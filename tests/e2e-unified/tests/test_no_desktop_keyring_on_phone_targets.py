"""No phone build resolves the freedesktop (D-Bus) credential stack.

Pure dependency-graph analysis: `cargo tree` is a manifest/lockfile resolution,
no compile, no nest, no driver — the same "one cargo metadata call is a manifest
parse" shape as test_feature_gated_test_coverage.py. It runs on EVERY machine,
which is the point: the compile-level guard for this class
(`apple-swift-build-check`'s `cargo check --target aarch64-apple-ios`) only runs
on macOS, and the android triple compiles on no dev VM at all.

THE INCIDENT. `fauna-credential-store` gated
its freedesktop arm `cfg(not(any(target_os = "macos", target_os = "windows")))`
— an exclusion list that silently reads as "anything that isn't macOS or Windows
speaks D-Bus". Phone targets are none of the above but ARE `unix`, so when
`fauna-client-sync` grew a dependency on that crate the iOS,
watchOS and Android slices all began resolving `secret-service` -> `zbus`, which
cannot compile for an apple phone. `just apple-ffi` — required by every
`--app ios` e2e run and every iOS `.xcarchive` — broke outright, while
`mac-debug`, `swift-test` and every merge gate on all three machines stayed
green. Android's case was worse in kind if not in effect: it compiled, so it was
quietly linking a Linux desktop D-Bus keyring client into the APK.

The arms are now a positive allow-list (macOS / Windows / Linux each named,
everything else landing on the inert `no_keyring` arm), so an unrecognised
target can no longer be mistaken for a Linux desktop. This test pins the
PROPERTY rather than that implementation: any future crate re-introducing a
desktop keyring stack into a phone graph fails here, whoever adds it.
"""

import os
import subprocess

import pytest

pytestmark = pytest.mark.tier_1

# Every EXPLICIT `--target` cargo invocation must opt out of workspace feature
# unification — `.cargo/config.toml` § feature unification, guard 2. Without it
# the resolve is "as if the whole workspace were selected", so the queries below
# answer for the WORKSPACE rather than for `fauna-ffi`: measured 2026-08-22, the
# day `[resolver] feature-unification = "workspace"` landed, `cargo tree -p
# fauna-ffi -i secret-service --target aarch64-apple-ios` reported the crate
# PRESENT — reached via `fauna-linux`, a desktop app no phone ever builds. Ten of
# this file's twelve cases went red against a graph no phone artifact has.
#
# A cross-target query that silently answers for a different target is the loud
# form of the failure this file's own docstring warns about: it doesn't read as
# all-clear, it reads as a five-alarm regression, and a gate that cries wolf is
# one people learn to skip past.
_SELECTED_RESOLVE = {**os.environ, "CARGO_RESOLVER_FEATURE_UNIFICATION": "selected"}

# The freedesktop keyring crate and the D-Bus binding it carries. Either one
# appearing in a phone graph is the defect.
_DESKTOP_KEYRING_CRATES = ("secret-service", "zbus")

# Every triple an apple phone or android build actually produces. Kept in step
# with `fauna_ffi::index_launch`'s CLIENT_BUILDS_INDEX (ios / android / watchos /
# tvos), which is this workspace's ratified statement of "phone, not desktop".
#
# ⚠ NOT keyed to `just apple-ffi`'s default slice list any more (2026-08-22): the
# watchOS slices became opt-in (`just apple-ffi-watch`) because nothing links
# them. The watch rows below stay — a `cargo tree` query needs no build, and with
# the compile gone these two rows plus `apple-swift-build-check`'s watchOS
# `cargo check` are the whole of what still watches that graph.
#
# ⚠ The `id=` values are deliberately underscore-separated and contain NO app
# name. conftest's `_parametrized_clients` used to split a test's bracketed
# parametrization on "-" and treat any token matching a known app as a client
# parametrization — so the natural ids `aarch64-apple-ios` and
# `aarch64-linux-android` tokenized to `ios` / `android` and got DESELECTED on
# every default run (whose app set is `[tui]`). That silently removed the two
# most important cases in this file — including iOS, the one that actually
# broke — while the watchOS cases passed and the run still read green. The
# matcher now keys on the real fixture behind each `callspec` entry rather
# than the id string — this direct `@pytest.mark.parametrize`
# is excluded regardless of its ids today — but a target triple still is not
# an app parametrization either way; keep these ids app-name-free.
_PHONE_TARGETS = (
    pytest.param("aarch64-apple-ios", id="apple_phone"),
    pytest.param("aarch64-apple-ios-sim", id="apple_phone_sim"),
    pytest.param("aarch64-apple-watchos", id="apple_watch"),
    pytest.param("aarch64-apple-watchos-sim", id="apple_watch_sim"),
    pytest.param("aarch64-linux-android", id="google_phone"),
)

# The positive control: a target that MUST still resolve the stack. Without it
# this whole file passes vacuously the day the crate is renamed, the query flag
# changes, or `-p fauna-ffi` stops resolving — the "a parse that yields nothing
# must not read as all clear" discipline.
_DESKTOP_CONTROL = "x86_64-unknown-linux-gnu"


def _tree_once(spec: str, target: str) -> subprocess.CompletedProcess:
    """One `cargo tree -i` invocation. No network, no build."""
    return subprocess.run(
        ["cargo", "tree", "-p", "fauna-ffi", "-i", spec, "--target", target],
        capture_output=True,
        text=True,
        timeout=180,
        env=_SELECTED_RESOLVE,
    )


def _ambiguous_specs(combined: str) -> list[str]:
    """The concrete `name@version` specs cargo offers when a bare name is ambiguous.

    Two versions of one crate in the lockfile turn `-i <name>` into
    `error: specification `<name>` is ambiguous`, followed by a `help:` list of
    the exact specs. That is NOT "absent from the graph" — but it exits non-zero
    and prints nothing to stdout, so a bare-name query silently degrades into a
    test that guards nothing. Measured 2026-08-22: `zbus` reached 4.4.0 + 5.14.0
    in `Cargo.lock` and every `zbus` case in this file went red, the positive
    control included.
    """
    if "is ambiguous" not in combined:
        return []
    specs: list[str] = []
    for line in combined.splitlines():
        stripped = line.strip()
        if "@" in stripped and " " not in stripped and not stripped.startswith("help:"):
            specs.append(stripped)
    return specs


def _reverse_tree(crate: str, target: str) -> str:
    """`cargo tree -i` output for one crate on one target, version-count-proof.

    A crate present at SEVERAL versions must be reported present if ANY of them
    reaches the graph, so an ambiguity is resolved by asking about each concrete
    spec rather than by giving up — the "a parse that yields nothing must not read
    as all clear" discipline, applied to cargo's own error surface.
    """
    proc = _tree_once(crate, target)
    combined = proc.stdout + proc.stderr
    specs = _ambiguous_specs(combined)
    if specs:
        out_parts: list[str] = []
        for spec in specs:
            sub = _tree_once(spec, target)
            sub_combined = sub.stdout + sub.stderr
            if sub.returncode != 0 and "nothing to print" not in sub_combined:
                pytest.fail(
                    f"cargo tree failed for {spec} on {target} in a way that is "
                    f"not 'absent from the graph' — the query itself is broken, "
                    f"so its silence proves nothing:\n{sub_combined}"
                )
            out_parts.append(sub.stdout)
        return "".join(out_parts)
    # `cargo tree -i` exits non-zero / warns "nothing to print" when the crate is
    # absent from the graph, which is exactly the state we want on phones. Errors
    # that are NOT that are a broken query and must not read as absence.
    if proc.returncode != 0 and "nothing to print" not in combined:
        pytest.fail(
            f"cargo tree failed for {crate} on {target} in a way that is not "
            f"'absent from the graph' — the query itself is broken, so its "
            f"silence proves nothing:\n{combined}"
        )
    return proc.stdout


@pytest.mark.parametrize("target", _PHONE_TARGETS)
@pytest.mark.parametrize("crate", _DESKTOP_KEYRING_CRATES)
def test_phone_target_does_not_resolve_the_freedesktop_keyring(crate, target):
    out = _reverse_tree(crate, target)
    assert crate not in out, (
        f"`{crate}` is back in the {target} dependency graph. A phone build must "
        f"never carry the freedesktop D-Bus credential stack: apple phones own "
        f"their secrets in the Swift-side Keychain and android in its own "
        f"keystore, and on apple phones this stack does not even compile — it is "
        f"what broke `just apple-ffi` on 2026-08-12.\n\n"
        f"Most likely cause: a new dependency edge reached "
        f"`fauna-credential-store` (or another crate pulling a desktop keyring) "
        f"from a module gated `cfg(any(unix, windows))` or similar — phones ARE "
        f"`unix`. Gate the arm POSITIVELY (name the desktop OSes) rather than by "
        f"excluding macOS/Windows.\n\nResolved via:\n{out}"
    )


@pytest.mark.parametrize("crate", _DESKTOP_KEYRING_CRATES)
def test_the_desktop_target_still_resolves_it(crate):
    """Positive control — see `_DESKTOP_CONTROL`."""
    out = _reverse_tree(crate, _DESKTOP_CONTROL)
    assert crate in out, (
        f"`{crate}` no longer resolves for {_DESKTOP_CONTROL} either, so the "
        f"phone assertions above are passing vacuously and are guarding nothing. "
        f"If the linux client genuinely dropped its Secret Service arm this "
        f"control needs rewriting against whatever replaced it — do not simply "
        f"delete it."
    )
