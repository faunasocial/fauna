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


def test_fill_values_follow_the_field_kind():
    assert ua.fill_value("contact-email-input") == "walk@example.com"
    assert ua.fill_value("event-dtstart") == "2026-12-01T10:00"
    assert ua.fill_value("event-summary") == "Walk"


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
