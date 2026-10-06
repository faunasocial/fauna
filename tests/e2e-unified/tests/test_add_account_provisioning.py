"""tier_3: an "Add account" that sets up its own nest holds custody without hijacking.

`docs/goal/behavior/onboarding.md` § Multi-account → *Append-mode
deferred/incomplete states + abandonment* (the provisioning-custody carve-out,
2026-09-25): the append wizard's confirm writes nothing, and the ONE mid-run
write — a provisioning run's claim-code mint before `create_server`
(`onboarding-provisioning.md` § 6 *The pending-provision slot*, custody precedes
dispatch) — registers the appended identity **inactive** beside its slot and
never moves the active pointer. The pointer moves at the wizard's terminal, as
every other append write does. Two consequences, one journey each:

**Journey 1 — a run that never finished.** The live account is the nest's own
admin (so `admin-tab` is a clean "still routed to #1" discriminator). Inside
"Add account" a standard-path run mints, then `create_server` 500s. Mid-run the
registry lists two accounts with the ADMIN still active and the appended row
holding its slot — the state the app's abandon handler re-dispatches over, so
this IS the no-hijack assertion. A quit-and-relaunch (the abandon a user cannot
undo) routes onto the admin's session with its rows intact, and the switcher
lists the appended identity as the resumable custody of a box that may be
billing: selecting it relaunches over its slot onto the records-less "Almost
ready" surface. Before the fix, the mint's `set_active` made the appended
identity active mid-run, and the relaunch landed on ITS "Almost ready" with the
admin's session gone — indistinguishable, from the user's chair, from being
switched to a stranger's half-built account.

**Journey 2 — the deferred-DNS exit is the terminal.** The same append, DNS
"Set up later", the fake box built: the wizard exits `AwaitingManualDns`, and
the append glue's terminal (`persist_awaiting_dns` + the app's switch) is where
the appended identity becomes active — on its "Almost ready" surface WITH the
records to paste, which a relaunch finds again, the previous account still listed
with its rows intact.

Both ride the client's file-backed store pinned across a real process relaunch
(`preserve_state_across_relaunch()`); the mid-run and post-terminal registry
states are read off that file (`helpers/registry_store.py`), independent of any
reconnect. tier_3: a real `fauna-nest` for the live account (`nest_instance`),
the fake cloud for the box. App-agnostic — the switcher, wizard and "Almost
ready" IDs are the same seven-app set — parametrized like `app` over the
selected apps; the run itself is the shared machine's, so the assertion is the
same on every app that installs the shared slot store.
"""
from __future__ import annotations

import json
import time

import pytest
from nacl.signing import SigningKey

from actions import ActionLayer
from common import build_registry_seed
from conftest import _build_app_config, _seeded_environment, get_available_apps
from drivers import create_driver
from helpers.budgets import APP_RELAUNCH_S, ORCHESTRATION_STEP_S, UI_SETTLE_S
from helpers.provisioning_drive import (
    HANDLE_DOMAIN,
    assert_records_less_almost_ready,
    overall,
    point_providers_at,
    require_durable_client_store,
    seed_standard_path_run,
    step_status,
)
from helpers.registry_store import read_registry_index, read_store_slot
from helpers.waiting import wait_registry_index, wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.crash_recovery]

# tests/e2e-unified/ui.yaml § settings (switcher) + navigation (admin-tab) —
# the same IDs every switcher module reads.
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ACTIVE_INDICATOR = "account-item-active-indicator"
ADD_BUTTON = "account-add-button"
ADMIN_TAB = "admin-tab"
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}


@pytest.fixture(params=get_available_apps())
def app_name(request):
    """The app under test, parametrized exactly as `app` is so `--app` selects
    it — but launched by the journey itself, seeded with a live account, which
    the session-cached `app` cannot be."""
    return request.param


def _appended_actor_id(app: ActionLayer) -> str:
    """The identity `seed_standard_path_run` seeds onto the machine — the one
    the append will register — derived the way the registry derives it."""
    secret = bytes.fromhex(app.onboarding._IMPORT_KEY_FOR_HANDLE_TESTS)
    return bytes(SigningKey(secret).verify_key).hex()


def _admin_seed(nest_instance):
    """The nest's own claimed ADMIN as the single, active registry row — the
    live account the append runs over. `admin-tab` then discriminates "routed
    to #1" from "routed to the appended identity" without a second read."""
    admin_sk = nest_instance["admin"]["signing_key"]
    actor = bytes(admin_sk.verify_key).hex()
    seed = build_registry_seed(
        [{"actor_id": actor, "secret_hex": bytes(admin_sk).hex(),
          "nest_url": nest_instance["url"], "device_id": "add-account-provision",
          "handle": "admin"}],
        active=actor,
    )
    return seed, actor


def _launch_live_admin(app_name, nest_instance, request):
    """Launch the app under test signed in as the admin, on a store the journey
    will pin across a relaunch. Returns the action layer."""
    config = _build_app_config(app_name, nest_instance, request)
    seed, _ = _admin_seed(nest_instance)
    config["seed_credentials"] = seed
    config["environment"] = {
        **config.get("environment", {}),
        **_seeded_environment(request, nest_instance),
    }
    driver = create_driver(app_name)
    driver.launch(config)
    app = ActionLayer(driver)
    _await_admin_session(driver, after="at the first launch")
    return app


def _await_admin_session(driver, *, after: str) -> None:
    """The authenticated shell, as the admin: `admin-tab` present."""
    driver.wait_for_state(
        lambda s: bool(s.get("session", {}).get("authenticated")), timeout=APP_RELAUNCH_S
    )
    try:
        driver.wait_for(ADMIN_TAB, timeout=APP_RELAUNCH_S)
    except Exception as exc:
        raise AssertionError(
            f"{after} the app did not come up as the ADMIN (no admin-tab) — the "
            f"launch routed onto some other account: " + driver.diagnose(ADMIN_TAB)
        ) from exc


def _wait_switcher_count(driver, n, timeout=APP_RELAUNCH_S):
    """Poll the registry-backed switcher UI until it lists exactly ``n`` accounts
    with one active — re-navigating each cycle, since a switch tears the shell
    down (the shape every switcher module shares)."""
    deadline = time.monotonic() + timeout
    last = -1
    while time.monotonic() < deadline:
        try:
            driver.set_state(ACCOUNT_PAGE_NAV)
            driver.wait_for(SWITCHER_LIST, timeout=5)
            last = driver.count(SWITCHER_ITEM)
            if last == n and driver.count(ACTIVE_INDICATOR) == 1:
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise AssertionError(
        f"switcher never reached {n} accounts (one active) within {timeout}s; "
        f"last SWITCHER_ITEM count={last}"
    )


def _enter_add_account(driver) -> None:
    """"Add account" from the Account settings page — the append wizard mounts
    at identity_choice over the live session."""
    _wait_switcher_count(driver, 1)
    driver.click(ADD_BUTTON)
    driver.wait_for("import-identity-button", timeout=UI_SETTLE_S)


def _relaunch_on_the_same_store(app: ActionLayer) -> None:
    """Quit and relaunch reading the SAME pinned store (no re-seed): the abandon
    a user performs by closing the app, and the cold launch routing that then
    runs."""
    require_durable_client_store(app)
    driver = app.driver
    driver.teardown()
    relaunch = dict(driver._launch_config)
    relaunch.pop("seed_credentials", None)
    driver.launch(relaunch)


def _assert_live_rows_intact(driver, admin_actor: str, *, after: str) -> None:
    """The account the user was adding FROM keeps its rows: the per-actor
    `nest_url` slot is what its next launch's silent challenge dials."""
    assert read_store_slot(driver, f"fauna/{admin_actor}/nest_url"), (
        f"{after} the live account's nest_url slot is gone — the append disturbed "
        f"the rows of the identity the user was adding from"
    )


@pytest.mark.feature("multiple-accounts")
def test_an_add_account_provisioning_run_holds_custody_without_hijacking_the_live_session(
    app_name, nest_instance, fake_cloud, request
):
    """Journey 1 (module docstring): mint → `create_server` 500s → the registry
    holds custody with the ADMIN still active → quit + relaunch routes to the
    admin → the appended identity waits in the switcher and resumes on "Almost
    ready" when selected."""
    app = _launch_live_admin(app_name, nest_instance, request)
    driver = app.driver
    _, admin_actor = _admin_seed(nest_instance)
    try:
        # (1) "Add account" over the live session, then a standard-path run
        # whose box order fails: the mint has already written custody.
        _enter_add_account(driver)
        fake_cloud.hetzner_cloud.create_server_always_fails()
        point_providers_at(app, fake_cloud)
        seed_standard_path_run(app, handle_domain=HANDLE_DOMAIN)
        appended = _appended_actor_id(app)
        assert appended != admin_actor
        app.click("provisioning-start-button")
        wait_until(
            lambda: step_status(app, "Server") == "Failed",
            ORCHESTRATION_STEP_S,
            diagnose=lambda: (
                "the Server step never failed, so the fake's 500 did not reach "
                f"the orchestrator and this journey proves nothing. overall={overall(app)!r}"
            ),
        )
        assert overall(app) == "Failed"

        # (2) THE DECIDED MID-RUN STATE: custody, never routing. Two rows —
        # the appended identity registered beside its pending-provision slot
        # (a paid box's claim code exists nowhere else) — and the ADMIN still
        # active: what the app's abandon handler re-dispatches over, and what
        # the next cold launch routes on. Before the fix this read `active ==
        # appended` — the hijack.
        index = wait_registry_index(
            lambda: read_registry_index(driver), 2, active=admin_actor, budget_s=UI_SETTLE_S
        )
        rows = [a["actor_id"] for a in index["accounts"]]
        assert rows == [admin_actor, appended], (
            "the mint must register the appended identity beside the live one, "
            f"in add order; rows={rows!r}"
        )
        slot = read_store_slot(driver, f"fauna/{appended}/awaiting_dns")
        assert slot, (
            "custody precedes dispatch: the appended identity's pending-provision "
            "slot must be on disk before the box was ever ordered"
        )
        assert json.loads(slot).get("claim_code"), f"the slot carries no claim code: {slot!r}"
        assert index["active"] == admin_actor, (
            "a mid-run custody write must never move the active pointer off the "
            f"account the user is adding from; index={index!r}"
        )
        _assert_live_rows_intact(driver, admin_actor, after="mid-run,")

        # (3) Abandon by quitting, then relaunch on the same store: the cold
        # launch routes onto the ADMIN's session — no hijack — and the
        # appended identity is listed, inactive, as the custody of its box.
        _relaunch_on_the_same_store(app)
        _await_admin_session(driver, after="after abandoning the append by quitting,")
        restored = driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == admin_actor,
            timeout=APP_RELAUNCH_S,
        )
        assert restored["session"]["actor_id"] == admin_actor
        _assert_live_rows_intact(driver, admin_actor, after="after the relaunch,")
        _wait_switcher_count(driver, 2)
        index = read_registry_index(driver)
        assert index["active"] == admin_actor and len(index["accounts"]) == 2, (
            f"the relaunch must not have re-seated the registry; index={index!r}"
        )

        # (4) Selecting the appended row is the foreground resume: the switch
        # relaunches over ITS slot onto the records-less "Almost ready" surface
        # (the box never existed, so nothing to paste and nothing to dial).
        driver.click(SWITCHER_ITEM, index=rows.index(appended))
        wait_registry_index(
            lambda: read_registry_index(driver), 2, active=appended, budget_s=APP_RELAUNCH_S
        )
        assert_records_less_almost_ready(
            app, after="after selecting the appended identity in the switcher,"
        )
        _assert_live_rows_intact(driver, admin_actor, after="on the appended resume,")
    finally:
        driver.teardown()


@pytest.mark.feature("multiple-accounts")
def test_an_add_account_deferred_dns_exit_switches_to_the_appended_identity_and_survives_a_relaunch(
    app_name, nest_instance, fake_cloud, request
):
    """Journey 2 (module docstring): DNS deferred, the fake box built, the
    `AwaitingManualDns` exit is the append's terminal — active moves to the
    appended identity on its "Almost ready" surface WITH records, found again
    after a relaunch, the previous account still listed with its rows intact."""
    app = _launch_live_admin(app_name, nest_instance, request)
    driver = app.driver
    _, admin_actor = _admin_seed(nest_instance)
    try:
        _enter_add_account(driver)
        point_providers_at(app, fake_cloud)
        # The deferred path is `dns.set_up_later`, set by the page's own button
        # — `set_captured_dns_credential_for_test` would clear it, so this seeds
        # by hand rather than through `seed_standard_path_run`.
        drv = driver
        drv.call_machine_method(
            "seed_identity", json.dumps(app.onboarding._IMPORT_KEY_FOR_HANDLE_TESTS)
        )
        appended = _appended_actor_id(app)
        drv.call_machine_method("set_current_handle", json.dumps(f"alice@{HANDLE_DOMAIN}"))
        drv.call_machine_method("set_step_for_test", json.dumps("DnsConfig"))
        drv.wait_for("dns-set-up-later-button", timeout=UI_SETTLE_S)
        drv.click("dns-set-up-later-button")
        drv.wait_for("vps-config-back-button", timeout=UI_SETTLE_S)
        drv.call_machine_method(
            "set_vps_state_for_test",
            json.dumps({
                "provider_id": "hetzner",
                "creds": {"api-token": "tkn"},
                "server_types": [{
                    "id": "cx23", "vcpu": 2, "mem_gb": 4.0, "disk_gb": 40,
                    "price_monthly_cents": 451, "currency": "EUR",
                }],
                "selected_server_type_id": "cx23",
                "locations": [{
                    "id": "fsn1", "name": "Falkenstein",
                    "city": "Falkenstein", "country": "DE",
                }],
                "selected_location_id": "fsn1",
            }),
        )
        drv.call_machine_method("set_step_for_test", json.dumps("NestProvisioning"))
        app.wait_for("provisioning-start-button")
        app.click("provisioning-start-button")
        wait_until(
            lambda: overall(app) == "Succeeded",
            ORCHESTRATION_STEP_S,
            diagnose=lambda: (
                "the deferred-DNS run never succeeded (Server is the only real "
                f"step): overall={overall(app)!r} server={step_status(app, 'Server')!r}"
            ),
        )
        # Mid-run custody again: the box exists now, the ADMIN is still active.
        index = wait_registry_index(
            lambda: read_registry_index(driver), 2, active=admin_actor, budget_s=UI_SETTLE_S
        )
        assert index["active"] == admin_actor, (
            f"the reach completion must not move the active pointer either; index={index!r}"
        )

        # The terminal: Continue → the records page → Continue exits
        # AwaitingManualDns, and the append glue registers-and-switches.
        wait_until(
            lambda: app.is_enabled("provisioning-continue-button"),
            UI_SETTLE_S,
            diagnose=lambda: app.driver.diagnose("provisioning-continue-button"),
        )
        app.click("provisioning-continue-button")
        app.wait_for("dns-post-instructions-continue-button", timeout=UI_SETTLE_S)
        app.click("dns-post-instructions-continue-button")

        index = wait_registry_index(
            lambda: read_registry_index(driver), 2, active=appended, budget_s=APP_RELAUNCH_S
        )
        assert [a["actor_id"] for a in index["accounts"]] == [admin_actor, appended]
        slot = read_store_slot(driver, f"fauna/{appended}/awaiting_dns")
        assert slot and json.loads(slot).get("dns_records_json", "").strip("[] "), (
            f"the exit must complete the slot with the records to paste; slot={slot!r}"
        )
        _assert_live_rows_intact(driver, admin_actor, after="after the append's terminal,")
        _assert_almost_ready_with_records(app, after="after the deferred-DNS exit,")

        # A relaunch finds the appended identity's surface again — the exit's
        # write is the routing's — and the previous account is still there.
        _relaunch_on_the_same_store(app)
        _assert_almost_ready_with_records(app, after="after relaunching on the appended identity,")
        index = read_registry_index(driver)
        assert index["active"] == appended and len(index["accounts"]) == 2, (
            f"the relaunch must keep both rows with the appended one active; index={index!r}"
        )
        _assert_live_rows_intact(driver, admin_actor, after="after the relaunch,")
    finally:
        driver.teardown()


def _assert_almost_ready_with_records(app: ActionLayer, *, after: str) -> None:
    """The "Almost ready" surface in its with-records mode: the record list is
    non-empty and "Copy all" is live (`onboarding-provisioning.md` § "Almost
    ready" surface, *Two modes*). Polled: the status row cycles through the
    probe copy, but the records block and the copy button are the mode's
    latency-independent signals."""
    try:
        app.driver.wait_for("awaiting-dns-records", timeout=APP_RELAUNCH_S)
    except Exception as exc:
        raise AssertionError(
            f"{after} the 'Almost ready' surface never rendered: "
            f"error={app.error_text()!r} " + app.driver.diagnose("awaiting-dns-records")
        ) from exc
    last = {"text": ""}

    def records_painted():
        last["text"] = app.driver.get_text("awaiting-dns-records")
        return bool(last["text"].strip())

    wait_until(
        records_painted,
        UI_SETTLE_S,
        diagnose=lambda: f"{after} the records block stayed empty: {last['text']!r}",
    )
    wait_until(
        lambda: app.driver.is_enabled("awaiting-dns-copy-button"),
        UI_SETTLE_S,
        diagnose=lambda: (
            f"{after} Copy all must be live with records to paste: "
            + app.driver.diagnose("awaiting-dns-copy-button")
        ),
    )
