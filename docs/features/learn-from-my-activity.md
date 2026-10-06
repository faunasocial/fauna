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
   - (none)
6. [app] Flicking fast through your feed teaches nothing; only a brief look at reading pace counts as passing a post over — `docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation
   - (none)
7. [app] If you later linger on a post you had scrolled past, the topic takes back the less like this and learns more like this; only your latest reaction to a post counts — `docs/goal/behavior/engagement-cues.md` § Cue vocabulary & derivation
   - (none)
8. [app] A topic whose switch is off learns nothing from what you linger on or skip — `docs/goal/behavior/engagement-cues.md` § Layer A — the private training feed
   - (none)
9. [app] Turning a topic's switch off stops further learning and keeps what it already learned — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - (none)
10. [nest] Your nest keeps what your app collected about your activity only sealed, in a form its owner cannot read — `docs/goal/behavior/engagement-cues.md` § At rest — the sealed cue rollup
   - (none)
11. [nest] Once enough people on your nest share the same verdict on a public post, that anonymous count also reaches other nests — `docs/goal/behavior/engagement-cues.md` § Layer B
   - (none)
12. [nest] Turning signal sharing off withdraws only your activity signals and leaves any spam reports you share untouched, and the other way round — `docs/goal/behavior/engagement-cues.md` § Implementation status today
   - (none)
13. [app] If your saved activity data cannot be opened, you are told so instead of it silently starting over — `docs/goal/behavior/engagement-cues.md` § Implementation status today
   - (none)
14. [app] What a topic learns from your lingering moves matching posts up your feed, and the new order is still there after a restart — `docs/goal/behavior/engagement-cues.md` § Goal
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips`
15. [app] A topic learns from your activity only after you turn its switch on; every topic starts with it off — `docs/goal/behavior/topic-factors.md` § Training signals
   - `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data`
16. [app] What your app collected on one device informs your feeds on your other devices — `docs/goal/behavior/engagement-cues.md` § At rest — the sealed cue rollup
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+86db0698 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+98b03c39 standalone |
| macos | ⚠ partial | 0.1.2-dev+ac32864b standalone |
| ios | ⚠ partial | 0.1.2-dev+ac32864b standalone |
| android | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips` | linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_via_navigate_away_flush_apple` | web (linux): passed, macos (macos): passed, ios (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_optin_producer_and_transparency_pane` | web (linux): passed, linux (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_toggle_and_pane_render` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_signal_sharing_persisted_optin_honoured_by_producer_after_relaunch` | — |
| 4 | nest | `tests/e2e-unified/tests/api/test_signal_sharing.py::test_signal_sharing_k_gate_transparency_flip_public_gate_and_opt_out` | nest (linux): passed |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
| 14 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_dwell_derives_watch_complete_reranks_and_rollup_round_trips` | linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_engagement_cues.py::test_engagement_toggle_and_clear_activity_data` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed, tui (windows): passed |
| 16 | app | (none) | — |
<!-- features-render:end -->
