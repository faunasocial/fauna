---
slug: address-book
title: Address book
section: everyday
goal: docs/goal/behavior/carddav-server.md § Goal
guide: docs/guides/calendar-and-contacts.md § Contacts: two different address books, on purpose
---

## What a user gets

A second segment of Contacts is your address book: the cards a standard contacts
app keeps on your nest. Open a card and its name, phone and email are there; the
card is stored encrypted on your nest, which opens it with the password of a
mail, calendar, contacts or file app you connect, and only for that app's
signed-in session, and the Fauna app opens it on your own device. Beyond that,
no permission you give your nest lets it read your address book. Your nest also
sees what any server that delivers your messages must: who you're connected to —
your contacts, whom you follow, the groups you're in — and when and how much you
send.

## Coverage contract

Stamped 2026-10-21 at 77c610862e.

1. [app] The address book lists your cards and opening one shows its details — `docs/goal/behavior/carddav-server.md` § What a CardDAV address book *is*, in Fauna terms
   - `tests/e2e-unified/tests/test_addressbook.py::test_address_book_lists_and_details_mda_sealed_card`
2. [nest] Cards rest sealed on your nest and serve back byte for byte — `docs/goal/architecture/message-segment-store.md` § Invariants
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_body_rests_sealed_in_its_segment_and_serves_byte_identically`
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_restore_tells_the_contacts_app_to_reconverge`
3. [app] A card added from another app appears while you stay on the address book — `docs/goal/behavior/carddav-server.md` § MDA ↔ nest WS-RPC contract
   - `tests/e2e-unified/tests/test_addressbook.py::test_a_card_added_from_another_app_appears_while_on_the_address_book`
4. [nest] When a contacts snapshot your nest holds is restored, your cards come back, and a contacts app that had already synced is told to fetch them again rather than handed an update that quietly leaves the restored card out — `docs/goal/behavior/carddav-server.md` § Storage model
   - `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_restore_tells_the_contacts_app_to_reconverge`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| linux | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| windows | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+3f9f2b95 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_addressbook.py::test_address_book_lists_and_details_mda_sealed_card` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_body_rests_sealed_in_its_segment_and_serves_byte_identically` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 2 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_restore_tells_the_contacts_app_to_reconverge` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_addressbook.py::test_a_card_added_from_another_app_appears_while_on_the_address_book` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | nest | `tests/e2e-unified/tests/test_dav_content_at_rest_e2e.py::test_card_restore_tells_the_contacts_app_to_reconverge` | nest (linux): passed, nest (macos): passed, nest (windows): passed |
<!-- features-render:end -->
