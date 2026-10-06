---
slug: formatting-as-you-type
title: See your formatting as you type
section: everyday
goal: docs/goal/ui/conversations.md § Compose-field inline markdown styling
guide: docs/guides/app-tour.md § Conversations
absences:
  tui: "docs/goal/ui/conversations.md § Compose-field inline markdown styling"
---

## What a user gets

The reply box shows your formatting while you write it: bold looks bold, code is
monospace, headings are larger and quotes are indented. The marks that make it so stay
out of sight until your cursor moves into the phrase they wrap, and a toolbar toggle
brings them all back for that box. What you typed is exactly what gets sent.

## Coverage contract

Stamped 2026-09-26 at 175da50d99.

1. [app] Formatting shows as you type: bold looks bold, code is monospace, headings are larger and quotes are indented — `docs/goal/ui/conversations.md` § Compose-field inline markdown styling
   - `tests/e2e-unified/tests/test_compose_live_styling.py::test_compose_styles_markdown_as_you_type`
2. [app] Formatting marks are hidden while you type, and a toggle reveals them — `docs/goal/ui/conversations.md` § Compose-field inline markdown styling
   - `tests/e2e-unified/tests/test_conversations_marker_toggle.py::test_compose_hides_inline_markers_by_default_and_toggle_reveals`
3. [app] Moving the caret into formatted text brings its marks back so you can edit them — `docs/goal/ui/conversations.md` § Compose-field inline markdown styling
   - `tests/e2e-unified/tests/test_compose_live_styling.py::test_moving_the_caret_into_formatted_text_brings_its_marks_back`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_compose_live_styling.py::test_compose_styles_markdown_as_you_type` | linux (linux): passed, tui (linux): skipped |
| 2 | app | `tests/e2e-unified/tests/test_conversations_marker_toggle.py::test_compose_hides_inline_markers_by_default_and_toggle_reveals` | linux (linux): passed, tui (linux): skipped |
| 3 | app | `tests/e2e-unified/tests/test_compose_live_styling.py::test_moving_the_caret_into_formatted_text_brings_its_marks_back` | linux (linux): passed, tui (linux): skipped |
<!-- features-render:end -->
