"""tier_3: a half-written calendar event survives an app restart — the ``events``
rail of draft-persistence v2 (``docs/goal/behavior/reserved-folders.md`` § Drafts
Sync + ``docs/goal/ui/events.md`` § Persistence).

The sibling of ``test_feed_draft_persistence.py`` and
``test_conversations_draft_persistence.py``, one rail over — and the proof that
the wire's third and last ``DRAFT_RAILS`` constant is finally written by an app
rather than merely accepted by the plane. The at-rest shape is proven tier_1 in
``fauna-client-caldav`` (``drafts.rs``) and the nest ``__drafts`` plane tier_3 in
``tests/api/test_drafts_sync.py``; what THIS proves is the *app wiring* end to
end — a compose edit reaches ``__drafts`` via the debounced ``fauna.drafts.put``,
and the next launch restores it via ``fauna.drafts.get`` into the form the user
actually looks at.

**Marked per app as each leg lands.** tui was the lead app and the first wired
(``apps/fauna-tui/src/events/drafts.rs``); linux
(``apps/fauna-linux/src/views/events/drafts.rs``) and web
(``apps/fauna-web/src/lib/event-drafts.ts`` over the shared
``fauna_wasm::event_drafts`` face) followed, then android
(``EventDraftsHost.kt``, which also built the shared ``fauna-ffi::event_drafts``
typed native face android/apple/windows all consume), then macOS + iOS
(``EventsVM``'s ``FfiEventDraftsSync`` glue, shared FaunaKit — one leg for
both targets), then windows (``EventDraftsService``, over the same typed
``FfiEventDraftsSync`` face) — the last of the seven. This is an ``--app``
deselection, not a skip: no app-gated skip is introduced, so nothing reports
``s`` in a summary line that reads like success (e2e-conventions.md point 7).

**Why the New Event opener and not a day-cell gesture:** the two openers are
deliberately different (``events.md`` § Persistence). New Event *resumes* the
persisted draft — that is the returning-user path this test is about — while a
day-cell double-click means *start a new event here* and clears. Driving the
wrong one would assert the rail is empty and pass vacuously forever.

The restart uses the DEFAULT fresh-store relaunch, never
``preserve_state_across_relaunch()``: the draft has to come back *from the nest*,
and client-local state surviving the relaunch would make every assertion here
pass vacuously. That choice is also why the calendar surface is re-hydrated after
the restart (step 3b) — a fresh store holds no cached mail key.

**The compose is only reachable behind a calendar**, which is why this test takes
``calendar_backend`` and creates one. The rail itself needs neither: drafts seal
under the owner's ``BackupKey``, no MSEK involved. The calendar is scaffolding
for the *observation* — web renders its New Event opener only when one exists —
not a dependency of the thing being proven.

**RED-verified at introduction (2026-08-17), not merely green.** With the restore
half mutated out of the tui leg (``restore_on_launch`` dropping its dispatch of
``Outcome::DraftsLoaded``) this test fails at step 5 and *only* there — both
preconditions still pass, so the save-side assertion is independently exercised —
reporting ``expected '…restart 5193', got ''``. That is the shape a regression
here must produce: an EMPTY form, never a stale value.
"""

import time

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    # android joined 2026-09-25 — the module's own landing order (docstring
    # above) already names `EventDraftsHost.kt` as the THIRD leg wired, before
    # macOS/iOS/windows; it was simply never added to this list. The test body
    # drives only generic actions (`app.events.*`, `app.driver.type_text`),
    # no per-app branch.
    pytest.mark.android,
    # The debounce itself must land this draft — see the marker's entry in
    # pytest.ini. Keeps a `drafts_autosave_window_ms` run from silently
    # recording this as a product red.
    pytest.mark.drafts_production_window,
]

#: The rail this test drives — one of the three frozen constants on the wire
#: (``fauna_protocol::drafts::DRAFT_RAILS``). Named once so a typo cannot make
#: the side-channel read a rail the app never writes and time out looking honest.
RAIL = "events"


def _wait_compose_summary(app, expected: str, timeout: float = 10.0) -> str:
    """Poll the new-event form's summary until it equals ``expected`` (or time
    out). The snapshot refresh after a mutator is async, so read condition-based
    rather than once. Returns the last value read, for the failure message."""
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = app.events.compose_summary_text()
        if last == expected:
            return last
        time.sleep(0.1)
    return last


def _wait_drafts_changed(node_url, actor_id, signing_key, baseline_blob, timeout: float = 20.0):
    """Poll the nest ``__drafts`` plane (via a fresh side-channel device for the
    same actor) until the debounced ``fauna.drafts.put`` lands — i.e. the blob
    appears or changes from ``baseline_blob``. Returns the observed blob, or
    ``None`` on timeout.

    Deterministic save-confirmation rather than a settle-sleep: the budget is a
    generous ceiling that a green run never pays (convention 14), and it anchors
    on the observable that *is* the persistence contract — the nest's own blob —
    instead of guessing how long the 1.5 s debounce plus a WS round trip takes on
    a loaded box."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    deadline = time.time() + timeout
    while time.time() < deadline:
        with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
            blob = dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")
        if blob is not None and blob != baseline_blob:
            return blob
        time.sleep(0.5)
    return None


@pytest.mark.feature("drafts-survive")
def test_event_compose_draft_survives_app_restart(
    logged_in_app, nest_instance, test_user, calendar_backend
):
    app = logged_in_app
    node_url = nest_instance["url"]
    actor_id = test_user["actor_id_bytes"]
    signing_key = bytes(test_user["signing_key"])

    draft_summary = "unfinished event that must survive a restart 5193"

    # 0. Baseline the rail's blob so step 2 can tell "the save landed" from
    #    "something was already there".
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(node_url, actor_id=actor_id, signing_key=signing_key) as dev:
        baseline_blob = dev.call("fauna.drafts.get", {"path": RAIL}).get("blob")

    # 1. Type an event summary and DON'T submit it. Every app routes compose
    #    text through a single door that schedules the debounced
    #    `fauna.drafts.put` (tui `events::set_field`, linux the form's
    #    `connect_changed`, web the inputs' `oninput` → `noteDraftEdit`), so one
    #    typed field is all it takes to arm the save.
    app.events.navigate()
    # A calendar has to exist before the compose is reachable on every app: web
    # gates the New Event opener on `{#if calendars.length > 0}` -- there is
    # nothing to author into without one -- so on that column `_open_compose`
    # waits for a button that is never rendered. tui's opener is ungated, which
    # is the only reason this test got away without the precondition while it was
    # tui-only; it inherited the gap the moment a second app was marked.
    #
    # `calendar_backend` (the fixture above) is the half that makes a calendar
    # POSSIBLE: the encrypted CalDAV store needs the actor's MSEK, and without it
    # `create_calendar` silently no-ops and the sidebar just stays at zero rows.
    # This create is the half that makes one CERTAIN, the same call every test in
    # `test_events.py` opens with.
    app.events.create_calendar(f"drafts-{int(time.time())}")
    app.events.open_compose()
    app.driver.type_text("event-summary", draft_summary)
    assert _wait_compose_summary(app, draft_summary) == draft_summary, (
        "precondition: the draft must be typed into the form before the restart"
    )

    # 2. Wait for the save to actually reach the nest before tearing the app
    #    down — a restart mid-put would drop the draft with nothing to restore,
    #    and the assertion at step 5 would then be testing the wrong thing.
    saved_blob = _wait_drafts_changed(node_url, actor_id, signing_key, baseline_blob)
    assert saved_blob is not None, (
        "precondition: the debounced fauna.drafts.put must reach the nest __drafts "
        f"plane at path={RAIL!r} before the restart (save-side persistence did not land)"
    )

    # 3. Force-quit + relaunch, replaying the same identity — the app restart.
    #    In-memory state dies here; only what reached the nest can come back.
    app.driver.hard_reload()

    # 3b. Re-hydrate the calendar surface. The relaunch is the DEFAULT fresh-store
    #     one (see the module docstring), so the client holds no cached mail key
    #     and cannot reopen the sealed CalDAV store until it re-fetches the mail
    #     config — which is what re-arms the calendar list, and therefore web's
    #     New Event opener. The `calendar_backend` fixture documents this exact
    #     re-request as the way to do it, and the confirm is idempotent.
    #
    #     Note what this does NOT touch: the drafts rail itself needs no mail and
    #     no MSEK — it seals under the owner's `BackupKey` — so the restore under
    #     assertion here is independent of everything in this step. All this buys
    #     is a reachable compose to observe the restore IN.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # 4. Back to the Events page, and open the form the way a returning user
    #    does. The relaunched session's post-auth hook kicked the launch restore,
    #    which re-fetches the `__drafts` blob and lands it through the page's
    #    ordinary outcome channel; the New Event opener resumes rather than
    #    clearing, which is what makes the restored draft visible here.
    app.events.navigate()
    app.events.open_compose()

    # 5. The restored draft must be in the form. A *value* assertion, not a
    #    non-empty one: a stale draft, an empty form and a placeholder all
    #    satisfy "non-empty", and this queue has been bitten by that class before.
    restored = _wait_compose_summary(app, draft_summary)
    assert restored == draft_summary, (
        "the event compose draft did not survive the app restart "
        f"(expected {draft_summary!r}, got {restored!r}); error={app.error_text()!r}"
    )

    # 6. Discard the draft this test just proved is DURABLE — the cleanup is part
    #    of the test, not an afterthought. `__drafts` is per-actor nest state on
    #    the session-scoped `test_user`, so a draft left behind is restored into
    #    the NEXT test's event form on every app that wires the events rail. That
    #    is not hypothetical: the conversations twin shipped exactly this leak and
    #    it broke a sibling test with a composer holding the previous test's body.
    #
    #    Clearing the field is the right instrument, not a side-channel wipe: it
    #    is the user-facing discard, it goes through the UI like every other
    #    mutation here (e2e-conventions.md point 8), and there is no "delete a
    #    rail" RPC — the plane is `fauna.drafts.{get,put}` only.
    #
    #    Asserted, never best-effort: a cleanup that silently no-ops re-arms the
    #    exact cross-test leak this closes, and the next victim would fail far
    #    away with a baffling message instead of here with this one.
    app.driver.clear_and_type("event-summary", "")
    assert _wait_drafts_changed(node_url, actor_id, signing_key, saved_blob) is not None, (
        "cleanup: clearing the summary must persist the emptied draft to the nest "
        f"__drafts plane at path={RAIL!r}, or it leaks into the next test's event form. "
        "The assertion above already PASSED — the drafts feature works; this is the "
        "teardown failing, so look at the compose clear path on this app, not at "
        "draft persistence."
    )
