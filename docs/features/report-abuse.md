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
on to that nest's administrators too, without your name. A report removes nothing and
is never shown to the person you reported; what changes is your own view, where what
you reported is replaced by a note that you reported it. You are told what became of
your report, and your Moderation page lists your reports and lets you withdraw one
while it is still open.

## Coverage contract

Stamped 2026-10-01 at e5a7e5d758.

1. [app] You can report someone else's post from its menu, picking a reason and adding a note if you wish; a report with no reason cannot be sent — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
2. [app] You can report an account from its profile — `docs/goal/behavior/moderation.md` § What a report carries
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn`
3. [app] You can report a message someone sent you from that message's menu — `docs/goal/behavior/moderation.md` § What a report carries
   - (none)
4. [app] The text of a private message or a restricted post reaches the administrators only if you tick the box that attaches it, and the box says they will be able to read it — `docs/goal/behavior/moderation.md` § What a report carries
   - (none)
5. [app] You can block the person in the same step as reporting them — `docs/goal/behavior/moderation.md` § What a report carries
   - (none)
6. [app] Reporting is not offered on your own posts, your own messages or your own profile — `docs/goal/behavior/moderation.md` § App surface
   - (none)
7. [app] Once sent, a report is acknowledged, naming the administrators it went to — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
8. [app] When the person you report lives on another nest, the acknowledgement names that nest too and says your name was not passed on — `docs/goal/behavior/moderation.md` § What the reporter is told
   - (none)
9. [app] What you reported stops showing for you, with a note that you reported it in its place — `docs/goal/behavior/moderation.md` § Corollary
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
10. [app] Reporting an account hides everything that account wrote from your own view — `docs/goal/behavior/moderation.md` § Corollary
   - (none)
11. [app] When an administrator resolves your report, you are told only whether it was acted on or dismissed, and your list of reports shows the same — `docs/goal/behavior/moderation.md` § What the reporter is told
   - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
12. [nest] The person you report is never told that they were reported, or by whom — `docs/goal/behavior/moderation.md` § What the reporter is told
   - (none)
13. [nest] A report removes, hides, labels or demotes nothing for anyone but you, and counts toward no reputation or shared spam signal — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - (none)
14. [nest] You can send only a limited number of reports an hour, and have one open report about the same thing at a time — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - (none)
15. [nest] A supervised account's report goes through like anyone else's, with no guardian's approval — `docs/goal/behavior/moderation.md` § Anti-abuse posture
   - (none)
16. [nest] A report about someone on another nest is passed on to that nest's administrators without your identity — `docs/goal/behavior/moderation.md` § Routing
   - (none)
17. [nest] A nest that cannot be reached when you report still receives the report once it is back, for up to seven days — `docs/goal/behavior/moderation.md` § Routing
   - (none)
18. [nest] A nest takes a passed-on report only about content or accounts it hosts — `docs/goal/behavior/moderation.md` § Routing
   - (none)
19. [nest] For a report passed on to another nest, the outcome its administrators record comes back to you — `docs/goal/behavior/moderation.md` § What the reporter is told
   - (none)
20. [nest] Withdrawing a report deletes your note and any text you attached, everywhere the report went — `docs/goal/behavior/moderation.md` § Where it lands
   - (none)
21. [nest] Deleting your account withdraws every report of yours that is still open, and leaves the resolved ones as the record of what was decided — `docs/goal/behavior/moderation.md` § Where it lands
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+cfad4af4 standalone |
| linux |  no run recorded | |
| windows | ⚠ partial | 0.1.3-dev+60888d4e standalone |
| macos | ⚠ partial | 0.1.3-dev+a5d2dc2b standalone |
| ios | ⚠ partial | 0.1.3-dev+a5d2dc2b standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+b265ecfe.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_an_account_report_shows_in_the_ledger_and_can_be_withdrawn` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | (none) | — |
| 4 | app | (none) | — |
| 5 | app | (none) | — |
| 6 | app | (none) | — |
| 7 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | (none) | — |
| 9 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | (none) | — |
| 11 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | nest | (none) | — |
| 13 | nest | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | nest | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | nest | (none) | — |
<!-- features-render:end -->
