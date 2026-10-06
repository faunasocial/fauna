"""Windows ConPTY backend for the tui driver, via `pywinpty`.

The Windows twin of `posix_pty.py`. Where POSIX allocates a pty with
`pty.openpty()`, Windows allocates a **pseudoconsole** (`CreatePseudoConsole`,
Win10 1809+) — here through `pywinpty`'s pexpect-style `PtyProcess`. Imported only
on Windows (`pty_backend.spawn_pty` routes by `os.name`), so `import winpty` — a
third-party dep absent on the POSIX boxes — never loads there.

Two ways ConPTY differs from a Unix pty, both handled here so the drain loop in
`tui.py` stays platform-agnostic:

- **A pseudoconsole is a terminal emulator, not a raw byte pipe.** It *re-renders*
  the child's output as a VT/ANSI stream and injects its own startup escapes
  (`ESC[?25l`, autowrap toggles, a relayed `ESC[c` device-attributes query, …).
  The driver never parses the frame bytes for element data (that is the HTTP
  agent's job); `app.out` is a post-mortem dump + the graphics observable, so the
  re-render is harmless. The relayed `ESC[c` even helps: the driver's DA1 answer
  path (`tui.py`) fires on it best-effort.
- **`pywinpty` speaks `str` (UTF-8), not `bytes`.** ConPTY emits a well-formed
  UTF-8 VT stream and `pywinpty` decodes it with an incremental decoder (never
  splitting a multibyte char across reads), so re-encoding to UTF-8 is lossless.
  `read()`/`write()` therefore convert at the seam and expose `bytes`, keeping the
  drain loop byte-identical to POSIX. `read()` blocks and raises `EOFError` at
  child exit (verified), which maps to the `b""`-means-EOF contract.

ConPTY has **no pixel-size concept**, so `cell_w`/`cell_h` are accepted and
ignored (`reports_pixel_size = False`) and the app falls back to its own assumed
cell size — a smaller inline image in the same cell box, never a missing one.
The *protocol* half is not affected: ConPTY relays the client's `ESC [ c` on the
master and forwards the reply to its stdin, so the client's real DA1 probe runs
here exactly as it does on a POSIX pty (the Windows arm of
`graphics::detect::probe_da1`, landed 2026-08-01).
"""

import subprocess
import time

import winpty

from .pty_backend import PtyBackend


class ConPtyBackend(PtyBackend):
    # ConPTY reports its size in cells only (module docstring).
    reports_pixel_size = False

    def __init__(self, cmd, env, rows, cols, cell_w, cell_h):
        self._cmd = list(cmd)
        # pexpect-style: argv list, env dict (str→str), dimensions = (rows, cols).
        # ConPTY has no pixel winsize, so cell_w/cell_h are unused here.
        self._proc = winpty.PtyProcess.spawn(
            self._cmd,
            env=dict(env),
            dimensions=(rows, cols),
        )
        self.pid = self._proc.pid

    def read(self, n: int) -> bytes:
        try:
            chunk = self._proc.read(n)
        except EOFError:
            return b""
        except winpty.WinptyError:
            return b""
        if not chunk:
            # `read()` blocks until data or EOF, so an empty return means the
            # child's console closed — surface it as EOF like `os.read`.
            return b""
        # str (UTF-8) → bytes; lossless for a well-formed VT stream.
        return chunk.encode("utf-8", "replace")

    def write(self, data: bytes) -> None:
        # Only the DA1 reply is ever written (pure ASCII); keystrokes go over the
        # HTTP agent, never the pty.
        self._proc.write(data.decode("utf-8", "replace"))

    def poll(self):
        if self._proc.isalive():
            return None
        rc = self._proc.exitstatus
        # Early-exit detection needs a non-None here once the child is dead.
        return rc if rc is not None else -1

    def wait(self, timeout=None):
        # pywinpty's wait() has no timeout arg, so poll isalive() to a deadline and
        # raise the Popen-shaped timeout the teardown path already handles.
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            if not self._proc.isalive():
                return self.poll()
            if deadline is not None and time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired(self._cmd, timeout)
            time.sleep(0.05)

    def terminate(self) -> None:
        try:
            self._proc.terminate()
        except Exception:
            pass

    def kill(self) -> None:
        try:
            self._proc.terminate(force=True)
        except Exception:
            try:
                self._proc.terminate()
            except Exception:
                pass

    def resize(self, rows: int, cols: int) -> None:
        try:
            self._proc.setwinsize(rows, cols)
        except Exception:
            pass

    def close(self) -> None:
        try:
            self._proc.close()
        except Exception:
            pass
