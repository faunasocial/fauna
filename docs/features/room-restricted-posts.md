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

Stamped 2026-10-01 at dfa27a899f.

1. [app] When you write a post, your group rooms are offered as its audience, and a post you address to one is sealed under that room's key, so its members open it and someone who was never in the room cannot — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
2. [app] A member of the room sees which room the post was for, by the name the room has in their own conversation list, and opens it to read the full post — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
3. [app] Someone who was never in the room sees only the teaser and a locked badge, not what you wrote — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked`
4. [app] A room post's photos open for the room's members together with its words, and for nobody else — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
5. [app] A room post is an ordinary post of yours — it appears on your profile and in your followers' feeds, not as a message inside the room — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
6. [app] Someone who joins a room that does not share its history cannot read the room posts written before they joined, and someone removed from a room reads none written after — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
7. [app] The labels a room's own moderation applies to a room post show on its card for the room's members, and for nobody else — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
8. [app] A room member who does not follow you does not get your room post in their feed: it goes to your followers, not to the room — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
9. [app] Only group rooms whose members hold the room's keys are offered as an audience, never a one-to-one conversation and never a group carried over another network — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
   - (none)
10. [app] A room you have been removed from is no longer offered as an audience — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
11. [app] If you lose your place in a room, its room posts in your feed stop naming the room and show as locked again — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
12. [app] Someone removed from an end-to-end room can still open the room posts written while they were a member — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
13. [nest] A room post never appears in the public feed or in Trending — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
14. [nest] A room post is never published to another network — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
15. [nest] No nest ever serves a room post's words in the clear: what your nest and your followers' nests hand out is the teaser and the sealed body — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
16. [nest] Searching posts finds a room post by its teaser only, never by the words sealed for the room — `docs/goal/ui/search.md` § The page's wire surface (what actually serves search)
    - (none)
17. [nest] In a community room, the room's own search finds room posts by their words for its members — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
18. [nest] Deleting a room post takes it out of the room's search and drops the labels the room gave it — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
19. [nest] When a community room's owner takes back the nest's read of the room, the room's search stops finding its room posts and their labels are gone — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
20. [nest] While a moderation flag withholds a room post, the room's search does not find it and its labels are not served — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
21. [app] A room member whose account is on another nest sees the room's labels on a room post too — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
22. [app] A room post's photo is sealed before it is uploaded, so no readable copy of it reaches any nest — `docs/goal/ui/media.md` § Encryption at rest
    - (none)
23. [app] Changing a post's audience after its photo was sealed for another one refuses the post, rather than publishing a photo nobody can open — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
24. [app] A repost of a room post shows everyone outside the room only its teaser and the locked badge — `docs/goal/behavior/restricted-posts.md` § Encryption at rest
    - (none)
25. [nest] A member who joined a community room without access to its earlier history is never shown, by the room's search, a room post written before they joined — `docs/goal/behavior/community-rooms.md` § Implementation status today
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_room_restricted_post.py::test_a_member_opens_a_room_post_and_an_outsider_sees_it_locked` | linux (linux): passed, tui (linux): passed |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | nest | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | nest | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | app | (none) | — |
| 22 | app | (none) | — |
| 23 | app | (none) | — |
| 24 | app | (none) | — |
| 25 | nest | (none) | — |
<!-- features-render:end -->
