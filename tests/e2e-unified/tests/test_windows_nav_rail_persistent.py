"""tier_3 E2E: the windows primary-tabs rail stands beside page content at the
default window, with nothing opened to find it.

`docs/goal/ui/README.md` § Navigation model: on desktop — windows included — "a
persistent sidebar/rail carrying the primary tabs"; the one width carve-out it
names is web's mobile bottom bar. WinUI's `NavigationView` in its default `Auto`
pane mode drops to `LeftMinimal` below 641 DIP — the rail vanishes behind a
hamburger whose toggle floats over the top-left of page content — and the app's
minimum AND default window is 600 DIP (`App.xaml.cs` `MinWindowDip`), so the app
opened in that mode and every windows e2e ran in it. `MainPage.xaml` now sets
`CompactModeThresholdWidth="0"`: `Auto` never chooses Minimal, so below the
expanded threshold the pane is the `LeftCompact` icon rail.

**What is observed, headlessly.** The question "is there a rail, and does
anything of the navigation lie over the page?" reduces to two UIA facts, no eye
needed:

* every primary tab is in the tree at launch — in Minimal mode a closed pane
  holds its `NavigationViewItem`s out of the tree entirely, and the bridge no
  longer opens the pane on a miss (`ElementFinder`, strategy 3), so a collapsed
  pane reads as absent here rather than being opened behind the test's back;
* the rail's right edge is at or left of the page heading's left edge — page
  content starts beside the rail, not under a floating toggle.

Frames are window-space physical pixels (`Actions.AttrOf` "frame"), so the
assertion compares edges, never DIP constants.
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

# The main menu's primary tabs plus the always-present footer entry, as
# `MainPage.xaml` declares them. The gated `admin-tab` / `family-tab` are left
# out: they are `Collapsed` until their gate fires, and `test_admin_nav.py` /
# the family tests own their reveal.
_PRIMARY_TABS = (
    "feed-tab",
    "conversations-tab",
    "contacts-tab",
    "profile-tab",
    "bridges-tab",
    "backups-tab",
    "media-tab",
    "search-tab",
    "events-tab",
    "moderation-tab",
    "notifications-tab",
    "settings-tab",
)


def _frame(driver, element_id: str) -> tuple[float, float, float, float]:
    raw = driver.get_attr(element_id, "frame")
    assert raw, (
        f"'{element_id}' has no frame; "
        f"{driver.diagnose(element_id, attrs=('frame',))}"
    )
    x, y, w, h = (float(p) for p in raw.split(","))
    assert w > 0 and h > 0, f"'{element_id}' frame {raw!r} has no size"
    return x, y, w, h


def test_the_primary_tabs_rail_stands_beside_page_content(logged_in_app):
    driver = logged_in_app.driver
    driver.navigate_to("feed")
    driver.wait_for("page-heading", timeout=30)

    missing = [tab for tab in _PRIMARY_TABS if driver.count(tab) < 1]
    assert not missing, (
        f"the nav rail does not hold {missing} at the default window — the "
        "NavigationView pane is collapsed (Minimal mode keeps a closed pane's "
        "items out of the UIA tree). "
        f"{driver.diagnose('feed-tab', attrs=('frame',))}"
    )
    assert driver.is_visible("feed-tab"), (
        "feed-tab is in the tree but offscreen — the rail is not showing its "
        f"first item. {driver.diagnose('feed-tab', attrs=('frame',))}"
    )

    tab_x, _, tab_w, _ = _frame(driver, "feed-tab")
    heading_x, _, _, _ = _frame(driver, "page-heading")
    assert tab_x + tab_w <= heading_x, (
        f"the rail (feed-tab x={tab_x:.0f} w={tab_w:.0f}, right edge "
        f"{tab_x + tab_w:.0f}) reaches over the page heading (x={heading_x:.0f}) — "
        "page content lies under the navigation instead of beside it"
    )
