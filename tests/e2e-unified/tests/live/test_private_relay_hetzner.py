"""Live, opt-in end-to-end test: the PRODUCTION home-with-public-relay deployment
(``docs/goal/architecture/nest/deployment-home-with-public-relay.md``) across
REAL infrastructure:

  * **public relay** — a real Hetzner VPS provisioned through the shared client
    onboarding/provisioning path (the same harness as
    ``test_hetzner_provision.py``), claimed through the client UI on the
    public NAT axis;
  * **private nest** — a Docker container ON THIS MACHINE running the production
    ``ghcr.io/faunasocial/nest:latest`` image (pulled, NEVER built locally),
    ``FAUNA_MODE=private`` — genuinely behind NAT: it dials OUT to the relay
    (at the URL the home-side pairing row records, ``https://<fqdn>``) and polls
    ``fauna.federation.sync.mail_pull``/``.mail_ack``; nothing dials in;
  * **one linux app** onboards BOTH boxes through real UI actions — the
    claim-code page + the terminal nat_mode_choice on each box. The HOME box
    commits PRIVATE there, which derives the four subsystem enables OFF
    (``onboarding.md`` § 3b — handle locality AND the NAT axis, ratified
    2026-07-13), so no fresh, divergent MSEK is minted at its claim. Mail on
    the PUBLIC box is then enabled explicitly through the mail-settings UI
    (``enable_mail_plain`` — mints the ``default`` credential with a password
    the test chooses; see the Phase-2 residual-race note); the HOME box gets
    only the admin mail toggle (MDA up, MTA stays down). One linked-nests
    **LinkBoth** (``nests-add-input``) then seeds both pairing rows AND
    auto-provisions the home mailbox (``MailRelayProvisionHook`` →
    ``ProvisionRelayMailbox``, re-sealing the fleet MSEK under that same
    default credential — no second enable-mail step, deployment doc
    § Pairing);
  * **send** — real internet mail: SMTP submission through **example.com**
    (port 465, AUTH PLAIN). The sender credential is MINTED BY THIS TEST, the
    way the live mail tests set up theirs: the client boots authenticated as
    the live box's admin (``FAUNA_LIVE_SECRET_HEX`` — its BackupKey
    derives from the identity seed, ``fauna-core/src/crypto.rs``), adds one
    PLAIN credential through the mail-settings UI (``add_credential_plain``),
    and AUTHs with the sub-addressed MUA username
    ``<handle>+<credential_id>@<domain>`` (RFC 5233 — parsed by the MTA's AUTH,
    ``internal/mta/auth.go``; MAIL FROM stays the canonical handle, which the
    actor owns). No pre-existing mail password is needed, and the credential is
    revoked again at teardown (best-effort; ids are nonce-unique). example.com
    DKIM-signs and MX-delivers to the Hetzner box's port 25;
  * **read** — (1) a python raw-wire IMAP client against the PRIVATE nest's
    mapped 993 (the canonical store; the MDA opens the relayed seal at AUTH),
    (2) the linux app UI conversations view with the session on the private
    nest, and (3) the core no-readable-copy property: the PUBLIC box's INBOX
    drains to 0 after the home box acks (tombstone + placement purge).

This is the real-infrastructure superset of the one-machine tier_4
``tests/platform/docker/test_mail_relay_two_nest.py`` (Slice 6): same relay
mechanics, but the public end is a real VPS with real DNS/ACME/perimeter, the
inbound leg is a real cross-internet delivery from a production nest, and the
provisioning/pairing surface is the real client UI instead of seal-helper RPCs.

Gating (all pre-existing conventions):
  * ``HETZNER_API_TOKEN`` + ``FAUNA_E2E_LIVE=1`` — the live-provision gate
    (``tests/live/conftest.py``; real € cost).
  * ``FAUNA_LIVE_NEST_URL`` (e.g. https://example.com) + the live box's
    mail-enabled admin identity seed, resolved per box
    (``live_box_door.admin_seed``: ``FAUNA_LIVE_SECRET_HEX`` > the box's
    staging-box file > ``~/.fauna-id``) — ambient, safe because the sender leg
    is non-destructive (one additional credential, revoked at teardown); the
    opt-in is the live-provision gate above. The SENDER credential is minted in-test (no
    ``FAUNA_LIVE_MAIL_PASSWORD`` needed) and, since 2026-08-16, so is the
    sender ADDRESS: it is derived from the secret once the client boots
    authenticated (``helpers/live_handle.py``). ``FAUNA_LIVE_MAIL_ADDRESS``
    still overrides. Skipped without the URL or a resolvable seed.
  * Docker (to run the private nest container).

Runtime **~5-10 min in practice** (observed: 435s 2026-07-09, ~256s 2026-07-19,
444s 2026-07-22). This used to be ~60-75 min, dominated by the ~30 min Hetzner
Cloud DNS propagation the FQDN TLS gate absorbs (``testing.md`` § Gap 3) — that
propagation has been ~1-2 min on every run since 2026-07-09 and looks fixed
provider-side. The 5400s/90-min per-test timeout ``tests/live/conftest.py``
applies is deliberately kept as the safety bound for a propagation regression;
it is NOT a runtime estimate, so don't tighten it to match the numbers above.
Teardown of the VPS + DNS is the ``box`` fixture's (always runs); the private
container is removed in this test's ``finally``.
"""
from __future__ import annotations

import json
import os
import smtplib
import ssl
import subprocess
import time
import uuid
from email.message import EmailMessage

import pytest

from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import live_box_door
from helpers.budgets import SERVICE_BOOT_S
from helpers.live_handle import derive_handle
from helpers.live_provision import (
    assert_mail_port,
    await_provisioning,
    drive_provisioning,
    finish_provisioned_wizard,
    retry_get,
    skip_unless_fresh_box_reach_app,
)
from helpers.waiting import wait_until
from tests.platform.docker.helpers import (
    await_mda_only_serving,
    bridge_diag,
    find_free_ports,
    imap_fetch_only_inbox_message,
    imap_inbox_count,
    is_commanded_up,
    remove_container,
    start_container_with_ports,
    svstat,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

# The example.com sending identity. The sender CREDENTIAL is minted in-test via
# the mail-settings UI, so no password env — and since 2026-08-16 the sender's
# HANDLE is derived from the secret too (`helpers/live_handle.py`), because this
# test boots authenticated as the live box's already-registered admin, which is
# exactly the case the derivation covers. `FAUNA_LIVE_MAIL_ADDRESS` survives as
# an override, so a run that sets it behaves identically to before.
# The admin seed is resolved per box (`live_box_door.admin_seed`), so a staging
# box this fleet provisioned needs nothing exported beside its URL.
LIVE_URL = os.environ.get("FAUNA_LIVE_NEST_URL", "").rstrip("/")
LIVE_SECRET, LIVE_SECRET_SOURCE = live_box_door.admin_seed(LIVE_URL)

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_provisioning,
    pytest.mark.live_box,  # serializes the shared example.com sender via the flock
    # No client platform marker — the app axis is decided in-body by
    # `skip_unless_fresh_box_reach_app`, so an app that cannot drive this yet is
    # TALLIED as unbuilt debt (convention 7) rather than silently deselected.
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.skipif(
        not (LIVE_URL and LIVE_SECRET),
        reason="private-relay live test sends via the live box: set "
        "FAUNA_LIVE_NEST_URL and provide its admin seed "
        f"({live_box_door.SEED_SOURCES}) (opt-in)",
    ),
]

# The production multi-arch image the private box runs — PULLED, never built
# (dev VMs are barred from building the nest image; the same :latest the
# wizard-provisioned Hetzner box pulls, so both ends run the identical build).
GHCR_IMAGE = "ghcr.io/faunasocial/nest:latest"

PRIVATE_CLAIM_CODE = "RLYLV1"

# The password the mail-settings UI mints the e2e box's `default` credential
# with (public box, phase 2). LinkBoth's ProvisionRelayMailbox re-seals the
# read recipe under that same first credential, so this ONE password AUTHs
# IMAP on BOTH boxes.
MUA_PASSWORD = "private-relay-live-mua-pw-1"  # gitleaks:allow

# VPS create + cloud-init + ACME + boot (same headroom as the provisioning test).
PROVISION_TIMEOUT_S = 1200


# ── Fixtures ─────────────────────────────────────────────────────────────────
@pytest.fixture(scope="module")
def ghcr_latest_image():
    """``docker pull`` the production ``:latest`` — a PULL, never a build (the
    heavy image build happens in CI; a pull of the CI-built multi-arch manifest
    resolves to the arm64 variant natively on this arm64 dev host)."""
    proc = subprocess.run(
        ["docker", "pull", GHCR_IMAGE], capture_output=True, text=True, timeout=900
    )
    if proc.returncode != 0:
        pytest.skip(f"cannot pull {GHCR_IMAGE}: {proc.stderr.strip()[-300:]}")
    return GHCR_IMAGE


# ── Client-drive helpers ─────────────────────────────────────────────────────
def _relaunch(app, extra: dict | None = None) -> None:
    """Fresh relaunch of the SAME linux app binary (new per-launch XDG +
    keyring/credential dirs, same launch config). Every nest/session switch in
    this test is a full relaunch: a ``set_state`` session patch does NOT
    re-authenticate an already-running client (the linux re-activation guard),
    whereas a relaunch runs the real launch-routing (wizard when unseeded, the
    silent-challenge authenticated boot when ``seed_credentials`` is passed)."""
    config = dict(app.driver._launch_config)
    config.pop("seed_credentials", None)
    if extra:
        config.update(extra)
    app.driver.teardown()
    app.driver.launch(config)


def _await_authenticated(app, timeout: float = 90.0) -> None:
    """Wait for a SEEDED relaunch's silent challenge to complete
    (``session.authenticated`` in the state snapshot). A nav-only ``set_state``
    fired BEFORE auth completes patches the pre-auth window and is silently
    dropped when the authenticated shell builds — the phase-0 failure mode of
    the first live run (the challenge takes seconds over a real WAN link)."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            sess = (app.driver.get_state() or {}).get("session") or {}
            if sess.get("authenticated"):
                return
            last = sess
        except Exception as e:  # agent still booting
            last = repr(e)
        time.sleep(1.0)
    raise AssertionError(
        f"seeded client never reached an authenticated session within {timeout}s "
        f"(last session state: {last}; error: {app.error_text()!r})"
    )


def _ensure_admin_mail_enabled(app, timeout: float = 45.0) -> None:
    """Drive the ``admin-mail-enabled-toggle`` to an EXPLICIT enabled=true.

    Fresh-box quirk: with the mail-enable row UNSET, ``get_mail_config``'s
    ``mail_enabled`` falls back to **true** (the "approved ⇒ enabled" legacy
    default pending the Stage-5 default-off flip —
    ``bridge_routing_handlers.rs::assemble_fetch_config_reply``), so the
    SwitchRow renders ON while ``/data/imap-enabled`` is absent and the bridge
    is down. In that state the first click persists an explicit **false**;
    a second click then persists **true**, which writes the flag and boots the
    bridge — the gesture a real admin performs on this surface today. When the
    toggle renders OFF (an explicit false already persisted), one click
    suffices."""
    app.driver.wait_for("admin-mail-enabled-toggle", timeout=20.0)
    time.sleep(3.0)  # MailPolicyMachine hydrate (get_mail_config round-trip)

    def _state() -> str:
        return (app.driver.get_attr("admin-mail-enabled-toggle", "state") or "").strip().lower()

    if _state() == "true":
        app.admin.toggle_mail_enabled()  # fallback-ON → persist explicit false
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and _state() != "false":
            time.sleep(1.0)
        assert _state() == "false", (
            f"toggle never re-rendered OFF after the first click; "
            f"error: {app.error_text()!r}"
        )
    app.admin.toggle_mail_enabled()      # OFF → persist explicit true (flag + boot)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and _state() != "true":
        time.sleep(1.0)
    assert _state() == "true", (
        f"toggle never re-rendered ON after the enable click; "
        f"error: {app.error_text()!r}"
    )


def _settled_mail_enabled(admin_secret_hex: str, nest_url: str, *, settle_s: float = 10.0) -> bool:
    """Poll `get_mail_config` for `settle_s` and return the SETTLED
    `mail_enabled` — the post-claim launch glue's fire-and-forget
    `enable_mail_with_generated_password` call (fired once LoggedIn is reached
    on a real-registerable-domain handle, `onboarding.md` § 3b) needs a moment
    to land. Still a settle window: the calendar-onboarding witnesses moved to
    the post-claim step's completion anchor
    (`helpers.waiting.await_serving_enablement_for`), and this live test
    has not yet."""
    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    actor_id, signing_key = bytes(sk.verify_key), bytes(sk)
    deadline = time.monotonic() + settle_s
    last = False
    while time.monotonic() < deadline:
        admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key)
        with admin_ws:
            cfg = admin_ws.call("fauna.bridges.get_mail_config", {})
        last = bool(cfg.get("mail_enabled"))
        time.sleep(1.0)
    return last


def _assert_enrollment_strict(admin_secret_hex: str, nest_url: str,
                              *, timeout: float = 90.0) -> None:
    """Bridge-isolation hardening, live half (security.md § Enrollment
    proof-of-possession contract): the installer-provisioned box must NOT be
    silently lenient — the artifact mints + blesses the bridge keys, so nest
    must self-report STRICT enrollment for both mail roles. This admin-WS read
    is the sanctioned no-SSH observable (`testing.md` § Gap 3 — a provisioned
    box carries no ssh key, so the registry can never be checked with a shell);
    the in-container mechanics behind the flag are proven by tier_4
    `test_bridge_enrollment_pop.py` on the same image. Polls briefly: the
    bridges (and the first enrollment the diagnostic rides beside) come up
    seconds after the mail enable."""
    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    actor_id, signing_key = bytes(sk.verify_key), bytes(sk)
    deadline = time.monotonic() + timeout
    strict = None
    while time.monotonic() < deadline:
        admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key)
        with admin_ws:
            reply = admin_ws.call("fauna.bridges.list_service_users", {})
        strict = reply.get("enrollment_strict")
        if strict is not None and strict.get("mta") and strict.get("mda"):
            print("[relay-live] enrollment_strict: mta+mda STRICT (blessed registry provisioned)")
            return
        time.sleep(5.0)
    if strict is None:
        raise AssertionError(
            "admin list_service_users reply carries no enrollment_strict — the "
            "deployed nest image predates the diagnostic (needs a production "
            "build ≥ 2026-07-22); cannot verify the box is not silently lenient"
        )
    raise AssertionError(
        f"PROVISIONING GAP: the installer-provisioned box self-reports lenient "
        f"enrollment ({strict}) — the blessed registry is missing/unreadable, so "
        f"any co-resident loopback peer could self-enroll a rogue bridge "
        f"(security.md § Enrollment proof-of-possession contract)"
    )


def _assert_bridge_confinement(admin_secret_hex: str, nest_url: str,
                               *, budget_s: float = SERVICE_BOOT_S) -> None:
    """The process-isolation twin of `_assert_enrollment_strict`, on the same
    no-SSH channel (`security.md` § Co-resident process trust boundary →
    *Confinement self-probe*).

    `enrollment_strict` proves the artifact wired the BLESSED REGISTRY. This
    proves the artifact wired the SANDBOX: on a freshly installer-provisioned
    box, each mail bridge must report a distinct non-root UID, an unreachable
    sealed store measured from inside its own Landlock domain, and an enforcing
    ruleset. Before this, those were the facts the 2026-07-22 chain could only
    get by SSHing into example.com with a human present — a path `testing.md`
    § Gap 3 rules out for a provisioned box, which carries no ssh key at all.

    ⚠ A self-report, so a *compromised* bridge could lie; this catches the
    honest misconfiguration (cloud-init or a compose override defeating the
    image hardening). The strong, external proof of the same properties is
    tier_4 `test_uid_isolation.py` on the same image.

    Deadline poll (convention 14): a green run returns as soon as both bridges
    have reported and pays nothing; only a genuine failure spends the budget.
    The bridges enroll seconds after the mail enable, and the report rides their
    first `register_service_user`.
    """
    sk = SigningKey(bytes.fromhex(admin_secret_hex))
    actor_id, signing_key = bytes(sk.verify_key), bytes(sk)

    def _reported() -> dict | None:
        admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=signing_key)
        with admin_ws:
            reply = admin_ws.call("fauna.bridges.list_service_users", {})
        by_role = {
            u["role"]: u for u in reply.get("service_users", [])
            if u.get("confinement") is not None
        }
        return by_role if {"mta", "mda"} <= set(by_role) else None

    by_role = wait_until(
        _reported, budget_s, interval=5.0,
        diagnose=lambda: (
            "no bridge on the provisioned box reported a confinement self-probe — "
            "either the deployed nest image predates the probe, or the bridges "
            "never reached register_service_user"
        ),
    )

    seen_uids = {}
    for role in ("mta", "mda"):
        c = by_role[role]["confinement"]
        assert c["sealed_store"] == "denied", (
            f"PROVISIONING GAP: the {role} bridge reports it can reach the sealed "
            f"store (sealed_store={c['sealed_store']!r}) — a MIME-parser RCE on "
            f"this box would reach nest.db (security.md § UID isolation, slice 4)"
        )
        assert c["landlock"] in ("fully", "partial"), (
            f"PROVISIONING GAP: the {role} bridge reports landlock={c['landlock']!r}. "
            f"`off` = the kernel lacks Landlock (or seccomp blocked its syscalls) "
            f"and the wrapper warned-and-continued UNSANDBOXED; `unknown` = the "
            f"bridge was never started through fauna-sandbox, i.e. the deployment "
            f"artifact was overridden"
        )
        assert c["uid"] != 0, f"the {role} bridge is running as ROOT on the provisioned box"
        seen_uids[role] = c["uid"]
    assert seen_uids["mta"] != seen_uids["mda"], (
        f"PROVISIONING GAP: both mail bridges share uid {seen_uids['mta']} — the "
        f"per-role UID split (slice 1) is not in effect, so one parser's RCE "
        f"reaches the other's key material"
    )
    print(f"[relay-live] bridge confinement: mta uid={seen_uids['mta']} "
          f"mda uid={seen_uids['mda']}, sealed store denied, landlock enforcing")


def _open_settings_page(app, *, nest_url: str, secret_hex: str, sub_id: str,
                        anchor_check, timeout: float = 90.0) -> None:
    """Land the authenticated shell on an embedded settings sub-page, robust to
    the freshly-booted shell racing a nav patch: re-assert the session AND the
    nav stack together (the injection path builds the window on the requested
    view — the ``claim_enable_and_ready`` mechanism; a nav-only patch fired
    while the shell is still building is silently dropped, the phase-0 failure
    mode of live run #2) and RETRY until the page's anchor element renders."""
    deadline = time.monotonic() + timeout
    while True:
        _set_state_resilient(app, {
            "session": {
                "authenticated": True,
                "node_url": nest_url,
                "secret_hex": secret_hex,
            },
            "nav": {"stack": [{"view": "settings"},
                              {"view": "settings", "id": sub_id}]},
        })
        time.sleep(2.0)
        if anchor_check():
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"settings sub-page {sub_id!r} never rendered on {nest_url} "
                f"within {timeout}s. error: {app.error_text()!r}"
            )


def _ui_claim(app, *, nest_url: str, claim_code: str, handle: str,
              secret_hex: str, nat_mode: str | None = None) -> None:
    """Claim a nest through the REAL client UI: import the identity via the
    paste-key onboarding flow, jump the wizard to the claim page for the known
    nest (the sanctioned known-nest navigation every tier_3 UI claim uses —
    ``helpers/mail_client_ui.py::claim_to_nat_mode_page``; everything
    downstream is the real client flow), type + submit the claim code, then
    confirm the terminal nat_mode_choice step. ``nat_mode`` selects a radio
    before confirming; omit it to accept the pre-selected seed (the
    confirm-only common case). The handle carries the domain, so the claim
    auto-registers it as the box's identity domain.

    No-modes retirement (ratified 2026-07-12): the storage-mode radio/confirm
    dance this helper used to drive is gone — every nest is sealed at rest
    unconditionally now, and claim lands directly on nat_mode_choice.

    Serving-enablement derivation (NAT conjunct ratified 2026-07-13,
    ``onboarding.md`` § 3b): the four claim-time subsystem intents derive from
    handle locality AND the NAT axis — a claim committed PRIVATE derives all
    four OFF even for a real-domain handle, so the launch glue mints nothing
    on that box. That is how this test's HOME box (Phase 5) expresses "this
    box runs no mail" through the client, which the retired
    ``onboarding-enable-*-checkbox`` unticks used to do: the home box's
    mailbox comes exclusively from the ``LinkBoth`` →
    ``ProvisionRelayMailbox`` fleet-MSEK re-seal, never an independent
    claim-time enable (the MSEK-divergence trap).
    """
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)
    app.driver.wait_for("handle-input", timeout=20)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest", json.dumps([nest_url, handle])
    )
    app.driver.wait_for("claim-code-input", timeout=30)
    app.driver.clear_and_type("claim-code-input", claim_code)
    app.driver.click("claim-code-submit-button")
    # The admin claim lands the wizard on nat_mode_choice. A live box may
    # take a few seconds longer than a local one — generous wait.
    app.driver.wait_for("nat-mode-confirm-button", timeout=60)
    app.onboarding.finish_nat_mode(mode=nat_mode)


def _set_state_resilient(app, state: dict, *, attempts: int = 6) -> None:
    """``driver.set_state`` with retries: the driver waits a HARD 10 s for the
    app's command ack, and a freshly-launched GTK client on a loaded shared dev
    box (siblings building — a shared dev host can routinely sit at loadavg 20+) can miss that
    window on the first patch while being perfectly healthy. The patch is
    idempotent (same session, same nav), so re-sending is safe; each retry also
    gives the UI thread another ~10 s to drain. Raises the last TimeoutError if
    every attempt misses."""
    last: Exception | None = None
    for i in range(attempts):
        try:
            app.driver.set_state(state)
            return
        except TimeoutError as e:  # app alive but the UI thread is starved
            last = e
            if not app.driver.is_app_alive():
                raise
            time.sleep(2.0 * (i + 1))
    raise AssertionError(
        f"the client never acknowledged a session/nav patch across {attempts} "
        f"attempts (loaded box?); last: {last!r}"
    )


def _reassert_session(app, *, nest_url: str, secret_hex: str, view: str) -> None:
    """Rebuild the authenticated window on ``view`` by re-asserting the SAME
    session the UI claim just established (the established admin-e2e mechanism
    from ``helpers/mail_client_ui.py`` — never an identity/nest switch)."""
    _set_state_resilient(app, {
        "session": {
            "authenticated": True,
            "node_url": nest_url,
            "secret_hex": secret_hex,
        },
        "nav": {"stack": [{"view": view}]},
    })
    time.sleep(2.0)


def _relax_inbound_perimeter(box, secret_hex: str) -> None:
    """Open the e2e box's spam gates that key on sender-host standing
    (greylist/DNSBL/FCrDNS/conn-rate) so the single example.com delivery is
    accepted on the FIRST attempt instead of depending on the sender's retry
    queue beating the test timeout. Setup, not the assertion (the same posture
    as ``test_mail_port25_inbound_live.py``); example.com is a real production
    sender, so SPF/DKIM/DMARC still pass on their own merits."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = SigningKey(bytes.fromhex(secret_hex))
    with WsRpcAdminClient(
        f"https://{box.ipv4}", actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    ) as adm:
        adm.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "helo_identity_required": False,
                "max_conn_per_min": 1000,
            },
        )


def _mint_sender_credential(app) -> tuple[str, str, str]:
    """Mint THIS RUN's example.com sender credential through the mail-settings UI —
    in-test credential setup, the way the live mail tests do theirs, but
    NON-destructively (an additional credential on the already-enabled mailbox,
    not a factory-reset + re-enable). The client boots authenticated as the live
    box's admin (its BackupKey derives deterministically from the
    identity seed — ``fauna_core::crypto::BackupKey::derive``), adds one PLAIN
    credential with a nonce-unique name, and reads the exact sub-addressed MUA
    username off the new credential row. Returns
    ``(mua_username, password, canonical_address)``."""
    live_url, live_secret = LIVE_URL, LIVE_SECRET
    cred_nonce = uuid.uuid4().hex[:6]
    display_name = f"Relay E2E {cred_nonce}"          # → credential_id relay-e2e-<nonce>
    password = uuid.uuid4().hex
    _relaunch(app, extra={"seed_credentials": {
        "secret_key": live_secret,
        "node_url": live_url,
        "device_id": "relay-live-sender",
    }})
    _await_authenticated(app)
    # The sender address is the admin's own mailbox = handle@domain, and the
    # handle is the box's answer, not ours: derive it from the identity we just
    # booted as rather than demanding FAUNA_LIVE_MAIL_ADDRESS (which still
    # overrides). Must come after the relaunch — `session.handle` lands on the
    # account fetch that follows auth.
    sender_addr, _sender_local = derive_handle(app, live_url)
    _open_settings_page(app, nest_url=live_url, secret_hex=live_secret,
                        sub_id="mail-settings", anchor_check=app.mail_settings.is_page_visible)
    before = app.mail_settings.credential_count()
    app.mail_settings.add_credential_plain(display_name, password)
    assert app.mail_settings.wait_for_credential_count_at_least(before + 1, timeout=30.0), (
        "adding this run's sender credential on the live box should append a row; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    username = None
    for i in range(app.mail_settings.credential_count()):
        u = app.mail_settings.credential_username(i)
        if f"+relay-e2e-{cred_nonce}@" in u:
            username = u
            break
    assert username, (
        f"the new credential row must render the sub-addressed MUA username "
        f"(…+relay-e2e-{cred_nonce}@…); rows: "
        f"{[app.mail_settings.credential_username(i) for i in range(app.mail_settings.credential_count())]}"
    )
    return username, password, sender_addr


def _revoke_sender_credential(app, mua_username: str) -> None:
    """Best-effort teardown of the run's minted sender credential (relaunches
    onto the live box and revokes the row matching the sub-addressed username).
    Failures are swallowed — ids are nonce-unique, so residue is inert."""
    live_url, live_secret = LIVE_URL, LIVE_SECRET
    _relaunch(app, extra={"seed_credentials": {
        "secret_key": live_secret,
        "node_url": live_url,
        "device_id": "relay-live-sender-cleanup",
    }})
    _await_authenticated(app)
    _open_settings_page(app, nest_url=live_url, secret_hex=live_secret,
                        sub_id="mail-settings", anchor_check=app.mail_settings.is_page_visible)
    for i in range(app.mail_settings.credential_count()):
        if app.mail_settings.credential_username(i) == mua_username:
            app.mail_settings.revoke_credential(i)
            return


def _submit_via_fauna_fan(auth_username: str, password: str, mail_from: str,
                          recipient: str, subject: str, body: str) -> None:
    """One outbound submission through the live box's port-465 submission
    listener (implicit TLS, verified cert): AUTH PLAIN as the sub-addressed
    ``auth_username`` (routes the minted credential — RFC 5233), MAIL FROM the
    canonical handle (owned by the same actor, ``submission.go`` local-part
    ownership). The box then DKIM-signs and MX-delivers to the recipient
    domain's MX (the Hetzner relay)."""
    mail_host = f"mail.{mail_from.split('@', 1)[-1]}"
    msg = EmailMessage()
    msg["From"] = mail_from
    msg["To"] = recipient
    msg["Subject"] = subject
    # A real MUA sets Message-ID; python's EmailMessage does NOT. Deployed
    # bridges (≤ 2026-07-09) pass an EMPTY original_msgid for a Message-ID-less
    # submission and nest rejects it as a misleading transient 451 ("Outbound
    # enqueue temporarily unavailable") — root-caused live 2026-07-09; the
    # bridge-side RFC 6409 §8.3 auto-stamp fix rides this same commit but
    # example.com runs an older build, so the test sets one explicitly.
    msg["Message-ID"] = f"<{uuid.uuid4().hex}@{mail_from.split('@', 1)[-1]}>"
    msg.set_content(body)
    with smtplib.SMTP_SSL(
        mail_host, 465, context=ssl.create_default_context(), timeout=60
    ) as smtp:
        smtp.login(auth_username, password)
        smtp.send_message(msg, from_addr=mail_from, to_addrs=[recipient])


# ── The test ─────────────────────────────────────────────────────────────────
@pytest.mark.feature("home-nest-behind-a-relay")
def test_private_relay_behind_hetzner_public_node(app, box, ghcr_latest_image, request):
    skip_unless_fresh_box_reach_app(app.driver, surface="the private-relay live e2e drive")
    token = os.environ["HETZNER_API_TOKEN"].strip()
    secret_hex = box.identity_secret_hex
    handle = box.handle  # admin@e2e-<runid>.<zone> — ONE identity claims both boxes

    # ── Phase 0: mint this run's example.com SENDER credential (in-test setup) ─
    # Preflight first: a live box that does not know the resolved seed (the
    # wrong box's seed, or a reset box) skips as environment before a paid VPS
    # is provisioned, naming where the seed came from.
    live_box_door.preflight_admin(LIVE_URL, LIVE_SECRET, LIVE_SECRET_SOURCE)
    sender_username, sender_password, sender_addr = _mint_sender_credential(app)
    print(f"\n[relay-live] sender credential minted on the live box: {sender_username}")

    # Revoke it at teardown NO MATTER where the test stops (run #3 leaked one by
    # only guarding the container phases). Finalizers run before the app/box
    # fixtures tear down, so the driver is still alive here; never mask the
    # test outcome.
    def _cleanup_sender_credential():
        try:
            _revoke_sender_credential(app, sender_username)
            print(f"[relay-live] revoked this run's sender credential {sender_username}")
        except Exception as e:  # noqa: BLE001
            print(f"[relay-live] teardown WARNING: sender-credential revoke failed: {e!r}")
    request.addfinalizer(_cleanup_sender_credential)

    # ── Phase 1: provision the PUBLIC relay on Hetzner, the normal way ──────
    # Fresh launch — the provisioning drive runs on a clean onboarding machine.
    _relaunch(app)
    print(f"[relay-live] provisioning public relay {box.subdomain} (mail box) ...")
    drive_provisioning(app, box, token)
    snap = await_provisioning(app, PROVISION_TIMEOUT_S)
    assert snap.get("overall") == "Succeeded", (
        f"provisioning did not succeed: overall={snap.get('overall')!r} "
        f"final_error={snap.get('final_error')!r} steps={snap.get('steps')!r}"
    )
    result = snap.get("result") or {}
    box.server_id = result.get("server_id")
    box.ipv4 = result.get("ipv4")
    box.fqdn = result.get("domain")
    claim_code = result.get("claim_code")
    assert box.server_id and box.ipv4 and box.fqdn and claim_code, (
        f"incomplete provisioning result: {result}"
    )
    public_ip_url = f"https://{box.ipv4}"
    public_fqdn_url = f"https://{box.fqdn}"
    print(f"[relay-live] provisioned: server={box.server_id} ip={box.ipv4} fqdn={box.fqdn}")

    # Health BY IP (the box boots domainless on its self-signed floor; DNS lags).
    health = retry_get(f"{public_ip_url}/api/v1/health", verify=False, timeout=15,
                       attempts=20, delay=6)
    assert health.json().get("status") == "ok", f"health not ok: {health.json()}"

    # ── Phase 2: finish the wizard on the public box + UI-enable mail ───────
    # The public box is ALREADY CLAIMED — provisioning's own Online step claimed
    # it by IP as its last substep (onboarding.md § 6 *Provisioning = build +
    # claim*), so `Succeeded` above means built and claimed. What is left is
    # the wizard's tail: Continue → nat_mode_choice dismissed → LoggedIn, in
    # the SAME launch the run happened in. (Until 2026-08-29 this phase
    # relaunched and typed the code into the claim-code page — the run itself
    # never claimed then; today that second claim is refused `already_claimed`,
    # see `finish_provisioned_wizard`.) The private box below is still claimed
    # through the claim-code page: it is claimed outside any provisioning run.
    #
    # This PUBLIC box is public-axis with a real-domain handle, so the derived
    # default (`onboarding.md` § 3b) legitimately auto-enables mail — with an
    # auto-generated, uncaptured password minted via
    # `enable_mail_with_generated_password` (`apps/fauna-linux/src/client.rs:4042`,
    # "Default" → credential_id "default") — the moment LoggedIn is reached,
    # racing this phase's explicit enable. `enable_mail_plain` assumes mail is
    # NOT yet enabled (it clicks the enable toggle); if the auto-mint already
    # landed, `MailSettingsMachine::enable_mail` correctly refuses ("mail
    # already enabled — use AddCredential instead",
    # `libs/fauna-client-mail-settings/src/machine.rs:1117`) rather than
    # silently toggling mail off — confirmed live 2026-07-19 (this exact
    # failure, at the old unconditional `enable_mail_plain` call). FIXED:
    # branch on the SETTLED derived state (`_settled_mail_enabled`, the
    # settle-window twin of `helpers.caldav_onboarding._derived_enablement`). Do NOT reach
    # for `add_credential_plain` here — Phase 6's
    # `MailSettingsMachine::provision_relay_mailbox` reuses
    # `cfg.mail.credentials.first()` for the home-box reseal (`machine.rs:1439`),
    # which would still be the auto-generated "default" row even after adding a
    # second credential; the correct move is `reveal_credential_secret` on that
    # FIRST row — a pure client-side read (the secret lives in the
    # `fauna.state.mail` plane, sealed client-side; the nest never sees it —
    # `machine.rs:1852`)
    # mirroring the real "(re)configure a MUA" user gesture.
    finish_provisioned_wizard(app)
    _reassert_session(app, nest_url=public_ip_url, secret_hex=secret_hex,
                      view="conversations")
    mail_already_derived_on = _settled_mail_enabled(secret_hex, public_ip_url)
    # Enable mail through the mail-settings UI: flips the deployment subsystem
    # (/data/imap-enabled → MTA/MDA boot + self-enroll + auto-approve on a real
    # provisioned box) AND mints the `default` PLAIN credential + the fleet
    # MSEK this whole test keys on — unless the derived default already beat
    # us to it, in which case we just reveal that credential's real password.
    _open_settings_page(app, nest_url=public_ip_url, secret_hex=secret_hex,
                        sub_id="mail-settings", anchor_check=app.mail_settings.is_page_visible)
    if mail_already_derived_on:
        assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=30.0), (
            "mail derived ON at claim should already render the auto-generated "
            f"'default' credential; error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
        )
        mua_password = app.mail_settings.reveal_credential_secret(index=0)
        assert mua_password, "reveal_credential_secret returned an empty secret"
    else:
        app.mail_settings.enable_mail_plain(MUA_PASSWORD, display_name="Default")
        assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=90.0), (
            "enabling mail through the UI should mint the first credential; "
            f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
        )
        mua_password = MUA_PASSWORD
    # AUTH with the canonical `<handle>@<fqdn>` alias set_mail_enabled writes
    # nest-side under the REGISTERED primary domain
    # (`mail_enable::ensure_admin_recipient_aliases`). The credential row's
    # rendered username derives the display domain from the session node_url —
    # `admin@<ip>` when the box is reached by IP pre-DNS (cosmetic; run #5) —
    # and an IP-domain address does not resolve for AUTH.
    mua_username = handle
    _relax_inbound_perimeter(box, secret_hex)
    print(f"[relay-live] public box claimed (NAT: public) + mail enabled through the "
          f"UI; MUA {mua_username} (row renders "
          f"{app.mail_settings.credential_username(0)!r})")
    # Bridge-isolation hardening (2026-07-22): the freshly installer-provisioned
    # box must self-report STRICT bridge enrollment — the live proof the
    # deployment artifact (cloud-init → entrypoint mint → run-script env) wired
    # the blessed registry, which no local tier can see.
    _assert_enrollment_strict(secret_hex, public_ip_url)
    # Its process-isolation twin: the artifact must also have wired the SANDBOX
    # (per-role UIDs + Landlock), which until the confinement self-probe landed
    # was only checkable over SSH — impossible on a provisioned box.
    _assert_bridge_confinement(secret_hex, public_ip_url)

    # ── Phase 3: FQDN TLS gate — absorbs the ~30 min Hetzner DNS propagation ─
    # (testing.md § Gap 3). After this both example.com and the private container
    # can resolve the box, and ACME has replaced the floor cert.
    retry_get(f"{public_fqdn_url}/api/v1/health", verify=True, timeout=15,
              attempts=240, delay=10)
    assert_mail_port(box.ipv4, 25, tls=False, expect=b"220", hostname=box.fqdn)
    assert_mail_port(box.ipv4, 993, tls=True, expect=b"* OK", hostname=box.fqdn)
    print("[relay-live] TLS + mail-port gates OK — public relay serving")

    # ── Phase 4: the PRIVATE nest on the local dev host — production :latest, behind NAT ────
    # The docker-compose.home.yml shape (bridge-networking variant, as the
    # tier_4 two-nest test): FAUNA_MODE=private (pre-claim NAT seed) +
    # FAUNA_LAN_BIND_IP so the published 993 reaches the MDA. No env names the
    # relay: Phase 6's LinkBoth records the relay's public URL on the home-side
    # pairing row, and that row is the worker's pull target (an OUTBOUND dial
    # through the relay's SNI router → nest, which is why NAT never matters).
    # Loopback-published ports only — nothing on this box is internet-reachable.
    priv_http, priv_993 = find_free_ports(2)
    priv_name = f"fauna-relay-private-live-{priv_http}"
    private_url = f"https://127.0.0.1:{priv_http}"
    start_container_with_ports(
        priv_name,
        {3000: priv_http, 993: priv_993},
        env={
            "FAUNA_MODE": "private",
            "FAUNA_LAN_BIND_IP": "0.0.0.0",
            "FAUNA_CLAIM_CODE": PRIVATE_CLAIM_CODE,
            "FAUNA_PORT": "3000",
        },
        image=ghcr_latest_image,
    )
    try:
        wait_for_health(priv_http, priv_name)
        print(f"[relay-live] private nest up: {priv_name} → {private_url} (IMAP :{priv_993})")

        # ── Phase 5: UI-claim the private (home) box + enable its MDA ───────
        # Same client, fresh wizard run, SAME identity + handle: claim-code UI →
        # nat_mode_choice committed PRIVATE. FAUNA_MODE=private seeds the NAT
        # axis (row absent → seed; nat_mode_choice pre-selects it) and the
        # explicit `nat_mode="private"` radio click makes the arrangement
        # deterministic even if the seed capture ever regresses. Committing
        # PRIVATE is what keeps the launch glue's derived subsystem enables OFF
        # on this box (`onboarding.md` § 3b NAT conjunct, ratified 2026-07-13)
        # — the home box mints NO MSEK at claim; its mailbox arrives only via
        # Phase 6's `LinkBoth` → `ProvisionRelayMailbox` fleet-MSEK re-seal
        # (the divergence trap this test proves against). Then the admin's mail
        # toggle (the real UI for `fauna.bridges.set_mail_enabled` —
        # UI-equivalence proven by
        # tests/platform/docker/test_mail_client_ui_enable_docker.py) brings up
        # the MDA; the MTA must stay down (no-MTA boot on the private axis).
        _relaunch(app)
        _ui_claim(app, nest_url=private_url, claim_code=PRIVATE_CLAIM_CODE,
                  handle=handle, secret_hex=secret_hex, nat_mode="private")
        _reassert_session(app, nest_url=private_url, secret_hex=secret_hex, view="admin")
        app.driver.wait_for("admin-dashboard-heading", timeout=30.0)
        app.admin.navigate_mail()
        _ensure_admin_mail_enabled(app)
        home_nest = {
            "url": private_url,
            "admin": {"signing_key": SigningKey(bytes.fromhex(secret_hex))},
        }
        # Post-enable convergence (waits /data/imap-enabled + MDA up, asserts the
        # MTA stays DOWN, waits auto-approval, provisions the self-signed LAN
        # cert — setup with no client surface for a test domain) — the shared
        # helper the API-driven two-nest test uses after ITS enable.
        await_mda_only_serving(priv_name, home_nest, ("127.0.0.1", priv_993), box.fqdn)
        print("[relay-live] private box claimed (NAT: private) via UI; MDA serving, MTA down")

        # ── Phase 6: ONE LinkBoth from the public session pairs the two boxes ─
        # Authenticated relaunch onto the PUBLIC box (seeded credential store →
        # the real silent-challenge boot), then the linked-nests page: entering
        # the home box's address seeds the reciprocal fauna.pair.add rows on
        # BOTH nests — the home box's row recording this session's relay URL,
        # the target its worker dials — AND fires MailRelayProvisionHook → ProvisionRelayMailbox
        # (fleet-MSEK read recipe onto the home box) — the one-action user flow.
        _relaunch(app, extra={"seed_credentials": {
            "secret_key": secret_hex,
            "node_url": public_fqdn_url,
            "device_id": "relay-live-e2e",
        }})
        _await_authenticated(app)
        _open_settings_page(app, nest_url=public_fqdn_url, secret_hex=secret_hex,
                            sub_id="nests",
                            anchor_check=app.linked_nests.is_page_visible)
        app.linked_nests.link(private_url)
        assert app.linked_nests.wait_for_pairing_count(1, timeout=30.0), (
            "the both-ends link should add one pairing on the public nest. "
            f"error: {app.linked_nests.page_error_text()!r}"
        )
        assert not app.linked_nests.page_error_text(timeout=2.0), (
            "a successful both-ends link must surface no error"
        )
        print("[relay-live] LinkBoth OK — both pairing rows seeded, home mailbox provisioned")

        # ── Phase 7: send real internet mail via example.com ───────────────────
        nonce = uuid.uuid4().hex[:12]
        subject = f"relay-live {nonce}"
        body = f"private-relay round-trip {nonce} sent via example.com submission.\n"
        _submit_via_fauna_fan(sender_username, sender_password, sender_addr,
                              handle, subject, body)
        print(f"[relay-live] submitted {subject!r} via example.com ({sender_addr} → {handle})")

        # ── Phase 8: read it DECRYPTED over IMAP from the PRIVATE nest ───────
        # Covers example.com's outbound queue + MX delivery + the relay worker's
        # ~10 s poll + pull/append/ack. Outer retry because AUTH itself can race
        # the async ProvisionRelayMailbox/receive path right after LinkBoth.
        deadline = time.monotonic() + 600.0
        raw = None
        last_err: Exception | None = None
        while raw is None and time.monotonic() < deadline:
            try:
                raw = imap_fetch_only_inbox_message(
                    "127.0.0.1", priv_993, box.fqdn, mua_username, mua_password,
                    timeout=90.0)
            except (AssertionError, TimeoutError) as e:
                last_err = e
                time.sleep(5.0)
        assert raw is not None, (
            f"the example.com mail never became readable on the private nest's "
            f"IMAP within 600s (last: {last_err})\n\n── home-box bridge diag ──\n"
            f"{bridge_diag(priv_name, ('mda',))}"
        )
        text = raw.decode("utf-8", errors="replace")
        assert subject in text, (
            f"private nest must serve the decrypted Subject; first 400B: {text[:400]!r}")
        assert nonce in text, (
            f"private nest must serve the decrypted body nonce; first 400B: {text[:400]!r}")
        assert sender_addr in text, (
            f"the decrypted mail must carry the example.com sender; first 400B: {text[:400]!r}")
        print("[relay-live] python IMAP read on the PRIVATE nest OK (decrypted)")

        # ── Phase 9: no-readable-copy — the PUBLIC box's INBOX drains to 0 ───
        # (deployment doc § Inbound step 6: ack → tombstone + placement purge.)
        deadline = time.monotonic() + 180.0
        remaining = None
        while time.monotonic() < deadline:
            remaining = imap_inbox_count(
                box.ipv4, 993, f"mail.{box.fqdn}", mua_username, mua_password,
                timeout=30)
            if remaining == 0:
                break
            time.sleep(3.0)
        assert remaining == 0, (
            f"public relay box must hold NO readable copy after the home box "
            f"acked the relay; its INBOX still shows {remaining} message(s)"
        )
        print("[relay-live] no-readable-copy OK — public INBOX at 0 after relay+ack")

        # No-MTA boot, re-checked after real traffic: the private box never
        # started its perimeter parser (deployment doc § Plaintext-mode behavior).
        mta_state = svstat(priv_name, "fauna-mail-bridge-mta")
        assert not is_commanded_up(mta_state), (
            f"private home box MTA must stay DOWN throughout; got: {mta_state!r}")

        # ── Phase 10: read it in the LINUX APP UI on the private nest ─────
        # Authenticated relaunch onto the home box; the shared receive loop
        # fetches + decrypts on its own — poll the conversations list for the
        # nonce (the pattern of tests/test_mail_client_receive.py).
        _relaunch(app, extra={"seed_credentials": {
            "secret_key": secret_hex,
            "node_url": private_url,
            "device_id": "relay-live-e2e-home",
        }})
        _await_authenticated(app)
        _reassert_session(app, nest_url=private_url, secret_hex=secret_hex,
                          view="conversations")
        deadline = time.monotonic() + 180.0
        found = None
        while time.monotonic() < deadline and found is None:
            for t in app.conversations.list_threads():
                if nonce in (t.label or "") or nonce in (t.snippet or ""):
                    found = t
                    break
            if found is None:
                time.sleep(2.0)
        threads_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
        assert found is not None, (
            f"the relayed email tagged {nonce!r} never surfaced in the linux "
            f"client's conversations view on the PRIVATE nest within 180s.\n"
            f"  threads: {threads_dump}\n"
            f"  conversations error: {app.error_text()!r}"
        )
        assert found.rail == "Smtp", (
            f"received mail must land on the Smtp rail; got {found.rail!r}")
        print("[relay-live] linux client UI read on the PRIVATE nest OK")

        print(f"[relay-live] ALL PHASES PASSED for {box.fqdn} — tearing down.")
    finally:
        remove_container(priv_name)
