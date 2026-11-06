"""The host half of the iOS store driver: obtain the app from TestFlight and
drive the install TestFlight delivered, from outside the app.

Ruling: ``docs/goal/architecture/feature-catalog.md`` § Implementation status
today, the bullet on how the iOS store install is driven. The harness's iOS
driver (``drivers/ios.py``) talks to the in-process TestAgent of a DEBUG build
on a simulator. Convention 15 compiles that agent and the ``AutomationRegistry``
out of every Release build, and a TestFlight delivery is a Release build Apple
re-signed, so nothing inside it can be reached. The store driver is a separate
XCUITest runner (``apps/fauna-apple/StoreDriver/``, built by
``just ios-store-driver``, bundle id ``social.fauna.storedriver``) that reaches
the app only through the accessibility tree. This module finds the paired
device, runs one runner method per call with ``xcodebuild
test-without-building``, and reads each outcome off the result bundle.

What it can NOT do, by construction, is everything the in-process agent does:
no credential seed, no log or download read, no launch environment. A journey
through it signs in through the app's own UI.

The subject facts are read host-side from ``devicectl``, never from the
runner. The runner's only part in the install is pressing TestFlight's button
and reading TestFlight's own record that the install finished.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

#: The app's store identity (``installers/ios.md`` § Store identity).
FAUNA_BUNDLE_ID = "social.fauna.fauna"

#: Where ``just ios-store-driver`` (signed) leaves the runner's test products.
DRIVER_PRODUCTS = "build/ios-store-driver/signed/Build/Products"
#: The runner's test identifiers, for ``-only-testing``.
DRIVER_TEST_CLASS = "StoreDriverUITests/StoreDriverTests"
OBTAIN_TEST = f"{DRIVER_TEST_CLASS}/testObtainFromTestFlight"
SIGN_IN_TEST = f"{DRIVER_TEST_CLASS}/testSignInToFeed"

#: Picks one paired device when several are present (its UDID or CoreDevice id).
DEVICE_ENV = "E2E_IOS_DEVICE"

#: What the witness needs; quoted by every environment skip.
DEVICE_NEED = (
    "a physical iPhone paired with this machine (Developer Mode on, Settings → "
    "Developer → Enable UI Automation on), registered in the development profile "
    "`Fauna Development social.fauna.storedriver` that signs the runner, with "
    "TestFlight installed and signed in to an Apple Account on the app's tester "
    "group (feature-catalog.md § Implementation status today, how the iOS store "
    "install is driven)"
)

#: The runner reports an environment state by skipping with ``state=<word>``.
ENVIRONMENT_STATES = frozenset({"testflight_absent", "signed_out", "not_available", "no_account"})

#: Ceiling on one runner method: TestFlight's download is bounded by the
#: runner's own install budget, plus the runner's launch on the device.
RUN_CEILING_S = 1500.0


class StoreDriverError(RuntimeError):
    """The runner could not be run, or answered with something unreadable."""


# ── devicectl ─────────────────────────────────────────────────────────────


def _devicectl_json(*args: str, timeout: float = 60) -> dict:
    with tempfile.TemporaryDirectory(prefix="row-567-devicectl-") as tmp:
        out = Path(tmp) / "out.json"
        proc = subprocess.run(
            ["xcrun", "devicectl", *args, "--json-output", str(out), "--quiet"],
            capture_output=True, text=True, timeout=timeout,
        )
        if not out.is_file():
            raise StoreDriverError(
                f"`devicectl {' '.join(args)}` wrote no JSON (rc={proc.returncode}): {proc.stderr.strip()[-800:]}"
            )
        return json.loads(out.read_text() or "{}")


@dataclass(frozen=True)
class Device:
    identifier: str  # CoreDevice identifier, what --device takes
    udid: str  # what xcodebuild's -destination id= takes
    name: str
    developer_mode: str


def paired_devices() -> list[Device]:
    """Physical iOS devices CoreDevice has paired with this machine."""
    data = _devicectl_json("list", "devices")
    found = []
    for d in data.get("result", {}).get("devices", []):
        hw = d.get("hardwareProperties", {})
        conn = d.get("connectionProperties", {})
        props = d.get("deviceProperties", {})
        if hw.get("platform") != "iOS" or hw.get("reality") != "physical":
            continue
        if conn.get("pairingState") != "paired":
            continue
        found.append(Device(
            identifier=d.get("identifier", ""),
            udid=hw.get("udid", ""),
            name=props.get("name", ""),
            developer_mode=str(props.get("developerModeStatus", "")),
        ))
    return found


def pick_device() -> Device | None:
    """The device this run drives: the one named by ``E2E_IOS_DEVICE``, else
    the only paired one. Several paired and none named is an error, because
    guessing would drive whichever phone happened to sort first."""
    devices = paired_devices()
    wanted = os.environ.get(DEVICE_ENV, "").strip()
    if wanted:
        match = [d for d in devices if wanted in (d.udid, d.identifier, d.name)]
        return match[0] if match else None
    if len(devices) > 1:
        raise StoreDriverError(
            f"{len(devices)} iOS devices are paired; name one with {DEVICE_ENV}=<udid>"
        )
    return devices[0] if devices else None


def app_record(device: Device, *, developer_only: bool) -> dict | None:
    """The device's record of the app, or None when it is not installed.

    ``developer_only`` lists only developer apps (devicectl's default: an app
    "is considered default if it is not a developer app"); otherwise every app
    is listed. A store delivery appears only in the second listing.
    """
    args = ["device", "info", "apps", "--device", device.identifier, "--bundle-id", FAUNA_BUNDLE_ID]
    if not developer_only:
        args.append("--include-all-apps")
    apps = _devicectl_json(*args).get("result", {}).get("apps", [])
    return next((a for a in apps if a.get("bundleIdentifier") == FAUNA_BUNDLE_ID), None)


def is_installed(device: Device) -> bool:
    return app_record(device, developer_only=False) is not None


def install_classification(device: Device) -> dict:
    """The device's own reading of the install, for the subject's second fact:
    a TestFlight or App Store delivery is not a developer app. An app signed
    with a development identity, or installed by Xcode or devicectl, is one.
    The fields beside the two listings are reported, so the first device run
    records exactly what this device says."""
    every = app_record(device, developer_only=False)
    dev = app_record(device, developer_only=True)
    return {
        "installed": every is not None,
        "listed_as_developer_app": dev is not None,
        "built_by_developer": (every or {}).get("builtByDeveloper"),
        "version": (every or {}).get("version"),
        "bundle_version": (every or {}).get("bundleVersion"),
    }


# ── the runner ────────────────────────────────────────────────────────────


def find_xctestrun(repo: Path) -> Path | None:
    runs = sorted((repo / DRIVER_PRODUCTS).glob("*.xctestrun"))
    return runs[-1] if runs else None


@dataclass
class RunnerOutcome:
    test: str
    result: str  # Passed | Failed | Skipped | unknown
    messages: list[str] = field(default_factory=list)
    log_tail: str = ""

    @property
    def state(self) -> str:
        """The ``state=<word>`` a skip carried, or ''."""
        for m in self.messages:
            text = m.strip()
            if text.startswith("state="):
                return text.split()[0].removeprefix("state=")
        return ""

    def describe(self) -> str:
        return f"{self.test}: {self.result}; messages: {self.messages}\nlog tail:\n{self.log_tail}"


def _walk(node: dict):
    yield node
    for child in node.get("children", []) or []:
        yield from _walk(child)


def _read_result(bundle: Path, test: str) -> tuple[str, list[str]]:
    proc = subprocess.run(
        ["xcrun", "xcresulttool", "get", "test-results", "tests", "--path", str(bundle)],
        capture_output=True, text=True, timeout=120,
    )
    if proc.returncode != 0:
        raise StoreDriverError(f"xcresulttool could not read {bundle}: {proc.stderr.strip()[-800:]}")
    method = test.rsplit("/", 1)[-1]
    for root in json.loads(proc.stdout).get("testNodes", []):
        for node in _walk(root):
            if node.get("nodeType") == "Test Case" and node.get("name", "").startswith(method):
                messages = [n.get("name", "") for n in _walk(node)
                            if n.get("nodeType") in ("Failure Message", "Skip Message")]
                return node.get("result", "unknown"), messages
    return "unknown", []


def _redact(text: str, secrets: tuple[str, ...]) -> str:
    for secret in secrets:
        if secret:
            text = text.replace(secret, "<redacted>")
    return text


def run_test(repo: Path, device: Device, test: str, env: dict[str, str],
             secrets: tuple[str, ...] = ()) -> RunnerOutcome:
    """Run one runner method on ``device`` and read its outcome.

    ``env`` reaches the runner's environment: xcodebuild passes every
    ``TEST_RUNNER_<NAME>`` variable to the test runner as ``<NAME>``.
    Every value in ``secrets`` is cut out of the log tail and the runner's
    messages before they can reach a failure: XCUITest's console prints each
    ``typeText`` activity with the text it typed.
    """
    xctestrun = find_xctestrun(repo)
    if xctestrun is None:
        raise StoreDriverError(f"no .xctestrun under {repo / DRIVER_PRODUCTS}; run `just ios-store-driver`")
    child_env = dict(os.environ)
    child_env.update({f"TEST_RUNNER_{k}": v for k, v in env.items()})
    with tempfile.TemporaryDirectory(prefix="row-567-store-run-") as tmp:
        bundle = Path(tmp) / "run.xcresult"
        log = Path(tmp) / "xcodebuild.log"
        with log.open("w") as fh:
            proc = subprocess.run(
                ["xcodebuild", "test-without-building", "-xctestrun", str(xctestrun),
                 "-destination", f"id={device.udid}", "-only-testing", test,
                 "-resultBundlePath", str(bundle)],
                stdout=fh, stderr=subprocess.STDOUT, env=child_env, timeout=RUN_CEILING_S,
            )
        tail = _redact(log.read_text(errors="replace"), secrets)[-4000:]
        if not bundle.exists():
            raise StoreDriverError(f"xcodebuild left no result bundle (rc={proc.returncode}):\n{tail}")
        result, messages = _read_result(bundle, test)
    return RunnerOutcome(test=test, result=result,
                         messages=[_redact(m, secrets) for m in messages], log_tail=tail)
