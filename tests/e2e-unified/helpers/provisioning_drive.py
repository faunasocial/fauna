"""Drive a standard-path provisioning run from a test, in the shape a user arrives with.

`onboarding.md` § 6 *Provisioning = build + claim*. Every journey that runs the
real orchestrator needs the same two things before it may click Start: the
provider base URLs pointed somewhere that is not the live internet, and every
value `run_provisioning_inner` reads seeded on the machine. Both were copied
into each test that needed them, and the copies had already drifted — one
seeded no identity at all and refused to build (`provisioning failed at
pending_provision_slot`, measured on windows 2026-08-30), which is a bug a
second copy of the block will always be one edit away from re-creating.

So the two acts live here, once, and read as sentences at the call site:
`point_providers_at(...)` then `seed_standard_path_run(...)`.

The two resume-side helpers at the bottom — `require_durable_client_store` and
`assert_records_less_almost_ready` — are the same story one journey later: what
a *relaunched* app must find after a run that never finished
(`onboarding-provisioning.md` § "Almost ready" surface), shared by the
first-run slot-recovery journeys and the "Add account" ones.
"""
from __future__ import annotations

import json

import pytest

from helpers.budgets import APP_RELAUNCH_S
from helpers.crash_recovery import launch_surface_dump
from helpers.waiting import wait_until
from i18n.strings import S

#: The fake cloud's Cloudflare zone. A run's handle domain must match it or the
#: Domain/Dns steps have no zone to write into.
HANDLE_DOMAIN = "example.test"


def point_providers_at(
    app, fake_cloud, *, nest_base_url: str | None = None, nest: dict | None = None
) -> None:
    """Redirect the run's VPS/DNS (and optionally nest) legs, and PROVE it landed.

    The read-back is not ceremony: without it a mis-delivered override leaves the
    orchestrator dialling the live internet, and every later assertion passes or
    fails for a reason that has nothing to do with the journey. The
    `provider_base_url` reader answers on every app.

    `nest_base_url` points the nest leg — the Online health poll *and* the
    machine's claim, since `WsNestApi::resolve()` prefers this override over the
    caller's URL — at a real never-claimed nest (the `provision_target_nest`
    fixture). Leave it `None` for a journey that never reaches Online.

    `nest` is that same fixture's raw dict — pass it whenever `nest_base_url`
    came from it (`nest["url"]`), so the session-cached app is relaunched
    trusting that nest BEFORE this override points it there
    (`e2e-automation-surface-gating.md` § The e2e trust seed). Leave both
    `None` for a journey that never reaches Online.

    ⚠ Named for the orchestrator's own `nest_base_url`, deliberately NOT
    `nest_url`: that spelling is a **peer-authority** field — the authority one
    nest is asked to dial another at — and feeding one from a handle's `url`
    makes a test invisible to the mode axis's class (8) classifier
    (`test_nest_mode_axis.py::test_no_new_peer_authority_is_fed_from_url_instead_of_peer_url`,
    which caught exactly this). Nothing here asks a nest to dial anything; this
    is a provider base URL this process overrides on its own machine.

    Cross-app: web rides the `?fauna_e2e_provider_base_urls` query-param channel
    (it reconstructs the wizard machine on reload); native apps route through the
    `call_machine_method` bridge to the shared machine's runtime setter — either
    way the bridge's machine ref and the run share one override.
    """
    if nest is not None:
        from conftest import _relaunch_trusting_nest

        _relaunch_trusting_nest(app.driver, nest)
    urls = dict(fake_cloud.url_map())
    if nest_base_url is not None:
        urls["nest"] = nest_base_url
    app.driver.set_provider_base_urls(urls)

    applied = app.driver.call_machine_method("provider_base_url", json.dumps("dns"))
    assert applied == urls["dns"], (
        "the provider-base-url override never reached the machine, so the run "
        "would escape to the live internet and every assertion below would be "
        f"about the wrong box. dns_override={applied!r} expected={urls['dns']!r}"
    )
    if nest_base_url is not None:
        applied = app.driver.call_machine_method("provider_base_url", json.dumps("nest"))
        assert applied == nest_base_url, (
            "the nest override must reach the machine before the run starts; "
            "without it the run claims nothing and a claim assertion would pass "
            f"vacuously against a fake. nest_override={applied!r}"
        )


def seed_standard_path_run(
    app,
    *,
    handle_domain: str = HANDLE_DOMAIN,
    server_type_id: str = "cx23",
    claim_code: str | None = None,
) -> None:
    """Seed everything `run_provisioning_inner` reads, then land on its page.

    Leaves the app on `NestProvisioning` with `provisioning-start-button`
    clickable. Seeds rather than drives the earlier pages because those have
    their own journeys; what is under test here starts at Start.

    ⚠ **The IDENTITY is part of "everything it reads."** Jumping straight to
    `NestProvisioning` is the only way to reach this page without one, and since
    the pending-provision slot landed (§ 6) the run mints and persists its claim
    code **before** `create_server` and refuses outright when it cannot. The slot
    is addressed by the identity being onboarded, so no identity means no write,
    means the run refuses with all four steps still Pending. That refusal is
    correct — custody precedes dispatch; a drive without an identity is the thing
    not shaped like a user, who always has one by this page.

    ⚠ That refusal only ever SHOWED on an app that installs a real slot store:
    an app whose store is absent takes `mint_and_persist_pending_provision`'s
    `(None, None)` arm, mints a bare code and never notices. A green run
    elsewhere was never evidence the seed was unnecessary.

    `claim_code` pins the code the run will present instead of minting one — for
    a journey whose stand-in box already exists and already knows its own code
    (`provision_target_nest["claim_code"]`). Pointing it the other way, having
    the harness write the machine's code into the box, would race a file write
    against the claim: the machine's code does not exist until mid-run.
    """
    drv = app.driver
    if claim_code is not None:
        drv.call_machine_method("set_provision_claim_code_for_test", json.dumps(claim_code))
    drv.call_machine_method(
        "seed_identity", json.dumps(app.onboarding._IMPORT_KEY_FOR_HANDLE_TESTS)
    )
    drv.call_machine_method("set_current_handle", json.dumps(f"alice@{handle_domain}"))
    # Credential keys are kebab-case: a snake_case key makes the dispatch return
    # None, which surfaces as "doesn't support VPS" rather than as a bad key.
    drv.call_machine_method(
        "set_captured_dns_credential_for_test",
        json.dumps({"provider_id": "cloudflare", "fields": {"api-token": "tkn"}}),
    )
    drv.call_machine_method(
        "set_vps_state_for_test",
        json.dumps({
            "provider_id": "hetzner",
            "creds": {"api-token": "tkn"},
            "server_types": [{
                "id": server_type_id, "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
                "price_monthly_cents": 451, "currency": "EUR",
            }],
            "selected_server_type_id": server_type_id,
            "locations": [{
                "id": "fsn1", "name": "Falkenstein",
                "city": "Falkenstein", "country": "DE",
            }],
            "selected_location_id": "fsn1",
        }),
    )
    drv.call_machine_method("set_step_for_test", json.dumps("NestProvisioning"))
    app.wait_for("provisioning-start-button")


def overall(app) -> str | None:
    """The run's `overall` enum, or `None` if the snapshot is not a dict yet."""
    snap = app.driver.call_machine_method("provisioning_snapshot")
    return snap.get("overall") if isinstance(snap, dict) else None


def step_status(app, kind: str) -> str | None:
    """One step row's `status` by `kind` (`Domain`/`Server`/`Dns`/`Online`).

    By kind rather than by index: the row order is a rendering fact, and a
    journey asserting "the Server step failed" should not silently start
    asserting about DNS if the order ever changes.
    """
    snap = app.driver.call_machine_method("provisioning_snapshot")
    if not isinstance(snap, dict):
        return None
    for step in snap.get("steps", []):
        if step.get("kind") == kind:
            return step.get("status")
    return None


def require_durable_client_store(app) -> None:
    """A slot-resume journey lives or dies on the client's own long-term store.

    The drivers hand a relaunched app a FRESH store by default (a new XDG base,
    a keyring namespace derived from the per-launch agent port), which is right
    for journeys whose recovery lives nest-side and fatal here: with a fresh
    store "the slot did not survive" is indistinguishable from "the client never
    wrote it", and the test would go green against a client that persists
    nothing.
    """
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a "
            "relaunch, so the pending-provision-slot assertion would be vacuous"
        )


def assert_records_less_almost_ready(app, *, after: str) -> None:
    """The "Almost ready" surface in its records-less mode (§ *Two modes*).

    The empty record list is what selects both of the mode's rules —
    `resting_message_key` and `copy_all_enabled` derive them, so all seven apps
    get both modes from one derivation. Telling a user to "add the DNS records
    below" under an empty list is the one thing this page must not do, and it is
    exactly what an interrupted *standard-path* run would have produced before
    the derivation landed.

    ⚠ **The status row is asserted with a deadline poll, not a single read**, and
    the reason is a real race rather than caution: the surface polls, and while a
    probe or claim is in flight the row carries the `checking`/`claiming` copy
    instead of the resting one (`set_awaiting_dns_state` at each `ClaimPhase`).
    The box here is unreachable by construction, so how long a probe occupies the
    row is a connect timeout — a wall clock the assertion must not depend on
    (convention 14). What is latency-independent is that the surface RESTS on the
    records-less copy, so that is what is waited for, under a generous ceiling.
    """
    try:
        app.driver.wait_for("awaiting-dns-status", timeout=APP_RELAUNCH_S)
    except Exception as exc:
        raise AssertionError(
            f"the relaunched client never rendered the 'Almost ready' surface "
            f"{after}. The launch routing takes that row on "
            f"`identity && awaiting-dns slot`, so this says the slot did not "
            f"survive, was not written, or was not read back. "
            f"error={app.error_text()!r} launch surface: {launch_surface_dump(app)}"
        ) from exc

    last = {"text": None}

    def resting_on_the_records_less_copy():
        last["text"] = app.driver.get_text("awaiting-dns-status")
        return S.onboarding.awaiting_dns.server_starting in last["text"]

    wait_until(
        resting_on_the_records_less_copy,
        APP_RELAUNCH_S,
        diagnose=lambda: (
            f"{after} the surface never rested on the records-less copy — there "
            f"is nothing for this user to paste, so the honest line is that the "
            f"server is starting. Last status row: {last['text']!r}. A page stuck "
            f"on the with-records copy is the mode derivation failing; one stuck "
            f"on checking/claiming is a probe that never returned."
        ),
    )

    records = app.driver.get_text("awaiting-dns-records")
    assert not records.strip(), (
        f"{after} the run wrote no DNS records (the standard path's are ours to "
        f"write), so the records block must be empty rather than showing a "
        f"half-filled instruction: {records!r}"
    )
    assert not app.driver.is_enabled("awaiting-dns-copy-button"), (
        f"{after} Copy all must be inert with an empty record list — a click that "
        f"silently copies nothing reads as a broken page: "
        + app.driver.diagnose("awaiting-dns-copy-button")
    )
