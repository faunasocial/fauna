---
slug: feed-read
title: Read your feed
section: everyday
goal: docs/goal/ui/feed.md § Layout & flow
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Your feed shows posts newest first, formatted the way they were written, and
opening a post shows it in full. If your connection drops and comes back, the feed
catches up on its own; nothing waits for you to pull.

## Coverage contract

Stamped 2026-10-01 at d8cb0887cb.

1. [app] Posts appear newest first, and the text is shown formatted rather than as raw markup — `docs/goal/ui/feed.md` § Layout & flow
   - `tests/e2e-unified/tests/test_feed.py::test_post_ordering_newest_first`
   - `tests/e2e-unified/tests/test_feed.py::test_post_body_renders_markdown`
   - `tests/e2e-unified/tests/test_sp_authenticated.py::test_feed_visible`
2. [app] Opening a post shows its author and full text — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed.py::test_post_detail_opens`
3. [app] A post published while you were disconnected appears after you reconnect, with no manual refresh — `docs/goal/architecture/transport.md` § Push events and `seq` numbering
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate`
4. [nest] Your nest serves your posts and keeps their order — `docs/goal/ui/feed.md` § The read model
   - `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_post_and_query`
5. [app] A feed with no posts in it says so, instead of showing a bare list — `docs/goal/ui/feed.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_feed_with_no_posts_says_so`
6. [app] Refreshing the feed you are on keeps its posts on screen until the new ones land; only switching to another feed clears the list — `docs/goal/ui/feed.md` § The read model
   - `tests/e2e-unified/tests/test_feed.py::test_a_refresh_keeps_the_posts_on_screen_and_only_a_switch_clears_them`
7. [app] A feed that cannot be loaded says so on the page, instead of just looking empty — `docs/goal/ui/feed.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_error.py::test_feed_error_surfaces_on_error_text`
8. [app] A post that came from another network shows its author by their name or handle there, never a raw id, and a nickname you gave them comes first — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - (none)
9. [nest] A post from another network arrives with its author's name, handle and picture, the picture served through your nest — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubInboundIngest::test_an_ingested_notes_author_carries_the_bridged_face`
10. [app] Blocking someone removes their posts from your own feeds, and unblocking brings them back — `docs/goal/behavior/moderation.md` § Corollary — block also hides
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_blocking_an_author_hides_their_posts_until_unblocked`
11. [app] Content removed under a legal obligation shows a notice in its place, in a quoted post, an opened post and a conversation, never a blank or an error — `docs/goal/behavior/moderation.md` § Legal takedown
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+d034eec4.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+4e31f706 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_ordering_newest_first` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_body_renders_markdown` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_sp_authenticated.py::test_feed_visible` | web (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_detail_opens` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_feed_rehydrate` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_post_and_query` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed_empty_state.py::test_a_feed_with_no_posts_says_so` | linux (linux): skipped, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed.py::test_a_refresh_keeps_the_posts_on_screen_and_only_a_switch_clears_them` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed_error.py::test_feed_error_surfaces_on_error_text` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubInboundIngest::test_an_ingested_notes_author_carries_the_bridged_face` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_blocking_an_author_hides_their_posts_until_unblocked` | tui (linux): passed |
| 11 | app | (none) | — |
<!-- features-render:end -->
