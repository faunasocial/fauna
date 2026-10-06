---
slug: trained-topics
title: Train your own topics
section: family and personalization
goal: docs/goal/behavior/topic-factors.md § Goal
guide: docs/guides/app-tour.md § Personalization
---

## What a user gets

Make a topic, tell it "more like this" on a post, and the feeds you weight it
into re-rank; the examples and the model survive a restart. A topic you trained can
be published as a labeler for others, as a list or as a small text model that
generalizes to posts it never saw.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] Training re-ranks the feed, and the example marker and model survive a reload — `docs/goal/behavior/topic-factors.md` § Training signals
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload`
2. [app] A trained topic publishes as a list labeler, or as a text model that generalizes — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler`
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
3. [app] A publish error is shown and clears on a fresh attempt — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_publish_name_too_long_error_survives_unrelated_rerender`
   - `tests/e2e-unified/tests/test_trained_topics.py::test_publish_error_clears_on_fresh_sheet_reopen`
4. [app] Your topics are listed with their names and how many examples each has learned from, and you create a new one by name — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload`
5. [app] Renaming a topic keeps what it has learned and keeps it in every feed that uses it — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - (none)
6. [app] Deleting a topic erases what it learned — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload`
7. [app] A feed that weighted a topic you deleted keeps working without it — `docs/goal/behavior/topic-factors.md` § Wire & registry
   - (none)
8. [app] Marking a post less like this pushes posts like it down in the feeds that use the topic — `docs/goal/behavior/topic-factors.md` § Goal
   - (none)
9. [app] Choosing the other mark on a post you already marked switches it, and choosing your current mark again takes the example back — `docs/goal/behavior/topic-factors.md` § Training signals
   - (none)
10. [app] Outside a feed built around one topic, more like this asks which topic to train and offers to start a new one — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - (none)
11. [app] Your topics and what they have learned follow you to your other devices — `docs/goal/behavior/topic-factors.md` § Goal
   - (none)
12. [app] With the most topics you may have, creating another is refused with a message naming the limit — `docs/goal/behavior/topic-factors.md` § At rest — seal + home
   - (none)
13. [app] Creating a topic with no name is refused with its own message — `docs/goal/behavior/topic-factors.md` § Implementation status today
   - (none)
14. [app] A topic you have not trained yet leaves your feeds' order unchanged — `docs/goal/behavior/topic-factors.md` § The model
   - (none)
15. [nest] Your nest never learns your topics' names, what they contain, or which posts you marked — `docs/goal/behavior/topic-factors.md` § Goal
   - (none)
16. [app] If a topic's saved learning cannot be opened you are told so, and it is never quietly reset to untrained — `docs/goal/behavior/topic-factors.md` § Implementation status today
   - (none)
17. [app] Before a topic is published you review what it will show and remove anything you would rather not endorse — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler`
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
18. [app] The publish sheet says what publishing discloses: that a model applies to posts it never saw, and that it reveals word patterns shared by your marked public posts — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
19. [app] A topic you publish carries no link to you — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler`
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
20. [app] Every time the publish sheet opens, the public name starts empty and never shows your private topic name — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - (none)
21. [app] Publishing a topic as a model is refused, asking for more public examples, when fewer than three of your marked public posts share any pattern — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - (none)
22. [nest] A published topic is a snapshot: training it further changes nothing for subscribers, and publishing again releases a new version — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - (none)
23. [nest] Publishing a list-published topic again as a model upgrades the same labeler, and its subscribers keep their subscription — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - (none)
24. [nest] A published model is built only from your marked public posts, never from restricted posts or from what you lingered on — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - (none)
25. [app] Publishing a topic as a list tells you that a list covers only the posts you have seen — `docs/goal/behavior/topic-factors.md` § Publishing a trained factor
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+8999339a.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+0d1e684d standalone |
| macos | ⚠ partial | 0.1.2-dev+f85ee000 standalone |
| ios | ⚠ partial | 0.1.2-dev+f85ee000 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+f872d502 live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): failed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_publish_name_too_long_error_survives_unrelated_rerender` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_publish_error_clears_on_fresh_sheet_reopen` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | (none) | — |
| 6 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_topic_train_reranks_and_marker_survives_reload` | web (linux): failed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | nest | (none) | — |
| 16 | app | (none) | — |
| 17 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): failed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): failed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): failed, linux (linux): failed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 20 | app | (none) | — |
| 21 | app | (none) | — |
| 22 | nest | (none) | — |
| 23 | nest | (none) | — |
| 24 | nest | (none) | — |
| 25 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
<!-- features-render:end -->
