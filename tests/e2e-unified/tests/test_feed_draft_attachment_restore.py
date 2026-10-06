"""tier_3: a half-written post's attached file comes back by name after an app
restart, its submit refuses rather than posting the text alone, and the remove
control lets the author post without it — ``docs/goal/ui/feed.md`` § Persistence
→ *Attachments by content address*.

The sibling of ``test_feed_draft_persistence.py``, which proves the draft's TEXT
survives. A draft carries its attachment as a handle — ``{name, size}``, never
bytes — because this rail uploads a file only when the post is submitted
(``docs/goal/ui/media.md`` § Encryption at rest). After a relaunch the file the
author picked is therefore a name with nothing behind it on this device, and
three things must hold, each asserted through the UI the author looks at:

1. the composer names the file (``compose-file-ready``), so the refusal never
   names a file the bar does not show;
2. posting refuses on ``compose-error`` with ``feed.compose_attachment_missing``
   naming that file, and keeps the draft — the text is never published alone;
3. ``compose-file-remove`` drops the handle, and the post then goes out
   text-only, because the author chose that.

Step 2's precondition is the attach-time half of the same ruling: a pick must
reach the shared manager as its hash-less handle the moment it is made, or the
draft saved before the submit carries no file at all and a relaunch loses it
with nothing left to refuse.

**tui leads** (``apps/fauna-tui/src/feed/mod.rs``: a typed ``compose-file`` path
stages its hash-less handle on the manager at once, and the bar renders off the
snapshot). The other apps join by adding their marker line as they lift the same
chip and remove control — the lift is the marker, not a new test.

The restart is the DEFAULT fresh-store relaunch, never
``preserve_state_across_relaunch()``: the handle has to come back from the nest's
``__drafts`` plane, and a surviving local pick would let step 1 pass while hiding
exactly the no-bytes-here case step 2 exists for.

**Ordering is what makes the sealed draft blob assertable.** The blob is opaque
to the test, so "a save landed" is observed as "the blob changed", and each wait
below follows exactly one change to the draft's own fields (text, then the
attach, then the remove, then the post clearing the composer). The refusal in
between changes none of them — a compose error is not part of a draft — so it
saves nothing and cannot be mistaken for the next save.

Latency-independent throughout (e2e convention 14): every wait polls the
observable that IS the contract — the nest's draft blob, the restored text, the
chip, the error — and its budget is a ceiling a green run never pays.
"""

import time
import uuid
from pathlib import Path

import pytest

from i18n.strings import S

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    # android already staged the pick + rendered the chip/remove before this
    # row; the id lift below is what makes it findable. android e2e only
    # compiles here — it needs a host emulator to actually run
    # (`actions/feed.py`), so this mark is build-verified, not run-verified.
    pytest.mark.android,
    pytest.mark.windows,
    # apple joined in its catalog trickle-down pass. The product leg landed
    # 2026-09-15 (stage-at-pick, name+size chip, `compose-file-remove`) — shared
    # `FeedVM.attachComposeFile`/`removeComposeAttachment`, with both shells wiring
    # `compose-file-ready`/`compose-file-remove`; adding these two markers was
    # already named in that work's own definition of success and never done.
    pytest.mark.macos,
    pytest.mark.ios,
    # The debounce itself must land this draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]

#: The draft rail this test drives (``fauna_protocol::drafts::DRAFT_RAILS``).
RAIL = "posts"

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def _poll(read, done, timeout: float = 15.0):
    """Read until ``done(value)`` or the budget runs out; returns the last value
    read, for the failure message."""
    deadline = time.monotonic() + timeout
    value = read()
    while not done(value) and time.monotonic() < deadline:
        time.sleep(0.2)
        value = read()
    return value


def _text_if_visible(driver, element_id: str) -> str:
    return driver.get_text(element_id) if driver.is_visible(element_id) else ""


def _draft_blob(node_url, actor_id, signing_key):
    """The rail's sealed blob as the nest holds it, read by a fresh side-channel
    device for the same actor."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        return dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")


def _wait_draft_saved(node_url, actor_id, signing_key, previous, timeout: float = 20.0):
    """Poll until the debounced ``fauna.drafts.put`` lands a blob other than
    ``previous``; ``None`` on timeout."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        blob = _draft_blob(node_url, actor_id, signing_key)
        if blob is not None and blob != previous:
            return blob
        time.sleep(0.5)
    return None


@pytest.mark.feature("drafts-survive")
def test_restored_draft_attachment_is_named_refused_and_removable(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    driver = app.driver
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])
    filename = FIXTURE_IMAGE.name
    # Unique per run, not a fixed literal: `nest_instance`/`test_user` are
    # session-scoped (shared across every app this test's marker list runs
    # against in one pytest invocation), and step 7 below actually PUBLISHES
    # this text — a static body would collide with the post an earlier app's
    # parametrization already left on the same actor's feed, failing step 5's
    # "nothing published yet" assertion on the second app for a reason that
    # has nothing to do with that app's own behavior (mirrors
    # test_feed_manager_singleton_reset_web.py's uuid4-suffixed probe bodies).
    body = f"a half-written post whose picture stays on the device it was picked on {uuid.uuid4().hex[:8]}"

    baseline = _draft_blob(node_url, actor_id, signing_key)

    # 1. The text first, saved — so the next save can only be the attach's.
    app.feed.navigate()
    app.feed.open_composer()
    driver.type_text("compose-text-field", body)
    typed = _poll(app.feed.compose_body_text, lambda t: t == body)
    assert typed == body, f"precondition: the post text must be in the composer, got {typed!r}"
    text_saved = _wait_draft_saved(node_url, actor_id, signing_key, baseline)
    assert text_saved is not None, (
        f"precondition: the typed draft must reach the nest __drafts plane at path={RAIL!r}"
    )

    # 2. Attach. The bar names the file at once, and the draft is saved again —
    #    carrying the file's handle, the only thing that can survive a relaunch.
    driver.set_input_files("compose-file", str(FIXTURE_IMAGE))
    chip = _poll(lambda: _text_if_visible(driver, "compose-file-ready"), lambda t: filename in t)
    assert filename in chip, (
        f"attaching {filename!r} must name it on compose-file-ready, got {chip!r}; "
        f"error={app.error_text()!r}"
    )
    attach_saved = _wait_draft_saved(node_url, actor_id, signing_key, text_saved)
    assert attach_saved is not None, (
        "attaching must re-save the draft with the file's handle — the pick has to reach "
        "the shared manager when it is made, not at submit, or a relaunch loses the file "
        "with nothing left to refuse"
    )

    # 3. Force-quit + relaunch on a fresh store: only what reached the nest returns.
    driver.hard_reload()
    app.feed.navigate()
    app.feed.open_composer()
    restored = _poll(app.feed.compose_body_text, lambda t: t == body)
    assert restored == body, (
        f"the draft text did not survive the restart (expected {body!r}, got {restored!r}); "
        f"error={app.error_text()!r}"
    )

    # 4. The restored draft's file is named on the bar, though this device holds
    #    no bytes for it.
    chip = _poll(lambda: _text_if_visible(driver, "compose-file-ready"), lambda t: filename in t)
    assert filename in chip, (
        f"the restored draft's attachment must be named on compose-file-ready before any "
        f"submit, got {chip!r} — otherwise the refusal names a file the bar does not show"
    )

    # 5. Posting refuses by name and keeps the draft; nothing is published.
    driver.click("post-submit-button")
    expected = S.feed.compose_attachment_missing(filename=filename)
    refusal = _poll(lambda: _text_if_visible(driver, "compose-error"), lambda t: t == expected)
    assert refusal == expected, (
        f"posting a restored draft whose file is not on this device must refuse on "
        f"compose-error with {expected!r}, got {refusal!r}; error={app.error_text()!r}"
    )
    assert app.feed.compose_body_text() == body, "the refused draft must keep its text"
    assert not app.feed.post_text_visible(body), (
        "the text must never be published without the file the author attached"
    )

    # 6. Remove the attachment: the chip goes and the removal is saved.
    driver.click("compose-file-remove")
    still_there = _poll(lambda: driver.is_visible("compose-file-ready"), lambda v: not v)
    assert not still_there, "compose-file-remove must drop the attachment's chip"
    assert app.feed.compose_body_text() == body, "removing the file must keep the text"
    removal_saved = _wait_draft_saved(node_url, actor_id, signing_key, attach_saved)
    assert removal_saved is not None, "removing the attachment must re-save the draft"

    # 7. Now the post goes out text-only, because the author chose that.
    driver.click("post-submit-button")
    assert app.feed.wait_for_post_text(body), (
        f"with the attachment removed, the post must publish text-only; "
        f"compose-error={_text_if_visible(driver, 'compose-error')!r} error={app.error_text()!r}"
    )

    # 8. Cleanup, asserted: the composer the post cleared must be saved as the
    #    draft, or this text is restored into the next test's composer on the
    #    session-scoped user (the leak ``test_feed_draft_persistence.py`` step 6
    #    documents).
    #
    #    "Cleared" is checked as "the post's own body is gone", not "== ''" —
    #    windows' compose-text-field falls back to scraping the PlaceholderText
    #    when the WinUI TextBox itself reads back empty (documented on
    #    ``ActionLayer.feed.compose_body_text``'s windows arm), so an emptied
    #    composer there reads back as the localized "Write a post..." hint,
    #    never "". Every other app's ``compose_body_text`` has no such
    #    fallback and reads back "" — so this predicate is exactly as strict
    #    as ``== ""`` there, and strict enough here: the placeholder can never
    #    itself contain the post's own body.
    cleared = _poll(app.feed.compose_body_text, lambda t: body not in t)
    assert body not in cleared, f"a published post must clear the composer, got {cleared!r}"
    assert _wait_draft_saved(node_url, actor_id, signing_key, removal_saved) is not None, (
        "cleanup: the cleared composer must persist to the nest __drafts plane, or it "
        "leaks into the next test. Every assertion above already PASSED — look at the "
        "post-submit compose clear on this app, not at the attachment chip."
    )
