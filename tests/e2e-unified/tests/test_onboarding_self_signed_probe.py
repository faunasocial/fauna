"""tier_3: the onboarding handle-check probe tolerates a self-signed local nest.

Regression guard for the fix `fix(rust): onboarding handle-check probe tolerates
self-signed local nests` (`OnboardingMachine::local_nest_probe_client`,
`libs/fauna-onboarding-machine/src/machine.rs`).

The bug: the Fauna-app onboarding handle-check nest-health probe used a
strict-WebPKI reqwest client, so reaching a bare-IP / domainless nest — which
serves the always-live **self-signed floor** (`security.md` § Transport trust;
`nest/domains-and-tls-bootstrap.md` § Boot, SANs `localhost`+`127.0.0.1`, never a
LAN IP) — failed the TLS handshake → `ConnectionRefused` → outcome
`RegisteredNoNest`. Repro: `test@<lan-ip>` reported "registered, no nest" while
`https://<lan-ip>` served the SPA in a browser. The fix routes a `target.is_local`
probe through `fauna_anon_client::tls_verify` `NoPinPolicy::AcceptProvisional` (the
channel-binding-tolerant `http_local_nest` client, `machine.rs:1633`) so it
accepts the self-signed cert provisionally; trust is then established by the
WS-RPC channel binding, not WebPKI.

Why a NEW e2e is owed (the gap that let it ship): EVERY existing e2e tier was
blind to this path. tier_1/2/3 nests run with `FAUNA_INSECURE_DISABLE_TLS=1` (set
process-wide in `conftest.py` `pytest_sessionstart`) so the nest API serves PLAIN
HTTP — the probe never does a TLS handshake, so the self-signed reachability path
is never walked (`nest/domains-and-tls-bootstrap.md` § Test posture). tier_4 tests
the nest *serving* TLS, not the client onboarding handle-check. The unit test added
with the fix (`accept_provisional_reaches_self_signed_cert`,
`libs/fauna-client/tests/reqwest_pinned_tls.rs`) covers the TLS-client *primitive*
only, not the onboarding wiring. This is the integration-level guard.

The crux is the nest fixture: it opts OUT of the harness's plain-HTTP escape
(`start_nest(serve_tls=True)` clears `FAUNA_INSECURE_DISABLE_TLS` for this one
nest) so the nest serves its self-signed floor over real HTTPS on its API
listener — the only e2e nest that does.

Acceptance for `installers/windows.md` § Implementation status → *Network-reachable
nest* (`test@<ip>` → `https://<ip>` → Reachable, ratified 2026-06-20): the
shared-Rust client fix this proves is the same one the Windows network-reachable
nest relies on. Native-only — a browser cannot programmatically trust a self-signed
cert (web is single-origin), so this is `--client linux` (the Linux workhorse client);
macOS/iOS use the same shared client but are macOS-machine-gated.
"""

from __future__ import annotations

import secrets
import sys
from pathlib import Path

import pytest

from helpers.app_surface import declared_absence, skip_unbuilt

# Mirror the sys.path bootstrap of the sibling local-onboarding e2e files so the
# shared `common` package (tests/common/) and the e2e-unified helpers resolve.
_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from common.nest import CLAIM_CODE  # noqa: E402
from conftest import get_available_apps  # noqa: E402

_avail_clients = get_available_apps()
if "linux" not in _avail_clients:
    pytest.skip(
        "drives the linux client onboarding handle-check over a real self-signed "
        "TLS nest (web cannot trust a self-signed cert; apple is mac-gated)",
        allow_module_level=True,
    )

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


def _require_native_self_signed_probe(driver) -> None:
    """The self-signed-tolerant onboarding handle-check probe is native-only
    (a browser cannot programmatically trust a self-signed cert — structural,
    testing.md § Cross-app e2e conventions, point 7); linux is the sole app
    that has lifted the driver-side onboarding flow this test drives so far,
    apple is separately mac-machine-gated fleet-wide."""
    if driver.is_web():
        declared_absence(
            driver,
            capability="a self-signed-cert-trusting TLS onboarding probe",
            doc="testing.md § Cross-app e2e conventions, point 7 (web is "
            "single-origin and cannot be told to trust a self-signed cert)",
        )
    elif not driver.is_linux():
        skip_unbuilt(
            driver,
            surface="the self-signed-tolerant onboarding handle-check e2e drive",
            detail="linux proves the shared-Rust "
            "OnboardingMachine::local_nest_probe_client fix end to end; "
            "windows/tui/android are the remaining cross-app follow-on "
            "(macOS/iOS use the same shared client but are mac-machine-gated "
            "fleet-wide, not unbuilt)",
            tracked="onboarding.md",
        )


# A bare LAN-IP handle: `resolve_handle_domain` classifies it `is_local=true` AND
# `scheme=https` (`libs/fauna-provisioning/src/probe.rs` — loopback `127.0.0.1`
# would resolve to *http* and never hit a TLS handshake, so it would NOT reproduce
# the bug; a non-loopback IP literal is the bug's exact path). The address is
# unrouteable RFC-1918 and is NOT where the nest actually listens — the e2e "nest"
# provider override redirects the probe's connection at the locally-bound test nest
# (the same redirect every local-nest onboarding e2e uses), while the typed host is
# what drives `is_local` → the channel-binding-tolerant probe client under test.
_LAN_IP_HANDLE_HOST = "192.168.1.57"


# ── Fixture: a fresh UNCLAIMED nest serving REAL self-signed HTTPS ─────────────
#
# `serve_tls=True` clears the process-wide `FAUNA_INSECURE_DISABLE_TLS` escape for
# THIS nest only, so it serves its always-live self-signed floor cert over HTTPS on
# its API listener (the returned `url` is `https://…`). `unclaimed=True`: the UI
# drives `fauna.auth.claim_admin` itself over the TLS connection — `start_nest`
# does NOT auto-claim over a plain-HTTP port.


@pytest.fixture
def self_signed_tls_nest(request, nest_mode, tmp_path_factory):
    """A fresh, never-claimed nest serving its self-signed floor over real HTTPS.

    Both options are honoured in every mode (the docker provider declares
    ``unclaimed`` and ``serve_tls``), so the two asserts below hold in a
    container as well — where TLS is not an opt-out but the image's own posture.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "self-signed-probe-nest",
        unclaimed=True, serve_tls=True)
    assert nest["url"].startswith("https://"), (
        f"serve_tls nest must report an https url, got {nest['url']!r}"
    )
    assert nest["admin"] is None, "fixture must hand the UI a genuinely unclaimed nest"
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("connect-and-sign-in")
def test_onboarding_handle_check_tolerates_self_signed_local_nest(
    app, self_signed_tls_nest
):
    """A `test@<lan-ip>` handle-check against a self-signed-HTTPS local nest reaches
    the nest (`UnregisteredUnclaimedNest`) instead of the pre-fix `RegisteredNoNest`
    dead-end, and the claim proceeds — proving the onboarding probe + the WS-RPC
    silent-challenge + the setup-status + the claim_admin RPC all tolerate the
    self-signed floor over real TLS."""
    _require_native_self_signed_probe(app.driver)

    nest = self_signed_tls_nest
    ob = app.onboarding

    # Relaunch trusting `nest` BEFORE `go_to_handle_entry` below imports an
    # identity into the in-progress wizard — a relaunch after would wipe it
    # (a relaunch is a fresh launch, convention 10).
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    # Fresh identity → handle_entry. A random 32-byte seed; the claim binds it as
    # admin (no pre-registration).
    secret_hex = secrets.token_hex(32)
    ob.go_to_handle_entry(secret_hex)
    ob.fill_handle(f"test@{_LAN_IP_HANDLE_HOST}")

    # Redirect the probe at the locally-bound HTTPS nest (set last, so it survives
    # any machine reset during identity import). The typed `192.168.1.57` host
    # still drives `is_local` → the AcceptProvisional probe client under test; the
    # override only supplies the reachable base URL (and `effective_nest_url` honors
    # it for the later claim RPC too — `machine.rs:1109`).
    app.driver.set_provider_base_urls({"nest": nest["url"]})
    ob.run_handle_check(timeout=45)

    # ── Primary regression assertion: the self-signed floor was REACHED. ──
    # Pre-fix (strict-WebPKI probe) → ConnectionRefused → `RegisteredNoNest`:
    #   Continue DISABLED + the "register anyway" `handle-control-checkbox` shown.
    # Post-fix (AcceptProvisional probe) → Reachable → `UnregisteredUnclaimedNest`:
    #   Continue ENABLED, no control checkbox (`machine.rs` complete()).
    msg = app.get_text("handle-message-area").strip()
    assert app.is_absent("handle-control-checkbox"), (
        "handle-check fell into the RegisteredNoNest dead-end (the 'register "
        "anyway' control checkbox is shown) — the strict-WebPKI probe failed the "
        f"self-signed TLS handshake. message-area={msg!r} error={app.error_text()!r}"
    )
    assert app.is_enabled("handle-entry-continue-button"), (
        "handle-check did not reach a continueable outcome over self-signed TLS "
        f"(expected UnregisteredUnclaimedNest). message-area={msg!r} "
        f"error={app.error_text()!r}"
    )

    # ── Continue routes to claim_code — proving the WS-RPC silent-challenge AND the
    # setup-status probe (both over the self-signed TLS connection) reported an
    # unregistered identity on an unclaimed nest. Reaching this page is already
    # three network round-trips over the self-signed floor. ──
    ob.submit_handle()
    app.driver.wait_for("claim-code-input", timeout=45)

    # ── Push as deep as stays stable: claim the nest. `claim-code-submit` fires
    # `fauna.auth.claim_admin` over `effective_nest_url` (= the override, self-signed
    # HTTPS); success lands directly on nat_mode_choice (no-modes retirement,
    # ratified 2026-07-12). Stop here — the post-login session connect would
    # target the persisted typed-handle URL (192.168.1.57), which isn't where
    # the nest listens. ──
    app.driver.clear_and_type("claim-code-input", CLAIM_CODE)
    app.driver.click("claim-code-submit-button")
    app.driver.wait_for("nat-mode-confirm-button", timeout=45)
    assert app.is_visible("nat-mode-confirm-button"), (
        "claim_admin over the self-signed TLS connection did not complete — the "
        f"wizard never reached nat_mode_choice. error={app.error_text()!r}"
    )
