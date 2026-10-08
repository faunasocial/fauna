---
slug: app-version-and-updates
title: Your app's version, and news of a newer one
section: getting in
goal: docs/goal/architecture/product-version.md § The model (ratified 2026-08-23)
guide: docs/guides/install.md § What "alpha" means for you
absences:
  web (outcome 2): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
  ios (outcome 2): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
  android (outcome 2): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
---

## What a user gets

Settings shows which version of Fauna you are running — one version number,
the same on every app, so "what version are you on?" always has one answer.
On a computer, where you install the app yourself, you can also ask it
whether a newer version is out, and when one is, it says so and where to get
it. The web app is always the version your nest serves, and the phone apps
are kept current by their stores, so there is nothing to ask there.

## Coverage contract

Stamped 2026-09-26 at dc54d3d61e.

1. [app] Settings shows the version of the app you are running, as the one version number every Fauna app and your nest share — `docs/goal/architecture/product-version.md` § The model (ratified 2026-08-23)
   - `tests/e2e-unified/tests/test_app_version_and_updates.py::test_settings_shows_the_version_you_are_running`
2. [app] You can ask the app whether a newer version is out, and when one is, it says so and where to get it — `docs/goal/architecture/installers/README.md` § Knowing a newer version is out
   - `tests/e2e-unified/tests/test_app_version_and_updates.py::test_asking_for_a_newer_version_says_so_and_where_to_get_it`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web |  no run recorded | |
| linux | ✅ full | 0.1.2-dev+78e73031 standalone |
| windows | ❌ failing | 0.1.3-dev+862c7d57 standalone |
| macos | ✅ full | 0.1.3-dev+1cf3db28 standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ✅ full | 0.1.2-dev+ef35c9ce.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_app_version_and_updates.py::test_settings_shows_the_version_you_are_running` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_app_version_and_updates.py::test_asking_for_a_newer_version_says_so_and_where_to_get_it` | linux (linux): passed, windows (windows): skipped, macos (macos): passed, tui (linux): passed |
| 2 | app | absent by design on web, ios, android | — |
<!-- features-render:end -->
