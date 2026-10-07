---
slug: feed-search
title: Search within a feed
section: everyday
goal: docs/goal/ui/feed.md § Where logic lives
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Type in the search box above a feed and it narrows to matching posts by text or
tag; clear it and the full feed is back.

## Coverage contract

Stamped 2026-09-19 at 23b3189e2b.

1. [app] Searching narrows the feed by text or tag, and clearing restores it — `docs/goal/ui/feed.md` § Where logic lives
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_filters_posts`
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_by_tag`
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_clear_restores_feed`
2. [nest] Your nest narrows any feed, built-in or custom, by the search text — `docs/goal/ui/feed.md` § Where logic lives
   - `tests/e2e-unified/tests/api/test_feed_search_api.py::test_local_feed_search_narrows_by_body`
   - `tests/e2e-unified/tests/api/test_feed_search_api.py::test_custom_any_feed_search_narrows_by_body`
3. [app] A search that matches no posts says so, instead of leaving a blank list — `docs/goal/ui/feed.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_search_with_no_matches_says_so`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+9bf9020c standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ✅ full | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+f872d502 live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_filters_posts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_by_tag` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_clear_restores_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_feed_search_api.py::test_local_feed_search_narrows_by_body` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_feed_search_api.py::test_custom_any_feed_search_narrows_by_body` | nest (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_search_with_no_matches_says_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
