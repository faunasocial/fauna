"""tier_3 E2E (linux, tui, web, android, windows): the SESSION-START SWEEP's
own round trip through the two directory-backed feeders — feeder #3
(published ATProto handle binding) and feeder #1's *sweep*-driven path
(genesis-seniority custody).

Both feeders read the same PLC audit-log fetch inside one sweep pass
(`libs/fauna-client-alert-sweep::execute_audit_plan`), so this is one test
that mints, tampers, and establishes a session to prove both banners —
`critical-alerts.md` § Implementation status today ("Not yet covered
end-to-end") and `atproto-pds-bridge.md` § State & data shape (feeder #3's
four shape decisions).

`test_atproto_custody_alarm.py` already proves feeder #1's SETTINGS-PAGE path
(`connect_map` refresh via a nav cycle). It does NOT prove the sweep path —
the sweep is one-shot per session and its own doc comment states the debounce
is re-expressed as a **read order**, not the settings machine's
two-convergence wait, so a settings-page nav cycle cannot stand in for it.
Feeder #3 has no settings-page trigger at all — the sweep is its only caller
(`critical-alerts.md`: "No app change was needed — this is the first feeder
to join purely by plugging into the sweep's seam").

The journey, driven the way a user would (convention 8):

1. mint a hosted did:plc identity through the depth selector — a real Go
   bridge submits the genesis op to `FakePlcDirectory`;
2. TAMPER the served log's `alsoKnownAs` to a handle at a domain the user does
   not control, then re-establish the session (the universal post-auth hook —
   the sweep's only trigger) → the feed page shows `atproto-handle:<did>`;
   un-tamper, re-establish, and the alarm clears;
3. TAMPER the served log's `rotationKeys[0]` instead (feeder #1's own tamper,
   already used by the settings-page test) and re-establish → the feed page
   shows `atproto-custody:<did>` — this time raised by the SWEEP, not a
   settings-page nav; un-tamper, re-establish, clears;
4. negative arm: tamper `alsoKnownAs` to a STALE localpart at the SAME
   domain — the rename window `handle_binding.rs` exists to not cry wolf
   over — and re-establish: the banner must stay DOWN.

A second, separate test mints a did:web identity (no directory log to audit —
handle custody IS domain custody, `atproto-pds-bridge.md` § State & data
shape) and asserts the sweep never raises `atproto-handle:*` for it — the
"nothing to check" silence `critical-alerts.md` § Feeders requires. The
did:web resolvability self-check (`VerifyIdentityResolvable`,
`resolve.go:100`) is warn-only at mint time ("the mint is already recorded;
the check exists to surface a broken deployment loudly, never to fail it"),
so no real HTTPS `.well-known` endpoint is needed for the mint itself to
complete and be reported by `atproto.get_integration_status`.

Latency discipline (testing.md convention 14): every wait is a deadline poll
for a caused state transition; no settle-sleeps, no wall-clock asserts.
"""

import os
import subprocess
import time

import pytest

from actions import ActionLayer
from conftest import get_available_apps
from helpers.bridge_enrollment import approve_bridge
from helpers.atproto_fakes import FakePlcDirectory
from helpers.budgets import ALERT_SWEEP_PASS_S, APP_RELAUNCH_S
from helpers.directory_launch import launch_app_with_directory
from helpers.waiting import alert_sweep_passes, await_sweep_pass_after

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.web, pytest.mark.android, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios]

# Mirrors `test_atproto_custody_alarm.CUSTODY_APPS` — every app that renders
# the banner and calls the sweep (`critical-alerts.md` § Implementation status
# today; TRACK 4 wired the sweep call on web/linux/android, windows joined the
# same day, apple joined 2026-08-10 — all seven apps call the sweep loop now).
SWEEP_APPS = ("linux", "tui", "web", "android", "windows", "macos", "ios")


def _apps(*want: str) -> list[str]:
    available = get_available_apps()
    return [a for a in want if a in available]


HANDLE_DOMAIN = "fauna.test"
# A syntactically-valid K-256 did:key no client in this test holds (mirrors
# test_atproto_custody_alarm.EVIL_SENIOR).
EVIL_SENIOR = "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme"  # gitleaks:allow

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"


def _alert_text(app) -> str:
    """The active alert row's text, or "" when the banner is down — the
    presence rule itself (mirrors `test_session_start_alert_sweep._alert_text`).
    """
    if not app.driver.is_visible(CRITICAL_ALERTS):
        return ""
    return app.driver.get_text(CRITICAL_ALERT) or ""


def _reestablish_session(app, session: dict) -> None:
    """Re-run the app's universal post-auth hook on the SAME actor/device —
    the sweep's only trigger (`session::establish` / `FaunaClient::connect`'s
    post-auth path). A settings-page nav cycle does not reach it: the sweep
    is one-shot per session establishment, proven by
    `test_session_start_alert_sweep.py`'s identical technique (calling
    `_login_app_as` a second time on the same actor)."""
    app.driver.set_state({
        "session": session,
        "nav": {"stack": [{"view": "feed"}]},
    })


def _poll_for_fragment(app, fragment: str, want_present: bool, deadline_s: float = 60.0) -> str:
    """Deadline-poll the feed-page banner until the presence of `fragment`
    matches `want_present`. Generous ceiling — a green run pays only the actual
    sweep round trip. Read-only: the caller re-establishes the session ONCE
    before polling, so the app's post-auth hook (and the sweep it dispatches)
    fires exactly once per call — this loop must not re-trigger it."""
    deadline = time.monotonic() + deadline_s
    text = ""
    while time.monotonic() < deadline:
        text = _alert_text(app)
        if (fragment in text) == want_present:
            return text
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
    return text


def _mint_hosted_identity(app, request, directory, nest, tmp_path_factory, method="plc"):
    """Boot a real atproto bridge against `directory`, register `alice`, and
    mint a hosted identity of `method` ("plc" | "web") through the app's own
    ATProto settings UI. Returns (proc, tail-of-log fn).

    Lifted from `test_atproto_custody_alarm.test_genesis_seniority_alarm_end_to_end`
    (the mint ceremony is identical; only the DID method selection is new)."""
    from conftest import _IS_WINDOWS, _repo_root
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from drivers.port_util import popen_group_kwargs, reap_descendants_of, track_process

    # NOTE: the bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time (2026-08-16) — not here, and no longer in the test bodies
    # either. Building it at this point, mid-session, left an already-
    # authenticated driver idle for the whole `just atproto-bridge-build`
    # dependency chain (it pulls in `mail-bridge-ffi`), which under real fleet
    # load is minutes long and can outlast whatever keeps a live session's UI
    # responsive. Collection time keeps that ordering (it is earlier still) AND
    # takes the recipe's unconditional build-slot wait out of the per-test
    # `timeout = 900`, which the in-body call used to blow through under
    # contention. Mirrors `test_atproto_custody_alarm.py`'s proven ordering:
    # build first, launch second.
    # Windows has no rpath/LD_LIBRARY_PATH equivalent, so `windows-atproto-bridge-build-e2e`
    # stages the exe (+ its fauna_ffi.dll/libunwind.dll) in target/, not
    # bins/fauna-bridges/ like the Unix flavors (justfile's `_windows-go-cgo-build`).
    # The e2e FLAVOR, not the production binary: the FAUNA_ATPROTO_* seams this
    # test sets are compiled only into `-tags fauna_e2e_fixtures` (convention 15,
    # e2e-automation-surface-gating.md → the Go bridges' leg); the shipped
    # flavor ignores them, so against it this test could not even reach its
    # fakes. The shipped flavor itself is exercised by test_atproto_bridge_enroll
    # (tier_3) and the nest image tests (tier_4).
    if _IS_WINDOWS:
        atproto_bin = str(_repo_root / "target" / "fauna-atproto-bridge-e2e.exe")
    else:
        atproto_bin = str(_repo_root / "bins" / "fauna-bridges" / "fauna-atproto-bridge-e2e")

    admin = nest["admin"]
    tmp = tmp_path_factory.mktemp(f"alert-sweep-bridge-{method}")
    keyfile_path = tmp / "atproto.pds.key"
    minted = subprocess.run(
        [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
        cwd=_repo_root, capture_output=True, text=True, check=True,
    )
    pubkey_hex = minted.stdout.strip()
    env = os.environ.copy()
    env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
    log_path = tmp / "bridge.log"
    log_fh = open(log_path, "wb")
    proc = subprocess.Popen(
        [
            atproto_bin,
            f"--keypair-file={keyfile_path}",
            f"--nest-endpoint={nest['url']}",
            f"--data-dir={tmp.as_posix()}",
            "--log-level=debug",
        ],
        stdout=log_fh, stderr=subprocess.STDOUT, env=env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()` is
    # `{}` there, so without this the bridge's only protection is the atexit
    # sweep a killed run never reaches (testing.md § point 9). No-op off Windows.
    reap_descendants_of(proc.pid)
    track_process(proc)

    def _tail() -> str:
        return log_path.read_text(errors="replace")[-4000:]

    admin_ws = WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with admin_ws:
        admin_ws.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": HANDLE_DOMAIN,
                "mta_sts_cert_mode": "per_host",
            },
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            rows = admin_ws.call(
                "fauna.bridges.list_service_users", {"status": "pending"}
            )["service_users"]
            if any(bytes(r["ed25519_pubkey"]).hex() == pubkey_hex for r in rows):
                break
            assert proc.poll() is None, "bridge exited pre-enroll; log:\n" + _tail()
            time.sleep(0.5)
        approve_bridge(
            nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

    bs = app.atproto_settings
    bs.navigate()
    # This is the FIRST navigation right after a fresh driver launch — the
    # generous `APP_RELAUNCH_S` budget, not the plain-UI default, since web's
    # boot has a documented, self-limiting double-mount that a plain 10s can
    # lose the race against under real load (see `is_page_visible`'s doc).
    page_visible = bs.is_page_visible(timeout=APP_RELAUNCH_S)
    if not page_visible and hasattr(app.driver, "console_log"):
        console = "\n".join(app.driver.console_log())
        assert page_visible, (
            f"atproto page unreachable: {app.error_text()!r}\nbrowser console:\n{console}"
        )
    assert page_visible, f"atproto page unreachable: {app.error_text()!r}"
    assert bs.wait_for_depth_enabled("hosted_visible"), (
        f"hosted must be selectable on a public domain: "
        f"{bs.depth_gate_marker('hosted_visible')!r} err={bs.page_error_text()!r}"
    )
    # The DID-method radio renders only "at (or entering) a hosted level"
    # (`apps/fauna-tui/src/settings/atproto.rs`: `at_hosted || targeting_hosted`)
    # — it does NOT exist before a depth rung is picked, so `select_depth` must
    # come first; "pre-mint" (ui.yaml) means before the mint completes, not
    # before choosing a level.
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), f"no transition card: {bs.page_error_text()!r}"
    if method == "web":
        assert bs.is_did_method_visible(), (
            f"the DID-method radio must be visible once staging the hosted "
            f"transition: {bs.page_error_text()!r}"
        )
        bs.select_did_method("web")
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"confirm failed: level={bs.depth_level()!r} err={bs.page_error_text()!r}"
    )

    if method == "plc":
        # Mirrors `test_atproto_custody_alarm.py`'s own proven completion
        # signal: the bridge's mint loop submits the genesis op to `directory`
        # synchronously with minting.
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not directory.snapshot():
            assert proc.poll() is None, "bridge died before minting; log:\n" + _tail()
            time.sleep(0.5)
        assert directory.snapshot(), "the bridge never submitted a genesis op; log:\n" + _tail()
    else:
        # did:web never touches the fake PLC directory at all (no directory in
        # that method's path), so the bridge's own log line — written
        # synchronously by the mint loop, same place "atproto identity minted"
        # appears in the plc case's log excerpt — is the completion signal.
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and "atproto identity minted" not in _tail():
            assert proc.poll() is None, "bridge died before minting; log:\n" + _tail()
            time.sleep(0.5)
        assert "atproto identity minted" in _tail(), f"did:web mint never completed; log:\n{_tail()}"

    return proc, _tail




@pytest.mark.parametrize("sweep_app", _apps(*SWEEP_APPS))
@pytest.mark.feature("critical-alerts")
def test_directory_feeders_alarm_via_session_start_sweep(
    sweep_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    from common.auth import register_handled_actor
    from conftest import _make_nest
    from drivers.port_util import untrack_process

    # The bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — before the driver session exists, which is the ordering
    # `_mint_hosted_identity`'s note calls load-bearing (satisfied a fortiori:
    # collection is earlier than any test body), and outside the per-test
    # timeout budget, which is what this used to get wrong. It ran
    # `subprocess.run(["just", ...])` right here until 2026-08-16; the recipe
    # takes the machine-wide build slot with a 5400 s bound, nested inside
    # `timeout = 900`, so a slot held elsewhere killed this test as a bare
    # `Timeout (>900.0s)` pointing at the build — indistinguishable from the
    # product regression this suite is under investigation for.
    # The fixture also picks the windows-vs-unix recipe, so that branch is gone
    # from here too.

    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "alert-sweep-directory-nest",
        claim_domain=HANDLE_DOMAIN,    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    driver = None
    spa_proxy_server = None
    try:
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        driver, spa_proxy_server = launch_app_with_directory(sweep_app, request, nest, directory)
        session = {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": alice["signing_key"].encode().hex(),
            "handle": f"alice@{HANDLE_DOMAIN}",
            "actor_id": alice["actor_id_bytes"].hex(),
            "device_id": "alert-sweep-e2e",
        }
        driver.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})
        app = ActionLayer(driver)

        proc, _tail = _mint_hosted_identity(
            app, request, directory, nest, tmp_path_factory, method="plc"
        )
        submissions = directory.snapshot()
        assert submissions, "the bridge never submitted a genesis op; log:\n" + _tail()
        did, op = submissions[0]
        assert op["rotationKeys"], "genesis op carries rotation keys"
        assert op["alsoKnownAs"] == [f"at://alice.{HANDLE_DOMAIN}"], (
            f"unexpected genesis alsoKnownAs: {op['alsoKnownAs']!r}"
        )

        # ── 1. Feeder #3, sweep-driven: alsoKnownAs at a domain the user does
        # not control → the sweep must alarm on the very NEXT session start,
        # on the feed page (no settings-page visit at all). ──
        directory.tamper_also_known_as(["at://alice.evil.example"])
        _reestablish_session(app, session)
        text = _poll_for_fragment(app, f"at {HANDLE_DOMAIN}", want_present=True)
        assert f"at {HANDLE_DOMAIN}" in text, (
            "feeder #3's sweep-driven alarm must appear once the published "
            f"handle names a domain the user does not control; banner reads "
            f"{text!r}; bridge log:\n{_tail()}"
        )
        assert "evil.example" in text, f"the alarm names what WAS published: {text!r}"

        directory.tamper_also_known_as(None)
        _reestablish_session(app, session)
        cleared_text = _poll_for_fragment(app, f"at {HANDLE_DOMAIN}", want_present=False)
        assert f"at {HANDLE_DOMAIN}" not in cleared_text, (
            f"a passing re-check must clear the handle-binding alarm; got {cleared_text!r}"
        )

        # ── 2. Feeder #1, SWEEP-driven this time (not the settings-page nav
        # cycle `test_atproto_custody_alarm.py` already proves): a senior key
        # this client does not hold. ──
        directory.tamper_senior_key(EVIL_SENIOR)
        _reestablish_session(app, session)
        text = _poll_for_fragment(app, "alice", want_present=True)
        assert "alice" in text, (
            "feeder #1's sweep-driven alarm must appear on the feed page; "
            f"banner reads {text!r}; bridge log:\n{_tail()}"
        )

        directory.tamper_senior_key(None)
        _reestablish_session(app, session)
        cleared_text = _poll_for_fragment(app, "alice", want_present=False)
        assert "alice" not in cleared_text, (
            f"a passing re-check must clear the sweep-driven custody alarm; got {cleared_text!r}"
        )

        # ── 3. Negative arm — a rename window: a STALE localpart at the SAME
        # domain must stay silent (handle_binding.rs: whole-handle equality
        # would cry wolf on every legitimate rename). ──
        #
        # A "must not happen" assert, anchored causally rather than to the
        # clock (convention 14, mechanism 2): read the sweep's pass counter at
        # the moment the condition is planted, re-establish, and wait for a pass
        # that BEGAN after the plant to finish. Only then is "no banner" a
        # verdict about the sweep's decision rather than about whether it has
        # got round to looking — which is all the 60s sleep here could ever say.
        started, _ = alert_sweep_passes(app.driver)
        directory.tamper_also_known_as([f"at://oldalice.{HANDLE_DOMAIN}"])
        _reestablish_session(app, session)
        await_sweep_pass_after(
            app.driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the stale localpart"
        )
        stale_text = _alert_text(app)
        assert f"at {HANDLE_DOMAIN}" not in stale_text, (
            "a stale localpart at the RIGHT domain must never alarm — "
            f"banner reads {stale_text!r}"
        )
        directory.tamper_also_known_as(None)
    finally:
        if driver is not None:
            try:
                driver.teardown()
            except Exception:
                pass
        if spa_proxy_server is not None:
            spa_proxy_server.shutdown()
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
            untrack_process(proc)
        nest_cleanup()
        directory.close()


@pytest.mark.parametrize("sweep_app", _apps(*SWEEP_APPS))
@pytest.mark.feature("critical-alerts")
def test_custody_alarm_reaches_a_fresh_sign_in_before_the_runtime_assembles(
    sweep_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """Feeder #1 on a FRESH sign-in, with no ATProto-page visit and no
    ``alert_sweep_wake`` poke: the sweep's first pass fires at the post-auth
    hook, before the account runtime it reads the rotation keyring through has
    assembled, so that pass finds the custody door refusing. The loop must
    re-run the pass at the door's readiness edge — not leave feeder #1 unrun
    until the 6 h re-sweep (`critical-alerts.md` § Mechanism → *How often the
    detector runs*, the readiness-edge rule).

    The fresh sign-in is a sign-out and a sign-in on the same device, in the
    running app, on every app: the runtime is torn down and assembled again,
    and the post-auth hook fires before it is up. On a native app the sign-out
    also erases the replica and retires the machine's writer key, so the minted
    rotation key is one the new sign-in can only read by keying its generation
    from escrow — and the prologue re-presents the rows that key opens, so the
    ring is whole at the readiness edge (`account-data-taxonomy.md` § The
    generation machinery → *Escrow recovery*, item (3)). Web's sign-out keeps
    the origin's store, so there the ring is read from the replica it left.
    (A relaunch is not the journey: on web it is a page reload, which drops the
    test-only fake-directory override the wasm chunks hold in memory.)
    The tamper lands while the app sits on the feed page, so neither the
    settings machine nor the running loop can raise the alarm first — only the
    new session's loop can, and the wake is never pressed."""
    from common.auth import open_registration, register_handled_actor
    from conftest import _make_nest
    from drivers.port_util import untrack_process

    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "alert-sweep-fresh-signin-nest",
        claim_domain=HANDLE_DOMAIN,
    )
    open_registration(nest)
    proc = None
    driver = None
    spa_proxy_server = None
    try:
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        driver, spa_proxy_server = launch_app_with_directory(sweep_app, request, nest, directory)
        session = {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": alice["signing_key"].encode().hex(),
            "handle": f"alice@{HANDLE_DOMAIN}",
            "actor_id": alice["actor_id_bytes"].hex(),
            "device_id": "alert-sweep-fresh-signin-e2e",
        }
        driver.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})
        app = ActionLayer(driver)

        proc, _tail = _mint_hosted_identity(
            app, request, directory, nest, tmp_path_factory, method="plc"
        )
        assert directory.snapshot(), "the bridge never submitted a genesis op; log:\n" + _tail()

        # Off the ATProto page before the tamper, so its convergence cannot be
        # what raises the alarm this test is about.
        driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        directory.tamper_senior_key(EVIL_SENIOR)

        app.settings.sign_out()
        driver.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})

        text = _poll_for_fragment(app, "alice", want_present=True, deadline_s=APP_RELAUNCH_S)
        assert "alice" in text, (
            "a fresh sign-in's sweep must run feeder #1 once the account runtime "
            "is readable — without a wake and without the ATProto page; banner "
            f"reads {text!r}; bridge log:\n{_tail()}"
        )
    finally:
        if driver is not None:
            try:
                driver.teardown()
            except Exception:
                pass
        if spa_proxy_server is not None:
            spa_proxy_server.shutdown()
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
            untrack_process(proc)
        nest_cleanup()
        directory.close()


@pytest.mark.parametrize("sweep_app", _apps("tui"))
@pytest.mark.feature("critical-alerts")
def test_did_web_identity_never_triggers_handle_binding_alarm(
    sweep_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """`plan_handle_binding` gates the feeder on `id.method == "plc"`
    (`fauna-client-alert-sweep/src/lib.rs`) — a did:web identity has no PLC
    directory log to audit at all (its custody IS domain custody), so the
    sweep must silently skip it rather than alarm. tui only: the claim is
    about the SWEEP's gate, identical on every app since it is shared Rust —
    the per-app UI proof already exists for feeder #3's plc path above."""
    from common.auth import register_handled_actor
    from conftest import _make_nest
    from drivers.port_util import untrack_process

    # The bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — before the driver session exists, which is the ordering
    # `_mint_hosted_identity`'s note calls load-bearing (satisfied a fortiori:
    # collection is earlier than any test body), and outside the per-test
    # timeout budget, which is what this used to get wrong. It ran
    # `subprocess.run(["just", ...])` right here until 2026-08-16; the recipe
    # takes the machine-wide build slot with a 5400 s bound, nested inside
    # `timeout = 900`, so a slot held elsewhere killed this test as a bare
    # `Timeout (>900.0s)` pointing at the build — indistinguishable from the
    # product regression this suite is under investigation for.
    # The fixture also picks the windows-vs-unix recipe, so that branch is gone
    # from here too.

    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "alert-sweep-didweb-nest",
        claim_domain=HANDLE_DOMAIN,    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    driver = None
    spa_proxy_server = None
    try:
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        driver, spa_proxy_server = launch_app_with_directory(sweep_app, request, nest, directory)
        session = {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": alice["signing_key"].encode().hex(),
            "handle": f"alice@{HANDLE_DOMAIN}",
            "actor_id": alice["actor_id_bytes"].hex(),
            "device_id": "alert-sweep-didweb-e2e",
        }
        driver.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})
        app = ActionLayer(driver)

        proc, _tail = _mint_hosted_identity(
            app, request, directory, nest, tmp_path_factory, method="web"
        )

        # No directory submission is expected for did:web (nothing to submit
        # to a PLC directory) — the identity is reported straight from the
        # nest's own record once the bridge's local mint completes.
        #
        # Same negative-assert shape as the stale-localpart arm, on the same
        # causal barrier: the did:web identity is already minted, so the pass
        # counter is read here and a pass that starts after this point is one
        # that saw it and declined to alarm.
        started, _ = alert_sweep_passes(app.driver)
        _reestablish_session(app, session)
        await_sweep_pass_after(
            app.driver, started, budget_s=ALERT_SWEEP_PASS_S, what="the did:web identity"
        )
        text = _alert_text(app)
        assert f"at {HANDLE_DOMAIN}" not in text, (
            "a did:web identity has no PLC log to audit and must never raise "
            f"the handle-binding alarm; banner reads {text!r}; bridge log:\n{_tail()}"
        )
    finally:
        if driver is not None:
            try:
                driver.teardown()
            except Exception:
                pass
        if spa_proxy_server is not None:
            spa_proxy_server.shutdown()
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
            untrack_process(proc)
        nest_cleanup()
        directory.close()
