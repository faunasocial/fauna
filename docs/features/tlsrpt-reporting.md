---
slug: tlsrpt-reporting
title: TLS failure reports are read for you, not mailed to you
section: your nest
goal: docs/goal/behavior/smtp-server.md § TLSRPT inbound report listener (RFC 8460)
guide: docs/guides/own-your-mail.md § Addresses: aliases, plus-addresses, and role addresses
---

## What a user gets

There is nothing to choose to get this. Every domain your nest serves asks
other mail servers to report when they could not make an encrypted connection
to it. Those daily reports come back to your nest, which reads them itself and
keeps them for 90 days. They never land in anyone's inbox. A broken report, or
one about a domain your nest does not serve, is dropped quietly, and the server
that sent it is never told to retry.

## Coverage contract

Stamped 2026-10-11 at e663c0077a.

1. [nest] A report mailed to the reporting address is stored and listed to the admin with its summary, and the report itself can be read back — `docs/goal/behavior/smtp-server.md` § TLSRPT inbound report listener (RFC 8460)
   - `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered`
2. [nest] A malformed report, or one about a domain the nest does not serve, is accepted by the mail server and not kept — `docs/goal/behavior/smtp-server.md` § TLSRPT inbound report listener (RFC 8460)
   - `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered`
3. [nest] No report ever reaches a mailbox — `docs/goal/behavior/smtp-server.md` § abuse@ / postmaster@ role-address routing
   - `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered`
4. [nest] Reports older than 90 days are removed — `docs/goal/behavior/smtp-server.md` § TLSRPT inbound report listener (RFC 8460)
   - `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_kept_inside_ninety_days_and_pruned_past_them`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| linux | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| windows | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| macos | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| ios | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| android | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |
| tui | ✅ full | 0.1.5-dev+5fa1147b.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_stored_and_listed_never_delivered` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_tlsrpt_reports.py::test_tlsrpt_report_is_kept_inside_ninety_days_and_pruned_past_them` | nest (linux): passed |
<!-- features-render:end -->
