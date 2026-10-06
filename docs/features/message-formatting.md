---
slug: message-formatting
title: Format your messages
section: everyday
goal: docs/goal/ui/conversations.md § Compose-field inline markdown styling
guide: docs/guides/app-tour.md § Conversations
---

## What a user gets

Bold, italic, code, links, headings and lists from a toolbar while you type, offered
as far as the conversation's network can carry them. The conversation list shows plain
text, and the message itself renders formatted.

## Coverage contract

Stamped 2026-09-19 at e9152e3330.

1. [app] The toolbar wraps your selection in italic, code, link, heading and list formatting — `docs/goal/ui/conversations.md` § Compose-field inline markdown styling
   - `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_italic_button_wraps_selection`
   - `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_code_button_wraps_selection`
   - `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_link_button_wraps_selection`
   - `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_heading_button_wraps_selection`
   - `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_list_button_wraps_selection`
   - `tests/e2e-unified/tests/test_messaging.py::test_markdown_toolbar_visible`
2. [app] The conversation list previews plain text; the open message renders formatted — `docs/goal/ui/conversations.md` § Layout & flow
   - `tests/e2e-unified/tests/test_conversations_markdown.py::test_list_snippet_is_plaintext_not_markdown`
   - `tests/e2e-unified/tests/test_conversations_markdown.py::test_detail_renders_markdown_on_select`
3. [app] The formatting a conversation offers follows what its network can carry — `docs/goal/ui/conversations.md` § State & data shape
   - `tests/e2e-unified/tests/test_capability_gating.py::test_fauna_oneonone_enables_all_compose_affordances`
   - `tests/e2e-unified/tests/test_capability_gating.py::test_smtp_enables_markdown`
   - `tests/e2e-unified/tests/test_capability_gating.py::test_bridged_thread_disables_attachment_and_topic`
4. [app] The bold button wraps your selection in bold — `docs/goal/ui/conversations.md` § Compose-field inline markdown styling
   - `tests/e2e-unified/tests/test_compose_markdown_styling.py::test_compose_richeditbox_value_and_toolbar`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_italic_button_wraps_selection` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_code_button_wraps_selection` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_link_button_wraps_selection` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_heading_button_wraps_selection` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_conversations_markdown_toolbar_wrap.py::test_list_button_wraps_selection` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_messaging.py::test_markdown_toolbar_visible` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_markdown.py::test_list_snippet_is_plaintext_not_markdown` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_conversations_markdown.py::test_detail_renders_markdown_on_select` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_capability_gating.py::test_fauna_oneonone_enables_all_compose_affordances` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_capability_gating.py::test_smtp_enables_markdown` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_capability_gating.py::test_bridged_thread_disables_attachment_and_topic` | linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_compose_markdown_styling.py::test_compose_richeditbox_value_and_toolbar` | linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
