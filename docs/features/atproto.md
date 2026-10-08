---
slug: atproto
title: AT Protocol
section: bridges and other networks
goal: docs/goal/ui/atproto.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § Bluesky, the destination: your nest as your Bluesky home
---

## What a user gets

Choose how deep Bluesky goes: off, your existing account linked, or your nest
as your Bluesky home, visible to the network or fully login-able so other Bluesky apps
sign in through it. Each step up shows what it will do before you confirm. App
passwords and third-party posting are yours to grant and revoke; the app watches the
public directory for anyone trying to take your identity and can contest it.

## Coverage contract

Stamped 2026-10-01 at 065ed2e2c2.

1. [app] The depth selector moves between off, linked and hosted, with a card explaining each hosted step first — `docs/goal/ui/atproto.md` § Transition semantics
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_atproto_settings_page_renders`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_depth_selector_renders_at_off`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_off_linked_cardless_ladder`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_linked_panel_reuses_the_shared_bridge_surface`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_hosted_ladder_through_the_card`
2. [app] App passwords for other Bluesky apps mint, reveal, and revoke, and the kill switch suspends them without deleting them — `docs/goal/behavior/atproto-pds-full.md` § F1 detail — app credentials, `createSession`, tokens
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_mint_reveal_and_revoke_round_trip`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_kill_switch_is_non_destructive`
3. [app] Letting other apps post as you is granted, renewed and revoked, and a lapsed grant reads as renewable — `docs/goal/behavior/atproto-pds-full.md` § D10
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_authorize_and_revoke_external_posting`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_reauthorizing_needs_no_revoke_first`
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_lapse_reads_as_reauthorize_here`
4. [app] Deleting your presence asks in its own dialog, then removes everything you published there and keeps your identity unless you chose to retire it too — `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_delete_presence_ceremony`
   - `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_retire_identity_opt_in_rides_the_delete_ceremony`
5. [app] A third-party app's sign-in shows you a code to approve in your own app, and a decline is clean — `docs/goal/behavior/atproto-pds-full.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_consent_ceremony_end_to_end`
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_declining_fails_the_browser_cleanly`
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_the_nests_own_authorization_server_issues_and_revokes_a_grant`
6. [app] An attempt to take your hosted identity alarms you, a forged one is never signed, and you can contest it within the window — `docs/goal/behavior/atproto-identity-custody.md` § The 72 h recovery-fork contest
   - `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_a_forged_seizure_alarms_but_is_never_signed`
   - `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_recovery_fork_contest_end_to_end`
7. [nest] Your nest mints your AT Protocol identity, publishes your public posts on the account it hosts for you, and accepts posts from other Bluesky apps — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_identity_mint.py::test_did_plc_mint_end_to_end`
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
   - `tests/e2e-unified/tests/test_atproto_external_write.py::test_external_app_write_becomes_a_fauna_post`
   - `tests/e2e-unified/tests/test_atproto_bridge_enroll.py::test_atproto_bridge_enrolls_end_to_end`
8. [nest] Other Bluesky apps sign in through your nest and read their timeline through it — `docs/goal/behavior/atproto-pds-full.md` § F3 detail — the D8 module, service proxying, preferences, DM switch
   - `tests/e2e-unified/tests/test_atproto_pds_auth.py::test_atproto_pds_f1_auth_core_end_to_end`
   - `tests/e2e-unified/tests/test_atproto_pds_proxy.py::test_atproto_pds_service_proxy_end_to_end`
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_the_pds_sends_clients_to_the_nests_authorization_server`
9. [nest] The Bluesky service in the released image stays off until you turn it on, and then answers at its own address — `docs/goal/behavior/atproto-pds-bridge.md` § Enable UX
   - `tests/e2e-unified/tests/platform/docker/test_atproto_hosted_enable_docker.py::test_atproto_hosted_enable_boots_docker_bridge`
   - `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_sni_router_routes_pds_to_the_atproto_bridge`
   - `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_createsession_rate_limit_carries_real_client_ip`
10. [app] Likes, replies, reposts, quotes, follows and mentions from Bluesky arrive in your notification list beside your Fauna ones — `docs/goal/behavior/bridges.md` § Notifications
   - `tests/e2e-unified/tests/test_bluesky_notifications_bridged.py::test_a_bridged_bluesky_notification_reaches_the_unified_list`
11. [app] Posts from the people you follow on Bluesky arrive in your feed and you can reply to them from Fauna — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/test_bluesky_feed_ingest.py::test_a_followed_bluesky_post_arrives_in_the_feed_and_a_reply_threads_under_it`
12. [nest] Only what you post after you start hosting is published; your earlier posts stay unpublished unless you asked for them — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
13. [app] Turning hosting on offers to also publish your existing public posts, and choosing it puts them on your Bluesky account — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - (none)
14. [nest] When your Fauna handle changes, your Bluesky handle follows it and the network is told — `docs/goal/behavior/atproto-pds-bridge.md` § Handle — derived from the Fauna handle
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
15. [nest] Your profile name and picture appear on your Bluesky account and follow your edits, a removed picture included — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
16. [nest] Images and video in your public posts appear with them on Bluesky — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
17. [nest] Your reply to a post that is on Bluesky threads under it there, and a reply to one that is not still publishes on its own — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
18. [nest] Deleting one of your posts also removes it from the Bluesky account your nest hosts — `docs/goal/behavior/atproto-pds-bridge.md` § Firm boundaries
   - (none)
19. [nest] Only your public posts go to Bluesky; a post you restricted, gated or sold never does — `docs/goal/behavior/atproto-pds-bridge.md` § Firm boundaries
   - (none)
20. [nest] Stepping down from hosting takes your Bluesky account offline at once, and stepping back up restores the same identity with nothing lost — `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
21. [nest] Starting to host your Bluesky home disconnects an account you had linked, and leaves that account itself untouched — `docs/goal/ui/atproto.md` § Transition semantics
   - (none)
22. [app] The first time you host, you choose between a portable identity, recommended, and one tied to your domain, and your handle is the same either way — `docs/goal/behavior/atproto-pds-bridge.md` § Enable UX
   - (none)
23. [app] After you step down, your hosted identity still shows, marked as deactivated, so you can see what stepping back up restores — `docs/goal/ui/atproto.md` § Errors & edge cases
   - (none)
24. [app] A change of level that fails leaves your level as it was and the card open, so you can try again — `docs/goal/ui/atproto.md` § Errors & edge cases
   - (none)
25. [nest] Hosting again after you permanently retired an identity gives you a brand-new one, never the retired one — `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
   - (none)
26. [nest] Your nest announces itself to the Bluesky network, so your posts show up in Bluesky apps and people there can follow you — `docs/goal/behavior/atproto-pds-bridge.md` § Goal
   - (none)
27. [app] You see every app signed in to your Bluesky account, with its name and what it may do, and revoking one ends its access — `docs/goal/behavior/atproto-pds-full.md` § Problem 3
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it`
28. [app] An app password you made can be shown again later, on any of your devices — `docs/goal/behavior/atproto-pds-full.md` § Implementation status today
   - (none)
29. [nest] An app signed in as you can never create, list or revoke app passwords or delete your account; those stay in your own app — `docs/goal/behavior/atproto-pds-full.md` § Problem 4
   - (none)
30. [app] When an app asks for a named bundle of permissions, the approval card shows the bundle's name and description and every permission in it — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_a_permission_set_resolves_through_the_bridge_onto_the_card_and_into_the_token`
31. [app] A connected app's row shows which permission bundle its permissions came from, as the approval card showed it — `docs/goal/behavior/atproto-oauth-provider.md` § Implementation status today
   - (none)
32. [nest] An app asking for a permission bundle your nest cannot verify is refused before you are ever asked to approve it — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_an_unresolvable_permission_set_is_refused_with_the_bridge_present`
33. [nest] An app you approved keeps exactly the permissions the card showed, even if the bundle's publisher changes it later — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - (none)
34. [app] A posting permission your identity never signed is shown as a mismatch, never as granted — `docs/goal/behavior/atproto-pds-full.md` § App surface
   - (none)
35. [nest] Posts already published through another app stay up after you stop letting other apps post as you — `docs/goal/behavior/atproto-pds-full.md` § D10
   - (none)
36. [app] When an attempt to take your identity can no longer be undone, the page says so plainly instead of offering a button that does nothing — `docs/goal/behavior/atproto-identity-custody.md` § The 72 h recovery-fork contest
   - (none)
37. [nest] With cross-posting set to automatic, every post you write is also published to your linked Bluesky account — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - (none)
38. [nest] With cross-posting set to manual, only the posts you tag for it are published to your linked Bluesky account — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - (none)
39. [nest] Deleting a post that was cross-posted to your linked Bluesky account removes the copy there — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - (none)
40. [app] Quoting a Bluesky post from Fauna publishes your quote on Bluesky carrying the post you quoted, whatever your cross-posting setting — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - (none)
41. [app] Liking or reposting a Bluesky post in your feed, and undoing it, does the same on Bluesky through your linked account — `docs/goal/behavior/bridges.md` § Interactions
   - (none)
42. [app] Replying to or quoting a Bluesky post with no Bluesky account linked is refused with a message telling you where to link one — `docs/goal/behavior/bridges.md` § Interactions
   - (none)
43. [app] Posts from a Bluesky custom feed you subscribed to arrive in your feed — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - (none)
44. [nest] A Bluesky post appears in your feed once, however often it is fetched and however many of your linked accounts see it — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - (none)
45. [nest] A repost in your Bluesky timeline arrives as the original post under its original author — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - (none)
46. [nest] A Bluesky post its author labelled arrives carrying that label as a content warning — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - (none)
47. [app] When you make an app password you choose whether the app using it may read and send your Bluesky direct messages — `docs/goal/behavior/atproto-pds-full.md` § D3
   - (none)
48. [nest] An app signed in with an app password reaches your Bluesky direct messages only when that password was made with direct-message access — `docs/goal/behavior/atproto-pds-full.md` § F3 detail — the D8 module, service proxying, preferences, DM switch
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_atproto_settings_page_renders` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_depth_selector_renders_at_off` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_off_linked_cardless_ladder` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_linked_panel_reuses_the_shared_bridge_surface` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_hosted_ladder_through_the_card` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_mint_reveal_and_revoke_round_trip` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_kill_switch_is_non_destructive` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_authorize_and_revoke_external_posting` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_reauthorizing_needs_no_revoke_first` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_lapse_reads_as_reauthorize_here` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_delete_presence_ceremony` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_retire_identity_opt_in_rides_the_delete_ceremony` | web (linux): skipped, linux (linux): skipped, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_consent_ceremony_end_to_end` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_declining_fails_the_browser_cleanly` | web (linux): passed, linux (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_the_nests_own_authorization_server_issues_and_revokes_a_grant` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_a_forged_seizure_alarms_but_is_never_signed` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_recovery_fork_contest_end_to_end` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_atproto_identity_mint.py::test_did_plc_mint_end_to_end` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_atproto_external_write.py::test_external_app_write_becomes_a_fauna_post` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_atproto_bridge_enroll.py::test_atproto_bridge_enrolls_end_to_end` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_atproto_pds_auth.py::test_atproto_pds_f1_auth_core_end_to_end` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_atproto_pds_proxy.py::test_atproto_pds_service_proxy_end_to_end` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_the_pds_sends_clients_to_the_nests_authorization_server` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_hosted_enable_docker.py::test_atproto_hosted_enable_boots_docker_bridge` | — |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_sni_router_routes_pds_to_the_atproto_bridge` | — |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_createsession_rate_limit_carries_real_client_ip` | — |
| 10 | app | `tests/e2e-unified/tests/test_bluesky_notifications_bridged.py::test_a_bridged_bluesky_notification_reaches_the_unified_list` | linux (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_bluesky_feed_ingest.py::test_a_followed_bluesky_post_arrives_in_the_feed_and_a_reply_threads_under_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 13 | app | (none) | — |
| 14 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 21 | nest | (none) | — |
| 22 | app | (none) | — |
| 23 | app | (none) | — |
| 24 | app | (none) | — |
| 25 | nest | (none) | — |
| 26 | nest | (none) | — |
| 27 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 28 | app | (none) | — |
| 29 | nest | (none) | — |
| 30 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_a_permission_set_resolves_through_the_bridge_onto_the_card_and_into_the_token` | web (linux): passed, linux (linux): passed |
| 31 | app | (none) | — |
| 32 | nest | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_an_unresolvable_permission_set_is_refused_with_the_bridge_present` | nest (linux): passed |
| 33 | nest | (none) | — |
| 34 | app | (none) | — |
| 35 | nest | (none) | — |
| 36 | app | (none) | — |
| 37 | nest | (none) | — |
| 38 | nest | (none) | — |
| 39 | nest | (none) | — |
| 40 | app | (none) | — |
| 41 | app | (none) | — |
| 42 | app | (none) | — |
| 43 | app | (none) | — |
| 44 | nest | (none) | — |
| 45 | nest | (none) | — |
| 46 | nest | (none) | — |
| 47 | app | (none) | — |
| 48 | nest | (none) | — |
<!-- features-render:end -->
