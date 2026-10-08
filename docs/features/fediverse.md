---
slug: fediverse
title: The fediverse
section: bridges and other networks
goal: docs/goal/behavior/activitypub.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § The Fediverse (Mastodon & friends)
---

## What a user gets

Turn federation on and you have a fediverse address. People on Mastodon and
its relatives follow you and get your posts, their favourites and boosts show on your
posts, deleting a post takes it back from them, and you follow accounts there and
read their posts in your feed. Your reply to a fediverse post, or your quote of
one, reaches its author.

## Coverage contract

Stamped 2026-10-08 at f7e52bc137.

1. [app] Federation turns on from the app and your address resolves from the outside — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - `tests/e2e-unified/tests/test_activitypub_live.py::test_activitypub_live_serving_surface`
2. [app] A post travels from a private home nest through its public relay to a real fediverse server — `docs/goal/behavior/activitypub.md` § The produce direction
   - `tests/e2e-unified/tests/live/test_activitypub_federation_live.py::test_activitypub_federation_live_three_party`
3. [app] Replying or quoting a fediverse post — `docs/goal/behavior/activitypub.md` § Implementation status today
   - `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_a_reply_to_a_fediverse_post_reaches_its_author`
   - `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_a_quote_of_a_fediverse_post_reaches_its_author`
4. [nest] A follow from the fediverse is accepted, your posts reach the follower, and a delete takes a post back — `docs/goal/behavior/activitypub.md` § The produce direction
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_follow_is_auto_accepted_and_delivered`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_post_is_pushed_to_follower_then_chased_by_delete`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_webfinger_discovery`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_actor_endpoint`
5. [nest] A real Mastodon-compatible server follows you, shows your post, sees your delete, and its favourites, boosts and posts reach you — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f2_follow_is_auto_accepted`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f3_post_reaches_follower_timeline`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f4_delete_removes_status_from_timeline`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f5_peer_reply_is_ingested`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f6_peer_favourite_mints_a_synthetic_upvote`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f7_peer_boost_mints_a_synthetic_repost`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f9_followed_peer_account_posts_are_ingested`
   - `tests/e2e-unified/tests/platform/fediverse/test_authorized_fetch.py::test_follow_is_accepted_under_authorized_fetch`
6. [nest] Your reply to a fediverse post reaches its author and joins the thread on their server, and your quote arrives there carrying the link to what you quoted — `docs/goal/behavior/activitypub.md` § Reply and quote
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_a_reply_or_quote_of_an_ingested_note_reaches_its_author`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f10_our_reply_joins_the_peer_status_thread`
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f11_our_quote_arrives_with_its_re_link`
7. [nest] Your fediverse address is your Fauna handle at your nest's domain — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_enable_derives_username_from_handle`
8. [nest] Changing your Fauna handle later leaves your fediverse address as it was — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
9. [nest] Fediverse servers see your profile's name, picture and bio on your account there — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
10. [nest] Fediverse servers can see how many followers you have, never who they are — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
11. [nest] The fediverse options you choose are kept by your nest: whether follows are accepted by themselves, your default visibility, whether your history is published — `docs/goal/behavior/activitypub.md` § Goal
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_update_settings`
12. [nest] With follows no longer accepted by themselves, a fediverse follow is not accepted and that follower receives none of your posts — `docs/goal/behavior/activitypub.md` § Goal
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_no_push_to_a_follower_that_never_got_accepted`
13. [nest] The default visibility you choose decides how your posts are addressed on your followers' servers: public, unlisted or followers only — `docs/goal/behavior/activitypub.md` § The produce direction
   - (none)
14. [nest] With publishing your history off, a fediverse server reading your account finds none of your earlier posts — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_outbox_empty_without_backfill`
15. [nest] With publishing your history on, your earlier public posts can be read from your fediverse account — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
16. [nest] A post you gated or sold never reaches the fediverse: it is neither delivered to followers nor listed in your history there — `docs/goal/behavior/activitypub.md` § The produce direction
   - (none)
17. [nest] Photos on your posts arrive as attachments on your followers' servers — `docs/goal/behavior/activitypub.md` § Reply and quote
   - (none)
18. [nest] Liking or boosting a fediverse post in your feed sends the favourite or boost to its author's server — `docs/goal/behavior/activitypub.md` § Goal
   - (none)
19. [nest] Unfollowing a fediverse account tells its server you no longer follow it — `docs/goal/behavior/activitypub.md` § Implementation status today
   - (none)
20. [nest] A fediverse reply to your post shows under it as a reply and moves its reply count — `docs/goal/behavior/activitypub.md` § Reply and quote
   - (none)
21. [nest] When someone on the fediverse takes back a favourite or boost of your post, it comes off your post — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f8_undo_retracts_both_synthetic_reactions`
22. [nest] A fediverse post its author deletes leaves your feed — `docs/goal/behavior/activitypub.md` § Post deletion
   - (none)
23. [nest] A fediverse post its author edits shows the edited version in your feed — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
24. [nest] A fediverse post that was not public, a direct or followers-only one, never appears in your feed — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
25. [nest] Your feed takes in fediverse posts only from accounts you follow, or addressed to you — `docs/goal/behavior/activitypub.md` § Architecture (current, shipped)
   - (none)
26. [nest] Deleting a post after you turned federation off still takes it back from the followers who received it — `docs/goal/behavior/activitypub.md` § Post deletion
   - (none)
27. [app] Replying to or quoting a fediverse post while your federation is off is refused with the reason, and nothing is posted — `docs/goal/behavior/activitypub.md` § Reply and quote
   - `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_replying_or_quoting_with_federation_off_is_refused_inline`
28. [app] With follows no longer accepted by themselves, each fediverse follow request shows on the network's card, and you approve or refuse it there — `docs/goal/behavior/bridges.md` § Follow requests
   - (none)
29. [nest] A follower you approve starts receiving your posts, and one you refuse is told no and receives nothing — `docs/goal/behavior/activitypub.md` § Follow requests
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_approving_a_request_sends_accept_and_later_posts_reach_it`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_refusing_a_request_sends_reject_and_later_posts_do_not_reach_it`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_a_held_back_follow_is_listed_as_a_request`
30. [nest] Turning follows back to accepted by themselves accepts the requests that were waiting — `docs/goal/behavior/activitypub.md` § Follow requests
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_turning_auto_accept_back_on_accepts_the_waiting_requests`
31. [nest] Fediverse servers show your account as one that approves its followers exactly when you have turned accepting by itself off — `docs/goal/behavior/activitypub.md` § Follow requests
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_actor_document_says_whether_followers_are_approved_by_hand`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ⚠ partial | |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_activitypub_live.py::test_activitypub_live_serving_surface` | linux (linux): passed |
| 2 | app | `tests/e2e-unified/tests/live/test_activitypub_federation_live.py::test_activitypub_federation_live_three_party` | linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_a_reply_to_a_fediverse_post_reaches_its_author` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_a_quote_of_a_fediverse_post_reaches_its_author` | tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_follow_is_auto_accepted_and_delivered` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_post_is_pushed_to_follower_then_chased_by_delete` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_webfinger_discovery` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_actor_endpoint` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f2_follow_is_auto_accepted` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f3_post_reaches_follower_timeline` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropProduce::test_f4_delete_removes_status_from_timeline` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f5_peer_reply_is_ingested` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f6_peer_favourite_mints_a_synthetic_upvote` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f7_peer_boost_mints_a_synthetic_repost` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f9_followed_peer_account_posts_are_ingested` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/fediverse/test_authorized_fetch.py::test_follow_is_accepted_under_authorized_fetch` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_a_reply_or_quote_of_an_ingested_note_reaches_its_author` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f10_our_reply_joins_the_peer_status_thread` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f11_our_quote_arrives_with_its_re_link` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_enable_derives_username_from_handle` | nest (linux): passed |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_update_settings` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_no_push_to_a_follower_that_never_got_accepted` | nest (linux): passed |
| 13 | nest | (none) | — |
| 14 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_outbox_empty_without_backfill` | nest (linux): passed |
| 15 | nest | (none) | — |
| 16 | nest | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | nest | `tests/e2e-unified/tests/platform/fediverse/test_interop.py::TestFediverseInteropConsume::test_f8_undo_retracts_both_synthetic_reactions` | nest (linux): passed |
| 22 | nest | (none) | — |
| 23 | nest | (none) | — |
| 24 | nest | (none) | — |
| 25 | nest | (none) | — |
| 26 | nest | (none) | — |
| 27 | app | `tests/e2e-unified/tests/test_feed_fediverse_reply.py::test_replying_or_quoting_with_federation_off_is_refused_inline` | tui (linux): failed |
| 28 | app | (none) | — |
| 29 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_approving_a_request_sends_accept_and_later_posts_reach_it` | nest (linux): passed |
| 29 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_refusing_a_request_sends_reject_and_later_posts_do_not_reach_it` | nest (linux): passed |
| 29 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_a_held_back_follow_is_listed_as_a_request` | nest (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFollowerDelivery::test_turning_auto_accept_back_on_accepts_the_waiting_requests` | nest (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_actor_document_says_whether_followers_are_approved_by_hand` | nest (linux): passed |
<!-- features-render:end -->
