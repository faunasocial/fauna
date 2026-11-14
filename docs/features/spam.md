---
slug: spam
title: Spam, your way
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-spam.md § Goal
guide: docs/guides/own-your-mail.md § Spam, your way
absences:
  web (outcome 4): "docs/goal/behavior/report-sharing.md § Client wire + transparency surface"
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

A spam filter of your own, trained by you: mark a message as spam and it learns,
undo a lesson, reset it, and see its history. Scoring runs on your device or in the
mail server before a message is ever shown, so spam lands in Junk on every mail app.
You can contribute your training to the nest's shared starting point, and share what
you report with other nests, in anonymous aggregate only.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

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
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_an_undo_or_reset_on_one_device_updates_the_history_open_on_another`
12. [app] A lesson you gave in a mail app is in your training history the next time you open it — `docs/goal/behavior/mail-spam.md` § Implementation status today
   - `tests/e2e-unified/tests/test_mail_spam_controls.py::test_a_lesson_given_in_a_mail_app_is_in_the_history_next_time`
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
26. [nest] Once three or more people who share reports have reported the same message, it scores as spam against your own threshold too, and a not-spam mark withdraws your report — `docs/goal/behavior/report-sharing.md` § Applying the aggregate — the user's own threshold
   - `tests/e2e-unified/tests/test_mail_bridge_report_aggregate.py::test_three_reports_file_the_message_at_your_own_line_and_not_spam_vetoes_it`
27. [nest] Reports gathered on another nest nudge the score of your copy by a small, capped amount — `docs/goal/behavior/report-sharing.md` § Federation exchange
   - `tests/e2e-unified/tests/api/test_federation_exchange_originator.py::test_reports_gathered_on_another_nest_nudge_your_copy_by_a_capped_amount`
28. [app] Once three or more people who share reports have reported a message already in your inbox, your own app files it to Junk against your own threshold, whether or not you have trained your filter — `docs/goal/behavior/report-sharing.md` § Applying the aggregate — the user's own threshold
   - `tests/e2e-unified/tests/test_mail_report_aggregate_app.py::test_three_reports_file_a_message_already_in_your_inbox`
   - `tests/e2e-unified/tests/test_mail_report_aggregate_app.py::test_three_reports_file_an_unscored_message_already_in_your_inbox`
29. [app] Mail that reached your inbox before your spam filter could judge it is checked by your own app as soon as your filter has learned enough to judge — no restart needed — and filed to Junk if your filter then judges it spam — `docs/goal/behavior/mail-spam.md` § Scoring placement
   - `tests/e2e-unified/tests/test_mail_spam_catch_up_app.py::test_mail_delivered_before_the_first_lesson_is_filed_once_the_model_exists`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+d8a27534 standalone |
| linux | ⚠ partial | 0.1.5-dev+101d08fa standalone |
| windows | ⚠ partial | 0.1.2-dev+a945dfd3 standalone |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+b7384d11 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.6-dev+d5e8a720 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_page_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_history_renders_then_undo` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_reset_clears_history` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_contribute_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_trains_sealed_model` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_moderation_client_model_write.py::test_mark_as_spam_writes_sealed_history_row_then_undo` — requires `moderation-queue-second-real-engine` (test harness) (not built: web, android, android-kids) | linux (linux): passed, windows (windows): error, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_client_spam_receive.py::test_client_scores_inbound_spam_to_junk` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_spam.py::test_mail_spam_report_share_toggle_and_published_list` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | absent by design on web | — |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_refiles_trained_spam_to_junk` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_keeps_ham_in_inbox_and_watermarks` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_is_per_actor_isolated` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_inbound_spam_scored_before_visible_via_select_poll` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_scored_before_visible.py::test_idle_gate_scores_appended_spam_before_announce` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_apply_spam_disposition.py::test_apply_spam_disposition_watermarks_and_moves_to_junk` | nest (linux): passed, nest (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_agent_side_junk_train_reseals_sealed_model_at_rest` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mda_junk_train_sealed.py::test_untrained_recipient_junk_train_writes_a_sealed_model_from_empty` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_spam_scoring_cold_start_inherits_baseline` | nest (linux): passed, nest (macos): passed |
| 7 | nest | `tests/e2e-unified/tests/test_spam_baseline_drain.py::test_spam_baseline_drains_sealed_contributors_via_keyless_grant` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out` | nest (linux): passed, nest (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_history_rows_show_the_message_the_lesson_and_where_it_was_given` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_reset_confirm_says_it_cannot_be_undone` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_an_undo_or_reset_on_one_device_updates_the_history_open_on_another` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_a_lesson_given_in_a_mail_app_is_in_the_history_next_time` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_contributing_is_off_until_turned_on_and_is_a_revocable_grant` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 14 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_mark_as_spam_is_offered_on_received_messages_only` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 15 | app | `tests/e2e-unified/tests/test_mail_spam_controls.py::test_your_own_spam_threshold_sorts_your_mail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 16 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_leaving_junk_in_a_mail_app_trains_not_spam` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py::test_training_history_kept_a_month_then_lessons_stay_but_cannot_be_undone` | nest (linux): passed, nest (macos): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_only_an_explicit_mark_trains` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_marking_junk_and_moving_it_to_junk_is_one_lesson` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 20 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_a_message_moved_back_out_of_junk_stays_out` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_bridge_spam_signals.py::test_own_filter_does_not_sort_until_it_has_learned_enough` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 22 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_shared_rules_spam_goes_to_junk_untrained_never_refused` | nest (linux): passed, nest (macos): passed |
| 23 | nest | `tests/e2e-unified/tests/test_spam_baseline_withdrawal.py::test_departure_withdraws_the_served_baseline_at_once` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_mail_spam_model_isolation.py::test_neither_another_user_nor_the_admin_reads_or_trains_a_users_model` | nest (linux): passed, nest (macos): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_report_sharing.py::test_report_sharing_k_gate_opt_in_transparency_and_opt_out` | nest (linux): passed, nest (macos): passed |
| 26 | nest | `tests/e2e-unified/tests/test_mail_bridge_report_aggregate.py::test_three_reports_file_the_message_at_your_own_line_and_not_spam_vetoes_it` | nest (linux): passed, nest (macos): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_federation_exchange_originator.py::test_reports_gathered_on_another_nest_nudge_your_copy_by_a_capped_amount` | nest (linux): passed, nest (macos): passed |
| 28 | app | `tests/e2e-unified/tests/test_mail_report_aggregate_app.py::test_three_reports_file_a_message_already_in_your_inbox` | web (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 28 | app | `tests/e2e-unified/tests/test_mail_report_aggregate_app.py::test_three_reports_file_an_unscored_message_already_in_your_inbox` | web (linux): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 29 | app | `tests/e2e-unified/tests/test_mail_spam_catch_up_app.py::test_mail_delivered_before_the_first_lesson_is_filed_once_the_model_exists` | web (linux): passed, windows (windows): passed, tui (linux): passed, tui (macos): passed, tui (windows): failed |
<!-- features-render:end -->
