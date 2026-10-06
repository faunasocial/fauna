# Cross-app E2E tests (unified)

One platform-agnostic pytest suite drives all six Fauna apps (web, Linux,
Windows, macOS, iOS, Android) through a shared UI-element vocabulary.

- `ui.yaml` — the canonical UI spec: every element ID, per page, shared by all
  apps. Tests only ever address elements by these IDs.
- `drivers/` — one `PlatformDriver` per client (Playwright for web, AT-SPI for
  Linux, FlaUI for Windows, XCUITest for Apple, UiAutomator for Android).
- `actions/` — platform-agnostic user actions built on the driver interface.
- `tests/` — the pytest suites; `tests/api/` holds API-contract tests that hit
  the nest directly with no UI.
- `conftest.py` — fixtures (`nest_instance`, `app`, `logged_in_app`, …) that
  build and spawn a real `fauna-nest` binary where a test needs one.

## Running

```sh
pytest tests/e2e-unified/tests/ -v                 # everything available here
pytest tests/e2e-unified/tests/ --app web          # one app (auto-deselects the rest)
pytest tests/e2e-unified/tests/ --tier 1,2         # by mocking depth (see below)
```

Which clients can run depends on the host OS: web runs anywhere Playwright +
the built SPA are available; Linux/Windows/macOS/iOS need their native app
built on the matching platform. `--app` restricts and deselects (`--client` is a kept alias); prefer it
over `-k`/`--ignore`.

## Test tiers (mocking depth)

Every test file carries exactly one `tier_N` marker declaring how much of the
stack is real — collection fails if a selected test has none. Fix a missing
marker with `scripts/tag-test-tiers.py` (idempotent, auto-classifies) or add
`pytestmark = pytest.mark.tier_N` at the top of the file.

| Marker | Stack | Meaning |
|---|---|---|
| `tier_1` | in-process | Pure Python/unit; no nest binary, no client driver. |
| `tier_2` | driver + mocks | Real client UI, but at least one backend binary/dep is stubbed (fake bridge, fake cloud, wiremock, state injection). |
| `tier_3` | full local stack | Every binary real and locally built; real wire, real SQLite, real crypto. Includes the API-only tests in `tests/api/`. |
| `tier_4` | Deployment artifact | The real nest Docker image plus its compose sidecars (`tests/platform/docker/`), an installed desktop package under real supervision (`tests/real_session/`), a live-remote deployed box, or a shipped **client artifact** — the macOS `Fauna.app`, a DMG-installed copy, the iOS device `.xcarchive` (`tests/artifact/`, opt-in behind `--macos-artifact`; `just e2e-macos-artifact-test`). Catches packaging/supervision bugs the binaries hide. Slowest, opt-in. |

Choosing a tier for a new test: real Docker image → `tier_4`; real nest binary
→ `tier_3`; real driver but any stubbed backend → `tier_2`; neither driver nor
nest → `tier_1`. Unsure between 3 and 4: if a directly-spawned binary can't
reproduce it (packaging/supervision concern), it's `tier_4`; otherwise `tier_3`.

The separate no-underscore markers `tier0`/`tier1`/`tier2` are a different
axis — feature breadth (smoke / core journey / extended journey). A test can
carry one of each (`pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]`).

## Ground rules

- **Never kill processes by name** (`pkill`/`killall`): the suite may share a
  machine with other work. Start/stop nests via the automation bridge HTTP API
  (`POST /nest/start`, `DELETE /nest?port=N`); fixtures own their subprocesses
  and clean up on exit.
- **Drive mutations through the UI** the way a user would; raw API calls are
  for API-contract tests, fixture setup, and black-box verification only.
- **Every UI element gets its ID from `ui.yaml`** — same IDs on all six
  apps; never invent app-specific IDs in a test.
- **Don't debug with screenshots** — assert on `error-message` text and
  element state so failures diagnose themselves.
