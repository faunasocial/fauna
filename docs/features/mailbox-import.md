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

Stamped 2026-10-01 at dfa27a899f.

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
   - (none)
7. [app] Before anything is copied you see a summary of the mailboxes about to come over — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
8. [app] While it runs you see how far it has got, overall and per mailbox, with the reason for every message it skipped or could not bring — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - (none)
9. [app] You can pause and resume an import, or cancel it and keep everything that already arrived — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
   - (none)
10. [app] Closing the app mid-import and opening it again brings you back to the same import, where it left off — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
11. [app] When it finishes you see how many messages were imported, skipped and failed, and can jump to your inbox or to the list of skipped messages — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source`
12. [app] Your mail keeps its shape: each mailbox lands as the matching mailbox, with its original dates and read, flagged and answered marks — `docs/goal/behavior/mailbox-migration.md` § Goal
    - (none)
13. [nest] Mail brought in by an earlier import, or saved into your mailbox from a mail app, is not copied a second time, and the skip is counted in the import's progress — `docs/goal/behavior/mailbox-migration.md` § Dedup vs. existing mail
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_dedup_skip_is_reported_in_the_progress_push`
    - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips`
14. [nest] Your old provider's password never reaches your nest — `docs/goal/behavior/mailbox-migration.md` § Credential handling
    - (none)
15. [nest] Word of how the import is going reaches your own apps and nobody else's — `docs/goal/behavior/mailbox-migration.md` § Progress lives nest-side
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_message_emits_progress_push`
    - `tests/e2e-unified/tests/api/test_import_push_api.py::test_pushes_are_scoped_to_the_importing_actor`
16. [app] Choosing a provider that needs an app password explains that it does and where to make one — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
17. [app] An Outlook, Hotmail or Office 365 account can be connected by signing in with Microsoft, and a personal account that cannot do that still connects with its username and password — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
18. [nest] A Microsoft sign-in used for an import never reaches your nest, and is thrown away when the import ends — `docs/goal/behavior/mailbox-migration.md` § Credential handling
    - (none)
19. [app] The app never offers an unencrypted connection to your old provider — `docs/goal/behavior/mailbox-migration.md` § The two TLS modes, and where each half lives
    - (none)
20. [app] If the connection to your old provider cannot be proven secure — its certificate does not check out, or it refuses to switch to an encrypted connection — the app stops before your password is sent — `docs/goal/behavior/mailbox-migration.md` § The two TLS modes, and where each half lives
    - (none)
21. [app] You can set the largest message to bring over: it starts at 50 MB and you can lower it — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
22. [app] A message larger than your size limit is skipped and listed as too large — `docs/goal/behavior/mailbox-migration.md` § Per-message flow
    - (none)
23. [app] A mailbox in your old account with no matching mailbox here is created for you under its original name — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
24. [app] The summary before the import says how many messages and roughly how much data are coming — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
25. [app] While an import runs you see a progress bar, how long it has been running and an estimate of the time left — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
26. [app] Pausing and resuming an import, or reopening the app in the middle of one, keeps the date you chose: it never quietly switches to importing everything — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - (none)
27. [app] Cancelling an import asks you to confirm first — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
28. [app] To pick an import up after the app was closed you type your old provider's password again: the app never keeps it — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - (none)
29. [app] The app remembers your old provider's server, port and connection security for the next time, until you delete them — `docs/goal/behavior/mailbox-migration.md` § Credential handling
    - (none)
30. [app] Starting a second import of the same old account on another device is refused with a message saying it is already being imported elsewhere — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - (none)
31. [app] Your old mailbox is left exactly as it was: importing marks nothing read and changes nothing there — `docs/goal/behavior/mailbox-migration.md` § Where the IMAP client runs — the transport seam
    - (none)
32. [app] If your old provider stops accepting your password part-way through, the import stops, says why, and offers to try again with new credentials — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - (none)
33. [app] If one of your old mailboxes disappears during the import, that is noted in the skipped list and the import carries on with the next one — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - (none)
34. [app] A message that cannot be read is counted as failed and the rest of the import carries on — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - (none)
35. [app] If the connection to your old provider drops, the app retries a few times and then pauses the import, which you resume from where it stopped once the connection is back — `docs/goal/behavior/mailbox-migration.md` § Source-side errors
    - (none)
36. [app] Losing the connection to your nest during an import loses nothing: it continues from the last message your nest saved — `docs/goal/behavior/mailbox-migration.md` § Nest-side errors
    - (none)
37. [app] An import that keeps failing stops itself, with the reason, rather than grinding on — `docs/goal/behavior/mailbox-migration.md` § Per-message error budget
    - (none)
38. [nest] Whether a message is one you already have is decided against your own mail only: another person's mail on the same nest never causes a skip, and never shows that they hold it — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - (none)
39. [nest] A stranger who sends you mail reusing the identifier of a message you have not yet imported cannot make the real message be skipped — `docs/goal/behavior/mailbox-migration.md` § The envelope key confirms a Message-ID hit
    - (none)
40. [nest] A copy of a message that was changed on the way — a footer added, the subject tagged — is imported as the separate message it now is, not skipped — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - (none)
41. [app] An advanced option to import duplicates anyway, off unless you turn it on, warns you that it stores mail you already have a second time — `docs/goal/behavior/mailbox-migration.md` § Opt-out per session
    - (none)
42. [nest] With duplicates allowed, a message you already have is imported again instead of skipped — `docs/goal/behavior/mailbox-migration.md` § Opt-out per session
    - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips`
43. [nest] A message your old provider filed under several labels lands only once — `docs/goal/behavior/mailbox-migration.md` § Don't do these
    - (none)
44. [nest] An imported message cannot carry in a spam verdict or an alias match your nest never made: those marks are removed as it arrives — `docs/goal/behavior/mailbox-migration.md` § Nest-side sealing
    - (none)
45. [nest] Imported mail rests sealed on your nest, and an import your nest cannot seal for you is refused rather than stored readable — `docs/goal/behavior/mailbox-migration.md` § Per-message flow
    - (none)
46. [nest] Imported mail counts against your mailbox storage — `docs/goal/behavior/mailbox-migration.md` § Quota composition
    - (none)
47. [nest] Imported mail is ordinary mail: your other apps, and any mail app you use, see it once it lands — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - (none)
48. [nest] An import left untouched for thirty days is forgotten, and running the wizard again skips what already arrived — `docs/goal/behavior/mailbox-migration.md` § Architectural rules
    - (none)
49. [app] If your old provider rebuilt a mailbox since the import began, you are warned and offered to restart the import, rather than having it carry on against the wrong messages — `docs/goal/behavior/mailbox-migration.md` § Resume protocol
    - (none)
50. [app] The finished import also shows how long it took — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
51. [app] A failed connection is explained in plain words, with a hint for your provider, never in your old server's raw reply — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
52. [app] Every mailbox starts ticked except the ones that hold discards or duplicates — Trash, Junk or Spam, and Gmail's All Mail, Bin, Important and Starred — which start unticked and can be ticked — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
53. [app] Nothing is imported until you confirm the summary — `docs/goal/behavior/mailbox-migration.md` § Wizard steps
    - (none)
54. [nest] Mail that was delivered to you, or that you sent, is not copied a second time by an import — `docs/goal/behavior/mailbox-migration.md` § Dedup scope
    - (none)
55. [app] When your mailbox runs out of room part-way through, the import pauses and asks whether to wait for more room, skip the rest, or cancel — `docs/goal/behavior/mailbox-migration.md` § Nest-side errors
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android | ⚠ partial | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_import.py::test_the_import_wizard_is_reachable_and_opens_on_the_source_step` | linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_mail_import.py::test_selecting_a_source_kind_seeds_the_host_and_port_drafts` | linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_mail_import.py::test_a_rejected_password_is_surfaced_and_the_user_retries_in_place` | linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 6 | app | (none) | — |
| 7 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | app | (none) | — |
| 10 | app | (none) | — |
| 11 | app | `tests/e2e-unified/tests/test_mail_import.py::test_an_import_walks_source_to_done_and_really_reads_the_source` | linux (linux): passed, tui (linux): passed |
| 12 | app | (none) | — |
| 13 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_dedup_skip_is_reported_in_the_progress_push` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips` | nest (linux): passed |
| 14 | nest | (none) | — |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_import_message_emits_progress_push` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_import_push_api.py::test_pushes_are_scoped_to_the_importing_actor` | nest (linux): passed |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
| 18 | nest | (none) | — |
| 19 | app | (none) | — |
| 20 | app | (none) | — |
| 21 | app | (none) | — |
| 22 | app | (none) | — |
| 23 | app | (none) | — |
| 24 | app | (none) | — |
| 25 | app | (none) | — |
| 26 | app | (none) | — |
| 27 | app | (none) | — |
| 28 | app | (none) | — |
| 29 | app | (none) | — |
| 30 | app | (none) | — |
| 31 | app | (none) | — |
| 32 | app | (none) | — |
| 33 | app | (none) | — |
| 34 | app | (none) | — |
| 35 | app | (none) | — |
| 36 | app | (none) | — |
| 37 | app | (none) | — |
| 38 | nest | (none) | — |
| 39 | nest | (none) | — |
| 40 | nest | (none) | — |
| 41 | app | (none) | — |
| 42 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_append_populates_dedup_index_so_a_later_import_skips` | nest (linux): passed |
| 43 | nest | (none) | — |
| 44 | nest | (none) | — |
| 45 | nest | (none) | — |
| 46 | nest | (none) | — |
| 47 | nest | (none) | — |
| 48 | nest | (none) | — |
| 49 | app | (none) | — |
| 50 | app | (none) | — |
| 51 | app | (none) | — |
| 52 | app | (none) | — |
| 53 | app | (none) | — |
| 54 | nest | (none) | — |
| 55 | app | (none) | — |
<!-- features-render:end -->
