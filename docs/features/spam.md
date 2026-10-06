---
slug: spam
title: Spam, your way
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-spam.md § Goal
guide: docs/guides/own-your-mail.md § Spam, your way
absences:
  web (outcome 4): "docs/goal/behavior/report-sharing.md § Client wire + transparency surface"
---

## What a user gets

A spam filter of your own, trained by you: mark a message as spam and it learns,
undo a lesson, reset it, and see its history. Scoring runs on your device or in the
mail server before a message is ever shown, so spam lands in Junk on every mail app.
You can contribute your training to the nest's shared starting point, and share what
you report with other nests, in anonymous aggregate only.

## Coverage contract

Stamped 2026-09-23 at 039e9619ca.

1. [app] Your training history lists, one lesson undoes, reset clears everything, and the contribute switch persists — `docs/goal/behavior/mail-spam.md` § Training-sample retention
   - `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_page_reachable`
   - `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_history_renders_then_undo`
   - `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_reset_clears_history`
   - `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_contribute_toggle_round_trips`
2. [app] Marking a message as spam trains your sealed model — `docs/goal/behavior/mail-spam.md` § Training signal sources
   - `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_trains_sealed_model`
   - `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_writes_sealed_history_row_then_undo`
3. [app] Spam that arrives is filed to Junk by your own device, with no mail app involved — `docs/goal/behavior/mail-spam.md` § Scoring placement
   - `tests/e2e-unified/tests/test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk`
4. [app] The reports you share and their anonymous aggregate are shown to you — `docs/goal/behavior/report-sharing.md` § Client wire + transparency surface
   - `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_report_share_toggle_and_published_list`
5. [nest] The mail server files trained spam to Junk before a mail app sees it, keeps ham in the inbox, and never uses one person's training on another — `docs/goal/behavior/mail-spam.md` § Scoring placement
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_refiles_trained_spam_to_junk`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_keeps_ham_in_inbox_and_watermarks`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_is_per_actor_isolated`
   - `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_inbound_spam_scored_before_visible_via_select_poll`
   - `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_idle_gate_scores_appended_spam_before_announce`
   - `tests/e2e-unified/tests/api/test_mail_apply_spam_disposition.py::test_apply_spam_disposition_watermarks_and_moves_to_junk`
6. [nest] Marking spam from a mail app trains your model too, sealed at rest — `docs/goal/behavior/imap-server.md` § `\Junk` flag-change ↔ spam-training contract
   - `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_agent_side_junk_train_reseals_sealed_model_at_rest`
   - `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_untrained_recipient_junk_train_writes_a_sealed_model_from_empty`
7. [nest] A newcomer inherits the nest's shared starting point, built only from sealed contributions with enough contributors to stay anonymous — `docs/goal/behavior/mail-spam.md` § Cold start
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_cold_start_inherits_baseline`
   - `tests/e2e-unified/tests/test_spam_baseline_drain.py::test_spam_baseline_drains_sealed_contributors_via_keyless_grant`
8. [nest] What you report reaches other nests only as an anonymous aggregate, and only if you opted in — `docs/goal/behavior/report-sharing.md` § The k-anonymity choke point
   - `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out`
9. [app] Each entry in your training history shows the message, whether you marked it spam or not spam, and where you marked it — `docs/goal/behavior/mail-spam.md` § 3. IMAP `\Junk` flag changes (third-party MUA path)
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_history_rows_show_the_message_the_lesson_and_where_it_was_given`
10. [app] Resetting your filter asks you to confirm first and says it cannot be undone — `docs/goal/behavior/mail-spam.md` § Reset
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_reset_confirm_says_it_cannot_be_undone`
11. [app] An undo or reset on one device updates the training history already open on your other devices — `docs/goal/behavior/mail-spam.md` § Reset
   - (none)
12. [app] A lesson you gave in a mail app is in your training history the next time you open it — `docs/goal/behavior/mail-spam.md` § Implementation status today
   - (none)
13. [app] Contributing your training is off until you turn it on, and turning it on shows as a grant you can revoke from your grant log — `docs/goal/behavior/mail-spam.md` § Encrypted-mode interaction
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_contributing_is_off_until_turned_on_and_is_a_revocable_grant`
14. [app] Marking a message as spam is offered on messages you received, never on your own — `docs/goal/behavior/mail-spam.md` § 1. Explicit "Mark as spam" gesture (Fauna app first-party) — SHIPPED on the conversation surface
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_mark_as_spam_is_offered_on_received_messages_only`
15. [app] You set your own spam threshold for your account, and your mail is sorted by it — `docs/goal/behavior/mail-spam.md` § Scoring placement
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_your_own_spam_threshold_sorts_your_mail`
16. [nest] Moving a message out of Junk, or clearing its junk mark, in a mail app teaches your filter that it is not spam — `docs/goal/behavior/imap-server.md` § `\Junk` flag-change ↔ spam-training contract
   - `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_leaving_junk_in_a_mail_app_trains_not_spam`
17. [nest] Training history is kept for a month; after that an entry can no longer be undone, but what it taught stays — `docs/goal/behavior/mail-spam.md` § Training-sample retention
   - `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py::test_training_history_kept_a_month_then_lessons_stay_but_cannot_be_undone`
18. [nest] Only an explicit mark trains your filter; reading, deleting, replying to or archiving a message never does — `docs/goal/behavior/mail-spam.md` § Training signal sources
   - `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_only_an_explicit_mark_trains`
19. [nest] Marking a message as junk and moving it to Junk in a mail app counts as one lesson, not two — `docs/goal/behavior/mail-spam.md` § 3. IMAP `\Junk` flag changes (third-party MUA path)
   - `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_marking_junk_and_moving_it_to_junk_is_one_lesson`
20. [nest] A message you move back out of Junk stays out; it is not sorted into Junk again — `docs/goal/behavior/mail-spam.md` § Scoring placement
   - `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_a_message_moved_back_out_of_junk_stays_out`
21. [nest] Your own filter does not start sorting mail until it has learned from enough of your marks — `docs/goal/behavior/mail-spam.md` § Cold start
   - `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_own_filter_does_not_sort_until_it_has_learned_enough`
22. [nest] Mail the server's shared rules score as spam goes to Junk, never bounced or held back, even before you have trained anything — `docs/goal/behavior/smtp-server.md` § Spam handling (user-visible)
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_shared_rules_spam_goes_to_junk_untrained_never_refused`
23. [nest] Stopping your contribution, resetting your filter or deleting your account takes your training out of the shared starting point at once — `docs/goal/behavior/mail-spam.md` § Cold start
   - `tests/e2e-unified/tests/test_spam_baseline_withdrawal.py::test_departure_withdraws_the_served_baseline_at_once`
24. [nest] No one else, not even your admin, can read or train your spam filter — `docs/goal/behavior/mail-spam.md` § Cross-actor isolation
   - `tests/e2e-unified/tests/api/test_mail_spam_model_isolation.py::test_neither_another_user_nor_the_admin_reads_or_trains_a_users_model`
25. [nest] Turning report sharing off withdraws the reports you already shared — `docs/goal/behavior/report-sharing.md` § Report capture — existing gestures only
   - `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+730eca3d standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+a945dfd3 standalone |
| macos | ⚠ partial | 0.1.2-dev+94e7cd97 standalone |
| ios | ⚠ partial | 0.1.2-dev+94e7cd97 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7050b81c.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_page_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_history_renders_then_undo` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_reset_clears_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): skipped, ios (macos): skipped, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_contribute_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_trains_sealed_model` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_writes_sealed_history_row_then_undo` | linux (linux): passed, windows (windows): error, tui (linux): failed |
| 3 | app | `tests/e2e-unified/tests/test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_report_share_toggle_and_published_list` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | absent by design on web | — |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_refiles_trained_spam_to_junk` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_keeps_ham_in_inbox_and_watermarks` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_is_per_actor_isolated` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_inbound_spam_scored_before_visible_via_select_poll` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_idle_gate_scores_appended_spam_before_announce` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_apply_spam_disposition.py::test_apply_spam_disposition_watermarks_and_moves_to_junk` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_agent_side_junk_train_reseals_sealed_model_at_rest` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_untrained_recipient_junk_train_writes_a_sealed_model_from_empty` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_cold_start_inherits_baseline` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_spam_baseline_drain.py::test_spam_baseline_drains_sealed_contributors_via_keyless_grant` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_history_rows_show_the_message_the_lesson_and_where_it_was_given` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_reset_confirm_says_it_cannot_be_undone` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_contributing_is_off_until_turned_on_and_is_a_revocable_grant` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_mark_as_spam_is_offered_on_received_messages_only` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_your_own_spam_threshold_sorts_your_mail` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_leaving_junk_in_a_mail_app_trains_not_spam` | nest (linux): passed, nest (windows): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py::test_training_history_kept_a_month_then_lessons_stay_but_cannot_be_undone` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_only_an_explicit_mark_trains` | nest (linux): passed, nest (windows): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_marking_junk_and_moving_it_to_junk_is_one_lesson` | nest (linux): passed, nest (windows): passed |
| 20 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_a_message_moved_back_out_of_junk_stays_out` | nest (linux): passed, nest (windows): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_own_filter_does_not_sort_until_it_has_learned_enough` | nest (linux): passed, nest (windows): passed |
| 22 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_shared_rules_spam_goes_to_junk_untrained_never_refused` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/test_spam_baseline_withdrawal.py::test_departure_withdraws_the_served_baseline_at_once` | nest (linux): passed, nest (windows): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_mail_spam_model_isolation.py::test_neither_another_user_nor_the_admin_reads_or_trains_a_users_model` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out` | nest (linux): passed |
<!-- features-render:end -->
