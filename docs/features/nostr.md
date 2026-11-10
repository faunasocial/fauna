---
slug: nostr
title: Nostr
section: bridges and other networks
goal: docs/goal/ui/nostr.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § Nostr: a personal Nostr home
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

Link a Nostr key, or let the app make one, and choose what cross-publishes.
Manage your relays and the people you follow, connect other Nostr apps through your
nest as their signer, and name the signers whose zap receipts you believe.

## Coverage contract

Stamped 2026-11-07 at 6a0ded1015.

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
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_another_nostr_client_reads_your_shared_posts_from_your_nest`
6. [nest] Turning sharing to Nostr off takes your posts off your nest's relay, and turning it back on brings them back — `docs/goal/ui/nostr.md` § The relay event store
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_turning_sharing_off_takes_posts_off_your_relay_and_on_brings_them_back`
7. [nest] Your nest gives you a Nostr address of the form you@your-domain that Nostr apps resolve to your key — `docs/goal/ui/nostr.md` § Goal
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_your_nostr_address_resolves_to_your_key`
8. [nest] A direct message sent to you over Nostr is accepted by your own nest, with no outside relay needed to deliver it — `docs/goal/ui/nostr.md` § The relay event store
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_direct_message_is_accepted_by_your_own_nest`
9. [app] Posts from the people you follow on Nostr arrive in your feed — `docs/goal/ui/nostr.md` § Implementation status today
   - `tests/e2e-unified/tests/test_nostr_inbound_feed.py::test_posts_from_someone_you_follow_on_nostr_arrive_in_your_feed`
10. [nest] A post someone you follow on Nostr deletes leaves your feed — `docs/goal/ui/nostr.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_followed_authors_delete_removes_the_post_from_your_feed`
11. [app] You reply to or quote a Nostr post in your feed like any other post — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_a_reply_to_a_nostr_post_reaches_nostr_threaded_under_it`
   - `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_a_quote_of_a_nostr_post_reaches_nostr_linking_it`
12. [nest] Your reply to a Nostr post reaches Nostr threaded under it, and your quote carries a link to what you quoted — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_your_reply_threads_under_a_nostr_post_and_your_quote_links_it`
13. [app] A reply or quote your Nostr settings cannot publish is refused with what to change on the Nostr page, and nothing is posted — `docs/goal/ui/nostr.md` § Errors & edge cases
   - `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_replying_or_quoting_with_no_relays_is_refused_naming_the_nostr_page`
14. [nest] With automatic publishing on, your new posts go out to your Nostr relays — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_automatic_publishing_sends_your_new_posts_to_your_relays`
15. [nest] With publishing replies off, your replies stay off your Nostr relays — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_with_publishing_replies_off_your_replies_stay_off_your_relays`
16. [nest] Deleting a post you published to Nostr takes it back there too — `docs/goal/ui/nostr.md` § Replying to and quoting a nostr note
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_deleting_a_published_post_takes_it_back_on_nostr`
17. [app] You can link a Nostr key you already have by pasting it, instead of having one made for you — `docs/goal/ui/nostr.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nostr_link_import.py::test_a_pasted_nsec_links_exactly_that_key`
18. [nest] A key held outside your nest, in a browser extension or a remote signer, links only once it has proven it is yours — `docs/goal/ui/nostr.md` § Errors & edge cases
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_browser_extension_key_links_only_once_proven`
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_remote_signer_key_links_only_once_its_bunker_proves_it`
19. [nest] A Nostr key already linked to another account on your nest cannot be linked to yours — `docs/goal/ui/nostr.md` § Errors & edge cases
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_key_another_account_holds_cannot_be_linked_to_yours`
20. [app] Your linked account says in plain words how it signs: with a key your nest made, a key you brought, or an outside signer — `docs/goal/ui/nostr.md` § User actions
   - `tests/e2e-unified/tests/test_nostr_signing_mode.py::test_the_signing_mode_reads_as_its_label`
21. [nest] If you remove every relay, nothing of yours is published anywhere; your nest never falls back to relays you did not choose — `docs/goal/ui/nostr.md` § Errors & edge cases
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_an_empty_relay_list_publishes_nowhere`
22. [nest] Unlinking your Nostr account disconnects every Nostr app that was using your nest as its signer — `docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer (bunker)
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_unlinking_disconnects_every_signer_app`
23. [app] You can always remove a zap signer you named, even when your plan no longer lets you add one — `docs/goal/ui/nostr.md` § Layout & flow
   - `tests/e2e-unified/tests/test_nostr_zap_signer_gate.py::test_a_named_zap_signer_stays_removable_when_zaps_are_turned_off`
24. [app] A private message someone sends you over Nostr shows on your Conversations page as a Nostr conversation you can open and read — `docs/goal/ui/nostr.md` § Implementation status today
   - `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_nostr_gift_wrap_arrives_as_a_nostr_bridged_room`
25. [nest] A Nostr post by someone no account on your nest follows is never taken into a feed, even when a relay sends it — `docs/goal/ui/nostr.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_post_by_someone_nobody_follows_never_reaches_a_feed`
26. [nest] A Nostr deletion removes only events by its own author from your nest's relay; one naming someone else's events removes nothing of theirs — `docs/goal/ui/nostr.md` § The relay event store
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_deletion_removes_only_its_own_authors_events_from_your_nests_relay`
27. [nest] Other Nostr apps can search the posts you share by their words on your nest's relay — `docs/goal/ui/nostr.md` § The relay event store
   - `tests/e2e-unified/tests/api/test_nostr_nest.py::test_other_nostr_apps_search_your_shared_posts_by_their_words`
28. [app] While the signer connect string is shown, the app keeps it out of screen recordings and screenshots as far as the device allows — `docs/goal/architecture/security.md` § On-screen secret exposure
   - `tests/e2e-unified/tests/test_screen_capture_suppression.py::test_a_revealed_signer_connect_string_suppresses_capture_until_left`
29. [nest] A relay that accepts your nest's connection and then never answers, stops reading or never pauses is given up on within a bound and tried again later, so publishing and refresh keep going for everyone on the nest — `docs/goal/architecture/nest/network-exposure.md` § Rulings
   - `tests/e2e-unified/tests/api/test_nostr_silent_relay_bounded.py::test_a_relay_that_never_answers_is_given_up_on_sat_out_and_tried_again_later`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.5-dev+962dc452 standalone |
| linux | ⚠ partial | 0.1.5-dev+962dc452 standalone |
| windows | ⚠ partial | 0.1.2-dev+d6ff0673 standalone |
| macos | ✅ full | 0.1.5-dev+3f9f2b95 standalone |
| ios | ✅ full | 0.1.5-dev+962dc452 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.5-dev+962dc452 standalone |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_nostr_page_renders` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_link_generate_and_unlink_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 1 | app | `tests/e2e-unified/tests/test_nostr.py::test_content_toggles_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_add_remove_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_invalid_url_rejected` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_relay_private_address_rejected` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 2 | app | `tests/e2e-unified/tests/test_nostr.py::test_follow_add_remove_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_section_renders_for_custodial_account` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 3 | app | `tests/e2e-unified/tests/test_nostr_bunker.py::test_bunker_connect_and_disconnect_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_zap_signer_section_renders_with_a_stated_empty_state` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 4 | app | `tests/e2e-unified/tests/test_nostr_zap_signers.py::test_designate_and_undesignate_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_another_nostr_client_reads_your_shared_posts_from_your_nest` | nest (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_turning_sharing_off_takes_posts_off_your_relay_and_on_brings_them_back` | nest (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_your_nostr_address_resolves_to_your_key` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_direct_message_is_accepted_by_your_own_nest` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_nostr_inbound_feed.py::test_posts_from_someone_you_follow_on_nostr_arrive_in_your_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_followed_authors_delete_removes_the_post_from_your_feed` | nest (linux): passed |
| 11 | app | `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_a_reply_to_a_nostr_post_reaches_nostr_threaded_under_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_a_quote_of_a_nostr_post_reaches_nostr_linking_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 12 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_your_reply_threads_under_a_nostr_post_and_your_quote_links_it` | nest (linux): passed |
| 13 | app | `tests/e2e-unified/tests/test_feed_nostr_reply.py::test_replying_or_quoting_with_no_relays_is_refused_naming_the_nostr_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_automatic_publishing_sends_your_new_posts_to_your_relays` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_with_publishing_replies_off_your_replies_stay_off_your_relays` | nest (linux): passed |
| 16 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_deleting_a_published_post_takes_it_back_on_nostr` | nest (linux): passed |
| 17 | app | `tests/e2e-unified/tests/test_nostr_link_import.py::test_a_pasted_nsec_links_exactly_that_key` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_browser_extension_key_links_only_once_proven` | nest (linux): passed |
| 18 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_remote_signer_key_links_only_once_its_bunker_proves_it` | nest (linux): passed |
| 19 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_key_another_account_holds_cannot_be_linked_to_yours` | nest (linux): passed |
| 20 | app | `tests/e2e-unified/tests/test_nostr_signing_mode.py::test_the_signing_mode_reads_as_its_label` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 21 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_an_empty_relay_list_publishes_nowhere` | nest (linux): passed |
| 22 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_unlinking_disconnects_every_signer_app` | nest (linux): passed |
| 23 | app | `tests/e2e-unified/tests/test_nostr_zap_signer_gate.py::test_a_named_zap_signer_stays_removable_when_zaps_are_turned_off` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 24 | app | `tests/e2e-unified/tests/test_bridged_conversation.py::test_a_nostr_gift_wrap_arrives_as_a_nostr_bridged_room` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 25 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_post_by_someone_nobody_follows_never_reaches_a_feed` | nest (linux): passed |
| 26 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_a_deletion_removes_only_its_own_authors_events_from_your_nests_relay` | nest (linux): passed |
| 27 | nest | `tests/e2e-unified/tests/api/test_nostr_nest.py::test_other_nostr_apps_search_your_shared_posts_by_their_words` | nest (linux): passed |
| 28 | app | `tests/e2e-unified/tests/test_screen_capture_suppression.py::test_a_revealed_signer_connect_string_suppresses_capture_until_left` | macos (macos): passed, ios (macos): passed |
| 29 | nest | `tests/e2e-unified/tests/api/test_nostr_silent_relay_bounded.py::test_a_relay_that_never_answers_is_given_up_on_sat_out_and_tried_again_later` | nest (linux): passed |
<!-- features-render:end -->
