---
slug: admin-mail-health
title: Mail health at a glance
section: admin area
goal: docs/goal/behavior/mail-deliverability.md § Admin-pane Deliverability surface
guide: docs/guides/admin-tour.md § Mail
---

## What a user gets

At the top of the Mail page, right under the mail on/off switch, one line says
whether mail is working — delivering, warming up, delayed, blocklisted, records
needing attention, the mail service not connected, or simply off — followed by
when mail was last delivered and last received. Seven short rows underneath show
each check behind that line. **Check again** re-runs the blocklist and
DNS-record checks on the spot; **Restart warm-up** (after the server's outgoing
address changed) asks you to press again before it starts the sending ramp over.
While the server's address is on a blocklist, a link to request removal
appears. The same one-line state shows as a **Mail** card on the dashboard, so a
problem is visible the moment you open the admin area.

## Coverage contract

Stamped 2026-10-13 at ab51f8d2bd.

1. [app] The Mail page shows the state the nest decided and its seven check rows, in order, with the heartbeat facts — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_renders_the_nest_state_and_seven_rows`
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
2. [app] Check again runs the blocklist check and the records check right away, without waiting for the daily run, and the readout shows their results — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_recheck_runs_a_self_check_and_rereads`
3. [app] The removal-request link is offered only while the server's address is blocklisted — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_delist_link_absent_while_not_listed`
4. [app] Restarting the warm-up takes a second, confirming press before anything changes — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm`
5. [app] The dashboard's Mail card shows the same state — `docs/goal/behavior/admin.md` § 1. Dashboard
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_dashboard_mail_card_shows_the_state`
6. [nest] Restarting the warm-up puts the nest back on the first day of the ramp — `docs/goal/behavior/mail-deliverability.md` § Manual reset
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_resets_warmup_round_trip`
7. [app] While mail is on and a mail helper is not connected — crashed, stuck restarting, or never approved — the Mail page and the dashboard's Mail card say the mail service is not connected — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_mail_on_with_no_helper_connected_reads_not_connected`
8. [app] While mail is off the Mail page still shows its health section, and the line reads off as a neutral state, not as a problem — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
9. [app] Turning mail on from the Mail page moves the health line away from off without leaving the page — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
10. [app] When the latest blocklist check finds the server's address listed, the line says the address is blocklisted — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page`
11. [app] When an outgoing message has failed at least once and is older than the delay-warning time, the line says outgoing mail is delayed — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_failed_message_older_than_the_warning_age_reads_outgoing_mail_delayed`
12. [nest] Mail held back by the sending warm-up never makes mail health read as delayed — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_mail_held_by_the_warmup_never_reads_as_delayed`
13. [app] When the latest records check finds a failing sender-policy, signing, reporting-policy or reverse-lookup record, the line says DNS records need attention — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_failing_records_read_dns_records_need_attention`
14. [app] While a new server's sending ramp is still under way and nothing worse holds, the line says warming up — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_healthy_server_inside_its_ramp_reads_warming_up`
15. [app] With mail on and no problem or ramp holding, the line says delivering — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_healthy_server_past_its_ramp_reads_delivering`
16. [nest] When several problems hold at once, mail health names the worst: off, then not connected, blocklisted, delayed, records needing attention, warming up — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_the_worst_of_several_problems_is_named`
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_off_and_not_connected_outrank_a_listing`
17. [app] An app that meets a mail-health state newer than itself says mail needs attention instead of failing — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_state_newer_than_the_app_reads_needs_attention`
18. [app] Last delivered and last received say how long ago each happened, or never when it has not happened yet — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_the_heartbeats_say_how_long_ago_or_never`
19. [nest] A nest that has sent or received nothing for a long time is not reported as a problem: the two times never change the health state — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_a_nest_that_never_sent_or_received_is_not_a_problem`
20. [nest] Last delivered moves when the nest hands a message to an outside server — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_last_delivered_moves_when_mail_leaves`
21. [nest] Last received moves when the nest accepts a message from outside — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_last_received_moves_when_mail_arrives`
22. [nest] Mail health tells the admin only when mail last left and last arrived, never whose mail, which domain or which message — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_mail_health_names_no_person_domain_or_message`
23. [app] While the server's address is listed, the blocklist row names the list and the reason the list gave — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page`
24. [app] A blocklist that did not answer shows as a warning on the blocklist row, distinct from both listed and not listed — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_blocklist_that_did_not_answer_is_a_warning_distinct_from_listed_and_clear`
25. [nest] A blocklist check that gets no answer leaves the earlier result standing, so an unanswered check never clears a listing — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_an_unanswered_check_leaves_the_listing_standing`
26. [app] The removal-request link leads to the removal page of the blocklist that lists the server's address — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page`
27. [nest] The records check behind mail health runs again by itself once a day, so the records row is current without anyone pressing Check again — `docs/goal/behavior/mail-deliverability.md` § Goal
    - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_nest_runs_its_daily_blocklist_self_check_unasked`
28. [nest] Restarting the warm-up keeps the count of all the mail the nest has ever sent — `docs/goal/behavior/mail-deliverability.md` § Manual reset
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_restarting_the_warmup_keeps_the_lifetime_count`
29. [app] After the confirming press the warm-up button goes back to its ordinary label, so a later press asks for confirmation again — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm`
30. [nest] Only an admin can read mail health: a member who asks is refused — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_a_member_cannot_read_mail_health`
31. [nest] Running the checks never changes a DNS record or a mail setting: it only reports — `docs/goal/behavior/mail-deliverability.md` § No automated remediation
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_running_the_checks_changes_no_record_or_setting`
32. [app] When the server's address is on several blocklists, the blocklist row names them all, and the removal-request link leads to the first of them that has a removal page — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_an_address_on_several_lists_names_them_all_and_links_the_first`
33. [nest] Pressing Check again a second time within a minute does not ask the blocklists again, and never clears a listing the earlier check found — `docs/goal/behavior/mail-deliverability.md` § Force-refresh
    - `tests/e2e-unified/tests/api/test_mail_health.py::test_a_second_check_within_the_minute_asks_no_list_and_keeps_the_listing`
34. [app] The mail health section points you to the two big mailbox providers' own sender dashboards, as plain help text with links — `docs/goal/behavior/mail-deliverability.md` § The admin-pane surface
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_the_sender_dashboard_note_points_to_gmail_and_outlook`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+fe81a038.dirty standalone |
| windows | ⚠ partial | 0.1.4-dev+09f6a74e.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| ios | ⚠ partial | 0.1.5-dev+61b844ca standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+cab1c46a standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_renders_the_nest_state_and_seven_rows` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_recheck_runs_a_self_check_and_rereads` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_delist_link_absent_while_not_listed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_dashboard_mail_card_shows_the_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): skipped, ios (macos): skipped, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_resets_warmup_round_trip` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_mail_on_with_no_helper_connected_reads_not_connected` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): skipped, ios (macos): skipped, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_failed_message_older_than_the_warning_age_reads_outgoing_mail_delayed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_mail_held_by_the_warmup_never_reads_as_delayed` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_failing_records_read_dns_records_need_attention` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_healthy_server_inside_its_ramp_reads_warming_up` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_healthy_server_past_its_ramp_reads_delivering` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_the_worst_of_several_problems_is_named` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_off_and_not_connected_outrank_a_listing` | nest (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_state_newer_than_the_app_reads_needs_attention` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): skipped, ios (macos): skipped, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_the_heartbeats_say_how_long_ago_or_never` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_a_nest_that_never_sent_or_received_is_not_a_problem` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_last_delivered_moves_when_mail_leaves` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_last_received_moves_when_mail_arrives` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_mail_health_names_no_person_domain_or_message` | nest (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_blocklist_that_did_not_answer_is_a_warning_distinct_from_listed_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_an_unanswered_check_leaves_the_listing_standing` | nest (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_nest_runs_its_daily_blocklist_self_check_unasked` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_restarting_the_warmup_keeps_the_lifetime_count` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_a_member_cannot_read_mail_health` | nest (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_running_the_checks_changes_no_record_or_setting` | nest (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_an_address_on_several_lists_names_them_all_and_links_the_first` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_mail_health.py::test_a_second_check_within_the_minute_asks_no_list_and_keeps_the_listing` | nest (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_the_sender_dashboard_note_points_to_gmail_and_outlook` | web (linux): passed, linux (linux): passed, macos (macos): skipped, ios (macos): skipped, tui (linux): passed |
<!-- features-render:end -->
