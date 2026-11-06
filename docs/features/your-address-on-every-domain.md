---
slug: your-address-on-every-domain
title: your-address-on-every-domain
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-multidomain.md § The model: one handle, addressable on every active domain
guide: docs/guides/admin-tour.md § Mail
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-10-27 at 7942c4870f.

1. [nest] Mail to your handle at any domain your nest hosts reaches you — `docs/goal/behavior/mail-multidomain.md` § The model: one handle, addressable on every active domain
   - `tests/e2e-unified/tests/api/test_mail_handle_every_domain.py::test_mail_to_the_handle_at_any_hosted_domain_reaches_its_owner`
2. [app] Someone who looks you up at another of your nest's domains finds you — `docs/goal/behavior/mail-multidomain.md` § The model: one handle, addressable on every active domain
   - `tests/e2e-unified/tests/test_your_address_on_every_domain.py::test_a_lookup_at_another_of_the_nests_domains_finds_you`
3. [app] You can join or sign in having reached your nest by any of its domains — `docs/goal/behavior/mail-multidomain.md` § Registration
   - `tests/e2e-unified/tests/test_your_address_on_every_domain.py::test_you_can_join_having_reached_the_nest_by_another_of_its_domains`
4. [nest] You can send as your handle from any of your nest's domains, and the message is signed for the domain it is sent from — `docs/goal/behavior/mail-multidomain.md` § Default: cross-domain submission allowed
   - `tests/e2e-unified/tests/api/test_mail_handle_every_domain.py::test_the_handle_sends_from_any_domain_signed_for_that_domain`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+e9efe17d standalone |
| linux | ✅ full | 0.1.5-dev+3b591c2e standalone |
| windows | ✅ full | 0.1.5-dev+07ebe995 standalone |
| macos | ✅ full | 0.1.5-dev+30b3fe7d standalone |
| ios | ✅ full | 0.1.5-dev+30b3fe7d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+5a315c21.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/api/test_mail_handle_every_domain.py::test_mail_to_the_handle_at_any_hosted_domain_reaches_its_owner` | nest (linux): passed, nest (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_your_address_on_every_domain.py::test_a_lookup_at_another_of_the_nests_domains_finds_you` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_your_address_on_every_domain.py::test_you_can_join_having_reached_the_nest_by_another_of_its_domains` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_mail_handle_every_domain.py::test_the_handle_sends_from_any_domain_signed_for_that_domain` | nest (linux): passed, nest (macos): passed |
<!-- features-render:end -->
