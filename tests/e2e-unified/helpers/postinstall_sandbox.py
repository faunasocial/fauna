"""Run the macOS `.pkg` sync postinstall against a throwaway home, never the box.

The question this answers (2026-10-27): does an UPGRADE over a pre-fix install
reach the log fix? A 0.1.2 `.pkg` (built 2026-08-28, before the redirect was
dropped) left `~/Library/LaunchAgents/social.fauna.sync-agent.plist` pointing
`StandardOutPath`/`StandardErrorPath` at `~/Library/Logs/Fauna/sync-agent.log`,
a file launchd never rotates — on macOS it reached 1.2 GB beside 5.9 GB of the
agent's own then-unbounded daily files (observability.md § Persistence &
privacy, *The file is bounded by construction*). If the next `.pkg` left that
plist alone, every such user would keep the redirect for ever.

The postinstall's upgrade path is `common.sh::install_launchagent`: a plist
whose bytes differ from the shipped heredoc is booted out, rewritten and
re-bootstrapped. Whether it does is a MECHANISM, so it is tested headlessly
here rather than only under the sudo-gated real install: the shipped scripts
run unmodified but for one appended override of `resolve_console_user` (which
asks `scutil`/`dscl` for the GUI user — absolute paths a PATH stub cannot
reach) that names a sandbox home, and `launchctl` and `chown` are PATH stubs
that record their argv and touch nothing. The real launchd, the real home and
the real user are never touched, so this runs as an ordinary user on any
machine with bash.

Callers hand in the script TEXTS, so one harness serves both the source tree
(`tests/test_macos_sync_postinstall_upgrade.py`, tier_1) and the bytes a built
`.pkg` ships (`test_installer.py::TestDryRun`).
"""

import os
import shutil
import subprocess
from pathlib import Path

SYNC_LABEL = "social.fauna.sync-agent"

#: The sync LaunchAgent exactly as the pre-fix `.pkg` wrote it — the
#: `8c0aa05c3c^` postinstall heredoc, `${USER_HOME}`/`${AGENT_EXEC}` filled in
#: by `pre_fix_plist`. Kept verbatim: the upgrade must recognise the real old
#: bytes, not a tidied stand-in.
_PRE_FIX_PLIST = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>social.fauna.sync-agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{agent_exec}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/Library/Logs/Fauna/sync-agent.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/Library/Logs/Fauna/sync-agent.log</string>
</dict>
</plist>
"""

_STUB = """#!/bin/bash
printf '%s\\n' "$(basename "$0") $*" >> "{log}"
exit 0
"""


def pre_fix_plist(home, agent_exec="/Applications/Fauna.app/Contents/MacOS/fauna-sync-agent"):
    """The pre-fix sync LaunchAgent for a user whose home is `home`."""
    return _PRE_FIX_PLIST.format(home=str(home), agent_exec=agent_exec)


def plist_path(home):
    return Path(home) / "Library" / "LaunchAgents" / f"{SYNC_LABEL}.plist"


class Sandbox:
    """One throwaway home + stub bin dir under `root`."""

    def __init__(self, root):
        self.root = Path(root)
        self.home = self.root / "home"
        self.scripts = self.root / "scripts"
        self.bin = self.root / "bin"
        self.calls_log = self.root / "calls.log"
        for d in (self.home, self.scripts, self.bin):
            d.mkdir(parents=True, exist_ok=True)
        for tool in ("launchctl", "chown"):
            stub = self.bin / tool
            # newline="\n" on every write here: the scripts compare bytes, and a
            # win host's default would CRLF them
            stub.write_text(_STUB.format(log=self.calls_log), newline="\n")
            stub.chmod(0o755)

    def plant(self, text):
        """Write `text` as the existing sync LaunchAgent (the "before" of an upgrade)."""
        p = plist_path(self.home)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, newline="\n")
        return p

    def run_postinstall(self, postinstall, common_sh):
        """Run the given postinstall + common.sh texts with the sandbox home as
        the console user's. Returns the CompletedProcess; `calls()` has the
        stubbed `launchctl`/`chown` invocations."""
        (self.scripts / "postinstall").write_text(postinstall, newline="\n")
        (self.scripts / "postinstall").chmod(0o755)
        (self.scripts / "common.sh").write_text(
            common_sh
            + "\n# --- postinstall_sandbox: the console user is the sandbox's ---\n"
            + "resolve_console_user() {\n"
            + f"    CONSOLE_USER=fauna-sandbox CONSOLE_UID=4242 USER_HOME='{self.home}'\n"
            + "    export CONSOLE_USER CONSOLE_UID USER_HOME\n"
            + "}\n",
            newline="\n",
        )
        env = {
            "PATH": f"{self.bin}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(self.home),
        }
        return subprocess.run(
            [shutil.which("bash") or "/bin/bash", str(self.scripts / "postinstall")],
            capture_output=True, text=True, timeout=60, env=env,
        )

    def calls(self):
        if not self.calls_log.exists():
            return []
        return self.calls_log.read_text().splitlines()

    def plist_text(self):
        return plist_path(self.home).read_text()
