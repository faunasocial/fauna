---
slug: mail-abuse-refused
title: Your mail server refuses abuse
section: your nest
goal: docs/goal/behavior/smtp-server.md § Inbound policy stack
guide: docs/guides/own-your-mail.md § Why this is normally hard, and what Fauna does about it
---

## What a user gets

There is nothing to choose to get this: the mail server cannot be used to relay spam, refuses
mail that forges your own domain, checks the sender's signatures and policy, scans
for malware, and holds a connection storm to a limit, all by default. An admin can
tighten or loosen the checks that have a setting in the admin area's mail policy.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] The server is not an open relay, sending needs a password, and mail forging your own domain is refused — `docs/goal/behavior/smtp-server.md` § Inbound policy stack
   - `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_no_open_relay_on_port25`
   - `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_submission_requires_auth`
   - `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_local_domain_spoofing_rejected`
2. [nest] Mail that passes its sender domain's checks is accepted, and mail that fails them is refused when that domain asks for failing mail to be rejected — `docs/goal/behavior/smtp-server.md` § Inbound policy stack
   - `tests/e2e-unified/tests/platform/docker/test_mail_security_accept.py::test_perimeter_accepts_aligned_pass_rejects_fail`
   - `tests/e2e-unified/tests/platform/docker/test_mail_security_accept.py::test_perimeter_accepts_dkim_aligned_pass_rejects_broken`
3. [nest] Infected mail is refused at the door, named as malware to the sending server, and clean mail is accepted — `docs/goal/behavior/mail-content-scanning.md` § Pipeline
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_scan_infected_rejects`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_scan_clean_delivers`
4. [nest] A storm of mail-app sign-ins to one account is held to a limit, while a calendar app making many requests in a row is never locked out — `docs/goal/behavior/caldav-server.md` § Authentication
   - `tests/e2e-unified/tests/platform/docker/test_mail_rate_limit_burst.py::test_imap_reconnect_storm_trips_rate_limit`
   - `tests/e2e-unified/tests/platform/docker/test_mail_rate_limit_burst.py::test_caldav_burst_stays_under_rate_limit_via_cache`
5. [nest] Mail that fails its sender domain's checks, where that domain asks for quarantine and not rejection, is accepted and filed as spam instead of being refused — `docs/goal/behavior/smtp-server.md` § Inbound policy stack
   - (none)
6. [nest] Mail from a server its sender domain says may not send for it is refused when the domain publishes no policy of its own that decides the matter, while a softer or neutral answer is never refused — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - (none)
7. [nest] A sending server that greets with an empty or garbled name, with the name every machine calls itself, or with a bare single-word name is refused — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
8. [nest] A sending server that greets with your nest's own name is refused as an impersonation — `docs/goal/behavior/smtp-server.md` § HELO / EHLO syntactic + identity validation
   - (none)
9. [nest] Mail whose sender address names a domain that does not exist is refused, and a sender address with no domain at all is refused as invalid — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
10. [nest] When a lookup the checks depend on cannot be answered, mail is not refused for that reason; the nest accepts it instead of turning a legitimate sender away — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - (none)
11. [nest] With nothing chosen, a server listed on the well-known public spam blocklist is refused as soon as it connects — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
12. [nest] With nothing chosen, a server that opens more than ten connections in a minute is told to try again later — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
13. [nest] One sending address cannot hold more than half of the server's incoming-mail connections at once; further connections from it are closed — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
14. [nest] One sending address cannot push more than about fifty messages an hour through the incoming-mail port — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
15. [nest] A single connection cannot address one message to more than a hundred recipients — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
16. [nest] A sending server that keeps making refused requests gets slower and slower answers, while a well-behaved sender is never slowed — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
17. [nest] A sending server that goes silent for half a minute in the middle of a conversation is disconnected, so slow senders cannot tie the server up — `docs/goal/behavior/smtp-server.md` § Connection-time limits
   - (none)
18. [nest] A message with an absurd number or size of header lines is refused before anything reads it — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
19. [nest] A message carrying no sender line, or more than one, is refused, whether it arrives from outside or is sent from a mail app — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - (none)
20. [nest] A sender cannot plant the nest's own delivery marks in a message to steer how it is filed or which of your addresses it seems to have reached; the copy you read carries only the marks your nest wrote — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
21. [nest] A sender cannot smuggle a second, hidden message inside one by using unusual line endings — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - (none)
22. [nest] The incoming-mail port never accepts a sign-in, so it cannot be used to guess passwords — `docs/goal/behavior/smtp-server.md` § Auth on each port
   - (none)
23. [nest] While the malware scanner is unavailable, incoming mail is put off so the sender retries later; it is never delivered unscanned — `docs/goal/behavior/mail-content-scanning.md` § Don't do these
   - (none)
24. [nest] Clean mail reaches you carrying the result of its scan — `docs/goal/behavior/mail-content-scanning.md` § How the result is sealed
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_no_open_relay_on_port25` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_submission_requires_auth` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_security_relay.py::test_local_domain_spoofing_rejected` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_security_accept.py::test_perimeter_accepts_aligned_pass_rejects_fail` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_security_accept.py::test_perimeter_accepts_dkim_aligned_pass_rejects_broken` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_scan_infected_rejects` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_scan_clean_delivers` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_rate_limit_burst.py::test_imap_reconnect_storm_trips_rate_limit` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_rate_limit_burst.py::test_caldav_burst_stays_under_rate_limit_via_cache` | nest (linux): passed |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
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
| 22 | nest | (none) | — |
| 23 | nest | (none) | — |
| 24 | nest | (none) | — |
<!-- features-render:end -->
