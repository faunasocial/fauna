---
slug: nest-serves-the-app
title: Your nest serves the app
section: your nest
goal: docs/goal/behavior/web-content-hosting.md § Serving the SPA is artifact wiring
guide: docs/guides/install.md § The no-install option: the web app
---

## What a user gets

There is nothing to choose to get this: the released nest image carries the web app and serves
it at its own address, on every start, with the security headers a browser expects.
A nest whose saved settings never recorded where the app is finds it again on the
next restart. An admin who would rather send visitors to the central app picks
that in the admin area.

## Coverage contract

Stamped 2026-10-01 at dc4b94e0f1.

1. [nest] The app is served at the nest's address, distinct from the info page, with its scripts typed correctly and its security headers present — `docs/goal/behavior/web-content-hosting.md` § Serving the SPA is artifact wiring
   - `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_app_path_serves_the_spa_not_the_info_page`
   - `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_app_and_root_are_not_the_same_page`
   - `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_spa_origin_security_headers_ride_the_real_mount`
   - `tests/e2e-unified/tests/test_static_serving.py::test_spa_js_modules_have_correct_content_type`
2. [nest] A nest whose saved settings do not say where the app is — one set up before that was recorded — finds it again on the next restart and serves the app — `docs/goal/behavior/web-content-hosting.md` § Serving the SPA is artifact wiring
   - `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_a_box_with_no_static_dir_self_heals_on_restart`
3. [nest] A nest that carries no copy of the app answers its app address as not found, never with its information page or anyone's website — `docs/goal/behavior/web-content-hosting.md` § Serving the SPA is artifact wiring
   - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back`
4. [nest] The app address refuses to be framed by another site whatever it answers, including when it sends you on to the central app — `docs/goal/behavior/web-content-hosting.md` § Same-origin security model
   - `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back`
5. [nest] The app page your nest serves runs only its own scripts, with no inline or outside scripts and no plugins — `docs/goal/behavior/web-content-hosting.md` § Implementation status today
   - (none)
6. [nest] Your nest never gives your browser a sign-in cookie, from the app or from any site it serves — `docs/goal/behavior/web-content-hosting.md` § Invariants this model relies on
   - (none)
7. [nest] A request for one of the app's script or style files that the nest does not have is answered as not found, never with the app's page — `docs/goal/behavior/web-content-hosting.md` § The nest-served `/app/` and the central origin
   - (none)
8. [app] Arriving at the app from a nest that sends visitors to the central app, the sign-in page already names that nest, and a link naming anything that is not a nest address is quietly ignored — `docs/goal/behavior/onboarding.md` § 2. Handle entry (`handle_entry`)
   - `tests/e2e-unified/tests/test_onboarding_nest_hint.py::test_the_nest_hint_prefills_the_domain_part_and_the_check_still_runs`
   - `tests/e2e-unified/tests/test_onboarding_nest_hint.py::test_an_unclassifiable_nest_hint_is_dropped_silently`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| linux | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| windows | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| macos | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| ios | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| android | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |
| tui | ⚠ partial | 0.1.2-dev+b3cb40c5 docker |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_app_path_serves_the_spa_not_the_info_page` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_app_and_root_are_not_the_same_page` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_spa_origin_security_headers_ride_the_real_mount` | nest (linux): passed |
| 1 | nest | `tests/e2e-unified/tests/test_static_serving.py::test_spa_js_modules_have_correct_content_type` | nest (linux): failed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_spa_serving.py::test_a_box_with_no_static_dir_self_heals_on_restart` | nest (linux): passed |
| 3 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back` | nest (linux): passed |
| 4 | nest | `tests/e2e-unified/tests/api/test_web_app_origin.py::test_the_admin_choice_flips_app_to_the_central_origin_and_back` | nest (linux): passed |
| 5 | nest | (none) | — |
| 6 | nest | (none) | — |
| 7 | nest | (none) | — |
| 8 | app | `tests/e2e-unified/tests/test_onboarding_nest_hint.py::test_the_nest_hint_prefills_the_domain_part_and_the_check_still_runs` | web (linux): passed |
| 8 | app | `tests/e2e-unified/tests/test_onboarding_nest_hint.py::test_an_unclassifiable_nest_hint_is_dropped_silently` | web (linux): passed |
<!-- features-render:end -->
