---
slug: standard-mail-apps
title: Use a regular mail app
section: mail, calendar and contacts
goal: docs/goal/behavior/mail-credentials.md § MUA setup conventions
guide: docs/guides/own-your-mail.md § Connecting a regular mail app
---

## What a user gets

Thunderbird, Apple Mail or any IMAP client reads and sends your mail with the
address and app password the Fauna app shows you: one password for reading, sending
and your calendar alike, a second password if you want one, and mail that reaches
your inbox the moment it arrives. Your mail rests sealed on the nest; the server
unseals it only for a client that holds your password.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A mail app configured with the settings the app shows receives and sends mail — `docs/goal/behavior/mail-credentials.md` § MUA setup conventions
   - `tests/e2e-unified/tests/test_mail_enable_then_mua_round_trip.py::test_client_enabled_mail_round_trips_through_a_normal_mua`
   - `tests/e2e-unified/tests/test_mail_enable_then_mua_round_trip.py::test_client_enabled_mail_submits_outbound_through_submission`
   - `tests/e2e-unified/tests/test_mail_send_external_and_imap_auth.py::test_one_credential_sends_to_external_and_imap_authenticates`
2. [app] A second app password works for mail and calendar alike, and the username forms a mail app might use are all accepted — `docs/goal/behavior/mail-credentials.md` § MUA setup conventions
   - `tests/e2e-unified/tests/test_mail_multi_credential_auth.py::test_second_credential_authenticates_over_caldav_and_imap`
   - `tests/e2e-unified/tests/test_mail_bare_username_auth.py::test_default_credential_username_form_matrix`
3. [nest] The mail server signs a client in with its password, serves mailboxes, pushes new mail at once, searches inside messages and reports quota — `docs/goal/behavior/imap-server.md` § Goal
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_login_and_select`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_wrong_password_rejected`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_append_fetch_roundtrip`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_idle_exists_push`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_search_body_axis_decrypts_index_hint`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_getquotaroot_reports_root_and_limits`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_qresync_select_inline_vanished_earlier`
4. [nest] Mail that arrives from outside reads back through the mail app byte for byte, including very large messages — `docs/goal/behavior/imap-server.md` § Content path
   - `tests/e2e-unified/tests/test_mail_inbound_to_imap.py::test_inbound_smtp_delivers_and_imap_fetch_decrypts`
   - `tests/e2e-unified/tests/test_mail_inbound_to_imap.py::test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte`
5. [nest] Mail sent from a mail app leaves signed, from your own address or an alias of yours, never as someone else, and without the record of your own device and network that a mail app stamps on it — `docs/goal/behavior/smtp-server.md` § Outbound submission flow (per-user view)
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_round_trip`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_signature_verifies`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_mail_from_owned_alias_accepted`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_mail_from_other_actor_rejected`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_from_header_other_actor_rejected`
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_strips_received_headers`
6. [nest] Your mail app always connects encrypted, and a password is never accepted over an unencrypted connection — `docs/goal/behavior/imap-server.md` § Don't do these
   - `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_cleartext_143_refuses_every_password_path`
7. [nest] Your mail app finds Inbox, Sent, Drafts, Junk, Trash and Archive already there and recognised for what they are, and they cannot be deleted or renamed — `docs/goal/behavior/imap-server.md` § Standard mailboxes
   - `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_six_special_use_mailboxes_seeded_and_protected`
8. [nest] Folders you create, and which folders you subscribe to, show the same in every mail app you use; a folder that still holds mail is not deleted by default — `docs/goal/behavior/imap-server.md` § Subscriptions
   - `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_folders_and_subscriptions_are_the_same_in_every_mail_app`
9. [nest] Reading, flagging, moving or deleting a message in one mail app shows up right away in your other mail apps, and a message you read in the app shows as read there too — `docs/goal/behavior/imap-server.md` § Push wiring
   - `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_changes_in_one_mail_app_are_pushed_to_another`
10. [nest] Opening a message in your mail app marks it read — `docs/goal/behavior/imap-server.md` § Body-section FETCH (RFC 9051 §6.4.5)
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_seen_on_non_peek_fetch`
11. [nest] Your mail app can fetch just a message's headers or a single attachment, decoded, without downloading the whole message — `docs/goal/behavior/imap-server.md` § Body-section FETCH (RFC 9051 §6.4.5)
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_sectioned_body_fetch`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_numbered_part_body_fetch`
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_binary_section_fetch`
12. [nest] After your nest is restored from a backup, a mail app holding newer state resyncs to what the nest holds instead of showing a mix — `docs/goal/behavior/imap-server.md` § Restore divergence detection
   - `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_mua_ahead_of_restored_nest_is_told_to_resync`
13. [nest] Your mail app sorts and filters mail by when it actually arrived, so a sender cannot backdate a message to hide it — `docs/goal/behavior/imap-server.md` § SEARCH
   - `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_internaldate_is_server_time_not_the_date_header`
14. [nest] When your mailbox is full, your mail app is told so when saving, copying or moving mail; deleting always still works, and one person's full mailbox never affects another's — `docs/goal/behavior/imap-server.md` § Quota enforcement points
   - `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_a_full_mailbox_refuses_saves_but_never_deletes`
15. [nest] When you send from a mail app to several people and one address on your nest does not exist, only that address is refused and everyone else still gets it — `docs/goal/behavior/smtp-server.md` § Recipient handling on submission (per-recipient, RCPT-time)
   - `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_partial_local_failure_delivers_rest`
16. [nest] A message too large to send from a mail app is refused as too large at once, not bounced later — `docs/goal/behavior/smtp-server.md` § Recipient handling on submission (per-recipient, RCPT-time)
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_an_oversize_mail_app_send_is_refused_at_once`
17. [nest] Mail you send from a mail app is saved to your Sent folder, where every mail app sees it — `docs/goal/behavior/smtp-server.md` § First-party client send + receive
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_mail_app_send_is_saved_to_sent`
18. [nest] Your mail app is told when a message has too many recipients or you have used today's sending allowance, and that allowance is shared with mail you send from the app and counts each recipient outside this nest once — `docs/goal/behavior/smtp-server.md` § Architectural rules
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_recipient_cap_and_daily_allowance_shared_with_app_sends`
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_a_multi_recipient_mail_app_send_spends_one_unit_per_outside_recipient`
19. [nest] After too many wrong passwords in a short time, further tries are briefly refused — `docs/goal/behavior/smtp-server.md` § Auth on each port
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_repeated_wrong_passwords_are_briefly_refused`
20. [nest] Older mail apps and tools that only speak the older mail dialect or the plain login command still sign in, over an encrypted connection — `docs/goal/behavior/imap-server.md` § Authentication
   - `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_login_command_and_rev1_dialect_over_tls`
21. [nest] Mail you send from a mail app to someone else on your nest lands straight in their inbox — `docs/goal/behavior/smtp-server.md` § Outbound submission flow (per-user view)
   - `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_mail_app_send_to_a_local_user_lands_in_their_inbox`
22. [nest] If the nest is briefly unavailable while a mail app is sending, the app is told to try again instead of the message being accepted and lost — `docs/goal/behavior/smtp-server.md` § Recipient handling on submission
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| linux | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| windows | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| macos | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| ios | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| android | ⚠ partial | 0.1.2-dev+74e26248 standalone |
| tui | ⚠ partial | 0.1.2-dev+74e26248 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_mail_enable_then_mua_round_trip.py::test_client_enabled_mail_round_trips_through_a_normal_mua` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_enable_then_mua_round_trip.py::test_client_enabled_mail_submits_outbound_through_submission` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_mail_send_external_and_imap_auth.py::test_one_credential_sends_to_external_and_imap_authenticates` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_multi_credential_auth.py::test_second_credential_authenticates_over_caldav_and_imap` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_mail_bare_username_auth.py::test_default_credential_username_form_matrix` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_login_and_select` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_wrong_password_rejected` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_append_fetch_roundtrip` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_idle_exists_push` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_search_body_axis_decrypts_index_hint` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_getquotaroot_reports_root_and_limits` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_qresync_select_inline_vanished_earlier` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_inbound_to_imap.py::test_inbound_smtp_delivers_and_imap_fetch_decrypts` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/test_mail_inbound_to_imap.py::test_an_over_frame_inbound_message_delivers_by_reference_and_fetches_byte_for_byte` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_round_trip` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_dkim_signature_verifies` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_mail_from_owned_alias_accepted` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_mail_from_other_actor_rejected` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_from_header_other_actor_rejected` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_strips_received_headers` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_cleartext_143_refuses_every_password_path` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_six_special_use_mailboxes_seeded_and_protected` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_folders_and_subscriptions_are_the_same_in_every_mail_app` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_changes_in_one_mail_app_are_pushed_to_another` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_seen_on_non_peek_fetch` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_sectioned_body_fetch` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_numbered_part_body_fetch` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_imap_binary_section_fetch` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_mua_ahead_of_restored_nest_is_told_to_resync` | nest (linux): passed |
| 13 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_internaldate_is_server_time_not_the_date_header` | nest (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_sessions.py::test_a_full_mailbox_refuses_saves_but_never_deletes` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/test_mail_bridge_mta.py::test_submission_partial_local_failure_delivers_rest` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_an_oversize_mail_app_send_is_refused_at_once` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_mail_app_send_is_saved_to_sent` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_recipient_cap_and_daily_allowance_shared_with_app_sends` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_a_multi_recipient_mail_app_send_spends_one_unit_per_outside_recipient` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_repeated_wrong_passwords_are_briefly_refused` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda_mailboxes.py::test_login_command_and_rev1_dialect_over_tls` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/test_mail_bridge_mua_submission.py::test_mail_app_send_to_a_local_user_lands_in_their_inbox` | nest (linux): passed |
| 22 | nest | (none) | — |
<!-- features-render:end -->
