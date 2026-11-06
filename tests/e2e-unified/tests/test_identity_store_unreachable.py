"""No protected store reachable → the app says so, and keeps the key nowhere
less protected.

`docs/features/identity-protected-on-this-device.md` outcome 2;
`docs/goal/architecture/apps/common.md` § Credential storage (the tui row: the
OS-native store when one answers, otherwise the passphrase-sealed file) and
`docs/goal/architecture/apps/tui.md` § Credential storage.

**tui's column.** tui's declared answer to "no protected store" is not an
error: it is the sealed arm, and it tells the person so before anything is
stored — the create surface, whose only prompt is the one saying there is no
secure key store here and that the keys will be kept encrypted with the
passphrase they choose. So the witness asserts exactly that, reached the way a
real SSH box reaches it: the launch's session bus answers no Secret Service
(an empty private bus, the driver's ``probed_store`` mode — no
``FAUNA_E2E_CREDENTIAL_DIR``, and no test-only force var either, so the
store's OWN probe decides, cause ``KeyringAbsent``). Then the "not quietly
somewhere less protected" half: after an identity is imported, the sealed
file is the one place it rests — no file under the launch's XDG base holds
the secret in any encoding, and the next launch asks for the passphrase
before it can read the identity back.

**linux's column** (the second test). linux has no sealed arm, so its promise
is a loud error (`apps/linux.md` § Credential Storage): on the same empty bus,
a ``use_real_keyring`` launch (real libsecret, no ``FAUNA_E2E_CREDENTIAL_DIR``)
generates an identity and confirms it, and the confirm-identity write fails.
The shared machine's ``identity_commit_failed`` puts the wizard back on the
screen the person confirmed on with the error in ``error-message``
(`apps/fauna-linux/src/views/onboarding/mod.rs::commit_confirmed_identity`).
The witness asserts the error is painted, the key on screen is still the one
the person was shown, the wizard did not advance to the recovery kit or
handle entry, and no file under the launch's XDG base holds the key. Before
2026-10-21 the write was log-only and the wizard advanced with nothing on
screen, so every assertion here was red against that code.

**macOS's and iOS's column** (the third test). One keychain and no file arm,
so linux's promise — a loud error — through the same shared
``identity_commit_failed``; the fault is the shared registry bridge's
``refuse_secret_writes_for_test``, the one way to make a keychain refuse on
demand (the test's own docstring says why that stands for "cannot be
reached").

**web's column** (the fifth test). A browser offers a page no protected
store at all, so web's answer is tui's: the sealed arm, said on the shared
``unlock`` page's create surface before anything is stored, and an identity
found nowhere in the origin's storage but sealed.

**android's column** (the fourth test). The Keystore-sealed store has no file
arm either, so the promise and the body are apple's — one shared body
(``_a_refused_confirm_is_loud_and_the_wizard_stays_put``), the same shared
fault, reached through the android agent's registry arm, and the shared
``identity_commit_failed`` reached through ``ConfirmCommit.kt``.

**windows' column** (the sixth test). Credential Manager is one more store with
no file arm in a shipped build, so the same body again, through
``TestAgent.cs``'s registry arm; it runs in the ordinary suite, on the launch's
file store, because the fault needs no real row.
"""

from __future__ import annotations

import base64
import contextlib
import os
import re
import time
from pathlib import Path

import pytest
from nacl.signing import SigningKey

from conftest import get_available_apps
from drivers import create_driver
from drivers.session_bus import PrivateSessionBus
from helpers.app_surface import skip_environment
from helpers.tui_headless_store import CONFIRM_INPUT, PASSPHRASE_INPUT, submit_passphrase
from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.feature("identity-protected-on-this-device"),
]

PASSPHRASE = "correct-horse-battery"
NAMESPACE = "fauna-tui-unreachable-store-e2e"
LOCKED_NOTICE = "unlock-keyring-locked-notice"

_ANSI = re.compile(r"\x1b(\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(\x07|\x1b\\)|[@-Z\\-_])")


@pytest.fixture(params=["tui"])
def launch_client(request):
    """tui-only, the client id in the test name for conftest's ``--app``
    filter (the ``test_tui_headless_credential_store.py`` shape)."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-tui is not available on this machine")
    return request.param


@pytest.fixture
def empty_bus():
    """A session bus that answers no Secret Service — "no protected store
    reachable", without touching the box's own bus."""
    import shutil

    if shutil.which("dbus-daemon") is None:
        skip_environment("dbus-daemon is not installed")
    bus = PrivateSessionBus().start()
    try:
        yield bus
    finally:
        bus.stop()


def _painted_text(stream_path: str) -> str:
    """Letters-only reading of everything the app painted: escapes, box
    drawing and whitespace dropped, so a prompt wrapped across rows still
    reads as one run of letters."""
    raw = Path(stream_path).read_bytes().decode("utf-8", errors="replace")
    return re.sub(r"[^A-Za-z]", "", _ANSI.sub("", raw))


def _encodings(secret: bytes) -> list[bytes]:
    return [
        secret,
        secret.hex().encode(),
        secret.hex().upper().encode(),
        base64.b64encode(secret),
        base64.urlsafe_b64encode(secret),
        base64.b64encode(secret).rstrip(b"="),
        base64.urlsafe_b64encode(secret).rstrip(b"="),
    ]


def _files_holding(root: Path, needles: list[bytes]) -> list[str]:
    hits = []
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        with contextlib.suppress(OSError):
            data = path.read_bytes()
            if any(n in data for n in needles):
                hits.append(str(path.relative_to(root)))
    return sorted(hits)


def _launch(tui_app_path, nest, xdg_base, bus, request):
    from conftest import _seeded_environment

    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest["url"],
        "probed_store": True,
        "xdg_base": str(xdg_base),
        "keyring_app": NAMESPACE,
        "environment": {
            **_seeded_environment(request, nest),
            "DBUS_SESSION_BUS_ADDRESS": bus.address,
        },
    })
    return driver


@pytest.mark.tui
# A private session bus is a linux box's (convention 7, the fourth skip kind); the
# environment skip in `empty_bus` stays for a linux box with no dbus daemon.
@pytest.mark.host_platform("linux")
def test_no_protected_store_is_said_and_the_key_rests_only_sealed(
    launch_client, tui_app_path, nest_instance, empty_bus, tmp_path, request
):
    from actions import ActionLayer

    secret = bytes(SigningKey.generate())
    xdg_base = tmp_path / "xdg"
    sealed = xdg_base / "config" / "fauna-tui" / f"{NAMESPACE}.sealed"

    driver = _launch(tui_app_path, nest_instance, xdg_base, empty_bus, request)
    try:
        # ── 1. The store's own probe found nothing, and the app SAYS so before
        # anything is stored: the create surface, with its prompt. ──────────
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        assert driver.is_visible(PASSPHRASE_INPUT)
        want = re.sub(r"[^A-Za-z]", "", S.unlock.create_prompt.split(",")[0])
        deadline = time.monotonic() + 10
        while want not in _painted_text(driver.frame_stream_path()):
            if time.monotonic() > deadline:
                pytest.fail(
                    "the create surface is up but its prompt — the words that tell the "
                    "person no secure key store is here — was never painted "
                    f"(looked for {S.unlock.create_prompt.split(',')[0]!r}); "
                    f"error={driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}"
                )
            time.sleep(0.5)
        assert not sealed.exists(), "a sealed file existed before the person chose a passphrase"
        assert driver.is_absent(LOCKED_NOTICE), (
            "no store answered at all (cause KeyringAbsent), yet the page says a "
            "keyring is there but locked — the notice is the locked cause's alone"
        )

        # ── 2. Choose the passphrase, then import an identity through the
        # wizard: the confirm is the durable write. ─────────────────────────
        submit_passphrase(driver, PASSPHRASE, confirm=PASSPHRASE)
        app = ActionLayer(driver)
        app.onboarding.navigate_to_status()
        app.onboarding.import_key(secret.hex())
        driver.wait_for("handle-input", timeout=30)

        # ── 3. Nowhere less protected: the sealed file exists and nothing
        # under the launch's XDG base holds the secret in any encoding. ─────
        assert sealed.is_file(), (
            f"no sealed store at {sealed} after the identity was confirmed; "
            f"error={app.error_text()!r}"
        )
        hits = _files_holding(xdg_base, _encodings(secret))
        assert not hits, (
            "the identity secret rests readable outside the sealed store, in: "
            f"{hits} (the 'somewhere less protected' outcome 2 forbids)"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()

    # ── 4. The next launch cannot read the identity without the passphrase:
    # it asks (unlock mode, no confirm), then finds the identity. ──────────
    driver = _launch(tui_app_path, nest_instance, xdg_base, empty_bus, request)
    try:
        driver.wait_for(PASSPHRASE_INPUT, timeout=30)
        assert driver.is_absent(CONFIRM_INPUT), (
            "the relaunch offered to CREATE a store — the identity was not kept "
            "in the sealed file"
        )
        submit_passphrase(driver, PASSPHRASE)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if driver.is_visible("handle-input") or driver.is_visible("create-identity-button"):
                break
            time.sleep(0.5)
        assert driver.is_absent("create-identity-button"), (
            "unlocked, but the identity was not found — it was never in the sealed store"
        )
        assert driver.is_visible("handle-input")
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()


@pytest.mark.tui
@pytest.mark.host_platform("linux")
def test_a_locked_keyring_is_said_on_the_create_surface_and_tui_still_starts(
    launch_client, tui_app_path, nest_instance, tmp_path, request
):
    """The other half of outcome 2 on tui: a protected store that ANSWERED but
    is locked (cause ``KeyringLocked``, the driver's ``locked_keyring`` launch —
    the store's own probe runs and answers locked) is reported on the create
    surface by ``unlock-keyring-locked-notice`` with the reviewed copy, and the
    app still starts: the passphrase is chosen and launch routing runs
    (``tui.md`` § Credential storage, ruled 2026-10-16 — a notice, never a
    refusal). Once the passphrase is chosen the notice is gone with the page."""
    from conftest import _seeded_environment

    xdg_base = tmp_path / "xdg"
    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "url": nest_instance["url"],
        "locked_keyring": True,
        "xdg_base": str(xdg_base),
        "keyring_app": NAMESPACE + "-locked",
        "environment": _seeded_environment(request, nest_instance),
    })
    try:
        driver.wait_for(CONFIRM_INPUT, timeout=30)
        assert driver.is_visible(LOCKED_NOTICE), (
            "the keyring answered locked, yet the create surface says nothing of it: "
            f"{driver.diagnose(LOCKED_NOTICE)}"
        )
        painted = " ".join(driver.get_text(LOCKED_NOTICE).split())
        assert painted == " ".join(S.unlock.keyring_locked_notice.split()), (
            f"the notice must carry the reviewed copy; painted {painted!r}"
        )

        submit_passphrase(driver, PASSPHRASE, confirm=PASSPHRASE)
        deadline = time.monotonic() + 30
        while driver.is_visible(CONFIRM_INPUT):
            if time.monotonic() > deadline:
                pytest.fail(
                    "a locked keyring must not stop the start: the passphrase was "
                    "chosen but the create surface stayed up; "
                    f"error={driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}"
                )
            time.sleep(0.5)
        assert driver.is_absent(LOCKED_NOTICE), (
            "the notice outlived the unlock page it belongs to"
        )
        assert (xdg_base / "config" / "fauna-tui" / f"{NAMESPACE}-locked.sealed").is_file(), (
            "the passphrase was chosen, yet no sealed store was created"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()


@pytest.fixture(params=["linux"])
def linux_client(request):
    """linux-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("fauna-linux is not available on this machine")
    return request.param


@pytest.mark.linux
def test_no_protected_store_is_a_loud_error_and_the_wizard_stays_put(
    linux_client, linux_app_path, nest_instance, empty_bus, tmp_path, request
):
    from conftest import _seeded_environment

    xdg_base = tmp_path / "xdg"
    xdg_base.mkdir()
    driver = create_driver("linux")
    # Real libsecret persistence over a bus that answers no Secret Service:
    # the production path, with no file backend to fall back on
    # (`drivers/linux.py` `build_launch_env`, `use_real_keyring`).
    driver.launch({
        "app_path": linux_app_path,
        "url": nest_instance["url"],
        "use_real_keyring": True,
        "keyring_app": "fauna-linux-unreachable-store-e2e",
        "xdg_base": str(xdg_base),
        "environment": {
            **_seeded_environment(request, nest_instance),
            "DBUS_SESSION_BUS_ADDRESS": empty_bus.address,
        },
    })
    try:
        driver.wait_for("create-identity-button", timeout=30)
        driver.click("create-identity-button")
        driver.wait_for("secret-key-display", timeout=10)
        shown = driver.get_text("secret-key-display").strip()
        assert re.fullmatch(r"[0-9a-f]{64}", shown), f"no key on screen: {shown!r}"
        driver.click("identity-continue-button")

        # ── 1. Loud: the error is painted — waited on as state, the line the
        # click settles into, never a delay. ─────────────────────────────────
        deadline = time.monotonic() + 30
        error = ""
        while time.monotonic() < deadline:
            if driver.is_visible("error-message"):
                error = driver.get_text("error-message").strip()
                if error:
                    break
            time.sleep(0.5)
        assert error, (
            "confirming an identity with no Secret Service reachable painted no "
            "error — the key is stored nowhere and the person was not told; "
            f"handle-input visible={driver.is_visible('handle-input')}"
        )

        # ── 2. The wizard did not advance as if the key were saved: still on
        # the key's own screen, showing the same key. ────────────────────────
        assert driver.is_visible("secret-key-display"), (
            f"the wizard left the key's screen after a failed save; error={error!r}"
        )
        assert driver.get_text("secret-key-display").strip() == shown, (
            "the key on screen changed after the failed save — the one the "
            "person was told to write down would be worthless"
        )
        for advanced in ("handle-input", "recovery-kit-confirm-button"):
            assert driver.is_absent(advanced), (
                f"{advanced} is up: the wizard advanced past a confirm whose key "
                f"was saved nowhere; error={error!r}"
            )

        # ── 3. Nowhere less protected: no file under the launch's XDG base
        # holds the key in any encoding. ─────────────────────────────────────
        hits = _files_holding(xdg_base, _encodings(bytes.fromhex(shown)))
        assert not hits, (
            f"the identity secret rests readable on disk, in: {hits} — linux has "
            "no file arm, so a failed keyring write must leave it nowhere"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()


@pytest.fixture(params=["macos", "ios"])
def apple_client(request):
    """The two keychain columns, the client id in the test name for
    conftest's ``--app`` filter (an indirect parametrization, for the reason
    ``test_account_switcher_apple.py``'s ``app_name`` fixture gives)."""
    if request.param not in get_available_apps():
        pytest.skip(f"{request.param} is not in this run's app set")
    return request.param


def _stored_copies(driver, secret: bytes) -> list[str]:
    """Where the app's e2e credential store holds ``secret`` in any encoding:
    the harness's credential dir on apple, the on-device credential file
    (``credential_map``, read over the bridge) on android."""
    needles = _encodings(secret)
    if hasattr(driver, "credential_map"):
        return sorted(
            key for key, value in driver.credential_map().items()
            if any(n in str(value).encode() for n in needles)
        )
    cred_dir = getattr(driver, "_cred_dir", None)
    return _files_holding(Path(cred_dir), needles) if cred_dir else []


def _a_refused_confirm_is_loud_and_the_wizard_stays_put(app_name, nest_instance, request):
    """The body both registry-fault columns run: arm the shared registry
    bridge's ``refuse_secret_writes_for_test`` (the store takes the write and
    keeps nothing; the shared ``persist_confirmed_identity`` read-back
    refuses), create an identity, confirm it with a real tap, and require the
    shared ``identity_commit_failed`` outcome — the error painted, the wizard
    still on the key's own screen with the same key, nothing past it, and no
    copy of the key in the app's e2e credential store."""
    from conftest import _seeded_environment
    from helpers.app_launch import app_launch_config
    from helpers.waiting import wait_until

    driver = create_driver(app_name)
    driver.launch({
        "url": nest_instance["url"],
        "environment": _seeded_environment(request, nest_instance),
        **app_launch_config(app_name, request),
    })
    try:
        driver.wait_for("create-identity-button", timeout=60)
        driver.call_machine_method("refuse_secret_writes_for_test", '{"refuse": true}')
        driver.click("create-identity-button")
        driver.wait_for("secret-key-display", timeout=15)
        shown = driver.get_text("secret-key-display").strip()
        assert re.fullmatch(r"[0-9a-f]{64}", shown), "no 64-hex key on screen"
        driver.click("identity-continue-button")

        # ── 1. Loud: the error is painted — waited on as state. ─────────────
        def _error():
            if driver.is_visible("error-message"):
                return driver.get_text("error-message").strip()
            return ""

        error = wait_until(
            _error, 30,
            diagnose=lambda: (
                "confirming an identity the store kept nothing of painted no "
                "error — the person was not told the key is stored nowhere; "
                f"handle-input visible={driver.is_visible('handle-input')}"
            ),
        ).replace(shown, "<key>")

        # ── 2. The wizard did not advance as if the key were saved. ─────────
        assert driver.is_visible("secret-key-display"), (
            f"the wizard left the key's screen after a failed save; error={error!r}"
        )
        assert driver.get_text("secret-key-display").strip() == shown, (
            "the key on screen changed after the failed save — the one the "
            "person was told to write down would be worthless"
        )
        for advanced in ("handle-input", "recovery-kit-confirm-button"):
            assert driver.is_absent(advanced), (
                f"{advanced} is up: the wizard advanced past a confirm whose key "
                f"was saved nowhere; error={error!r}"
            )

        # ── 3. Nowhere less protected: the app's e2e credential store holds
        # no copy of the key. ────────────────────────────────────────────────
        hits = _stored_copies(driver, bytes.fromhex(shown))
        assert not hits, (
            f"the identity secret rests in the credential store, in: {hits} "
            "— the refused write kept a copy after all"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.call_machine_method("refuse_secret_writes_for_test", '{"refuse": false}')
        with contextlib.suppress(Exception):
            driver.teardown()


@pytest.mark.macos
@pytest.mark.ios
def test_a_keychain_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put(
    apple_client, nest_instance, request
):
    """macOS and iOS: the keychain write of a confirmed identity fails → the
    wizard says so and stays on the key's own screen.

    `apps/common.md` § Credential storage: a store failure "is a loud error,
    not a silent fallback", and the apple apps have no fallback arm — one
    keychain, so the promise is linux's (the test above). What an apple
    keychain does when it cannot be reached is refuse the write
    (`errSecInteractionNotAllowed` before first unlock, `errSecMissingEntitlement`
    on a signature that cannot reach the access group). Nothing a user does
    produces that on demand, so the fault is the shared registry bridge's
    ``refuse_secret_writes_for_test`` (compiled out of release artifacts —
    convention 15): the store keeps nothing and the shared
    ``persist_confirmed_identity`` read-back refuses, the same `Err` a refused
    `SecItemAdd` reaches. Everything from there is the app's own: the confirm
    is a real tap, and the shared machine's ``identity_commit_failed`` is what
    must put the error on screen. Before 2026-10-22 apple logged the failure
    and let the wizard advance to handle entry holding a key stored nowhere —
    every assertion below was red against that code.
    """
    _a_refused_confirm_is_loud_and_the_wizard_stays_put(apple_client, nest_instance, request)


@pytest.fixture(params=["android"])
def android_client(request):
    """android-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("android is not in this run's app set")
    return request.param


@pytest.mark.android
def test_a_keystore_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put(
    android_client, nest_instance, request
):
    """android: the Keystore-sealed store's write of a confirmed identity
    fails → the wizard says so and stays on the key's own screen.

    android's store is ``EncryptedSharedPreferences`` under a Keystore master
    key, with no fallback arm, so its promise is apple's (the test above): a
    loud error. "The Keystore cannot be reached" makes that encrypted write
    throw or keep nothing (an invalidated master key, an unavailable keystore
    daemon), and nothing a user does on an emulator produces it on demand —
    an e2e launch also rests its slots in the ``FAUNA_E2E_CREDENTIAL_FILE``
    backend (convention 10), not the Keystore file. So the fault is the same
    shared registry-bridge fault apple's arm uses, reached through the
    agent's ``call_machine_method`` registry arm (``TestAgent.kt``): the store
    keeps nothing and the shared read-back refuses, the same ``Err`` a failed
    Keystore-backed write reaches. Everything from there is the app's own:
    the confirm is a real tap, and ``ConfirmCommit.kt``'s
    ``commitConfirmedIdentity`` calling the shared ``identity_commit_failed``
    is what must put the error on screen.
    """
    _a_refused_confirm_is_loud_and_the_wizard_stays_put(android_client, nest_instance, request)


@pytest.fixture(params=["windows"])
def windows_client(request):
    """windows-only, the client id in the test name for conftest's ``--app``
    filter."""
    if request.param not in get_available_apps():
        pytest.skip("windows is not in this run's app set")
    return request.param


@pytest.mark.windows
def test_a_credential_manager_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put(
    windows_client, nest_instance, request
):
    """windows: the Credential Manager write of a confirmed identity is lost →
    the wizard says so and stays on the key's own screen.

    Credential Manager is the app's only store, with no file arm in a shipped
    build, so its promise is apple's (the test above the android one): a loud
    error. It is a per-user OS service nothing here may stop, and what it does
    when it cannot take a write is lose it: ``CredWriteW`` refuses a full set,
    the arm's write is best-effort and log-only (`apps/windows.md` § Key
    storage → *The whole set has a budget too*), and the read-back in the
    shared ``persist_confirmed_identity`` is what turns that into an ``Err``.
    So the fault is the shared registry-bridge fault again, reached through
    ``TestAgent.cs``'s registry arm, on the launch's own file store, and it
    reaches the same ``Err`` a lost write does: the store keeps nothing and the
    read-back refuses. Everything from there is the app's own: the confirm is
    a real tap, and ``OnboardingViewModel``'s shared ``identity_commit_failed``
    is what must put the error on screen.
    """
    _a_refused_confirm_is_loud_and_the_wizard_stays_put(windows_client, nest_instance, request)


@pytest.mark.web
def test_a_browser_has_no_protected_store_and_says_so_and_the_key_rests_only_sealed(
    nest_instance, spa_url
):
    """web's column. A browser offers a page no protected store at all, so
    web's answer is tui's sealed arm (`common.md` § Credential storage → *The
    web app's posture*): the create surface says there is no secure key store
    here before anything is stored, the person chooses a passphrase, and an
    identity imported through the wizard rests nowhere less protected — no
    place in the origin (``localStorage``, ``sessionStorage``, IndexedDB,
    OPFS) holds the secret in any encoding — and the next launch asks for the
    passphrase before it can read the identity back.

    The test build keeps the plain arm unless the origin opts in
    (`web.md` § Identity store at rest → *Test builds*): a shipped build has
    no plain arm, so the opt-in on a fresh origin is the shipped launch."""
    from actions import ActionLayer
    from helpers.web_store import origin_holdings
    from tests.test_web_sealed_identity_store import (
        CONFIRM_INPUT as WEB_CONFIRM, OPT_IN_KEY, PASSPHRASE_INPUT as WEB_PASSPHRASE,
        _submit,
    )

    secret = bytes(SigningKey.generate())
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    try:
        driver.wait_for("import-identity-button", timeout=90)
        driver.eval_js(f"localStorage.setItem({OPT_IN_KEY!r}, '1')")
        driver.hard_reload()

        # ── 1. The app SAYS so before anything is stored. ────────────────────
        driver.wait_for(WEB_CONFIRM, timeout=30)
        assert driver.is_visible(WEB_PASSPHRASE)
        assert S.unlock.create_prompt in driver.eval_js("document.body.innerText"), (
            "the create surface is up but its prompt — the words that tell the "
            "person no secure key store is here — is not on the page"
        )
        assert driver.eval_js("localStorage.getItem('fauna/seal')") is None, (
            "a seal header existed before the person chose a passphrase"
        )

        # ── 2. Choose the passphrase, then import through the wizard. ────────
        _submit(driver, PASSPHRASE, confirm=PASSPHRASE)
        app = ActionLayer(driver)
        driver.wait_for("import-identity-button", timeout=60)
        app.onboarding.import_key(secret.hex())
        driver.wait_for("handle-input", timeout=30)

        # ── 3. Nowhere less protected. ───────────────────────────────────────
        assert driver.eval_js("localStorage.getItem('fauna/seal') !== null"), (
            f"no seal header after the identity was confirmed; error={app.error_text()!r}"
        )
        held = origin_holdings(driver, _encodings(secret))
        hits = sorted({w for places in held["found"].values() for w in places})
        assert not hits, (
            "the identity secret rests readable outside the sealed rows, in: "
            f"{hits} (the 'somewhere less protected' outcome 2 forbids; searched "
            f"{held['places']} places)"
        )

        # ── 4. The next launch asks before it can read the identity. ─────────
        driver.hard_reload()
        driver.wait_for(WEB_PASSPHRASE, timeout=30)
        assert driver.is_absent(WEB_CONFIRM), (
            "the relaunch offered to CREATE a store — the identity was not kept sealed"
        )
        _submit(driver, PASSPHRASE)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if driver.is_visible("handle-input") or driver.is_visible("create-identity-button"):
                break
            time.sleep(0.5)
        assert driver.is_absent("create-identity-button"), (
            "unlocked, but the identity was not found — it was never in the sealed store"
        )
        assert driver.is_visible("handle-input"), (
            f"unlocked, but the wizard did not resume at handle entry; error={app.error_text()!r}"
        )
    finally:
        with contextlib.suppress(Exception):
            driver.teardown()
