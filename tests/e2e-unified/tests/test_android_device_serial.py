"""tier_1: the run-level android device axis — `helpers/android_device.py`.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode
(*Mode mechanics* — a run-level harness input, the `--app`/`--nest` class) and
§ Default app and nest mode → *Android's run venue* ("any run-level way to name
the device" is one of the three things that section lists as not built).

WHY THIS FILE EXISTS. `drivers/android.py` has always
accepted a `device_serial` and passed `-s`, and nothing ever set it: the arm was
dead code, and the availability probe asked a *different question* from the one
the driver then acted on — "is some device attached" versus "is the device I was
told to use attached". On a box with an emulator and a USB handset that
difference is a run that silently measures the wrong device and reports it as
the named one. The parse and the precedence live in the helper precisely so they
can be pinned here without a device, an emulator, or an adb server (none of
which the primary dev VM can provide today).
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from helpers import android_device  # noqa: E402

pytestmark = pytest.mark.tier_1


# ── resolution precedence: flag, then env, then "whatever is attached" ──────


def test_the_flag_wins():
    assert android_device.resolve_device_serial(
        "emulator-5554", {android_device.ENV_VAR: "R5CT1234"}
    ) == "emulator-5554"


def test_the_env_is_the_fallback():
    assert android_device.resolve_device_serial(
        None, {android_device.ENV_VAR: "R5CT1234"}
    ) == "R5CT1234"


def test_absent_everywhere_means_whichever_device_is_attached():
    assert android_device.resolve_device_serial(None, {}) is None


def test_a_blank_env_value_is_not_a_serial():
    assert android_device.resolve_device_serial(None, {android_device.ENV_VAR: "  "}) is None


def test_surrounding_whitespace_is_stripped():
    assert android_device.resolve_device_serial("  emulator-5554 \n", {}) == "emulator-5554"


def test_a_present_but_empty_flag_is_an_error_not_a_default():
    # `--device-serial "$UNSET_VAR"` in a recipe. Defaulting here would adopt an
    # arbitrary attached device under a command line that plainly meant to name
    # one — the same posture `resolve_nest_mode` takes for `--nest ""`.
    with pytest.raises(android_device.AndroidDeviceError):
        android_device.resolve_device_serial("", {})
    with pytest.raises(android_device.AndroidDeviceError):
        android_device.resolve_device_serial("   ", {})


# ── `adb devices` parsing ───────────────────────────────────────────────────


_TWO_DEVICES = (
    "List of devices attached\n"
    "emulator-5554\tdevice\n"
    "R5CT1234ABC\tdevice\n"
    "\n"
)


def test_the_header_is_not_a_device():
    assert android_device.parse_adb_devices(_TWO_DEVICES) == [
        ("emulator-5554", "device"),
        ("R5CT1234ABC", "device"),
    ]


def test_daemon_chatter_is_dropped():
    noisy = (
        "* daemon not running; starting now at tcp:5037\n"
        "* daemon started successfully\n"
        "List of devices attached\n"
        "emulator-5554\tdevice\n"
    )
    assert android_device.parse_adb_devices(noisy) == [("emulator-5554", "device")]


def test_empty_output_parses_to_nothing():
    assert android_device.parse_adb_devices("") == []
    assert android_device.parse_adb_devices("List of devices attached\n\n") == []


# ── presence: the named device, in a usable state ───────────────────────────


def test_a_named_serial_must_be_the_one_attached():
    assert android_device.device_present(_TWO_DEVICES, "emulator-5554")
    assert android_device.device_present(_TWO_DEVICES, "R5CT1234ABC")
    assert not android_device.device_present(_TWO_DEVICES, "R5CT9999XYZ")


def test_without_a_serial_any_ready_device_will_do():
    assert android_device.device_present(_TWO_DEVICES, None)
    assert not android_device.device_present("List of devices attached\n", None)


def test_unauthorized_and_offline_are_not_usable():
    # A driver would discover this at its first `adb install`, with a message
    # that diagnoses nothing. An honest False here produces the harness's own
    # skip instead.
    out = (
        "List of devices attached\n"
        "emulator-5554\toffline\n"
        "R5CT1234ABC\tunauthorized\n"
    )
    assert not android_device.device_present(out, None)
    assert not android_device.device_present(out, "emulator-5554")
    assert not android_device.device_present(out, "R5CT1234ABC")


def test_a_serial_containing_the_word_device_is_not_a_state():
    # The regression this pins: the probe this replaced was
    # `any("device" in line for line in lines)`, which answered True for a line
    # whose SERIAL carried the word regardless of the device's actual state.
    out = "List of devices attached\nmy-device-01\toffline\n"
    assert not android_device.device_present(out, None)
    assert not android_device.device_present(out, "my-device-01")


# ── the adb prefix is one fact, shared by probe and driver ──────────────────


def test_adb_argv_shape(monkeypatch):
    monkeypatch.setattr(android_device.shutil, "which", lambda name: None)
    assert android_device.adb_argv(None) == ["adb"]
    assert android_device.adb_argv("emulator-5554") == ["adb", "-s", "emulator-5554"]


def test_adb_argv_puts_the_server_before_the_device(monkeypatch):
    # `-L` is a global option naming which SERVER the client talks to; it rides
    # every invocation, ahead of the device selection.
    monkeypatch.setattr(android_device.shutil, "which", lambda name: None)
    assert android_device.adb_argv(None, _SERVER) == ["adb", "-L", _SERVER]
    assert android_device.adb_argv("emulator-5554", _SERVER) == [
        "adb", "-L", _SERVER, "-s", "emulator-5554",
    ]


def test_adb_argv_resolves_the_executable_through_path(monkeypatch):
    # PATHEXT-aware on Windows, where a bare `adb` handed to CreateProcess does
    # not find an `adb.cmd` a shell would.
    monkeypatch.setattr(android_device.shutil, "which", lambda name: f"/sdk/{name}.cmd")
    assert android_device.adb_argv("emulator-5554")[0] == "/sdk/adb.cmd"


# ── the adb server axis: `--adb-server` / `E2E_ADB_SERVER` ──────────────────

_SERVER = "tcp:127.0.0.1:18509"


def test_the_adb_server_flag_wins_over_the_env():
    env = {android_device.ADB_SERVER_ENV_VAR: "tcp:127.0.0.1:1"}
    assert android_device.resolve_adb_server(_SERVER, env) == _SERVER


def test_the_adb_server_env_is_the_fallback():
    env = {android_device.ADB_SERVER_ENV_VAR: f" {_SERVER} "}
    assert android_device.resolve_adb_server(None, env) == _SERVER


def test_no_adb_server_anywhere_means_a_local_server():
    assert android_device.resolve_adb_server(None, {}) is None
    assert android_device.resolve_adb_server(None, {android_device.ADB_SERVER_ENV_VAR: " "}) is None


def test_a_present_but_empty_adb_server_flag_is_an_error():
    with pytest.raises(android_device.AndroidDeviceError):
        android_device.resolve_adb_server("  ", {})


def test_a_hostless_spec_is_refused_because_adb_would_start_a_server_there():
    # `tcp:<port>` is adb's LOCAL form: a client that finds nothing listening
    # starts a server on it. With the tunnel down that is a fresh, empty local
    # server answering in the remote one's name. Measured 2026-10-01, adb 37.0.0:
    # only the explicit-host form answers "cannot start server on remote host".
    with pytest.raises(android_device.AndroidDeviceError, match="LOCAL"):
        android_device.resolve_adb_server("tcp:5037", {})


@pytest.mark.parametrize("spec", ["tcp:10.0.0.2:5037", "tcp:localhost:5037", "tcp:0.0.0.0:5037"])
def test_a_non_loopback_adb_server_is_refused(spec):
    # adb's server protocol is an unauthenticated device shell (constraint 2 of
    # *Android's run venue*). `localhost` is refused too: it is a NAME, and what
    # it resolves to is not this harness's to assume.
    with pytest.raises(android_device.AndroidDeviceError, match="non-loopback"):
        android_device.resolve_adb_server(spec, {})


@pytest.mark.parametrize("spec", [
    "127.0.0.1:5037", "tcp:127.0.0.1", "tcp:127.0.0.1:0", "tcp:127.0.0.1:70000",
    "tcp:127.0.0.1:5037 -s x", "localfilesystem:/tmp/adb.sock",
])
def test_a_malformed_adb_server_spec_is_refused(spec):
    with pytest.raises(android_device.AndroidDeviceError):
        android_device.resolve_adb_server(spec, {})


# ── the probe: asks the run's server about the run's device ─────────────────


class _Ran:
    def __init__(self, stdout="", returncode=0):
        self.stdout, self.returncode = stdout, returncode


def _capture_probe(monkeypatch, answer):
    seen = []

    def fake_run(argv, **kwargs):
        seen.append((argv, kwargs))
        if isinstance(answer, BaseException):
            raise answer
        return answer

    monkeypatch.setattr(android_device.shutil, "which", lambda name: None)
    monkeypatch.setattr(android_device.subprocess, "run", fake_run)
    return seen


def test_the_probe_asks_the_named_server_and_finds_the_named_device(monkeypatch):
    seen = _capture_probe(monkeypatch, _Ran(_TWO_DEVICES))
    assert android_device.probe_device("emulator-5554", _SERVER)
    assert seen[0][0] == ["adb", "-L", _SERVER, "devices"]
    assert seen[0][1]["timeout"] == 5


def test_the_probe_without_a_server_is_the_bare_command(monkeypatch):
    seen = _capture_probe(monkeypatch, _Ran(_TWO_DEVICES))
    assert android_device.probe_device(None, None)
    assert seen[0][0] == ["adb", "devices"]


def test_an_unreachable_server_reads_as_no_device_not_as_an_error(monkeypatch):
    # What the real client does for a dead explicit-host socket (measured, see
    # `resolve_adb_server`): exit 1 at once, nothing on stdout.
    _capture_probe(monkeypatch, _Ran("", returncode=1))
    assert not android_device.probe_device("emulator-5554", _SERVER)


def test_a_probe_that_never_answers_is_false_at_its_timeout(monkeypatch):
    import subprocess

    _capture_probe(monkeypatch, subprocess.TimeoutExpired(cmd="adb", timeout=5))
    assert not android_device.probe_device("emulator-5554", _SERVER)


def test_a_missing_adb_is_false_not_a_crash(monkeypatch):
    _capture_probe(monkeypatch, FileNotFoundError("adb"))
    assert not android_device.probe_device(None, None)


# ── run-level state ─────────────────────────────────────────────────────────


def test_the_run_level_serial_defaults_to_none_outside_a_run():
    # Restored by the fixture below rather than left set: this module-level
    # value is shared with every other test in the process.
    assert android_device.device_serial() is None


@pytest.fixture(autouse=True)
def _restore_run_level_serial():
    before = android_device.device_serial(), android_device.adb_server()
    yield
    android_device.set_device_serial(before[0])
    android_device.set_adb_server(before[1])


def test_conftest_can_push_the_adb_server_in():
    android_device.set_adb_server(_SERVER)
    assert android_device.adb_server() == _SERVER


def test_conftest_can_push_the_serial_in():
    android_device.set_device_serial("emulator-5554")
    assert android_device.device_serial() == "emulator-5554"


# ── the wiring itself, pinned at the source ─────────────────────────────────
#
# The helper above is pure and fully tested, and the driver has ALWAYS accepted
# a `device_serial` — yet the `-s` arm was dead code, because nothing ever set
# it. That is a flow break BETWEEN two working symbols, which no test of either
# symbol can see: a helper test passes, a driver test passes, and the run still
# drives an arbitrary device. Executing the wiring for real would mean spawning
# a sub-pytest through the whole conftest chain, so it is pinned at the source
# instead — weaker than a behavioural assertion, and strictly stronger than the
# nothing that let the arm rot in the first place. Parsed as AST rather than
# grepped, so a mention inside a comment or docstring cannot satisfy it.


def _conftest_function_source(name: str) -> str:
    import ast

    path = os.path.join(os.path.dirname(__file__), "..", "conftest.py")
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    for node in ast.walk(ast.parse(text)):
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return ast.get_source_segment(text, node) or ""
    raise AssertionError(f"conftest has no function named {name!r}")


def test_the_option_is_declared():
    src = _conftest_function_source("pytest_addoption")
    assert '"--device-serial"' in src
    assert '"--adb-server"' in src


def test_configure_pushes_the_serial_into_the_helper():
    # Without this call the helper answers None for every run and the flag is
    # inert — the exact shape of the bug this row closed.
    src = _conftest_function_source("pytest_configure")
    assert "resolve_device_serial" in src
    assert "set_device_serial" in src


def test_configure_pushes_the_adb_server_into_the_helper():
    src = _conftest_function_source("pytest_configure")
    assert "resolve_adb_server" in src
    assert "set_adb_server" in src


def test_the_availability_probe_consults_the_run_s_device():
    src = _conftest_function_source("_android_available")
    assert "device_serial()" in src, (
        "the probe stopped reading the run's serial; it is back to asking "
        "'is SOME device attached' while the driver drives a named one"
    )
    assert "probe_device" in src


def test_the_availability_probe_consults_the_run_s_adb_server():
    # The same flow break one level up: a probe that asks a LOCAL server while
    # the driver drives a tunnelled one answers about the wrong machine.
    src = _conftest_function_source("_android_available")
    assert "adb_server()" in src
    assert '"adb", "devices"' not in src, (
        "the probe builds its own adb command line again; it must go through "
        "android_device.probe_device so the run's `-L` reaches it"
    )


def test_the_launch_config_carries_the_serial_and_the_nest_ports():
    # The bridge APK and this run's device ride the config through
    # `android_device.run_launch_facts` — the one home the launch harness reads
    # too — so `_build_app_config` no longer spells `"device_serial"` itself.
    # The delegation is pinned here and the key where it is now spelled, below.
    src = _conftest_function_source("_build_app_config")
    assert "run_launch_facts" in src
    assert '"nest_ports"' in src


def test_the_run_launch_facts_carry_the_serial_and_the_test_apk(tmp_path):
    android_device.set_device_serial("emulator-5554")
    facts = android_device.run_launch_facts(tmp_path)
    assert facts["device_serial"] == "emulator-5554"
    assert facts["test_apk"] == str(tmp_path / android_device.TEST_APK)
    android_device.set_device_serial(None)
    assert android_device.run_launch_facts(tmp_path)["device_serial"] is None


def test_the_run_launch_facts_carry_the_adb_server(tmp_path):
    # The driver reads its server off the launch config, so a fact dropped here
    # is a driver that probes the tunnelled server and drives a local one.
    assert android_device.run_launch_facts(tmp_path)["adb_server"] is None
    android_device.set_adb_server(_SERVER)
    assert android_device.run_launch_facts(tmp_path)["adb_server"] == _SERVER
