---
slug: privacy-settings
title: Privacy: who can reach you, and your spam threshold
section: your data and devices
goal: docs/goal/ui/settings.md § User actions
guide: docs/guides/who-can-see-what.md § The short version
---

## What a user gets

Choose who may start a conversation with you: anyone, contacts only, or nobody.
Set how aggressive your spam filter is and whether your training helps others. Both
persist across relaunch and show the real value, never a default — and while your
nest is still being asked, the page says so rather than marking a guess.

## Coverage contract

Stamped 2026-10-01 at d8cb0887cb.

1. [app] Your inbox mode persists and shows the real value after a relaunch — `docs/goal/ui/settings.md` § User actions
   - `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_toggle`
   - `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch`
2. [app] Your spam preferences save and survive reopening the page — `docs/goal/ui/settings.md` § Spam threshold slider labels
   - `tests/e2e-unified/tests/test_settings.py::test_spam_preferences`
   - `tests/e2e-unified/tests/test_settings.py::test_spam_preferences_persist_web`
3. [nest] Your nest keeps your inbox mode — `docs/goal/behavior/direct-messages.md` § Reach policy
   - `tests/e2e-unified/tests/api/test_contacts_api.py::test_inbox_mode_api`
4. [app] While your nest is still being asked, the page never marks a setting you did not choose — `docs/goal/ui/settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_is_unknown_while_its_fetch_is_still_pending`
   - `tests/e2e-unified/tests/test_settings.py::test_privacy_nav_names_no_mode_while_the_mode_read_is_held`
5. [nest] A stranger cannot start a conversation with you unless your inbox mode allows it, while people you have accepted always get through — `docs/goal/behavior/direct-messages.md` § Reach policy
   - `tests/e2e-unified/tests/api/test_conversation_reach_policy.py::test_a_stranger_is_refused_until_the_recipient_accepts_them`
6. [nest] With your inbox closed, someone on another nest cannot reach you either — nothing of theirs lands on your side — `docs/goal/behavior/direct-messages.md` § Reach policy
   - `tests/e2e-unified/tests/api/test_conversation_reach_policy.py::test_a_closed_inbox_holds_against_a_sender_on_another_nest`
7. [app] Opening the privacy page never changes the setting it shows you — `docs/goal/ui/settings.md` § Layout & flow
   - `tests/e2e-unified/tests/test_settings.py::test_opening_the_privacy_page_never_writes_the_inbox_mode_back`
8. [app] The spam filter's strength reads back in words — aggressive, moderate or permissive — for the value you chose — `docs/goal/ui/settings.md` § Spam threshold slider labels
   - (none)
9. [app] Content flagged above your own spam threshold is collapsed in your own view — `docs/goal/behavior/moderation.md` § Categories & enforcement
   - `tests/e2e-unified/tests/test_family.py::test_own_spam_threshold_collapses_flagged_conversation_message`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+4ca6f9ab standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+6d8dc256.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_toggle` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_pre_selects_the_accounts_real_mode_after_a_relaunch` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings.py::test_spam_preferences` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_settings.py::test_spam_preferences_persist_web` | web (linux): passed, linux (linux): skipped, windows (windows): skipped, macos (macos): skipped, ios (macos): skipped, tui (linux): error |
| 3 | nest | `tests/e2e-unified/tests/api/test_contacts_api.py::test_inbox_mode_api` | nest (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_settings.py::test_inbox_mode_is_unknown_while_its_fetch_is_still_pending` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): skipped |
| 4 | app | `tests/e2e-unified/tests/test_settings.py::test_privacy_nav_names_no_mode_while_the_mode_read_is_held` | tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_conversation_reach_policy.py::test_a_stranger_is_refused_until_the_recipient_accepts_them` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_conversation_reach_policy.py::test_a_closed_inbox_holds_against_a_sender_on_another_nest` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_settings.py::test_opening_the_privacy_page_never_writes_the_inbox_mode_back` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_family.py::test_own_spam_threshold_collapses_flagged_conversation_message` | linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
