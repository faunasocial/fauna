"""tier_3 (local CI): the wizard's ``LoggedIn`` terminal must not tear the app
down when the stored identity secret is empty.

Per ``docs/goal/behavior/onboarding.md`` § Wizard exit handling, the
``LoggedIn { nest_url, handle }`` outcome's whole contract is "save
``(nest_url, handle)`` to the long-term identity store … navigate to the
authenticated UI (feed)". **No quit path is sanctioned there** — and the
document is emphatic about why: the retired ``InviteSubmitted`` variant's
per-app exit arms "had quietly become five different behaviors (quit the app /
blank page / fall back to identity-choice / a sessionless shell / a dead-end
text screen)", and deleting that exit is what made the surface un-divergable.

**What this pins.** The terminal must take the identity secret from the
*machine* (``effective_secret()`` — the secret the wizard just authenticated
with, always present at this moment), not from the long-term identity store, which
holds it only if moment 1's confirm-identity write landed. That write can silently not land:
``persist_confirmed_identity``'s read-back exists precisely because
``SecretStore::set`` is infallible *by signature*, and every app's
confirm-identity write is log-only on failure (``onboarding.md`` § Architectural
rules 5 + the ``add_account`` doc in ``libs/fauna-client-accounts/src/lib.rs``).
So "authenticated session, empty identity store" is a state a real user's box can
reach — a keyring that kept nothing, or a machine-only seeded identity —
and on linux that state used to reach ``window.close()``, which on the
non-append path quits the process through ``connect_close_request``: the user
watches the app vanish at the exact moment onboarding succeeded, with no window
and no message.

**Why the existing local suite never caught it.** Every other local claim flow
seeds identity through ``ob.import_key()`` — the real UI import, which runs
``confirm_imported_identity`` and therefore moment 1 — so the identity store is
always populated by the time the terminal reads it. Only the paid live
provisioning e2e (``tests/live/test_hetzner_provision.py``, ``just
e2e-live-provision``) seeds via the ``seed_identity`` machine method, which
writes machine state and nothing else; it is opt-in, so the gap sat unrun while
the wizard evolved. This test reproduces that drive path headlessly against a
local unclaimed nest — no paid box — which is what makes the mechanism testable
rather than something only a human running the paid flow could witness. The
quit is a mechanism, and mechanisms get a headless test; a paid live run is
the last inch, never a mechanism's only witness.

Cross-app by construction: it drives ``call_machine_method`` + the shared
ui.yaml claim IDs, so every app with the in-process wizard asserts the same
contract. tui and android already read the machine here; apple and web survive
an empty store without quitting. **windows had the same class of bug,
differently shaped**: ``App.onOnboardingCompleted`` gated on the long-term store
read (``secretStore.LoadSecret()``/``LoadNestUrl()``) and, finding it empty on
this test's seed-identity-driven path, logged ``"long-term store missing
secret/nest_url after wizard Done — staying on OnboardingPage"`` and returned
— no quit (unlike linux's old bug), but the terminal never ran and the app
never reached the authenticated shell: a stall, not a crash, caught by the
same shell-arrival assertion below rather than the pid-quit check. Fixed by
reading the secret from ``OnboardingViewModel.EffectiveSecret`` (the machine),
captured before ``OnLoggedIn`` tears it down — the same capture-before-teardown
shape ``SealCapturedDnsCredentialAsync``/``MintDefaultTrustSetAsync`` already
used.
"""

from __future__ import annotations

import json
import secrets
import sys
from pathlib import Path

import pytest

# Mirror the sys.path bootstrap of the other local-onboarding e2e files so the
# shared `common` package (tests/common/) and the e2e-unified helpers resolve.
_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from common.nest import CLAIM_CODE  # noqa: E402
from conftest import MAIL_PRIMARY_DOMAIN, get_available_apps  # noqa: E402
from helpers.authenticated_shell import SHELL_MARKERS  # noqa: E402
from helpers.waiting import wait_until  # noqa: E402

_avail_apps = get_available_apps()
if not any(c in _avail_apps for c in ("linux", "macos", "ios", "tui", "windows")):
    pytest.skip(
        "drives the linux/macOS/iOS/tui/windows in-process admin-claim onboarding wizard",
        allow_module_level=True,
    )

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


# The authed app shell has mounted once any main-view landmark is visible: this
# test only needs "the terminal completed and the app is still here", and apps
# legitimately differ on where they land. The set is shared (which app lands
# where is documented there) — a private copy is what traced two 120s windows hangs to.
_LOGGED_IN_MARKERS = SHELL_MARKERS

# Convention 14: named budgets sized far above any non-pathological delay, with
# deadline polls rather than fixed waits. A green run pays only true latency.
_CLAIM_PAGE_BUDGET_S = 45.0    # unclaimed-nest probe → claim_code page
_TERMINAL_BUDGET_S = 120.0     # claim → nat_mode dismissal → authed shell

# The app's own words when it takes the teardown branch this test forbids
# (`views/onboarding/mod.rs`, the `LoggedIn` arm). Best-effort evidence only —
# see `_app_log` for why it is not the primary observable.
_TEARDOWN_LOG_MARK = "tearing the wizard down"

# windows' own failure signature (pre-fix): it does not quit (no `_app_proc`,
# so `_app_pid` is always None there — the pid-quit check below is skipped by
# construction) and does not log the linux teardown line either. Instead
# `App.onOnboardingCompleted` (`App.xaml.cs`) gated on the long-term store read
# and bailed, silently stalling on the onboarding page. Same "app never
# reaches the authenticated shell" symptom under the shared `_landed()` poll,
# just a different log line to name it by — now only the defensive fallback
# branch (both the machine read AND the store are empty), never the primary
# path.
_WINDOWS_STALL_LOG_MARK = "staying on OnboardingPage"


@pytest.fixture
def empty_store_unclaimed_nest(request, nest_mode, tmp_path_factory):
    """A fresh **unclaimed** nest serving real self-signed HTTPS on loopback.

    Same posture as ``test_mail_enable_at_admin_claim.py``'s fixture: after
    Pillar C (uniform https) the loopback ``…@127.0.0.1:{port}`` handle resolves
    to ``https://127.0.0.1:{port}``, the app trusts the self-signed floor via
    channel binding, and no ``set_provider_base_urls`` override is needed.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "empty-store-terminal-nest",
        handle_domain_seed=MAIL_PRIMARY_DOMAIN, unclaimed=True, serve_tls=True,
    )
    assert nest["admin"] is None, "fixture must hand the UI a genuinely unclaimed nest"
    try:
        yield nest
    finally:
        cleanup()


def _app_pid(app):
    """The app process's identity, or ``None`` where the driver hides it.

    **This is the observable that proves the app quit**, and the two obvious
    alternatives are both wrong here — each cost a red-verification run to
    establish, so do not "simplify" back to them:

    * *Bridge liveness* does not work. When the app dies the driver answers by
      relaunching it (``PlatformDriver.recover`` → ``teardown()`` + ``launch()``),
      so by the time a test looks, a healthy bridge is answering again. A quit
      shows up as a fresh process back at onboarding, never as a bridge that
      stays visibly dead.
    * *The app log* does not survive it either: the relaunch reopens ``app.err``
      with mode ``"w"``, truncating the very line that named the teardown.

    A relaunch necessarily mints a new process, so an identity that changed
    across the terminal IS the quit, recorded after the fact.
    """
    proc = getattr(app.driver, "_app_proc", None)
    return getattr(proc, "pid", None) if proc is not None else None


def _app_log(app) -> str:
    """This launch's app log, or "" where the driver keeps none.

    Best-effort corroboration for the failure message only: it holds the
    teardown line when the app is still the process that logged it, and is
    truncated out from under us when the driver relaunched (see `_app_pid`).
    Never assert on its absence.
    """
    reader = getattr(app.driver, "app_stderr_text", None)
    if reader is None:
        return ""
    try:
        return reader() or ""
    except Exception:
        return ""


def _quit_message(app) -> str:
    return (
        "the app QUIT at the LoggedIn terminal with an empty "
        "stored identity secret — its process was replaced, which only happens "
        "when the driver relaunches an app that died. That is the "
        "onboarding.md § Wizard exit handling violation this test pins: the "
        "terminal must take the secret from the machine (effective_secret()), "
        "which always has it here, not from the identity store, which holds it "
        "only if moment 1's confirm-identity write landed."
    )


def _diagnose(app, pid_before) -> str:
    """Failure text that names the cause, per convention 6."""
    if _app_pid(app) != pid_before:
        verdict = _quit_message(app)
    elif _TEARDOWN_LOG_MARK in _app_log(app):
        verdict = (
            "the app logged the forbidden teardown branch but its process "
            "survived — the terminal still gates on the identity store."
        )
    elif _WINDOWS_STALL_LOG_MARK in _app_log(app):
        verdict = (
            "the app logged the long-term-store stall and never reached the "
            "authenticated shell — both OnboardingViewModel.EffectiveSecret "
            "(the machine) AND secretStore.LoadSecret()/LoadNestUrl() (the "
            "fallback) came back empty."
        )
    else:
        verdict = (
            "the app neither quit nor logged the teardown branch, so this is "
            "a DIFFERENT failure — read the app log below."
        )
    tail = "\n".join(_app_log(app).splitlines()[-30:])
    return (
        f"never reached the authenticated shell after the claim. {verdict} "
        f"error={app.error_text()!r} {app.driver.diagnose('feed-view')}"
        + (f"\n--- app log (last 30 lines) ---\n{tail}" if tail else "")
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_logged_in_terminal_survives_an_empty_secret_slot(app, empty_store_unclaimed_nest):
    """Seed the identity through the machine only — the drive path that skips
    moment 1 — then claim to ``LoggedIn`` and require the same app process to
    reach the authenticated shell.

    RED before the fix: linux's terminal read the secret from the identity store,
    found it absent, and tore the wizard down, which quits the app on the
    non-append path.
    """
    secret_hex = secrets.token_bytes(32).hex()
    port = empty_store_unclaimed_nest["port"]
    handle = f"admin@127.0.0.1:{port}"

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, empty_store_unclaimed_nest)

    ob = app.onboarding
    ob.navigate_to_status()

    # The whole point: identity reaches the MACHINE and nothing else. This is
    # `seed_identity`, exactly as `helpers/live_provision.py::drive_provisioning`
    # does it — never `ob.import_key()`, whose UI path runs
    # `confirm_imported_identity` and would populate the identity store, hiding the
    # very state under test.
    app.driver.call_machine_method("seed_identity", json.dumps(secret_hex))

    ob.fill_handle(handle)
    ob.run_handle_check(timeout=45)
    ob.submit_handle()

    app.driver.wait_for("claim-code-input", timeout=_CLAIM_PAGE_BUDGET_S)
    app.driver.clear_and_type("claim-code-input", CLAIM_CODE)
    app.driver.click("claim-code-submit-button")

    # Pinned AFTER the claim and BEFORE the terminal, so the comparison spans
    # exactly the transition under test and nothing else.
    pid_before = _app_pid(app)

    # A successful claim lands on nat_mode_choice; dismissing it exits to
    # LoggedIn — the terminal under test. Poll both the dismissal and the
    # landing, since the post-claim launch may surface a retry screen first.
    def _landed():
        # Tolerate a raise: when the app quits, the driver notices mid-call and
        # relaunches it, so calls around that moment legitimately throw. The
        # verdict is the process identity below, not what this lookup does.
        try:
            if any(app.driver.is_visible(m) for m in _LOGGED_IN_MARKERS):
                return True
        except Exception:
            pass
        if pid_before is not None and _app_pid(app) != pid_before:
            pytest.fail(_quit_message(app))
        try:
            ob.finish_nat_mode()
        except Exception:
            pass
        for btn in ("launch-retry-button",):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        return False

    wait_until(
        _landed,
        _TERMINAL_BUDGET_S,
        interval=2.0,
        diagnose=lambda: _diagnose(app, pid_before),
    )

    # Arrival alone would not catch a quit whose relaunch happened to land
    # somewhere authenticated-looking, so assert the process identity outright.
    # Skipped only where the driver hides the process — the arrival assertion
    # above is the contract and runs on every app.
    if pid_before is not None:
        assert _app_pid(app) == pid_before, _quit_message(app)
