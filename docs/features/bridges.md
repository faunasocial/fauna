---
slug: bridges
title: Link other networks
section: bridges and other networks
goal: docs/goal/behavior/bridges.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § One feed, many networks
---

## What a user gets

The Bridges page links your account to other networks and unlinks it again. A
bridge that cannot be linked right now says why instead of offering a dead button.
Once a bridge is on, you can subscribe a feed of yours to a source on that network,
and only the bridges your nest can actually serve are offered.

## Coverage contract

Stamped 2026-10-01 at 065ed2e2c2.

1. [app] A network links and unlinks from its card, and a bridge that cannot be linked shows the nest's own reason — `docs/goal/behavior/bridges.md` § Link modes
   - `tests/e2e-unified/tests/test_bridges.py::test_activitypub_links_through_the_bridges_page`
   - `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_shows_the_nests_own_reason`
   - `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_falls_back_to_the_generic_reason`
2. [app] A feed subscribes to a source on a linked network and unsubscribes again; with no bridge on, the option is not offered — `docs/goal/behavior/bridges.md` § Layout & flow
   - `tests/e2e-unified/tests/test_feed_bridge_subscribe.py::test_bridge_subscribe_and_unsubscribe_round_trip`
   - `tests/e2e-unified/tests/test_feed_bridge_selector_gating.py::test_bridge_subscribe_toggle_hidden_when_nest_supports_no_bridges`
3. [nest] Your nest lists the bridges it can serve and how each links — `docs/goal/behavior/bridges.md` § State & data shape
   - `tests/e2e-unified/tests/api/test_bluesky_oauth.py::test_bluesky_listed_with_oauth_link_mode`
   - `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_bridge_api_shows_activitypub`
   - `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_lists_the_bridges_it_compiled_in`
4. [nest] A bridge the deployment cannot serve yet is offered as unavailable with the nest's own reason, and becomes available once the deployment meets its precondition — `docs/goal/behavior/bridges.md` § Errors & edge cases
   - `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_offers_the_bluesky_bridge`
   - `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_domainless_shipped_image_refuses_bluesky_with_a_reason`
5. [app] A linked network shows its own options on its card, and a change you make there is kept without a save button — `docs/goal/behavior/bridges.md` § Bridge settings
   - `tests/e2e-unified/tests/test_bridges.py::test_a_card_setting_saves_on_change`
6. [app] Every linked network's card lets you choose whether its posts show in search and cap how many do — `docs/goal/behavior/bridges.md` § Bridge settings
   - `tests/e2e-unified/tests/test_bridges.py::test_the_search_toggle_and_cap_persist`
7. [app] On a linked network that supports follows, you follow an account there by its address, with an optional nickname, and unfollow it from the card — `docs/goal/behavior/bridges.md` § Follows
   - `tests/e2e-unified/tests/test_bridges.py::test_a_follow_keeps_its_nickname`
8. [app] Once a network is linked, its card shows your identity on that network — `docs/goal/behavior/bridges.md` § Link modes
   - `tests/e2e-unified/tests/test_bridges.py::test_the_card_shows_the_linked_identity`
9. [app] An admin installs a plugin from its document's address, reviews what would run on the nest on its install card before approving, sees it listed as running, and uninstalls it — `docs/goal/architecture/third-party.md` § The runner contract
   - `tests/e2e-unified/tests/test_plugin_install.py::test_an_admin_installs_a_plugin_from_its_card_and_uninstalls_it`
10. [app] A user connects a plugin their admin installed from its card on the Bridges page, approves it in Connected apps, sees it connected there with a link back to its card, and disconnects it while it stays installed — `docs/goal/architecture/third-party.md` § The principal model
    - `tests/e2e-unified/tests/test_plugin_install.py::test_a_user_connects_a_plugin_from_its_settings_card_and_disconnects_it`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+8b423269 standalone |
| ios | ⚠ partial | 0.1.2-dev+c438a386 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_links_through_the_bridges_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_shows_the_nests_own_reason` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_falls_back_to_the_generic_reason` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_bridge_subscribe.py::test_bridge_subscribe_and_unsubscribe_round_trip` | web (linux): failed, linux (linux): passed, windows (windows): failed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_bridge_selector_gating.py::test_bridge_subscribe_toggle_hidden_when_nest_supports_no_bridges` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_bluesky_oauth.py::test_bluesky_listed_with_oauth_link_mode` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_bridge_api_shows_activitypub` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_lists_the_bridges_it_compiled_in` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_offers_the_bluesky_bridge` | nest (linux): failed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_domainless_shipped_image_refuses_bluesky_with_a_reason` | nest (linux): failed |
| 5 | app | `tests/e2e-unified/tests/test_bridges.py::test_a_card_setting_saves_on_change` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_bridges.py::test_the_search_toggle_and_cap_persist` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_bridges.py::test_a_follow_keeps_its_nickname` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_bridges.py::test_the_card_shows_the_linked_identity` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_an_admin_installs_a_plugin_from_its_card_and_uninstalls_it` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 10 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_user_connects_a_plugin_from_its_settings_card_and_disconnects_it` | web (linux): passed, tui (linux): passed |
<!-- features-render:end -->
