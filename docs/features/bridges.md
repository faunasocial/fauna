---
slug: bridges
title: Link other networks
section: bridges and other networks
goal: docs/goal/behavior/bridges.md § Goal
guide: docs/guides/bridges-bluesky-nostr.md § One feed, many networks
absences:
  android-kids: "docs/goal/behavior/account-age-band.md § The account age band"
  ios-kids: "docs/goal/behavior/account-age-band.md § The account age band"
---

## What a user gets

The Bridges page links your account to other networks and unlinks it again. A
bridge that cannot be linked right now says why instead of offering a dead button.
Once a bridge is on, you can subscribe a feed of yours to a source on that network,
and only the bridges your nest can actually serve are offered.

## Coverage contract

Stamped 2026-11-07 at ac836c937b.

1. [app] A network links and unlinks from its card, and a bridge that cannot be linked shows the nest's own reason, or a generic one when the nest gave none — `docs/goal/behavior/bridges.md` § Link modes
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
11. [app] A bridge app you connected appears on the Bridges page as a network row with its own glyph — `docs/goal/architecture/third-party.md` § Implementation status today
   - `tests/e2e-unified/tests/test_plugin_install.py::test_a_connected_bridge_app_is_listed_as_a_network_row_with_its_glyph`
12. [app] A connected plugin's card shows its settings for your account, and an edit you make there is kept — `docs/goal/architecture/third-party.md` § Implementation status today
   - `tests/e2e-unified/tests/test_plugin_install.py::test_a_bound_plugins_card_keeps_a_setting_edit`
13. [app] A plugin that faulted or failed to start is listed as restarting or stopped, with its reason — `docs/goal/architecture/third-party.md` § Implementation status today
   - `tests/e2e-unified/tests/test_plugin_install.py::test_a_faulted_plugin_shows_its_fault_count_and_last_fault`
   - `tests/e2e-unified/tests/test_plugin_install.py::test_a_plugin_that_refused_to_start_is_listed_stopped_with_its_reason`
14. [nest] A plugin that declares a web path or sub-name on the nest answers requests there, and the nest's own routes and users' sites are untouched — `docs/goal/architecture/third-party.md` § Implementation status today
   - `tests/e2e-unified/tests/api/test_plugin_ingress.py::test_a_plugins_declared_path_and_sub_name_answer_and_the_nests_own_stay_put`
15. [nest] A private message that reached you over a linked network never enters your nest's search; only what that network published openly can be found there — `docs/goal/behavior/content-index.md` § Bridge content in the Search corpus
   - `tests/e2e-unified/tests/api/test_bridge_private_content_not_searchable.py::test_a_private_bridge_message_never_enters_search_and_a_public_one_does`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+5f525618 docker |
| linux | ✅ full | 0.1.5-dev+5f525618 docker |
| windows | ⚠ partial | 0.1.5-dev+5f525618 docker |
| macos | ⚠ partial | 0.1.5-dev+5f525618 docker |
| ios | ⚠ partial | 0.1.5-dev+5f525618 docker |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+5f525618 docker |
| android-kids | — absent | |
| ios-kids | — absent | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_links_through_the_bridges_page` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_shows_the_nests_own_reason` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_bridges.py::test_activitypub_link_blocked_falls_back_to_the_generic_reason` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_bridge_subscribe.py::test_bridge_subscribe_and_unsubscribe_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_feed_bridge_selector_gating.py::test_bridge_subscribe_toggle_hidden_when_nest_supports_no_bridges` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_bluesky_oauth.py::test_bluesky_listed_with_oauth_link_mode` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_activitypub_federation.py::TestActivityPubFederation::test_bridge_api_shows_activitypub` | nest (linux): passed, nest (macos): passed |
| 3 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_lists_the_bridges_it_compiled_in` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_shipped_image_offers_the_bluesky_bridge` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/platform/docker/test_bridge_availability_in_image.py::test_domainless_shipped_image_refuses_bluesky_with_a_reason` | nest (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_bridges.py::test_a_card_setting_saves_on_change` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_bridges.py::test_the_search_toggle_and_cap_persist` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 7 | app | `tests/e2e-unified/tests/test_bridges.py::test_a_follow_keeps_its_nickname` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_bridges.py::test_the_card_shows_the_linked_identity` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_an_admin_installs_a_plugin_from_its_card_and_uninstalls_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 10 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_user_connects_a_plugin_from_its_settings_card_and_disconnects_it` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (macos): passed, tui (windows): passed |
| 11 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_connected_bridge_app_is_listed_as_a_network_row_with_its_glyph` | web (linux): passed, linux (linux): passed, macos (macos): failed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 12 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_bound_plugins_card_keeps_a_setting_edit` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 13 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_faulted_plugin_shows_its_fault_count_and_last_fault` | web (linux): passed, linux (linux): passed, macos (macos): failed, ios (macos): failed, tui (linux): passed, tui (windows): passed |
| 13 | app | `tests/e2e-unified/tests/test_plugin_install.py::test_a_plugin_that_refused_to_start_is_listed_stopped_with_its_reason` | web (linux): passed, linux (linux): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed, tui (windows): passed |
| 14 | nest | `tests/e2e-unified/tests/api/test_plugin_ingress.py::test_a_plugins_declared_path_and_sub_name_answer_and_the_nests_own_stay_put` | nest (linux): passed |
| 15 | nest | `tests/e2e-unified/tests/api/test_bridge_private_content_not_searchable.py::test_a_private_bridge_message_never_enters_search_and_a_public_one_does` | nest (linux): passed |
<!-- features-render:end -->
