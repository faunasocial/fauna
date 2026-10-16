---
slug: import-your-social-archive
title: Bring your posts from Facebook or Instagram
section: bridges and other networks
goal: docs/goal/behavior/archive-import.md § Goal
guide: docs/guides/import-your-social-archive.md § 2. Import it
---

## What a user gets

Download the export archive Facebook or Instagram gives you and bring it into
Fauna from the app. Your posts and albums come back at the dates they
happened, shown to the same kind of people who could see them before, each
marked with the service it came from, and your events land in your own
calendar. The untouched archive rests sealed in a
folder on your nest, so nothing in it is ever lost. Other people's comments and
messages in your archive are kept for you alone and never republished — and
once a friend who also imported links with you, your comments and reactions on
each other's posts come back, each signed by the person who wrote it.

## Coverage contract

Stamped 2026-10-09 at 53a1f516a7.

1. [app] Run the import from start to finish and your own posts appear in your feed at the dates you first posted them, each marked with the service it came from — `docs/goal/behavior/archive-import.md` § Goal
   - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
2. [app] The import is reachable from Settings and opens on choosing which service your archive is from, with how to request the export there — `docs/goal/behavior/archive-import.md` § The wizard and its machine
   - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_is_reachable_and_opens_on_the_source_step`
3. [app] An Instagram archive imports just as a Facebook one does — `docs/goal/behavior/archive-import.md` § Goal
   - (none)
4. [app] Opening your archive shows what it holds — the account, and how many posts, albums and other records — before you choose anything, and your friends lists are kept in the archive, never imported as contacts — `docs/goal/behavior/archive-import.md` § The wizard and its machine
   - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
   - `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder`
   - `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not`
5. [app] A file that is not an archive from a supported service, or an export in the wrong format, is refused with a message naming what to request instead — `docs/goal/behavior/archive-import.md` § The wizard and its machine
   - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
6. [app] You choose which kinds of record to bring, limit it to a date range, or make everything visible only to you — `docs/goal/behavior/archive-import.md` § The wizard and its machine
   - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
   - `tests/e2e-unified/tests/test_archive_import.py::test_a_one_day_only_me_import_takes_that_whole_day_visible_to_the_owner_alone`
7. [app] Each post keeps its original audience: a public post stays public, a friends-only post opens for the people who follow you, and a post whose audience was private, a custom list or unrecorded lands visible only to you — `docs/goal/behavior/archive-import.md` § What each category becomes
   - `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not`
   - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
   - `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder`
8. [app] Photo albums come back as posts carrying their pictures, at the album's own date and audience — `docs/goal/behavior/archive-import.md` § What each category becomes
   - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
9. [app] The archive rests in a folder of its own on your nest, which you can find among your folders — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
   - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
10. [app] An import that stops part-way — paused, or the app closed — picks up where it left off when you come back, and nothing arrives twice — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder`
11. [app] You can pause an import yourself, or cancel it and keep everything that already arrived — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_user_pauses_resumes_and_cancels_and_what_arrived_stays`
12. [app] When it finishes you see how many records were imported, and the archive's folder by name — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
13. [app] Your old profile's name, bio, links and picture are offered as a one-tap fill-in at the end, never applied without asking — `docs/goal/behavior/archive-import.md` § What each category becomes
    - (none)
14. [app] Importing the same archive again, or a newer one, adds only what is new — `docs/goal/behavior/archive-import.md` § Goal
    - `tests/e2e-unified/tests/test_archive_import.py::test_importing_again_adds_only_what_is_new_into_a_folder_of_its_own`
15. [app] Other people's comments on your posts are kept for you alone and shown folded under the post they were left on, never published — `docs/goal/behavior/archive-import.md` § What each category becomes
    - (none)
16. [app] Your message threads from the archive are kept for you alone and listed among your conversations, read-only, marked as coming from an archive — `docs/goal/behavior/archive-import.md` § What each category becomes
    - (none)
17. [nest] A post you imported is never sent on to another network or shown to anonymous visitors from outside Fauna — `docs/goal/behavior/archive-import.md` § Compatibility
    - (none)
18. [app] When a contact who also imported confirms, as you do, that they are the same person as in your archive, their comments and reactions on your imported posts, and yours on theirs, come back automatically — each one signed by the person who wrote it — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
19. [app] Unlinking from a contact stops future merges, while what already came back stays with the person who wrote it — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
20. [app] Your events land in your calendar at their original times, with where they were and your own reply — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_imported_events_land_in_the_calendar_at_their_times_visible_to_the_owner_alone`
21. [app] A friends-only or private imported post shows anyone who cannot open it only a neutral line naming the service it came from, never any of its own words — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not`
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
22. [app] A friends-only post imported before anyone follows you opens for whoever follows you afterwards — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not`
23. [app] Your imported events are visible to you alone — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_imported_events_land_in_the_calendar_at_their_times_visible_to_the_owner_alone`
24. [app] Nobody named in your archive — a commenter, someone tagged, a message partner, an event guest — gets a profile or a contact in Fauna because of your import — `docs/goal/behavior/archive-import.md` § Architectural rules
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
25. [app] Your ad interests, login history, saved items and liked pages are never imported — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
26. [app] Nothing is written to your nest until you press Start: backing out before then leaves no trace there — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
27. [app] Before you start you see how many records will be imported and roughly how much will be uploaded — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
28. [app] Before you import you are told how many posts carry a recorded audience and will keep it, and how many carry none and will be visible only to you — `docs/goal/behavior/archive-import.md` § Element IDs
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
29. [app] A date you mistype, or a start date after the end date, stops you from going on, rather than importing with no range at all — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
30. [app] A date range takes in the whole of both the first and the last day you name — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_one_day_only_me_import_takes_that_whole_day_visible_to_the_owner_alone`
31. [app] The first step links straight to the place on the other service where you request your export — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start`
32. [app] While an import runs you see progress for each kind of record and an overall bar — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder`
33. [app] While an import runs you see how long it has been going and an estimate of the time left — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder`
34. [app] One record the app cannot read never stops the import: it is skipped and listed with the reason — `docs/goal/behavior/archive-import.md` § Parser contract
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
35. [app] An import started on one of your devices can be picked up and finished from another — `docs/goal/behavior/archive-import.md` § The wizard and its machine
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_started_on_one_device_is_finished_from_another`
36. [app] An export in a newer layout than the app knows still imports what the app recognizes, rather than being refused — `docs/goal/behavior/archive-import.md` § Parser contract
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
37. [app] Accented letters and non-Latin text in your posts come through as you wrote them, not as garbled characters — `docs/goal/behavior/archive-import.md` § Parser contract
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not`
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates`
38. [app] A record with more pictures than the app will take is noted in the skipped list with how many were dropped, never trimmed silently — `docs/goal/behavior/archive-import.md` § Parser contract
    - `tests/e2e-unified/tests/test_archive_import.py::test_a_record_with_too_many_pictures_is_noted_in_the_skip_list_with_how_many_were_dropped`
39. [app] The archive's folder counts toward your storage allowance like any folder — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts`
40. [app] The archive's folder is backed up like any other folder — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts`
41. [app] The archive stays on your nest and is not copied down to your devices — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_stays_on_the_nest_and_no_device_holds_a_copy`
42. [app] Deleting the archive's folder removes the archive and leaves every post it produced in place — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts`
43. [app] Each archive you import rests in a folder of its own, so a newer export never overwrites an older one — `docs/goal/behavior/archive-import.md` § Storage — the archive folder
    - `tests/e2e-unified/tests/test_archive_import.py::test_importing_again_adds_only_what_is_new_into_a_folder_of_its_own`
44. [nest] Your nest cannot read your archive or anything the app worked out from it: it only holds them sealed — `docs/goal/behavior/archive-import.md` § Architectural rules
    - `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_the_nest_holds_the_archive_and_what_the_app_worked_out_only_sealed`
45. [nest] An imported post never links back to the original and carries nothing that identifies it on the other service — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_an_imported_post_carries_nothing_that_identifies_it_on_the_other_service`
46. [app] You and others can like, reply to and repost an imported post just as any other post — `docs/goal/behavior/archive-import.md` § What each category becomes
    - `tests/e2e-unified/tests/test_archive_import.py::test_an_imported_post_can_be_liked_replied_to_and_reposted_by_the_owner_and_others`
47. [nest] The private audience your only-you imports rest under is never offered to anyone, and an attempt to subscribe to it is refused as if it did not exist — `docs/goal/behavior/monetization.md` § The unifying model — tier as entitlement
    - `tests/e2e-unified/tests/api/test_only_me_tier_never_offered.py::test_the_only_you_audience_is_never_offered_and_cannot_be_subscribed`
48. [nest] A subscriber to even your highest tier never opens a post you imported as visible only to you — `docs/goal/behavior/monetization.md` § The unifying model — tier as entitlement
    - `tests/e2e-unified/tests/api/test_only_me_tier_never_offered.py::test_a_subscriber_to_the_highest_tier_never_opens_an_only_you_import`
49. [nest] Nobody can look you up by your identity on the other service: no nest keeps a directory of them — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_nobody_can_look_the_importer_up_by_their_identity_on_the_other_service`
50. [app] Linking with someone from your archive is offered only for people who are already your accepted contacts — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
51. [app] When a contact matches someone in your archive, the app asks you once whether they are the same person, and never asks again for that contact — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
52. [app] The exchange that links you with a contact never shows up as messages in your conversation with them — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
53. [app] Once a friend's comment comes back as their own reply, the private copy kept under your post is no longer shown, so it never appears twice — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
54. [app] A friend's reaction that comes back shows as the matching emoji, and a plain like as a like — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
55. [app] After you are linked, a newer import on either side brings back the new comments and reactions, and nothing arrives twice — `docs/goal/behavior/archive-import.md` § Linking and merging (phase two)
    - (none)
56. [app] The folder holds your archive untouched — exactly the file you chose — so a later version of the app can read it again without another export — `docs/goal/behavior/archive-import.md` § Goal
    - `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts`
57. [app] From the finished import you can open a view of just the posts you imported, review what was skipped, and go to the archive's folder — `docs/goal/behavior/archive-import.md` § The wizard and its machine
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
| android | ⚠ partial | |
| tui | ⚠ partial | 0.1.4-dev+46d105c1.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_is_reachable_and_opens_on_the_source_step` | tui (linux): passed |
| 3 | app | (none) | — |
| 4 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder` | tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not` | tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_one_day_only_me_import_takes_that_whole_day_visible_to_the_owner_alone` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder` | tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder` | tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_user_pauses_resumes_and_cancels_and_what_arrived_stays` | tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 13 | app | (none) | — |
| 14 | app | `tests/e2e-unified/tests/test_archive_import.py::test_importing_again_adds_only_what_is_new_into_a_folder_of_its_own` | tui (linux): passed |
| 15 | app | (none) | — |
| 16 | app | (none) | — |
| 17 | nest | (none) | — |
| 18 | app | (none) | — |
| 19 | app | (none) | — |
| 20 | app | `tests/e2e-unified/tests/test_archive_import.py::test_imported_events_land_in_the_calendar_at_their_times_visible_to_the_owner_alone` | tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not` | tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not` | tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_archive_import.py::test_imported_events_land_in_the_calendar_at_their_times_visible_to_the_owner_alone` | tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_one_day_only_me_import_takes_that_whole_day_visible_to_the_owner_alone` | tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_wizard_guards_each_step_and_writes_nothing_before_start` | tui (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder` | tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_restart_mid_import_resumes_from_the_folder` | tui (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_started_on_one_device_is_finished_from_another` | tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_follower_sees_a_friends_only_import_and_a_stranger_does_not` | tui (linux): passed |
| 37 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_import_walks_to_done_and_the_posts_land_at_their_original_dates` | tui (linux): passed |
| 38 | app | `tests/e2e-unified/tests/test_archive_import.py::test_a_record_with_too_many_pictures_is_noted_in_the_skip_list_with_how_many_were_dropped` | tui (linux): passed |
| 39 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts` | tui (linux): passed |
| 40 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts` | tui (linux): passed |
| 41 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_stays_on_the_nest_and_no_device_holds_a_copy` | tui (linux): passed |
| 42 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts` | tui (linux): passed |
| 43 | app | `tests/e2e-unified/tests/test_archive_import.py::test_importing_again_adds_only_what_is_new_into_a_folder_of_its_own` | tui (linux): passed |
| 44 | nest | `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_the_nest_holds_the_archive_and_what_the_app_worked_out_only_sealed` | nest (linux): passed |
| 45 | nest | `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_an_imported_post_carries_nothing_that_identifies_it_on_the_other_service` | nest (linux): passed |
| 46 | app | `tests/e2e-unified/tests/test_archive_import.py::test_an_imported_post_can_be_liked_replied_to_and_reposted_by_the_owner_and_others` | tui (linux): passed |
| 47 | nest | `tests/e2e-unified/tests/api/test_only_me_tier_never_offered.py::test_the_only_you_audience_is_never_offered_and_cannot_be_subscribed` | nest (linux): passed |
| 48 | nest | `tests/e2e-unified/tests/api/test_only_me_tier_never_offered.py::test_a_subscriber_to_the_highest_tier_never_opens_an_only_you_import` | nest (linux): passed |
| 49 | nest | `tests/e2e-unified/tests/api/test_archive_import_nest_promises.py::test_nobody_can_look_the_importer_up_by_their_identity_on_the_other_service` | nest (linux): passed |
| 50 | app | (none) | — |
| 51 | app | (none) | — |
| 52 | app | (none) | — |
| 53 | app | (none) | — |
| 54 | app | (none) | — |
| 55 | app | (none) | — |
| 56 | app | `tests/e2e-unified/tests/test_archive_import.py::test_the_archive_folder_counts_is_backed_up_holds_the_untouched_zip_and_deleting_it_keeps_the_posts` | tui (linux): passed |
| 57 | app | (none) | — |
<!-- features-render:end -->
