---
slug: nest-rules
title: nest-rules
section: your nest
goal: docs/goal/behavior/nest-rules.md § Goal
guide: docs/guides/admin-tour.md § Nest
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] An admin writes a rule and the reason members will read, saves it, and sees which version of the nest's rules is now in force — `docs/goal/behavior/nest-rules.md` § The shared rule editor
   - `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced`
2. [app] Something a nest rule blocks is replaced, for every member, by a notice naming the nest and the admin's reason, and nothing lets a member show it anyway — `docs/goal/behavior/nest-rules.md` § The nest content policy
   - `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced`
3. [app] Settings shows the rules your nest has in force, with their version — `docs/goal/behavior/nest-rules.md` § Acceptance at joining, and a change of rules
   - `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced`
4. [app] When the admin changes the rules, members who are connected are told in a one-line notice that leads to them — `docs/goal/behavior/nest-rules.md` § Acceptance at joining, and a change of rules
   - `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced`
5. [app] Before you join, the joining page shows the nest's rules, each with its reason, just above the terms notice — `docs/goal/behavior/nest-rules.md` § Acceptance at joining, and a change of rules
   - `tests/e2e-unified/tests/test_nest_rules.py::test_joining_shows_the_nests_rules_with_the_terms_notice`
6. [nest] The nest's public web pages show the same notice in place of what its rules block — `docs/goal/behavior/nest-rules.md` § The nest content policy
   - `tests/e2e-unified/tests/api/test_nest_rules_web_notice.py::test_the_public_web_pages_show_the_rules_notice_in_place_of_what_they_block`
7. [app] A rule this app cannot apply — written in a newer rule language, or needing a newer app — is listed as not applied on this device, at joining and in Settings — `docs/goal/behavior/nest-rules.md` § Acceptance at joining, and a change of rules
   - `tests/e2e-unified/tests/test_nest_rules.py::test_a_rule_this_device_cannot_apply_is_listed_as_not_applied_at_joining_and_in_settings`
8. [app] A nest rule set to collapse folds matching content behind a reveal instead of hiding it outright — `docs/goal/behavior/nest-rules.md` § The nest content policy
   - `tests/e2e-unified/tests/test_nest_rules.py::test_a_nest_collapse_rule_folds_the_post_behind_a_reveal`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.6-dev+8438e5ce standalone |
| linux | ⚠ partial | 0.1.5-dev+0cfdd6c6 standalone |
| windows | ⚠ partial | 0.1.5-dev+ccd7fba0.dirty standalone |
| macos | ⚠ partial | 0.1.5-dev+76b81e1c standalone |
| ios | ⚠ partial | 0.1.5-dev+76b81e1c standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+8438e5ce standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_joining_shows_the_nests_rules_with_the_terms_notice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_nest_rules_web_notice.py::test_the_public_web_pages_show_the_rules_notice_in_place_of_what_they_block` | nest (linux): passed, nest (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_a_rule_this_device_cannot_apply_is_listed_as_not_applied_at_joining_and_in_settings` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_a_nest_collapse_rule_folds_the_post_behind_a_reveal` | web (linux): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
