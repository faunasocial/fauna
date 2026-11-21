"""tier_3: text the app shows can be selected and copied
(`docs/goal/ui/README.md` § Text the person reads can be copied; the catalog
page `docs/features/copy-what-you-read.md`).

The rule is one decision per app — linux's `read_text.rs` turns on gtk's own
selection for every label that is not a control's caption, web shows its
read-only text as plain DOM text and every clickable row asks
`#lib/selection-click` before acting — so the witness is one driver read the
same on every app, `driver.is_text_selectable(id)` (`drivers/base.py`), asked of
the kinds of text the rule names: a message, a value, and text inside a row whose
click opens something. Web answers it with the selection a person makes, a real
mouse drag; linux with the gtk label's own Select All (its context menu's item,
since a drag cannot be made inside the process); macos with Select All
on the element's own SwiftUI text view (a click and Edit ▸ Select All, since a
pointer drag selects nothing while the app is not the active one); ios with a
long-press made inside the app process, read True when it selects in place or
raises the platform's Copy action (the edit menu, which the agent takes down
before it replies, so no menu stands into the next test); android with a real
long-press, read back with the selection's own Ctrl+C copy (or, when the copy
wrote nothing, the selection toolbar the press brought up). An app whose agent
does not answer the read yet is unbuilt there (`surfaces.toml`
`text-selectable-read`), never a pass.

What the drag-select does to the row it ends in, and whether a selection outlives
a repaint, need the selection itself read back (`driver.selected_text()`), which
web, macos, linux, ios and android serve (`text-selection-read`; macos reads it
through the first responder's own Services write, linux from the gtk label
holding it, ios from the first-responder text input, android with the
selection's own Ctrl+C copy, which leaves it standing). linux, macos and ios are excused from
outcome 3 by design (every row that opens something keeps its text unselectable
there — the page's `absences`). ios is excused from outcome 4 by design too: on
the app's pages the platform answers the long-press with the whole-text edit
menu, so no in-place selection exists for a repaint to keep (`e2e-conventions.md`
§ Driver primitives, *Reading whether text can be selected*).
"""

from __future__ import annotations

import time

import pytest

from helpers.app_surface import require_surface
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

# Generous, named, and latency-independent (convention 14): every wait below is a
# poll on state, and the ceiling only bounds a genuinely broken surface.
SURFACE_BUDGET_S = 60.0


def _banner_stands(app, message: str) -> bool:
    """The page's own `error-message` element is painted and carries `message`
    — the element itself, which the drag needs; `error_text()` may answer from
    the state protocol while the element is not (or no longer) in the tree."""
    driver = app.driver
    try:
        return driver.is_visible("error-message") and message in driver.get_text("error-message")
    except Exception:  # noqa: BLE001 — a repaint between the two reads
        return False


def _show_feed_error(app, message: str) -> None:
    """The feed page's `error-message`, raised through the feed's own error
    seam — the arrangement `test_feed_error.py` uses: the page's launch loads
    may replace the snapshot that carries the error, so the injection repeats
    until the banner element stands."""
    app.feed.navigate()
    deadline = time.monotonic() + SURFACE_BUDGET_S
    while time.monotonic() < deadline and not _banner_stands(app, message):
        app.feed.inject_error_for_test(message=message)
        try:
            wait_until(lambda: _banner_stands(app, message), 5.0, interval=0.1)
        except AssertionError:
            continue  # a settling load replaced the snapshot; inject again
    assert _banner_stands(app, message), (
        f"the injected feed error never stood on the page: error={app.error_text()!r} "
        f"{app.driver.diagnose('error-message')}"
    )


def _select_feed_error(app, message: str) -> bool:
    """Drag-select the standing feed error. A late launch load may clear the
    banner between it standing and the drag — the load doing its job, not the
    surface failing — so a drag that found no banner raises it again and drags
    again; a banner that stood through the drag answers for itself."""
    driver = app.driver
    deadline = time.monotonic() + SURFACE_BUDGET_S
    while True:
        _show_feed_error(app, message)
        selectable = driver.is_text_selectable("error-message")
        if selectable or _banner_stands(app, message) or time.monotonic() >= deadline:
            return bool(selectable)


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("copy-what-you-read")
def test_an_error_message_the_app_shows_can_be_selected(logged_in_app):
    """Outcome 1: an error or status message the app shows can be selected."""
    app = logged_in_app
    driver = app.driver
    require_surface(driver, "text-selectable-read")
    assert _select_feed_error(app, "a message you can copy (selectable text test)") is True, (
        "the page's error message must be selectable with the platform's own "
        f"gesture: {driver.diagnose('error-message')}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("copy-what-you-read")
def test_a_value_the_app_shows_can_be_selected(logged_in_app):
    """Outcome 2: a value the app shows — here this device's own key
    fingerprint on its card in the device list — can be selected."""
    app = logged_in_app
    driver = app.driver
    require_surface(driver, "text-selectable-read")
    app.backups.navigate_devices()
    driver.wait_for("device-card", timeout=SURFACE_BUDGET_S)
    # The fingerprint is painted once the account runtime has answered.
    wait_until(
        lambda: driver.is_visible("device-own-fingerprint"), SURFACE_BUDGET_S,
        diagnose=lambda: driver.diagnose("device-own-fingerprint"),
    )
    assert driver.is_text_selectable("device-own-fingerprint") is True, (
        "a value the app shows must be selectable: "
        f"{driver.diagnose('device-own-fingerprint')}"
    )


@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("copy-what-you-read")
def test_selecting_text_inside_a_row_that_opens_does_not_open_it(logged_in_app):
    """Outcome 3: a drag that selects text inside a post card — a row whose
    click opens the post — selects the text and does not open the post; a plain
    click on the same card still opens it."""
    app = logged_in_app
    driver = app.driver
    require_surface(driver, "text-selectable-read")
    require_surface(driver, "text-selection-read")
    body = "selecting this text opens nothing (selectable text test)"
    app.feed.navigate()
    app.feed.create_post(body)
    scope = "post-card[0]"
    assert body in driver.get_text("feed-post-text", scope=scope), driver.diagnose("feed-post-text")

    assert driver.is_text_selectable("feed-post-text", scope=scope) is True, (
        f"a post's text must be selectable: {driver.diagnose('feed-post-text')}"
    )
    selected = driver.selected_text() or ""
    assert selected.strip() and selected.strip() in body, (
        f"the drag must have selected part of the post's text, got {selected!r}"
    )
    driver.barrier()
    assert driver.is_absent("feed-post-detail-body"), (
        "a drag that selects text inside the card must not open the post: "
        f"{driver.diagnose('feed-post-detail-body')}"
    )

    # Selecting is not acting, and acting is unchanged: a click still opens it.
    app.feed.open_post_detail(0)
    wait_until(
        lambda: driver.is_visible("feed-post-detail-body"), SURFACE_BUDGET_S,
        diagnose=lambda: "a plain click on the card must still open the post: "
        + str(driver.diagnose("feed-post-detail-body")),
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.android
@pytest.mark.feature("copy-what-you-read")
def test_a_selection_survives_the_page_refreshing_unchanged_text(logged_in_app):
    """Outcome 4: a selection survives the page refreshing itself while the text
    is unchanged — the feed error is raised again with the same text (the store
    notifies the page as on any observer tick) and the selection still stands."""
    app = logged_in_app
    driver = app.driver
    require_surface(driver, "text-selectable-read")
    require_surface(driver, "text-selection-read")
    message = "a selection that outlives a repaint (selectable text test)"
    assert _select_feed_error(app, message) is True, driver.diagnose("error-message")
    before = (driver.selected_text() or "").strip()
    # The drag spans the whole banner, which may carry the page's own lead-in
    # ("Failed to load posts: …") around the injected message.
    shown = " ".join(driver.get_text("error-message").split())
    assert before and " ".join(before.split()) in shown, (
        f"the drag selected {before!r}, the banner shows {shown!r}"
    )

    app.feed.inject_error_for_test(message=message)
    driver.barrier()
    assert message in app.error_text(), app.error_text()
    after = (driver.selected_text() or "").strip()
    assert after == before, (
        f"a repaint that left the text unchanged must keep the selection: "
        f"before {before!r}, after {after!r}"
    )
