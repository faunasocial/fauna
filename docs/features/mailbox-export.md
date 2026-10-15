---
slug: mailbox-export
title: Take your mailbox with you
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-export.md § Goal
guide: docs/guides/own-your-mail.md § Taking your mail with you
---

## What a user gets

Walk a five-step wizard and your whole mailbox comes back to you as one archive
file — in mbox, Maildir++ or a folder of `.eml` files, whichever your next mail
app reads. Pick which mailboxes, narrow it to a date range, and strip the transit
headers if you would rather not carry them. Nobody who runs the server can read
the archive on its way to you: it is sealed to a key only your app holds, and it
stays that way until it lands on your own disk.

## Coverage contract

Stamped 2026-10-12 at 670184dce8.

1. [app] Export your mailbox from the wizard and get an archive holding the mail the account actually has — `docs/goal/behavior/mail-export.md` § Goal
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done`
2. [app] The export wizard is reachable from mail settings — `docs/goal/behavior/mail-export.md` § UX shape
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_wizard_reachable`
3. [app] Nobody else can read or download your export, and the server never holds a key to the archive it stores for you — `docs/goal/behavior/mail-export.md` § Sealed-blob delivery
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_archive_is_sealed_to_its_owner`
4. [app] A truncated or still-running archive is refused rather than saved as if it were your whole mailbox — `docs/goal/behavior/mail-export.md` § Download flow
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_truncated_or_unfinished_archive`
5. [app] You choose which mailboxes to export: every mailbox but Trash and Junk starts selected, and one you leave out is not in the archive — `docs/goal/behavior/mail-export.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_scope_starts_without_trash_and_junk_and_leaves_out_a_deselected_mailbox`
6. [app] You can narrow the export to a date range: each end takes in its whole day, and a message is placed by when it arrived in your mailbox, not by the date its sender wrote — `docs/goal/behavior/mail-export.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_date_range_takes_whole_days_by_arrival_and_discard_removes_the_export`
7. [app] A date that is not a real day, or a start date after the end date, is refused before the export starts, never quietly treated as no range at all — `docs/goal/behavior/mail-export.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_date_that_is_not_a_day_or_a_range_that_runs_backwards`
8. [app] Transit headers are kept unless you ask to strip them, and stripping removes every header a relaying or receiving server stamped on a message to record the path it took, in whichever format you chose — `docs/goal/behavior/mail-export.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders`
9. [app] Stripping the transit headers leaves everything the sender wrote, the sender's own signature included, so a shared archive can still be checked as unaltered — `docs/goal/behavior/mail-export.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders`
10. [app] Before anything starts you see how many messages are about to be exported, roughly how large the archive will be and roughly how long it will take, and nothing starts until you confirm — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_confirm_estimates_size_and_time_before_anything_starts`
11. [app] While an export runs you see how many messages are exported, skipped and failed, with the time elapsed and the time remaining — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes`
12. [app] While an export runs you see each mailbox's own progress, and the reason for every message that could not be exported — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_failure_is_shown_with_its_reason_in_the_error_log`
13. [app] You can pause an export and resume it later — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes`
14. [app] Cancelling an export asks you to confirm, and a cancelled export leaves no partial archive behind on your nest — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app`
15. [app] Closing the app mid-export and opening it again brings you back to the same export's progress — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app`
16. [app] Resuming an export in an app that was not running it — after a restart, or on another of your devices — runs it again from the first message, as the same export — `docs/goal/behavior/mail-export.md` § Resume — warm continues the stream, cold restarts it (ratified 2026-09-21)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_cannot_pause_it_and_resuming_there_restarts_it`
17. [app] An app that is not running the export does not pause it, so it never stalls the device that is, while any of your devices can cancel it — `docs/goal/behavior/mail-export.md` § Resume — warm continues the stream, cold restarts it (ratified 2026-09-21)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_cannot_pause_it_and_resuming_there_restarts_it`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_can_cancel_it_and_no_partial_archive_remains`
18. [app] When an export finishes you see how many messages were exported, how long it took and how large the archive is — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes`
19. [app] Discard removes a finished export from your nest at once, without waiting for it to expire — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_date_range_takes_whole_days_by_arrival_and_discard_removes_the_export`
20. [app] A finished export can be downloaded and opened from any of your devices, not only the one that ran it — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_finished_on_one_device_downloads_and_opens_on_another`
21. [app] An export made before your mail keys were rotated still opens afterwards — `docs/goal/behavior/mail-export.md` § Key material
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_an_export_made_before_a_key_rotation_still_opens_after_it`
22. [app] What you download is one file, a zip inside a single layer of zstd compression, which any file manager opens once it is decompressed — `docs/goal/behavior/mail-export.md` § Container shape — zip inside zstd (pinned 2026-09-20)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done`
23. [app] The archive's file name says whose mailbox it is, in which format and from which day — `docs/goal/behavior/mail-export.md` § Compression wrapper
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates`
24. [app] The same mailbox, the same choices and the same format always give the same archive, byte for byte apart from embedded timestamps, so you can check one export against another — `docs/goal/behavior/mail-export.md` § Goal
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_the_same_export_twice_is_the_same_archive`
25. [app] Every message comes with its mailbox, its arrival date and its read, flagged, answered and draft marks, in whichever format you chose — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates`
26. [app] Two mailboxes whose names differ only in capital letters, or one named like a Windows device, still unpack as separate, usable folders on any computer — `docs/goal/behavior/mail-export.md` § Archive paths on the extracting filesystem — the escape ladder (pinned 2026-09-21)
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates`
27. [app] Unpacking the archive never writes anything outside the folder you unpack it into, whatever your mailboxes are called — `docs/goal/behavior/mail-export.md` § Mailbox names in archive paths — one injective encoding (pinned 2026-09-21)
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates`
28. [app] If a message cannot be opened, the whole export fails and says which mailbox and which message, rather than handing you an archive quietly missing it — `docs/goal/behavior/mail-export.md` § An unopenable record fails the session (ratified 2026-09-21)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_an_unopenable_message_fails_the_whole_export_naming_its_mailbox_and_message`
29. [app] An export that failed shows as failed, with the reason, on your other devices and after a restart — `docs/goal/behavior/mail-export.md` § A failed export is a record the wizard shows until the user moves on (ratified 2026-10-11)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_a_failed_export_shows_as_failed_after_a_restart_and_dismiss_clears_it`
30. [nest] A finished export stays downloadable for thirty days and is then removed, after which its link answers not found — `docs/goal/behavior/mail-export.md` § Expiry
    - `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_a_finished_export_is_kept_thirty_days_then_gone`
31. [nest] An export larger than the per-export size limit stops with a refusal that says to narrow it or split it — `docs/goal/behavior/mail-export.md` § Quota composition
    - `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_an_export_over_the_size_limit_is_refused`
32. [nest] Only so many of your exports run at once, three by default, and another is refused until one finishes, fails or is cancelled — `docs/goal/behavior/mail-export.md` § Quota composition
    - `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_only_three_exports_run_at_once_and_another_is_refused_until_one_ends`
33. [nest] Finished exports you have not discarded count toward a per-person disk allowance, and a new export is refused once they fill it, until you discard one — `docs/goal/behavior/mail-export.md` § Quota composition
    - `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_one_persons_exports_never_hold_more_than_the_per_person_footprint`
34. [nest] Exporting never counts against your mailbox storage — `docs/goal/behavior/mail-export.md` § Quota composition
    - `tests/e2e-unified/tests/api/test_mail_quota_composition_api.py::test_exporting_never_counts_against_your_mailbox_storage`
35. [nest] An admin cannot start an export of someone else's mailbox — `docs/goal/behavior/mail-export.md` § Cross-actor isolation
    - `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_an_admin_cannot_start_an_export_of_someone_elses_mailbox`
36. [app] Every download of your export leaves you a security notice naming the format and the address it was downloaded from — `docs/goal/behavior/mail-export.md` § Download flow
    - `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates`
37. [nest] A suspended or locked-out account cannot download an export, even with a sign-in it still holds — `docs/goal/behavior/mail-export.md` § Download flow
    - `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_a_suspended_or_locked_out_account_cannot_download_an_export_with_a_sign_in_it_holds`
38. [nest] Deleting your account deletes your exports with it — `docs/goal/behavior/mail-export.md` § Reclaim — the row is the authority, the file follows
    - `tests/e2e-unified/tests/api/test_mail_export_reclaim_api.py::test_deleting_your_account_deletes_your_exports_with_it`
39. [nest] After you move to a new identity, exports made under the old one are gone, and you export again under the new one — `docs/goal/behavior/mail-export.md` § Reclaim — the row is the authority, the file follows
    - `tests/e2e-unified/tests/api/test_mail_export_reclaim_api.py::test_after_moving_to_a_new_identity_the_old_exports_are_gone_and_you_export_again`
40. [app] The wizard offers three archive formats — mbox, Maildir++ and a zip of `.eml` files — with mbox chosen unless you pick another — `docs/goal/behavior/mail-export.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_offers_three_formats_with_mbox_chosen`
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders`
41. [app] When you go to delete your account, the confirmation reminds you to export your mail first — `docs/goal/ui/settings.md` § Layout & flow
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+765e666a.dirty standalone |
| linux | ⚠ partial | 0.1.4-dev+670184dc.dirty standalone |
| windows | ⚠ partial | 0.1.2-dev+15642c38.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+05f6e2ef standalone |
| ios | ⚠ partial | 0.1.2-dev+2924f9f9 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.4-dev+d6e5448b.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_wizard_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_archive_is_sealed_to_its_owner` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_truncated_or_unfinished_archive` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_scope_starts_without_trash_and_junk_and_leaves_out_a_deselected_mailbox` | linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_date_range_takes_whole_days_by_arrival_and_discard_removes_the_export` | linux (linux): failed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_date_that_is_not_a_day_or_a_range_that_runs_backwards` | linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders` | linux (linux): failed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders` | linux (linux): failed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_confirm_estimates_size_and_time_before_anything_starts` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes` | linux (linux): failed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes` | linux (linux): failed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_failure_is_shown_with_its_reason_in_the_error_log` | linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes` | linux (linux): failed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app` | linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here` | linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app` | linux (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here` | linux (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_cannot_pause_it_and_resuming_there_restarts_it` | linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_after_a_restart_returns_to_the_export_and_resume_restarts_it_here` | linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_cancel_asks_first_and_leaves_no_partial_archive_from_any_app` | linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_cannot_pause_it_and_resuming_there_restarts_it` | linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_another_device_can_cancel_it_and_no_partial_archive_remains` | linux (linux): failed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_progress_counts_and_times_it_then_pauses_resumes_and_finishes` | linux (linux): failed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_date_range_takes_whole_days_by_arrival_and_discard_removes_the_export` | linux (linux): failed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_finished_on_one_device_downloads_and_opens_on_another` | linux (linux): failed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_an_export_made_before_a_key_rotation_still_opens_after_it` | linux (linux): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates` | linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_the_same_export_twice_is_the_same_archive` | linux (linux): passed, tui (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates` | linux (linux): passed, tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates` | linux (linux): passed, tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates` | linux (linux): passed, tui (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_an_unopenable_message_fails_the_whole_export_naming_its_mailbox_and_message` | linux (linux): passed, tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_a_failed_export_shows_as_failed_after_a_restart_and_dismiss_clears_it` | web (linux): passed, linux (linux): failed, tui (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_a_finished_export_is_kept_thirty_days_then_gone` | nest (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_an_export_over_the_size_limit_is_refused` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_only_three_exports_run_at_once_and_another_is_refused_until_one_ends` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_mail_portability_hooks_api.py::test_one_persons_exports_never_hold_more_than_the_per_person_footprint` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/api/test_mail_quota_composition_api.py::test_exporting_never_counts_against_your_mailbox_storage` | nest (linux): passed |
| 35 | nest | `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_an_admin_cannot_start_an_export_of_someone_elses_mailbox` | nest (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_mail_export_archive.py::test_every_format_keeps_each_mailbox_apart_with_its_marks_and_dates` | linux (linux): passed, tui (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/api/test_mail_export_nest_api.py::test_a_suspended_or_locked_out_account_cannot_download_an_export_with_a_sign_in_it_holds` | nest (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/api/test_mail_export_reclaim_api.py::test_deleting_your_account_deletes_your_exports_with_it` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/api/test_mail_export_reclaim_api.py::test_after_moving_to_a_new_identity_the_old_exports_are_gone_and_you_export_again` | nest (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_offers_three_formats_with_mbox_chosen` | linux (linux): passed, tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_strip_removes_transit_headers_and_keeps_the_senders` | linux (linux): failed, tui (linux): passed |
| 41 | app | (none) | — |
<!-- features-render:end -->
