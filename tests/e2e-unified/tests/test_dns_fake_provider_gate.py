"""Convention 15's `fauna-client-dns` residual: the DNS fake-provider seam
(`SentinelDnsProvider` + its native env-var switch and wasm enable hook) must
be compile-gated out of release artifacts like every other e2e seam.

`docs/goal/architecture/e2e-conventions.md` point 15, rule (a): a shared
crate's test-only seam gates its own VISIBILITY on
`#[cfg(any(test, debug_assertions, feature = "test-helpers"))]` — never on the
caller remembering not to flip a runtime switch. Before this fix,
`fauna_client_dns`'s `SentinelDnsProvider`, `fake_dns_zones_from_token`, and
the native `FAUNA_DNS_PROVIDER_FAKE` env-var read were UNGATED: they compiled
into every native release artifact (linux, and — via `fauna-ffi` — windows,
macOS, iOS, android), and the type also rode wasm's production bundle. A
`FAUNA_DNS_PROVIDER_FAKE=1` set in a user's environment would have silently
made domain-ownership verification always succeed with no real registrar
call.

Pure text analysis of the crate source and manifests — mirrors
`test_ffi_flavor_split.py`'s style for the same convention; no build.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_DNS_LIB = _REPO / "libs" / "fauna-client-dns" / "src" / "lib.rs"
_GATE = '#[cfg(any(test, debug_assertions, feature = "test-helpers"))]'


def _lib_text() -> str:
    return _DNS_LIB.read_text(encoding="utf-8")


def _gated_items(text: str) -> set[str]:
    """Item names (fn/struct/const/static) whose nearest preceding
    non-attribute, non-doc-comment line is exactly the rule-(a) gate."""
    lines = text.splitlines()
    gated = set()
    for i, line in enumerate(lines):
        if line.strip() != _GATE:
            continue
        j = i + 1
        while j < len(lines) and (
            lines[j].strip().startswith("#[") or lines[j].strip().startswith("///")
        ):
            j += 1
        if j < len(lines):
            m = re.search(r"\b(?:fn|struct|const|static)\s+(\w+)", lines[j])
            if m:
                gated.add(m.group(1))
    return gated


def test_sentinel_dns_provider_is_gated():
    gated = _gated_items(_lib_text())
    assert "SentinelDnsProvider" in gated, (
        "fauna_client_dns::SentinelDnsProvider ships unconditionally into every "
        f"release artifact — gate it behind `{_GATE}` "
        "(e2e-conventions.md convention 15, rule (a))"
    )


def test_fake_dns_zones_from_token_is_gated():
    gated = _gated_items(_lib_text())
    assert "fake_dns_zones_from_token" in gated, (
        "fauna_client_dns::fake_dns_zones_from_token ships unconditionally — "
        f"gate it behind `{_GATE}`"
    )


def test_dns_provider_fake_env_var_read_is_gated():
    text = _lib_text()
    matches = list(re.finditer(r'std::env::var_os\("FAUNA_DNS_PROVIDER_FAKE"\)', text))
    assert matches, "the native FAUNA_DNS_PROVIDER_FAKE env-var read moved or was removed"
    for m in matches:
        window = text[: m.start()].splitlines()[-15:]
        assert any(_GATE in ln for ln in window), (
            "the native FAUNA_DNS_PROVIDER_FAKE env-var read is not inside a "
            f"function gated by `{_GATE}` — it compiles into every native "
            "release artifact (linux, and via fauna-ffi: windows/macOS/iOS/android)"
        )


def test_wasm_enable_dns_fake_hook_is_gated():
    gated = _gated_items(_lib_text())
    for name in (
        "enable_dns_provider_fake_for_test",
        "dns_fake_enabled",
        "set_dns_fake_enabled",
    ):
        assert name in gated, (
            f"fauna_client_dns::{name} is only target-gated to wasm32, not to "
            f"debug/test-helpers — gate it behind `{_GATE}` too"
        )


def test_fauna_wasm_forwards_client_dns_test_helpers():
    manifest = (_REPO / "libs" / "fauna-wasm" / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"test-helpers\s*=\s*\[(.*?)\]", manifest, re.DOTALL)
    assert m, "fauna-wasm/Cargo.toml has no test-helpers feature list"
    assert "fauna-client-dns/test-helpers" in m.group(1), (
        "fauna-wasm's test-helpers feature must forward fauna-client-dns/test-helpers "
        "so wasm-core-test still wires the DNS fake provider once fauna-client-dns "
        "gates it behind its own test-helpers feature"
    )
