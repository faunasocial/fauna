---
slug: app-version-and-updates
title: Your app's version, and news of a newer one
section: getting in
goal: docs/goal/architecture/product-version.md § The model (ratified 2026-08-23)
guide: docs/guides/install.md § What "alpha" means for you
absences:
  web (outcomes 2, 3, 4): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
  ios (outcomes 2, 3, 4): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
  android (outcomes 2, 3, 4): "docs/goal/architecture/installers/README.md § Knowing a newer version is out"
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
3. [app] You can always ask, and the app also looks once when you sign in and only tells you: it never checks for a newer version on a timer — `docs/goal/architecture/installers/README.md` § Knowing a newer version is out
   - `tests/e2e-unified/tests/test_app_version_and_updates.py::test_signing_in_looks_once_and_only_tells_you`
4. [app] The app never downloads or installs a newer version by itself; it only tells you one is out — `docs/goal/architecture/installers/README.md` § Knowing a newer version is out
   - (none)

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | ✅ full | 0.1.5-dev+110d1248 standalone |
| linux | ⚠ partial | 0.1.4-dev+ea391d07 standalone |
| windows | ⚠ partial | 0.1.3-dev+74556739 standalone |
| macos | ⚠ partial | 0.1.3-dev+1cf3db28 standalone |
| ios |  no run recorded | |
| android |  no run recorded | |
| tui | ⚠ partial | 0.1.3-dev+e5c92300.dirty standalone |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_app_version_and_updates.py::test_settings_shows_the_version_you_are_running` | web (linux): passed, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | `tests/e2e-unified/tests/test_app_version_and_updates.py::test_asking_for_a_newer_version_says_so_and_where_to_get_it` | web (linux): skipped, linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
| 2 | app | absent by design on web, ios, android | — |
| 3 | app | `tests/e2e-unified/tests/test_app_version_and_updates.py::test_signing_in_looks_once_and_only_tells_you` | web (linux): skipped |
| 3 | app | absent by design on web, ios, android | — |
| 4 | app | (none) | — |
| 4 | app | absent by design on web, ios, android | — |
<!-- features-render:end -->
