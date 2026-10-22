"""The store-listing screenshots — the real app, on fixture content, captured to PNG.

A store listing needs screenshots of the app as a user meets it
(`docs/goal/architecture/installers/windows.md` § Store distribution, the
screenshots paragraph). This module produces them through the harness rather than
by hand: the real app, signed in to a test nest, shows injected fixture content
(invented people at reserved domains, never anyone's real details), and the
driver sizes the app's OWN window and captures it alone — never the screen, so
no other window on a shared dev box can land in a listing image.

**Gate:** `--store-listing`. It writes files into the checkout instead of
guarding behaviour, so `conftest.py` prunes this directory without the flag.

    just e2e-store-listing-screenshots --app windows

**What a shot may show.** The harness drives the Debug build, which carries
every feature plane, while a store package may leave some out — the Microsoft
Store package has no payments, zaps or device-to-device sharing (the flavour
paragraph of the same section). So the fixture content sets nothing that paints
those planes: no tips, no prices, no paid tiers, and no folders page, whose
offline-share section paints whenever that plane is compiled in.

Each PNG is checked against Microsoft's desktop limits: at least 1366 × 768,
at most 3840 × 2160, under 50 MB.
"""

from __future__ import annotations

import shutil
import time
from pathlib import Path

import pytest

from drivers.base import PlatformDriver
from helpers import app_surface, budgets
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

REPO = Path(__file__).resolve().parents[4]
DEFAULT_OUT = REPO / "tests" / "e2e-unified" / "screenshots" / "store-listing"  # git-ignored

# The window's size in device-independent pixels: 1440 × 900 is 1440 × 900 px at
# 100 % and 2880 × 1800 at 200 %, so the visible frame (the window less its
# invisible resize border) clears 1366 × 768 and stays under 3840 × 2160 at
# every scale up to 240 %.
WINDOW_DIP = (1440, 900)
MIN_PX = (1366, 768)
MAX_PX = (3840, 2160)
MAX_BYTES = 50 * 1024 * 1024

_MINUTE_MS = 60_000


def _hex_id(n: int) -> str:
    """A fixed 32-byte hex id (author or post) — stable across runs."""
    return f"{n:064x}"


def _feed_posts(now_ms: int) -> list[dict]:
    """Bridged posts by invented authors at reserved domains.

    Fediverse and Nostr only: a bridged post's author face is what those bridges
    deliver today, while a Bluesky post paints no author name yet
    (`docs/guides/bridges-bluesky-nostr.md`, the status table) — a listing image
    must not show the app doing more than it does."""
    people = [
        ("Robin Ashgrove", "@robin@garden.example.org", "activitypub",
         "Spring cleaning at the community garden on Saturday. Bring gloves, we have the coffee.",
         14, 3),
        ("Tove Elmwood", "@tove@social.example.org", "activitypub",
         "Finished the cabin roof before the first frost. Pictures on Sunday, once the light is back.",
         31, 6),
        ("Kit Marlow", "kit@example.net", "nostr",
         "Reading group picks for November: two short novels and one very long poem.",
         9, 4),
        ("Ola Brennvik", "@ola@harbour.example.org", "activitypub",
         "The ferry timetable changes on Monday: the early boat now leaves at 06:40.",
         7, 1),
        ("Juniper Holt", "juniper@example.net", "nostr",
         "Third attempt at a rye sourdough. Better crust, the crumb still needs work.",
         22, 5),
    ]
    posts = []
    for i, (name, handle, source, body, likes, replies) in enumerate(people):
        posts.append({
            "post_id": _hex_id(0x5100 + i),
            "author": _hex_id(0xA100 + i),
            "author_display": {"display_name": name, "handle": handle, "avatar_url": None},
            "body": body,
            "verification": "Verified",
            "timestamp": now_ms - (i * 47 + 12) * _MINUTE_MS,
            "source": source,
            "like_count": likes,
            "reply_count": replies,
        })
    return posts


def _capture(driver, out_dir: Path, name: str) -> Path:
    """Capture the app's window as `<out_dir>/<name>.png`, checked against the
    desktop limits; returns the kept file."""
    shot = driver.capture_window(f"store-listing-{name}", *WINDOW_DIP)
    width, height = shot["width"], shot["height"]
    assert shot["settled"], (
        f"{name}: the window never rendered the same frame twice within the bridge's "
        f"deadline (an animation or a caret still running) — not keeping a mid-reflow image"
    )
    assert width >= MIN_PX[0] and height >= MIN_PX[1], (
        f"{name}: {width}x{height} px is under the store's {MIN_PX[0]}x{MIN_PX[1]} minimum "
        f"(scale {shot['scale']})"
    )
    assert width <= MAX_PX[0] and height <= MAX_PX[1], (
        f"{name}: {width}x{height} px is over the store's {MAX_PX[0]}x{MAX_PX[1]} maximum "
        f"(scale {shot['scale']})"
    )
    out_dir.mkdir(parents=True, exist_ok=True)
    kept = out_dir / f"{name}.png"
    shutil.copyfile(shot["path"], kept)
    assert kept.stat().st_size < MAX_BYTES, f"{name}: {kept.stat().st_size} bytes is over 50 MB"
    return kept


@pytest.fixture
def listing_dir(request, logged_in_app) -> Path:
    root = request.config.getoption("--store-listing-out")
    return (Path(root) if root else DEFAULT_OUT) / app_surface.app_name(logged_in_app.driver)


def _require_capture(driver) -> None:
    if type(driver).capture_window is PlatformDriver.capture_window:
        app_surface.skip_unbuilt(
            driver,
            surface="capture_window (a sized capture of the app's own window)",
            detail="built on windows (flaui-bridge /window/capture); the other "
            "store apps each need their own capture",
            tracked="docs/goal/architecture/installers/windows.md § Store distribution",
        )


def test_store_listing_feed(logged_in_app, listing_dir):
    """The feed: posts from people on Mastodon-compatible servers and on Nostr."""
    app = logged_in_app
    _require_capture(app.driver)
    posts = _feed_posts(int(time.time() * 1000))
    assert app.feed.seed_posts(posts) == len(posts), (
        f"the feed did not hold the {len(posts)} injected posts: "
        f"{app.driver.diagnose('post-card')}"
    )
    _capture(app.driver, listing_dir, "1-feed")


PEER = "lena@example.com"


def test_store_listing_conversation(logged_in_app, listing_dir):
    """Conversations: an open chat, with other chats and an email in the list."""
    app = logged_in_app
    _require_capture(app.driver)
    conv = app.conversations
    conv.inject_inbound_for_test(
        rail="Smtp", sender="events@library.example.org",
        subject="Late opening on Thursdays",
        body="From next week the library stays open until nine on Thursdays.",
    )
    conv.inject_inbound_for_test(
        rail="FaunaMls", sender="olav@example.org",
        body="Chapter four was quite the turn. Save your thoughts for Tuesday!",
    )
    conv.inject_and_open_thread(
        rail="FaunaMls", sender=PEER,
        body="Are we still on for the hike on Saturday?",
    )
    # Inbound only: the seam's forced-own message keeps the peer as its sender
    # (`seed_own_message`'s docstring), so an "own" reply would show the peer's name.
    conv.inject_inbound_for_test(
        rail="FaunaMls", sender=PEER,
        body="I will bring the thermos and the map. Meet at the trailhead at nine?",
    )
    wait_until(
        lambda: app.driver.count("dm-message-text") >= 2,
        budgets.UI_SETTLE_S,
        diagnose=lambda: app.driver.diagnose("dm-message-text"),
    )
    _capture(app.driver, listing_dir, "2-conversation")
