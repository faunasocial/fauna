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

Stamped 2026-10-01 at dfa27a899f.

1. [app] Export your mailbox from the wizard and get an archive holding the mail the account actually has — `docs/goal/behavior/mail-export.md` § Goal
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done`
2. [app] The export wizard is reachable from mail settings — `docs/goal/behavior/mail-export.md` § UX shape
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_wizard_reachable`
3. [app] Nobody else can read or download your export, and the server never holds a key to the archive it stores for you — `docs/goal/behavior/mail-export.md` § Sealed-blob delivery
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_archive_is_sealed_to_its_owner`
4. [app] A truncated or still-running archive is refused rather than saved as if it were your whole mailbox — `docs/goal/behavior/mail-export.md` § Download flow
   - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_truncated_or_unfinished_archive`
5. [app] You choose which mailboxes to export: every mailbox but Trash and Junk starts selected, and one you leave out is not in the archive — `docs/goal/behavior/mail-export.md` § Wizard steps
   - (none)
6. [app] You can narrow the export to a date range: each end takes in its whole day, and a message is placed by when it arrived in your mailbox, not by the date its sender wrote — `docs/goal/behavior/mail-export.md` § Wizard steps
   - (none)
7. [app] A date that is not a real day, or a start date after the end date, is refused before the export starts, never quietly treated as no range at all — `docs/goal/behavior/mail-export.md` § Wizard steps
   - (none)
8. [app] Transit headers are kept unless you ask to strip them, and stripping removes every header a relaying or receiving server stamped on a message to record the path it took, in whichever format you chose — `docs/goal/behavior/mail-export.md` § Wizard steps
   - (none)
9. [app] Stripping the transit headers leaves everything the sender wrote, the sender's own signature included, so a shared archive can still be checked as unaltered — `docs/goal/behavior/mail-export.md` § Wizard steps
   - (none)
10. [app] Before anything starts you see how many messages are about to be exported, roughly how large the archive will be and roughly how long it will take, and nothing starts until you confirm — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
11. [app] While an export runs you see how many messages are exported, skipped and failed, with the time elapsed and the time remaining — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
12. [app] While an export runs you see each mailbox's own progress, and the reason for every message that could not be exported — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
13. [app] You can pause an export and resume it later — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
14. [app] Cancelling an export asks you to confirm, and a cancelled export leaves no partial archive behind on your nest — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
15. [app] Closing the app mid-export and opening it again brings you back to the same export's progress — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
16. [app] Resuming an export in an app that was not running it — after a restart, or on another of your devices — runs it again from the first message, as the same export — `docs/goal/behavior/mail-export.md` § Resume — warm continues the stream, cold restarts it (ratified 2026-09-21)
    - (none)
17. [app] An app that is not running the export does not pause it, so it never stalls the device that is, while any of your devices can cancel it — `docs/goal/behavior/mail-export.md` § Resume — warm continues the stream, cold restarts it (ratified 2026-09-21)
    - (none)
18. [app] When an export finishes you see how many messages were exported, how long it took and how large the archive is — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
19. [app] Discard removes a finished export from your nest at once, without waiting for it to expire — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
20. [app] A finished export can be downloaded and opened from any of your devices, not only the one that ran it — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
21. [app] An export made before your mail keys were rotated still opens afterwards — `docs/goal/behavior/mail-export.md` § Key material
    - (none)
22. [app] What you download is one file, a zip inside a single layer of zstd compression, which any file manager opens once it is decompressed — `docs/goal/behavior/mail-export.md` § Container shape — zip inside zstd (pinned 2026-09-20)
    - `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done`
23. [app] The archive's file name says whose mailbox it is, in which format and from which day — `docs/goal/behavior/mail-export.md` § Compression wrapper
    - (none)
24. [app] The same mailbox, the same choices and the same format always give the same archive, byte for byte apart from embedded timestamps, so you can check one export against another — `docs/goal/behavior/mail-export.md` § Goal
    - (none)
25. [app] Every message comes with its mailbox, its arrival date and its read, flagged, answered and draft marks, in whichever format you chose — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
26. [app] Two mailboxes whose names differ only in capital letters, or one named like a Windows device, still unpack as separate, usable folders on any computer — `docs/goal/behavior/mail-export.md` § Archive paths on the extracting filesystem — the escape ladder (pinned 2026-09-21)
    - (none)
27. [app] Unpacking the archive never writes anything outside the folder you unpack it into, whatever your mailboxes are called — `docs/goal/behavior/mail-export.md` § Mailbox names in archive paths — one injective encoding (pinned 2026-09-21)
    - (none)
28. [app] If a message cannot be opened, the whole export fails and says which mailbox and which message, rather than handing you an archive quietly missing it — `docs/goal/behavior/mail-export.md` § An unopenable record fails the session (ratified 2026-09-21)
    - (none)
29. [app] An export that failed shows as failed, with the reason, on your other devices and after a restart — `docs/goal/behavior/mail-export.md` § Resume — warm continues the stream, cold restarts it (ratified 2026-09-21)
    - (none)
30. [nest] A finished export stays downloadable for thirty days and is then removed, after which its link answers not found — `docs/goal/behavior/mail-export.md` § Expiry
    - (none)
31. [nest] An export larger than the per-export size limit stops with a refusal that says to narrow it or split it — `docs/goal/behavior/mail-export.md` § Quota composition
    - (none)
32. [nest] Only so many of your exports run at once, three by default, and another is refused until one finishes, fails or is cancelled — `docs/goal/behavior/mail-export.md` § Quota composition
    - (none)
33. [nest] Finished exports you have not discarded count toward a per-person disk allowance, and a new export is refused once they fill it, until you discard one — `docs/goal/behavior/mail-export.md` § Quota composition
    - (none)
34. [nest] Exporting never counts against your mailbox storage — `docs/goal/behavior/mail-export.md` § Quota composition
    - (none)
35. [nest] An admin cannot start an export of someone else's mailbox — `docs/goal/behavior/mail-export.md` § Cross-actor isolation
    - (none)
36. [app] Every download of your export leaves you a security notice naming the format and the address it was downloaded from — `docs/goal/behavior/mail-export.md` § Download flow
    - (none)
37. [nest] A suspended or locked-out account cannot download an export, even with a sign-in it still holds — `docs/goal/behavior/mail-export.md` § Download flow
    - (none)
38. [nest] Deleting your account deletes your exports with it — `docs/goal/behavior/mail-export.md` § Reclaim — the row is the authority, the file follows
    - (none)
39. [nest] After you move to a new identity, exports made under the old one are gone, and you export again under the new one — `docs/goal/behavior/mail-export.md` § Reclaim — the row is the authority, the file follows
    - (none)
40. [app] The wizard offers three archive formats — mbox, Maildir++ and a zip of `.eml` files — with mbox chosen unless you pick another — `docs/goal/behavior/mail-export.md` § Wizard steps
    - (none)
41. [app] When you go to delete your account, the confirmation reminds you to export your mail first — `docs/goal/ui/settings.md` § Layout & flow
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+765e666a.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+15642c38.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+05f6e2ef standalone |
| ios | ⚠ partial | 0.1.2-dev+2924f9f9 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_wizard_reachable` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_archive_is_sealed_to_its_owner` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_refuses_a_truncated_or_unfinished_archive` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | (none) | — |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | app | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
| 18 | app | (none) | — |
| 19 | app | (none) | — |
| 20 | app | (none) | — |
| 21 | app | (none) | — |
| 22 | app | `tests/e2e-unified/tests/test_mail_export.py::test_mail_export_runs_to_done` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 23 | app | (none) | — |
| 24 | app | (none) | — |
| 25 | app | (none) | — |
| 26 | app | (none) | — |
| 27 | app | (none) | — |
| 28 | app | (none) | — |
| 29 | app | (none) | — |
| 30 | nest | (none) | — |
| 31 | nest | (none) | — |
| 32 | nest | (none) | — |
| 33 | nest | (none) | — |
| 34 | nest | (none) | — |
| 35 | nest | (none) | — |
| 36 | app | (none) | — |
| 37 | nest | (none) | — |
| 38 | nest | (none) | — |
| 39 | nest | (none) | — |
| 40 | app | (none) | — |
| 41 | app | (none) | — |
<!-- features-render:end -->
