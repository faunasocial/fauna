"""How a ui.yaml element id becomes a constant member name, in one place.

Three consumers must agree on this, or the id-constant mechanism quietly breaks:

  * `ui-ids-generate.py` — emits the constants.
  * `lint-ui-elements.py` — asks "does this app implement this element?", which after
    adoption means finding a *constant reference*, not a string literal.
  * `lint-ui-registry.py` — asks the reverse, "what ids does this app render?".

Before this module existed the two lints knew only the literal form, so the first app
to adopt constants would have reported as implementing **nothing**: every id would
turn up missing on tui while the app rendered them correctly. That failure is silent
and looks exactly like a catastrophic regression, which is why the derivation and the
reference shapes live together here rather than being re-spelled per script.

Owner doc: `docs/goal/architecture/build-system.md` § Generated element-id constants.
"""
from __future__ import annotations

import re

# The element-id shape: lowercase alnum segments joined by '-', at least two segments,
# `_` allowed inside a segment (three registry ids carry one, `inbox-mode-allow_knock`).
KEBAB = re.compile(r"[a-z][a-z0-9_]*(?:-[a-z0-9_]+)+")


def parts(element_id: str) -> list[str]:
    return [p for p in re.split(r"[-_]", element_id) if p]


def scream(element_id: str) -> str:
    """Rust / Kotlin / TS / Python: `feed-post-text` -> `FEED_POST_TEXT`."""
    return "_".join(p.upper() for p in parts(element_id))


def camel(element_id: str) -> str:
    """Swift: `feed-post-text` -> `feedPostText`."""
    head, *tail = parts(element_id)
    return head + "".join(p.capitalize() for p in tail)


def pascal(element_id: str) -> str:
    """C#: `feed-post-text` -> `FeedPostText`."""
    return "".join(p.capitalize() for p in parts(element_id))


# How a *reference* to the constant reads in each app's source. Keyed by the app names
# both lints use (they differ: lint-ui-elements splits apple into ios/macos).
#
# windows carries the XAML form as well as the C# one — the markup shape proven on
# Windows is `{x:Bind ids:Ids.FeedPostText}`, so `Ids.FeedPostText` is the substring both
# reach for. tui/linux carry the aliased and fully-qualified Rust forms, since a file
# may `use fauna_ui_ids as ids;` or spell the crate out.
REFERENCE_TEMPLATES: dict[str, list[str]] = {
    "web": ["IDS.{scream}"],
    "windows": ["Ids.{pascal}"],
    "linux": ["ids::{scream}", "fauna_ui_ids::{scream}"],
    "tui": ["ids::{scream}", "fauna_ui_ids::{scream}"],
    "apple": ["Ids.{camel}"],
    "ios": ["Ids.{camel}"],
    "macos": ["Ids.{camel}"],
    "android": ["Ids.{scream}"],
}


def reference_forms(app: str, element_id: str) -> list[str]:
    """Every source spelling of a constant reference to `element_id` in `app`."""
    names = {
        "scream": scream(element_id),
        "camel": camel(element_id),
        "pascal": pascal(element_id),
    }
    return [tpl.format(**names) for tpl in REFERENCE_TEMPLATES.get(app, [])]
