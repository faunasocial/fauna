---
slug: calendar-in-standard-apps
title: Your calendar in Apple Calendar, Thunderbird and friends
section: mail, calendar and contacts
goal: docs/goal/behavior/caldav-server.md § Goal
guide: docs/guides/calendar-and-contacts.md § Connecting Apple Calendar, Thunderbird, DAVx⁵…
---

## What a user gets

Any CalDAV calendar app connects with your address and app password, finds
your calendars by itself, adds new ones, and sees an event the moment you make it in
the Fauna app. Invitations you send from any of them reach the attendees, including
people on other nests and people with no mailbox, and their replies come back.
Events rest sealed on your nest.

## Coverage contract

Stamped 2026-09-23 at 6605debcb9.

1. [app] A calendar app finds your calendars from the address alone and can add a new calendar — `docs/goal/behavior/caldav-server.md` § Calendar collection model
   - `tests/e2e-unified/tests/test_caldav_discovery_sequence.py::test_apple_style_caldav_discovery_walk`
   - `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip`
2. [app] An event made in the Fauna app shows in the calendar app, and three clients see one another's changes — `docs/goal/behavior/caldav-server.md` § Sync model (RFC 6578)
   - `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_client_sealed_event_visible_to_mua`
   - `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_guard_real_domain_mail_caldav`
   - `tests/e2e-unified/tests/test_caldav_live_nest.py::test_three_client_caldav_roundtrip`
3. [app] Calendars work from a fresh nest whatever kind of handle you chose — `docs/goal/behavior/caldav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/test_caldav_onboarding_derived_enablement.py::test_caldav_derived_enablement_by_address_type`
   - `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant`
4. [app] An invitation reaches an attendee with no mailbox, on your nest or another, sealed — `docs/goal/behavior/caldav-server.md` § Server-side auto-schedule
   - `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda`
   - `tests/e2e-unified/tests/test_caldav_autoschedule_cross_nest.py::test_cross_nest_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda`
   - `tests/e2e-unified/tests/test_caldav_autoschedule_live_nest.py::test_live_caldav_autoschedule_fans_imip`
5. [nest] The server stores and serves events, sends invitations and cancellations, and delivers in-domain ones straight to the inbox — `docs/goal/behavior/caldav-server.md` § Scheduling & invitations
   - `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_caldav_put_report_roundtrip`
   - `tests/e2e-unified/tests/platform/docker/test_caldav_autoschedule_imip.py::test_caldav_organizer_autoschedule_fans_imip_request_and_cancel`
   - `tests/e2e-unified/tests/platform/docker/test_caldav_autoschedule_imip.py::test_caldav_organizer_autoschedule_in_domain_attendee_delivered_to_inbox`
   - `tests/e2e-unified/tests/platform/windows/test_caldav_imap_serving.py::test_windows_caldav_imap_round_trip`
6. [nest] Calendars serve on the standard port beside everything else, and on a bare address for a home nest — `docs/goal/behavior/caldav-server.md` § Network exposure
   - `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_sni_router_splits_nest_and_mda_caldav`
   - `tests/e2e-unified/tests/platform/docker/test_caldav_bare_ip_serving.py::test_bare_ip_caldav_listener_serves_on_published_port`
   - `tests/e2e-unified/tests/platform/docker/test_domained_claim_acme_serves_caldav.py::test_domained_claim_sets_primary_dns_acme_and_caldav`
7. [nest] Events rest sealed on your nest and serve back byte for byte, across a snapshot and restore — `docs/goal/architecture/message-segment-store.md` § Invariants
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_calendar_body_rests_sealed_in_its_segment_and_serves_byte_identically`
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_every_dav_record_at_rest_is_a_sealed_envelope_across_many_records`
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_snapshot_create_then_restore_round_trips_a_dav_body`
8. [app] A calendar app you connect for the first time already finds a calendar called "Personal", even if you have never opened the Fauna app — `docs/goal/behavior/caldav-server.md` § Lazy "Personal" calendar
   - `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip`
   - `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant`
9. [app] A name, colour or description you give a calendar in your calendar app sticks, even when you set it the moment you create it — `docs/goal/behavior/caldav-server.md` § Create-then-rename race (collection PROPPATCH visibility retry)
   - `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip`
10. [app] Your calendar app is told the server sends the invitations, so each person you invite gets one, not two — `docs/goal/behavior/caldav-server.md` § Server-side auto-schedule (the MDA advertises `calendar-auto-schedule`)
   - `tests/e2e-unified/tests/test_caldav_discovery_sequence.py::test_apple_style_caldav_discovery_walk`
11. [app] Your calendar works in calendar apps with email turned off — `docs/goal/behavior/caldav-server.md` § Independent enablement
   - `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant`
12. [app] When a reply to your invitation arrives, it shows on your event in your calendar app once your Fauna app has synced — `docs/goal/behavior/caldav-server.md` § The one operation with a cost: applying a `REPLY` to the organizer's stored event
   - `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_a_reply_by_mail_shows_on_the_organizers_event_in_the_calendar_app`
13. [app] Marking yourself Interested in Fauna shows as Tentative in your calendar app, and a Tentative picked there stays Tentative in Fauna — `docs/goal/behavior/caldav-server.md` § RSVP semantics: `interested` projects to `TENTATIVE` (asymmetric, by necessity)
   - `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_interested_shows_as_tentative_and_a_calendar_apps_tentative_stays_tentative`
14. [nest] Nobody can reach your calendar, or learn whether your account exists, without signing in — `docs/goal/behavior/caldav-server.md` § Authentication
   - `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_sni_router_splits_nest_and_mda_caldav`
15. [nest] After too many wrong passwords in a short time, your calendar app is locked out for a while — `docs/goal/behavior/caldav-server.md` § Authentication
   - `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_caldav_auth_event_carries_real_client_ip`
16. [nest] Your calendar app always connects encrypted, and a password is never accepted over an unencrypted connection — `docs/goal/behavior/caldav-server.md` § Don't do these
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_password_is_never_accepted_over_an_unencrypted_connection`
17. [nest] If you change the same event in two calendar apps at once, the later save is told the event changed instead of silently overwriting it — `docs/goal/behavior/caldav-server.md` § Write surface
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_the_later_of_two_concurrent_edits_is_told_the_event_changed`
18. [nest] A broken or incomplete event sent from a calendar app is refused, and one that is too large is refused as too large — `docs/goal/behavior/caldav-server.md` § iCalendar parsing rules
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_broken_event_is_refused_and_an_oversized_one_is_refused_as_too_large`
19. [nest] An invitation someone sends you by email shows up in your calendar app — `docs/goal/behavior/caldav-server.md` § Server-side auto-schedule (the MDA advertises `calendar-auto-schedule`)
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_an_invitation_mailed_from_another_server_lands_on_the_calendar`
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_an_invitation_from_a_colleague_on_the_same_nest_lands_on_the_calendar`
20. [nest] Accepting or declining an invitation in your calendar app sends your reply to the organizer — `docs/goal/behavior/caldav-server.md` § Server-side auto-schedule (the MDA advertises `calendar-auto-schedule`)
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_answering_an_invitation_in_a_calendar_app_replies_to_the_organizer`
21. [nest] Editing an event in a calendar app keeps what only Fauna knows about it, such as your Interested reply — `docs/goal/behavior/caldav-server.md` § Event resources
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_editing_in_a_calendar_app_keeps_what_only_fauna_knows`
22. [nest] A calendar app that has been away too long downloads your whole calendar again instead of showing a partial one — `docs/goal/behavior/caldav-server.md` § Stale sync-token handling
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_calendar_app_away_too_long_is_sent_to_a_full_resync`
23. [nest] After your nest is restored from a backup, a calendar app holding newer state resyncs to what the nest holds, and its edits keep working — `docs/goal/behavior/caldav-server.md` § Stale sync-token handling
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_after_a_restore_a_calendar_app_resyncs_and_its_edits_keep_working`
24. [nest] Your calendar app can ask for a date range and gets every event in it, including each occurrence of a repeating one — `docs/goal/behavior/caldav-server.md` § Read surface (brief)
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_date_range_holds_every_occurrence_of_a_repeating_event`
25. [nest] Moving an event to another calendar in your calendar app moves it, and a failed move leaves the original where it was — `docs/goal/behavior/caldav-server.md` § Write surface
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_moving_an_event_moves_it_and_a_failed_move_leaves_it`
26. [nest] Calendar apps accept events set many years ahead — `docs/goal/behavior/caldav-server.md` § Don't do these
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_events_many_years_ahead_are_accepted`
27. [nest] No one else on your nest, not even the admin, can read or change your calendars — `docs/goal/behavior/caldav-server.md` § Architectural rules
   - `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_no_one_else_on_the_nest_can_read_or_change_your_calendar`
28. [app] On a nest with no domain, your mail settings show the address and port to enter in a calendar app — `docs/goal/behavior/caldav-server.md` § Network exposure
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
| android |  no run recorded | |
| tui | ⚠ partial | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_caldav_discovery_sequence.py::test_apple_style_caldav_discovery_walk` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_client_sealed_event_visible_to_mua` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_guard_real_domain_mail_caldav` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_caldav_live_nest.py::test_three_client_caldav_roundtrip` | — |
| 3 | app | `tests/e2e-unified/tests/test_caldav_onboarding_derived_enablement.py::test_caldav_derived_enablement_by_address_type` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py::test_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda` | linux (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_cross_nest.py::test_cross_nest_mailbox_less_attendee_gets_sealed_scheduling_through_real_mda` | linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_caldav_autoschedule_live_nest.py::test_live_caldav_autoschedule_fans_imip` | — |
| 5 | nest | `tests/e2e-unified/tests/test_mail_bridge_mda.py::test_mda_caldav_put_report_roundtrip` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_autoschedule_imip.py::test_caldav_organizer_autoschedule_fans_imip_request_and_cancel` | — |
| 5 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_autoschedule_imip.py::test_caldav_organizer_autoschedule_in_domain_attendee_delivered_to_inbox` | — |
| 5 | nest | `tests/e2e-unified/tests/platform/windows/test_caldav_imap_serving.py::test_windows_caldav_imap_round_trip` | — |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_sni_router_splits_nest_and_mda_caldav` | — |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_bare_ip_serving.py::test_bare_ip_caldav_listener_serves_on_published_port` | — |
| 6 | nest | `tests/e2e-unified/tests/platform/docker/test_domained_claim_acme_serves_caldav.py::test_domained_claim_sets_primary_dns_acme_and_caldav` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_calendar_body_rests_sealed_in_its_segment_and_serves_byte_identically` | nest (linux): passed, nest (windows): passed |
| 7 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_every_dav_record_at_rest_is_a_sealed_envelope_across_many_records` | nest (linux): passed, nest (windows): passed |
| 7 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_snapshot_create_then_restore_round_trips_a_dav_body` | nest (linux): passed, nest (windows): passed |
| 8 | app | `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_caldav_mkcalendar_create.py::test_macos_style_mkcalendar_create_then_event_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_caldav_discovery_sequence.py::test_apple_style_caldav_discovery_walk` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_caldav_onboarding_variants.py::test_caldav_onboarding_variant` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_a_reply_by_mail_shows_on_the_organizers_event_in_the_calendar_app` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_caldav_client_seal_to_mua.py::test_interested_shows_as_tentative_and_a_calendar_apps_tentative_stays_tentative` | linux (linux): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_sni_router_splits_nest_and_mda_caldav` | — |
| 15 | nest | `tests/e2e-unified/tests/platform/docker/test_caldav_sni_router.py::test_caldav_auth_event_carries_real_client_ip` | — |
| 16 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_password_is_never_accepted_over_an_unencrypted_connection` | nest (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_the_later_of_two_concurrent_edits_is_told_the_event_changed` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_broken_event_is_refused_and_an_oversized_one_is_refused_as_too_large` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_an_invitation_mailed_from_another_server_lands_on_the_calendar` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_an_invitation_from_a_colleague_on_the_same_nest_lands_on_the_calendar` | nest (linux): passed |
| 20 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_answering_an_invitation_in_a_calendar_app_replies_to_the_organizer` | nest (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_editing_in_a_calendar_app_keeps_what_only_fauna_knows` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_calendar_app_away_too_long_is_sent_to_a_full_resync` | nest (linux): passed |
| 23 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_after_a_restore_a_calendar_app_resyncs_and_its_edits_keep_working` | nest (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_a_date_range_holds_every_occurrence_of_a_repeating_event` | nest (linux): passed |
| 25 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_moving_an_event_moves_it_and_a_failed_move_leaves_it` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_events_many_years_ahead_are_accepted` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/test_caldav_nest_outcomes.py::test_no_one_else_on_the_nest_can_read_or_change_your_calendar` | nest (linux): passed |
| 28 | app | (none) | — |
<!-- features-render:end -->
