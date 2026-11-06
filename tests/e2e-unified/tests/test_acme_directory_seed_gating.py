"""Convention 15, the ACME directory seed: `FAUNA_E2E_ACME_*` must be compiled
out of anything that ships.

`docs/goal/architecture/e2e-gated-seeds.md` § The ACME directory seed. The
seed redirects the client's DNS-01 orders — manual and managed — from
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
  7. each app's `auto_renew_wake` poke (tui, linux, android, web, macos, ios),
     which drives the tick to that order, is compiled out of a release build
     (web: of a production `vite build`, behind `__FAUNA_E2E_AUTOMATION__`;
     macos + ios: the shared FaunaKit cadence's `#if DEBUG`);
  8. web's reading (*The web order*): the wasm seam holds the directory alone,
     under the gate, read from `localStorage` under `fauna-wasm`'s
     `test-helpers`; the nest reads the name server and serves the challenge
     mirror kind under `test-hooks` alone; the CA and name-server variables
     reach no wasm reader and the web driver writes neither.

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
    """Both targets compile the module (web's reading is its directory parser),
    under the one test-capable gate."""
    lib = _LIB.read_text(encoding="utf-8")
    m = re.search(r"#\[cfg\(([^\n]*)\)\]\npub mod e2e_acme_seam;", lib)
    assert m, "lib.rs no longer declares `pub mod e2e_acme_seam;` under a cfg gate"
    assert m.group(1) == _GATE, (
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
    web_gated = (
        "#[cfg(all(\n"
        '            target_arch = "wasm32",\n'
        f"            {_GATE}\n"
        "        ))]\n"
        "        let order_cfg = match &self.acme_order_test_seam {"
    )
    assert web_gated in body, "obtain_managed_cert's web directory arm lost its test-capable gate"
    assert body.count("acme_order_test_seam") == 2, (
        "obtain_managed_cert reads the seam outside its two gated arms (native, web)"
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
    assert seam.count("send_txt_updates_by_zone(") == 1, (
        "the mirror no longer sends through exactly one shared per-zone call site"
    )
    core = _CORE_DNS.read_text(encoding="utf-8")
    for fn in ("send_txt_update(", "send_txt_updates_by_zone("):
        assert f"#[cfg({_GATE})]\npub async fn {fn}" in core, (
            f"fauna_core's RFC 2136 UPDATE sender `{fn}` lost its test-capable gate"
        )
    assert core.count("send_txt_update(") == 3, (
        "send_txt_update is called inside fauna-core beyond the per-zone sender and its own unit test"
    )
    # Web's mirror: the sentinel's nest-relayed call, inside the gated
    # decorator, the one caller of the nest seam's mirror method.
    assert sentinel[1].count("self.mirror_via_nest(") == 2, (
        "the web publish + teardown mirror calls left the sentinel decorator"
    )
    assert lib.count(".e2e_mirror_txt(published, withdrawn)") == 2, (
        "the nest seam's mirror is called from somewhere other than the sentinel's "
        "`mirror_via_nest` and the wasm nest's own forward"
    )


_WASM_DNS = _REPO / "libs" / "fauna-wasm" / "src" / "mail_admin.rs"
_NEST_DNS = _REPO / "bins" / "fauna-nest" / "src" / "dns_handlers.rs"
_NEST_CARGO = _REPO / "bins" / "fauna-nest" / "Cargo.toml"
_NEST_ALLOW = _REPO / "bins" / "fauna-nest" / "src" / "bridge_method_allowlist.rs"
_WEB_DRIVER = _REPO / "tests" / "e2e-unified" / "drivers" / "web.py"
_CONFTEST = _REPO / "tests" / "e2e-unified" / "conftest.py"
_WEB_GATE = (
    "#[cfg(all(\n"
    '    target_arch = "wasm32",\n'
    f"    {_GATE}\n"
    "))]\n"
)


def test_the_web_seam_holds_the_directory_alone_under_the_gate():
    """*The web order*, leg 1: the wasm twin of the seam is gated like the
    native one and carries nothing but the directory."""
    lib = _LIB.read_text(encoding="utf-8")
    head = f"{_WEB_GATE}pub struct AcmeOrderTestSeam {{"
    assert head in lib, "the wasm AcmeOrderTestSeam lost its test-capable gate"
    body = lib.split(head, 1)[1].split("\n}\n", 1)[0]
    fields = re.findall(r"^\s*pub (\w+):", body, re.M)
    assert fields == ["directory_url"], (
        f"the web seam carries {fields}; it is the directory alone — the browser's "
        "fetch is the client and the gate is the nest-relayed probe"
    )
    seam = _SEAM.read_text(encoding="utf-8")
    web = seam.split("pub fn web_seam_from_directory(", 1)[1].split("\n}\n", 1)[0]
    assert "directory_from_value(" in web, "the web reading lost the shared https:// refusal"


def test_the_wasm_reading_is_the_test_helpers_constructor_alone():
    """`fauna-wasm` reads the directory from `localStorage` under its
    `test-helpers` feature, in the credentialed constructor — and names no CA or
    name-server variable."""
    wasm = _WASM_DNS.read_text(encoding="utf-8")
    assert '#[cfg(feature = "test-helpers")]\nfn e2e_acme_directory_seed()' in wasm, (
        "fauna-wasm's localStorage read of the ACME directory lost its test-helpers gate"
    )
    install = '#[cfg(feature = "test-helpers")]\n        let machine = match e2e_acme_directory_seed()'
    assert install in wasm, "the credentialed constructor's seam install lost its test-helpers gate"
    assert wasm.count("with_acme_order_test_seam(") == 1, "the web seam is installed outside the one door"
    assert "E2E_ACME_DIRECTORY_ENV" in wasm, "fauna-wasm no longer reads the directory under the variable's name"
    for absent in ("E2E_ACME_CA_PEM_ENV", "E2E_ACME_NAMESERVER_ENV", "FAUNA_E2E_ACME_CA_PEM", "FAUNA_E2E_ACME_NAMESERVER"):
        assert absent not in wasm, f"fauna-wasm names {absent}, which has no wasm reader"


def test_the_nest_reads_the_name_server_and_serves_the_mirror_under_test_hooks_alone():
    """*The web order*, legs 2 and 3: the nest's environment read and the
    mirror kind's handler and registration are `test-hooks` code; any other
    build passes no override."""
    nest = _NEST_DNS.read_text(encoding="utf-8")
    reader = nest.split("fn e2e_acme_nameserver() -> Option<std::net::SocketAddr> {", 1)
    assert len(reader) == 2, "the nest's name-server reading moved"
    body = reader[1].split("\n}\n", 1)[0]
    on, off = body.split('#[cfg(not(feature = "test-hooks"))]', 1)
    assert '#[cfg(feature = "test-hooks")]' in on and "std::env::var(E2E_ACME_NAMESERVER_ENV)" in on, (
        "the nest's environment read left its test-hooks arm"
    )
    assert "None" in off and "env::var" not in off, "the nest's production arm reads the environment"
    assert nest.count("std::env::var(") == 1, "dns_handlers reads the environment outside the gated reader"
    assert '#[cfg(feature = "test-hooks")]\nfn e2e_mirror_txt_handler()' in nest, (
        "the mirror handler lost its test-hooks gate"
    )
    reg = nest.split('#[cfg(feature = "test-hooks")]\n    b.add(\n        "fauna.dns.e2e_mirror_txt"', 1)
    assert len(reg) == 2, "the mirror kind is registered outside a test-hooks gate"
    assert nest.count('"fauna.dns.e2e_mirror_txt"') == 2, (
        "the mirror kind is named beyond its gated registration and its permission check"
    )
    mirror = nest.split("fn e2e_mirror_txt_handler() -> RpcHandler {", 1)[1].split("\n}\n", 1)[0]
    assert "let name_server = e2e_acme_nameserver();" in mirror and "send_txt_updates_by_zone(\n                server," in mirror, (
        "the mirror no longer sends to the name server the nest read itself"
    )
    cargo = _NEST_CARGO.read_text(encoding="utf-8")
    hooks = cargo.split("\ntest-hooks = [", 1)[1].split("]", 1)[0]
    assert '"fauna-core/test-helpers"' in hooks, (
        "a release-profile test-hooks nest no longer forwards fauna-core/test-helpers, "
        "so the mirror's UPDATE sender is missing where the kind is served"
    )
    allow = _NEST_ALLOW.read_text(encoding="utf-8")
    assert '"fauna.dns.e2e_mirror_txt" => matches!(class, Admin),' in allow, (
        "the mirror kind's allowlist arm is not Admin alone"
    )


def test_the_web_and_nest_spellings_agree():
    """The variable's four spellings beyond the native seam: the nest's reader,
    the harness's nest-seed writer, and the web driver's `localStorage` key —
    and the web driver writes neither of the variables web does not read."""
    nest = _NEST_DNS.read_text(encoding="utf-8")
    assert 'E2E_ACME_NAMESERVER_ENV: &str = "FAUNA_E2E_ACME_NAMESERVER"' in nest
    conftest = _CONFTEST.read_text(encoding="utf-8")
    assert '_NEST_LAUNCH_SEED_KEYS = ("FAUNA_E2E_ACME_NAMESERVER",)' in conftest, (
        "the harness's nest-seed writer no longer carries the name server"
    )
    web_launch = conftest.split('elif app_name == "web":', 1)[1].split("return {", 1)[0]
    assert "_apply_acme_pebble_env(env, request)" in web_launch, (
        "the web launch config no longer carries the ACME seed, so the driver writes no directory"
    )
    driver = _WEB_DRIVER.read_text(encoding="utf-8")
    assert '_ACME_DIRECTORY_KEY = "FAUNA_E2E_ACME_DIRECTORY_URL"' in driver
    assert driver.count("self._write_acme_directory_seed()") == 2, (
        "the web driver writes the directory at launch and again after serve_from"
    )
    for absent in ("FAUNA_E2E_ACME_CA_PEM", "FAUNA_E2E_ACME_NAMESERVER"):
        assert absent not in driver, f"the web driver names {absent}, which web never reads"


_CORE_WAKE = _REPO / "libs" / "fauna-core" / "src" / "loop_wake.rs"
_CORE_GATE = 'any(debug_assertions, feature = "e2e-agent")'
_TUI_GATE = 'any(test, debug_assertions, feature = "e2e-agent")'


def _assert_the_shared_wake_is_gated() -> None:
    """`LoopWake` lives once in `fauna-core` (tui and linux both consume it): the
    notifier, its `wake`, and the raced arm of `sleep` are gated, and a release
    build's `sleep` is the bare clock."""
    src = _CORE_WAKE.read_text(encoding="utf-8")
    assert f"#[cfg({_CORE_GATE})]\n    notify:" in src, "LoopWake's notifier lost its gate"
    assert f"#[cfg({_CORE_GATE})]\n    pub fn wake(" in src, "LoopWake::wake lost its gate"
    assert f"#[cfg({_CORE_GATE})]\n        tokio::select!" in src, "the raced wait lost its gate"
    assert f"#[cfg(not({_CORE_GATE}))]\n        tokio::time::sleep(period).await;" in src, (
        "LoopWake::sleep lost its release-build twin"
    )


def _assert_the_app_forwards_the_seam(app: str) -> None:
    """A release-profile e2e build of the app keeps the seam only because its own
    `e2e-agent` feature forwards `fauna-core/e2e-agent`."""
    manifest = (_REPO / "apps" / app / "Cargo.toml").read_text(encoding="utf-8")
    e2e_agent = manifest.split("\ne2e-agent = [", 1)[1].split("\n]", 1)[0]
    assert '"fauna-core/e2e-agent"' in e2e_agent, (
        f"{app}'s e2e-agent feature no longer forwards fauna-core's, so a release e2e build loses the wake"
    )


def test_the_tui_auto_renew_wake_is_compiled_out_of_a_release_build():
    """The tick's poke (`auto_renew_wake`) races the wait only in a test-capable
    tui build: the shared `LoopWake` is gated, and tui forwards the gate."""
    _assert_the_shared_wake_is_gated()
    _assert_the_app_forwards_the_seam("fauna-tui")


_LINUX_CLIENT = _REPO / "apps" / "fauna-linux" / "src" / "client.rs"


def test_the_linux_auto_renew_wake_is_compiled_out_of_a_release_build():
    """linux's poke is tui's shape: the shared `LoopWake` is gated, linux forwards
    the gate, and the client's `wake_auto_renew` (the agent arm's one door) is
    gated too."""
    _assert_the_shared_wake_is_gated()
    _assert_the_app_forwards_the_seam("fauna-linux")
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


_WEB_LIB = _REPO / "apps" / "fauna-web" / "src" / "lib"
_WEB_SRC = _REPO / "apps" / "fauna-web" / "src"


def test_the_web_auto_renew_wake_is_compiled_out_of_a_production_build():
    """web's poke (`auto_renew_wake`, and its `__fauna_autoRenewWakeForTest`
    hook) races the tick's wait only under `__FAUNA_E2E_AUTOMATION__`, which a
    production `vite build` constant-folds away: the loop's wait and the wake's
    body both sit behind the constant, and the command module that installs the
    poke is imported by `#lib/e2e-automation` alone."""
    tick = (_WEB_LIB / "cert-auto-renew.ts").read_text(encoding="utf-8")
    assert "sleep: (ms) => (__FAUNA_E2E_AUTOMATION__ ? wake.sleep(ms) : plainSleep(ms))," in tick, (
        "the web tick's raceable wait left the automation constant"
    )
    wake = tick.split("export function wakeCertAutoRenewForTest(): void {", 1)
    assert len(wake) == 2, "wakeCertAutoRenewForTest moved"
    assert wake[1].split("\n}\n", 1)[0].strip() == "if (__FAUNA_E2E_AUTOMATION__) currentWake.wake();", (
        "the web wake's body is no longer gated on the automation constant"
    )
    importers = [
        p.relative_to(_WEB_SRC).as_posix()
        for p in _WEB_SRC.rglob("*")
        if p.suffix in (".ts", ".svelte")
        and "from '#lib/cert-auto-renew-e2e'" in p.read_text(encoding="utf-8")
        and p.name != "cert-auto-renew-e2e.ts"
    ]
    assert importers == ["lib/e2e-automation.ts"], (
        f"the auto_renew_wake command module is reached from {importers}, not the e2e surface alone"
    )
    surface = (_WEB_LIB / "e2e-commands.ts").read_text(encoding="utf-8")
    assert "if (__FAUNA_E2E_AUTOMATION__)" in surface, "the e2e command surface lost its gating note"


_APPLE = _REPO / "apps" / "fauna-apple"
_APPLE_CADENCE = _APPLE / "FaunaKit" / "Sources" / "FaunaKit" / "Core" / "DnsAutoRenewCadence.swift"


def _lines_outside_debug(src: str) -> list[str]:
    """The lines of a Swift file that a Release build compiles: those under no
    `#if DEBUG` block (its `#else` branch counts as Release). Nesting-aware for
    every `#if`, so an inner `#if !FAUNA_KIDS` neither opens nor closes the
    DEBUG region."""
    out: list[str] = []
    stack: list[bool] = []  # per open #if: is this branch DEBUG-only?
    for line in src.splitlines():
        t = line.strip()
        if t.startswith("#if"):
            stack.append(t.split("//")[0].strip() in ("#if DEBUG", "#if DEBUG && !FAUNA_KIDS"))
            continue
        if t.startswith("#else") and stack:
            stack[-1] = False
            continue
        if t.startswith("#endif") and stack:
            stack.pop()
            continue
        if not any(stack):
            out.append(line)
    return out


def test_the_apple_auto_renew_wake_is_compiled_out_of_a_release_build():
    """macos + ios share one leg in FaunaKit: `DnsAutoRenewCadence`'s raceable
    wait (`CadenceWake`), the per-start mint and `wakeForTest` sit under
    `#if DEBUG`, a Release loop waits on the bare clock, and both apps'
    `auto_renew_wake` arms live in their DEBUG-only test-agent dispatch."""
    src = _APPLE_CADENCE.read_text(encoding="utf-8")
    release = "\n".join(_lines_outside_debug(src))
    for name in ("CadenceWake", "wakeForTest", "wake.sleep(", "wake.wake()"):
        assert name in src, f"the apple cadence no longer names {name}"
        assert name not in release, f"{name} reaches a Release build of the apple cadence"
    assert "try? await Task.sleep(nanoseconds: autoRenewPollSecs() * 1_000_000_000)" in release, (
        "the apple cadence's Release wait is no longer the bare clock"
    )
    for app in ("Fauna-macOS/App/FaunaMacApp.swift", "Fauna-iOS/App/FaunaApp.swift"):
        text = (_APPLE / app).read_text(encoding="utf-8")
        assert 'case "auto_renew_wake":' in text, f"{app} does not answer auto_renew_wake"
        assert "wakeForTest" not in "\n".join(_lines_outside_debug(text)), (
            f"{app}'s auto_renew_wake arm reaches a Release build"
        )
