---
slug: copy-what-you-read
title: Copy the text the app shows you
section: everyday
goal: docs/goal/ui/README.md § Text the person reads can be copied
guide: docs/guides/app-tour.md § One app, three shapes
absences:
  linux (outcome 3): "docs/goal/ui/README.md § Text the person reads can be copied"
  macos (outcome 3): "docs/goal/ui/README.md § Text the person reads can be copied"
  ios (outcome 3): "docs/goal/ui/README.md § Text the person reads can be copied"
  ios (outcome 4): "docs/goal/ui/README.md § Text the person reads can be copied"
---

## What a user gets

Text the app shows you, rather than a button you press, can be selected and
copied the way your device always does it: an error you want to search for or
pass on, an address, a fingerprint, a message or a post. Selecting text never
presses anything, so selecting words inside a row does not open it, and a
selection you are making stays put while the page refreshes around it.

## Coverage contract

Stamped 2026-11-19 at 890d27537a.

1. [app] An error or status message the app shows can be selected and copied — `docs/goal/ui/README.md` § Text the person reads can be copied
   - `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_an_error_message_the_app_shows_can_be_selected`
2. [app] A value the app shows — an address, an identifier, a fingerprint — can be selected and copied — `docs/goal/ui/README.md` § Text the person reads can be copied
   - `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_a_value_the_app_shows_can_be_selected`
3. [app] Selecting text inside a row that opens something selects it and does not open it, while a plain click still opens it — `docs/goal/ui/README.md` § Text the person reads can be copied
   - `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_selecting_text_inside_a_row_that_opens_does_not_open_it`
4. [app] A selection survives the page refreshing itself while the text is unchanged — `docs/goal/ui/README.md` § Text the person reads can be copied
   - `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_a_selection_survives_the_page_refreshing_unchanged_text`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.7-dev+bf45494c standalone |
| linux | ✅ full | 0.1.7-dev+98ac59f7 standalone |
| windows |  no run recorded | |
| macos | ✅ full | 0.1.7-dev+e42f7d34 standalone |
| ios | ✅ full | 0.1.7-dev+9743183c standalone |
| android |  no run recorded | |
| tui |  no run recorded | |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_an_error_message_the_app_shows_can_be_selected` — requires `text-selectable-read` (test harness) (not built: windows, tui) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_a_value_the_app_shows_can_be_selected` — requires `text-selectable-read` (test harness) (not built: windows, tui) | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_selecting_text_inside_a_row_that_opens_does_not_open_it` — requires `text-selectable-read` (test harness) (not built: windows, tui); `text-selection-read` (test harness) (not built: windows, tui) | web (linux): passed |
| 3 | app | absent by design on linux, macos, ios, ios-kids | — |
| 4 | app | `tests/e2e-unified/tests/test_read_only_text_is_selectable.py::test_a_selection_survives_the_page_refreshing_unchanged_text` — requires `text-selectable-read` (test harness) (not built: windows, tui); `text-selection-read` (test harness) (not built: windows, tui) | web (linux): passed, linux (linux): passed, macos (macos): passed |
| 4 | app | absent by design on ios, ios-kids | — |
<!-- features-render:end -->
