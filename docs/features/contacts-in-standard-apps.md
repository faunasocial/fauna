---
slug: contacts-in-standard-apps
title: Your address book in standard contacts apps
section: mail, calendar and contacts
goal: docs/goal/behavior/carddav-server.md § Goal
guide: docs/guides/calendar-and-contacts.md § Contacts: two different address books, on purpose
---

## What a user gets

Any CardDAV contacts app connects with the same address and app password,
finds your address book from the server name alone, and reads and writes cards that
rest sealed on your nest.

## Coverage contract

Stamped 2026-09-23 at 6605debcb9.

1. [app] A contacts app runs the full create, read, update, delete cycle, and finds the book from the server name alone — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_host_only_autodiscovery`
2. [app] A contacts app that has already synced fetches only what changed since last time, including cards deleted elsewhere — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
3. [app] The first time a contacts app connects, your address book is already there — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
4. [app] Deleting an address book from a contacts app removes it for good — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
5. [app] The app password you made for mail signs your contacts app in too; there is no separate contacts password — `docs/goal/behavior/carddav-server.md` § Process topology & attach pattern — a new protocol on the existing MDA (no new process)
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_host_only_autodiscovery`
6. [app] On a nest with mail on, contacts apps work straight away without anyone switching contacts on — `docs/goal/behavior/carddav-server.md` § Independent enablement — `carddav_enabled`
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip`
7. [app] Cards you add in a contacts app never become Fauna contacts or contact requests; your address book and your Fauna contacts stay separate — `docs/goal/behavior/carddav-server.md` § What a CardDAV address book *is*, in Fauna terms
   - `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_cards_from_a_contacts_app_never_become_fauna_contacts`
8. [nest] A contacts app that has been away too long is told to fetch the whole address book again, rather than handed an incomplete update — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_away_too_long_is_sent_to_a_full_resync`
9. [nest] Editing or deleting a card from an out-of-date copy is refused instead of overwriting a newer version — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_editing_or_deleting_from_an_out_of_date_copy_is_refused`
10. [nest] Cards in either common vCard format are accepted, and a malformed card is refused rather than stored half-understood — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_both_vcard_formats_are_accepted_and_a_malformed_card_is_refused`
11. [nest] A contacts app can create more address books alongside the default one — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_creates_another_address_book_beside_the_default`
12. [nest] Renaming an address book or changing its description in a contacts app is kept — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_renaming_an_address_book_and_its_description_is_kept`
13. [nest] A contacts app can search your address book by name, email or another field — `docs/goal/behavior/carddav-server.md` § Address-book collection model + read / write / sync surface
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_searches_the_address_book_by_field`
14. [nest] Given only your email address, a contacts app with automatic setup finds your address book from your domain — `docs/goal/behavior/dns-management.md` § Records covered
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_given_only_an_email_address_a_contacts_app_finds_the_address_book`
15. [nest] A contacts app that sends just your username, without the domain, still signs in — `docs/goal/behavior/carddav-server.md` § Network exposure & discovery
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_signing_in_with_just_the_username_is_let_in`
16. [nest] Without your password a contacts app gets nothing, and after too many wrong passwords further tries are briefly refused — `docs/goal/behavior/carddav-server.md` § Process topology & attach pattern — a new protocol on the existing MDA (no new process)
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_without_the_password_nothing_is_served_and_repeated_guessing_is_braked`
17. [nest] Your contacts keep working in a contacts app when mail is turned off — `docs/goal/behavior/carddav-server.md` § Independent enablement — `carddav_enabled`
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_contacts_keep_working_with_mail_turned_off`
18. [nest] When contacts are switched off for the nest, contacts apps can no longer connect to or discover the address book — `docs/goal/behavior/carddav-server.md` § Independent enablement — `carddav_enabled`
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_switching_contacts_off_closes_the_door_and_the_signpost`
19. [nest] On a home nest with no domain, a contacts app reaches your address book by its bare address — `docs/goal/behavior/carddav-server.md` § Independent enablement — `carddav_enabled`
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_on_a_home_nest_with_no_domain_contacts_are_reached_by_the_bare_address`
20. [nest] Another user on the same nest can never see or change your address book, even when both books share a name — `docs/goal/behavior/carddav-server.md` § Architectural rules
   - `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_another_user_can_never_reach_your_address_book_even_with_the_same_name`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+c9fb0cf1 standalone |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| macos | ✅ full | 0.1.2-dev+96eae039 standalone |
| ios | ✅ full | 0.1.2-dev+96eae039 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+7663afa6 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_host_only_autodiscovery` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_host_only_autodiscovery` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_carddav_direct_url_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_carddav_roundtrip.py::test_cards_from_a_contacts_app_never_become_fauna_contacts` | linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_away_too_long_is_sent_to_a_full_resync` | nest (linux): passed, nest (windows): passed |
| 9 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_editing_or_deleting_from_an_out_of_date_copy_is_refused` | nest (linux): passed, nest (windows): passed |
| 10 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_both_vcard_formats_are_accepted_and_a_malformed_card_is_refused` | nest (linux): passed, nest (windows): passed |
| 11 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_creates_another_address_book_beside_the_default` | nest (linux): passed, nest (windows): passed |
| 12 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_renaming_an_address_book_and_its_description_is_kept` | nest (linux): passed, nest (windows): passed |
| 13 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_searches_the_address_book_by_field` | nest (linux): passed, nest (windows): passed |
| 14 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_given_only_an_email_address_a_contacts_app_finds_the_address_book` | nest (linux): passed, nest (windows): passed |
| 15 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_a_contacts_app_signing_in_with_just_the_username_is_let_in` | nest (linux): passed, nest (windows): passed |
| 16 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_without_the_password_nothing_is_served_and_repeated_guessing_is_braked` | nest (linux): passed, nest (windows): passed |
| 17 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_contacts_keep_working_with_mail_turned_off` | nest (linux): passed, nest (windows): passed |
| 18 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_switching_contacts_off_closes_the_door_and_the_signpost` | nest (linux): passed, nest (windows): passed |
| 19 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_on_a_home_nest_with_no_domain_contacts_are_reached_by_the_bare_address` | nest (linux): passed, nest (windows): passed |
| 20 | nest | `tests/e2e-unified/tests/test_carddav_nest_outcomes.py::test_another_user_can_never_reach_your_address_book_even_with_the_same_name` | nest (linux): passed, nest (windows): passed |
<!-- features-render:end -->
