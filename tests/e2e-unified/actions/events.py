from __future__ import annotations

import re
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


#: How long a week/day grid may take to render a just-created event's positioned
#: block. Generous on purpose (convention 14): a green run polls once and pays
#: nothing, while the ceiling sits far above any non-pathological layout delay on
#: a heavily loaded build machine — the budget is not a timing assertion.
BLOCK_RENDER_BUDGET_S = 30

#: How long the calendar sidebar may take to list a calendar that was just
#: created (or that the nest provisioned at claim time). Same rationale.
CALENDAR_LIST_BUDGET_S = 20

#: How long a created event may take to appear in the agenda — one seal + PUT +
#: re-query round trip. The old ceiling here was 10 s, which is inside the noise
#: band of a heavily loaded build machine.
EVENT_CREATE_BUDGET_S = 30

#: How long a month-nav click may take to update `calendar-date-label`. Same
#: rationale as the budgets above — generous on purpose.
MONTH_NAV_BUDGET_S = 10


class EventsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver
        # Calendars this action layer created and that still hold no event, so
        # `select_calendar` knows an *empty* agenda is the sound observable that
        # its selection landed (see `_wait_for_agenda_narrowed`). Cleared
        # wholesale by `create_event` — after any create, no calendar is
        # guaranteed empty, and losing the barrier only costs coverage, never
        # correctness.
        self._empty_calendars: set[str] = set()
        # Form state captured at the instant of the last `create-event` click,
        # replayed into `_wait_for_event`'s failure (see `_submit_compose`).
        self._last_submit: dict | None = None

    def navigate(self) -> None:
        """Navigate to the events page via sidebar tab."""
        self.driver.navigate_to("events")
        # Wait for the page to render — view toggle buttons confirm the header loaded
        self.driver.wait_for("calendar-view-agenda", timeout=10)
        # Establish a consistent starting view. The app/driver is reused across
        # tests (session-scoped driver + reset() between tests), so a prior test
        # may have left a different calendar view active. Reset to Agenda — the
        # canonical default, which lists ALL events regardless of date; month/
        # week/day are date-filtered and wouldn't show events outside the visible
        # range, making event-creation assertions order-dependent.
        self.driver.click("calendar-view-agenda")

    def create_calendar(self, name: str) -> None:
        """Create a new calendar and wait until **that** calendar is listed.

        The barrier matches the row by name, not `count("calendar-item") > 0`:
        the actor is session-scoped and the sidebar already lists the
        nest-provisioned "Personal" calendar (plus every calendar earlier tests
        created), so a bare count is satisfied *before this create lands* and the
        caller's `select_calendar` then races a row that isn't there yet.
        """
        self.driver.click("new-calendar-btn")
        self.driver.wait_for("calendar-name")
        self.driver.clear_and_type("calendar-name", name)
        self.driver.click("create-calendar")
        self._wait_for_calendar(name)
        self._empty_calendars.add(name)

    def _wait_for_calendar(self, name: str,
                           timeout: float = CALENDAR_LIST_BUDGET_S) -> int:
        """Return the index of the `calendar-item` row whose text carries `name`,
        waiting for it to render. Raises with a self-diagnosing message (rule 6).
        """
        deadline = time.monotonic() + timeout
        listed: list[str] = []
        while time.monotonic() < deadline:
            try:
                listed = self.calendar_names()
            except LookupError:
                # A row vanished between `get_texts`' count and its per-index
                # read: the sidebar repainted mid-read (the create form closing,
                # a reload landing). Not an answer — poll again.
                time.sleep(0.3)
                continue
            for i, text in enumerate(listed):
                if name in text:
                    return i
            time.sleep(0.3)
        raise TimeoutError(
            f"calendar {name!r} never appeared in the sidebar: listed={listed!r}, "
            f"{self.driver.diagnose('calendar-item')}"
        )

    def calendar_names(self) -> list[str]:
        """Text of every listed `calendar-item` (carries the calendar name on
        every app — apple registers `value: { cal.name }`, linux/web/windows
        render the name as the row's label)."""
        return self.driver.get_texts("calendar-item")

    def select_calendar(self, name: str) -> None:
        """Click the calendar named `name`, waiting for it to be listed first.

        Waits rather than falling back to "click the first calendar": the old
        fallback turned a not-yet-loaded sidebar into a silent click on the wrong
        calendar (and an empty sidebar into a silent no-op), so the downstream
        failure surfaced pages later as a missing event. Callers pass a name they
        created or one the nest provisions, so a name that never lists is a real
        failure worth reporting where it happens (e2e rule 6 / point 11).

        Selecting is **asynchronous** on every app (the VM re-queries the
        calendar's events after the click), so this also waits for the agenda to
        actually narrow — see `_wait_for_agenda_narrowed` for why that barrier is
        only sound for a calendar we created.
        """
        self.driver.click("calendar-item", index=self._wait_for_calendar(name))
        if name in self._empty_calendars:
            self._wait_for_agenda_narrowed(name)

    def _wait_for_agenda_narrowed(self, name: str,
                                  timeout: float = CALENDAR_LIST_BUDGET_S) -> None:
        """Wait until the agenda has re-rendered for a just-created calendar.

        Zero event cards is a *sound* identity here for exactly the reason zero
        snapshots is in `test_backups.py`: the calendar was created moments ago
        and nothing has put an event in it, so an empty agenda cannot mean
        anything except "the selection landed". Without this barrier the click
        returns while the VM is still awaiting its re-query, and the caller reads
        its baseline off the **previous** calendar's list — the `initial_count=11
        … count=1` class of failure, where a count barrier waits for a list to
        grow that is about to shrink instead.

        There is no cross-app "selected calendar" marker to wait on (`ui.yaml`
        events page has no such element), which is why this leans on emptiness.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("event-card") == 0:
                return
            time.sleep(0.3)
        # Rule 6: this timeout must say WHICH of the two causes it is, because
        # they need opposite fixes. `event-card` is a bare count; the summaries
        # carry identity. Real rows still on screen (summaries name events of
        # some other calendar) = the selection genuinely did not narrow, a
        # product bug. Cards counted but no summaries behind them, or summaries
        # that read blank = the client's automation registry is still serving
        # rows the UI already removed, a harness bug. Without both numbers the
        # failure is a coin flip between them.
        cards = self.driver.count("event-card")
        summaries = self.event_summaries()
        raise TimeoutError(
            f"the agenda never narrowed to the freshly-created calendar {name!r} "
            f"within {timeout}s: event-card count={cards}, "
            f"event-card-summary count={len(summaries)}, summaries={summaries!r}; "
            f"{self.driver.diagnose('event-card')}"
        )

    def create_event(self, summary: str, start: str, end: str) -> None:
        """Create a new event in the currently selected calendar."""
        self._open_compose()
        self.driver.clear_and_type("event-summary", summary)
        self.driver.clear_and_type("event-dtstart", start)
        self.driver.clear_and_type("event-dtend", end)
        self._submit_compose(summary, start, end)
        self._wait_for_event(summary)
        self._wait_for_create_settled()

    def _submit_compose(self, summary: str, start: str, end: str) -> None:
        """Click `create-event`, recording the form's state at the instant of
        the click so a lost create can name its own cause.

        `_wait_for_event`'s timeout can only report the state 30 s LATER, by
        which time the compose is gone and every candidate looks identical —
        which is why `test_event_create_and_delete[macos]` survived four
        sessions of theories. The three readings taken here split the field at
        the one moment that matters:

        * summary reads back ≠ what we typed → `clear_and_type` wrote into a
          form the client had already torn down (or a stale registry slot),
          so the click submitted a dead form's state.
        * submit disabled at click time → the client considered the form
          incomplete or already-submitting. The apple in-process agent used to
          fire `activate` WITHOUT consulting the registered `isEnabled`, so a
          disabled submit was driven anyway — unlike web, where Playwright
          auto-waits for enabled — and reading the flag here was the ONLY way to
          see that divergence. `InProcessAutomationServer` now gates all five
          actuation routes on `isEnabled`: by default it refuses with a named 409
          (since 2026-08-05), and under `--permissive-actuation` it drives anyway
          but NSLogs a `DISABLED-ACTUATION` marker (`FaunaE2E.strictEnabled`;
          `tests/test_apple_disabled_actuation.py`). This reading stays valuable
          — it is recorded at the instant of the click, which the marker's
          timestamp cannot tie to a specific test.
        * both healthy → the create was genuinely submitted and lost
          downstream (VM guard, API, or nest), not mis-driven by the harness.
        """
        self._last_submit = {
            "typed_summary": summary,
            "summary_read_back": self._safe_text("event-summary"),
            "dtstart_read_back": self._safe_text("event-dtstart"),
            "dtend_read_back": self._safe_text("event-dtend"),
            "submit_enabled": self.driver.is_enabled("create-event"),
            "submit_visible": self.driver.is_visible("create-event"),
            "wanted_start": start,
            "wanted_end": end,
        }
        self.driver.click("create-event")

    def _safe_text(self, element_id: str) -> str:
        """`get_text` that reports rather than raises — this runs on the
        diagnostic path, where a missing element IS the finding."""
        try:
            return self.driver.get_text(element_id)
        except Exception as exc:  # noqa: BLE001 - the exception is the datum
            return f"<{type(exc).__name__}: {exc}>"

    def _wait_for_create_settled(self,
                                 timeout: float = EVENT_CREATE_BUDGET_S) -> None:
        """Wait until the client is no longer *mid-create*.

        `_wait_for_event` alone is NOT a sound postcondition for `create_event`,
        because the agenda has **more than one writer**. On apple,
        `EventsVM.refreshIfChanged` — driven by the 10 s poll *and* by the
        `fauna.calendar.changed` push, which both apps wire
        (`EventSplitView.swift`, `CalendarListView.swift`) — commits a freshly
        fetched list under a generation it merely *observes*. So it can render
        the just-created event while `createEvent` is still awaiting its **own**
        re-query, i.e. while the compose is still open and `creatingEvent` is
        still true. (`createEvent`'s own commit provably cannot do this: it
        assigns the list and clears `showNewEvent` in one actor hop, so the
        driver can never observe the list between them.)

        `create_event` would then return with a create still in flight, and the
        next `_open_compose` reads a compose visibility that is *changing under
        it* — skip the toggle and type into a form the client is about to tear
        down, or click the toggle and close a live one. Both end in a silent
        no-op create with no `error-message`: e2e point 11's exact shape, and
        the recorded failure of `test_event_create_and_delete[macos]`
        (`rendered=['Meeting A']`, compose shut, nothing created, no error).

        The barrier asks the client's own question — *are you still
        submitting?* — instead of assuming a compose lifecycle, because the
        lifecycle genuinely differs: apple/web/tui take the form down, while
        android dismisses its sheet and linux closes its modal dialog
        (`event_form.rs`) the instant the button is pressed.

        What IS uniform is that a client mid-create disables its submit
        control: apple's `isSubmitDisabled` and web's `disabled={creatingEvent
        || ...}` are the same predicate. So: settled ⇔ the compose is gone, or
        the submit button is back to enabled. Latency-independent state, never
        wall-clock timing (convention 14).
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.driver.is_visible("event-summary"):
                return
            if self.driver.is_enabled("create-event"):
                return
            time.sleep(0.3)
        # Rule 6: name which half held it. A compose that is still up with a
        # still-disabled submit means the client never finished the create it
        # acknowledged (the event rendered, so the write landed) — a stuck
        # `creatingEvent`. Anything else means the ids moved.
        raise TimeoutError(
            f"the client was still mid-create {timeout}s after the event "
            f"rendered — compose never settled: "
            f"{self.driver.diagnose('event-summary')}, "
            f"{self.driver.diagnose('create-event', attrs=('enabled',))}, "
            f"{self.driver.diagnose('error-message')}"
        )

    def _open_compose(self) -> None:
        """Make the new-event form open, whatever state the page arrived in.

        `new-event-btn` is a **toggle** on at least macOS (`MacEventListView`
        renders it as New event / Cancel over one `showNewEvent` flag), so a blind
        click *closes* an already-open compose and the caller then types into a
        form that is on its way out — a silent no-op create with no
        `error-message`, which is e2e point 11's exact shape. A compose can be
        left open by an earlier test that only asserted the form appeared, and by
        `EventsVM.createEvent`'s own error branch, so the state is not this
        action's to assume.
        """
        if not self.driver.is_visible("event-summary"):
            self.driver.click("new-event-btn")
        self.driver.wait_for("event-summary")

    def _wait_for_event(self, summary: str,
                        timeout: float = EVENT_CREATE_BUDGET_S) -> None:
        """Wait until the agenda lists an event card carrying `summary`.

        Identity, not `count("event-card") > initial_count` (which this replaced,
        and which was the single largest source of red in this module). A count
        delta is unobservable in two separate ways here: the selection re-query
        can *shrink* the list between the baseline read and the create, and on
        macOS the agenda is a virtualizing SwiftUI `List` (`MacEventListView`),
        so `count` saturates at the handful of rows the pane fits and stops
        tracking the data at all. Matching the summary answers the question the
        caller actually has — *did my event land?* — and is immune to both.

        Callers keep the agenda short (`select_calendar` on a freshly created
        calendar) so the row is guaranteed rendered; a long list plus a
        virtualizing client can hide any row, identity-matched or not.
        """
        self._empty_calendars.clear()
        deadline = time.monotonic() + timeout
        rendered: list[str] = []
        while time.monotonic() < deadline:
            try:
                rendered = self.event_summaries()
            except LookupError:
                # A row vanished between the count and its read: the page
                # repainted mid-read (the compose form closing, the agenda
                # re-query landing). That is a read that did not match yet, not
                # an answer — `event_summaries` leaves the race to its pollers.
                time.sleep(0.3)
                continue
            if any(summary in text for text in rendered):
                return
            time.sleep(0.3)
        # Rule 6. `rendered` + `error-message` together already say "no event and
        # no error", i.e. the create was refused upstream of the API call — but
        # not WHERE. The compose form's own state splits the two candidates:
        # `event-summary` reading back the summary we typed means the form is
        # live and the `create-event` click is what failed, while a visible
        # `event-summary` whose text is EMPTY (or stale) means `clear_and_type`
        # wrote into a form the client had already torn down, so the click then
        # submitted that dead form's blank state. `create-event`'s enabled flag
        # separates a third: a submit button correctly disabled on a blank form.
        raise TimeoutError(
            f"event {summary!r} never appeared in the agenda within {timeout}s: "
            f"rendered={rendered!r}, {self.driver.diagnose('event-card')}, "
            f"{self.driver.diagnose('error-message')}, "
            f"compose form: {self.driver.diagnose('event-summary')}, "
            f"{self.driver.diagnose('create-event', attrs=('enabled',))}; "
            # The state above is 30 s stale by construction. This is the state
            # at the click itself, which is what actually separates the causes.
            f"AT CLICK TIME: {self._last_submit!r}; "
            # Ordered least- to most-mutating: the registry dump is a pure read,
            # the re-render probe changes only the view mode, and the sweep
            # changes the selection (which re-queries, so it must come last).
            f"REGISTRY: {self._registry_dump()}; "
            f"RE-RENDER: {self._probe_pure_rerender(summary)}; "
            f"OTHER CALENDARS: {self._sweep_other_calendars(summary)}"
        )

    #: Registry ids the agenda-row dump reports on — the row component and the
    #: two leaves a lost create is asserted through.
    _AGENDA_ROW_IDS = ("event-card", "event-card-summary")

    def _registry_dump(self) -> str:
        """Failure-path only: the client's own view of its agenda-row slots.

        `count()` answers "how many rows can the driver see", which conflates
        *the row was never built* with *the row was built and the client is
        reporting it absent*. Apple's in-process registry keeps every slot it
        ever placed, tagged with why each one currently reads visible or hidden
        (live sentinel geometry vs. the two off-screen votes), so its `/tree`
        dump separates exactly those two — see `AutomationRegistry.debugDump`.

        Empty on every app whose bridge has no `/tree` (`tree()` returns ""),
        which is the honest answer there rather than a fabricated one.
        """
        try:
            dump = getattr(self.driver, "tree", lambda: "")()
        except Exception as exc:  # noqa: BLE001 - never mask the real failure
            return f"<registry dump failed: {type(exc).__name__}: {exc}>"
        if not dump:
            return "<no /tree on this bridge>"
        kept: list[str] = []
        keeping = False
        for line in dump.splitlines():
            if line.startswith(" "):
                if keeping:
                    kept.append(line.strip())
                continue
            keeping = line.split(" ")[0] in self._AGENDA_ROW_IDS
            if keeping:
                kept.append(line)
        return " | ".join(kept) if kept else "<no agenda-row slots registered>"

    def _probe_pure_rerender(self, summary: str,
                             timeout: float = 5) -> str:
        """Failure-path only: does a pure RE-RENDER surface the missing event?

        Splits the last two candidates once the click is known good and the
        write is known to have landed (see `_submit_compose` /
        `_sweep_other_calendars`) — and they need opposite fixes:

        * **stale DATA** — the client's own post-create re-query committed a
          list that did not yet contain the event, and nothing corrected it
          afterwards. The fix is in the view model's query/publish ordering.
        * **stale RENDER** — the model already holds the event; the agenda's
          view never took the new row (macOS renders it in a *virtualizing*
          SwiftUI `List`) or never registered it. The fix is in the view /
          registration, and touching the query would be pure noise.

        The discriminator is a view-mode round trip: switching away and back
        rebuilds the agenda **from the model, with no re-query at all**. That is
        what `_sweep_other_calendars` cannot do — selecting a calendar re-queries
        *and* rebuilds, so its answer is compatible with either cause, which is
        why the recorded failure of `test_event_create_and_delete[macos]` stayed
        ambiguous after it was added.
        """
        try:
            before = self.event_summaries()
            self.switch_view("month")
            self.switch_view("agenda")
            deadline = time.monotonic() + timeout
            after: list[str] = []
            while time.monotonic() < deadline:
                after = self.event_summaries()
                if any(summary in text for text in after):
                    return (f"STALE RENDER — {summary!r} appeared after a "
                            f"month→agenda round trip that ran no query, so the "
                            f"model already held it "
                            f"(before={before!r}, after={after!r})")
                time.sleep(0.3)
            return (f"STALE DATA — {summary!r} still absent after a month→agenda "
                    f"round trip (before={before!r}, after={after!r}), so the "
                    f"model itself lacks the event")
        except Exception as exc:  # noqa: BLE001 - never mask the real failure
            return f"<re-render probe failed: {type(exc).__name__}: {exc}>"

    def _sweep_other_calendars(self, summary: str) -> str:
        """Failure-path only: look for `summary` under every OTHER calendar.

        Splits the two candidates left once the click is known good (form live,
        fields correct, submit enabled — see `_submit_compose`): the event was
        written to the WRONG calendar (it turns up here, and the culprit is the
        calendar id the create resolved), versus it was never persisted at all
        (it turns up nowhere, and `api.createEvent` returned success for a write
        the nest did not keep — the far more serious reading, since the user
        sees the compose close as if it worked).

        Deliberately mutates the page — the test has already failed, so the only
        cost is the selection, and the answer is worth far more than a pristine
        end state. Never raises: a diagnostic that masks the real failure is
        worse than no diagnostic.
        """
        try:
            found: list[str] = []
            for i, name in enumerate(self.calendar_names()):
                self.driver.click("calendar-item", index=i)
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if any(summary in t for t in self.event_summaries()):
                        found.append(name)
                        break
                    time.sleep(0.3)
            return (f"{summary!r} FOUND under {found!r} — created into the wrong "
                    f"calendar" if found else
                    f"{summary!r} is under NO calendar — the create was "
                    f"acknowledged but never persisted")
        except Exception as exc:  # noqa: BLE001 - never mask the real failure
            return f"<sweep failed: {type(exc).__name__}: {exc}>"

    def open_compose(self) -> None:
        """Public door onto :meth:`_open_compose` — make the new-event form open,
        whatever state the page arrived in.

        Exposed for the drafts-rail restart test, which must open the form the
        way a returning user does (the New Event opener, which RESUMES a
        persisted draft) rather than through a day-cell gesture (which starts a
        fresh event — ``docs/goal/ui/events.md`` § Persistence)."""
        self._open_compose()

    def compose_summary_text(self) -> str:
        """Return the new-event form's current ``event-summary`` value — used to
        assert an event draft survived an app restart
        (``docs/goal/behavior/reserved-folders.md`` § Drafts Sync,
        ``docs/goal/ui/events.md`` § Persistence). The events twin of
        ``FeedActions.compose_body_text``.

        Wired apps only — each adds its arm as its events-rail leg lands, and
        the rest raise loudly rather than silently passing, since a vacuous read
        is exactly how a draft assertion would pass on an app that persists
        nothing.

        * **tui** paints ``Element::input("event-summary", st.new_event_summary,
          …)`` (``apps/fauna-tui/src/events/mod.rs``) and ``Element::input``'s
          ``value`` IS the element's ``text``, so the agent's element read
          returns the draft summary verbatim, and ``""`` when empty (no
          placeholder fallback).
        * **linux** paints it as the new-event dialog's ``gtk::Entry``
          (``apps/fauna-linux/src/views/events/event_form.rs``), and the
          automation's ``text_of`` reads a ``gtk::Entry`` through
          ``Entry::text`` — the entry's *value*, again never its placeholder,
          so an empty form reads ``""`` here too.
        * **web** paints it as a plain ``<input type="text">``
          (``routes/events/+page.svelte``); the web bridge's generic
          ``/element/text`` special-cases ``<input>``/``<textarea>``/``<select>``
          to read ``.value`` (``web-bridge/server.py``), so the same plain read
          returns the draft summary verbatim and ``""`` when empty — the identical
          arm ``FeedActions.compose_body_text`` already relies on for the feed
          composer's ``<textarea>``.
        * **macos/ios** back it with an ``automationField``-bound ``TextField``
          (``MacEventFormView.swift``/``EventFormView.swift``) — the same
          registry-direct read ``FeedActions.compose_body_text``'s apple arm
          uses: the in-process agent's element read returns the bound
          ``summary`` verbatim, ``""`` when empty (no placeholder-scrape
          fallback).
        * **windows** paints it as a plain WinUI ``TextBox``
          (``EventsPage.xaml``'s ``EventSummaryBox``) — the FlaUI bridge's
          ``GetText`` reads it via ``ValuePattern.Value``, the field's live
          text, so this returns the draft summary verbatim, and ``""`` when
          empty — an Edit control's empty ``ValuePattern`` is authoritative,
          exactly as ``FeedActions.compose_body_text``'s windows arm records."""
        if (self.driver.is_tui() or self.driver.is_linux() or self.driver.is_web()
                or self.driver.is_macos() or self.driver.is_ios()
                or self.driver.is_windows() or self.driver.is_android()):
            # android tags the form's summary `OutlinedTextField`
            # `Ids.EVENT_SUMMARY` (`EventsScreen.kt`), a Compose text field whose
            # element text is the field's own value — the same plain read
            # `FeedActions.compose_body_text`'s android arm makes.
            return self.driver.get_text("event-summary")
        raise NotImplementedError(
            "events compose_summary_text has no arm for this app until its "
            "events-rail draft leg lands (events.md § Implementation status today)"
        )

    def event_count(self) -> int:
        return self.driver.count("event-card")

    def event_summaries(self) -> list[str]:
        """Text of every rendered `event-card-summary`.

        A count()-then-index read races a live add/remove transition — a row
        can vanish between `count()` and its own `get_text()`, or an
        in-flight rebuild can transiently collapse `count()` itself to fewer
        rows than really exist a beat later (found live:
        `test_event_create_and_delete[ios]` under sibling contention — a
        delete-triggered re-render briefly read as zero rows, not just the
        one row actually being removed). This function stays a single honest
        read and lets a `LookupError` from that race propagate rather than
        guessing at a truncated/empty snapshot a caller could mistake for
        real state — see `_wait_for_event_gone` for the caller that actually
        needs to tolerate the race, and why a debounced poll is the correct
        place to absorb it, not this accessor.
        """
        count = self.driver.count("event-card-summary")
        return [self.driver.get_text("event-card-summary", index=i) for i in range(count)]

    def wait_for_listed(self, present, absent=(),
                        timeout: float = EVENT_CREATE_BUDGET_S) -> list[str]:
        """Poll until every summary in ``present`` is listed and none in ``absent``
        is, returning the last read of :meth:`event_summaries`.

        Identity on both sides, never a count: the session actor's calendars
        accumulate other tests' events, so only named summaries say which calendar
        the page is showing. A read that races a re-render (``LookupError``, see
        :meth:`event_summaries`) is just a poll that did not match yet. On timeout
        returns the last read so the caller's assertion shows it."""
        present, absent = set(present), set(absent)
        deadline = time.monotonic() + timeout
        shown: list[str] = []
        while True:
            try:
                shown = self.event_summaries()
            except LookupError:
                shown = []
            if present <= set(shown) and not (absent & set(shown)):
                return shown
            if time.monotonic() >= deadline:
                return shown
            time.sleep(0.3)

    def delete_event(self, summary: str) -> None:
        """Delete an event by clicking its delete button.

        Opens the event popover and clicks the delete button.
        """
        self._click_event_card_by_summary(summary)
        self.driver.wait_for("event-detail-summary")
        self.driver.click("event-delete-btn")
        self._wait_for_event_gone(summary)

    def _wait_for_event_gone(self, summary: str,
                             timeout: float = EVENT_CREATE_BUDGET_S) -> None:
        """Wait until no agenda card carries `summary` any more.

        Replaces a bare `time.sleep(3)`. That sleep was a wall-clock timing
        assertion in convention 14's DEFUNCT class: on a loaded machine three
        seconds is not reliably enough for delete + re-query, so the caller's
        `assert summary not in event_summaries()` was reading a list that
        simply had not refreshed yet — and on a fast machine it burned three
        seconds of every green run for nothing.

        Disappearance is a positive, latency-independent state change (the row
        is there, then it is not), so it polls to a generous deadline instead:
        a green run pays one poll, and the ceiling sits far above any
        non-pathological round trip rather than encoding an expected duration.

        A single "gone" read is not trusted alone: an in-flight delete can
        briefly rebuild the whole agenda (a full re-query, or a torn-down
        row set) and read as zero rows for one poll tick even though other
        events are still there a beat later — found live:
        `test_event_create_and_delete[ios]` under sibling contention, where
        that flash made an unrelated event read as gone too. Two consecutive
        polls agreeing `summary` is absent is the debounce; a poll that
        raises `LookupError` (the list mid-shrink, see `event_summaries`) or
        still lists `summary` both reset the streak, same as one that
        hasn't settled yet.
        """
        deadline = time.monotonic() + timeout
        rendered: list[str] = []
        consecutive_gone = 0
        while time.monotonic() < deadline:
            try:
                rendered = self.event_summaries()
            except LookupError:
                consecutive_gone = 0
                time.sleep(0.3)
                continue
            if any(summary in text for text in rendered):
                consecutive_gone = 0
            else:
                consecutive_gone += 1
                if consecutive_gone >= 2:
                    return
            time.sleep(0.3)
        raise TimeoutError(
            f"event {summary!r} was still listed {timeout}s after its delete "
            f"was confirmed: rendered={rendered!r}, "
            f"{self.driver.diagnose('event-card')}, "
            f"{self.driver.diagnose('error-message')}"
        )

    def _click_event_card_by_summary(self, summary: str) -> None:
        """Click on an event card containing the given summary text."""
        count = self.driver.count("event-card-summary")
        for i in range(count):
            text = self.driver.get_text("event-card-summary", index=i)
            if summary in text:
                self.driver.click("event-card", index=i)
                return
        # Fallback: click first card
        if self.driver.count("event-card") > 0:
            self.driver.click("event-card")

    def open_event_detail(self, index: int = 0) -> None:
        """Click on an event to open detail page."""
        self.driver.click("event-card", index=index)
        self.driver.wait_for("event-detail-summary")

    def rsvp_on_detail(self, status: str) -> None:
        """RSVP on the event detail page."""
        self.driver.click(f"event-detail-rsvp-{status}")

    def attendee_count(self) -> int:
        return self.driver.count("attendee-item")

    def invite_attendee(self, identifier: str) -> None:
        """Type an attendee identifier into `attendee-invite-field` and Invite.

        On the encrypted CalDAV path the identifier is an **email address**
        (`mailto:` CAL-ADDRESS — caldav-server.md § Scheduling & invitations);
        on the legacy actor-id path (the 5 clients not yet flipped) it is a hex
        actor id. The field + button are the same ids; only the accepted value
        differs until Step 4c unifies them.
        """
        self.driver.clear_and_type("attendee-invite-field", identifier)
        self.driver.click("attendee-invite-button")

    def rsvp_event(self, summary: str, status: str = "accepted") -> None:
        """Open an event by summary and RSVP with the given status."""
        self._click_event_card_by_summary(summary)
        self.driver.wait_for("event-detail-summary")

        status_map = {
            "accepted": "going",
            "going": "going",
            "interested": "interested",
            "declined": "decline",
            "decline": "decline",
        }
        btn_status = status_map.get(status, "going")
        self.driver.click(f"event-detail-rsvp-{btn_status}")
        self._wait_for_attendee_item()

    def _wait_for_attendee_item(self, timeout: float = EVENT_CREATE_BUDGET_S) -> None:
        """Wait until at least one `attendee-item` is registered.

        Replaces a bare `time.sleep(3)` after an RSVP click. That sleep was a
        wall-clock timing assertion in convention 14's DEFUNCT class: the RSVP
        write is an async seal + PUT + re-query round trip — the same class of
        latency `EVENT_CREATE_BUDGET_S` already budgets for event creation, so
        it reuses that ceiling rather than inventing a shorter one. A non-empty
        roster is the latency-independent observable (an event starts with no
        attendee rows); the caller's own `event_rsvp_count` still re-polls for
        whatever exact count it needs.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("attendee-item") > 0:
                return
            time.sleep(0.3)
        raise TimeoutError(
            f"no attendee-item registered {timeout}s after an RSVP click: "
            f"{self.driver.diagnose('attendee-item')}"
        )

    def invite_to_event(self, summary: str, identifier: str) -> None:
        """Open an event by summary and invite an attendee.

        `identifier` is an email address on the encrypted path (linux today),
        a hex actor id on the legacy path — see `invite_attendee`.
        """
        self._click_event_card_by_summary(summary)
        self.driver.wait_for("event-detail-summary")
        self.invite_attendee(identifier)

    def event_rsvp_count(self, summary: str) -> int:
        """Return the number of attendees for an event.

        The detail surface renders attendees from a snapshot taken when it
        opens, and the RSVP's attendee fetch is async, so re-open the detail
        each poll iteration until the attendee appears — waiting on a single
        (possibly pre-fetch) snapshot is racy. (On web, re-selecting the event
        re-fetches attendees.)

        The linux note this docstring used to carry — "the detail is an autohide
        popover that dismisses on the first re-click" — was stale: linux's detail
        has been the persistent, observer-driven panel in `event_detail.rs` since
        that module replaced `show_quick_preview`, precisely because a snapshot
        popover never showed a freshly-RSVP'd attendee.
        """
        if self.driver.count("attendee-item") > 0:
            return self.driver.count("attendee-item")
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            self._click_event_card_by_summary(summary)
            try:
                self.driver.wait_for("event-detail-summary", timeout=3)
            except TimeoutError:
                time.sleep(0.5)
                continue
            n = self.driver.count("attendee-item")
            if n > 0:
                return n
            time.sleep(0.5)
        return self.driver.count("attendee-item")

    def switch_view(self, mode: str) -> None:
        """Switch calendar view: agenda, month, week, day."""
        self.driver.click(f"calendar-view-{mode}")
        # The stack transition is ~100ms, but AT-SPI needs time to update
        # SHOWING states after the stack child changes. Use wait_for on the
        # marker element specific to each view mode.
        marker_ids = {
            "week": "calendar-week-grid",
            "day": "calendar-day-timeline",
            "month": "events-month-grid",
        }
        marker = marker_ids.get(mode)
        if marker:
            try:
                self.driver.wait_for(marker, timeout=5)
            except TimeoutError:
                pass  # Fall through; the test assertion will report the real failure
        else:
            # agenda has no dedicated container marker in ui.yaml (unlike
            # month/week/day's own grid components) — its absence is the
            # observable: poll until every OTHER mode's grid marker has
            # cleared, instead of a blind sleep (convention 14).
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:  # deadline-ok: documented below — the test's own assertion reports the real failure
                if not any(self.driver.is_visible(m) for m in marker_ids.values()):
                    break
                time.sleep(0.1)
            # Fall through either way; the test assertion reports the real failure

    def event_block_texts(self) -> list[str]:
        """Text of every positioned `calendar-event-block` in the week/day grid.

        Each app's block reads back the event **summary** — web renders it in
        a `block-summary` span, windows sets it as the button's UIA `Name`, tui
        formats `"HH:MM–HH:MM Summary"`, android puts it in the card, and apple
        registers `text: { event.summary }` alongside the event-id `value`. So a
        substring match on this list is the portable identity check.
        """
        return self.driver.get_texts("calendar-event-block")

    def allday_band_text(self) -> str:
        """The week/day grid's ``calendar-allday-band`` — the band above the timed
        grid that carries the visible range's all-day events — or ``""`` when the
        range has none and the band is not painted (tui paints it only then)."""
        if not self.driver.is_visible("calendar-allday-band"):
            return ""
        return self.driver.get_text("calendar-allday-band")

    def event_block_layout(self, summary: str) -> dict | None:
        """Where the timed block for ``summary`` sits in the week/day grid:
        ``{"start": "HH:MM", "end": "HH:MM", "column": i, "columns": n}`` — its
        drawn start and end, and which of how many side-by-side columns its clash
        group packs it into (``column`` 0-based; a block alone is ``0`` of ``1``).
        Raises ``LookupError`` naming the rendered blocks when none is for
        ``summary`` (convention 6).

        tui draws this layout as the block's own text (``events/grids.rs``:
        ``"HH:MM–HH:MM Summary"``, suffixed ``" [i/n]"`` when ``n > 1``). web,
        linux, macOS, iOS and windows position blocks in pixels, so they are MEASURED: see
        :meth:`_measured_block_layout`. ``None`` on an app whose driver cannot read
        a block's geometry yet, so a witness of the layout is marked only where
        this answers."""
        if (self.driver.is_web() or self.driver.is_linux()
                or self.driver.is_macos() or self.driver.is_ios()
                or self.driver.is_windows()):
            return self._measured_block_layout(summary)
        if not self.driver.is_tui():
            return None
        pattern = re.compile(
            r"^(?:\S+ \d+ )?(\d{2}:\d{2})–(\d{2}:\d{2}) (.*?)(?: \[(\d+)/(\d+)\])?$"
        )
        texts: list[str] = []
        for _ in range(5):  # a repaint mid-read (see wait_for_event_block) is not an answer
            try:
                texts = self.event_block_texts()
                break
            except LookupError:
                time.sleep(0.3)
        for text in texts:
            m = pattern.match(text)
            if m and m.group(3) == summary:
                start, end, _, col, cols = m.groups()
                return {
                    "start": start,
                    "end": end,
                    "column": int(col) - 1 if col else 0,
                    "columns": int(cols) if cols else 1,
                }
        raise LookupError(f"no calendar-event-block for {summary!r}; rendered: {texts!r}")

    #: The drawn-position read's rounding, in minutes: a block's edge measured in
    #: pixels lands within a pixel or two of its minute (borders, the block's
    #: inset), and a layout promise is about the slot a block fills, not the pixel.
    _MEASURE_ROUND_MIN = 5

    def _timeline_frames(
        self, summary: str | None = None
    ) -> tuple[list[tuple[str, tuple]], tuple, tuple]:
        """``(blocks, slot_00_00, slot_01_00)`` — every ``calendar-event-block``'s
        ``(text, (x, y, w, h))`` and the frames of the midnight and 01:00
        ``events-time-slot-{HH-MM}`` markers, all in one coordinate space.

        The slot markers are the grid's own ruler on every app that draws them:
        each tiles its day column's full width at its own minute, so the
        midnight marker is the column's origin and width and the 01:00 one gives
        pixels per hour — no per-app pixel constant is copied into this layer.
        web reads the rects from the DOM; linux, macOS and iOS from
        ``/registry``'s per-element ``frame`` (window space on each); windows and
        android as :meth:`_clipped_timeline_frames` says, which needs ``summary``
        to know which block to bring into view. Day view only: there, each marker
        id names exactly one slot."""
        if self.driver.is_web():
            data = self.driver.eval_js(
                "(() => { const r = (el) => { const b = el.getBoundingClientRect();"
                " return [b.left, b.top, b.width, b.height]; };"
                " const q = (id) => document.querySelector(`[data-testid=\"${id}\"]`);"
                " const s0 = q('events-time-slot-00-00'), s1 = q('events-time-slot-01-00');"
                " if (!s0 || !s1) return null;"
                " return { s0: r(s0), s1: r(s1), blocks: [...document.querySelectorAll("
                "'[data-testid=\"calendar-event-block\"]')].map((b) => [b.textContent, r(b)]) };"
                " })()"
            )
            if not data:
                raise LookupError(
                    "the day grid draws no events-time-slot markers to measure against; "
                    f"{self.driver.diagnose('events-time-slot-00-00')}"
                )
            blocks = [(text or "", tuple(frame)) for text, frame in data["blocks"]]
            return blocks, tuple(data["s0"]), tuple(data["s1"])
        if self.driver.is_windows():
            return self._clipped_timeline_frames(summary, viewport="calendar-day-body")
        if self.driver.is_android():
            return self._clipped_timeline_frames(summary, viewport=None)
        records = self.driver.registry_snapshot() or []

        def frame(record: dict) -> tuple:
            return tuple(float(v) for v in record["frame"].split(","))

        by_id = {r["id"]: r for r in records if r.get("frame")}
        if "events-time-slot-00-00" not in by_id or "events-time-slot-01-00" not in by_id:
            raise LookupError(
                "the day grid publishes no events-time-slot frames to measure against; "
                f"{self.driver.diagnose('events-time-slot-00-00')}"
            )
        blocks = [
            (r.get("text") or "", frame(r))
            for r in records
            if r["id"] == "calendar-event-block" and r.get("frame")
        ]
        return blocks, frame(by_id["events-time-slot-00-00"]), frame(by_id["events-time-slot-01-00"])

    def _clipped_timeline_frames(
        self, summary: str | None, *, viewport: str | None
    ) -> tuple[list[tuple[str, tuple]], tuple, tuple]:
        """:meth:`_timeline_frames` where the bridge's ``frame`` attribute
        (``"x,y,w,h"``) is CLIPPED to the scroller's viewport: windows (UIA's
        ``BoundingRectangle``) and android (accessibility ``getBoundsInScreen``).

        A block or slot marker scrolled out of view reads as an EMPTY rect and a
        half-scrolled one as its visible part — and the day grid opens scrolled
        to ~08:00 on both, midnight above the fold. So the ``summary`` block is
        brought WHOLLY into view first, and the midnight and 01:00 frames
        returned are extrapolated from the first and last WHOLE
        ``events-time-slot-HH-00`` markers on screen with it (a marker the
        viewport edge clips is shorter and its top is the edge's, so it is left
        out). Same grid, same ruler — only the two markers it is read off move.

        ``viewport`` names the scroller (windows' ``calendar-day-body``). There
        "wholly" is decided here, not by the bridge's visible-fraction scroll:
        that fraction is computed on the already-clipped rect, so a sliver reads
        as 100 %. A block edge lying on a viewport edge while the grid runs on
        past it is clipped; the view is stepped past that edge by scrolling the
        slot marker half an hour beyond it into view, and re-read. ``None``
        (android, whose scroller carries no id) trusts the bridge's scroll:
        ``ACTION_SHOW_ON_SCREEN`` is Compose's ``bringIntoView`` of the node's
        whole rect, and every block this layer measures is shorter than the
        viewport."""

        def rect(value: str | None) -> tuple | None:
            r = tuple(float(v) for v in value.split(",")) if value else None
            return r if r and r[2] > 0 and r[3] > 0 else None

        texts = self.driver.get_texts("calendar-event-block")
        index = next((i for i, t in enumerate(texts) if summary and summary in t), None)
        if index is not None:
            self.driver.scroll_to("calendar-event-block", index)
        seen: set[tuple] = set()
        for _ in range(48):  # a half-hour step at most per pass: 24 h is 48 of them
            frames = self.driver.get_attrs("calendar-event-block", "frame")
            blocks = [(t or "", r) for t, v in zip(texts, frames) if (r := rect(v))]
            on_screen: list[tuple[int, tuple]] = []
            for hour in range(24):
                r = rect(self.driver.get_attr(f"events-time-slot-{hour:02d}-00", "frame"))
                if r:
                    on_screen.append((hour, r))
                elif on_screen:
                    break  # the viewport shows one contiguous band of hours
            whole_h = max((r[3] for _, r in on_screen), default=0.0)
            whole = [(hour, r) for hour, r in on_screen if r[3] >= whole_h - 0.5]
            if len(whole) < 2:
                raise LookupError(
                    "the day grid shows fewer than two whole events-time-slot-HH-00 "
                    f"markers to measure against (on screen: {on_screen!r}); "
                    f"{self.driver.diagnose('events-time-slot-00-00')}"
                )
            (a, ra), (b, rb) = whole[0], whole[-1]
            hour_px = (rb[1] - ra[1]) / (b - a)
            midnight = ra[1] - a * hour_px
            s0 = (ra[0], midnight, ra[2], ra[3])
            s1 = (ra[0], midnight + hour_px, ra[2], ra[3])

            mine = rect(frames[index]) if index is not None and index < len(frames) else None
            body = rect(self.driver.get_attr(viewport, "frame")) if viewport else None
            if mine is None or body is None:
                return blocks, s0, s1
            top, bottom = body[1], body[1] + body[3]
            cut_below = mine[1] + mine[3] >= bottom - 1 and midnight + 24 * hour_px > bottom + 1
            cut_above = mine[1] <= top + 1 and midnight < top - 1
            if not (cut_below or cut_above):
                return blocks, s0, s1
            if mine in seen:  # the last step moved nothing: stepping on cannot help
                break
            seen.add(mine)
            edge = bottom + hour_px / 2 if cut_below else top - hour_px / 2
            minute = min(max(int((edge - midnight) / hour_px * 4) * 15, 0), 23 * 60 + 45)
            self.driver.scroll_to(f"events-time-slot-{minute // 60:02d}-{minute % 60:02d}")
        raise LookupError(
            f"could not bring the {summary!r} block wholly into the day view to "
            f"measure it; last frame {mine!r}, viewport {body!r}"
        )

    def _measured_block_layout(self, summary: str) -> dict:
        """:meth:`event_block_layout` read off the drawn block: its top and bottom
        against the slot ruler (:meth:`_timeline_frames`) give its start and end,
        and its width against the column's gives how many columns its clash group
        packs into — its left edge then says which one. Minutes round to
        :attr:`_MEASURE_ROUND_MIN`.

        The end is the drawn bottom, so it is only as honest as the drawing: an
        app that pads a short block past its end (a minimum height) reads long.
        The layout witness asserts ends only on hour-plus blocks."""
        texts: list[str] = []
        for _ in range(5):  # a repaint mid-read is not an answer (see wait_for_event_block)
            blocks, s0, s1 = self._timeline_frames(summary)
            texts = [text for text, _ in blocks]
            mine = [f for text, f in blocks if summary in text]
            if mine:
                break
            time.sleep(0.3)
        else:
            raise LookupError(f"no calendar-event-block for {summary!r}; rendered: {texts!r}")
        x, y, w, h = mine[0]
        col_x, col_y, col_w, _ = s0
        hour_px = s1[1] - s0[1]
        assert hour_px > 0 and w > 0, f"unmeasurable day grid: slot 00:00={s0}, 01:00={s1}, block={mine[0]}"

        def hhmm(px: float) -> str:
            step = self._MEASURE_ROUND_MIN
            minutes = int(round((px - col_y) / hour_px * 60 / step)) * step
            return f"{minutes // 60:02d}:{minutes % 60:02d}"

        columns = max(1, round(col_w / w))
        return {
            "start": hhmm(y),
            "end": hhmm(y + h),
            "column": min(columns - 1, max(0, round((x - col_x) / (col_w / columns)))),
            "columns": columns,
        }

    def wait_for_event_block(self, summary: str,
                             timeout: float = BLOCK_RENDER_BUDGET_S) -> None:
        """Wait until the week/day grid renders the block for `summary`.

        The identity match matters: the grids query **all** calendars when none
        is selected, and the actor is session-scoped, so `count(...) >= 1` is
        satisfied by any earlier test's event in the same week — it can pass with
        the event under test entirely absent.
        """
        deadline = time.monotonic() + timeout
        rendered: list[str] = []
        while time.monotonic() < deadline:
            try:
                rendered = self.event_block_texts()
            except LookupError:
                # The grid repainted between `get_texts`' count and a read — a
                # read that did not match yet, never an answer.
                time.sleep(0.3)
                continue
            if any(summary in text for text in rendered):
                return
            time.sleep(0.3)
        raise AssertionError(
            f"no calendar-event-block rendered for {summary!r} within {timeout}s: "
            f"blocks={rendered!r}, {self.driver.diagnose('calendar-event-block')}"
        )

    def back_from_detail(self) -> None:
        self.driver.click("event-detail-back")

    # --- Reminders (event detail surface) ---

    def open_detail_by_summary(self, summary: str, timeout: float = 10) -> None:
        """Open an event's detail surface by matching its card summary.

        Waits for event cards to render first — after a calendar select or a
        page remount the agenda loads its events asynchronously, so clicking
        immediately can race ahead of the cards appearing.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("event-card") > 0:
                break
            time.sleep(0.3)
        self._click_event_card_by_summary(summary)
        self.driver.wait_for("event-detail-summary")

    def set_reminder(self, offset: str) -> None:
        """Pick a reminder preset offset (e.g. 'PT1H') and confirm it.

        Requires the event detail to be open with no reminder currently set
        (the preset select + Set button are only shown in that state).
        """
        self.driver.select("event-detail-reminder-select", offset)
        self.driver.click("event-detail-reminder-set")
        # The set call + state flip to the current-reminder view is async.
        self.driver.wait_for("event-detail-reminder-current", timeout=10)

    def reminder_current_label(self) -> str:
        """Return the currently-set reminder's display label."""
        return self.driver.get_text("event-detail-reminder-current")

    def remove_reminder(self) -> None:
        """Clear the currently-set reminder (detail must be open, reminder set)."""
        self.driver.click("event-detail-reminder-remove")
        # State flips back to the preset select once the delete completes.
        self.driver.wait_for("event-detail-reminder-select", timeout=10)

    def has_reminder(self) -> bool:
        """True if a reminder is currently set (the current-label is shown)."""
        return not self.driver.is_absent("event-detail-reminder-current")

    # --- Refused scheduling changes (ui/events.md § Refused scheduling
    #     changes; caldav-server.md § Who may mutate an existing event over the
    #     inbound rail → *Surfacing*) ---

    def refused_change_count(self) -> int:
        """How many refused-change rows the page is showing.

        `0` covers the ordinary account, whose list is absent entirely — the
        group paints only when there is something to report, so a missing
        container here is the empty state, not a build gap.
        """
        return self.driver.count("refused-change-item")

    def refused_changes(self) -> list[dict[str, str]]:
        """Every refused-change row as `{title, author, reason, time}`.

        One honest read per row, like `event_summaries`: a row can appear
        between the count and the per-index reads (another device's refusal
        syncs in), so a `LookupError` from that race propagates rather than
        being papered over — `wait_for_refused_change` is the caller that
        absorbs it.
        """
        rows = []
        for i in range(self.driver.count("refused-change-item")):
            rows.append(
                {
                    "title": self.driver.get_text("refused-change-title", index=i),
                    "author": self.driver.get_text("refused-change-author", index=i),
                    "reason": self.driver.get_text("refused-change-reason", index=i),
                    "time": self.driver.get_text("refused-change-time", index=i),
                }
            )
        return rows

    def wait_for_refused_change(
        self, title_substring: str, timeout: float = 120.0
    ) -> dict[str, str]:
        """Wait until a refused-change row naming `title_substring` is on the
        page, and return it.

        **Re-navigates each pass.** The list is loaded with the page's calendar
        hydration rather than by a poll of its own (the rows change only when
        the inbound drain refuses something), so re-entering the page is what
        fetches it — the same shape `_wait_for_calendar` uses.
        """
        deadline = time.monotonic() + timeout
        seen: list[dict[str, str]] = []
        while time.monotonic() < deadline:
            try:
                for row in self.refused_changes():
                    if title_substring in row["title"]:
                        return row
                seen = self.refused_changes()
            except Exception:  # noqa: BLE001 - a mid-refresh read race; retry
                pass
            # The loop's exit is the ROW's arrival and its failure is the
            # deadline above, so a slow machine polls more times rather than
            # reading state that has not landed yet.
            time.sleep(1.0)  # sleep-ok: a deadline poll's pacing (convention 14 mechanism 1)
            self.navigate()
        raise AssertionError(
            f"no refused-change row naming {title_substring!r} appeared within "
            f"{timeout}s; rows on the page: {seen!r}"
        )

    def dismiss_refused_change(self, index: int = 0) -> None:
        """Dismiss one refused-change row — the list's only control (the ruling
        gives it no apply-anyway affordance)."""
        self.driver.click("refused-change-dismiss", index=index)

    # --- Extended event form fields ---

    def create_event_full(self, summary: str, start: str, end: str,
                          description: str = "", location: str = "") -> None:
        """Create an event with all optional form fields.

        `event-form-description` / `event-form-location` are typed **without an
        `is_visible` guard**, deliberately. Until 2026-08-10 both were wrapped
        in one, so an app whose compose lacked the input simply created an event
        without it and every caller passed — e2e convention 7's shape (an
        app-dependent skip that reports as success), one layer down in the
        action code where the skip ratchet cannot see it. ui.yaml scopes both to
        the `event-form` component with no platform carve-out, so a missing
        input is a build gap and must fail loudly here.
        """
        self._open_compose()
        self.driver.clear_and_type("event-summary", summary)
        self.driver.clear_and_type("event-dtstart", start)
        self.driver.clear_and_type("event-dtend", end)
        if description:
            self.driver.clear_and_type("event-form-description", description)
        if location:
            self.driver.clear_and_type("event-form-location", location)
        self._submit_compose(summary, start, end)
        self._wait_for_event(summary)
        self._wait_for_create_settled()

    # --- Detail-surface text rows (events.md § Element IDs: the event_detail
    # `event-detail-*` family) ---

    def detail_time_text(self) -> str:
        """The event's rendered time range (detail must be open)."""
        return self.driver.get_text("event-detail-time")

    def detail_location_text(self) -> str:
        """The event's rendered location (detail must be open)."""
        return self.driver.get_text("event-detail-location")

    def detail_description_text(self) -> str:
        """The event's rendered description (detail must be open)."""
        return self.driver.get_text("event-detail-description")

    # --- Range navigation ---
    #
    # `events-prev-month` / `events-next-month` pan the **visible range**, one
    # range per click (events.md § User actions) — a month in month view, a
    # week in week view, a day in day view, and nothing in the date-unfiltered
    # agenda. The ids keep their "month" spelling from ui.yaml; the helpers
    # below are named for the id, not the distance.

    def prev_month(self) -> None:
        """Pan back one visible range."""
        before = self.date_label()
        self.driver.click("events-prev-month")
        self._wait_for_date_label_change(before)

    def next_month(self) -> None:
        """Pan forward one visible range."""
        before = self.date_label()
        self.driver.click("events-next-month")
        self._wait_for_date_label_change(before)

    def pan_forward_without_waiting(self) -> None:
        """Click `events-next-month` with no expectation that anything moves.

        The agenda view has no date range to pan, so its label is *supposed* to
        hold still — which makes `next_month`'s change-barrier the wrong tool
        there (it would time out on correct behaviour). Callers asserting
        inertness use this and then poll for the absence themselves.
        """
        self.driver.click("events-next-month")

    def _wait_for_date_label_change(self, before: str,
                                    timeout: float = MONTH_NAV_BUDGET_S) -> None:
        """Wait until `calendar-date-label`'s text differs from `before`.

        Replaces a bare `time.sleep(0.5)` after a month-nav click. That sleep
        was a wall-clock timing assertion in convention 14's DEFUNCT class:
        half a second is not reliably enough for the re-render on a loaded
        build machine. The label actually changing is the latency-independent
        observable — callers (e.g. `test_month_navigation`) already compare a
        before/after label, so this just moves that same comparison earlier.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.date_label() != before:
                return
            time.sleep(0.1)
        raise TimeoutError(
            f"calendar-date-label still reads {before!r} {timeout}s after a "
            f"month-nav click: {self.driver.diagnose('calendar-date-label')}"
        )

    def wait_for_date_label(self, expected: str,
                            timeout: float = MONTH_NAV_BUDGET_S) -> None:
        """Wait until `calendar-date-label` reads `expected`.

        The positive-wait half of `_wait_for_date_label_change`, for asserting
        a label *returns to* (or stays at) a known value across a view switch —
        the switch repaints the label, so a bare read straight after it can
        catch the outgoing view's text. Polling to a generous deadline costs a
        green run nothing and never asserts on wall-clock timing (convention
        14); a label that moved for real never reaches `expected`, so the
        assertion still discriminates.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.date_label() == expected:
                return
            time.sleep(0.1)
        raise AssertionError(
            f"calendar-date-label never settled on {expected!r} within {timeout}s "
            f"— it reads {self.date_label()!r}: "
            f"{self.driver.diagnose('calendar-date-label')}"
        )

    def date_label(self) -> str:
        """Get the current date/range label text."""
        return self.driver.get_text("calendar-date-label")

    def toggle_calendar_visibility(self, name: str) -> None:
        """Flip the `calendar-visibility` box on the row for calendar `name`.

        Scoped to that calendar's row. The id is `indexed: true` (ui.yaml,
        ratified 2026-08-02) precisely because there is one box **per
        calendar**; the pre-hardening version was a bare, concept-free
        ``click("calendar-visibility")`` on whatever row happened to be first,
        with no assertion at all — which is exactly how one ui.yaml id survived
        for months implemented as three incompatible concepts (a display filter
        on linux, a create-form private/public picker on four apps, a dead
        list-collapse button on windows) without a single test noticing
        (`goal/ui/events.md` § Implementation status today, 2026-08-02).

        The row index is the same one `calendar-item` uses: the two affordances
        are one sidebar row per calendar on every app, so
        ``_wait_for_calendar`` resolves both.
        """
        self.driver.click("calendar-visibility", index=self._wait_for_calendar(name))

    def hide_calendar(self, name: str, hidden_summary: str,
                      timeout: float = CALENDAR_LIST_BUDGET_S) -> None:
        """Uncheck `name`'s box and wait until `hidden_summary` has left the list.

        The concept assertion the bare click never made: `calendar-visibility`
        **filters the displayed no-selection union** (events.md § Where logic
        lives → *Which calendars display*). A toggle that flips a checkbox and
        changes nothing else passes a click-only check and fails this one.

        Latency-independent per convention 14 — disappearance is a positive
        state change, polled to a generous deadline rather than slept on, so a
        green run pays one poll and the ceiling sits far above any
        non-pathological re-render.
        """
        self.toggle_calendar_visibility(name)
        self._wait_for_event_gone(hidden_summary, timeout=timeout)

    def show_calendar(self, name: str, shown_summary: str,
                      timeout: float = CALENDAR_LIST_BUDGET_S) -> None:
        """Re-check `name`'s box and wait until `shown_summary` is listed again.

        The other half of the filter contract — re-checking restores the row to
        the union, so a toggle that only ever subtracts is caught too.
        """
        self.toggle_calendar_visibility(name)
        self._wait_for_event(shown_summary, timeout=timeout)

    def is_month_grid_visible(self) -> bool:
        """Check if the month grid is visible."""
        return self.driver.is_visible("events-month-grid")

    def has_day_cell(self, iso_date: str) -> bool:
        """True if the month grid exposes the indexed day cell for ``iso_date``.

        Used to skip the day-cell test on clients that have not yet rolled out
        ``events-day-cell`` (events.md § Implementation status today). Month view
        must be active.
        """
        return not self.driver.is_absent(f"events-day-cell-{iso_date}")

    def click_day_cell(self, iso_date: str) -> None:
        """Single-click the month-grid day cell for ISO date ``YYYY-MM-DD``.

        Microsoft Outlook month→day interaction (events.md § Layout & flow):
        drills into Day view for that date. Month view must be active; after the
        click, waits for the day-timeline marker so callers can assert at once.
        """
        self.driver.click(f"events-day-cell-{iso_date}")
        try:
            self.driver.wait_for("calendar-day-timeline", timeout=5)
        except TimeoutError:
            pass  # the test assertion reports the real failure

    def double_click_day_cell(self, iso_date: str) -> None:
        """Double-click the month-grid day cell for ISO date ``YYYY-MM-DD``.

        Microsoft Outlook month→new-event interaction (events.md § Layout & flow):
        opens the new-event compose prefilled with that date. Month view must be
        active; after the double-click, waits for the start-datetime field so
        callers can assert at once (absorbs the single-vs-double-click debounce).
        """
        self.driver.double_click(f"events-day-cell-{iso_date}")
        try:
            self.driver.wait_for("event-dtstart", timeout=5)
        except TimeoutError:
            pass  # the test assertion reports the real failure

    def has_time_slot(self, hh: int, mm: int) -> bool:
        """True if the active week/day time grid exposes the indexed empty time
        slot for ``HH:MM``.

        Used to skip the empty-slot quick-create tests on apps that have not
        yet rolled out ``events-time-slot`` (events.md § Implementation status
        today). Week or Day view must be active.

        Deliberately ``count() > 0``, NOT ``is_visible()``: this is a *capability*
        probe — "does this app paint slot targets at all" — and its answer must
        not depend on where the grid happens to be scrolled. The grids tile all
        24h (1440px on windows) inside a viewport that shows a few hours around
        the ~08:00 auto-scroll anchor, so a perfectly built slot at 13:30 is
        simply below the fold. windows made that concrete: ``is_visible`` there
        is ``found && !IsOffscreen`` (flaui-bridge ``Actions.cs::IsVisible``) and
        it reported the WEEK test's 13:30 slot as unbuilt while the DAY test's
        09:15 slot — same code, same markers, taller viewport — passed. That is a
        skip standing in for coverage, which convention 7 exists to forbid, and a
        viewport-dependent assertion, which convention 14 exists to forbid.

        Scroll position does not have to be corrected for the click that follows:
        ``click_time_slot`` resolves through the bridge's InvokePattern path,
        which acts on a realized element regardless of offscreen state (the
        scroll-retry in ``_post_with_scroll`` is for 404/not-realized, a
        different condition).
        """
        return self.driver.count(f"events-time-slot-{hh:02d}-{mm:02d}") > 0

    def click_time_slot(self, hh: int, mm: int) -> None:
        """Click the empty week/day-grid time slot for ``HH:MM``.

        Outlook empty-space interaction (events.md § Week & day timeline
        views): opens the new-event compose prefilled with the column's date
        AND the slot's snapped time. Week or Day view must be active; after
        the click, waits for the start-datetime field so callers can assert at
        once. In week view the slot ids repeat per day column and the driver
        resolves the FIRST showing match (the week's leading column, which is
        locale-dependent) — assert the exact date only from Day view, where
        the single column makes it unambiguous.
        """
        self.driver.click(f"events-time-slot-{hh:02d}-{mm:02d}")
        try:
            self.driver.wait_for("event-dtstart", timeout=5)
        except TimeoutError:
            pass  # the test assertion reports the real failure

    # --- RSVP on event cards (inline, not detail page) ---

    def has_card_rsvp(self) -> bool:
        """Does this app paint the CARD-level rsvp trio (`event-rsvp-*`)?

        The detail-page trio (`event-detail-rsvp-*`) is a different surface and
        is built everywhere — `test_event_rsvp` covers it. Used to declare the
        card-level rollout as counted `skip_unbuilt` debt rather than let it
        fail with a bare `LookupError` on apps that have not built it
        (events.md § Implementation status today).
        """
        return self.driver.is_visible("event-rsvp-going")

    def rsvp_going(self, index: int = 0) -> None:
        """Click RSVP going on an event card."""
        self.driver.click("event-rsvp-going", index=index)

    def rsvp_interested(self, index: int = 0) -> None:
        """Click RSVP interested on an event card."""
        self.driver.click("event-rsvp-interested", index=index)

    def rsvp_decline(self, index: int = 0) -> None:
        """Click RSVP decline on an event card."""
        self.driver.click("event-rsvp-decline", index=index)

    def rsvp_on_card(self, summary: str, status: str = "going") -> None:
        """Find the event card matching `summary` by text and RSVP inline on it,
        without opening the detail page. `event-rsvp-*` is indexed 1:1 with
        `event-card-summary` (both render once per agenda-row, in the same
        stable ForEach order), so the summary lookup's index carries over.
        """
        count = self.driver.count("event-card-summary")
        for i in range(count):
            text = self.driver.get_text("event-card-summary", index=i)
            if summary in text:
                self.driver.click(f"event-rsvp-{status}", index=i)
                return
        raise AssertionError(f"no event card found with summary containing {summary!r}")

    # ── Calendar-level .ics import / export (events.md § Import / Export) ──

    def has_calendar_file_controls(self) -> bool:
        """Whether the selected calendar exposes `calendar-export-button` — the
        probe a test uses to `skip_unbuilt` an app that has not wired the
        import/export ids yet (events.md § Implementation status today)."""
        return self.driver.is_visible("calendar-export-button")

    def export_selected_calendar(self) -> str:
        """Press `calendar-export-button` and return the path of the `.ics`
        file it wrote into the driver's download directory.

        Identity, not a count: the directory can hold files an earlier export
        in the same session wrote, so the new file is the one that was not
        there before the press (convention 14 — a deadline-poll on the file
        appearing, never a sleep)."""
        from helpers import budgets
        from helpers.waiting import wait_until
        import os

        download_dir = self.driver.download_dir()
        if not download_dir:
            raise AssertionError(
                "this driver declares no download_dir(); calendar-export-button "
                "writes into the platform downloads location, so the driver must "
                "expose where that is under e2e"
            )
        before = set(os.listdir(download_dir)) if os.path.isdir(download_dir) else set()
        self.driver.click("calendar-export-button")

        def _new_ics():
            if not os.path.isdir(download_dir):
                return None
            fresh = [n for n in os.listdir(download_dir)
                     if n.endswith(".ics") and n not in before]
            if not fresh:
                return None
            path = os.path.join(download_dir, fresh[0])
            # Written in one call, but a read racing the write would see a
            # truncated file: wait until it holds a whole VCALENDAR.
            try:
                with open(path, encoding="utf-8") as fh:
                    body = fh.read()
            except OSError:
                return None
            return path if "END:VCALENDAR" in body else None

        return wait_until(
            _new_ics,
            budgets.RPC_ROUNDTRIP_S,
            diagnose=lambda: (
                f"no new .ics in {download_dir} (before={sorted(before)!r}, "
                f"now={sorted(os.listdir(download_dir)) if os.path.isdir(download_dir) else None!r}); "
                f"{self.driver.diagnose('error-message')}"
            ),
        )

    def import_calendar_file(self, path: str) -> None:
        """Choose `path` in `calendar-import-file` and press
        `calendar-import-button`. Web's `calendar-import-file` is a real
        `<input type=file>` (a browser cannot read a typed path), every other
        app's is a text input a picker fills — the same platform branch as
        `MediaActions.upload_file`, kept in the action layer."""
        if self.driver.is_web():
            self.driver.set_input_files("calendar-import-file", path)
        else:
            self.driver.wait_for("calendar-import-file", timeout=15.0)
            self.driver.clear_and_type("calendar-import-file", path)
        self.driver.click("calendar-import-button")

    def press_import_with_no_file(self) -> None:
        """Press `calendar-import-button` with nothing chosen — the case
        events.md § Import / Export says must answer on `error-message`."""
        if not self.driver.is_web():
            self.driver.wait_for("calendar-import-file", timeout=15.0)
            self.driver.clear_and_type("calendar-import-file", "")
        self.driver.click("calendar-import-button")

    def no_ics_chosen_text(self) -> str:
        """What the no-file guard says: `events.ics_file_required` everywhere
        but tui, which types a path and so says `events.ics_path_required`."""
        from i18n.strings import S

        return S.events.ics_path_required if self.driver.is_tui() else S.events.ics_file_required
