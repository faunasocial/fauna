"""tier_1 pins for "this iOS simulator is usable" — no simulator, no app, no nest.

`simctl list devices` reports a device `Booted` at the START of its boot, not the
end. Measured on this box 2026-09-22 against a freshly created ephemeral device:
`list devices` said `Booted` after **1.1 s**, and `simctl bootstatus -b` returned
only after **513 s**. In that 512-second window the device is up in name and its
app-layer services (installd, FrontBoard) are not, so every `simctl` call
`launch()` makes next either blocks to its ceiling — `terminate` 120 s,
`uninstall` 120 s, `install` 300 s, ≈540 s of nothing — or is refused with
`Application "social.fauna.fauna" is unknown to FrontBoard`.

That is what the driver used to do, and the shape of the resulting failure is
why it went misdiagnosed for so long: only the FIRST test of an invocation dies,
because the boot finishes while that test is failing, so every later test passes
and the daemon looks like it wedged and healed itself. `_run_simctl`'s own
timeout message asserted exactly that — *"the simulator is wedged, not slow"* —
and sent the reader to `xcrun simctl shutdown all`, a machine-wide cure that
would drop a sibling session's simulator (convention 9's process-safety rule)
for a daemon that was never wedged: it was never asked to finish booting.

So the pins here are about the PRECONDITION, not about recovery:

  * `boot_and_wait_until_usable` waits on `bootstatus -b`, the one call that
    waits for boot completion — and a stub that only ever reports the *word*
    `Booted` must not satisfy it. That is the regression that would reintroduce
    the whole defect, and it is invisible in a green suite, because the harm
    lands in exactly one test per invocation.
  * every call it makes stays BOUNDED (convention 9: the harness bounds every
    wait), and `bootstatus` gets its own ceiling, since it is legitimately the
    longest wait in the driver.
  * a device that never finishes booting FAILS HERE, loudly, naming the device —
    rather than falling through to be an anonymous timeout nine minutes later in
    somebody else's `simctl` call (convention 11: an illegal state is refused,
    never half-honoured).
"""

from __future__ import annotations

import os
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import ios as ios_driver  # noqa: E402

pytestmark = pytest.mark.tier_1

_UDID = "11111111-2222-3333-4444-555555555555"


class _Calls(list):
    """Every `simctl` invocation, as (subcommand-ish args, timeout)."""

    def names(self):
        # `xcrun simctl <verb> ...` → the verb
        return [a[2] for a, _kw in self]

    def timeout_for(self, verb):
        for args, kw in self:
            if args[2] == verb:
                return kw["timeout"]
        raise AssertionError(f"no {verb!r} call in {self.names()}")


@pytest.fixture
def simctl(monkeypatch):
    """Stub `_run_simctl`, recording every call. Nothing here touches a real
    simulator — a proof about the boot precondition must not need one, or it
    could only ever run on one box."""
    calls = _Calls()
    outcomes: dict[str, subprocess.CompletedProcess] = {}

    def fake(args, **kw):
        calls.append((args, kw))
        verb = args[2]
        return outcomes.get(
            verb, subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        )

    monkeypatch.setattr(ios_driver, "_run_simctl", fake)
    calls.outcomes = outcomes
    return calls


def test_a_usable_device_is_one_that_finished_booting(simctl):
    ios_driver.boot_and_wait_until_usable(_UDID)

    assert simctl.names() == ["boot", "bootstatus"], (
        "the precondition is `bootstatus -b` — the only simctl call that waits "
        "for boot COMPLETION"
    )
    args, _ = simctl[1]
    assert args == ["xcrun", "simctl", "bootstatus", _UDID, "-b"], (
        "-b is what makes bootstatus wait for the boot instead of reporting on it"
    )


def test_the_word_booted_does_not_satisfy_the_precondition(simctl):
    """The regression guard, and the whole reason this file exists: a device
    that merely *lists* as Booted must not be accepted. Restoring the old
    `simctl list devices` poll — whose stub answer below is what that poll
    keyed on — turns this red, because no `bootstatus` call is made."""
    simctl.outcomes["list devices"] = subprocess.CompletedProcess(
        [], 0, stdout=f"    iPhone 17 Pro ({_UDID}) (Booted)", stderr=""
    )

    ios_driver.boot_and_wait_until_usable(_UDID)

    # Keyed on the whole argv, never on a parsed "verb": `simctl list devices`
    # spells its sub-command in TWO words, so a check reading `args[2]` alone
    # compares against `"list"` and can never fire — a pin that cannot go red is
    # worse than no pin, since it reads as coverage.
    polled = [args for args, _kw in simctl if "list" in args]
    assert not polled, (
        f"a `list devices` poll ({polled}) reports the START of the boot "
        "(measured: 1.1 s in, against 513 s to finish), so accepting it is the "
        "512-second window in which every later simctl call blocks on a device "
        "that is not up"
    )


def test_every_call_is_bounded(simctl):
    ios_driver.boot_and_wait_until_usable(_UDID)

    for args, kw in simctl:
        assert kw.get("timeout"), (
            f"unbounded simctl call {args[2]!r} — convention 9: the harness "
            "bounds every wait, and a bare `bootstatus -b` waits forever"
        )


def test_bootstatus_gets_its_own_ceiling_longer_than_boots(simctl):
    """Finishing a boot is legitimately much longer than `boot` returning, so it
    cannot share `boot`'s ceiling — 513 s measured against a 300 s `_SIMCTL_BOOT_S`
    would have failed a device that was perfectly healthy."""
    ios_driver.boot_and_wait_until_usable(_UDID)

    assert simctl.timeout_for("bootstatus") == ios_driver._SIMCTL_BOOTSTATUS_S
    assert ios_driver._SIMCTL_BOOTSTATUS_S > ios_driver._SIMCTL_BOOT_S


def test_a_device_that_never_finishes_booting_fails_here(simctl):
    """Loud, naming the device — never a fall-through. Falling through is what
    turned this into an anonymous `selectors.poll` timeout nine minutes later in
    an unrelated call."""
    simctl.outcomes["bootstatus"] = subprocess.CompletedProcess(
        [], 164, stdout="", stderr="device failed to boot"
    )

    with pytest.raises(RuntimeError) as excinfo:
        ios_driver.boot_and_wait_until_usable(_UDID)

    message = str(excinfo.value)
    assert _UDID in message
    assert "never finished booting" in message
    assert "device failed to boot" in message, "the failure quotes simctl's own words"


def test_the_driver_routes_its_precondition_through_the_one_spelling(monkeypatch):
    """`_ensure_booted` must not grow a second idea of "booted" beside this one —
    that second idea IS the defect. conftest's collection-time hoist calls the
    same function, so the launch finds a finished device and pays nothing."""
    seen = []
    monkeypatch.setattr(ios_driver, "boot_and_wait_until_usable", seen.append)
    driver = ios_driver.IosInProcessDriver.__new__(ios_driver.IosInProcessDriver)

    driver._ensure_booted(_UDID)

    assert seen == [_UDID]
