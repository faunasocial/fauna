# Apple e2e baselines

Written by the `--baseline-json` pytest hook and consumed by `e2e-apple-baseline` — the
interim apple regression gate until test CI exists. See
`docs/goal/architecture/apps/apple-e2e-automation.md` § Baseline discipline.

`apple-baseline.json` is checked in — it's the last-known-good outcome of every apple e2e
test, refreshed by whoever last ran the recipe and judged the delta "understood" (expected
apple-gated reds, a real regression chased down, etc.). It is not a source of truth for
anything except "what did we last see" — the code and the tests remain the actual truth.

## Schema

```json
{
  "head_sha": "<git rev-parse HEAD at generation time>",
  "generated_at": "<ISO-8601 UTC timestamp>",
  "clients": {
    "macos": {
      "duration_s": 123.4,
      "counts": {"passed": 500, "failed": 3, "skipped": 40, "xfailed": 2, "error": 0},
      "tests": {
        "tests/test_foo.py::test_bar": {"outcome": "passed", "duration": 0.52}
      }
    },
    "ios": { "...": "same shape" }
  }
}
```

`outcome` is one of `passed`, `failed`, `skipped`, `xfailed`, `xpassed`, `error`.

The per-app `tests` map is produced by a pytest plugin hook in `conftest.py`
(`pytest_addoption`'s `--baseline-json`, `pytest_runtest_logreport`,
`pytest_sessionfinish`) — it's a generic "dump per-test outcome+duration to JSON" hook,
usable standalone (`pytest ... --baseline-json=/tmp/out.json`) independent of the apple
baseline script.

## Delta semantics

A test's outcome bucket: `PASS_LIKE = {passed, xpassed}`, `FAIL_LIKE = {failed, error}`,
`SKIP_LIKE = {skipped, xfailed}`.

- **new red** — was PASS_LIKE, now FAIL_LIKE. The regression signal; the script exits 1 if
  any client has one.
- **new green** — was FAIL_LIKE, now PASS_LIKE.
- **new skip** — wasn't SKIP_LIKE, now is.
- **added / removed** — the node id is new to / gone from the suite.

This is the format a future CI gate would consume directly — don't fork it; extend it in
place if CI needs more (e.g. a `git log` link, a flakiness counter).

## `windows-app-census.txt` — the previous pass's windows census

Not an apple baseline: the last pass's windows app census, one line per selected test FILE —
its windows-selected item count, its tiers, and the app-launching fixtures its REAL fixture
closure reaches (`helpers/fixture_closure.py`, never fixture names). It exists so the windows
app-gate drain's first step, "diff the census against the previous pass", is runnable:
the next pass regenerates the table over this file and reads `git diff`. A new line is a new
file; a changed count is item growth inside an existing file (which a new-file scan cannot
see); a changed fixture column is a changed closure. The regenerated file is committed with the
pass that swept what the diff showed.


