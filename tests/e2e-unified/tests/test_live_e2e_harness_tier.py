"""tier_4 (live-remote, OPT-IN, ADDITIVE): define the ``e2e-harness`` tier on a live
box through the admin Tiers page — the app UI only, no nest API.

Why this exists (user ruling 2026-10-04: *"create the tier, but do it using tui
admin UI only — no api use"*): a live run's account keeps the box's ``free`` caps
unless the box's admin has defined a tier named ``e2e-harness``, which the run's
account then moves itself onto (``testing.md`` § Default app and nest mode, *Live
mode* (d)). Without it a full live sweep hits cap refusals (devices, feeds) partway
through and records them as failures. Creating that tier is the admin's choice,
never the harness's, so this is NOT part of a sweep: it runs only when
``FAUNA_LIVE_DEFINE_HARNESS_TIER=1`` is set, as one deliberate admin act per box.

The flow, every mutation a tui admin-UI action (convention 8), every wait a poll on
observable state (convention 14):

  1. Open the admin Tiers page as the box's admin.
  2. If a tier named ``e2e-harness`` is already listed, stop — nothing to do (the
     test never edits an existing tier).
  3. Read the ``free`` row's five caps off the page, type them into the add form
     with ``max_devices=64`` and ``max_feeds=1000`` (caps that do not bind for one
     run's identity, the other caps copied from ``free``), choose Add tier.
  4. Read the new row back: every cap persisted.

Blast-radius argument (testing.md § The shared-box rule, non-destructive carve-out
— required on every non-destructive ``live_box`` test):
  - **What it mutates:** one new tier definition (``fauna.admin.tiers.create``)
    named ``e2e-harness``. Nothing else: no existing tier, user, account, setting
    or credential is read for writing, and it never touches ``free``.
  - **Why invisible to the human:** a tier nobody is assigned to changes nothing
    for any member; it shows up as one extra row in the admin's tier list and as
    one extra count card on the dashboard. No factory reset, no logout, no
    deletion, no modification of pre-existing data.
  - **What teardown removes:** nothing — and that is stated rather than hidden:
    the wire has no tier delete (admin.md § 3), so the tier is permanent. It is
    additive-only and was explicitly approved for dev.example.com on 2026-10-04.
    Re-running is a no-op (step 2), so a repeat never duplicates it.

Test taxonomy: tier_4 (live-remote — a real deployed box).
"""
from __future__ import annotations

import os
import time

import pytest

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_only,
    pytest.mark.live_box,
    # Class (3)'s admin-shell scan would otherwise deselect this on live — it
    # drives the admin Tiers page by design. Re-admitted under the shared-box
    # rule's non-destructive carve-out: the blast-radius argument above.
    pytest.mark.live_ok,
    pytest.mark.skipif(
        os.environ.get("FAUNA_LIVE_DEFINE_HARNESS_TIER") != "1",
        reason="defines the e2e-harness tier on the live box (permanent, additive): "
        "set FAUNA_LIVE_DEFINE_HARNESS_TIER=1 and run with --nest live:URL — one "
        "deliberate admin act per box, never part of a sweep (see module docstring)",
    ),
]

TIER_NAME = "e2e-harness"
#: Caps that do not bind for one run's identity; the other
#: three are copied from the box's `free` tier.
DEVICES, FEEDS = "64", "1000"
CAPS = ("inbox", "storage", "devices", "blob-size", "feeds")


def _wait(pred, what: str, timeout: float = 20.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not pred():
        time.sleep(0.3)
    assert pred(), what


def test_the_admin_defines_the_e2e_harness_tier(admin_app):
    admin = admin_app.admin
    admin.navigate_settings()
    _wait(
        lambda: admin.tier_definition_count() >= 1,
        f"tier definitions did not load. error: {admin_app.error_text()!r}",
    )
    names = admin.tier_names()
    if TIER_NAME in names:
        # Already defined: never edit, never duplicate. Say what is there, so a
        # no-op run is distinguishable from a creating one.
        index = names.index(TIER_NAME)
        found = {cap: admin.tier_cap_value(cap, index=index).strip() for cap in CAPS}
        print(f"[live-tier] {TIER_NAME!r} already defined, caps {found!r}; tiers {names!r}")
        return

    assert "free" in names, f"the box has no `free` tier to copy caps from: {names!r}"
    free = names.index("free")
    caps = {cap: admin.tier_cap_value(cap, index=free).strip() for cap in CAPS}
    caps["devices"], caps["feeds"] = DEVICES, FEEDS

    before = admin.tier_definition_count()
    admin.edit_new_tier_name(TIER_NAME)
    for cap, value in caps.items():
        admin.edit_new_tier_cap(cap, value)
    admin.add_tier()

    _wait(
        lambda: admin.tier_definition_count() == before + 1,
        f"the new tier did not appear after create+refetch. rows: "
        f"{admin.tier_names()!r}. error: {admin_app.error_text()!r}",
    )
    index = admin.tier_names().index(TIER_NAME)
    for cap, value in caps.items():
        assert admin.tier_cap_value(cap, index=index).strip() == value, (
            f"cap {cap!r} of the new tier did not persist"
        )
    print(f"[live-tier] defined {TIER_NAME!r} with caps {caps!r}; tiers {admin.tier_names()!r}")
