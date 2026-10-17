---
slug: room-restricted-posts
title: Post to a room
section: everyday
goal: docs/goal/behavior/restricted-posts.md § Encryption at rest
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

When you write a post, the audience picker lists the group conversations you
are in. Pick one and your post is still yours — on your profile, in the feeds
of people who follow you — but its full text and photos open only for that
room's members; everyone else sees the teaser you wrote and a locked card. A
member just opens the post to read it, and sees which room it was for.

## Coverage contract

Stamped 2026-10-13 at f8d0f9ac76.

1. [app] When you write a post, your group rooms are offered as its audience, and a post you address to one is sealed under that room's key, so its members open it and someone who was never in the room cannot — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
2. [app] A member of the room sees which room the post was for, by the name the room has in their own conversation list, and opens it to read the full post — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
3. [app] Someone who was never in the room sees only the teaser and a locked badge, not what you wrote — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
4. [app] A room post's photos open for the room's members together with its words, and for nobody else — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_posts_photo_rests_sealed_and_opens_for_a_member`
5. [app] A room post is an ordinary post of yours — it appears on your profile and in your followers' feeds, not as a message inside the room — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
6. [app] Someone who joins a room that does not share its history cannot read the room posts written before they joined, and someone removed from a room reads none written after — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_newcomer_to_a_room_that_shares_no_history_opens_no_earlier_room_post`
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_opens_no_room_post_written_after`
7. [app] The labels a room's own moderation applies to a room post show on its card for the room's members, and for nobody else — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_rooms_labels_show_on_a_room_posts_card_for_its_members_only`
8. [app] A room post never goes through the room: nobody in the room is sent it as a message there, and a member who does not follow you comes across it only the way they come across your other posts — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_post_is_never_a_message_in_the_room_and_reaches_a_member_like_any_post`
9. [app] Only group rooms whose members hold the room's keys are offered as an audience, never a one-to-one conversation and never a group carried over another network — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
10. [app] A room you have been removed from is no longer offered as an audience — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room`
11. [app] If you lose your place in a room, its room posts in your feed stop naming the room and show as locked again — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room`
12. [app] Someone removed from an end-to-end room can still open the room posts written while they were a member — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room`
13. [nest] A room post never appears in the public feed or in Trending — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_trending_federation.py::test_a_room_post_never_trends_nor_crosses_the_exchange`
14. [nest] A room post is never published to another network — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_room_post_is_never_published_to_nostr`
15. [nest] No nest ever serves a room post's words in the clear: what your nest and your followers' nests hand out is the teaser and the sealed body — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_no_door_serves_a_room_posts_words_in_the_clear`
16. [nest] Searching posts finds a room post by its teaser only, never by the words sealed for the room — `docs/goal/ui/search.md` § The page's wire surface (what actually serves search)
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_post_search_finds_a_room_post_by_its_teaser_only`
17. [nest] In a community room, the room's own search finds room posts by their words for its members — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_community_rooms_search_finds_a_room_post_by_its_words`
18. [nest] Deleting a room post takes it out of the room's search and drops the labels the room gave it — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_deleting_a_room_post_takes_it_out_of_the_rooms_search_and_drops_its_labels`
19. [nest] When a community room's owner takes back the nest's read of the room, the room's search stops finding its room posts and their labels are gone — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_taking_back_the_nests_read_ends_the_rooms_search_and_labels_for_its_posts`
20. [nest] While a moderation flag withholds a room post, the room's search does not find it and its labels are not served — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_moderation_flag_withholds_a_room_posts_search_hit_and_labels`
21. [app] A room member whose account is on another nest sees the room's labels on a room post too — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
22. [app] A room post's photo is sealed before it is uploaded, so no readable copy of it reaches any nest — `docs/goal/ui/media.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_posts_photo_rests_sealed_and_opens_for_a_member`
23. [app] Attaching a photo and choosing the room afterwards still seals the photo for the room — the audience in force when you post decides its seal, never the one selected when the file was picked — so a member opens the picture and an outsider cannot — `docs/goal/ui/media.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_photo_attached_before_the_room_is_chosen_is_still_sealed_for_the_room`
24. [app] A repost of a room post shows everyone outside the room only its teaser and the locked badge — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_repost_of_a_room_post_shows_an_outsider_the_teaser_and_the_lock`
25. [nest] A member who joined a community room without access to its earlier history is never shown, by the room's search, a room post written before they joined — `docs/goal/behavior/community-rooms.md` § Implementation status today
    - `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_joiner_without_the_rooms_history_never_finds_an_earlier_room_post`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.4-dev+899b58ab.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_posts_photo_rests_sealed_and_opens_for_a_member` | linux (linux): skipped, tui (linux): passed |
| 5 | app | (none) | — |
| 6 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_newcomer_to_a_room_that_shares_no_history_opens_no_earlier_room_post` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_opens_no_room_post_written_after` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_rooms_labels_show_on_a_room_posts_card_for_its_members_only` | tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_post_is_never_a_message_in_the_room_and_reaches_a_member_like_any_post` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room` | linux (linux): skipped, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room` | linux (linux): skipped, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_removed_from_the_room_keeps_its_earlier_posts_and_loses_the_room` | linux (linux): skipped, tui (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_trending_federation.py::test_a_room_post_never_trends_nor_crosses_the_exchange` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_room_post_is_never_published_to_nostr` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_no_door_serves_a_room_posts_words_in_the_clear` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_post_search_finds_a_room_post_by_its_teaser_only` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_community_rooms_search_finds_a_room_post_by_its_words` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_deleting_a_room_post_takes_it_out_of_the_rooms_search_and_drops_its_labels` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_taking_back_the_nests_read_ends_the_rooms_search_and_labels_for_its_posts` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_moderation_flag_withholds_a_room_posts_search_hit_and_labels` | nest (linux): passed |
| 21 | app | (none) | — |
| 22 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_room_posts_photo_rests_sealed_and_opens_for_a_member` | linux (linux): skipped, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_photo_attached_before_the_room_is_chosen_is_still_sealed_for_the_room` | tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_repost_of_a_room_post_shows_an_outsider_the_teaser_and_the_lock` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_room_post_nest_promises.py::test_a_joiner_without_the_rooms_history_never_finds_an_earlier_room_post` | nest (linux): passed |
<!-- features-render:end -->
