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
   - `tests/e2e-unified/tests/test_atproto_backfill_count.py::test_history_opt_in_names_the_public_post_count`
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
   - `tests/e2e-unified/tests/test_atproto_history_backfill.py::test_choosing_history_publishes_existing_public_posts`
14. [nest] When your Fauna handle changes, your Bluesky handle follows it and the network is told — `docs/goal/behavior/atproto-pds-bridge.md` § Handle — derived from the Fauna handle
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
15. [nest] Your profile name and picture appear on your Bluesky account and follow your edits, a removed picture included — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
16. [nest] Images and video in your public posts appear with them on Bluesky — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
17. [nest] Your reply to a post that is on Bluesky threads under it there, and a reply to one that is not still publishes on its own — `docs/goal/behavior/atproto-pds-bridge.md` § Projection & backfill
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
18. [nest] Deleting one of your posts also removes it from the Bluesky account your nest hosts — `docs/goal/behavior/atproto-pds-bridge.md` § Firm boundaries
   - `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_deleting_a_post_removes_it_from_the_hosted_account`
19. [nest] Only your public posts go to Bluesky; a post you restricted, gated or sold never does — `docs/goal/behavior/atproto-pds-bridge.md` § Firm boundaries
   - `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_a_gated_or_sold_post_never_reaches_the_hosted_account`
20. [nest] Stepping down from hosting takes your Bluesky account offline at once, and stepping back up restores the same identity with nothing lost — `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
   - `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose`
21. [nest] Starting to host your Bluesky home disconnects an account you had linked, and leaves that account itself untouched — `docs/goal/ui/atproto.md` § Transition semantics
   - `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_starting_to_host_unlinks_the_linked_account_and_leaves_it_untouched`
22. [app] The first time you host, you choose between a portable identity, recommended, and one tied to your domain, and your handle is the same either way — `docs/goal/behavior/atproto-pds-bridge.md` § Enable UX
   - `tests/e2e-unified/tests/test_atproto_did_method.py::test_either_identity_choice_mints_the_same_handle`
23. [app] After you step down, your hosted identity still shows, marked as deactivated, so you can see what stepping back up restores — `docs/goal/ui/atproto.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_a_stepped_down_identity_shows_marked_deactivated`
24. [app] A hosted level your nest cannot offer is greyed with the reason instead of failing after you confirm, and your level stays as it was — `docs/goal/ui/atproto.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_depth_selector_renders_at_off`
25. [nest] Hosting again after you permanently retired an identity gives you a brand-new one, never the retired one — `docs/goal/behavior/atproto-pds-bridge.md` § Disable & revocation
   - `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_hosting_again_after_a_retirement_mints_a_fresh_identity`
26. [nest] Your nest announces itself to the Bluesky network, so your posts show up in Bluesky apps and people there can follow you — `docs/goal/behavior/atproto-pds-bridge.md` § Goal
   - `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_the_nest_announces_itself_to_the_relay_once_the_identity_resolves`
27. [app] You see every app signed in to your Bluesky account, with its name and what it may do, and revoking one ends its access — `docs/goal/behavior/atproto-pds-full.md` § Problem 3
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it`
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_revoke_bites_at_the_very_next_write_and_each_session_is_ceilinged`
28. [app] An app password you made can be shown again later, on any of your devices — `docs/goal/behavior/atproto-pds-full.md` § Implementation status today
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_an_app_password_is_shown_again_on_another_device`
29. [nest] An app signed in as you can never create, list or revoke app passwords or delete your account; those stay in your own app — `docs/goal/behavior/atproto-pds-full.md` § Problem 4
   - `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_an_app_signed_in_as_you_cannot_manage_app_passwords_or_delete_the_account`
30. [app] When an app asks for a named bundle of permissions, the approval card shows the bundle's name and description and every permission in it — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_a_permission_set_resolves_through_the_bridge_onto_the_card_and_into_the_token`
31. [app] A connected app's row shows which permission bundle its permissions came from, as the approval card showed it — `docs/goal/behavior/atproto-oauth-provider.md` § Implementation status today
   - `tests/e2e-unified/tests/test_connected_apps.py::test_a_connected_apps_row_names_the_permission_set_it_came_from`
32. [nest] An app asking for a permission bundle your nest cannot verify is refused before you are ever asked to approve it — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_an_unresolvable_permission_set_is_refused_with_the_bridge_present`
33. [nest] An app you approved keeps exactly the permissions the card showed, even if the bundle's publisher changes it later — `docs/goal/behavior/atproto-oauth-provider.md` § F4 detail — the OAuth provider
   - `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_an_approved_app_keeps_the_permissions_its_card_showed_after_the_set_changes`
34. [app] A posting permission your identity never signed is shown as a mismatch, never as granted — `docs/goal/behavior/atproto-pds-full.md` § App surface
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_an_unsigned_posting_permission_shows_as_a_mismatch`
35. [nest] Posts already published through another app stay up after you stop letting other apps post as you — `docs/goal/behavior/atproto-pds-full.md` § D10
   - `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_posts_published_through_another_app_stay_up_after_posting_is_revoked`
36. [app] When an attempt to take your identity can no longer be undone, the page says so plainly instead of offering a button that does nothing — `docs/goal/behavior/atproto-identity-custody.md` § The 72 h recovery-fork contest
   - `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_a_seizure_past_its_window_says_so_and_offers_no_button`
37. [nest] With cross-posting set to automatic, every post you write is also published to your linked Bluesky account — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_on_automatic_every_post_is_published_to_the_linked_account`
38. [nest] With cross-posting set to manual, only the posts you tag for it are published to your linked Bluesky account — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_on_manual_only_posts_tagged_for_it_are_published`
39. [nest] Deleting a post that was cross-posted to your linked Bluesky account removes the copy there — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_deleting_a_cross_posted_post_removes_its_copy`
40. [app] Quoting a Bluesky post from Fauna publishes your quote on Bluesky carrying the post you quoted, whatever your cross-posting setting — `docs/goal/behavior/bridges.md` § Cross-posting (Fauna → Bluesky)
   - `tests/e2e-unified/tests/test_bluesky_feed_reference.py::test_a_quote_of_a_bluesky_post_is_published_on_bluesky_carrying_it`
41. [app] Liking or reposting a Bluesky post in your feed, and undoing it, does the same on Bluesky through your linked account — `docs/goal/behavior/bridges.md` § Interactions
   - `tests/e2e-unified/tests/test_bluesky_feed_interactions.py::test_a_bluesky_like_or_repost_and_its_undo_reach_bluesky`
42. [app] Replying to or quoting a Bluesky post with no Bluesky account linked is refused with a message telling you where to link one — `docs/goal/behavior/bridges.md` § Interactions
   - `tests/e2e-unified/tests/test_bluesky_feed_reference.py::test_replying_or_quoting_with_no_bluesky_account_linked_is_refused_with_the_remedy`
43. [app] Posts from a Bluesky custom feed you subscribed to arrive in your feed — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/test_bluesky_feed_ingest.py::test_posts_from_a_subscribed_bluesky_custom_feed_arrive_in_the_feed`
44. [nest] A Bluesky post appears in your feed once, however often it is fetched and however many of your linked accounts see it — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_bluesky_post_lands_in_the_feed_once_across_polls_and_linked_accounts`
45. [nest] A repost in your Bluesky timeline arrives as the original post under its original author — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_bluesky_repost_arrives_as_the_original_post_under_its_author`
46. [nest] A Bluesky post its author labelled arrives carrying that label as a content warning — `docs/goal/behavior/bridges.md` § Unified feed ingestion
   - `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_self_labelled_bluesky_post_arrives_carrying_its_label_as_a_content_warning`
47. [app] When you make an app password you choose whether the app using it may read and send your Bluesky direct messages — `docs/goal/behavior/atproto-pds-full.md` § D3
   - `tests/e2e-unified/tests/test_atproto_settings.py::test_an_app_password_carries_dm_access_only_when_the_toggle_was_on`
48. [nest] An app signed in with an app password reaches your Bluesky direct messages only when that password was made with direct-message access — `docs/goal/behavior/atproto-pds-full.md` § F3 detail — the D8 module, service proxying, preferences, DM switch
   - `tests/e2e-unified/tests/test_atproto_pds_proxy.py::test_atproto_app_password_reaches_dms_only_with_dm_access`
49. [app] Opening the thread of a Bluesky post in your feed shows the conversation around it, replies from people you do not follow included, with the post you opened marked, and tells you when the post is gone from Bluesky — `docs/goal/behavior/bridges.md` § Bluesky-native thread view
   - `tests/e2e-unified/tests/test_bluesky_thread_view.py::test_a_bluesky_posts_thread_shows_the_conversation_with_the_opened_post_marked`
   - `tests/e2e-unified/tests/test_bluesky_thread_view.py::test_a_thread_whose_post_is_gone_at_its_origin_says_so`
50. [app] A Bluesky direct message arrives in Conversations as a Bluesky conversation with that person, and your reply reaches them alone — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_bridged_dm_legs.py::test_a_bluesky_dm_arrives_as_a_bluesky_room_and_the_reply_reaches_that_peer_alone`
51. [app] Typing a Bluesky handle, with or without its `@`, into the recipient picker starts a Bluesky conversation with the person it names, and your message reaches them alone; a handle Bluesky does not know starts nothing — `docs/goal/ui/conversations.md` § Where logic lives
   - `tests/e2e-unified/tests/test_bridged_dm_legs.py::test_a_bluesky_room_started_from_the_picker_by_handle_reaches_the_resolved_peer_alone`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_atproto_settings_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_depth_selector_renders_at_off` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_off_linked_cardless_ladder` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_linked_panel_reuses_the_shared_bridge_surface` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_hosted_ladder_through_the_card` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_atproto_backfill_count.py::test_history_opt_in_names_the_public_post_count` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_mint_reveal_and_revoke_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_kill_switch_is_non_destructive` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_authorize_and_revoke_external_posting` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_reauthorizing_needs_no_revoke_first` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_lapse_reads_as_reauthorize_here` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_delete_presence_ceremony` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_retire_identity_opt_in_rides_the_delete_ceremony` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_consent_ceremony_end_to_end` | web (linux): error, linux (linux): passed, tui (linux): passed |
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
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_hosted_enable_docker.py::test_atproto_hosted_enable_boots_docker_bridge` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_sni_router_routes_pds_to_the_atproto_bridge` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/platform/docker/test_atproto_pds_sni_router.py::test_createsession_rate_limit_carries_real_client_ip` | nest (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_bluesky_notifications_bridged.py::test_a_bridged_bluesky_notification_reaches_the_unified_list` | web (linux): passed, linux (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_bluesky_feed_ingest.py::test_a_followed_bluesky_post_arrives_in_the_feed_and_a_reply_threads_under_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_atproto_history_backfill.py::test_choosing_history_publishes_existing_public_posts` | tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_deleting_a_post_removes_it_from_the_hosted_account` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_a_gated_or_sold_post_never_reaches_the_hosted_account` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/test_atproto_firehose_post.py::test_public_post_reaches_the_firehose` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_starting_to_host_unlinks_the_linked_account_and_leaves_it_untouched` | nest (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_atproto_did_method.py::test_either_identity_choice_mints_the_same_handle` | tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_a_stepped_down_identity_shows_marked_deactivated` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_depth_selector_renders_at_off` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 25 | nest | `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_hosting_again_after_a_retirement_mints_a_fresh_identity` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/test_atproto_hosted_account.py::test_the_nest_announces_itself_to_the_relay_once_the_identity_resolves` | nest (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_typed_code_connects_the_app_and_revoke_ends_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_revoke_bites_at_the_very_next_write_and_each_session_is_ceilinged` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_an_app_password_is_shown_again_on_another_device` | tui (linux): passed |
| 29 | nest | `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_an_app_signed_in_as_you_cannot_manage_app_passwords_or_delete_the_account` | nest (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_a_permission_set_resolves_through_the_bridge_onto_the_card_and_into_the_token` | web (linux): passed, linux (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_connected_apps.py::test_a_connected_apps_row_names_the_permission_set_it_came_from` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/test_atproto_pds_consent.py::test_an_unresolvable_permission_set_is_refused_with_the_bridge_present` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_an_approved_app_keeps_the_permissions_its_card_showed_after_the_set_changes` | nest (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_an_unsigned_posting_permission_shows_as_a_mismatch` | tui (linux): passed |
| 35 | nest | `tests/e2e-unified/tests/test_atproto_pds_app_limits.py::test_posts_published_through_another_app_stay_up_after_posting_is_revoked` | nest (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_atproto_custody_alarm.py::test_a_seizure_past_its_window_says_so_and_offers_no_button` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 37 | nest | `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_on_automatic_every_post_is_published_to_the_linked_account` | nest (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_on_manual_only_posts_tagged_for_it_are_published` | nest (linux): passed |
| 39 | nest | `tests/e2e-unified/tests/test_bluesky_linked_account.py::test_deleting_a_cross_posted_post_removes_its_copy` | nest (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_bluesky_feed_reference.py::test_a_quote_of_a_bluesky_post_is_published_on_bluesky_carrying_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 41 | app | `tests/e2e-unified/tests/test_bluesky_feed_interactions.py::test_a_bluesky_like_or_repost_and_its_undo_reach_bluesky` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_bluesky_feed_reference.py::test_replying_or_quoting_with_no_bluesky_account_linked_is_refused_with_the_remedy` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 43 | app | `tests/e2e-unified/tests/test_bluesky_feed_ingest.py::test_posts_from_a_subscribed_bluesky_custom_feed_arrive_in_the_feed` | tui (linux): passed |
| 44 | nest | `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_bluesky_post_lands_in_the_feed_once_across_polls_and_linked_accounts` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_bluesky_repost_arrives_as_the_original_post_under_its_author` | nest (linux): passed |
| 46 | nest | `tests/e2e-unified/tests/test_bluesky_feed_ingest_nest.py::test_a_self_labelled_bluesky_post_arrives_carrying_its_label_as_a_content_warning` | nest (linux): passed |
| 47 | app | `tests/e2e-unified/tests/test_atproto_settings.py::test_an_app_password_carries_dm_access_only_when_the_toggle_was_on` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 48 | nest | `tests/e2e-unified/tests/test_atproto_pds_proxy.py::test_atproto_app_password_reaches_dms_only_with_dm_access` | nest (linux): passed |
| 49 | app | `tests/e2e-unified/tests/test_bluesky_thread_view.py::test_a_bluesky_posts_thread_shows_the_conversation_with_the_opened_post_marked` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 49 | app | `tests/e2e-unified/tests/test_bluesky_thread_view.py::test_a_thread_whose_post_is_gone_at_its_origin_says_so` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 50 | app | `tests/e2e-unified/tests/test_bridged_dm_legs.py::test_a_bluesky_dm_arrives_as_a_bluesky_room_and_the_reply_reaches_that_peer_alone` | web (linux): passed, tui (linux): passed |
| 51 | app | `tests/e2e-unified/tests/test_bridged_dm_legs.py::test_a_bluesky_room_started_from_the_picker_by_handle_reaches_the_resolved_peer_alone` | web (linux): passed, tui (linux): passed |
<!-- features-render:end -->
