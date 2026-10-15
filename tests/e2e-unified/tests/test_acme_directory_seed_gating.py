"""Convention 15, the ACME directory seed: `FAUNA_E2E_ACME_*` must be compiled
out of anything that ships.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The ACME directory
seed. The seed redirects the client's DNS-01 orders — manual and managed — from
the CA constant (`tls-certificates.md` § ACME settings) to the harness's pebble,
and trusts pebble's root for that one client — so a seed that reached a release
artifact would be a CA redirect on the path that issues the nest's certificate.
Seven halves, each a false green alone:

  1. the module holding the variables is declared under the test-capable gate;
  2. the machine builder's install has a same-signature production twin;
  3. a plaintext directory is refused even under the gate;
  4. the Rust and harness spellings of the three variables agree (and the
     retired records-directory variable is named nowhere);
  5. the managed order consults the seam only in a gated arm;
  6. the provider fake's mirror into pebble's name server is called only from
     the gated sentinel decorator, and the RFC 2136 UPDATE sender it calls
     (`fauna_core::authoritative_dns::send_txt_update`) is gated too;
  7. each app's `auto_renew_wake` poke (tui, linux, android), which drives the
     tick to that order, is compiled out of a release build.

The shipping-dep-line half (no `[dependencies]` line turning
`fauna-client-dns/test-helpers` on) is the general scan in
`test_test_helpers_dep_line_gating.py`. Pure text analysis — no build.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_LIB = _REPO / "libs" / "fauna-client-dns" / "src" / "lib.rs"
_SEAM = _REPO / "libs" / "fauna-client-dns" / "src" / "e2e_acme_seam.rs"
_HARNESS = _REPO / "tests" / "e2e-unified" / "helpers" / "acme_pebble.py"
_GATE = 'any(test, debug_assertions, feature = "test-helpers")'
_VARS = (
    "FAUNA_E2E_ACME_DIRECTORY_URL",
    "FAUNA_E2E_ACME_CA_PEM",
    "FAUNA_E2E_ACME_NAMESERVER",
)
#: Retired 2026-10-14 (*The name server's reach*): the managed mirror is an
#: UPDATE to the name server, not a write into a host directory. Removed, not
#: aliased — spelled in pieces so this file is not itself a reader.
_RETIRED = "FAUNA_E2E_ACME_" + "RECORDS_DIR"
_CORE_DNS = _REPO / "libs" / "fauna-core" / "src" / "authoritative_dns.rs"


def test_the_seed_module_is_declared_under_the_test_capable_gate():
    lib = _LIB.read_text(encoding="utf-8")
    m = re.search(r"#\[cfg\(all\(\s*not\(target_arch = \"wasm32\"\),\s*(.*?)\s*\)\)\]\s*pub mod e2e_acme_seam;", lib, re.S)
    assert m, "lib.rs no longer declares `pub mod e2e_acme_seam;` under a cfg(all(not(wasm32), …)) gate"
    assert m.group(1).rstrip(",") == _GATE, (
        f"the e2e ACME seed module's gate changed to {m.group(1)!r}; it must stay {_GATE!r} "
        "so a release build neither reads nor names FAUNA_E2E_ACME_*"
    )
    for var in _VARS:
        assert var not in lib, f"lib.rs names {var} outside the gated module"


def test_the_builder_install_has_a_production_twin():
    lib = _LIB.read_text(encoding="utf-8")
    on = f"#[cfg({_GATE})]\n    fn maybe_seed_acme_directory("
    off = f"#[cfg(not({_GATE}))]\n    fn maybe_seed_acme_directory("
    assert on in lib and off in lib, (
        "the native builder's `maybe_seed_acme_directory` lost its gated arm or its "
        "same-signature production twin"
    )


def test_a_plaintext_directory_is_refused():
    seam = _SEAM.read_text(encoding="utf-8")
    assert 'starts_with("https://")' in seam, (
        "e2e_acme_seam.rs no longer refuses a non-https directory URL"
    )


def test_the_rust_and_harness_spellings_agree():
    seam = _SEAM.read_text(encoding="utf-8")
    harness = _HARNESS.read_text(encoding="utf-8")
    for var in _VARS:
        assert f'"{var}"' in seam, f"e2e_acme_seam.rs no longer names {var}"
        assert f'"{var}"' in harness, (
            f"helpers/acme_pebble.py no longer seeds {var} — the seed would be silently unset"
        )


def test_the_retired_records_directory_variable_is_named_nowhere():
    for path in (_LIB, _SEAM, _HARNESS, _REPO / "tests" / "e2e-unified" / "conftest.py"):
        assert _RETIRED not in path.read_text(encoding="utf-8"), f"{path} still names {_RETIRED}"
    android = _REPO / "apps" / "fauna-android" / "app" / "src"
    for kt in android.rglob("*.kt"):
        assert _RETIRED not in kt.read_text(encoding="utf-8"), f"{kt} still names {_RETIRED}"


def test_the_managed_order_reaches_the_seed_only_under_the_gate():
    """The managed `IssueCert` path (`obtain_managed_cert`, which the auto-renew
    tick drives) consults the seam in a gated arm only — its production branch
    orders from the CA constant through `obtain_certificate_dns01`."""
    lib = _LIB.read_text(encoding="utf-8")
    body = lib.split("async fn obtain_managed_cert(", 1)[1].split("\n    }\n", 1)[0]
    gated = (
        "#[cfg(all(\n"
        '            not(target_arch = "wasm32"),\n'
        f"            {_GATE}\n"
        "        ))]\n"
        "        if let Some(seam) = &self.acme_order_test_seam {"
    )
    assert gated in body, "obtain_managed_cert's seam arm lost its test-capable gate"
    assert body.count("acme_order_test_seam") == 1, (
        "obtain_managed_cert reads the seam outside its one gated arm"
    )
    assert "obtain_certificate_dns01(" in body.split(gated, 1)[1], (
        "obtain_managed_cert lost its production order through the CA constant"
    )


def test_the_fake_provider_mirrors_into_the_name_server_only_from_the_gated_module():
    """The provider fake's write into pebble's name server is the seed module's
    `mirror_fake_txt` (gated with the module), called only from the sentinel
    decorator, itself compiled out of a release build."""
    lib = _LIB.read_text(encoding="utf-8")
    calls = lib.count("e2e_acme_seam::mirror_fake_txt(")
    assert calls == 2, f"expected the publish + teardown mirror calls, found {calls}"
    sentinel = lib.split("impl DnsProviderSeam for SentinelDnsProvider {", 1)
    assert len(sentinel) == 2, "the sentinel decorator's impl moved"
    head = sentinel[0].rsplit("\n\n", 1)[-1]
    assert f"#[cfg({_GATE})]" in head, "the sentinel decorator's impl lost its gate"
    assert sentinel[1].count("e2e_acme_seam::mirror_fake_txt(") == 2, (
        "a mirror call sits outside the sentinel decorator"
    )
    seam = _SEAM.read_text(encoding="utf-8")
    assert seam.count("send_txt_update(") == 1, "the mirror no longer sends exactly one UPDATE call site"
    core = _CORE_DNS.read_text(encoding="utf-8")
    assert f"#[cfg({_GATE})]\npub async fn send_txt_update(" in core, (
        "fauna_core's RFC 2136 UPDATE sender lost its test-capable gate"
    )
    assert core.count("send_txt_update(") == 2, (
        "send_txt_update is called inside fauna-core beyond its own unit test"
    )


_TUI_WAKE = _REPO / "apps" / "fauna-tui" / "src" / "loop_wake.rs"
_TUI_GATE = 'any(test, debug_assertions, feature = "e2e-agent")'


def test_the_tui_auto_renew_wake_is_compiled_out_of_a_release_build():
    """The tick's poke (`auto_renew_wake`) races the wait only in a test-capable
    tui build: the `LoopWake` notifier, its `wake`, and the raced arm of
    `sleep` are gated, and a release build's `sleep` is the bare clock."""
    src = _TUI_WAKE.read_text(encoding="utf-8")
    assert f"#[cfg({_TUI_GATE})]\n    pub(crate) notify:" in src, "LoopWake's notifier lost its gate"
    assert f"#[cfg({_TUI_GATE})]\n    pub fn wake(" in src, "LoopWake::wake lost its gate"
    assert f"#[cfg({_TUI_GATE})]\n        tokio::select!" in src, "the raced wait lost its gate"
    assert f"#[cfg(not({_TUI_GATE}))]\n        tokio::time::sleep(period).await;" in src, (
        "LoopWake::sleep lost its release-build twin"
    )


_LINUX_WAKE = _REPO / "apps" / "fauna-linux" / "src" / "loop_wake.rs"
_LINUX_CLIENT = _REPO / "apps" / "fauna-linux" / "src" / "client.rs"


def test_the_linux_auto_renew_wake_is_compiled_out_of_a_release_build():
    """linux's poke is tui's shape: the `LoopWake` notifier, its `wake`, and the
    raced arm of `sleep` are gated, a release build's `sleep` is the bare clock,
    and the client's `wake_auto_renew` (the agent arm's one door) is gated too."""
    src = _LINUX_WAKE.read_text(encoding="utf-8")
    assert f"#[cfg({_TUI_GATE})]\n    notify:" in src, "linux LoopWake's notifier lost its gate"
    assert f"#[cfg({_TUI_GATE})]\n    pub fn wake(" in src, "linux LoopWake::wake lost its gate"
    assert f"#[cfg({_TUI_GATE})]\n        tokio::select!" in src, "linux's raced wait lost its gate"
    assert f"#[cfg(not({_TUI_GATE}))]\n        tokio::time::sleep(period).await;" in src, (
        "linux LoopWake::sleep lost its release-build twin"
    )
    client = _LINUX_CLIENT.read_text(encoding="utf-8")
    assert f"#[cfg({_TUI_GATE})]\n    pub fn wake_auto_renew(" in client, (
        "FaunaClient::wake_auto_renew lost its gate"
    )


_ANDROID = _REPO / "apps" / "fauna-android" / "app" / "src"


def test_the_android_auto_renew_wake_is_compiled_out_of_a_release_build():
    """android's poke lives in the debug source set (`AutoRenewWake.kt`); the
    production loop reaches it only through `TestAgent.autoRenewWaiter`, whose
    shipping twin (`src/noAgent`, taken by every shipping flavor) is a bare
    `delay` — so a release APK carries no notifier."""
    assert (_ANDROID / "debug" / "java" / "com" / "fauna" / "app" / "testing" / "AutoRenewWake.kt").is_file()
    for shipping in ("main", "noAgent", "noKids", "kids"):
        for kt in (_ANDROID / shipping).rglob("*.kt"):
            assert "AutoRenewWake" not in kt.read_text(encoding="utf-8"), (
                f"{kt} names the debug-only AutoRenewWake"
            )
    twin = (_ANDROID / "noAgent" / "java" / "com" / "fauna" / "app" / "testing" / "TestAgent.kt").read_text(
        encoding="utf-8"
    )
    assert (
        "suspend fun autoRenewWaiter(): suspend (Long) -> Unit = { ms -> kotlinx.coroutines.delay(ms) }"
        in twin
    ), "the shipping twin's auto-renew wait is no longer the bare clock"
    loop = (_ANDROID / "noKids" / "java" / "com" / "fauna" / "app" / "core" / "KidsExcisedApi.kt").read_text(
        encoding="utf-8"
    )
    assert "TestAgent.autoRenewWaiter()" in loop, "the auto-renew loop no longer takes its wait from the twin"
