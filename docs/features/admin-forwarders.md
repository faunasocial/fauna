---
slug: admin-forwarders
title: Aliases that forward outside
section: admin area
goal: docs/goal/behavior/admin.md § 4. Aliases
guide: docs/guides/admin-tour.md § Aliases
---

## What a user gets

Give an address on your domain to someone with no mailbox here: mail to it
forwards to their address elsewhere. A target on a domain you host is refused,
because that would be an alias, not a forward.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A forwarder is created, listed with its target, and deleted; a hosted-domain target is refused on the page — `docs/goal/behavior/admin.md` § 4. Aliases
   - `tests/e2e-unified/tests/test_admin_aliases.py::test_admin_aliases_forwarder_create_and_delete`
   - `tests/e2e-unified/tests/test_admin_aliases.py::test_admin_aliases_forwarder_to_hosted_domain_rejected`
2. [nest] A forwarder the admin creates is listed with its target until it is deleted; mail sent to it is accepted and queued for the outside address under the admin's account, with no copy kept on the nest; and a target on a domain the nest hosts is refused — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_admin_creates_lists_deletes_forwarder`
   - `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_local_domain_target`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_admin_forwarder_redirects_to_external`
3. [nest] When forwarded mail cannot be delivered at the outside address, the bounce comes back to the admin's inbox and never to the original sender — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_forwarder_ndr.py::test_mail_deploy_forwarder_ndr_seals_locally_no_hairpin`
4. [nest] Mail a forwarder sends on leaves with an envelope sender on the nest's own domain, so the destination's sender checks pass — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - (none)
5. [nest] Mail that has already been round a forwarding loop is not sent round again — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - (none)
6. [nest] Forwards from a forwarder count against the admin's hourly forwarding limit, and those over it wait and go out later — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - (none)
7. [nest] When the nest cannot take a forward on right now, the sending server is told to try again later; the mail is never accepted and then lost — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
   - (none)
8. [nest] A member who writes to a forwarder address from inside the nest reaches the outside address too — `docs/goal/behavior/mail-forwarding.md` § Implementation status today
   - (none)
9. [app] Creating a forwarder on an address someone already holds, or on a reserved name, is refused on the page with the reason — `docs/goal/behavior/admin.md` § Errors & edge cases
   - (none)
10. [nest] The nest refuses a forwarder on a domain it does not host or on a reserved name — `docs/goal/behavior/mail-aliases.md` § Kind 7
    - `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_unhosted_domain`
    - `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_reserved_local_part`
11. [nest] The nest refuses a forwarder on an address a member or another forwarder already holds — `docs/goal/behavior/mail-aliases.md` § Kind 7
    - (none)
12. [nest] The admin's forwarders never appear among the admin's own personal addresses — `docs/goal/behavior/admin.md` § 4. Aliases
    - (none)
13. [nest] Only an admin can create, list or delete forwarders — `docs/goal/behavior/admin.md` § 4. Aliases
    - `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_non_admin_denied_on_create_forwarder`
14. [nest] Mail sent to a forwarder is delivered to the outside address's mail server — `docs/goal/behavior/mail-forwarding.md` § Admin external forwarders
    - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+cdb1bad0 standalone |
| linux | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| windows | ⚠ partial | 0.1.2-dev+a704dbe4.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+8b423269 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_aliases.py::test_admin_aliases_forwarder_create_and_delete` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_aliases.py::test_admin_aliases_forwarder_to_hosted_domain_rejected` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_admin_creates_lists_deletes_forwarder` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_local_domain_target` | nest (linux): passed, nest (macos): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_admin_forwarder_redirects_to_external` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_forwarder_ndr.py::test_mail_deploy_forwarder_ndr_seals_locally_no_hairpin` | — |
| 4 | nest | (none) | — |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | app | (none) | — |
| 10 | nest | `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_unhosted_domain` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_create_forwarder_rejects_reserved_local_part` | nest (linux): passed |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | nest | `tests/e2e-unified/tests/api/test_mail_forwarder.py::test_non_admin_denied_on_create_forwarder` | nest (linux): passed |
| 14 | nest | (none) | — |
<!-- features-render:end -->
