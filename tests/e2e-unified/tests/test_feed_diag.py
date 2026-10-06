"""Diagnostic test for feed post creation on Windows.

Traces each step of the compose flow and reads error state after submission.
"""
import time
import pytest

pytestmark = [pytest.mark.tier0, pytest.mark.tier_3]


def test_feed_compose_diagnostics(logged_in_app):
    """Step through compose and report what fails."""
    driver = logged_in_app.driver

    # 1. Are we on the feed page?
    state = driver.get_state()
    assert state is not None, "No state from agent"
    nav_view = state.get("nav", {}).get("stack", [{}])[0].get("view")
    print(f"Current view: {nav_view}")

    # 2. Is compose-text-field visible?
    visible = driver.is_visible("compose-text-field")
    print(f"compose-text-field visible: {visible}")
    assert visible, "compose-text-field not visible on feed page"

    # 3. Type a unique post body (unique so a re-run against the session-scoped
    #    nest can't false-positive on a prior run's identical post).
    post_text = f"diag-post-test-{time.monotonic_ns()}"
    driver.type_text("compose-text-field", post_text)
    time.sleep(0.5)

    # 4. Is post-submit-button visible?
    submit_visible = driver.is_visible("post-submit-button")
    print(f"post-submit-button visible: {submit_visible}")
    assert submit_visible, "post-submit-button not visible"

    # 5. Check state before submit — is session authenticated?
    session = state.get("session", {})
    print(f"authenticated: {session.get('authenticated')}")
    print(f"node_url: {session.get('node_url')}")
    print(f"actor_id: {session.get('actor_id')}")
    print(f"secret_hex present: {bool(session.get('secret_hex'))}")

    # 6. Count posts before
    initial_count = driver.count("feed-post-text")
    print(f"Posts before submit: {initial_count}")

    # 7. Click submit
    driver.click("post-submit-button")

    # 8. Poll up to 20s for the new post to appear. Assert by *content*, not
    # count: the feed is capped at the page limit (50), so once it's full the
    # count stays flat even though the new post lands at the top (newest-first).
    # A count-increase still satisfies the small-feed case.
    deadline = time.monotonic() + 20
    final_count = initial_count
    top_text = ""
    appeared = False
    while time.monotonic() < deadline:
        final_count = driver.count("feed-post-text")
        top_text = driver.get_text("feed-post-text") or ""
        if post_text in top_text or final_count > initial_count:
            appeared = True
            break
        time.sleep(0.3)

    error_state = driver.get_state("messages.error")
    print(f"Error after submit: {error_state!r}")

    has_error = logged_in_app.has_error()
    error_text = logged_in_app.error_text()
    print(f"has_error: {has_error}, error_text: {error_text!r}")
    print(f"Posts after submit: {final_count}; top post: {top_text!r}")

    if error_text:
        pytest.fail(f"Post submission produced error: {error_text}")
    if not appeared:
        pytest.fail(
            f"Post did not appear: top post {top_text!r} does not contain "
            f"{post_text!r} and count stayed {initial_count}"
        )
