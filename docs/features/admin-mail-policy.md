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

Stamped 2026-10-01 at 308c995169.

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
   - (none)
8. [app] Turning mail off on the Mail page stops the mail server for everyone on the nest — `docs/goal/behavior/admin.md` § 6. Mail
   - (none)
9. [nest] Mail scoring above the junk threshold the admin sets goes to Junk, so moving the threshold changes what lands there — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
   - (none)
10. [nest] Once the admin sets a reject threshold, mail scoring above it is refused to the sending server instead of delivered — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
11. [nest] No mail is held back from a member on its spam score: whatever the admin's thresholds, a message is delivered, filed to the member's Junk, or refused to the sending server — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
12. [nest] Mail from a server on one of the admin's blocklists is refused at the door — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
13. [nest] A server that opens connections faster than the admin's per-minute limit is turned away, and a limit of zero admits all — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
14. [nest] With the identity checks on, a sending server whose greeting name or reverse lookup does not match its address is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
15. [nest] With reject-on-no-reverse-name on, mail from a server that has no reverse name is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
16. [nest] Mail larger than the size the admin sets is refused as too large, arriving from outside or sent from a mail app; a size above 250 MB cannot be set — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
17. [nest] With greylisting off, a first-time sender is accepted on its first try — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
18. [nest] With plus-addressing off, mail to an address with something added after a plus sign no longer reaches the member — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
19. [nest] With wildcard prefixes off, mail to addresses under a member's wildcard prefix no longer reaches them — `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter
    - (none)
20. [nest] A name the admin adds to the reserved list cannot be taken as an address by any member, and clearing the list frees the names the admin added — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
    - (none)
21. [nest] A full-confidence count that is not above the minimum-samples count is refused and nothing is saved — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - (none)
22. [nest] Members' spam training history is kept for as many days as the admin sets — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - (none)
23. [nest] Every member's mailbox fills at the size the admin sets, and mail beyond it is refused as mailbox full, except mail to the role addresses — `docs/goal/behavior/mail-policy-config.md` § Per-tier quotas
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses`
24. [nest] Once the admin allows it, a mail app can delete a folder that still holds mail — `docs/goal/behavior/mail-policy-config.md` § IMAP server policy
    - (none)
25. [nest] One address cannot hold more simultaneous mail-app connections than the admin allows: more are closed until one frees, and zero lifts the cap — `docs/goal/behavior/mail-policy-config.md` § Submission policy
    - (none)
26. [nest] With log-only on, or a sender check switched off, mail failing that check is accepted rather than refused; with signature enforcement on, mail whose signature fails is refused — `docs/goal/behavior/mail-policy-config.md` § Inbound authentication enforcement
    - (none)
27. [nest] The retry schedule, the give-up time, the delay-warning time and the bounce-repeat window the admin sets decide when a member is warned about, or gets back, a message that keeps failing — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - (none)
28. [nest] Error codes the admin lists as temporary are retried instead of bounced, and with IPv6 off the server delivers over IPv4 only — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - (none)
29. [nest] With backscatter suppression on, no bounce is sent for mail whose sender failed its checks; switched off, those bounces are sent — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - (none)
30. [nest] Turning outbound TLS reports off stops the nest sending them to other domains — `docs/goal/behavior/mail-policy-config.md` § Outbound delivery
    - (none)
31. [app] A numeric field saved with something that is not a number keeps the value already saved — `docs/goal/behavior/mail-policy-config.md` § What `0` means on an unsigned knob
    - (none)
32. [nest] An allowance set to zero — daily sends, mailbox size, extra addresses — is accepted, and members then get none — `docs/goal/behavior/mail-policy-config.md` § What `0` means on an unsigned knob
    - (none)
33. [nest] Saving any other spam setting never switches the standing shared baseline off; only its own switch does — `docs/goal/behavior/mail-policy-config.md` § Spam (per-user training)
    - (none)
34. [app] Before the shared baseline is published the admin is asked to confirm, and told it reflects the training of the members who opted in — `docs/goal/behavior/mail-spam.md` § Cold start
    - (none)
35. [app] When some opted-in members could not be included in a publish, the result says how many — `docs/goal/behavior/mail-spam.md` § Cold start
    - (none)
36. [nest] With standing publish on, the nest republishes the shared baseline by itself once a day, under the same floors — `docs/goal/behavior/mail-spam.md` § Cold start
    - (none)
37. [nest] The baseline's state shows only what is served now: never who contributed, and never when or why it was withdrawn — `docs/goal/behavior/mail-spam.md` § Cold start
    - (none)
38. [nest] The nest checks its own sending address against the blocklists every day without being asked — `docs/goal/behavior/mail-deliverability.md` § The check
    - (none)
39. [nest] When the newsletter unsubscribe secret is rotated, new issues carry new links and the links in mail already sent stop working — `docs/goal/behavior/mail-mass-mailing.md` § Secret rotation
    - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation`
40. [nest] When the forwarding secret is rotated, bounces to mail forwarded before the rotation still find their way back until they expire — `docs/goal/behavior/mail-forwarding.md` § SRS scheme
    - (none)
41. [nest] Only an admin can run or read the deliverability checks, the blocklist self-check and the warm-up state; a member who tries is refused — `docs/goal/behavior/mail-deliverability.md` § Architectural rules
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostics`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_status`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_reset`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check_history`
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostic_run_history`
42. [nest] Only an admin can save the sender-check, sending, mail-app, delivery and alias groups — `docs/goal/behavior/mail-policy-config.md` § Architectural rules
    - (none)
43. [nest] No member can take a standard role address — postmaster, abuse, noc, security, or the two report addresses — as an address or as a handle, whatever the admin's reserved list holds, even when it is empty — `docs/goal/behavior/mail-aliases.md` § Reserved local-parts (uncircumventable)
    - (none)
44. [app] An admin can turn the sending warm-up off, or change its schedule — `docs/goal/behavior/mail-deliverability.md` § The ramp
    - (none)
45. [app] An admin can turn the nest's daily blocklist check off — `docs/goal/behavior/mail-deliverability.md` § Disable
    - (none)
46. [app] An admin can choose how long the history of blocklist checks is kept — `docs/goal/behavior/mail-deliverability.md` § Storage
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f3c1e99a standalone |
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
| 6 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_standing_baseline_toggle_and_state_text` | linux (linux): skipped, tui (linux): passed |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | nest | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | nest | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | nest | (none) | — |
| 22 | nest | (none) | — |
| 23 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses` | nest (linux): passed |
| 24 | nest | (none) | — |
| 25 | nest | (none) | — |
| 26 | nest | (none) | — |
| 27 | nest | (none) | — |
| 28 | nest | (none) | — |
| 29 | nest | (none) | — |
| 30 | nest | (none) | — |
| 31 | app | (none) | — |
| 32 | nest | (none) | — |
| 33 | nest | (none) | — |
| 34 | app | (none) | — |
| 35 | app | (none) | — |
| 36 | nest | (none) | — |
| 37 | nest | (none) | — |
| 38 | nest | (none) | — |
| 39 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation` | nest (linux): passed |
| 40 | nest | (none) | — |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostics` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_status` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_warmup_reset` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_blocklist_self_check_history` | nest (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_non_admin_denied_on_diagnostic_run_history` | nest (linux): passed |
| 42 | nest | (none) | — |
| 43 | nest | (none) | — |
| 44 | app | (none) | — |
| 45 | app | (none) | — |
| 46 | app | (none) | — |
<!-- features-render:end -->
