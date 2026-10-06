"""The shared "did onboarding land?" marker set, pinned against the drift that broke it.

`docs/goal/architecture/e2e-conventions.md` § point 6 (*failures must diagnose
themselves*) and § point 7 (*a skip is not coverage*). These are tier_1: the
marker set and the diagnosis are pure data and a pure function over a duck-typed
driver, so both pin without a nest, a driver or an app.

**The failure this closes.** `test_trust_prompt.py` kept a private copy of the
marker tuple that had drifted to `("feed-tab", "settings-tab", "main-tab-view")` —
a set with **no windows-workable member at all**. Both of its answer-the-offer
journeys then hung for the full 120 s budget against an app that had in fact
landed (its own log showed `MainPage` mounted and the admin tab revealed), and
the failure message said nothing, because the diagnosis called `driver.tree()` —
a route only the apple bridge implements.

Both halves of that are guarded here, because both are one-line edits away from
returning and neither shows up as a red until someone runs the windows leg:
a marker set that no longer names the landed page's own content, and a
diagnosis that goes quiet when a probe is unavailable.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers.authenticated_shell import (  # noqa: E402
    SHELL_MARKERS,
    describe_markers,
)

#: Nav entries are corroboration, never the landing signal. On windows a
#: revealed `NavigationView` entry is in the UIA tree with an OFFSCREEN rect, and
#: `is_visible()` tests `!IsOffscreen` — see
#: `WindowsBridgeDriver.is_nav_tab_revealed`, which exists for exactly this.
_NAV_ENTRIES = ("feed-tab", "settings-tab")

#: Registered under `global.platform_elements.ios` in `ui.yaml`, so it can never
#: be the marker that answers for another app.
_IOS_ONLY = ("main-tab-view",)


class _Driver:
    """A driver that renders `visible`/`count` per element, and refuses `get_text`.

    The refusal is the point of the second test: `PlatformDriver.diagnose`
    guards every probe individually, so an accessor a platform does not
    implement must be *reported in place* rather than swallowing the whole
    diagnosis.
    """

    def __init__(self, visible=(), counted=()):
        self._visible = set(visible)
        self._counted = set(counted)

    def is_visible(self, element_id, *, scope=None):
        return element_id in self._visible

    def count(self, element_id, *, scope=None):
        return 1 if element_id in self._counted or element_id in self._visible else 0

    def get_text(self, element_id, *, scope=None):
        raise NotImplementedError("this bridge does not surface text")

    def diagnose(self, element_id, *, attrs=(), scope=None):
        # The real `PlatformDriver.diagnose`, reduced to the two probes these
        # tests are about; the production one is exercised by its own callers.
        parts = [f"visible={self.is_visible(element_id)!r}",
                 f"count={self.count(element_id)!r}"]
        try:
            parts.append(f"text={self.get_text(element_id)!r}")
        except Exception as e:  # noqa: BLE001 — diagnostic only
            parts.append(f"text=<{type(e).__name__}: {e}>")
        return f"[{element_id}: " + ", ".join(parts) + "]"


class _App:
    def __init__(self, driver):
        self.driver = driver

    def error_text(self):
        return ""


def test_the_marker_set_names_a_landed_page_not_only_chrome():
    """At least one marker must be the landed page's OWN content.

    This is the invariant the drift broke: a set made only of nav entries and a
    platform-scoped ID describes the app's chrome, and windows renders that
    chrome offscreen — so the set answered False forever on a fully-mounted app.
    """
    content_markers = set(SHELL_MARKERS) - set(_NAV_ENTRIES) - set(_IOS_ONLY)
    assert content_markers, (
        "every marker is either a nav entry or platform-scoped, so no app whose "
        "nav renders offscreen (windows) can ever match — the exact shape that "
        "hung test_trust_prompt.py for 120s on an app that had landed"
    )
    assert "feed-view" in SHELL_MARKERS, (
        "feed-view is the landed feed page's own content and the only member "
        "windows can see; dropping it re-opens row 179"
    )


def test_a_marker_that_is_present_but_offscreen_is_told_from_one_never_rendered():
    """`visible=False, count=1` and `visible=False, count=0` are different verdicts.

    Naming that difference is what turns this failure from a silent 120s timeout
    into a diagnosis: rendered-but-offscreen is a marker-choice bug, while
    never-rendered means the app genuinely did not land.
    """
    offscreen = _App(_Driver(visible=(), counted=("feed-tab",)))
    text = describe_markers(offscreen)
    assert "[feed-tab: visible=False, count=1" in text, text

    never = _App(_Driver(visible=(), counted=()))
    assert "[feed-view: visible=False, count=0" in describe_markers(never)


def test_the_diagnosis_reports_an_unavailable_probe_instead_of_going_quiet():
    """A probe a platform does not implement is named in place.

    The diagnosis this replaced called `driver.tree()`, which every non-apple
    bridge answers with `""` by contract — so on six of seven apps the whole
    message was empty. A diagnosis that can silently degrade to nothing is worse
    than none, because it reads as evidence the app said nothing.
    """
    text = describe_markers(_App(_Driver(visible=("feed-view",))))
    assert "NotImplementedError" in text, text
    assert text.strip(), "a diagnosis must never be empty"


@pytest.mark.parametrize("marker", SHELL_MARKERS)
def test_every_marker_is_registered_in_ui_yaml(marker):
    """No marker may be a typo — a misspelled ID is silently never visible."""
    import yaml

    ui = yaml.safe_load(
        (Path(__file__).resolve().parents[1] / "ui.yaml").read_text(encoding="utf-8")
    )
    assert marker in (ui.get("elements") or {}), (
        f"{marker!r} is not in ui.yaml's element registry, so it can never match"
    )
