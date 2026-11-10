"""Conversation message formatting: checklist items paint as ticked and
unticked boxes and a nested list indents (message-formatting outcome 5;
render-model.md § D7 — task lists and nested lists).

Driven through the e2e inject seam, which hands the body to the same shared
markdown parser and the same document paint as a received message.
"""

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux, pytest.mark.android,
              pytest.mark.macos, pytest.mark.ios]


# The read that keeps a task item's box and a nested item's indent is the
# painted read (`ConversationActions.message_painted_lines`, `helpers/
# painted_lines.py`): tui publishes it off its paint, web reads its DOM; the
# other apps' one-line `get_text` drops both, so each joins when its agent
# publishes the same read (`surfaces.toml` `document-painted-read`).
@pytest.mark.feature("message-formatting")
def test_checklist_items_paint_boxes_and_a_nested_list_indents(logged_in_app):
    """render-model.md § D7: a message written with checklist items shows a
    ticked box for ``- [x]`` and an unticked one for ``- [ ]``, and a nested
    list shows indented under its parent.

    Each half is told apart from its wrong answer: the ticked item must paint
    ``☑`` and never ``☐`` (and the unticked one the reverse), so a renderer
    painting every box the same fails; and the nested item must start further
    right than its parent, so a renderer flattening the list fails."""
    conv = logged_in_app.conversations
    body = (
        "- [x] packed the tent\n"
        "- [ ] bought the stove\n"
        "- groceries\n"
        "  - oats"
    )
    conv.inject_and_open_thread(
        rail="FaunaMls", sender="carol-checklist@self-nest.test", subject=None, body=body
    )
    assert logged_in_app.driver.count("dm-message-text") >= 1, "no message bubble"

    lines = conv.message_painted_lines(0)
    assert lines, "the app published no painted line of dm-message-text"

    def line_with(words: str) -> str:
        hits = [ln for ln in lines if words in ln]
        assert len(hits) == 1, f"expected one painted line with {words!r}; painted={lines!r}"
        return hits[0]

    ticked = line_with("packed the tent")
    unticked = line_with("bought the stove")
    assert "☑" in ticked and "☐" not in ticked, f"ticked item painted {ticked!r}"
    assert "☐" in unticked and "☑" not in unticked, f"unticked item painted {unticked!r}"
    assert "[x]" not in ticked and "[ ]" not in unticked, f"raw task markers painted: {lines!r}"

    parent = line_with("groceries")
    child = line_with("oats")

    def indent(ln: str) -> int:
        return len(ln) - len(ln.lstrip())

    assert indent(child) > indent(parent), (
        f"nested item not indented under its parent: parent={parent!r} child={child!r}"
    )
