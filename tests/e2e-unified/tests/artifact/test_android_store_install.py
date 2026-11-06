"""The Android app as GOOGLE PLAY delivered it — obtained from the store, then driven.

Subject: the app the Play Store installed on a device — the split set Play
delivers, signed with Google's app signing key — not the debug APK the harness
bridge installs, and not the universal APK Play's Developer API generates
(``adb install`` of that is the shell installing, which is not the store
install and never colours this outcome). The ruling, its reasons and the
human inch: ``docs/goal/architecture/feature-catalog.md`` § Implementation
status today, the bullet on how the Android store install is driven.

**Why a driver of its own.** The harness bridge instruments
``social.fauna.fauna`` in the app's own UID, and Android attaches an
instrumentation only to a package signed with the instrumentation's own
certificate; Play re-signs every delivery, so that bridge can never reach a
store install. The store driver (``apps/fauna-android/store-driver/``, built by
``just android-store-driver``) is a self-instrumenting APK with a package and
key of its own that reaches the app only through ``UiAutomation`` — the ids the
release build keeps in the accessibility tree (``testTagsAsResourceId``,
``e2e-automation-surface-gating.md`` § Implementation status today). It has
none of the bridge's same-UID powers, so the journey signs in through the
app's own UI. Host half: ``helpers/android_store_driver.py``.

**The subject is asserted on two facts, because either alone is matched by
something else:** the installer of record is the Play Store, and the
installed certificate is Play's app signing key
(``installers/android.md`` § Implementation status today, the Play signing
keys). An ``adb install`` of the Play-generated universal APK matches the
second and not the first; a debug build matches neither.

**Environment.** It needs a device with the Play Store, signed in to an
account on the internal testers list — a second AVD on a Google Play system
image beside the venue's ``google_apis`` one. Absent that, every test here skips as ``skip_environment`` naming the AVD.
The sign-in leg needs no input of its own: it signs in on ``test.fauna.build``,
the staging box on the UAT channel, as this machine's own tester
(``linux-tester``, minted here by ``just org-tester-mint`` and admitted by the
provisioning drive from the tracked registry of public keys) — or, on the dev
host, where the drive keeps the reviewers' identities, as Google Play's
reviewer account — importing the identity file, exactly the steps a Play
reviewer takes (``helpers/store_install_account.py`` says why that nest and
which account). Missing both, the sign-in test skips as ``skip_environment``
naming the recipe. The device's
network must be one the box's cloud firewall admits (``org-nests.md``
§ fauna.build → *Exposure*), as for the store reviewer journey run from the
dev host.

**On the shared box** (``testing.md`` § The shared-box rule, non-destructive
carve-out): the journey only signs in and reads the feed — no content, no
setting, nothing of anyone else's. It leaves one device row on the account's
roster, as every fresh install does; the store reviewer journey's sign-in
(``helpers.org_account_session.clear_stale_devices``) removes it on its next
run, and so would ``just e2e-store-reviewer test.fauna.build -k google``.

**Not repeatable on the same device without an uninstall.** If the app is
already installed, the module refuses to adopt it unless its installer of
record is the Play Store — a sideloaded copy left by another suite would
otherwise pass for the store install. To run again, uninstall it from the
device (``adb uninstall social.fauna.fauna``) so the store installs it afresh.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from common import get_repo_root
from helpers import android_store_driver as store
from helpers.android_store_driver import StoreDriver
from helpers.app_surface import skip_environment
from helpers.store_install_account import PLAY_ACCOUNT, witness_account

# tier_4: the subject is a shipped client artifact as its channel delivered it
# (`testing.md` § The four-tier taxonomy, point 4's shipped-client-artifact arm).
pytestmark = [pytest.mark.tier_4, pytest.mark.android]

@pytest.fixture(scope="module")
def play_device():
    """A device with the Play Store on this run's adb server, or an
    environment skip naming the AVD the witness needs."""
    if not store.device_attached():
        skip_environment(f"no android device attached; the store-install witness needs {store.PLAY_AVD_NEED}")
    if not store.package_installed(store.PLAY_PACKAGE):
        skip_environment(
            f"the attached device has no Play Store ({store.PLAY_PACKAGE}); "
            f"the store-install witness needs {store.PLAY_AVD_NEED}"
        )
    return True


@pytest.fixture(scope="module")
def driver_apk() -> Path:
    """The store driver APK ``just android-store-driver`` built — read, never
    built here (convention 9's prebuild bullet): the build is the recipe's."""
    apk = Path(get_repo_root()) / store.DRIVER_APK
    if not apk.is_file():
        pytest.fail(
            f"the store driver APK {apk} is missing — run `just android-store-driver` "
            f"first (it builds once the bump round admits the module's dependencies: "
            f"the head of apps/fauna-android/store-driver/build.gradle.kts)",
            pytrace=False,
        )
    return apk


@pytest.fixture(scope="module")
def store_driver(play_device, driver_apk):
    with StoreDriver(driver_apk) as driver:
        yield driver


@pytest.fixture(scope="module")
def store_install(store_driver):
    """Obtain the app the way a tester does: open its Play listing, press
    Install, and wait for the package manager to have it.

    An app already on the device is adopted only if the Play Store installed
    it; anything else is a sideload this module must not mistake for its
    subject, so it fails and says how to clear the device.
    """
    if store.package_installed(store.FAUNA_PACKAGE):
        installer = store.installer_of_record()
        assert installer == store.PLAY_PACKAGE, (
            f"{store.FAUNA_PACKAGE} is already installed, by {installer or 'the shell'}, "
            f"not by the Play Store — it is not the store install this module witnesses. "
            f"Uninstall it (`adb uninstall {store.FAUNA_PACKAGE}`) and run again."
        )
        return store_driver
    assert store_driver.open_listing(), "the Play Store never came to the front on the app's listing"
    answer = store_driver.press_install()
    state = answer.get("state")
    if state == "signed_out":
        skip_environment(
            "the Play Store on the attached device is signed out — the one-time "
            "sign-in to an account on the internal testers list is the owner's step "
            f"({store.PLAY_AVD_NEED})"
        )
    if state == "not_available":
        skip_environment(
            "the Play Store does not serve the app's listing to the signed-in account "
            "on this device — the account must be on the internal testers list"
        )
    assert state in ("pressed", "installed"), (
        f"the app's Play listing showed no Install button (state {state!r}); "
        f"visible texts: {answer.get('texts')}"
    )
    assert store.wait_for_install(), (
        f"Install was pressed but {store.FAUNA_PACKAGE} was not installed within "
        f"{store.INSTALL_BUDGET_S:.0f}s"
    )
    return store_driver


def _assert_the_subject() -> None:
    """The two subject facts, asserted by every test that colours anything."""
    installer = store.installer_of_record()
    assert installer == store.PLAY_PACKAGE, (
        f"installer of record is {installer!r}, not the Play Store — this is not "
        f"the store install (an `adb install` names the shell)"
    )
    digests = store.installed_certificate_digests()
    assert store.PLAY_APP_SIGNING_CURRENT in digests, (
        f"the installed APK is not signed with Play's app signing key "
        f"{store.PLAY_APP_SIGNING_CURRENT}; apksigner read {digests}"
    )
    foreign = [d for d in digests if d not in store.PLAY_APP_SIGNING_KEYS]
    assert not foreign, (
        f"the installed APK carries certificates that are not Play's app signing "
        f"keys: {foreign} (the upload key, or a debug key, means it is not a Play delivery)"
    )


def test_the_store_installs_the_play_signed_app(store_install):
    """The obtain half and the subject: the Play Store installed the app, and
    what it installed is signed with Play's app signing key."""
    _assert_the_subject()


@pytest.mark.feature("get-the-app")
def test_the_store_installed_app_signs_in_to_the_feed(store_install):
    """``get-the-app`` outcome 1, the phone arm: installed from the store, the
    app opens and you are signed in to your account.

    It re-asserts its own subject before its outcome — the linux module's
    lesson (``test_linux_installed_product.py``): a narrowed run must not
    colour this cell green while driving something else.
    """
    _assert_the_subject()
    account = witness_account(PLAY_ACCOUNT)
    driver = store_install
    driver.stop_app()
    driver.launch_app()
    # The release build opens on onboarding's identity choice — through the
    # app's own UI from here on; nothing is seeded. Import the witness's
    # identity, as the Play reviewer does from the review notes.
    driver.wait_for("import-identity-button", timeout=60)
    driver.click("import-identity-button")
    driver.wait_for("paste-secret-field", timeout=15)
    driver.clear_and_type("paste-secret-field", account.secret_hex)
    driver.click("import-submit-button")
    driver.wait_for("handle-input", timeout=30)
    driver.clear_and_type("handle-input", account.handle)
    driver.click("handle-check-button")
    _wait_enabled(driver, "handle-entry-continue-button", 60)
    driver.click("handle-entry-continue-button")
    driver.wait_for("feed-tab", timeout=180)
    assert driver.is_visible("feed-tab"), (
        f"the store-installed app never reached the feed after signing in as "
        f"{account.handle!r}; error element: {driver.error_text()!r}"
    )


def _wait_enabled(driver, element_id: str, timeout: float) -> None:
    import time

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:  # deadline-ok: waits on the button's enabled state
        if driver.is_visible(element_id) and driver.is_enabled(element_id):
            return
        time.sleep(0.5)  # sleep-ok: poll interval of a state read
    raise AssertionError(
        f"'{element_id}' never enabled — the handle check did not pass. "
        f"handle-message-area: {driver.get_text('handle-message-area')!r}; "
        f"{driver.diagnose(element_id)}"
    )
