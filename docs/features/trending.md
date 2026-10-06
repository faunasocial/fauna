---
slug: trending
title: See what is trending
section: everyday
goal: docs/goal/behavior/trending.md § The Trending feed
guide: docs/guides/app-tour.md § Feeds
---

## What a user gets

A built-in Trending feed shows public posts that are gathering attention, on
your nest and on the nests it federates with. Staying on Trending survives the feed
refreshing.

## Coverage contract

Stamped 2026-09-19 at 23b3189e2b.

1. [app] Choosing Trending shows public posts, and a refresh keeps you on Trending — `docs/goal/behavior/trending.md` § The Trending feed
   - `tests/e2e-unified/tests/test_feed.py::test_trending_feed_selection`
   - `tests/e2e-unified/tests/test_feed.py::test_trending_selection_survives_a_feed_re_pull`
2. [nest] A post trending on a federated nest reaches yours — `docs/goal/behavior/trending.md` § Federation exchange
   - `tests/e2e-unified/tests/api/test_trending_federation.py::test_trending_cycle_fetches_peer_only_post_and_surfaces_it`
3. [nest] A public post people here engage with right now rises into Trending, and falls back out when that engagement is withdrawn — `docs/goal/behavior/trending.md` § Local velocity
   - `tests/e2e-unified/tests/api/test_trending_federation.py::test_trending_cycle_fetches_peer_only_post_and_surfaces_it`
4. [app] Trending is offered like any other factor, so you can weight it into a feed of your own or into every feed — `docs/goal/behavior/trending.md` § The Trending feed
   - `tests/e2e-unified/tests/test_feed_creation.py::test_trending_is_offered_as_a_factor_for_one_feed_or_every_feed`
5. [app] Your own feed factors still apply on Trending — a post your global factors or muted words sink stays sunk there — `docs/goal/behavior/trending.md` § The Trending feed
   - `tests/e2e-unified/tests/test_feed.py::test_a_global_factor_still_sinks_a_post_on_trending`
   - `tests/e2e-unified/tests/test_feed.py::test_a_muted_word_still_sinks_a_post_on_trending`
6. [nest] Your nest tells other nests a post is trending only once several different people here engaged with it, and never for a restricted post — `docs/goal/behavior/trending.md` § Federation exchange
   - `tests/e2e-unified/tests/api/test_trending_federation.py::test_export_withholds_below_k_and_restricted_posts`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.2-dev+72d6a508 standalone |
| linux | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| windows | ✅ full | 0.1.2-dev+301434f2.dirty standalone |
| macos | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| ios | ✅ full | 0.1.2-dev+a1f84cc6 standalone |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_trending_feed_selection` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_feed.py::test_trending_selection_survives_a_feed_re_pull` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): failed |
| 2 | nest | `tests/e2e-unified/tests/api/test_trending_federation.py::test_trending_cycle_fetches_peer_only_post_and_surfaces_it` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_trending_federation.py::test_trending_cycle_fetches_peer_only_post_and_surfaces_it` | nest (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_feed_creation.py::test_trending_is_offered_as_a_factor_for_one_feed_or_every_feed` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed.py::test_a_global_factor_still_sinks_a_post_on_trending` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | app | `tests/e2e-unified/tests/test_feed.py::test_a_muted_word_still_sinks_a_post_on_trending` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 6 | nest | `tests/e2e-unified/tests/api/test_trending_federation.py::test_export_withholds_below_k_and_restricted_posts` | nest (linux): passed |
<!-- features-render:end -->
