---
slug: admin-nest
title: Nest: network mode, pairing, ports, updates, region, identity, sign-in keys and takedowns
section: admin area
goal: docs/goal/behavior/admin.md § N. Nest
guide: docs/guides/admin-tour.md § Nest
---

## What a user gets

The Nest page holds what applies to the whole nest: whether it is public or
behind a home router, whether other nests may link to it, the port it serves on,
pending operating-system updates with a restart button, the region it declares,
a deliberate rotation of the nest's own identity that every app follows silently,
the keys outside apps' sign-ins are signed with — replaced as a precaution or at once
after a leak, each control saying what it costs first — and a legal takedown of a
post that you can overturn.

## Coverage contract

Stamped 2026-10-01 at e129e78aa0.

1. [app] The network mode flips from the page and is read back from the nest — `docs/goal/behavior/admin.md` § N. Nest
   - `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_control_renders`
   - `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_flip_round_trips`
2. [app] Whether other nests may link is a switch — `docs/goal/behavior/admin.md` § N. Nest
   - `tests/e2e-unified/tests/test_admin_nest.py::test_nest_page_renders`
   - `tests/e2e-unified/tests/test_admin_nest.py::test_nest_pairing_toggle_flips`
3. [app] The serving port is set from the page and a bad port is refused before it is sent — `docs/goal/architecture/nest/common.md` § Serving ports
   - `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_field_renders`
   - `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_round_trips`
   - `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_invalid_surfaces_error`
4. [app] The Nest page says whether the server's operating system is up to date, how many security updates are waiting and that a restart is pending, and its restart-now button, offered only then, asks the server to restart — `docs/goal/architecture/installers/vps.md` § 4 — Admin visibility
   - `tests/e2e-unified/tests/test_host_maintenance.py::test_os_maintenance_status_not_reported_without_host_data`
   - `tests/e2e-unified/tests/test_host_maintenance.py::test_os_maintenance_status_up_to_date`
   - `tests/e2e-unified/tests/test_host_maintenance.py::test_os_restart_pending_shows_button_and_triggers`
5. [app] The declared region round-trips and a malformed code is refused on the page — `docs/goal/behavior/region-blocking.md` § Region determination
   - `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips`
   - `tests/e2e-unified/tests/test_admin_nest.py::test_malformed_region_is_refused_client_side`
6. [app] The admin rotates the nest's identity from the app, and every app re-pins silently — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
   - `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app`
   - `tests/e2e-unified/tests/test_nest_rotation_repin.py::test_a_committed_rotation_repins_silently_through_the_chain`
7. [app] A legal takedown removes a post from serving only once the admin has given the legal reference and confirmed a summary naming the post and that reference, and it can be overturned — `docs/goal/behavior/moderation.md` § Legal takedown (the legal-compulsion carve-out)
   - `tests/e2e-unified/tests/test_admin_legal_takedown.py::test_the_admin_takes_down_and_restores_a_post_through_the_app`
8. [nest] Your nest serves the record of its identity rotations to anyone who asks, and a rotation ends every sign-in made before it — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
   - `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_the_chain_carries_the_hop_a_real_rotation_wrote`
   - `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_a_rotation_evicts_every_bearer_minted_before_it`
9. [app] The admin reads the nest's outside-app sign-in keys and replaces them as a precaution, replaces them at once after a leak, or ends every saved sign-in — the two that end sign-ins stating what they cost and waiting for a confirm before they act — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
   - `tests/e2e-unified/tests/test_admin_oauth_issuer_keys.py::test_the_admin_walks_the_three_sign_in_key_controls_through_the_app`
10. [app] Reports people file reach a queue on the Nest page, and opening one fills in the takedown form, which still demands its legal citation — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
11. [app] Each report in the queue shows what was reported, the reason, the reporter's note and excerpt, where it came from and how old it is — `docs/goal/behavior/moderation.md` § App surface
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked`
12. [app] Marking a report acted on or dismissed records the decision and takes it off the queue — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked`
13. [app] A new report notifies each admin and points them at the queue — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
14. [nest] Many reports of the same thing in one day ring each admin once — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_many_reports_of_one_thing_in_a_day_ring_the_admin_once`
15. [nest] Resolving a report changes nothing about the reported content or its author; only the takedown console and a suspension do — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_resolving_a_report_changes_nothing_about_the_content_or_its_author`
16. [app] On a deployment whose port is fixed, the serving port shows read-only and says the deployment serves on 443 — `docs/goal/architecture/nest/common.md` § Serving ports
    - `tests/e2e-unified/tests/test_serving_port.py::test_a_router_fronted_nest_shows_the_serving_port_read_only`
17. [nest] A serving port the nest cannot use never takes it down: it stays reachable on the port it had, so the admin can pick another — `docs/goal/architecture/nest/common.md` § Serving ports
    - `tests/e2e-unified/tests/api/test_serving_port_bind_fallback.py::test_an_unbindable_serving_port_leaves_the_nest_reachable_on_the_port_it_had`
18. [nest] Only an admin learns whether the server has updates or a restart pending; anyone else is told nothing is pending — `docs/goal/architecture/installers/vps.md` § 4 — Admin visibility
    - `tests/e2e-unified/tests/api/test_host_maintenance.py::test_os_fields_admin_gated_off_anonymous`
19. [nest] The server installs security updates by itself and restarts itself when nobody is connected, or within a day at the latest — `docs/goal/architecture/installers/vps.md` § 2 — Reboot
    - `tests/e2e-unified/tests/platform/docker/test_host_reboot_coordinator.py::test_unattended_upgrades_on_with_its_own_reboot_off`
    - `tests/e2e-unified/tests/platform/docker/test_host_reboot_coordinator.py::test_coordinator_reboots_idle_or_at_the_ceiling`
20. [app] When the nest has stopped hearing from its region's authority, the region section warns that the last rules received still apply — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_nest.py::test_region_section_warns_when_the_authority_has_gone_quiet`
21. [app] The admin chooses whether the nest's own address opens the app the nest ships or hands visitors to the central app, and sees the address they will be sent to — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_web_app_origin.py::test_admin_flips_app_to_the_central_origin_and_back`
22. [nest] When the admin chooses the central app, the nest's own app address sends visitors there with the nest filled in, choosing the bundled app stops that at once, a nest with no domain keeps serving its own app, and only an admin can change the choice — `docs/goal/behavior/web-content-hosting.md` § Same-origin security model
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back`
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_domainless_box_serves_bundled_under_central`
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_non_admin_cannot_set_the_choice`
23. [app] The admin takes down a single conversation message under a legal order from the same console, and it stops being delivered from then on — `docs/goal/behavior/moderation.md` § Legal takedown
    - `tests/e2e-unified/tests/test_legal_takedown_notice.py::test_a_message_taken_down_from_a_report_shows_the_notice_to_a_member_who_reads_it_later`
24. [nest] The identity rotation is refused while the removal of an admin is still pending — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
    - `tests/e2e-unified/tests/api/test_admin_roster.py::test_the_identity_rotation_is_refused_while_an_admin_removal_is_pending`
25. [app] While the app cannot yet tell which admins will inherit the new identity, the rotation's confirm stays visible but disabled, says why, and lists nobody — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_nest.py::test_the_rotation_confirm_stays_disabled_while_the_roster_is_unresolved`
26. [nest] After the admin replaces the sign-in key as a precaution, the old key stays published for a set time beside the new one, so sign-ins it signed keep verifying — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_rotation_adds_a_key_and_keeps_serving_the_old_one`
27. [nest] After the admin replaces the sign-in key because of a leak, the very next look at the nest's published keys shows only the new one — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_a_forced_rotation_drops_every_other_key_from_the_jwks_at_once`
28. [nest] After the admin ends every saved sign-in, each connected outside app is refused the next time it refreshes and leaves the member's list of connected apps until it is approved again — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - `tests/e2e-unified/tests/api/test_oauth_session_secret_rotation.py::test_ending_every_saved_sign_in_refuses_each_outside_apps_next_refresh`
29. [app] A declared region always shows whose rules apply to the nest, and withdrawing the declaration clears it — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips`
30. [app] Before the identity rotation is sent, its confirm lists the admins who will inherit the new identity — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
    - `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app`
31. [nest] The nest reports the server's pending updates and pending restart, passes a restart request on to the server, and refuses the request when it has no server to ask — `docs/goal/architecture/installers/vps.md` § 4 — Admin visibility
    - `tests/e2e-unified/tests/api/test_host_maintenance.py::test_host_maintenance_status_and_restart`
32. [nest] The nest follows the update channel it was installed from until an admin chooses one, reports the channel in force where its updater can read it, keeps the admin's choice across a restart, follows none when it was installed from a single build, and lets only an admin read or change it — `docs/goal/architecture/nest/update-channel.md` § The channel as nest state
    - `tests/e2e-unified/tests/api/test_update_channel.py::test_the_seed_is_followed_until_the_admin_chooses_and_the_choice_survives_a_restart`
    - `tests/e2e-unified/tests/api/test_update_channel.py::test_a_non_channel_tag_leaves_the_box_pinned`
    - `tests/e2e-unified/tests/api/test_update_channel.py::test_an_unknown_kind_is_the_predates_the_choice_answer`
    - `tests/e2e-unified/tests/api/test_update_channel.py::test_a_non_admin_cannot_read_or_set_it_and_an_unknown_channel_is_refused`
33. [nest] The nest tells the admin the version its channel has reached, read from the registry, and answers without it when the registry has no build on that channel or cannot be reached — `docs/goal/architecture/nest/update-channel.md` § The channel as nest state
    - `tests/e2e-unified/tests/api/test_update_channel.py::test_the_channel_head_version_is_read_from_the_registry_and_absent_without_it`
34. [nest] The server's updater keeps the nest on its channel and never on an older build: it leaves a current server alone, holds while the channel is behind, moves the nest onto a newer build with its data and settings and a clean disconnect for its apps, follows a channel the admin changes from the app, and finishes a move a crash cut short — `docs/goal/architecture/nest/update-channel.md` § The updater
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_the_updater_hands_itself_off_to_the_build_it_moved_the_nest_to`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_hand_off_killed_between_create_and_start_settles_to_one_updater`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_current_when_the_channel_head_is_the_running_build`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_hold_when_the_channel_head_is_older_than_the_running_build`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_apply_recreates_the_nest_on_the_newer_build_with_its_configuration`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_channel_change_from_the_app_is_applied_on_the_next_poll`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_recreate_cut_short_is_put_back_at_the_next_start`
    - `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_pull_of_a_head_whose_blob_is_gone_is_reported_and_moves_nothing`
35. [app] The admin blocks a reported post from its queue row only after confirming a summary that names the post and the report's reason; everyone but its author then sees a notice in its place, the author keeps their own view and can appeal, and Unblock serves the same post again — `docs/goal/behavior/moderation.md` § Admin block of a reported item
    - `tests/e2e-unified/tests/test_admin_block.py::test_the_admin_blocks_a_reported_post_and_unblocks_it_through_the_app`
36. [app] The admin changes which builds the server installs from the app, and the status line says what the server follows and runs: following the chosen channel, waiting with both versions when that channel is behind the server's version, or following none on a server installed from a single build until a channel is chosen — `docs/goal/architecture/nest/update-channel.md` § The UI
    - `tests/e2e-unified/tests/test_admin_update_channel.py::test_admin_moves_the_box_between_channels_and_the_status_follows_the_decision`
    - `tests/e2e-unified/tests/test_admin_update_channel.py::test_a_pinned_box_follows_no_channel_until_one_is_chosen`
37. [app] On a server the installer image does not run, the Nest page's update section says the server is not run from the image and shows the version it runs, and its channel choices and save are inert — `docs/goal/architecture/nest/update-channel.md` § The UI
    - `tests/e2e-unified/tests/test_admin_update_channel.py::test_a_nest_the_image_does_not_run_says_so_and_its_controls_are_inert`
38. [nest] A newly built server whose first download of the nest fails keeps trying until it succeeds, on every start, so a registry outage or a build not yet published never leaves it out of reach — `docs/goal/architecture/installers/vps.md` § What Happens Behind the Scenes
    - `tests/e2e-unified/tests/platform/docker/test_first_boot_nest_up.py::test_runcmd_starts_the_unit_instead_of_the_compose`
    - `tests/e2e-unified/tests/platform/docker/test_first_boot_nest_up.py::test_nest_up_retries_until_compose_succeeds`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.4-dev+52904896 standalone |
| linux | ⚠ partial | 0.1.4-dev+52904896 standalone |
| windows | ⚠ partial | 0.1.4-dev+52904896 standalone |
| macos | ⚠ partial | 0.1.4-dev+52904896 standalone |
| ios | ⚠ partial | 0.1.4-dev+52904896 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.4-dev+52904896 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_control_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_flip_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_nest_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_nest_pairing_toggle_flips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_field_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_invalid_surfaces_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_host_maintenance.py::test_os_maintenance_status_not_reported_without_host_data` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_host_maintenance.py::test_os_maintenance_status_up_to_date` | web (linux): passed, linux (linux): passed, windows (windows): error, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_host_maintenance.py::test_os_restart_pending_shows_button_and_triggers` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_malformed_region_is_refused_client_side` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_rotation_repin.py::test_a_committed_rotation_repins_silently_through_the_chain` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_legal_takedown.py::test_the_admin_takes_down_and_restores_a_post_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_the_chain_carries_the_hop_a_real_rotation_wrote` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_a_rotation_evicts_every_bearer_minted_before_it` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_oauth_issuer_keys.py::test_the_admin_walks_the_three_sign_in_key_controls_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_restricted_posts_text_reaches_the_admins_when_ticked` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_many_reports_of_one_thing_in_a_day_ring_the_admin_once` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_abuse_reporting.py::test_resolving_a_report_changes_nothing_about_the_content_or_its_author` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_serving_port.py::test_a_router_fronted_nest_shows_the_serving_port_read_only` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 17 | nest | `tests/e2e-unified/tests/api/test_serving_port_bind_fallback.py::test_an_unbindable_serving_port_leaves_the_nest_reachable_on_the_port_it_had` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_host_maintenance.py::test_os_fields_admin_gated_off_anonymous` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/platform/docker/test_host_reboot_coordinator.py::test_unattended_upgrades_on_with_its_own_reboot_off` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/platform/docker/test_host_reboot_coordinator.py::test_coordinator_reboots_idle_or_at_the_ceiling` | nest (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_region_section_warns_when_the_authority_has_gone_quiet` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_admin_web_app_origin.py::test_admin_flips_app_to_the_central_origin_and_back` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_domainless_box_serves_bundled_under_central` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_non_admin_cannot_set_the_choice` | nest (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_legal_takedown_notice.py::test_a_message_taken_down_from_a_report_shows_the_notice_to_a_member_who_reads_it_later` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 24 | nest | `tests/e2e-unified/tests/api/test_admin_roster.py::test_the_identity_rotation_is_refused_while_an_admin_removal_is_pending` | nest (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_the_rotation_confirm_stays_disabled_while_the_roster_is_unresolved` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_rotation_adds_a_key_and_keeps_serving_the_old_one` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_a_forced_rotation_drops_every_other_key_from_the_jwks_at_once` | nest (linux): passed |
| 28 | nest | `tests/e2e-unified/tests/api/test_oauth_session_secret_rotation.py::test_ending_every_saved_sign_in_refuses_each_outside_apps_next_refresh` | nest (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_host_maintenance.py::test_host_maintenance_status_and_restart` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_update_channel.py::test_the_seed_is_followed_until_the_admin_chooses_and_the_choice_survives_a_restart` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_update_channel.py::test_a_non_channel_tag_leaves_the_box_pinned` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_update_channel.py::test_an_unknown_kind_is_the_predates_the_choice_answer` | nest (linux): passed |
| 32 | nest | `tests/e2e-unified/tests/api/test_update_channel.py::test_a_non_admin_cannot_read_or_set_it_and_an_unknown_channel_is_refused` | nest (linux): passed |
| 33 | nest | `tests/e2e-unified/tests/api/test_update_channel.py::test_the_channel_head_version_is_read_from_the_registry_and_absent_without_it` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_the_updater_hands_itself_off_to_the_build_it_moved_the_nest_to` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_hand_off_killed_between_create_and_start_settles_to_one_updater` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_current_when_the_channel_head_is_the_running_build` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_hold_when_the_channel_head_is_older_than_the_running_build` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_apply_recreates_the_nest_on_the_newer_build_with_its_configuration` | nest (linux): failed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_channel_change_from_the_app_is_applied_on_the_next_poll` | nest (linux): failed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_recreate_cut_short_is_put_back_at_the_next_start` | nest (linux): passed |
| 34 | nest | `tests/e2e-unified/tests/platform/docker/test_update_channel_updater.py::test_a_pull_of_a_head_whose_blob_is_gone_is_reported_and_moves_nothing` | nest (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_admin_block.py::test_the_admin_blocks_a_reported_post_and_unblocks_it_through_the_app` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_admin_update_channel.py::test_admin_moves_the_box_between_channels_and_the_status_follows_the_decision` | web (linux): passed, tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_admin_update_channel.py::test_a_pinned_box_follows_no_channel_until_one_is_chosen` | web (linux): passed, tui (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_admin_update_channel.py::test_a_nest_the_image_does_not_run_says_so_and_its_controls_are_inert` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/platform/docker/test_first_boot_nest_up.py::test_runcmd_starts_the_unit_instead_of_the_compose` | nest (linux): passed |
| 38 | nest | `tests/e2e-unified/tests/platform/docker/test_first_boot_nest_up.py::test_nest_up_retries_until_compose_succeeds` | nest (linux): passed |
<!-- features-render:end -->
