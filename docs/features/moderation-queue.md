---
slug: moderation-queue
title: Correct the classifier
section: family and personalization
goal: docs/goal/behavior/moderation.md § Goal
guide: docs/guides/app-tour.md § Personalization
---

## What a user gets

Your own moderation queue shows what your device flagged, including spam
detected in an encrypted message after it was decrypted, and lets you correct the
classifier. Where your nest's administrator has acted on your content under a
legal obligation, the queue also shows that decision and lets you appeal it with
a reason, which goes on the record for an administrator to review. Below the
queue, the page lists the reports you made about other people's content and lets
you withdraw one while it is still open.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] The queue is reachable and honest when empty — `docs/goal/behavior/moderation.md` § Layout & flow
   - `tests/e2e-unified/tests/test_moderation.py::test_navigate_to_moderation`
   - `tests/e2e-unified/tests/test_moderation.py::test_moderation_correction_count`
2. [app] Spam your device detects in an encrypted message shows in the queue, and a correction trains your sealed model — `docs/goal/behavior/moderation.md` § Categories & enforcement
   - `tests/e2e-unified/tests/test_moderation_local_detection.py::test_local_spam_detection_surfaces_in_moderation_queue`
   - `tests/e2e-unified/tests/test_moderation_local_detection.py::test_web_local_spam_detection_surfaces_in_moderation_queue`
   - `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_train_correction_writes_sealed_model_at_rest`
3. [app] You can appeal a decision from its entry in your queue: the appeal needs a reason, is recorded for an administrator to review, and the entry stays, still open to appeal — `docs/goal/behavior/moderation.md` § Legal takedown
   - `tests/e2e-unified/tests/test_moderation_appeal.py::test_the_author_appeals_a_takedown_through_the_app`
4. [nest] A label attached to a post filters it from feeds and counts in the stats — `docs/goal/behavior/moderation.md` § State & data shape
   - `tests/e2e-unified/tests/api/test_content_moderation.py::test_spam_filtering_pipeline`
5. [app] Each item in your queue shows what it was flagged as, how confident the flag is, and any action taken on it — `docs/goal/behavior/moderation.md` § Layout & flow
   - (none)
6. [app] When an administrator removes your content under a legal obligation, your queue gains an entry for it, and the entry stays if the decision is reversed — `docs/goal/behavior/moderation.md` § Legal takedown
   - `tests/e2e-unified/tests/test_admin_legal_takedown.py::test_the_admin_takes_down_and_restores_a_post_through_the_app`
7. [nest] Only the author of a removed post can appeal its removal, an appeal needs a reason of bounded length, and a repeat while one is pending is recorded once — `docs/goal/behavior/moderation.md` § Errors & edge cases
   - `tests/e2e-unified/tests/api/test_moderation_appeal.py::test_a_post_appeal_is_the_authors_bounded_and_once_per_decision`
8. [nest] An appeal is accepted only against a real decision, and a decision stays appealable after it is reversed — `docs/goal/behavior/moderation.md` § Errors & edge cases
   - `tests/e2e-unified/tests/api/test_moderation_appeal.py::test_an_appeal_needs_a_real_enforcement_record`
9. [app] Your moderation page lists the reports you made, each with its subject, reason, where it went and its status, and lets you withdraw one while it is open — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn`
10. [nest] What your device flags in your encrypted messages stays on your device; your nest never learns of it — `docs/goal/behavior/moderation.md` § State & data shape
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+068e152c.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_moderation.py::test_navigate_to_moderation` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_moderation.py::test_moderation_correction_count` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_local_detection.py::test_local_spam_detection_surfaces_in_moderation_queue` | linux (linux): passed, windows (windows): error, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_local_detection.py::test_web_local_spam_detection_surfaces_in_moderation_queue` | web (linux): passed, linux (linux): skipped, windows (windows): error, macos (macos): skipped, ios (macos): skipped, tui (linux): error |
| 2 | app | `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_train_correction_writes_sealed_model_at_rest` | linux (linux): passed, windows (windows): error, tui (linux): failed |
| 3 | app | `tests/e2e-unified/tests/test_moderation_appeal.py::test_the_author_appeals_a_takedown_through_the_app` | tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_content_moderation.py::test_spam_filtering_pipeline` | nest (linux): passed |
| 5 | app | (none) | — |
| 6 | app | `tests/e2e-unified/tests/test_admin_legal_takedown.py::test_the_admin_takes_down_and_restores_a_post_through_the_app` | linux (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_moderation_appeal.py::test_a_post_appeal_is_the_authors_bounded_and_once_per_decision` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_moderation_appeal.py::test_an_appeal_needs_a_real_enforcement_record` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn` | web (linux): passed, tui (linux): passed |
| 10 | nest | (none) | — |
<!-- features-render:end -->
