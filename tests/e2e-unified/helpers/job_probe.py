"""A cargo build whose jobs can be counted, for the per-grant jobserver's tests
(core-bound.md, *A grant's width is live*): a workspace of crates whose build
scripts each mark themselves live and hold their job until released. Nothing
here reads a clock: an empty jobserver (Linux's FIFO, Windows's named semaphore)
is what bounds the jobs, so "two run and no third can start" is a state, and a
deadline only ends a wedged run.
"""

import json
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

from drivers.port_util import popen_group_kwargs, reap_descendants_of

_REPO = Path(__file__).resolve().parents[3]
#: What a wedged run gets, counted from the latest sign that the real focus
#: slot is still queueing or was just granted: that queue is the box's load,
#: bounded by build-slot's own give-up, never a state this deadline judges
#: (convention 14 — a loaded Linux held the focus grant 708 s once, past a
#: deadline that counted it).
DEADLINE_S = 600.0

#: The real focus slot's own words about its queue: each waiting reprint, and
#: the grant line closing them (build-slot.py's GRANTED_PREFIX).
_FOCUS_QUEUE = re.compile(r"^\[build-slot\] (?:waiting \d+s for a 'focus' grant|granted a 'focus' slot)")

#: Windows' stand-in for `env -u FAUNA_SLOT_HELD_FOCUS K=V … <cmd>`: argv[1] is
#: the JSON of the variables to set, the rest the command, whose exit code is relayed.
_TRAMPOLINE = ("import json, os, subprocess, sys; os.environ.pop('FAUNA_SLOT_HELD_FOCUS', None); "
               "os.environ.update(json.loads(sys.argv[1])); sys.exit(subprocess.call(sys.argv[2:]))")

_BUILD_RS = r"""
use std::{env, fs, path::Path, process, thread, time::Duration};
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = env::var("JOB_PROBE_DIR").unwrap();
    let live = Path::new(&dir).join(format!("{}.live", process::id()));
    fs::write(&live, "").unwrap();
    while !Path::new(&dir).join("release").exists() {
        thread::sleep(Duration::from_millis(50));
    }
    fs::remove_file(&live).unwrap();
}
"""


def _workspace(root: Path, crates: int) -> None:
    members = [f"c{i}" for i in range(crates)]
    names = ", ".join(f'"{c}"' for c in members)
    (root / "Cargo.toml").write_text(f'[workspace]\nresolver = "2"\nmembers = [{names}]\n')
    for c in members:
        (root / c / "src").mkdir(parents=True)
        (root / c / "Cargo.toml").write_text(
            f'[package]\nname = "{c}"\nversion = "0.0.0"\nedition = "2021"\n')
        (root / c / "src" / "lib.rs").write_text("")
        (root / c / "build.rs").write_text(_BUILD_RS)


class ProbeBuild:
    """A real `build` grant of width k in a scratch slot namespace, its cargo
    behind whatever boundary a real grant's is, counted on the box by a real
    focus slot around it. `with` it: leaving releases the jobs and reaps it.
    `slot` is the slot script, which the caller's own path names."""

    def __init__(self, tmp: Path, slot: str, k: int = 2, crates: int = 6):
        # The live markers sit under the build's own target dir: a bounded
        # compile's root is read-only, its target one of the few writable
        # paths (build-compile-cache.md, the kache boundary), so a build
        # script's marker anywhere else is refused.
        self.probe, self.slots, self.tmp = tmp / "target" / "probe", tmp / "slots", tmp
        self.slot = slot
        self.k, self.crates = k, crates
        self.out = ""
        self.proc: subprocess.Popen | None = None
        self._lines: list[str] = []
        self._queue_seen = 0.0
        self._reader: threading.Thread | None = None

    def __enter__(self) -> "ProbeBuild":
        workspace = self.tmp / "ws"
        workspace.mkdir()
        self.probe.mkdir(parents=True)
        _workspace(workspace, self.crates)
        pin = (_REPO / "rust-toolchain.toml").read_text(encoding="utf-8")
        env = dict(os.environ, JOB_PROBE_DIR=str(self.probe), FAUNA_SLOT_COMPRESS="0",
                   RUSTUP_TOOLCHAIN=re.search(r'channel\s*=\s*"([^"]+)"', pin).group(1),
                   CARGO_TARGET_DIR=str(self.tmp / "target"))
        for var in ("FAUNA_SLOT_DIR", "FAUNA_SLOT_HELD_BUILD", "FAUNA_SLOT_HELD_FOCUS",
                    "CARGO_MAKEFLAGS", "MAKEFLAGS"):
            env.pop(var, None)
        # The inner grant is a scratch namespace's, so the real focus slot's
        # leaf marker stays behind: that slot is what counts the compile.
        inner = {"FAUNA_SLOT_DIR": str(self.slots),
                 # The scratch grant's floors, off: a tight box must not hold it.
                 "FAUNA_SLOT_MEM_FLOOR_GB": "0", "FAUNA_SLOT_MEM_STALL_PCT": "0",
                 "FAUNA_SLOT_DISK_FLOOR_GB": "0", "FAUNA_SLOT_CPU_BUSY_PCT": "0",
                 "FAUNA_SLOT_UNTRACKED_BUSY": "0"}
        grant = [sys.executable, self.slot, "--pool", "build", "--cores", f"{self.k}-{self.k}", "--"]
        if sys.platform == "win32":
            # No `env` or `bash` on Windows: a Python trampoline drops the
            # marker and names the namespace, and cargo runs through the MSVC
            # wrapper (scripts/cargo-win.cmd), which links the build scripts.
            # The wrapper waits on its command, so no exec is chained.
            outer = [sys.executable, "-c", _TRAMPOLINE, json.dumps(inner), *grant,
                     "cmd", "/c", str(_REPO / "scripts" / "cargo-win.cmd"),
                     "build", "--offline", "--workspace"]
        else:
            # A shell between the two: exec after exec keeps one pid, and systemd
            # names a transient scope by pid, so a second one on it is refused.
            outer = ["bash", "-c", '"$@"; exit $?', "--", "env", "-u", "FAUNA_SLOT_HELD_FOCUS",
                     *(f"{k}={v}" for k, v in inner.items()),
                     *grant, "cargo", "build", "--offline", "--workspace"]
        self.proc = subprocess.Popen(
            [sys.executable, self.slot, "--pool", "focus", "--", *outer],
            cwd=workspace, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            **popen_group_kwargs())
        reap_descendants_of(self.proc.pid)
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()
        return self

    def _read(self) -> None:
        """Drain the build's output (convention 13), noting the focus queue's lines."""
        for line in self.proc.stdout:
            self._lines.append(line)
            if _FOCUS_QUEUE.match(line):
                self._queue_seen = time.monotonic()

    def __exit__(self, *exc) -> None:
        (self.probe / "release").write_text("")
        self.proc.wait(timeout=300)
        self._reader.join(timeout=60)
        self.out = "".join(self._lines)
        print(self.out[-4000:])  # a failure in the body reads the build's own words

    def until(self, what: str, ok) -> None:
        """Wait on a state, never a time: the deadline runs from the later of
        the wait's start and the focus slot's latest queue line."""
        start = time.monotonic()
        while not ok():
            assert self.proc.poll() is None, f"the build ended before {what}"
            assert time.monotonic() < max(start, self._queue_seen) + DEADLINE_S, f"never reached: {what}"
            time.sleep(0.05)

    def live(self) -> int:
        return len([n for n in os.listdir(self.probe) if n.endswith(".live")])

    def fifo(self) -> str:
        """The grant's FIFO, once the grant stands."""
        def found():
            return [n for n in (os.listdir(self.slots) if self.slots.exists() else ())
                    if n.endswith(".fifo")]
        self.until("the grant's FIFO", found)
        return str(self.slots / found()[0])

    @staticmethod
    def pipe_is_empty(fd: int) -> bool:
        try:
            os.write(fd, os.read(fd, 1))
        except BlockingIOError:
            return True
        return False
