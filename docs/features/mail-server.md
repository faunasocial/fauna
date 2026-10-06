---
slug: mail-server
title: Mail from and to the outside world
section: your nest
goal: docs/goal/behavior/smtp-server.md § Goal
guide: docs/guides/own-your-mail.md § Why this is normally hard, and what Fauna does about it
---

## What a user gets

There is nothing to choose to get this: once mail is on, the nest runs a real mail server. Mail
from anywhere reaches your inbox, mail you send leaves signed so the big providers
accept it, a sender that was told to try again does deliver on the retry, a failed
delivery comes back to you and never to a stranger, and an upgrade never loses a
mailbox. The signing keys are minted and rotated by the nest itself. How strict the
server is with incoming mail is the admin's to tune in the admin area; the defaults
need no tuning.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] The mail server in the released image starts and answers on every one of its mail ports, and does so again after an upgrade that adds to its stored data — `docs/goal/behavior/mail-bridge-lifecycle.md` § Lifecycle phases
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_lifecycle.py::test_mail_deploy_lifecycle_to_serving`
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_schema_upgrade.py::test_mail_bridge_serves_after_schema_upgrade`
2. [nest] Mail from outside arrives and can be read in a mail app, and mail you send leaves signed, on the released image; mail from the open internet arrives on a live nest — `docs/goal/behavior/smtp-server.md` § Goal
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_inbound_round_trip.py::test_mail_deploy_inbound_round_trip`
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_outbound_submission.py::test_mail_deploy_outbound_submission`
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_bidirectional_round_trip.py::test_mail_deploy_bidirectional_round_trip`
   - `tests/e2e-unified/tests/test_mail_port25_inbound_live.py::test_port25_external_inbound_live`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_mx_round_trip`
3. [nest] A sender asked to try again delivers on the retry — `docs/goal/behavior/smtp-server.md` § Greylisting
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_greylist_retry.py::test_mail_deploy_greylist_then_retry_delivered`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_greylist_defers_first_then_passes_on_retry`
4. [nest] Outgoing mail is signed with a key the nest minted itself, the signature verifies against the record the nest gives the admin to publish, and it verifies again under the new key after the nest rotates it — `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_dkim_auto_enable.py::test_nest_auto_provisions_dkim_signed_outbound`
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_dkim_auto_enable.py::test_nest_dkim_signature_verifies_against_published_record`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_scheduled_rotation_mints_new_selector_nestside`
5. [nest] A delivery the receiving server refuses for good comes back to you straight away, sealed, and never to a stranger; a slow one warns you once — `docs/goal/behavior/smtp-server.md` § Outbound delivery (MTA → external MX)
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_bounce_on_bad_external_recipient`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_delay_warning_emitted_once`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay`
   - `tests/e2e-unified/tests/platform/docker/test_mail_deploy_forwarder_ndr.py::test_mail_deploy_forwarder_ndr_seals_locally_no_hairpin`
6. [nest] Outgoing delivery honours what the receiving domain publishes about its mail servers: under a policy it enforces, a mismatched or unencrypted server gets no mail; under a policy it is only testing, the mail is delivered; and where it pins its certificate, only a server matching the pin gets the mail — `docs/goal/behavior/smtp-server.md` § MX resolution + IPv4/IPv6 mixed handling
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_enforce_refuses_mismatched_mx`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_enforce_requires_starttls`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_testing_mode_delivers`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_dane_pin_match_delivers`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_dane_pin_mismatch_refuses`
7. [nest] A very large message is delivered rather than accepted and lost — `docs/goal/behavior/smtp-server.md` § Message size limits
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_over_the_raw_inline_ceiling_now_delivers`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_formerly_over_the_at_rest_ceiling_now_delivers`
8. [nest] The standard role addresses reach the admin even with no alias for them — `docs/goal/behavior/smtp-server.md` § abuse@ / postmaster@ role-address routing
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_role_address_postmaster_routes_to_admin`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses`
9. [nest] While the nest is briefly unreachable its mail helpers keep answering and ask senders to retry, so incoming mail arrives once the nest is back — `docs/goal/behavior/mail-bridge-lifecycle.md` § Reconnecting
   - (none)
10. [nest] An outside mail server that will not switch to an encrypted connection cannot hand mail to your nest, and is told encryption is required — `docs/goal/behavior/smtp-server.md` § TLS posture per port
   - (none)
11. [nest] An outside server or a mail app that offers only an outdated version of the encryption protocol cannot connect to the mail ports — `docs/goal/behavior/smtp-server.md` § TLS posture per port
   - (none)
12. [nest] An outside server or a mail app that offers only older, weaker ciphers cannot connect to the mail ports — `docs/goal/behavior/smtp-server.md` § TLS posture per port
   - (none)
13. [nest] Mail to an address nobody on the nest holds is refused to the sending server at once as unknown, while the same message's other recipients still get it — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_role_address_postmaster_routes_to_admin`
14. [nest] Mail to a member who has never turned mail on is refused for that recipient alone, while the other recipients still get it — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - (none)
15. [nest] Mail to someone whose account has just passed to a successor is put off, not refused, until the successor first signs in, so the sender's server delivers it later — `docs/goal/behavior/smtp-server.md` § Error / tempfail strategy
   - (none)
16. [nest] A message that cannot be delivered yet keeps being retried, each recipient on its own, for five days before it comes back as undeliverable — `docs/goal/behavior/smtp-server.md` § Retry schedule
   - (none)
17. [nest] A bounce names the recipient that failed and what the receiving server said, and carries your message's headers without its body — `docs/goal/behavior/smtp-server.md` § Permanent-failure bounce generation
   - (none)
18. [nest] Mail that was itself a bounce never produces a bounce in return — `docs/goal/behavior/smtp-server.md` § Backscatter suppression
   - (none)
19. [nest] Mail to a domain that publishes no mail-server record goes to the domain's own address, but when the lookup itself fails the message waits and is retried instead of going there — `docs/goal/behavior/smtp-server.md` § MX resolution + IPv4/IPv6 mixed handling
   - (none)
20. [nest] When the recipient's preferred mail server cannot be reached, your message is delivered through its next one — `docs/goal/behavior/smtp-server.md` § MX resolution + IPv4/IPv6 mixed handling
   - (none)
21. [nest] A recipient whose mail server answers over only one of the two internet address families still gets your mail — `docs/goal/behavior/smtp-server.md` § MX resolution + IPv4/IPv6 mixed handling
   - (none)
22. [nest] A domain that asks for reports on encrypted delivery is sent one each day saying how the nest's deliveries to it went — `docs/goal/behavior/smtp-server.md` § TLSRPT outbound reporter
   - (none)
23. [nest] Mail to the standard role addresses is accepted on the first try, even from a sender the nest has never seen — `docs/goal/behavior/smtp-server.md` § abuse@ / postmaster@ role-address routing
   - (none)
24. [nest] No automatic reply is ever sent from a role address — `docs/goal/behavior/smtp-server.md` § abuse@ / postmaster@ role-address routing
   - (none)
25. [nest] Once a sender has got through by retrying, its later mail to the same person is accepted on the first try for thirty days — `docs/goal/behavior/smtp-server.md` § Greylisting
   - (none)
26. [nest] When the mail server stops, a delivery already under way is given time to finish, and a server starting a new message is asked to try again later — `docs/goal/behavior/mail-bridge-lifecycle.md` § Shutting down
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_graceful_shutdown_drains_and_421s`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_graceful_shutdown_byes_and_exits_clean`
27. [nest] When the nest rotates a domain's signing key, it keeps signing with the old key for a day after the new one is ready, so receiving servers can learn the new record first — `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)
   - (none)
28. [nest] A rotated signing key, or a domain added while mail is running, signs outgoing mail straight away with no restart — `docs/goal/behavior/mail-bridge-lifecycle.md` § Implementation status today
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_force_rotate_dkim_flips_active_selector_live`
29. [nest] The nest keeps signing with the same key until it rotates it, so a signing record published once keeps verifying — `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)
   - (none)
30. [nest] After the mail-sending helper is given a new key, outgoing mail is still signed with the same signing key, so the published signing record keeps verifying — `docs/goal/behavior/mail-bridge-lifecycle.md` § Service-user re-keying
   - (none)
31. [nest] If mail starts before its certificate is ready, the mail ports begin offering encryption within about a minute, with no restart — `docs/goal/behavior/mail-bridge-lifecycle.md` § Implementation status today
   - (none)
32. [nest] On a new server, mail beyond the day's warm-up allowance is accepted and delivered the next day, never bounced — `docs/goal/behavior/mail-deliverability.md` § Enforcement at submission time
   - (none)
33. [nest] Bounces, automatic replies and calendar invitations are never held back by the warm-up — `docs/goal/behavior/mail-deliverability.md` § Implementation status today
   - (none)
34. [nest] Being on a public blocklist never stops the nest trying to deliver your mail; the receiving servers decide — `docs/goal/behavior/mail-deliverability.md` § Architectural rules
   - (none)
35. [nest] Two nests exchange mail with each other as ordinary mail servers, and a reply comes back — `docs/goal/behavior/smtp-server.md` § Goal
   - `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest_smtp.py::test_two_party_external_smtp_round_trip_and_reply`
36. [nest] The signature on outgoing mail verifies for a plain message and for one with an attachment alike, and is made with the key of the domain the mail is sent from — `docs/goal/behavior/mail-bridge-lifecycle.md` § DKIM provisioning (automatic)
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_signature_verifies`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_multipart_dkim_verifies`
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_an_app_send_to_an_outside_recipient_arrives_dkim_signed`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_per_domain_from_selects_second_domain_key`
37. [nest] A new server's daily sending allowance starts at fifty messages — `docs/goal/behavior/mail-deliverability.md` § The ramp
   - `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_warmup_status`
38. [nest] The allowance grows day by day along a fixed ramp, and is lifted altogether after thirty days — `docs/goal/behavior/mail-deliverability.md` § The ramp
   - (none)
39. [nest] Nothing moves the ramp back but an admin restarting it: an upgrade or a restart of the nest leaves it where it was — `docs/goal/behavior/mail-deliverability.md` § Architectural rules
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android | ⚠ partial | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_lifecycle.py::test_mail_deploy_lifecycle_to_serving` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_schema_upgrade.py::test_mail_bridge_serves_after_schema_upgrade` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_inbound_round_trip.py::test_mail_deploy_inbound_round_trip` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_outbound_submission.py::test_mail_deploy_outbound_submission` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_bidirectional_round_trip.py::test_mail_deploy_bidirectional_round_trip` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/test_mail_port25_inbound_live.py::test_port25_external_inbound_live` | — |
| 2 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_mx_round_trip` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_greylist_retry.py::test_mail_deploy_greylist_then_retry_delivered` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_greylist_defers_first_then_passes_on_retry` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_dkim_auto_enable.py::test_nest_auto_provisions_dkim_signed_outbound` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_dkim_auto_enable.py::test_nest_dkim_signature_verifies_against_published_record` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_scheduled_rotation_mints_new_selector_nestside` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_bounce_on_bad_external_recipient` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_delay_warning_emitted_once` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_deploy_forwarder_ndr.py::test_mail_deploy_forwarder_ndr_seals_locally_no_hairpin` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_enforce_refuses_mismatched_mx` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_enforce_requires_starttls` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_mta_sts_testing_mode_delivers` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_dane_pin_match_delivers` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_outbound_dane_pin_mismatch_refuses` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_over_the_ws_rpc_cap_is_never_accepted_then_lost` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_over_the_raw_inline_ceiling_now_delivers` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_message_formerly_over_the_at_rest_ceiling_now_delivers` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_role_address_postmaster_routes_to_admin` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_over_admin_lowered_quota_552_but_role_address_bypasses` | nest (linux): passed |
| 9 | nest | (none) | — |
| 10 | nest | (none) | — |
| 11 | nest | (none) | — |
| 12 | nest | (none) | — |
| 13 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_role_address_postmaster_routes_to_admin` | nest (linux): passed |
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
| 25 | nest | (none) | — |
| 26 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_inbound_graceful_shutdown_drains_and_421s` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_graceful_shutdown_byes_and_exits_clean` | nest (linux): passed |
| 27 | nest | (none) | — |
| 28 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_force_rotate_dkim_flips_active_selector_live` | nest (linux): passed |
| 29 | nest | (none) | — |
| 30 | nest | (none) | — |
| 31 | nest | (none) | — |
| 32 | nest | (none) | — |
| 33 | nest | (none) | — |
| 34 | nest | (none) | — |
| 35 | nest | `tests/e2e-unified/tests/platform/docker/test_mail_relay_two_nest_smtp.py::test_two_party_external_smtp_round_trip_and_reply` | — |
| 36 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_signature_verifies` | nest (linux): passed |
| 36 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_multipart_dkim_verifies` | nest (linux): passed |
| 36 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_an_app_send_to_an_outside_recipient_arrives_dkim_signed` | nest (linux): passed |
| 36 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_per_domain_from_selects_second_domain_key` | nest (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/api/test_mail_deliverability.py::test_admin_reads_warmup_status` | nest (linux): passed |
| 38 | nest | (none) | — |
| 39 | nest | (none) | — |
<!-- features-render:end -->
