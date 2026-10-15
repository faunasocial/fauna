---
slug: learn-from-my-activity
title: Learn from my activity
section: family and personalization
goal: docs/goal/behavior/engagement-cues.md § Goal
guide: docs/guides/app-tour.md § Personalization
---

## What a user gets

Let a topic learn from what you actually linger on: a switch per topic, with a
button to clear what it collected. Optionally share your signals on public posts in
anonymous aggregate, and see the anonymous counts your nest shares with other nests.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] Lingering on a post teaches each topic you switched on, and what your app collected is saved to your nest when you leave the feed or close the app — `docs/goal/behavior/engagement-cues.md` § Layer A — the private training feed
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips`
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush`
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple`
2. [app] The per-topic switch persists and clear empties what was collected — `docs/goal/behavior/engagement-cues.md` § Layer A — the private training feed
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data`
3. [app] Sharing your activity signals is off until you turn it on, and the sharing pane shows the anonymous counts your nest shares with other nests — `docs/goal/behavior/engagement-cues.md` § Layer B — opt-in k-anonymized contribution (the `signal:*` factors)
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_optin_producer_and_transparency_pane`
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_toggle_and_pane_render`
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_persisted_optin_honoured_by_producer_after_relaunch`
4. [nest] Shared signals count only in anonymous aggregate, only on public posts, and stop when you opt out — `docs/goal/behavior/engagement-cues.md` § Layer B — opt-in k-anonymized contribution (the `signal:*` factors)
   - `tests/e2e-unified/tests/api/test_signal_sharing.py::test_signal_sharing_k_gate_transparency_flip_public_gate_and_opt_out`
5. [app] Scrolling quickly past a post in a topic that learns from your activity teaches it less like this — `docs/goal/behavior/engagement-cues.md` § Layer A — the private training feed
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_scrolling_past_a_post_teaches_less_like_this_and_lingering_later_takes_it_back`
6. [app] Flicking fast through your feed teaches nothing; only a brief look at reading pace counts as passing a post over — `docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_flicking_fast_past_a_post_teaches_nothing`
7. [app] If you later linger on a post you had scrolled past, the topic takes back the less like this and learns more like this; only your latest reaction to a post counts — `docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_scrolling_past_a_post_teaches_less_like_this_and_lingering_later_takes_it_back`
8. [app] A topic whose switch is off learns nothing from what you linger on or skip — `docs/goal/behavior/engagement-cues.md` § Layer A — the private training feed
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_a_switched_off_topic_learns_nothing_and_switching_off_keeps_what_it_learned`
9. [app] Turning a topic's switch off stops further learning and keeps what it already learned — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_a_switched_off_topic_learns_nothing_and_switching_off_keeps_what_it_learned`
10. [nest] Your nest keeps what your app collected about your activity only sealed, in a form its owner cannot read — `docs/goal/behavior/engagement-cues.md` § At rest — the sealed cue rollup
   - `tests/e2e-unified/tests/api/test_personalization_sealed_at_rest.py::test_the_engagement_rollup_rests_on_the_nest_only_sealed`
11. [nest] Once enough people on your nest share the same verdict on a public post, that anonymous count also reaches other nests — `docs/goal/behavior/engagement-cues.md` § Layer B
   - `tests/e2e-unified/tests/api/test_federation_exchange_originator.py::test_a_shared_verdict_enough_people_agree_on_reaches_the_peer_nest_as_a_count`
12. [nest] Turning signal sharing off withdraws only your activity signals and leaves any spam reports you share untouched, and the other way round — `docs/goal/behavior/engagement-cues.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_signal_sharing.py::test_opting_out_of_one_sharing_family_leaves_the_other_shared`
13. [app] If your saved activity data cannot be opened, you are told so instead of it silently starting over — `docs/goal/behavior/engagement-cues.md` § Implementation status today
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_saved_activity_data_that_cannot_be_opened_is_reported_not_started_over`
14. [app] What a topic learns from your lingering moves matching posts up your feed, and the new order is still there after a restart — `docs/goal/behavior/engagement-cues.md` § Goal
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips`
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple`
15. [app] A topic learns from your activity only after you turn its switch on; every topic starts with it off — `docs/goal/behavior/topic-factors.md` § Training signals
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data`
16. [app] What your app collected on one device informs your feeds on your other devices — `docs/goal/behavior/engagement-cues.md` § At rest — the sealed cue rollup
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_what_one_device_collected_informs_the_feeds_on_your_other_device`
17. [app] Playing a post's video to the end in your feed teaches each topic you switched on, however briefly the post was on screen — `docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_a_video_played_to_its_end_derives_watch_complete_through_the_media_arm`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.4-dev+52904896 standalone |
| linux | ⚠ partial | 0.1.4-dev+52904896 standalone |
| windows | ⚠ partial | 0.1.2-dev+98b03c39 standalone |
| macos | ⚠ partial | 0.1.2-dev+ac32864b standalone |
| ios | ⚠ partial | 0.1.2-dev+ac32864b standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.4-dev+52904896 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips` | linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple` | web (linux): passed, macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_optin_producer_and_transparency_pane` | web (linux): passed, linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_toggle_and_pane_render` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_persisted_optin_honoured_by_producer_after_relaunch` | — |
| 4 | nest | `tests/e2e-unified/tests/api/test_signal_sharing.py::test_signal_sharing_k_gate_transparency_flip_public_gate_and_opt_out` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_scrolling_past_a_post_teaches_less_like_this_and_lingering_later_takes_it_back` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_flicking_fast_past_a_post_teaches_nothing` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_scrolling_past_a_post_teaches_less_like_this_and_lingering_later_takes_it_back` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_a_switched_off_topic_learns_nothing_and_switching_off_keeps_what_it_learned` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_a_switched_off_topic_learns_nothing_and_switching_off_keeps_what_it_learned` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_personalization_sealed_at_rest.py::test_the_engagement_rollup_rests_on_the_nest_only_sealed` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_federation_exchange_originator.py::test_a_shared_verdict_enough_people_agree_on_reaches_the_peer_nest_as_a_count` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_signal_sharing.py::test_opting_out_of_one_sharing_family_leaves_the_other_shared` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_saved_activity_data_that_cannot_be_opened_is_reported_not_started_over` | tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips` | linux (linux): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple` | web (linux): passed, macos (macos): passed, ios (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 16 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_what_one_device_collected_informs_the_feeds_on_your_other_device` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_a_video_played_to_its_end_derives_watch_complete_through_the_media_arm` | web (linux): passed, linux (linux): passed |
<!-- features-render:end -->
