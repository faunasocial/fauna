"""Read an app's screen-capture posture — the ``capture_posture`` state observable.

``docs/goal/architecture/security.md`` § On-screen secret exposure, rule 2: while a
minted credential is revealed the app suppresses capture, and releases it on hide
or navigate-away; rule 1: the root-secret screens never suppress. This reader is
how an e2e witnesses both against the RUNNING app (the tier_1
``test_screen_capture_posture.py`` pins the source; this pins the behaviour).

The observable, one key at one depth (apple's ``ScreenCapturePosture`` owns the
publishing side; a windows or android leg publishes the same shape):

- ``mechanism`` — ``"window-exclusion"`` where the platform excludes a window
  from every capture (macOS ``sharingType = .none`` on a disposable reveal window
  per revealed secret, windows ``WDA_EXCLUDEFROMCAPTURE``, android
  ``FLAG_SECURE``), or ``"blank-on-capture"`` on iOS, the partial control that
  blanks the value while a recording is live or the app leaves the foreground and
  does NOT stop a still screenshot. ``"unavailable"`` is the generic shape for an
  app whose platform primitive cannot honour rule 2 on the running system, with
  ``unavailable_because`` saying why; such an app takes no hold, because a hold
  it can never release would leave the whole app uncapturable. No app publishes
  it today: macOS did on releases whose window bit is one-way (macOS 27) until
  its reveal windows replaced the app-window flip, so the bit now lands on a
  window that is thrown away and never has to come back.
- ``holds`` — how many revealed secrets hold a suppression right now (on macOS,
  the reveal windows standing).
- ``window_excluded`` — the real platform bit read back off the app's windows
  (on macOS, whether ANY of them reads ``.none`` — the strict read, so a reveal
  window outliving its reveal fails the clear path), published only where such a
  bit exists (absent on iOS, never a stand-in ``false``).
"""
from __future__ import annotations

from dataclasses import dataclass

from helpers.waiting import wait_until

CAPTURE_POSTURE_KEY = "capture_posture"


@dataclass(frozen=True)
class CapturePosture:
    mechanism: str
    holds: int
    window_excluded: bool | None
    # Diagnosis only: the excluded windows' class + visibility, where published.
    excluded_windows: tuple[str, ...] = ()
    # Why the platform cannot suppress here, when `mechanism` is "unavailable".
    unavailable_because: str = ""

    @property
    def suppressed(self) -> bool:
        """Whether suppression is in force by every measure the app publishes."""
        if self.window_excluded is not None:
            return self.holds > 0 and self.window_excluded
        return self.holds > 0

    @property
    def released(self) -> bool:
        """Whether no suppression is left by any measure — the clear path whose
        failure leaves the whole app unscreenshottable."""
        return self.holds == 0 and not self.window_excluded


def capture_posture(driver) -> CapturePosture | None:
    """The app's posture, or ``None`` if it publishes none.

    ``None`` is a refusal, not a release (convention 11): an app that has not
    built the observable must never read as "not suppressing", because the
    exempt-screen assertions ask exactly that negative question.
    """
    value = driver.get_state(CAPTURE_POSTURE_KEY)
    if not isinstance(value, dict):
        return None
    mechanism, holds = value.get("mechanism"), value.get("holds")
    if not isinstance(mechanism, str) or not isinstance(holds, int):
        return None
    excluded = value.get("window_excluded")
    windows = value.get("excluded_windows")
    return CapturePosture(
        mechanism=mechanism,
        holds=holds,
        window_excluded=excluded if isinstance(excluded, bool) else None,
        excluded_windows=tuple(windows) if isinstance(windows, list) else (),
        unavailable_because=str(value.get("unavailable_because") or ""),
    )


def require_capture_posture(driver) -> CapturePosture:
    posture = capture_posture(driver)
    assert posture is not None, (
        f"the app publishes no `{CAPTURE_POSTURE_KEY}` state observable — the "
        "screen-capture witness cannot read its posture (security.md § On-screen "
        "secret exposure; apple's ScreenCapturePosture is the reference shape)"
    )
    return posture


def wait_for_posture(driver, want, *, budget_s: float, what: str) -> CapturePosture:
    """Poll until ``want(posture)`` holds; fail with the last reading.

    The hold is taken and dropped by the reveal's own render, so the deadline is
    a generous ceiling over a state transition, never a settle (convention 14).
    """
    seen: list[CapturePosture | None] = []

    def check():
        posture = capture_posture(driver)
        seen.append(posture)
        return posture if posture is not None and want(posture) else None

    return wait_until(
        check,
        budget_s,
        diagnose=lambda: f"{what}: the app's capture posture never got there — "
        f"last read {seen[-1] if seen else None!r}",
    )


def skip_if_unavailable(driver, posture: CapturePosture) -> None:
    """On a system where the app cannot suppress, witness the no-harm half and
    skip — never a pass, which would let the catalog claim a control this system
    does not give.

    The no-harm half is the point of the ``unavailable`` arm: with a revealed
    secret on screen the app holds nothing and no window is excluded, so the
    user's machine is not left uncapturable by a control that cannot let go.
    """
    if posture.mechanism != "unavailable":
        return
    assert posture.released, (
        "the app reports it cannot suppress capture on this system, yet a "
        f"window is excluded or a hold is live — {posture!r}"
    )
    from helpers.app_surface import skip_environment

    skip_environment(
        "screen-capture suppression is unavailable on this system: "
        f"{posture.unavailable_because or 'the app says so'}"
    )
