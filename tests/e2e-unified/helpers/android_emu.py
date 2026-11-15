"""Android's emulator placement (A2 amended) — its constants, the blocks the owner pastes
on the emulator's machine, and the probe that reads it from the dev VM.

`testing.md` § Default app and nest mode → *A2 amended — the design*: the
emulators run on the machine that can virtualize one, under a dedicated
standard account (`VENUE_ACCOUNT`), as `launchd` DAEMONS; that machine runs NO
adb server and holds no adb private key. One SSH tunnel, dialed from it, carries
remote forwards only -- one per emulator, a dev-VM loopback port to that
emulator's own adbd port (`LEGS`). On the dev VM a emulator adb server of its own
(`ADB_SERVER_SPEC`, the user unit `fauna-android-emu-adb.service`) attaches
each emulator with `adb connect`, trusting it through a emulator key pair loaded
by that server alone (`ADB_VENDOR_KEYS`). So every `adb forward` and `adb
reverse` lands on the dev VM, and a emulator run takes its ports from
`find_free_port()` like a run against a local device.

This module is the ONE home of the emulator setup's numbers and names, and everything
spelled from them is generated here: the dev VM's `authorized_keys` line for
the tunnel key (`authorized_keys_line`, written by `--authorize`), the tunnel's
`ssh` argv (`tunnel_command`), the daemons' plists, and every block the owner
pastes (`--adbkey`, `--key`, `--tunnel`, `--daemons`, `--agents`, `--netcheck`,
`--play-setup`, `--listeners`). `--agents` is the design's fallback: a job
whose daemon check fails (C1 the tunnel, C9 an emulator) runs instead as a user
agent of the emulator setup account, `--daemons --no-<job>` leaving its daemon out. The host side, step by step with checks C1–C12,
lives in the emulator setup's setup notes.

**Every pasted block has the paste-safe shape**: `(`, `set -e`, one short command per line (70
columns or fewer), `)`, with no `#` line, no heredoc and no backslash
continuation -- a terminal copy hard-wraps long lines and macOS's interactive
zsh runs a `#` line as a command. Its first line is an account guard, so a
block pasted into the wrong account stops at once. `check_paste_safe` is the
rule; the tier_1 test runs it over every generator's output.

**Why the emulator setup server is not on adb's default port 5037.** Any bare `adb`
call on the dev VM starts a server there, and a emulator device attached to it
would be handed to every sibling session's bare `adb`. A port of the emulator setup's
own keeps the two apart.
"""

from __future__ import annotations

import base64
import hashlib
import ipaddress
import os
import plistlib
import re
import shlex
import socket
import subprocess
import sys
from dataclasses import dataclass

# ── The constants ───────────────────────────────────────────────────────────

#: The dedicated standard macOS account the whole emulator belongs to.
VENUE_ACCOUNT = "faunaemu"
VENUE_HOME = f"/Users/{VENUE_ACCOUNT}"

#: The emulator setup adb server's port on the dev VM (loopback), and its socket spec --
#: what `--adb-server` is given for a emulator run.
VENUE_ADB_SERVER_PORT = 18508
ADB_SERVER_SPEC = f"tcp:127.0.0.1:{VENUE_ADB_SERVER_PORT}"


@dataclass(frozen=True)
class Leg:
    """One emulator's tunnel leg: the dev VM's loopback port that reaches the
    emulator's adbd on the emulator machine's loopback. The emulator is started
    with its console port named, so its adbd port (console + 1) is fixed."""

    avd: str | None  # None: a leg kept for a seat that has no AVD yet
    dev_vm_port: int
    console_port: int

    @property
    def adbd_port(self) -> int:
        return self.console_port + 1

    @property
    def serial(self) -> str:
        """The device's serial on the emulator setup server: its connect address."""
        return f"127.0.0.1:{self.dev_vm_port}"


#: The AVDs the emulator setup's setup creates.
AVD_NAME = "fauna-e2e"
PLAY_AVD_NAME = "fauna-e2e-play"

#: Every leg of the tunnel: the emulator setup AVD, the Play AVD of the store-install
#: witness, and one kept for the second seat's.
LEGS = (
    Leg(AVD_NAME, 18509, 5554),
    Leg(PLAY_AVD_NAME, 18510, 5556),
    Leg(None, 18511, 5558),
)

#: The serial of the emulator setup's standing AVD. A run that names no other device
#: drives this one: with the Play AVD up beside it adb with no `-s` refuses, so
#: the verify queue's drain names it (delayed-testing.md § The verify queue).
VENUE_SERIAL = LEGS[0].serial
PLAY_SERIAL = LEGS[1].serial

#: The emulator setup's adb key pair on the dev VM: the private half is read by the
#: emulator server alone (`ADB_VENDOR_KEYS`); the public half is the emulator setup
#: account's `~/.android/adbkey.pub`, which the emulator hands its guest.
VENUE_ADB_KEY = os.path.join(os.path.expanduser("~"), ".config", "fauna",
                             "android-emu", "adbkey")

#: The emulator setup adb server's user unit on the dev VM.
VENUE_ADB_UNIT = "fauna-android-emu-adb.service"

#: The tunnel's forwarding-only key, in the emulator setup account's home.
TUNNEL_KEY = f"{VENUE_HOME}/.ssh/fauna_android_e2e_tunnel"

#: The comment every emulator `authorized_keys` line carries; `--authorize`
#: replaces the line carrying it, and check C6 counts it.
TUNNEL_KEY_COMMENT = "fauna android e2e tunnel"

#: The dev VM's `authorized_keys` the tunnel key lands in.
AUTHORIZED_KEYS = os.path.join(os.path.expanduser("~"), ".ssh", "authorized_keys")

#: One dead loopback port: the key's only `permitopen`, so a key lifted out of
#: the emulator setup account cannot open a local forward to any dev-VM service.
DEAD_PERMITOPEN = "127.0.0.1:1"

#: The SDK, hand-unpacked into the emulator setup account's home (host.md step 2).
SDK = f"{VENUE_HOME}/Library/Android/sdk"

#: The daemons' labels, and the throwaway one of check C1.
EMULATOR_LABEL = "social.fauna.android-emulator"
PLAY_EMULATOR_LABEL = "social.fauna.android-emulator-play"
TUNNEL_LABEL = "social.fauna.android-tunnel"
NETCHECK_LABEL = "social.fauna.emulator-netcheck"
DAEMON_DIR = "/Library/LaunchDaemons"
AGENT_DIR = f"{VENUE_HOME}/Library/LaunchAgents"

#: The emulator setup's three standing jobs by the name a flag gives them. Each runs as a
#: daemon (step 8) or -- the fallback of a failed C1 (the tunnel) or C9 (an
#: emulator) -- as a user agent of the emulator setup account (`agents`), never both.
JOBS = {"tunnel": TUNNEL_LABEL, "emulator": EMULATOR_LABEL, "play": PLAY_EMULATOR_LABEL}

#: The Play system image of the store-install witness (host.md step 2: read
#: 2026-11-04 from Google's listing, revision 9). The listing publishes SHA-1
#: alone for a system image.
PLAY_IMAGE_URL = ("https://dl.google.com/android/repository/sys-img/"
                  "google_apis_playstore/arm64-v8a-35_r09.zip")
PLAY_IMAGE_BYTES = 1789211399
PLAY_IMAGE_SHA1 = "b01416bc0bca12a85295dad179f9b8a1cae3d1e6"

#: The widest line a pasted block may carry.
PASTE_WIDTH = 70


class AndroidEmuError(RuntimeError):
    """The emulator setup cannot serve the request -- a malformed argument, a block that
    would break the paste rule."""


# ── The paste-safe block ────────────────────────────────────────────────────


def check_paste_safe(block: str) -> None:
    """Raise unless `block` has the paste-safe shape (module docstring)."""
    lines = block.split("\n")
    problems = []
    if lines[:2] != ["(", "set -e"] or lines[-1] != ")":
        problems.append("does not open with `(` + `set -e` and close with `)`")
    for n, line in enumerate(lines, 1):
        if len(line) > PASTE_WIDTH:
            problems.append(f"line {n} is {len(line)} columns: {line!r}")
        if line.lstrip().startswith("#"):
            problems.append(f"line {n} is a comment")
        if "<<" in line:
            problems.append(f"line {n} is a heredoc")
        if line.rstrip().endswith("\\"):
            problems.append(f"line {n} continues with a backslash")
        if "\t" in line:
            problems.append(f"line {n} carries a tab")
    if problems:
        raise AndroidEmuError("not paste-safe: " + "; ".join(problems))


def _guard(for_venue: bool) -> list[str]:
    if for_venue:
        return [f"echo 'For the {VENUE_ACCOUNT} account Terminal.'",
                f'test "$(id -un)" = {VENUE_ACCOUNT}']
    return ["echo 'For the admin account Terminal (sudo).'",
            f'test "$(id -un)" != {VENUE_ACCOUNT}']


def _block(body: list[str], *, for_venue: bool) -> str:
    block = "\n".join(["(", "set -e", *_guard(for_venue), *body, ")"])
    check_paste_safe(block)
    return block


def _sq(text: str) -> str:
    """`text` single-quoted for sh."""
    return "'" + text.replace("'", "'\\''") + "'"


def _emit_text(text: str, width: int = PASTE_WIDTH) -> list[str]:
    """Lines that, run inside a `{ … }` group, print `text` byte for byte:
    each text line as `printf %s '<piece>'` pieces within the paste width, then
    `echo` for its newline (none after a last line that has none)."""
    out = []
    lines = text.split("\n")
    for n, line in enumerate(lines):
        rest = line
        while rest:
            piece = rest[:width]
            while len("printf %s " + _sq(piece)) > width:
                piece = piece[:-1]
            out.append("printf %s " + _sq(piece))
            rest = rest[len(piece):]
        if n < len(lines) - 1:
            out.append("echo")
    return out


def _write_file(path_var: str, text: str) -> list[str]:
    """Write `text` to the file named by shell variable `path_var`."""
    return ["{", *_emit_text(text), f'}} > "${path_var}"']


# ── The dev VM's authorized_keys line ───────────────────────────────────────

_ED25519 = re.compile(r"\s*ssh-ed25519\s+(AAAAC3NzaC1lZDI1NTE5[A-Za-z0-9+/]+={0,2})(\s.*)?\Z")


def authorized_keys_line(public_key: str, address: str) -> str:
    """The ONE line the tunnel key is accepted by: from the address the
    emulator's machine dials from, a forced `/bin/false`, `restrict`, port
    forwarding, one `permitlisten` per leg and a single dead `permitopen`."""
    match = _ED25519.match(public_key or "")
    if not match or "\n" in public_key or "\r" in public_key:
        raise AndroidEmuError("the key is not one ssh-ed25519 public key")
    try:
        ip = ipaddress.IPv4Address((address or "").strip())
    except ipaddress.AddressValueError as exc:
        raise AndroidEmuError(f"{address!r} is not an IPv4 address") from exc
    options = [f'from="{ip}"', 'command="/bin/false"', "restrict", "port-forwarding"]
    options += [f'permitlisten="127.0.0.1:{leg.dev_vm_port}"' for leg in LEGS]
    options.append(f'permitopen="{DEAD_PERMITOPEN}"')
    return f"{','.join(options)} ssh-ed25519 {match.group(1)} {TUNNEL_KEY_COMMENT}"


def authorize(public_key: str, address: str, path: str = AUTHORIZED_KEYS) -> str:
    """Replace any earlier emulator line in `path` with the generated one; return
    check C6's reading (`<count> line(s) carry '<comment>'`)."""
    line = authorized_keys_line(public_key, address)
    try:
        with open(path, encoding="utf-8") as fh:
            kept = [ln for ln in fh.read().splitlines() if TUNNEL_KEY_COMMENT not in ln]
    except FileNotFoundError:
        kept = []
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    tmp = f"{path}.fauna-emulator.tmp"
    with open(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w",
              encoding="utf-8") as fh:
        fh.write("\n".join([*kept, line]) + "\n")
    os.replace(tmp, path)
    with open(path, encoding="utf-8") as fh:
        count = sum(TUNNEL_KEY_COMMENT in ln for ln in fh)
    return f"C6: {count} line(s) in {path} carry '{TUNNEL_KEY_COMMENT}' (want 1)"


# ── The tunnel ──────────────────────────────────────────────────────────────

_SSH_WORD = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")

#: The tunnel's options, after `-N` (host.md step 7, verbatim).
TUNNEL_OPTIONS = (
    "-F", "none", "-i", TUNNEL_KEY,
    "-o", "IdentitiesOnly=yes", "-o", "IdentityAgent=none",
    "-o", "ForwardAgent=no", "-o", "ForwardX11=no",
    "-o", "StrictHostKeyChecking=yes", "-o", "PermitLocalCommand=no",
    "-o", "LogLevel=ERROR", "-o", "BatchMode=yes",
    "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=30",
)


def tunnel_forwards() -> list[tuple[str, int, int]]:
    """Every forward of the tunnel as `(flag, dev-VM port, adbd port)` --
    remote forwards only."""
    return [("-R", leg.dev_vm_port, leg.adbd_port) for leg in LEGS]


def tunnel_command(login: str, dev_vm_address: str) -> list[str]:
    """The `ssh` argv the emulator's machine runs to dial the dev VM."""
    for name, value in (("login", login), ("address", dev_vm_address)):
        if not _SSH_WORD.match(value or ""):
            raise AndroidEmuError(f"tunnel {name} {value!r} is not a plain user/host word")
    argv = ["/usr/bin/ssh", "-N", *TUNNEL_OPTIONS]
    for flag, listen, target in tunnel_forwards():
        argv += [flag, f"127.0.0.1:{listen}:127.0.0.1:{target}"]
    return [*argv, f"{login}@{dev_vm_address}"]


def _this_machine_address() -> str:
    """The address this machine's default route sources from. A UDP `connect`
    sends nothing; it only makes the kernel pick the interface."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.connect(("192.0.2.1", 9))  # TEST-NET-1, never routed
        return s.getsockname()[0]


def _defaults(login: str, address: str) -> tuple[str, str]:
    import getpass

    return login or getpass.getuser(), address or _this_machine_address()


def tunnel_spec(login: str = "", dev_vm_address: str = "") -> str:
    """What `just android-tunnel-spec` prints: the by-hand tunnel (step 7), its
    argv built up over short `set --` lines."""
    login, dev_vm_address = _defaults(login, dev_vm_address)
    argv = tunnel_command(login, dev_vm_address)
    body = ["set -- -N"]
    rest = argv[2:]
    i = 0
    while i < len(rest):
        word = rest[i]
        pair = [word, rest[i + 1]] if word in ("-o", "-i", "-F", "-R") else [word]
        body.append('set -- "$@" ' + " ".join(shlex.quote(w) for w in pair))
        i += len(pair)
    body += ["echo 'Tunnel up; Ctrl-C ends it.'", f"{argv[0]} \"$@\""]
    return _block(body, for_venue=True)


# ── The daemons ─────────────────────────────────────────────────────────────


def _log(label: str) -> str:
    return f"{VENUE_HOME}/Library/Logs/fauna-{label.removeprefix('social.fauna.')}.log"


def emulator_command(leg: Leg) -> list[str]:
    """The emulator's command line in a daemon: headless, software rendering,
    both cameras off, no audio (testing.md, A2 amended points 1 and 3)."""
    return [f"{SDK}/emulator/emulator", "-avd", leg.avd, "-port", str(leg.console_port),
            "-no-window", "-no-audio", "-no-boot-anim", "-no-snapshot", "-no-metrics",
            "-gpu", "swiftshader_indirect", "-camera-back", "none", "-camera-front", "none"]


def _daemon_plist(label: str, command: list[str], *, keep_alive: bool = True,
                  log: str | None = None, agent: bool = False) -> str:
    """One job's plist: a daemon that runs as the emulator setup account, or (`agent`)
    a user agent of it -- the same keys but `UserName`/`GroupName`, which an
    agent takes from its login session. Output is truncated at every start
    (`exec … > log 2>&1`), so a log holds one run."""
    log = log or _log(label)
    shell = "exec " + " ".join(shlex.quote(w) for w in command) + f" > {shlex.quote(log)} 2>&1"
    body = {"Label": label}
    if not agent:
        body |= {"UserName": VENUE_ACCOUNT, "GroupName": "staff"}
    body |= {
        "ProgramArguments": ["/bin/sh", "-c", shell],
        "EnvironmentVariables": {"HOME": VENUE_HOME, "ANDROID_HOME": SDK},
        "RunAtLoad": True,
    }
    if keep_alive:
        body["KeepAlive"] = True
    text = plistlib.dumps(body, sort_keys=False).decode()
    return text.replace("\t", "  ")


def _check_jobs(jobs) -> frozenset[str]:
    jobs = frozenset(jobs)
    unknown = jobs - set(JOBS)
    if unknown:
        raise AndroidEmuError(f"no emulator job {sorted(unknown)}; the jobs are {sorted(JOBS)}")
    return jobs


def _job_plists(login: str, dev_vm_address: str, jobs, *, agent: bool) -> dict[str, str]:
    commands = {
        "emulator": lambda: emulator_command(LEGS[0]),
        "play": lambda: emulator_command(LEGS[1]),
        "tunnel": lambda: tunnel_command(login, dev_vm_address),
    }
    return {JOBS[job]: _daemon_plist(JOBS[job], commands[job](), agent=agent)
            for job in ("emulator", "play", "tunnel") if job in jobs}


def daemon_plists(login: str, dev_vm_address: str, *, without=()) -> dict[str, str]:
    """Every standing daemon's plist, by label, but the jobs in `without`
    (those run as user agents instead -- `agent_plists`)."""
    return _job_plists(login, dev_vm_address, set(JOBS) - _check_jobs(without), agent=False)


def agent_plists(login: str, dev_vm_address: str, jobs) -> dict[str, str]:
    """The user agents of the emulator setup account for `jobs`, by label: the daemons'
    labels, command lines and logs, in the account's login session."""
    return _job_plists(login, dev_vm_address, _check_jobs(jobs), agent=True)


def _install_daemon(label: str, plist: str) -> list[str]:
    return [
        f"l={label}",
        't=$(mktemp -d)/p.plist',
        *_write_file("t", plist),
        'plutil -lint "$t"',
        'f=/Library/LaunchDaemons/$l.plist',
        'sudo launchctl bootout system/$l 2>/dev/null || true',
        'sudo install -o root -g wheel -m 644 "$t" "$f"',
        'sudo launchctl bootstrap system "$f"',
        'echo "installed $l"',
    ]


def daemons(login: str = "", dev_vm_address: str = "", *, without=()) -> str:
    """What `just android-emu-daemons` prints (step 8): the admin block that
    writes and bootstraps every standing daemon but the jobs in `without` --
    the fallback of a failed C1 (`tunnel`) or C9 (`emulator`, `play`), each of
    those then a user agent of the emulator setup account (`agents`). A left-out job's
    daemon is booted out and its plist removed, so one label is never both."""
    login, dev_vm_address = _defaults(login, dev_vm_address)
    body = [f"sudo -u {VENUE_ACCOUNT} mkdir -p {VENUE_HOME}/Library/Logs"]
    for label, plist in daemon_plists(login, dev_vm_address, without=without).items():
        body += _install_daemon(label, plist)
    for job in sorted(_check_jobs(without)):
        body += [
            f"l={JOBS[job]}",
            "sudo launchctl bootout system/$l 2>/dev/null || true",
            f'sudo rm -f "{DAEMON_DIR}/$l.plist"',
            'echo "no daemon $l"',
        ]
    body.append("echo 'On the dev VM: just android-emu-probe'")
    return _block(body, for_venue=False)


def _install_agent(label: str, plist: str) -> list[str]:
    return [
        f"l={label}",
        f'test ! -e "{DAEMON_DIR}/$l.plist"',
        't=$(mktemp -d)/p.plist',
        *_write_file("t", plist),
        'plutil -lint "$t"',
        f'f="{AGENT_DIR}/$l.plist"',
        'launchctl bootout "$d/$l" 2>/dev/null || true',
        'install -m 644 "$t" "$f"',
        'launchctl bootstrap "$d" "$f"',
        'echo "installed $l"',
    ]


def agents(login: str = "", dev_vm_address: str = "", *, jobs=()) -> str:
    """What `just android-emu-agents` prints: the fallback of a failed C1
    (`tunnel`) or C9 (`emulator`, `play`) -- those jobs as user agents of the
    emulator account, pasted into a Terminal inside that account's own login
    session, which the owner leaves open in the background (the emulator setup is down
    after a restart until that login). A job still installed as a daemon stops
    the block before its agent is written: run `daemons(without=jobs)` first."""
    jobs = _check_jobs(jobs)
    if not jobs:
        raise AndroidEmuError(f"name the jobs to run as user agents: {sorted(JOBS)}")
    login, dev_vm_address = _defaults(login, dev_vm_address)
    body = [
        "d=gui/$(id -u)",
        'launchctl print "$d" > /dev/null',
        f"mkdir -p {VENUE_HOME}/Library/Logs",
        f"mkdir -p {AGENT_DIR}",
    ]
    for label, plist in agent_plists(login, dev_vm_address, jobs).items():
        body += _install_agent(label, plist)
    body.append("sleep 10")
    for label in agent_plists(login, dev_vm_address, jobs):
        body += [f"l={label}", 'launchctl print "$d/$l" | grep "state ="']
    body += ["echo 'Leave this login open in the background.'",
             "echo 'On the dev VM: just android-emu-probe'"]
    return _block(body, for_venue=True)


def netcheck(dev_vm_address: str = "") -> str:
    """What `just android-emu-netcheck` prints (check C1): a throwaway daemon
    of the emulator setup account running the system's `nc` against the dev VM's SSH
    port, its exit code read, then removed."""
    _, dev_vm_address = _defaults("x", dev_vm_address)
    if not _SSH_WORD.match(dev_vm_address):
        raise AndroidEmuError(f"address {dev_vm_address!r} is not a plain host word")
    plist = _daemon_plist(NETCHECK_LABEL, ["/usr/bin/nc", "-z", "-w", "5", dev_vm_address, "22"],
                          keep_alive=False, log="/tmp/fauna-emulator-netcheck.log")
    body = [
        *_install_daemon(NETCHECK_LABEL, plist),
        "sleep 10",
        "sudo launchctl print system/$l | grep 'last exit code'",
        "sudo launchctl bootout system/$l || true",
        'sudo rm -f "$f"',
        "echo 'C1 passes on: last exit code = 0'",
    ]
    return _block(body, for_venue=False)


# ── The keys ────────────────────────────────────────────────────────────────


def adbkey_block(pub_path: str | None = None) -> str:
    """What `just android-emu-adbkey` prints (check C4a): the emulator setup key's
    PUBLIC half written to the emulator setup account's `~/.android/adbkey.pub` in short
    pieces, then its checksum compared with this machine's."""
    pub_path = pub_path or f"{VENUE_ADB_KEY}.pub"
    with open(pub_path, "rb") as fh:
        data = fh.read()
    text = data.decode("ascii")
    digest = hashlib.sha256(data).hexdigest()
    body = [
        "mkdir -p ~/.android",
        "k=~/.android/adbkey.pub",
        *_write_file("k", text),
        f"s={digest[:32]}",
        f"s=${{s}}{digest[32:]}",
        'h=$(shasum -a 256 "$k")',
        'test "${h%% *}" = "$s"',
        "echo 'C4a: OK'",
    ]
    return _block(body, for_venue=True)


def venue_key_fingerprint(pub_path: str | None = None) -> str | None:
    """The emulator setup key's fingerprint as the guest's "Allow USB debugging?"
    dialog prints it (MD5 of the key blob), or None with no key."""
    try:
        with open(pub_path or f"{VENUE_ADB_KEY}.pub", "rb") as fh:
            blob = base64.b64decode(fh.read().split()[0])
    except (OSError, IndexError, ValueError):
        return None
    digest = hashlib.md5(blob).hexdigest().upper()  # adb's own display form, not a security use
    return ":".join(digest[i:i + 2] for i in range(0, len(digest), 2))


def key_block(dev_vm_address: str = "") -> str:
    """What `just android-emu-key` prints (step 6): the tunnel key made once,
    its public half printed, and the address the emulator's machine dials the
    dev VM from."""
    _, dev_vm_address = _defaults("x", dev_vm_address)
    if not _SSH_WORD.match(dev_vm_address):
        raise AndroidEmuError(f"address {dev_vm_address!r} is not a plain host word")
    body = [
        "mkdir -p -m 700 ~/.ssh",
        "k=~/.ssh/fauna_android_e2e_tunnel",
        f"c={_sq(TUNNEL_KEY_COMMENT)}",
        'test -e "$k" || ssh-keygen -q -t ed25519 -N "" -C "$c" -f "$k"',
        f"a={dev_vm_address}",
        "i=$(route -n get \"$a\" | awk '/interface:/{print $2}')",
        "echo 'On the dev VM: just android-emu-authorize KEY ADDRESS'",
        "echo 'KEY (the whole line, quoted):'",
        'cat "$k.pub"',
        "echo 'ADDRESS:'",
        'ipconfig getifaddr "$i"',
    ]
    return _block(body, for_venue=True)


# ── The Play AVD's windowed sign-in ─────────────────────────────────────────


def play_setup() -> str:
    """What `just android-emu-play-setup` prints (step 5): refuses while a
    tunnel of the emulator setup account runs, downloads and verifies the Play image
    when missing, unpacks it with `ditto`, and boots the Play AVD with a window
    for the owner's sign-in. It installs nothing afterwards: the Play
    emulator's daemon is part of the daemons block."""
    leg = LEGS[1]
    url_head, url_tail = PLAY_IMAGE_URL.rsplit("/sys-img/", 1)
    body = [
        f"if pgrep -u {VENUE_ACCOUNT} -f fauna_android_e2e_tunnel >/dev/null; then",
        "echo 'An emulator tunnel runs. From the admin Terminal first:'",
        f"echo 'sudo launchctl bootout system/{TUNNEL_LABEL}'",
        "exit 1",
        "fi",
        f"sdk={SDK}",
        'e="$sdk/emulator/emulator"',
        'test -x "$e"',
        f'test -f ~/.android/avd/{PLAY_AVD_NAME}.ini',
        'img="$sdk/system-images/android-35/google_apis_playstore"',
        'if test ! -f "$img/arm64-v8a/system.img"; then',
        "d=~/fauna-sdk-downloads/play-image",
        'mkdir -p "$d"',
        'z="$d/arm64-v8a-35_r09.zip"',
        f"u={url_head}/sys-img/",
        f"u=${{u}}{url_tail}",
        f"echo 'Downloading the Play image ({PLAY_IMAGE_BYTES} bytes)'",
        'test -f "$z" || curl -fL --output "$z.part" "$u"',
        'test -f "$z" || mv "$z.part" "$z"',
        'h=$(shasum -a 1 "$z")',
        f'if test "${{h%% *}}" != {PLAY_IMAGE_SHA1}; then',
        'rm -f "$z"',
        "echo 'CHECKSUM MISMATCH: nothing unpacked'",
        "exit 1",
        "fi",
        "echo 'CHECKSUM OK'",
        'rm -rf "$d/unpacked"',
        'ditto -x -k "$z" "$d/unpacked"',
        'mkdir -p "$img"',
        'mv "$d/unpacked/arm64-v8a" "$img/arm64-v8a"',
        'rm -rf "$d/unpacked"',
        "fi",
        "echo 'Sign the Play Store in to the THROWAWAY tester account,'",
        "echo 'then power the emulator off from its own menu.'",
        f'"$e" -avd {PLAY_AVD_NAME} -port {leg.console_port} -no-audio </dev/null',
        "echo 'PLAY SETUP DONE'",
    ]
    return _block(body, for_venue=True)


# ── What listens (C11, C12) ─────────────────────────────────────────────────


def listeners() -> str:
    """What `just android-emu-listeners` prints (checks C11, C12)."""
    body = [
        "sudo lsof -nP -iTCP -sTCP:LISTEN",
        "sudo lsof -nP -iUDP",
        "echo 'C11: no faunaemu line on * or a non-loopback address'",
        "if pgrep -lf 'adb.*server'; then",
        "echo 'C11 FAILS: an adb server runs on this machine'",
        "exit 1",
        "fi",
        "echo 'C11: no adb server: OK'",
        "echo 'C12: every loopback line above is reachable from the guest'",
    ]
    return _block(body, for_venue=False)


# ── The emulator setup adb server on the dev VM ──────────────────────────────────────


def adb_server_unit(adb: str, key: str = VENUE_ADB_KEY) -> str:
    """The user unit that keeps the emulator setup adb server up on the dev VM."""
    return "\n".join([
        "[Unit]",
        f"Description=Fauna android emulator adb server ({ADB_SERVER_SPEC})",
        "",
        "[Service]",
        f"Environment=ADB_VENDOR_KEYS={key}",
        # The LISTEN spec names no host: adb's server refuses one ("listening
        # on specified hostname currently unsupported", measured 2026-11-11)
        # and binds loopback alone without `-a`. Clients keep the explicit
        # host (`resolve_adb_server`).
        f"ExecStart={adb} -L tcp:{VENUE_ADB_SERVER_PORT} server nodaemon",
        "Restart=always",
        "RestartSec=5",
        "",
        "[Install]",
        "WantedBy=default.target",
        "",
    ])


def adb_public_key(private_pem: str) -> str:
    """adb's public-key form of an RSA-2048 private key: base64 of its
    524-byte blob (word count, n0inv, modulus and R^2 mod n as little-endian
    32-bit words, exponent). Derived here rather than by `adb keygen`, which
    segfaults on the primary dev VM (an x86-64 adb under emulation; measured
    2026-11-11); the derivation is pinned against a key pair adb itself made."""
    import struct

    out = subprocess.run(["openssl", "rsa", "-in", private_pem, "-noout", "-modulus"],
                         capture_output=True, text=True, check=True).stdout
    n = int(out.strip().split("=", 1)[1], 16)
    if n.bit_length() != 2048:
        raise AndroidEmuError(f"{private_pem} is not an RSA-2048 key")
    words = 64
    n0inv = (-pow(n, -1, 1 << 32)) % (1 << 32)
    rr = pow(2, 2 * 32 * words, n)
    blob = (struct.pack("<II", words, n0inv) + n.to_bytes(256, "little")
            + rr.to_bytes(256, "little") + struct.pack("<I", 65537))
    return base64.b64encode(blob).decode()


def make_venue_key(path: str = VENUE_ADB_KEY) -> None:
    """The emulator setup's adb key pair, made once: a PKCS#8 RSA-2048 private half
    (mode 600, as adb writes its own) and `<path>.pub` in adb's form."""
    os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
    if not os.path.exists(path):
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA",
                        "-pkeyopt", "rsa_keygen_bits:2048", "-out", path],
                       check=True, capture_output=True)
    os.chmod(path, 0o600)
    if not os.path.exists(f"{path}.pub"):
        with open(f"{path}.pub", "w", encoding="ascii") as fh:
            fh.write(f"{adb_public_key(path)} fauna-android-emu")


def install_adb_server() -> str:
    """What `just android-emu-adb-server` does on the dev VM: make the emulator setup
    key pair once (`make_venue_key`), write the user unit,
    and enable and start it. Returns one line saying what it found."""
    from helpers import android_device

    adb = android_device.adb_argv(None)[0]
    if not os.path.isabs(adb):
        raise AndroidEmuError("adb is not installed on this machine")
    make_venue_key()
    unit_dir = os.path.join(os.path.expanduser("~"), ".config", "systemd", "user")
    os.makedirs(unit_dir, exist_ok=True)
    with open(os.path.join(unit_dir, VENUE_ADB_UNIT), "w", encoding="utf-8") as fh:
        fh.write(adb_server_unit(adb))
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "enable", "--now", VENUE_ADB_UNIT], check=True)
    subprocess.run(["systemctl", "--user", "restart", VENUE_ADB_UNIT], check=True)
    state = subprocess.run(["systemctl", "--user", "is-active", VENUE_ADB_UNIT],
                           capture_output=True, text=True).stdout.strip()
    return f"{VENUE_ADB_UNIT}: {state}; key {VENUE_ADB_KEY} ({venue_key_fingerprint()})"


# ── The probe: is the emulator setup up, read from the dev VM ────────────────────────


def _adb(args: list[str], timeout: float):
    from helpers import android_device

    return subprocess.run([*android_device.adb_argv(None, ADB_SERVER_SPEC), *args],
                          capture_output=True, text=True, timeout=timeout)


def _leg_name(serial: str) -> str:
    for leg in LEGS:
        if leg.serial == serial:
            return f"{serial} ({leg.avd or 'second seat'})"
    return serial


def connect_legs(serials=None, timeout: float = 5) -> tuple[bool, dict[str, str]]:
    """`adb connect` each leg on the emulator setup server (idempotent) -> `(server
    answered, {serial: adb's reply})`. Raises only what `subprocess` raises."""
    replies = {}
    for serial in serials or [leg.serial for leg in LEGS if leg.avd]:
        result = _adb(["connect", serial], timeout)
        reply = (result.stdout + result.stderr).strip()
        if result.returncode != 0 and "connect" not in reply.lower():
            return False, {}
        if "cannot connect to daemon" in reply or "daemon not running" in reply:
            return False, {}
        replies[serial] = reply
    return True, replies


def _refused(reply: str) -> bool:
    low = reply.lower()
    return "failed to connect" in low or "refused" in low or "cannot connect" in low


def venue_devices(timeout: float = 5, serials=None) -> tuple[list, str]:
    """`(the serials in state device, one line saying why)`, asked of the
    emulator's adb server on this machine; no serial when it is down. Each leg is
    `adb connect`ed first. The down-line names the leg: the server unit, a
    refused tunnel leg, an `unauthorized` device (with the emulator setup key's
    fingerprint), a booting one. The verify queue's drain reads the serials.

    Built from `android_device.adb_argv` with the emulator setup's spec: only `-L
    tcp:127.0.0.1:<port>` makes a client facing a dead port exit without
    starting a server of its own there (`resolve_adb_server`)."""
    from helpers import android_device

    unit = f"systemctl --user status {VENUE_ADB_UNIT}"
    try:
        answered, replies = connect_legs(serials, timeout)
        if not answered:
            return [], (f"emulator down: the emulator setup adb server at {ADB_SERVER_SPEC} is down "
                        f"-- {unit}; `just android-emu-adb-server` installs it")
        result = _adb(["devices"], timeout)
    except subprocess.TimeoutExpired:
        return [], (f"emulator down: the emulator setup adb server at {ADB_SERVER_SPEC} did not "
                    f"answer in {timeout:g} s -- {unit}")
    except OSError as exc:
        return [], f"emulator unknown: adb cannot run on this machine ({exc.strerror or exc})"
    if result.returncode != 0:
        return [], (f"emulator down: the emulator setup adb server at {ADB_SERVER_SPEC} is down "
                    f"-- {unit}")
    rows = dict(android_device.parse_adb_devices(result.stdout))
    ready = [serial for serial, state in rows.items() if state == "device"]
    if ready:
        return ready, f"emulator up: {ADB_SERVER_SPEC} serves {', '.join(ready)}"
    refused = [s for s, reply in replies.items() if _refused(reply) and s not in rows]
    if refused:
        return [], ("emulator down: tunnel leg refused for "
                    f"{', '.join(_leg_name(s) for s in refused)} -- the tunnel from the "
                    f"emulator's machine is not up, or the emulator setup is not installed")
    unauthorized = [s for s, state in rows.items() if state == "unauthorized"]
    if unauthorized:
        return [], (f"emulator down: {', '.join(_leg_name(s) for s in unauthorized)} "
                    f"unauthorized -- the guest does not trust the emulator setup key "
                    f"{venue_key_fingerprint() or '(no key on this machine)'}")
    seen = ", ".join(f"{_leg_name(s)} {state}" for s, state in rows.items()) or "none attached"
    return [], (f"emulator down: the tunnel answers but no device is ready ({seen}) "
                f"-- the emulator is still booting or stopped")


def probe_venue(timeout: float = 5) -> tuple[bool, str]:
    """`(up, one line saying why)` -- `venue_devices` read as up or down."""
    ready, line = venue_devices(timeout)
    return bool(ready), line


def probe_play(timeout: float = 5) -> tuple[bool, str]:
    """`(up, one line)` for the Play emulator alone."""
    ready, line = venue_devices(timeout)
    if PLAY_SERIAL in ready:
        return True, f"Play emulator up: {ADB_SERVER_SPEC} serves {PLAY_SERIAL}"
    if ready:
        return False, (f"Play emulator down: the emulator setup answers ({', '.join(ready)}) but "
                       f"{PLAY_SERIAL} is not ready -- its daemon is not installed or it "
                       f"is still booting")
    return False, f"Play emulator down: {line}"


# ── The command line the recipes run ────────────────────────────────────────

_USAGE = """usage: android_emu.py [--tunnel|--adbkey|--key|
       --daemons [--no-tunnel] [--no-emulator] [--no-play]|
       --agents [--tunnel] [--emulator] [--play]|--netcheck|--play-setup|--listeners|--probe [--play]|--adb-server|
       --authorize '<public key>' <address>] [login] [address]"""


def main(argv: list[str]) -> int:
    flag, rest = (argv[0], argv[1:]) if argv and argv[0].startswith("--") else ("--tunnel", argv)
    rest = [a for a in rest if a]
    if flag == "--probe":
        up, line = probe_play() if rest[:1] == ["--play"] else probe_venue()
        print(line)
        return 0 if up else 1
    if flag == "--authorize":
        if len(rest) != 2:
            print(_USAGE, file=sys.stderr)
            return 2
        print(authorize(rest[0], rest[1]))
        return 0
    if flag == "--adb-server":
        print(install_adb_server())
        return 0
    if flag in ("--daemons", "--agents"):
        prefix = "--no-" if flag == "--daemons" else "--"
        flags = [a for a in rest if a.startswith("--")]
        named = {a.removeprefix(prefix) for a in flags}
        if any(not a.startswith(prefix) for a in flags) or not named <= set(JOBS):
            print(_USAGE, file=sys.stderr)
            return 2
        rest = [a for a in rest if not a.startswith("--")]
        if flag == "--daemons":
            print(daemons(*rest[:2], without=named))
        else:
            print(agents(*rest[:2], jobs=named))
        return 0
    printers = {
        "--tunnel": lambda: tunnel_spec(*rest[:2]),
        "--adbkey": adbkey_block,
        "--key": lambda: key_block(*rest[-1:]),
        "--netcheck": lambda: netcheck(*rest[-1:]),
        "--play-setup": play_setup,
        "--listeners": listeners,
    }
    if flag not in printers:
        print(_USAGE, file=sys.stderr)
        return 2
    print(printers[flag]())
    return 0


if __name__ == "__main__":
    # The recipes run this file as a script: make `helpers` importable.
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    try:
        sys.exit(main(sys.argv[1:]))
    except AndroidEmuError as exc:
        print(f"android emulator: {exc}", file=sys.stderr)
        sys.exit(2)
