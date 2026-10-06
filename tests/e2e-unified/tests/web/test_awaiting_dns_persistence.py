"""Awaiting-manual-DNS ("Almost ready") persistence — force-quit / relaunch.

The deferred-DNS path: the user provisions a nest, chooses "Set up later" for
DNS, and the wizard exits `Done` with `wizard_outcome() == AwaitingManualDns`
(`docs/goal/behavior/onboarding.md` § "Almost ready" surface). Two paths reach
the surface and BOTH must render it identically:

  1. the same-session exit from `dns_post_instructions`, and
  2. the relaunch hydration — the long-term store's awaiting-manual-dns slot ->
     `LaunchMachine`'s `WizardAt{AwaitingManualDns}` row -> `seed_awaiting_manual_dns`.

Web-specific, like `test_pending_invite_persistence.py`: the store is
localStorage and "force-quit / relaunch" is `hard_reload()` (the browser context
survives it, which is exactly what makes the slot's round-trip observable). The
cross-app cousin is case I of `test_onboarding_launch_routing_smoke.py`, which
web cannot join until `make_cred_store` grows a web adapter.

The regression this pins (the reason the record is stored as machine-produced
JSON rather than a hand-built object): `DnsRecordPlain` is `record_type` in serde
but `recordType` in the WASM bindings, so a client that `JSON.stringify`s the
*bound* type into the slot round-trips the records to an EMPTY LIST -- an "Almost
ready" page with nothing to add at the registrar, invisible until a relaunch.
"""

import json

from common.accounts import actor_id_hex
from helpers import web_store

import pytest

pytestmark = [pytest.mark.web, pytest.mark.tier_2]


SECRET_HEX = "11" * 32
NEST_URL = "https://nest.example.invalid"
HANDLE = "alice@example.invalid"
CLAIM_CODE = "claim-code-abc123"

# Exactly what `awaitingDnsRecordsJson()` emits: serde's snake_case
# `record_type`, NOT the binding's `recordType`.
RECORDS_JSON = json.dumps([
    {"record_type": "A", "name": "@", "value": "203.0.113.7", "ttl": 300, "priority": None},
    {"record_type": "MX", "name": "@", "value": "mail.example.invalid", "ttl": 300, "priority": 10},
])


def _seed_awaiting_dns_slot(app) -> None:
    """Write the identity + its per-actor 4-field awaiting-manual-dns record, as
    the deferred-DNS wizard exit does (`persist_awaiting_dns`). `handle` is
    the field every pre-registry 3-key slot omitted."""
    actor = web_store.seed_identity(app.driver, SECRET_HEX)
    web_store.seed(
        app.driver,
        {
            f"fauna/{actor}/awaiting_dns": web_store.awaiting_dns_record(
                nest_url=NEST_URL,
                handle=HANDLE,
                dns_records_json=RECORDS_JSON,
                claim_code=CLAIM_CODE,
            )
        },
    )


def _read_awaiting_dns_slot(app) -> dict:
    """The identity's per-actor `fauna/{actor}/awaiting_dns` record as the four
    fields this file asserts on, every one None when the slot is absent."""
    record = web_store.read_record(app.driver, actor_id_hex(SECRET_HEX), "awaiting_dns") or {}
    return {
        "nest_url": record.get("nest_url"),
        "handle": record.get("handle"),
        "records_json": record.get("dns_records_json"),
        "claim_code": record.get("claim_code"),
    }


def test_relaunch_with_awaiting_dns_slot_lands_on_the_almost_ready_surface(app):
    """The launch row: identity + awaiting-dns slot -> "Almost ready", NOT the
    silent challenge and NOT handle_entry.

    This is the whole point of the row being checked *before* the
    silent-challenge row: the nest's DNS hasn't propagated, so challenging it
    could only fail through to `launch_retry` / `handle_entry` and silently
    discard the nest the user just paid for and provisioned.
    """
    _seed_awaiting_dns_slot(app)
    app.driver.hard_reload()

    assert app.is_visible("awaiting-dns-records"), (
        "relaunch with an awaiting-dns slot must land on the 'Almost ready' surface: "
        f"{app.driver.diagnose('awaiting-dns-records')} error={app.error_text()!r}"
    )

    # The nest was discarded if we fell through to the wizard's first page.
    assert app.is_absent("handle-input"), (
        "relaunch fell through to handle_entry -- the half-provisioned nest was "
        f"discarded. error={app.error_text()!r}"
    )

    # The records survived the JSON round-trip. An empty list here is the
    # camelCase trap: the page renders, with nothing to add at the registrar.
    records_text = app.get_text("awaiting-dns-records")
    assert "203.0.113.7" in records_text, (
        "the seeded DNS records round-tripped to an empty/garbled list -- the user "
        f"sees an 'Almost ready' page with nothing to add. text={records_text!r}"
    )
    assert "mail.example.invalid" in records_text, f"second record missing: {records_text!r}"

    assert app.is_visible("awaiting-dns-status"), "the surface must show a status message"
    assert app.is_visible("awaiting-dns-recheck-button"), "the surface must offer an explicit probe"
    assert app.is_visible("awaiting-dns-copy-button"), "the surface must offer copy-all"


def test_deferred_dns_exit_persists_all_four_fields_including_handle(app):
    """The same-session exit: continuing from `dns_post_instructions` renders the
    surface AND writes the 4-field slot, so the next launch can rehydrate it.

    A 3-field slot (the legacy shape: no `handle`) cannot satisfy
    `seed_awaiting_manual_dns`, so the surface would never come back.
    """
    web_store.seed_identity(app.driver, SECRET_HEX)
    app.driver.call_machine_method("seed_identity", json.dumps(SECRET_HEX))
    # `nest_url` and `handle` come from the wizard's own state at the exit
    # (`current_handle()` for the handle -- the field the legacy slot omitted);
    # the shared action seeds the records + the succeeded ProvisioningSnapshot
    # whose `result.claim_code` the exit reads.
    app.driver.call_machine_method("set_nest_url", json.dumps(NEST_URL))
    app.driver.call_machine_method("set_current_handle", json.dumps(HANDLE))
    app.onboarding.go_to_dns_post_instructions_with_records([
        "A example.invalid 203.0.113.7",
        "MX example.invalid mail.example.invalid",
    ])

    app.click("dns-post-instructions-continue-button")

    assert app.is_visible("awaiting-dns-records"), (
        "the deferred-DNS exit must render 'Almost ready' in-session, not leave the "
        f"user parked on dns_post_instructions: {app.driver.diagnose('awaiting-dns-records')} "
        f"error={app.error_text()!r}"
    )

    slot = _read_awaiting_dns_slot(app)
    assert slot["nest_url"] == NEST_URL, f"nest_url not persisted: {slot!r}"
    # The claim code the shared action's ProvisioningSnapshot.result carries.
    assert slot["claim_code"] == "test-claim-code", f"claim_code not persisted: {slot!r}"
    # The field the legacy 3-key slot omitted, and the reason it was dead storage.
    assert slot["handle"] == HANDLE, (
        f"handle not persisted -- the slot cannot satisfy the seeder: {slot!r}"
    )

    # Written via `awaitingDnsRecordsJson()`, so serde's snake_case survives.
    records = json.loads(slot["records_json"])
    assert len(records) == 2, (
        "records round-tripped to an empty/short list -- the bound type was "
        f"stringified instead of using awaitingDnsRecordsJson(): {slot['records_json']!r}"
    )
    assert records[0]["record_type"] == "A", (
        f"expected serde's `record_type`, got the binding's camelCase: {records[0]!r}"
    )


def test_copy_all_is_disabled_in_the_records_less_mode_and_live_with_records(app):
    """The records-less mode's second rule (the first being the status copy):
    "Copy all" is INERT, because there is nothing to copy. A button that
    answers a click by silently copying the empty string reads as a broken
    page rather than an empty one -- and it is disabled rather than removed,
    since ui.yaml scopes the ID to this page's required elements. Mirrors
    tui's `copy_all_is_disabled_in_the_records_less_mode_and_live_with_records`.
    """
    app.onboarding.go_to_awaiting_manual_dns(records=[])
    assert not app.driver.is_enabled("awaiting-dns-copy-button"), (
        "a resumed standard-path run has no records, so Copy all must be inert: "
        f"{app.driver.diagnose('awaiting-dns-copy-button')}"
    )

    # Re-seed the SAME session with records -- `seed_awaiting_manual_dns` just
    # rewrites the machine's slot state, no relaunch needed to observe the flip.
    app.onboarding.go_to_awaiting_manual_dns()
    assert app.driver.is_enabled("awaiting-dns-copy-button"), (
        "with records to add at a registrar, Copy all is exactly the affordance "
        f"the mode exists for: {app.driver.diagnose('awaiting-dns-copy-button')}"
    )
