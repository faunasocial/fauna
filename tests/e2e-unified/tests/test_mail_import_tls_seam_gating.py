"""Convention 15, the mail-import source-TLS half: the e2e trust seed must be
compiled out of anything that ships.

`docs/goal/architecture/e2e-automation-surface-gating.md` § The convention +
§ The e2e trust seed. The sync agent's IPC half is pinned by
`test_agent_ipc_seam_gating.py` and the native-FFI half by
`test_ffi_flavor_split.py`; this file pins a surface neither can see.

**Why this surface needs its own witness.** `FAUNA_E2E_IMAP_EXTRA_CA_PEM` is
read inside `NativeImapConnector::new` — the ONE door every native consumer
builds its source-IMAP trust store through (`rpc_glue`'s `RpcImportSourceNest`
for linux/tui, `fauna-ffi`'s `FfiMailImportClient` for apple/android/windows).
That reach is exactly why it is worth a gate nobody can quietly drop: it is a
**trust-anchor** switch on the connection that carries the user's foreign-mailbox
password (`mailbox-migration.md` § Credential handling), sitting on the one
constructor all six native apps call. It crosses no UniFFI or wasm boundary, so
it leaves no trace in any generated binding and both FFI witnesses are blind to
it.

**Four independent halves, because each alone false-greens.**

  1. *The gate exists, on both items.* An arm added without a `cfg`, or with one
     that is always true, ships. Read from the source.
  2. *The gate's feature is off in a shipped build.* `debug_assertions` is off
     under `--release` by construction, but `feature = "test-helpers"` is not: a
     `[dependencies]` line naming it turns it on unconditionally, surviving even
     `--no-default-features`. That is how twelve seams reached five release
     artifacts through `fauna-ffi` until 2026-08-01 (`test_ffi_flavor_split.py`'s
     rule (b)).
  3. *The seed still only ADDS an anchor.* The value of a gate on an
     accept-any-certificate switch is a fraction of the value of never having
     built one. `FAUNA_INSECURE_TLS` — the fleet's retired accept-any path — is
     the cautionary tale, and rustls spells the regression `dangerous()`.
  4. *The Rust and Python spellings of the variable agree.* The Rust side reads
     an env var; the harness writes one. Nothing else connects the two, so a
     rename on either side degrades silently into "the seed is simply never set"
     — a TLS handshake failure that reads like a mail-import wizard bug.

Pure text analysis of Rust sources and Cargo manifests — no build, no driver.
"""

import re
import sys
from pathlib import Path

import pytest

from helpers.manifest_seams import shipping_feature_offenders

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_NATIVE = _REPO / "libs" / "fauna-mail" / "src" / "imap_client" / "native.rs"
_SEED_TEST = _REPO / "libs" / "fauna-mail" / "tests" / "imap_client_trust_seed.rs"
_HOST_MAP_TEST = _REPO / "libs" / "fauna-mail" / "tests" / "imap_client_host_map_seed.rs"
_CONFTEST = _REPO / "tests" / "e2e-unified" / "conftest.py"
_PRESETS = _REPO / "libs" / "fauna-client-mail-settings" / "src" / "import.rs"

# The two production sites that build a source-IMAP trust store. Both must go
# through `NativeImapConnector::new`, which is where the seed lives.
_CONSUMERS = (
    _REPO / "libs" / "fauna-client-mail-settings" / "src" / "rpc_glue.rs",
    _REPO / "libs" / "fauna-ffi" / "src" / "mail_import.rs",
)

# The gate every e2e-only seam in this family wears. `debug_assertions` covers
# the dev/e2e builds (the harness builds debug binaries), `test` the crate's own
# suites, and the feature is the explicit opt-in for a release-profile e2e build.
# A shipped artifact has none of the three.
_SEAM_GATE = '#[cfg(any(test, debug_assertions, feature = "test-helpers"))]'

# The env var's name, as the Python harness spells it. Half 4 pins the Rust side
# to this string.
_SEED_ENV = "FAUNA_E2E_IMAP_EXTRA_CA_PEM"

# The preset-host redirect seed (`e2e-gated-seeds.md` § The preset-host
# redirect seed): the seventh seed, on the same door. It moves where a mapped
# host is DIALLED — never the name the handshake verifies — and the harness maps
# one host, the Outlook preset's.
_HOST_MAP_ENV = "FAUNA_E2E_IMAP_HOST_MAP"

# Each seed on `NativeImapConnector::new`: (constant, gated reader). The reader
# is the live switch; each carries a `cfg(not(...))` no-op twin.
_NEW_SEEDS = (
    ("E2E_EXTRA_CA_ENV", "e2e_extra_root"),
    ("E2E_HOST_MAP_ENV", "e2e_host_redirects"),
)

# Crates whose `test-helpers` feature reaches this seam. `fauna-mail` declares
# it; `fauna-client-mail-settings` forwards it (the linux/tui construction site),
# `fauna-ffi` forwards it (apple/android/windows). Turning ANY of them on from a
# shipping dep line puts the seed back into a release artifact.
_SEAM_FEATURE_CRATES = ("fauna-mail", "fauna-client-mail-settings", "fauna-ffi")


def _gated_items() -> list[str]:
    """Every item in `native.rs` that sits directly under the seam gate.

    Derived rather than listed, so a second seed-adjacent item added tomorrow is
    covered with no list for anyone to update.
    """
    text = _NATIVE.read_text(encoding="utf-8")
    return re.findall(
        re.escape(_SEAM_GATE) + r"\s*\n\s*(?:pub\s+)?(?:const|fn)\s+(\w+)",
        text,
    )


def test_the_seed_constant_and_its_reader_are_both_gated():
    """Both halves of the seed carry the gate — and the derivation actually
    finds them, so this file cannot pass by scanning nothing.

    The self-check matters more than it looks: the assertions below are about an
    absence in release builds, and an absence over an empty set is vacuously
    true. If `native.rs` is restructured so the regex stops matching, this fails
    loudly here rather than going quietly green while the seed ships.

    Gating only one half still ships the other. The constant alone in a release
    binary is a `strings` hit naming a trust-override knob that does nothing —
    misleading, and the exact thing convention 15's verification method looks
    for. The reader alone is worse: it is the live switch.
    """
    gated = _gated_items()
    items = [name for seed in _NEW_SEEDS for name in seed] + ["parse_host_map"]
    for item in items:
        assert item in gated, (
            f"`{item}` is no longer directly under {_SEAM_GATE} in "
            "libs/fauna-mail/src/imap_client/native.rs. It is the e2e source-trust "
            "seed on `NativeImapConnector::new` — the one constructor all six "
            "native apps build their source-IMAP trust store through — so an "
            "ungated one is a trust-anchor override shipped in every release "
            "artifact (convention 15). If it was renamed, rename it here; if it "
            "was deleted, delete this assertion with it."
        )


@pytest.mark.parametrize("reader", [reader for _const, reader in _NEW_SEEDS])
def test_the_seed_is_read_only_from_inside_the_gated_reader(reader):
    """`NativeImapConnector::new` reaches each seed's env var through its gated
    helper and nowhere else.

    The failure this catches is a well-meant inline: moving the `env::var` call
    up into `new`'s body "to save a function" drops it out from under the gate,
    because `new` itself is unconditional. The gate would still be there, on a
    helper nothing calls.
    """
    text = _NATIVE.read_text(encoding="utf-8")

    # Anchor on the GATE + definition together, not on the name alone: the
    # module also carries a `cfg(not(...))` no-op twin of the same signature
    # (convention 15's production plumbing), so a bare `find("fn <reader>")`
    # would resolve to whichever of the two happens to be written first —
    # making this test pass or fail on declaration order.
    gated = re.search(
        re.escape(_SEAM_GATE) + r"\s*\n\s*fn " + reader + r"\b",
        text,
    )
    assert gated, (
        f"libs/fauna-mail/src/imap_client/native.rs has no `{reader}` "
        f"directly under {_SEAM_GATE}. The reader is the live switch; ungated it "
        "ships in every release artifact."
    )

    # Every environment read sits inside a function that is itself directly
    # under the gate — the nearest `fn` before each `std::env::var(` is one of
    # the gated items.
    gated_items = set(_gated_items())
    for occurrence in re.finditer(r"std::env::var\(", text):
        enclosing = re.findall(r"\bfn (\w+)", text[: occurrence.start()])
        assert enclosing and enclosing[-1] in gated_items, (
            "libs/fauna-mail/src/imap_client/native.rs reads the process "
            f"environment inside `{enclosing[-1] if enclosing else '?'}`, which is "
            "not directly under the seam gate. The seeds' compile-time exclusion "
            "is the security boundary, and `new()` itself is unconditional — "
            "inlining a read up into it (\"to save a function\") puts it in every "
            "release artifact while leaving the gate sitting on a helper nothing "
            "calls."
        )

    twin = re.search(
        r"#\[cfg\(not\(any\(test, debug_assertions, feature = \"test-helpers\"\)\)\)\]"
        r"\s*\n\s*fn " + reader + r"\b",
        text,
    )
    assert twin, (
        f"the production no-op twin of `{reader}` is gone, so `new()` no "
        "longer compiles in a shipped build — or, worse, the gate was widened so "
        "one body serves both. The twin IS the statement that a release build "
        "has no seed (convention 15)."
    )


def test_the_seed_only_adds_a_trust_anchor():
    """The seed routes through `with_extra_root_pem`, and this module never
    reaches for rustls' certificate-verification escape hatch.

    `mailbox-migration.md` § The two TLS modes: the source password crosses this
    connection. "Trust this specific CA" and "trust anything" are not the same
    lever, and the fleet already retired the second one — `FAUNA_INSECURE_TLS`.
    A gate on an accept-any switch would still be an accept-any switch in every
    debug build a developer runs.
    """
    text = _NATIVE.read_text(encoding="utf-8")
    reader = text[text.find("fn e2e_extra_root") :]
    assert "with_extra_root_pem" in reader, (
        "`e2e_extra_root` no longer routes through `with_extra_root_pem`, the "
        "additive trust-anchor path. Anything else is a new certificate-"
        "verification policy on the connection carrying the user's foreign-"
        "mailbox password."
    )
    assert ".dangerous()" not in text, (
        "libs/fauna-mail/src/imap_client/native.rs calls rustls' `dangerous()` "
        "certificate-verification escape hatch. The source-IMAP path has no "
        "accept-any mode in ANY build — that is what retired `FAUNA_INSECURE_TLS`."
    )


@pytest.mark.parametrize(
    ("seed_env", "seed_test"),
    [(_SEED_ENV, _SEED_TEST), (_HOST_MAP_ENV, _HOST_MAP_TEST)],
    ids=["trust", "host-map"],
)
def test_the_rust_and_harness_spellings_of_the_seed_agree(seed_env, seed_test):
    """Each env var's name is identical on both sides of the seam.

    Nothing but this string connects the Rust reader to the Python harness that
    sets it, so a rename on either side does not fail — it silently stops seeding,
    and the wizard's TLS handshake starts failing for what looks like an app bug.
    """
    native = _NATIVE.read_text(encoding="utf-8")
    assert f'"{seed_env}"' in native, (
        f"libs/fauna-mail/src/imap_client/native.rs no longer names {seed_env}. "
        "The harness sets that exact string; a rename on one side alone leaves the "
        "seed permanently unset."
    )
    assert f'"{seed_env}"' in seed_test.read_text(encoding="utf-8"), (
        f"{seed_test.relative_to(_REPO)} no longer names {seed_env}"
    )
    assert f'env["{seed_env}"]' in _CONFTEST.read_text(encoding="utf-8"), (
        f"tests/e2e-unified/conftest.py no longer writes {seed_env} — the Rust "
        "reader is then never seeded"
    )


def test_the_host_map_refuses_a_non_loopback_target():
    """Loopback only, even under the gate (`e2e-gated-seeds.md` § The
    preset-host redirect seed, the terms): the seed may point a connection
    nowhere but into the harness's own box. The parser checks the address and
    panics, and the Rust unit test that pins it is still there by name — as the
    Microsoft authority seed's plaintext refusal is pinned above."""
    text = _NATIVE.read_text(encoding="utf-8")
    parser = text[text.find("fn parse_host_map") :]
    parser = parser[: parser.find("\n}\n")]
    assert "is_loopback()" in parser and ("panic!(" in parser or "assert!(" in parser), (
        "`parse_host_map` no longer refuses a non-loopback address — the "
        "preset-host redirect seed could then point a source connection off the box."
    )
    assert re.search(
        r"#\[should_panic[^\]]*FAUNA_E2E_IMAP_HOST_MAP[^\]]*\]\s*\n\s*"
        r"fn the_host_map_refuses_a_non_loopback_target\b",
        text,
    ), (
        "native.rs's `the_host_map_refuses_a_non_loopback_target` unit test is gone "
        "or no longer expects a panic naming the variable."
    )


def test_the_harness_maps_the_outlook_host_and_no_other():
    """The harness writes one entry, for the Outlook preset's host, spelled as
    the Rust preset spells it.

    One host is a term of the ruling, not a convenience: mapping Gmail's or
    iCloud's host would serve a witness whose payload is a user's app password,
    which needs its own ruling (`e2e-gated-seeds.md` § The preset-host redirect
    seed, ground (2) re-weighed). And a spelling drift between the harness and
    `provider_preset` would leave the map naming a host the app never dials —
    the walk then reaches for Microsoft's real server.
    """
    presets = _PRESETS.read_text(encoding="utf-8")
    m = re.search(r'ImportSourceKind::Outlook => Some\(\("([^"]+)"', presets)
    assert m, "provider_preset's Outlook arm is no longer a literal host in import.rs"
    preset_host = m.group(1)

    sys.path.insert(0, str(_REPO / "tests" / "e2e-unified" / "fakes"))
    import fake_imap_source

    assert fake_imap_source.OUTLOOK_IMAP_HOST == preset_host, (
        f"the harness's OUTLOOK_IMAP_HOST ({fake_imap_source.OUTLOOK_IMAP_HOST!r}) "
        f"is not provider_preset's Outlook host ({preset_host!r})"
    )

    conftest = _CONFTEST.read_text(encoding="utf-8")
    writes = re.findall(r'env\["' + _HOST_MAP_ENV + r'"\] = (.+)', conftest)
    assert writes == ['f"{host}=127.0.0.1:{port}"'], (
        f"conftest.py must write {_HOST_MAP_ENV} exactly once, as the one entry "
        f"`f\"{{host}}=127.0.0.1:{{port}}\"`; found {writes!r}"
    )
    assert "host, port = session_preset_redirect()" in conftest, (
        f"conftest.py's {_HOST_MAP_ENV} entry no longer comes from "
        "`fake_imap_source.session_preset_redirect()`"
    )
    source = Path(fake_imap_source.__file__).read_text(encoding="utf-8")
    body = source[source.find("def session_preset_redirect") :]
    body = body[: body.find("\ndef ")]
    assert "return OUTLOOK_IMAP_HOST, " in body, (
        "`session_preset_redirect` no longer returns the Outlook preset's host"
    )


def test_no_shipping_dep_line_turns_the_seed_feature_on():
    """No crate in the workspace enables the seed's feature on a
    `[dependencies]` line, where cargo turns it on unconditionally for every
    artifact.

    A consumer that needs the seed forwards it from its OWN opt-in feature
    (`fauna-tui`'s `e2e-agent` → `fauna-client-mail-settings/test-helpers` →
    `fauna-mail/test-helpers` is the reference chain here), so the build recipe
    decides, not the manifest. Naming it on a dep line instead survives
    `--no-default-features` and every release recipe in the tree — convention 15
    rule (b).

    Delegates the scan itself to `helpers.manifest_seams.shipping_feature_offenders`
    (multi-line-aware; see that module's docstring), narrowed to this family's own
    three crates so the message below stays specific to the mail TLS seed; the
    general, all-crate form of this same scan lives in
    `test_test_helpers_dep_line_gating.py`.
    """
    offenders = shipping_feature_offenders(
        "test-helpers", _REPO, crates=set(_SEAM_FEATURE_CRATES)
    )
    assert not offenders, (
        "a shipping dependency line enables the mail-import source-trust seed's "
        "feature, so `FAUNA_E2E_IMAP_EXTRA_CA_PEM` is compiled into release "
        "artifacts — a trust-anchor override on the connection carrying the "
        "user's foreign-mailbox password (convention 15 rule (b)). Forward it "
        "from the consumer's own `e2e-agent`/`test-helpers` feature instead:\n  "
        + "\n  ".join(offenders)
    )


def test_both_native_consumers_build_their_trust_store_through_the_one_door():
    """`rpc_glue` and `fauna-ffi` reach their source trust store via
    `NativeImapConnector::new`, and neither assembles a `RootCertStore` itself.

    This is the assertion that makes the seed's placement mean anything. The
    whole design is that `new` is the ONE door every native consumer goes
    through — that is why no app-side plumbing changed to reach six apps, and
    why no consumer can be the one that forgot. A consumer that later hand-rolls
    its own `RootCertStore` silently opts out: the seed stops reaching that
    app's import path, and the tier_3 walk starts failing at the handshake for
    reasons that look nothing like the cause.

    The second half is worth as much as the first. A hand-built store is not
    only unseeded — it is a *second* trust policy for the connection carrying
    the user's foreign-mailbox password, decided somewhere no one is reviewing
    trust decisions.
    """
    for path in _CONSUMERS:
        text = path.read_text(encoding="utf-8")
        rel = path.relative_to(_REPO)
        assert "NativeImapConnector::new(" in text, (
            f"{rel} no longer builds its source connector with "
            "`NativeImapConnector::new`. That call IS the source-IMAP trust "
            "seed's door, so this consumer's import path is now unseeded — and "
            "whatever replaced it is a second, unreviewed trust policy on the "
            "connection carrying the user's foreign-mailbox password."
        )
        assert "RootCertStore" not in text, (
            f"{rel} assembles its own `RootCertStore`. Source-server trust is "
            "decided in exactly one place (`fauna_mail::imap_client::native`); a "
            "second one bypasses both the WebPKI roots it sets up and the e2e "
            "trust seed that rides on them."
        )


# ── The Microsoft authority seed (`e2e-automation-surface-gating.md` § The
# Microsoft authority seed): the fifth gated seed, on the ONE door that builds
# the Outlook sign-in's endpoints.

_OAUTH = _REPO / "libs" / "fauna-mail" / "src" / "imap_client" / "oauth_device.rs"
_AUTHORITY_ENV = "FAUNA_E2E_MICROSOFT_AUTHORITY"


def test_the_microsoft_authority_seed_is_gated_with_a_production_twin():
    """The constant and its reader carry the gate, the reader has its
    same-signature no-op twin, and the environment is read nowhere but inside
    the gated reader.

    The seed redirects the identity far end the device code and the refresh
    token are sent to; ungated, every release artifact would carry a switch
    that points a user's Microsoft sign-in at another host.
    """
    text = _OAUTH.read_text(encoding="utf-8")
    gated = re.findall(
        re.escape(_SEAM_GATE) + r"\s*\n\s*(?:pub\s+)?(?:const|fn)\s+(\w+)",
        text,
    )
    for item in ("E2E_AUTHORITY_ENV", "authority_base", "seeded_authority"):
        assert item in gated, (
            f"`{item}` is no longer directly under {_SEAM_GATE} in "
            "libs/fauna-mail/src/imap_client/oauth_device.rs — the Microsoft "
            "authority seed would ship in every release artifact (convention 15)."
        )
    twin = re.search(
        r"#\[cfg\(not\(any\(test, debug_assertions, feature = \"test-helpers\"\)\)\)\]"
        r"\s*\n\s*fn authority_base\b",
        text,
    )
    assert twin, (
        "the production no-op twin of `authority_base` is gone — the twin IS "
        "the statement that a shipped build has no authority seed."
    )
    reader = re.search(re.escape(_SEAM_GATE) + r"\s*\n\s*fn authority_base\b", text)
    assert reader
    for occurrence in re.finditer(r"std::env::var\(", text):
        assert occurrence.start() > reader.end(), (
            "oauth_device.rs reads the process environment outside the gated "
            "`authority_base` reader, which `MicrosoftIdentity::client` — "
            "unconditional — would then carry into every release artifact."
        )
    assert f'"{_AUTHORITY_ENV}"' in text, (
        f"oauth_device.rs no longer names {_AUTHORITY_ENV}, the spelling "
        "`e2e-gated-seeds.md` § The Microsoft authority seed rules."
    )


def test_the_microsoft_authority_seed_refuses_a_plaintext_authority():
    """`https://` only, even under the gate: a token never crosses a
    plaintext hop in a test either (the gating doc's terms)."""
    text = _OAUTH.read_text(encoding="utf-8")
    body = text[text.find("fn seeded_authority") :]
    assert 'starts_with("https://")' in body and "panic!(" in body, (
        "`seeded_authority` no longer refuses a non-https authority."
    )
