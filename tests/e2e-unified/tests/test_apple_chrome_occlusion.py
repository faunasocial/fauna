"""tier_3 E2E: the Apple apps' global status chrome takes its own layout space
and covers nothing on the page.

`docs/goal/architecture/apps/sync-agent.md` § Local agent health (Placement):
`sync-agent-status` is a global shell element, sibling to `connection-status`,
at the top of the sidebar on the apps that drive a local agent. The owner saw
the macOS window with a full-width strip carrying both indicators drawn OVER the
top lines of the sidebar and the page: the strip hung on the
`NavigationSplitView` as a `.safeAreaInset(edge: .top)`, which on macOS never
reached the split's columns, so they laid out underneath it.

**What is observed, headlessly.** Occlusion is a frame intersection, so no eye
is needed: every frame the in-process driver reports (`get_attr(id, "frame")`)
is window-space, top-left origin (`AutomationRegistry.sentinelGeometry`), across
both split columns alike. On each page visited, no chrome element's frame may
intersect a content element's frame (`page-heading`, the sidebar's first row).

macOS also covers the collapsed sidebar: `connection-status` is always visible
(`transport.md` § Connection-status indicator), so with the sidebar column
hidden the two indicators move to the detail column's top strip — present,
visible, and still clear of `page-heading`. iOS has no sidebar and no local
agent; it is measured on its one chrome element against the page heading.
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3]

# Pages visited, with the sidebar's first row on each (the Settings shell swaps
# the main list for its own rail, whose first row is the leave-settings button).
_PAGES = (
    ("conversations", "conversations-tab"),
    ("events", "conversations-tab"),
    ("settings", "settings-nav-back"),
)


def _frame(
    driver, element_id: str, *, point_ok: bool = False
) -> tuple[float, float, float, float] | None:
    """The element's window-space frame, or None when it has none.

    `point_ok` admits a zero-size frame as a POINT: on macOS `page-heading` is
    a 0x0 marker overlaid at the top edge of the page content
    (`FaunaKit/Views/PageTitle.swift`, the macOS arm — a toolbar principal item
    crashes NSToolbar there), so its frame is the content's top-centre point,
    and chrome covering the top of the page contains that point. A frame of
    all zeros is a detached slot, never a point."""
    raw = driver.get_attr(element_id, "frame")
    if not raw:
        return None
    x, y, w, h = (float(p) for p in raw.split(","))
    if point_ok and w == 0 and h == 0 and (x, y) != (0.0, 0.0):
        return x, y, w, h
    if w <= 0 or h <= 0:
        return None
    return x, y, w, h


def _intersects(a, b) -> bool:
    ax, ay, aw, ah = a
    bx, by, bw, bh = b
    return ax < bx + bw and bx < ax + aw and ay < by + bh and by < ay + ah


def _assert_no_occlusion(driver, chrome_ids, content_ids, where: str) -> None:
    chrome = {i: _frame(driver, i) for i in chrome_ids}
    missing = [i for i, f in chrome.items() if f is None]
    assert not missing, (
        f"{where}: chrome {missing} has no live frame — it must stay on screen; "
        + "; ".join(driver.diagnose(i, attrs=("frame",)) for i in missing)
    )
    content = {i: _frame(driver, i, point_ok=i == "page-heading") for i in content_ids}
    assert content.get("page-heading"), (
        f"{where}: page-heading has no frame; "
        f"{driver.diagnose('page-heading', attrs=('frame',))}"
    )
    overlaps = [
        f"{c} {chrome[c]} over {p} {content[p]}"
        for c in chrome_ids
        for p in content_ids
        if content.get(p) and _intersects(chrome[c], content[p])
    ]
    assert not overlaps, (
        f"{where}: global chrome covers page content (frames x,y,w,h, "
        f"window space): {overlaps}"
    )


@pytest.mark.macos
def test_macos_status_chrome_covers_no_page_content(logged_in_app):
    driver = logged_in_app.driver
    chrome_ids = ("connection-status", "sync-agent-status")
    for page, first_row in _PAGES:
        driver.navigate_to(page)
        driver.wait_for("page-heading", timeout=30)
        driver.wait_for(first_row, timeout=15)
        _assert_no_occlusion(
            driver, chrome_ids, ("page-heading", first_row), f"macos {page}"
        )


@pytest.mark.macos
def test_macos_status_chrome_stays_visible_with_the_sidebar_collapsed(logged_in_app):
    driver = logged_in_app.driver
    driver.navigate_to("conversations")
    driver.wait_for("page-heading", timeout=30)
    result = driver.call_command("sidebar_set_visible", {"visible": False})
    assert not (isinstance(result, dict) and "error" in result), (
        f"sidebar_set_visible refused: {result!r}"
    )
    try:
        for chrome_id in ("connection-status", "sync-agent-status"):
            driver.wait_for(chrome_id, timeout=15)
            assert driver.is_visible(chrome_id), (
                f"{chrome_id} is not visible with the sidebar collapsed; "
                f"{driver.diagnose(chrome_id, attrs=('frame',))}"
            )
        _assert_no_occlusion(
            driver,
            ("connection-status", "sync-agent-status"),
            ("page-heading",),
            "macos conversations, sidebar collapsed",
        )
    finally:
        driver.call_command("sidebar_set_visible", {"visible": True})


@pytest.mark.ios
def test_ios_connection_status_covers_no_page_content(logged_in_app):
    driver = logged_in_app.driver
    for page, _ in _PAGES:
        driver.navigate_to(page)
        driver.wait_for("page-heading", timeout=30)
        _assert_no_occlusion(
            driver, ("connection-status",), ("page-heading",), f"ios {page}"
        )
