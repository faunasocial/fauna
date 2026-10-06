r"""tier_3 e2e (real, ISOLATED dev-build sync agent): windows unprovision-on-teardown glue proof.

The consequence-1 teardown glue (`App.xaml.cs` `SignOutHandler`/`SwitchAccountHandler`
-> `CapabilityProvisioner.UnprovisionAsync` -> `UnprovisionCapability` over the pipe,
landed) is verified only by compile + the switcher tier_3 staying green
(`test_account_switcher_windows.py`) — but that suite runs with `HydrationSessionEnabled`
false (no `FAUNA_E2E_REAL_SYNC_AGENT`), so it never exercises the new path at all. The
two mechanism halves ARE tested elsewhere (`DagCborProvisionTests` for the wire
round-trip; `fauna-sync-agent::pipe_server::capability_persists_reloads_and_unprovisions`
for the in-process state machine) — this is the end-to-end GLUE PROOF only: a real
`fauna-sync-agent.exe`, driven by the real app over a real named pipe, actually gets
unprovisioned when the app switches accounts or signs out.

`docs/goal/architecture/apps/windows.md` § On-demand hydration host +
`long-term-store.md` § Multi-account evolution (consequence 1).

**⚠ The switch is driven by a SWITCHER ROW TAP, not by append-mode "Add account".**
Both reach the same `SwitchAccountHandler`, but only the row tap can assert what
happens *after* the switch in this harness, and the reason is load-bearing enough to
state here so nobody "simplifies" it back:

  `nest_instance` serves **plain HTTP** (`serve_tls=False` — `conftest.py::_make_nest`),
  so a seeded account's `nest_url` is `http://127.0.0.1:<port>`. The append wizard does
  not use the seeded URL: on `AlreadyOnNest` it *derives* the incoming account's
  `nest_url` from the typed handle's domain via
  `fauna_provisioning::probe::resolve_handle_domain_with_local_port`, which is
  **uniform-https by ratified design** (`probe.rs` — "an explicit port is honored for
  EVERY host-class — uniform https"; Pillar C, `730303718`). So an appended account is
  persisted at `https://localhost:<port>`, the post-switch `LaunchMachine.Start()`
  silent-challenge dials `wss://` at a plain-HTTP nest, rustls rejects the reply
  (`ws connect: IO error: received corrupt message of type InvalidContentType`), the
  launch lands `Offline(transient)` instead of `Online`, and `StartMainAppAsync` ->
  `StartHydrationSession` is never reached — so the incoming identity is never
  re-provisioned and this test's step (2) hangs forever on a symptom that looks exactly
  like a product bug in the teardown glue. It is not: it is the harness's plain-HTTP
  nest meeting the uniform-https derivation. (`test_account_switcher_windows.py`'s
  append test does not notice, because it asserts only the registry index, never that
  the switched-to session reaches `Online`.)

  ⚠ **UPDATED 2026-08-14 — the answer is NOT a `serve_tls=True` nest.** This paragraph
  used to send readers to one (`cross_nest_foreign`'s flag), and that prescription is
  superseded: the sanctioned harness answer to uniform-https is the
  `provider_base_urls["nest"]` override seam, named as such in the resolution's own
  comment (`probe.rs` — "the tier_3 e2e harness reaches a plain-HTTP test nest through
  the `provider_base_urls["nest"]` override seam … while `state.nest_url` records this
  resolved https URL"), and since 2026-08-13 it mirrors into the process-global
  **store-read** dial (`fauna_launch_machine::dial`) — the exact leg an append-entered
  switch takes. One `driver.set_provider_base_urls({"nest": nest["url"]})` therefore
  proves the append-entered switch reaches a working session, with no TLS nest and no
  `localhost`-vs-`127.0.0.1` authority mismatch to reconcile. Proven on tui + linux
  (`test_account_switcher_{tui,linux}.py`'s append journeys);
  the windows leg of that lift is captured elsewhere. It is
  still not this test's subject: the row tap exercises the identical
  `SwitchAccountHandler` with both accounts on the harness's own URL, which is what
  makes the teardown/rebuild observable at all.

**Two observables, deliberately different** (both already wired, nothing new added):

* Switch: `SwitchAccountHandler` step 4 AWAITS the unprovision before step 5
  re-provisions under the incoming identity — so the agent's capability slot is
  cleared for an interval too short to reliably catch by polling a live status (no
  sync folders are bound in this test, so `reconcile_engines` has nothing to tear
  down and the round-trip is fast). Instead this reads the agent's own log line
  (`tracing::info!("capability un-provisioned; stopping engines")`,
  `pipe_server.rs::handle_unprovision_capability`) from the byte offset recorded
  just before the switch-triggering click — a durable record immune to the timing
  race, and offset-scoped so the switch's unprovision can't be confused with the
  sign-out's later one.
* Sign-out: `SignOutHandler` fires unprovision fire-and-forget and stops the hydration
  loop — the cleared state is DURABLE, so a live poll of `GetServiceStatus` ->
  `ServiceStatusInfo.connection` (`Connected` iff `state.capability.is_some()` —
  `pipe_server.rs::handle_get_service_status`) is reliable: it settles to
  `Disconnected`, and the "never goes back" half is anchored to the
  `data.sync.agent_install_in_flight` causal barrier rather than a settle window (see
  the assertion's own note — this is where row 57 hid).

**⚠ WHAT AN EMPTY SWITCH DELTA MEANS — measured 2026-09-11, read this before
re-diagnosing one.** This test red'd twice with the
switch delta completely EMPTY — not merely missing the teardown line, but carrying
nothing at all — and the obvious readings are all wrong:

* **It is not the old assertion racing a slow-but-correct teardown.** That was the
  leading hypothesis and it is REFUTED by measurement: instrumented at the exact
  instant the old single-read assertion fired, both the teardown AND the
  re-provision line were already present (delta 971 bytes). The activation flips
  `session.actor_id` at `App.xaml.cs`'s `_cryptoService.LoadFromSecret`, which is
  *after* the awaited `UnprovisionSyncAgentAsync`, so the state wait this test
  gates on cannot return before the push has been made.
* **It is not a feature-gated log line.** `pipe_server.rs::unprovision_now`'s
  `tracing::info!` sits outside that function's only `cfg`/`is_production` gate,
  and `fauna_log` defaults to `info` on an unbuffered stderr layer.

What is left is that the push genuinely did not land — and until this pass every
one of the three ways that happens was SILENT, which is why two reproductions
produced no diagnosis between them. All three now announce themselves:
`SyncAgentSessionHost.UnprovisionAsync` logs whether it held a live session,
`SyncAgentSession.UnprovisionAsync` logs a teardown another cause already claimed,
and shared Rust's `SyncAgentProvisioner::unprovision` logs the push's own outcome
(delivered / unreachable / refused) instead of discarding it. The prime suspect is
the last one under load: the push is best-effort by design and a lost message is
*anticipated* by `on-demand-files.md` § Multi-account × File Provider ("is asked to
unprovision, and made to reconcile if the asking fails"), with the signed-out
marker as the backstop — so a recurrence is a load artifact to be read off the app
log, not a mystery to re-instrument. Both reds were at 06:43 inside/behind a
19-file batch; 4 later runs on a quiet box are green.

**Isolation (testing.md § conventions point 10).** This run spawns its OWN
`fauna-sync-agent.exe` on a per-session pipe + a fresh `--data-dir`
(`helpers.windows_sync_agent.running_agent` — the one sanctioned spawner) and points
the app at it with `FAUNA_E2E_SYNC_PIPE`. It therefore never touches the machine-global
per-SID pipe `\\.\pipe\fauna-sync.<SID>`, so the box's INSTALLED product (or a sibling
checkout's agent) can keep running alongside it — the contamination hazard that
previously made this module demand an exclusive box is structurally gone, which is why
there is no foreign-agent guard here any more.

Both env vars are passed directly in the launch config rather than via the
`real_sync_agent` / `isolated_sync_agent` marker plumbing, because this module drives a
direct `create_driver("windows")` + `driver.launch(...)` (like
`test_account_switcher_windows.py`) rather than the `app`/`logged_in_app` fixtures that
route through `_build_app_config`. The markers are still declared for registry
consistency (pytest.ini) and because `sync_agent_binary` is shared with the other
real-agent suites.
"""
from __future__ import annotations

import time

import pytest

from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.sync_agent_ipc import SyncAgentClient
from helpers.windows_sync_agent import running_agent

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
]

# tests/e2e-unified/ui.yaml § settings (account).
ACCOUNT_PAGE_NAV = {
    "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]}
}
SWITCHER_LIST = "account-switcher-list"
SWITCHER_ITEM = "account-switcher-item"
ACTIVE_INDICATOR = "account-item-active-indicator"
SIGN_OUT_BUTTON = "sign-out-button"
SIGN_OUT_CONFIRM_BUTTON = "sign-out-confirm-button"
CREATE_IDENTITY_BUTTON = "create-identity-button"

UNPROVISION_LOG_LINE = "capability un-provisioned; stopping engines"
#: `pipe_server.rs::handle_provision_capability`'s own line, emitted
#: unconditionally once a capability reaches the slot. Read as the POSITIVE
#: re-provision signal after the switch — see `_wait_log_line`'s note on why
#: `connection == "Connected"` cannot serve as one.
PROVISION_LOG_LINE = "on-demand hydration capability provisioned"


def _seed_two_accounts(nest_instance):
    """A regular-user account (row 0, active) + the nest's claimed-admin account
    (row 1), both on the harness's own plain-HTTP ``nest_instance["url"]``.

    Duplicated from ``test_account_switcher_windows.py`` rather than cross-imported:
    no e2e test module imports another today, and this is the small-helper half of
    that module's own "import the module-level helpers directly, or duplicate the
    small ones" note. Add order == display order.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_actor = bytes(admin_sk.verify_key).hex()
    admin_secret = bytes(admin_sk).hex()

    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()

    url = nest_instance["url"]
    seed = build_registry_seed(
        [
            {"actor_id": user_actor, "secret_hex": user_secret, "nest_url": url,
             "device_id": "unprov-user", "handle": "user"},
            {"actor_id": admin_actor, "secret_hex": admin_secret, "nest_url": url,
             "device_id": "unprov-admin", "handle": "admin"},
        ],
        active=user_actor,
    )
    return seed, user_actor, admin_actor


def _wait_switcher_ready(driver, n, timeout=45):
    """Navigate to Settings → Account and poll until the switcher lists ``n`` rows
    with one active, RE-NAVIGATING each cycle (a just-rebuilt frame can drop a
    single fire-and-wait nav — same reason as the switcher module's twin)."""
    deadline = time.monotonic() + timeout
    last_error = "none (loop never executed — timeout <= 0?)"
    while time.monotonic() < deadline:
        try:
            driver.set_state(ACCOUNT_PAGE_NAV)
            driver.wait_for(SWITCHER_LIST, timeout=5)
            rows = driver.count(SWITCHER_ITEM)
            if rows == n and driver.count(ACTIVE_INDICATOR) == 1:
                return
            last_error = (
                f"'{SWITCHER_LIST}' realized, but {SWITCHER_ITEM} count={rows} (want {n})"
            )
        except Exception as e:
            last_error = f"{type(e).__name__}: {e}"
        time.sleep(0.5)

    try:
        app_error = driver.get_text("error-message")
    except Exception as e:
        app_error = f"(error-message unreadable: {type(e).__name__}: {e})"
    raise AssertionError(
        f"switcher never reached {n} accounts (one active) within {timeout}s.\n"
        f"  last step failure: {last_error}\n"
        f"  app error-message: {app_error!r}"
    )


def _wait_connection(client: SyncAgentClient, want: str, timeout: float = 60.0) -> dict:
    """Poll GetServiceStatus until `connection` reads `want` ("Connected" /
    "Disconnected"). Raises with the last-seen status for a self-diagnosing
    failure (testing.md point 6)."""
    deadline = time.monotonic() + timeout
    last: dict = {}
    while time.monotonic() < deadline:
        try:
            last = client.service_status()
            if last.get("connection") == want:
                return last
        except OSError:
            pass  # agent mid-restart / pipe momentarily unavailable
        time.sleep(0.5)
    raise AssertionError(
        f"connection never reached {want!r} within {timeout:.0f}s; last status={last!r}"
    )


def _read_log_delta(log_path, offset: int) -> str:
    """The agent's stderr capture since ``offset``. ``running_agent`` redirects the
    agent's stderr here, and `fauna_log::init` writes every `tracing` event to BOTH
    stderr and `<data_dir>/logs/` — so this capture carries the provisioning lines
    without needing to glob the rolling file."""
    with open(log_path, "r", errors="replace") as f:
        f.seek(offset)
        return f.read()


def _wait_log_line(log_path, offset: int, needle: str, timeout: float) -> str:
    """Deadline-poll the agent's log delta since ``offset`` until ``needle`` appears;
    return the delta. Raises with the whole delta on timeout (point 6).

    **Why a wait and not a bare read of the delta** (and why this replaced a
    `_wait_connection(client, "Connected")` on the re-provision half): the
    agent's `connection` field is `Connected` iff *some* capability sits in the
    slot (`pipe_server.rs::handle_get_service_status` — "a provisioned capability
    IS the connected signal"), and the slot is single. Across a switch the
    OUTGOING account's capability satisfies that predicate perfectly, so polling
    it proves nothing about the incoming one: a switch that tore nothing down and
    re-provisioned nothing passes it instantly, and then the real assertion below
    reads an EMPTY delta with no explanation of which half went missing. The
    agent's own lines are per-EVENT and offset-scoped, so waiting on them is the
    only reading of this window that can tell "never happened" from "not yet".
    Latency-independent by construction (a durable record + a generous deadline,
    e2e-conventions.md § point 14) — no settle window anywhere.
    """
    deadline = time.monotonic() + timeout
    delta = ""
    while time.monotonic() < deadline:
        delta = _read_log_delta(log_path, offset)
        if needle in delta:
            return delta
        time.sleep(0.25)
    raise AssertionError(
        f"the agent never logged {needle!r} within {timeout:.0f}s of the offset; "
        f"log delta:\n{delta[-4000:]}"
    )


def _wait_log_line_after(log_path, offset: int, first: str, second: str, timeout: float) -> str:
    """Like `_wait_log_line`, but ``second`` must appear AFTER ``first`` in the
    same delta — an ORDERING assertion, which is what consequence 1 actually says.

    Deliberately no byte arithmetic: the caller's ``offset`` comes from
    ``stat().st_size`` (bytes) while the delta is decoded TEXT, and on Windows
    CRLF translation makes those two coordinate systems disagree — so deriving "a
    byte offset just past ``first``" from a character index would drift, and drift
    EARLIER, quietly re-admitting a pre-teardown provision as if it proved the
    rebuild. Splitting the decoded text needs no coordinates at all.
    """
    deadline = time.monotonic() + timeout
    delta = ""
    while time.monotonic() < deadline:
        delta = _read_log_delta(log_path, offset)
        _, sep, tail = delta.partition(first)
        if sep and second in tail:
            return delta
        time.sleep(0.25)
    raise AssertionError(
        f"the agent never logged {second!r} after {first!r} within {timeout:.0f}s; "
        f"log delta:\n{delta[-4000:]}"
    )


def test_windows_unprovision_on_switch_and_signout(
    nest_instance,
    windows_app_path,
    sync_agent_binary,
    isolated_sync_agent_pipe_name,
    tmp_path,
    request,
):
    seed, _user_actor, admin_actor = _seed_two_accounts(nest_instance)

    pipe_path = r"\\.\pipe\{}".format(isolated_sync_agent_pipe_name)
    data_dir = tmp_path / "isolated-sync-agent-data"
    data_dir.mkdir()
    log_path = tmp_path / "isolated-sync-agent.log"
    # The APP's own shell log, teed into one thread-tagged file
    # (`FaunaApp.Core/Logs/E2eTrace.cs`, via `FAUNA_E2E_AGENT_LOG` below) and
    # folded into every failure here by `_app_trace`.
    #
    # Load-bearing, not a convenience: what the agent's log does NOT say cannot
    # distinguish the three ways this teardown goes silent, and all three are
    # app-side. `SyncAgentSessionHost.UnprovisionAsync` no-ops when it holds no
    # published session; `SyncAgentSession.UnprovisionAsync` no-ops when a prior
    # `Stop()` already claimed the teardown; and `_provisioner.Unprovision()`
    # swallows its own failure into a `ShellLog.Warn`. Only the app log separates
    # them — and this module drives a direct `create_driver`/`launch` rather than
    # the `app` fixture, so nothing else here would dump it.
    app_trace_path = tmp_path / "app-shell.log"

    def _app_trace() -> str:
        if not app_trace_path.exists():
            return "\n── app shell log: ABSENT (FAUNA_E2E_AGENT_LOG never written) ──\n"
        return (
            "\n── app shell log (windows, FAUNA_E2E_AGENT_LOG), tail ──\n"
            + app_trace_path.read_text(errors="replace")[-6000:]
        )

    # The agent must already be serving before the app's first hydration tick.
    #
    # ⚠ UPDATED: this used to say "SpawnSyncAgentDetached does NOT honour
    # FAUNA_E2E_SYNC_PIPE, so an app that probes first would spawn onto the box's
    # real per-SID pipe". Both halves of that premise are gone. The C# spawner was
    # lifted into shared Rust (`fauna_client_sync::agent_spawner`), and BOTH sides
    # of the override are honoured in a test-capable build — the connect side
    # (`fauna_ipc::endpoint::e2e_pipe_override`) and the spawn side
    # (`agent_spawner::pinned_from_env`), each `#[cfg]`-gated out of a shipping
    # `--release` build because the gate, not the env read, is the security
    # boundary (convention 15). So an app that probed first would now find/spawn on
    # the SESSION pipe, not the per-SID one.
    #
    # Pre-spawning is still right, for a different reason: an app-spawned agent
    # would pick its own `--data-dir` and its own stderr, so this test would have
    # no log to read the teardown out of — and the log IS the observable here.
    with running_agent(sync_agent_binary, pipe_path, data_dir, log_path):
        driver = create_driver("windows")
        driver.launch({
            "app_path": windows_app_path,
            "url": nest_instance["url"],
            "seed_credentials": seed,
            "environment": {
                **_seeded_environment(request, nest_instance),
                "FAUNA_E2E_REAL_SYNC_AGENT": "1",
                "FAUNA_E2E_SYNC_PIPE": isolated_sync_agent_pipe_name,
                "FAUNA_E2E_AGENT_LOG": str(app_trace_path),
            },
        })
        client = SyncAgentClient(pipe_path, timeout=30)
        try:
            driver.wait_for_state(
                lambda s: bool(s.get("session", {}).get("authenticated")), timeout=60
            )

            # (1) Login provisions the isolated agent — HydrationSessionService's
            # first tick finds the pipe already served and pushes a capability.
            client.wait_for_pipe()
            _wait_connection(client, "Connected", timeout=60)

            # (2) SWITCH: tap the admin row. Record the log offset RIGHT BEFORE the
            # click — the unprovision->reprovision window is too fast to catch by
            # polling live status, so the durable log line is the observable.
            _wait_switcher_ready(driver, 2, timeout=45)
            offset_before_switch = log_path.stat().st_size
            driver.click(SWITCHER_ITEM, index=1)

            driver.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == admin_actor,
                timeout=60,
            )

            # THE TEARDOWN HALF. Wait on the agent's own line rather than reading
            # the delta once: the switch's unprovision is fire-then-await across a
            # process boundary, so a bare read can lose to it and report "never"
            # for "not yet" (`_wait_log_line`'s note).
            try:
                switch_log_delta = _wait_log_line(
                    log_path, offset_before_switch, UNPROVISION_LOG_LINE, timeout=60
                )
            except AssertionError as e:
                raise AssertionError(
                    "the switch must unprovision the OLD identity's capability "
                    "before re-provisioning under the new one "
                    "(on-demand-files.md § Multi-account × File Provider, "
                    f"consequence 1).\n{e}\n{_app_trace()}"
                ) from None

            # THE REBUILD HALF, and it is NOT `connection == "Connected"`: the slot
            # is single, so the outgoing account's own capability answers that
            # predicate and a switch that did nothing at all would pass it (see
            # `_wait_log_line`). The positive signal is the agent logging a
            # provision AFTER the offset — and after the teardown line, which the
            # slice below anchors, since consequence-1 ordering is the subject.
            try:
                _wait_log_line_after(
                    log_path,
                    offset_before_switch,
                    UNPROVISION_LOG_LINE,
                    PROVISION_LOG_LINE,
                    timeout=60,
                )
            except AssertionError as e:
                raise AssertionError(
                    "the switch tore the OLD identity's capability down but never "
                    "re-provisioned under the INCOMING one — the agent is left "
                    f"serving nobody.\n{e}\n{_app_trace()}"
                ) from None

            # (3) SIGN-OUT: durable clear, never re-provisions after.
            offset_before_signout = log_path.stat().st_size
            driver.set_state(ACCOUNT_PAGE_NAV)
            driver.wait_for(SIGN_OUT_BUTTON, timeout=15)
            driver.click(SIGN_OUT_BUTTON)
            driver.wait_for(SIGN_OUT_CONFIRM_BUTTON, timeout=10)
            driver.click(SIGN_OUT_CONFIRM_BUTTON)
            driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=30)

            _wait_connection(client, "Disconnected", timeout=30)

            # CAUSAL BARRIER, not a settle window (e2e-conventions.md § point 14).
            #
            # The only thing that can re-provision after sign-out is a session install
            # still in flight: the build takes ~10 s of nest round-trips, and until it
            # finishes it may still reach the agent. So wait for that to DRAIN, then
            # assert — the assertion is then about state ("no install can still
            # provision"), not about elapsed time.
            #
            # ⚠ This assert used to be `time.sleep(5.0)` + re-read, which is why row 57
            # survived: it fails only when the re-provision happens to land inside those
            # five seconds. On the tree that found the bug it went 1 fail then 1 pass with
            # no code change, and an earlier session recorded it green. A wall-clock bet on
            # a real defect is the worst pairing there is — green means nothing. Do not
            # reintroduce a sleep here.
            # Check the barrier is actually WIRED before relying on it. A missing key
            # would make the wait below pass instantly (and a `.get(..., False)` would
            # hide that), silently degrading this back to the un-anchored assert it
            # replaces — a barrier that always succeeds is worse than none, because it
            # reads as coverage.
            sync_block = driver.get_state().get("data", {}).get("sync") or {}
            assert "agent_install_in_flight" in sync_block, (
                "data.sync.agent_install_in_flight is missing, so the causal barrier "
                "below is not wired and would pass vacuously — see "
                f"AppDataSnapshot.GetSyncForState. sync block was {sync_block!r}"
            )

            # Direct indexing, not .get(): if the key vanishes later this must raise,
            # never quietly default to "no install in flight".
            driver.wait_for_state(
                lambda s: not s["data"]["sync"]["agent_install_in_flight"],
                timeout=60,
            )

            status = client.service_status()
            assert status.get("connection") == "Disconnected", (
                f"the agent is STILL PROVISIONED after sign-out, with no install left in "
                f"flight to explain it — a signed-out account is being served "
                f"(on-demand-files.md § Multi-account × File Provider, consequence 1); "
                f"status={status!r}\n"
                f"agent log since sign-out:\n"
                f"{_read_log_delta(log_path, offset_before_signout)[-4000:]}"
                f"{_app_trace()}"
            )
        finally:
            driver.teardown()
