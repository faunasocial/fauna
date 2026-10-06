---
slug: nostr
title: Nostr
section: bridges and other networks
goal: docs/goal/ui/nostr.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § Nostr: a personal Nostr home
---

## What a user gets

Link a Nostr key, or let the app make one, and choose what cross-publishes.
Manage your relays and the people you follow, connect other Nostr apps through your
nest as their signer, and name the signers whose zap receipts you believe.

## Coverage contract

Stamped 2026-10-01 at f5dec3d933.

1. [app] Link with a generated key, see your public key and content switches, and unlink — `docs/goal/ui/nostr.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nostr.py::test_nostr_page_renders`
   - `tests/e2e-unified/tests/test_nostr.py::test_link_generate_and_unlink_round_trip`
   - `tests/e2e-unified/tests/test_nostr.py::test_content_toggles_round_trip`
2. [app] Add and remove relays and follows; a bad relay address is refused — `docs/goal/ui/nostr.md` § User actions
   - `tests/e2e-unified/tests/test_nostr.py::test_relay_add_remove_round_trip`
   - `tests/e2e-unified/tests/test_nostr.py::test_relay_invalid_url_rejected`
   - `tests/e2e-unified/tests/test_nostr.py::test_relay_private_address_rejected`
   - `tests/e2e-unified/tests/test_nostr.py::test_follow_add_remove_round_trip`
3. [app] Connect another Nostr app through your nest as its signer, and disconnect it — `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer (bunker)
   - `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_section_renders_for_custodial_account`
   - `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_connect_and_disconnect_round_trip`
4. [app] Name the zap signers you believe, and see when none is named — `docs/goal/behavior/monetization.md` § Zap receipts — the trust model
   - `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_zap_signer_section_renders_with_a_stated_empty_state`
   - `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_designate_and_undesignate_round_trip`
5. [nest] Other Nostr apps read the posts you chose to share straight from your nest, which answers as your own relay — `docs/goal/ui/nostr.md` § The relay event store
   - (none)
6. [nest] Turning sharing to Nostr off takes your posts off your nest's relay, and turning it back on brings them back — `docs/goal/ui/nostr.md` § The relay event store
   - (none)
7. [nest] Your nest gives you a Nostr address of the form you@your-domain that Nostr apps resolve to your key — `docs/goal/ui/nostr.md` § Goal
   - (none)
8. [nest] A direct message sent to you over Nostr is accepted by your own nest, with no outside relay needed to deliver it — `docs/goal/ui/nostr.md` § The relay event store
   - (none)
9. [app] Posts from the people you follow on Nostr arrive in your feed — `docs/goal/ui/nostr.md` § Implementation status today
   - (none)
10. [nest] A post someone you follow on Nostr deletes leaves your feed — `docs/goal/ui/nostr.md` § Implementation status today
   - (none)
11. [app] You reply to or quote a Nostr post in your feed like any other post — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - (none)
12. [nest] Your reply to a Nostr post reaches Nostr threaded under it, and your quote carries a link to what you quoted — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - (none)
13. [app] A reply or quote your Nostr settings cannot publish is refused with what to change on the Nostr page, and nothing is posted — `docs/goal/ui/nostr.md` § Errors & edge cases
   - (none)
14. [nest] With automatic publishing on, your new posts go out to your Nostr relays — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - (none)
15. [nest] With publishing replies off, your replies stay off your Nostr relays — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - (none)
16. [nest] Deleting a post you published to Nostr takes it back there too — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - (none)
17. [app] You can link a Nostr key you already have by pasting it, instead of having one made for you — `docs/goal/ui/nostr.md` § Layout & flow
   - (none)
18. [nest] A key held outside your nest, in a browser extension or a remote signer, links only once it has proven it is yours — `docs/goal/ui/nostr.md` § Errors & edge cases
   - (none)
19. [nest] A Nostr key already linked to another account on your nest cannot be linked to yours — `docs/goal/ui/nostr.md` § Errors & edge cases
   - (none)
20. [app] Your linked account says in plain words how it signs: with a key your nest made, a key you brought, or an outside signer — `docs/goal/ui/nostr.md` § User actions
   - (none)
21. [nest] If you remove every relay, nothing of yours is published anywhere; your nest never falls back to relays you did not choose — `docs/goal/ui/nostr.md` § Errors & edge cases
   - (none)
22. [nest] Unlinking your Nostr account disconnects every Nostr app that was using your nest as its signer — `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer (bunker)
   - (none)
23. [app] You can always remove a zap signer you named, even when your plan no longer lets you add one — `docs/goal/ui/nostr.md` § Layout & flow
   - (none)
24. [app] A private message someone sends you over Nostr shows on your Conversations page as a Nostr conversation you can open and read — `docs/goal/ui/nostr.md` § Implementation status today
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_nostr_gift_wrap_arrives_as_a_nostr_bridged_room`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+d6ff0673 standalone |
| linux | ⚠ partial | 0.1.2-dev+c4a95c20 standalone |
| windows | ⚠ partial | 0.1.2-dev+d6ff0673 standalone |
| macos | ⚠ partial | |
| ios | ⚠ partial | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+d6ff0673 standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_nostr_page_renders` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_link_generate_and_unlink_round_trip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_content_toggles_round_trip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_add_remove_round_trip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_invalid_url_rejected` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_private_address_rejected` | web (linux): passed, linux (linux): passed, windows (windows): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_follow_add_remove_round_trip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_section_renders_for_custodial_account` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_connect_and_disconnect_round_trip` | web (linux): passed, web (windows): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_zap_signer_section_renders_with_a_stated_empty_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_designate_and_undesignate_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | nest | (none) | — |
| 9 | app | (none) | — |
| 10 | nest | (none) | — |
| 11 | app | (none) | — |
| 12 | nest | (none) | — |
| 13 | app | (none) | — |
| 14 | nest | (none) | — |
| 15 | nest | (none) | — |
| 16 | nest | (none) | — |
| 17 | app | (none) | — |
| 18 | nest | (none) | — |
| 19 | nest | (none) | — |
| 20 | app | (none) | — |
| 21 | nest | (none) | — |
| 22 | nest | (none) | — |
| 23 | app | (none) | — |
| 24 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_nostr_gift_wrap_arrives_as_a_nostr_bridged_room` | web (linux): passed, linux (linux): passed, tui (linux): passed |
<!-- features-render:end -->
