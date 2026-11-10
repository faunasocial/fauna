"""tier_3: the custody of a box's recovery seed, witnessed through the app —
`recover-a-lost-nest` outcomes 7, 8, 9 and 10.

``box-recovery.md`` § Goal: after total box loss the admin's app rebuilds a
same-identity box, which it can only do if some device of the admin's custodies
the box's deployment seed. Three properties of that custody are app-observable
and had no app-level witness (``docs/features/recover-a-lost-nest.md``):

* **7 — an unconfirmed custody warns, in the shared words.** § The plane-era
  recovery floor, (c) The writes: a custody-leg run that ends with custody
  unconfirmed for an admin owes a warning, and every host renders the ONE
  ``fauna_client_config::custody_leg_warning`` text onto its warning surface.
  No ordinary input makes a healthy nest refuse its own admin the seed, so the
  nest's test hook refuses ``fauna.admin.deployment_seed.get``
  (``rpc_hold_test_hook.rs`` § Refusal) — the fetch failure the leg must
  report, never a mock of the leg.
* **8 — a co-admin's device custodies the box too.** § Mechanism: the seed is
  handed to any roster admin over ``fauna.admin.deployment_seed.get``, and the
  shared custody leg runs at every post-auth edge, so an admin who did NOT
  claim the box still comes to hold its seed: a fresh device of theirs lists
  the box for recovery and its command carries the box's own seed.
* **9 — choosing cloud rebuilds with the seed and re-points the domain.**
  § Recovery UI (step 4): the cloud re-provision's server create hands the new
  server the box's own seed in its cloud-init, and its Dns step points the
  box's domain at the new server. The VPS and DNS providers are the fake cloud
  (``fakes/fake_cloud.py``); the witness is the two requests it recorded.
* **10 — the self-hosted command carries the seed, and copying waits for it.**
  § Recovery UI (step 4): one shared render site emits the
  ``FAUNA_DEPLOYMENT_SEED=…`` line; until the resolver answers the page shows
  the pending placeholder and the copy button is disabled. The resolver's cold
  read is held at the nest (``fauna.sync.changes.list``, the fleet feed the
  cold replica walks) so the pending state is a state the test stands in, not
  an interval it races (convention 14).

tui led (``testing.md`` § Default app and nest mode); linux, web and android
followed in their trickle-down, then macos and ios, through one per-app seam
(``_Seats``): the journeys are the same calls on every app, and only how a
device comes up differs. Every box is a dedicated nest from ``start_boxes`` (TLS-serving for a
native seat, plain HTTP with the SPA's origin allowed for a web seat) — the
rpc-hold arms are per-nest, so nothing leaks into the session's shared nest.
"""

from __future__ import annotations

import json
import re
import secrets
import sys
import time
import urllib.request
from pathlib import Path

import pytest

_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer  # noqa: E402
from conftest import (  # noqa: E402
    _get_ios_second_seat_udid,
    _nest_id_hex,
    _seeded_environment,
    get_available_apps,
)
from common.auth import (  # noqa: E402
    _authed_call,
    _resolve_base,
    claim_admin,
    create_actor_and_register,
)
from drivers import create_driver  # noqa: E402
from helpers.app_launch import app_launch_config  # noqa: E402
from helpers.box_recovery_launch import (  # noqa: E402
    CUSTODY_LEG_UNCONFIRMED,
    RECOVER_BOX_ITEM,
    app_path_for,
    claim_fresh_identity,
    enter_nest_recovery,
    nest_id_of_seed,
    recheck_into_nest_recovery,
    resolve_row_nest_id,
    rust_log,
    selfhosted_command_seeds,
    start_boxes,
    wait_for_authed_app_or_fail,
    wait_for_custody_leg,
    wait_for_selfhosted_seed,
)
from helpers.indexed_rows import (  # noqa: E402
    click_row,
    diagnose_row,
    row_absent,
    row_texts,
)
from helpers.budgets import ORCHESTRATION_STEP_S  # noqa: E402
from helpers.provisioning_drive import (  # noqa: E402
    point_providers_at,
    seed_provider_choices_and_open_run,
)
from helpers.rpc_hold import (  # noqa: E402
    arm_rpc_hold,
    refuse_rpc,
    release_rpc_hold,
    wait_for_held_rpc,
    wait_for_refused_rpc,
)
from helpers.waiting import wait_until  # noqa: E402
from i18n.strings import S  # noqa: E402

_APPS = [c for c in ("tui", "linux", "web", "android", "macos", "ios")
         if c in get_available_apps()]
if not _APPS:
    pytest.skip("drives the onboarding and recovery pages against live nests",
                allow_module_level=True)

pytestmark = pytest.mark.tier_3

#: The kind the custody leg fetches the seed over (`custody_leg.rs`).
_SEED_GET = "fauna.admin.deployment_seed.get"
#: The fleet feed the pre-login resolver's cold read walks
#: (`fauna_account_plane::cold_replica`).
_COLD_FEED = "fauna.sync.changes.list"


def _device(client: str, request, nest: dict, *, second_seat: bool = False):
    """A fresh native device — its own store, its own credentials — on
    identity_choice. On ios the simulator facts come from the run's
    `ios_setup`; `second_seat` puts the device on the session's second
    simulator, because a launch on the run's own one would uninstall the app
    already live there."""
    config = {
        "environment": {
            **_seeded_environment(request, nest),
            "RUST_LOG": rust_log(client),
        },
    }
    if client == "ios":
        config.update(app_launch_config(client, request))
        if second_seat:
            udid = _get_ios_second_seat_udid()
            if udid is None:
                pytest.skip("could not allocate a second iOS simulator")
            config["udid"] = udid
    else:
        config["app_path"] = app_path_for(client)
    driver = create_driver(client)
    driver.launch(config)
    return driver


def _require_built(client: str) -> None:
    """Skip before any box starts when an app this run's seats need is unbuilt.
    android's custodying seat is a tui one (`_Seats`), so it needs both. ios's
    build is the run's `ios_setup`, fetched at the device's launch."""
    if client in ("web", "ios"):
        return
    app_path_for(client)
    if client == "android":
        app_path_for("tui")


class _Seats:
    """The per-app seam: how the box's two devices come up. The ASSERTED device
    (`fresh`) — the one whose pages each outcome reads — is always the app under
    test; the CUSTODYING device (`holder`) is the admin's other device, whose
    custody leg rests the box's seed in their account plane.

    * tui, linux, macos — a process per device, each over its own fresh
      dirs (macos's driver mints a fresh HOME per launch).
    * web — the asserted device is the primary page, launched first; the holder
      is a twin page in its own BrowserContext (its own origin store, so its
      own device), whose teardown closes only its page. Both dial the box
      through the provider-base-url channel, which re-mounts the page — so it
      is set before any identity exists, with the fake cloud's legs when a run
      needs them.
    * android — the emulator runs one app at a time, so the phone is the
      asserted device and the holder is a tui seat beside it (the
      phone-plus-desktop pairing `alice_builder_seats` uses).
    * ios — both devices are the iOS app: the first one up runs on the run's
      own simulator, any later one on the session's second simulator (a
      launch on the first would uninstall the app live there), the seat
      `test_box_recovery_two_nest.py`'s fresh device takes.

    `urls` are extra provider base URLs (the fake cloud's) the asserted
    device's run dials; a native seat takes them at the step
    (`point_providers`)."""

    def __init__(self, client: str, request, nest: dict, *, urls=None):
        self.client = client
        self._request = request
        self._nest = nest
        self._urls = dict(urls or {})
        self._live: list = []
        self._native_up = 0
        self._primary = None
        if client == "web":
            primary = create_driver("web")
            self._live.append(primary)
            primary.launch({
                "url": f"{request.getfixturevalue('spa_url')}/app/",
                "environment": _seeded_environment(request, nest),
            })
            # The box wins the nest leg over any fake's: the page takes the map
            # at launch, before its handle check, which probes through that
            # leg — a fake cloud's `/nest` there reads as no nest running.
            primary.set_provider_base_urls({**self._urls, "nest": nest["url"]})
            self._primary = primary

    def fresh(self):
        """The asserted device, on identity_choice."""
        if self._primary is not None:
            return self._primary
        return self._native(self.client)

    def holder(self):
        """The admin's custodying device, on identity_choice."""
        if self._primary is not None:
            twin = self._primary.open_twin_page()
            self._live.append(twin)
            twin.set_provider_base_urls({"nest": self._nest["url"]})
            return twin
        return self._native("tui" if self.client == "android" else self.client)

    def _native(self, client: str):
        """Bring up one native device of `client`; a second iOS one takes the
        second simulator."""
        driver = _device(client, self._request, self._nest,
                         second_seat=client == "ios" and self._native_up > 0)
        if client == "ios":
            self._native_up += 1
        self._live.append(driver)
        return driver

    def await_custody(self, driver, secret_hex: str) -> None:
        """The custody leg has rested the box's row in `driver`'s own account
        store. Native reads the leg's own log line; a browser's log is a bounded
        console ring whose absences prove nothing, so web reads the store."""
        if not driver.is_web():
            wait_for_custody_leg(driver)
            return
        script = (
            "(async () => (await window.__fauna_recoveryBoxesLocalForTest("
            f"{secret_hex!r})).length)()"
        )
        wait_until(
            lambda: (driver.eval_js(script) or 0) > 0,
            60.0,
            diagnose=lambda: (
                "this tab's own store never came to hold the custodied box: "
                f"{driver.eval_js(script)!r}\n"
                f"{driver.app_stderr_text()[-8000:]}"
            ),
        )

    def point_providers(self, app, fake_cloud) -> None:
        """Point the asserted device's run at the fake cloud. A web page took
        the URLs at launch (re-pointing re-mounts the page, which would drop
        the wizard's place), so there only the read-back runs."""
        if not app.driver.is_web():
            # The nest leg stays on the box: the run's first act reads the
            # box's seed from it, through the same dial override.
            point_providers_at(app, fake_cloud, nest_base_url=self._nest["url"])
            return
        applied = app.driver.call_machine_method(
            "provider_base_url", json.dumps("dns"))
        assert applied == self._urls["dns"], (
            "the provider-base-url override never reached the machine, so the "
            f"run would escape to the live internet: dns_override={applied!r} "
            f"expected={self._urls['dns']!r}"
        )

    def leave(self, driver) -> None:
        """Tear one device down now (a web twin closes only its page)."""
        if driver in self._live:
            self._live.remove(driver)
        driver.teardown()

    def close(self) -> None:
        """Tear down every device still up, the last brought up first (a web
        twin before its primary). Safe to repeat."""
        while self._live:
            self._live.pop().teardown()


def _list_until_offered(app, handle: str, *, holder=None) -> list[str]:
    """On `nest_recovery`: re-enter the page until it lists a box. The list is
    read on page entry from the nest's copy of the account plane, which the
    custodying device publishes in the background — a state poll, bounded only
    against a hang."""
    deadline = time.monotonic() + 180.0
    labels = row_texts(app.driver, RECOVER_BOX_ITEM)
    while not labels and time.monotonic() < deadline:
        app.click("recover-back-button")
        recheck_into_nest_recovery(app, handle)
        labels = row_texts(app.driver, RECOVER_BOX_ITEM)
    tail = (f"\n--- custodying device's log tail ---\n"
            f"{holder.app_stderr_text()[-8000:]}" if holder is not None else "")
    assert labels, (
        "a fresh device reaching the box never listed it for recovery: "
        f"{diagnose_row(app.driver, RECOVER_BOX_ITEM, 0)}{tail}"
    )
    return labels


def _run_due(nest: dict) -> None:
    """Run the nest's pending-action executor once (`test-hooks`), so the
    day-delayed roster grant lands now — never a wait (convention 14)."""
    base, ctx = _resolve_base(nest["port"], nest.get("url"))
    req = urllib.request.Request(f"{base}/api/v1/test/pending_actions/run_due",
                                 data=b"", method="POST")
    with urllib.request.urlopen(req, timeout=30.0, context=ctx) as resp:
        assert resp.status == 200, f"run_due hook returned {resp.status}"


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize("client", _APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_an_unconfirmed_custody_warns_the_admin_in_the_shared_words(
    client, nest_mode, tmp_path_factory, request
):
    """Outcome 7: the box refuses its claiming admin the seed fetch, so the
    custody leg ends unconfirmed — and the app says so on `warning-message`,
    in exactly the shared `launch.recovery_custody_failed` text."""
    _require_built(client)  # skip before any box starts when the app is unbuilt
    (nest,), stop_boxes = start_boxes(
        request, nest_mode, tmp_path_factory, "custody-warning",
        browser=client == "web")
    seats = None
    try:
        port = nest["port"]
        # Armed before the device exists: the leg's first run is at the
        # store-ready edge right after the claim.
        refuse_rpc(port, _SEED_GET)
        seats = _Seats(client, request, nest)
        driver = seats.fresh()
        app = ActionLayer(driver)
        claim_fresh_identity(app, secrets.token_hex(32),
                             f"admin{secrets.token_hex(3)}@127.0.0.1:{port}",
                             nest["claim_code"])

        # The leg's fetch reached the box and was turned away …
        wait_for_refused_rpc(port, _SEED_GET)
        # … and the admin is told, in the one shared wording.
        expected = S.launch.recovery_custody_failed
        wait_until(
            lambda: app.warning_text() == expected,
            60.0,
            diagnose=lambda: (
                f"warning-message reads {app.warning_text()!r}, wanted the shared "
                f"custody warning {expected!r}; the leg logged an unconfirmed "
                f"outcome: {CUSTODY_LEG_UNCONFIRMED in driver.app_stderr_text()}"
            ),
        )
    finally:
        release_rpc_hold(nest["port"], _SEED_GET)
        if seats is not None:
            seats.close()
        stop_boxes()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize("client", _APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_a_co_admins_device_custodies_a_box_it_did_not_claim(
    client, nest_mode, tmp_path_factory, request
):
    """Outcome 8: another identity claimed the box; this admin was added to the
    roster later. Signing in on their own device runs the custody leg, which
    fetches the seed as any roster admin may — so a fresh device of theirs,
    holding only their identity, lists the box and resolves its own seed."""
    from nacl.signing import SigningKey

    _require_built(client)
    (nest,), stop_boxes = start_boxes(
        request, nest_mode, tmp_path_factory, "coadmin-custody",
        browser=client == "web")
    seats = None
    try:
        port = nest["port"]
        # The precondition, standing for a claim and a roster grant made long
        # ago from devices not under test: the claimer claims, admits this
        # admin, and grants them the admin role.
        claimer = SigningKey.generate()
        claim_admin(port, nest["claim_code"], base_url=nest.get("url"),
                    handle="claimer" + secrets.token_hex(3), signing_key=claimer)
        co_admin = create_actor_and_register(
            port, base_url=nest.get("url"), admin_signing_key=claimer)
        grant = _authed_call(nest["url"], claimer, "fauna.admin.admins.add",
                             {"actor_id": co_admin["actor_id_bytes"]})
        if grant.get("status") == "pending":
            _run_due(nest)
        admins = {
            bytes(a["actor_id"]).hex()
            for a in _authed_call(nest["url"], claimer,
                                  "fauna.admin.admins.list", {})["admins"]
        }
        assert co_admin["actor_id_hex"] in admins, (
            f"the co-admin grant did not run: {admins!r}")
        secret_hex = co_admin["signing_key"].encode().hex()
        handle = f"{co_admin['handle']}@127.0.0.1:{port}"

        # ---- The co-admin signs in on their device: the custody leg runs at
        # the post-auth edge and captures the seed it never saw at a claim. ----
        seats = _Seats(client, request, nest)
        holder = seats.holder()
        app = ActionLayer(holder)
        app.onboarding.navigate_to_status()
        app.onboarding.import_key(secret_hex)  # -> handle_entry
        app.onboarding.fill_handle(handle)
        app.onboarding.run_handle_check(timeout=45)
        app.onboarding.submit_handle()
        wait_for_authed_app_or_fail(app)
        seats.await_custody(holder, secret_hex)

        # ---- A fresh device of the co-admin's, reaching the box: it is
        # offered for recovery, and its command carries the box's own seed. ----
        fresh = seats.fresh()
        fresh_app = ActionLayer(fresh)
        enter_nest_recovery(fresh_app, secret_hex, handle)
        _list_until_offered(fresh_app, handle, holder=holder)
        assert row_absent(fresh, RECOVER_BOX_ITEM, 1), (
            "the co-admin custodies exactly one box: "
            f"{diagnose_row(fresh, RECOVER_BOX_ITEM, 1)}"
        )
        offered = resolve_row_nest_id(fresh_app, 0).lower()
        box_id = _nest_id_hex(port).lower()
        assert offered == box_id, (
            "the co-admin's recovery command must carry the box's own seed "
            f"({box_id[:8]}…); it derives to {offered[:8]}…"
        )
    finally:
        if seats is not None:
            seats.close()
        stop_boxes()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize("client", _APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_the_selfhosted_command_carries_the_seed_and_copy_waits_for_it(
    client, nest_mode, tmp_path_factory, request
):
    """Outcome 10: on a fresh device the self-hosted page resolves its command
    from the box's copy of the account plane. While that read is held at the
    box the page shows the pending placeholder and copying is disabled; once
    it answers, the line carries `FAUNA_DEPLOYMENT_SEED=<the box's seed>` and
    copying is enabled."""
    _require_built(client)
    (nest,), stop_boxes = start_boxes(
        request, nest_mode, tmp_path_factory, "selfhosted-command",
        browser=client == "web")
    seats = None
    try:
        port = nest["port"]
        secret_hex = secrets.token_hex(32)
        handle = f"admin{secrets.token_hex(3)}@127.0.0.1:{port}"

        # ---- The admin claims the box; their device custodies its seed. ----
        seats = _Seats(client, request, nest)
        holder = seats.holder()
        claim_fresh_identity(ActionLayer(holder), secret_hex, handle,
                             nest["claim_code"])
        seats.await_custody(holder, secret_hex)

        # ---- A fresh device lists the box (the custodied row has reached the
        # box's plane copy) … ----
        fresh = seats.fresh()
        app = ActionLayer(fresh)
        enter_nest_recovery(app, secret_hex, handle)
        _list_until_offered(app, handle, holder=holder)
        # The custodying device's own sync would park on the hold too, and
        # its parked request would pass for this page's: it goes now.
        seats.leave(holder)

        # ---- … and opens the self-hosted page with the resolver's cold read
        # held at the box. ----
        arm_rpc_hold(port, _COLD_FEED)
        try:
            click_row(fresh, RECOVER_BOX_ITEM, 0)
            app.click("recover-method-selfhosted-button")
            fresh.wait_for("recover-selfhosted-command", timeout=30)
            wait_for_held_rpc(
                port, _COLD_FEED,
                diagnose=lambda: fresh.diagnose("recover-selfhosted-command"))
            pending = fresh.get_text("recover-selfhosted-command") or ""
            assert pending == S.onboarding.recovery.selfhosted_command_pending, (
                "while the command is unresolved the page must show the pending "
                f"placeholder, got {pending!r}"
            )
            assert not selfhosted_command_seeds(app), (
                f"no seed may show before the resolver answered: {pending!r}")
            assert not app.is_enabled("recover-selfhosted-copy-button"), (
                "copying must wait until the command is resolved: "
                f"{fresh.diagnose('recover-selfhosted-copy-button')}"
            )
        finally:
            release_rpc_hold(port, _COLD_FEED)

        seed = wait_for_selfhosted_seed(app, "the box")
        command = fresh.get_text("recover-selfhosted-command") or ""
        assert f"FAUNA_DEPLOYMENT_SEED={seed}" in command, (
            f"the installer line must hand the seed to the installer: {command!r}")
        box_id = _nest_id_hex(port).lower()
        assert nest_id_of_seed(seed) == box_id, (
            f"the command must carry the box's own seed ({box_id[:8]}…); it "
            f"derives to {nest_id_of_seed(seed)[:8]}…"
        )
        wait_until(
            lambda: app.is_enabled("recover-selfhosted-copy-button"),
            15.0,
            diagnose=lambda: fresh.diagnose("recover-selfhosted-copy-button"),
        )
    finally:
        release_rpc_hold(nest["port"], _COLD_FEED)
        if seats is not None:
            seats.close()
        stop_boxes()


#: The lost box's own domain, served as the fake cloud's one DNS zone
#: (`CloudflareHandlers.serve_zone`), and the address the fake's
#: `POST /servers` answers with. Public-shaped, as the suite's other
#: domain-bearing boxes are (`claim_domain="fauna.test"`): the claim registers a
#: `mail_domain` only when `is_public_dns_name` passes it, so a local name
#: (`box.localhost`) leaves the box domainless and its recovery self-hosted.
_BOX_DOMAIN = "box.fauna.test"
_NEW_SERVER_IP = "203.0.113.5"
_SEED_LINE = re.compile(r"FAUNA_DEPLOYMENT_SEED: ([0-9a-f]{64})")


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.parametrize("client", _APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_cloud_recovery_rebuilds_with_the_saved_seed_and_repoints_the_domain(
    client, nest_mode, tmp_path_factory, request, fake_cloud
):
    """Outcome 9: choosing cloud for a lost box re-provisions it carrying its
    saved identity — the server create's cloud-init hands the new server the
    box's own seed — and the run points the box's own domain at the new
    server (`box-recovery.md` § Recovery UI (step 4): cloud re-provision reuses
    `vps_config` + `nest_provisioning`, the seed rides
    `CloudInitParams.deployment_seed`, the orchestrator re-points the A record
    as its `Dns` step).

    The box carries a domain because a cloud recovery needs one: custody
    records the box's domain from its nest info, and a domainless box is routed
    to self-hosted instead. The fake DNS serves a zone of the box's own name,
    so the run's Domain step has its zone. The witness is the two requests the fake
    recorded — never the run reaching `Succeeded`: the rebuilt server never
    exists, so the Online step has nothing to reach."""
    from nacl.signing import SigningKey

    _require_built(client)
    (nest,), stop_boxes = start_boxes(
        request, nest_mode, tmp_path_factory, "cloud-recovery",
        browser=client == "web")
    seats = None
    try:
        port = nest["port"]
        box_id = _nest_id_hex(port).lower()
        # The precondition, standing for a claim made long ago: the box was
        # claimed onto its domain. A wire act, honoured in every nest mode.
        admin_key = SigningKey.generate()
        local = "admin" + secrets.token_hex(3)
        claim_admin(port, nest["claim_code"], base_url=nest.get("url"),
                    handle=local, mail_domain=_BOX_DOMAIN, signing_key=admin_key)
        secret_hex = admin_key.encode().hex()
        handle = f"{local}@127.0.0.1:{port}"
        # The box reports the domain it was claimed onto — what custody records.
        reported = _authed_call(nest["url"], admin_key, "fauna.nest.info", {}).get("domain")
        assert reported == _BOX_DOMAIN, (
            f"the box claimed onto {_BOX_DOMAIN!r} must report it in its nest "
            f"info (custody's domain source); it reports {reported!r}")

        # ---- The admin signs in on their device: the custody leg captures the
        # box's seed, with the domain the box reports. ----
        seats = _Seats(client, request, nest, urls=fake_cloud.url_map())
        holder = seats.holder()
        app = ActionLayer(holder)
        app.onboarding.navigate_to_status()
        app.onboarding.import_key(secret_hex)  # -> handle_entry
        app.onboarding.fill_handle(handle)
        app.onboarding.run_handle_check(timeout=45)
        app.onboarding.submit_handle()
        wait_for_authed_app_or_fail(app)
        seats.await_custody(holder, secret_hex)

        # ---- A fresh device lists the box, and the admin chooses cloud. ----
        fresh = seats.fresh()
        fresh_app = ActionLayer(fresh)
        enter_nest_recovery(fresh_app, secret_hex, handle)
        _list_until_offered(fresh_app, handle, holder=holder)
        seats.leave(holder)
        click_row(fresh, RECOVER_BOX_ITEM, 0)
        fresh_app.click("recover-method-cloud-button")
        fresh.wait_for("vps-config-back-button", timeout=30)

        # ---- The provider choices the config pages capture, at the fake; then
        # the run. ----
        fake_cloud.cloudflare.serve_zone(_BOX_DOMAIN)
        seats.point_providers(fresh_app, fake_cloud)
        seed_provider_choices_and_open_run(fresh_app)
        fresh_app.click("provisioning-start-button")

        def a_records():
            return [
                r for r in fake_cloud.dns_record_creates()
                if r.get("type") == "A" and r.get("content") == _NEW_SERVER_IP
            ]

        def snapshot():
            return json.dumps(fresh.call_machine_method("provisioning_snapshot"))

        wait_until(
            lambda: bool(fake_cloud.server_creates()) and bool(a_records()),
            ORCHESTRATION_STEP_S * 3,
            diagnose=lambda: (
                f"the recovery run never both created a server and pointed a "
                f"record at it: {len(fake_cloud.server_creates())} create(s), "
                f"DNS creates {fake_cloud.dns_record_creates()!r}; "
                f"snapshot={snapshot()}; error={fresh_app.error_text()!r}"
            ),
        )

        creates = fake_cloud.server_creates()
        assert len(creates) == 1, (
            f"the run should create exactly one server, saw {len(creates)}")
        user_data = creates[0].get("user_data") or ""
        seeds = _SEED_LINE.findall(user_data)
        assert len(seeds) == 1, (
            "the rebuilt server's cloud-init must hand it the box's saved seed "
            f"(one FAUNA_DEPLOYMENT_SEED line), found {len(seeds)}:\n{user_data}"
        )
        assert nest_id_of_seed(seeds[0]) == box_id, (
            f"the server must be built with the LOST box's own seed "
            f"({box_id[:8]}…); the one sent derives to "
            f"{nest_id_of_seed(seeds[0])[:8]}…"
        )
        assert creates[0].get("name") == _BOX_DOMAIN.replace(".", "-"), (
            "the server is named after the box's own domain: "
            f"{creates[0].get('name')!r}"
        )
        apex = {_BOX_DOMAIN, "@"}
        assert any(r.get("name") in apex for r in a_records()), (
            f"the box's domain {_BOX_DOMAIN!r} must be pointed at the new server "
            f"{_NEW_SERVER_IP}; the A records written: {a_records()!r}"
        )
    finally:
        if seats is not None:
            seats.close()
        stop_boxes()
