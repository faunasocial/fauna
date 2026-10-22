---
slug: mailbox-import
title: Bring your mail from another provider
section: mail, calendar and contacts
goal: docs/goal/behavior/mailbox-migration.md § Goal
guide: docs/guides/own-your-mail.md § Moving in from your old provider
---

## What a user gets

Move your existing mail — from Gmail, Outlook, iCloud or any other IMAP
server — into your own mailbox without leaving the app. A short wizard asks
where your mail is, lets you choose which mailboxes to bring and from what
date, shows you how much is coming before you start, and then shows its
progress as the mail arrives. Your old provider's password stays in your app:
your nest never sees it. Mailboxes, dates and read, flagged and answered marks
come across as they were — a Gmail message filed under several labels lands
once — and mail you already have is not copied twice.

## Coverage contract

Stamped 2026-10-17 at 32c3482b1a.

1. [app] Run the import from start to finish and your mail arrives — the app reads exactly the mailboxes you chose from your old provider and reports what it brought over — `docs/goal/behavior/mailbox-migration.md` § Goal
   - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
2. [app] The import wizard is reachable from mail settings and opens on choosing where your mail is now — `docs/goal/behavior/mailbox-migration.md` § UX shape
   - `tests/e2e-unified/tests/test_mail_import.py::test_the_import_wizard_is_reachable_and_opens_on_the_source_step`
3. [app] Picking a well-known provider fills in its server details for you — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_selecting_a_source_kind_seeds_the_host_and_port_drafts`
4. [app] When the connection fails the wizard says so and keeps what you typed, so you correct what was wrong and try again from the same step — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_a_rejected_password_is_surfaced_and_the_user_retries_in_place`
5. [app] You choose which mailboxes to bring, and one you leave out is not read at all — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
6. [app] You can bring only mail since a date you choose, and nothing older is imported or counted against your storage — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date`
7. [app] Before anything is copied you see a summary of the mailboxes about to come over — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
8. [app] While it runs you see how far it has got, overall and per mailbox, with the reason for every message it skipped or could not bring — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date`
9. [app] You can pause and resume an import, or cancel it and keep everything that already arrived — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date`
   - `tests/e2e-unified/tests/test_mail_import.py::test_cancel_asks_first_and_keeps_what_already_arrived`
10. [app] Closing the app mid-import and opening it again brings you back to the same import, where it left off — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on`
11. [app] When it finishes you see how many messages were imported, skipped and failed, and can jump to your inbox or to the list of skipped messages — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
    - `tests/e2e-unified/tests/test_mail_import.py::test_review_skipped_on_the_done_screen_lists_every_skipped_message_and_mailbox`
12. [app] Your mail keeps its shape: each mailbox lands as the matching mailbox, with its original dates and read, flagged and answered marks — `docs/goal/behavior/mailbox-migration.md` § Goal
    - `tests/e2e-unified/tests/test_mail_import.py::test_imported_mail_keeps_its_shape_and_an_unmatched_mailbox_arrives_under_its_own_name`
13. [nest] Mail brought in by an earlier import, or saved into your mailbox from a mail app, is not copied a second time, and the skip is counted in the import's progress — `docs/goal/behavior/mailbox-migration.md` § Dedup vs. existing mail
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_dedup_skip_is_reported_in_the_progress_push`
    - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips`
14. [nest] Your old provider's password never reaches your nest — `docs/goal/behavior/mailbox-migration.md` § Credential handling
    - `tests/e2e-unified/tests/api/test_mail_import_client_api.py::test_the_old_providers_password_never_reaches_the_nest`
15. [nest] Word of how the import is going reaches your own apps and nobody else's — `docs/goal/behavior/mailbox-migration.md` § Progress lives nest-side
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_message_emits_progress_push`
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_pushes_are_scoped_to_the_importing_actor`
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_batch_emits_exactly_one_progress_push`
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_finalize_emits_complete_push`
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_fail_session_emits_error_push`
16. [app] Choosing a provider that needs an app password explains that it does and where to make one — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_provider_that_needs_an_app_password_says_so_and_where_to_make_one`
17. [app] An Outlook, Hotmail or Office 365 account connects by signing in with Microsoft: the app shows a short code, you enter it on Microsoft's sign-in page in any browser, and the import carries on without a password — `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the device-code grant
    - `tests/e2e-unified/tests/test_mail_import.py::test_outlook_signs_in_with_microsoft_and_imports`
18. [nest] A Microsoft sign-in used for an import never reaches your nest, and is thrown away when the import ends — `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the device-code grant
    - `tests/e2e-unified/tests/api/test_mail_import_client_api.py::test_the_microsoft_token_never_reaches_the_nest`
19. [app] The app never offers an unencrypted connection to your old provider — `docs/goal/behavior/mailbox-migration.md` § The two TLS modes, and where each half lives
    - `tests/e2e-unified/tests/test_mail_import.py::test_the_app_never_offers_an_unencrypted_connection`
20. [app] If the connection to your old provider cannot be proven secure — its certificate does not check out, or it refuses to switch to an encrypted connection — the app stops before your password is sent — `docs/goal/behavior/mailbox-migration.md` § The two TLS modes, and where each half lives
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_unverifiable_certificate_stops_before_the_password_is_sent`
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_refused_starttls_stops_before_the_password_is_sent`
21. [app] You can set the largest message to bring over: it starts at 50 MB and you can lower it — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large`
22. [app] A message larger than your size limit is skipped and listed as too large — `docs/goal/behavior/mailbox-migration.md` § Per-message flow
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large`
23. [app] A mailbox in your old account with no matching mailbox here is created for you under its original name — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_imported_mail_keeps_its_shape_and_an_unmatched_mailbox_arrives_under_its_own_name`
24. [app] The summary before the import says how many messages and roughly how much data are coming — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_status_size_source_is_estimated_without_reading_its_mailboxes`
25. [app] While an import runs you see a progress bar, how long it has been running and an estimate of the time left — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date`
26. [app] Pausing and resuming an import, or reopening the app in the middle of one, keeps the date you chose: it never quietly switches to importing everything — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date`
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on`
27. [app] Cancelling an import asks you to confirm first — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_cancel_asks_first_and_keeps_what_already_arrived`
28. [app] To pick an import up after the app was closed you type your old provider's password again: the app never keeps it — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on`
29. [app] The app remembers your old provider's server, port and connection security for the next time, until you delete them — `docs/goal/behavior/mailbox-migration.md` § Credential handling
    - `tests/e2e-unified/tests/test_mail_import.py::test_the_old_providers_server_is_remembered_for_next_time_until_it_is_forgotten`
30. [app] Starting a second import of the same old account on another device is refused with a message saying it is already being imported elsewhere — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_second_import_of_the_same_account_on_another_device_is_refused`
31. [app] Your old mailbox is left exactly as it was: importing marks nothing read and changes nothing there — `docs/goal/behavior/mailbox-migration.md` § Where the IMAP client runs — the transport seam
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
32. [app] If your old provider stops accepting your password part-way through, the import stops, says why, and offers to try again with new credentials — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_password_refused_part_way_through_stops_the_import_and_asks_for_new_credentials`
33. [app] If one of your old mailboxes disappears during the import, that is noted in the skipped list and the import carries on with the next one — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_mailbox_that_vanishes_part_way_through_is_noted_and_the_next_one_continues`
    - `tests/e2e-unified/tests/test_mail_import.py::test_review_skipped_on_the_done_screen_lists_every_skipped_message_and_mailbox`
34. [app] A message that cannot be read is counted as failed and the rest of the import carries on — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_unreadable_message_is_counted_failed_and_the_rest_arrives`
35. [app] If the connection to your old provider drops, the app retries a few times and then pauses the import, which you resume from where it stopped once the connection is back — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_source_that_drops_and_stays_down_pauses_the_import_and_resume_carries_on`
36. [app] Losing the connection to your nest during an import loses nothing: it continues from the last message your nest saved — `docs/goal/behavior/mailbox-migration.md` § Nest-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_losing_the_nest_connection_mid_import_loses_nothing`
37. [app] An import that keeps failing stops itself, with the reason, rather than grinding on — `docs/goal/behavior/mailbox-migration.md` § Per-message error budget
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_that_keeps_failing_stops_itself_with_the_reason`
38. [nest] Whether a message is one you already have is decided against your own mail only: another person's mail on the same nest never causes a skip, and never shows that they hold it — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_message_someone_else_holds_is_never_skipped_for_you_and_never_shows_they_hold_it`
39. [nest] A stranger who sends you mail reusing the identifier of a message you have not yet imported cannot make the real message be skipped — `docs/goal/behavior/mailbox-migration.md` § The envelope key confirms a Message-ID hit
    - `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_a_stranger_reusing_a_message_id_cannot_make_the_real_message_be_skipped`
40. [nest] A copy of a message that was changed on the way — a footer added, the subject tagged — is imported as the separate message it now is, not skipped — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_copy_changed_on_the_way_is_imported_as_its_own_message_not_skipped`
41. [app] An advanced option to import duplicates anyway, off unless you turn it on, warns you that it stores mail you already have a second time — `docs/goal/behavior/mailbox-migration.md` § Opt-out per session
    - `tests/e2e-unified/tests/test_mail_import.py::test_import_duplicates_anyway_stores_mail_already_in_the_mailbox_a_second_time`
    - `tests/e2e-unified/tests/test_mail_import.py::test_mail_already_in_the_mailbox_is_skipped_while_import_duplicates_is_off`
42. [nest] With duplicates allowed, a message you already have is imported again instead of skipped — `docs/goal/behavior/mailbox-migration.md` § Opt-out per session
    - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips`
43. [nest] A message your old provider filed under several labels lands only once — `docs/goal/behavior/mailbox-migration.md` § Don't do these
    - `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_message_filed_under_several_labels_lands_only_once`
44. [nest] An imported message cannot carry in a spam verdict or an alias match your nest never made: those marks are removed as it arrives — `docs/goal/behavior/mailbox-migration.md` § Nest-side sealing
    - `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_imported_mail_is_there_over_imap_carrying_only_marks_the_nest_wrote`
45. [nest] Imported mail rests sealed on your nest, and an import your nest cannot seal for you is refused rather than stored readable — `docs/goal/behavior/mailbox-migration.md` § Per-message flow
    - `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_imported_mail_rests_sealed_on_the_nest`
    - `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_an_import_the_nest_cannot_seal_is_refused_and_stores_nothing`
46. [nest] Imported mail counts against your mailbox storage — `docs/goal/behavior/mailbox-migration.md` § Quota composition
    - `tests/e2e-unified/tests/api/test_mail_quota_composition_api.py::test_imported_mail_counts_against_your_mailbox_storage`
47. [nest] Imported mail is ordinary mail: your other apps, and any mail app you use, see it once it lands — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_imported_mail_is_there_over_imap_carrying_only_marks_the_nest_wrote`
48. [nest] An import left untouched for thirty days is forgotten, and running the wizard again skips what already arrived — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_an_import_untouched_for_thirty_days_is_forgotten`
49. [app] If your old provider rebuilt a mailbox since the import began, you are warned and offered to restart the import, rather than having it carry on against the wrong messages — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_mailbox_the_provider_rebuilt_is_warned_about_and_restarted_without_importing_twice`
50. [app] The finished import also shows how long it took — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large`
51. [app] A failed connection is explained in plain words, with a hint for your provider, never in your old server's raw reply — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_failed_connection_is_explained_in_plain_words_never_the_servers_reply`
52. [app] Every mailbox starts ticked except the ones that hold discards or duplicates — Trash, Junk or Spam, and Gmail's All Mail, Bin, Important and Starred — which start unticked and can be ticked — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_discard_and_duplicate_mailboxes_start_unticked_and_are_never_hidden`
53. [app] Nothing is imported until you confirm the summary — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
54. [nest] Mail that was delivered to you, or that you sent, is not copied a second time by an import — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_mail_delivered_to_you_is_not_copied_a_second_time_by_an_import`
    - `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_mail_you_sent_is_not_copied_a_second_time_by_an_import`
55. [app] When your mailbox runs out of room part-way through, the import pauses and asks whether to wait for more room, skip the rest, or cancel — `docs/goal/behavior/mailbox-migration.md` § Nest-side errors
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_full_mailbox_pauses_and_skip_the_rest_finishes_without_reading_further`
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_full_mailbox_pauses_and_resume_after_the_admin_makes_room_brings_the_rest`
56. [app] An import that failed shows as failed, with the reason, on your other devices and after a restart, until you dismiss it or start another — `docs/goal/behavior/mailbox-migration.md` § A failed import is a record the wizard shows until the user moves on (ratified 2026-10-11)
    - `tests/e2e-unified/tests/test_mail_import.py::test_a_failed_import_shows_as_failed_after_a_restart_and_dismiss_clears_it`
57. [app] When an admin has entered their organisation's own Microsoft app registration, the Outlook import signs in with it instead of the default one — `docs/goal/behavior/mailbox-migration.md` § Microsoft sign-in — the device-code grant
    - `tests/e2e-unified/tests/test_mail_import_microsoft_registration.py::test_an_admin_set_microsoft_registration_is_the_client_id_the_sign_in_sends`
    - `tests/e2e-unified/tests/test_admin_mail.py::test_microsoft_registration_save_round_trips`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.4-dev+2a0b03b4 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_import.py::test_the_import_wizard_is_reachable_and_opens_on_the_source_step` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_import.py::test_selecting_a_source_kind_seeds_the_host_and_port_drafts` | web (linux): failed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_rejected_password_is_surfaced_and_the_user_retries_in_place` | linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date` | linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date` | linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date` | linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_import.py::test_cancel_asks_first_and_keeps_what_already_arrived` | linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on` | linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_import.py::test_review_skipped_on_the_done_screen_lists_every_skipped_message_and_mailbox` | linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_import.py::test_imported_mail_keeps_its_shape_and_an_unmatched_mailbox_arrives_under_its_own_name` | linux (linux): passed, tui (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_dedup_skip_is_reported_in_the_progress_push` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_mail_import_client_api.py::test_the_old_providers_password_never_reaches_the_nest` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_message_emits_progress_push` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_pushes_are_scoped_to_the_importing_actor` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_batch_emits_exactly_one_progress_push` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_finalize_emits_complete_push` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_fail_session_emits_error_push` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_provider_that_needs_an_app_password_says_so_and_where_to_make_one` | linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_import.py::test_outlook_signs_in_with_microsoft_and_imports` | linux (linux): passed, tui (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_mail_import_client_api.py::test_the_microsoft_token_never_reaches_the_nest` | nest (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_mail_import.py::test_the_app_never_offers_an_unencrypted_connection` | linux (linux): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_unverifiable_certificate_stops_before_the_password_is_sent` | linux (linux): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_refused_starttls_stops_before_the_password_is_sent` | linux (linux): passed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large` | linux (linux): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large` | linux (linux): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_mail_import.py::test_imported_mail_keeps_its_shape_and_an_unmatched_mailbox_arrives_under_its_own_name` | linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_status_size_source_is_estimated_without_reading_its_mailboxes` | linux (linux): passed, tui (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date` | linux (linux): passed, tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_paused_import_resumes_where_it_stopped_and_keeps_its_since_date` | linux (linux): passed, tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on` | linux (linux): passed, tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_mail_import.py::test_cancel_asks_first_and_keeps_what_already_arrived` | linux (linux): passed, tui (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_reopened_after_the_app_closed_asks_for_the_password_and_carries_on` | linux (linux): passed, tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_mail_import.py::test_the_old_providers_server_is_remembered_for_next_time_until_it_is_forgotten` | linux (linux): passed, tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_second_import_of_the_same_account_on_another_device_is_refused` | linux (linux): passed, tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_password_refused_part_way_through_stops_the_import_and_asks_for_new_credentials` | linux (linux): passed, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_mailbox_that_vanishes_part_way_through_is_noted_and_the_next_one_continues` | linux (linux): passed, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_mail_import.py::test_review_skipped_on_the_done_screen_lists_every_skipped_message_and_mailbox` | linux (linux): passed, tui (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_unreadable_message_is_counted_failed_and_the_rest_arrives` | linux (linux): passed, tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_source_that_drops_and_stays_down_pauses_the_import_and_resume_carries_on` | tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_mail_import.py::test_losing_the_nest_connection_mid_import_loses_nothing` | linux (linux): passed, tui (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_that_keeps_failing_stops_itself_with_the_reason` | linux (linux): passed, tui (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_message_someone_else_holds_is_never_skipped_for_you_and_never_shows_they_hold_it` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_a_stranger_reusing_a_message_id_cannot_make_the_real_message_be_skipped` | nest (linux): passed |
| 40 | nest | `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_copy_changed_on_the_way_is_imported_as_its_own_message_not_skipped` | nest (linux): passed |
| 41 | app | `tests/e2e-unified/tests/test_mail_import.py::test_import_duplicates_anyway_stores_mail_already_in_the_mailbox_a_second_time` | linux (linux): passed, tui (linux): passed |
| 41 | app | `tests/e2e-unified/tests/test_mail_import.py::test_mail_already_in_the_mailbox_is_skipped_while_import_duplicates_is_off` | linux (linux): passed, tui (linux): passed |
| 42 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips` | nest (linux): passed |
| 43 | nest | `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_a_message_filed_under_several_labels_lands_only_once` | nest (linux): passed |
| 44 | nest | `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_imported_mail_is_there_over_imap_carrying_only_marks_the_nest_wrote` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_imported_mail_rests_sealed_on_the_nest` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/api/test_mail_import_nest_api.py::test_an_import_the_nest_cannot_seal_is_refused_and_stores_nothing` | nest (linux): passed |
| 46 | nest | `tests/e2e-unified/tests/api/test_mail_quota_composition_api.py::test_imported_mail_counts_against_your_mailbox_storage` | nest (linux): passed |
| 47 | nest | `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_imported_mail_is_there_over_imap_carrying_only_marks_the_nest_wrote` | nest (linux): passed |
| 48 | nest | `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_an_import_untouched_for_thirty_days_is_forgotten` | nest (linux): passed |
| 49 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_mailbox_the_provider_rebuilt_is_warned_about_and_restarted_without_importing_twice` | linux (linux): passed, tui (linux): passed |
| 50 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_message_over_the_users_size_limit_is_skipped_as_too_large` | linux (linux): passed, tui (linux): passed |
| 51 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_failed_connection_is_explained_in_plain_words_never_the_servers_reply` | tui (linux): passed |
| 52 | app | `tests/e2e-unified/tests/test_mail_import.py::test_discard_and_duplicate_mailboxes_start_unticked_and_are_never_hidden` | tui (linux): passed |
| 53 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 54 | nest | `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_mail_delivered_to_you_is_not_copied_a_second_time_by_an_import` | nest (linux): passed |
| 54 | nest | `tests/e2e-unified/tests/api/test_mail_import_inbound_seam_api.py::test_mail_you_sent_is_not_copied_a_second_time_by_an_import` | nest (linux): passed |
| 55 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_full_mailbox_pauses_and_skip_the_rest_finishes_without_reading_further` | linux (linux): passed, tui (linux): passed |
| 55 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_full_mailbox_pauses_and_resume_after_the_admin_makes_room_brings_the_rest` | linux (linux): passed, tui (linux): passed |
| 56 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_failed_import_shows_as_failed_after_a_restart_and_dismiss_clears_it` | linux (linux): passed, tui (linux): passed |
| 57 | app | `tests/e2e-unified/tests/test_mail_import_microsoft_registration.py::test_an_admin_set_microsoft_registration_is_the_client_id_the_sign_in_sends` | tui (linux): passed |
| 57 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_microsoft_registration_save_round_trips` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
