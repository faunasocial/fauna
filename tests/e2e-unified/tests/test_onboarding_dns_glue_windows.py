"""tier_2 e2e: the onboarding→launch DNS-credential hand-off on **windows**.

The windows twin of ``test_onboarding_dns_glue_app.py``. A DNS-provider
credential captured at the onboarding DNS step must be sealed into the admin's
``fauna.state.dns`` at the ``LoggedIn`` outcome, via the post-onboarding
``DnsManagementMachine::PutCredentials`` path — one store, one writer
(``dns-management.md`` § Where the credential lives; ``onboarding.md`` §4). This
gates the windows ``OnboardingViewModel.HandleWizardOutcome`` ``LoggedIn`` arm
(``SealCapturedDnsCredentialAsync``) — the lift of linux's
``launch_main_app_after_signin -> dns_put_credentials``.

The test drives the **real** ``test@localhost`` onboarding happy-path all the way
to ``WizardOutcome::LoggedIn`` against a real unclaimed nest (so the seal's
``fauna.state.dns`` plane write is real end-to-end), then asserts the credential
appears on ``admin-dns`` (``admin-dns-credential-item``).

tier_2 (not tier_3): the nest binary, the admin claim, and the config seal
round-trip are all real, but the **external DNS provider** is faked — the
captured credential carries the ``fake-dns-ok:<zone>`` sentinel and the windows
launch config sets ``FAUNA_DNS_PROVIDER_FAKE`` (the native twin of web's
``enableDnsFakeProviderForTest``), so the launch glue's ``PutCredentials.verify()``
succeeds offline. The captured credential is injected through the native
cross-app bridge route ``call_machine_method("set_captured_dns_credential_for_test")``
(the shared ``OnboardingMachine::call_machine_method`` arm — the localhost
happy-path skips the DNS step, so the wizard never captures one on its own).
Mirrors the native ``test_admin_dns_managed.py`` fake-provider harness;
classified the same way (``scripts/tag-test-tiers.py`` EXPLICIT_OVERRIDES).
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
from pathlib import Path

import pytest

_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer
from common import CLAIM_CODE
from conftest import get_available_apps

if "windows" not in get_available_apps():
    pytest.skip("windows client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier1, pytest.mark.windows, pytest.mark.tier_2]

_ZONE = "e2e-onboard.test"


def _client_log_tail(driver=None, n: int = 120) -> str:
    """Tail the windows FaunaApp client log (the in-process Rust FFI `tracing`
    output, incl. any `ws connect … os error 10061` line + the host:port it
    dialed). `App.OnLaunched` installs the daily-rolling file under
    `<BackupPaths.DataDir>\\logs\\fauna.log.<date>` (App.xaml.cs InstallLogging).
    The bridge does NOT redirect FaunaApp stdout, so this on-disk log is the only
    place the client's connect error surfaces. Returns the most-recently-modified
    log's tail (the run we just drove).

    Reads the LAUNCH's data dir, not `%LOCALAPPDATA%`: the driver isolates every
    windows launch under its own root (e2e rule 10), and on a dev box the real
    profile is the *installed* app's — tailing it would diagnose the developer's
    desktop instead of the run."""
    base = None
    if driver is not None:
        env = (getattr(driver, "_session_body", None) or {}).get("environment", {})
        base = env.get("FAUNA_E2E_DATA_DIR") or getattr(driver, "_data_dir", None)
    log_dir = Path(base) / "logs" if base else (
        Path(os.environ.get("LOCALAPPDATA", "")) / "Fauna" / "logs")
    if not log_dir.is_dir():
        return f"(no client log dir at {log_dir})"
    candidates = sorted(log_dir.glob("fauna.log*"), key=lambda p: p.stat().st_mtime)
    if not candidates:
        return f"(no fauna.log* files in {log_dir})"
    latest = candidates[-1]
    try:
        lines = latest.read_text(errors="replace").splitlines()
    except OSError as e:
        return f"(could not read {latest}: {e})"
    return f"[{latest.name}, last {n} lines]\n" + "\n".join(lines[-n:])


def _seal_failure_diagnostics(nest: dict, app) -> str:
    """On a seal-never-appeared failure, capture the three facts that split the
    root-cause tree — nest crash vs windows-client mis-dial vs harness — so the
    assertion diagnoses itself (e2e rule #6) without a manually-instrumented
    re-run. (Authored 2026-06-22 root-causing the original failure: the e2e
    `reset`/`logout` test commands didn't wire `OnOnboardingCompleted`, so the
    post-onboarding handoff never re-pointed the main client off the `:443` launch
    default — the seal succeeded but admin-dns couldn't read it back. Diagnostic #1
    proved the nest stays alive [not a crash]; #2 that its listener never closed
    after the encrypted commit; #3 that the refusal was a wrong-endpoint dial.
    Kept permanently as a regression tripwire for this authed-reconnect path.)"""
    out = ["===== ONBOARDING-SEAL FAILURE DIAGNOSTICS ====="]

    # (1) Did the nest PROCESS exit? None = alive (so the fault is client/harness:
    #     a mis-dial or timing); a non-None exit code = the nest binary crashed.
    rc = nest["proc"].poll()
    out.append(f"(1) nest proc.poll() = {rc!r}  "
               f"({'ALIVE → client/harness issue' if rc is None else 'EXITED → nest crash'})")

    # (2) nest.log tail — a panic after `storage mode committed: encrypted`
    #     (a nest bug), or healthy silence (listener stays up → look client-side)?
    #
    # `log_path` is a STANDALONE-only key: the standalone provider redirects the
    # nest process's own stdout to a file it names, while a container's log lives
    # in the daemon (`docker logs`) and the docker handle publishes no such key.
    # Reached through `.get` rather than `[...]` because this whole helper runs
    # only on the failure path — a `KeyError` here would replace the assertion
    # that actually failed, which is the one thing a diagnostic must never do.
    log_path = nest.get("log_path")
    if log_path is None:
        out.append("(2) nest.log: not available in this nest mode "
                   "(no `log_path` key — a container's log lives in the daemon)")
    else:
        try:
            nlines = Path(log_path).read_text(errors="replace").splitlines()
            out.append("(2) nest.log [last 60 lines]:\n" + "\n".join(nlines[-60:]))
        except OSError as e:
            out.append(f"(2) nest.log unreadable: {e}")

    # (3) The client refusal — `os error 10061` (connection refused) to a port the
    #     nest IS listening on means the client dialed a DIFFERENT address (e.g.
    #     `[::1]:port` for a `localhost` handle vs the nest's IPv4 bind), not a TLS
    #     or timeout fault.
    client_tail = _client_log_tail(getattr(app, "driver", None))
    out.append("(3) windows client log:\n" + client_tail)
    refusals = [ln for ln in client_tail.splitlines()
                if re.search(r"os error 10061|refused|ws connect|connect.*error", ln, re.I)]
    out.append("(3b) refusal/connect lines:\n" + ("\n".join(refusals) if refusals else "(none matched)"))

    out.append(f"bind endpoint (nest): 127.0.0.1:{nest['port']}  |  nest url: {nest.get('url')}")
    try:
        out.append(f"app.error_text() = {app.error_text()!r}")
    except Exception as e:
        out.append(f"app.error_text() raised: {e}")
    out.append("===== END ONBOARDING-SEAL FAILURE DIAGNOSTICS =====")
    return "\n".join(out)


@pytest.fixture
def windows_onboard_nest(request, nest_mode, tmp_path_factory):
    """A fresh, never-claimed nest the windows app onboards against.

    Started through the mode provider seam; the windows FaunaApp child process
    reaches it at 127.0.0.1:<port> via the `test@127.0.0.1:<port>` handle.
    `unclaimed=True` leaves the nest claimable with `CLAIM_CODE`, and both it and
    `serve_tls` are honoured in every mode (the docker provider declares both),
    so the handle this journey types resolves to the same https authority
    against a container.

    Serves REAL self-signed HTTPS (`serve_tls=True`): after Pillar C (uniform
    https) the `test@127.0.0.1:{port}` handle resolves to `https://127.0.0.1:{port}`,
    so the production-faithful posture is a nest serving TLS there. The floor cert's
    SANs cover `127.0.0.1`+`localhost`, so the IP literal validates against the
    captured cert; the windows app trusts it via channel-binding (probe + the
    graduated SPKI pin the **post-LoggedIn** DNS-seal plane write reuses). No
    `set_provider_base_urls` override is needed — the resolved https URL points
    straight at the nest.

    The handle is the **IPv4 literal** `127.0.0.1`, NOT `localhost` (matches the
    linux/macos sibling onboarding tests — priority #1 uniformity). `start_nest`
    binds the nest **IPv4-only** (`--bind 127.0.0.1:{port}`); on a Windows VM
    `localhost` resolves to `::1` (AAAA) *first*, so a `localhost` handle would make
    the post-LoggedIn C# `NestRpcClient` dial `[::1]:{port}` (no listener) — a
    defensive ambiguity an IPv4 literal removes. Real handles are `user@domain` /
    `user@ip`, never `localhost`, so this is the production-faithful choice."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "win-onboard-dns-nest",
        unclaimed=True, serve_tls=True)
    yield nest
    cleanup()


@pytest.fixture
def windows_fresh_app(_driver_cache):
    """A windows ActionLayer reset to a fresh onboarding wizard."""
    from drivers.http_bridge import BridgeDead

    driver = _driver_cache("windows")
    try:
        driver.reset()
    except (BridgeDead, TimeoutError):
        if not driver.recover():
            pytest.skip("windows bridge died and could not recover")
        driver.reset()
    yield ActionLayer(driver)


def test_onboarding_seals_captured_dns_credential_windows(windows_fresh_app, windows_onboard_nest):
    app = windows_fresh_app
    driver = app.driver
    port = windows_onboard_nest["port"]
    # IPv4 literal, NOT `localhost` — matches the linux/macos sibling tests
    # (priority #1) and avoids the win `localhost`→`::1`-first ambiguity for the
    # post-LoggedIn C# NestRpcClient (see `windows_onboard_nest` docstring).
    handle = f"test@127.0.0.1:{port}"

    # Relaunch trusting `windows_onboard_nest` BEFORE identity creation below
    # — a relaunch after would discard the in-progress wizard state.
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(driver, windows_onboard_nest)

    # ── Onboarding: create identity ─────────────────────────────────────
    # reset() returns when session.authenticated==false, but the OnboardingPage
    # Navigate is async — the identity-choice screen may not have rendered yet.
    # Re-reset once if the first element doesn't appear (the reset→onboarding
    # transition is racy on the FlaUI bridge).
    try:
        driver.wait_for("create-identity-button", timeout=40)
    except TimeoutError:
        driver.reset()
        driver.wait_for("create-identity-button", timeout=40)
    driver.click("create-identity-button")
    driver.wait_for("identity-continue-button", timeout=15)
    driver.click("identity-continue-button")

    # Pillar C (uniform https): `test@127.0.0.1:{port}` resolves to
    # `https://127.0.0.1:{port}` and the nest serves real self-signed HTTPS there
    # (`serve_tls=True`), so NO `set_provider_base_urls` override is needed — the
    # probe AND the post-LoggedIn DNS-seal plane write reach the nest
    # directly and trust the self-signed floor via the graduated SPKI pin.

    # ── Handle check → claim-code (unclaimed nest) ──────────────────────
    driver.wait_for("handle-input", timeout=20)
    driver.clear_and_type("handle-input", handle)
    driver.click("handle-check-button")
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if app.is_enabled("handle-entry-continue-button"):
            break
        time.sleep(0.5)
    else:
        err = app.error_text() if hasattr(app, "error_text") else ""
        pytest.fail(f"Continue never enabled for {handle}; error={err!r}")
    driver.click("handle-entry-continue-button")

    driver.wait_for("claim-code-input", timeout=20)
    driver.clear_and_type("claim-code-input", CLAIM_CODE)
    driver.click("claim-code-submit-button")

    # ── Reach nat_mode_choice (admin claim succeeded) ────────────────────
    # Claim now lands the wizard directly on nat_mode_choice (no-modes
    # retirement, ratified 2026-07-12) — the former intervening
    # encryption_mode_choice step is retired.
    driver.wait_for("nat-mode-confirm-button", timeout=30)

    # ── Inject the onboarding-captured DNS credential ────────────────────
    # The localhost path skips the DNS step, so the wizard never captures a
    # credential on its own. Inject one carrying the fake-provider sentinel via
    # the native cross-app bridge (the new
    # `set_captured_dns_credential_for_test` arm on the shared machine), just
    # before the terminal confirm. `captured_dns_credential()` then yields
    # `Some` at LoggedIn, exercising the real launch glue.
    driver.call_machine_method(
        "set_captured_dns_credential_for_test",
        json.dumps({
            "provider_id": "cloudflare",
            "fields": {"api-token": f"fake-dns-ok:{_ZONE}"},
        }),
    )

    # ── Confirm NAT mode → LoggedIn → the seal fires ──────────────────────
    # The wizard reaches Done, and so the LoggedIn seal glue only fires, once
    # nat_mode_choice (onboarding.md § 3b-bis) is confirmed. This no-ops until
    # the windows nat_mode_choice view lands (Slice-2 fan-out).
    app.onboarding.finish_nat_mode()
    # The wizard exits to LoggedIn and the app hands off to MainPage; the seal is
    # fire-and-forget. Open admin-dns and poll for the sealed credential (the
    # page hydrates `fauna.state.dns`). FAUNA_DNS_PROVIDER_FAKE is set in the
    # windows launch config, so the seal's verify() succeeds offline.
    time.sleep(3)
    app.admin.navigate_dns()
    # The admin-dns page hydrates its credential list once on load (config.get);
    # the seal persists the credential out-of-band, so click the page's refresh
    # affordance (re-Hydrate) each poll to pick up the freshly-sealed credential.
    deadline = time.monotonic() + 40
    last = -1
    while time.monotonic() < deadline:
        last = app.admin.dns_credential_count()
        if last >= 1:
            break
        if driver.is_visible("admin-dns-refresh-button"):
            driver.click("admin-dns-refresh-button")
        time.sleep(1.5)
    err = app.error_text()
    # Capture the diagnostics before failing, so the run is self-diagnosing (e2e
    # rule #6) and a single run splits the root-cause tree (nest crash vs
    # windows-client mis-dial vs harness) without a manually-instrumented re-run.
    diag = "" if last >= 1 else _seal_failure_diagnostics(windows_onboard_nest, app)
    if diag:
        print("\n" + diag)
    assert last >= 1, (
        "onboarding-captured DNS credential never sealed into fauna.state.dns "
        f"(admin-dns-credential-item count={last}, error={err!r}). The LoggedIn "
        "launch glue (OnboardingViewModel.SealCapturedDnsCredentialAsync -> "
        "PutCredentials), the set_captured_dns_credential_for_test bridge arm, or "
        "the FAUNA_DNS_PROVIDER_FAKE wiring is broken.\n\n" + diag
    )
    assert "cloudflare" in app.admin.dns_credential_providers(), (
        f"sealed credential provider mismatch: {app.admin.dns_credential_providers()!r}"
    )
