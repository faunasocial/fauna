"""`mail-bridge-ffi` must own a FLAVOR-PRIVATE `libfauna_ffi` slot — the shared
`target/release/` one is written by every host build of fauna-ffi in the
workspace.

`test_mac_host_tree_is_shared.py` already names this collision and pins one half
of it: mac's host recipes build implicit-host (the right call — asking for the
host's own triple buys a second artifact tree and no sharing) and route their own
artifact out through `--artifact-dir`. But that isolation is ONE-WAY. It stops
apple's `.a` from being clobbered; it does nothing about apple's build clobbering
the shared *cdylib* slot — and `mail-bridge-ffi` read exactly that slot, so the
other side of the collision it names was never isolated at all.

The failure was silent and fail-dangerous, and it is measured (2026-08-29):

  * `mail-bridge-ffi`'s cargo step is SOURCE-keyed (`--stamp`), so on a tree with
    no `libs/` change it reads fresh and cargo is skipped — leaving whatever
    foreign flavor last wrote the shared slot in place.
  * its bindgen gate is ARTIFACT-MTIME-keyed (`--source "$LIB"`), so the foreign
    write makes it stale and it regenerates from the WRONG cdylib.
  * running the pinned `uniffi-bindgen-go` over a default-features
    `libfauna_ffi.so` emits **26** namespaces against this flavor's **10**, and
    `sync-generated-tree.py` writes all 26 into the committed
    `libs/fauna-mail-go` — the 16 stray crate directories and ~75 k-line diff a
    macOS installer build produced from a clean tree that touched no `libs/`
    source.

The same slot is the cgo link path, the cgo rpath, the runtime
`LD_LIBRARY_PATH`, and what the macOS installer stages into the shipped bridge
payload — where the leak is a build-FLAVOR leak (the app flavor carries
`payments`/`zaps` and the whole `store-safe` app surface; it is a strict superset
of the labeler build, so it loads and runs). So this pins every consumer, not
just the bindgen: an isolated bindgen with a shared link path would still load a
foreign flavor at run time, because `LD_LIBRARY_PATH` takes precedence over the
binary's RUNPATH.

Structural/text analysis only, and deliberately so: the real-build verification
costs a release build of fauna-ffi plus a second one to switch flavors back, and
that thrash is exactly what the per-checkout cargo-target quota punishes. It was
run by hand once, at the change that introduced the slot; this file is what keeps
a future session from "simplifying" the private slot back into the shared one.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_JUSTFILE = _REPO / "justfile"

# The shared cargo output slot, in the spellings the justfile uses for it.
_SHARED_SLOT = re.compile(r"\$TARGET_DIR/release(?![\w/])")


def _recipe_body(text: str, name: str) -> str:
    """The indented body of one justfile recipe — `name:` or a parametrized
    `name arg *ARGS:` declaration alike. Duplicated from
    `test_mac_host_tree_is_shared.py` per this suite's no-cross-import-between-
    test-files convention (small helpers get duplicated, not imported)."""
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
    """Recipe body with comment lines dropped — the comments *explain* the
    shared slot at length, so a naive substring search over the raw body can
    never go red."""
    return "\n".join(
        ln for ln in body.splitlines() if not ln.lstrip().startswith("#")
    )


def _justfile() -> str:
    return _JUSTFILE.read_text(encoding="utf-8")


#: What a `$(cat "$FILE")` read-back of `just mail-ffi-slot`'s captured
#: stdout inlines to, for `_resolve`'s purposes — it must contain the literal
#: substring "mail-ffi-slot" so the private-slot assertions still see it.
_MAIL_FFI_SLOT_OUTPUT = "mail-ffi-slot-output"


def _fold_streamed_mail_ffi_slot_capture(code: str) -> str:
    """Inline the file-streamed `just mail-ffi-slot` idiom so `_resolve` can
    see through it.

    `test_no_unbounded_shell_capture.py` bans a raw `$(just mail-ffi-slot …)`
    capture, so the producer recipes stream it through a temp file instead —
    `FILE="$(mktemp)"; just mail-ffi-slot … >"$FILE"; X="…/$(cat "$FILE")"`. That is still, textually, a derivation from `mail-ffi-slot`;
    it is only opaque to a naive `NAME="value"` scan because the read-back
    embeds its own quotes. Find the file variable `just mail-ffi-slot`
    writes to and replace every `$(cat "$FILE")` (or unquoted `$(cat $FILE)`)
    read-back with a marker carrying the same substring a direct assignment
    would have, before `_resolve` ever tries to parse assignments out of the
    line.
    """
    for m in re.finditer(
        r"^\s*just mail-ffi-slot\b[^\n]*>\s*\"?\$(\w+)\"?\s*$", code, re.M
    ):
        file_var = m.group(1)
        code = re.sub(
            rf'\$\(cat\s+"?\${re.escape(file_var)}"?\)',
            _MAIL_FFI_SLOT_OUTPUT,
            code,
        )
    return code


def _resolve(code: str, var: str) -> str:
    """The value of shell variable `var` in `code`, with its own `$OTHER`
    references expanded from the assignments around it.

    The recipes derive the path in steps (`FFI_DIR="$TARGET_DIR/{{...}}"`, then
    `LIB="$FFI_DIR/$LIBNAME"`), so a literal substring check on the last
    assignment alone would pass for a `LIB` pointing anywhere at all. Resolving
    is what makes "does this end up in the shared slot?" an honest question.

    `$TARGET_DIR` is deliberately left unexpanded: it is the base every slot
    hangs off, not a discriminator, and its own assignment embeds a quoted
    `cargo metadata` pipeline that no simple scan should try to parse.
    """
    code = _fold_streamed_mail_ffi_slot_capture(code)
    assigns = {
        name: value
        for name, value in re.findall(r'^\s*(\w+)="([^"]*)"', code, re.M)
        if name != "TARGET_DIR"
    }
    assert var in assigns, f"{var} is not assigned in this recipe"
    value = assigns[var]
    for _ in range(10):  # bounded: these chains are two or three links deep
        expanded = re.sub(
            r"\$\{(\w+)[^}]*\}|\$(\w+)",
            lambda m: assigns.get(m.group(1) or m.group(2), m.group(0)),
            value,
        )
        if expanded == value:
            break
        value = expanded
    return value


# ── the slot itself ──────────────────────────────────────────────────────────

def _slot_recipe_body() -> str:
    m = re.search(r'^mail-ffi-slot profile="release":\n((?:^[ \t].*\n?)*)',
                   _justfile(), re.M)
    assert m, (
        "justfile has no `mail-ffi-slot` recipe — the flavor-private "
        "libfauna_ffi slot every mail-bridge consumer derives from. (Not a "
        "bare `mail_ffi_slot` variable: a `:=` variable is evaluated once per "
        "`just` PROCESS, before any recipe parameter exists, so it cannot "
        "carry a caller's `profile` — installers/macos.md § Size & build "
        "profile.)"
    )
    return m.group(1)


def test_the_slot_recipe_is_profile_parameterized_and_a_subdirectory():
    """`mail-ffi-slot` takes the cargo profile as a parameter (so it can be
    the SAME derivation for every caller, dist-shipping build included) and
    every branch names a real subdirectory of the profile dir. A bare
    `echo "{{profile}}"` (no subdirectory) would satisfy every other
    assertion in this file while reintroducing the exact collision one level
    up."""
    body = _slot_recipe_body()
    generic = re.search(r'echo "\{\{profile\}\}/([^"]+)"', body)
    assert generic, (
        'mail-ffi-slot\'s general-profile branch does not echo a '
        '`{{profile}}/<subdirectory>` path — must be a subdirectory UNDER '
        'the profile dir, not the profile dir itself: the bare profile slot '
        'is written by every host build of fauna-ffi in the workspace.'
    )
    dev = re.search(r'dev\) echo "debug/([^"]+)"', body)
    assert dev, (
        "mail-ffi-slot has no `dev` -> `debug` PROFILE_DIR mapping (cargo's "
        "own dir-naming exception — mirrors `_apple-ffi-host-flavor` / "
        "`_windows-go-cgo-env`'s identical case statement)"
    )
    assert dev.group(1) == generic.group(1), (
        f'mail-ffi-slot names a different subdirectory for `dev` '
        f'("{dev.group(1)}") than for every other profile '
        f'("{generic.group(1)}") — the flavor suffix must be identical '
        f"across profiles, only the profile-dir prefix varies."
    )


def test_the_slot_path_carries_the_feature_flavor():
    """The flavor belongs in the PATH, not only in a comment. A second
    mail-bridge flavor must land in its own directory rather than silently
    sharing this one — the same reason windows keys its private slot by
    `${FEATURES:-production}` and apple's by `${FEATURES:-production}`."""
    body = _slot_recipe_body()
    generic = re.search(r'echo "\{\{profile\}\}/([^"]+)"', body)
    assert generic
    features = re.search(
        r"--no-default-features --features (\S+)",
        _code(_recipe_body(_justfile(), "mail-bridge-ffi")),
    )
    assert features, "mail-bridge-ffi no longer names its feature flavor"
    assert features.group(1) in generic.group(1), (
        f'mail-ffi-slot\'s subdirectory ("{generic.group(1)}") does not name '
        f'the flavor it holds ("{features.group(1)}"). Two flavors sharing '
        f"one private slot is the shared-slot bug again, one level down."
    )


# ── the producer ─────────────────────────────────────────────────────────────

def test_mail_bridge_ffi_copies_into_the_private_slot_inside_the_gate():
    """The copy must be part of the gated build command, not a step of its own.

    A separate `--source <shared> --target <private>` gate would re-copy
    whenever ANOTHER recipe touched the shared slot — importing exactly the
    wrong cdylib it exists to keep out. Inside the command, the private slot can
    only ever be written by a build this recipe just ran. (Same shape, and same
    reasoning, as `_windows-ffi-flavor`'s `&& cp "$CARGO_OUT_DIR/fauna_ffi.dll"
    "$FFI_DLL"`.)"""
    code = _code(_recipe_body(_justfile(), "mail-bridge-ffi"))
    build = re.search(r"cargo build [^\n]*-p fauna-ffi[^\n]*", code)
    assert build, "mail-bridge-ffi no longer builds fauna-ffi"
    assert "cp " in build.group(0), (
        "mail-bridge-ffi's cargo build no longer copies its cdylib into the "
        "flavor-private slot in the SAME gated command. A copy outside the gate "
        "re-imports the shared slot's current contents whenever a foreign build "
        "touches it."
    )


def test_the_bindgen_reads_the_private_slot_not_the_shared_one():
    """The gate's `--source` and the bindgen's input are the same private copy.

    This is the assertion that would have caught the original bug: the bindgen
    gate was `--source "$LIB"` with `$LIB` derived straight from
    `$TARGET_DIR/release/`."""
    code = _code(_recipe_body(_justfile(), "mail-bridge-ffi"))
    lib = _resolve(code, "LIB")
    assert not _SHARED_SLOT.search(lib), (
        f'mail-bridge-ffi: LIB resolves to "{lib}" — the SHARED cargo output '
        f"slot. Every host build of fauna-ffi writes it — mac's "
        f"`apple-ffi-host release`, any `cargo build --release -p fauna-ffi` — "
        f"so the bindgen would emit whichever feature flavor built last."
    )
    assert "mail-ffi-slot" in lib, (
        f'mail-bridge-ffi: LIB resolves to "{lib}", which does not derive from '
        f"the shared `mail-ffi-slot` recipe — it is respelling the path"
    )


def test_the_bindgen_helper_takes_its_library_from_the_caller():
    """`_mail-bridge-ffi-bindgen` must not re-derive the path. It used to probe
    `$TARGET_DIR/release/libfauna_ffi.{so,dylib}` / `fauna_ffi.dll` in turn —
    a second derivation of the slot, free to drift from the caller's."""
    text = _justfile()
    assert re.search(r"^_mail-bridge-ffi-bindgen \w+:", text, re.M), (
        "_mail-bridge-ffi-bindgen takes no library argument — it is deriving "
        "the cdylib path a second time, independently of the recipe that owns "
        "the slot"
    )
    code = _code(_recipe_body(text, "_mail-bridge-ffi-bindgen"))
    assert not _SHARED_SLOT.search(code), (
        "_mail-bridge-ffi-bindgen still names the shared "
        "`$TARGET_DIR/release` slot"
    )


def test_the_freshness_gate_reads_the_private_slot_too():
    """`mail-bridge-ffi-check` builds ungated — so it always restored the
    labeler flavor to the shared slot first, read 10 namespaces there, and
    disagreed with the build's own 26 in the OPPOSITE direction. That is what
    made the symptom read as "the committed tree is stale". It reads the private
    copy for the same reason, and to stay correct if a sibling's build lands
    between its cargo step and its bindgen."""
    code = _code(_recipe_body(_justfile(), "mail-bridge-ffi-check"))
    lib = _resolve(code, "LIB")
    assert not _SHARED_SLOT.search(lib), (
        f'mail-bridge-ffi-check: LIB resolves to "{lib}" — the shared slot'
    )
    assert "mail-ffi-slot" in lib, (
        f'mail-bridge-ffi-check: LIB resolves to "{lib}", which does not derive '
        f"from the shared `mail-ffi-slot` recipe"
    )


# ── the consumers ────────────────────────────────────────────────────────────

def test_no_cgo_recipe_links_against_the_shared_slot():
    """An isolated bindgen with a shared link path still ships the bug: the Go
    bridge would LINK against, and at run time LOAD, whichever flavor built
    last. `LD_LIBRARY_PATH` beats the binary's RUNPATH, so the runtime half
    matters even where the link half is right."""
    text = _justfile()
    offenders = [
        (i + 1, ln.strip())
        for i, ln in enumerate(text.splitlines())
        if ("CGO_LDFLAGS" in ln or "LD_LIBRARY_PATH" in ln)
        and _SHARED_SLOT.search(ln)
    ]
    assert not offenders, (
        "these justfile lines still point the Go bridge's cgo link / runtime "
        "search at the SHARED `$TARGET_DIR/release` slot instead of "
        "`$TARGET_DIR/$(just mail-ffi-slot ...)`:\n  "
        + "\n  ".join(f"line {n}: {ln}" for n, ln in offenders)
    )


def test_the_e2e_harness_points_at_the_private_slot():
    """`conftest.py` sets `LD_LIBRARY_PATH` for every spawned mail-bridge and
    seal-helper. Pointed at the shared slot it would override the binary's
    RUNPATH and load a foreign flavor — the one consumer where fixing the
    justfile alone is not enough."""
    conftest = (_REPO / "tests/e2e-unified/conftest.py").read_text(encoding="utf-8")
    fn = re.search(
        r"def _mail_bridge_ffi_dir\(\).*?(?=\n(?:def|@|class)\s)", conftest, re.S
    )
    assert fn, (
        "conftest.py has no `_mail_bridge_ffi_dir()` — the bridge's "
        "libfauna_ffi directory must resolve to the flavor-private slot"
    )
    assert "mail-ffi-slot" in fn.group(0), (
        "conftest.py's bridge FFI directory no longer derives from the "
        "justfile's `mail-ffi-slot` recipe — it is respelling the path, or "
        "has gone back to the shared `<cargo target>/release`"
    )


def test_the_macos_installer_stages_the_private_slot():
    """The installer runs `just mail-bridge-build "$PROFILE"`, then `just
    mac-app release "$PROFILE"` — which rebuilds fauna-ffi implicit-host with
    the app's DEFAULT features and overwrites the shared slot — and only THEN
    stages the dylib. So the .pkg shipped the app-flavored cdylib as the
    mail-bridge's FFI. Silent, because the app flavor is a strict superset of
    the labeler one."""
    build_sh = (_REPO / "installer/macos/build.sh").read_text(encoding="utf-8")
    m = re.search(r"^FFI_DYLIB=(.+)$", build_sh, re.M)
    assert m, "installer/macos/build.sh no longer defines FFI_DYLIB"
    dylib = m.group(1)
    assert not re.search(r"\$\{?TARGET_DIR\}?/release/", dylib), (
        f"installer/macos/build.sh stages FFI_DYLIB={dylib} from the SHARED "
        f"slot. `just mac-app release` runs between the bridge build and this "
        f"staging step and overwrites that slot with the app's default-features "
        f"cdylib, so the .pkg would ship the app flavor as the bridge's FFI."
    )
    slot = re.search(r"just mail-ffi-slot\b", build_sh)
    assert slot, (
        "installer/macos/build.sh must derive the slot from the justfile's "
        "`mail-ffi-slot` recipe (`just mail-ffi-slot \"$PROFILE\"`) rather "
        "than respelling the path — one home for it, or the two drift"
    )
