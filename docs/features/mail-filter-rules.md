---
slug: mail-filter-rules
title: Mail filter rules
section: mail, calendar and contacts
goal: docs/goal/behavior/email-filters.md § Email filter rules
guide: docs/guides/own-your-mail.md § Forwarding
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Rules that file, drop, forward or auto-reply to mail as it arrives, made and
edited in Settings. They run on the nest, so they apply whichever mail app you read
with, and an auto-reply answers each sender once and never answers a machine.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] Rules are created, edited in place and deleted from Settings — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_crud`
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_edit`
   - `tests/e2e-unified/tests/platform/bridge/test_email_filter_crud.py::test_email_filter_create_and_delete`
   - `tests/e2e-unified/tests/test_email_filter_nav_readiness.py::test_filter_edit_visible_correct_under_slow_load`
2. [nest] Rules run as mail arrives: file to a folder, refuse, drop, and auto-reply once per sender without answering machines — `docs/goal/behavior/smtp-server.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders`
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_reject_single_recipient_refuses_at_data`
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_reject_multi_recipient_drops_without_dsn`
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_autoreply_sends_once_then_rate_limited`
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_autoreply_suppressed_by_loop_guard`
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_email_filter_api`
3. [nest] Forward-all sends your mail on with a rewritten envelope, bounces come back to you, and the rate is capped — `docs/goal/behavior/mail-forwarding.md` § Two forwarding shapes
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_forward_all_enqueues_forwarded_outbound_row`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_forward_all_dispatches_under_srs`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_srs_bounce_routes_to_forwarder`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forward_rate_cap_suppresses_over_cap_forward_and_notifies`
4. [app] In the rule editor you can make a rule that files matching mail into a folder, adds a label, forwards it or replies for you — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_files_labels_and_replies_from_the_form`
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit`
5. [app] A rule can match on the spam score or on what a header says, as well as on the sender, the subject, the body and whether a header is present — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_matches_a_header_value_or_the_spam_score`
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_edit`
6. [app] You can order your rules and mark one to let later rules apply too — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_order_and_continue_are_set_from_the_form`
7. [app] A rule the editor cannot fully show is still listed, but never opens in a form that would lose part of it when saved — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_the_form_cannot_show_is_listed_but_never_opens`
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit`
8. [app] In a forwarding rule you choose the address and whether to keep your own copy — `docs/goal/behavior/mail-forwarding.md` § Per-rule "forward to"
   - `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit`
9. [app] You can forward all your incoming mail by entering an address in Settings, and stop by clearing it — `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all"
   - `tests/e2e-unified/tests/test_mail_forwarding_settings.py::test_forward_all_mail_is_set_and_stopped_from_settings`
10. [app] You can set your own hourly forwarding limit — `docs/goal/behavior/mail-forwarding.md` § Per-account forward rate-limit
   - `tests/e2e-unified/tests/test_mail_forwarding_settings.py::test_your_own_hourly_forwarding_limit_is_set_from_settings`
11. [app] You can turn on an automatic reply and write its message — `docs/goal/behavior/mail-policy-config.md` § Tier 3 — per-account (user)
   - `tests/e2e-unified/tests/test_mail_account_settings.py::test_an_automatic_reply_is_turned_on_with_its_message_and_off_again`
12. [nest] A rule can match on the spam score, on words in the body or on a header — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders`
13. [nest] A rule can add a label that your mail app shows as a keyword — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders`
14. [nest] The first matching rule decides unless it is marked to continue, and then later rules apply too, in order — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders`
15. [nest] A rule can keep a sender's mail in your inbox even when it would otherwise be filed as spam — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_allow_rule_keeps_a_senders_mail_in_the_inbox_past_every_scoring_pass`
16. [nest] No rule ever files incoming mail into Sent, Drafts or a held mailbox — `docs/goal/behavior/email-filters.md` § Email filter rules
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_file_into_sent_drafts_or_held_refused_at_create_and_placement`
17. [nest] A forwarding rule sends matching mail on to the address it names, keeping your copy or not as the rule says — `docs/goal/behavior/mail-forwarding.md` § Per-rule "forward to"
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_loop_suppressed_keeps_local_copy`
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_redirect_forwards_without_local_copy`
18. [nest] Forwarding everything still keeps a copy in your own mailbox — `docs/goal/behavior/mail-forwarding.md` § Per-account "forward all"
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay`
19. [nest] A forwarding loop is stopped: you still receive the mail, only the extra forward is dropped — `docs/goal/behavior/mail-forwarding.md` § Loop detection
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_loop_suppressed_keeps_local_copy`
20. [nest] Bounces sent to you are never forwarded on — `docs/goal/behavior/mail-forwarding.md` § Don't do these
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_never_forwards_a_bounce`
21. [nest] Forwarding everything to an address on this same server is refused — `docs/goal/behavior/mail-forwarding.md` § Don't do these
   - `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_all_to_a_hosted_domain_is_refused`
22. [nest] Forwards over your hourly limit wait and go out later rather than being lost — `docs/goal/behavior/mail-forwarding.md` § Per-account forward rate-limit
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forward_over_the_hourly_cap_waits_and_goes_out_later`
23. [nest] When a forward has to be dropped, you get a notice in the app, not an email — `docs/goal/behavior/mail-forwarding.md` § Queue ceiling
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forward_rate_cap_suppresses_over_cap_forward_and_notifies`
24. [nest] A forward that keeps failing bounces to you at most once per message in a week — `docs/goal/behavior/mail-forwarding.md` § NDR rate-limit
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_failing_forward_bounces_to_its_owner_once_per_message_a_week`
25. [nest] Forwarded mail keeps the original sender and its signature, so it still passes the destination's checks — `docs/goal/behavior/mail-forwarding.md` § Architectural rules
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forwarded_mail_keeps_its_sender_and_signature`
26. [nest] Mail that arrives forging one of your nest's own domains is never forwarded on — not by forward-all, a rule or an admin forwarder — while your own copy still arrives — `docs/goal/behavior/mail-forwarding.md` § Spoof floor
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_spoof_of_the_nests_own_domain_is_delivered_but_never_forwarded`
27. [nest] Forwards count against your daily sending allowance and wait once it is spent, rather than being lost — `docs/goal/behavior/mail-forwarding.md` § Architectural rules
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forwards_draw_on_the_daily_allowance_and_wait_once_it_is_spent`
28. [nest] A bounce to a forwarding address that was forged or has expired is refused, and one for a forward that no longer exists is dropped without reaching you — `docs/goal/behavior/mail-forwarding.md` § Bounce decode on inbound
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_forged_or_expired_bounce_is_refused_and_an_orphan_is_dropped`
29. [nest] When the forward queue is full, mail a rule would have redirected is delivered to your own mailbox instead of being lost — `docs/goal/behavior/mail-forwarding.md` § Queue ceiling
   - `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_redirect_the_full_forward_queue_cannot_take_lands_in_the_owners_mailbox`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| linux | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| windows | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+c9e7fe5d standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_crud` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/platform/bridge/test_email_filter_crud.py::test_email_filter_create_and_delete` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_email_filter_nav_readiness.py::test_filter_edit_visible_correct_under_slow_load` | windows (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_reject_single_recipient_refuses_at_data` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_reject_multi_recipient_drops_without_dsn` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_autoreply_sends_once_then_rate_limited` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_autoreply_suppressed_by_loop_guard` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_email_filter_api` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_forward_all_enqueues_forwarded_outbound_row` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_forward_all_dispatches_under_srs` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_srs_bounce_routes_to_forwarder` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forward_rate_cap_suppresses_over_cap_forward_and_notifies` | nest (linux): passed, nest (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_files_labels_and_replies_from_the_form` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_matches_a_header_value_or_the_spam_score` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_order_and_continue_are_set_from_the_form` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_the_form_cannot_show_is_listed_but_never_opens` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_settings.py::test_email_filter_forward_keeps_its_copy_mode_through_an_edit` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_forwarding_settings.py::test_forward_all_mail_is_set_and_stopped_from_settings` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_forwarding_settings.py::test_your_own_hourly_forwarding_limit_is_set_from_settings` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_account_settings.py::test_an_automatic_reply_is_turned_on_with_its_message_and_off_again` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders` | nest (linux): passed, nest (macos): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders` | nest (linux): passed, nest (macos): passed |
| 14 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_delivery_time_filters_route_to_folders` | nest (linux): passed, nest (macos): passed |
| 15 | nest | `tests/e2e-unified/tests/test_mail_spam_threshold_override.py::test_allow_rule_keeps_a_senders_mail_in_the_inbox_past_every_scoring_pass` | nest (linux): passed, nest (macos): passed |
| 16 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_file_into_sent_drafts_or_held_refused_at_create_and_placement` | nest (linux): passed, nest (macos): passed |
| 17 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_loop_suppressed_keeps_local_copy` | nest (linux): passed, nest (macos): passed |
| 17 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_redirect_forwards_without_local_copy` | nest (linux): passed, nest (macos): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay` | nest (linux): passed, nest (macos): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_loop_suppressed_keeps_local_copy` | nest (linux): passed, nest (macos): passed |
| 20 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_rule_never_forwards_a_bounce` | nest (linux): passed, nest (macos): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_bridge_filter_delivery.py::test_forward_all_to_a_hosted_domain_is_refused` | nest (linux): passed, nest (macos): passed |
| 22 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forward_over_the_hourly_cap_waits_and_goes_out_later` | nest (linux): passed, nest (macos): passed |
| 23 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forward_rate_cap_suppresses_over_cap_forward_and_notifies` | nest (linux): passed, nest (macos): passed |
| 24 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_failing_forward_bounces_to_its_owner_once_per_message_a_week` | nest (linux): passed, nest (macos): passed |
| 25 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forwarded_mail_keeps_its_sender_and_signature` | nest (linux): passed, nest (macos): passed |
| 26 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_spoof_of_the_nests_own_domain_is_delivered_but_never_forwarded` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_forwards_draw_on_the_daily_allowance_and_wait_once_it_is_spent` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_forged_or_expired_bounce_is_refused_and_an_orphan_is_dropped` | nest (linux): passed |
| 29 | nest | `tests/e2e-unified/tests/test_mail_bridge_forward_floors.py::test_a_redirect_the_full_forward_queue_cannot_take_lands_in_the_owners_mailbox` | nest (linux): passed |
<!-- features-render:end -->
