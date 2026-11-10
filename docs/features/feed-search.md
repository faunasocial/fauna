---
slug: feed-search
title: Search within a feed
section: everyday
goal: docs/goal/ui/feed.md § Where logic lives
guide: docs/guides/app-tour.md § Feeds
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Type in the search box above a feed and it narrows to matching posts by text or
tag; clear it and the full feed is back.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] Searching narrows the feed by text or tag, and clearing restores it — `docs/goal/ui/feed.md` § Where logic lives
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_filters_posts`
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_by_tag`
   - `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_clear_restores_feed`
2. [nest] Your nest narrows any feed, built-in or custom, by the search text — `docs/goal/ui/feed.md` § Where logic lives
   - `tests/e2e-unified/tests/api/test_feed_search_api.py::test_local_feed_search_narrows_by_body`
   - `tests/e2e-unified/tests/api/test_feed_search_api.py::test_custom_any_feed_search_narrows_by_body`
3. [app] A search that matches no posts says so, instead of leaving a blank list — `docs/goal/ui/feed.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_search_with_no_matches_says_so`
4. [nest] Search, and a custom feed's text rule, match posts from Bluesky, the fediverse and Nostr by their words, and stop matching them once that bridge is turned off — `docs/goal/architecture/feed-read-model.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_fediverse_post_is_found_by_its_words_until_the_bridge_is_off_for_search`
   - `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_bluesky_post_is_found_by_its_words_until_the_bridge_is_off_for_search`
   - `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_nostr_post_is_found_by_its_words_until_the_bridge_is_off_for_search`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+2e8014b4 standalone |
| linux | ✅ full | 0.1.5-dev+00668dad standalone |
| windows | ✅ full | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.6-dev+aa0b8e99 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+aa0b8e99 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_filters_posts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_by_tag` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_search.py::test_feed_search_clear_restores_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_feed_search_api.py::test_local_feed_search_narrows_by_body` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_feed_search_api.py::test_custom_any_feed_search_narrows_by_body` | nest (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_search_with_no_matches_says_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_fediverse_post_is_found_by_its_words_until_the_bridge_is_off_for_search` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_bluesky_post_is_found_by_its_words_until_the_bridge_is_off_for_search` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_feed_search_bridged.py::test_a_nostr_post_is_found_by_its_words_until_the_bridge_is_off_for_search` | nest (linux): passed |
<!-- features-render:end -->
