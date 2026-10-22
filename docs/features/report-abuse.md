---
slug: report-abuse
title: Report a post, a message or an account
section: family and personalization
goal: docs/goal/behavior/moderation.md § User-initiated reporting
guide: docs/guides/app-tour.md § Profile
---

## What a user gets

Tell your nest's administrators about a post, a message someone sent you, or an
account you think they should see. You pick a reason, can add a note, and can block
the person in the same step. A private message stays private unless you tick the box
that attaches its text. When the person lives on another nest, your report is passed
on to that nest's administrators too, without your name. A report removes nothing by itself, and the person you report is never told who reported them; what changes is your own view, where what you reported is replaced by a note that you reported it. You are told what became of your report, and the Moderation page (in the web app, a section of Settings → Privacy) lists your reports and lets you withdraw one while it is still open.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] You can report someone else's post from its menu, picking a reason and adding a note if you wish; a report with no reason cannot be sent — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
2. [app] You can report an account from its profile — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn`
3. [app] You can report a message someone sent you from that message's menu — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked`
4. [app] The text of a private message or a restricted post reaches the administrators only if you tick the box that attaches it, and the box says they will be able to read it — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked`
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked`
5. [app] You can block the person in the same step as reporting them — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_a_post_can_block_its_author_in_the_same_step`
6. [app] Reporting is not offered on your own posts, your own messages or your own profile — `docs/goal/behavior/moderation.md` § App surface
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_is_not_offered_on_your_own_post_or_profile`
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked`
7. [app] Once sent, a report is acknowledged, naming the administrators it went to — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
8. [app] When the person you report lives on another nest, the acknowledgement names that nest too and says your name was not passed on — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_on_a_foreign_post_reaches_its_home_nest_and_the_outcome_returns`
9. [app] What you reported stops showing for you, with a note that you reported it in its place — `docs/goal/behavior/moderation.md` § Corollary
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
10. [app] Reporting an account hides everything that account wrote from your own view — `docs/goal/behavior/moderation.md` § Corollary
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_an_account_hides_everything_it_wrote`
11. [app] When an administrator resolves your report, you are told only whether it was acted on or dismissed, and your list of reports shows the same — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
12. [nest] The person you report is never told that they were reported, or by whom — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_the_reported_author_is_told_nothing`
13. [nest] A report removes, hides, labels or demotes nothing for anyone but you, and counts toward no reputation or shared spam signal — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_report_changes_nothing_for_anyone_but_the_reporter`
14. [nest] You can send only a limited number of reports an hour, and have one open report about the same thing at a time — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_reporter_is_rate_limited_and_holds_one_open_report_per_subject`
15. [nest] A supervised account's report goes through like anyone else's, with no guardian's approval — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_supervised_accounts_report_needs_no_guardian`
16. [nest] A report about someone on another nest is passed on to that nest's administrators without your identity — `docs/goal/behavior/moderation.md` § Routing
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_forwarded_report_names_no_reporter_and_its_outcome_comes_back`
17. [nest] A nest that cannot be reached when you report still receives the report once it is back, for up to seven days — `docs/goal/behavior/moderation.md` § Routing
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_report_reaches_a_home_nest_that_was_down_once_it_is_back`
18. [nest] A nest takes a passed-on report only about content or accounts it hosts — `docs/goal/behavior/moderation.md` § Routing
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_nest_refuses_a_forwarded_report_about_a_subject_it_does_not_host`
19. [nest] For a report passed on to another nest, the outcome its administrators record comes back to you — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_forwarded_report_names_no_reporter_and_its_outcome_comes_back`
20. [nest] Withdrawing a report deletes your note and any text you attached, everywhere the report went — `docs/goal/behavior/moderation.md` § Where it lands
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_withdrawing_a_report_deletes_its_note_and_excerpt_on_both_nests`
21. [nest] Deleting your account withdraws every report of yours that is still open, and leaves the resolved ones as the record of what was decided — `docs/goal/behavior/moderation.md` § Where it lands
   - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_deleting_the_reporters_account_withdraws_open_reports_and_keeps_resolved_ones`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.3-dev+60888d4e standalone |
| macos | ⚠ partial | 0.1.3-dev+a5d2dc2b standalone |
| ios | ⚠ partial | 0.1.3-dev+a5d2dc2b standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.3-dev+59631bd4.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked` | web (linux): passed, linux (linux): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked` | web (linux): passed, linux (linux): failed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_a_post_can_block_its_author_in_the_same_step` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_is_not_offered_on_your_own_post_or_profile` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_received_message_is_reported_from_its_menu_and_its_text_goes_only_if_ticked` | web (linux): passed, linux (linux): failed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_on_a_foreign_post_reaches_its_home_nest_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_reporting_an_account_hides_everything_it_wrote` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_the_reported_author_is_told_nothing` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_report_changes_nothing_for_anyone_but_the_reporter` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_reporter_is_rate_limited_and_holds_one_open_report_per_subject` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_supervised_accounts_report_needs_no_guardian` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_forwarded_report_names_no_reporter_and_its_outcome_comes_back` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_report_reaches_a_home_nest_that_was_down_once_it_is_back` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_nest_refuses_a_forwarded_report_about_a_subject_it_does_not_host` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_a_forwarded_report_names_no_reporter_and_its_outcome_comes_back` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_withdrawing_a_report_deletes_its_note_and_excerpt_on_both_nests` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_deleting_the_reporters_account_withdraws_open_reports_and_keeps_resolved_ones` | nest (linux): passed |
<!-- features-render:end -->
