"""tier_1 unit tests for the tui driver's pty backend seam (drivers/pty_backend.py).

Cross-platform by construction: `spawn_pty` picks `PosixPtyBackend` (pty/fcntl/
termios) on POSIX and `ConPtyBackend` (pywinpty/ConPTY) on Windows, and these
tests assert the ONE contract the platform-agnostic drain loop relies on, so the
SAME test proves both backends — a POSIX run guards against a Windows-side
refactor breaking the primary path, and the Windows run is the only e2e proof the
ConPTY leg works at all.

No product stack: the child is a throwaway `python -c` helper, no nest, no client
driver, no fauna-tui — so the "the driver is a terminal emulator" duties (surface
the child's output as bytes; deliver a written reply to the child's stdin, which
is exactly how the real DA1 answer reaches the app) are proven headlessly, not
deferred to a manual/graphics run. Listed in conftest's _CLIENT_INDEPENDENT_FILES.
"""

import os
import sys
import time

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.pty_backend import spawn_pty  # noqa: E402

pytestmark = pytest.mark.tier_1


def _spawn(child_code: str):
    """Spawn a python child on a fresh pty via the platform backend."""
    return spawn_pty(
        [sys.executable, "-c", child_code],
        dict(os.environ),
        rows=40,
        cols=120,
        cell_w=8,
        cell_h=16,
    )


def _drain(backend, deadline_s: float = 10.0) -> bytes:
    """Read to EOF like the driver's drain thread (b"" only at EOF)."""
    buf = b""
    end = time.monotonic() + deadline_s
    while time.monotonic() < end:
        chunk = backend.read(65536)
        if not chunk:
            return buf
        buf += chunk
    return buf


def _teardown(backend):
    try:
        backend.terminate()
    except Exception:
        pass
    try:
        backend.wait(timeout=5)
    except Exception:
        try:
            backend.kill()
        except Exception:
            pass
    backend.close()


def test_spawn_surfaces_child_output_as_bytes():
    """read() surfaces what the child writes to its terminal, as bytes."""
    marker = "FAUNA_PTY_MARKER_7f3a91"
    backend = _spawn(f"import sys; sys.stdout.write({marker!r}); sys.stdout.flush()")
    try:
        out = _drain(backend)
    finally:
        _teardown(backend)
    assert marker.encode() in out, f"marker missing from pty stream: {out!r}"


def test_read_returns_empty_at_eof_and_poll_reports_exit():
    """After the child exits, read() returns b"" (EOF) and poll() a non-None code
    — the contract the drain loop ('empty ⇒ EOF, return') and launch()'s
    early-exit detection both depend on."""
    backend = _spawn("import sys; sys.stdout.write('x'); sys.stdout.flush()")
    try:
        _drain(backend)  # reads to EOF; a spurious b"" would fail elsewhere
        # An immediate post-EOF read stays empty (never blocks, never data).
        assert backend.read(4096) == b""
        deadline = time.monotonic() + 5
        rc = backend.poll()
        while rc is None and time.monotonic() < deadline:
            time.sleep(0.05)
            rc = backend.poll()
        assert rc is not None, "poll() still None after child exit"
    finally:
        _teardown(backend)


def test_write_reaches_child_stdin():
    """A byte written to the master reaches the child's stdin — the driver's
    terminal-emulator duty, and exactly how the real DA1 reply reaches the app.
    Input buffers in the pty, so this holds regardless of read/write ordering."""
    child = (
        "import sys; line = sys.stdin.readline().strip(); "
        "sys.stdout.write('GOT[' + line + ']'); sys.stdout.flush()"
    )
    backend = _spawn(child)
    try:
        time.sleep(0.5)  # let the child reach readline (not required — pty buffers)
        backend.write(b"PINGVALUE\r\n")
        out = _drain(backend)
    finally:
        _teardown(backend)
    assert b"GOT[PINGVALUE]" in out, f"reply never reached child stdin: {out!r}"
