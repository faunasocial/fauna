"""The W3 (account-data-plane.md § Workstreams) account runtime is **hosted by this app**, and pumps when elected.

`docs/goal/architecture/account-data-plane.md` § The account store → *The
client-side lifecycle*, and § Implementation status today → *Built — W3 the
second app host*.

**What this catches that nothing else can.** Every other proof of the account
runtime is a unit or a rust-side rig: the shared assembly's tier_1 pin the
fifteen params, and `conformance_account_runtime.rs` drives two runtimes over
real nest handlers. All of them *construct* the runtime. None can see an app
that wires the assembly and never calls it, calls it on a path that does not run
at login, or starts it into a state where no pass ever completes — the class of
gap that left every app but tui hosting no runtime at all until 2026-08-18 while
the plane looked built.

It is deliberately **app-agnostic**: every app that hosts the runtime answers
the same convention-11 legs, so this file is the trickle-down's acceptance test
rather than a per-app one. An app that has not built the host routes through
`skip_unbuilt` (convention 7 — the ratchet counts it, `--strict-app` fails it).

⚠ **What this file must never assume, because its first authoring pass did and
was deleted for it: that this app is the process that PUMPS.** W5.1's
engine-singleton election decides that, and on any desktop the co-located
`fauna-sync-agent` hosts the same store and may hold the lock. A non-holder runs
no pass by design, so its counters sit frozen at `(0, 0)` — a *correct* reading
for a healthy app. Measured 2026-08-18 in a single run, same code: linux reached
`(4, 3)` on one app instance while both linux and tui sat frozen on a later one.
So every assertion below is **gated on the role**, and the two roles get
different, both-meaningful assertions.

⚠ Never assert a wall-clock delay here (convention 14). The pump has a
30 s-class backstop ticker, so "sleep and hope" would be defunct by
construction; positive waits are named budgets + deadline polls.
"""

from __future__ import annotations

import json

import pytest

from helpers.waiting import (
    account_pump_cycles,
    account_runtime_role_or_skip,
    await_account_runtime_assembled,
    await_pump_cycle_after,
    poke_account_pump,
)

pytestmark = pytest.mark.tier_3

# One poked pass is local work plus, at most, the nest round trips a reconcile
# makes, on a box that may be running several other suites. Generous by design
# and far above any non-pathological delay — the assertion is "a pass
# completed", never "a pass completed quickly".
PUMP_PASS_S = 120

# 32 zero bytes: no enrolled device carries this id, so every app's device-set
# reader must read no row for it. Spelled as the id a journey would pass so a
# future session meeting it in a log knows a probe put it there.
UNKNOWN_DEVICE_ID_HEX = "00" * 32


def test_the_app_hosts_an_account_runtime_and_pumps_when_elected(logged_in_app, test_user):
    """The trickle-down's acceptance test: assembly, then the pump, one app.

    **Two phases in ONE test because the pump assertion needs the assembly this
    test just made**, not because a second test is unaffordable — see
    `test_the_runtime_comes_back_after_a_sign_out_and_sign_in` below, which
    deliberately spends the second login.

    ⚠ **An earlier version of this docstring justified the single test with a
    claim that is false: "each test in this file gets its own relaunched app
    instance".** It does not. `helpers/module_relaunch.at_module_boundary`
    relaunches at a **module** boundary only (`module_name == last` returns
    `None` within a file), and `reset()` is an in-process factory reset, never a
    relaunch (`drivers/http_bridge.py::reset` posts the `reset` command and
    waits for `authenticated == false`). So a second test here costs one
    reset + re-login on the SAME process — which is a property worth asserting,
    not a wasted instance.

    **Phase 1 is unconditional** — the app ends up hosting a runtime. That does
    not depend on the election: whether this process or the co-located agent
    ends up pumping, the app must have built and installed a runtime of its own,
    which is exactly what "app X hosts the W3 account runtime" means and what a
    leg for a new app has to deliver.

    **Phase 2 is gated on the role, and that gate is the point.** A non-holder
    runs no pass by design, so asserting a pass unconditionally would fail on a
    perfectly healthy app whenever the sync agent won the lock — a flaky test
    that reads as a product bug. Not-the-holder inverts to the property that IS
    guaranteed there: the runtime exists and runs nothing, and convergence
    arrives through the shared store.

    ⚠ Proves the pump **ran**, never what it did — a pass with nothing due
    completes identically. Effects belong to the tests that own them; what is
    asserted here is precisely what those tests must be able to assume.
    """
    app = logged_in_app
    account_runtime_role_or_skip(app.driver)
    runtime_up, is_holder = await_account_runtime_assembled(app.driver)
    assert runtime_up, "guarded by await_account_runtime_assembled above"

    # The store dir follows the writer key out of the platform's cloud backup
    # (`common.md` § Credential storage → *the store dir follows the row*,
    # 2026-08-26): on a phone the key is restore-excluded, so a store dir that
    # restored without it would strand the account plane on the new device.
    # Only a driver whose platform has an OBSERVABLE exclusion offers this read
    # (iOS: the `com_apple_backup_excludeItem` xattr on the assembled dir);
    # android's is a manifest rule its own `CloudBackupPostureTest` reads, and
    # the desktops state `NotApplicable` — nothing on disk to observe, so the
    # absence of the accessor is the platform's answer, not a skipped check.
    observe_exclusion = getattr(app.driver, "account_store_cloud_backup_excluded", None)
    if observe_exclusion is not None:
        excluded = observe_exclusion(test_user["actor_id_hex"])
        assert excluded is True, (
            "the assembled account-store dir must be excluded from the platform's "
            f"cloud backup (got {excluded!r}; None = the dir was not found where "
            "the driver expects it, False = assembled but NOT excluded — the "
            "shell arm did not run or did not land)"
        )

    if not is_holder:
        # The non-holder half of the contract. Not a skip: this is a real
        # assertion about a real, ratified state — the app hosts a runtime that
        # correctly declines to pump, and its counters stay honest rather than
        # inventing passes it never ran.
        started, completed = account_pump_cycles(app.driver)
        poke_account_pump(app.driver)
        assert account_pump_cycles(app.driver) == (started, completed), (
            "a non-holder must run NO pass, even when poked — the counters "
            "moving here would mean two co-located processes are pumping one "
            "store, which is the state the W5.1 election exists to prevent"
        )
        return

    started, _completed = account_pump_cycles(app.driver)
    poke_account_pump(app.driver)
    await_pump_cycle_after(
        app.driver,
        started,
        budget_s=PUMP_PASS_S,
        what="the account_pump_now poke on the elected runtime",
    )


def test_the_runtime_comes_back_after_a_sign_out_and_sign_in(logged_in_app):
    """A second sign-in **in one process** must assemble a runtime too.

    The `app` fixture resets before every test and `logged_in_app` signs in
    after it, so simply being the file's SECOND test makes this the sign-out →
    sign-in cycle: `reset` tears the account runtime down through the app's
    canonical one-list seam (linux `actor_scope::reset_actor_scoped_state`;
    the shared `install`/`teardown` pair bumps `INSTALL_GEN`), and the login
    that follows must build a fresh one.

    **Why this is a product property and not a harness detail.** A user who
    signs out and back in — or switches accounts — gets no app restart. If the
    runtime only ever assembles on the first authentication of a process, every
    preference surface silently falls back to the blob rail and the account's
    own scopes are walked by the agent alone, for the rest of that app run,
    with nothing on screen saying so. That is invisible to every unit test:
    the shared assembly's tier_1 construct a runtime, and
    `conformance_account_runtime.rs` drives two of them, but neither can see an
    app that installs on one code path and not on the other. It is exactly the
    gap the landing found in the first place, one login later.

    **This test is also the row's open question, made falsifiable.** The
    addendum records a measured, undiagnosed reading —
    a later app instance sitting at `role: (False, False)` for 240 s on tui and
    linux alike — and proposes "the relaunched instance never runs its
    post-auth hook" as the better-supported suspect. Whatever the cause, it has
    to show up here or on the first test; the two together separate *never
    assembles* from *assembles once per process*.
    """
    app = logged_in_app
    account_runtime_role_or_skip(app.driver)
    runtime_up, _is_holder = await_account_runtime_assembled(app.driver)
    assert runtime_up, (
        "the app assembled no account-store runtime on a SECOND sign-in in the "
        "same process, having assembled one on the first. The teardown ran "
        "(sign-out is what the fixture's reset does) but the re-install did "
        "not, or it failed: grep the app log for 'account runtime:' — "
        "'mounted the account store' (installed), 'assembly failed' (tried and "
        "lost), 'superseded during assembly' (a teardown landed mid-assembly), "
        "or none of them at all (the post-auth hook never fired on this path)"
    )


def _device_set_report(raw) -> dict:
    """The reader's answer as a dict, whichever way this app's bridge hands it
    back — a JSON *string* (tui stashes the reader's text) or an already-decoded
    object (windows' state blob types the slot as a JSON value). Same tolerance
    as `test_crash_recovery_journeys._device_set_state`; not pinned to one
    bridge's serialization. ``None`` is not coerced: it is the "reader unbuilt"
    signal and the caller asserts on it."""
    return json.loads(raw) if isinstance(raw, str) else raw


def test_the_device_set_reader_answers_a_report_on_every_app_hosting_the_runtime(logged_in_app):
    """The fleet-removal convergence reader is BUILT on this app: an id no device
    carries reads `{"found": false}`, never `None`.

    `docs/goal/architecture/account-data-taxonomy.md` § The generation machinery
    → *Fleet-scope reclamation*, clause (4): `device_set_state` is the e2e's
    only observation path over the `fauna.state.device-set` plane, and
    `test_crash_recovery_journeys._require_device_set_reader` treats **any
    non-`None` answer as "the reader is built"** — so an app that answers `None`
    turns that journey into a skip, which is not coverage (convention 7).

    **Why a standing test rather than the journey's own probe.** The journey is
    `killable_app`-gated, and windows supports no unclean kill, so the probe
    never reaches windows at all: the one app whose reader was missing was the
    one app the probe could not see. This asserts the reader's *presence* where
    it is observable, on every app that hosts the runtime, by driving the same
    command the journey does.

    What turns it red: an app's dispatcher losing its `device_set_state` arm
    (windows: `TestAgent.ProcessCommand` falling through to its `default:`
    refusal, which stashes no result), or the FFI wrapper it reads through being
    compiled out of the build under test.
    """
    app = logged_in_app
    account_runtime_role_or_skip(app.driver)
    # The reader asks the assembled runtime's store. Before assembly it still
    # answers `{"found": false}` (no store to read) — which is exactly the
    # answer asserted below, so without this barrier a green run would prove
    # nothing about the runtime-backed path.
    await_account_runtime_assembled(app.driver)

    raw = app.driver.call_command("device_set_state", {"device_id_hex": UNKNOWN_DEVICE_ID_HEX})

    assert raw is not None, (
        "this app hosts an account runtime but its `device_set_state` command "
        "answered nothing (the result slot is empty). A built reader always "
        "answers a JSON object — at minimum {\"found\": false} — so None means "
        "the dispatcher has no arm for it (the unknown-command refusal is on "
        f"`error-message`: {app.error_text()!r}) or the arm stashed no result. "
        "Wire it over the shared `fauna_client_account_runtime::device_set_state_json`."
    )
    assert _device_set_report(raw) == {"found": False}, (
        f"an id no device carries ({UNKNOWN_DEVICE_ID_HEX[:8]}…) must read "
        f"{{\"found\": false}} from the account store, got {raw!r}"
    )


@pytest.mark.windows
def test_the_device_set_reader_reports_not_found_before_any_sign_in(app):
    """With no connected client the reader REPORTS `{"found": false}`; it never
    answers `None`.

    `None` is the wire signal for "reader unbuilt" that
    `_require_device_set_reader` probes, so a signed-out app that answered it
    would read as a missing reader. Apple pins the same rule in
    `DeviceSetStateTestCommandTests.swift`; windows can have no such unit twin
    (`FaunaApp.Tests` references `FaunaApp.Core` only, so neither `App` nor
    `TestAgent` is reachable), which is why it is pinned here at e2e level.

    What turns it red: the arm answering the wrapper's `null` (no connected
    client) straight through instead of reporting not-found.
    """
    raw = app.driver.call_command("device_set_state", {"device_id_hex": UNKNOWN_DEVICE_ID_HEX})

    assert raw is not None, (
        "a signed-out app answered `device_set_state` with nothing. No connected "
        "client is a quiet not-found REPORT, not an absence — None means "
        "\"reader unbuilt\" to the journey's probe."
    )
    assert _device_set_report(raw) == {"found": False}, f"got {raw!r}"


@pytest.mark.windows
@pytest.mark.parametrize(
    "payload",
    [{}, {"device_id_hex": 7}],
    ids=["missing-id", "id-not-a-string"],
)
def test_a_device_set_state_command_without_a_string_device_id_is_refused_loudly(app, payload):
    """A missing or mistyped `device_id_hex` is a loud refusal that NAMES the
    field, and leaves the result slot empty — never folded into `not found`.

    Convention 11's bad-payload clause. A silent not-found would make every
    journey assertion of the form "the row is NOT there" pass on a typo'd key:
    a vacuous green. Apple's `DeviceSetStateTestCommand` refuses the same way.

    ⚠ **The assertion must name the field, not merely the command.** Before the
    arm exists the dispatcher's `default:` refusal already names
    `device_set_state` ("unknown command: device_set_state"), so a bare
    command-name check would pass against an app with no reader at all. Only a
    refusal from the arm itself talks about `device_id_hex`.
    """
    assert not app.has_error(), (
        "a pre-existing error would make the read below ambiguous. "
        f"Current text: {app.error_text()!r}"
    )

    raw = app.driver.call_command("device_set_state", payload)

    assert raw is None, (
        f"a refused command must leave the result slot empty, got {raw!r} — "
        "a value there is a stale neighbour's answer or a not-found report "
        "standing in for a refusal"
    )
    refusal = app.error_text()
    assert "device_id_hex" in refusal, (
        f"payload {payload!r} was not refused on the field it got wrong. Got "
        f"{refusal!r}; the arm must say it needs a `device_id_hex` string."
    )
