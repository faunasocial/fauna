---
slug: get-the-app
title: Get the app
section: getting in
goal: docs/goal/architecture/installers/README.md § The channel contract
guide: docs/guides/install.md § The no-install option: the web app
---

## What a user gets

Every nest serves the web app itself by default, so on any device with a browser
you open your nest's address and you are in — nothing to install. The six native
apps (Linux, Windows, macOS, iOS, Android and the terminal app) are built from
source today, outside Google Play's invitation-only testing of the Android app
and the pre-release downloads of the Windows, Linux and terminal apps; public
releases arrive once a version passes acceptance, and from then on the nest and
every app's release channel that exists ship together at one version.

## Coverage contract

Stamped 2026-09-19 at 0bb8814071.

1. [app] Getting the app the way your platform provides it — served by your nest in the browser, installed on a computer, installed from the store on a phone — ends with the app open and you signed in to your account — `docs/goal/architecture/installers/README.md` § The channel contract
   - `tests/e2e-unified/tests/test_static_serving.py::test_the_nest_served_app_drives_a_login_to_feed_journey`
   - `tests/e2e-unified/tests/platform/docker/test_docker_e2e.py::test_docker_browser_onboarding`
   - `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_the_bundle_drives_a_login_to_feed_journey`
   - `tests/e2e-unified/tests/artifact/test_macos_dmg_install.py::test_the_dmg_installed_app_launches_and_renders`
   - `tests/e2e-unified/tests/platform/windows/test_installer.py::TestFullJourneyInstalledApp::test_installed_app_spawns_the_installers_own_sync_agent`
   - `tests/e2e-unified/tests/platform/windows/test_installer.py::TestFullJourneyInstalledApp::test_a_file_added_to_a_ui_bound_folder_appears_in_media`
   - `tests/e2e-unified/tests/artifact/test_linux_installed_product.py::test_the_installed_app_drives_a_login_to_feed_journey`
   - `tests/e2e-unified/tests/artifact/test_tui_installed_product.py::test_the_installed_app_drives_a_login_to_feed_journey`
   - `tests/e2e-unified/tests/artifact/test_android_store_install.py::test_the_store_installed_app_signs_in_to_the_feed`
   - `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_installed_app_signs_in_to_the_feed`
   - `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_installed_app_signs_in_to_the_feed`
2. [nest] Your nest is a ready-made image you can run anywhere Docker runs — `docs/goal/architecture/installers/docker.md` § Goal
   - `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_image_structure`
   - `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_health`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+3ef7fefd docker |
| linux | ✅ full | 0.1.5-dev+3ef7fefd docker |
| windows | ⚠ partial | 0.1.5-dev+dc4662a8.dirty standalone |
| macos | ✅ full | 0.1.5-dev+3ef7fefd docker |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.5-dev+4f5374ff standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_static_serving.py::test_the_nest_served_app_drives_a_login_to_feed_journey` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/platform/docker/test_docker_e2e.py::test_docker_browser_onboarding` | web (linux): passed |
| 1 | app | `tests/e2e-unified/tests/artifact/test_macos_app_bundle.py::test_the_bundle_drives_a_login_to_feed_journey` | macos (macos): passed |
| 1 | app | `tests/e2e-unified/tests/artifact/test_macos_dmg_install.py::test_the_dmg_installed_app_launches_and_renders` | macos (macos): passed |
| 1 | app | `tests/e2e-unified/tests/platform/windows/test_installer.py::TestFullJourneyInstalledApp::test_installed_app_spawns_the_installers_own_sync_agent` | windows (windows): passed |
| 1 | app | `tests/e2e-unified/tests/platform/windows/test_installer.py::TestFullJourneyInstalledApp::test_a_file_added_to_a_ui_bound_folder_appears_in_media` | windows (windows): failed |
| 1 | app | `tests/e2e-unified/tests/artifact/test_linux_installed_product.py::test_the_installed_app_drives_a_login_to_feed_journey` | linux (linux): passed |
| 1 | app | `tests/e2e-unified/tests/artifact/test_tui_installed_product.py::test_the_installed_app_drives_a_login_to_feed_journey` | tui (linux): passed, tui (macos): passed |
| 1 | app | `tests/e2e-unified/tests/artifact/test_android_store_install.py::test_the_store_installed_app_signs_in_to_the_feed` | — |
| 1 | app | `tests/e2e-unified/tests/artifact/test_ios_store_install.py::test_the_testflight_installed_app_signs_in_to_the_feed` | — |
| 1 | app | `tests/e2e-unified/tests/artifact/test_windows_store_install.py::test_the_store_installed_app_signs_in_to_the_feed` | — |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_image_structure` | nest (linux): passed |
| 2 | nest | `tests/e2e-unified/tests/platform/docker/test_nest.py::test_docker_nest_health` | nest (linux): passed |
<!-- features-render:end -->
