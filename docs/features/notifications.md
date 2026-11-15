---
slug: notifications
title: Notifications
section: everyday
goal: docs/goal/behavior/notifications.md § Goal
guide: docs/guides/app-tour.md § Notifications
---

## What a user gets

Notifications list what happened, each with an icon for its kind and an unread
badge in the navigation. New ones arrive while you watch, and one tap marks them all
read.

## Coverage contract

Stamped 2026-11-07 at b844c771f6.

1. [app] The list loads and mark-all-read clears the unread count — `docs/goal/behavior/notifications.md` § User actions
   - `tests/e2e-unified/tests/test_notifications.py::test_navigate_to_notifications`
   - `tests/e2e-unified/tests/test_notifications.py::test_mark_notifications_read`
2. [app] Each row carries an icon for its kind — `docs/goal/behavior/notifications.md` § Layout & flow
   - `tests/e2e-unified/tests/test_notifications_type_icon.py::test_notification_row_shows_type_icon`
3. [app] A new notification appears on the open page without a reload — `docs/goal/behavior/notifications.md` § Where logic lives
   - `tests/e2e-unified/tests/test_push_live_refresh.py::test_push_live_refreshes_mounted_notifications_page`
   - `tests/e2e-unified/tests/test_sp_linux_ws_rpc_push.py::test_linux_ws_rpc_push_pump`
4. [app] Tapping a notification takes you to what it is about — `docs/goal/behavior/notifications.md` § User actions
   - `tests/e2e-unified/tests/test_notification_tap_through.py::test_tapping_a_like_notification_opens_the_liked_post`
5. [nest] Your nest registers the device for push and generates its own push keys, no set-up needed — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/api/test_push_api.py::test_push_vapid_key_self_generated_by_default`
   - `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_unsubscribe`
   - `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_apns`
6. [app] A security notice from your nest — a new sign-in, a pending account action, a replaced recovery key — shows in the list with its full detail — `docs/goal/behavior/notifications.md` § Security notices
   - `tests/e2e-unified/tests/test_notifications_security_notice.py::test_security_notice_shows_in_the_list_with_its_full_detail`
   - `tests/e2e-unified/tests/test_notifications_security_notice.py::test_pending_action_notice_shows_in_the_list_with_its_full_detail`
7. [app] The unread count moves while you are anywhere in the app, not only on the Notifications page — `docs/goal/behavior/notifications.md` § Architectural rules
   - `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_resilience`
8. [nest] With the app closed, a new message, knock or invite reaches your device as a push — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_reaches_an_offline_device_for_a_knock_a_message_and_an_invite`
   - `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_is_decided_per_device`
   - `tests/e2e-unified/tests/api/test_push_dispatch.py::test_a_stand_in_announce_delivers_and_never_suppresses`
   - `tests/e2e-unified/tests/platform/windows/test_agent_push_arm_end_to_end.py::test_a_frame_is_withheld_while_the_app_is_open_and_toasted_once_it_closes`
   - `tests/e2e-unified/tests/platform/linux/test_agent_push_arm_end_to_end.py::test_a_frame_is_withheld_while_the_app_is_open_and_posted_once_it_closes`
9. [app] Push you turn off in the app stays off, and push follows whoever is signed in — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/test_push_settings.py::test_push_turned_off_stays_off_across_a_relaunch`
   - `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_sign_out_drops_the_row_and_the_next_sign_in_rearms`
   - `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_turned_off_stays_off_across_a_relaunch`
   - `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_windows_sign_out_drops_the_row_and_the_next_sign_in_rearms`
   - `tests/e2e-unified/tests/test_push_settings.py::test_linux_push_turned_off_stays_off_across_a_relaunch`
   - `tests/e2e-unified/tests/test_push_settings.py::test_linux_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_linux_sign_out_drops_the_row_and_the_next_sign_in_rearms`
   - `tests/e2e-unified/tests/test_push_settings.py::test_web_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_android_push_turned_off_stays_off_across_a_relaunch`
   - `tests/e2e-unified/tests/test_push_settings.py::test_android_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_apple_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_android_sign_out_drops_the_row_and_the_next_sign_in_rearms`
10. [nest] Your nest only sends a push to a real push service on the public internet, never to an address inside its own network — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_refuses_an_endpoint_the_nest_must_never_dial`
11. [app] Each notification reads in your app's language, and one your app is too old to know still reads as a whole sentence, never a code — `docs/goal/behavior/notifications.md` § Localized body (ratified 2026-09-20)
   - `tests/e2e-unified/tests/test_notifications_localized_body.py::test_a_row_paints_its_localized_body_and_an_unknown_key_paints_the_summary`
12. [app] You can dismiss one notification or clear the whole list, a security notice staying for its first fourteen days; a contact request you turn away takes its notification with it — `docs/goal/behavior/notifications.md` § Retention (ratified 2026-09-24)
   - `tests/e2e-unified/tests/test_notifications_dismiss_and_clear.py::test_dismiss_deletes_one_row_and_clear_all_deletes_the_rest`
   - `tests/e2e-unified/tests/test_notifications_dismiss_and_clear.py::test_dismissing_a_knock_takes_its_doorbell_row_with_it`
13. [app] A notification about someone in your contacts names them by their name or handle, never by an identifier — `docs/goal/behavior/value-formatting.md` § Implementation status today
   - `tests/e2e-unified/tests/test_contacts_accept_opens_conversation.py::test_accepting_a_contact_request_lets_you_start_the_conversation`
14. [app] A security notice is never hidden by a muted word — `docs/goal/behavior/notifications.md` § Muted words (ratified 2026-10-09)
   - `tests/e2e-unified/tests/test_notifications_muted.py::test_a_muted_word_collapses_the_notification_rows_that_carry_it`
15. [app] A knock from a stranger shows one notice in your language, and a flood of them replaces that notice rather than piling up — `docs/goal/behavior/notifications.md` § Localized body (ratified 2026-09-20)
   - `tests/e2e-unified/tests/test_notifications_knock_toast.py::test_a_flood_of_knocks_raises_one_localized_notice`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+88622a93 standalone |
| linux | ⚠ partial | 0.1.5-dev+5a9a5fa8 standalone |
| windows | ✅ full | 0.1.5-dev+acde58f9 standalone |
| macos | ⚠ partial | 0.1.5-dev+3f9f2b95 standalone |
| ios | ⚠ partial | 0.1.5-dev+c9e7fe5d standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+acde58f9 standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_notifications.py::test_navigate_to_notifications` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_notifications.py::test_mark_notifications_read` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_notifications_type_icon.py::test_notification_row_shows_type_icon` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_push_live_refresh.py::test_push_live_refreshes_mounted_notifications_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sp_linux_ws_rpc_push.py::test_linux_ws_rpc_push_pump` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_notification_tap_through.py::test_tapping_a_like_notification_opens_the_liked_post` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_vapid_key_self_generated_by_default` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_unsubscribe` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_apns` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 6 | app | `tests/e2e-unified/tests/test_notifications_security_notice.py::test_security_notice_shows_in_the_list_with_its_full_detail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_notifications_security_notice.py::test_pending_action_notice_shows_in_the_list_with_its_full_detail` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_resilience` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_reaches_an_offline_device_for_a_knock_a_message_and_an_invite` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_is_decided_per_device` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_push_dispatch.py::test_a_stand_in_announce_delivers_and_never_suppresses` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/platform/windows/test_agent_push_arm_end_to_end.py::test_a_frame_is_withheld_while_the_app_is_open_and_toasted_once_it_closes` | nest (windows): passed |
| 8 | nest | `tests/e2e-unified/tests/platform/linux/test_agent_push_arm_end_to_end.py::test_a_frame_is_withheld_while_the_app_is_open_and_posted_once_it_closes` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_push_turned_off_stays_off_across_a_relaunch` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_sign_out_drops_the_row_and_the_next_sign_in_rearms` | tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_turned_off_stays_off_across_a_relaunch` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_windows_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_windows_sign_out_drops_the_row_and_the_next_sign_in_rearms` | windows (windows): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_linux_push_turned_off_stays_off_across_a_relaunch` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_linux_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_linux_sign_out_drops_the_row_and_the_next_sign_in_rearms` | linux (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_web_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | web (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_android_push_turned_off_stays_off_across_a_relaunch` | — |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_android_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | — |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_apple_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | macos (macos): passed, ios (macos): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_android_sign_out_drops_the_row_and_the_next_sign_in_rearms` | — |
| 10 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_refuses_an_endpoint_the_nest_must_never_dial` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_notifications_localized_body.py::test_a_row_paints_its_localized_body_and_an_unknown_key_paints_the_summary` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_notifications_dismiss_and_clear.py::test_dismiss_deletes_one_row_and_clear_all_deletes_the_rest` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_notifications_dismiss_and_clear.py::test_dismissing_a_knock_takes_its_doorbell_row_with_it` | macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_contacts_accept_opens_conversation.py::test_accepting_a_contact_request_lets_you_start_the_conversation` | windows (windows): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 14 | app | `tests/e2e-unified/tests/test_notifications_muted.py::test_a_muted_word_collapses_the_notification_rows_that_carry_it` | web (linux): passed, windows (windows): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_notifications_knock_toast.py::test_a_flood_of_knocks_raises_one_localized_notice` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
<!-- features-render:end -->
