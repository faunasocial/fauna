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

Stamped 2026-09-24 at 89bb22083d.

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
9. [app] Push you turn off in the app stays off, and push follows whoever is signed in — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/test_push_settings.py::test_push_turned_off_stays_off_across_a_relaunch`
   - `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting`
   - `tests/e2e-unified/tests/test_push_settings.py::test_sign_out_drops_the_row_and_the_next_sign_in_rearms`
10. [nest] Your nest only sends a push to a real push service on the public internet, never to an address inside its own network — `docs/goal/architecture/apps/common.md` § Push Notifications
   - `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_refuses_an_endpoint_the_nest_must_never_dial`
11. [app] Each notification reads in your app's language, and one your app is too old to know still reads as a whole sentence, never a code — `docs/goal/behavior/notifications.md` § Localized body (ratified 2026-09-20)
   - `tests/e2e-unified/tests/test_notifications_localized_body.py::test_a_row_paints_its_localized_body_and_an_unknown_key_paints_the_summary`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_notifications.py::test_navigate_to_notifications` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_notifications.py::test_mark_notifications_read` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_notifications_type_icon.py::test_notification_row_shows_type_icon` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_push_live_refresh.py::test_push_live_refreshes_mounted_notifications_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_sp_linux_ws_rpc_push.py::test_linux_ws_rpc_push_pump` | — |
| 4 | app | `tests/e2e-unified/tests/test_notification_tap_through.py::test_tapping_a_like_notification_opens_the_liked_post` | linux (linux): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_vapid_key_self_generated_by_default` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_unsubscribe` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_apns` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_notifications_security_notice.py::test_security_notice_shows_in_the_list_with_its_full_detail` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_notifications_security_notice.py::test_pending_action_notice_shows_in_the_list_with_its_full_detail` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_flip_resilience.py::test_nest_flip_resilience` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_reaches_an_offline_device_for_a_knock_a_message_and_an_invite` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_push_dispatch.py::test_push_is_decided_per_device` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_push_turned_off_stays_off_across_a_relaunch` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_push_follows_whoever_is_signed_in_and_switching_keeps_the_setting` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_push_settings.py::test_sign_out_drops_the_row_and_the_next_sign_in_rearms` | tui (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_push_api.py::test_push_subscribe_refuses_an_endpoint_the_nest_must_never_dial` | nest (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_notifications_localized_body.py::test_a_row_paints_its_localized_body_and_an_unknown_key_paints_the_summary` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
<!-- features-render:end -->
