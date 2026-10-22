---
slug: dmarc-reporting
title: DMARC reports are read for you, not mailed to you
section: your nest
goal: docs/goal/behavior/dmarc-reporting.md § Aggregate-report receive (RUA)
guide: docs/guides/own-your-mail.md § Addresses: aliases, plus-addresses, and role addresses
---

## What a user gets

There is nothing to choose to get this. Every domain your nest serves asks the big mail providers for a daily report
on who sent mail in its name. Those reports come back to your nest, which reads
them itself and keeps them for 90 days. They never land in anyone's inbox. A
broken report, or one about a domain your nest does not serve, is dropped
quietly, and the provider that sent it is never told to retry.

Two things are an admin's to choose, on the mail admin page: how long the
reports are kept, and whether to also ask providers for *forensic* reports —
one per failed message, naming the people it was sent to. Those are off unless
an admin turns them on, and the switch says what they carry.

An admin reads the daily reports on the Mail reports page: the totals for a period and domain, the busiest sources and senders, and each report in full.

## Coverage contract

Stamped 2026-10-17 at 3daf881ad6.

1. [nest] A report mailed to the reporting address is stored and listed to the admin with its summary, and the report itself can be read back — `docs/goal/behavior/dmarc-reporting.md` § Storage
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
2. [nest] A malformed report, or one about a domain the nest does not serve, is accepted by the mail server and not kept — `docs/goal/behavior/dmarc-reporting.md` § XML validation
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
3. [nest] No report ever reaches a mailbox — `docs/goal/behavior/dmarc-reporting.md` § Envelope-time routing
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered`
4. [nest] Reports older than 90 days are removed — `docs/goal/behavior/dmarc-reporting.md` § Retention
   - `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it`
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

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ✅ full | 0.1.4-dev+ce18d539.dirty standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+cab1c46a standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_oversize_report_keeps_an_envelope_for_triage_and_retention_prunes_it` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_forensic_opt_in_asks_peers_for_reports_in_the_expected_record` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_mail.py::test_dmarc_policy_save_round_trips` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_dmarc_reports.py::test_dmarc_forensic_report_is_stored_without_recipients_in_the_index` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_mail_reports.py::test_mail_reports_page_shows_a_mailed_in_report` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
