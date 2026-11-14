"""tier_1: android's run emulator — `helpers/android_emu.py`.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*A2 amended — the design* (the emulators under a dedicated account as `launchd`
daemons, no adb server on their machine, a tunnel of remote forwards to each
emulator's adbd, the dev VM's own emulator adb server attaching them by `adb
connect`, the hardening each point names); the owner's steps and checks C1–C12
live in the emulator setup's setup notes.

WHY THIS FILE EXISTS. The emulator setup's machine is set up by the owner pasting blocks
this module prints, and the dev VM accepts its tunnel by a line this module
writes. Nothing here needs that machine, an emulator or a tunnel: the
constants, the `authorized_keys` line, the tunnel's argv, the shape of every
pasted block (the paste rule), the bytes a
block writes, and the probe's reading of each down leg are all checked against
the generated text, a fake `adb` and a scratch home.
"""

import os
import plistlib
import re
import stat
import subprocess
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from helpers import android_device, android_emu as V  # noqa: E402

pytestmark = pytest.mark.tier_1

_E2E_ROOT = os.path.join(os.path.dirname(__file__), "..")
_REPO_ROOT = os.path.join(_E2E_ROOT, "..", "..")

_LOGIN, _ADDR = "user", "10.211.55.7"
_KEY = ("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI" + "A" * 43
        + " faunaemu@host")


@pytest.fixture(scope="module")
def venue_pub(tmp_path_factory):
    """An emulator key pair made the way `make_venue_key` makes the real one."""
    path = str(tmp_path_factory.mktemp("emulator-key") / "adbkey")
    V.make_venue_key(path)
    return f"{path}.pub"


#: Every flag set the fallback is used with: a failed C1 (the tunnel), a failed
#: C9 (either emulator or both), or all three.
_FALLBACKS = [("tunnel",), ("emulator",), ("play",), ("emulator", "play"),
              ("emulator", "play", "tunnel")]


def _blocks(venue_pub) -> dict[str, str]:
    """Every block the owner pastes, by generator."""
    return {
        "tunnel": V.tunnel_spec(_LOGIN, _ADDR),
        "daemons": V.daemons(_LOGIN, _ADDR),
        **{f"daemons --no-{' --no-'.join(jobs)}": V.daemons(_LOGIN, _ADDR, without=jobs)
           for jobs in _FALLBACKS},
        **{f"agents --{' --'.join(jobs)}": V.agents(_LOGIN, _ADDR, jobs=jobs)
           for jobs in _FALLBACKS},
        "netcheck": V.netcheck(_ADDR),
        "key": V.key_block(_ADDR),
        "adbkey": V.adbkey_block(venue_pub),
        "play-setup": V.play_setup(),
        "listeners": V.listeners(),
    }


_VENUE_BLOCKS = {"tunnel", "key", "adbkey", "play-setup"}


# ── the constants ───────────────────────────────────────────────────────────


def test_each_leg_has_its_own_ports_and_the_adbd_port_follows_the_console():
    ports = [leg.dev_vm_port for leg in V.LEGS]
    assert ports == [18509, 18510, 18511]
    assert [leg.adbd_port for leg in V.LEGS] == [5555, 5557, 5559]
    assert all(leg.adbd_port == leg.console_port + 1 for leg in V.LEGS)
    assert V.VENUE_ADB_SERVER_PORT not in ports


def test_every_dev_vm_port_is_below_the_ephemeral_range_and_off_adbs_default():
    # Linux hands out 32768+; a sibling's find_free_port() can never take one.
    for port in [V.VENUE_ADB_SERVER_PORT, *(leg.dev_vm_port for leg in V.LEGS)]:
        assert port < 32768 and port != 5037


def test_the_serials_are_the_legs_connect_addresses():
    assert V.VENUE_SERIAL == "127.0.0.1:18509"
    assert V.PLAY_SERIAL == "127.0.0.1:18510"
    assert android_device.is_connect_serial(V.VENUE_SERIAL)
    assert [leg.avd for leg in V.LEGS[:2]] == [V.AVD_NAME, V.PLAY_AVD_NAME]


def test_the_venue_server_spec_is_one_the_harness_accepts():
    assert V.ADB_SERVER_SPEC == "tcp:127.0.0.1:18508"
    assert android_device.resolve_adb_server(V.ADB_SERVER_SPEC, {}) == V.ADB_SERVER_SPEC


# ── the dev VM's authorized_keys line ───────────────────────────────────────


def _options(line: str) -> list[str]:
    return re.findall(r'[a-z-]+(?:="[^"]*")?', line.split(" ssh-ed25519 ", 1)[0])


def test_the_authorized_keys_line_carries_exactly_the_hardening_options():
    line = V.authorized_keys_line(_KEY, _ADDR)
    assert _options(line) == [
        f'from="{_ADDR}"', 'command="/bin/false"', "restrict", "port-forwarding",
        'permitlisten="127.0.0.1:18509"', 'permitlisten="127.0.0.1:18510"',
        'permitlisten="127.0.0.1:18511"', 'permitopen="127.0.0.1:1"',
    ]
    assert line.endswith(" " + V.TUNNEL_KEY_COMMENT)
    # The key's own comment is replaced: the emulator setup's is the one C6 counts.
    assert "faunaemu@host" not in line


@pytest.mark.parametrize("key", [
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ test",
    "",
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA\nssh-ed25519 AAAAC3NzaC1lZDI1NTE5BBBB",
    'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA",command="/bin/sh',
])
def test_anything_but_one_ed25519_key_is_refused(key):
    with pytest.raises(V.AndroidEmuError):
        V.authorized_keys_line(key, _ADDR)


@pytest.mark.parametrize("address", ["", "host.local", "10.0.0.300", "10.0.0.1,0.0.0.0/0", "::1"])
def test_anything_but_one_ipv4_address_is_refused(address):
    with pytest.raises(V.AndroidEmuError):
        V.authorized_keys_line(_KEY, address)


def test_authorize_replaces_an_earlier_venue_line_and_keeps_the_rest(tmp_path):
    path = tmp_path / ".ssh" / "authorized_keys"
    path.parent.mkdir()
    path.write_text(f"ssh-ed25519 AAAAown owner\nold {V.TUNNEL_KEY_COMMENT}\n")
    reading = V.authorize(_KEY, _ADDR, str(path))
    lines = path.read_text().splitlines()
    assert lines == ["ssh-ed25519 AAAAown owner", V.authorized_keys_line(_KEY, _ADDR)]
    assert reading.startswith("C6: 1 line(s)")
    assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_authorize_writes_a_first_line_into_a_missing_file(tmp_path):
    path = tmp_path / ".ssh" / "authorized_keys"
    assert V.authorize(_KEY, _ADDR, str(path)).startswith("C6: 1 line(s)")


# ── the tunnel ──────────────────────────────────────────────────────────────


def test_the_tunnel_is_remote_forwards_only_one_per_leg_loopback_at_both_ends():
    argv = V.tunnel_command(_LOGIN, _ADDR)
    forwards = [argv[i + 1] for i, word in enumerate(argv) if word in ("-R", "-L", "-D")]
    assert [argv[i] for i, word in enumerate(argv) if word in ("-L", "-D")] == []
    assert forwards == [f"127.0.0.1:{leg.dev_vm_port}:127.0.0.1:{leg.adbd_port}"
                        for leg in V.LEGS]


def test_the_tunnel_carries_the_hardened_option_list_verbatim():
    argv = V.tunnel_command(_LOGIN, _ADDR)
    assert argv[:6] == ["/usr/bin/ssh", "-N", "-F", "none", "-i", V.TUNNEL_KEY]
    options = [argv[i + 1] for i, word in enumerate(argv) if word == "-o"]
    assert options == [
        "IdentitiesOnly=yes", "IdentityAgent=none", "ForwardAgent=no", "ForwardX11=no",
        "StrictHostKeyChecking=yes", "PermitLocalCommand=no", "LogLevel=ERROR",
        "BatchMode=yes", "ExitOnForwardFailure=yes", "ServerAliveInterval=30",
    ]
    assert argv[-1] == f"{_LOGIN}@{_ADDR}"


@pytest.mark.parametrize("login, address", [
    ("user; rm -rf ~", _ADDR), (_LOGIN, "$(id)"), ("", _ADDR), (_LOGIN, "-oProxyCommand=x"),
])
def test_tunnel_arguments_that_are_not_plain_words_are_refused(login, address):
    with pytest.raises(V.AndroidEmuError):
        V.tunnel_command(login, address)


def test_the_by_hand_tunnel_block_rebuilds_the_same_argv(tmp_path):
    # Run the block's `set --` lines under sh with `ssh` replaced by a printer.
    block = V.tunnel_spec(_LOGIN, _ADDR)
    body = block.split("\n")[2:-1]
    body = [ln for ln in body if not ln.startswith(("test ", "echo "))]
    body[-1] = body[-1].replace("/usr/bin/ssh", "printf '%s\\n'")
    out = subprocess.run(["sh", "-c", "\n".join(body)], capture_output=True, text=True,
                         check=True).stdout.splitlines()
    assert out == V.tunnel_command(_LOGIN, _ADDR)[1:]


# ── every pasted block: the paste-safe shape, the account guard ─────────────


def test_every_generator_prints_a_paste_safe_block(venue_pub):
    for name, block in _blocks(venue_pub).items():
        V.check_paste_safe(block)  # raises, naming the line
        lines = block.split("\n")
        assert lines[:2] == ["(", "set -e"] and lines[-1] == ")", name
        assert all(len(ln) <= 70 for ln in lines), name
        assert not any(ln.lstrip().startswith("#") or "<<" in ln or ln.endswith("\\")
                       for ln in lines), name


@pytest.mark.parametrize("bad", [
    "(\nset -e\n# a comment\n)", "(\nset -e\n" + "x" * 71 + "\n)",
    "(\nset -e\ncat <<EOF\nEOF\n)", "(\nset -e\nls \\\n-l\n)", "set -e\nls",
])
def test_the_shape_check_refuses_each_unsafe_form(bad):
    with pytest.raises(V.AndroidEmuError):
        V.check_paste_safe(bad)


def test_every_block_opens_with_the_guard_of_its_account(venue_pub):
    for name, block in _blocks(venue_pub).items():
        guard = block.split("\n")[3]
        want = "=" if name in _VENUE_BLOCKS or name.startswith("agents") else "!="
        assert guard == f'test "$(id -un)" {want} {V.VENUE_ACCOUNT}', name


def test_no_block_starts_an_adb_server_or_carries_a_private_key(venue_pub):
    for name, block in _blocks(venue_pub).items():
        for line in block.split("\n"):
            if line.startswith("echo "):
                continue
            words = line.split()
            if any(w.endswith("/adb") or w == "adb" for w in words):
                assert not {"server", "start-server", "fork-server", "nodaemon"} & set(words), \
                    (name, line)
        assert "PRIVATE KEY" not in block, name
        if not name.startswith("agents"):
            assert "LaunchAgents" not in block and "gui/" not in block, name


# ── the bytes a block writes ────────────────────────────────────────────────


def _run_write(text: str, tmp_path) -> bytes:
    out = tmp_path / "written"
    script = "\n".join([f"t={out}", *V._write_file("t", text)])
    subprocess.run(["sh", "-c", script], check=True)
    return out.read_bytes()


def test_the_daemon_plists_are_written_byte_for_byte(tmp_path):
    plists = {**V.daemon_plists(_LOGIN, _ADDR)}
    plists |= {f"agent {k}": v for k, v in V.agent_plists(_LOGIN, _ADDR, V.JOBS).items()}
    for label, plist in plists.items():
        assert _run_write(plist, tmp_path) == plist.encode(), label


def test_the_adbkey_block_writes_the_public_half_and_its_checksum_passes(venue_pub, tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_id = bin_dir / "id"
    fake_id.write_text(f"#!/bin/sh\necho {V.VENUE_ACCOUNT}\n")
    fake_id.chmod(0o755)
    home = tmp_path / "home"
    home.mkdir()
    env = {**os.environ, "HOME": str(home), "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}
    done = subprocess.run(["sh", "-c", V.adbkey_block(venue_pub)], env=env,
                          capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    assert "C4a: OK" in done.stdout
    with open(venue_pub, "rb") as fh:
        assert (home / ".android" / "adbkey.pub").read_bytes() == fh.read()


def test_a_block_pasted_into_the_wrong_account_stops_at_its_guard(venue_pub, tmp_path):
    home = tmp_path / "home"
    home.mkdir()
    done = subprocess.run(["sh", "-c", V.adbkey_block(venue_pub)],
                          env={**os.environ, "HOME": str(home)}, capture_output=True)
    assert done.returncode != 0
    assert not (home / ".android").exists()


def test_the_venue_public_key_is_adbs_own_form(venue_pub):
    # A key pair adb made itself, on this machine, is the reference when there
    # is one; otherwise the blob's fixed layout is checked.
    import base64

    blob = base64.b64decode(open(venue_pub).read().split()[0])
    assert len(blob) == 524 and int.from_bytes(blob[:4], "little") == 64
    assert int.from_bytes(blob[-4:], "little") == 65537
    own = os.path.expanduser("~/.android/adbkey")
    if os.path.exists(own) and os.path.exists(own + ".pub"):
        assert V.adb_public_key(own) == open(own + ".pub").read().split()[0]
    assert re.fullmatch(r"([0-9A-F]{2}:){15}[0-9A-F]{2}", V.venue_key_fingerprint(venue_pub))


# ── the daemons ─────────────────────────────────────────────────────────────


def _plists() -> dict[str, dict]:
    return {label: plistlib.loads(text.encode())
            for label, text in V.daemon_plists(_LOGIN, _ADDR).items()}


def test_every_daemon_runs_as_the_venue_account_and_stays_up():
    plists = _plists()
    assert set(plists) == {V.EMULATOR_LABEL, V.PLAY_EMULATOR_LABEL, V.TUNNEL_LABEL}
    for label, plist in plists.items():
        assert plist["Label"] == label
        assert (plist["UserName"], plist["GroupName"]) == (V.VENUE_ACCOUNT, "staff")
        assert plist["RunAtLoad"] is True and plist["KeepAlive"] is True
        assert "SessionCreate" not in plist
        assert plist["EnvironmentVariables"] == {"HOME": V.VENUE_HOME, "ANDROID_HOME": V.SDK}
        sh, dash_c, command = plist["ProgramArguments"]
        assert (sh, dash_c) == ("/bin/sh", "-c") and command.startswith("exec ")
        # The redirect truncates the log at every start: one run per log.
        assert command.endswith(f" > {V.VENUE_HOME}/Library/Logs/"
                                f"fauna-{label.removeprefix('social.fauna.')}.log 2>&1")


def test_each_emulator_runs_headless_on_its_console_port_with_cameras_off():
    plists = _plists()
    for label, leg in ((V.EMULATOR_LABEL, V.LEGS[0]), (V.PLAY_EMULATOR_LABEL, V.LEGS[1])):
        command = plists[label]["ProgramArguments"][2]
        assert f"-avd {leg.avd} -port {leg.console_port} " in command
        for flag in ("-no-window", "-no-audio", "-gpu swiftshader_indirect",
                     "-camera-back none", "-camera-front none", "-no-snapshot"):
            assert flag in command, (label, flag)


def test_the_tunnel_daemon_runs_exactly_the_tunnel():
    import shlex

    command = _plists()[V.TUNNEL_LABEL]["ProgramArguments"][2]
    words = shlex.split(command.removeprefix("exec ").rsplit(" > ", 1)[0])
    assert words == V.tunnel_command(_LOGIN, _ADDR)


def _installed(block: str) -> list[str]:
    """The labels a block writes a plist for, in order."""
    labels, label = [], None
    for line in block.split("\n"):
        if line.startswith("l="):
            label = line.removeprefix("l=")
        elif line == "t=$(mktemp -d)/p.plist":
            labels.append(label)
    return labels


def test_the_no_tunnel_form_leaves_the_tunnel_out():
    assert set(V.daemon_plists(_LOGIN, _ADDR, without={"tunnel"})) == {
        V.EMULATOR_LABEL, V.PLAY_EMULATOR_LABEL}
    assert V.TUNNEL_LABEL not in _installed(V.daemons(_LOGIN, _ADDR, without={"tunnel"}))


@pytest.mark.parametrize("jobs", _FALLBACKS)
def test_no_label_is_printed_by_both_generators_for_one_flag_set(jobs):
    labels = {V.JOBS[j] for j in jobs}
    daemons_block = V.daemons(_LOGIN, _ADDR, without=jobs)
    agents_block = V.agents(_LOGIN, _ADDR, jobs=jobs)
    assert set(_installed(agents_block)) == labels
    assert set(_installed(daemons_block)) == set(V.JOBS.values()) - labels
    # Together the two blocks give each job exactly one home ...
    assert sorted(_installed(daemons_block) + _installed(agents_block)) == sorted(V.JOBS.values())
    # ... and the daemons block removes the daemon of every job moved to an agent.
    for label in labels:
        assert f"l={label}\nsudo launchctl bootout system/$l 2>/dev/null || true\n" \
               f'sudo rm -f "/Library/LaunchDaemons/$l.plist"' in daemons_block


def test_an_agent_is_the_daemon_without_its_account_keys():
    every = V.JOBS.values()
    daemon = {k: plistlib.loads(v.encode()) for k, v in V.daemon_plists(_LOGIN, _ADDR).items()}
    agent = {k: plistlib.loads(v.encode())
             for k, v in V.agent_plists(_LOGIN, _ADDR, V.JOBS).items()}
    assert set(daemon) == set(agent) == set(every)
    for label in every:
        want = {k: v for k, v in daemon[label].items() if k not in ("UserName", "GroupName")}
        assert agent[label] == want, label
        assert agent[label]["RunAtLoad"] is True and agent[label]["KeepAlive"] is True


def test_the_agents_block_bootstraps_into_the_venue_accounts_login_session():
    block = V.agents(_LOGIN, _ADDR, jobs=V.JOBS)
    lines = block.split("\n")
    assert lines[3] == f'test "$(id -un)" = {V.VENUE_ACCOUNT}'  # never root, never the owner
    assert "sudo" not in block and "system/" not in block
    assert "d=gui/$(id -u)" in lines and 'launchctl print "$d" > /dev/null' in lines
    assert block.count('launchctl bootstrap "$d" "$f"') == 3
    assert f'f="{V.VENUE_HOME}/Library/LaunchAgents/$l.plist"' in lines
    # A daemon of the same label still installed stops the block before its agent.
    assert block.count('test ! -e "/Library/LaunchDaemons/$l.plist"') == 3


def test_the_agents_block_needs_a_job_and_knows_only_the_three():
    with pytest.raises(V.AndroidEmuError):
        V.agents(_LOGIN, _ADDR, jobs=())
    with pytest.raises(V.AndroidEmuError):
        V.agents(_LOGIN, _ADDR, jobs={"adb"})
    with pytest.raises(V.AndroidEmuError):
        V.daemons(_LOGIN, _ADDR, without={"adb"})


@pytest.mark.parametrize("argv, rc", [
    (["--agents", "--tunnel", _LOGIN, _ADDR], 0), (["--agents", "--play", "--emulator", _LOGIN, _ADDR], 0),
    (["--daemons", "--no-play", _LOGIN, _ADDR], 0), (["--agents", "--no-tunnel"], 2),
    (["--daemons", "--tunnel"], 2), (["--agents", "--adb"], 2),
])
def test_the_command_line_selects_jobs_per_generator(argv, rc, capsys):
    assert V.main(argv) == rc


def test_the_daemons_block_installs_into_launch_daemons_through_sudo():
    block = V.daemons(_LOGIN, _ADDR)
    assert block.count("sudo install -o root -g wheel -m 644") == 3
    assert block.count('sudo launchctl bootstrap system "$f"') == 3
    assert "f=/Library/LaunchDaemons/$l.plist" in block


def test_the_netcheck_daemon_runs_nc_once_and_is_removed():
    block = V.netcheck(_ADDR)
    assert f"l={V.NETCHECK_LABEL}" in block
    assert "sudo launchctl print system/$l | grep 'last exit code'" in block
    assert 'sudo rm -f "$f"' in block
    plist = plistlib.loads(V._daemon_plist(
        V.NETCHECK_LABEL, ["/usr/bin/nc", "-z", "-w", "5", _ADDR, "22"],
        keep_alive=False, log="/tmp/fauna-emulator-netcheck.log").encode())
    assert "KeepAlive" not in plist and plist["UserName"] == V.VENUE_ACCOUNT


# ── the Play setup ──────────────────────────────────────────────────────────


def test_the_play_setup_refuses_while_a_tunnel_runs_and_installs_nothing():
    block = V.play_setup()
    assert f"pgrep -u {V.VENUE_ACCOUNT} -f fauna_android_e2e_tunnel" in block
    assert "ditto -x -k" in block and "unzip" not in block
    assert "launchctl" not in block.replace(f"sudo launchctl bootout system/{V.TUNNEL_LABEL}", "")
    assert f"-avd {V.PLAY_AVD_NAME} -port {V.LEGS[1].console_port}" in block
    assert V.PLAY_IMAGE_SHA1 in block


def _stub(path, body):
    path.write_text("#!/bin/sh\n" + body)
    path.chmod(0o755)


@pytest.fixture
def play_host(tmp_path):
    """The emulator setup account's machine as the Play block sees it: a scratch home,
    every outside program a stub that logs its call."""
    bin_dir, home = tmp_path / "bin", tmp_path / "home"
    bin_dir.mkdir()
    log = tmp_path / "calls"
    _stub(bin_dir / "id", f"echo {V.VENUE_ACCOUNT}\n")
    _stub(bin_dir / "pgrep", "exit 1\n")
    _stub(bin_dir / "curl", f'echo curl >> {log}\nwhile [ "$1" != --output ]; do shift; done\n'
                            'echo zip > "$2"\n')
    _stub(bin_dir / "shasum", f'echo shasum >> {log}\necho "$SHA $3"\n')
    _stub(bin_dir / "ditto", f'echo ditto >> {log}\nmkdir -p "$4/arm64-v8a"\n'
                             'touch "$4/arm64-v8a/system.img"\n')
    sdk = home / V.SDK.removeprefix(V.VENUE_HOME + "/")
    (sdk / "emulator").mkdir(parents=True)
    (home / ".android" / "avd").mkdir(parents=True)
    (home / ".android" / "avd" / f"{V.PLAY_AVD_NAME}.ini").write_text("")
    _stub(sdk / "emulator" / "emulator", f"echo emulator >> {log}\n")
    # The block names the SDK by its absolute path in the emulator setup account.
    block = V.play_setup().replace(V.SDK, str(sdk))
    env = {**os.environ, "HOME": str(home), "SHA": V.PLAY_IMAGE_SHA1,
           "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}
    return block, env, log


def test_the_play_setup_downloads_verifies_unpacks_then_boots(play_host):
    block, env, log = play_host
    done = subprocess.run(["sh", "-c", block], env=env, capture_output=True, text=True)
    assert done.returncode == 0, done.stdout + done.stderr
    assert log.read_text().split() == ["curl", "shasum", "ditto", "emulator"]
    assert "PLAY SETUP DONE" in done.stdout


def test_a_checksum_mismatch_unpacks_and_boots_nothing(play_host):
    block, env, log = play_host
    done = subprocess.run(["sh", "-c", block], env={**env, "SHA": "0" * 40},
                          capture_output=True, text=True)
    assert done.returncode != 0 and "CHECKSUM MISMATCH" in done.stdout
    assert log.read_text().split() == ["curl", "shasum"]


# ── the emulator setup adb server unit ───────────────────────────────────────────────


def test_the_unit_runs_the_venue_server_on_its_port_with_the_venue_key_alone():
    unit = V.adb_server_unit("/sdk/adb", "/keys/adbkey")
    assert "ExecStart=/sdk/adb -L tcp:18508 server nodaemon" in unit
    assert "Environment=ADB_VENDOR_KEYS=/keys/adbkey" in unit
    assert "Restart=always" in unit and "WantedBy=default.target" in unit


# ── the probe: is the emulator setup up, from the dev VM ─────────────────────────────

_FAKE_ADB = """#!/bin/sh
printf '%s\\n' "$*" >> "$FAKE_ADB_LOG"
case "$*" in
  *" connect "*) cat "$FAKE_ADB_CONNECT"; exit "$FAKE_ADB_RC" ;;
  *) cat "$FAKE_ADB_DEVICES"; exit "$FAKE_ADB_RC" ;;
esac
"""


@pytest.fixture
def probe_adb(tmp_path, monkeypatch):
    """A fake `adb` first on PATH; `answer(connect, devices, rc)` sets what it
    prints per subcommand, `issued()` reads the recorded argv lines."""
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    _stub(bin_dir / "adb", _FAKE_ADB.removeprefix("#!/bin/sh\n"))
    log, conn, devs = tmp_path / "adb.log", tmp_path / "connect", tmp_path / "devices"
    monkeypatch.setenv("FAKE_ADB_LOG", str(log))
    monkeypatch.setenv("FAKE_ADB_CONNECT", str(conn))
    monkeypatch.setenv("FAKE_ADB_DEVICES", str(devs))
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    def answer(connect="already connected to x", devices="List of devices attached\n", rc=0):
        conn.write_text(connect)
        devs.write_text(devices)
        monkeypatch.setenv("FAKE_ADB_RC", str(rc))

    def issued():
        return log.read_text().splitlines() if log.exists() else []

    answer()
    return answer, issued


def _devices(*rows):
    return "List of devices attached\n" + "".join(f"{s}\t{st}\n" for s, st in rows)


def test_the_probe_connects_each_leg_then_lists_on_the_venue_server(probe_adb):
    answer, issued = probe_adb
    answer(devices=_devices((V.VENUE_SERIAL, "device")))
    V.probe_venue()
    spec = V.ADB_SERVER_SPEC
    assert issued() == [f"-L {spec} connect {V.VENUE_SERIAL}",
                        f"-L {spec} connect {V.PLAY_SERIAL}", f"-L {spec} devices"]


def test_a_down_server_names_the_server_unit(probe_adb):
    answer, _ = probe_adb
    answer(connect="* cannot start server on remote host\nerror: cannot connect to daemon",
           rc=1)
    up, line = V.probe_venue()
    assert not up and V.VENUE_ADB_UNIT in line and "adb server" in line


def test_a_refused_leg_names_the_tunnel_leg(probe_adb):
    answer, _ = probe_adb
    answer(connect="failed to connect to '127.0.0.1:18509': Connection refused")
    up, line = V.probe_venue()
    assert not up and "tunnel leg refused" in line and f"{V.VENUE_SERIAL} (fauna-e2e)" in line


def test_an_unauthorized_device_names_the_venue_key_fingerprint(probe_adb, monkeypatch):
    answer, _ = probe_adb
    monkeypatch.setattr(V, "venue_key_fingerprint", lambda *a: "AA:BB")
    answer(devices=_devices((V.VENUE_SERIAL, "unauthorized")))
    up, line = V.probe_venue()
    assert not up and "unauthorized" in line and "AA:BB" in line


def test_a_booting_device_reads_down_and_says_so(probe_adb):
    answer, _ = probe_adb
    answer(devices=_devices((V.VENUE_SERIAL, "offline")))
    up, line = V.probe_venue()
    assert not up and f"{V.VENUE_SERIAL} (fauna-e2e) offline" in line and "booting" in line


def test_a_ready_device_reads_up_and_names_it(probe_adb):
    answer, _ = probe_adb
    answer(devices=_devices((V.VENUE_SERIAL, "device")))
    assert V.probe_venue() == (True, f"emulator up: {V.ADB_SERVER_SPEC} serves {V.VENUE_SERIAL}")


def test_the_play_probe_names_the_play_serial_not_just_the_venue(probe_adb):
    answer, _ = probe_adb
    answer(devices=_devices((V.VENUE_SERIAL, "device")))
    up, line = V.probe_play()
    assert not up and V.PLAY_SERIAL in line and V.VENUE_SERIAL in line
    answer(devices=_devices((V.VENUE_SERIAL, "device"), (V.PLAY_SERIAL, "device")))
    assert V.probe_play() == (True, f"Play emulator up: {V.ADB_SERVER_SPEC} serves {V.PLAY_SERIAL}")


def test_no_adb_at_all_reads_unknown_never_raises(tmp_path, monkeypatch):
    # Off PATH is not enough: `adb_argv` falls back to the SDK roots, present
    # on the primary dev VM -- so every lookup is pointed at an empty directory.
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("ANDROID_HOME", raising=False)
    monkeypatch.delenv("ANDROID_SDK_ROOT", raising=False)
    up, line = V.probe_venue()
    assert not up and line.startswith("emulator unknown")


# ── the recipes ─────────────────────────────────────────────────────────────


@pytest.mark.parametrize("recipe, argument", [
    ("android-tunnel-spec", "--tunnel"), ("android-emu-adbkey", "--adbkey"),
    ("android-emu-key", "--key"), ("android-emu-authorize", "--authorize"),
    ("android-emu-daemons", "--daemons"), ("android-emu-netcheck", "--netcheck"),
    ("android-emu-play-setup", "--play-setup"), ("android-emu-listeners", "--listeners"),
    ("android-emu-adb-server", "--adb-server"), ("android-emu-probe", "--probe"),
    ("android-emu-agents", "--agents"),
])
def test_the_venue_recipes_run_this_module(recipe, argument):
    with open(os.path.join(_REPO_ROOT, "justfile"), encoding="utf-8") as fh:
        justfile = fh.read()
    body = justfile.split(f"\n{recipe}", 1)
    assert len(body) == 2, f"the justfile has no `{recipe}` recipe"
    head = body[1].split("\n\n", 1)[0]
    assert f"tests/e2e-unified/helpers/android_emu.py {argument}" in head


@pytest.mark.parametrize("retired", ["android-emu-play-agent"])
def test_the_retired_recipes_are_gone(retired):
    with open(os.path.join(_REPO_ROOT, "justfile"), encoding="utf-8") as fh:
        assert f"\n{retired}" not in fh.read()
