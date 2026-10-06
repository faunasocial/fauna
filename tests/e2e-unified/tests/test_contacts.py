import pytest


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


@pytest.mark.feature("contacts")
def test_view_contacts(logged_in_app):
    """Navigate to contacts and verify the roster + knocks load without error.

    The contacts page hydrates by calling ``fauna.contacts.list`` +
    ``fauna.knocks.list`` on mount; a transport break (e.g. a client still
    calling the deleted ``GET /api/v1/contacts|knocks/{actor}`` HTTP routes,
    which now 404) is caught into the page's ``error-message`` banner. A bare
    ``count >= 0`` check passes even on that error (the roster falls back to
    empty), so assert the load actually succeeded.
    """
    logged_in_app.contacts.navigate()
    assert logged_in_app.contacts.contact_count() >= 0, (
        "contact roster should be queryable (count never negative): "
        f"{logged_in_app.driver.diagnose('contacts-view')}"
    )
    assert not logged_in_app.has_error(), (
        f"contacts page surfaced an error after load: "
        f"{logged_in_app.error_text()!r}"
    )


def test_actor_id_field_visible(logged_in_app):
    """Verify actor ID lookup field is visible on contacts page."""
    logged_in_app.contacts.navigate()
    assert logged_in_app.driver.is_visible("contact-actor-id-field"), (
        "contacts page should show the actor-id lookup field: "
        f"{logged_in_app.driver.diagnose('contact-actor-id-field')}"
    )
    assert logged_in_app.driver.is_visible("contact-actor-id-lookup"), (
        "contacts page should show the actor-id lookup button: "
        f"{logged_in_app.driver.diagnose('contact-actor-id-lookup')}"
    )


def test_contact_search_field_visible(logged_in_app):
    """Verify the contacts search field is present."""
    logged_in_app.contacts.navigate()
    assert logged_in_app.driver.is_visible("contacts-search-field"), (
        "contacts page should show the search field: "
        f"{logged_in_app.driver.diagnose('contacts-search-field')}"
    )


@pytest.mark.feature("contacts")
def test_contact_find_error_on_bad_lookup(logged_in_app):
    """Looking up a nonexistent actor ID should show an error."""
    logged_in_app.contacts.navigate()
    logged_in_app.contacts.find_by_actor_id("nonexistent_actor_00000000")
    error = logged_in_app.contacts.find_error_text()
    # Either a specific error element or the general error-message
    assert error or logged_in_app.has_error(), (
        "a bad actor-id lookup should surface an error (specific element or "
        f"the general error-message banner); find_error_text()={error!r} "
        f"error_text()={logged_in_app.error_text()!r}"
    )


@pytest.mark.feature("contacts")
def test_knock_count_accessible(logged_in_app):
    """Verify knock count is accessible (may be 0 on fresh nest)."""
    logged_in_app.contacts.navigate()
    count = logged_in_app.contacts.knock_count()
    assert count >= 0, (
        f"knock count should be queryable (count never negative); got {count}: "
        f"{logged_in_app.driver.diagnose('contacts-view')}"
    )


def test_contacts_view_visible(logged_in_app):
    """Verify the contacts-view container is present."""
    logged_in_app.contacts.navigate()
    assert logged_in_app.driver.is_visible("contacts-view"), (
        "the contacts-view container should be present: "
        f"{logged_in_app.driver.diagnose('contacts-view')}"
    )


@pytest.mark.feature("contacts")
def test_roster_filter_narrows_by_handle(handled_logged_in_app):
    """Typing in ``contacts-search-field`` narrows the loaded roster by handle.

    This is the UI-wiring guard for the roster-filter lift (contacts.md §
    Implementation status, Order of work step 4): every app routes the
    ``contacts-search-field`` query through the shared
    ``fauna_core::format::contact_matches_filter`` predicate over the contact's
    ``handle`` / ``domain`` / ``actor-id`` (web via ``contactMatchesFilter``, linux
    via the actor_id→handle/domain side-index). The matching *logic* is unit-tested
    (``fauna-core`` ``format.rs`` ``contact_matches_filter_over_handle_domain_actor_id``)
    and the wire carrying ``handle``/``domain`` is tier_3-tested
    (``conformance_contacts.rs``); this asserts the rendered roster actually
    hides/shows rows as the user types.

    ``handled_logged_in_app`` logs the app in as a *handled* actor (alice) on a
    open-registration nest. We register a second handled actor (bob) whose
    handle is a letters-only string (so a handle query can't accidentally match
    the hex actor-id), accept him as alice over WS-RPC so he lands in her roster
    as an ``accepted`` contact (``fauna.knocks.accept`` upserts unconditionally —
    ``contacts_handlers.rs``), then drive the search field. The driver's
    ``count`` only sees *showing* rows — web omits filtered rows from the DOM,
    linux marks ``set_filter_func``-filtered rows ``!is_child_visible``
    (``automation/find.rs`` ``is_showing``), and macOS's SwiftUI ``.filter`` over
    the roster ``ForEach`` never renders a filtered row, so it doesn't register
    in-process — every app reads a narrowed roster as a lower ``contact-name``
    count.

    Runs on every ``HttpBridgeDriver`` client, which includes the native
    macOS/iOS in-process drivers (``MacosInProcessDriver``/``IosInProcessDriver``
    subclass ``InProcessAgentDriver(HttpBridgeDriver)``) — ``handled_logged_in_app``
    only ``skip``s a non-bridge driver, so this is NOT web/linux-exclusive.
    VERIFIED green ``--client macos`` once apple wired the macOS ``searchFilter``
    (before that the macOS field was a no-op
    and this would have failed); web/linux green from authoring. iOS VERIFIED green
    N+36 once apple registered the `contact-name`/`contact-status` rows in-process
    (`automationText`/`automationValue` parity with macOS; before
    that `contact-name` count=0 and this failed).
    """
    import secrets

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN

    app = handled_logged_in_app
    alice = app.handled_actor
    nest = app.handled_nest

    # Register bob with a letters-only handle and make alice accept him so he is
    # the sole row in her roster. Search "rosterbob" can only match his handle
    # (hex actor-ids have no letters > 'f'), isolating the handle dimension.
    bob_handle = "rosterbob" + secrets.token_hex(2)
    bob = register_handled_actor(nest["port"], handle=bob_handle, domain=MAIL_PRIMARY_DOMAIN)
    with WsRpcAdminClient(
        nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as client:
        client.call("fauna.knocks.accept", {"peer_id": bob["actor_id_hex"]})

    # First mount of the contacts page fetches the now-seeded roster.
    app.contacts.navigate()
    app.driver.wait_for("contact-name", timeout=15)
    before = app.contacts.contact_count()
    assert before >= 1, (
        "alice's roster should contain the accepted contact bob after seeding: "
        f"count={before}; {app.driver.diagnose('contacts-view')}"
    )

    # Handle match → bob stays (the new capability: web matched actor-id only
    # before the lift, so this row would vanish on a regression).
    app.contacts.search("rosterbob")
    assert app.contacts.contact_count() == before, (
        f"a query matching bob's handle ({bob_handle!r}) should keep his row; "
        f"roster narrowed unexpectedly to {app.contacts.contact_count()}"
    )
    assert app.driver.is_absent("contacts-no-matches"), (
        "the no-matches message must not show while the filtered roster still "
        f"has rows: {app.driver.diagnose('contacts-no-matches')}"
    )

    # No match → roster narrows to empty. A non-empty roster narrowed to zero
    # rows must show a DISTINGUISHABLE "no matches" message, not silently
    # render nothing (the bug: contacts.md § Errors & edge cases) nor the
    # true-empty-roster "No contacts yet." text (linux's mirror-image bug).
    app.contacts.search("zzqqxxnomatch")
    assert app.contacts.contact_count() == 0, (
        "a non-matching query should hide every contact row; still showing "
        f"{app.contacts.contact_count()}"
    )
    assert app.driver.is_visible("contacts-no-matches"), (
        "a roster filtered to zero matches should show a distinguishable "
        f"'no matches' message: {app.driver.diagnose('contacts-no-matches')}"
    )

    # Cleared → roster restored, no-matches message gone.
    app.contacts.search("")
    assert app.contacts.contact_count() == before, (
        "clearing the query should restore the full roster; got "
        f"{app.contacts.contact_count()} (expected {before})"
    )
    assert app.driver.is_absent("contacts-no-matches"), (
        "the no-matches message must not show once the query is cleared: "
        f"{app.driver.diagnose('contacts-no-matches')}"
    )
