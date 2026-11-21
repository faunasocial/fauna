"""tier_1: the possibility sweep's ENGINE (`helpers/ui_walk_actuation.py`) over a
stub driver — no binary, no nest.

The sweep is a checker, so its bookkeeping is pinned where a step costs
microseconds: a checker that cries wolf gets deleted, and one whose bookkeeping
reports clean while it judged nothing is worse (convention 17's first discipline).
Each case below is a frame sequence the live sweep can meet, with the verdict it
must reach.
"""

import pytest

from helpers import ui_walk_actuation as ua

pytestmark = pytest.mark.tier_1


def rec(id, index=0, *, text="", enabled=True, actuable=False, editable=False, scope=""):
    return {"id": id, "index": index, "text": text, "enabled": enabled,
            "actuable": actuable, "editable": editable, "scope": scope}


def btn(id, **kw):
    return rec(id, actuable=True, **kw)


def field(id, text=""):
    return rec(id, editable=True, text=text)


class StubDriver:
    """`frames` maps a page to its entry frame; `on_click` maps an element id
    to the frame the click produces (or a callable over the stub)."""

    def __init__(self, frames, on_click=None, on_type=None):
        self.frames = frames
        self.on_click = on_click or {}
        self.on_type = on_type
        self.current = None
        self.calls = []

    def set_state(self, patch):
        page = patch["nav"]["stack"][0]["view"]
        self.calls.append(("enter", page))
        self.current = list(self.frames[page])

    def barrier(self):
        pass

    def registry_snapshot(self):
        return list(self.current)

    def registry_frame(self):
        # No `reads_owed` count, like every app but linux: read as loaded.
        return self.registry_snapshot(), 0

    def click(self, id, index=0):
        self.calls.append(("click", id))
        nxt = self.on_click.get(id)
        if isinstance(nxt, Exception):
            raise nxt
        if callable(nxt):
            nxt = nxt(self)
        if nxt is not None:
            self.current = list(nxt)

    def type_text(self, id, text):
        self.calls.append(("type", id, text))
        self.current = [dict(r, text=text) if r["id"] == id else r for r in self.current]

    def press_key(self, id, key):
        self.calls.append(("key", id, key))


def run(frames, on_click, lists=None, pages=None):
    report = ua.ActuationReport()
    driver = StubDriver(frames, on_click)
    sweep = ua.ActuationSweep(driver, report, lists or ua.DeclaredLists())
    for p in pages or list(frames):
        sweep.sweep_page(p)
    return report, driver


HEAD = rec("page-heading", text="Events")


def test_an_actuation_that_changes_the_frame_passes():
    page = [HEAD, btn("refresh-btn")]
    report, _ = run({"events": page},
                    {"refresh-btn": page + [rec("event-card", text="x")]})
    assert report.actuated == ["events/refresh-btn[0]"]
    assert not report.violations


def test_an_actuation_that_changes_nothing_and_says_nothing_is_a_violation():
    page = [HEAD, btn("refresh-btn")]
    report, _ = run({"events": page}, {"refresh-btn": page})
    assert len(report.violations) == 1
    assert "refresh-btn" in report.violations[0]


def test_a_painted_error_passes_even_when_nothing_else_changed():
    page = [HEAD, rec("error-message", text=""), btn("go-btn")]
    after = [HEAD, rec("error-message", text="No calendar yet"), btn("go-btn")]
    report, _ = run({"events": page}, {"go-btn": after})
    assert not report.violations


def test_the_declared_nothing_changes_list_holds_a_finding_back_by_name():
    page = [HEAD, btn("refresh-btn")]
    lists = ua.DeclaredLists(nothing_changes=[ua.ListEntry("refresh-btn", "unchanged list")])
    report, _ = run({"events": page}, {"refresh-btn": page}, lists)
    assert not report.violations
    assert report.declared_unchanged == ["events/refresh-btn[0]"]


def test_the_never_list_is_never_clicked_and_is_reported():
    page = [HEAD, btn("sign-out-button"), btn("x-btn")]
    lists = ua.DeclaredLists(never_actuate=[ua.ListEntry("sign-out*", "ends the session")])
    report, driver = run({"settings": page},
                         {"x-btn": page + [rec("new")]}, lists)
    assert ("click", "sign-out-button") not in driver.calls
    assert report.never_actuated == {"settings/sign-out-button": "ends the session"}


def test_disabled_and_inert_elements_are_not_actuated():
    page = [HEAD, btn("off-btn", enabled=False), rec("label")]
    report, driver = run({"events": page}, {})
    assert not [c for c in driver.calls if c[0] == "click"]
    assert report.actuated == []


def test_an_id_is_actuated_once_across_pages():
    """Shell chrome repeated on every page is judged on the first page only."""
    a = [rec("page-heading", text="A"), btn("feed-tab")]
    b = [rec("page-heading", text="B"), btn("feed-tab")]
    report, driver = run({"a": a, "b": b}, {"feed-tab": a + [rec("moved")]})
    assert [c for c in driver.calls if c == ("click", "feed-tab")] == [("click", "feed-tab")]


def _zero_calendar_frames():
    """The defect that started the invariant: the form opens, the save closes
    it, and the page underneath is exactly as it was — no error painted."""
    page = [HEAD, rec("events-empty", text="No calendars yet."), btn("new-event-btn"),
            rec("error-message", text="")]
    form = page + [field("event-summary"), field("event-dtstart", "2026-12-01T10:00"),
                   btn("create-event"), btn("event-form-discard")]
    return page, form


def test_a_form_that_closes_and_leaves_the_page_unchanged_is_the_violation():
    page, form = _zero_calendar_frames()
    report, driver = run({"events": page},
                         {"new-event-btn": form, "create-event": page})
    assert report.forms_submitted == ["events/new-event-btn[0] → create-event"]
    assert len(report.violations) == 1 and "form closed" in report.violations[0]
    # The empty field was filled by kind; the prefilled one was left alone.
    assert ("type", "event-summary", "Walk") in driver.calls
    assert not [c for c in driver.calls if c[:2] == ("type", "event-dtstart")]
    # Discard was never taken for the submit.
    assert ("click", "event-form-discard") not in driver.calls


def test_a_form_whose_submit_paints_an_error_passes():
    page, form = _zero_calendar_frames()
    errored = [r if r["id"] != "error-message" else dict(r, text="Create a calendar first")
               for r in page]
    report, _ = run({"events": page},
                    {"new-event-btn": form, "create-event": errored})
    assert not report.violations


def test_a_form_whose_submit_adds_a_row_passes():
    page, form = _zero_calendar_frames()
    report, _ = run({"events": page},
                    {"new-event-btn": form, "create-event": page + [rec("event-card", text="Walk")]})
    assert not report.violations


def test_a_form_that_stays_as_it_was_on_submit_is_a_violation():
    page, form = _zero_calendar_frames()
    report, _ = run({"events": page},
                    {"new-event-btn": form, "create-event": lambda d: d.current})
    assert len(report.violations) == 1 and "stayed" in report.violations[0]


def test_a_form_with_no_identifiable_submit_is_unjudged_never_clean():
    page = [HEAD, btn("open-btn")]
    form = page + [field("thing-input"), btn("one-btn"), btn("two-btn")]
    report, _ = run({"events": page}, {"open-btn": form})
    assert not report.violations
    assert any("no identifiable submit" in u for u in report.unjudged)


def test_a_refused_click_is_unjudged_never_clean():
    page = [HEAD, btn("go-btn")]
    report, _ = run({"events": page}, {"go-btn": RuntimeError("agent: disabled")})
    assert report.actuated == []
    assert report.unjudged and "click refused" in report.unjudged[0]


def test_a_barrier_that_failed_after_the_click_is_unjudged_never_a_violation():
    """A barrier that raised did not settle the frame (web's throws on a hung
    navigation; a full-document navigation destroys its evaluate), so the read
    after it may be the OLD page: judging it would report a nav tab as "did not
    change" when the sweep merely looked too early. It is unjudged, by reason."""
    page = [HEAD, btn("events-tab")]

    class FailingBarrier(StubDriver):
        def barrier(self):
            if ("click", "events-tab") in self.calls:
                raise RuntimeError("barrier: 1 navigation(s) still in flight after 15000 ms")

    report = ua.ActuationReport()
    sweep = ua.ActuationSweep(FailingBarrier({"feed": page}, {"events-tab": page}),
                              report, ua.DeclaredLists())
    sweep.sweep_page("feed")
    assert not report.violations
    assert any("events-tab" in u and "frame did not settle" in u
               and "15000 ms" in u for u in report.unjudged), report.unjudged


def test_volatile_text_is_excluded_so_a_ticking_clock_cannot_mask_a_violation():
    """A clock that moves between the two entry reads must not count as the
    actuation's change."""
    ticks = iter(range(1000))

    class Ticking(StubDriver):
        def registry_snapshot(self):
            return [dict(r, text=str(next(ticks))) if r["id"] == "clock" else r
                    for r in self.current]

    page = [HEAD, rec("clock"), btn("go-btn")]
    report = ua.ActuationReport()
    sweep = ua.ActuationSweep(Ticking({"events": page}, {"go-btn": page}), report,
                              ua.DeclaredLists())
    sweep.sweep_page("events")
    assert report.volatile == {"events": 1}
    assert len(report.violations) == 1


def _minute_clock_sweep(lists):
    """A minute-granular clock (`1m`) that rolls to `2m` after the first step:
    the two entry reads, seconds apart, see it still."""
    rolled = {"yes": False}

    class Rolling(StubDriver):
        def registry_snapshot(self):
            text = "2m" if rolled["yes"] else "1m"
            return [dict(r, text=text) if r["id"] == "agent-uptime" else r
                    for r in self.current]

    page = [HEAD, rec("agent-uptime", text="1m"), btn("a-btn"), btn("b-btn")]

    def roll(_driver):
        rolled["yes"] = True
        return page + [rec("x")]

    report = ua.ActuationReport()
    sweep = ua.ActuationSweep(Rolling({"events": page}, {"a-btn": roll, "b-btn": roll}),
                              report, lists)
    sweep.sweep_page("events")
    return report


def test_a_minute_clock_the_entry_reads_could_not_see_tick_is_a_restore_line_undeclared():
    report = _minute_clock_sweep(ua.DeclaredLists())
    assert len(report.restore_failures) == 1
    assert "agent-uptime" in report.restore_failures[0]


def test_a_declared_clock_text_is_volatile_so_its_tick_is_no_restore_line():
    """A text that moves with the wall clock at a coarser grain than the two
    entry reads are apart (a humanized uptime: `1m` → `2m`) is declared by
    id, and compared by identity and state alone, like a measured volatile."""
    lists = ua.DeclaredLists(clock_texts=[ua.ListEntry("*-uptime", "humanized uptime")])
    report = _minute_clock_sweep(lists)
    assert not report.restore_failures
    assert report.volatile == {"events": 1}


def test_fill_values_follow_the_field_kind():
    assert ua.fill_value("contact-email-input") == "walk@example.com"
    assert ua.fill_value("event-summary") == "Walk"


def test_date_fields_are_filled_with_today_so_a_saved_item_lands_in_view():
    # A page opens on the current period; a fixed date outside it made a
    # successful event save read as "the form closed and nothing changed".
    import datetime

    today = datetime.date.today().isoformat()
    assert ua.fill_value("event-dtstart") == f"{today}T10:00"
    assert ua.fill_value("event-dtend") == f"{today}T11:00"
    assert ua.fill_value("snapshot-date") == today


def test_the_filled_date_is_read_at_fill_time_so_a_run_across_midnight_fills_the_new_day(monkeypatch):
    # The date was once frozen at import: a 35-minute suite that crossed
    # midnight filled yesterday's date and failed the test above.
    import datetime
    import types

    class Tomorrow(datetime.date):
        @classmethod
        def today(cls):
            return datetime.date(2031, 1, 2)

    monkeypatch.setattr(ua, "datetime", types.SimpleNamespace(date=Tomorrow))
    assert ua.fill_value("event-dtstart") == "2031-01-02T10:00"
    assert ua.fill_value("snapshot-date") == "2031-01-02"


def test_the_shipped_lists_load_and_every_entry_has_a_reason():
    lists = ua.DeclaredLists.load()
    assert lists.never("exit-tab") and lists.never("sign-out-button")
    assert lists.never("feed-post-delete-button")
    assert lists.never("new-event-btn") is None
    assert lists.never("calendar-event-block") is None


def test_an_entry_without_a_reason_is_refused(tmp_path):
    p = tmp_path / "lists.toml"
    p.write_text('[[never_actuate]]\nid = "x"\n')
    with pytest.raises(AssertionError, match="reason"):
        ua.DeclaredLists.load(p)


def test_a_flipped_attr_is_a_change_even_when_the_text_is_not():
    """A checkbox's `checked`, a copy's `copied`: the text alone would miss them."""
    box = dict(rec("kind-checkbox", actuable=True, text="Mail"), attrs={"checked": "false"})
    page = [HEAD, box]
    flipped = [HEAD, dict(box, attrs={"checked": "true"})]
    report, _ = run({"backups": page}, {"kind-checkbox": flipped})
    assert not report.violations


def test_re_choosing_the_current_choice_is_classed_apart_not_a_violation():
    page = [HEAD, dict(btn("calendar-view-agenda"), attrs={"state": "on"})]
    report, _ = run({"events": page}, {"calendar-view-agenda": page})
    assert not report.violations
    assert report.current_choice == ["events/calendar-view-agenda[0]"]


def test_the_report_names_each_current_choice_target():
    report = ua.ActuationReport()
    report.current_choice.append("events/calendar-view-agenda[0]")
    assert "  current   events/calendar-view-agenda[0]" in report.lines()


def test_the_report_names_each_actuated_target():
    report = ua.ActuationReport()
    report.actuated.append("profile/profile-posts-tab[0]")
    assert "  actuated  profile/profile-posts-tab[0]" in report.lines()


def test_an_unselected_choice_that_changes_nothing_is_still_a_violation():
    page = [HEAD, dict(btn("calendar-view-week"), attrs={"state": "off"})]
    report, _ = run({"events": page}, {"calendar-view-week": page})
    assert len(report.violations) == 1


def test_a_page_s_own_tab_is_judged_from_another_page():
    a = [rec("page-heading", text="Feed"), btn("feed-tab"), btn("events-tab")]
    b = [rec("page-heading", text="Events"), btn("feed-tab"), btn("events-tab")]
    report, driver = run({"feed": a, "events": b},
                         {"feed-tab": a, "events-tab": b}, pages=["feed", "events"])
    assert report.actuated == ["feed/events-tab[0]", "events/feed-tab[0]"]
    assert not report.violations


def test_a_page_left_changed_is_re_entered_through_its_own_tab():
    page = [HEAD, btn("events-tab"), btn("open-btn"), btn("go-btn")]
    sub = [HEAD, btn("events-tab"), rec("sub-heading", text="Sub")]
    settings = [rec("page-heading", text="Settings"), btn("events-tab")]
    driver = StubDriver({"events": page, "settings": settings},
                        {"open-btn": sub, "events-tab": page, "go-btn": page + [rec("x")]})
    real_set = driver.set_state

    def sticky(patch):
        # A nav patch to the page keeps its open sub-page, as tui's does.
        stuck = driver.current is not None and any(r["id"] == "sub-heading" for r in driver.current)
        real_set(patch)
        if stuck and patch["nav"]["stack"][0]["view"] == "events":
            driver.current = list(sub)

    driver.set_state = sticky
    report = ua.ActuationReport()
    ua.ActuationSweep(driver, report, ua.DeclaredLists()).sweep_page("events")
    assert ("click", "events-tab") in driver.calls
    assert not report.restore_failures
    assert report.actuated == ["events/open-btn[0]", "events/go-btn[0]"]


def test_a_submit_spends_the_fresh_account_and_the_next_step_starts_on_a_new_one():
    """A submitted form may create what a later affordance needs (a calendar
    before the event save), moving the account past the first-use state the
    invariant is about. With a `fresh_account` callback the sweep swaps to a new
    fresh account after every submit, before the next actuation."""
    page = [HEAD, btn("new-calendar-btn"), btn("new-event-btn"), rec("error-message", text="")]
    cal_form = page + [field("calendar-name"), btn("create-calendar")]
    ev_form = page + [field("event-summary"), btn("create-event")]
    report = ua.ActuationReport()
    driver = StubDriver({"events": page}, {
        "new-calendar-btn": cal_form,
        "create-calendar": page + [rec("calendar-item", text="Walk")],
        "new-event-btn": ev_form,
        "create-event": page,
    })

    def fresh():
        driver.calls.append(("fresh-account",))

    sweep = ua.ActuationSweep(driver, report, ua.DeclaredLists(), fresh_account=fresh)
    sweep.sweep_page("events")
    steps = [c[1] if c[0] == "click" else c[0] for c in driver.calls]
    # the swap falls after the calendar submit and before the event opener
    assert steps.index("fresh-account") > steps.index("create-calendar")
    assert steps.index("fresh-account") < steps.index("new-event-btn")
    assert report.accounts_spent == 1
    # the event save on the new account left the page unchanged: the violation
    assert any("create-event" in v for v in report.violations)


class BoundedAccountsApp:
    """An app whose account list is bounded the way the shared registry bounds
    it (`long-term-store.md` § Multi-account evolution → *The index is
    bounded*: 11 signed-in accounts): a sign-in past the bound is refused and
    the app stays on the account it had — what the live tui did at the 12th
    swap of a full sweep."""

    BOUND = 11

    def __init__(self):
        self.accounts = []
        self.live = None
        self.calls = []

    def reset(self):
        self.calls.append("reset")
        self.accounts, self.live = [], None

    def sign_in(self, user):
        self.calls.append(("sign-in", user))
        if user not in self.accounts:
            if len(self.accounts) >= self.BOUND:
                return  # refused: IndexFull, still the previous account
            self.accounts.append(user)
        self.live = user


def test_every_fresh_account_is_signed_in_on_a_reset_app_so_the_account_bound_is_never_met():
    """A full sweep submits more forms than the app's account list holds; each
    swap must land however many came before it (apps 6658: the 12th swap of a
    full tui sweep was refused and Settings went unswept)."""
    app = BoundedAccountsApp()
    serial = iter(range(1000))

    def register():
        return f"actor-{next(serial)}"

    def sign_in(user):
        app.sign_in(user)
        assert app.live == user, f"swap to {user} was refused; still on {app.live}"

    for _ in range(3 * BoundedAccountsApp.BOUND):
        ua.sign_in_on_a_fresh_device(app, register, sign_in)
    assert len(app.accounts) == 1
    # the reset precedes every sign-in, never follows it
    resets = [c for i, c in enumerate(app.calls) if i % 2 == 0]
    assert resets == ["reset"] * (3 * BoundedAccountsApp.BOUND)


# --- the engine gaps the first full tui corpus left unjudged ---------------


class KeyedStub(StubDriver):
    """A stub whose `press_key` produces a frame too (`on_key[(id, key)]`)."""

    def __init__(self, frames, on_click=None, on_key=None):
        super().__init__(frames, on_click)
        self.on_key = on_key or {}

    def press_key(self, id, key):
        self.calls.append(("key", id, key))
        nxt = self.on_key.get((id, key))
        if callable(nxt):
            nxt = nxt(self)
        if nxt is not None:
            self.current = list(nxt)


def _sweep(driver, page, lists=None):
    report = ua.ActuationReport()
    ua.ActuationSweep(driver, report, lists or ua.DeclaredLists()).sweep_page(page)
    return report


def test_a_form_with_no_submit_button_is_submitted_by_enter():
    """A search box is submitted by a real Enter keystroke, not left unjudged."""
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    form = page + [field("thing-search-box"), btn("one-btn"), btn("two-btn")]
    found = form + [rec("search-result", text="Walk")]
    driver = KeyedStub({"events": page}, {"open-btn": form},
                       {("thing-search-box", "Enter"): found})
    report = _sweep(driver, "events")
    assert ("key", "thing-search-box", "Enter") in driver.calls
    assert report.forms_submitted == ["events/open-btn[0] → thing-search-box ⏎"]
    assert not report.violations
    assert not report.unjudged


def test_an_enter_submit_that_changes_nothing_with_no_button_at_all_is_a_violation():
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    form = page + [field("thing-search-box")]
    report = _sweep(KeyedStub({"events": page}, {"open-btn": form}), "events")
    assert len(report.violations) == 1 and "Enter" in report.violations[0]


def test_an_enter_submit_that_changes_nothing_beside_ambiguous_buttons_stays_unjudged():
    """With buttons the sweep could not tell apart, Enter changing nothing is
    no proof the form offers nothing — one of the buttons may be its submit."""
    page = [HEAD, btn("open-btn")]
    form = page + [field("thing-input"), btn("one-btn"), btn("two-btn")]
    report = _sweep(KeyedStub({"events": page}, {"open-btn": form}), "events")
    assert not report.violations
    assert any("no identifiable submit" in u for u in report.unjudged)


def test_a_lookup_button_is_an_identifiable_submit():
    got = ua.submit_candidate([btn("contact-actor-id-lookup"), btn("other-btn")])
    assert got["id"] == "contact-actor-id-lookup"


def test_a_never_listed_field_is_not_filled_and_the_step_is_judged_as_a_plain_actuation():
    """A file picker is never typed into: the actuation that painted it is
    judged by its frame change alone."""
    feed = [HEAD, btn("media-tab")]
    media = [rec("page-heading", text="Media"), field("file-upload"), btn("media-tab")]
    lists = ua.DeclaredLists(never_actuate=[ua.ListEntry("file-upload", "a file picker")])
    driver = KeyedStub({"feed": feed}, {"media-tab": media})
    report = _sweep(driver, "feed", lists)
    assert not [c for c in driver.calls if c[0] in ("type", "key")]
    assert not report.unjudged and not report.violations
    assert report.never_actuated == {"feed/media-tab[0] file-upload": "a file picker"}


def test_a_disabled_field_is_not_filled_and_the_form_is_still_judged():
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    form = page + [field("snapshot-name"),
                   rec("restore-confirm-input", editable=True, enabled=False),
                   btn("snapshot-create-button")]
    driver = KeyedStub({"backups": page},
                       {"open-btn": form, "snapshot-create-button": page + [rec("snapshot-row")]})
    report = _sweep(driver, "backups")
    assert not [c for c in driver.calls if c[:2] == ("type", "restore-confirm-input")]
    assert ("type", "snapshot-name", "Walk") in driver.calls
    assert not report.unjudged
    assert report.forms_submitted == ["backups/open-btn[0] → snapshot-create-button"]


def test_a_form_of_only_disabled_fields_is_judged_as_a_plain_actuation():
    page = [HEAD, btn("open-btn")]
    opened = page + [rec("restore-confirm-input", editable=True, enabled=False)]
    driver = KeyedStub({"backups": page}, {"open-btn": opened})
    report = _sweep(driver, "backups")
    assert not report.unjudged and not report.violations
    assert report.actuated == ["backups/open-btn[0]"]


def test_a_settings_sub_page_is_left_by_its_back_gesture_so_every_rail_row_is_judged():
    """tui keeps Settings on the sub-page a rail row opened, through the nav
    patch and the tab re-entry alike; the sub-page's own back affordance brings
    the rail back, so no row is "gone before its turn"."""
    hub = [rec("page-heading", text="Settings"), btn("settings-tab"),
           btn("settings-nav-row[a]"), btn("settings-nav-row[b]"), btn("settings-nav-row[c]")]
    feed = [rec("page-heading", text="Feed"), btn("settings-tab")]
    state = {"sub": None}

    def sub(key):
        return [rec("page-heading", text=f"Sub {key}"), btn("settings-tab"),
                btn("settings-nav-back")]

    def opener(key):
        def go(_d):
            state["sub"] = key
            return sub(key)
        return go

    def back(_d):
        state["sub"] = None
        return hub

    driver = KeyedStub({"settings": hub, "feed": feed},
                       {"settings-nav-row[a]": opener("a"), "settings-nav-row[b]": opener("b"),
                        "settings-nav-row[c]": opener("c"), "settings-nav-back": back,
                        "settings-tab": lambda _d: sub(state["sub"]) if state["sub"] else hub})
    real_set = driver.set_state

    def sticky(patch):
        real_set(patch)
        if patch["nav"]["stack"][0]["view"] == "settings" and state["sub"]:
            driver.current = sub(state["sub"])

    driver.set_state = sticky
    report = _sweep(driver, "settings")
    assert report.actuated == ["settings/settings-nav-row[a][0]",
                               "settings/settings-nav-row[b][0]",
                               "settings/settings-nav-row[c][0]"]
    assert not report.unjudged
    assert not report.restore_failures


def test_a_back_affordance_the_entry_frame_paints_is_never_taken_to_restore():
    """Only a back affordance the actuation brought restores; one the page
    itself offers at entry would leave the page."""
    page = [HEAD, btn("open-btn"), btn("wizard-back-button"), rec("error-message", text="")]
    sub = [HEAD, btn("wizard-back-button"), rec("sub-heading", text="Sub")]
    driver = KeyedStub({"events": page, "settings": [HEAD]},
                       {"open-btn": sub, "events-tab": sub,
                        "wizard-back-button": page + [rec("left")]})
    real_set = driver.set_state

    def sticky(patch):
        stuck = driver.current is not None and any(r["id"] == "sub-heading" for r in driver.current)
        real_set(patch)
        if stuck and patch["nav"]["stack"][0]["view"] == "events":
            driver.current = list(sub)

    driver.set_state = sticky
    _sweep(driver, "events")
    clicks = [c[1] for c in driver.calls if c[0] == "click"]
    # wizard-back-button is clicked once, as its own target, never as a restore
    assert clicks.count("wizard-back-button") <= 1


def test_an_overlay_with_no_field_is_closed_by_escape_on_its_own_record():
    """A presented sheet that keeps through the nav patch and the tab re-entry
    alike, paints no field and no back affordance, and takes Escape only on its
    own record (apple's compose sheet: every other record refuses a named key)
    is closed by Escape addressed to the record the actuation brought — never
    judged against the sheet as found."""
    page = [HEAD, btn("open-btn"), btn("go-btn"), rec("error-message", text="")]
    sheet = page + [rec("thing-dialog", scope="thing-dialog[0]")]
    state = {"up": False}

    def opener(_d):
        state["up"] = True
        return sheet

    def escape(_d):
        state["up"] = False
        return page

    driver = KeyedStub({"events": page, "settings": [rec("page-heading", text="S")]},
                       {"open-btn": opener, "go-btn": page + [rec("x")],
                        "events-tab": lambda _d: sheet if state["up"] else page},
                       {("thing-dialog", "Escape"): escape})
    real_set, real_key = driver.set_state, driver.press_key

    def sticky(patch):
        real_set(patch)
        if patch["nav"]["stack"][0]["view"] == "events" and state["up"]:
            driver.current = list(sheet)

    def keyed(id, key):
        if (id, key) not in driver.on_key:
            driver.calls.append(("key-refused", id, key))
            raise RuntimeError(f"409: {id} takes no named key on apple")
        real_key(id, key)

    driver.set_state, driver.press_key = sticky, keyed
    report = _sweep(driver, "events")
    assert ("key", "thing-dialog", "Escape") in driver.calls
    assert not report.restore_failures
    assert report.actuated == ["events/open-btn[0]", "events/go-btn[0]"]


def _choice(id, on):
    return {**btn(id), "attrs": {"state": "on" if on else "off"}}


def test_a_moved_one_of_n_choice_is_chosen_again_to_restore():
    """A view mode or filter an earlier step moved persists through re-entry
    (apple keeps it); the restore chooses the entry's choice again instead of
    judging every later step against the moved one."""
    state = {"mode": "agenda"}

    def frame():
        return [HEAD, _choice("calendar-view-agenda", state["mode"] == "agenda"),
                _choice("calendar-view-week", state["mode"] == "week"), btn("go-btn")]

    def choose(mode):
        def go(_d):
            state["mode"] = mode
            return frame()
        return go

    driver = StubDriver({"events": frame(), "settings": [HEAD]},
                        {"calendar-view-agenda": choose("agenda"),
                         "calendar-view-week": choose("week"),
                         "events-tab": lambda _d: frame(),
                         "go-btn": lambda _d: frame() + [rec("x")]})
    real_set = driver.set_state

    def sticky(patch):
        real_set(patch)
        driver.current = frame() if patch["nav"]["stack"][0]["view"] == "events" \
            else driver.current

    driver.set_state = sticky
    report = _sweep(driver, "events")
    assert not report.restore_failures
    assert report.current_choice == ["events/calendar-view-agenda[0]"]
    assert "events/go-btn[0]" in report.actuated


def test_a_moved_rail_selection_marked_selected_is_chosen_again_to_restore():
    """Linux marks a rail's current row `selected: true` (a list row's
    selection), not `state: on`: the restore reads the current choice by the
    same rule the judge does (`is_current_choice`), so a Settings rail row an
    earlier step moved off is chosen again before the next step — one rule set for every app's way of painting a choice."""
    state = {"row": "settings-account"}

    def _rail_row(id):
        r = btn(id)
        if state["row"] == id:
            r["attrs"] = {"selected": "true"}
        return r

    def frame():
        return [HEAD, _rail_row("settings-account"), _rail_row("settings-limits"), btn("go-btn")]

    def choose(id):
        def go(_d):
            state["row"] = id
            return frame()
        return go

    driver = StubDriver({"settings": frame(), "feed": [HEAD]},
                        {"settings-account": choose("settings-account"),
                         "settings-limits": choose("settings-limits"),
                         "settings-tab": lambda _d: frame(),
                         "go-btn": lambda _d: frame() + [rec("x")]})
    real_set = driver.set_state

    def sticky(patch):
        real_set(patch)
        driver.current = frame() if patch["nav"]["stack"][0]["view"] == "settings" \
            else driver.current

    driver.set_state = sticky
    report = _sweep(driver, "settings")
    assert not report.restore_failures
    assert "settings/go-btn[0]" in report.actuated


def test_a_container_s_joined_text_drifting_is_no_restore_failure():
    """Linux's window record joins every label in the window into its text, so
    it drifts whenever anything anywhere repaints; its children are compared
    record by record already. The restore compares a record that scopes other
    records by its identity and state, never its look —
    while a child's own drift still fails the restore."""
    state = {"tick": 0}

    def frame(child_text="Hello"):
        win = rec("main-window", text=f"Hello {state['tick']}")
        return [HEAD, win, rec("label", text=child_text, scope="main-window[0]"),
                btn("go-btn", scope="main-window[0]"), btn("other-btn", scope="main-window[0]")]

    def go(_d):
        state["tick"] += 1
        return frame() + [rec("x")]

    driver = StubDriver({"feed": frame(), "settings": [HEAD]},
                        {"go-btn": go, "other-btn": lambda _d: frame() + [rec("y")],
                         "feed-tab": lambda _d: frame()})
    real_set = driver.set_state

    def sticky(patch):
        real_set(patch)
        driver.current = frame() if patch["nav"]["stack"][0]["view"] == "feed" \
            else driver.current

    driver.set_state = sticky
    report = _sweep(driver, "feed")
    assert not report.restore_failures
    assert ua.container_keys(frame()) == {("main-window", 0, "")}


def test_a_fresh_account_takes_a_fresh_entry_frame():
    """After a submit swaps in a new account, the page's entry is read again —
    the new account's handle is not an earlier step's leftover."""
    who = {"n": 1}

    def frame():
        return [rec("profile-handle", text=f"user{who['n']}"), btn("open-btn"),
                btn("go-btn"), rec("error-message", text="")]

    form_open = {"up": False}

    def page_frame():
        f = frame()
        return f + [field("thing-name-input"), btn("thing-save")] if form_open["up"] else f

    def opener(_d):
        form_open["up"] = True
        return page_frame()

    def save(_d):
        form_open["up"] = False
        return frame() + [rec("thing-card", text="Walk")]

    driver = StubDriver({"profile": frame(), "settings": [HEAD]},
                        {"open-btn": opener, "thing-save": save,
                         "go-btn": lambda _d: frame() + [rec("x")],
                         "profile-tab": lambda _d: frame()})
    real_set = driver.set_state

    def entering(patch):
        real_set(patch)
        if patch["nav"]["stack"][0]["view"] == "profile":
            driver.current = page_frame()

    driver.set_state = entering

    def renew():
        who["n"] += 1
        form_open["up"] = False

    report = ua.ActuationReport()
    ua.ActuationSweep(driver, report, ua.DeclaredLists(), fresh_account=renew).sweep_page("profile")
    assert report.accounts_spent == 1
    assert report.forms_submitted
    assert not report.restore_failures


def test_a_restore_failure_names_what_the_earlier_step_left_behind():
    """A restore line carries the records that differ, so the corpus says what
    stayed up — never only that something did (convention 6)."""
    page = [HEAD, btn("open-btn"), btn("go-btn")]
    sheet = page + [rec("thing-dialog", scope="thing-dialog[0]")]
    # A nav patch that keeps the sheet up, and no key that closes it.
    driver = StubDriver({"events": page, "settings": [HEAD]},
                        {"open-btn": sheet, "events-tab": sheet, "go-btn": sheet + [rec("x")]})
    real_set = driver.set_state

    def sticky(patch):
        up = driver.current is not None and any(r["id"] == "thing-dialog" for r in driver.current)
        real_set(patch)
        if up and patch["nav"]["stack"][0]["view"] == "events":
            driver.current = list(sheet)

    driver.set_state = sticky
    report = _sweep(driver, "events")
    assert len(report.restore_failures) == 1
    assert "+thing-dialog[0]@thing-dialog[0]" in report.restore_failures[0]
    assert ua.restore_diff(frozenset(), frozenset()) == ""


# --- the four tui forms Enter could not submit -------------


def test_a_backup_id_is_not_read_as_a_back_button():
    """`back` is a whole id token, never a substring: a substring test hid the
    backup-destination form's own confirm, and the step fell to Enter."""
    got = ua.submit_candidate([btn("backup-destination-add-confirm-button"),
                               btn("backup-destination-add-cancel-button")])
    assert got["id"] == "backup-destination-add-confirm-button"
    assert ua.submit_candidate([btn("settings-nav-back"), btn("thing-save")])["id"] == "thing-save"


def test_the_backup_destination_form_is_submitted_by_its_confirm():
    page = [HEAD, btn("backup-destination-add-button"), rec("error-message", text="")]
    form = page + [field("backup-destination-url-input"), field("backup-destination-name-input"),
                   btn("backup-destination-add-confirm-button"),
                   btn("backup-destination-add-cancel-button")]
    errored = [dict(r, text="could not reach the nest") if r["id"] == "error-message" else r
               for r in form]
    driver = KeyedStub({"backups": page},
                       {"backup-destination-add-button": form,
                        "backup-destination-add-confirm-button": errored})
    report = _sweep(driver, "backups")
    assert ("type", "backup-destination-url-input", "https://example.com") in driver.calls
    assert report.forms_submitted == [
        "backups/backup-destination-add-button[0] → backup-destination-add-confirm-button"]
    assert not report.unjudged and not report.violations


def test_a_policy_cell_is_filled_with_a_number_so_its_save_arms():
    assert ua.fill_value("feature-policy-editor-cell-input") == "5"


def test_the_feature_limit_editor_is_submitted_by_its_save():
    page = [HEAD, btn("feature-limits-own-edit-button"), rec("error-message", text="")]
    form = page + [btn("feature-policy-editor-on-radio"), btn("feature-policy-editor-off-radio"),
                   field("feature-policy-editor-cell-input"),
                   btn("feature-policy-editor-save-button"),
                   btn("feature-policy-editor-cancel-button")]
    saved = page + [rec("feature-limits-own-summary", text="5 a day")]
    driver = KeyedStub({"settings": page},
                       {"feature-limits-own-edit-button": form,
                        "feature-policy-editor-save-button": saved})
    report = _sweep(driver, "settings")
    assert report.forms_submitted == [
        "settings/feature-limits-own-edit-button[0] → feature-policy-editor-save-button"]
    assert not report.unjudged and not report.violations


def test_a_submit_the_fill_left_disabled_is_named_unjudged_and_no_enter_is_pressed():
    """The form's own validation refused the fill value: the sweep says which
    submit, and what it reads, instead of pressing Enter past the "not yet"."""
    page = [HEAD, btn("open-btn")]
    form = page + [btn("thing-on-radio"), btn("thing-off-radio"), field("thing-input"),
                   btn("thing-save-button", enabled=False, text="Not a number")]
    driver = KeyedStub({"settings": page}, {"open-btn": form})
    report = _sweep(driver, "settings")
    assert not [c for c in driver.calls if c[0] == "key"]
    assert not report.violations
    assert len(report.unjudged) == 1
    assert "thing-save-button" in report.unjudged[0] and "Not a number" in report.unjudged[0]


def _nostr_frames():
    page = [HEAD, btn("nostr-link-button"), rec("error-message", text="")]
    linked = [HEAD, rec("error-message", text=""),
              field("nostr-relay-input"), btn("nostr-add-relay"),
              field("nostr-follow-pubkey-input"), field("nostr-follow-petname-input"),
              btn("nostr-add-follow"),
              field("nostr-zap-signer-pubkey-input"), field("nostr-zap-signer-label-input"),
              btn("nostr-zap-signer-add-btn"), btn("nostr-unlink-button")]
    return page, linked


def test_fields_pair_with_their_own_section_s_submit():
    _, linked = _nostr_frames()
    fields = [r for r in linked if r["editable"]]
    buttons = [r for r in linked if r["actuable"]]
    got = [(b["id"], [f["id"] for f in fs]) for b, fs in ua.submit_groups(fields, buttons)]
    assert got == [
        ("nostr-add-relay", ["nostr-relay-input"]),
        ("nostr-add-follow", ["nostr-follow-pubkey-input", "nostr-follow-petname-input"]),
        ("nostr-zap-signer-add-btn",
         ["nostr-zap-signer-pubkey-input", "nostr-zap-signer-label-input"]),
    ]


def test_a_surface_of_several_small_forms_submits_each_section():
    page, linked = _nostr_frames()

    def adds(row):
        return lambda d: d.current + [rec(row, text="x")]

    driver = KeyedStub({"nostr": page},
                       {"nostr-link-button": linked,
                        "nostr-add-relay": adds("nostr-relay-item"),
                        "nostr-add-follow": adds("nostr-follow-item"),
                        "nostr-zap-signer-add-btn": adds("nostr-zap-signer-item")})
    report = _sweep(driver, "nostr")
    assert ("type", "nostr-relay-input", "wss://relay.example.com") in driver.calls
    assert ("type", "nostr-follow-pubkey-input", ua._FILL_PUBKEY) in driver.calls
    assert report.forms_submitted == [
        "nostr/nostr-link-button[0] → nostr-add-relay",
        "nostr/nostr-link-button[0] → nostr-add-follow",
        "nostr/nostr-link-button[0] → nostr-zap-signer-add-btn",
    ]
    assert ("click", "nostr-unlink-button") not in driver.calls
    assert not report.unjudged and not report.violations


def test_a_section_whose_submit_changes_nothing_is_the_violation():
    page, linked = _nostr_frames()
    driver = KeyedStub({"nostr": page},
                       {"nostr-link-button": linked,
                        "nostr-add-relay": lambda d: d.current + [rec("nostr-relay-item")],
                        "nostr-add-follow": lambda d: d.current,
                        "nostr-zap-signer-add-btn": lambda d: d.current + [rec("z")]})
    report = _sweep(driver, "nostr")
    assert len(report.violations) == 1 and "nostr-add-follow" in report.violations[0]


def test_a_declared_live_filter_is_not_a_form_and_the_actuation_is_judged_by_its_page_change():
    feed = [HEAD, btn("conversations-tab")]
    convs = [rec("page-heading", text="Conversations"), field("conversation-search-box"),
             btn("new-conversation-button")]
    lists = ua.DeclaredLists(filter_inputs=[ua.ListEntry("conversation-search-box", "filters")])
    driver = KeyedStub({"feed": feed}, {"conversations-tab": convs})
    report = _sweep(driver, "feed", lists)
    assert not [c for c in driver.calls if c[0] in ("type", "key")]
    assert not report.unjudged and not report.violations
    assert report.live_filters == {"feed/conversations-tab[0] conversation-search-box": "filters"}


def test_the_shipped_lists_declare_the_conversation_search_box_a_live_filter():
    assert ua.DeclaredLists.load().live_filter("conversation-search-box")


def test_an_id_judged_as_a_form_s_submit_is_not_actuated_again_on_a_later_page():
    """The contacts lookup is judged as the contacts form's submit; the contacts
    page, swept later, does not actuate it again with its field empty."""
    feed = [HEAD, btn("contacts-tab"), rec("error-message", text="")]
    contacts = [rec("page-heading", text="Contacts"), field("contact-actor-id-input"),
                btn("contact-actor-id-lookup"), rec("error-message", text="")]
    found = contacts + [rec("contact-row", text="Walk")]
    report = ua.ActuationReport()
    driver = KeyedStub({"feed": feed, "contacts": contacts},
                       {"contacts-tab": contacts, "contact-actor-id-lookup": found})
    sweep = ua.ActuationSweep(driver, report, ua.DeclaredLists())
    sweep.sweep_page("feed")
    sweep.sweep_page("contacts")
    assert [c for c in driver.calls if c == ("click", "contact-actor-id-lookup")] == [
        ("click", "contact-actor-id-lookup")]
    assert not report.unjudged and not report.violations


def _sticky(driver, page, frame):
    """The page keeps its own sub-state through a `nav` re-entry (apple)."""
    real_set = driver.set_state

    def sticky(patch):
        real_set(patch)
        if patch["nav"]["stack"][0]["view"] == page:
            driver.current = frame()

    driver.set_state = sticky


def test_a_cycling_control_the_sweep_moved_is_pressed_round_to_its_entry_value():
    """A button that cycles its own value (a sort order) persists through
    re-entry; the restore presses it again until its entry value returns, so
    later steps are judged from the entry frame."""
    orders = ["Latest activity", "Oldest first", "Name"]
    state = {"i": 0}

    def frame():
        return [HEAD, btn("conversation-sort", text=orders[state["i"]]), btn("go-btn")]

    def cycle(_d):
        state["i"] = (state["i"] + 1) % len(orders)
        return frame()

    driver = StubDriver({"conversations": frame(), "settings": [HEAD]},
                        {"conversation-sort": cycle,
                         "conversations-tab": lambda _d: frame(),
                         "go-btn": lambda _d: frame() + [rec("x")]})
    _sticky(driver, "conversations", frame)
    report = _sweep(driver, "conversations")
    assert not report.restore_failures, report.restore_failures
    assert state["i"] == 0
    assert "conversations/go-btn[0]" in report.actuated


def test_a_control_whose_look_moved_but_was_never_pressed_is_not_pressed_to_restore():
    """Only a control the sweep itself pressed, and whose own look that press
    moved, is pressed again: a button whose label changed because of ANOTHER
    step (a backup's "Restore from just now") is never actuated by the
    restore."""
    state = {"backed_up": False}

    def frame():
        label = "Restore from just now" if state["backed_up"] else "Restore from never"
        return [HEAD, btn("snapshot-now", text="Back up now"),
                btn("restore-btn", text=label), btn("go-btn")]

    def back_up(_d):
        state["backed_up"] = True
        return frame()

    lists = ua.DeclaredLists(never_actuate=[ua.ListEntry("restore-btn", "sovereign")])
    driver = StubDriver({"backups": frame(), "settings": [HEAD]},
                        {"snapshot-now": back_up, "backups-tab": lambda _d: frame(),
                         "go-btn": lambda _d: frame() + [rec("x")]})
    _sticky(driver, "backups", frame)
    _sweep(driver, "backups", lists)
    assert ("click", "restore-btn") not in driver.calls


def test_one_lasting_leftover_is_named_once_with_the_steps_judged_against_it():
    """A selection no gesture undoes (apple's `calendar-item`: a calendar once
    chosen stays chosen) is named ONCE, with how many later steps were judged
    against it — not repeated before every step, which buried the page's
    other causes. A different leftover gets its own line."""
    state = {"picked": False, "dialog": False}

    def frame():
        out = [HEAD, btn("calendar-item", text="Personal"),
               btn("a-btn"), btn("b-btn"), btn("open-btn")]
        if state["picked"]:
            out.append(btn("calendar-export-button"))
        if state["dialog"]:
            out.append(rec("thing-dialog"))
        return out

    def pick(_d):
        state["picked"] = True
        return frame()

    def open_(_d):
        state["dialog"] = True
        return frame()

    driver = StubDriver({"events": frame(), "settings": [HEAD]},
                        {"calendar-item": pick, "events-tab": lambda _d: frame(),
                         "a-btn": lambda _d: frame() + [rec("x")],
                         "b-btn": lambda _d: frame() + [rec("y")],
                         "open-btn": open_})
    _sticky(driver, "events", frame)
    report = _sweep(driver, "events")
    picked = [r for r in report.restore_failures if "calendar-export-button" in r]
    assert len(picked) == 1, report.restore_failures
    assert "before `a-btn`" in picked[0] and "2 later step(s)" in picked[0]
    assert "events/open-btn[0]" in report.actuated

def test_restore_and_volatile_lines_name_the_records_that_moved():
    """Convention 6: a restore failure names the records that differ from the
    entry frame, and a volatile page names its moving records — a bare count
    cannot say which record hid a change or held a page off its entry frame."""
    ticks = iter(range(1000))

    class Ticking(StubDriver):
        def registry_snapshot(self):
            return [dict(r, text=str(next(ticks))) if r["id"] == "clock" else r
                    for r in self.current]

    page = [HEAD, rec("clock"), btn("pin-btn"), btn("go-btn")]
    pinned = page + [rec("pin-badge", text="Pinned")]
    driver = Ticking({"events": page}, {"pin-btn": pinned, "go-btn": pinned})
    real_set = driver.set_state

    def sticky(patch):
        # The pin is a lasting change: re-entering the page keeps it.
        was = driver.current
        real_set(patch)
        if was is not None and any(r["id"] == "pin-badge" for r in was):
            driver.current = list(pinned)

    driver.set_state = sticky
    report = ua.ActuationReport()
    ua.ActuationSweep(driver, report, ua.DeclaredLists()).sweep_page("events")
    assert report.volatile_ids == {"events": ["clock[0]"]}
    assert len(report.restore_failures) == 1
    assert "pin-badge[0]" in report.restore_failures[0]
    assert any("clock[0]" in line for line in report.lines() if "volatile" in line)


class SelectStub(KeyedStub):
    """A stub with drop-downs: `options[id]` is the option list a select paints
    (None = the driver cannot read it); `select` paints the chosen value as the
    record's text and refuses a value the select does not offer. `type_text`
    on a select is refused, as the real drivers refuse it ("not editable"), and
    so is Enter on one."""

    def __init__(self, frames, on_click=None, on_key=None, options=None, selects=()):
        super().__init__(frames, on_click, on_key)
        self.options = options or {}
        self.selects = set(selects)

    def option_texts(self, id, index=0, *, scope=None):
        return self.options.get(id)

    def select(self, id, value, *, index=0, scope=None):
        self.calls.append(("select", id, value))
        if value not in (self.options.get(id) or []):
            raise AssertionError(f"{id} does not offer {value!r}")
        self.current = [dict(r, text=value) if r["id"] == id else r for r in self.current]

    def type_text(self, id, text):
        if id in self.selects:
            raise AssertionError(f"'{id}' is not editable")
        super().type_text(id, text)

    def press_key(self, id, key):
        if id in self.selects:
            raise AssertionError(f"Enter on drop-down '{id}' refused")
        super().press_key(id, key)


def _select_sweep(driver, page, lists=None):
    report = ua.ActuationReport()
    kinds = {i: "select" for i in driver.selects}
    ua.ActuationSweep(driver, report, lists or ua.DeclaredLists(), kinds).sweep_page(page)
    return report


def test_a_select_in_a_form_is_filled_by_choosing_an_option_never_typed_into():
    page = [HEAD, btn("subscribe-btn"), rec("error-message", text="")]
    form = page + [field("subscription-claim-tier-select"),
                   field("subscription-claim-note"), btn("subscription-claim-save")]
    saved = page + [rec("subscription-row", text="Gold")]
    driver = SelectStub({"subscriptions": page},
                        {"subscribe-btn": form, "subscription-claim-save": saved},
                        options={"subscription-claim-tier-select": ["Gold", "Silver"]},
                        selects={"subscription-claim-tier-select"})
    report = _select_sweep(driver, "subscriptions")
    assert ("select", "subscription-claim-tier-select", "Gold") in driver.calls
    assert not [c for c in driver.calls if c[:2] == ("type", "subscription-claim-tier-select")]
    assert not report.unjudged and not report.violations
    assert report.forms_submitted == [
        "subscriptions/subscribe-btn[0] → subscription-claim-save"]


def test_a_select_takes_the_first_option_that_is_not_its_current_one():
    assert ua.select_choice(["Gold", "Silver"], "") == "Gold"
    assert ua.select_choice(["Gold", "Silver"], "Gold") == "Silver"
    assert ua.select_choice(["Gold"], "Gold") is None
    assert ua.select_choice([], "") is None


def test_a_select_that_offers_no_option_is_unjudged_by_name():
    page = [HEAD, btn("restore-btn")]
    form = page + [field("restore-snapshot-select"), btn("restore-confirm-button")]
    driver = SelectStub({"backups": page}, {"restore-btn": form},
                        options={"restore-snapshot-select": []},
                        selects={"restore-snapshot-select"})
    report = _select_sweep(driver, "backups")
    assert any("restore-snapshot-select" in u and "select offers no option" in u
               for u in report.unjudged)
    assert not [c for c in driver.calls if c[0] == "select"]


def test_a_surface_of_only_selects_is_a_view_control_judged_by_its_own_frame_change():
    """A sort order and a folder filter are view controls, not a form: no fill,
    no Enter on a drop-down — the actuation that painted them is judged by its
    own frame change, and each is listed beside the count line."""
    feed = [HEAD, btn("media-tab")]
    media = [rec("page-heading", text="Media"), btn("media-tab"),
             field("media-sort-select", "Newest"), field("media-folder-filter")]
    driver = SelectStub({"feed": feed}, {"media-tab": media},
                        options={"media-sort-select": ["Newest", "Oldest"],
                                 "media-folder-filter": ["All"]},
                        selects={"media-sort-select", "media-folder-filter"})
    report = _select_sweep(driver, "feed")
    assert not [c for c in driver.calls if c[0] in ("type", "key", "select")]
    assert not report.unjudged and not report.violations and not report.forms_submitted
    assert set(report.live_filters) == {"feed/media-tab[0] media-sort-select",
                                        "feed/media-tab[0] media-folder-filter"}
    assert any(line.startswith("  filter    feed/media-tab[0] media-sort-select")
               for line in report.lines())


def test_a_select_only_surface_with_its_own_submit_is_a_form_and_is_submitted():
    """The view-control rule is for a surface with NO submit: a restore picker
    beside its confirm is a form — its select is chosen and the confirm pressed."""
    page = [HEAD, btn("restore-btn"), rec("error-message", text="")]
    form = page + [field("restore-snapshot-select"), btn("restore-confirm")]
    driver = SelectStub({"backups": page},
                        {"restore-btn": form, "restore-confirm": page + [rec("restore-done")]},
                        options={"restore-snapshot-select": ["snap-1"]},
                        selects={"restore-snapshot-select"})
    report = _select_sweep(driver, "backups")
    assert ("select", "restore-snapshot-select", "snap-1") in driver.calls
    assert report.forms_submitted == ["backups/restore-btn[0] → restore-confirm"]
    assert not report.live_filters and not report.unjudged


def test_a_select_beside_a_declared_filter_input_with_no_submit_is_a_view_control():
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    opened = page + [field("conversation-search-box"), field("log-level-filter")]
    lists = ua.DeclaredLists(filter_inputs=[ua.ListEntry("conversation-search-box", "a filter")])
    driver = SelectStub({"logs": page}, {"open-btn": opened},
                        options={"log-level-filter": ["Info", "Debug"]},
                        selects={"log-level-filter"})
    report = _select_sweep(driver, "logs", lists)
    assert not report.unjudged and not report.violations
    assert "logs/open-btn[0] log-level-filter" in report.live_filters


def test_a_form_mixing_text_and_select_with_no_submit_presses_enter_in_a_text_field():
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    form = page + [field("thing-search-box"), field("thing-kind-select")]
    found = form + [rec("search-result", text="Walk")]
    driver = SelectStub({"events": page}, {"open-btn": form},
                        {("thing-search-box", "Enter"): found},
                        options={"thing-kind-select": ["Any", "Posts"]},
                        selects={"thing-kind-select"})
    report = _select_sweep(driver, "events")
    assert ("key", "thing-search-box", "Enter") in driver.calls
    assert ("select", "thing-kind-select", "Any") in driver.calls
    assert not report.unjudged and not report.violations

def test_a_declared_nothing_changes_link_is_never_taken_as_a_form_s_submit():
    """A link that opens outside the app (the terms link, declared in the
    `nothing_changes` list) is not a submit: the web corpus of 2026-11-19 read
    the general sub-page's lone terms link as its form's submit and reported the
    form "stayed as it was". The form is judged by its own submit or by Enter."""
    page = [HEAD, btn("open-btn"), rec("error-message", text="")]
    form = page + [field("display-name-field"), btn("settings-terms-link")]
    saved = form + [rec("saved-note", text="Saved")]
    driver = KeyedStub({"settings": page}, {"open-btn": form},
                       {("display-name-field", "Enter"): saved})
    lists = ua.DeclaredLists(nothing_changes=[ua.ListEntry("*terms-link", "opens the browser")])
    report = _sweep(driver, "settings", lists)
    assert ("click", "settings-terms-link") not in driver.calls
    assert report.forms_submitted == ["settings/open-btn[0] → display-name-field ⏎"]
    assert not report.violations


class LiveFilterSelectStub(SelectStub):
    """The search page's shape: choosing the type filter re-fires the query held
    in the text field at that moment — a no-op while the field is blank
    (`search-type-filter` re-runs the LIVE buffer, tui's `SetTypeFilter`) — and
    the submit paints the outcome only if no fire already painted it."""

    def __init__(self, *a, outcome, field_id, **kw):
        super().__init__(*a, **kw)
        self.outcome, self.field_id = outcome, field_id

    def fire(self):
        if not any(r["id"] == self.outcome["id"] for r in self.current):
            self.current = self.current + [self.outcome]
        return self.current

    def select(self, id, value, *, index=0, scope=None):
        super().select(id, value, index=index, scope=scope)
        typed = next((r for r in self.current if r["id"] == self.field_id), {})
        if (typed.get("text") or "").strip():
            self.fire()


def test_a_form_s_selects_are_chosen_before_its_text_is_typed_so_a_live_filter_spends_nothing():
    """A select that re-fires the form's query on change (the search page's type
    filter), chosen after the text was typed, runs the search itself: the submit
    then re-runs the same query onto the same frame and reads as "stayed as it
    was" — the web corpus of 2026-11-20 reported exactly that for
    `search-submit-button`, its select read blank because its
    options paint only once the page's wasm is up. Chosen first, over a blank
    field, it fires nothing, and the submit is judged by the outcome it alone
    paints."""
    page = [HEAD, btn("search-tab"), rec("error-message", text="")]
    form = page + [field("search-query-field"), field("search-type-filter"),
                   btn("search-submit-button")]
    outcome = rec("search-no-results", text="No results for walk.")
    driver = LiveFilterSelectStub(
        {"feed": page},
        {"search-tab": form, "search-submit-button": lambda d: d.fire()},
        options={"search-type-filter": ["All", "Posts"]},
        selects={"search-type-filter"},
        outcome=outcome, field_id="search-query-field")
    report = _select_sweep(driver, "feed")
    order = [c[:2] for c in driver.calls if c[0] in ("select", "type")]
    assert order == [("select", "search-type-filter"), ("type", "search-query-field")]
    assert report.forms_submitted == ["feed/search-tab[0] → search-submit-button"]
    assert not report.violations and not report.unjudged


# --- the entry frame is read once the page has loaded -------


class LoadingDriver(StubDriver):
    """An app that paints a page's first read asynchronously and reports the
    reads it still owes (`reads_owed`): the first `owed` frame reads after an
    entry see the loading frame and a count above zero, then the loaded one."""

    def __init__(self, loading, loaded, owed, on_click=None):
        super().__init__({"events": loading, "settings": [HEAD]}, on_click)
        self.loading, self.loaded, self.owed_reads = loading, loaded, owed
        self.left = 0

    def set_state(self, patch):
        super().set_state(patch)
        if patch["nav"]["stack"][0]["view"] == "events" and self.left == 0 \
                and self.current == list(self.loading):
            self.left = self.owed_reads

    def registry_frame(self):
        self.frame_reads = getattr(self, "frame_reads", 0) + 1
        if self.left > 0:
            self.left -= 1
            if self.left == 0:
                self.current = list(self.loaded)
                self.frames["events"] = list(self.loaded)
            return list(self.loading), 1
        return list(self.current), 0


def test_the_entry_frame_waits_for_the_page_s_first_read():
    """An entry read while the page's calendar list is still on its way saw
    `new-event-btn` disabled and judged every later step against that; the
    entry is read once the app owes no read."""
    loading = [HEAD, btn("new-event-btn", enabled=False), btn("go-btn")]
    loaded = [HEAD, btn("new-event-btn"), btn("go-btn")]
    driver = LoadingDriver(loading, loaded, owed=3,
                           on_click={"new-event-btn": loaded + [rec("x")],
                                     "go-btn": loaded + [rec("y")]})
    report = _sweep(driver, "events")
    # The loaded button was swept: it was enabled in the entry frame.
    assert ("click", "new-event-btn") in driver.calls
    assert not report.restore_failures, report.restore_failures
    assert not report.unloaded


def test_a_page_whose_reads_never_settle_is_read_as_found_and_named():
    """The wait is bounded in reads, never a clock: a page still owed a read
    after `LOAD_READS` reads is read as found and named once."""
    page = [HEAD, btn("go-btn")]
    driver = LoadingDriver(page, page, owed=10 * ua.LOAD_READS,
                           on_click={"go-btn": page + [rec("y")]})
    report = _sweep(driver, "events")
    assert list(report.unloaded) == ["events"]
    # The bound is paid once: the page's later reads take the frame as found.
    assert driver.frame_reads <= ua.LOAD_READS + 1
    assert any(line.startswith("  unloaded  events:") for line in report.lines())


def test_an_app_without_a_count_is_read_as_loaded():
    """Every app but linux reports no `reads_owed`: its frame is read as before."""
    page = [HEAD, btn("go-btn")]
    report, driver = run({"events": page, "settings": [HEAD]}, {"go-btn": page + [rec("y")]},
                         pages=["events"])
    assert not report.unloaded
    assert ("click", "go-btn") in driver.calls


def test_a_frame_that_changes_only_on_a_second_barrier_read_is_a_named_settle_race():
    """The first barrier read the old page and the second the new one: the
    app's barrier returned before the gesture landed. That is reported unjudged
    by name (a barrier defect), never as the affordance's violation and never
    silently passed."""
    page = [HEAD, btn("events-tab")]
    arrived = [rec("page-heading", text="Calendar")]

    class LateFrame(StubDriver):
        reads_after_click = 0

        def registry_snapshot(self):
            if ("click", "events-tab") in self.calls:
                self.reads_after_click += 1
                if self.reads_after_click >= 2:
                    return list(arrived)
            return list(self.current)

    report = ua.ActuationReport()
    ua.ActuationSweep(LateFrame({"feed": page}, {"events-tab": page}),
                      report, ua.DeclaredLists()).sweep_page("feed")
    assert not report.violations
    assert any("events-tab" in u and "settle race" in u for u in report.unjudged), report.unjudged


def test_a_frame_still_unchanged_on_the_second_read_stays_the_violation():
    page = [HEAD, btn("refresh-btn")]
    report, _ = run({"events": page}, {"refresh-btn": page})
    assert len(report.violations) == 1
    assert not any("settle race" in u for u in report.unjudged)
