---
slug: region-rules
title: region-rules
section: family and personalization
goal: docs/goal/behavior/region-blocking.md § The content plane
guide: docs/guides/app-tour.md § Settings
---

## What a user gets

This text is being written and will appear here once it has been reviewed.

## Coverage contract

Stamped 2026-11-07 at ef4db96e39.

1. [app] A post in your feed that your region's rules block is replaced, on that card alone, by a notice saying it is not shown in your region, naming the authority and giving the authority's reason in its own words, and nothing of the post's body shows — `docs/goal/behavior/region-blocking.md` § The blocked render and the transparency surface
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
2. [app] A post a rule only collapses shows the notice and the reason with a reveal, and choosing it shows the post — `docs/goal/behavior/region-blocking.md` § The blocked render and the transparency surface
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
3. [app] A post opened in its full view that a rule blocks shows the notice, with the authority and its reason, in place of the body — `docs/goal/behavior/region-blocking.md` § Where it composes — the render seam
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_a_region_withheld_post_opened_in_full_shows_the_notice`
4. [app] A message in a conversation that a rule covers is replaced by the same notice with the reason in place of its text, and the other messages read as usual — `docs/goal/behavior/region-blocking.md` § Where it composes — the render seam
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
5. [app] On an end-to-end encrypted conversation the notice also says the item is hidden from you on this device and that nothing was removed, detected or reported — `docs/goal/behavior/region-blocking.md` § The policy document
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
6. [app] Settings shows your device's region and where that came from, each policy in force with its authority and version, and when it was last checked — `docs/goal/behavior/region-blocking.md` § The blocked render and the transparency surface
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
7. [app] When the app has not been able to check for updated rules for a while, Settings warns, and the rules it last saw stay in force — `docs/goal/behavior/region-blocking.md` § Fail posture
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_settings_warns_when_the_rules_could_not_be_checked_lately`
8. [nest] The nest's public web pages show the same notice, with the authority's reason, in place of a post that the rules of the region its admin declares block — `docs/goal/behavior/region-blocking.md` § The nest-as-publisher leg
   - `tests/e2e-unified/tests/api/test_region_rules_web_notice.py::test_the_public_web_pages_show_the_region_notice_in_place_of_what_the_situs_policy_blocks`
9. [app] The rules stay in force on your device across a restart, even when your nest no longer carries them — `docs/goal/behavior/region-blocking.md` § How an app obtains its region's policy
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`
10. [app] Rules published in a format your app does not understand are listed in Settings as not applied, and block nothing — `docs/goal/behavior/region-blocking.md` § The policy document
   - `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux |  no run recorded | |
| windows |  no run recorded | |
| macos |  no run recorded | |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.6-dev+938600f7.dirty standalone |
| android-kids |  no run recorded | |
| ios-kids |  no run recorded | |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 2 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 3 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_a_region_withheld_post_opened_in_full_shows_the_notice` | tui (macos): passed |
| 4 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 5 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 6 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 7 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_settings_warns_when_the_rules_could_not_be_checked_lately` | tui (macos): passed |
| 8 | nest | `tests/e2e-unified/tests/api/test_region_rules_web_notice.py::test_the_public_web_pages_show_the_region_notice_in_place_of_what_the_situs_policy_blocks` | nest (linux): passed |
| 9 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
| 10 | app | `tests/e2e-unified/tests/test_region_content_policy.py::test_region_policy_reaches_the_render_and_the_settings_surface` | tui (macos): passed |
<!-- features-render:end -->
