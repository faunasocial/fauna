"""The host half of the Android store driver — obtain the app from Google Play
and drive the install the store delivered, from outside the app.

Ruling: ``docs/goal/architecture/feature-catalog.md`` § Implementation status
today, the bullet on how the Android store install is driven. The harness
bridge (``drivers/android.py`` + ``app/src/androidTest/.../bridge/``) instruments
``social.fauna.fauna`` in the app's own UID, so it can attach only to a build
signed with the debug key — never to a Play delivery, which Google re-signs
with the app signing key. The store driver is a separate, self-instrumenting
APK (``apps/fauna-android/store-driver/``, ``just android-store-driver``) that
reaches the app through ``UiAutomation`` alone; this module installs and starts
it, forwards its port, and speaks its small HTTP surface.

What it can NOT do, by construction, is everything the harness bridge does in
the app's UID: no credential seed, no log or download read, no launch
environment. A journey through it signs in through the app's own UI.

The subject facts — installer of record and signing certificate — are read
host-side from ``pm`` and from the installed base APK, never from the driver:
the driver's only part in the install is pressing the store's button.
"""

from __future__ import annotations

import glob
import json
import os
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from drivers.port_util import drain_pipes, find_free_port, popen_group_kwargs, reap_descendants_of
from helpers import android_device, android_venue

#: The app's store identity (``installers/android.md`` § Store identity).
FAUNA_PACKAGE = "social.fauna.fauna"
#: The Play Store app — the installer of record a store install names.
PLAY_PACKAGE = "com.android.vending"

#: The driver APK ``just android-store-driver`` builds.
DRIVER_APK = "apps/fauna-android/store-driver/build/outputs/apk/debug/store-driver-debug.apk"
DRIVER_PACKAGE = "social.fauna.storedriver"
DRIVER_INSTRUMENTATION = f"{DRIVER_PACKAGE}/com.fauna.storedriver.StoreDriverInstrumentation"
#: Device-side port; beside the harness bridge's 18500 (``StoreDriverInstrumentation``).
DRIVER_DEVICE_PORT = 18510

#: The AVD the witness needs (the venue's AVD is a ``google_apis`` image, which
#: carries no Play Store).
PLAY_AVD_NEED = (
    "the fauna-e2e-play AVD (serial emulator-5556, an arm64 Google Play system "
    "image, system-images;android-35;google_apis_playstore;arm64-v8a) with the "
    "Play Store signed in once to an account on the internal testers list; run "
    "`just e2e-android-store-test --device-serial emulator-5556` "
    "(feature-catalog.md § Implementation status today, how the Android "
    "store install is driven)"
)

#: Play's app signing certificates, classic SHA-256 (``installers/android.md``
#: § Implementation status today, the Play signing keys). ``CURRENT`` is the
#: key Play signs every delivery with since the 2026-10-09 rotation; ``PREVIOUS``
#: may still appear as the lineage's older signer on the same APK.
PLAY_APP_SIGNING_CURRENT = (
    "CF:9E:02:2E:9C:55:18:5C:29:BE:E5:26:02:2F:C5:E4:B1:A6:B2:8D:D9:3B:CE:2F:9A:73:49:71:A4:C2:FD:BA"
)
PLAY_APP_SIGNING_PREVIOUS = (
    "19:0C:09:C0:17:C8:93:3F:36:99:81:BB:1D:20:C5:83:49:D1:6F:22:47:9D:34:9E:55:64:83:97:E3:AE:72:F3"
)
PLAY_APP_SIGNING_KEYS = frozenset({PLAY_APP_SIGNING_CURRENT, PLAY_APP_SIGNING_PREVIOUS})

#: How long the store's download and install may take after Install is pressed:
#: the bundle's arm64 split is ~90 MB (installers/android.md, the first release).
INSTALL_BUDGET_S = 600.0


class StoreDriverError(RuntimeError):
    """The driver answered with an error, or never answered."""


def adb() -> list[str]:
    """This run's adb prefix — the same server and device the harness drives."""
    return android_device.adb_argv(android_device.device_serial(), android_device.adb_server())


def adb_shell(*args: str, timeout: float = 30) -> str:
    out = subprocess.run([*adb(), "shell", *args], capture_output=True, text=True, timeout=timeout)
    return out.stdout


def device_attached() -> bool:
    return android_device.probe_device(android_device.device_serial(), android_device.adb_server())


def package_installed(package: str) -> bool:
    """``pm list packages <p>`` names exactly ``package:<p>`` (it prefix-matches)."""
    listing = adb_shell("pm", "list", "packages", package)
    return f"package:{package}" in listing.split()


def device_sdk() -> int:
    raw = adb_shell("getprop", "ro.build.version.sdk").strip()
    return int(raw) if raw.isdigit() else 0


def installer_of_record(package: str = FAUNA_PACKAGE) -> str:
    """The installing package ``pm get-install-source-info`` names (API 30+),
    falling back to ``dumpsys package``'s ``installerPackageName`` below it."""
    info = adb_shell("pm", "get-install-source-info", package)
    m = re.search(r"installingPackageName\s*:\s*(\S+)", info) or re.search(
        r"installerPackageName\s*:\s*(\S+)", info
    )
    if not m:
        dump = adb_shell("dumpsys", "package", package, timeout=60)
        m = re.search(r"installerPackageName=(\S+)", dump)
    return m.group(1) if m else ""


def _apksigner() -> str:
    roots = [os.environ.get("ANDROID_HOME"), os.environ.get("ANDROID_SDK_ROOT"),
             os.path.join(os.path.expanduser("~"), "android-sdk")]
    for root in filter(None, roots):
        found = sorted(glob.glob(os.path.join(root, "build-tools", "*", "apksigner")))
        if found:
            return found[-1]
    raise StoreDriverError("apksigner not found under the Android SDK's build-tools")


def _normalise(digest: str) -> str:
    hexdigits = digest.replace(":", "").strip()
    return ":".join(hexdigits[i:i + 2] for i in range(0, len(hexdigits), 2)).upper()


def installed_certificate_digests(package: str = FAUNA_PACKAGE) -> list[str]:
    """Every certificate SHA-256 ``apksigner`` reads off the INSTALLED base APK,
    verified for this device's API level — the signer the device itself trusts.

    The APK is pulled from the path ``pm path`` names; a store delivery is a
    split set, and the base carries the signature every split must match.
    """
    paths = [line.split(":", 1)[1] for line in adb_shell("pm", "path", package).splitlines()
             if line.startswith("package:")]
    base = next((p for p in paths if p.endswith("/base.apk")), paths[0] if paths else None)
    if not base:
        raise StoreDriverError(f"`pm path {package}` names no APK")
    with tempfile.TemporaryDirectory(prefix="row-3517-store-apk-") as tmp:
        local = Path(tmp) / "base.apk"
        subprocess.run([*adb(), "pull", base, str(local)], check=True, capture_output=True, timeout=300)
        sdk = device_sdk()
        argv = [_apksigner(), "verify", "--print-certs", "--verbose"]
        if sdk:
            argv += ["--min-sdk-version", str(sdk)]
        out = subprocess.run([*argv, str(local)], capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        raise StoreDriverError(f"apksigner refused the installed APK:\n{out.stdout}\n{out.stderr}")
    return [_normalise(m) for m in re.findall(r"certificate SHA-256 digest:\s*([0-9a-fA-F:]+)", out.stdout)]


def wait_for_install(package: str = FAUNA_PACKAGE, budget: float = INSTALL_BUDGET_S) -> bool:
    """Poll ``pm list packages`` until ``package`` is installed (the row's
    host-side completion read; the store's progress UI is never parsed)."""
    deadline = time.monotonic() + budget
    while time.monotonic() < deadline:  # deadline-ok: completion is the package manager's state, polled; the budget only bounds a download
        if package_installed(package):
            return True
        time.sleep(2)  # sleep-ok: poll interval of a state read, not a timing assertion
    return package_installed(package)


class StoreDriver:
    """The started store driver: instrumentation running, port forwarded.

    ``stop()`` ends ``am instrument`` and removes the forward; use it as a
    context manager.
    """

    def __init__(self, apk: Path):
        self._apk = apk
        self._proc: subprocess.Popen | None = None
        self._lease = None
        self._local_port: int | None = None
        self._log = None

    def __enter__(self) -> "StoreDriver":
        self.start()
        return self

    def __exit__(self, *exc) -> None:
        self.stop()

    def start(self) -> None:
        subprocess.run([*adb(), "install", "-r", "-t", str(self._apk)],
                       check=True, capture_output=True, timeout=120)
        if android_device.adb_server():
            self._lease = android_venue.lease_bridge_forward_port()
            self._local_port = self._lease.port
        else:
            self._local_port = find_free_port()
        subprocess.run([*adb(), "forward", f"tcp:{self._local_port}", f"tcp:{DRIVER_DEVICE_PORT}"],
                       check=True, capture_output=True, timeout=30)
        self._proc = subprocess.Popen(
            [*adb(), "shell", "am", "instrument", "-w", "-e", "port", str(DRIVER_DEVICE_PORT),
             DRIVER_INSTRUMENTATION],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, **popen_group_kwargs(),
        )
        reap_descendants_of(self._proc.pid)
        self._log = drain_pipes(self._proc)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:  # deadline-ok: launch gate on /health answering
            if self._proc.poll() is not None:
                raise StoreDriverError(f"the store driver's instrumentation exited: {self._log}")
            try:
                self._get("/health", timeout=2)
                return
            except (OSError, StoreDriverError):
                time.sleep(0.5)  # sleep-ok: poll interval of the launch gate
        raise StoreDriverError(f"the store driver never answered /health: {self._log}")

    def stop(self) -> None:
        if self._proc and self._proc.poll() is None:
            self._proc.terminate()
            try:
                self._proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self._proc.kill()
        if self._local_port is not None:
            subprocess.run([*adb(), "forward", "--remove", f"tcp:{self._local_port}"],
                           capture_output=True, timeout=30)
            self._local_port = None
        if self._lease:
            self._lease.release()
            self._lease = None

    # ── wire ────────────────────────────────────────────────────────────

    def _url(self, path: str) -> str:
        return f"http://127.0.0.1:{self._local_port}{path}"

    def _send(self, req: urllib.request.Request, timeout: float) -> dict:
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                body = resp.read()
        except urllib.error.HTTPError as e:
            raise StoreDriverError(f"{req.get_method()} {req.full_url}: {e.code} {e.read()!r}") from e
        return json.loads(body or b"{}")

    def _get(self, path: str, params: dict | None = None, timeout: float = 30) -> dict:
        query = ("?" + urllib.parse.urlencode(params)) if params else ""
        return self._send(urllib.request.Request(self._url(path + query)), timeout)

    def _post(self, path: str, body: dict | None = None, timeout: float = 60) -> dict:
        data = json.dumps(body or {}).encode()
        req = urllib.request.Request(self._url(path), data=data, method="POST",
                                     headers={"Content-Type": "application/json"})
        return self._send(req, timeout)

    # ── obtain ──────────────────────────────────────────────────────────

    def open_listing(self) -> bool:
        return bool(self._post("/store/open").get("opened"))

    def press_install(self) -> dict:
        """``{"state": pressed|installed|signed_out|not_available|unknown, "texts": [...]}``."""
        return self._post("/store/install", timeout=90)

    # ── the app, from outside ───────────────────────────────────────────

    def launch_app(self) -> None:
        self._post("/app/launch")

    def stop_app(self) -> None:
        self._post("/app/stop")

    # ── the PlatformDriver subset the onboarding actions use ────────────

    def click(self, element_id: str, index: int = 0) -> None:
        self._post("/element/click", {"id": element_id, "index": index})

    def type_text(self, element_id: str, text: str) -> None:
        self._post("/element/type", {"id": element_id, "text": text})

    def clear_and_type(self, element_id: str, text: str) -> None:
        self._post("/element/clear", {"id": element_id})
        self.type_text(element_id, text)

    def get_text(self, element_id: str, index: int = 0) -> str:
        return self._get("/element/text", {"id": element_id, "index": index})["text"]

    def is_visible(self, element_id: str) -> bool:
        return bool(self._get("/element/visible", {"id": element_id}).get("visible"))

    def is_enabled(self, element_id: str, index: int = 0) -> bool:
        return bool(self._get("/element/enabled", {"id": element_id, "index": index}).get("enabled"))

    def wait_for(self, element_id: str, timeout: float = 10) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:  # deadline-ok: waits on an element's presence, a state
            if self.is_visible(element_id):
                return
            time.sleep(0.25)  # sleep-ok: poll interval of a state read
        if not self.is_visible(element_id):
            raise TimeoutError(f"'{element_id}' not visible within {timeout}s; "
                               f"error element: {self.error_text()!r}")

    def error_text(self) -> str:
        try:
            return self.get_text("error-message") if self.is_visible("error-message") else ""
        except StoreDriverError:
            return ""

    def diagnose(self, element_id: str) -> str:
        return f"'{element_id}' visible={self.is_visible(element_id)}; error element: {self.error_text()!r}"
