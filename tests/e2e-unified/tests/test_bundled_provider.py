"""The bundled-provider journey — one intermediary as registrar + DNS + VPS
(`docs/goal/architecture/provisioning/bundled-provider-api.md`;
`onboarding.md` §§ 4–6; `registry.md` § Bundled provider).

Drives the wizard the way a user would (e2e convention 8): on `dns_config`
the generic `bundled` row is the one provider eligible with BOTH checkboxes on
(all three capabilities), the user types the intermediary's address into the
`base-url` field, presses the `hosted-auth` field's button (the RFC 8628
device flow — the fake approves the code the way the user's browser step
would), verifies, sees the first-year price, fills the WHOIS contact
(registrant = the user), ticks the price-confirm + withdrawal
acknowledgement, and continues; `vps_config` is pre-selected with the same
provider and the same sign-in; the BoM discloses the renewal price; *Buy and
set up* runs the real orchestrator against the fake intermediary end to end
(register → zone → server → records → health → PTR).

Every call the wizard makes to the provider goes to the address the user
TYPED — only the nest leg is redirected, so a `vps`/`dns` override can never
mask the typed-base-URL path. That one override points at a REAL never-claimed
nest (`provision_target_nest`), because on the standard path the run's `Online`
step ends by CLAIMING the box over WS-RPC and no fake serves WS-RPC; the long
comment at the override says why in full.

tui is the lead app (`docs/goal/architecture/testing.md` § Default app and
nest mode — the rust-first ordering); linux, web, and android are the first
three trickle-down legs; the other three
render a `hosted-auth` field as a secret/password input until their own
trickle-down lands (`onboarding.md` § 4 degradation rule) and are declared
`skip_unbuilt` here.
"""
from __future__ import annotations

import json

import pytest

from drivers.machine_test_setter import set_handle_check_snapshot
from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.waiting import wait_until

# tier_3, not tier_2: the run's nest leg is a REAL `fauna-nest` binary
# (`provision_target_nest`). It has to be — see the override comment in the test.
pytestmark = pytest.mark.tier_3

# The seeded handle's domain — what the fake registers and the zone it mints.
HANDLE_DOMAIN = "not-a-real-fauna-e2e-domain.test"

CONTACT = {
    "first_name": "Test",
    "last_name": "User",
    "email": "hello@example.test",
    "phone": "+1.5555550100",
    "address1": "1 Example Way",
    "city": "Exampleton",
    "state": "EX",
    "postal_code": "00000",
    "country": "US",
}


def _require_hosted_auth_button(driver) -> None:
    if app_name(driver) not in ("tui", "linux", "web", "android", "macos", "ios", "windows"):
        skip_unbuilt(
            driver,
            surface="the hosted-auth field button (dns-credentials-form-api-token as a button)",
            detail="renders the bundled provider's hosted-auth field as a secret input for now",
            tracked="onboarding.md § 4 hosted-auth rendering rule — per-app trickle-down rows in each app's own queue",
        )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_bundled_provider_buys_domain_and_server_through_one_row(
    app, fake_cloud, provision_target_nest
):
    drv = app.driver
    _require_hosted_auth_button(drv)
    bundled = fake_cloud.bundled
    nest = provision_target_nest

    # Only the NEST leg is redirected — every registrar/DNS/VPS call still goes
    # to the address the user TYPES below, so a stray override can never mask a
    # wrong typed-base-URL path.
    #
    # It points at a REAL never-claimed nest rather than at `fake_cloud`'s
    # `/nest`, and that is not incidental: since `onboarding.md` § 6
    # *Provisioning = build + claim* (2026-08-29) the run's `Online` step ends by
    # CLAIMING the box over WS-RPC, and `fake_cloud` serves `/api/v1/health` over
    # HTTP and nothing over WS-RPC — werkzeug cannot serve a WS upgrade at all.
    # Pointed at the fake, `claim_provisioned_box`'s probe takes a 500, the claim
    # substep lands `Failed`, and `provisioning-continue-button` — gated on
    # `claim_completed`, not on `Succeeded` alone — never enables. One override
    # carries both legs: the orchestrator's `{nest_base_url}/api/v1/health` poll
    # and the claim, because `WsNestApi::resolve()` prefers it over the caller's
    # URL. Same shape as `test_provisioning_claims_a_real_nest.py`.
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(drv, nest)
    drv.set_provider_base_urls({"nest": nest["url"]})
    # The stand-in box already exists and already knows its own claim code, so
    # the run presents that one instead of minting one the box never heard of.
    drv.call_machine_method(
        "set_provision_claim_code_for_test", json.dumps(nest["claim_code"])
    )

    # ── handle_entry → dns_config ────────────────────────────────────────
    # Identity through the UI, then the handle-check OUTCOME is seeded via
    # the sanctioned test-setter (fixture setup, convention 8 — the same idiom
    # test_handle_entry_outcomes.py uses) rather than the live DoH probe, so
    # the journey never depends on outbound DNS from the test box; every
    # mutation under test below is a driver UI action.
    app.onboarding.go_to_handle_entry()
    set_handle_check_snapshot(app, {
        "phase": "Complete",
        "outcome": {"DomainAvailable": {"buyable_via_provider": True, "price": None}},
        "message": {
            "key": "onboarding.handle_check.outcome.domain_available_unpriced",
            "args": {"domain": HANDLE_DOMAIN},
        },
        "continue_enabled": True,
        "control_checkbox_visible": False,
        "control_checkbox_checked": False,
    }, handle=f"alice@{HANDLE_DOMAIN}")
    wait_until(lambda: app.is_enabled("handle-entry-continue-button"), 10.0,
               diagnose=lambda: drv.diagnose("handle-entry-continue-button"))
    drv.click("handle-entry-continue-button")
    app.wait_for("dns-buy-domain-checkbox")
    # DomainAvailable landing: buy_domain=true AND same_provider_for_vps=true
    # (machine defaults) — exactly the state in which the bundled row is the
    # only eligible one (registrar + vps).
    wait_until(
        lambda: drv.is_visible("dns-provider-row[bundled]") and not drv.is_disabled("dns-provider-row[bundled]"),
        10.0,
        diagnose=lambda: "bundled row not eligible with both checkboxes on: " + drv.diagnose("dns-provider-row[bundled]"),
    )
    for other in ("porkbun", "hetzner"):
        if drv.is_visible(f"dns-provider-row[{other}]"):
            assert drv.is_disabled(f"dns-provider-row[{other}]"), (
                f"{other} lacks registrar+vps and must be disabled with both boxes on"
            )
    drv.click("dns-provider-row[bundled]")
    app.wait_for("dns-credentials-form-base-url")
    drv.fill("dns-credentials-form-base-url", bundled.base_url())

    # The hosted-auth field is a BUTTON carrying the field's derived id; its
    # label is the machine's sign-in state (onboarding.md § 4).
    app.wait_for("dns-credentials-form-api-token")
    assert "Sign in" in drv.get_text("dns-credentials-form-api-token"), drv.diagnose("dns-credentials-form-api-token")
    drv.click("dns-credentials-form-api-token")
    wait_until(
        lambda: drv.get_text("dns-credentials-form-api-token").strip() == "Connected",
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "hosted-auth never reached Connected: " + drv.get_text("dns-credentials-form-api-token"),
    )
    assert bundled.device_codes, "the wizard must have started the device flow at the typed address"

    drv.click("dns-verify-button")
    wait_until(
        # `is_visible_scrolled`, not a bare `is_visible`, for the same reason as
        # `vps-server-type-radio[0]` below: at this moment the page carries the
        # price display, the whole unfilled 9-field contact form, and only THEN
        # this checkbox, so on a window shorter than the form it is
        # rendered-but-below-the-fold — which windows' UIA `IsOffscreen` read
        # calls invisible. Measured 2026-09-10: verify had plainly succeeded
        # (`dns-status-text` already read "…10.99 EUR. Tick the confirm box…",
        # `error=''`) while this wait timed out at 90 s.
        lambda: drv.is_visible_scrolled("dns-price-confirm-checkbox"),
        ORCHESTRATION_STEP_S,
        # Convention 6: the status line alone cannot separate "verify never
        # ran" from "verify ran and the provider refused" — the refusal lands
        # on error-message, and the verify button's own enabled state says
        # whether the machine even thinks the credentials are complete.
        diagnose=lambda: (
            "no price quote after verify: status=" + drv.get_text("dns-status-text")
            + f" | error={app.error_text()!r}"
            + f" | verify-button-enabled={app.is_enabled('dns-verify-button')}"
            + f" | api-token-button={drv.get_text('dns-credentials-form-api-token')!r}"
        ),
    )
    assert "10.99" in drv.get_text("dns-tld-price-display"), drv.get_text("dns-tld-price-display")
    # registrant = the user → the contact form is up (requires_contact = true).
    app.wait_for("dns-contact-form")
    app.onboarding.fill_dns_contact(CONTACT)
    # The price-confirm checkbox carries the withdrawal acknowledgement —
    # one string for every registrar (onboarding.md § 4).
    assert "withdraw" in drv.get_text("dns-price-confirm-checkbox").lower()
    drv.click("dns-price-confirm-checkbox")
    wait_until(lambda: app.is_enabled("dns-config-continue-button"), 10.0,
               diagnose=lambda: "Continue stayed disabled: " + drv.get_text("dns-status-text"))
    drv.click("dns-config-continue-button")

    # ── vps_config: same provider, same sign-in, the provider's own catalog ──
    app.wait_for("vps-verify-button")
    assert drv.get_text("vps-credentials-form-api-token").strip() == "Connected", (
        "the sign-in done on the DNS form must carry over to the VPS form"
    )
    # Rendered is not enabled — the same trap the shared verified-VPS helper hit.
    drv.wait_until_enabled("vps-verify-button", timeout=10)
    drv.click("vps-verify-button")
    # `is_visible_scrolled`, not a bare `is_visible`: the radio group is the LAST
    # thing on a long vps_config form inside a scroll container, so on a window
    # shorter than the form it is rendered-but-below-the-fold — which windows'
    # UIA `IsOffscreen` read reports as invisible. The helper degrades to a plain
    # `is_visible` on drivers without scroll support and never masks a genuinely
    # absent element, so this stays one cross-app assertion.
    wait_until(lambda: drv.is_visible_scrolled("vps-server-type-radio[0]"), ORCHESTRATION_STEP_S,
               # Convention 6, same split as the DNS verify above: the radio's
               # absence cannot say whether the probe was refused, never ran,
               # or ran and returned an empty catalog.
               diagnose=lambda: (
                   "no server types after VPS verify: " + drv.diagnose("vps-server-type-radio")
                   + f" | error={app.error_text()!r}"
                   + f" | verify-button-enabled={app.is_enabled('vps-verify-button')}"
                   + f" | api-token-button={drv.get_text('vps-credentials-form-api-token')!r}"
               ))
    # A count: a sixth radio rendered below the fold of the scrolling provisioning
    # view would read "not visible" on windows and pass this vacuously.
    assert drv.count("vps-server-type-radio[5]") == 0, "the catalog is capped at five"
    if drv.is_visible("vps-location-picker"):
        drv.select("vps-location-picker", "Europe 1")
    # Mail mode is on for a real domain → the ≥ 2 GB tier; `small` is 2 GB.
    drv.click("vps-server-type-radio[0]")
    wait_until(lambda: app.is_enabled("vps-config-continue-button"), 10.0)
    drv.click("vps-config-continue-button")

    # ── nest_provisioning: the BoM discloses the renewal price ───────────
    app.wait_for("provisioning-start-button")
    domain_line = drv.get_text("provisioning-bom-domain-line")
    assert "10.99" in domain_line and "14.99" in domain_line and "then" in domain_line, domain_line
    assert "5.99" in drv.get_text("provisioning-bom-vps-line")

    drv.click("provisioning-start-button")
    wait_until(
        lambda: app.is_enabled("provisioning-continue-button"),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "provisioning never Succeeded; snapshot=" + str(drv.call_machine_method("provisioning_snapshot")),
    )

    # ── what the intermediary saw ────────────────────────────────────────
    assert len(bundled.registered) == 1
    reg = bundled.registered[0]
    assert reg["name"] == HANDLE_DOMAIN
    assert reg["years"] == 1
    assert reg["agreed_price_cents"] == bundled.registration_cents
    assert reg["contact"]["first_name"] == CONTACT["first_name"], "the registrant is the user"
    assert reg["whois_privacy"] is True
    servers = list(bundled.servers.values())
    assert len(servers) == 1
    srv = servers[0]
    assert srv["labels"].get("managed-by") == "fauna"
    assert srv["server_type"] == "small" and srv["location"] == "eu-1"
    assert "#cloud-config" in srv["user_data"], "cloud-init passed through verbatim"
    zone_id = next(z["id"] for z in bundled.zones.values() if z["name"] == HANDLE_DOMAIN)
    recs = bundled.records[zone_id]
    assert any(r["type"] == "A" and r["name"] == "@" and r["value"] == "203.0.113.5" for r in recs), recs
    assert any(r["type"] == "MX" for r in recs), recs
    assert bundled.ptr[srv["id"]] == f"mail.{HANDLE_DOMAIN}"
