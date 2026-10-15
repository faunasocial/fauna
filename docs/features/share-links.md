---
slug: share-links
title: Share a link to a file
section: your data and devices
goal: docs/goal/behavior/share-links.md § Flows
guide: docs/guides/app-tour.md § Media
---

## What a user gets

Make a link to a file and send it to anyone: they open it in an ordinary
browser, with no account. A file in a folder you have made public is served
as it is; a private file of your own stays sealed on your nest, and the link
itself carries the key that opens it. Each link expires after
the time you choose, and you can see every link you have made and revoke any
of them, after which it stops working for everyone.

## Coverage contract

Stamped 2026-10-13 at 899b58ab1b.

1. [app] A file in a public folder offers a link; the link appears only once it is registered, a stranger opens it over plain HTTP, it is listed as active with its name, and revoking it from the list stops it working — `docs/goal/behavior/share-links.md` § Flows
   - `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link`
2. [nest] A link's file name rests sealed on the nest, and a link registered without its sealed name is refused — `docs/goal/behavior/share-links.md` § The filename rests sealed
   - (none)
3. [app] When you make a link you choose how long it lasts — a day, a week, a month or a year, a week unless you choose otherwise — and never forever — `docs/goal/behavior/share-links.md` § Expiry
   - `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly`
4. [nest] A link stops working for everyone once the time you chose has passed — `docs/goal/behavior/share-links.md` § What a link is
   - `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_stops_for_everyone_once_its_time_has_passed`
5. [app] Each link in your list shows when it expires — `docs/goal/behavior/share-links.md` § List
   - `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly`
6. [app] A link whose time has run out stays in your list, marked as expired — `docs/goal/behavior/share-links.md` § List
   - `tests/e2e-unified/tests/test_share_links.py::test_a_link_whose_time_has_run_out_stays_listed_marked_expired`
7. [app] A revoked link stays in your list, marked as revoked, with nothing left to copy or revoke on its row — `docs/goal/behavior/share-links.md` § List
   - `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link`
8. [app] Copy on an active link in your list gives exactly the link that was made — `docs/goal/behavior/share-links.md` § List
   - `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly`
9. [app] If your nest does not record a new link, you are shown no link, you are told why, and you can try again from the same place — `docs/goal/behavior/share-links.md` § Create
   - `tests/e2e-unified/tests/test_share_links.py::test_a_link_the_nest_did_not_record_is_never_shown_and_create_tries_again`
10. [app] Revoking a link asks you to confirm first — `docs/goal/behavior/share-links.md` § Revoke
    - `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link`
11. [nest] A link keeps opening the file exactly as it was when the link was made, even after you edit, rename or move the file — `docs/goal/behavior/share-links.md` § What a link is
    - `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_keeps_opening_the_file_as_it_was_after_it_is_edited_and_moved`
12. [nest] A link stops working once the version of the file it points at is no longer kept on your nest — `docs/goal/behavior/share-links.md` § What a link is
    - `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_stops_once_its_version_is_no_longer_kept`
13. [app] The person opening a link receives the file under its own name — `docs/goal/behavior/share-links.md` § The filename rests sealed
    - `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link`
14. [nest] A link your nest could never serve — to a sealed file with no key in the link, or with a key over a file that needs none — is refused when it is made, so nobody ever holds a link that cannot open — `docs/goal/behavior/share-links.md` § Which files can be linked
    - `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_the_nest_could_never_serve_is_refused_when_it_is_made`
15. [nest] A link to a file removed under a legal order answers that the file is unavailable for legal reasons — `docs/goal/behavior/moderation.md` § Legal takedown
    - `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_to_a_file_removed_under_a_legal_order_answers_unavailable_for_legal_reasons`
16. [app] After you take your account back, the links you had made still work and are in your list, where you can revoke them — `docs/goal/behavior/succession-repoint-axis.md` § The declared re-point axis
    - `tests/e2e-unified/tests/test_share_links.py::test_links_made_before_taking_the_account_back_still_work_and_can_be_revoked`
17. [app] A private file — one in a folder only you can open — offers a share link too — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_the_author_links_a_private_file_and_the_link_is_the_key`
    - `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link`
    - `tests/e2e-unified/tests/test_media_upload_one_shape.py::test_a_media_upload_into_a_private_folder_takes_a_link_a_stranger_opens`
18. [app] A file in a folder you share with named people offers no share link — `docs/goal/behavior/share-links.md` § The private-file extension
    - (none)
19. [app] Making a link to a private file tells you that the link itself is the key: anyone who gets hold of it opens the file until it expires or you revoke it — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_the_author_links_a_private_file_and_the_link_is_the_key`
20. [app] Someone with no account opens a private file's link in an ordinary browser and gets the file, checked to be exactly the one you linked — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser`
21. [nest] Your nest never sees the contents of a privately linked file, its name, or the key that opens it — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke`
22. [nest] Revoking a private link, or its expiry, stops your nest serving every part of it — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke`
23. [app] The page a private link opens shows a picture, sound, video or plain text in place and offers every other kind of file only as a download — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser`
24. [app] The key in a private link is never sent to any server, not even your own nest — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser`
25. [app] Opening your own private link while signed in treats you exactly like a stranger: it touches nothing of your account — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser`
26. [app] The page a private link opens leaves the link in the address bar untouched, so the person can keep it — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser`
27. [nest] A service that fetches a private link to build a preview learns nothing about the file, not even its name — `docs/goal/behavior/share-links.md` § The private-file extension
    - `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+7739afc6.dirty standalone |
| linux | ⚠ partial | 0.1.2-dev+7739afc6.dirty standalone |
| windows | ⚠ partial | 0.1.2-dev+9a58388f.dirty standalone |
| macos | ⚠ partial | 0.1.2-dev+39cf14cf standalone |
| ios |  no run recorded | |
| android | ⚠ partial | 0.1.2-dev+7739afc6.dirty standalone |
| tui | ⚠ partial | 0.1.2-dev+7739afc6.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link` | web (linux): passed, linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | nest | (none) | — |
| 3 | app | `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly` | tui (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_stops_for_everyone_once_its_time_has_passed` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_share_links.py::test_a_link_whose_time_has_run_out_stays_listed_marked_expired` | tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link` | web (linux): passed, linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_share_links.py::test_four_lengths_a_week_by_default_shown_in_the_list_and_copied_exactly` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_share_links.py::test_a_link_the_nest_did_not_record_is_never_shown_and_create_tries_again` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link` | web (linux): passed, linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_keeps_opening_the_file_as_it_was_after_it_is_edited_and_moved` | nest (linux): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_stops_once_its_version_is_no_longer_kept` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link` | web (linux): passed, linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_the_nest_could_never_serve_is_refused_when_it_is_made` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_share_links_nest_promises.py::test_a_link_to_a_file_removed_under_a_legal_order_answers_unavailable_for_legal_reasons` | nest (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_share_links.py::test_links_made_before_taking_the_account_back_still_work_and_can_be_revoked` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_the_author_links_a_private_file_and_the_link_is_the_key` | web (linux): passed, linux (linux): passed, android (linux): error, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_share_links.py::test_create_open_list_and_revoke_a_share_link` | web (linux): passed, linux (linux): error, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_media_upload_one_shape.py::test_a_media_upload_into_a_private_folder_takes_a_link_a_stranger_opens` | tui (linux): passed |
| 18 | app | (none) | — |
| 19 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_the_author_links_a_private_file_and_the_link_is_the_key` | web (linux): passed, linux (linux): passed, android (linux): error, tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser` | web (linux): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke` | nest (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser` | web (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser` | web (linux): passed |
| 25 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser` | web (linux): passed |
| 26 | app | `tests/e2e-unified/tests/test_share_links_private.py::test_a_stranger_opens_a_private_link_in_a_browser` | web (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_share_links_private_arm.py::test_the_nest_serves_a_private_link_blind_and_stops_on_revoke` | nest (linux): passed |
<!-- features-render:end -->
