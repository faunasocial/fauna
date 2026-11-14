---
slug: dmarc-reporting
title: DMARC reports are read for you, not mailed to you
section: your nest
goal: docs/goal/behavior/dmarc-reporting.md § Aggregate-report receive (RUA)
guide: docs/guides/own-your-mail.md § Addresses: aliases, plus-addresses, and role addresses
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

There is nothing to choose to get this. Every domain your nest serves asks the big mail providers for a daily report
on who sent mail in its name. Those reports come back to your nest, which reads
them itself and keeps them for 90 days. They never land in anyone's inbox. A
broken report, or one about a domain your nest does not serve, is dropped
quietly, and the provider that sent it is never told to retry.

Three things are an admin's to choose, on the mail admin page: how long the reports are kept; whether to also ask providers for *forensic* reports — one per failed message, naming the people it was sent to; and whether your nest sends reports of its own. Forensic reports are off unless an admin turns them on, and the switch says what they carry. Your nest's own reports go once a day to each domain that asks for them in its DMARC record, about the mail it received claiming to be from that domain; they are on unless an admin switches off **Send daily DMARC reports to other domains**.

An admin reads the daily reports on the Mail reports page: the totals for a period and domain, the busiest sources and senders, and each report in full.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

1. [nest] A report mailed to the reporting address is stored and listed to the admin with its summary, and the report itself can be read back — `docs/goal/behavior/dmarc-reporting.md` § Storage
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
2. [nest] A malformed report, or one about a domain the nest does not serve, is accepted by the mail server and not kept — `docs/goal/behavior/dmarc-reporting.md` § XML validation
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
3. [nest] No report ever reaches a mailbox — `docs/goal/behavior/dmarc-reporting.md` § Envelope-time routing
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
4. [nest] Reports older than the retention the admin set — 90 days unless changed — are removed, and an oversize report, kept only as its envelope for triage and listed as such, goes with them — `docs/goal/behavior/dmarc-reporting.md` § Retention
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it`
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_tuned_retention_drives_the_prune_and_the_oversize_envelope_goes_with_it`
5. [app] An admin can turn forensic reports on, and is told at the switch that they carry other people's addresses — `docs/goal/behavior/dmarc-reporting.md` § Default off
   - `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips`
6. [nest] With forensic reports turned on, every domain's expected DMARC record asks providers to send them to the nest's report address — `docs/goal/behavior/dmarc-reporting.md` § Default off
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_forensic_opt_in_asks_peers_for_reports_in_the_expected_record`
7. [app] An admin can choose how long aggregate and forensic reports are kept, within the limits the nest enforces — `docs/goal/behavior/dmarc-reporting.md` § Retention
   - `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips`
8. [nest] A forensic report mailed to the reporting address is stored, listed to the admin without any recipient of the reported message, and readable in full — `docs/goal/behavior/dmarc-reporting.md` § Storage
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_forensic_report_is_stored_without_recipients_in_the_index`
9. [app] An admin can read the reports peers sent about their domains: how many messages they saw and how many passed, for a chosen period and domain, the top sources and senders, each report, and any one report in full — `docs/goal/behavior/dmarc-reporting.md` § What the page shows
   - `tests/e2e-unified/tests/test_admin_mail_reports.py::test_mail_reports_page_shows_a_mailed_in_report`
10. [app] An admin can turn off the daily reports the nest sends to other domains, and turn them back on — `docs/goal/behavior/dmarc-reporting.md` § Generation is on by default
   - `tests/e2e-unified/tests/test_admin_mail.py::test_outbound_dmarc_reports_switch_round_trips`
11. [nest] A deployment-wide DMARC setting an admin saves becomes the base of every domain's published record, and a per-domain reject stands even while the deployment is softened — `docs/goal/behavior/dmarc-reporting.md` § DMARC policy publication
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_deployment_dmarc_setting_is_every_domains_base_and_a_per_domain_reject_stands`
12. [nest] A DMARC policy save that asks the impossible is refused: a sample rate over 100, a report address that is not exactly one plain mailto, or a retention outside its bounds — `docs/goal/behavior/dmarc-reporting.md` § DMARC policy publication
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_policy_save_asking_the_impossible_is_refused`
13. [nest] While forensic reports are not opted in, the nest keeps none, whatever a sender mails — `docs/goal/behavior/dmarc-reporting.md` § Forensic-report receive (RUF)
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_no_forensic_report_is_kept_while_forensic_reports_are_not_opted_in`
14. [nest] The daily aggregate report goes only where the domain's policy allows and nothing a stranger wrote reaches its headers, and it never counts mail claiming one of our own domains — `docs/goal/behavior/dmarc-reporting.md` § Outbound aggregate-report sender
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_daily_report_goes_only_where_allowed_and_never_counts_our_own_domains`
15. [nest] The daily aggregate report counts the mail the perimeter refused, with the disposition the inbound enforcement switches applied — `docs/goal/behavior/dmarc-reporting.md` § Outbound aggregate-report sender
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_peer_mail_the_perimeter_refuses_is_counted_with_disposition_reject`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+b7384d11 standalone |
| linux | ✅ full | 0.1.5-dev+b7384d11 standalone |
| windows | ✅ full | 0.1.5-dev+5e765178.dirty standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+b7384d11 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+cab1c46a standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it` | nest (linux): passed, nest (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_tuned_retention_drives_the_prune_and_the_oversize_envelope_goes_with_it` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_forensic_opt_in_asks_peers_for_reports_in_the_expected_record` | nest (linux): passed, nest (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_forensic_report_is_stored_without_recipients_in_the_index` | nest (linux): passed, nest (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_mail_reports.py::test_mail_reports_page_shows_a_mailed_in_report` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_outbound_dmarc_reports_switch_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_deployment_dmarc_setting_is_every_domains_base_and_a_per_domain_reject_stands` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_policy_save_asking_the_impossible_is_refused` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_no_forensic_report_is_kept_while_forensic_reports_are_not_opted_in` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_daily_report_goes_only_where_allowed_and_never_counts_our_own_domains` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_peer_mail_the_perimeter_refuses_is_counted_with_disposition_reject` | nest (linux): passed, nest (windows): passed |
<!-- features-render:end -->
