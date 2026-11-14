---
slug: feed-compose
title: Write a post
section: everyday
goal: docs/goal/ui/feed.md § User actions
guide: docs/guides/app-tour.md § Feeds
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Write a post from the bar at the top of your feed or open the full editor for
longer pieces. Tags become chips, files attach from the composer, and what you
publish shows up in your feed at once.

## Coverage contract

Stamped 2026-11-01 at a6047fbd8b.

1. [app] A post you write appears in your feed at once, and several in a row each appear — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed.py::test_create_post`
   - `tests/e2e-unified/tests/test_feed.py::test_create_multiple_posts`
   - `tests/e2e-unified/tests/test_sp_authenticated.py::test_create_post`
2. [app] Tags on a post show as chips — `docs/goal/ui/feed.md` § Post content types
   - `tests/e2e-unified/tests/test_feed.py::test_post_with_tags`
3. [app] The full editor opens from the compose bar — `docs/goal/ui/feed.md` § Layout & flow
   - `tests/e2e-unified/tests/test_feed_compose_dialog.py::test_compose_dialog_button_opens_feed_compose_dialog`
4. [app] The composer offers a way to attach a file — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed.py::test_compose_attach_affordance_offered`
5. [app] Text typed while a post is still being sent survives the submit — only what was actually sent is cleared — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_text_typed_while_a_post_sends_survives_the_submit`
6. [app] The audience picked for a post stays picked while you keep typing — it never silently falls back to Public — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_the_audience_picked_for_a_post_stays_picked_while_you_keep_typing`
7. [app] A post that fails to send says so in the composer and keeps what you wrote, so you can try again — `docs/goal/ui/feed.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_a_post_that_fails_to_send_says_so_and_keeps_what_you_wrote`
8. [nest] Your nest stores a post only from the connection of the person who wrote it: a post uploaded by anyone else is refused and nothing of it is kept — `docs/goal/ui/feed.md` § Post creation
   - `tests/e2e-unified/tests/api/test_post_create_author_only_nest.py::test_a_post_uploaded_by_anyone_but_its_author_is_refused_and_not_kept`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+646e0070 standalone |
| linux | ✅ full | 0.1.5-dev+cbd813be standalone |
| windows | ✅ full | 0.1.3-dev+a6f1524b standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.6 docker |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+390d61b5 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_create_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_create_multiple_posts` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_sp_authenticated.py::test_create_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_with_tags` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_compose_dialog.py::test_compose_dialog_button_opens_feed_compose_dialog` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed.py::test_compose_attach_affordance_offered` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_text_typed_while_a_post_sends_survives_the_submit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_the_audience_picked_for_a_post_stays_picked_while_you_keep_typing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed_compose_in_flight.py::test_a_post_that_fails_to_send_says_so_and_keeps_what_you_wrote` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_post_create_author_only_nest.py::test_a_post_uploaded_by_anyone_but_its_author_is_refused_and_not_kept` | nest (linux): passed |
<!-- features-render:end -->
