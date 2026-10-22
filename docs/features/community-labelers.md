---
slug: community-labelers
title: Community labelers
section: family and personalization
goal: docs/goal/architecture/content-moderation-and-ranking.md § Tier-3 community models & background re-processing
guide: docs/guides/app-tour.md § Personalization
---

## What a user gets

Browse labelers other people published, inspect what one does before you
subscribe, and see your subscriptions on the Personalization page. A labeler that
needs a newer app says so, on its row or when you inspect it, and cannot be
subscribed to.

## Coverage contract

Stamped 2026-10-09 at 0ddfae47dc.

1. [app] The catalog lists published labelers, inspect shows one before subscribing, and subscribing surfaces it on Personalization — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_catalog_browse_inspect_subscribe_unsubscribe`
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_empty_states_mark_a_loaded_page_not_a_loading_one`
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_personalization_feeds_and_muted_words_links`
2. [nest] A labeler you subscribe to scores your mail through a grant you minted, and nothing else — `docs/goal/architecture/content-moderation-and-ranking.md` § Sealing tier-1 (capability tiering)
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability`
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_obligation_seeded_and_drained_at_ingest_without_config_changed`
3. [app] Subscribing to a labeler that reads your mail trusts your nest's mail service to run that one labeler, and only it, over your mail — the trust shows on the Nests page naming the labeler, and unsubscribing withdraws it — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_subscribing_a_mail_labeler_trusts_the_mail_service_with_it_and_unsubscribing_withdraws_it`
4. [app] A mail labeler that cannot run yet, because your nest has no mail service or your account has no mail, says so on the page even though the subscription is recorded — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing`
5. [app] Subscribing to a labeler that reads only public posts, to a curated list or to a text model gives nobody access to your private content, and no trust entry appears for it — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing`
6. [nest] Letting your nest read and filter your mail does not let any community labeler read it; only that labeler's own grant does — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_the_mail_filtering_grant_lets_no_community_labeler_read_your_mail`
7. [nest] Only the labeler you subscribed to, exactly as its publisher signed it, ever runs over your mail; your nest cannot swap in another — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_your_nest_cannot_swap_another_module_in_for_the_labeler_you_subscribed_to`
8. [app] You can inspect a labeler again after subscribing to it, not only before — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing`
9. [nest] When a labeler you subscribe to publishes a new version, the mail it already scored is scored again by the new one — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_a_new_labeler_version_rescores_the_mail_the_old_one_scored`
10. [app] A labeler you subscribe to that scores public posts scores the posts you see — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_feed_scoring.py::test_a_subscribed_labeler_over_public_posts_scores_the_posts_you_see`
11. [app] A labeler whose signature does not check out shows as unverified and cannot be subscribed to — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_labeler_whose_signature_does_not_check_out_cannot_be_subscribed`
12. [app] Before you subscribe to a curated list, you see every post it scores and the score it gives each — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler`
13. [app] Before you subscribe to a text model, you see its whole vocabulary: each word or phrase, which way it pushes, and how many examples it came from — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model`
14. [app] A labeler your app is too old to read leaves your feeds as they were instead of scoring them wrongly — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_feed_scoring.py::test_a_labeler_your_app_is_too_old_to_read_leaves_your_feeds_as_they_were`
15. [nest] A text-model labeler you subscribe to scores posts only on your own device; your nest never scores your posts with it — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/api/test_labeler_subscriptions.py::test_a_text_model_subscription_leaves_the_nest_nothing_to_score_with`
16. [nest] A subscribed labeler runs sealed off from the network, so it cannot send what it reads anywhere — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_a_subscribed_labeler_that_wants_the_network_never_runs`
17. [nest] A curated list you subscribe to scores the posts it names, including ones that arrive later, and stops when you unsubscribe — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/api/test_labeler_subscriptions.py::test_a_curated_list_scores_the_posts_it_names_including_later_ones_until_unsubscribe`
18. [app] A labeler that needs a newer app says so on its row in the catalog — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_an_unsupported_artifact_version_says_so_on_its_catalog_row`
19. [app] Unsubscribing takes a labeler off your Personalization page, and its row in the catalog offers subscribing again — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_catalog_browse_inspect_subscribe_unsubscribe`
20. [nest] Mail you already had when you subscribed to a labeler is scored too, and taking its grant back stops the scoring until you grant it again — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability`
21. [app] A labeler that needs a newer app than yours says so when you inspect it, and cannot be subscribed to — `docs/goal/architecture/content-moderation-and-ranking.md` § Tier-3 community models & background re-processing
   - `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_labeler_newer_than_this_app_reads_inert_and_cannot_be_subscribed`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+13d662f5 standalone |
| linux | ✅ full | 0.1.2-dev+13d662f5 standalone |
| windows | ⚠ partial | 0.1.2-dev+13d662f5 standalone |
| macos | ⚠ partial | 0.1.2-dev+13d662f5 standalone |
| ios | ⚠ partial | 0.1.2-dev+13d662f5 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+13d662f5 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_catalog_browse_inspect_subscribe_unsubscribe` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_empty_states_mark_a_loaded_page_not_a_loading_one` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_personalization_feeds_and_muted_words_links` | windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability` | nest (linux): passed, nest (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_obligation_seeded_and_drained_at_ingest_without_config_changed` | nest (linux): passed, nest (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_subscribing_a_mail_labeler_trusts_the_mail_service_with_it_and_unsubscribing_withdraws_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_the_mail_filtering_grant_lets_no_community_labeler_read_your_mail` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_your_nest_cannot_swap_another_module_in_for_the_labeler_you_subscribed_to` | nest (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_mail_labeler_that_cannot_run_says_so_and_a_keyless_labeler_trusts_nothing` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_a_new_labeler_version_rescores_the_mail_the_old_one_scored` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_labeler_feed_scoring.py::test_a_subscribed_labeler_over_public_posts_scores_the_posts_you_see` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_labeler_whose_signature_does_not_check_out_cannot_be_subscribed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_pruned_list_labeler` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_trained_topics.py::test_trained_factor_publishes_as_a_scrubbed_text_model` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_labeler_feed_scoring.py::test_a_labeler_your_app_is_too_old_to_read_leaves_your_feeds_as_they_were` | web (linux): error, linux (linux): passed, tui (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_labeler_subscriptions.py::test_a_text_model_subscription_leaves_the_nest_nothing_to_score_with` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_a_subscribed_labeler_that_wants_the_network_never_runs` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_labeler_subscriptions.py::test_a_curated_list_scores_the_posts_it_names_including_later_ones_until_unsubscribe` | nest (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_an_unsupported_artifact_version_says_so_on_its_catalog_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_labeler_catalog_browse_inspect_subscribe_unsubscribe` | web (linux): passed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/test_capability_labeler_drain.py::test_labeler_drain_scores_mail_via_subscription_and_user_minted_capability` | nest (linux): passed, nest (windows): passed |
| 21 | app | `tests/e2e-unified/tests/test_labeler_catalog.py::test_a_labeler_newer_than_this_app_reads_inert_and_cannot_be_subscribed` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
<!-- features-render:end -->
