"""tier_1: a macOS binary or dylib a checkout links names no other checkout.

Pure in-process analysis of scripts/check_linked_paths.py over `otool`
listings, plus the build script and justfile wiring that hold the rule. No
cargo, no go, no otool. Why the rule exists (a cgo bridge loading a sibling
checkout's `libfauna_ffi.dylib` through a cached link's absolute install name):
the script's docstring and build-compile-cache.md § Cargo target dir layout
(mac), *A linked artifact names no checkout*.
"""

import importlib.util
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_SCRIPT = _REPO / "scripts" / "check_linked_paths.py"

# The listing that started it: one checkout's bridge naming another's build-dir output.
_SIBLING_L = """bins/fauna-bridges/fauna-atproto-bridge-e2e:
\t/src/checkout-b/target/release/build/fauna-ffi/9085a2bcd2534dae/out/libfauna_ffi.dylib (compatibility version 0.0.0, current version 0.0.0)
\t/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation (compatibility version 150.0.0, current version 5027.0.69)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1359.0.0)
"""
_RPATH_L = """bins/fauna-bridges/fauna-atproto-bridge-e2e:
\t@rpath/libfauna_ffi.dylib (compatibility version 0.0.0, current version 0.0.0)
\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1359.0.0)
"""
_OWN = Path("/src/checkout-a")


def _load():
    spec = importlib.util.spec_from_file_location("_check_linked_paths", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_sibling_checkout_path_is_refused():
    mod = _load()
    bad = mod.offending(mod.referenced_paths(_SIBLING_L), [_OWN / "target", _OWN])
    assert bad == [
        "/src/checkout-b/target/release/build/"
        "fauna-ffi/9085a2bcd2534dae/out/libfauna_ffi.dylib"
    ]


def test_rpath_and_system_paths_pass():
    mod = _load()
    assert mod.offending(mod.referenced_paths(_RPATH_L), [_OWN]) == []


def test_own_checkout_path_passes_and_dotdot_escape_does_not():
    mod = _load()
    own = str(_OWN / "target/release/libfauna_ffi.dylib")
    escape = str(_OWN / "target/../../checkout-b/target/release/libfauna_ffi.dylib")
    assert mod.offending([own, escape], [_OWN]) == [escape]


def test_install_name_listing_is_parsed():
    """`otool -D` prints the header and the install name alone, no version note."""
    mod = _load()
    listing = "target/release/libfauna_ffi.dylib:\n@rpath/libfauna_ffi.dylib\n"
    # `-D` does not indent its path; headers are the lines ending in ':'.
    assert mod.referenced_paths(listing) == ["@rpath/libfauna_ffi.dylib"]


def test_build_script_sets_rpath_install_name_for_apple_cdylibs():
    src = (_REPO / "libs/fauna-ffi-cdylib/build.rs").read_text(encoding="utf-8")
    assert "CARGO_CFG_TARGET_VENDOR" in src and '"apple"' in src
    assert "cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libfauna_ffi.dylib" in src


def test_recipes_run_the_check():
    justfile = (_REPO / "justfile").read_text(encoding="utf-8")
    for recipe in ("mail-bridge-ffi profile=", "_go-cgo-build cmd"):
        # A recipe body is every indented line after its header.
        lines = justfile.split(f"\n{recipe}", 1)[1].splitlines()[1:]
        body = []
        for line in lines:
            if line and not line[0].isspace():
                break
            body.append(line)
        assert any("scripts/check_linked_paths.py" in line for line in body), recipe
