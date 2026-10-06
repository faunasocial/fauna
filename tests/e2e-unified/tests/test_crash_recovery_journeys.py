"""Adversarial crash-recovery journeys — the Client-state recoverability invariant.

`docs/goal/architecture/nest/common.md` § Client-state recoverability: **no
state a client can put the nest into is unrecoverable by a client — ever,
including a client crash at any point during the operation.** The per-write
verification question there ("if the client dies the instant after each
individual write, can a client still recover the box?") is this file's
test-case generator; the transitions covered here are the ones its
§ Implementation status listed as *not yet audited end-to-end*: mail
enable/disable, domain add/remove — plus the already-audited claim and factory
reset, now exercised adversarially instead of by design review.

Shape of every journey (helpers/crash_recovery.py):

  0. relaunch the session app seeded for its crash nest BEFORE anything points
     it there (`conftest._relaunch_trusting_nest`;
     `e2e-automation-surface-gating.md` § The e2e trust seed). A relaunch is a
     fresh launch, so it can only come ahead of the choreography — after the
     crash it would erase the state under test — and every relaunch after the
     kill re-reads the environment it seeded. One seed holds for the whole
     journey: the nest identity it names survives a factory reset and a nest
     restart (`nest/common.md` § the data dir, `nest_deployment.key`);
  1. drive the operation through the client UI (the only config surface —
     testing.md § point 8);
  2. SIGKILL the client at the seam — after the nest's dispatch-receipt beacon
     confirms the request is genuinely in flight (`NestLogWatch`, never a
     fixed sleep; the matched line is the anti-vacuous proof), or immediately
     after the triggering click for a pre-receipt seam;
  3. read nest ground truth over the side-channel WS-RPC (either intermediate
     state is legitimate — the invariant demands recoverability from ALL of
     them, so journeys assert recovery from whichever resulted, not a forced
     outcome);
  4. relaunch (`hard_reload()` — tolerates the dead child; the replayed
     `set_state` session stands in for the crashed client's locally-persisted
     identity, per the e2e in-memory-keychain posture) and drive the nest to
     the working end state THROUGH THE UI.

Kill scope: only the driver's OWN spawned app child / the fixture's OWN nest
child is ever signalled (process safety, e2e-unified/README.md).

macOS/linux/tui drivers own their app child and support the kill directly;
ios (2026-07-15) supports it too, despite `simctl` nominally owning the
process — `simctl launch` reports a REAL host PID (`drivers/ios.py`
`IosInProcessDriver.kill_uncleanly`, empirically verified against `ps`/`kill`),
so it SIGKILLs that specific PID, never a name/bundle-id-based kill. web's
kill is the page closed with its BrowserContext kept (`drivers/web.py`
`kill_uncleanly` — no unload handler runs, the storage survives). windows (the
FlaUI bridge owns the app) still needs its own unclean-kill shape — a declared
skip until then (drivers/base.py `kill_uncleanly`).
"""

from __future__ import annotations

import json
import os
import re
import secrets
import threading
import time

import pytest

from common.nest import resume_after_self_exit, stop_nest, start_nest_in_place
from conftest import _relaunch_trusting_nest
from helpers.fleet import (
    FLEET_MEMBER_VISIBLE_S,
    SiblingSeats,
    await_fleet_member,
    device_set_state,
    fleet_id_hex,
    poked_pass,
    require_device_set_reader,
)
from helpers.crash_recovery import (
    CRASH_NEST_RUST_LOG,
    NestLogWatch,
    inject_admin_session,
    launch_surface_dump,
    setup_status,
)
from helpers.folder_share import (
    SHARE_ROSTER_S,
    content_key_get,
    headless_member,
    roster_index,
    share_through_owner_ui,
)
from helpers.rpc_hold import (
    arm_rpc_hold,
    refuse_rpc,
    release_rpc_hold,
    wait_for_held_rpc,
)
from helpers.trust_seed_witness import TRUST_SEED_ENV, await_a_tip_sealed_row
from common.cred_store import attach_account_store
from helpers import enrollment
from helpers.bridge_enrollment import enroll_bridge
from helpers.app_surface import app_name
from helpers.waiting import await_device_removal_ready, wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.crash_recovery,
              pytest.mark.destructive]


# ---------------------------------------------------------------------------
# Dedicated nests (never the shared session nest_instance — these journeys
# mutate deployment state: domains, mail enablement, factory reset).
#
# Both go through the run's mode provider (`_start_dedicated_nest`), so a
# container answers them as readily as a local binary; the hand-rolled
# `_terminate_nest` they used to share went with the routing, because the
# provider's own cleanup is what knows how this nest is torn down.
# ---------------------------------------------------------------------------

@pytest.fixture
def crash_nest(request, nest_mode, tmp_path_factory):
    """A dedicated CLAIMED nest with the dispatch-receipt beacon enabled
    (RUST_LOG debug for fauna_nest), so the admin shell mounts directly (see
    conftest `admin_app`), started by the run's mode provider.

    `extra_env` is the only option, and it is the artifact-set IPC kind rather
    than a product choice: `RUST_LOG` is what makes the nest emit the beacon
    these journeys time their kills against. Docker catalogues it
    (`conftest._DOCKER_EXTRA_ENV`) and the image's run-script imports the
    container env, so the beacon reaches a container's log at the same DEBUG
    level it reaches a file beside a binary.

    No storage-mode commit: the storage-mode axis is retired — every nest stores
    content sealed from first boot (`storage-modes.md` § Implementation status
    today), so there is no mode to choose and nothing to commit."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "crash-nest",
        extra_env={"RUST_LOG": CRASH_NEST_RUST_LOG})
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def unclaimed_crash_nest(request, nest_mode, tmp_path_factory):
    """A dedicated fresh, NEVER-claimed nest with the dispatch-receipt beacon
    enabled — the mid-claim journey drives the real UI claim itself.

    The claim code comes off the handle (`nest["claim_code"]`), never the
    `common.nest.CLAIM_CODE` constant: that constant is a fact about
    *standalone*, while docker's provider mints a random code per container."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "crash-nest-unclaimed",
        unclaimed=True, extra_env={"RUST_LOG": CRASH_NEST_RUST_LOG})
    try:
        yield nest
    finally:
        cleanup()


def _admin_secret_hex(nest) -> str:
    return nest["admin"]["signing_key"].encode().hex()


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key),
                            signing_key=bytes(sk))


# ---------------------------------------------------------------------------
# Journey 1 — kill the client mid-DOMAIN-ADD
# (common.md § Implementation status: "domain add/remove" was un-audited)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_client_mid_domain_add_box_recovers(killable_app, crash_nest):
    """UI submits `fauna.bridges.add_local_domain`; the client is SIGKILL'd the
    instant the nest's dispatcher receives it (beacon-timed). Whatever state
    resulted, a relaunched client must see the truthful domain list and be
    able to drive the box to "domain registered" — no off-box fix."""
    app = killable_app
    domain = "crash-recovery.test"

    _relaunch_trusting_nest(app.driver, crash_nest)
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    app.driver.click("admin-dns-add-domain-button")
    time.sleep(0.5)  # reveal animation (mirrors AdminActions.add_domain)
    app.driver.type_text("admin-dns-add-domain-input", domain)

    watch = NestLogWatch(crash_nest)
    app.driver.click("admin-dns-add-domain-submit-button")
    beacon = watch.wait_for_dispatch("fauna.bridges.add_local_domain")
    app.driver.kill_uncleanly()

    # Ground truth: the box must be alive and answering either way; the domain
    # is either committed or not — BOTH are legitimate intermediate states.
    with _admin_ws(crash_nest) as adm:
        reply = adm.call("fauna.bridges.list_local_domains", {})
    committed = any(row.get("domain_name") == domain
                    for row in reply.get("active", []))

    # Recovery: relaunch, re-establish the admin (the crashed client's
    # persisted identity), and drive to the end state through the UI — retry
    # the add only if the crash beat the commit, exactly what a human does.
    app.driver.hard_reload()
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    if domain not in app.admin.dns_domain_names():
        assert not committed, (
            f"nest committed {domain!r} (beacon: {beacon!r}) but the relaunched "
            f"client's admin-dns page does not list it — the recovered client "
            f"sees an untruthful state. error={app.error_text()!r}"
        )
        app.admin.add_domain(domain)

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if domain in app.admin.dns_domain_names():
            break
        time.sleep(0.5)
    assert domain in app.admin.dns_domain_names(), (
        f"relaunched client could not drive the box to 'domain registered' "
        f"after a mid-add crash (pre-kill committed={committed}, beacon: "
        f"{beacon!r}). error={app.error_text()!r}"
    )

    # The trust seed reached the crash nest: a generation tip resolved there for
    # the admin (helpers/trust_seed_witness.py). One journey witnesses it for the
    # file — the seeding is the same line in every journey, pinned by
    # `test_r14_trust_seed_default.py`. A session launched unseeded has no seed to
    # witness.
    if (app.driver.relaunch_environment() or {}).get(TRUST_SEED_ENV):
        await_a_tip_sealed_row(
            app, crash_nest, crash_nest["admin"], where="the crash nest"
        )


# ---------------------------------------------------------------------------
# Journey 2 — kill the client mid-MAIL-ENABLE
# (common.md § Implementation status: "mail enable/disable" was un-audited.
#  Mail enable is the richest client-driven sequence: set_mail_enabled +
#  recipient-key/MSEK provisioning + canonical alias — a crash can tear the
#  client-side sequence between those writes.)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset", "turn-on-mail")
def test_kill_client_mid_mail_enable_box_recovers(killable_app, crash_nest):
    """UI enables mail (toggle → credential form → submit); the client is
    SIGKILL'd at the first enable dispatch. A relaunched client must land on a
    truthful mail-settings page and be able to reach "mail enabled with a
    credential" — no off-box fix, no wedged half-enabled state."""
    app = killable_app

    _relaunch_trusting_nest(app.driver, crash_nest)
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.mail_settings.navigate()
    app.driver.wait_for("mail-settings-enabled-toggle", timeout=15.0)
    app.driver.click("mail-settings-enabled-toggle")
    app.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
    app.driver.clear_and_type("mail-add-credential-name-input", "Default")

    watch = NestLogWatch(crash_nest)
    app.driver.click("mail-add-credential-submit-button")
    # The enable sequence's first nest write; kill the instant it's received.
    beacon = watch.wait_for(
        r"kind[=:].*(set_mail_enabled|mail\.enable|enable_mail)")
    app.driver.kill_uncleanly()

    # Ground truth: box answers; enablement is whatever the torn sequence left.
    services = setup_status(crash_nest["url"])  # alive + sane claim state

    # Recovery through the UI: relaunch → mail settings must render a truthful
    # state, and the enable flow must be drivable to completion (idempotent
    # re-enable or resume — either UI shape is fine, the END STATE is the
    # assertion).
    app.driver.hard_reload()
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.mail_settings.navigate()
    app.driver.wait_for("mail-settings-enabled-toggle", timeout=15.0)

    if app.mail_settings.credential_count() < 1:
        app.mail_settings.enable_mail(display_name="Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"relaunched client could not drive the box to 'mail enabled with a "
        f"credential' after a mid-enable crash (beacon: {beacon!r}; services "
        f"at kill: {services!r}). error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 3 — kill the client mid-CLAIM
# (already audited by design — handle-less admin unrepresentable, claim gate
#  keyed on the admin row — now exercised adversarially through the real UI.)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_client_mid_claim_box_recovers(killable_app, unclaimed_crash_nest):
    """UI claim of a fresh nest (import identity → claim-code submit — the
    real POST /api/v1/claim-admin); the client is SIGKILL'd right after the
    submit click, mid-flight. The box must land claimable-or-claimed, never
    a "claimable-but-claimed" limbo, and the SAME identity must be able to
    finish onboarding from a relaunched client."""
    import json as _json

    from nacl.signing import SigningKey

    app = killable_app
    nest = unclaimed_crash_nest
    admin_sk = SigningKey.generate()
    admin_secret_hex = bytes(admin_sk).hex()
    handle = "admin@localhost"

    st = setup_status(nest["url"])
    assert st.get("claimed") is False, f"precondition: fresh nest, got {st!r}"

    # Real UI claim front-half (mirrors helpers.mail_client_ui.
    # claim_to_encryption_page — only the DNS discovery is faked):
    _relaunch_trusting_nest(app.driver, nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(admin_secret_hex)
    app.driver.wait_for("handle-input", timeout=15)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        _json.dumps([nest["url"], handle]),
    )
    app.driver.wait_for("claim-code-input", timeout=20)
    app.driver.clear_and_type("claim-code-input", nest["claim_code"])
    app.driver.click("claim-code-submit-button")
    # No settle wait: the kill lands while the claim POST is in flight (the
    # macOS driver's post-click settle is ~0.5s; the claim round-trip
    # typically outlives it, and EITHER pre/post-commit outcome is a
    # legitimate state to recover from).
    app.driver.kill_uncleanly()

    # Ground truth: never a torn claim — either still fresh (retry open) or
    # fully claimed (admin row exists). common.md pins the gate to the ADMIN
    # row, so a half-completed claim still reads claimed=false.
    st = setup_status(nest["url"])
    assert st.get("claimed") == st.get("admin_exists"), (
        f"torn claim state after mid-claim client crash: {st!r} — "
        f"claimed and admin_exists must agree (nest/common.md § claim gate)"
    )

    # Recovery: relaunch and finish onboarding with the SAME identity.
    #
    # Whatever the server-side outcome, the SIGKILL almost always leaves a TORN
    # client store: import writes `fauna_secret` first, the nest binding
    # (`fauna_node_url`) only at claim success, so the kill routinely lands
    # between them. On web the relaunch preserves that torn store, so the launch
    # machine resumes the wizard at handle_entry (identity-only) rather than a
    # fresh identity_choice, and the same-origin fallback connection the layout
    # pumps open backs off on the still-unregistered identity — both recovery
    # arms below are written to tolerate that, per common.md § Client-state
    # recoverability (recovery must work from EVERY resulting state).
    app.driver.hard_reload()
    if st["claimed"]:
        # The claim committed server-side before the crash; the identity IS the
        # admin. Reach the working admin shell. The store is almost always torn
        # (killed before `fauna_node_url` persisted), so the account registry
        # holds an UNBOUND identity that keeps winning over an injected
        # nest binding — a plain inject can never stick and the admin
        # shell's fail-closed checkIsAdmin bounces to settings forever.
        # `torn_store_resilient` clears the torn registry first so the inject
        # rebuilds the account WITH the binding (see inject_admin_session).
        inject_admin_session(app, nest["url"], admin_secret_hex,
                             torn_store_resilient=True)
    else:
        # The crash beat the claim; the code is intact (single-use, unconsumed
        # — a lingering readable code can't mint a second admin, common.md).
        # Redo the claim through the UI; it must succeed. The relaunch resumes
        # the SAME imported identity from the torn store, so drive from whichever
        # onboarding surface came back (handle_entry on web's preserved store,
        # identity_choice on a wiped native store) rather than assuming a fresh
        # identity_choice.
        ob = app.onboarding
        ob.resume_or_import_identity(admin_secret_hex)
        app.driver.call_machine_method(
            "navigate_to_claim_code_for_known_nest",
            _json.dumps([nest["url"], handle]),
        )
        app.driver.wait_for("claim-code-input", timeout=20)
        app.driver.clear_and_type("claim-code-input", nest["claim_code"])
        app.driver.click("claim-code-submit-button")
        # Claim success routes the wizard to the terminal nat_mode_choice step.
        # (The old encryption-mode page — `encrypt-storage-radio` — is retired:
        # no-modes, ratified 2026-07-12; every nest is sealed at rest
        # unconditionally, so re-claiming just re-claims. Mirror the canonical
        # claim helper, helpers/mail_client_ui.py.)
        app.driver.wait_for("nat-mode-confirm-button", timeout=20)

    st = setup_status(nest["url"])
    assert st.get("claimed") is True and st.get("admin_exists") is True, (
        f"relaunched client could not finish the claim after a mid-claim "
        f"crash: {st!r}. error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 4 — kill the client mid-FACTORY-RESET (the reply-loss window)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_client_mid_factory_reset_floor_holds(killable_app, crash_nest):
    """UI factory reset; the client is SIGKILL'd the instant the nest receives
    `fauna.admin.factory_reset` — BEFORE the reply-carried claim code ever
    reaches a client. The boot-reconcile half must hold: the nest self-exits,
    and the harness (as the s6 supervisor) restarts it into the marker wipe —
    the box MUST land fresh/unclaimed and healthy, never wedged.

    CR-1 (common.md § Client-state recoverability) is now FIXED, so this asserts
    both halves. The client mints the post-reset claim code and persists it to
    its long-term store BEFORE dispatching, then pins it via
    `FactoryResetRequest.new_claim_code`. So the reply dying with the client
    costs nothing: the relaunched client finds the slot, routes to the pre-filled
    claim page (`LaunchWizardEntry::PendingFactoryReset`), and completes the
    re-claim — no re-provision, no operator, no SSH.

    Both halves, in order:
      1. the FLOOR — the box lands fresh/unclaimed and healthy (boot reconcile);
      2. the RE-CLAIM — a relaunched client drives that box back to claimed
         through the UI, using a code that now exists *only* because it was
         persisted before the crash.
    """
    app = killable_app
    nest = crash_nest

    # This is the one journey whose recovery lives CLIENT-side, so it needs the
    # opposite of the harness's default relaunch contract: the app's long-term
    # store must survive the SIGKILL, exactly as a real user's would. Without the
    # pin the driver hands the relaunched process a fresh store, and "the slot did
    # not survive" would be indistinguishable from "the client never wrote it" —
    # the test would go green on a client that still had CR-1.
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so the CR-1 re-claim assertion would be vacuous"
        )

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_nest()
    if app.driver.count("admin-factory-reset-button") == 0:
        app.admin.navigate_settings()
    # Visible != enabled: the danger-zone GroupBox is `.disabled(vm.isBusy)`
    # on apple, so the button renders before it will accept a click.
    app.driver.wait_until_enabled("admin-factory-reset-button", timeout=15.0)
    app.driver.click("admin-factory-reset-button")
    app.driver.wait_for("admin-factory-reset-confirm-button", timeout=10.0)

    watch = NestLogWatch(nest)
    app.driver.click("admin-factory-reset-confirm-button")
    beacon = watch.wait_for_dispatch("fauna.admin.factory_reset")
    app.driver.kill_uncleanly()  # the reply (with the new code) dies with it

    # The handler stages the marker, replies into the void, and exit(0)s,
    # expecting a supervisor to restart it into the wipe. Who plays that part is
    # the mode's business, not this test's: standalone has no supervisor so the
    # harness becomes one, docker has s6 doing it in the image exactly as
    # production does. Either way the next line is what proves the box came back
    # — and came back wiped.
    resume_after_self_exit(nest, timeout=30)

    from common.nest import _wait_nest_fresh
    _wait_nest_fresh(nest["url"], timeout=60.0)  # floor: fresh/unclaimed

    # The floor is genuinely client-reachable: a relaunched client's real
    # handle-check against this box must see the unclaimed state (the claim
    # page is where a code — once a client can know one, CR-1 — gets entered).
    st = setup_status(nest["url"])
    assert st == {"claimed": False, "admin_exists": False} or (
        st.get("claimed") is False and st.get("admin_exists") is False
    ), f"post-reset box is not at the recovery floor: {st!r} (beacon: {beacon!r})"

    # ---- Half 2: the RE-CLAIM (the assertion CR-1 used to block) ----
    #
    # Ground truth: the code the wiped box actually booted with. The client never
    # saw the reply that carried it, so if the two agree below, the ONLY way the
    # client could know it is the slot it persisted before dispatching.
    with open(os.path.join(nest["tmp_dir"], "claim-code")) as f:
        booted_code = f.read().strip()

    # Relaunch via recover(), NOT hard_reload(): hard_reload replays the injected
    # admin session, which would authenticate against a box that no longer has an
    # admin. A real user just reopens the app — so let the app boot cold and run
    # its own launch routing, which is the code path under test.
    assert app.driver.recover(), "relaunch after the crash failed"

    # The launch machine finds the pending-factory-reset slot and routes to the
    # claim page with the code PRE-FILLED. The admin is never asked to type a code
    # that only ever existed in a reply their client never rendered.
    try:
        app.driver.wait_for("claim-code-input", timeout=30.0)
    except Exception as exc:
        # Dump the surface INSTEAD of leaving a bare count=0 behind: which
        # testids exist says whether the page booted at all, and whether the
        # slot is readable in the store says whether this is a lost write or a
        # missed read — the two this journey keeps being unable to tell apart.
        raise AssertionError(
            "the relaunched client never rendered the pre-filled claim page "
            f"(CR-1 re-claim). error={app.error_text()!r} "
            f"launch surface: {launch_surface_dump(app)}"
        ) from exc
    prefilled = app.driver.get_text("claim-code-input")
    assert prefilled == booted_code, (
        "the relaunched client must resume the claim with the code it persisted "
        f"before dispatch: prefilled={prefilled!r} booted={booted_code!r} "
        f"(empty ⇒ the slot did not survive the SIGKILL — CR-1 regressed) "
        f"error={app.error_text()!r}"
    )

    # And it completes: the box goes from the floor back to claimed, driven
    # entirely through the client UI.
    app.driver.click("claim-code-submit-button")

    deadline = time.monotonic() + 60.0
    final = None
    while time.monotonic() < deadline:
        final = setup_status(nest["url"])
        if final.get("claimed") and final.get("admin_exists"):
            break
        time.sleep(0.5)
    assert final and final.get("claimed") and final.get("admin_exists"), (
        "a client SIGKILL'd mid-factory-reset must still be able to re-claim its "
        f"box from the persisted slot — box never returned to claimed: {final!r} "
        f"error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 5 — kill the NEST mid-operation (the boot-reconcile half)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_nest_mid_domain_add_boot_reconciles(killable_app, crash_nest):
    """The other half of the invariant's crash matrix: SIGKILL the NEST the
    instant it receives a client-driven write (`stop_nest(graceful=False)` —
    the fixture's own child; no close frame, no flush). After a supervisor
    restart the box must be healthy and hold an untorn state — the domain
    either committed or absent, and the surviving admin can drive it to
    registered through the UI either way."""
    app = killable_app
    nest = crash_nest
    domain = "nest-crash.test"

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    app.driver.click("admin-dns-add-domain-button")
    time.sleep(0.5)
    app.driver.type_text("admin-dns-add-domain-input", domain)

    watch = NestLogWatch(nest)
    app.driver.click("admin-dns-add-domain-submit-button")
    watch.wait_for_dispatch("fauna.bridges.add_local_domain")
    stop_nest(nest, graceful=False)  # SIGKILL mid-handler
    start_nest_in_place(nest)        # supervisor restart → boot reconcile

    # Untorn ground truth + client-driven completion. The client survived the
    # nest flip (its reconnect machinery is the seamless path) but re-inject
    # the session anyway so the assertion doesn't hinge on reconnect timing.
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    if domain not in app.admin.dns_domain_names():
        app.admin.add_domain(domain)
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if domain in app.admin.dns_domain_names():
            break
        time.sleep(0.5)
    assert domain in app.admin.dns_domain_names(), (
        f"admin could not drive the box to 'domain registered' after a "
        f"mid-add NEST crash + restart. error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 9 — kill the NEST mid-MTA-REVOKE (the last interior tear)
# (common.md § Client-state recoverability: a bridge *revoke* of an MTA is one
#  durable write, the enrollment flip. The nest holds every mail domain's DKIM
#  key itself, so a revoke — crashed or not — touches no DKIM key and no
#  published record.)
# ---------------------------------------------------------------------------

def _seed_pending_bridge(nest, role: str = "mta") -> str:
    """Enroll a pending bridge over the bridge's own anonymous
    ``request_enrollment`` kind — fixture setup (testing.md § point 8 carve-out
    (b); the revoke *action* below is UI-driven). Nothing proves possession of
    the key at enrollment, so a synthetic 32-byte value exercises the
    approve→roster→rotate flow. Returns the hex pubkey. Mirrors
    test_admin_bridges_pending.py::_seed_pending_bridge (crash_nest is fresh, so
    its enable axes are off and the enrollment lands pending)."""
    pubkey = secrets.token_bytes(32)
    status = enroll_bridge(nest["url"], pubkey, role, f"{role}-{secrets.token_hex(4)}")
    assert status == "pending", f"seed must land pending, got {status!r} (mail enabled?)"
    return pubkey.hex()


def _bridge_card_index(app, id_prefix: str, pubkey_hex: str) -> "int | None":
    """Index of the ``{id_prefix}-pubkey-hex`` card whose text contains
    ``pubkey_hex`` (scoped find — nest is dedicated so there's usually one)."""
    count = app.driver.count(f"{id_prefix}-pubkey-hex")
    for i in range(count):
        txt = app.driver.get_text(f"{id_prefix}-pubkey-hex", index=i) or ""
        if pubkey_hex in txt:
            return i
    return None


def _poll_bridge_card(app, id_prefix, pubkey_hex, *, want, timeout=15.0):
    """Poll until the card is present (``want=True``) or gone (``want=False``)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        idx = _bridge_card_index(app, id_prefix, pubkey_hex)
        if (idx is not None) == want:
            return idx
        time.sleep(0.5)
    return _bridge_card_index(app, id_prefix, pubkey_hex)


def _dkim_selectors(nest, domain: str) -> list:
    """Admin side-channel read of the nest-held DKIM selectors for a domain
    (`fauna.bridges.list_dkim_selectors` — the observation, not a mutation)."""
    with _admin_ws(nest) as adm:
        reply = adm.call("fauna.bridges.list_dkim_selectors", {"domain": domain})
    return reply.get("selectors", [])


@pytest.mark.feature("factory-reset")
def test_kill_nest_mid_mta_revoke_leaves_dkim_key_untouched(killable_app, crash_nest):
    """The last interior tear (common.md § Client-state recoverability): a nest
    crash in the middle of an MTA revoke must leave the box untorn and drivable
    to `revoked` through the UI — and the domain's DKIM key exactly as it was.
    The nest holds each mail domain's DKIM key and signs at the outbound
    hand-out, so an MTA revoke/re-key touches no DKIM key and no published
    record: the successor MTA relays mail the nest signs with the same key, and
    the record an admin already put in DNS stays valid.

    This journey drives the revoke through the UI rotate-confirm (the mutation
    is client-driven — testing.md § point 8), SIGKILLs the fixture's own nest at
    the revoke dispatch (beacon-timed, never a fixed sleep — the matched line is
    the anti-vacuous proof the revoke was genuinely in flight), restarts it,
    drives the revoke to completion through the UI, and asserts the domain's
    published DKIM selectors are UNCHANGED.

    A race-timed SIGKILL usually completes the fast synchronous handler either
    side of the window (same limit journeys 4-6 document), so THIS journey is
    the end-to-end recovery proof through a real nest crash, not a
    deterministic tear guard."""
    app = killable_app
    nest = crash_nest
    domain = "mta-revoke-crash.test"

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))

    # --- setup (fixture arrangement, not the action under test): a mail domain
    # — adding it is what makes the nest mint its DKIM key — plus a pending MTA.
    with _admin_ws(nest) as adm:
        adm.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "per_host"},
        )
    before = _dkim_selectors(nest, domain)
    assert before and all(s.get("public_dns_value") for s in before), (
        f"precondition: adding {domain!r} must mint its nest-held DKIM key "
        f"(got selectors {before!r})"
    )
    pubkey_hex = _seed_pending_bridge(nest, "mta")

    # Approve the seeded MTA through the UI so it enters the Approved roster.
    app.admin.navigate_bridges_pending()
    pidx = _poll_bridge_card(app, "admin-bridges-pending", pubkey_hex, want=True)
    assert pidx is not None, (
        f"seeded pending MTA {pubkey_hex[:16]}… not found. error={app.error_text()!r}"
    )
    app.driver.click("admin-bridges-pending-approve-button", index=pidx)
    aidx = _poll_bridge_card(app, "admin-bridges-approved", pubkey_hex, want=True)
    assert aidx is not None, (
        f"approved MTA {pubkey_hex[:16]}… not in the Approved roster. "
        f"error={app.error_text()!r}"
    )

    # --- action under test: rotate (=revoke) through the UI; SIGKILL the nest the
    # instant its dispatcher receives `fauna.bridges.revoke_service_user`.
    app.driver.click("admin-bridges-approved-rotate-button", index=aidx)
    app.driver.wait_for("admin-bridges-rotate-confirm-button", timeout=10.0)
    watch = NestLogWatch(nest)
    app.driver.click("admin-bridges-rotate-confirm-button")
    watch.wait_for_dispatch("fauna.bridges.revoke_service_user")
    stop_nest(nest, graceful=False)   # SIGKILL mid-revoke-handler
    start_nest_in_place(nest)         # supervisor restart → boot reconcile

    # --- recover: drive the revoke to completion through the UI (the crash either
    # committed it or rolled it back — both untorn). Re-inject the session so the
    # assertion doesn't hinge on the client's own reconnect timing.
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_bridges_pending()
    still = _poll_bridge_card(app, "admin-bridges-approved", pubkey_hex, want=True)
    if still is not None:
        # The crash rolled the revoke back; the MTA is still approved. Re-drive it.
        app.driver.click("admin-bridges-approved-rotate-button", index=still)
        app.driver.wait_for("admin-bridges-rotate-confirm-button", timeout=10.0)
        app.driver.click("admin-bridges-rotate-confirm-button")
    gone = _poll_bridge_card(app, "admin-bridges-approved", pubkey_hex, want=False)
    assert gone is None, (
        f"admin could not drive the MTA {pubkey_hex[:16]}… to revoked after a "
        f"mid-revoke NEST crash + restart. error={app.error_text()!r}"
    )

    # --- the tear assertion: a completed MTA revoke, across a nest crash, left
    # the domain's nest-held DKIM key and its published record exactly as they
    # were — same selectors, same public values.
    def _published(selectors):
        return sorted((s["selector"], s["public_dns_value"]) for s in selectors)

    after = _dkim_selectors(nest, domain)
    assert _published(after) == _published(before), (
        f"an MTA revoke across a nest crash changed {domain!r}'s DKIM records — "
        f"a revoke must touch no DKIM key. before={_published(before)!r} "
        f"after={_published(after)!r}"
    )


# ---------------------------------------------------------------------------
# Journey 6 — the STALE slot (CR-2: a failed reset must not trap the client)
# ---------------------------------------------------------------------------

def _settled_launch_surface(driver, timeout: float = 30.0) -> str:
    """Poll until a relaunched client settles on the logged-in shell or the claim
    page, and say which. Waiting on only one of the two would time out ambiguously
    ("not visible after 30s" cannot tell a trapped client from a slow one), and a
    bare count() right after relaunch would race the boot.

    Checks BOTH `feed-tab` and `conversations-tab` for "logged-in shell reached":
    apple's own default landing tab is Conversations, not Feed
    (`AppState.selectedTab = "conversations"`), so `feed-tab` alone under-detects
    an apple relaunch that settled correctly — a real gap found investigating this
    test's iOS `[ios]` CR-2 leg (2026-07-15): `driver.get_state()["nav"]` showed
    `{"view": "conversations"}` with `session.authenticated == True` — a perfectly
    healthy, non-trapped launch — while this helper still reported "neither" for
    30s and the test failed on a phantom regression. Every other app's landing
    tab already satisfied `feed-tab`, which is why this went unnoticed until apple
    exercised the `[ios]` leg."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if driver.count("feed-tab") > 0 or driver.count("conversations-tab") > 0:
            return "feed"
        if driver.count("claim-code-input") > 0:
            return "claim"
        time.sleep(0.25)
    return "neither"


def _hold_factory_reset_dispatch(app, nest) -> None:
    """Confirm the reset against a LIVE nest that parks the dispatch ahead of
    its handler — the permanently-failed reset both CR-2 journeys need,
    arranged by construction instead of by winning a race.

    `rpc_hold_test_hook` parks a named kind in `dispatch_core::spawn_dispatch`,
    which sits BEFORE `meta_lookup` and therefore before `factory_reset_handler`
    ever runs: while the hold is armed the box cannot stage a reset marker, no
    matter how long the request sits there. So the caller may leave the nest up
    across the confirm click and still hold `claimed == true` deterministically.

    Two independent reasons the nest must be up for that click, either of which
    alone forces this shape:

    - **The apple actuation gate.** `fauna.admin.factory_reset` is `OnlineOnly`
      (`libs/fauna-protocol/src/offline_class.rs`), so `.faunaGate` disables the
      confirm control the instant the connection drops and the harness refuses
      to drive it — 409, correctly: no real user could click it either
      (`apple-e2e-automation.md` § The actuation gate). Clicking a live control
      is the honest arrangement; `--permissive-actuation` would be a
      session-wide bypass of a gate that is telling the truth.
    - **The client's own mint+persist.** Journey 11 asserts the slot the client
      itself wrote, and apple's `AdminNestVM.factoryReset()` awaits
      `api.getAccount()` before persisting — unbounded against an unreachable
      nest.

    The hold is never released: both callers kill this nest while it is still
    parked, and `crash_nest` is per-test, so nothing armed can outlive the test.
    """
    port = nest["port"]
    arm_rpc_hold(port, "fauna.admin.factory_reset")
    # Same "visible != enabled" reason as the danger-zone button above: the
    # confirm control has its own gate, and `wait_for` proves only that it
    # rendered.
    app.driver.wait_until_enabled("admin-factory-reset-confirm-button", timeout=15.0)
    app.driver.click("admin-factory-reset-confirm-button")
    # Arrival, not a sleep: `holding >= 1` says the request genuinely reached
    # the nest and is parked, which (mint-then-persist-then-dispatch) also
    # witnesses that the client's slot is already durable.
    # "No request arrived" has several very different causes and the nest-side
    # counter can distinguish none of them, so hand the failure the app's own
    # view (`e2e-conventions.md` point 6): a confirm control still on screen
    # says the click never dispatched; an error line says the client refused to
    # dispatch (the read-back guard's `factory_reset_persist_failed` arm).
    def _diagnose() -> str:
        confirm = app.driver.count("admin-factory-reset-confirm-button")
        return (
            f"app-side: confirm-button count={confirm}, "
            f"error={app.error_text()!r}, "
            f"launch surface: {launch_surface_dump(app)}"
        )

    wait_for_held_rpc(port, "fauna.admin.factory_reset", diagnose=_diagnose)


@pytest.mark.feature("factory-reset")
def test_failed_factory_reset_does_not_trap_the_client(killable_app, crash_nest):
    """CR-2 (common.md § Client-state recoverability). The mirror image of
    journey 4: the client persists its pending-factory-reset slot (CR-1, by
    design — the code must exist before the request goes out), but the dispatch
    then fails PERMANENTLY and the box is never wiped.

    The slot is deliberately not cleared on a dispatch error, because an error
    cannot distinguish "the nest never reset" from "the nest reset and the reply
    was lost" — and clearing in the second case is CR-1 all over again. But the
    slot short-circuits ahead of every other launch row, so a slot left behind by
    a permanently-failed reset used to route the client to a pre-filled claim page
    for a box that is still claimed and healthy — on EVERY launch, with no in-app
    exit short of a sign-out (which erases the identity). The nest is fine; the
    CLIENT is trapped.

    The fix is a boot reconcile (same doctrine as the reset itself): with a slot
    present, ask the box. It answers `claimed = true` ⇒ the reset never landed ⇒
    the slot is stale ⇒ clear it and take the ordinary rows.

    ⚠ Grace refinement (2026-07-17): `claimed = true` implies STALE only for a
    slot minted longer than `FACTORY_RESET_CLAIM_GRACE_SECS` ago. A FRESH slot
    on a still-claimed box is a reset IN FLIGHT (the box answers `Claimed` for
    a moment between the dispatch and its self-exit), and clearing it there
    destroys the only copy of the claim code moments before the wipe — the
    measured CR-1 loss journey 4 kept hitting. So the machine now HONORS a
    within-grace slot on `Claimed` (a recoverable claim page beats an
    unrecoverable clear), and this journey's subject is the STALE arm: the
    `seed_pending_factory_reset` seam arranges a slot with no mint timestamp
    (the pre-grace shape, read as stale), OVERWRITING any fresh slot the UI
    click's own mint landed. The within-grace failed-dispatch scenario — fresh
    slot + still-claimed box → pre-filled claim page with a visible
    "already claimed" exit — is a separate journey.

    Making the dispatch fail PERMANENTLY, deterministically, turns out to be
    the hard part. The two more-"natural" approaches are both provably racy
    and are kept here as the reasoning trail; the third is what this journey
    does:

    1. **Kill the nest before the click** (the original shape). The click
       still mints + persists the slot, then dispatches into what should be a
       dead socket — but the client's WS-RPC transport reconnects and retries
       automatically with NO per-call timeout (`fauna-ws-substrate::supervisor`,
       backoff 1s→60s, never gives up). If this harness's own later
       `start_nest_in_place()` resurrects the box before that retry gives up,
       the "permanently failed" dispatch actually SUCCEEDS against the
       resurrected nest, staging a real marker — the box then wipes on its
       next boot instead of staying claimed, and the precondition assertion
       below fails with `claimed: False` (confirmed empirically, not
       theorized).
    2. **Let the nest live through the click, kill it right as the dispatch
       arrives** (`NestLogWatch.wait_for_dispatch`, mirroring journey 4). Also
       racy, just at a different boundary: the receipt beacon
       (`dispatch_core.rs::spawn_dispatch`) fires before the kind-specific
       handler runs, but `stage_factory_reset` is synchronous local I/O with
       no `.await` in front of it — it outruns the cross-process
       log-poll-then-SIGKILL round trip almost every time (confirmed
       empirically: the marker was already staged by the time the SIGKILL
       landed).

    Both fail for the same underlying reason: an external, higher-latency
    Python reaction cannot reliably preempt a fast, synchronous in-process
    step on either side of the wire.

    3. **Park the dispatch in the nest, ahead of its handler** — what this
       journey does now (`_hold_factory_reset_dispatch`). `rpc_hold_test_hook`
       (landed 2026-08-27, `--features test-hooks`) holds a named kind in
       `dispatch_core::spawn_dispatch`, which runs BEFORE `meta_lookup` and so
       before `factory_reset_handler`: while armed, the box cannot stage a
       marker however long the request sits there. That removes the race
       rather than trying to win it — approaches 1 and 2 both needed an
       external actor to land *between* two fast in-process steps, and this
       one simply stops the second step from existing. The nest can then stay
       LIVE across the confirm click, and is taken down afterwards with the
       request still parked, which is what makes the dispatch permanently
       failed.

    ⚠ Approach 1 is also no longer *performable* on apple, independently of
    its raciness: `fauna.admin.factory_reset` is `OnlineOnly`, so `.faunaGate`
    disables the confirm control the moment the connection drops and the
    harness refuses to drive it (409 — correctly, since no real user could
    click it either). Confirming against a live nest is the honest
    arrangement, not a workaround for the gate; the session-wide
    `--permissive-actuation` bypass would be the workaround, and it would be
    bypassing a gate that is telling the truth.

    The client is still killed with no observation of the dispatch's own
    outcome, and the resulting fixture state is still arranged directly. The
    parked request witnesses that the client's own mint + persist landed (the
    mint precedes the dispatch by construction), so what the seam does here is
    AGE that record — move its `minted_at_secs` past the mint grace in the same host-visible
    credential file `launch()`'s `seed_credentials` already seeds pre-launch —
    rather than invent one. This is fixture arrangement, not the mutation
    under test
    (testing.md § point 8 carve-out (b)): CR-1
    (`test_kill_client_mid_factory_reset_floor_holds`) already proves the real
    mint-then-persist mechanism end-to-end; this journey's subject is the
    boot-time reconcile against a slot + a still-claimed box, which is
    exercised for real by the relaunch below either way.
    """
    app = killable_app
    nest = crash_nest

    # Same reason as journey 4: the slot is client-side state, so a relaunch that
    # silently discards the long-term store would make this test green on a client
    # that never wrote a slot at all — i.e. vacuous.
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so the stale-slot assertion would be vacuous"
        )

    # The stale slot is arranged through the driver-agnostic
    # `seed_pending_factory_reset` seam below (drivers/base.py), which writes each
    # platform's REAL store format — the per-actor registry slot
    # `fauna/<actor_id>/pending_factory_reset` in each platform's own durable store
    # — so the launch reconcile reads back a GENUINE slot on every killable driver. (A
    # path-only write of one platform's shape into another's store would not be read
    # back as a slot at all, so the client would land logged-in because there is NO
    # trap, not because the reconcile cleared one — a false green, worse than a red;
    # e2e_status_and_tips § Crash-recovery.) `killable_app` +
    # `preserve_state_across_relaunch()` above already restrict this journey to the
    # drivers that both support an unclean kill and implement the seam (linux +
    # macOS/iOS); a killable driver without the seam fails loudly (NotImplementedError),
    # never skips silently (testing.md § point 11).

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_nest()
    if app.driver.count("admin-factory-reset-button") == 0:
        app.admin.navigate_settings()
    # Visible != enabled: the danger-zone GroupBox is `.disabled(vm.isBusy)`
    # on apple, so the button renders before it will accept a click.
    app.driver.wait_until_enabled("admin-factory-reset-button", timeout=15.0)
    app.driver.click("admin-factory-reset-button")
    app.driver.wait_for("admin-factory-reset-confirm-button", timeout=10.0)

    # Park the reset's dispatch in the nest, ahead of its handler, so the box
    # can never be wiped no matter how the click goes (docstring, approach 3).
    # The nest stays LIVE across the confirm click, which is what a real admin
    # does — and what the apple actuation gate requires, since
    # `fauna.admin.factory_reset` is OnlineOnly and `.faunaGate` disables the
    # confirm control the instant the connection drops.
    _hold_factory_reset_dispatch(app, nest)

    # SIGKILL the client with its request still parked — do not wait for or
    # observe the dispatch's own outcome (see the docstring for why that's
    # exactly the un-winnable race). This destroys the client's factoryReset()
    # Task before it can ever see a reply.
    app.driver.kill_uncleanly()

    # Now take the nest down, still holding. The parked task dies with the
    # process having never reached `stage_factory_reset`, so the dispatch is
    # permanently failed and the box was never wiped — and the client that
    # issued it is already gone, so there is nothing left to retry against the
    # resurrection below.
    stop_nest(nest, graceful=False)

    # Age the pending-factory-reset slot in the durable store into the STALE
    # shape this journey's arm is about — the seam ages `minted_at_secs` past the
    # mint grace on
    # the record the client itself just wrote (or writes one for the active
    # account), so the install carries a genuine per-actor slot (drivers/base.py
    # owns why writing any other slot passes VACUOUSLY). The per-platform store shape lives in
    # the driver, not this test (testing.md § point 3), so this one journey runs
    # uniformly on every killable driver.
    app.driver.seed_pending_factory_reset(nest["url"], "admin", "e2e-cr2-synthetic-code")

    # The box comes back untouched — never wiped, still claimed. Deterministic:
    # the reset's dispatch was parked ahead of its handler for its whole life
    # and died there, so `stage_factory_reset` never ran; and the client that
    # issued it is dead, so there is no retry to reach this resurrection.
    start_nest_in_place(nest)
    deadline = time.monotonic() + 60.0
    st = None
    while time.monotonic() < deadline:
        st = setup_status(nest["url"])
        if st.get("claimed"):
            break
        time.sleep(0.5)
    assert st and st.get("claimed"), (
        f"precondition broken — the box must still be CLAIMED (the reset never "
        f"dispatched), else this journey is testing the CR-1 arm instead: {st!r}"
    )

    # Relaunch cold (recover(), not hard_reload(): a real user just reopens the
    # app, and the app's own launch routing is the code path under test).
    assert app.driver.recover(), "relaunch after the failed reset failed"

    # THE ASSERTION. Pre-fix, the stale slot short-circuits here and the client
    # lands on the pre-filled claim page for a box that is perfectly healthy —
    # forever, on every launch. Post-fix, the probe says `claimed`, the slot is
    # cleared, and the ordinary silent challenge logs the admin straight back in.
    surface = _settled_launch_surface(app.driver)
    assert surface == "feed" and app.driver.count("claim-code-input") == 0, (
        "a stale pending-factory-reset slot hijacked the launch: the client "
        f"settled on {surface!r}, not the logged-in shell — it is sitting on a "
        "claim page for a box that was never reset and is still claimed, with no "
        f"in-app exit (CR-2). error={app.error_text()!r} "
        f"nav={(app.driver.get_state() or {}).get('nav')!r} "
        f"session={(app.driver.get_state() or {}).get('session')!r}"
    )

    # And the slot is genuinely GONE, not merely out-ranked this once: relaunch
    # again and the client must STILL come up logged in. (A reconcile that routed
    # past a stale slot without deleting it would pass the assertion above and then
    # trap the user on the very next launch — so this is the assertion that makes
    # "cleared" mean cleared.)
    assert app.driver.recover(), "second relaunch failed"
    surface = _settled_launch_surface(app.driver)
    assert surface == "feed" and app.driver.count("claim-code-input") == 0, (
        f"the second launch settled on {surface!r}: the stale slot was routed past "
        f"but never cleared, so every future launch is trapped again (CR-2). "
        f"error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 11 — the FRESH slot honored within grace (the mirror twin of
# journey 6: a failed dispatch, not a genuine reset, but the box happens to
# still answer `Claimed` while the slot is still fresh — common.md § Client-
# state recoverability, grace refinement, ":431" queued journey)
# ---------------------------------------------------------------------------

def _wait_for_own_pending_factory_reset_mint(app, timeout: float = 40.0) -> None:
    """Block until the CLIENT'S OWN pre-dispatch mint+persist (CR-1's
    mint-then-persist-before-dispatch ordering) has genuinely landed on
    disk — never a fixed sleep (an empirically-measured real gap, not a
    theorized one: the first run of this journey on linux failed by killing
    too early, landing on 'feed' because no slot had been minted yet).

    Why this is needed at all (linux-specific, found empirically): unlike
    web (`doFactoryReset` sources the handle from a local `localStorage`
    cache — no network), linux's `Client::factory_reset` (client.rs) first
    awaits `AccountClient::get()` over the admin's LIVE session to source
    the AUTHORITATIVE handle — mirroring the same "ask the still-live
    session first" fix common.md documents for apple. Against this
    journey's already-dead nest (§ arrangement) that call cannot succeed; it
    runs out its full default RPC deadline (30s, `fauna-client::client.rs`)
    before falling back to the local account cache and proceeding to mint
    +persist. A driver with no on-disk credential file to poll (web) returns
    immediately — its own pre-mint step has no such network dependency.
    """
    cred_dir = getattr(app.driver, "_resolved_credential_dir", None)
    keyring_app = getattr(app.driver, "_resolved_keyring_app", None)
    if not cred_dir or not keyring_app:
        return
    import json
    import os

    path = os.path.join(cred_dir, f"{keyring_app}.json")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with open(path) as fh:
                store = json.load(fh)
            if any("pending_factory_reset" in k for k in store):
                return
        except (OSError, ValueError):
            pass
        time.sleep(0.5)
    raise TimeoutError(
        f"the client's own pending-factory-reset mint never landed in "
        f"{path!r} within {timeout:.0f}s of the confirm click — either the "
        f"pre-mint account lookup hasn't timed out yet (raise the bound) or "
        f"the mint/persist step itself is broken"
    )


@pytest.mark.feature("factory-reset")
def test_within_grace_failed_reset_honors_fresh_slot_with_visible_exit(
    killable_app, crash_nest
):
    """The within-grace failed-dispatch scenario the grace refinement exists
    for (common.md :429/:431), and the HONOR twin of journey 6's STALE arm:
    same held-dispatch arrangement, but this time the client's OWN mint+persist
    slot is left exactly as it wrote it — FRESH, not overwritten by the
    `seed_pending_factory_reset` seam — so the reconcile's `Claimed` arm must
    HONOR it (routes to the pre-filled claim page) rather than clear it.

    Why "the box still answers `Claimed`" is the recoverable branch, not a
    bug: between a client's reset dispatch and the box's self-exit the box
    genuinely still answers `Claimed` for a moment — and clearing a
    freshly-minted slot in exactly that window destroys the only copy of the
    claim code moments before the wipe (this was journey 4's real CR-1 loss,
    closed by this same grace mechanism — see the tombstones above). This
    journey's precondition is different from journey 4's: here the dispatch
    never reaches its handler at all (permanently failed, not merely
    in-flight), so the box is genuinely still claimed and healthy — the
    within-grace HONOR must therefore show a visible, retryable "already
    claimed" exit, not silently re-clear (CR-1 again) or strand the user.

    Arrangement mirrors journey 6's held-dispatch shape (see that docstring
    for why the two more-"natural" alternatives are racy): the nest is LIVE
    across the click but parks `fauna.admin.factory_reset` ahead of its
    handler, so `stage_factory_reset` never runs and the nest is then taken
    down with the request still parked — the box comes back untouched, still
    claimed, and the client that issued the request is dead, so no transport
    retry can reach the resurrection. Journey 6 then OVERWRITES the
    resulting slot with a stale (no-mint-timestamp) one via
    `seed_pending_factory_reset`; this journey does the opposite — it does
    NOT call that seam, leaving the client's own CR-1 mint+persist (which
    happens before the dispatch, by construction — `persistence.rs`'s
    mint-then-persist ordering) untouched, so the slot the reconcile sees is
    genuinely fresh (minted seconds ago, deep inside the 15-minute grace).
    Because journey 6 doesn't depend on the client's own mint landing at all,
    it never surfaces `_wait_for_own_pending_factory_reset_mint`'s gap below;
    this journey does, and must wait for it before killing the client.

    Assertions, per the LEAD track spec:
      1. the relaunch lands on the pre-filled claim page, not feed (the slot
         was honored, not cleared);
      2. submitting it against the still-claimed box surfaces a legible
         "already claimed" message — on `claim-code-status`, the element the
         claim page's per-field feedback actually renders on (NOT
         `error-message`, which `test_claim_code_invalid_renders_status_
         not_error_message` already establishes is not used for this class of
         rejection);
      3. the back-out affordance (`claim-code-back-button`) works — the page
         is never a dead end.
    """
    app = killable_app
    nest = crash_nest

    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a "
            "relaunch, so the fresh-slot assertion would be vacuous"
        )

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    app.admin.navigate_nest()
    if app.driver.count("admin-factory-reset-button") == 0:
        app.admin.navigate_settings()
    # Visible != enabled: the danger-zone GroupBox is `.disabled(vm.isBusy)`
    # on apple, so the button renders before it will accept a click.
    app.driver.wait_until_enabled("admin-factory-reset-button", timeout=15.0)
    app.driver.click("admin-factory-reset-button")
    app.driver.wait_for("admin-factory-reset-confirm-button", timeout=10.0)

    # Same mechanism as journey 6 (see its docstring, approach 3): park the
    # dispatch in the nest ahead of its handler, with the nest LIVE across the
    # confirm click. This journey needs the live nest twice over — the apple
    # actuation gate refuses the OnlineOnly confirm control while
    # disconnected, AND the client's own mint+persist must genuinely land,
    # which on apple sits behind a pre-persist `getAccount()` that has no
    # bound against an unreachable nest.
    _hold_factory_reset_dispatch(app, nest)

    # Unlike journey 6 (which overwrites the slot regardless, so it never
    # needs to wait), THIS journey's assertion depends on the client's own
    # mint+persist having genuinely landed — so wait for the observable
    # ground truth (the on-disk slot) before killing, never a fixed sleep.
    # The parked dispatch above already implies it (the mint precedes the
    # request by construction), but the on-disk read is the direct witness and
    # costs nothing on a green run.
    _wait_for_own_pending_factory_reset_mint(app)

    # SIGKILL the client (same non-observation reasoning as journey 6's
    # docstring for why not to wait on the dispatch itself). The wait above
    # already confirmed the slot is durable; nothing further arranges it.
    app.driver.kill_uncleanly()

    # Take the nest down with the request still parked — the handler never
    # ran, so the reset is permanently failed and the box was never wiped.
    stop_nest(nest, graceful=False)

    # The box comes back untouched — never wiped, still claimed (identical
    # precondition check to journey 6; the dispatch never reached
    # `stage_factory_reset`, so no marker was ever staged).
    start_nest_in_place(nest)
    deadline = time.monotonic() + 60.0
    st = None
    while time.monotonic() < deadline:
        st = setup_status(nest["url"])
        if st.get("claimed"):
            break
        time.sleep(0.5)
    assert st and st.get("claimed"), (
        f"precondition broken — the box must still be CLAIMED (the reset "
        f"never dispatched), else this journey is testing a different arm "
        f"entirely: {st!r}"
    )

    # Relaunch WELL within the 15-minute grace window (this happens within
    # seconds of the mint) — the honor arm must fire.
    assert app.driver.recover(), "relaunch after the failed reset failed"

    # ASSERTION 1: honored, not cleared. Landing on "feed" here would mean
    # the fresh slot was wrongly cleared — the CR-1 loss this grace mechanism
    # exists to prevent (journey 4's original bug, reached through this
    # journey's own scenario instead of journey 4's in-flight race).
    surface = _settled_launch_surface(app.driver)
    assert surface == "claim" and app.driver.count("claim-code-input") > 0, (
        f"a FRESH pending-factory-reset slot was not honored: settled on "
        f"{surface!r}, not the pre-filled claim page. error={app.error_text()!r} "
        f"launch surface: {launch_surface_dump(app)}"
    )

    # ASSERTION 2: submitting against the still-claimed box surfaces a
    # legible "already claimed" message on claim-code-status (the element
    # `test_claim_code_invalid_renders_status_not_error_message` establishes
    # this page's per-field feedback actually uses) — never a raw wire code,
    # never a silent stall.
    app.driver.click("claim-code-submit-button")
    status_deadline = time.monotonic() + 15.0
    status_text = ""
    while time.monotonic() < status_deadline:
        status_text = app.driver.get_text("claim-code-status")
        if "already" in status_text.lower() or app.driver.is_enabled(
            "claim-code-submit-button"
        ):
            break
        time.sleep(0.25)
    assert "already" in status_text.lower() and "claim" in status_text.lower(), (
        f"submitting against a still-claimed box must surface a legible "
        f"'already claimed' message on claim-code-status, got {status_text!r}. "
        f"error={app.error_text()!r}"
    )
    assert app.driver.is_enabled("claim-code-submit-button"), (
        "submit must re-enable after the already-claimed rejection — the "
        f"page must not get stuck: {status_text!r}"
    )

    # ASSERTION 3: back-out works — never a dead end (ui.yaml claim_code
    # § transitions: click claim-code-back-button → handle_entry).
    app.driver.click("claim-code-back-button")
    app.driver.wait_for("handle-input", timeout=10.0)


# ---------------------------------------------------------------------------
# Journey 7 — kill the client mid-DOMAIN-REMOVE
# (common.md § Client-state recoverability line 433: "domain remove" was the
#  un-audited inverse of journey 1. The nest handler is a SINGLE atomic
#  soft-delete — `db.soft_delete_mail_domain`, one UPDATE stamping `removed_at`,
#  the dependent DKIM/TLS/alias rows left intact for the atomic 30-day GC — so
#  a torn state cannot orphan anything and recovery is a truthful list + an
#  idempotent re-remove (`WHERE removed_at IS NULL` makes the retry a no-op).)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_client_mid_domain_remove_box_recovers(killable_app, crash_nest):
    """Add a domain, then UI-remove it (`fauna.bridges.remove_local_domain`); the
    client is SIGKILL'd the instant the nest's dispatcher receives the remove
    (beacon-timed). Whatever state resulted, a relaunched client must see the
    truthful domain list and be able to drive the box to "domain removed" — no
    off-box fix, no wedged half-removed state."""
    app = killable_app
    domain = "crash-recovery-remove.test"

    # Arrange: the FIRST domain a fresh box gets is auto-assigned as its
    # unremovable PRIMARY ("First domain claimed is the deployment's primary" —
    # bridge_routing_handlers.rs::add_local_domain_handler; a primary remove is
    # refused as fauna.protocol.malformed). The loopback crash_nest starts with
    # none, so seed a primary first, then add the NON-PRIMARY target we remove.
    _relaunch_trusting_nest(app.driver, crash_nest)
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    if not app.admin.dns_domain_names():
        app.admin.add_domain("crash-recovery-primary.test")
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            if "crash-recovery-primary.test" in app.admin.dns_domain_names():
                break
            time.sleep(0.5)
    app.admin.add_domain(domain)
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if domain in app.admin.dns_domain_names():
            break
        time.sleep(0.5)
    assert domain in app.admin.dns_domain_names(), (
        f"precondition: {domain!r} must be added (non-primary) before the journey "
        f"can kill mid-remove. error={app.error_text()!r}"
    )

    # Act: click that row's remove button, SIGKILL at the dispatch seam.
    idx = app.admin.dns_domain_names().index(domain)
    watch = NestLogWatch(crash_nest)
    app.driver.click("admin-dns-domain-remove-button", index=idx)
    beacon = watch.wait_for_dispatch("fauna.bridges.remove_local_domain")
    app.driver.kill_uncleanly()

    # Ground truth: the box must be alive and answering either way; the domain is
    # either soft-deleted or still active — BOTH are legitimate intermediate
    # states (the invariant demands recovery from whichever resulted).
    with _admin_ws(crash_nest) as adm:
        reply = adm.call("fauna.bridges.list_local_domains", {})
    still_active = any(row.get("domain_name") == domain
                       for row in reply.get("active", []))

    # Recovery: relaunch, re-establish the admin, drive to "domain removed"
    # through the UI — retry the remove only if the crash beat the commit, exactly
    # what a human does.
    app.driver.hard_reload()
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.admin.navigate_dns()
    app.driver.wait_for("admin-dns-add-domain-button", timeout=15.0)
    if domain in app.admin.dns_domain_names():
        assert still_active, (
            f"relaunched client's admin-dns page lists {domain!r} as active but the "
            f"nest soft-deleted it (beacon: {beacon!r}) — the recovered client sees "
            f"an untruthful state. error={app.error_text()!r}"
        )
        app.admin.remove_domain(domain)

    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        if domain not in app.admin.dns_domain_names():
            break
        time.sleep(0.5)
    assert domain not in app.admin.dns_domain_names(), (
        f"relaunched client could not drive the box to 'domain removed' after a "
        f"mid-remove crash (pre-kill still_active={still_active}, beacon: "
        f"{beacon!r}). error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 8 — kill the client mid-MAIL-DISABLE
# (common.md § Client-state recoverability line 433: "mail disable" was the
#  un-audited inverse of journey 2. DisableMail is a CLIENT-orchestrated
#  multi-RPC cascade — per credential: revoke_wrapped_mls_blob then
#  revoke_wrapped_submission_token then a per-row mail-plane save; the MSEK
#  clear is the last write (`fauna-client-mail-settings::machine.rs disable_mail`).
#  A crash tears it BETWEEN rows; each nest DELETE is idempotent and each row's
#  progress is committed, so a re-dispatched disable resumes from the survivors.)
# ---------------------------------------------------------------------------

@pytest.mark.feature("factory-reset")
def test_kill_client_mid_mail_disable_box_recovers(killable_app, crash_nest):
    """Enable mail with TWO credentials, then UI-disable (toggle → destructive
    confirm → DisableMail); the client is SIGKILL'd at the first credential's
    revoke dispatch, tearing the multi-RPC cascade mid-flight. A relaunched
    client must land on a truthful mail-settings page and be able to drive mail
    to fully disabled (0 credentials) — no off-box fix, no wedged half-disabled
    state."""
    app = killable_app

    # Arrange: mail enabled with two credentials so the disable cascade is
    # genuinely multi-write (a kill after the first revoke leaves the second).
    _relaunch_trusting_nest(app.driver, crash_nest)
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.mail_settings.navigate()
    app.driver.wait_for("mail-settings-enabled-toggle", timeout=15.0)
    app.mail_settings.enable_mail(display_name="Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        f"precondition: mail enable failed. error={app.error_text()!r}"
    )
    app.mail_settings.add_credential("Second")
    assert app.mail_settings.wait_for_credential_count_at_least(2, timeout=15.0), (
        f"precondition: second credential add failed. error={app.error_text()!r}"
    )

    # Act: destructive-confirm disable, SIGKILL at the first revoke dispatch. The
    # first cascade write is always revoke_wrapped_mls_blob for credential #1
    # (`disable_mail` → `revoke_credential`), so that beacon is the seam.
    app.driver.click("mail-settings-enabled-toggle")  # snaps back; opens the confirm
    app.driver.wait_for("mail-settings-disable-confirm-button", timeout=10.0)
    watch = NestLogWatch(crash_nest)
    app.driver.click("mail-settings-disable-confirm-button")
    beacon = watch.wait_for_dispatch("fauna.bridges.revoke_wrapped_mls_blob")
    app.driver.kill_uncleanly()

    # Ground truth: box answers; credentials/enablement are whatever the torn
    # cascade left.
    services = setup_status(crash_nest["url"])

    # Recovery through the UI: relaunch → mail settings renders truthfully, and
    # disable is drivable to completion. If any credential survived the tear, the
    # MSEK is still set (it is cleared LAST), so the toggle is still on and a
    # re-dispatched disable resumes; if the cascade happened to finish first,
    # the count is already 0.
    app.driver.hard_reload()
    inject_admin_session(app, crash_nest["url"], _admin_secret_hex(crash_nest))
    app.mail_settings.navigate()
    app.driver.wait_for("mail-settings-enabled-toggle", timeout=15.0)

    if app.mail_settings.credential_count() > 0:
        app.mail_settings.disable_mail()

    deadline = time.monotonic() + 20.0
    while time.monotonic() < deadline:
        if app.mail_settings.credential_count() == 0:
            break
        time.sleep(0.5)
    assert app.mail_settings.credential_count() == 0, (
        f"relaunched client could not drive mail to fully disabled (0 credentials) "
        f"after a mid-disable crash (beacon: {beacon!r}; services at kill: "
        f"{services!r}). error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 10 — kill the NEST mid-admin-mail-disable + boot-reconcile the
# DB-toggle-vs-`imap-enabled`-flag tear
# (common.md § Client-state recoverability: the ADMIN deployment-wide
#  `set_mail_enabled(false)` toggle — distinct from journey 8's per-USER
#  DisableMail — does TWO durable writes: the authoritative `mail_enabled` DB
#  toggle AND the derived `{data_dir}/imap-enabled` flag file the MDA's s6
#  run-script gates on. A nest crash between them tears them. The DB is the
#  single decision point; the boot reconcile re-asserts the flag from it.)
# ---------------------------------------------------------------------------

MAIL_ENABLE_FLAG = "imap-enabled"  # nest mail_enable.rs::MAIL_ENABLE_FLAG


def _imap_enabled_flag_path(nest) -> str:
    """On-disk `{data_dir}/imap-enabled` flag. The nest's data dir IS its
    tmp_dir (db path is `{tmp_dir}/nest.db`, so `data_dir_from_db_path` = the
    tmp_dir). Read directly as external black-box verification (testing.md
    § point 8 carve-out (c) — observing the box's own on-disk output, not a UI
    mutation standing in for a user)."""
    return os.path.join(nest["tmp_dir"], MAIL_ENABLE_FLAG)


def _deployment_mail_enabled(nest) -> bool:
    """Admin side-channel read of the deployment-wide mail toggle
    (`fauna.bridges.get_mail_config` → FetchConfigReply.mail_enabled — an
    observation, not a mutation). NOTE this collapses the UNSET toggle to True
    (`get_mail_enabled().unwrap_or(true)`, bridge_routing_handlers.rs), so it
    can't tell 'never touched' from 'explicitly on' — the on-disk flag below is
    the authoritative 'explicitly enabled' signal. Only meaningful once the
    toggle has been explicitly written (Some(false) → False here)."""
    with _admin_ws(nest) as adm:
        return bool(adm.call("fauna.bridges.get_mail_config", {})["mail_enabled"])


def _drive_mail_flag(app, nest, *, present: bool, timeout: float = 40.0) -> None:
    """Drive the deployment-wide mail toggle until the on-disk `imap-enabled`
    flag matches `present`, THROUGH THE ADMIN UI (`admin-mail-enabled-toggle` →
    `fauna.bridges.set_mail_enabled`; testing.md § point 8 — the mutation under
    test is UI-driven, never a raw RPC).

    The flag file — written only by an explicit `set_mail_enabled` — is the
    ground truth (the wire `mail_enabled` projection defaults the unset toggle to
    True, so a fresh box shows the toggle ON with no flag on disk; reaching
    'flag present' from there takes two clicks: off, then on). Clicks at most
    once every few seconds, re-checking the flag each time, so a landed click is
    never double-toggled into an overshoot."""
    flag = _imap_enabled_flag_path(nest)
    app.admin.navigate_mail()
    app.driver.wait_for("admin-mail-enabled-toggle", timeout=15.0)
    deadline = time.monotonic() + timeout
    last_click = -100.0
    while time.monotonic() < deadline:
        if os.path.exists(flag) == present:
            return
        if time.monotonic() - last_click > 3.0:
            app.admin.toggle_mail_enabled()  # flips + dispatches set_mail_enabled
            last_click = time.monotonic()
        time.sleep(0.5)
    assert os.path.exists(flag) == present, (
        f"admin could not drive the `imap-enabled` flag to present={present} "
        f"through the UI. error={app.error_text()!r}"
    )


@pytest.mark.feature("factory-reset")
def test_kill_nest_mid_admin_mail_disable_reconciles(killable_app, crash_nest):
    """The admin deployment-wide mail-DISABLE tear (common.md § Client-state
    recoverability). `set_mail_enabled(false)` writes TWICE — the authoritative
    `mail_enabled` DB toggle, then the derived `{data_dir}/imap-enabled` flag the
    MDA's s6 run-script gates on. A nest crash between them tears them (DB says
    off, flag still present). The DB is the single decision point; the boot
    reconcile (`reconcile_mail_enable_flag_once`, spawned at startup) re-asserts
    the flag from it, so any torn state self-heals with no off-box fix.

    Two prongs, mirroring journey 9's pattern (deterministic tear guard + real
    crash recovery):

    A. REAL-CRASH RECOVERY. Drive the disable through the admin UI, SIGKILL the
       fixture's own nest at the `set_mail_enabled` dispatch (beacon-timed — the
       anti-vacuous proof it was in flight), restart, and drive mail to a
       consistent OFF state through the UI. The two writes are ADJACENT (no await
       between them), so a race-timed kill lands cleanly either side of a
       ~zero-width window far more often than inside it — this prong is therefore
       the end-to-end RECOVERY proof through a real crash, not the tear proof.

    B. BOOT-RECONCILE WIRING (the non-vacuous tear proof). Deterministically seed
       the exact torn on-disk state a crash between the two writes leaves — DB
       OFF (just driven) with `imap-enabled` re-present (fixture arrangement,
       testing.md § point 8 carve-out (b)) — restart the real nest binary, and
       assert the boot reconcile removes the flag PROMPTLY (well inside the 60s
       periodic interval, so ONLY an immediate boot reconcile passes). This is
       the tier_3 proof that the boot reconcile is wired into the shipped binary;
       the heal LOGIC is proved by the nest unit test
       `reconcile_removes_flag_when_disabled_but_flag_present`."""
    app = killable_app
    nest = crash_nest
    flag = _imap_enabled_flag_path(nest)

    _relaunch_trusting_nest(app.driver, nest)
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))

    # --- precondition: deployment mail explicitly ON — an explicit
    # `set_mail_enabled(true)` (DB toggle Some(true) AND the `imap-enabled` flag
    # written). The wire projection's unset→true default can't stand in for it:
    # the flag file is written only by a real toggle.
    _drive_mail_flag(app, nest, present=True)
    assert os.path.exists(flag), (
        f"precondition: enabling deployment mail should have created "
        f"{MAIL_ENABLE_FLAG!r} at {flag!r}. error={app.error_text()!r}"
    )

    # --- Prong A: disable through the admin UI; SIGKILL the nest the instant its
    # dispatcher receives `fauna.bridges.set_mail_enabled`.
    app.admin.navigate_mail()
    app.driver.wait_for("admin-mail-enabled-toggle", timeout=15.0)
    watch = NestLogWatch(nest)
    app.driver.click("admin-mail-enabled-toggle")  # dispatches set_mail_enabled(false)
    beacon = watch.wait_for_dispatch("fauna.bridges.set_mail_enabled")
    stop_nest(nest, graceful=False)   # SIGKILL mid-handler
    start_nest_in_place(nest)         # supervisor restart → boot reconcile

    # --- recover: drive mail to a consistent OFF state through the UI. The crash
    # landed cleanly either side of the ~zero-width window (or, rarely, torn — the
    # boot reconcile heals that at restart); either way a client can drive it to a
    # consistent OFF (flag absent) with no off-box fix — the recovery proof.
    inject_admin_session(app, nest["url"], _admin_secret_hex(nest))
    _drive_mail_flag(app, nest, present=False)
    assert not os.path.exists(flag), (
        f"after a mid-disable NEST crash + restart, a client could not drive the "
        f"`imap-enabled` flag to a consistent OFF state (beacon: {beacon!r}). "
        f"error={app.error_text()!r}"
    )

    # --- Prong B: seed the exact torn state a crash between the two writes leaves
    # (DB OFF — just driven — with the flag re-present), restart, and assert the
    # BOOT reconcile removes it well inside the 60s periodic interval. Only an
    # immediate first-tick reconcile can pass this — the anti-vacuous proof the
    # reconcile is wired at boot, not merely reachable on the slow tick.
    assert not _deployment_mail_enabled(nest), "prong B precondition: DB toggle is OFF"
    with open(flag, "w") as fh:
        fh.write("")  # the torn flag a crash would have stranded
    assert os.path.exists(flag)
    stop_nest(nest, graceful=True)
    start_nest_in_place(nest)

    deadline = time.monotonic() + 20.0
    while os.path.exists(flag) and time.monotonic() < deadline:
        time.sleep(0.25)
    assert not os.path.exists(flag), (
        f"the nest's BOOT reconcile did not remove a seeded torn `imap-enabled` "
        f"flag (DB toggle OFF) within 20s of restart — the flag-vs-state reconcile "
        f"is not wired at boot (it would only run on the 60s periodic tick), so the "
        f"box serves a stale mail-enable flag for a full interval after every "
        f"restart. error={app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# Journey 12 — kill the client BETWEEN THE TWO LEGS of a devices-page removal
# (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
#  reclamation*, clause (4), *The completion rule*; `common.md` § Client-state
#  recoverability.)
#
# A removal is two legs on two systems — `fauna.sync.devices.delete` at the
# nest and the absorbing `Removed` row on the account-data plane — and the nest
# deletion is the SINGLE decision point. The durable intent
# (`<actor>/pending-fleet-removals` in the credential slot) is staged BEFORE
# it; a client that dies in between leaves a device the nest has forgotten but
# the fleet still wraps to, for ever, with the user already told it was
# removed. The pump's once-per-full-pass reconcile
# (`fleet_removal::complete_pending`) is what closes that: roster asked, row
# absent ⇒ `Gone` ⇒ the `Removed` rows are journaled, with NO user gesture.
#
# The kill window is ARRANGED, never raced (convention 14). `rpc_hold` parks a
# named kind in `dispatch_core::spawn_dispatch`, which sits AHEAD of the
# handler, so with `fauna.sync.devices.delete` armed the sequence is exact:
#
#   1. the page stages the intent — it precedes the RPC by construction
#      (`DevicesMachine::remove_device`) — and dispatches;
#   2. `wait_for_held_rpc` proves the request reached the nest and is parked:
#      the client is alive, the deletion has NOT run;
#   3. the client is SIGKILL'd while it is parked;
#   4. only THEN is the hold released, so the nest commits the deletion with
#      the client already dead and its reply going nowhere.
#
# That is the between-the-legs state by construction rather than by winning a
# race — the limit journeys 4-6 and 9 document — which is why this journey can
# assert the reconcile itself instead of "whatever resulted". It fails if
# `complete_pending` is removed from the pump: nothing else ever journals that
# row.
# ---------------------------------------------------------------------------

#: The removed sibling's `Removed` row reaching the relaunched remover's own
#: plane replica. A poked pass returns on the first poll; the budget only ever
#: buys a genuine failure its diagnosis (convention 14).
FLEET_REMOVAL_CONVERGED_S = 180.0

#: The nest reflecting the released deletion. One already-dispatched handler.
ROSTER_DELETION_VISIBLE_S = 60.0


def _staged_fleet_removals(driver, actor_id_hex: str) -> dict[str, list[str]]:
    """This launch's staged removal intents, as `{row: [target fleet id, …]}`.

    Reads the slot the app itself wrote — `<actor>/pending-fleet-removals`,
    `fauna_core::fleet_removal::encode_pending`'s spelling
    (``"<row>:<id>,<id>;<row>@<ms>;<row>:<id>"`` — a ``<row>@<ms>`` group is
    the reconcile's sighting of the row still on the roster, and carries no
    ``:``, so it is skipped here) — through the harness's one
    account-store adapter. Public values only: rows and fleet ids, never key
    material (`principal_bundle`'s own note on the attribute).

    ⚠ Read it BEFORE the relaunch as the between-the-legs proof, and again
    AFTER as the completion proof: `hard_reload()` gives the app a fresh
    credential dir, and what carries the intent into it is the driver's
    principal-slot carry (WHOLE slots — the bare writer entry plus every
    ``<actor>/…`` entry beside it — `drivers/http_bridge.py`), which is exactly
    the production fact that a real install keeps its slot across a restart.
    """
    store = attach_account_store(app_name(driver), driver)
    encoded = store.read_map().get(f"{actor_id_hex}/pending-fleet-removals") or ""
    staged: dict[str, list[str]] = {}
    for group in encoded.split(";"):
        if not group or ":" not in group:
            continue
        row, ids = group.split(":", 1)
        staged[row] = [i for i in ids.split(",") if i]
    return staged


@pytest.fixture
def fleet_sibling_seat(request, crash_nest):
    """Factory for a SECOND enrolled device on ``crash_nest`` —
    ``seat = fleet_sibling_seat(user)`` (`helpers.fleet.SiblingSeats`, the one
    home of the sibling seat, shared with the member-removal journey)."""
    seats = SiblingSeats(request, crash_nest)
    try:
        yield seats.launch
    finally:
        seats.teardown()


def test_kill_client_between_the_removal_legs_reconcile_finishes_it(
    killable_app, crash_nest, fleet_sibling_seat, request
):
    """Two enrolled devices; seat one removes seat two from Settings → Devices
    and is SIGKILL'd while the nest still holds the delete parked. The
    deletion is then committed with no client alive to settle it. A relaunched
    seat one must journal seat two's `Removed` row on its FIRST full pump pass,
    with no user gesture — clause (4)'s completion rule, end to end.

    The three things that make it a proof rather than a hopeful sequence:

    * the intent is read out of the credential slot AFTER the kill, so the
      client provably died between the legs (the vacuous alternative — it
      settled before the kill — would show an empty slot);
    * the nest's roster is read over the admin side channel, so the deletion
      provably committed (on a roster that still held the row, `complete_pending`
      correctly writes nothing — it waits out the in-flight bound, then drops
      the intent);
    * the sibling is stopped before the gesture, so nothing re-registers its
      row underneath the reconcile and turns a completion back into a wait.
    """
    from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps
    from conftest import _E2E_LOGIN_DEVICE_ID, _make_user

    app = killable_app
    nest = crash_nest
    driver = app.driver
    url = nest["url"]
    port = nest["port"]

    # A dedicated nest enforces its tier quotas, and the shipped `free` seed is
    # sized for ONE person — 2 devices. This journey's whole premise is a second
    # enrolled device, and each seat's co-located sync agent takes a row beside
    # its app's, so the shipped cap refuses seat two's enrolment before it can
    # publish anything (measured: an `enrollment-refused` slot entry and no
    # `grant-registered` row). Admitted at a cap that does not bind, the way the
    # shared identity's own fixture does; the cap itself is proven nest-side by
    # `conformance_device_tier_cap.rs`, so this costs no coverage.
    set_tier_caps(
        nest["port"],
        admin_signing_key=nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES,
        base_url=url,
    )

    _relaunch_trusting_nest(driver, nest)
    user = _make_user(nest)
    actor = user["actor_id_hex"]
    enrollment.sign_in(app, request, nest, user)
    require_device_set_reader(driver)

    # --- arrange: two genuinely distinct enrolled devices on one account, each
    # behind its own enrolment latch (never a settle-sleep — helpers/enrollment).
    writer_one, row_one, _ = enrollment.await_enrollment(app, url, user)
    me = fleet_id_hex(writer_one)

    sibling = fleet_sibling_seat(user)
    writer_two, row_two, roster = enrollment.await_enrollment(sibling, url, user)
    target = fleet_id_hex(writer_two)

    assert row_one != row_two and me != target, (
        f"the two seats did not enrol as distinct devices (rows {row_one!r} vs "
        f"{row_two!r}, fleet ids {me[:16]}… vs {target[:16]}…) — they shared a "
        f"credential world, so there is no sibling to remove."
    )

    # The removal reads client-held plane state, never the roster, so the merge
    # is the real precondition — and the only one a fake roster row cannot meet.
    await_fleet_member(app, sibling, target, budget_s=FLEET_MEMBER_VISIBLE_S)

    # Stop the sibling BEFORE the gesture. A live seat re-registers its own row
    # on its next enrolment pass, and the reconcile asks the roster: a row back
    # in it reads as a deletion that may still be in flight, so the intent
    # waits (then drops unwritten) and no `Removed` row lands within this
    # journey's budget — a red with nothing wrong with the product. A removed device
    # is one that is gone, which is what this models.
    sibling.driver.teardown()

    # --- the roster→card mapping, asserted rather than assumed. The page
    # renders `fauna.sync.devices.list` in order (`DevicesMachine::render_devices`
    # maps it 1:1), and both rows carry the same plaintext label
    # (`SELF_REGISTER_LABEL`), so a name lookup cannot tell them apart. The
    # `device-this-mark-badge` is the honest discriminator and doubles as the
    # proof the ordering held.
    #
    # ⚠ WHICH row the mark names is the app's own rule, not ours to assume
    # (`behavior/devices.md` § This-device marker — `this_device_row`: the
    # ENROLLED row wins, the app's own `device.db` id is the fallback). tui and
    # linux read the enrolled row, so the mark sits on seat one's enrolment row.
    # macOS still compares the app's own id (the gap 
    # retires) and hosts a sync agent, so its enrolled row and its own-id row —
    # the forced login id, which its app registers for itself — are TWO roster
    # rows, and the mark sits on the second. Both are rows seat one may call
    # itself; the sibling's never is. Accepting either keeps the proof (the mark
    # is on exactly one card and it is one of seat one's own) without pinning
    # the rule, and needs no edit when row 332 moves macOS onto the enrolled row.
    app.backups.navigate_devices()
    await_device_removal_ready(driver)
    driver.wait_for("device-card", timeout=30.0)
    listed = list(enrollment.roster(url, user))
    assert row_one in listed and row_two in listed, (
        f"the nest's roster {listed} lost one of the two enrolled rows "
        f"({row_one!r}, {row_two!r}) before the gesture."
    )
    index_one, index_two = listed.index(row_one), listed.index(row_two)
    painted = app.backups.device_count()
    assert painted == len(listed), (
        f"the devices page paints {painted} card(s) for a roster of "
        f"{len(listed)} row(s) {listed} — the index mapping this journey aims "
        f"its click with does not hold. {driver.diagnose('device-card')}"
    )
    # Read once for every card, so a red names the whole picture — WHICH card
    # does carry the mark — rather than only the card the click was aimed at.
    marks = [
        driver.count("device-this-mark-badge", scope=f"device-card[{i}]")
        for i in range(painted)
    ]
    picture = (
        f"roster order (short id: label) "
        f"{[(rid[:8], lbl) for rid, lbl in enrollment.roster(url, user).items()]}; "
        f"seat one's enrolment row {row_one[:8]} is card[{index_one}], the "
        f"sibling's {row_two[:8]} is card[{index_two}]; this-device badge "
        f"count per card: {marks}"
    )
    own_cards = {index_one}
    if _E2E_LOGIN_DEVICE_ID in listed:
        own_cards.add(listed.index(_E2E_LOGIN_DEVICE_ID))
    marked = [i for i, n in enumerate(marks) if n]
    assert len(marked) == 1 and marked[0] in own_cards, (
        f"the This-device mark is on card(s) {marked}, not exactly one of seat "
        f"one's own {sorted(own_cards)} (its enrolment row, or the forced login id "
        f"the app registers for itself), so the roster order is not the paint "
        f"order and card[{index_two}] is not the sibling — refusing to aim a "
        f"removal at an unidentified row. {picture}. "
        f"{driver.diagnose('device-this-mark-badge')}"
    )
    assert marks[index_two] == 0, (
        f"card[{index_two}] — the sibling's row {row_two!r} — is ALSO marked as "
        f"this device; the two seats resolved to one enrolled row. {picture}."
    )

    # --- the action: remove the sibling, and kill seat one while the nest holds
    # the delete parked ahead of its handler. `wait_for_held_rpc` is the
    # anti-vacuous proof the request genuinely arrived (and therefore that the
    # intent, which precedes it, is staged); the release AFTER the kill is what
    # commits the deletion with no client left to settle it.
    #
    # ⚠ The click is dispatched on a THREAD, and that is load-bearing rather
    # than tidy. A bridge command is acked only once the app has applied it, and
    # `remove_device` is awaited end to end — so against a PARKED delete the
    # click does not return until the client's own RPC budget expires and the
    # page paints "the nest took too long". By then the park is gone and the
    # window is closed: measured exactly that way before this thread existed,
    # as `holding: 0, released: 0` with a timeout on `error-message`. The
    # watcher has to be the one waiting, not the clicker.
    arm_rpc_hold(port, "fauna.sync.devices.delete")
    clicker = threading.Thread(
        target=lambda: driver.click("device-remove-button", index=index_two),
        name="devices-remove-click",
        daemon=True,   # it dies with the app it is driving; its failure, if any,
    )                  # is read off the hold status and `error-message` below.
    try:
        def _diagnose() -> str:
            return (
                f"app-side: device-card count={driver.count('device-card')}, "
                f"error={app.error_text()!r}. No delete arrived: the page refused "
                f"the removal before dispatching (an unavailable account runtime, "
                f"or a row that resolved to no fleet member) — read `error-message`."
            )

        clicker.start()
        wait_for_held_rpc(port, "fauna.sync.devices.delete", diagnose=_diagnose)
        driver.kill_uncleanly()
    finally:
        # Released the instant the client is dead, so the handler commits ahead
        # of the nest noticing the dropped socket.
        release_rpc_hold(port, "fauna.sync.devices.delete")
        clicker.join(timeout=30.0)

    # --- ground truth 1: the nest deletion committed (the decision point).
    wait_until(
        lambda: row_two not in enrollment.roster(url, user),
        ROSTER_DELETION_VISIBLE_S,
        interval=0.5,
        diagnose=lambda: (
            f"the released `fauna.sync.devices.delete` never removed {row_two!r} "
            f"from the roster {sorted(enrollment.roster(url, user))}. With the row "
            f"still held the reconcile writes nothing by design, so the rest of "
            f"this journey would assert nothing."
        ),
    )

    # --- ground truth 2: the client died BETWEEN the legs. The intent is still
    # staged, naming the row it deleted and the fleet id it owes a `Removed`
    # row. An empty slot here is the vacuous kill this journey is built to rule
    # out — and the hold makes it unreachable, so it reads as a real failure.
    staged = _staged_fleet_removals(driver, actor)
    assert row_two in staged and target in staged[row_two], (
        f"seat one holds no staged removal for {row_two!r} → {target[:16]}… after "
        f"the kill (slot: { {k: [t[:16] for t in v] for k, v in staged.items()} }). "
        f"Either the fleet leg never armed — the row resolved to no member, so "
        f"`DevicesMachine::remove_device` deleted the nest row alone — or the "
        f"intent was settled before the kill, which the parked delete forbids. "
        f"Nothing is left for the reconcile to finish."
    )

    # --- recover: relaunch and let the pump reconcile. NO gesture — the page is
    # never driven again; the poke only spends the production cadence.
    driver.hard_reload()
    enrollment.sign_in(app, request, nest, user)
    last: dict = {}

    def removed():
        poked_pass(driver, what="the fleet-removal reconcile")
        state = device_set_state(driver, target)
        last.clear()
        last.update(state)
        return state.get("found") and state.get("state") == "removed"

    wait_until(
        removed, FLEET_REMOVAL_CONVERGED_S, interval=1.0,
        diagnose=lambda: (
            f"a relaunched seat one never journaled the `Removed` row for "
            f"{target[:16]}… (last device-set read: {last}); its staged intent "
            f"now reads "
            f"{ {k: [t[:16] for t in v] for k, v in _staged_fleet_removals(driver, actor).items()} }. "
            f"The nest row was deleted and the intent survived the crash, so "
            f"`fleet_removal::complete_pending` — the pump's once-per-full-pass "
            f"reconcile — either did not run or could not read the roster. The "
            f"removed device stays a verified fleet member and a wrap target for "
            f"ever, with the user already told it was removed: clause (4)'s leak."
        ),
    )
    assert last.get("removed_by") == me, (
        f"the `Removed` row for {target[:16]}… is attributed to "
        f"{str(last.get('removed_by'))[:16]}…, not to the removing device "
        f"{me[:16]}… — the reconcile wrote it under the wrong writer."
    )

    # --- and the intent is spent: `settle`'s `Gone` arm clears it only once
    # every `Removed` row is journaled, so a surviving entry would mean the
    # next pass rewrites the row for ever.
    leftover = _staged_fleet_removals(driver, actor)
    assert row_two not in leftover, (
        f"the reconcile journaled the `Removed` row but left {row_two!r} staged "
        f"({ {k: [t[:16] for t in v] for k, v in leftover.items()} }) — the intent "
        f"is never spent and every later pass re-settles it."
    )


# ---------------------------------------------------------------------------
# Journey 13 — kill the owner's app BETWEEN A FOLDER MEMBER REMOVAL'S STAGED
# ROTATION AND ITS PUBLISH
# (`mls-group-key-material.md` § M2 → *Rotate-on-removal*, the launch-time
#  resume and its second edge; `common.md` § Client-state recoverability.)
#
# Removing a member is a chain on three systems, in this order
# (`FoldersAuthor::remove_member` → `drive_removal` → `finish_rotation`):
#
#   1. the fresh content key is STAGED in the owner's folder-key custody — the
#      sentinel, durable before anything is sent;
#   2. the MLS Remove commit is distributed and merged (the epoch advances);
#   3. the rotated key envelope is PUBLISHED — `fauna.folders.content_key.put`;
#   4. the member is evicted from the nest roster — `fauna.folders.members.evict`;
#   5. custody commits the generation and settles the sentinel.
#
# An owner whose app dies after 1 and before 3 leaves a member the user already
# removed still rostered and still served the folder's key bundle. Nothing but
# the owner's next launch can finish it: the launch folder pass
# (`resume_pending_removals`) re-drives every staged sentinel.
#
# The kill window is ARRANGED, never raced (convention 14), with the seam
# journey 12 uses and for the same reason: `rpc_hold` parks a named kind ahead
# of its handler. An outside observable DOES separate the stage from the
# publish — the publish is its own wire kind and nothing earlier in the chain
# sends it — so no fault-injection seam inside the app was needed (convention
# 15). With `fauna.folders.content_key.put` armed:
#
#   1. the owner's Remove click stages the sentinel and distributes the commit
#      — both precede the publish by construction — and dispatches the put;
#   2. `wait_for_held_rpc` proves the put reached the nest and is parked: the
#      app is alive, the publish has NOT run;
#   3. the app is killed while it is parked;
#   4. the parked put is then REFUSED, not released: its handler never runs, so
#      the nest is left exactly as a crash before the publish leaves it.
#
# It fails if the launch pass stops running, runs before custody is readable
# and gives up, or cannot finish a removal whose commit already went out.
# ---------------------------------------------------------------------------

#: The relaunched owner finishing the removal: one launch pass — engine
#: restore, custody read, publish, evict, settle. Returns on the first poll
#: that sees the eviction; the budget only buys a failure its diagnosis.
REMOVAL_RESUMED_S = 180.0

_PUBLISH_KIND = "fauna.folders.content_key.put"

#: The launch pass's own completion line, per app: the shared native trigger
#: (`fauna_client_folders::launch_resume`) and web's (`$lib/conversations`).
#: Logged only after `resume_pending_removals` returned `Ok`, which is after
#: the sentinel settled — the app's own word that the staging is gone.
_RESUMED_LINES = (
    re.compile(r"folders: resumed (\d+) crash-staged member removal\(s\) at launch"),
    re.compile(r"folders: launch pass ran — (\d+) crash-staged member removal\(s\) resumed"),
)


def _launch_log(driver) -> str:
    """This launch's app log: the browser console on web, the captured stderr
    on the apps that own a process."""
    if driver.is_web():
        return "\n".join(driver.console_log())
    return driver.app_stderr_text()


def _folder_pass_lines(driver) -> list[str]:
    """The tail of this launch's folder-pass log lines, for a failure message."""
    return [ln for ln in _launch_log(driver).splitlines() if "folders:" in ln][-10:]


def _removals_resumed_at_launch(driver) -> int:
    """How many staged removals this launch's folder pass reports it finished."""
    text = _launch_log(driver)
    return sum(int(m.group(1)) for rx in _RESUMED_LINES for m in rx.finditer(text))


@pytest.mark.parametrize(
    "folder_share_owner_app",
    # tui is the lead app. On web the kill is the tab closing with its storage
    # kept (`WebDriver.kill_uncleanly`), and the relaunch a fresh boot over it.
    # linux drives the launch resume from its own restore trigger
    # (`conv_backend.rs::resume_folder_removals`), not the `fauna-ffi` factory.
    # macos and ios take the resume from the `fauna-ffi` session factory, and
    # read the launch line through the driver's `app_stderr_text`. windows takes
    # the same factory resume; its kill is the FlaUI bridge's hard
    # `DELETE /session` (`WindowsDriver.kill_uncleanly`) and its app log is
    # append-shared across relaunches, which the `>= 1` count below tolerates.
    [
        "tui",
        pytest.param("web", marks=pytest.mark.web),
        pytest.param("linux", marks=pytest.mark.linux),
        pytest.param("macos", marks=pytest.mark.macos),
        pytest.param("ios", marks=pytest.mark.ios),
        pytest.param("windows", marks=pytest.mark.windows),
    ],
    indirect=True,
)
@pytest.mark.real_conversations
@pytest.mark.timeout(1200)
@pytest.mark.feature("share-a-folder")
def test_kill_owner_between_a_member_removals_stage_and_publish_relaunch_finishes_it(
    folder_share_owner_app,
):
    """An owner shares a folder with two people and removes one; the app dies
    with the removal staged and its rotated key not yet published. The next
    launch must finish the removal with no user step: the removed person is off
    the nest's roster and refused the key bundle, the other is untouched, and
    the app reports the staged removal resumed.

    What makes it a proof rather than a hopeful sequence:

    * the publish is PARKED at the nest when the app is killed, and then
      refused — so the app provably died after the stage and before the
      publish (the vacuous alternative, a removal that had already finished,
      would show the member gone from the roster before the relaunch; that is
      asserted against);
    * a second member stays, so "the roster lost a member" cannot be satisfied
      by the folder losing its whole group, and the member removed is checked
      by identity;
    * the relaunch drives no gesture on the folder until the nest already
      shows the eviction — the page is opened afterwards only to read the
      owner's own roster.

    The store is pinned across the relaunch: a crashed app comes back over its
    own long-term store (its MLS engine state included), which is the crash
    this journey is about; a fresh store would be a different journey, the
    restore of a lost device.
    """
    from helpers.app_surface import skip_unbuilt
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    driver = app.driver
    port = nest["port"]

    if not driver.supports_unclean_kill():
        skip_unbuilt(
            driver,
            surface="the unclean-kill primitive (driver-owned Popen/pty child)",
            detail="see PlatformDriver.kill_uncleanly",
            tracked="testing.md",
        )
    if not driver.preserve_state_across_relaunch():
        skip_unbuilt(
            driver,
            surface="a long-term store the driver can keep across a relaunch",
            detail="the resumed removal reads the crashed app's own MLS engine "
            "state; without the pin the relaunch is a lost-device restore",
            tracked="drivers/http_bridge.py::preserve_state_across_relaunch",
        )

    # --- arrange: a folder shared with two people who never open an app.
    leaves = headless_member(nest, "leaves")
    stays = headless_member(nest, "stays")
    ob = app.backups
    name = f"crash-removal-{secrets.token_hex(4)}"
    ob.navigate_folders()
    ob.create_folder_via_wizard(name)
    share_through_owner_ui(app, name, leaves, want=1)
    row = share_through_owner_ui(app, name, stays, want=2)

    def nest_roster() -> list[str]:
        return conv_api.folder_member_actors(port, owner, name)

    rostered = nest_roster()
    assert leaves["actor_id_hex"] in rostered and stays["actor_id_hex"] in rostered, (
        f"both shares must have landed on the nest's roster before the removal; it "
        f"reads {rostered!r}"
    )
    served = content_key_get(nest, leaves, name)
    assert isinstance(served, dict) and served.get("sealed"), (
        "while on the roster the member must be served the folder's key bundle — "
        f"the baseline the refusal below is measured against; got {served!r}"
    )

    # --- the action: remove one member, and kill the owner while the nest
    # holds the publish parked. The click rides a thread for journey 12's
    # reason: a bridge command is acked once the app has applied it, and the
    # removal is awaited end to end, so against a parked publish the click does
    # not return — the watcher has to be the one waiting, not the clicker.
    target = roster_index(ob, leaves["handle"], row=row)
    click_error: list[BaseException] = []

    def _click() -> None:
        try:
            ob.remove_shared_member(target, row=row)
        except BaseException as exc:  # the app dies under it by design
            click_error.append(exc)

    arm_rpc_hold(port, _PUBLISH_KIND)
    clicker = threading.Thread(target=_click, name="folder-member-remove-click", daemon=True)
    try:
        clicker.start()
        wait_for_held_rpc(
            port,
            _PUBLISH_KIND,
            diagnose=lambda: (
                f"app-side: roster count={ob.shared_member_count()}, "
                f"error={app.error_text()!r}, click raised {click_error!r}. No "
                f"publish arrived: the removal failed before its rotation (the MLS "
                f"Remove was refused or deferred — read `error-message`), or the "
                f"click never reached the member's row."
            ),
        )
        driver.kill_uncleanly()
        # Refused, not released: the parked publish is answered with an error
        # and its handler never runs. Nobody is left to read the answer.
        refuse_rpc(port, _PUBLISH_KIND)
    finally:
        release_rpc_hold(port, _PUBLISH_KIND)
        clicker.join(timeout=30.0)

    # --- ground truth: the app died BETWEEN the stage and the publish. The
    # eviction follows the publish in the same chain, so a roster that still
    # lists the member — and a nest that still serves them the key bundle —
    # is the removal unfinished at the nest.
    rostered = nest_roster()
    assert leaves["actor_id_hex"] in rostered, (
        f"the removed member is already off the nest's roster {rostered!r} with the "
        f"owner's app dead and its publish refused — the removal finished before "
        f"the kill, so nothing is left for the launch to resume and the rest of "
        f"this journey would assert nothing."
    )
    still = content_key_get(nest, leaves, name)
    assert isinstance(still, dict) and still.get("sealed"), (
        "between the stage and the publish the nest still serves the removed "
        f"member the key bundle — the leak the resume exists to close; got {still!r}"
    )

    # --- recover: relaunch, and let the launch pass finish it. NO gesture.
    driver.hard_reload()

    wait_until(
        lambda: leaves["actor_id_hex"] not in nest_roster(),
        REMOVAL_RESUMED_S,
        interval=1.0,
        diagnose=lambda: (
            f"a relaunched owner never finished the staged removal: the nest's "
            f"roster still reads {nest_roster()!r} (removed member "
            f"{leaves['actor_id_hex'][:16]}…). The launch folder pass either did "
            f"not run, ran before the account runtime was readable and gave up, "
            f"or could not complete a removal whose Remove commit was already "
            f"distributed. error={app.error_text()!r}; folder-pass lines in this "
            f"launch's log: {_folder_pass_lines(driver)!r}"
        ),
    )

    rostered = nest_roster()
    assert stays["actor_id_hex"] in rostered, (
        f"the resumed removal took the wrong person, or the whole group: the "
        f"member who stays is off the nest's roster {rostered!r}."
    )
    refused = content_key_get(nest, leaves, name)
    assert not (isinstance(refused, dict) and refused.get("sealed")), (
        "the nest must refuse a removed member the folder's key bundle once the "
        f"removal is finished; got {refused!r}"
    )

    # --- the staging is gone: the pass logs its count only after
    # `resume_pending_removals` returned, which is after the sentinel settled.
    wait_until(
        lambda: _removals_resumed_at_launch(driver) >= 1,
        REMOVAL_RESUMED_S,
        interval=1.0,
        diagnose=lambda: (
            "the nest shows the eviction, but this launch's folder pass never "
            "reported a resumed removal — the eviction landed and the custody "
            "commit that settles the sentinel did not, so every later launch "
            f"re-drives it. Folder-pass lines: {_folder_pass_lines(driver)!r}"
        ),
    )

    # --- and the owner's own app agrees: one member, the one who stays.
    ob.navigate_folders()
    row = ob.find_and_expand_folder(name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    wait_until(
        lambda: ob.shared_member_count() == 1,
        SHARE_ROSTER_S,
        diagnose=lambda: (
            f"the relaunched owner's roster reads {ob.shared_member_count()} "
            f"member(s), not the one who stays; error={app.error_text()!r}"
        ),
    )
    assert stays["handle"] in ob.member_handle(0, row=row), (
        f"the owner's roster shows {ob.member_handle(0, row=row)!r}, not the member "
        f"who stays ({stays['handle']!r})."
    )
