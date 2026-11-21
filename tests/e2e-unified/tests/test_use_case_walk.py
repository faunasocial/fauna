"""tier_1: the journey harness — the driver's gesture record and the walk check.

Owner: `docs/goal/behavior/main-use-cases.md` § The journeys, point 2. No app:
the comparison (`helpers/use_case_walk.py::compare_walk`) runs on hand-built
records, and the record itself (`drivers/gesture_record.py`) on a driver
subclass whose gestures do nothing.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import gesture_record  # noqa: E402
from drivers.base import PlatformDriver  # noqa: E402
from helpers.use_case_walk import (  # noqa: E402
    Recorded,
    assert_use_case_walked,
    compare_walk,
    setup_gestures,
    step_matches,
    steps_of,
)

pytestmark = pytest.mark.tier_1

STEPS = ["conversations-tab", "new-conversation-button", "recipient-picker-input",
         "dm-text-field", "dm-send-button"]


def rec(*elements, setup=()):
    return [Recorded(e, e in setup) for e in elements]


# ── the comparison ───────────────────────────────────────────────────────────

def test_the_steps_in_order_pass():
    assert compare_walk(STEPS, rec(*STEPS)) is None


def test_gestures_before_the_first_step_and_after_the_last_are_the_tests_own():
    record = rec("compose-text-field", *STEPS, "dm-message-row", "dm-reply-button")
    assert compare_walk(STEPS, record) is None


def test_a_missing_step_fails_and_names_it():
    record = rec("conversations-tab", "new-conversation-button", "dm-text-field",
                 "dm-send-button")
    why = compare_walk(STEPS, record)
    assert why is not None and "'recipient-picker-input'" in why


def test_a_step_never_reached_at_the_end_is_named():
    why = compare_walk(STEPS, rec(*STEPS[:3]))
    assert why == ("step 4 ('dm-text-field') was never performed after step 3 "
                   "('recipient-picker-input')")


def test_an_absent_first_step_is_named():
    assert compare_walk(STEPS, rec(*STEPS[1:])) == (
        "step 1 ('conversations-tab') was never performed")


def test_steps_out_of_order_fail():
    record = rec("conversations-tab", "new-conversation-button", "dm-text-field",
                 "recipient-picker-input", "dm-send-button")
    why = compare_walk(STEPS, record)
    assert why is not None and "undeclared gesture on 'dm-text-field'" in why


def test_an_undeclared_extra_gesture_between_steps_fails():
    record = rec("conversations-tab", "new-conversation-button", "topic-toggle-button",
                 "recipient-picker-input", "dm-text-field", "dm-send-button")
    why = compare_walk(STEPS, record)
    assert why == ("undeclared gesture on 'topic-toggle-button' between step 2 "
                   "('new-conversation-button') and step 3 ('recipient-picker-input')")


def test_a_declared_setup_gesture_between_steps_passes():
    record = rec("conversations-tab", "new-conversation-button", "sheet-dismiss",
                 "recipient-picker-input", "dm-text-field", "dm-send-button",
                 setup=("sheet-dismiss",))
    assert compare_walk(STEPS, record) is None


def test_a_repeat_on_the_step_just_walked_is_one_step():
    record = rec("conversations-tab", "new-conversation-button", "recipient-picker-input",
                 "recipient-picker-input", "dm-text-field", "dm-text-field",
                 "dm-send-button")
    assert compare_walk(STEPS, record) is None


def test_a_step_twice_in_a_row_needs_two_gestures():
    steps = ["wizard-name-input", "wizard-next-button", "wizard-next-button",
             "wizard-create-button"]
    assert compare_walk(steps, rec(*steps)) is None
    why = compare_walk(steps, rec("wizard-name-input", "wizard-next-button",
                                  "wizard-create-button"))
    assert why is not None and "'wizard-create-button'" in why


def test_a_false_start_is_retried_from_the_next_first_step():
    record = rec("conversations-tab", "feed-tab", *STEPS)
    assert compare_walk(STEPS, record) is None


def test_the_furthest_attempt_is_reported():
    record = rec("conversations-tab", "feed-tab",
                 "conversations-tab", "new-conversation-button", "feed-tab")
    why = compare_walk(STEPS, record)
    assert why is not None and "between step 2" in why


# ── the indexed-id fold ──────────────────────────────────────────────────────

@pytest.mark.parametrize("step,element,ok", [
    ("folder-row[<folder>]", "folder-row", True),
    ("folder-row[<folder>]", "folder-row[photos]", True),
    ("folder-row[<folder>]", "folder-share-button", False),
    ("settings-nav-row[folders]", "settings-nav-row[folders]", True),
    ("settings-nav-row[folders]", "settings-nav-row", True),
    ("settings-nav-row[folders]", "settings-nav-row[sessions]", False),
    ("feed-tab", "feed-tab", True),
    ("feed-tab", "feed-tab[0]", True),
    ("feed-tab", "feed-tabs", False),
])
def test_an_indexed_id_folds_to_its_base(step, element, ok):
    assert step_matches(step, element) is ok


# ── the driver's record ──────────────────────────────────────────────────────

class _Driver(PlatformDriver):
    """Gestures that do nothing; `clear_and_type` clicks first like some bridges."""

    def find_element(self, element_id, index=0, *, scope=None):
        return None

    def click(self, element_id, index=0, *, scope=None):
        if element_id == "broken-button":
            raise LookupError(element_id)

    def type_text(self, element_id, text, *, scope=None):
        pass

    def clear_and_type(self, element_id, text, *, index=0, scope=None):
        self.click(element_id, index, scope=scope)
        self.type_text(element_id, text, scope=scope)

    def get_text(self, element_id, index=0, *, scope=None):
        return ""

    def is_visible(self, element_id, *, scope=None):
        return True

    def press_key(self, element_id, key, *, scope=None):
        pass

    def get_attr(self, *a, **k):
        return None

    def launch(self, config):
        pass

    def teardown(self):
        pass

    def screenshot(self, name):
        raise NotImplementedError

    def navigate_to(self, view):
        pass


# The stub implements only what the record touches; the base's other abstract
# methods (waits, counts, …) are never called here.
_Driver.__abstractmethods__ = frozenset()


class _Subclass(_Driver):
    def click(self, element_id, index=0, *, scope=None):
        super().click(element_id, index, scope=scope)


def _make(cls=_Driver):
    try:
        return cls()
    except TypeError as exc:  # an abstract method the stub misses
        pytest.fail(f"stub driver is incomplete: {exc}")


def _elements(driver):
    return [(g.method, g.element, g.setup)
            for g in gesture_record.gestures_since_test_start(driver)]


def test_gestures_are_recorded_and_reads_are_not():
    d = _make()
    gesture_record.mark_test_start()
    d.click("feed-tab")
    d.get_text("feed-title")
    d.is_visible("compose-text-field")
    d.type_text("compose-text-field", "hi")
    d.press_key("compose-text-field", "Enter")
    assert _elements(d) == [("click", "feed-tab", False),
                            ("type_text", "compose-text-field", False),
                            ("press_key", "compose-text-field", False)]


def test_only_the_outermost_gesture_records():
    d = _make()
    gesture_record.mark_test_start()
    d.clear_and_type("handle-input", "alice")
    d.fill("handle-input", "bob")
    assert _elements(d) == [("clear_and_type", "handle-input", False),
                            ("clear_and_type", "handle-input", False)]


def test_a_super_call_in_an_override_records_once():
    d = _make(_Subclass)
    gesture_record.mark_test_start()
    d.click("feed-tab")
    assert _elements(d) == [("click", "feed-tab", False)]


def test_a_gesture_that_raises_records_nothing():
    d = _make()
    gesture_record.mark_test_start()
    with pytest.raises(LookupError):
        d.click("broken-button")
    assert _elements(d) == []


def test_setup_gestures_are_flagged():
    d = _make()
    gesture_record.mark_test_start()
    with setup_gestures(d):
        d.click("sheet-dismiss")
    d.click("feed-tab")
    assert _elements(d) == [("click", "sheet-dismiss", True),
                            ("click", "feed-tab", False)]


def test_only_gestures_since_the_test_start_count():
    d = _make()
    d.click("earlier-test-button")
    gesture_record.mark_test_start()
    d.click("feed-tab")
    assert _elements(d) == [("click", "feed-tab", False)]


def test_every_driver_class_records():
    import importlib
    for mod, cls in [("drivers.http_bridge", "HttpBridgeDriver"),
                     ("drivers.tui", "TuiDriver")]:
        klass = getattr(importlib.import_module(mod), cls)
        for name in gesture_record.GESTURE_METHODS:
            fn = getattr(klass, name)
            if name in ("double_click", "select", "set_input_files") and \
                    fn is getattr(PlatformDriver, name):
                continue
            assert getattr(fn, "__gesture_recorded__", False), f"{cls}.{name}"


# ── against the catalogue ────────────────────────────────────────────────────

def test_the_assertion_reads_the_catalogue_and_diagnoses_itself():
    d = _make()
    gesture_record.mark_test_start()
    steps = steps_of("read-feed-and-post")
    for el in steps:
        d.click(el)
    assert_use_case_walked(d, "read-feed-and-post")

    gesture_record.mark_test_start()
    d.click(steps[-1])
    with pytest.raises(AssertionError) as err:
        assert_use_case_walked(d, "read-feed-and-post")
    msg = str(err.value)
    assert "'read-feed-and-post' was not walked" in msg
    assert "catalogued steps (2)" in msg and "recorded gestures (1" in msg


def test_an_unknown_use_case_is_refused():
    with pytest.raises(AssertionError, match="not in use-cases.yaml"):
        steps_of("no-such-use-case")
