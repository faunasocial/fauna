---
slug: admin-mail-policy
title: Mail policy
section: admin area
goal: docs/goal/behavior/admin.md § 6. Mail
guide: docs/guides/admin-tour.md § Mail
---

## What a user gets

One flat form sets the mail server's policy: whether new members get mail by
themselves, the spam thresholds and how much the shared filter trusts training, the
penalty for mail to an unlisted address, connection limits, the idle timeout, alias
caps, and a button that publishes the nest's shared spam baseline once enough people
contribute. A change applies to the running server without a restart.

## Coverage contract

Stamped 2026-10-11 at 29b0959751.

1. [app] The Mail page carries the mail switch, the switch that gives new members mail, and the spam, sender-check, sending, mail-app, delivery and alias groups; a field opens showing the value the nest holds, and saving a new value changes it on the nest with no error — `docs/goal/behavior/admin.md` § 6. Mail
   - `tests/e2e-unified/tests/test_admin_mail.py::test_mail_policy_page_renders`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_new_policy_groups_render`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_spam_threshold_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_bayesian_weight_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_imap_idle_timeout_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_alias_policy_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_auth_max_conn_per_ip_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_auto_enable_new_users_toggle_round_trips`
2. [app] The unlisted-address penalty saves and survives saving a sibling knob — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
   - `tests/e2e-unified/tests/test_admin_mail_unlisted_penalty.py::test_unlisted_penalty_save_round_trips`
   - `tests/e2e-unified/tests/test_admin_mail_unlisted_penalty.py::test_unlisted_penalty_survives_other_knob_save`
3. [app] Publishing the shared spam baseline is withheld until enough people contribute — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
   - `tests/e2e-unified/tests/test_admin_mail.py::test_spam_tier2_training_knobs_render`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_publish_spam_baseline_controls_render`
   - `tests/e2e-unified/tests/test_admin_mail.py::test_publish_spam_baseline_withheld`
4. [nest] The nest accepts an admin's save of each of the five policy groups, refuses a member who is not an admin saving the spam group, and refuses spam thresholds in an order that makes no sense. A saved change takes hold on the running server with no restart — a new mail-app connection is dropped at the new idle timeout, and mail for a newly added domain is accepted and for a removed one refused — and it changes what happens to members and their mail: a member cannot go over the alias cap, and mail to an unlisted address goes to Junk under the penalty and to the inbox without it — `docs/goal/behavior/mail-policy-config.md` § Architectural rules
   - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_admin_puts_policy_substruct_over_wire`
   - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_put_spam_policy_rejects_threshold_ordering_violation_over_wire`
   - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_non_admin_denied_on_put_policy_kind`
   - `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_exact_alias_cap_enforced_from_put_alias_policy`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_idle_timeout_hot_reloads_without_restart`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart`
   - `tests/e2e-unified/tests/api/test_mail_unlisted_recipient_penalty.py::test_unlisted_recipient_penalty_files_catchall_mail_to_junk`
5. [nest] The admin runs the deliverability checks and gets a result for each; runs a blocklist check across the default blocklists, where an immediate second run is turned away as too soon; reads the sending warm-up's day and daily allowance; and reads the history of both kinds of check, newest first, with who ran each diagnostic — `docs/goal/behavior/mail-deliverability.md` § Symptom diagnostics
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_runs_diagnostics_over_wire`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_blocklist_self_check_history`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_runs_blocklist_self_check_and_rate_limit`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_warmup_status`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_diagnostic_run_history`
6. [app] With enough contributors, publishing the shared spam baseline shows on the page how many people it was built from; publishing again with nothing new says it is waiting for more activity; and switching standing publish off withdraws the baseline, the page then saying none is published — `docs/goal/behavior/mail-spam.md` § Cold start
   - `tests/e2e-unified/tests/test_admin_mail.py::test_standing_baseline_toggle_and_state_text`
7. [app] Saving spam thresholds in an order that makes no sense shows the nest's refusal on the page, and the earlier values stay saved — `docs/goal/behavior/admin.md` § 6. Mail
   - `tests/e2e-unified/tests/test_admin_mail.py::test_out_of_order_spam_thresholds_show_the_nest_refusal`
8. [app] Turning mail off on the Mail page stops the mail server for everyone on the nest — `docs/goal/behavior/admin.md` § 6. Mail
   - `tests/e2e-unified/tests/test_admin_mail.py::test_turning_mail_off_on_the_mail_page_stops_the_mail_server`
9. [nest] Mail scoring above the junk threshold the admin sets goes to Junk, so moving the threshold changes what lands there — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
   - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_junk_threshold_moves_scored_mail_and_reject_threshold_refuses_it`
10. [nest] Once the admin sets a reject threshold, mail scoring above it is refused to the sending server instead of delivered — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_junk_threshold_moves_scored_mail_and_reject_threshold_refuses_it`
11. [nest] No mail is held back from a member on its spam score: whatever the admin's thresholds, a message is delivered, filed to the member's Junk, or refused to the sending server — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_no_mail_is_held_back_on_its_spam_score`
12. [nest] Mail from a server on one of the admin's blocklists is refused at the door — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_server_on_an_admin_blocklist_is_refused_at_the_door`
13. [nest] A server that opens connections faster than the admin's per-minute limit is turned away, and a limit of zero admits all — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_connection_rate_limit_turns_fast_server_away_and_zero_admits_all`
14. [nest] With the identity checks on, a sending server whose greeting name or reverse lookup does not match its address is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_identity_checks_refuse_a_mismatched_greeting_or_reverse_lookup`
15. [nest] With reject-on-no-reverse-name on, mail from a server that has no reverse name is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_reject_on_no_reverse_name_refuses_a_server_without_one`
16. [nest] Mail larger than the size the admin sets is refused as too large, arriving from outside or sent from a mail app; a size above 250 MB cannot be set — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_size_cap_refuses_larger_mail_inbound_and_from_a_mail_app`
17. [nest] With greylisting off, a first-time sender is accepted on its first try — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_greylisting_off_accepts_a_first_time_sender_on_its_first_try`
18. [nest] With plus-addressing off, mail to an address with something added after a plus sign no longer reaches the member — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_plus_addressing_off_stops_tagged_mail_reaching_the_member`
19. [nest] With wildcard prefixes off, mail to addresses under a member's wildcard prefix no longer reaches them — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_wildcard_prefixes_off_stops_mail_under_the_prefix_reaching_the_member`
20. [nest] A name the admin adds to the reserved list cannot be taken as an address by any member, and clearing the list frees the names the admin added — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
    - `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_admin_added_reserved_name_refused_then_freed_by_clearing`
21. [nest] A full-confidence count that is not above the minimum-samples count is refused and nothing is saved — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_full_confidence_not_above_min_samples_refused_and_nothing_saved`
22. [nest] Members' spam training history is kept for as many days as the admin sets — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py::test_training_history_kept_for_the_days_the_admin_sets`
23. [nest] Every member's mailbox fills at the size the admin sets, and mail beyond it is refused as mailbox full, except mail to the role addresses — `docs/goal/behavior/mail-policy-config.md` § Per-tier quotas
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses`
24. [nest] Once the admin allows it, a mail app can delete a folder that still holds mail — `docs/goal/behavior/mail-policy-config.md` § IMAP server policy
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_delete_nonempty_allowed_lets_a_mail_app_delete_a_folder_still_holding_mail`
25. [nest] One address cannot hold more simultaneous mail-app connections than the admin allows: more are closed until one frees, and zero lifts the cap — `docs/goal/behavior/mail-policy-config.md` § Submission policy
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_per_address_connection_cap_closes_extra_mail_app_connections_and_zero_lifts_it`
26. [nest] With log-only on, or a sender check switched off, mail failing that check is accepted rather than refused; with signature enforcement on, mail whose signature fails is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound authentication enforcement
    - `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_log_only_or_a_check_switched_off_accepts_and_dkim_enforcement_refuses`
27. [nest] The retry schedule, the give-up time, the delay-warning time and the bounce-repeat window the admin sets decide when a member is warned about, or gets back, a message that keeps failing — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_retry_schedule_warning_and_give_up_times_decide_a_failing_message`
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_bounce_repeat_window_decides_when_the_same_message_bounces_again`
28. [nest] Error codes the admin lists as temporary are retried instead of bounced, and with IPv6 off the server delivers over IPv4 only — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_codes_the_admin_lists_as_temporary_are_retried_not_bounced`
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_ipv6_off_delivers_over_ipv4_only`
29. [nest] With backscatter suppression on, no bounce is sent for mail whose sender failed its checks; switched off, those bounces are sent — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - (none)
30. [nest] Turning outbound TLS reports off stops the nest sending them to other domains — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_tlsrpt_switched_off_sends_no_report`
31. [app] A numeric field saved with something that is not a number keeps the value already saved — `docs/goal/behavior/mail-policy-config.md` § What `0` means on an unsigned knob
    - `tests/e2e-unified/tests/test_admin_mail.py::test_non_numeric_entry_keeps_the_saved_value`
32. [nest] An allowance set to zero — daily sends, mailbox size, extra addresses — is accepted, and members then get none — `docs/goal/behavior/mail-policy-config.md` § What `0` means on an unsigned knob
    - `tests/e2e-unified/tests/test_mail_policy_effects.py::test_allowances_set_to_zero_are_accepted_and_members_get_none`
33. [nest] Saving any other spam setting never switches the standing shared baseline off; only its own switch does — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_saving_other_spam_knobs_never_switches_standing_baseline_off`
34. [app] Before the shared baseline is published the admin is asked to confirm, and told it reflects the training of the members who opted in — `docs/goal/behavior/mail-spam.md` § Cold start
    - `tests/e2e-unified/tests/test_admin_mail.py::test_publishing_the_baseline_asks_first_and_says_whose_training_it_reflects`
35. [app] When some opted-in members could not be included in a publish, the result says how many — `docs/goal/behavior/mail-spam.md` § Cold start
    - `tests/e2e-unified/tests/test_admin_mail.py::test_publish_result_says_how_many_could_not_be_included`
36. [nest] With standing publish on, the nest republishes the shared baseline by itself once a day, under the same floors — `docs/goal/behavior/mail-spam.md` § Cold start
    - `tests/e2e-unified/tests/test_spam_baseline_standing.py::test_standing_publish_republishes_the_baseline_once_a_day_under_the_floors`
37. [nest] The baseline's state shows only what is served now: never who contributed, and never when or why it was withdrawn — `docs/goal/behavior/mail-spam.md` § Cold start
    - `tests/e2e-unified/tests/test_spam_baseline_standing.py::test_baseline_state_shows_only_what_is_served_never_who_or_when_withdrawn`
38. [nest] The nest checks its own sending address against the blocklists every day without being asked — `docs/goal/behavior/mail-deliverability.md` § The check
    - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_nest_runs_its_daily_blocklist_self_check_unasked`
39. [nest] When the newsletter unsubscribe secret is rotated, new issues carry new links and the links in mail already sent stop working — `docs/goal/behavior/mail-mass-mailing.md` § Secret rotation
    - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation`
40. [nest] When the forwarding secret is rotated, bounces to mail forwarded before the rotation still find their way back until they expire — `docs/goal/behavior/mail-forwarding.md` § SRS scheme
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_srs_bounce_to_mail_forwarded_before_a_secret_rotation_still_reaches_the_forwarder`
41. [nest] Only an admin can run or read the deliverability checks, the blocklist self-check and the warm-up state; a member who tries is refused — `docs/goal/behavior/mail-deliverability.md` § Architectural rules
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostics`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_status`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_reset`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check_history`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostic_run_history`
42. [nest] Only an admin can save the sender-check, sending, mail-app, delivery and alias groups — `docs/goal/behavior/mail-policy-config.md` § Architectural rules
    - `tests/e2e-unified/tests/test_mail_admin_policy.py::test_non_admin_denied_on_every_other_policy_group`
    - `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_non_admin_denied_on_put_alias_policy`
43. [nest] No member can take a standard role address — postmaster, abuse, noc, security, or the two report addresses — as an address or as a handle, whatever the admin's reserved list holds, even when it is empty — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
    - `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_role_names_refused_as_alias_and_handle_with_the_list_empty`
44. [nest] Turning outbound DMARC reports off stops the nest sending them to other domains — `docs/goal/behavior/dmarc-reporting.md` § Generation is on by default
    - (none)
45. [app] The Mail page rotates the newsletter unsubscribe secret on a second press of its button: the first press only asks, and once rotated new issues carry new links while the links in mail already sent stop working — `docs/goal/behavior/mail-mass-mailing.md` § Secret rotation
    - `tests/e2e-unified/tests/test_admin_mail_secret_rotation.py::test_rotating_the_unsubscribe_secret_takes_a_second_press_and_retires_old_links`
46. [app] The Mail page rotates the forwarding secret on a second press of its button: the first press only asks, and once rotated forwards leave under a new address while a bounce to mail forwarded before still reaches the member — `docs/goal/behavior/mail-forwarding.md` § SRS scheme
    - `tests/e2e-unified/tests/test_admin_mail_secret_rotation.py::test_rotating_the_forwarding_secret_takes_a_second_press_and_keeps_old_bounces_home`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| linux | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| windows | ⚠ partial | 0.1.2-dev+ab96a0f8.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+8b423269 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_mail_policy_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_new_policy_groups_render` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_spam_threshold_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_bayesian_weight_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_imap_idle_timeout_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_alias_policy_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_auth_max_conn_per_ip_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_auto_enable_new_users_toggle_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_mail_unlisted_penalty.py::test_unlisted_penalty_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_mail_unlisted_penalty.py::test_unlisted_penalty_survives_other_knob_save` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_spam_tier2_training_knobs_render` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_publish_spam_baseline_controls_render` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_publish_spam_baseline_withheld` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_admin_puts_policy_substruct_over_wire` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_put_spam_policy_rejects_threshold_ordering_violation_over_wire` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_non_admin_denied_on_put_policy_kind` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_exact_alias_cap_enforced_from_put_alias_policy` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_idle_timeout_hot_reloads_without_restart` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_mail_unlisted_recipient_penalty.py::test_unlisted_recipient_penalty_files_catchall_mail_to_junk` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_runs_diagnostics_over_wire` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_blocklist_self_check_history` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_runs_blocklist_self_check_and_rate_limit` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_warmup_status` | nest (linux): passed, nest (macos): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_diagnostic_run_history` | nest (linux): passed, nest (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_standing_baseline_toggle_and_state_text` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_out_of_order_spam_thresholds_show_the_nest_refusal` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_turning_mail_off_on_the_mail_page_stops_the_mail_server` | tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_junk_threshold_moves_scored_mail_and_reject_threshold_refuses_it` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_junk_threshold_moves_scored_mail_and_reject_threshold_refuses_it` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_no_mail_is_held_back_on_its_spam_score` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_server_on_an_admin_blocklist_is_refused_at_the_door` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_connection_rate_limit_turns_fast_server_away_and_zero_admits_all` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_identity_checks_refuse_a_mismatched_greeting_or_reverse_lookup` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_reject_on_no_reverse_name_refuses_a_server_without_one` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_size_cap_refuses_larger_mail_inbound_and_from_a_mail_app` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_greylisting_off_accepts_a_first_time_sender_on_its_first_try` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_plus_addressing_off_stops_tagged_mail_reaching_the_member` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_wildcard_prefixes_off_stops_mail_under_the_prefix_reaching_the_member` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_admin_added_reserved_name_refused_then_freed_by_clearing` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_full_confidence_not_above_min_samples_refused_and_nothing_saved` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_mail_spam_history_retention.py::test_training_history_kept_for_the_days_the_admin_sets` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_delete_nonempty_allowed_lets_a_mail_app_delete_a_folder_still_holding_mail` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_per_address_connection_cap_closes_extra_mail_app_connections_and_zero_lifts_it` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_perimeter_dns.py::test_log_only_or_a_check_switched_off_accepts_and_dkim_enforcement_refuses` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_retry_schedule_warning_and_give_up_times_decide_a_failing_message` | nest (linux): failed |
| 27 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_admin_bounce_repeat_window_decides_when_the_same_message_bounces_again` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_codes_the_admin_lists_as_temporary_are_retried_not_bounced` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_ipv6_off_delivers_over_ipv4_only` | nest (linux): passed |
| 29 | nest | (none) | — |
| 30 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_tlsrpt_switched_off_sends_no_report` | nest (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_non_numeric_entry_keeps_the_saved_value` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/test_mail_policy_effects.py::test_allowances_set_to_zero_are_accepted_and_members_get_none` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_saving_other_spam_knobs_never_switches_standing_baseline_off` | nest (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_publishing_the_baseline_asks_first_and_says_whose_training_it_reflects` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_publish_result_says_how_many_could_not_be_included` | web (linux): passed, linux (linux): passed, macos (macos): passed, tui (linux): passed |
| 36 | nest | `tests/e2e-unified/tests/test_spam_baseline_standing.py::test_standing_publish_republishes_the_baseline_once_a_day_under_the_floors` | nest (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/test_spam_baseline_standing.py::test_baseline_state_shows_only_what_is_served_never_who_or_when_withdrawn` | nest (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_nest_runs_its_daily_blocklist_self_check_unasked` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation` | nest (linux): passed |
| 40 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_srs_bounce_to_mail_forwarded_before_a_secret_rotation_still_reaches_the_forwarder` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostics` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_status` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_reset` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check_history` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostic_run_history` | nest (linux): passed |
| 42 | nest | `tests/e2e-unified/tests/test_mail_admin_policy.py::test_non_admin_denied_on_every_other_policy_group` | nest (linux): passed |
| 42 | nest | `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_non_admin_denied_on_put_alias_policy` | nest (linux): passed |
| 43 | nest | `tests/e2e-unified/tests/api/test_mail_alias_policy.py::test_role_names_refused_as_alias_and_handle_with_the_list_empty` | nest (linux): passed |
| 44 | nest | (none) | — |
| 45 | app | `tests/e2e-unified/tests/test_admin_mail_secret_rotation.py::test_rotating_the_unsubscribe_secret_takes_a_second_press_and_retires_old_links` | web (linux): passed, tui (linux): passed |
| 46 | app | `tests/e2e-unified/tests/test_admin_mail_secret_rotation.py::test_rotating_the_forwarding_secret_takes_a_second_press_and_keeps_old_bounces_home` | web (linux): passed, tui (linux): passed |
<!-- features-render:end -->
