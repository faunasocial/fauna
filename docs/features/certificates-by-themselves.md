---
slug: certificates-by-themselves
title: Certificates, by themselves
section: your nest
goal: docs/goal/architecture/nest/tls-certificates.md § Goal
guide: docs/guides/who-can-see-what.md § Your handle, your domains, and security certificates
---

## What a user gets

There is nothing to choose to get this: your nest serves an encrypted connection from its first
second, on a certificate it made itself, and a nest the internet can reach swaps in
a publicly trusted one once its domain resolves to it, with no restart, for the
nest, for mail and for calendars alike. A nest the certificate authority cannot
reach — a home nest, or one whose web port is blocked — gets its trusted
certificate through the admin's app instead. The mail records that tell other servers which certificate
to expect always follow the one actually served.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] A nest with a real name that cannot yet get a public certificate still serves encrypted connections at once — `docs/goal/architecture/nest/tls-certificates.md` § A. The self-signed floor
   - `tests/e2e-unified/tests/platform/docker/test_nest.py::test_real_hostname_serves_floor_https_under_acme`
2. [nest] A public certificate is obtained through the image's own client and the connection flips to trusted with no restart — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_real_acme_issuance_flips_floor_to_trusted`
   - `tests/e2e-unified/tests/platform/docker/test_domained_claim_acme_serves_caldav.py::test_domained_claim_sets_primary_dns_acme_and_caldav`
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail`
3. [nest] The mail records that tell other servers which certificate to expect follow the one actually served: the record pinning the nest's own certificate is published only while that certificate is served and withdrawn once a trusted one is, and the policy asking senders for strict checking is in force only while a trusted one is — `docs/goal/architecture/nest/tls-certificates.md` § D. MTA-STS honesty + DANE/TLSA for the self-signed MX (mail)
   - `tests/e2e-unified/tests/platform/docker/test_dane_tlsa_cert_coupling.py::test_dane_tlsa_floor_present_then_trusted_withdraws`
   - `tests/e2e-unified/tests/platform/docker/test_mta_sts_serving.py::test_mta_sts_never_enforces_on_the_floor_or_inside_the_window`
4. [nest] A nest with a public address of its own and no domain yet serves a publicly trusted certificate for that address, so a browser opens it without a warning — `docs/goal/architecture/nest/tls-certificates.md` § B-IP. The IP bridge cert
   - `tests/e2e-unified/tests/platform/docker/test_acme_ip_bridge_host_published.py::test_host_published_address_orders_and_serves_the_ip_bridge`
5. [nest] When a certificate is due for renewal and no admin's app has renewed it, the nest sends the admin a push reminder to open the app, which arrives even when the app is closed — `docs/goal/architecture/nest/tls-certificates.md` § C. Client-driven DNS + the mobile-offline expiry hazard
   - `tests/e2e-unified/tests/api/test_cert_nudge_reminder.py::test_the_admin_is_reminded_once_a_day_and_only_once_told`
6. [nest] A self-signed certificate left in place on a nest that can get a trusted one is replaced by a trusted certificate without anyone acting — `docs/goal/architecture/nest/tls-certificates.md` § Keeping the cert alive
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_real_acme_issuance_flips_floor_to_trusted`
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_self_signed_stopgap_heals_to_trusted_and_reaches_mail_and_calendar_without_restart`
7. [nest] A nest that cannot get a trusted certificate yet keeps trying at a pace the certificate authority allows, even across restarts, and tries again within minutes once the cause is fixed — `docs/goal/architecture/nest/tls-certificates.md` § Keeping the cert alive
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_failed_issuance_stays_paced_across_a_restart_and_picks_up_the_fix`
8. [nest] A public nest renews its trusted certificate by itself well before it expires, and again whenever the names it serves change — `docs/goal/architecture/nest/tls-certificates.md` § Keeping the cert alive
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_a_certificate_inside_its_renewal_lead_is_renewed_by_itself`
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_second_domain_not_pointing_here_never_breaks_the_primarys_certificate`
9. [nest] When a trusted certificate has expired or does not cover a name, the nest serves its self-signed certificate for that name instead, and the apps keep working — `docs/goal/architecture/nest/tls-certificates.md` § The two-layer model
   - `tests/e2e-unified/tests/platform/docker/test_expired_trusted_cert_serves_floor.py::test_expired_trusted_default_cert_serves_the_floor`
10. [nest] The self-signed certificate renews itself and keeps the same key, so a mail app that accepted it once is not asked again — `docs/goal/architecture/nest/tls-certificates.md` § A. The self-signed floor
   - `tests/e2e-unified/tests/platform/docker/test_domainless_floor_tls.py::test_the_floor_renews_itself_on_the_same_key`
11. [nest] Switching on a service with a name of its own before that name resolves never holds up the main certificate; the service's name joins the trusted certificate once it resolves — `docs/goal/architecture/nest/tls-certificates.md` § B. Trusted-cert acquisition
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_an_unresolved_service_name_never_holds_up_the_certificate_and_joins_once_it_resolves`
12. [nest] A newly issued or renewed certificate is served on the mail and calendar ports within moments, with no restart — `docs/goal/architecture/nest/tls-certificates.md` § Keeping the cert alive
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail`
   - `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_self_signed_stopgap_heals_to_trusted_and_reaches_mail_and_calendar_without_restart`
13. [nest] Opening any of the nest's names over plain http sends the visitor to the secure address on that same name — `docs/goal/architecture/nest/domains-and-tls-bootstrap.md` § Implementation status today
   - `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_plain_http_on_each_served_name_redirects_to_https_on_that_name`
14. [nest] A nest with no name at all also serves an encrypted connection from its first second, on a certificate it made itself — `docs/goal/architecture/nest/tls-certificates.md` § A. The self-signed floor
   - `tests/e2e-unified/tests/platform/docker/test_domainless_floor_tls.py::test_domainless_nest_serves_tls_on_its_own_certificate`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| linux | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| windows | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| macos | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| ios | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| android | ✅ full | 0.1.2-dev+b3cb40c5 docker |
| tui | ✅ full | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_nest.py::test_real_hostname_serves_floor_https_under_acme` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_real_acme_issuance_flips_floor_to_trusted` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_domained_claim_acme_serves_caldav.py::test_domained_claim_sets_primary_dns_acme_and_caldav` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_dane_tlsa_cert_coupling.py::test_dane_tlsa_floor_present_then_trusted_withdraws` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_mta_sts_serving.py::test_mta_sts_never_enforces_on_the_floor_or_inside_the_window` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_ip_bridge_host_published.py::test_host_published_address_orders_and_serves_the_ip_bridge` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_cert_nudge_reminder.py::test_the_admin_is_reminded_once_a_day_and_only_once_told` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_real_acme_issuance_flips_floor_to_trusted` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_self_signed_stopgap_heals_to_trusted_and_reaches_mail_and_calendar_without_restart` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_failed_issuance_stays_paced_across_a_restart_and_picks_up_the_fix` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_a_certificate_inside_its_renewal_lead_is_renewed_by_itself` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_second_domain_not_pointing_here_never_breaks_the_primarys_certificate` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_expired_trusted_cert_serves_floor.py::test_expired_trusted_default_cert_serves_the_floor` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_floor_tls.py::test_the_floor_renews_itself_on_the_same_key` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_an_unresolved_service_name_never_holds_up_the_certificate_and_joins_once_it_resolves` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_domainless_add_domain_acquires_acme_cert_and_serves_mail` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_add_domain_acme_serves_mail.py::test_a_self_signed_stopgap_heals_to_trusted_and_reaches_mail_and_calendar_without_restart` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/platform/docker/test_acme_http01_pebble_issuance.py::test_plain_http_on_each_served_name_redirects_to_https_on_that_name` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/platform/docker/test_domainless_floor_tls.py::test_domainless_nest_serves_tls_on_its_own_certificate` | nest (linux): passed |
<!-- features-render:end -->
