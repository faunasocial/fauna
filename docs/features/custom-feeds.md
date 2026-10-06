---
slug: custom-feeds
title: Build your own feeds
section: everyday
goal: docs/goal/ui/feed.md § Feed-rule types
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

Make a feed from rules: posts containing a phrase, carrying a hashtag, under a
content label, and more. Weigh what ranks it, for this feed alone or for all your
feeds. Feeds you no longer want delete in one step.

## Coverage contract

Stamped 2026-10-01 at d8cb0887cb.

1. [app] A feed built from a rule shows only the posts that match it — `docs/goal/ui/feed.md` § Feed-rule types
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_body_contains`
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_has_hashtag`
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_label_below_rule`
2. [app] The rule editor shows the right input for each rule kind and refuses an empty rule — `docs/goal/ui/feed.md` § Where logic lives
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_rule_builder_input_widget_switches_on_rule_type`
   - `tests/e2e-unified/tests/test_feed_creation.py::test_feed_add_rule_button_disabled_for_invalid_input`
3. [app] You can weigh a ranking factor for one feed or for all of them — `docs/goal/architecture/content-moderation-and-ranking.md` § Composition
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_with_factor_weight_sets_local_composition`
   - `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_with_global_factor_merges_into_factor_set`
4. [app] A feed can be deleted — `docs/goal/ui/feed.md` § User actions
   - `tests/e2e-unified/tests/test_feed_bridge_subscribe.py::test_feed_delete_button_removes_the_feed`
5. [nest] Your nest stores your feeds and applies their rules when it serves them — `docs/goal/ui/feed.md` § Feed-rule types
   - `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_custom_filter`
   - `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_crud_lifecycle`
   - `tests/e2e-unified/tests/api/test_feed_filter.py::test_feed_single_author_filter`
   - `tests/e2e-unified/tests/api/test_feed_filter.py::test_feed_multi_author_filter`
   - `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_with_several_rules_serves_all_or_any`
6. [app] A feed with several rules can require every rule to match or just one of them — `docs/goal/ui/feed.md` § Feed-rule types
   - `tests/e2e-unified/tests/test_feed_creation.py::test_a_feed_with_several_rules_requires_all_of_them_or_any_one`
7. [app] Each labeler you subscribe to is offered as a factor to weigh into a feed, and it re-ranks that feed — `docs/goal/architecture/content-moderation-and-ranking.md` § Distributed report sharing
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
8. [app] Your trained topics are offered, by the names you gave them, when you pick a feed's ranking factors — `docs/goal/behavior/topic-factors.md` § Authoring surface & picker
   - (none)
9. [app] You can weigh how many people watched a post to the end, or skipped it, into a feed of your own — `docs/goal/behavior/engagement-cues.md` § Layer B
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+4bc2efab standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_body_contains` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_has_hashtag` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_label_below_rule` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_rule_builder_input_widget_switches_on_rule_type` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_feed_add_rule_button_disabled_for_invalid_input` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_with_factor_weight_sets_local_composition` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_create_feed_with_global_factor_merges_into_factor_set` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_bridge_subscribe.py::test_feed_delete_button_removes_the_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_custom_filter` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_crud_lifecycle` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_filter.py::test_feed_single_author_filter` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_filter.py::test_feed_multi_author_filter` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_feed_api.py::test_feed_with_several_rules_serves_all_or_any` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_a_feed_with_several_rules_requires_all_of_them_or_any_one` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): failed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
<!-- features-render:end -->
