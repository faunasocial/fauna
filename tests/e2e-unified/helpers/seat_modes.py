"""Seat modes and the app each launches — the ONE table (stdlib only).

A convention-16 seat set is parametrized by seat MODES (`native+native`), never
by app names; this module resolves a mode to the app it launches on a platform.
Three readers share it: `helpers/sync_seats.py` (`make_seat`, re-exporting every
name), conftest's `_apps_for_seat_pair` (the prebuild, the lane and the feature
ledger), and `scripts/features_catalog.py`'s two-source reading, which resolves a
last-green directory key such as `[2seat-native+native]` to the app it witnessed
(feature-catalog.md § Cell semantics, *Two sources, one observation*). It
imports nothing beyond the standard library so the parse-only catalog can load
it by path.
"""

from __future__ import annotations

# How a seat is driven. `tui` and `native` drive the real UI for sign-in /
# adopt / bind and then leave the subject to plain file I/O. The retired
# `engine` (headless-daemon) mode is gone for good — see the module docstring.
SEAT_MODES = ("tui", "native")


# Which concrete app a `native` seat means on each platform. One mode name, one
# resolution point — so `seat_set_id` reads `native+native` on every box and a
# new platform's seat is a row here rather than a new mode name (#1/#3).
#
# All three GUI desktops as of 2026-08-10. `linux` is the one row whose platform
# key and app name coincide (`sys.platform == "linux"`, the app is `linux`);
# that is a coincidence of naming, not a rule — do not "simplify" the lookup into
# an identity fallback, which would silently resolve a `native` seat on any future
# unix that has no app at all, exactly the case `make_seat`'s NotImplementedError
# exists to make loud.
NATIVE_APPS = {"win32": "windows", "darwin": "macos", "linux": "linux"}


def native_app_for_platform(sys_platform: str) -> str | None:
    """The app a ``native`` seat drives here, or None if this platform has none."""
    return NATIVE_APPS.get(sys_platform)


# A PHONE seat: the account's mobile app, signed in as one more device of the
# account and bound to no folder. It is deliberately NOT in :data:`SEAT_MODES` —
# those are the folder-syncing modes the convergence matrix multiplies over, and
# a phone keeps no same-account peer leg and syncs no folder through an agent
# (`p2p.md` § Implementation status today: phones are excused from the
# peer-leg outcomes). What a phone seat is for is a journey about the DEVICE
# rather than its files — the Devices page's participation switch, whose
# outcomes the phones still owe — paired with a desktop seat that is.
PHONE_MODE = "phone"

# Which phone app a `phone` seat means on each platform: the iOS simulator on
# macOS, the android emulator on Linux (the `linux` key — the box whose harness
# drives the emulator over adb, not the phone's own OS). One phone per pair: the
# android venue provisions ONE emulator and runs one package per device
# (testing.md § *Android's run venue*), so a phone seat is always the `phone`
# half of a pair whose other seat is a desktop one — never two phones. The
# device reaches the harness nest through the `adb reverse` the driver opens at
# launch for every nest port in the seat's config (`drivers/android.py::launch`).
PHONE_APPS = {"darwin": "ios", "linux": "android"}


def phone_app_for_platform(sys_platform: str) -> str | None:
    """The app a ``phone`` seat drives here, or None if this platform has none."""
    return PHONE_APPS.get(sys_platform)


# A WEB seat: the account's web app in a browser, signed in as one more device of
# the account and bound to no folder — a browser hosts no sync agent, so like the
# phone it keeps no peer leg and stays out of :data:`SEAT_MODES`. What it is for
# is a journey that READS the Devices page from the web app while a desktop seat
# is the device under view (`devices.md` § Device names: every app draws the
# roster through the shared `DevicesMachine::render_devices`).
WEB_MODE = "web"

# Where a `web` seat runs: Linux alone, the one machine that builds the web app
# (`build-system.md` § The Deno build sandbox).
WEB_APPS = {"linux": "web"}

# The seat modes that are a device of the account and nothing else — no folder,
# no agent, driven by :class:`DeviceSeatDriver`. In a pair with a folder-syncing
# seat it is the device-only seat the journey is about, so the feature ledger
# credits it alone (conftest's ``_apps_for_seat_pair``).
DEVICE_ONLY_MODES = (PHONE_MODE, WEB_MODE)


def seat_mode_app(mode: str, sys_platform: str) -> str | None:
    """The app a seat of ``mode`` launches on ``sys_platform`` — the ONE
    resolution of a mode to an app (the prebuild, the lane and the ledger read
    it through conftest's ``_apps_for_seat_pair``), or None for a mode this
    platform cannot launch."""
    if mode == "native":
        return native_app_for_platform(sys_platform)
    if mode == PHONE_MODE:
        return phone_app_for_platform(sys_platform)
    if mode == WEB_MODE:
        return WEB_APPS.get(sys_platform)
    return mode if mode in SEAT_MODES else None


#: The directory's platform names (`docs/features/last-green/<platform>.json`)
#: against the `sys.platform` values the tables above key on.
SNAPSHOT_SYS_PLATFORM = {"linux": "linux", "macos": "darwin", "windows": "win32"}


def apps_for_seat_set(modes, sys_platform: str, *, under_test: bool = False) -> tuple:
    """The apps a seat set launches on ``sys_platform`` — ``()`` when ``modes``
    is not a seat set; a mode this platform cannot launch contributes nothing.

    ``under_test`` asks the feature ledger's question instead — which apps the
    set WITNESSES — and differs only for a set with a device-only seat
    (`DEVICE_ONLY_MODES`): its folder-syncing partner is a helper there, so only
    the device-only seat is credited (feature-catalog.md § The marker)."""
    if not isinstance(modes, (tuple, list)) or not modes:
        return ()
    if not all(isinstance(mode, str) for mode in modes):
        return ()
    device_only = set(DEVICE_ONLY_MODES)
    if not set(modes) <= set(SEAT_MODES) | device_only:
        return ()
    if under_test and device_only & set(modes):
        modes = [mode for mode in modes if mode in device_only]
    return tuple(a for a in (seat_mode_app(m, sys_platform) for m in modes) if a)
