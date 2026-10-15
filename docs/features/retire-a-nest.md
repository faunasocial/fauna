---
slug: retire-a-nest
title: Retire a nest
section: your nest
goal: docs/goal/behavior/nest-retirement.md § Goal
guide: docs/guides/retire-a-nest.md
---

## What a user gets

A server your app set up in your cloud account can be taken down from the app too,
even when the nest on it no longer answers. You enter your cloud token, see just the
servers Fauna created there, type the name of the one to delete, and the app removes
the DNS records that pointed at it before it deletes the server itself — so none
of your names still points at an address your provider may hand to someone else.
Where the app has no access to your DNS, it lists the records for you to remove
yourself. If your
domain came from a provider that can hand out its transfer code, the app fetches that
too.

## Coverage contract

Stamped 2026-10-13 at ab51f8d2bd.

1. [app] The retire view lists the servers Fauna created in your cloud account, each with the domain it served where the app can confirm one — `docs/goal/behavior/nest-retirement.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
2. [app] Deleting a server needs its name typed exactly, and removes the DNS records pointing at it before the server, leaving every other record alone — `docs/goal/behavior/nest-retirement.md` § Confirm shape
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
3. [app] If the DNS cleanup fails the server is left standing, and you choose to try again or delete it anyway — never a different server — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_failed_dns_stops_before_the_server_and_force_deletes_only_it`
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_leaving_a_failed_run_disarms_it_for_every_other_row`
4. [app] Where the provider offers it, the app fetches the domain's transfer code, or tells you when the registry lock lifts — `docs/goal/behavior/nest-retirement.md` § Transfer authorization code
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg`
5. [app] A signed-in admin can retire the server they are signed into from the admin Nest page — `docs/goal/behavior/nest-retirement.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into`
6. [app] The DNS cleanup also works through the DNS access you already gave the app, even when the cloud token you type cannot reach your domain's records — `docs/goal/behavior/nest-retirement.md` § Credential stance
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_cleans_dns_through_the_held_credential`
7. [app] When your nest cannot be reached, the screen that says so always offers to retire a server, and that needs only your cloud token, never a recovery key — `docs/goal/behavior/nest-retirement.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
8. [app] The app asks for your cloud token every time you open the retire view and forgets it when you leave; it is never saved — `docs/goal/behavior/nest-retirement.md` § Credential stance
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_forgets_the_token_on_leaving_and_asks_again`
9. [app] The server you are signed into is marked as such in the list — `docs/goal/behavior/nest-retirement.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into`
10. [app] A server that carries no Fauna marker is listed with a note saying so — `docs/goal/behavior/nest-retirement.md` § Layout & flow
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_lists_unmarked_servers_with_a_note_behind_the_same_typed_confirm`
11. [app] When the account holds no server Fauna created, the app says so, and says that servers created before 8 July 2026 carry no marker and are retired from the provider's own dashboard — `docs/goal/behavior/nest-retirement.md` § Layout & flow
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_empty_account_says_so_and_names_the_pre_marker_case`
12. [app] A server whose domain the app cannot confirm shows no domain: the app never guesses one from the server's name — `docs/goal/behavior/nest-retirement.md` § Box → domain attribution
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
13. [app] A server with no confirmed domain can still be deleted, and no DNS record is touched for it — `docs/goal/behavior/nest-retirement.md` § Box → domain attribution
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns`
14. [app] A server that served several domains shows every other domain confirmed to point at it — `docs/goal/behavior/nest-retirement.md` § Box → domain attribution
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
15. [app] Before deleting, the app shows which server it is about to delete: its name, its address and every domain it served — `docs/goal/behavior/nest-retirement.md` § Confirm shape
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
16. [app] The confirm warns that everything on the server is destroyed and can be recovered only from a backup — `docs/goal/behavior/nest-retirement.md` § Confirm shape
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns`
17. [app] The confirm says which DNS records will be removed automatically, or that none will be and the list of what to remove by hand follows — `docs/goal/behavior/nest-retirement.md` § Confirm shape
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns`
18. [app] Deleting the server you are signed into warns that your nest will stop existing — `docs/goal/behavior/nest-retirement.md` § Confirm shape
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into`
19. [app] After the server you were signed into is deleted, the app returns to its start — `docs/goal/behavior/nest-retirement.md` § Where logic lives
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into`
20. [app] If the app is closed part-way through, the server stays listed until it is actually deleted, and retiring it again finishes the job — `docs/goal/behavior/nest-retirement.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_run_that_stopped_leaves_the_server_listed_and_retiring_again_finishes`
21. [app] A server that is already gone when the app comes to delete it counts as deleted, not as an error — `docs/goal/behavior/nest-retirement.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_server_already_gone_counts_as_deleted`
22. [app] The mail-policy records that share their names with other services are left in place and listed for you to remove by hand — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_run_that_stopped_leaves_the_server_listed_and_retiring_again_finishes`
23. [app] When the app has no access to your domain's DNS, nothing is removed automatically, and the by-hand list starts with the records still pointing at the server, each with the value that identifies it — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_without_dns_access_removes_nothing_and_lists_what_still_points_at_the_server`
24. [app] After you choose to delete the server despite a failed DNS step, the records that were not removed join the by-hand list, each with the value that identifies it — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_failed_dns_stops_before_the_server_and_force_deletes_only_it`
25. [app] Choosing to delete the server anyway repeats the warning that records left behind point at an address someone else may be given — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - (none)
26. [app] When a server served several domains, the records pointing at it are removed from every one of them, not only the main one — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
27. [app] Records for another domain whose DNS the app cannot reach are listed for you to remove by hand — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_lists_another_domains_records_by_hand_when_its_dns_is_out_of_reach`
28. [app] The records the app itself published for the domain — its mail-signing key, its Bluesky handle record and its nest record — are removed too — `docs/goal/behavior/nest-retirement.md` § DNS cleanup — scope and order
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_removes_the_records_the_app_itself_published`
29. [app] When it is done the app shows what happened and lists whatever DNS is left for you to remove by hand — `docs/goal/behavior/nest-retirement.md` § Layout & flow
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
30. [app] Where the provider cannot hand out the transfer code, the server's row says to get it from your registrar's own dashboard — `docs/goal/behavior/nest-retirement.md` § Transfer authorization code
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server`
31. [app] A fetched transfer code is forgotten when you leave the view, never saved — `docs/goal/behavior/nest-retirement.md` § Transfer authorization code
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_is_forgotten_on_leaving_and_offered_only_for_a_confirmed_domain`
32. [app] Getting the transfer code deletes nothing — `docs/goal/behavior/nest-retirement.md` § Layout & flow
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg`
33. [app] The transfer code is offered only for a server whose domain the app has confirmed — `docs/goal/behavior/nest-retirement.md` § Transfer authorization code
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_is_forgotten_on_leaving_and_offered_only_for_a_confirmed_domain`
34. [app] Retiring a server does not throw away your nest's recovery key, so the same nest can later be recovered elsewhere from your backups — `docs/goal/behavior/nest-retirement.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_keeps_the_nests_recovery_key`
35. [app] The list holds every server Fauna created in the account, however many there are — `docs/goal/behavior/nest-retirement.md` § Errors & edge cases
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_list_holds_every_server_however_many`
36. [app] A provider you sign in to, rather than paste a token for, works in the retire view as it does when setting a nest up — `docs/goal/behavior/nest-retirement.md` § Where logic lives
    - `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.4-dev+811bd5e7.dirty standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.4-dev+e0aed9d5.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_failed_dns_stops_before_the_server_and_force_deletes_only_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_leaving_a_failed_run_disarms_it_for_every_other_row` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into` | tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_cleans_dns_through_the_held_credential` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_forgets_the_token_on_leaving_and_asks_again` | tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into` | tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_lists_unmarked_servers_with_a_note_behind_the_same_typed_confirm` | tui (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_empty_account_says_so_and_names_the_pre_marker_case` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 12 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns` | tui (linux): passed |
| 14 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 15 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 16 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns` | tui (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns` | tui (linux): passed |
| 18 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into` | tui (linux): passed |
| 19 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into` | tui (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_run_that_stopped_leaves_the_server_listed_and_retiring_again_finishes` | tui (linux): passed |
| 21 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_server_already_gone_counts_as_deleted` | tui (linux): passed |
| 22 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_a_run_that_stopped_leaves_the_server_listed_and_retiring_again_finishes` | tui (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_without_dns_access_removes_nothing_and_lists_what_still_points_at_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 24 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_failed_dns_stops_before_the_server_and_force_deletes_only_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 25 | app | (none) | — |
| 26 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 27 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_lists_another_domains_records_by_hand_when_its_dns_is_out_of_reach` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_removes_the_records_the_app_itself_published` | tui (linux): passed |
| 29 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 30 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_walk_lists_confirms_and_cleans_dns_before_the_server` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 31 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_is_forgotten_on_leaving_and_offered_only_for_a_confirmed_domain` | tui (linux): passed |
| 32 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 33 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_is_forgotten_on_leaving_and_offered_only_for_a_confirmed_domain` | tui (linux): passed |
| 34 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_keeps_the_nests_recovery_key` | tui (linux): passed |
| 35 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_list_holds_every_server_however_many` | tui (linux): passed |
| 36 | app | `tests/e2e-unified/tests/test_nest_retire.py::test_retire_transfer_code_leg` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
