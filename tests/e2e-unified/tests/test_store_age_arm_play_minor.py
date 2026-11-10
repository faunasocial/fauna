"""The android store-age arm's minor path, driven through Google Play's real calls.

Subject: ``StoreAgeGlue``'s real Age Signals and Play Integrity calls — the
Play half of the store-age arm (``docs/goal/behavior/account-age-band.md``
§ The account age band, D3 store signals at onboarding, D5 provenance, and the
half-armed android arm). Every other test over the arm stands Play in: the two
age-notice tests of ``test_invite_request_states.py`` inject the claim through
the machine's ``set_age_claim``; the JVM ``StoreAgeClaimTest`` drives
``StoreAgeClaim.build`` over a fake Play half;
``api/test_registration_posture.py::test_a_sign_up_reported_as_a_minor_is_pointed_to_a_guardian``
pins the nest alone. Here nothing is injected: the app's own invite-request
submit runs ``OnboardingHost.attachStoreAgeClaim``, which asks the Play Store on
the device, and the nest's request row is what the admin reads.

**tier_4** — the deciding dependency is a real deployed service the harness
cannot stand in for: Google Play on a Play-image device, signed in to the
internal-testing tester account Google placed under its under-18 defaults
(``account-age-band.md`` § Implementation status today, the paragraph on the
store-age arm's minor-path fixture; ``installers/android.md`` § Implementation
status today, the Play tester accounts bullet). The debug app carries the real
Play twin (``src/storeAge/``: every Play-distributed build type, ``debug``
among them — only ``foss`` compiles the inert one).

**Environment.** Run on the fauna-e2e-play AVD:
``--app android --device-serial emulator-5556``.
Without an attached device carrying the Play Store and a signed-in Google
account, the test skips as ``skip_environment`` naming that AVD; the Play Store
sign-in itself is the owner's step on that AVD.

**The outcomes.** What Play returns for the fixture account is not known from
the code, so the test separates three:

1. **A minor band is recorded** — the row carries ``u13`` / ``13-15`` /
   ``16-17`` with provenance ``attested-android`` when the nest lists
   ``android`` in ``attestation_platforms``, ``none`` (declared-only) when it
   does not. Asserted.
2. **No claim at all** — Play shared no range for the account (declined, not
   eligible, or an error the glue degraded on). The row carries neither band
   nor provenance. That is a FINDING to record, not a failure: the test passes,
   records it as a ``play_age_signal`` property with the glue's own degrade
   reasons from logcat, and warns so the run's summary shows it.
3. **Anything else** — an adult band, a band whose provenance disagrees with
   the nest's platform list, a provenance with no band — fails, naming the
   glue's degrade reasons.

**Play's own consent prompt.** The Age Signals access request may show a Play
dialog the bridge cannot answer; a submit that never reaches PendingReview
fails with the page's state and the glue's log lines, which say so.

**Convention 14.** Every wait is a deadline poll on state the app or the nest
reaches, under a named ceiling a green run never spends.
"""

from __future__ import annotations

import json
import time
import warnings

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import android_store_driver as store
from helpers.app_surface import require_surface, skip_environment

pytestmark = [pytest.mark.tier_4, pytest.mark.android]

#: The device this file's tests need: the verify queue's drain reads it and
#: runs a request for this file on that serial, not the venue's own AVD
#: (delayed-testing.md § The verify queue, the device rule).
E2E_DEVICE_SERIAL = "emulator-5556"

#: The AVD this witness needs, named for the environment skip.
PLAY_AVD = (
    f"the fauna-e2e-play AVD (serial {E2E_DEVICE_SERIAL}, a "
    "Google Play system image) with the Play Store signed in to the under-18 "
    "internal-testing tester account; run with `--app android --device-serial "
    f"{E2E_DEVICE_SERIAL}`"
)

#: The submit round trip with the Play round in front of it: the Age Signals
#: request, the nonce mint, the Integrity verdict and the anonymous submit. A
#: ceiling, spent only by a failure.
SUBMIT_BUDGET_S = 120.0

#: The wire tokens of the minor bands (`fauna_protocol::age::AgeBand`).
MINOR_BANDS = frozenset({"u13", "13-15", "16-17"})

#: The glue's degrade line (`OnboardingHost.attachStoreAgeClaim`).
DEGRADE_MARK = "store-age claim degraded:"


class PlayAgeSignalFinding(UserWarning):
    """Play shared no age range for the fixture account — a recorded finding."""


@pytest.fixture
def play_signed_in_device():
    """A device with the Play Store and a signed-in Google account, or an
    environment skip naming the AVD. The account's name is never read: only
    that one of type ``com.google`` exists."""
    if not store.device_attached():
        skip_environment(f"no android device attached; this witness needs {PLAY_AVD}")
    if not store.package_installed(store.PLAY_PACKAGE):
        skip_environment(
            f"the attached device has no Play Store ({store.PLAY_PACKAGE}); "
            f"this witness needs {PLAY_AVD}"
        )
    if "type=com.google" not in store.adb_shell("dumpsys", "account"):
        skip_environment(
            f"the attached device's Play Store is signed in to no Google account; "
            f"this witness needs {PLAY_AVD}"
        )
    return True


def _admin_client(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _degrade_reasons() -> list[str]:
    """The glue's own degrade lines from the device's log — what Play refused,
    or why the attestation was skipped. App-side Kotlin `Log.w` lines."""
    out = store.adb_shell("logcat", "-d", "-s", "OnboardingHost:W", timeout=30)
    return [ln.split(DEGRADE_MARK, 1)[1].strip() for ln in out.splitlines() if DEGRADE_MARK in ln]


def _status_text(app) -> str:
    try:
        return app.driver.get_text("invite-request-status")
    except Exception as exc:  # a read failure is itself a reportable fact
        return f"<unreadable: {exc}>"


def _wait_pending_review(app) -> None:
    deadline = time.monotonic() + SUBMIT_BUDGET_S
    while time.monotonic() < deadline:
        if app.driver.is_visible("invite-request-recheck-button"):
            return
        time.sleep(0.3)
    raise AssertionError(
        "the real submit (Play round first) never reached PendingReview within "
        f"{SUBMIT_BUDGET_S}s: {app.driver.diagnose('invite-request-recheck-button')} "
        f"status={_status_text(app)!r} error={app.error_text()!r} "
        f"glue degrade reasons={_degrade_reasons()!r} — a Play consent dialog the "
        "bridge cannot answer would hold the submit here"
    )


def _request_row(admin_ws, handle: str) -> dict:
    rows = admin_ws.call("fauna.admin.invite_requests.list", {})["invite_requests"]
    mine = [r for r in rows if r["handle"] == handle]
    assert len(mine) == 1, (
        f"expected exactly one invite request for {handle!r}, the nest holds {len(mine)}: "
        f"{[{k: r.get(k) for k in ('id', 'status', 'age_band', 'age_provenance')} for r in mine]!r}"
    )
    return mine[0]


def test_a_play_minor_submits_an_invite_request_carrying_the_store_band(
    play_signed_in_device, app, nest_instance, record_property
):
    # `play_signed_in_device` first: a box with no Play-image device skips
    # before the app is launched at all.
    require_surface(app.driver, "machine-async-dispatch-bridge")
    nest_url = nest_instance["url"]
    handle = f"playminor{int(time.time()) % 1_000_000}"

    # The requester's half, through the app's own UI: a fresh identity, the
    # wizard on this nest's invite-request page (a test nest is not
    # DNS-discoverable, so the known-nest hop stands in for the handle lookup,
    # as in `test_pending_invite_journey.py`).
    store.adb_shell("logcat", "-c")
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(bytes(SigningKey.generate()).hex())
    app.driver.wait_for("handle-input", timeout=15)
    app.driver.call_machine_method(
        "navigate_to_invite_request_for_known_nest", json.dumps([nest_url, handle]),
    )
    app.driver.wait_for("invite-request-submit-button", timeout=20)

    # Which provenance the nest can grant: its own platform list, read the way
    # the glue reads it (the glue mints its own nonce at submit).
    raw = app.driver.call_machine_method("request_age_nonce", "")
    minted = json.loads(raw) if isinstance(raw, str) else raw
    assert minted and "Ok" in minted, f"the nonce mint must succeed: {minted!r}"
    armed = "android" in minted["Ok"]["attestation_platforms"]

    app.driver.click("invite-request-submit-button")
    _wait_pending_review(app)

    row = _request_row(_admin_client(nest_instance), handle)
    band, provenance = row.get("age_band"), row.get("age_provenance")
    reasons = _degrade_reasons()
    record_property("play_age_signal", json.dumps({
        "armed": armed, "band": band, "provenance": provenance, "degrade_reasons": reasons,
    }))

    if band is None:
        # Outcome 2: Play shared no range — the finding this witness exists to record.
        assert provenance is None, (
            f"a row with no band carries provenance {provenance!r}: the nest recorded "
            "a claim's provenance with no claim"
        )
        warnings.warn(PlayAgeSignalFinding(
            f"Play shared no age range for the signed-in account (armed={armed}); "
            f"the glue's degrade reasons: {reasons!r}"
        ))
        return

    assert band in MINOR_BANDS, (
        f"the fixture account is under Google's under-18 defaults, but Play's range "
        f"folded to {band!r}, not a minor band; degrade reasons={reasons!r}"
    )
    want = "attested-android" if armed else "none"
    assert provenance == want, (
        f"band {band!r} recorded with provenance {provenance!r}, but the nest "
        f"{'lists' if armed else 'does not list'} android in attestation_platforms, "
        f"so it must be {want!r}; degrade reasons={reasons!r}"
    )
