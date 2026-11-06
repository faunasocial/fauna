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

Stamped 2026-10-27 at 7942c4870f.

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

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+3b591c2e standalone |
| linux | ✅ full | 0.1.5-dev+3b591c2e standalone |
| windows | ✅ full | 0.1.5-dev+3b591c2e standalone |
| macos | ✅ full | 0.1.5-dev+76b81e1c standalone |
| ios | ✅ full | 0.1.5-dev+76b81e1c standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+3b591c2e standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_an_admins_keyword_rule_blocks_a_post_visibly_and_a_change_is_announced` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_rules.py::test_joining_shows_the_nests_rules_with_the_terms_notice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_nest_rules_web_notice.py::test_the_public_web_pages_show_the_rules_notice_in_place_of_what_they_block` | nest (linux): passed, nest (macos): passed |
<!-- features-render:end -->
