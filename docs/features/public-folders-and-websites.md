---
slug: public-folders-and-websites
title: Make a folder public, or a website
section: your data and devices
goal: docs/goal/ui/folders.md § Audience and website serving
guide: docs/guides/who-can-see-what.md § Folders you make public
---

## What a user gets

A folder is private until you say otherwise. Make it public, after an explicit
confirmation, and anyone can read it; turn on the website switch and your nest serves
its files at your address. Turning it back to private stops the serving at once and
re-seals what your devices opened. The devices of people you shared it with never
open anything: what they add stays sealed while the folder is public and afterwards.

## Coverage contract

Stamped 2026-09-19 at 25eff180d4.

1. [app] Making a folder public asks you to confirm first, and the website switch is separate from the audience — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_folder_audience_control.py::test_the_audience_control_arms_before_it_publishes`
   - `tests/e2e-unified/tests/test_folder_audience_control.py::test_the_website_toggle_is_orthogonal_to_the_audience`
2. [app] A file dropped into a public website folder is served at your address; a private folder is not served even with the switch on — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_public_website_folder_serve.py::test_public_website_folder_serves_the_file_a_user_dropped_in_it`
   - `tests/e2e-unified/tests/test_public_website_folder_serve.py::test_a_private_folder_is_not_served_even_with_the_website_toggle_on`
   - `tests/e2e-unified/tests/test_media_upload_one_shape.py::test_a_media_upload_into_a_public_website_folder_is_served_at_the_owners_address`
3. [app] Turning a shared folder back to private re-seals what your devices opened, and what a member's device adds stays sealed the whole time — `docs/goal/behavior/folders.md` § Publicly-synced follow
   - `tests/e2e-unified/tests/test_folder_bound_flip_back.py::test_a_bound_folder_flips_back_from_public_and_reseals_its_corpus`
   - `tests/e2e-unified/tests/test_folder_bound_flip_back.py::test_a_member_seat_keeps_what_it_adds_sealed_through_the_public_window_and_after`
4. [nest] Your nest serves a public website folder and stops the moment it goes private — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_web_folder_audience.py::test_a_born_public_website_folder_serves_and_the_toggle_gates_it`
   - `tests/e2e-unified/tests/api/test_web_folder_audience.py::test_a_flip_back_to_private_stops_the_anonymous_serve_on_the_next_request`
5. [app] A folder set to serve a website says whether anyone can really reach it, and when they cannot, names the other switch you still have to turn on — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_folder_audience_copy.py::test_a_website_folder_says_whether_anyone_can_reach_it`
6. [nest] Turning the website switch on for a folder whose files are already there serves those files, not an empty site — `docs/goal/behavior/web-content-hosting.md` § Content model
   - `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_turning_the_website_switch_on_serves_the_files_already_there`
7. [app] Before a folder goes public, the confirmation says that its file and folder names become public too, and that making it private again protects only what you add afterwards — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_folder_audience_copy.py::test_the_go_public_confirmation_states_both_consequences`
8. [app] A folder you are sharing offers no way to make it private while it is still shared, and says the sharing has to go first — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_folder_audience_copy.py::test_a_shared_folder_offers_no_private_and_says_why`
9. [nest] A public folder refuses to also be served to standard file apps or put behind a paywall, whichever you turn on second, and the refusal names what to change — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_public_refuses_webdav_and_a_paywall_whichever_moves_second`
10. [nest] A file your nest could be made to run is never served from your site, however it got into the folder — `docs/goal/behavior/web-content-hosting.md` § Content model
   - `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_an_executable_file_is_never_served_however_it_got_into_the_folder`
11. [app] A folder made public before your app could record that you did it says so, and confirming it public again from the folder's row restores that record — `docs/goal/ui/folders.md` § Audience and website serving
   - `tests/e2e-unified/tests/test_folder_audience_reconfirm.py::test_an_unattested_public_folder_is_healed_by_the_owners_reconfirm`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| linux | ⚠ partial | 0.1.2-dev+ce6fd0a0 standalone |
| windows | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| macos | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| ios | ⚠ partial | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+15971055.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_folder_audience_control.py::test_the_audience_control_arms_before_it_publishes` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_folder_audience_control.py::test_the_website_toggle_is_orthogonal_to_the_audience` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | app | `tests/e2e-unified/tests/test_public_website_folder_serve.py::test_public_website_folder_serves_the_file_a_user_dropped_in_it` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_public_website_folder_serve.py::test_a_private_folder_is_not_served_even_with_the_website_toggle_on` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_media_upload_one_shape.py::test_a_media_upload_into_a_public_website_folder_is_served_at_the_owners_address` | tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_bound_flip_back.py::test_a_bound_folder_flips_back_from_public_and_reseals_its_corpus` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_folder_bound_flip_back.py::test_a_member_seat_keeps_what_it_adds_sealed_through_the_public_window_and_after` | web (linux): passed, tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_web_folder_audience.py::test_a_born_public_website_folder_serves_and_the_toggle_gates_it` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_web_folder_audience.py::test_a_flip_back_to_private_stops_the_anonymous_serve_on_the_next_request` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_folder_audience_copy.py::test_a_website_folder_says_whether_anyone_can_reach_it` | tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_turning_the_website_switch_on_serves_the_files_already_there` | nest (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_folder_audience_copy.py::test_the_go_public_confirmation_states_both_consequences` | tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_folder_audience_copy.py::test_a_shared_folder_offers_no_private_and_says_why` | tui (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_public_refuses_webdav_and_a_paywall_whichever_moves_second` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_web_folder_serving_switches.py::test_an_executable_file_is_never_served_however_it_got_into_the_folder` | nest (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_folder_audience_reconfirm.py::test_an_unattested_public_folder_is_healed_by_the_owners_reconfirm` | tui (linux): passed |
<!-- features-render:end -->
