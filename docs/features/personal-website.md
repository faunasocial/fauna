---
slug: personal-website
title: Publish to the web
section: your data and devices
goal: docs/goal/behavior/web-content-hosting.md § Goal
guide: docs/guides/who-can-see-what.md § Public posts and your profile
---

## What a user gets

Publish a post to the open web from its menu and copy a link that loads for
anyone; unpublish it from Settings. Opt into your own subdomain and your published
posts serve there. A paywalled post gets a link that unlocks the full text for the
holder; a nest with no serving domain says so instead of handing you a dead link.

## Coverage contract

Stamped 2026-10-01 at 308c995169.

1. [app] A post publishes from its menu, the copied link loads, and it unpublishes from Settings — `docs/goal/behavior/web-content-hosting.md` § Published-post management
   - `tests/e2e-unified/tests/test_web_authoring.py::test_publishing_a_post_from_the_feed_overflow_menu`
   - `tests/e2e-unified/tests/test_web_authoring.py::test_published_post_management_section`
   - `tests/e2e-unified/tests/test_web_authoring.py::test_a_copied_web_link_actually_serves`
2. [app] A paywalled post's copied link unlocks the full text — `docs/goal/behavior/web-content-hosting.md` § Paywalled serving
   - `tests/e2e-unified/tests/test_web_authoring.py::test_a_copied_paywall_link_serves_the_full_body`
3. [app] The subdomain switch opts you in and out, and the page names the address your nest actually serves — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/test_web_authoring.py::test_user_subdomain_toggle_round_trip`
   - `tests/e2e-unified/tests/test_web_authoring.py::test_the_subdomain_url_row_names_the_host_the_nest_serves_on`
4. [app] With no serving domain, the copy-link action is disabled and says why — `docs/goal/behavior/web-content-hosting.md` § Published-post management
   - `tests/e2e-unified/tests/test_web_authoring.py::test_the_overflow_copy_verb_is_dead_without_a_serving_origin`
5. [nest] Your nest serves your subdomain, lists your published posts to you alone, and refuses a stranger publishing your post — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_serves_via_host_header`
   - `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_round_trips`
   - `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_feed_projects_web_slug_for_published_posts_only`
   - `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_publish_list_is_caller_scoped`
   - `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_a_stranger_republish_is_refused_and_the_authors_web_slug_is_unaffected`
   - `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_is_per_actor`
   - `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_is_user_class`
   - `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_publish_list_joins_the_gated_tier`
6. [app] When your nest has taken your published pages offline after a failed rebuild, the page says so, and stops saying so once they are back — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/test_web_authoring.py::test_a_site_the_nest_took_dark_tells_its_author`
7. [nest] A page you unpublish or delete stops loading for visitors, and stays gone even if your nest restarts before it has finished taking it down — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_web_revoke_durability.py::test_an_unpublished_or_deleted_page_stays_gone_across_a_restart`
8. [nest] Published pages your nest took offline after a failed rebuild come back by themselves, with nothing for you to do — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_web_revoke_durability.py::test_a_blanked_site_comes_back_at_the_next_boot`
9. [nest] A domain of your own, once it points at your nest and is verified, serves your site over HTTPS, and stops serving it the moment you withdraw the domain — `docs/goal/behavior/web-content-hosting.md` § Custom domains and TLS
   - `tests/e2e-unified/tests/api/test_web_host_routing.py::test_a_verified_custom_domain_serves_the_site_and_stops_when_withdrawn`
10. [nest] Publishing posts is enough to have a site: a front page that lists them and a feed people can subscribe to, with no template written — `docs/goal/behavior/web-content-hosting.md` § Routing, render, serving
   - `tests/e2e-unified/tests/api/test_web_host_routing.py::test_publishing_alone_gives_a_front_page_and_a_feed`
11. [nest] Nobody's published site can answer at the addresses where the app, your nest's own services or its mail live — `docs/goal/behavior/web-content-hosting.md` § Same-origin security model
   - `tests/e2e-unified/tests/api/test_web_host_routing.py::test_no_user_site_answers_at_the_reserved_hosts_and_paths`
   - `tests/e2e-unified/tests/platform/docker/test_spa_shadow_by_user_site.py::test_a_user_site_never_shadows_the_mounted_spa_at_app`
12. [nest] A script on someone's published site can never take control of pages beyond that site's own address — `docs/goal/behavior/web-content-hosting.md` § Invariants this model relies on
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+72d6a508 standalone |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+c04c2468 standalone |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_publishing_a_post_from_the_feed_overflow_menu` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_published_post_management_section` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 1 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_a_copied_web_link_actually_serves` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_a_copied_paywall_link_serves_the_full_body` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_user_subdomain_toggle_round_trip` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 3 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_the_subdomain_url_row_names_the_host_the_nest_serves_on` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 4 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_the_overflow_copy_verb_is_dead_without_a_serving_origin` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, ios (macos): passed, tui (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_serves_via_host_header` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_round_trips` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_feed_projects_web_slug_for_published_posts_only` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_publish_list_is_caller_scoped` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_a_stranger_republish_is_refused_and_the_authors_web_slug_is_unaffected` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_is_per_actor` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_subdomain_hosting.py::test_subdomain_opt_in_is_user_class` | nest (linux): passed |
| 5 | nest | `tests/e2e-unified/tests/api/test_web_publish_projections.py::test_publish_list_joins_the_gated_tier` | nest (linux): passed |
| 6 | app | `tests/e2e-unified/tests/test_web_authoring.py::test_a_site_the_nest_took_dark_tells_its_author` | web (linux): passed, linux (linux): passed, tui (linux): passed |
| 7 | nest | `tests/e2e-unified/tests/api/test_web_revoke_durability.py::test_an_unpublished_or_deleted_page_stays_gone_across_a_restart` | nest (linux): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_web_revoke_durability.py::test_a_blanked_site_comes_back_at_the_next_boot` | nest (linux): passed |
| 9 | nest | `tests/e2e-unified/tests/api/test_web_host_routing.py::test_a_verified_custom_domain_serves_the_site_and_stops_when_withdrawn` | nest (linux): passed |
| 10 | nest | `tests/e2e-unified/tests/api/test_web_host_routing.py::test_publishing_alone_gives_a_front_page_and_a_feed` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/api/test_web_host_routing.py::test_no_user_site_answers_at_the_reserved_hosts_and_paths` | nest (linux): passed |
| 11 | nest | `tests/e2e-unified/tests/platform/docker/test_spa_shadow_by_user_site.py::test_a_user_site_never_shadows_the_mounted_spa_at_app` | nest (linux): passed |
| 12 | nest | (none) | — |
<!-- features-render:end -->
