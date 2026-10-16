"""tier_3 E2E: S4-C — the client-side genesis-seniority custody alarm, and the
recovery fork that undoes what it detects.

The ratified MUST (`atproto-pds-bridge.md` § State & data shape): after a
hosted mint, the client resolves the published ``did:plc`` from the public PLC
directory — its own HTTPS connection, no nest dependency — and asserts
``rotationKeys[0]`` is its own stored senior rotation key, alarming on
mismatch. The alarm renders on the cross-page critical-alerts banner
(`critical-alerts.md`; ui.yaml ``global:`` ``critical-alerts`` /
``critical-alert[N]``), so it is visible from any page, not just Settings →
Bluesky.

Full production path, driven the way a user would: a real client mints through
the depth selector (the machine generates the senior rotation key client-side), a
real Go bridge submits the genesis op to ``FakePlcDirectory``, and the client's
own directory read decides the verdict — native via the
``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` env var (same artifact/test IPC the bridge
uses), web via the ``enable_fake_plc_directory`` driver hook (a browser has no
process env; see ``drivers/web.py``):

1. mint → the directory's honest log carries the user's key senior;
2. TAMPER the served log (a senior key this client does not hold) → re-open
   the page (``connect_map`` refresh) → the alarm appears, and stays visible on
   the feed page — the every-page half of the contract;
3. un-tamper → re-open → the re-check passes and CLEARS the alarm — which also
   proves the honest-directory path raises no false alarm.

The other two tests are a matched pair over the recovery fork, because detection
without a reachable remedy is what shipped before 2026-08-02:

- the **refusal** (`test_a_forged_seizure_alarms_but_is_never_signed`) — a
  forged standing op above an honest genesis raises the alarm and renders the
  contest card, but the client will not sign a fork from a log it cannot
  authenticate;
- the **remedy** (`test_recovery_fork_contest_end_to_end`) — a seizure the
  bridge genuinely signs with the junior rotation key the genesis lists is
  contested through the tui ceremony, and the directory records a fork that
  nullifies it. That signature can only come from the bridge (the fake holds no
  key the genesis lists, and handing it one would be a worse hole than that
  was), which is what the `-tags fauna_e2e_seize` build flavor exists for.

Latency discipline (testing.md convention 14): every wait is a deadline poll
for a caused state transition (mint recorded / alarm present / alarm absent);
no settle-sleeps, no wall-clock asserts.

App-parametrized over the apps that render the banner (`critical-alerts.md`
§ Implementation status today tracks the roll-out). It is a *global* surface, so
this suite is where a new app's banner is proven end-to-end: the alarm has to
cross a real machine → the shared registry → that app's own chrome, which is
precisely the seam no in-process test can reach.
"""

import contextlib
import subprocess
import time

import pytest

from conftest import get_available_apps
from drivers import create_driver
from helpers.app_surface import skip_unbuilt
from helpers.atproto_hosted_world import hosted_world
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.web, pytest.mark.android, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios]

# The apps that render `critical-alerts` today — every app now. android's leg (2026-08-01) needs a connected adb device to
# actually run — `_apps()` intersects against `get_available_apps()`, so this
# is a no-op selection on a machine with no emulator attached, until one is.
CUSTODY_APPS = ("linux", "tui", "web", "android", "windows", "macos", "ios")


def _apps(*want: str) -> list[str]:
    """The wanted apps this run actually selected, intersected with what the
    machine / ``--app`` offers — so ``--app tui`` runs [tui] and a bare ubuntu run
    stays [linux, tui]. Mirrors ``test_version_mismatch_launch._apps``."""
    available = get_available_apps()
    return [a for a in want if a in available]

# The tampered "senior" key: a syntactically-valid K-256 did:key that no client
# in this test holds (the S0 probe's spec-vector shape, not a real secret).
EVIL_SENIOR = "did:key:zQ3shokFTS3brHcDQrn82RUDfCZESWL1ZdCEJwekUDPQiYBme"  # gitleaks:allow

CRITICAL_ALERTS = "critical-alerts"
CRITICAL_ALERT = "critical-alert"


def _nav_cycle(app):
    """Leave and re-enter the atproto page: ``connect_map`` fires the machine
    refresh, which re-runs the custody check against the (possibly tampered)
    directory."""
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    app.atproto_settings.navigate()


def _directory_reads(directory) -> str:
    """One line splitting a silent custody alarm in two: did the client ever
    ASK the directory after the tamper?

    Feeder #1's whole verdict rests on `GET /{did}/log/audit`, so a failure
    with **zero post-tamper reads** is upstream of the check (the client never
    ran it, or never reached this fake — the `FAUNA_ATPROTO_PLC_DIRECTORY_URL`
    seam, the page's refresh trigger, or the machine's own gating), while reads
    with no banner is downstream (verdict, registry, or rendering). Reporting
    it here is what keeps the next diagnosis off the ~80-minute guess-and-rerun
    loop this test cost on windows (conventions point 6 — read the diagnostic
    BEFORE the assertion, so the failure explains itself)."""
    reads = list(directory.audit_reads)
    after = sum(1 for _did, tampered in reads if tampered)
    return (
        f"directory audit-log reads: {len(reads)} total, {after} AFTER the tamper "
        + (
            "→ the client never re-read the log, so the break is UPSTREAM of the "
            "custody verdict (env seam / refresh trigger / machine gating)"
            if after == 0
            else "→ the client DID re-read the tampered log, so the break is "
            "DOWNSTREAM (verdict, registry post, or banner rendering)"
        )
    )


def _banner_shape(app) -> str:
    """Split the DOWNSTREAM half `_directory_reads` narrows to: is the alert in
    the registry (rows present) but not *visible*, or never posted at all?

    `is_visible` is a conjunction — the element must exist, be shown, AND not
    be offscreen (memory `reference_windows_e2e_is_visible_offscreen`) — so on
    its own a False tells you nothing about which conjunct failed. The row
    count separates them: >=1 row means the feeder posted and the registry
    reached the bound collection, leaving only rendering/offscreen; 0 rows
    means the break is at the verdict or the registry post, upstream of any
    XAML question."""
    try:
        container = app.driver.count(CRITICAL_ALERTS)
        rows = app.driver.count(CRITICAL_ALERT)
    except Exception as exc:  # a probe must never mask the real assertion
        return f"banner shape: probe failed ({type(exc).__name__}: {exc})"
    return (
        f"banner shape: {container} `critical-alerts` container(s), {rows} "
        f"`critical-alert` row(s) "
        + (
            "→ the alert DID reach the bound collection, so the break is "
            "RENDERING (visibility/offscreen), not the feeder"
            if rows
            else "→ no rows bound, so the break is the VERDICT or the registry "
            "post, not rendering"
        )
    )


def _poll_alarm_state(app, want_visible: bool, deadline_s: float = 60.0) -> bool:
    """Deadline-poll nav cycles until the critical-alerts banner reaches the
    wanted state. Generous ceiling — a green run pays only the actual latency
    (mint recording + one refresh round-trip)."""
    deadline = time.monotonic() + deadline_s
    while time.monotonic() < deadline:
        _nav_cycle(app)
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
        if app.driver.is_visible(CRITICAL_ALERTS) == want_visible:
            return True
    return app.driver.is_visible(CRITICAL_ALERTS) == want_visible


def _poll_contest_card_visible(app, bs, deadline_s: float = 60.0) -> bool:
    """Deadline-poll nav cycles until `atproto-contest-card` renders.

    The custody banner and the contest card are both derived from the SAME
    settings-machine convergence (`refresh()` runs `check_custody()` then
    `converge_contest()` in one sequential pass — machine.rs), but they are
    NOT guaranteed to appear on the same tick everywhere: `check_custody`'s
    own alarm can also be raised by the independent one-shot session-start
    sweep (`run_critical_alert_sweep`, wired on every app per
    `critical-alerts.md` § Mechanism → *Who runs the detector*), which is
    NOT gated on this machine instance's own convergence state. So
    `_poll_alarm_state` returning True proves the alarm is up; it does not
    prove THIS settings-machine instance has itself converged far enough to
    populate `contest` (`converge_contest_reporting`'s own gate:
    `s.custody_alarmed.as_deref() == Some(d.as_str())`, set only by this
    same instance's `check_custody`). A single immediate check after one
    `bs.navigate()` was measured flaky under heavy machine load (2026-08-22)
    — testing.md convention 14: assert latency-independent state via a
    deadline poll, not a one-shot check, whenever two states can converge on
    different ticks."""
    deadline = time.monotonic() + deadline_s
    while time.monotonic() < deadline:
        bs.navigate()
        if bs.is_contest_card_visible():
            return True
        time.sleep(0.5)  # sleep-ok: pacing between poll iterations, not a settle-wait
    return bs.is_contest_card_visible()


@contextlib.contextmanager
def _minted_identity(custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory):
    """Bring the whole S4-C world up and mint through the UI, then hand the
    caller ``(app, directory, did, user_key, tail, seize, integration_status)``.

    Shared by every test in this file because the bring-up — nest, real Go
    bridge, admin approval, a real client pointed at the fake directory
    (``helpers.atproto_hosted_world.hosted_world``), and a mint driven through
    the depth selector — is identical for detecting a custody violation and for
    undoing one, and it is the expensive part.

    ``seize(did_key)`` signs a **real** hostile rotation with the bridge's own
    listed junior key and returns the submitted op's CID — the compromised-box
    half of the recovery-fork contest, which no fixture can fake (the fake
    directory holds no key the genesis lists). It runs the SAME e2e-flavor
    binary (`-tags fauna_e2e_seize`) as the long-lived bridge, so the process
    that signs the seizure is by construction the one that minted the genesis.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _repo_root

    with hosted_world(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as world:
        app, directory, alice, nest = world.app, world.directory, world.alice, world.nest
        bs = app.atproto_settings
        _tail = world.tail

        # ── 1. Mint through the UI: the machine generates the senior rotation
        # key on THIS client and sends only its pubkey. ──
        bs.navigate()
        assert bs.is_page_visible(), f"atproto page unreachable: {app.error_text()!r}"
        assert bs.wait_for_depth_enabled("hosted_visible"), (
            f"hosted must be selectable on a public domain: "
            f"{bs.depth_gate_marker('hosted_visible')!r} err={bs.page_error_text()!r}"
        )
        bs.select_depth("hosted_visible")
        assert bs.wait_for_card(), f"no transition card: {bs.page_error_text()!r}"
        bs.confirm_transition()
        assert bs.wait_for_depth_level("hosted_visible"), (
            f"confirm failed: level={bs.depth_level()!r} err={bs.page_error_text()!r}"
        )

        did, user_key = world.await_genesis()

        def _seize(did_key: str) -> str:
            """Drive the bridge's own signing key against the identity it minted.

            This is the ratified threat, not a new capability: the genesis lists
            the bridge's junior rotation key, and PLC accepts a next op from any
            listed key (`atproto-pds-bridge.md` § State & data shape). The op is
            built and signed by the SAME production calls the handle-rename hook
            makes, so what the contest is proven against is the shipped
            mechanism.

            Synchronous by design — a failure surfaces as a non-zero exit with
            the bridge's own error on stderr, rather than as a later assertion
            about a side effect that never happened.
            """
            run = subprocess.run(
                [
                    world.atproto_bin,
                    f"--keypair-file={world.keyfile_path}",
                    f"--nest-endpoint={nest['url']}",
                    f"--seize-did={did}",
                    f"--seize-rotation-key={did_key}",
                    "--log-level=warn",
                ],
                cwd=_repo_root, capture_output=True, text=True, env=world.env,
            )
            assert run.returncode == 0, (
                "the bridge failed to sign the seizure "
                f"(exit {run.returncode}):\n{run.stderr}"
            )
            return run.stdout.strip().splitlines()[-1]

        def _integration_status() -> dict:
            """The nest's own record of alice's integration, read over the
            wire as alice (`fauna.bridges.atproto.get_integration_status`) —
            for the durable nest-side facts the page does not paint, such as
            the retirement intent."""
            with WsRpcAdminClient(
                nest["url"],
                actor_id=alice["actor_id_bytes"],
                signing_key=bytes(alice["signing_key"]),
            ) as alice_ws:
                return alice_ws.call("fauna.bridges.atproto.get_integration_status", {})

        yield app, directory, did, user_key, _tail, _seize, _integration_status


@pytest.mark.parametrize("custody_app", _apps(*CUSTODY_APPS))
@pytest.mark.feature("critical-alerts")
def test_genesis_seniority_alarm_end_to_end(
    custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    with _minted_identity(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as (
        app,
        directory,
        _did,
        _user_key,
        _tail,
        _seize,
        _status,
    ):
        # ── 2. TAMPER: the directory now claims a senior key this client does
        # not hold. The re-opened page's custody check must alarm — and the
        # banner must show on a NON-atproto page (the every-page contract). ──
        directory.tamper_senior_key(EVIL_SENIOR)
        alarm_appeared = _poll_alarm_state(app, want_visible=True)
        console = "\n".join(app.driver.console_log()) if custody_app == "web" and not alarm_appeared else ""
        assert alarm_appeared, (
            "the custody alarm must appear once the directory's published log "
            f"contradicts this client's key; {_directory_reads(directory)}"
            f"\n{_banner_shape(app)}"
            f"\nbridge log:\n{_tail()}"
            + (f"\nbrowser console:\n{console}" if console else "")
        )
        alarm_text = app.driver.get_text(CRITICAL_ALERT, 0)
        assert "alice" in alarm_text, (
            f"the alarm names the handle: {alarm_text!r}"
        )
        # The banner is the ONLY surface that fires when an identity is under
        # attack, so it must not route the victim solely at the party most
        # likely to be the attacker: it names the remedy and where to find it
        # (user directive 2026-08-02; copy owned by
        # i18n/strings/en.yaml critical_alerts.atproto_custody_mismatch).
        assert "undo" in alarm_text.lower(), (
            "the alarm must tell the user a remedy exists, not only to contact "
            f"whoever runs the nest: {alarm_text!r}"
        )
        app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        assert app.driver.is_visible(CRITICAL_ALERTS), (
            "the critical-alerts banner renders on EVERY page, not just Bluesky"
        )

        # ── 3. Un-tamper: the honest log verifies (the user's key IS senior)
        # and the re-check CLEARS the alarm — no false alarm on a clean
        # directory. ──
        directory.tamper_senior_key(None)
        assert _poll_alarm_state(app, want_visible=False), (
            "a passing re-check must clear the custody alarm"
        )


# The apps that render the contest ceremony (`atproto-contest-*`). tui is the
# lead app; the other six follow in the batched trickle-down. Deliberately NOT folded into the alarm test above as a
# conditional leg: a leg that quietly does nothing on six of seven apps reports
# as a pass, which is the hidden-gap shape convention 7 exists to stop.
CONTEST_APPS = ("tui", "linux", "android", "web", "macos", "ios", "windows")


@pytest.mark.parametrize("custody_app", _apps(*CUSTODY_APPS))
@pytest.mark.feature("atproto")
def test_a_forged_seizure_alarms_but_is_never_signed(
    custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """A FORGED seizure raises the alarm but never obtains a signature
    (`atproto-pds-bridge.md` § State & data shape, the recovery-fork contest).

    Every link up to the refusal is real: a standing op above an honest genesis
    claims the identity, the client's OWN directory read finds it, the alarm
    fires, and the contest card renders the honest no-remedy state — detection is
    deliberately permissive, so a log the client cannot authenticate still
    explains itself rather than leaving the alarm pointing at a blank page. What
    must not happen, and is asserted here, is a fork: the op carries no signature
    from any key the chain lists, and the user's senior rotation key must never
    sign bytes a directory merely *asserted*.

    ⚠ **This test does not prove a successful contest, deliberately.** It is the
    negative half of a pair: `inject_forged_rotation` cannot sign as a key the
    genesis lists — only the user's client and the bridge hold those — so what it
    produces is a forged log, and until 2026-08-02 the assertions here claimed
    the remedy because nothing verified signatures. The positive half is
    `test_recovery_fork_contest_end_to_end` below, which drives a genuinely
    bridge-signed seizure through the ``_seize`` seam.

    `tamper_senior_key` is the wrong hook here either way: it rewrites the
    genesis too, which is honestly `not-contestable` (there is no earlier state
    to return to).
    """
    if custody_app not in CONTEST_APPS:
        driver = create_driver(custody_app)
        skip_unbuilt(
            driver,
            surface="atproto-contest-card",
            detail="the recovery-fork ceremony landed on tui (lead app) 2026-08-02",
            tracked="ui/atproto.md § Implementation status today; ",
        )

    with _minted_identity(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as (
        app,
        directory,
        did,
        user_key,
        _tail,
        _seize,
        _status,
    ):
        bs = app.atproto_settings

        # ── The box seizes the identity: a standing op, chained honestly onto
        # the genesis, whose senior rotation key is one this client never held.
        # The genesis itself stays verifiable, so the violation is at standing
        # index 1 — the contestable shape. ──
        directory.inject_forged_rotation(EVIL_SENIOR, did=did)
        assert _poll_alarm_state(app, want_visible=True), (
            "a seized identity must raise the custody alarm; "
            f"{_directory_reads(directory)}; {_banner_shape(app)}; log:\n" + _tail()
        )
        assert directory.standing_rotation_keys(did)[0] == EVIL_SENIOR, (
            "precondition: the world currently believes the box's key is senior"
        )

        # ── The remedy is on the page the alarm points at, ABOVE the selector.
        # The card renders — detection is deliberately permissive, so a log the
        # client cannot authenticate still raises the banner and still explains
        # itself. ──
        assert _poll_contest_card_visible(app, bs), (
            "the contest card must render on a standing custody violation; "
            f"page error={bs.page_error_text()!r}"
        )

        # ── But NOTHING is signed, because this log does not authenticate.
        #
        # `inject_forged_rotation` produces an op the fake cannot sign — it
        # carries the previous op's signature over different bytes — so
        # the client refuses to build a fork from it. That is
        # the property under test here, and it is the one that matters most:
        # an attacker who can only *serve* a log (a hostile directory, a TLS
        # MITM) must not be able to obtain the user's senior-key signature, nor
        # induce the user into nullifying their own real history.
        #
        # This case proves only the refusal, on purpose. The successful contest
        # is the next test, over a seizure the bridge really signs. ──
        assert bs.contest_state() == "not-contestable", (
            "a log whose operations do not authenticate must NOT offer a "
            f"contest; state={bs.contest_state()!r}"
        )
        # …and it must SAY so. A no-remedy state that renders no explanation is
        # the shape the close accidentally shipped: the banner shouts
        # that the identity may be seized, and the page it sends the user to is
        # blank. The copy names the actual condition — the record, not a key.
        detail = bs.contest_detail()
        assert "does not check out" in detail, (
            "the card must explain that the published record itself is what "
            f"failed, not that a remedy was declined: {detail!r}"
        )
        assert not directory.forks, (
            "nothing may be submitted from an unauthenticatable log; "
            f"forks={directory.forks!r}"
        )
        assert directory.standing_rotation_keys(did)[0] != user_key, (
            "precondition intact: the forged seizure still stands, uncontested"
        )
        assert _poll_alarm_state(app, want_visible=True), (
            "and the alarm stays up — refusing to act is not the same as "
            "deciding the user is safe"
        )


@pytest.mark.parametrize("custody_app", _apps(*CUSTODY_APPS))
@pytest.mark.feature("atproto")
def test_recovery_fork_contest_end_to_end(
    custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """The 72 h recovery fork, driven the way a user would (`atproto-pds-bridge.md`
    § State & data shape, the recovery-fork contest; ceremony IDs approved
    2026-08-02).

    This is the whole remedy, end to end, and every link is real — including the
    attack. The bridge signs a rotation that displaces the user's senior key,
    using its own junior key, which the genesis lists and PLC accepts ops from:
    the ratified threat model made drivable, not a fixture pretending. The
    client's OWN directory read finds it, authenticates the chain through the
    contested op, the user reads what happened and confirms,
    and the fork this client signs with its own held recovery key is what the
    directory accepts. The alarm then comes down on the **directory's** evidence
    — never on our own submit — which is why the final assertion polls the banner
    rather than the response.

    Two hooks are deliberately NOT used here. `tamper_senior_key` rewrites the
    genesis too, which is honestly `not-contestable` (there is no earlier state
    to return to). `inject_forged_rotation` produces a log no key ever signed,
    which the client must refuse — that is the test above.
    """
    if custody_app not in CONTEST_APPS:
        driver = create_driver(custody_app)
        skip_unbuilt(
            driver,
            surface="atproto-contest-card",
            detail="the recovery-fork ceremony landed on tui (lead app) 2026-08-02",
            tracked="ui/atproto.md § Implementation status today; ",
        )

    with _minted_identity(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as (
        app,
        directory,
        did,
        user_key,
        _tail,
        seize,
        _status,
    ):
        bs = app.atproto_settings

        # ── The box seizes the identity for real: the bridge builds an update op
        # over the standing head with `rotationKeys[0]` replaced, and signs it
        # with the junior key the genesis lists. The genesis stays verifiable, so
        # the violation is at standing index 1 — the contestable shape. ──
        hostile_cid = seize(EVIL_SENIOR)
        assert _poll_alarm_state(app, want_visible=True), (
            "a seized identity must raise the custody alarm; log:\n" + _tail()
        )
        assert directory.standing_rotation_keys(did)[0] == EVIL_SENIOR, (
            "precondition: the world currently believes the box's key is senior"
        )

        # ── The remedy is on the page the alarm points at, ABOVE the selector. ──
        assert _poll_contest_card_visible(app, bs), (
            "the contest card must render on a standing custody violation; "
            f"page error={bs.page_error_text()!r}"
        )
        assert bs.contest_state() == "contestable", (
            "a violation above the genesis, over a log that authenticates, is "
            f"contestable — a fork point exists; state={bs.contest_state()!r}"
        )
        detail = bs.contest_detail()
        # the copy must not let the user believe undoing
        # this evicts the box entirely. It keeps the key it publishes with.
        assert "still post as you" in detail, (
            "the detail must name what undoing does NOT restore: " f"{detail!r}"
        )

        # ── The ceremony: open, read, confirm. ──
        bs.open_contest()
        assert bs.is_contest_confirm_visible(), (
            "the contest button opens the confirm card"
        )
        bs.confirm_contest()

        # ── The directory's own evidence, not ours: the fork was accepted and
        # the hostile op is no longer standing. ──
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not directory.forks:
            time.sleep(0.5)  # sleep-ok: pacing between poll iterations
        assert directory.forks, (
            "the client must submit a recovery fork to the directory; page "
            f"error={bs.page_error_text()!r}\nbridge log:\n{_tail()}"
        )
        _fork_did, fork_point, nullified = directory.forks[0]
        assert hostile_cid in nullified, (
            f"the fork must nullify the op it displaces: {nullified!r} "
            f"(seizure cid {hostile_cid!r})"
        )
        assert fork_point != hostile_cid, "a fork chains to the op BEFORE the violation"
        assert directory.standing_rotation_keys(did)[0] == user_key, (
            "after the fork the user's own key is senior again — the whole point"
        )

        # ── And the alarm comes down off that evidence, with no new mechanism. ──
        assert _poll_alarm_state(app, want_visible=False), (
            "a successful contest must clear the custody alarm on the next "
            f"convergence; page error={bs.page_error_text()!r}"
        )


@pytest.mark.parametrize("custody_app", _apps(*CONTEST_APPS))
@pytest.mark.feature("atproto")
def test_a_seizure_past_its_window_says_so_and_offers_no_button(
    custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """When an attempt to take the identity can no longer be undone, the page
    says so plainly instead of offering a button that does nothing
    (`atproto-identity-custody.md` § The 72 h recovery-fork contest; catalog
    `atproto` 36).

    The seizure is the same real bridge-signed rotation the contest test drives;
    the fake directory then serves its `createdAt` 73 h in the past
    (`FakePlcDirectory.backdate`), the shape PLC itself would refuse a fork
    over. The card must render the `window-closed` state with its explanation,
    and the `atproto-contest` button must be absent — a live button there would
    sign a fork the directory is bound to reject. Nothing is submitted.

    tui first, the lead app; the other apps gain their parametrization in the
    cross-app lift (the ceremony itself is on all seven, `CONTEST_APPS`)."""
    with _minted_identity(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as (
        app,
        directory,
        did,
        _user_key,
        _tail,
        seize,
        _status,
    ):
        bs = app.atproto_settings
        hostile_cid = seize(EVIL_SENIOR)
        directory.backdate(did, hostile_cid, 73 * 60 * 60)
        assert _poll_alarm_state(app, want_visible=True), (
            "a seized identity must raise the custody alarm; log:\n" + _tail()
        )
        assert _poll_contest_card_visible(app, bs), (
            "the contest card must render on a standing custody violation, "
            f"even one past its window; page error={bs.page_error_text()!r}"
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and bs.contest_state() != "window-closed":
            time.sleep(0.5)  # sleep-ok: pacing between poll iterations
            bs.navigate()
        assert bs.contest_state() == "window-closed", (
            "a seizure older than PLC's 72 h recovery window is honestly "
            f"window-closed; state={bs.contest_state()!r} "
            f"detail={bs.contest_detail()!r}"
        )
        assert bs.contest_detail().strip(), (
            "the closed window must be explained in words, not a bare state"
        )
        assert app.driver.is_absent("atproto-contest"), (
            "no contest button may be offered once the window has closed: "
            f"{app.driver.diagnose('atproto-contest')}"
        )
        assert not directory.forks, (
            f"nothing may be submitted over a closed window: {directory.forks!r}"
        )


# The apps that render `atproto-delete-tombstone` today: tui leads (2026-09-26),
# macos + ios follow through the shared FaunaKit `AtprotoSettingsView`; linux,
# web and android landed in the batched trickle-down, and windows is the last
# app, so all seven render it.
RETIRE_APPS = ("tui", "macos", "ios", "linux", "web", "android", "windows")


@pytest.mark.parametrize("custody_app", _apps(*CUSTODY_APPS))
@pytest.mark.feature("atproto")
def test_retire_identity_opt_in_rides_the_delete_ceremony(
    custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    """S5 slice 5b's opt-in, through the app UI (`atproto-pds-bridge.md`
    § Disable & revocation layer 2; `atproto-delete-tombstone` user-approved
    2026-09-26).

    Needs a PUBLISHED did:plc, which is why it lives in this module's world
    (real bridge, genesis in the fake directory) rather than beside the plain
    delete ceremony in `test_atproto_settings.py`, whose nest never mints one.

    Pins what the ceremony owns: the tick is offered only on the delete card,
    unticked; ticking it replaces the "your identity is kept" promise with the
    cannot-be-undone line (never both); and confirming runs the sweep and THEN
    records the retirement intent nest-side — the durable half the client's
    converge pass acts on. The converge itself (sweep observed finished via the
    DID's PDS → tombstone signed with the held key) is pinned by the machine's
    retirement tests: the genesis this world publishes names the nest's public
    hostname as its PDS, which this box cannot reach, so the probe answers
    quiet-retry here by design.
    """
    if custody_app not in RETIRE_APPS:
        driver = create_driver(custody_app)
        skip_unbuilt(
            driver,
            surface="atproto-delete-tombstone",
            detail="the retire opt-in landed on tui (lead app) 2026-09-26, then macos + ios, linux, web and android",
            tracked="ui/atproto.md § Implementation status today; ",
        )

    with _minted_identity(
        custody_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
    ) as (
        app,
        _directory,
        did,
        _user_key,
        _tail,
        _seize,
        integration_status,
    ):
        bs = app.atproto_settings
        assert did.startswith("did:plc:"), f"precondition: a published did:plc, got {did!r}"
        # The fixture returns once the genesis reaches the directory, but the
        # page last converged when the identity was still `pending` with no
        # DID — where the machine rightly greys the opt-in as unpublished. Re-
        # open the page (the `connect_map` refresh) until nest reports it active.
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            _nav_cycle(app)
            if S.atproto_settings.identity_status_active in (bs.hosted_handle_text() or ""):
                break
            time.sleep(0.5)  # sleep-ok: pacing between poll iterations
        assert S.atproto_settings.identity_status_active in (bs.hosted_handle_text() or ""), (
            f"precondition: the page shows the minted identity active. "
            f"summary={bs.hosted_handle_text()!r} error={bs.page_error_text()!r}"
        )
        assert bs.is_delete_presence_visible(), "a standing presence offers the delete action"

        # Only inside the ceremony (§ Don't do these: never a step-down).
        assert not bs.is_retire_identity_visible(), (
            "the retire opt-in must not exist outside the delete ceremony"
        )
        bs.open_delete_confirm()
        assert bs.wait_for_delete_card(), f"the ceremony opens. error={bs.page_error_text()!r}"
        assert bs.is_retire_identity_visible(), (
            "the opt-in rides the delete card. diagnose: "
            f"{app.driver.diagnose('atproto-delete-tombstone')}"
        )
        assert bs.retire_identity_state() == "off", (
            f"never pre-ticked; state={bs.retire_identity_state()!r}"
        )

        _SENTINEL = "HANDLE-SENTINEL"

        def _tail_of(line: str) -> str:
            return line.split(_SENTINEL, 1)[1].lstrip()

        kept = _tail_of(S.atproto_settings.delete_confirm_identity_kept(handle=_SENTINEL))
        retired = _tail_of(S.atproto_settings.delete_confirm_identity_retired(handle=_SENTINEL))
        card = bs.delete_card_text()
        assert kept in card and retired not in card, f"unticked: the identity is kept. card={card!r}"

        bs.toggle_retire_identity()
        assert bs.wait_for_retire_identity_state("on"), (
            f"the tick takes. state={bs.retire_identity_state()!r} error={bs.page_error_text()!r}"
        )
        card = bs.delete_card_text()
        assert retired in card, f"ticked: the card must say it cannot be undone. card={card!r}"
        assert kept not in card, (
            "ticked: the card must no longer promise the identity survives — the "
            f"two lines are mutually exclusive. card={card!r}"
        )

        # Confirm: sweep first, then the durable intent.
        bs.confirm_delete()
        assert bs.wait_for_no_delete_card(timeout=20.0), (
            f"the ceremony completes. error={bs.page_error_text()!r}"
        )
        deadline = time.monotonic() + 20
        identity = {}
        while time.monotonic() < deadline:
            identity = integration_status().get("identity") or {}
            if identity.get("tombstone_requested"):
                break
            time.sleep(0.5)  # sleep-ok: pacing between poll iterations
        assert identity.get("status") in ("deleted", "tombstoned"), (
            f"the sweep ran first: the identity is recorded deleted. identity={identity!r}"
        )
        assert identity.get("tombstone_requested") or identity.get("status") == "tombstoned", (
            "confirming a ticked ceremony must record the retirement intent nest-side. "
            f"identity={identity!r} page error={bs.page_error_text()!r}"
        )
        assert bs.page_error_text() in ("", None), (
            f"a completed ceremony is not an error. error={bs.page_error_text()!r}"
        )
