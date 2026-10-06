"""Pseudo-terminal transport backends for the tui driver.

The tui driver **is** a terminal emulator: it allocates a pseudo-terminal,
launches `fauna-tui` on the slave end, owns the master, drains the
escape-sequence byte stream the app renders (→ `app.out`), and writes replies
back (the DA1 answer). That allocation is the **one** platform-divergent seam —
POSIX uses `pty`/`fcntl`/`termios`, Windows uses **ConPTY** (`CreatePseudoConsole`
via `pywinpty`). Everything above the seam in `tui.py` — the drain loop, the DA1
detect/answer, frame capture — is platform-agnostic and speaks only the
`PtyBackend` contract below, so it is untouched on either platform.

Split into three files on purpose (the 1000-session cold read): this factory
imports nothing platform-specific at module top, so it loads everywhere;
`posix_pty.py` (`import pty, fcntl, termios`) is imported only on POSIX and
`conpty.py` (`import winpty`) only on Windows — each is obviously one platform's
backend, and neither's imports can break the other platform's collection.
"""

import os


class PtyBackend:
    """A child process attached to a pseudo-terminal whose master this owns.

    The contract the platform-agnostic drain loop relies on:

    - ``read(n)`` **blocks** and returns ``b""`` **only at true EOF** (the child's
      terminal closed) — exactly like ``os.read(master, n)``, so the drain loop's
      "empty chunk ⇒ EOF, return" logic is identical on both platforms. A spurious
      empty return would make the drain thread exit early and the app's render loop
      would then block on a full pty buffer, so a backend must never return ``b""``
      while the child is alive and its terminal open.
    - ``write(data)`` sends bytes to the child's terminal input (only the DA1 reply
      today; element actuation goes over the HTTP agent, never the pty).
    - ``poll`` / ``wait`` / ``terminate`` / ``kill`` / ``pid`` are **Popen-compatible**
      so the backend can be handed straight to ``port_util.track_process`` /
      ``terminate_tree`` (which duck-type Popen and already branch on ``os.name``).
    - ``close`` releases the master handle; ``resize`` is provided for completeness
      (no current test resizes after launch).
    - ``reports_pixel_size`` says whether this terminal answers the *pixel* half
      of its size, i.e. whether ``cell_w``/``cell_h`` reached the app at all. It
      is a property of the terminal, not of the OS — which is why a test that
      needs the app's effective cell size reads this rather than checking
      ``os.name`` (`testing.md` point 7: platform checks never live in test
      files). POSIX ptys report it on the winsize ioctl; ConPTY has no pixel-size
      concept, so the app falls back to its own assumed cell size there.
    """

    pid: int

    #: See the class docstring. Overridden per backend.
    reports_pixel_size: bool = False

    def read(self, n: int) -> bytes:
        raise NotImplementedError

    def write(self, data: bytes) -> None:
        raise NotImplementedError

    def poll(self):
        """Child exit code, or ``None`` while it is still running."""
        raise NotImplementedError

    def wait(self, timeout=None):
        """Block until the child exits; raise ``subprocess.TimeoutExpired`` on timeout."""
        raise NotImplementedError

    def terminate(self) -> None:
        """Graceful stop (POSIX: group SIGTERM; Windows: ConPTY terminate)."""
        raise NotImplementedError

    def kill(self) -> None:
        """Force stop."""
        raise NotImplementedError

    def resize(self, rows: int, cols: int) -> None:
        raise NotImplementedError

    def close(self) -> None:
        """Release the pty master handle (best-effort)."""
        raise NotImplementedError


def spawn_pty(cmd, env, rows, cols, cell_w, cell_h, stderr_file=None) -> PtyBackend:
    """Spawn ``cmd`` on a fresh pseudo-terminal and return the owning backend.

    ``cell_w``/``cell_h`` are the pixel size of one character cell: POSIX reports
    them on the winsize ioctl (pixel dims = cols*cell_w × rows*cell_h) so the app
    can size an inline image to its cell box (`tui.md` § Rendering). **ConPTY has
    no pixel-size concept**, so the Windows backend accepts and ignores them and
    the app falls back to its own assumed cell size there — a smaller picture in
    the same cell box, never a missing one. The backend reports which of the two
    it is on ``reports_pixel_size``, so a test asserting on image geometry asks
    the terminal rather than the OS. The *protocol* half is unaffected: ConPTY
    relays the ``ESC [ c`` query and forwards the reply, so detection runs for
    real on both (`graphics::detect::probe_da1`'s two arms).

    ``stderr_file`` splits the child's stderr onto its own file on POSIX so a
    panic isn't shredded by frame escapes; ConPTY attaches the whole console, so
    the Windows backend ignores it and stderr rides the pty stream into ``app.out``.
    """
    if os.name == "nt":
        from .conpty import ConPtyBackend

        return ConPtyBackend(cmd, env, rows, cols, cell_w, cell_h)
    from .posix_pty import PosixPtyBackend

    return PosixPtyBackend(cmd, env, rows, cols, cell_w, cell_h, stderr_file)
