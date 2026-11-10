---
slug: feed-delete-own-post
title: Delete your own post
section: everyday
goal: docs/goal/ui/feed.md § Post deletion
guide: docs/guides/app-tour.md § Feeds
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Your own posts have a delete action behind their menu, with a confirmation. It
is gone from the feed once you confirm; other people's posts offer no such action.

## Coverage contract

Stamped 2026-11-01 at a6047fbd8b.

1. [app] Deleting one of your posts, with confirmation, removes it from the feed — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/test_feed_post_delete.py::test_delete_own_post_removes_it_from_the_feed`
2. [app] Someone else's post offers no delete — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/test_feed_post_delete.py::test_delete_affordance_absent_on_another_authors_post`
3. [nest] Only a post's author can delete it — anyone else's attempt is refused and the post stays — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_only_the_author_can_delete_a_post`
4. [nest] A deleted post is gone, not hidden — no feed returns it any more and it can no longer be opened — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_a_deleted_post_is_gone_for_every_reader`
5. [nest] Deleting a reply, repost or quote of yours takes its count back off the post it referenced — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_deleting_a_reference_post_reverses_the_targets_count`
6. [nest] Deleting a post you had published as a web page takes that page down with it — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_deleting_a_published_post_takes_its_web_page_down`
7. [app] Deleting your post leaves other people's replies and quotes of it standing; where they showed your post they show that it is no longer there — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/test_feed_post_delete.py::test_deleting_your_post_leaves_replies_and_quotes_standing_and_says_it_is_gone`
8. [nest] A post you deleted that somebody re-sends to your nest is not stored again and not passed on to other nests again — `docs/goal/ui/feed.md` § Post deletion
   - `tests/e2e-unified/tests/api/test_post_forwarding.py::test_a_deleted_post_replayed_by_a_peer_nest_is_not_stored_again`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+962dc452 standalone |
| linux | ✅ full | 0.1.5-dev+962dc452 standalone |
| windows | ✅ full | 0.1.2-dev+83a219a8 standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+962dc452 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+962dc452 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_post_delete.py::test_delete_own_post_removes_it_from_the_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_post_delete.py::test_delete_affordance_absent_on_another_authors_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_only_the_author_can_delete_a_post` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_a_deleted_post_is_gone_for_every_reader` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_deleting_a_reference_post_reverses_the_targets_count` | nest (linux): passed, nest (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_post_delete_nest.py::test_deleting_a_published_post_takes_its_web_page_down` | nest (linux): passed, nest (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed_post_delete.py::test_deleting_your_post_leaves_replies_and_quotes_standing_and_says_it_is_gone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_post_forwarding.py::test_a_deleted_post_replayed_by_a_peer_nest_is_not_stored_again` | nest (linux): passed, nest (macos): passed |
<!-- features-render:end -->
