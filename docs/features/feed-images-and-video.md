---
slug: feed-images-and-video
title: Images and video in posts
section: everyday
goal: docs/goal/ui/feed.md § Post content types
guide: docs/guides/app-tour.md § Feeds
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Attach a picture or a video and it shows in the post: pictures inline with a
tap-to-enlarge view, videos as a thumbnail that plays in place when you tap it
(never on its own). Pictures carry their provenance badge when the file has one.
The hidden tags a camera writes into a photo — where it was taken, when, on what
— are removed on your own device, before the picture is ever sent. A picture
linked from somewhere else on the internet stays blocked until you choose to
load it, so the site hosting it does not learn you read the post.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

1. [app] A picture you attach shows in the post, alone or beside tags — `docs/goal/ui/feed.md` § Post content types
   - `tests/e2e-unified/tests/test_feed.py::test_post_with_image`
   - `tests/e2e-unified/tests/test_feed.py::test_post_with_image_and_tags`
   - `tests/e2e-unified/tests/test_feed.py::test_post_has_image_by_text_is_scoped_to_the_named_post`
2. [app] The picture is stored once, with a small preview served for the feed — `docs/goal/ui/feed.md` § Post content types
   - `tests/e2e-unified/tests/test_feed.py::test_post_image_uploads_via_sidecar_and_round_trips`
   - `tests/e2e-unified/tests/test_feed.py::test_post_image_thumbnail_is_uploaded_and_served`
3. [app] Tapping a picture opens it full screen — `docs/goal/ui/feed.md` § Layout & flow
   - `tests/e2e-unified/tests/test_feed_image_lightbox.py::test_post_image_click_opens_lightbox`
4. [app] A video you attach shows as a thumbnail on the post — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_thumbnail.py::test_video_attachment_renders_video_thumbnail`
5. [app] A picture with signed provenance shows its badge — `docs/goal/ui/feed.md` § Post content types
   - `tests/e2e-unified/tests/test_feed.py::test_post_image_shows_c2pa_badge_when_uploaded_with_provenance`
6. [app] A picture linked from the open internet stays blocked until you load it, then paints — `docs/goal/architecture/render-model.md` § D3 — Remote-image reveal state moves into the manager
   - `tests/e2e-unified/tests/test_feed_remote_image.py::test_body_remote_image_is_blocked_until_revealed_then_paints`
7. [app] A picture on a post is still there after the nest's storage clean-up runs — `docs/goal/behavior/backup-restore.md` § 9. Garbage Collection
   - `tests/e2e-unified/tests/test_feed.py::test_post_image_survives_a_blob_gc_sweep`
8. [app] A photo you post arrives with its location tag already gone — stripped on your device, never sent — `docs/goal/ui/media.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_feed.py::test_post_image_strips_exif_gps_before_the_nest_stores_it`
9. [app] A picture on an audience-restricted post opens for a reader entitled to the post, and never shows as scrambled bytes — `docs/goal/ui/media.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_gated_post_compose.py::test_a_restricted_post_s_picture_opens_for_an_entitled_reader`
10. [app] A picture attached to a post is uploaded when you post it, so it is still there however long you spend writing — `docs/goal/ui/media.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_feed.py::test_a_picture_is_uploaded_when_you_post_so_writing_time_never_loses_it`
11. [app] A picture whose provenance is only *claimed* shows no badge — the badge means the app checked the picture itself — `docs/goal/ui/media.md` § Encryption at rest
   - `tests/e2e-unified/tests/test_feed.py::test_a_forged_c2pa_assertion_paints_no_provenance_badge`
12. [app] Tapping a video on a post plays it, and it never starts on its own — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_video_plays_it_in_place`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_video_plays_against_a_self_signed_nest`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_video_played_to_its_end_reads_its_whole_length`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_sealed_video_plays_after_its_post_unlocks`
   - `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_ask_confirm_hands_the_feed_clip_to_the_os_player`
13. [app] A video on a post from the fediverse shows nothing from its server until you tap it, then plays through your nest — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_bridged_video_plays_it_through_the_ticketed_proxy`
   - `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_bridged_video_hands_off_through_the_nest_proxy`
14. [app] A video you are playing keeps playing when the feed refreshes or a new post arrives above it — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_playing_video_survives_a_feed_reload`
   - `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_feed_refresh_leaves_a_running_handoff_alone`
15. [app] You can leave the feed in the middle of a video and come back to it: the app carries on, and the video is there to play again — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_leaving_the_feed_while_a_video_plays_keeps_the_app_running`
   - `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_leaving_the_feed_mid_handoff_and_coming_back_opens_the_video_again`
16. [app] A post with several pictures or videos shows all of them, in the order they were attached — `docs/goal/architecture/render-model.md` § Implementation status today
   - `tests/e2e-unified/tests/test_feed_media_items.py::test_every_media_item_paints_in_order_with_its_alt`
17. [app] A picture on a post from Bluesky or the fediverse shows straight away, fetched through your nest, with no button to load it — `docs/goal/architecture/render-model.md` § Implementation status today
   - `tests/e2e-unified/tests/test_feed_bridged_image.py::test_a_bridged_posts_picture_paints_at_once_through_the_nest`
18. [app] A picture or video carries its author's own description as its label, and a generic label only when there is none — `docs/goal/architecture/render-model.md` § Implementation status today
   - `tests/e2e-unified/tests/test_feed_media_items.py::test_every_media_item_paints_in_order_with_its_alt`
19. [app] A video on an audience-restricted post plays for a reader entitled to the post once it unlocks — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_sealed_video_plays_after_its_post_unlocks`
   - `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_sealed_video_hands_off_once_its_post_unlocks`
20. [app] A picture on a post that lives on another nest, and a relayed profile's picture, load even when that nest has only a self-signed certificate — `docs/goal/architecture/transport-trust.md` § Implementation status today
   - `tests/e2e-unified/tests/test_federated_follow_cross_nest.py::test_a_reader_follows_an_author_on_another_nest_and_their_posts_reach_the_readers_feed`
   - `tests/e2e-unified/tests/test_profile_remote_read_cross_nest.py::test_a_remote_actors_profile_shows_their_name_only_once_they_open_it`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+646e0070 standalone |
| linux | ✅ full | 0.1.7-dev+c0e2aa0e standalone |
| windows | ⚠ partial | |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+79c80402 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5 docker |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_with_image` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_with_image_and_tags` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_has_image_by_text_is_scoped_to_the_named_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_uploads_via_sidecar_and_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_thumbnail_is_uploaded_and_served` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_image_lightbox.py::test_post_image_click_opens_lightbox` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_video_thumbnail.py::test_video_attachment_renders_video_thumbnail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_shows_c2pa_badge_when_uploaded_with_provenance` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed_remote_image.py::test_body_remote_image_is_blocked_until_revealed_then_paints` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_survives_a_blob_gc_sweep` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_strips_exif_gps_before_the_nest_stores_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_gated_post_compose.py::test_a_restricted_post_s_picture_opens_for_an_entitled_reader` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_feed.py::test_a_picture_is_uploaded_when_you_post_so_writing_time_never_loses_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 11 | app | `tests/e2e-unified/tests/test_feed.py::test_a_forged_c2pa_assertion_paints_no_provenance_badge` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_video_plays_it_in_place` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_video_plays_against_a_self_signed_nest` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_video_played_to_its_end_reads_its_whole_length` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_sealed_video_plays_after_its_post_unlocks` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_ask_confirm_hands_the_feed_clip_to_the_os_player` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_bridged_video_plays_it_through_the_ticketed_proxy` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 13 | app | `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_bridged_video_hands_off_through_the_nest_proxy` | tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_playing_video_survives_a_feed_reload` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 14 | app | `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_feed_refresh_leaves_a_running_handoff_alone` | tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_leaving_the_feed_while_a_video_plays_keeps_the_app_running` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_leaving_the_feed_mid_handoff_and_coming_back_opens_the_video_again` | tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_feed_media_items.py::test_every_media_item_paints_in_order_with_its_alt` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_feed_bridged_image.py::test_a_bridged_posts_picture_paints_at_once_through_the_nest` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_feed_media_items.py::test_every_media_item_paints_in_order_with_its_alt` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_a_sealed_video_plays_after_its_post_unlocks` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed |
| 19 | app | `tests/e2e-unified/tests/test_tui_feed_video_external_open.py::test_a_sealed_video_hands_off_once_its_post_unlocks` | tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_federated_follow_cross_nest.py::test_a_reader_follows_an_author_on_another_nest_and_their_posts_reach_the_readers_feed` | linux (linux): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
| 20 | app | `tests/e2e-unified/tests/test_profile_remote_read_cross_nest.py::test_a_remote_actors_profile_shows_their_name_only_once_they_open_it` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
