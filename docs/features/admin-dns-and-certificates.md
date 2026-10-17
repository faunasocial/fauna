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
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status`
14. [app] The admin re-checks every record against public DNS on demand, and the page re-checks by itself while it stays open — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_rechecks_on_demand_and_by_itself_while_open`
15. [app] Each record's exact value copies to the clipboard, ready to paste at a DNS provider — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_record_value_copies_to_the_clipboard`
16. [app] A record that is not yet live never stops the admin adding a domain or carrying on — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status`
17. [app] One switch hands every domain to Fauna at once, while each domain's choice is still remembered separately — `docs/goal/behavior/dns-management.md` § The two modes
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable`
18. [app] A domain shows as managed by Fauna only on a device holding a credential that covers it; on any other device, or for another admin, it shows as manual — `docs/goal/behavior/dns-management.md` § The two modes
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable`
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_another_admin_reads_manual_for_a_domain_the_first_manages`
19. [app] Removing a held DNS-provider credential returns the domains it covered to manual — `docs/goal/behavior/dns-management.md` § Where the credential lives
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable`
20. [app] A DNS-provider credential added on one of the admin's devices is there on their other devices too, without entering it again — `docs/goal/behavior/dns-management.md` § Where the credential lives
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_credential_added_on_one_device_is_on_the_other`
21. [app] If publishing a managed domain's records fails, the page says why and the domain stays managed rather than quietly turning manual — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_failed_publish_says_why_and_stays_managed`
22. [app] For a domain Fauna manages, records are published again whenever they change — a key rotation, a policy edit, a domain added — and whenever the page finds one missing or changed — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_records_republished_and_superseded_policy_taken_down`
23. [app] When Fauna manages a domain, an outdated record — an old signing key, a renamed member's old handle record — is taken down rather than left beside its replacement — `docs/goal/behavior/dns-management.md` § Fauna-managed
    - `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_records_republished_and_superseded_policy_taken_down`
24. [nest] The record list includes the nest's own address records, for its main name and for its mail server, and the two may point at different machines — `docs/goal/behavior/dns-management.md` § Records covered
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_record_list_carries_the_nest_and_mail_host_addresses_separately`
25. [nest] Every public domain the nest hosts lists the identity record a fresh app uses to recognise the nest, and a domain that is not public lists none — `docs/goal/behavior/dns-management.md` § Records covered
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_identity_record_listed_on_every_public_domain_and_on_no_other`
26. [nest] Switching on a service that needs a name of its own — the relay, or the Bluesky server — adds that name's address record to the list — `docs/goal/behavior/dns-management.md` § Records covered
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_approving_the_bluesky_server_adds_its_address_record`
    - `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_a_connected_relay_adds_its_address_record_to_the_primary`
27. [nest] Each domain's DMARC record asks receivers to reject forged mail, with strict alignment, and to send their aggregate reports to the nest — `docs/goal/behavior/dmarc-reporting.md` § Goal
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_each_domain_publishes_a_strict_reject_dmarc_policy_reporting_to_the_nest`
28. [app] The list marks the primary domain, and the primary cannot be removed: its remove control is unavailable and the nest refuses — `docs/goal/behavior/mail-multidomain.md` § Primary domain cannot be removed
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_primary_domain_is_marked_and_cannot_be_removed`
29. [app] Before the first domain is added the admin is warned that it becomes the permanent primary, with a rename as the only way out — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - (none)
30. [nest] Adding a domain that was recently removed is refused, pointing at its restore instead — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_adding_a_recently_removed_domain_is_refused_toward_restore`
31. [app] Removing a domain asks the admin to confirm first, saying what stops working, how long a restore stays possible and what is lost after that — `docs/goal/behavior/mail-multidomain.md` § Removing a local domain
    - (none)
32. [nest] Once a domain is removed, mail sent to it is refused — `docs/goal/behavior/mail-multidomain.md` § On confirm
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart`
33. [nest] While a domain is removed, members' addresses on it stop receiving mail, and restoring it brings them and its catch-all back untouched — `docs/goal/behavior/mail-multidomain.md` § Re-add within 30 days
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_removed_domain_addresses_stop_receiving_and_restore_brings_them_back`
34. [nest] A restored domain signs mail with the same key as before, so receivers keep accepting its signatures — `docs/goal/behavior/mail-multidomain.md` § Re-add within 30 days
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_a_restored_domain_publishes_the_same_signing_key`
35. [nest] A removed domain can be restored for thirty days; after that the domain, its signing key and members' addresses on it are gone for good — `docs/goal/behavior/mail-multidomain.md` § After 30 days
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_a_removed_domain_is_restorable_for_thirty_days_then_gone`
36. [app] If the catch-all member's account passes to a successor identity, the catch-all is cleared rather than passed on, and the page says why it now reads none — `docs/goal/behavior/mail-multidomain.md` § Catch-all designation and account succession
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_catch_all_is_cleared_when_its_member_is_succeeded`
37. [nest] Mail to a domain's postmaster, abuse, noc or security address reaches the member the admin delegated it to on that domain — `docs/goal/behavior/mail-multidomain.md` § Per-domain override
    - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_delegated_role_address_mail_reaches_the_delegated_member`
38. [nest] A domain the admin adds starts with its mail transport-security policy in testing, and the nest moves it to enforced by itself once the mail certificate is trusted and seven days have passed; no one sets it by hand — `docs/goal/behavior/mail-multidomain.md` § Wizard steps
    - (none)
39. [app] Starting a rename, the admin chooses how long the old and new domains both keep working, from one to thirty days, seven by default — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_rename_grace_period_is_chosen_at_the_start`
40. [app] While a rename is under way the page shows its progress and lets the admin finish early, extend the grace period or abort, each naming its cost first — `docs/goal/behavior/mail-primary-domain-rename.md` § UX surface
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_an_in_flight_rename_shows_its_banner_and_aborts_from_it`
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_a_rename_in_its_grace_period_extends_and_finishes_early_from_the_banner`
41. [nest] A rename cannot be finished before its grace period ends unless the admin explicitly overrides — `docs/goal/behavior/mail-primary-domain-rename.md` § Wire shapes
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups`
42. [nest] Aborting a rename, even during its grace period, makes the old domain the primary again — `docs/goal/behavior/mail-primary-domain-rename.md` § Lifecycle
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_aborting_a_rename_during_grace_makes_the_old_domain_primary_again`
43. [nest] After a rename, members keep their addresses on the old domain and get none on the new one unless they add it themselves — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups`
44. [nest] When a rename completes, the old domain stays as an ordinary hosted domain; removing it is a separate choice — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups`
45. [nest] During a rename's grace period, servers still sending to the old mail host keep delivering — `docs/goal/behavior/mail-primary-domain-rename.md` § MX invalidation
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_during_grace_the_old_mail_host_delivers_and_old_unsubscribe_links_keep_working`
46. [nest] Unsubscribe links in list mail sent before a rename keep working on the old domain for as long as it stays hosted — `docs/goal/behavior/mail-primary-domain-rename.md` § Architectural rules
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_during_grace_the_old_mail_host_delivers_and_old_unsubscribe_links_keep_working`
47. [app] After a rename, a member's handle shows on the new primary domain — `docs/goal/behavior/mail-primary-domain-rename.md` § Display-handle interaction
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_after_a_rename_the_profile_shows_the_handle_on_the_new_primary`
48. [nest] After a rename, people who join the nest sign up on the new domain — `docs/goal/behavior/mail-primary-domain-rename.md` § What the user sees post-rename
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups`
49. [nest] Neither domain of an unfinished rename can be removed until the rename finishes or is aborted — `docs/goal/behavior/mail-primary-domain-rename.md` § RPC refusal codes
    - `tests/e2e-unified/tests/api/test_primary_domain_rename_lifecycle.py::test_neither_domain_of_an_unfinished_rename_can_be_removed`
50. [nest] A rename onto a domain with weaker mail transport security than the current primary is refused — `docs/goal/behavior/mail-primary-domain-rename.md` § Goal
    - `tests/e2e-unified/tests/api/test_primary_domain_rename_lifecycle.py::test_a_rename_onto_a_weaker_tls_posture_is_refused_until_it_catches_up`
51. [nest] The admin can rename away from a primary domain that has lapsed or been seized, and the rename completes although the old domain no longer resolves — `docs/goal/behavior/mail-primary-domain-rename.md` § Renaming away from a dead domain
    - `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_rename_away_from_a_dead_primary_completes`
52. [nest] Only an admin can start, read or change a rename — `docs/goal/behavior/mail-primary-domain-rename.md` § Wire shapes
    - `tests/e2e-unified/tests/test_primary_domain_rename.py::test_non_admin_denied_on_rename_kind`
53. [app] With no DNS-provider credential, issuing a certificate shows the challenge record to paste at the registrar, then a button that completes it, or cancel — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_shows_the_challenge_record_to_paste_and_cancel_clears_it`
54. [app] A manual issuance left half-done is still waiting with the same record to paste after a restart or on another device, and a failed attempt keeps it for a retry — `docs/goal/architecture/nest/tls-certificates.md` § Surviving an interrupted manual issuance
    - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_survives_an_app_restart_with_the_same_record`
    - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_is_waiting_with_the_same_record_on_another_device`
    - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_a_failed_manual_completion_keeps_the_card_for_a_retry`
55. [app] A certificate on a managed or delegated domain renews by itself while one of the admin's apps is running, with no tap — `docs/goal/architecture/nest/tls-certificates.md` § C. Client-driven DNS + the mobile-offline expiry hazard
    - `tests/e2e-unified/tests/test_admin_dns_cert_auto_renew_tick.py::test_a_managed_domain_on_the_floor_is_issued_by_the_tick_with_no_tap`
56. [nest] Issuing from one domain's row renews every name the nest serves, so mail apps are never pushed back onto an untrusted certificate — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - `tests/e2e-unified/tests/api/test_dns_cert_issue_covers_listener_api.py::test_issuing_for_a_second_domain_orders_every_listener_name`
57. [nest] Adding a second domain that does not point at the nest yet never breaks the primary domain's certificate; the new domain gets its own once it points here — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
    - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_second_domain_not_pointing_here_never_breaks_the_primarys_certificate`
58. [nest] The first domain added to a nest that had none becomes the nest's own name at once: its handles and its web address follow without a restart — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Add domains after install
    - `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_the_first_domain_added_to_a_domainless_nest_becomes_its_name_at_once`
59. [app] Completing a manual issuance waits until the domain's own name servers serve the pasted record, so a slow DNS provider does not make it fail — `docs/goal/architecture/nest/tls-certificates.md` § Surviving an interrupted manual issuance
   - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_complete_waits_for_the_name_servers_then_issues`
60. [app] The temporary record a certificate issuance publishes is not shown as missing on the records page between renewals — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_never_lists_the_issuance_challenge_record_as_a_standing_record`
   - `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_complete_waits_for_the_name_servers_then_issues`
61. [app] An admin setting up a nest from inside its own home network never has that private network address published as the nest's public address record — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Host-address acquisition
   - `tests/e2e-unified/tests/test_admin_dns_host_address_lan.py::test_an_admin_on_the_home_network_never_publishes_its_private_address`
62. [app] Every record of every hosted domain is listed with its type and the exact value to publish — `docs/goal/behavior/dns-management.md` § Manual + live verification
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status`
63. [app] Each domain's row shows who its catch-all and its role addresses currently go to — `docs/goal/behavior/mail-multidomain.md` § Per-domain role-address routing
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_row_shows_where_catch_all_and_role_addresses_go`
64. [app] The primary domain's row is the one that offers the rename — `docs/goal/behavior/mail-primary-domain-rename.md` § UX surface
    - `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_the_primary_row_is_the_one_that_offers_the_rename`
65. [nest] A domain added after install receives mail once its certificate is in place — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Add domains after install (client-driven → ACME)
    - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_domain_added_after_install_receives_mail_once_its_certificate_is_in_place`
66. [nest] An admin can soften one domain's DMARC policy to quarantine or none, which changes that domain's published record and no other, and setting it back to reject restores the default; a member who is not an admin cannot — `docs/goal/behavior/dmarc-reporting.md` § Multi-domain deployments
    - `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_softens_one_domains_dmarc_policy_over_wire`
67. [app] An admin softens one domain's DMARC policy from its row on the Domains page, sees it held after reopening the page, and setting it back to reject clears it — `docs/goal/behavior/dmarc-reporting.md` § Multi-domain deployments
    - `tests/e2e-unified/tests/test_admin_dns.py::test_admin_softens_one_domains_dmarc_policy_from_its_row`

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
| 1 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_domain_records` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
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
| 11 | app | `tests/e2e-unified/tests/test_admin_picker_label_collision.py::test_admin_dns_pickers_stay_injective_when_labels_collide` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_admin_picker_all_accounts.py::test_admin_dns_pickers_offer_an_account_older_than_the_newest_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_rechecks_on_demand_and_by_itself_while_open` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_record_value_copies_to_the_clipboard` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_another_admin_reads_manual_for_a_domain_the_first_manages` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_manage_all_and_the_credential_that_makes_it_actionable` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_credential_added_on_one_device_is_on_the_other` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_failed_publish_says_why_and_stays_managed` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_records_republished_and_superseded_policy_taken_down` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_admin_dns_managed.py::test_admin_dns_managed_records_republished_and_superseded_policy_taken_down` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_record_list_carries_the_nest_and_mail_host_addresses_separately` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_identity_record_listed_on_every_public_domain_and_on_no_other` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_approving_the_bluesky_server_adds_its_address_record` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/platform/docker/test_iroh_relay.py::test_a_connected_relay_adds_its_address_record_to_the_primary` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_each_domain_publishes_a_strict_reject_dmarc_policy_reporting_to_the_nest` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_primary_domain_is_marked_and_cannot_be_removed` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 29 | app | (none) | — |
| 30 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_adding_a_recently_removed_domain_is_refused_toward_restore` | nest (linux): passed |
| 31 | app | (none) | — |
| 32 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_mta_local_domains_hot_reloads_without_restart` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_removed_domain_addresses_stop_receiving_and_restore_brings_them_back` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_a_restored_domain_publishes_the_same_signing_key` | nest (linux): passed |
| 35 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_a_removed_domain_is_restorable_for_thirty_days_then_gone` | nest (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_catch_all_is_cleared_when_its_member_is_succeeded` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_delegated_role_address_mail_reaches_the_delegated_member` | nest (linux): passed |
| 38 | nest | (none) | — |
| 39 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_rename_grace_period_is_chosen_at_the_start` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_an_in_flight_rename_shows_its_banner_and_aborts_from_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_a_rename_in_its_grace_period_extends_and_finishes_early_from_the_banner` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 41 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups` | nest (linux): passed |
| 42 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_aborting_a_rename_during_grace_makes_the_old_domain_primary_again` | nest (linux): passed |
| 43 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups` | nest (linux): passed |
| 44 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_during_grace_the_old_mail_host_delivers_and_old_unsubscribe_links_keep_working` | nest (linux): passed |
| 46 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_during_grace_the_old_mail_host_delivers_and_old_unsubscribe_links_keep_working` | nest (linux): passed |
| 47 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_after_a_rename_the_profile_shows_the_handle_on_the_new_primary` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 48 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_finished_rename_keeps_the_old_domain_and_its_addresses_and_moves_signups` | nest (linux): passed |
| 49 | nest | `tests/e2e-unified/tests/api/test_primary_domain_rename_lifecycle.py::test_neither_domain_of_an_unfinished_rename_can_be_removed` | nest (linux): passed |
| 50 | nest | `tests/e2e-unified/tests/api/test_primary_domain_rename_lifecycle.py::test_a_rename_onto_a_weaker_tls_posture_is_refused_until_it_catches_up` | nest (linux): passed |
| 51 | nest | `tests/e2e-unified/tests/platform/docker/test_primary_domain_rename_flip.py::test_a_rename_away_from_a_dead_primary_completes` | nest (linux): passed |
| 52 | nest | `tests/e2e-unified/tests/test_primary_domain_rename.py::test_non_admin_denied_on_rename_kind` | nest (linux): passed |
| 53 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_shows_the_challenge_record_to_paste_and_cancel_clears_it` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 54 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_survives_an_app_restart_with_the_same_record` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 54 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_issue_is_waiting_with_the_same_record_on_another_device` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 54 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_a_failed_manual_completion_keeps_the_card_for_a_retry` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 55 | app | `tests/e2e-unified/tests/test_admin_dns_cert_auto_renew_tick.py::test_a_managed_domain_on_the_floor_is_issued_by_the_tick_with_no_tap` | linux (linux): passed, tui (linux): passed |
| 56 | nest | `tests/e2e-unified/tests/api/test_dns_cert_issue_covers_listener_api.py::test_issuing_for_a_second_domain_orders_every_listener_name` | nest (linux): passed |
| 57 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_second_domain_not_pointing_here_never_breaks_the_primarys_certificate` | nest (linux): passed |
| 58 | nest | `tests/e2e-unified/tests/api/test_admin_domain_lifecycle.py::test_the_first_domain_added_to_a_domainless_nest_becomes_its_name_at_once` | nest (linux): passed |
| 59 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_complete_waits_for_the_name_servers_then_issues` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 60 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_never_lists_the_issuance_challenge_record_as_a_standing_record` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 60 | app | `tests/e2e-unified/tests/test_admin_dns_cert_manual_issue.py::test_manual_complete_waits_for_the_name_servers_then_issues` | linux (linux): passed, android (linux): error, tui (linux): passed |
| 61 | app | `tests/e2e-unified/tests/test_admin_dns_host_address_lan.py::test_an_admin_on_the_home_network_never_publishes_its_private_address` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 62 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_lists_every_record_with_type_value_and_live_status` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 63 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_dns_row_shows_where_catch_all_and_role_addresses_go` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 64 | app | `tests/e2e-unified/tests/test_admin_dns_domain_lifecycle.py::test_the_primary_row_is_the_one_that_offers_the_rename` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 65 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_domain_added_after_install_receives_mail_once_its_certificate_is_in_place` | nest (linux): passed |
| 66 | nest | `tests/e2e-unified/tests/test_mail_admin_local_domains.py::test_admin_softens_one_domains_dmarc_policy_over_wire` | nest (linux): passed |
| 67 | app | `tests/e2e-unified/tests/test_admin_dns.py::test_admin_softens_one_domains_dmarc_policy_from_its_row` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
<!-- features-render:end -->
