---
slug: feed-images-and-video
title: Images and video in posts
section: everyday
goal: docs/goal/ui/feed.md § Post content types
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Attach a picture or a video and it shows in the post: pictures inline with a
tap-to-enlarge view, videos as a thumbnail that plays in place when you tap it
(never on its own). Pictures carry their provenance badge
when the file has one. The hidden tags a camera writes into a photo — where it was
taken, when, on what — are removed on your own device, before the picture is ever
sent. A picture linked from somewhere else on the internet stays blocked until you
choose to load it, so nobody learns you read the post.

## Coverage contract

Stamped 2026-09-19 at 3ac2cf0c74.

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
12. [app] Tapping a video on a post plays it right there, and it never starts on its own — `docs/goal/architecture/render-model.md` § D6 — Feed body adopts the same `RenderDocument`
   - `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_video_plays_it_in_place`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_with_image` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_with_image_and_tags` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_post_has_image_by_text_is_scoped_to_the_named_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_uploads_via_sidecar_and_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_thumbnail_is_uploaded_and_served` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_image_lightbox.py::test_post_image_click_opens_lightbox` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_video_thumbnail.py::test_video_attachment_renders_video_thumbnail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_shows_c2pa_badge_when_uploaded_with_provenance` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed_remote_image.py::test_body_remote_image_is_blocked_until_revealed_then_paints` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_survives_a_blob_gc_sweep` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_feed.py::test_post_image_strips_exif_gps_before_the_nest_stores_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_gated_post_compose.py::test_a_restricted_post_s_picture_opens_for_an_entitled_reader` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_feed.py::test_a_picture_is_uploaded_when_you_post_so_writing_time_never_loses_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_feed.py::test_a_forged_c2pa_assertion_paints_no_provenance_badge` | windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 12 | app | `tests/e2e-unified/tests/test_feed_video_playback.py::test_tapping_a_video_plays_it_in_place` | web (linux): passed |
<!-- features-render:end -->
