r"""tier_3 e2e (in-process, no real agent needed): macOS sign-out runs the
session-teardown checklist — the wiring proof for the `onSignOut` hook.

**The bug this proves fixed.** `SignOutSection` (shared FaunaKit, `sign-out-button` ->
`sign-out-confirm-button`) called only `StatusVM.signOut(sessionState:)` (durable
credential wipe) plus a per-platform `onSignedOut` closure that did nothing but flip
`MacAppState.isOnboarded` / `AppState.isOnboarding` — it never ran the "drop everything
the outgoing identity owns" checklist `switchAccount()`/the test-only `logoutKeepData()`
both run (`client?.shutdown()`, `unprovisionSyncAgent()`, DNS/subscription cadence stop,
File Provider sign-out). So tapping the REAL Sign Out button left the live `FaunaClient`
and its observers — and, on macOS, a real per-user `fauna-sync-agent` if one was
provisioned — running and acting under the identity the user believed they had signed
out of, until the app was quit. Fixed by wiring `AppState.onSignOut` /
`MacAppState.onSignOut` (mirroring `onFactoryReset`/`onSwitchAccount`) to the SAME
`tearDownSessionForSwitch()` + `FileProviderCoordinator.signOut()` checklist, and having
`SignOutSection`'s three callers (`SettingsShellView` macOS, `SettingsView` iOS x2)
invoke it before their own `isOnboarded`/`isOnboarding` flip (tracked internally).

**What this test proves, and what it deliberately does NOT.** `unprovisionSyncAgent()`
now logs both branches of its own nil-guard (`[sync-agent] unprovisioning` vs
`[sync-agent] unprovision skipped`) — added alongside this test specifically so a live
check can tell "the teardown ran and found nothing to unprovision" apart from "the
teardown never ran at all" (before this fix, a nil-provisioner early return and the
`onAccountReset` closure never calling `unprovisionSyncAgent` in the first place read
IDENTICALLY from the outside: silence). `logged_in_app` uses the fast `set_state()`
e2e-injection login path (`applySessionPatch`), which never runs the full boot machine
(`completeAuthenticatedLaunch`) and therefore usually builds no REAL
`syncAgentProvisioner` — so this is a proof that the WIRING fires, not an end-to-end
proof that a REAL agent gets unprovisioned. That stronger proof needs a genuinely
credential-seeded fresh launch (`macos.py`'s `seed_credentials`, which routes through
the real launch machine) driving a REAL `FfiChildAgentSpawner` agent, and it now EXISTS
as its own file, `test_sync_agent_unprovision_macos_real.py` — which is why the
assertion here stays on the shared `"[sync-agent] unprovision"` prefix and claims
nothing about which branch fired.

**Which branch it does see depends on the SELECTION, not on this file** (corrected
2026-09-22; it used to say "can only ever observe
`unprovision skipped`"). `_apply_real_sync_agent_env` is session-wide: if any selected
test carries the `real_sync_agent` marker — `test_sync_agent_survives_macos_window_close.py`
does, and a `--app macos` sweep selects it beside this one — then
`FAUNA_E2E_REAL_SYNC_AGENT` is on for the whole run, and `applySessionPatch`'s own
real-agent branch (retire-then-`startSyncAgentProvisioner`, added after this file was
written) builds a genuine provisioner even on the `set_state` login path. Since that
provisioner moved to `MacAppState`, the sign-out below reaches it and logs
`unprovisioning`; selected alone, this file still sees `unprovision skipped`. Both
satisfy the prefix, which is the point of asserting on the prefix.

**Two events emit the same log line — the offset is what disambiguates them.** The
session-cached `app`/`logged_in_app` driver (`_driver_cache`) reuses ONE app process
across the whole file, so its own housekeeping issues a `reset` (the test-only
`resetToFactory()`, which ALSO calls `unprovisionSyncAgent()`) before this test's login
is even injected — confirmed live while building this test (two `unprovision skipped`
lines appear within ~1s of process start, before any UI interaction). This test records
the log offset AFTER login settles and asserts only on the delta since the sign-out
click, so the fixture's own reset noise can never be mistaken for what the click causes.
"""
from __future__ import annotations

import time

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.macos, pytest.mark.destructive]

UNPROVISION_LOG_PREFIX = "[sync-agent] unprovision"


def _wait_for_log(driver, needle: str, offset: int, timeout: float = 20.0) -> str:
    """Poll the app's stderr until `needle` appears past `offset`, and return the
    delta. Raises with the delta for a self-diagnosing failure (convention 6).

    **Polled, not read once** (convention 14). `sign_out()` returns when the UI
    has landed on the wizard; the teardown checklist it triggered runs on past
    that — `tearDownSessionForSwitch` awaits two client shutdowns and an account
    runtime stop (which has its own 5 s budget) before `unprovisionSyncAgent()`
    ever logs. A single read at click-return therefore asserts on wall-clock
    luck: this file's read landed 0.8 ms ahead of the line it wanted, measured
    2026-09-22. The wait is bounded and returns the
    instant the line lands, so a green run costs nothing.
    """
    deadline = time.monotonic() + timeout
    delta = ""
    while time.monotonic() < deadline:
        delta = driver.app_stderr_text()[offset:]
        if needle in delta:
            return delta
        time.sleep(0.2)
    return delta


def test_macos_sign_out_runs_the_session_teardown_checklist(logged_in_app):
    """Extends `test_sign_out.py`'s cross-app `test_sign_out_returns_to_onboarding`
    (which only checks the UI landing state — already correct before this fix, since
    `onAccountReset` DID flip `isOnboarded`) with the assertion that gap actually
    hid: does sign-out also tear the live session down, not just re-route the UI."""
    app = logged_in_app
    driver = app.driver

    # Offset AFTER login has fully settled — excludes the driver-cache reuse
    # cleanup's own reset (which also logs an unprovision line; see module
    # docstring) so only the sign-out click's own effect is in the delta.
    offset = len(driver.app_stderr_text())

    app.settings.sign_out()

    delta = _wait_for_log(driver, UNPROVISION_LOG_PREFIX, offset)
    assert UNPROVISION_LOG_PREFIX in delta, (
        f"sign-out must run the session-teardown checklist (`tearDownSessionForSwitch`, "
        f"via the `onSignOut` hook) — regression of the fix where `SignOutSection`'s "
        f"`onAccountReset` only flipped `isOnboarded` and never tore the live session "
        f"down; error={app.error_text()!r}\napp.err delta since the click:\n"
        f"{delta[-4000:]}"
    )
