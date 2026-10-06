---
slug: mailing-lists
title: Mailing lists
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-mass-mailing.md § Goal
guide: docs/guides/own-your-mail.md § Newsletters and mailing lists
---

## What a user gets

Run a newsletter from your own address: make a list, add members one at a time
or by pasting a block, and send. Every message carries a one-click unsubscribe that
works, an unsubscribed member stays on the list as unsubscribed, and sends are
capped so one list cannot burn your domain's reputation.

## Coverage contract

Stamped 2026-09-23 at 039e9619ca.

1. [app] Make a list, open its members, add and unsubscribe members one at a time or by pasting many, and delete the list — `docs/goal/behavior/mail-mass-mailing.md` § `mail-lists` page UX
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_page_reachable`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_add_via_sheet_lands_on_the_nest`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_open_members_scopes_the_page`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_add_and_unsubscribe_round_trip`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_batch_import_lands_every_valid_address`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_delete_cascades_on_the_nest`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_page_reachable`
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_nothing_selected_falls_back_to_first_list`
2. [nest] A send reaches only subscribed members, carries a working one-click unsubscribe by link and by mail, and is capped per send and per day — `docs/goal/behavior/mail-mass-mailing.md` § RFC 8058 one-click unsubscribe
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_unsubscribed_member_excluded_from_send`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_one_click_unsubscribe_via_https`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_one_click_unsubscribe_via_mailto`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_resub_undoes_unsubscribe`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_send_cap_rejects_oversize`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_day_cap_tempfails`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation`
3. [app] Each list shows how many members are subscribed, when you last sent, and how much you have sent today — `docs/goal/behavior/mail-mass-mailing.md` § Layout
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_row_shows_member_count_last_send_and_todays_meter`
4. [app] You can edit a list's details after making it — `docs/goal/behavior/mail-mass-mailing.md` § Layout
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_edit_sheet_changes_a_lists_details`
5. [app] When you make a list you can add a description, a help link, an archive link, and a lower limit on recipients per send — `docs/goal/behavior/mail-mass-mailing.md` § Add list sheet
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_add_sheet_sets_description_links_and_per_send_cap`
6. [app] Deleting a list asks you to confirm first and says its members go with it — `docs/goal/behavior/mail-mass-mailing.md` § The list as an alias row
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_delete_confirm_says_the_members_go_too`
7. [app] The members page counts who is subscribed and who has unsubscribed, and lists each member with their status — `docs/goal/behavior/mail-mass-mailing.md` § `mail-list-members` page
   - `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_add_and_unsubscribe_round_trip`
8. [app] You can re-subscribe a member who unsubscribed — `docs/goal/behavior/mail-mass-mailing.md` § `mail-list-members` page
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_resubscribe_from_the_members_page`
9. [app] When you paste many addresses, invalid ones are skipped and you are told how many — `docs/goal/behavior/mail-mass-mailing.md` § `mail-list-members` page
   - `tests/e2e-unified/tests/test_mail_lists_controls.py::test_import_tally_says_how_many_were_skipped`
10. [app] You can send an issue to a list from the app — `docs/goal/behavior/mail-mass-mailing.md` § Composing a list message
   - (none)
11. [app] Before sending, you see how many subscribers it will reach and your allowance for today — `docs/goal/behavior/mail-mass-mailing.md` § Composing a list message
   - (none)
12. [app] A list send shows as one entry in your sent mail, not one per recipient — `docs/goal/behavior/mail-mass-mailing.md` § Composing a list message
   - (none)
13. [app] You see a send's delivery progress as a whole, not per recipient — `docs/goal/behavior/mail-mass-mailing.md` § Composing a list message
   - (none)
14. [app] You are warned when you are close to today's list-sending limit, and an over-limit send is explained where you composed it — `docs/goal/behavior/mail-mass-mailing.md` § The per-day per-account cap
   - (none)
15. [app] Setting an archive link that points off your own server asks you once before saving — `docs/goal/behavior/mail-mass-mailing.md` § Don't do these
   - (none)
16. [nest] Adding a member at one of your own domains is refused, and you are pointed to aliases instead — `docs/goal/behavior/mail-mass-mailing.md` § Don't do these
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_member_on_local_domain_refused_pointing_at_aliases`
17. [nest] A list address that is taken or reserved is refused — `docs/goal/behavior/mail-mass-mailing.md` § Reserved local-part: `unsubscribe@`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_address_taken_or_reserved_refused`
18. [nest] Mail sent to a list's address is refused, because a list only sends — `docs/goal/behavior/mail-mass-mailing.md` § Pattern
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_mail_to_a_list_address_is_refused`
19. [nest] Sending as the list from a regular mail app is refused; issues go out only as list sends — `docs/goal/behavior/mail-mass-mailing.md` § Pattern
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_sending_as_the_list_from_a_mail_app_is_refused`
20. [nest] Every issue is marked as list mail with the list's name and address, so recipients' mail apps treat it as one — `docs/goal/behavior/mail-mass-mailing.md` § RFC 2369 list headers
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers`
21. [nest] The help link in every issue opens a page that explains how to subscribe and unsubscribe — `docs/goal/behavior/mail-mass-mailing.md` § RFC 2369 list headers
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_help_link_opens_a_subscribe_unsubscribe_page`
22. [nest] Each issue is signed for your domain, and the signature covers its unsubscribe headers, so receivers accept it as yours — `docs/goal/behavior/mail-mass-mailing.md` § Architectural rules
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers`
23. [nest] Opening an unsubscribe link in a browser only shows a confirm button; nothing changes until the recipient confirms, and an expired link says so — `docs/goal/behavior/mail-mass-mailing.md` § The HTTPS endpoint
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_unsubscribe_get_is_read_only_and_expired_link_says_so`
24. [nest] Mail to the bare unsubscribe address, with no token, is refused with an explanation — `docs/goal/behavior/mail-mass-mailing.md` § Reserved local-part: `unsubscribe@`
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_bare_unsubscribe_address_refused_with_explanation`
25. [nest] A member who unsubscribed stays unsubscribed if you add or import them again; only re-subscribing brings them back — `docs/goal/behavior/mail-mass-mailing.md` § Architectural rules
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_unsubscribe_is_sticky_through_readd_and_import`
26. [nest] Sending a newsletter does not use up your everyday sending allowance — `docs/goal/behavior/mail-mass-mailing.md` § Goal
   - `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_actor_rate_cap_not_consumed_by_list_send`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+f42d08e7 standalone |
| linux | ⚠ partial | 0.1.2-dev+42006402.dirty standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+73d24d4b standalone |
| ios | ⚠ partial | 0.1.2-dev+73d24d4b standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_page_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_add_via_sheet_lands_on_the_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_open_members_scopes_the_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_add_and_unsubscribe_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_batch_import_lands_every_valid_address` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_lists_delete_cascades_on_the_nest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_page_reachable` | linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_nothing_selected_falls_back_to_first_list` | linux (linux): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_unsubscribed_member_excluded_from_send` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_one_click_unsubscribe_via_https` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_one_click_unsubscribe_via_mailto` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_resub_undoes_unsubscribe` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_send_cap_rejects_oversize` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_day_cap_tempfails` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_token_deterministic_across_secret_rotation` | nest (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_row_shows_member_count_last_send_and_todays_meter` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_edit_sheet_changes_a_lists_details` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_add_sheet_sets_description_links_and_per_send_cap` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_delete_confirm_says_the_members_go_too` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_mail_lists.py::test_mail_list_members_add_and_unsubscribe_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_resubscribe_from_the_members_page` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_lists_controls.py::test_import_tally_says_how_many_were_skipped` | tui (linux): passed |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_member_on_local_domain_refused_pointing_at_aliases` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_address_taken_or_reserved_refused` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_mail_to_a_list_address_is_refused` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_sending_as_the_list_from_a_mail_app_is_refused` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_help_link_opens_a_subscribe_unsubscribe_page` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_send_stamps_rfc8058_headers` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_unsubscribe_get_is_read_only_and_expired_link_says_so` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_bare_unsubscribe_address_refused_with_explanation` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_unsubscribe_is_sticky_through_readd_and_import` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_mail_lists_send.py::test_list_per_actor_rate_cap_not_consumed_by_list_send` | nest (linux): passed |
<!-- features-render:end -->
