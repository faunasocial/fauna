"""tier_1: every `adb` command line `drivers/android.py` issues, against a fake adb.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*Android's run venue* (constraint 3: the device reaches the test nest as its own
loopback via `adb reverse`, and the run can name its device) and
`e2e-conventions.md` convention 14 (assert latency-independent state, never wall
clock).

WHY THIS FILE EXISTS. No android e2e run has ever
been recorded -- `docs/features/ledger/` has no `android.json` -- and the run
venue is still the user's open decision. So the harness half of an android run
has never executed anywhere, and the three things that would stop the FIRST
journey on ANY venue are all testable here on a machine with no device at all:

  1. the reverse tunnel per nest port, without which the app cannot reach the
     nest even once,
  2. its removal at teardown keyed on the driver's OWN record of what it opened
     -- the forward's port is a different mapping in the other direction, and
     keying removal on it would leak every reverse onto the device,
  3. `-s <serial>` on every single adb invocation, so a box with two devices
     attached cannot have one command land on the wrong one.

The instrument is a fake `adb` executable on a private PATH dir that appends its
own argv to a log -- the shape `test_cargo_target_seed.py` and
`test_apple_e2e_agent_staging_reap.py` use. The assertions are over the recorded
command LINES: pure state, no timing.

**Two network calls are neutralized, and that is the point of the design, not a
shortcut.** `launch()` ends with `_wait_for_health(timeout=30)` and a
`_post("/session")`, both of which talk to a bridge that does not exist here.
Left alone this file would spend 30 s per test and end in `TimeoutError` before
reaching a single assertion about command lines -- it would be a slow test of
nothing. Both are replaced with recorders, so what remains under test is exactly
the adb surface this file claims to pin.

No process is ever killed here beyond the driver's own teardown of its own fake
child (the process-safety rule: never `pkill`/`os.kill` anything on a shared
machine).
"""

import os
import stat
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.android import (  # noqa: E402
    AndroidBridgeDriver,
    nest_ports_from_config,
)

pytestmark = pytest.mark.tier_1


_FAKE_ADB = """#!/usr/bin/env bash
# Records its own argv, one invocation per line, and succeeds. `adb shell am
# instrument -w` is long-lived in reality; here it exits immediately, which the
# driver tolerates because _wait_for_health is neutralized by the fixture.
printf '%s\\n' "$*" >> "$FAKE_ADB_LOG"
exit 0
"""

# The Windows twin of `_FAKE_ADB`: same one-line-per-invocation record, same
# success. `%*` is the argv as passed; no argument the driver issues contains a
# space, so the recorded line matches the bash `$*` form token for token.
_FAKE_ADB_CMD = """@echo off
>>"%FAKE_ADB_LOG%" echo %*
exit /b 0
"""


@pytest.fixture
def fake_adb(tmp_path, monkeypatch):
    """A fake `adb` first on PATH, plus a reader for the recorded argv lines."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    if sys.platform == "win32":
        # CreateProcess cannot exec a `#!` script; the same recorder as a batch
        # file, which the driver's PATHEXT-aware lookup finds as `adb`.
        adb = bin_dir / "adb.cmd"
        adb.write_text(_FAKE_ADB_CMD, encoding="utf-8", newline="\r\n")
    else:
        adb = bin_dir / "adb"
        adb.write_text(_FAKE_ADB, encoding="utf-8")
    adb.chmod(adb.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    log = tmp_path / "adb.log"
    monkeypatch.setenv("FAKE_ADB_LOG", str(log))
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    def lines():
        if not log.exists():
            return []
        return [ln for ln in log.read_text(encoding="utf-8").splitlines() if ln.strip()]

    return lines


@pytest.fixture(autouse=True)
def bridge_posts(monkeypatch):
    """Neutralize the two bridge HTTP calls; return the recorded `_post` list.

    See the module docstring: `_wait_for_health` would burn its full 30 s
    timeout and raise, and `_post("/session")` would fail on a connection that
    was never going to exist. Both are recorded instead of performed, so the
    launch runs to completion and the adb assertions are reached.

    ⚠ This fixture deliberately yields a LIST, and `_driver()` below is a plain
    function rather than a fixture. Convention 17's layer-(b) frame probe
    resolves a driver **by value** out of `item.funcargs`
    (`helpers/frame_invariants.driver_for`), so any fixture handing back a
    driver instance tells the harness this test left a real app frame to
    observe. It did not: there is no app, no bridge and no device here. The
    probe would then issue a real `GET /app/state`, have it refused, and print
    a `[BRIDGE DEAD during …]` banner on every test in this file — a false
    signal in exactly the diagnostic channel a real android run will need to be
    readable. Keep the driver out of `funcargs`.
    """
    posts = []

    def record(self, path, body=None):
        # The adb log AS IT STOOD when the app was launched. Captured here
        # rather than compared by line index afterwards, because the
        # `am instrument` line is written by a `Popen`'d child and therefore
        # lands at a moment nothing in this process controls — an ordering
        # assertion against it is wall-clock-dependent, which convention 14
        # makes defunct rather than merely flaky. (Measured: one such
        # assertion passed four mutation runs and failed the fifth.) Every
        # adb call that matters here is synchronous, so a snapshot taken at
        # the launch instant is exact state.
        log = os.environ.get("FAKE_ADB_LOG")
        issued = []
        if log and os.path.exists(log):
            with open(log, encoding="utf-8") as fh:
                issued = [ln for ln in fh.read().splitlines() if ln.strip()]
        posts.append((path, body, issued))

    monkeypatch.setattr(AndroidBridgeDriver, "_wait_for_health", lambda self, timeout=30: None)
    monkeypatch.setattr(AndroidBridgeDriver, "_post", record)
    monkeypatch.setattr(AndroidBridgeDriver, "_delete", lambda self, path: None)
    return posts


def _driver():
    """A driver instance, built in the test body — see `bridge_posts`'s warning."""
    return AndroidBridgeDriver()


def _config(**over):
    cfg = {
        "url": "http://127.0.0.1:13001",
        "app_path": "/tmp/app-debug.apk",
        "test_apk": "/tmp/app-debug-androidTest.apk",
        "environment": {},
    }
    cfg.update(over)
    return cfg


def _matching(lines, *tokens):
    return [ln for ln in lines if all(t in ln.split() for t in tokens)]


# ── nest_ports_from_config: what gets a reverse ─────────────────────────────


def test_session_nest_port_comes_from_the_url():
    assert nest_ports_from_config(_config()) == [13001]


def test_extra_nests_are_included_in_first_seen_order():
    cfg = _config(nest_ports=[13002, 13003])
    assert nest_ports_from_config(cfg) == [13001, 13002, 13003]


def test_a_port_named_twice_is_reversed_once():
    # conftest passes the session nest in `nest_ports` too; a duplicated
    # `adb reverse` for one port is not an error on a device, but a duplicated
    # entry in the removal list is, so de-duplication happens at the source.
    cfg = _config(nest_ports=[13001, 13002])
    assert nest_ports_from_config(cfg) == [13001, 13002]


def test_a_url_without_a_port_contributes_nothing():
    assert nest_ports_from_config({"url": "https://example.com"}) == []


def test_string_ports_and_junk_are_handled_not_crashed():
    cfg = _config(nest_ports=["13002", None, "", "not-a-port", 0])
    assert nest_ports_from_config(cfg) == [13001, 13002]


# ── launch: the exact command lines ─────────────────────────────────────────


def test_launch_reverses_every_nest_port(fake_adb):
    driver = _driver()
    driver.launch(_config(nest_ports=[13002, 13003]))

    for port in (13001, 13002, 13003):
        assert _matching(fake_adb(), "reverse", f"tcp:{port}"), (
            f"no `adb reverse` for nest port {port}; issued: {fake_adb()}"
        )


def test_reverse_maps_the_port_to_itself(fake_adb):
    # `tcp:<p> tcp:<p>`, not two different ports: the whole point is that the
    # app can be handed the harness's ordinary http://127.0.0.1:<port> with no
    # rewriting on either side.
    driver = _driver()
    driver.launch(_config())
    assert "reverse tcp:13001 tcp:13001" in fake_adb()


def test_the_forward_is_still_opened_and_is_not_a_reverse(fake_adb):
    driver = _driver()
    driver.launch(_config())
    forwards = [ln for ln in fake_adb() if ln.startswith("forward ")]
    assert len(forwards) == 1
    assert driver._local_port is not None
    assert f"tcp:{driver._local_port}" in forwards[0]
    # The bridge's device port, not a nest port.
    assert "tcp:18500" in forwards[0]


def test_launch_installs_both_apks(fake_adb):
    driver = _driver()
    driver.launch(_config())
    installs = [ln for ln in fake_adb() if ln.startswith("install ")]
    assert len(installs) == 2
    assert any("app-debug.apk" in ln for ln in installs)
    assert any("app-debug-androidTest.apk" in ln for ln in installs)


def test_the_reverse_precedes_the_app_launch(fake_adb, bridge_posts):
    # The /session POST is what starts the app. An app that reaches its first
    # nest call before the reverse exists fails with a bare connection error
    # naming nothing, so the ordering is load-bearing rather than tidy.
    #
    # Asserted against the adb log AS IT STOOD at the launch instant — see
    # `bridge_posts`. Comparing line indices afterwards would race the
    # asynchronously-written `am instrument` line.
    driver = _driver()
    driver.launch(_config(nest_ports=[13002]))

    sessions = [issued for path, _body, issued in bridge_posts if path == "/session"]
    assert sessions, "the app was never launched"
    at_launch = sessions[0]
    for port in (13001, 13002):
        assert f"reverse tcp:{port} tcp:{port}" in at_launch, (
            f"the app was launched before nest port {port} had a reverse; "
            f"adb calls issued by then: {at_launch}"
        )


# ── the serial reaches every single invocation ──────────────────────────────


def test_every_adb_call_carries_the_serial(fake_adb):
    driver = _driver()
    driver.launch(_config(device_serial="emulator-5554", nest_ports=[13002]))
    driver.teardown()

    issued = fake_adb()
    assert issued, "the fake adb was never invoked"
    for line in issued:
        assert line.startswith("-s emulator-5554 "), (
            f"an adb call went to whichever device was attached: {line!r}"
        )


def test_without_a_serial_no_dash_s_is_passed(fake_adb):
    driver = _driver()
    driver.launch(_config())
    assert not any(ln.startswith("-s ") for ln in fake_adb())


# ── the venue: a remote adb server, and only ports the tunnel carries ───────
#
# `testing.md` § Default app and nest mode → *Android's run venue*: with
# `adb_server` in the launch config the driver is a pure client of a server at
# the far end of a tunnel, so every command names that server and every port
# either end listens on comes from the venue's fixed ranges.

_SERVER = "tcp:127.0.0.1:18509"


@pytest.fixture
def venue(tmp_path, monkeypatch):
    """A venue launch config, with leases taken in a private directory so this
    file never contends with a real android run on the machine."""
    from helpers import android_venue

    monkeypatch.setattr(android_venue, "lease_dir", lambda: str(tmp_path / "leases"))
    nest = android_venue.NEST_PORTS[0]
    return android_venue, _config(url=f"http://127.0.0.1:{nest}", adb_server=_SERVER)


def test_every_adb_call_names_the_remote_server(fake_adb, venue):
    _venue, config = venue
    driver = _driver()
    driver.launch({**config, "device_serial": "emulator-5554"})
    driver.teardown()

    issued = fake_adb()
    assert issued, "the fake adb was never invoked"
    for line in issued:
        assert line.startswith(f"-L {_SERVER} -s emulator-5554 "), (
            f"an adb call went to a local adb server: {line!r}"
        )


def test_without_an_adb_server_no_dash_l_is_passed(fake_adb):
    driver = _driver()
    driver.launch(_config())
    assert not any("-L" in ln.split() for ln in fake_adb())


def test_the_venue_forward_port_comes_from_the_fixed_range(fake_adb, venue):
    android_venue, config = venue
    driver = _driver()
    driver.launch(config)
    assert driver._local_port == android_venue.BRIDGE_FORWARD_PORTS[0]
    assert f"-L {_SERVER} forward tcp:{driver._local_port} tcp:18500" in fake_adb()


def test_two_venue_seats_get_different_forward_ports_and_give_them_back(fake_adb, venue):
    android_venue, config = venue
    first, second = _driver(), _driver()
    first.launch(config)
    second.launch(config)
    assert (first._local_port, second._local_port) == tuple(android_venue.BRIDGE_FORWARD_PORTS[:2])

    first.teardown()
    third = _driver()
    third.launch(config)
    assert third._local_port == android_venue.BRIDGE_FORWARD_PORTS[0]


def test_a_venue_reverse_for_an_untunnelled_nest_port_is_refused(fake_adb, venue):
    # The reverse would succeed and the app would then fail its first nest call
    # with a connection error naming nothing; the driver knows the cause.
    android_venue, config = venue
    driver = _driver()
    driver.launch(config)
    with pytest.raises(android_venue.AndroidVenueError, match="tunnelled range"):
        driver.ensure_reverse(13099)
    assert not _matching(fake_adb(), "reverse", "tcp:13099")


def test_a_venue_launch_pointed_at_an_untunnelled_nest_fails_before_the_app_starts(
    fake_adb, venue, bridge_posts
):
    android_venue, config = venue
    with pytest.raises(android_venue.AndroidVenueError):
        _driver().launch({**config, "url": "http://127.0.0.1:13001"})
    assert not [path for path, _body, _issued in bridge_posts if path == "/session"]


# ── teardown: removal keyed on the driver's OWN reverse record ──────────────


def test_teardown_removes_every_reverse_it_opened(fake_adb):
    driver = _driver()
    driver.launch(_config(nest_ports=[13002, 13003]))
    driver.teardown()

    removals = [ln for ln in fake_adb() if ln.startswith("reverse --remove ")]
    assert sorted(removals) == sorted(
        [f"reverse --remove tcp:{p}" for p in (13001, 13002, 13003)]
    )


def test_reverse_removal_is_not_keyed_on_the_forward_port(fake_adb):
    # The regression this pins: teardown used to know only `_local_port`, which
    # is the FORWARD (host -> device). Removing `tcp:<forward>` as if it were a
    # reverse leaves every nest reverse claimed on the device against the next
    # run, and removes a mapping that was never opened in that direction.
    driver = _driver()
    driver.launch(_config())
    forward_port = driver._local_port
    driver.teardown()

    assert f"reverse --remove tcp:{forward_port}" not in fake_adb()
    assert f"forward --remove tcp:{forward_port}" in fake_adb()


def test_teardown_clears_the_record_so_a_second_teardown_is_quiet(fake_adb):
    driver = _driver()
    driver.launch(_config())
    driver.teardown()
    before = len(fake_adb())
    driver.teardown()
    after = [ln for ln in fake_adb()[before:] if ln.startswith("reverse --remove")]
    assert after == []


def test_ensure_reverse_is_idempotent_and_records_late_nests(fake_adb):
    # A nest started AFTER the app launched -- the multi-nest fixtures' shape --
    # is invisible to launch()'s config and comes in through here.
    driver = _driver()
    driver.launch(_config())
    driver.ensure_reverse(13099)
    driver.ensure_reverse(13099)

    assert len(_matching(fake_adb(), "reverse", "tcp:13099")) == 1
    driver.teardown()
    assert "reverse --remove tcp:13099" in fake_adb()


# ── the credential seed: an EMPTY seed is still a seed ──────────────────────


def test_an_empty_seed_is_posted_not_dropped(fake_adb, bridge_posts):
    # The on-device credential file outlives the launch that wrote it (the app's
    # own filesDir; `adb install -r` keeps data), and the bridge forwards
    # whatever file exists. So a launch that means "no stored identity" must
    # SEND an empty seed, which the bridge writes over the stale file — a
    # truthiness check here dropped it, and the fresh launch inherited an
    # earlier test's identity (`common.launch_harness.AndroidLaunchHarness`).
    driver = _driver()
    driver.launch(_config(seed_credentials={}))
    (body,) = [body for path, body, _ in bridge_posts if path == "/session"]
    assert body["seed_credentials"] == {}


def test_no_seed_key_posts_no_seed(fake_adb, bridge_posts):
    # The `app` fixture's launch names no seed at all: that stays "leave the
    # on-device file alone", distinct from the empty seed above.
    driver = _driver()
    driver.launch(_config())
    (body,) = [body for path, body, _ in bridge_posts if path == "/session"]
    assert "seed_credentials" not in body


# ── the on-device credential file: read-back and the re-auth verdict ────────
#
# Both files live in the app's own filesDir, which no host path reaches: the
# instrumentation shares the app's UID, so the bridge is the one boundary
# crossing, in both directions (`long-term-store.md` § Implementation status
# today — the android read-back gap). These pin the driver half of that
# crossing: which bridge verb each call issues and what it sends.


def test_credential_map_reads_the_bridge_read_back(monkeypatch):
    gets = []

    def fake_get(self, path, params=None):
        gets.append(path)
        return {"credentials": {"fauna/index": '{"active":"a","accounts":[]}'}}

    monkeypatch.setattr(AndroidBridgeDriver, "_get", fake_get)
    assert _driver().credential_map() == {"fauna/index": '{"active":"a","accounts":[]}'}
    assert gets == ["/credentials"]


def test_credential_map_refuses_a_malformed_answer(monkeypatch):
    # A bridge that answers without the `credentials` object must not read as
    # an empty store: every reader treats "empty" as a real answer (an erase
    # that worked, an index not yet written), so a shape error has to raise.
    monkeypatch.setattr(AndroidBridgeDriver, "_get", lambda self, path, params=None: {})
    with pytest.raises(RuntimeError, match="credentials"):
        _driver().credential_map()


def test_write_reauth_verdict_posts_the_verdict(bridge_posts):
    _driver().write_reauth_verdict("approve")
    assert [(p, b) for p, b, _ in bridge_posts] == [("/reauth-result", {"verdict": "approve"})]


def test_write_reauth_verdict_none_posts_null_to_remove_the_file(bridge_posts):
    # ABSENT reads as decline (fail-closed), so `None` must reach the bridge as
    # an explicit null — the remove instruction — not as a dropped key.
    _driver().write_reauth_verdict(None)
    assert [(p, b) for p, b, _ in bridge_posts] == [("/reauth-result", {"verdict": None})]
