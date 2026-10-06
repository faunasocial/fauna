---
slug: feed-interactions
title: Like, reply, repost and quote
section: everyday
goal: docs/goal/ui/feed.md § Interaction bar
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Every post has the four buttons: like, reply, repost and quote, each with its
count. Like and repost toggle back off; a reply or a quote is a post of your own,
and the counts on the original move as you act.

## Coverage contract

Stamped 2026-09-19 at 23b3189e2b.

1. [app] Liking moves that post's count and a second tap takes it back — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_like_moves_its_own_count`
   - `tests/e2e-unified/tests/test_feed.py::test_like_toggle_records_and_reverses_the_callers_like`
2. [app] Replying creates a reply carrying your text and moves the reply count — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_reply_creates_a_post_and_moves_its_own_count`
3. [app] Reposting creates your repost and a second tap removes it — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_repost_toggle_creates_and_removes_the_callers_repost`
4. [app] Quoting creates a quote post and moves the quote count; the button is on every post — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_quote_creates_a_post_and_moves_its_own_count`
   - `tests/e2e-unified/tests/test_feed.py::test_feed_quote_button_present`
   - `tests/e2e-unified/tests/test_feed.py::test_interaction_bar_has_quote_and_all_buttons`
5. [nest] Your nest counts each person once and never double-counts a repeated tap — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/api/test_engagement_counts.py::test_like_increments_like_count_idempotently`
   - `tests/e2e-unified/tests/api/test_engagement_counts.py::test_two_actors_each_count_once`
   - `tests/e2e-unified/tests/api/test_engagement_counts.py::test_reference_post_increments_target_count_idempotently`
   - `tests/e2e-unified/tests/api/test_unified_interact.py::test_fauna_post_like_returns_ok`
6. [app] A repost in your feed says who reposted it and shows the original post inside it, and opening it takes you to the original — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_a_repost_names_the_reposter_shows_the_original_and_opens_it`
7. [app] Each of the four buttons carries its count, and shows no number until the post has activity — `docs/goal/ui/feed.md` § Interaction bar
   - `tests/e2e-unified/tests/test_feed.py::test_a_count_shows_no_number_until_the_post_has_activity`
8. [app] Replying or quoting with your own words under a restricted post you cannot write for — someone else's paid post, a room you are not in — is refused with the reason and nothing is posted; a plain repost or bare quote of it still works — `docs/goal/ui/feed.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_feed_restricted_reference.py::test_words_under_a_restricted_post_are_refused_and_a_bare_repost_or_quote_still_works`
9. [app] A reply under a post for a room you are in, or under your own paid post, goes to that same audience: everyone sees that you replied, only that audience can open what you wrote — `docs/goal/ui/feed.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_reply.py::test_a_members_reply_to_a_room_post_opens_for_the_room_and_nobody_else`
10. [app] Replying under a restricted post you cannot write for goes public only after you tick "Post my reply publicly" in the reply box, which first tells you the reply would be public; left unticked, the reply is refused with the reason — `docs/goal/ui/feed.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_reply.py::test_an_outsiders_reply_to_a_room_post_is_refused_until_confirmed_public`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+3677c3d4 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+e82b932f.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_like_moves_its_own_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_like_toggle_records_and_reverses_the_callers_like` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_reply_creates_a_post_and_moves_its_own_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed.py::test_repost_toggle_creates_and_removes_the_callers_repost` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed.py::test_quote_creates_a_post_and_moves_its_own_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed.py::test_feed_quote_button_present` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed.py::test_interaction_bar_has_quote_and_all_buttons` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_engagement_counts.py::test_like_increments_like_count_idempotently` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_engagement_counts.py::test_two_actors_each_count_once` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_engagement_counts.py::test_reference_post_increments_target_count_idempotently` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_unified_interact.py::test_fauna_post_like_returns_ok` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed.py::test_a_repost_names_the_reposter_shows_the_original_and_opens_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed.py::test_a_count_shows_no_number_until_the_post_has_activity` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_feed_restricted_reference.py::test_words_under_a_restricted_post_are_refused_and_a_bare_repost_or_quote_still_works` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_room_restricted_reply.py::test_a_members_reply_to_a_room_post_opens_for_the_room_and_nobody_else` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_room_restricted_reply.py::test_an_outsiders_reply_to_a_room_post_is_refused_until_confirmed_public` | tui (linux): passed |
<!-- features-render:end -->
