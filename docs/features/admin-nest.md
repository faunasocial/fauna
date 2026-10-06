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
    - (none)
12. [app] Marking a report acted on or dismissed records the decision and takes it off the queue — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
13. [app] A new report notifies each admin and points them at the queue — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns`
14. [nest] Many reports of the same thing in one day ring each admin once — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - (none)
15. [nest] Resolving a report changes nothing about the reported content or its author; only the takedown console and a suspension do — `docs/goal/behavior/moderation.md` § Where it lands, who acts, and with what
    - (none)
16. [app] On a deployment whose port is fixed, the serving port shows read-only and says the deployment serves on 443 — `docs/goal/architecture/nest/common.md` § Serving ports
    - (none)
17. [nest] A serving port the nest cannot use never takes it down: it stays reachable on the port it had, so the admin can pick another — `docs/goal/architecture/nest/common.md` § Serving ports
    - (none)
18. [nest] Only an admin learns whether the server has updates or a restart pending; anyone else is told nothing is pending — `docs/goal/architecture/installers/vps.md` § 4 — Admin visibility
    - `tests/e2e-unified/tests/api/test_host_maintenance.py::test_os_fields_admin_gated_off_anonymous`
19. [nest] The server installs security updates by itself and restarts itself when nobody is connected, or within a day at the latest — `docs/goal/architecture/installers/vps.md` § 2 — Reboot
    - (none)
20. [app] When the nest has stopped hearing from its region's authority, the region section warns that the last rules received still apply — `docs/goal/behavior/admin.md` § N. Nest
    - (none)
21. [app] The admin chooses whether the nest's own address opens the app the nest ships or hands visitors to the central app, and sees the address they will be sent to — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_web_app_origin.py::test_admin_flips_app_to_the_central_origin_and_back`
22. [nest] When the admin chooses the central app, the nest's own app address sends visitors there with the nest filled in, choosing the bundled app stops that at once, a nest with no domain keeps serving its own app, and only an admin can change the choice — `docs/goal/behavior/web-content-hosting.md` § Same-origin security model
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back`
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_domainless_box_serves_bundled_under_central`
    - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_non_admin_cannot_set_the_choice`
23. [app] The admin takes down a single conversation message under a legal order from the same console, and it stops being delivered from then on — `docs/goal/behavior/moderation.md` § Legal takedown
    - (none)
24. [nest] The identity rotation is refused while the removal of an admin is still pending — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
    - (none)
25. [app] While the app cannot yet tell which admins will inherit the new identity, the rotation's confirm stays visible but disabled, says why, and lists nobody — `docs/goal/behavior/admin.md` § N. Nest
    - (none)
26. [nest] After the admin replaces the sign-in key as a precaution, the old key stays published for a set time beside the new one, so sign-ins it signed keep verifying — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_rotation_adds_a_key_and_keeps_serving_the_old_one`
27. [nest] After the admin replaces the sign-in key because of a leak, the very next look at the nest's published keys shows only the new one — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_a_forced_rotation_drops_every_other_key_from_the_jwks_at_once`
28. [nest] After the admin ends every saved sign-in, each connected outside app is refused the next time it refreshes and leaves the member's list of connected apps until it is approved again — `docs/goal/behavior/authorization-server.md` § The issuer (TP5)
    - (none)
29. [app] A declared region always shows whose rules apply to the nest, and withdrawing the declaration clears it — `docs/goal/behavior/admin.md` § N. Nest
    - `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips`
30. [app] Before the identity rotation is sent, its confirm lists the admins who will inherit the new identity — `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed rotation
    - `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app`
31. [nest] The nest reports the server's pending updates and pending restart, passes a restart request on to the server, and refuses the request when it has no server to ask — `docs/goal/architecture/installers/vps.md` § 4 — Admin visibility
    - `tests/e2e-unified/tests/api/test_host_maintenance.py::test_host_maintenance_status_and_restart`

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
| tui | ⚠ partial | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_control_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_admin_nat_mode.py::test_nat_mode_flip_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_nest_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_nest_pairing_toggle_flips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_field_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_serving_port.py::test_serving_port_invalid_surfaces_error` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_host_maintenance.py::test_os_maintenance_status_up_to_date` | web (linux): passed, linux (linux): passed, windows (windows): error, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_host_maintenance.py::test_os_restart_pending_shows_button_and_triggers` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_malformed_region_is_refused_client_side` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): failed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_rotation_repin.py::test_a_committed_rotation_repins_silently_through_the_chain` | linux (linux): passed, tui (linux): passed, tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_admin_legal_takedown.py::test_the_admin_takes_down_and_restores_a_post_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_the_chain_carries_the_hop_a_real_rotation_wrote` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_nest_rotation_chain.py::test_a_rotation_evicts_every_bearer_minted_before_it` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_admin_oauth_issuer_keys.py::test_the_admin_walks_the_three_sign_in_key_controls_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | tui (linux): passed |
| 11 | app | (none) | — |
| 12 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_abuse_reporting.py::test_a_report_reaches_the_admin_queue_and_the_outcome_returns` | tui (linux): passed |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | app | (none) | — |
| 17 | nest | (none) | — |
| 18 | nest | `tests/e2e-unified/tests/api/test_host_maintenance.py::test_os_fields_admin_gated_off_anonymous` | nest (linux): passed |
| 19 | nest | (none) | — |
| 20 | app | (none) | — |
| 21 | app | `tests/e2e-unified/tests/test_admin_web_app_origin.py::test_admin_flips_app_to_the_central_origin_and_back` | linux (linux): skipped, tui (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_domainless_box_serves_bundled_under_central` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_a_non_admin_cannot_set_the_choice` | nest (linux): passed |
| 23 | app | (none) | — |
| 24 | nest | (none) | — |
| 25 | app | (none) | — |
| 26 | nest | `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_rotation_adds_a_key_and_keeps_serving_the_old_one` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_oauth_issuer.py::test_a_forced_rotation_drops_every_other_key_from_the_jwks_at_once` | nest (linux): passed |
| 28 | nest | (none) | — |
| 29 | app | `tests/e2e-unified/tests/test_admin_nest.py::test_region_declaration_round_trips` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_nest_rotation_admin_journey.py::test_the_admin_rotates_the_deployment_seed_through_the_app` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): failed, tui (linux): passed |
| 31 | nest | `tests/e2e-unified/tests/api/test_host_maintenance.py::test_host_maintenance_status_and_restart` | nest (linux): passed |
<!-- features-render:end -->
