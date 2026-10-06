"""Diagnostic for tag rendering."""
import time
import uuid
import pytest

pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]


def test_tag_rendering(logged_in_app):
    """Create post with tags and check element counts."""
    text = f"tagdiag-{uuid.uuid4().hex[:8]}"
    logged_in_app.feed.create_post_with_tags(text=text, tags="rust, svelte, wasm")
    time.sleep(2)

    driver = logged_in_app.driver

    post_card_count = driver.count("post-card")
    print(f"post-card count: {post_card_count}")

    tag_chip_global = driver.count("tag-chip")
    print(f"tag-chip global count: {tag_chip_global}")

    feed_post_text_count = driver.count("feed-post-text")
    print(f"feed-post-text count: {feed_post_text_count}")

    # Try to find our post
    for i in range(min(post_card_count, 3)):
        try:
            card_text = driver.get_text("post-card", index=i)
            print(f"post-card[{i}] text: {card_text[:80]!r}")
        except Exception as e:
            print(f"post-card[{i}] ERROR: {e}")

    for i in range(min(feed_post_text_count, 3)):
        try:
            post_text = driver.get_text("feed-post-text", index=i)
            print(f"feed-post-text[{i}]: {post_text[:80]!r}")
        except Exception as e:
            print(f"feed-post-text[{i}] ERROR: {e}")

    assert tag_chip_global > 0, "No tag-chip elements found globally"
