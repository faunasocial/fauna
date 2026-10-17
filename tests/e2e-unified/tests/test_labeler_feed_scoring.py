"""A subscribed community labeler over public posts scores the subscriber's
feeds on the subscriber's own device (``content-moderation-and-ranking.md``
§ Tier-3 → *Where it runs*, position (1), ruled 2026-10-09).

The module runs in the shared feed manager's sealed-scorer seam — the one
position where the subscriber's lens meets a post — so every app inherits it
from shared Rust; tui leads (the lead-app rule). The nest scores nothing for
the factor (its term is 0, invariant (f), pinned at the nest tier by
``tests/api/test_labeler_subscriptions.py``), so a re-rank here is the app's
own scoring and nothing else.

tier_3: real nest, real app, real wire. Publishing is a publisher's act no app
offers for a ``wasm`` module, so it is fixture setup (convention 8 carve-out
(b), ``helpers/labeler_publish.py``); subscribing, posting, composing the feed
and opening it are the user's gestures, driven through the app.

Each test publishes its own labeler and posts its own uniquely-tagged pair, and
unsubscribes before it ends — ``test_user`` is session-scoped.
"""

import time
import uuid

import pytest

from helpers.labeler_publish import (
    FIXTURES,
    new_publisher,
    publish_labeler_version,
)

pytestmark = pytest.mark.tier_3

# The committed cat detector: emits one `cat` label (900 per-mille) on any
# input carrying the bytes "cat", and nothing otherwise.
_CAT_WAT = (FIXTURES / "cat_labeler.wat").read_bytes()

# A post the module labels, and a control it does not. The control carries no
# "cat" anywhere (its unique suffix is hex, which never spells it).
_CAT_TEXT = "the cat curled up on the windowsill"
_CONTROL_TEXT = "the bus timetable changes on monday"

# A label-output revision no build reads (`LABEL_ABI_CURRENT` is 1).
_NEWER_LABEL_ABI = 999


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _publish(run_seal_helper, nest_instance) -> tuple[bytes, bytes, str]:
    """Version 1 of a fresh post-kind `wasm` labeler over the cat fixture.
    Returns ``(labeler_id, publisher_seed, factor)``."""
    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, _CAT_WAT,
        version=1, content_kind="post", artifact_kind="wasm",
    )
    return labeler_id, pub_seed, "labeler:" + labeler_id.hex()


def _subscribe(app, factor: str) -> None:
    cat = app.labeler_catalog
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    assert index is not None, (
        f"published labeler {factor!r} not in the catalog; error={app.error_text()!r}"
    )
    cat.subscribe(index)
    assert cat.wait_for_subscribed_state(index, subscribed=True), (
        f"subscribe never flipped the row; error={app.error_text()!r}"
    )


def _unsubscribe(app, factor: str) -> None:
    cat = app.labeler_catalog
    cat.navigate_catalog()
    index = cat.find_index_by_factor(factor)
    if index is not None and cat.is_unsubscribe_visible(index):
        cat.unsubscribe(index)
        cat.wait_for_subscribed_state(index, subscribed=False)


def _post_pair(app) -> tuple[str, str]:
    """Post the cat post FIRST and the control after it, so the recency
    tiebreak puts the control above: only the labeler can invert them.
    Returns the two needles (their unique tags, matched in the rendered cards)."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.feed.open_feed("General")
    cat_needle, control_needle = _unique("cat"), _unique("control")
    # Hex suffixes never spell "cat"; the needle prefix is on the cat post only.
    assert "cat" not in control_needle
    app.feed.create_post(text=f"{_CAT_TEXT} {cat_needle}")
    time.sleep(1.2)  # sleep-ok: strictly-newer created_at (epoch-seconds resolution)
    app.feed.create_post(text=f"{_CONTROL_TEXT} {control_needle}")
    return cat_needle, control_needle


def _open_factor_feed(app, factor: str) -> None:
    feed_name = _unique("labeler-feed")
    app.feed.create_feed_with_factor(name=feed_name, factor_label=factor, weight="5.0")
    app.feed.open_feed(feed_name)


def _positions(app, cat_needle: str, control_needle: str, timeout: float = 30.0):
    """Wait until both posts render (a state, never a time — convention 14),
    then return their positions; ``None`` for whichever never rendered."""
    def find(needle: str):
        return next(
            (
                i
                for i in range(app.feed.post_count())
                if needle in (app.feed.post_text(i) or "")
            ),
            None,
        )

    deadline = time.monotonic() + timeout
    a, b = find(cat_needle), find(control_needle)
    while (a is None or b is None) and time.monotonic() < deadline:
        time.sleep(0.3)
        a, b = find(cat_needle), find(control_needle)
    return a, b


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("community-labelers")
def test_a_subscribed_labeler_over_public_posts_scores_the_posts_you_see(
    logged_in_app, nest_instance, run_seal_helper
):
    """`community-labelers` outcome 10: subscribe to a `wasm` labeler over
    public posts, compose it into a feed, and the post it labels orders above
    a newer one it does not — scored on this device, in the shared seam."""
    app = logged_in_app
    _, _, factor = _publish(run_seal_helper, nest_instance)
    _subscribe(app, factor)
    try:
        cat_needle, control_needle = _post_pair(app)
        _open_factor_feed(app, factor)
        cat_at, control_at = _positions(app, cat_needle, control_needle)
        assert cat_at is not None and control_at is not None, (
            f"both posts must render in the composed feed (cat at {cat_at}, "
            f"control at {control_at}); error={app.error_text()!r}"
        )
        assert cat_at < control_at, (
            f"the subscribed labeler's cat label must lift the cat post over the "
            f"NEWER control (so this is not recency): cat at {cat_at}, control "
            f"at {control_at}; error={app.error_text()!r}"
        )
    finally:
        _unsubscribe(app, factor)


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("community-labelers")
def test_a_labeler_your_app_is_too_old_to_read_leaves_your_feeds_as_they_were(
    logged_in_app, nest_instance, run_seal_helper
):
    """`community-labelers` outcome 14: a labeler subscribed at a revision this
    app reads, then republished as one it does not (`label_abi` newer than
    `LABEL_ABI_CURRENT`), leaves the feed in the order it would have without
    it — the prepare step refuses the newer module and composes nothing
    (invariant (e)), so it never scores wrongly. The subscription predates the
    revision because inspect refuses to subscribe a too-new labeler outright."""
    app = logged_in_app
    labeler_id, pub_seed, factor = _publish(run_seal_helper, nest_instance)
    _subscribe(app, factor)
    try:
        publish_labeler_version(
            run_seal_helper, nest_instance["url"], labeler_id, pub_seed, _CAT_WAT,
            version=2, content_kind="post", artifact_kind="wasm",
            label_abi=_NEWER_LABEL_ABI,
        )
        cat_needle, control_needle = _post_pair(app)
        _open_factor_feed(app, factor)
        cat_at, control_at = _positions(app, cat_needle, control_needle)
        assert cat_at is not None and control_at is not None, (
            f"both posts must render in the composed feed (cat at {cat_at}, "
            f"control at {control_at}); error={app.error_text()!r}"
        )
        assert control_at < cat_at, (
            f"a labeler this app cannot read must leave the recency order as it "
            f"was — the newer control first — never score the cat post up: cat "
            f"at {cat_at}, control at {control_at}; error={app.error_text()!r}"
        )
    finally:
        _unsubscribe(app, factor)
