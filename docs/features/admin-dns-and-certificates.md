---
slug: admin-dns-and-certificates
title: Domains, DNS records and certificates
section: admin area
goal: docs/goal/behavior/dns-management.md § Goal
guide: docs/guides/admin-tour.md § DNS
---

## What a user gets

One page lists every domain the nest hosts and every DNS record each needs,
with the exact value and whether it is live. Add a domain and it gets its records,
its keys and, once DNS is right, a real certificate; remove one and restore it within
a month. Hand the app a DNS provider credential, which never leaves your device, and
it publishes the records for you and renews certificates by DNS. Each domain has a
catch-all member, overrides for the standard role addresses, and a certificate
badge; the primary domain can be renamed.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A hosted domain appears on the page with its records, each showing a value, and the reverse-lookup record carries a note that it is set at the server's provider — `docs/goal/behavior/dns-management.md` § Records covered
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_domain_records`
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_ptr_row_shows_provider_note`
2. [app] A domain is added, removed and restored from the page — `docs/goal/behavior/mail-multidomain.md` § Adding a new local domain (the admin workflow)
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_add_remove_restore_domain`
3. [app] A DNS-provider credential the provider accepts is listed with its provider and the zones it covers, and one the provider refuses shows the reason and is not kept; a domain a held credential covers switches to Fauna-managed and its records are published; without a covering credential the switch says why — `docs/goal/behavior/dns-management.md` § Fauna-managed
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_credentials_list_and_refresh`
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_add_credential_shows_provider_error`
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_toggle_managed_without_credential_shows_error`
   - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_publish_success`
4. [app] Choosing a domain's catch-all member on its row, or the member who takes one of its role addresses, saves the choice on the nest; choosing none, or the default, clears it; and setting one role address leaves the others as they were — `docs/goal/behavior/mail-multidomain.md` § Per-domain role-address routing
   - `tests/e2e-unified/tests/test_admin_catch_all.py::test_admin_catch_all_designate_and_clear`
   - `tests/e2e-unified/tests/test_admin_role_address.py::test_admin_role_address_designate_merge_and_clear`
5. [app] Each domain shows a certificate badge — renewal needed while the nest serves its self-signed certificate — and an issue button; a domain delegated for renewal gains an auto-renew switch that starts on and turns off and on again, a manual domain has none, and a delegation can be removed again — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - `tests/e2e-unified/tests/test_admin_dns_cert.py::test_admin_dns_cert_status_renders_on_floor`
   - `tests/e2e-unified/tests/test_admin_dns_cert_issue.py::test_admin_dns_cert_issue_button_renders_per_domain`
   - `tests/e2e-unified/tests/test_admin_dns_cert_auto_renew.py::test_admin_dns_auto_renew_default_on_and_toggles`
   - `tests/e2e-unified/tests/test_admin_dns_cert_delegate.py::test_admin_dns_cert_delegate_and_remove`
6. [app] A real certificate is re-issued by DNS from the app against a live nest — `docs/goal/architecture/nest/tls-certificates.md` § C. Client-driven DNS + the mobile-offline expiry hazard
   - `tests/e2e-unified/tests/live/test_dns01_cert_renewal_hetzner.py::test_dns01_reissues_the_nest_certificate_through_the_client_ui`
7. [app] The rename sheet opens from a domain's row with a picker for the new primary, and cancelling closes it without starting a rename — `docs/goal/behavior/mail-primary-domain-rename.md` § UX surface
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_rename_sheet_opens_and_cancels`
8. [nest] A domain an admin adds is listed as active, and adding it again changes nothing; a member who is not an admin is refused; and claiming a nest with a real domain makes that domain the primary, with mail records listed for it — `docs/goal/behavior/mail-multidomain.md` § The `mail_domains` model
   - `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_adds_and_lists_local_domain_over_wire`
   - `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_non_admin_denied_on_local_domain_kind`
   - `tests/e2e-unified/tests/api/test_claim_primary_domain.py::test_claim_real_domain_registers_primary_and_dns`
9. [nest] A domain added to a nest that had none gets a trusted certificate covering its mail name within minutes, and the mail server presents that certificate — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Add domains after install (client-driven → ACME)
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail`
10. [nest] Starting a rename onto anything but one of the nest's own additional domains is refused — `docs/goal/behavior/mail-primary-domain-rename.md` § Lifecycle
   - `tests/e2e-unified/tests/test_primary_domain_rename.py::test_start_refuses_bogus_target_over_wire`
11. [app] Two members who share a display name still appear as distinct, unambiguous choices in the catch-all and role-address pickers — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_dns_pickers_stay_injective_when_labels_collide`
12. [app] Any member can be chosen as a domain's catch-all or role address, however many members the nest holds — `docs/goal/behavior/admin.md` § 2. Users
   - `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_dns_pickers_offer_an_account_older_than_the_newest_page`
13. [app] Each record shows whether public DNS already serves it, is missing it, serves a wrong value, or is still being checked — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - (none)
14. [app] The admin re-checks every record against public DNS on demand, and the page re-checks by itself while it stays open — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - (none)
15. [app] Each record's exact value copies to the clipboard, ready to paste at a DNS provider — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - (none)
16. [app] A record that is not yet live never stops the admin adding a domain or carrying on — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - (none)
17. [app] One switch hands every domain to Fauna at once, while each domain's choice is still remembered separately — `docs/goal/behavior/dns-management.md` § The two modes
    - (none)
18. [app] A domain shows as managed by Fauna only on a device holding a credential that covers it; on any other device, or for another admin, it shows as manual — `docs/goal/behavior/dns-management.md` § The two modes
    - (none)
19. [app] Removing a held DNS-provider credential returns the domains it covered to manual — `docs/goal/behavior/dns-management.md` § Where the credential lives
    - (none)
20. [app] A DNS-provider credential added on one of the admin's devices is there on their other devices too, without entering it again — `docs/goal/behavior/dns-management.md` § Where the credential lives
    - (none)
21. [app] If publishing a managed domain's records fails, the page says why and the domain stays managed rather than quietly turning manual — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - (none)
22. [app] For a domain Fauna manages, records are published again whenever they change — a key rotation, a policy edit, a domain added — and whenever the page finds one missing or changed — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - (none)
23. [app] When Fauna manages a domain, an outdated record — an old signing key, a renamed member's old handle record — is taken down rather than left beside its replacement — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - (none)
24. [nest] The record list includes the nest's own address records, for its main name and for its mail server, and the two may point at different machines — `docs/goal/behavior/dns-management.md` § Records covered
    - (none)
25. [nest] Every public domain the nest hosts lists the identity record a fresh app uses to recognise the nest, and a domain that is not public lists none — `docs/goal/behavior/dns-management.md` § Records covered
    - (none)
26. [nest] Switching on a service that needs a name of its own — the relay, or the Bluesky server — adds that name's address record to the list — `docs/goal/behavior/dns-management.md` § Records covered
    - (none)
27. [nest] Each domain's DMARC record asks receivers to reject forged mail, with strict alignment, and to send their aggregate reports to the nest — `docs/goal/behavior/dmarc-reporting.md` § Goal
    - (none)
28. [app] The list marks the primary domain, and the primary cannot be removed: its remove control is unavailable and the nest refuses — `docs/goal/behavior/mail-multidomain.md` § Primary domain cannot be removed
    - (none)
29. [app] Before the first domain is added the admin is warned that it becomes the permanent primary, with a rename as the only way out — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - (none)
30. [nest] Adding a domain that was recently removed is refused, pointing at its restore instead — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - (none)
31. [app] Removing a domain asks the admin to confirm first, saying what stops working, how long a restore stays possible and what is lost after that — `docs/goal/behavior/mail-multidomain.md` § Removing a local domain
    - (none)
32. [nest] Once a domain is removed, mail sent to it is refused — `docs/goal/behavior/mail-multidomain.md` § On confirm
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart`
33. [nest] While a domain is removed, members' addresses on it stop receiving mail, and restoring it brings them and its catch-all back untouched — `docs/goal/behavior/mail-multidomain.md` § Re-add within 30 days
    - (none)
34. [nest] A restored domain signs mail with the same key as before, so receivers keep accepting its signatures — `docs/goal/behavior/mail-multidomain.md` § Re-add within 30 days
    - (none)
35. [nest] A removed domain can be restored for thirty days; after that the domain, its signing key and members' addresses on it are gone for good — `docs/goal/behavior/mail-multidomain.md` § After 30 days
    - (none)
36. [app] If the catch-all member's account passes to a successor identity, the catch-all is cleared rather than passed on, and the page says why it now reads none — `docs/goal/behavior/mail-multidomain.md` § Catch-all designation and account succession
    - (none)
37. [nest] Mail to a domain's postmaster, abuse, noc or security address reaches the member the admin delegated it to on that domain — `docs/goal/behavior/mail-multidomain.md` § Per-domain override
    - (none)
38. [nest] A domain the admin adds starts with its mail transport-security policy in testing, and the nest moves it to enforced by itself once the mail certificate is trusted and seven days have passed; no one sets it by hand — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - (none)
39. [app] Starting a rename, the admin chooses how long the old and new domains both keep working, from one to thirty days, seven by default — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - (none)
40. [app] While a rename is under way the page shows its progress and lets the admin finish early, extend the grace period or abort, each naming its cost first — `docs/goal/behavior/mail-primary-domain-rename.md` § UX surface
    - (none)
41. [nest] A rename cannot be finished before its grace period ends unless the admin explicitly overrides — `docs/goal/behavior/mail-primary-domain-rename.md` § Wire shapes
    - (none)
42. [nest] Aborting a rename, even during its grace period, makes the old domain the primary again — `docs/goal/behavior/mail-primary-domain-rename.md` § Lifecycle
    - (none)
43. [nest] After a rename, members keep their addresses on the old domain and get none on the new one unless they add it themselves — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - (none)
44. [nest] When a rename completes, the old domain stays as an ordinary hosted domain; removing it is a separate choice — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - (none)
45. [nest] During a rename's grace period, servers still sending to the old mail host keep delivering — `docs/goal/behavior/mail-primary-domain-rename.md` § MX invalidation
    - (none)
46. [nest] Unsubscribe links in list mail sent before a rename keep working on the old domain for as long as it stays hosted — `docs/goal/behavior/mail-primary-domain-rename.md` § Architectural rules
    - (none)
47. [app] After a rename, a member's handle shows on the new primary domain — `docs/goal/behavior/mail-primary-domain-rename.md` § Display-handle interaction
    - (none)
48. [nest] After a rename, people who join the nest sign up on the new domain — `docs/goal/behavior/mail-primary-domain-rename.md` § What the user sees post-rename
    - (none)
49. [nest] Neither domain of an unfinished rename can be removed until the rename finishes or is aborted — `docs/goal/behavior/mail-primary-domain-rename.md` § RPC refusal codes
    - (none)
50. [nest] A rename onto a domain with weaker mail transport security than the current primary is refused — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - (none)
51. [nest] The admin can rename away from a primary domain that has lapsed or been seized, and the rename completes although the old domain no longer resolves — `docs/goal/behavior/mail-primary-domain-rename.md` § Renaming away from a dead domain
    - (none)
52. [nest] Only an admin can start, read or change a rename — `docs/goal/behavior/mail-primary-domain-rename.md` § Wire shapes
    - `tests/e2e-unified/tests/test_primary_domain_rename.py::test_non_admin_denied_on_rename_kind`
53. [app] With no DNS-provider credential, issuing a certificate shows the challenge record to paste at the registrar, then a button that completes it, or cancel — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - (none)
54. [app] A manual issuance left half-done is still waiting with the same record to paste after a restart or on another device, and a failed attempt keeps it for a retry — `docs/goal/architecture/nest/tls-certificates.md` § Surviving an interrupted manual issuance
    - (none)
55. [app] A certificate on a managed or delegated domain renews by itself while one of the admin's apps is running, with no tap — `docs/goal/architecture/nest/tls-certificates.md` § C. Client-driven DNS + the mobile-offline expiry hazard
    - (none)
56. [nest] Issuing from one domain's row renews every name the nest serves, so mail apps are never pushed back onto an untrusted certificate — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - (none)
57. [nest] Adding a second domain that does not point at the nest yet never breaks the primary domain's certificate; the new domain gets its own once it points here — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - (none)
58. [nest] The first domain added to a nest that had none becomes the nest's own name at once: its handles and its web address follow without a restart — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Add domains after install
    - (none)
59. [app] The admin chooses, from the app, which certificate authority the nest uses and the contact address it gives that authority — `docs/goal/architecture/nest/tls-certificates.md` § ACME settings
   - (none)
60. [app] Completing a manual issuance waits until the domain's own name servers serve the pasted record, so a slow DNS provider does not make it fail — `docs/goal/architecture/nest/tls-certificates.md` § Surviving an interrupted manual issuance
   - (none)
61. [app] The temporary record a certificate issuance publishes is not shown as missing on the records page between renewals — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - (none)
62. [app] An admin setting up a nest from inside its own home network never has that private network address published as the nest's public address record — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Host-address acquisition
   - (none)
63. [app] Every record of every hosted domain is listed with its type and the exact value to publish — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - (none)
64. [app] Each domain's row shows who its catch-all and its role addresses currently go to — `docs/goal/behavior/mail-multidomain.md` § Per-domain role-address routing
    - (none)
65. [app] The primary domain's row is the one that offers the rename — `docs/goal/behavior/mail-primary-domain-rename.md` § UX surface
    - (none)
66. [nest] A domain added after install receives mail once its certificate is in place — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Add domains after install (client-driven → ACME)
    - (none)
67. [nest] An admin can soften one domain's DMARC policy to quarantine or none, which changes that domain's published record and no other, and setting it back to reject restores the default; a member who is not an admin cannot — `docs/goal/behavior/dmarc-reporting.md` § Multi-domain deployments
    - `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_softens_one_domains_dmarc_policy_over_wire`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_domain_records` | web (linux): passed, linux (linux): failed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_ptr_row_shows_provider_note` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_add_remove_restore_domain` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_credentials_list_and_refresh` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_add_credential_shows_provider_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_toggle_managed_without_credential_shows_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_publish_success` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_catch_all.py::test_admin_catch_all_designate_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_admin_role_address.py::test_admin_role_address_designate_merge_and_clear` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_dns_cert.py::test_admin_dns_cert_status_renders_on_floor` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_dns_cert_issue.py::test_admin_dns_cert_issue_button_renders_per_domain` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_dns_cert_auto_renew.py::test_admin_dns_auto_renew_default_on_and_toggles` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_dns_cert_delegate.py::test_admin_dns_cert_delegate_and_remove` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/live/test_dns01_cert_renewal_hetzner.py::test_dns01_reissues_the_nest_certificate_through_the_client_ui` | — |
| 7 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_rename_sheet_opens_and_cancels` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_adds_and_lists_local_domain_over_wire` | nest (linux): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_non_admin_denied_on_local_domain_kind` | nest (linux): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_claim_primary_domain.py::test_claim_real_domain_registers_primary_and_dns` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/test_primary_domain_rename.py::test_start_refuses_bogus_target_over_wire` | nest (linux): passed, nest (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_dns_pickers_stay_injective_when_labels_collide` | web (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_dns_pickers_offer_an_account_older_than_the_newest_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | (none) | — |
| 14 | app | (none) | — |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | app | (none) | — |
| 18 | app | (none) | — |
| 19 | app | (none) | — |
| 20 | app | (none) | — |
| 21 | app | (none) | — |
| 22 | app | (none) | — |
| 23 | app | (none) | — |
| 24 | nest | (none) | — |
| 25 | nest | (none) | — |
| 26 | nest | (none) | — |
| 27 | nest | (none) | — |
| 28 | app | (none) | — |
| 29 | app | (none) | — |
| 30 | nest | (none) | — |
| 31 | app | (none) | — |
| 32 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart` | nest (linux): passed |
| 33 | nest | (none) | — |
| 34 | nest | (none) | — |
| 35 | nest | (none) | — |
| 36 | app | (none) | — |
| 37 | nest | (none) | — |
| 38 | nest | (none) | — |
| 39 | app | (none) | — |
| 40 | app | (none) | — |
| 41 | nest | (none) | — |
| 42 | nest | (none) | — |
| 43 | nest | (none) | — |
| 44 | nest | (none) | — |
| 45 | nest | (none) | — |
| 46 | nest | (none) | — |
| 47 | app | (none) | — |
| 48 | nest | (none) | — |
| 49 | nest | (none) | — |
| 50 | nest | (none) | — |
| 51 | nest | (none) | — |
| 52 | nest | `tests/e2e-unified/tests/test_primary_domain_rename.py::test_non_admin_denied_on_rename_kind` | nest (linux): passed |
| 53 | app | (none) | — |
| 54 | app | (none) | — |
| 55 | app | (none) | — |
| 56 | nest | (none) | — |
| 57 | nest | (none) | — |
| 58 | nest | (none) | — |
| 59 | app | (none) | — |
| 60 | app | (none) | — |
| 61 | app | (none) | — |
| 62 | app | (none) | — |
| 63 | app | (none) | — |
| 64 | app | (none) | — |
| 65 | app | (none) | — |
| 66 | nest | (none) | — |
| 67 | nest | `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_softens_one_domains_dmarc_policy_over_wire` | nest (linux): passed |
<!-- features-render:end -->
