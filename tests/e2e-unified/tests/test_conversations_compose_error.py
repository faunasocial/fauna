"""tier_3: a failed compose-send surfaces on the conversations page's
``error-message`` element (``docs/goal/ui/conversations.md`` § Errors & edge
cases) instead of being swallowed — the e2e half of the silent-no-op
fix ("I click Send and nothing happens").

The shared mechanism is ``ComposeState.send_state`` (``Idle | Sending |
Failed { reason }``): ``ConversationsManager::send``/``send_new_thread`` stamp
``Failed { reason }`` on the thread's draft on a backend send error, and each
app renders that into its page ``error-message`` element (linux:
``views/conversations/detail.rs`` ``render()`` reads the active compose's
``send_state``). Linux is the lead app; web/windows/tui/macos/ios render the
same shared state into their own ``error-message`` as the cross-area lift.
android joined: ``ConversationDetailScreen``'s
``pageErrorReason`` already combined ``snapshot?.error`` and
``compose.sendState``'s ``Failed`` arm into the one ``Ids.ERROR_MESSAGE``
surface — the only missing piece was the ``conversations_inject_page_error``
test-agent command itself, added this pass mirroring linux's
``handle_conversations_inject_page_error`` / windows'
``ConversationsCommands.InjectPageError`` (``conversations_inject_send_failure``
already existed).

``reason`` is a shared ``LocalizedText`` (the single key
``conversations.unified.error_send`` plus the backend's own detail as
``{message}``) — the same carrier ``ConversationsSnapshot.error`` uses, since
architectural rule 3 ("never hardcode English") governs the whole element rather
than one of its two producers. Every app therefore resolves it through its own
i18n pipeline, which is why the assertions below check *containment plus
resolution* rather than verbatim equality.

Why an injection seam and not a real failure: a mail-OFF nest does NOT fail the
send — ``email_handlers::enqueue_outbound`` writes a queue row and returns
``Ok`` (``remote_queued``), so the client send *succeeds into a void* and
``send_state`` never reaches ``Failed``. There is no product path that fails a
send on demand, so this drives ``ConversationsManager::inject_send_failure_for_test``
— the same observable state a real backend rejection leaves, proven by the
shared characterization test ``failed_send_stamps_send_state_failed_for_surfacing``.

The file also covers the page's **other** producer of ``error-message``: a
failed **membership/label** wire op (``confirm_add_participant`` /
``remove_participant`` / ``rename_thread``), which stamps
``ConversationsSnapshot.error`` rather than a compose's ``send_state``. Until
2026-07-29 those three swallowed their failures entirely, so a failed add closed
the overlay and did nothing visible — the dropped-command shape
``docs/goal/architecture/testing.md`` point 11 forbids. tui is the lead app leg;
the other six are entrusted and widen the marker set on
that test as each lands.
"""

import time

import pytest

# Generous, named, and latency-independent (testing.md point 14): the assertion
# is on *state* — the label eventually carries the reason — never on how fast a
# loaded box got there. A green run pays only the poll interval; the ceiling
# exists solely so a genuinely broken surface fails in bounded time rather than
# riding the 900s per-test timeout.
ERROR_SURFACE_BUDGET_S = 60.0

# Linux was the lead app for compose-error surfacing; web/windows/tui are
# lifted (each renders the same shared ComposeState.send_state into its
# error-message via the per-app `conversations_inject_send_failure` hook).
# apple/android remain the cross-area follow-on. Per-function markers (not a
# module-level `pytestmark`) because the windows-only copy-button test below
# must NOT inherit `tui`/`macos`/`ios` — a module-level list is a UNION over
# every item in the file (`conftest.py` `pytest_collection_modifyitems`), so
# it would incorrectly pull that test into every one of those `--client` runs.
@pytest.mark.tier_3
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("conversations")
def test_failed_send_surfaces_on_error_message(logged_in_app):
    app = logged_in_app

    app.conversations.navigate()

    # A clean conversations page surfaces no error (the page-level error-message
    # label is built hidden and render() keeps it hidden when send_state != Failed).
    assert not app.driver.is_visible("error-message"), (
        "conversations page must start with no error surfaced"
    )

    # Materialize + open (select) a thread to fail a send on. create_mls_group
    # bypasses welcome / key-package distribution and opens the thread, so the
    # detail pane's `active_detail` branch — the one a real new-thread send
    # failure lands on (send_new_thread selects the materialized thread before
    # send stamps Failed) — is the surface under test. The new-thread compose is
    # closed, so it does not shadow the read.
    thread_id = app.conversations.create_mls_group(["bob@self-nest.test"])
    assert thread_id, "fixture group thread must materialize + open"
    assert not app.driver.is_visible("error-message"), (
        "an opened thread with no send failure must still show no error"
    )

    reason = "nest rejected fauna.email.send"
    app.conversations.inject_send_failure_for_test(thread_id, reason)

    # The injection stamps `send_state = Failed { reason }`, selects the thread,
    # and fires the snapshot observer; the linux error-message label renders on
    # the deferred UI-thread refresh that the command return races, so poll for
    # the label to appear (condition-based waiting, per the action layer).
    deadline = time.time() + 5.0
    while time.time() < deadline:
        if app.driver.is_visible("error-message"):
            break
        time.sleep(0.1)

    assert app.driver.is_visible("error-message"), (
        "a failed compose-send must surface on the page error-message label; it "
        f"stayed hidden (the send failure was swallowed). error_text={app.error_text()!r}"
    )
    # The reason is a `LocalizedText` (`conversations.unified.error_send` + the
    # backend detail as `{message}`), so assert the two mutations an equality
    # check could not separate: the backend's own detail reaches the user, and
    # the app RESOLVED the key rather than painting it. Same three-assert shape
    # as the membership test below — one carrier, one assertion style.
    shown = app.driver.get_text("error-message")
    assert reason in shown, (
        f"the error-message must show the backend's own reason (got {shown!r})"
    )
    assert "conversations.unified" not in shown, (
        f"the i18n key must be resolved through the app's pipeline, not painted raw: {shown!r}"
    )
    assert "send" in shown.lower(), (
        "the resolved text must be the send template, not some other error the "
        f"page happened to be showing: {shown!r}"
    )


@pytest.mark.tier_3
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.android  # marked 2026-09-13
@pytest.mark.feature("conversations")
def test_failed_membership_op_surfaces_on_error_message(logged_in_app):
    """The page's second error producer: a failed add/remove/rename stamps
    ``ConversationsSnapshot.error``, not a compose's ``send_state``.

    tui is the lead app leg; linux (`page_error_text` in `detail.rs`) and web
    (`membershipError` in `+page.svelte`, `injectPageError` wasm seam) landed
    2026-07-29 / 2026-08-02 respectively.

    **windows joined 2026-08-04**, once the collision that made this whole
    assertion class unobservable there was fixed. Worth carrying, because the
    shape recurs: windows' `error-message` AutomationId was worn by *three*
    surfaces at once — `MainPage.GlobalErrorText`, 24 per-page 1×1
    `ErrorTextMirror` TextBlocks, and the page's own InfoBar — and the first two
    were **unconditionally on-screen** (the mirrors' "cleared" value was a
    literal `" "`). The FlaUI bridge resolves an id window-wide and checks only
    `!IsOffscreen`, so `is_visible("error-message")` was *structurally* true on a
    clean page and the opening `assert not ...` could never fail — nor pass
    meaningfully. The fix makes each shim leave the UIA tree when it carries no
    message (`MessageShim.ShouldShow`), which is what linux gets structurally
    from `automation/find.rs::is_showing` and tui/web from building the surface
    hidden. Note both tests below assert only *positives* after the first line,
    which is why the collision survived: a positive assertion passes whether or
    not the mechanism is real.

    **Widen this marker set** as each of macos/ios lands its read
    (the 2026-07-29 conversations page-error block).
    android joined: the render (this
    screen's ``pageErrorReason``) was already app-agnostic-shared, but the
    ``conversations_inject_page_error`` test-agent command itself did not
    exist on android's ``TestAgent`` until this pass — so "an arm that lands
    needs nothing here but its marker" holds for the *render* half but must
    never be assumed for the *test-agent wiring* half; check both.
    """
    app = logged_in_app

    app.conversations.navigate()
    assert not app.driver.is_visible("error-message"), (
        "conversations page must start with no error surfaced"
    )

    # The `{message}` substitution, distinctive enough that finding it in the
    # rendered label proves the app resolved *this* injection and did not just
    # happen to be showing some other error.
    detail = "no key package published for that person"
    app.conversations.inject_page_error_for_test(detail)

    deadline = time.time() + ERROR_SURFACE_BUDGET_S
    while time.time() < deadline:
        if app.driver.is_visible("error-message"):
            break
        time.sleep(0.1)

    assert app.driver.is_visible("error-message"), (
        "a failed membership wire op must surface on the page error-message "
        "label; it stayed hidden, which is exactly the swallowed-failure shape "
        f"this surface exists to close. error_text={app.error_text()!r}"
    )
    shown = app.driver.get_text("error-message")
    assert detail in shown, (
        "the backend's own reason must reach the user — an unrecoverable "
        "refusal (a cross-nest member whose roster this nest cannot read) is "
        f"only actionable if it says why. got {shown!r}"
    )
    # Two mutations assertion-by-existence would pass, so both are named: the
    # app must RESOLVE the i18n key rather than paint it, and the resolved
    # template must actually be the add-participant one.
    assert "conversations.unified" not in shown, (
        f"the i18n key must be resolved through the app's pipeline, not painted raw: {shown!r}"
    )
    assert "add" in shown.lower(), (
        "the resolved text must be the add-participant template, not some other "
        f"error the page happened to be showing: {shown!r}"
    )


# Windows-only: `error-message-copy-button` (ui.yaml optional_elements, registry
# line ~4057) is windows-first via the CopyableInfoBar UserControl
# (Controls/CopyableInfoBar.xaml) — other apps adopt the affordance later.
# This is the mechanism half of the verification
# (docs/goal/ui/conversations.md § Errors & edge cases); the pixels half (icon
# placement, drag-selectable text) needs a human and is captured separately.
@pytest.mark.tier_3
@pytest.mark.windows
def test_failed_send_error_copy_button_copies_full_text(logged_in_app):
    app = logged_in_app

    app.conversations.navigate()
    thread_id = app.conversations.create_mls_group(["bob@self-nest.test"])
    assert thread_id, "fixture group thread must materialize + open"

    reason = "nest rejected fauna.email.send (copy-button check)"
    app.conversations.inject_send_failure_for_test(thread_id, reason)

    deadline = time.time() + 5.0
    while time.time() < deadline:
        if app.driver.is_visible("error-message"):
            break
        time.sleep(0.1)
    assert app.driver.is_visible("error-message"), (
        "a failed compose-send must surface on error-message before the copy "
        f"button can be exercised. error_text={app.error_text()!r}"
    )

    assert app.driver.is_visible("error-message-copy-button"), (
        "CopyableInfoBar must render the copy button inline with the error: "
        f"{app.driver.diagnose('error-message-copy-button')}"
    )

    app.driver.click("error-message-copy-button")

    # Attack "clipboard contents aren't readable headlessly" (the caveat
    # test_settings_logs.py's log-copy-button assertion carries, per the project's
    # needs-a-human rule) rather than assume it: this is a real interactive
    # windows desktop session, so the OS clipboard is genuinely readable.
    clipboard_text = app.driver.get_clipboard_text()
    # The copy affordance's contract is "the FULL error string, verbatim" — which
    # is the *rendered* text, template included, not the raw backend detail (the
    # reason became a LocalizedText resolved through the windows i18n pipeline).
    # Asserting against the element's own text keeps the invariant that matters:
    # nothing shown is lost on the way to the clipboard.
    #
    # `endswith`, not `==`: `get_text` aggregates the element's accessible text,
    # and the InfoBar contributes its severity icon's name ("Error icon ") ahead
    # of the message. That prefix is chrome — it is not part of the error and has
    # no business on the clipboard — so the invariant is that the shown text ends
    # with exactly what was copied. That still fails if the copy truncates,
    # reorders, or drops any part of the message. (Equality could never hold here:
    # before the `error-message` shims were collapsed this read resolved
    # nondeterministically to an always-present empty shim OR to this same
    # icon-prefixed InfoBar, so this assertion had never once passed on windows.)
    shown = app.driver.get_text("error-message")
    assert clipboard_text and shown.endswith(clipboard_text), (
        "clicking the copy button must put the FULL rendered error on the OS "
        f"clipboard verbatim (got {clipboard_text!r}, shown {shown!r})"
    )
    assert reason in clipboard_text, (
        "…and the backend's own detail must be part of what was copied (got "
        f"{clipboard_text!r})"
    )
