"""E2E coverage for the nest_provisioning page rendering.

Per docs/goal/behavior/onboarding.md §6 + the provisioning-progress design
(tracked internally):

  - Snapshot has fixed shape: 4 step rows (Domain/Server/Dns/Online),
    `overall` enum, optional `started_at_ms`/`finished_at_ms`/`result`/
    `final_error`.
  - Each step has `kind`, `status`, `substep`, `attempt`, `max_attempts`,
    `last_error`, `skip_reason`, `started_at_ms`, `finished_at_ms`.
  - Page renders 4 `provisioning-step-row[i]` rows with checkbox / label /
    substep / error sub-elements.
  - `provisioning-cancel-button` visible only when `overall == Running`.
  - `provisioning-retry-button` visible only when `overall == Failed`.
  - `provisioning-elapsed` rendered once `started_at_ms` is set.
  - `provisioning-continue-button` enabled only when `overall == Succeeded`.

Tests fixture-drive snapshots through the e2e bridge so they exercise
pure rendering — no real orchestrator run. The cancel-mid-Online →
retry-succeeds path (below) DOES run a real orchestrator against the
fake-cloud fixture, cross-app.
"""
import json

import pytest

from common.nest import start_nest_in_place, stop_nest
from drivers.machine_test_setter import set_provisioning_snapshot
from helpers.budgets import ORCHESTRATION_STEP_S, UI_SETTLE_S
from helpers.provisioning_drive import point_providers_at, seed_standard_path_run
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_2

#: Where each driver family keeps the client's own account of itself, in the
#: order to try — deliberately the same pair, in the same order, as
#: `helpers/app_log_section.py::_TEXT_ATTRS` and `instance_guard.py`: one
#: spelling of "the app's own words".
_APP_LOG_ATTRS = ("app_log_text", "app_stderr_text")


def _app_own_log(driver):
    """`(attr_name, text)` from the first log reader this driver has, else `(None, None)`."""
    for attr in _APP_LOG_ATTRS:
        reader = getattr(driver, attr, None)
        if reader is None:
            continue
        return attr, (reader() or "")
    return None, None


def _assert_orchestrator_narrated_its_attempts(app):
    """The shared orchestrator's per-attempt narration must reach the app's own log.

    This asserts the whole chain a post-mortem of a stalled run depends on:
    `run_step` (`libs/fauna-provisioning/src/progress.rs`) emits one `info!` per
    attempt → the app's process-global subscriber (`fauna_log::init`, whose
    default filter is `info`) → the on-disk rolling file / captured stderr → the
    driver's log reader. Every link is shared, so this holds for every app that
    keeps a client log.

    **Why this is asserted and not merely available.** A live windows
    provisioning run froze in the `Online` step with `attempt 2/480` still
    pending after 1200s, and its 27-minute captured app log contained exactly
    ONE line — because `run_step`, the loop driving every provisioning step, had
    no `tracing` calls and its crate had no `tracing` dependency at all. The
    single most diagnostic fact about a stalled run (which attempt, of how many,
    was in flight) was unobservable by construction, and a session read that
    silence as broken log plumbing rather than absent instrumentation. This
    assertion is what stops that silence coming back unnoticed.

    A driver with neither reader (web — the browser keeps no client-side log
    file, and its wasm build has no stderr layer) cannot answer, so there is
    nothing to assert there; that is a driver-capability absence, not a skipped
    assertion — every app that HAS a log must show the lines.
    """
    attr, text = _app_own_log(app.driver)
    if attr is None:
        return
    assert "fauna_provisioning" in text and "attempt" in text, (
        "The orchestrator ran (this test drives it to Succeeded), so run_step's "
        "per-attempt narration must appear in the app's own log. Not found in "
        f"{attr}() — either the shared instrumentation regressed, or this app's "
        "tracing no longer reaches its log. Log was "
        f"{len(text.splitlines())} line(s); tail:\n"
        + "\n".join(text.splitlines()[-40:])
    )


def _step(kind, *, status='Pending', substep=None, attempt=0,
          max_attempts=3, last_error=None, skip_reason=None,
          started_at_ms=None, finished_at_ms=None):
    return {
        'kind': kind,
        'status': status,
        'substep': substep,
        'attempt': attempt,
        'max_attempts': max_attempts,
        'last_error': last_error,
        'skip_reason': skip_reason,
        'started_at_ms': started_at_ms,
        'finished_at_ms': finished_at_ms,
    }


def _snapshot(*, overall='Idle', steps=None,
              started_at_ms=None, finished_at_ms=None,
              result=None, final_error=None):
    if steps is None:
        steps = [_step(k) for k in ('Domain', 'Server', 'Dns', 'Online')]
    return {
        'overall': overall,
        'steps': steps,
        'started_at_ms': started_at_ms,
        'finished_at_ms': finished_at_ms,
        'result': result,
        'final_error': final_error,
    }


# ── Static rendering ──────────────────────────────────────────────────


def test_idle_renders_four_pending_rows(app):
    """Snapshot's `idle()` constructor: all four rows Pending, no
    cancel/retry, Continue disabled."""
    set_provisioning_snapshot(app, _snapshot(overall='Idle'))
    assert app.is_visible('provisioning-progress'), (
        "Idle snapshot should render the provisioning-progress page: "
        f"{app.driver.diagnose('provisioning-progress')}"
    )
    assert app.count('provisioning-step-row') == 4, (
        "the page should render exactly 4 step rows (Domain/Server/Dns/Online); "
        f"got {app.count('provisioning-step-row')}: "
        f"{app.driver.diagnose('provisioning-step-row')}"
    )
    # Step labels render in fixed order.
    assert S.onboarding.provision.step.domain in app.get_text('provisioning-step-label', scope='provisioning-step-row[0]'), (
        "row 0 label should be 'Domain': "
        f"{app.driver.diagnose('provisioning-step-label', scope='provisioning-step-row[0]')}"
    )
    assert S.onboarding.provision.step.server in app.get_text('provisioning-step-label', scope='provisioning-step-row[1]'), (
        "row 1 label should be 'Server': "
        f"{app.driver.diagnose('provisioning-step-label', scope='provisioning-step-row[1]')}"
    )
    assert S.onboarding.provision.step.dns    in app.get_text('provisioning-step-label', scope='provisioning-step-row[2]'), (
        "row 2 label should be 'DNS': "
        f"{app.driver.diagnose('provisioning-step-label', scope='provisioning-step-row[2]')}"
    )
    assert S.onboarding.provision.step.online in app.get_text('provisioning-step-label', scope='provisioning-step-row[3]'), (
        "row 3 label should be 'Online': "
        f"{app.driver.diagnose('provisioning-step-label', scope='provisioning-step-row[3]')}"
    )
    # No cancel/retry in Idle.
    assert app.is_absent('provisioning-cancel-button'), (
        "no Cancel button should show in Idle: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    assert app.is_absent('provisioning-retry-button'), (
        "no Retry button should show in Idle: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    # Continue exists but disabled.
    assert app.is_visible('provisioning-continue-button'), (
        "Continue button should exist in Idle: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )
    assert not app.is_enabled('provisioning-continue-button'), (
        "Continue should be disabled in Idle (only enabled on Succeeded): "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )


def test_running_shows_cancel_and_substep(app):
    """`overall == Running` and the matching step.status == Running:
    Cancel button visible, substep text rendered."""
    steps = [
        _step('Domain', status='Skipped', substep='StatusSkipped'),
        _step('Server', status='Running', substep='ServerCreating',
              attempt=1, max_attempts=3, started_at_ms=10_000),
        _step('Dns'),
        _step('Online'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Running', steps=steps, started_at_ms=10_000,
    ))
    assert app.is_visible('provisioning-cancel-button'), (
        "Running overall should show the Cancel button: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    assert app.is_enabled('provisioning-cancel-button'), (
        "the Cancel button should be enabled while Running: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    assert app.is_absent('provisioning-retry-button'), (
        "no Retry button should show while Running: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    # Substep text on the Running row uses the i18n value, not the raw
    # SubstepKey.
    sub = app.get_text('provisioning-substep', scope='provisioning-step-row[1]')
    assert S.onboarding.provision.substep.server_creating in sub, (
        "Running Server row should show the i18n substep 'Creating server'; "
        f"got {sub!r}: "
        f"{app.driver.diagnose('provisioning-substep', scope='provisioning-step-row[1]')}"
    )
    # Continue stays disabled.
    assert not app.is_enabled('provisioning-continue-button'), (
        "Continue should stay disabled while Running: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )


def test_running_with_retry_attempt_shows_attempt_counter(app):
    """When attempt > 1 and max_attempts > 1, substep gets the
    "(attempt N of M)" suffix per `step_attempt_template`."""
    steps = [
        _step('Domain', status='Succeeded'),
        _step('Server', status='Succeeded'),
        _step('Dns', status='Running', substep='DnsAddingDomainRecords',
              attempt=2, max_attempts=3),
        _step('Online'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Running', steps=steps, started_at_ms=20_000,
    ))
    sub = app.get_text('provisioning-substep', scope='provisioning-step-row[2]')
    assert S.onboarding.provision.substep.dns_adding_domain_records in sub, (
        "Running Dns row should show the i18n substep 'Adding domain records'; "
        f"got {sub!r}: "
        f"{app.driver.diagnose('provisioning-substep', scope='provisioning-step-row[2]')}"
    )
    # Attempt counter appended.
    assert '2' in sub and '3' in sub, (
        "substep should carry the '(attempt 2 of 3)' counter when attempt>1; "
        f"got {sub!r}"
    )


def test_skipped_step_shows_skipped_substep(app):
    """`StatusSkipped` substep maps to "Already configured — skipped"."""
    steps = [
        _step('Domain', status='Skipped', substep='StatusSkipped',
              skip_reason='ZoneAlreadyVerified'),
        _step('Server'),
        _step('Dns'),
        _step('Online'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Running', steps=steps, started_at_ms=30_000,
    ))
    sub = app.get_text('provisioning-substep', scope='provisioning-step-row[0]')
    assert S.onboarding.provision.substep.status_skipped in sub, (
        "StatusSkipped substep should map to 'Already configured — skipped'; "
        f"got {sub!r}: "
        f"{app.driver.diagnose('provisioning-substep', scope='provisioning-step-row[0]')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_failed_renders_error_and_retry_button(app):
    """`overall == Failed` + a step's `last_error` populated: error line
    rendered on that row, run-level Retry button visible."""
    steps = [
        _step('Domain', status='Succeeded'),
        _step('Server', status='Failed', substep='ServerCreating',
              attempt=3, max_attempts=3,
              last_error='provider returned 500: insufficient capacity'),
        _step('Dns'),
        _step('Online'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Failed', steps=steps,
        started_at_ms=40_000, finished_at_ms=42_500,
        final_error='Step Server failed: provider returned 500: insufficient capacity',
    ))
    assert app.is_visible('provisioning-retry-button'), (
        "Failed overall should show the Retry button: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    assert app.is_enabled('provisioning-retry-button'), (
        "the Retry button should be enabled on Failed: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    assert app.is_absent('provisioning-cancel-button'), (
        "no Cancel button should show on Failed: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    err = app.get_text('provisioning-step-error', scope='provisioning-step-row[1]')
    assert 'insufficient capacity' in err, (
        "the failed Server row should render its last_error text; "
        f"got {err!r}: "
        f"{app.driver.diagnose('provisioning-step-error', scope='provisioning-step-row[1]')}"
    )
    # Continue stays disabled on Failed.
    assert not app.is_enabled('provisioning-continue-button'), (
        "Continue should stay disabled on Failed: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )


def test_succeeded_enables_continue_hides_cancel_and_retry(app):
    """Terminal-success snapshot: Continue is the only action."""
    steps = [
        _step('Domain', status='Succeeded'),
        _step('Server', status='Succeeded'),
        _step('Dns', status='Succeeded'),
        _step('Online', status='Succeeded'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Succeeded', steps=steps,
        started_at_ms=50_000, finished_at_ms=80_000,
        result={
            'server_id': 'srv-1',
            'ipv4': '203.0.113.10',
            'domain': 'example.com',
            'claim_code': 'claim-xyz',
        },
    ))
    assert app.is_enabled('provisioning-continue-button'), (
        "Succeeded should enable Continue: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )
    assert app.is_absent('provisioning-cancel-button'), (
        "no Cancel button should show on Succeeded: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    assert app.is_absent('provisioning-retry-button'), (
        "no Retry button should show on Succeeded: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_continue_on_a_claimed_succeeded_run_lands_on_nat_mode_choice(app):
    """Continue on a `Succeeded` standard-path run lands on `nat_mode_choice`
    (onboarding.md § 6 *Provisioning = build + claim*, ratified 2026-08-29):
    `Succeeded` there means built **and claimed**, so this page never exits
    to `Done`/`LoggedIn` — the § 3b-bis tail owns the exit, exactly as after
    a claim-code submit.

    Cross-app twin of the routing half of
    `tests/web/test_provisioning_continue_to_nat_mode.py` (web-only for its
    `window.location` assertion), driven through the ONE action every flow
    that provisioned a real box uses to leave this page —
    `OnboardingActions.continue_from_provisioning` — so the live Hetzner
    suites' wizard tail (`helpers/live_provision.py::finish_provisioned_wizard`)
    has a headless witness on every app. The claim is stood in for with
    `set_claim_completed_for_test` (there is no nest here to claim; the
    end-to-end claim is the live test's), and the snapshot is fixture-driven
    rather than orchestrated on purpose: a fake-cloud run's own claiming
    substep can only fail against a fake with no WS-RPC nest, which puts
    `overall` back to `Failed` before any click.
    """
    app.driver.call_machine_method("set_current_handle", json.dumps("alice@example.test"))
    steps = [
        _step('Domain', status='Succeeded'),
        _step('Server', status='Succeeded'),
        _step('Dns', status='Succeeded'),
        _step('Online', status='Succeeded'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Succeeded', steps=steps,
        started_at_ms=50_000, finished_at_ms=80_000,
        result={
            'server_id': 'srv-1',
            'ipv4': '203.0.113.10',
            'domain': 'example.test',
            'claim_code': 'claim-xyz',
        },
    ))
    # The box a real run would have claimed by now. Continue REFUSES an
    # unclaimed box on this path (the state gate, not a timing one).
    app.driver.call_machine_method("set_claim_completed_for_test", json.dumps(True))

    app.onboarding.continue_from_provisioning()

    assert app.onboarding.nat_mode_showing(), (
        "Continue on a claimed Succeeded run must land on nat_mode_choice "
        "(onboarding.md § 6), not stay on the provisioning page or exit: "
        f"{app.driver.diagnose('nat-mode-confirm-button')} "
        f"error={app.error_text()!r}"
    )


def test_cancelled_shows_retry(app):
    """Soft-cancel terminal: no Cancel (already done), but Retry IS shown
    so the user can resume the stopped run (onboarding.md §6 — Retry is
    visible for Failed *or* Cancelled; idempotency skips completed steps).
    Continue stays disabled until the resumed run succeeds. Without this a
    cancelled run would strand the user with only Back."""
    steps = [
        _step('Domain', status='Succeeded'),
        _step('Server', status='Failed', substep='StatusCancelled',
              last_error='Cancelled'),
        _step('Dns'),
        _step('Online'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Cancelled', steps=steps,
        started_at_ms=60_000, finished_at_ms=61_000,
        final_error='Cancelled',
    ))
    assert app.is_absent('provisioning-cancel-button'), (
        "a Cancelled terminal run should not show Cancel (already done): "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )
    assert app.is_visible('provisioning-retry-button'), (
        "a Cancelled run should show Retry so the user can resume: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    assert not app.is_enabled('provisioning-continue-button'), (
        "Continue should stay disabled until the resumed run succeeds: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )


def test_elapsed_renders_when_started(app):
    """`provisioning-elapsed` becomes visible once `started_at_ms` is set."""
    set_provisioning_snapshot(app, _snapshot(
        overall='Running',
        steps=[
            _step('Domain', status='Running', substep='DomainVerifyingZone',
                  attempt=1, max_attempts=3, started_at_ms=70_000),
            _step('Server'), _step('Dns'), _step('Online'),
        ],
        started_at_ms=70_000,
    ))
    assert app.is_visible('provisioning-elapsed'), (
        "provisioning-elapsed should be visible once started_at_ms is set: "
        f"{app.driver.diagnose('provisioning-elapsed')}"
    )


# ── Top-region price summary ("Bill of Materials") ──────────────────────
#
# `bill_of_materials()`/`set_dns_availability_for_test` are cross-app in
# shared Rust; the marker list below is dropped to `[]` once every app has
# landed its own leg of this same page region. Linux, tui, web, macOS, and
# iOS (all 2026-07-20/22) render it; Windows/Android still owe theirs.


def _land_on_provisioning(app):
    """Land the wizard at nest_provisioning without seeding any snapshot —
    just enough for the page (and its always-present `provisioning-price-bom`
    container) to render."""
    app.driver.call_machine_method("set_step_for_test", json.dumps("NestProvisioning"))


def _seed_vps_selection(app, *, price_monthly_cents=451, currency='EUR'):
    app.driver.call_machine_method(
        "set_vps_state_for_test",
        json.dumps({
            "provider_id": "hetzner",
            "server_types": [{
                "id": "cax11", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
                "price_monthly_cents": price_monthly_cents, "currency": currency,
            }],
            "selected_server_type_id": "cax11",
        }),
    )


@pytest.mark.linux
@pytest.mark.tui
def test_bom_vps_line_renders_selected_price_domain_line_hidden(app):
    """`provisioning-bom-vps-line` always renders the selected server
    type's monthly price once vps_config has a selection; the domain line
    stays hidden when the wizard isn't buying a new domain (onboarding.md
    §6 — `bill_of_materials()`'s default/no-buy-domain shape)."""
    _land_on_provisioning(app)
    _seed_vps_selection(app, price_monthly_cents=451, currency='EUR')

    assert app.is_visible('provisioning-price-bom'), (
        "the price-summary container should always render on nest_provisioning: "
        f"{app.driver.diagnose('provisioning-price-bom')}"
    )
    assert app.is_visible('provisioning-bom-vps-line'), (
        "the VPS line should render once a server type is selected: "
        f"{app.driver.diagnose('provisioning-bom-vps-line')}"
    )
    vps_text = app.get_text('provisioning-bom-vps-line')
    assert S.onboarding.provision.step.server in vps_text, (
        f"VPS line should carry the same step label the progress row below it uses; got {vps_text!r}"
    )
    assert '4.51' in vps_text and 'EUR' in vps_text, (
        f"VPS line should show the selected server type's price; got {vps_text!r}"
    )
    assert app.is_absent('provisioning-bom-domain-line'), (
        "no domain line should render when the wizard isn't buying a new domain: "
        f"{app.driver.diagnose('provisioning-bom-domain-line')}"
    )


@pytest.mark.linux
@pytest.mark.tui
def test_bom_domain_line_renders_when_buying_a_new_domain(app):
    """Both lines render together when the wizard is buying a new domain
    AND a VPS server type is selected — the domain line's price mirrors
    what `dns-tld-price-display` already showed and the user already
    agreed to earlier in the wizard."""
    _land_on_provisioning(app)
    _seed_vps_selection(app, price_monthly_cents=899, currency='EUR')
    app.driver.call_machine_method(
        "set_dns_availability_for_test",
        json.dumps({"buy_domain": True, "price_cents": 1099, "currency": "EUR"}),
    )

    assert app.is_visible('provisioning-bom-domain-line'), (
        "the domain line should render once the wizard is buying a new domain: "
        f"{app.driver.diagnose('provisioning-bom-domain-line')}"
    )
    domain_text = app.get_text('provisioning-bom-domain-line')
    assert S.onboarding.provision.step.domain in domain_text, (
        f"domain line should carry the same step label the progress row below it uses; got {domain_text!r}"
    )
    assert '10.99' in domain_text and 'EUR' in domain_text, (
        f"domain line should show the quoted registration price; got {domain_text!r}"
    )
    vps_text = app.get_text('provisioning-bom-vps-line')
    assert '8.99' in vps_text, (
        f"VPS line should still show its own (different) price alongside the domain line; got {vps_text!r}"
    )


def test_deferred_dns_path_shows_three_skipped(app):
    """`provision_nest_no_dns` path: only Server is Succeeded; Domain,
    Dns, Online are Skipped (no domain registration / DNS publishing /
    health poll). Page handles this via the same four-row component."""
    steps = [
        _step('Domain', status='Skipped', substep='StatusSkipped',
              skip_reason='ZoneAlreadyVerified'),
        _step('Server', status='Succeeded'),
        _step('Dns', status='Skipped', substep='StatusSkipped'),
        _step('Online', status='Skipped', substep='StatusSkipped'),
    ]
    set_provisioning_snapshot(app, _snapshot(
        overall='Succeeded', steps=steps,
        started_at_ms=80_000, finished_at_ms=82_000,
    ))
    # All four rows still render; Continue enabled.
    assert app.count('provisioning-step-row') == 4, (
        "the deferred-DNS path should still render all 4 step rows; "
        f"got {app.count('provisioning-step-row')}: "
        f"{app.driver.diagnose('provisioning-step-row')}"
    )
    assert app.is_enabled('provisioning-continue-button'), (
        "the deferred-DNS Succeeded path should enable Continue: "
        f"{app.driver.diagnose('provisioning-continue-button')}"
    )


# ── Top-region price summary ("Bill of Materials", onboarding.md §6) ────


def _set_vps_state(app, *, price_monthly_cents=600, currency='EUR'):
    app.driver.call_machine_method('set_vps_state_for_test', json.dumps({
        'provider_id': 'hetzner',
        'server_types': [{
            'id': 'cax11', 'vcpu': 2, 'mem_gb': 4.0, 'disk_gb': 40,
            'price_monthly_cents': price_monthly_cents, 'currency': currency,
        }],
        'selected_server_type_id': 'cax11',
    }))


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.windows
def test_price_bom_shows_vps_line_only_by_default(app):
    """The VPS line is always present once a server type is selected
    (vps_config's Continue requires one); the domain line is absent when
    the wizard isn't buying a new domain (dns_config's buy-domain path not
    taken)."""
    set_provisioning_snapshot(app, _snapshot(overall='Idle'))
    _set_vps_state(app, price_monthly_cents=600, currency='EUR')
    # Convention 14 — `call_machine_method` acks receipt, not the re-render it
    # triggers (`e2e-latency-independent-assertions.md` § The convention —
    # convention 14), so an instant `is_visible` right after it races the bind
    # under load. Poll the state asserted, not a proxy.
    wait_until(
        lambda: app.is_visible('provisioning-price-bom'),
        UI_SETTLE_S,
        diagnose=lambda: (
            "the price-bom container never rendered after a VPS server type "
            f"was selected: {app.driver.diagnose('provisioning-price-bom')}"
        ),
    )
    wait_until(
        lambda: app.is_visible('provisioning-bom-vps-line'),
        UI_SETTLE_S,
        diagnose=lambda: (
            "the VPS line never appeared after a VPS server type was "
            f"selected: {app.driver.diagnose('provisioning-bom-vps-line')}"
        ),
    )
    assert app.is_absent('provisioning-bom-domain-line'), (
        "the domain line should be absent when buy_domain wasn't set: "
        f"{app.driver.diagnose('provisioning-bom-domain-line')}"
    )
    text = app.get_text('provisioning-bom-vps-line')
    assert '6.00' in text and 'EUR' in text, (
        f"the VPS line should render the formatted monthly price; got {text!r}: "
        f"{app.driver.diagnose('provisioning-bom-vps-line')}"
    )
    assert S.onboarding.provision.step.server in text, (
        f"the VPS line should render the shared step label 'Server'; got {text!r}"
    )
    assert 'month' in text.lower(), (
        f"the recurring VPS line should carry a monthly-recurrence cue; got {text!r}"
    )


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_price_bom_shows_both_lines_when_buying_domain(app):
    """The domain line appears alongside the VPS line when the wizard is
    buying a new domain — up to two line items total, domain (one-time)
    then VPS (recurring)."""
    set_provisioning_snapshot(app, _snapshot(overall='Idle'))
    _set_vps_state(app, price_monthly_cents=600, currency='EUR')
    app.driver.call_machine_method('set_dns_availability_for_test', json.dumps({
        'provider_id': 'cloudflare',
        'buy_domain': True,
        'price_cents': 500,
        'currency': 'EUR',
    }))
    # Same instant-sample shape as the VPS-only test above: the DNS-availability
    # fixture push acks before the re-render lands, so poll instead of sampling
    # (convention 14).
    wait_until(
        lambda: app.is_visible('provisioning-bom-domain-line'),
        UI_SETTLE_S,
        diagnose=lambda: (
            "the domain line never appeared after buying a new domain was "
            f"set: {app.driver.diagnose('provisioning-bom-domain-line')}"
        ),
    )
    domain_text = app.get_text('provisioning-bom-domain-line')
    assert '5.00' in domain_text and 'EUR' in domain_text, (
        f"the domain line should render the formatted one-time price; got {domain_text!r}: "
        f"{app.driver.diagnose('provisioning-bom-domain-line')}"
    )
    assert S.onboarding.provision.step.domain in domain_text, (
        f"the domain line should render the shared step label 'Domain'; got {domain_text!r}"
    )
    assert 'month' not in domain_text.lower(), (
        f"the one-time domain line must not carry the recurring monthly cue; got {domain_text!r}"
    )
    assert app.is_visible('provisioning-bom-vps-line'), (
        "the VPS line should still be present alongside the domain line: "
        f"{app.driver.diagnose('provisioning-bom-vps-line')}"
    )


@pytest.mark.feature("set-up-a-nest-from-the-app")
def test_cancel_mid_online_then_retry_succeeds(app, fake_cloud, provision_target_nest):
    """Full-orchestrator cancel→retry (onboarding.md §6 click-handler table).
    The wizard runs the real shared orchestrator with the VPS/DNS HTTP
    redirected at the Python fake and the **nest leg pointed at a real, never-
    claimed `fauna-nest`** — the box the run is building. It starts STOPPED, so
    the Online step's health poll fails and parks in its 5s retry backoff,
    giving a deterministic window to Cancel (soft-cancel → overall ==
    Cancelled, resources intact); the box then comes up and Retry re-runs —
    idempotency replays the built steps, the health poll answers, and the
    run's own claiming substep claims the box for real, which is what
    `Succeeded` means on this path.

    ⚠ **The real nest is not decoration.** Before `Succeeded` meant "built AND
    claimed" (§ 6, 2026-08-29) a pure fake could carry this journey; now it
    cannot — a fake cloud serves no WS-RPC, the claim fails, and `Online` lands
    `Failed` with `Continue` never enabling except in a sub-millisecond window
    (a timing dependence, convention 14). Faking the nest here would make the
    final assertion unreachable, not merely shallow.

    Cross-app: the provider-base-url redirection is delivered through each
    driver's `set_provider_base_urls`. Web rides the
    `?fauna_e2e_provider_base_urls` query-param channel (reconstructs the wizard
    machine on reload); native apps (linux/…) route through the
    `call_machine_method` bridge to the shared machine's runtime
    `set_provider_base_urls` setter (Track 2)."""
    drv = app.driver
    # VPS/DNS at the fake; the nest leg at the real binary.
    point_providers_at(
        app, fake_cloud, nest_base_url=provision_target_nest["url"], nest=provision_target_nest
    )
    # The box is not up yet — the state the Online step exists to wait out. Its
    # health poll therefore fails and parks in the 5s retry backoff, which is
    # the window to Cancel in. Brought back after the cancel lands, so Retry
    # drives the run to Succeeded.
    stop_nest(provision_target_nest)

    # Everything `run_provisioning_inner` reads, so the run reaches Online
    # without driving the real verify_dns/verify_vps probes — and the box's own
    # claim code, which the harness wrote before boot, so the run presents that
    # one rather than minting a code the box never heard of.
    seed_standard_path_run(
        app,
        server_type_id="cax11",
        claim_code=provision_target_nest["claim_code"],
    )

    # Start the real orchestrator.
    app.click("provisioning-start-button")

    # Wait until the Online step (row index 3) is actively running. Poll the
    # live snapshot so a stall/failure surfaces the exact step + error.
    last = {"snap": None}

    def online_running():
        snap = drv.call_machine_method("provisioning_snapshot")
        last["snap"] = snap
        if not isinstance(snap, dict):
            return False
        for s in snap.get("steps", []):
            if s.get("kind") == "Online" and s.get("status") == "Running":
                return True
        return False

    wait_until(
        online_running,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "Online step never reached Running. snapshot=" + json.dumps(last["snap"]),
    )
    assert app.is_visible("provisioning-cancel-button"), (
        "Cancel button should be visible while Online is Running: "
        f"{app.driver.diagnose('provisioning-cancel-button')} "
        f"snapshot={json.dumps(last['snap'])}"
    )

    # Cancel mid-Online → soft-cancel → overall == Cancelled; the page
    # shows Retry (Failed-or-Cancelled), Continue stays disabled.
    app.click("provisioning-cancel-button")

    def cancelled_with_retry():
        last["snap"] = drv.call_machine_method("provisioning_snapshot")
        return app.is_visible("provisioning-retry-button")

    wait_until(
        cancelled_with_retry,
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "Retry button never appeared after Cancel. snapshot=" + json.dumps(last["snap"]),
    )
    assert not app.is_enabled("provisioning-continue-button"), (
        "Continue should stay disabled after Cancel (run not Succeeded): "
        f"{app.driver.diagnose('provisioning-continue-button')} "
        f"snapshot={json.dumps(last['snap'])}"
    )

    # The box comes up. Retry resumes; idempotency replays the earlier steps,
    # the health poll answers, and the claiming substep claims the real box —
    # `Succeeded` on this path means built AND claimed, so Continue enabling
    # below is the claim landing, not just the build finishing.
    start_nest_in_place(provision_target_nest)
    app.click("provisioning-retry-button")
    wait_until(
        lambda: app.is_enabled("provisioning-continue-button"),
        ORCHESTRATION_STEP_S,
        diagnose=lambda: "Provisioning never reached Succeeded after Retry",
    )
    assert app.is_absent("provisioning-retry-button"), (
        "Retry button should be gone once the resumed run Succeeded: "
        f"{app.driver.diagnose('provisioning-retry-button')}"
    )
    assert app.is_absent("provisioning-cancel-button"), (
        "Cancel button should be gone once the resumed run Succeeded: "
        f"{app.driver.diagnose('provisioning-cancel-button')}"
    )

    # The run above is the only free path that drives the REAL orchestrator, so
    # it is the one place the per-attempt narration can be proven end-to-end
    # without spending a live box.
    _assert_orchestrator_narrated_its_attempts(app)
