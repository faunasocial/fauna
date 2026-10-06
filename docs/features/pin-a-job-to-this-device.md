---
slug: pin-a-job-to-this-device
title: Run a job on this device
section: your data and devices
goal: docs/goal/behavior/participants.md § The assignment picker
guide: docs/guides/app-tour.md § Settings
absences:
  web: "docs/goal/behavior/participants.md § The assignment picker"
  ios: "docs/goal/behavior/participants.md § The assignment picker"
  android: "docs/goal/behavior/participants.md § The assignment picker"
---

## What a user gets

On the Task delegation page, a job this computer can run offers *This device*
beside the automatic choice. Choose it and the job stays here: the pin is
remembered across restarts and on every device you own, and clearing it hands
the job back to automatic. Phones, tablets and the web app run no heavy jobs,
so they never offer it; on them the page still shows which device is doing
each job.

## Coverage contract

Stamped 2026-09-26 at ef1315baff.

1. [app] Pinning a job to this device persists, and clearing it returns to automatic — `docs/goal/behavior/participants.md` § The assignment picker
   - `tests/e2e-unified/tests/test_task_delegation.py::test_pinning_the_kind_this_app_runs_to_this_device_persists`

## Status

<!-- features-render:begin -->
| App | Status | Stamp |
|---|---|---|
| web | — absent | |
| linux | ✅ full | 0.1.2-dev+c4a95c20 standalone |
| windows | ✅ full | 0.1.2-dev+0d1e684d standalone |
| macos | ✅ full | 0.1.2-dev+f85ee000 standalone |
| ios | — absent | |
| android | — absent | |
| tui | ✅ full | 0.1.2-dev+831d48ef live |

| Outcome | Surface | Witness | Newest outcome |
|---|---|---|---|
| 1 | app | `tests/e2e-unified/tests/test_task_delegation.py::test_pinning_the_kind_this_app_runs_to_this_device_persists` | linux (linux): passed, windows (windows): passed, macos (macos): passed, tui (linux): passed |
<!-- features-render:end -->
