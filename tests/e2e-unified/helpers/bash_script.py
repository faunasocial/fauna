"""Run a shell snippet from a file, never as `bash -c <snippet>`.

On Windows Git's bash.exe launcher cuts a command line near 8 KB without a word:
the cut reads as `syntax error: unexpected end of file` at a line near the end,
and a test that extracts a few of a check script's functions into its snippet
passes that length easily. A file has no such limit. The bytes are written
LF-only, so no CR reaches bash on Windows; the path is handed over in its forward-
slash spelling, which Git's bash reads.
"""
from __future__ import annotations

import os
import subprocess
import tempfile
from pathlib import Path


def run_bash(snippet: str, *, args: tuple[str, ...] = (), **kwargs) -> subprocess.CompletedProcess:
    """`subprocess.run(["bash", <file holding snippet>, *args], **kwargs)` — the
    file removed afterwards. `kwargs` pass through (env, cwd, capture_output,
    text, timeout, …)."""
    fd, path = tempfile.mkstemp(suffix=".sh", prefix="bash-snippet-")
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(snippet.replace("\r\n", "\n").encode("utf-8"))
        return subprocess.run(["bash", Path(path).as_posix(), *args], **kwargs)
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
