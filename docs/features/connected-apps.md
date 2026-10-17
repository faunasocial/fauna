---
slug: connected-apps
title: Connected apps
section: your data and devices
goal: docs/goal/ui/connected-apps.md § Goal
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

One page lists everything that acts for you from outside Fauna: websites you signed
in to, apps on your other devices, apps using one of your app passwords, the Nostr
apps your nest signs for. Each row says what the app may reach in plain words, and
you disconnect it there. You connect an app on another device by typing the code it
shows you, an app on this device hands you a link that opens its request here, and a
request from an app you have never approved waits quietly on this page instead of
interrupting you.

## Coverage contract

Stamped 2026-10-02 at df61f1d791.

1. [app] Typing the code an app on another device shows you opens its approval card, and approving it connects the app — `docs/goal/ui/connected-apps.md` § User actions
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it`
2. [app] A connected app has a row saying what it may reach in plain words, and disconnecting it there ends its access — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it`
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_revoke_bites_at_the_very_next_write_and_each_session_is_ceilinged`
3. [app] A request from an app you have not approved waits on this page, and choosing never to see requests from that app removes it and keeps its later ones away — `docs/goal/ui/connected-apps.md` § User actions
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_quiet_push_lands_in_the_tray_and_never_show_blocks_the_app`
4. [nest] An app you have never approved cannot send you a notification, while one you already approved can — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_quiet_push_notifies_only_a_client_alice_has_approved`
5. [nest] An app you chose never to hear from is answered like any other, so it cannot tell that you blocked it — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_blocked_clients_push_is_answered_uniformly_and_opens_nothing`
6. [nest] Disconnecting an app ends everything it held, however many times it had signed in — `docs/goal/architecture/third-party.md` § The roster model
   - `tests/e2e-unified/tests/api/test_third_party_principals.py::test_the_consent_mints_one_principal_and_revoke_ends_everything_it_holds`
7. [app] The apps signed in to your Bluesky account and the Nostr apps your nest signs for are listed on this page only; the Bluesky and Nostr pages no longer carry lists of their own — `docs/goal/ui/connected-apps.md` § Architectural rules
   - `tests/e2e-unified/tests/test_connected_apps.py::test_the_lifted_rows_render_here_and_no_longer_on_their_old_pages`
8. [app] A Nostr app your nest signs for has its row here, and disconnecting it there stops your nest signing for it — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_nostr_signer_app_has_its_row_and_disconnecting_it_ends_the_connection`
9. [app] An app signed in with one of your Bluesky app passwords has its row here — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_an_app_signed_in_with_an_app_password_has_its_row`
10. [app] Your mail app passwords have their rows here, each with its own disconnect — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_mail_app_password_is_a_roster_row_and_no_longer_on_the_mail_page`
11. [app] An app your administrator installed on your nest has its row here, with a way to its settings — `docs/goal/ui/connected-apps.md` § Layout & flow
   - (none)
12. [app] Each row says when the app was last used and how long its access lasts — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_each_row_says_when_the_app_was_last_used_and_how_long_it_lasts`
13. [app] A code that is unknown or has expired gets one plain message asking you to get a new code from the app — `docs/goal/ui/connected-apps.md` § User actions
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_code_nobody_issued_gets_one_plain_message_and_opens_no_card`
14. [app] Declining a request tells the app no, and nothing is connected — `docs/goal/ui/connected-apps.md` § User actions
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_declined_handoff_tells_the_app_no`
15. [app] You can see the apps you chose never to hear from, and let one ask again — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_blocked_app_is_listed_and_unblock_lifts_the_block`
16. [nest] An app you allowed to read your nest's public timelines can read them over its own connection and nothing beyond what you allowed, narrowing what it may do takes effect on that connection at once, and disconnecting the app cuts the connection — `docs/goal/architecture/transport-connection.md` § Connection lifecycle
   - `tests/e2e-unified/tests/api/test_third_party_session.py::test_a_principal_session_is_gated_by_ceiling_scope_and_revoke`
   - `tests/e2e-unified/tests/api/test_third_party_session.py::test_revoking_one_grant_closes_only_that_familys_session`
17. [app] A link from an app on this device opens that app's approval card on this page, and approving it connects the app — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_handoff_link_opens_the_card_and_approving_connects_the_app`
18. [app] A link that has already been used or has expired gets one plain message asking you to start again from the app — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_handoff_link_opens_the_card_and_approving_connects_the_app`
19. [nest] An app on this device that handed you a link waits while you decide, gets its access only once you approve, and can use that link only once — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_same_device_handoff_opens_alices_row_and_its_poll_turns_into_tokens`
20. [nest] Declining the request an app on this device handed you tells that app no — `docs/goal/behavior/authorization-server.md` § Consent
   - `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_declined_handoff_answers_access_denied`
21. [nest] An app you allowed to keep its own records can write and read exactly those records as itself and nothing else of yours, and disconnecting it cuts it off while the records it wrote stay yours — `docs/goal/architecture/third-party-kinds.md` § The record doors
   - `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_connected_app_writes_and_reads_exactly_its_own_kinds`
22. [nest] An app run as a service on someone else's server cannot even ask to read your mail — `docs/goal/architecture/encryption-at-rest.md` § Capability tiering
   - `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_remote_principal_cannot_ask_for_the_accounts_mail`
23. [nest] An app you allowed to carry your conversations on another network can deliver messages into a conversation here and take your replies out, while your nest keeps both sealed and reads neither — `docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue (caller classes)
   - `tests/e2e-unified/tests/api/test_bridged_conversation_kinds.py::test_a_bridge_carries_a_conversation_both_ways_through_the_sealed_mailbox`
24. [nest] An app you allowed to put files into one of your folders can drop them in and learns nothing back but that they arrived, the files rest sealed to you, and it cannot reach any other folder or one that keeps no content on the server — `docs/goal/behavior/folder-deposit.md` § The deposit
   - `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_connected_app_deposits_into_a_folder_blind_over_both_doors`
   - `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_qualified_request_against_the_bare_declaration_deposits_without_a_choice`
25. [nest] An app that keeps its records on its own server reaches exactly those records over the web, and they are the same records the app reads on your device; a request from it that is not its own, or not properly signed, is refused, and disconnecting it shuts that door too — `docs/goal/architecture/third-party-kinds.md` § The record doors
   - `tests/e2e-unified/tests/api/test_ext_records_http.py::test_a_remote_principal_reads_and_writes_its_records_over_http`
26. [app] A file an app put into one of your folders shows up in that folder on each of your devices after their next sync, once, and is kept sealed like your other files — `docs/goal/behavior/folder-deposit.md` § The deposit
   - `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_a_parked_deposit_once`
   - `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_a_deposit_larger_than_one_message`
   - `tests/e2e-unified/tests/test_folder_deposit_adoption_on_demand_linux.py::test_an_on_demand_seat_adopts_a_parked_deposit`
27. [nest] An app you allowed to hear about changes is told when its own records in your account change — only that something changed, never what — over its connection, by asking the web, or at an address of its own it gave when you connected it, and never hears about anything else of yours — `docs/goal/architecture/transport.md` § Push events
   - `tests/e2e-unified/tests/api/test_third_party_events.py::test_a_connected_app_hears_only_its_own_scopes_change`
28. [app] Turning off Allow external apps pauses every connected app at once — including one already connected — and its row stays listed, marked paused; turning it back on lets it work again with nothing to redo — `docs/goal/behavior/atproto-pds-full.md` § Detailed design
   - `tests/e2e-unified/tests/test_connected_apps.py::test_the_external_apps_switch_pauses_a_connected_app_and_on_restores_it`
29. [nest] An app on this device you allowed to use your personalization reads and trains your trained topics and viewing cues with keys to exactly those, reads your list of trained topics but never changes it, and cannot delete what you trained — `docs/goal/behavior/authorization-server.md` § Scope grammar
   - `tests/e2e-unified/tests/api/test_third_party_session.py::test_the_personalization_arm_reads_and_trains_the_accounts_models`
30. [nest] An app run as a service on someone else's server cannot even ask to use your personalization — `docs/goal/behavior/authorization-server.md` § Scope grammar
   - `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_remote_principal_cannot_ask_for_the_accounts_personalization`
31. [nest] An app you allowed to read your feeds reads your own custom feeds, in your order, and sees which posts you liked and reposted — public posts only: a post for paying subscribers or a closed audience never reaches it, and an app allowed only your public timelines reads neither your feeds nor your likes — `docs/goal/behavior/authorization-server.md` § Scope grammar
   - `tests/e2e-unified/tests/api/test_third_party_session.py::test_the_account_arm_reads_the_accounts_feeds_over_public_posts`
32. [nest] An app you allowed to follow for you follows and unfollows accounts on the free followers tier only — it can never buy or leave a paid tier, never learns which paid tier you hold, and its follow is refused until one of your own apps has published your post-quantum key, so the author always seals to you the stronger way — `docs/goal/behavior/authorization-server.md` § Scope grammar
   - `tests/e2e-unified/tests/api/test_third_party_follow.py::test_a_principal_follows_and_reads_only_the_followers_bit`
33. [nest] An app you allowed to get Bluesky service tokens gets a short-lived one, signed with your Bluesky account's key without ever holding it, for exactly the services and actions it named when you connected it — never one that could change your account — and the moment you take that permission back its next request is refused — `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment infrastructure
   - `tests/e2e-unified/tests/api/test_atproto_oracle.py::test_a_principal_gets_a_service_token_minted_by_the_bridge_for_its_declared_pair_only`
34. [nest] An app you allowed to put files into one of your folders can drop in files of several megabytes, each resting sealed to you, and it cannot fill your space: once the room your plan gives what apps drop in is full, it is refused until your devices take the files in — `docs/goal/behavior/folder-deposit.md` § The inbox quota
   - `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_deposit_larger_than_one_message_rests_sealed_and_the_inbox_quota_binds`
35. [app] An app your nest has used your keys for shows on its row how many times that happened in the current window and what the most recent use was and when, worded as its approval card worded the permission — `docs/goal/ui/connected-apps.md` § Layout & flow
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_principal_the_oracle_served_shows_what_the_nest_did_on_its_row`
36. [nest] An app you allowed to post for you publishes public posts as you, signed with a key your nest keeps for posting that your approval set up, never your own; it can never post to a paying or closed audience, it can take back only its own posts, and the moment you revoke that posting key its next post is refused — `docs/goal/architecture/key-material-hierarchy.md` § Audience: deployment infrastructure
   - `tests/e2e-unified/tests/api/test_post_author_principal.py::test_a_principal_authors_and_retracts_public_posts_until_the_delegation_is_revoked`
37. [app] A file an app put into a folder you share with someone shows up for them too, after their next sync, sealed like the folder's other files — only your devices ever open what the app dropped in — `docs/goal/behavior/folder-deposit.md` § The deposit
   - `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_into_a_bound_folder_and_a_member_receives_it`
38. [nest] An app you allowed to read one of your folders reads its files over WebDAV exactly as you saved them, and nothing else: no other folder of yours, nothing without the keys your approval gave it, and nothing from the moment you stop serving that folder — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_device_principal_reads_a_served_folder_over_dav_through_the_consent_path`
   - `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_qualified_request_against_the_bare_declaration_reads_without_a_choice`
39. [nest] Only an app running on your own device can ask to read a folder, and only one you already serve over WebDAV: approving a read of a folder you do not serve is turned down with the reason, grants nothing and leaves the request waiting — `docs/goal/behavior/webdav-server.md` § Key model
   - `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_confidential_client_is_refused_the_folder_read_scope_at_the_push`
   - `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_the_approve_refuses_a_folder_the_owner_does_not_serve`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.4-dev+60e6f5d5 standalone |
| linux | ⚠ partial | 0.1.4-dev+60e6f5d5 standalone |
| windows | ⚠ partial | 0.1.2-dev+be5542fa.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+e17a00b6.dirty standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+46de983d.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_revoke_bites_at_the_very_next_write_and_each_session_is_ceilinged` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_quiet_push_lands_in_the_tray_and_never_show_blocks_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_quiet_push_notifies_only_a_client_alice_has_approved` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_blocked_clients_push_is_answered_uniformly_and_opens_nothing` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_third_party_principals.py::test_the_consent_mints_one_principal_and_revoke_ends_everything_it_holds` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_the_lifted_rows_render_here_and_no_longer_on_their_old_pages` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_nostr_signer_app_has_its_row_and_disconnecting_it_ends_the_connection` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_an_app_signed_in_with_an_app_password_has_its_row` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_mail_app_password_is_a_roster_row_and_no_longer_on_the_mail_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 11 | app | (none) | — |
| 12 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_each_row_says_when_the_app_was_last_used_and_how_long_it_lasts` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_code_nobody_issued_gets_one_plain_message_and_opens_no_card` | tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_declined_handoff_tells_the_app_no` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed |
| 15 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_blocked_app_is_listed_and_unblock_lifts_the_block` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_third_party_session.py::test_a_principal_session_is_gated_by_ceiling_scope_and_revoke` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_third_party_session.py::test_revoking_one_grant_closes_only_that_familys_session` | nest (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_handoff_link_opens_the_card_and_approving_connects_the_app` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed |
| 18 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_handoff_link_opens_the_card_and_approving_connects_the_app` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_same_device_handoff_opens_alices_row_and_its_poll_turns_into_tokens` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/api/test_oauth_consent_starts.py::test_a_declined_handoff_answers_access_denied` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_connected_app_writes_and_reads_exactly_its_own_kinds` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_remote_principal_cannot_ask_for_the_accounts_mail` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/api/test_bridged_conversation_kinds.py::test_a_bridge_carries_a_conversation_both_ways_through_the_sealed_mailbox` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_connected_app_deposits_into_a_folder_blind_over_both_doors` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_qualified_request_against_the_bare_declaration_deposits_without_a_choice` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_ext_records_http.py::test_a_remote_principal_reads_and_writes_its_records_over_http` | nest (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_a_parked_deposit_once` | tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_a_deposit_larger_than_one_message` | tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_folder_deposit_adoption_on_demand_linux.py::test_an_on_demand_seat_adopts_a_parked_deposit` | linux (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_third_party_events.py::test_a_connected_app_hears_only_its_own_scopes_change` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_the_external_apps_switch_pauses_a_connected_app_and_on_restores_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 29 | nest | `tests/e2e-unified/tests/api/test_third_party_session.py::test_the_personalization_arm_reads_and_trains_the_accounts_models` | nest (linux): passed |
| 30 | nest | `tests/e2e-unified/tests/api/test_ext_kinds.py::test_a_remote_principal_cannot_ask_for_the_accounts_personalization` | nest (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_third_party_session.py::test_the_account_arm_reads_the_accounts_feeds_over_public_posts` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_third_party_follow.py::test_a_principal_follows_and_reads_only_the_followers_bit` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_atproto_oracle.py::test_a_principal_gets_a_service_token_minted_by_the_bridge_for_its_declared_pair_only` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/api/test_folder_deposit.py::test_a_deposit_larger_than_one_message_rests_sealed_and_the_inbox_quota_binds` | nest (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_principal_the_oracle_served_shows_what_the_nest_did_on_its_row` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 36 | nest | `tests/e2e-unified/tests/api/test_post_author_principal.py::test_a_principal_authors_and_retracts_public_posts_until_the_delegation_is_revoked` | nest (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_folder_deposit_adoption.py::test_an_owner_seat_adopts_into_a_bound_folder_and_a_member_receives_it` | tui (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_device_principal_reads_a_served_folder_over_dav_through_the_consent_path` | nest (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_qualified_request_against_the_bare_declaration_reads_without_a_choice` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_a_confidential_client_is_refused_the_folder_read_scope_at_the_push` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/api/test_folder_read_principal.py::test_the_approve_refuses_a_folder_the_owner_does_not_serve` | nest (linux): passed |
<!-- features-render:end -->
