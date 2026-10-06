---
slug: admin-mail-health
title: Mail health at a glance
section: admin area
goal: docs/goal/behavior/mail-deliverability.md § Admin-pane Deliverability surface
guide: docs/guides/admin-tour.md § Mail
---

## What a user gets

At the top of the Mail page, right under the mail on/off switch, one line says
whether mail is working — delivering, warming up, delayed, blocklisted, records
needing attention, the mail service not connected, or simply off — followed by
when mail was last delivered and last received. Seven short rows underneath show
each check behind that line. **Check again** re-runs the blocklist and
DNS-record checks on the spot; **Restart warm-up** (after the server's outgoing
address changed) asks you to press again before it starts the sending ramp over.
While the server's address is on a blocklist, a link to request removal
appears. The same one-line state shows as a **Mail** card on the dashboard, so a
problem is visible the moment you open the admin area.

## Coverage contract

Stamped 2026-10-01 at dfa27a899f.

1. [app] The Mail page shows the state the nest decided and its seven check rows, in order, with the heartbeat facts — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_renders_the_nest_state_and_seven_rows`
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
2. [app] Check again runs the blocklist check and the records check right away, without waiting for the daily run, and the readout shows their results — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_recheck_runs_a_self_check_and_rereads`
3. [app] The removal-request link is offered only while the server's address is blocklisted — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_delist_link_absent_while_not_listed`
4. [app] Restarting the warm-up takes a second, confirming press before anything changes — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm`
5. [app] The dashboard's Mail card shows the same state — `docs/goal/behavior/admin.md` § 1. Dashboard
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_dashboard_mail_card_shows_the_state`
6. [nest] Restarting the warm-up puts the nest back on the first day of the ramp — `docs/goal/behavior/mail-deliverability.md` § Manual reset
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_resets_warmup_round_trip`
7. [app] While mail is on and a mail helper is not connected — crashed, stuck restarting, or never approved — the Mail page and the dashboard's Mail card say the mail service is not connected — `docs/goal/behavior/mail-deliverability.md` § Admin-pane Deliverability surface
   - (none)
8. [app] While mail is off the Mail page still shows its health section, and the line reads off as a neutral state, not as a problem — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
9. [app] Turning mail on from the Mail page moves the health line away from off without leaving the page — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
   - `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled`
10. [app] When the latest blocklist check finds the server's address listed, the line says the address is blocklisted — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
11. [app] When an outgoing message has failed at least once and is older than the delay-warning time, the line says outgoing mail is delayed — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
12. [nest] Mail held back by the sending warm-up never makes mail health read as delayed — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
13. [app] When the latest records check finds a failing sender-policy, signing, reporting-policy or reverse-lookup record, the line says DNS records need attention — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
14. [app] While a new server's sending ramp is still under way and nothing worse holds, the line says warming up — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
15. [app] With mail on and no problem or ramp holding, the line says delivering — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
16. [nest] When several problems hold at once, mail health names the worst: off, then not connected, blocklisted, delayed, records needing attention, warming up — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
17. [app] An app that meets a mail-health state newer than itself says mail needs attention instead of failing — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
18. [app] Last delivered and last received say how long ago each happened, or never when it has not happened yet — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
19. [nest] A nest that has sent or received nothing for a long time is not reported as a problem: the two times never change the health state — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
20. [nest] Last delivered moves when the nest hands a message to an outside server — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
21. [nest] Last received moves when the nest accepts a message from outside — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
22. [nest] Mail health tells the admin only when mail last left and last arrived, never whose mail, which domain or which message — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
23. [app] While the server's address is listed, the blocklist row names the list and the reason the list gave — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - (none)
24. [app] A blocklist that did not answer shows as a warning on the blocklist row, distinct from both listed and not listed — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - (none)
25. [nest] A blocklist check that gets no answer leaves the earlier result standing, so an unanswered check never clears a listing — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - (none)
26. [app] The removal-request link leads to the removal page of the blocklist that lists the server's address — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - (none)
27. [nest] The records check behind mail health runs again by itself once a day, so the records row is current without anyone pressing Check again — `docs/goal/behavior/mail-deliverability.md` § Goal
    - (none)
28. [nest] Restarting the warm-up keeps the count of all the mail the nest has ever sent — `docs/goal/behavior/mail-deliverability.md` § Manual reset
    - (none)
29. [app] After the confirming press the warm-up button goes back to its ordinary label, so a later press asks for confirmation again — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm`
30. [nest] Only an admin can read mail health: a member who asks is refused — `docs/goal/behavior/mail-deliverability.md` § The mail health readout on
    - (none)
31. [nest] Running the checks never changes a DNS record or a mail setting: it only reports — `docs/goal/behavior/mail-deliverability.md` § No automated remediation
    - (none)
32. [app] When the server's address is on several blocklists, the blocklist row names them all, and the removal-request link leads to the first of them that has a removal page — `docs/goal/behavior/mail-deliverability.md` § Admin-pane rendering
    - (none)
33. [nest] Pressing Check again a second time within a minute does not ask the blocklists again, and never clears a listing the earlier check found — `docs/goal/behavior/mail-deliverability.md` § Force-refresh
    - (none)
34. [app] The mail health section points you to the two big mailbox providers' own sender dashboards, as plain help text with links — `docs/goal/behavior/mail-deliverability.md` § The admin-pane surface
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| tui | ⚠ partial | 0.1.2-dev+4ad1c6f0.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_renders_the_nest_state_and_seven_rows` | tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_recheck_runs_a_self_check_and_rereads` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_delist_link_absent_while_not_listed` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm` | tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_dashboard_mail_card_shows_the_state` | tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_resets_warmup_round_trip` | nest (linux): passed |
| 7 | app | (none) | — |
| 8 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_readout_reads_off_when_mail_is_disabled` | tui (linux): passed |
| 10 | app | (none) | — |
| 11 | app | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | nest | (none) | — |
| 17 | app | (none) | — |
| 18 | app | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | (none) | — |
| 21 | nest | (none) | — |
| 22 | nest | (none) | — |
| 23 | app | (none) | — |
| 24 | app | (none) | — |
| 25 | nest | (none) | — |
| 26 | app | (none) | — |
| 27 | nest | (none) | — |
| 28 | nest | (none) | — |
| 29 | app | `tests/e2e-unified/tests/test_admin_mail_health.py::test_warmup_reset_needs_the_confirm` | tui (linux): passed |
| 30 | nest | (none) | — |
| 31 | nest | (none) | — |
| 32 | app | (none) | — |
| 33 | nest | (none) | — |
| 34 | app | (none) | — |
<!-- features-render:end -->
