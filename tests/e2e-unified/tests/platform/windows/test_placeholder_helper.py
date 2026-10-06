"""tier_1: the cfapi-placeholder observation predicate, with no cfapi involved.

The predicate this pins — "does this file still carry its placeholder bits?" — is
the ground truth the installer full-journey e2e asserts hydration on
(``docs/goal/behavior/file-sync.md`` § *A placeholder is present-but-unreadable*:
a cloud-only placeholder carries ``FILE_ATTRIBUTE_OFFLINE`` /
``FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS``, *"which a hydrated file no longer
carries"*).

Splitting it out is deliberate. The journey test needs a live sync root, an
elevated install and an exclusive box; this predicate needs none of those —
``SetFileAttributesW`` can raise ``OFFLINE`` on an ordinary file, so the read
side is provable in milliseconds by any session. Leaving it welded to the
journey test would have made "the attribute read works" un-observable except
through a 20-minute elevated run, which is how a wrong predicate hides behind a
red journey test and earns three wrong diagnoses (the ``FileIdentity`` lesson).

What this canNOT prove, and does not claim to: that cfapi actually *clears*
those bits on a real fetch. Only a live sync root shows that — and it already
does, in ``fauna-sync-agent::cfapi_live_integration``. This pins that *our
reader agrees with the OS* about what the bits mean.
"""

from __future__ import annotations

import ctypes
import sys

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="Windows file attributes"),
    pytest.mark.tier_1,
]

from helpers import windows_placeholder as ph  # noqa: E402


def _set_attrs(path, attrs: int) -> None:
    ok = ctypes.windll.kernel32.SetFileAttributesW(str(path), attrs)
    if not ok:
        raise OSError(f"SetFileAttributesW failed: WinError {ctypes.get_last_error()}")


def test_an_ordinary_file_is_not_a_placeholder(tmp_path):
    """The negative that matters: a real, local file must never read as cloud-only.
    If it did, the journey test would call a never-hydrated file hydrated."""
    f = tmp_path / "ordinary.txt"
    f.write_text("real bytes on disk")

    assert not ph.is_placeholder(f), ph.describe(f)
    assert ph.wait_for_hydrated(f, timeout=1.0), ph.describe(f)


def test_an_offline_file_reads_as_a_placeholder(tmp_path):
    """The positive: OFFLINE alone is enough to read as cloud-only.

    cfapi sets OFFLINE + RECALL_ON_DATA_ACCESS together on a real placeholder,
    but the predicate must fire on either bit — a file the OS has marked offline
    is one whose bytes are not on disk, whoever set the bit."""
    f = tmp_path / "offline.txt"
    f.write_text("x" * 64)
    _set_attrs(f, ph.FILE_ATTRIBUTE_OFFLINE)

    assert ph.is_placeholder(f), ph.describe(f)
    # And the hydration wait must NOT report success for it.
    assert not ph.wait_for_hydrated(f, timeout=1.0), ph.describe(f)


def test_clearing_the_bit_flips_the_predicate_to_hydrated(tmp_path):
    """The transition the journey test waits on, in miniature: bits set → the
    file reads as a placeholder; bits cleared → it reads as hydrated. This is
    exactly what a real cfapi FETCH_DATA does to the file, minus cfapi."""
    f = tmp_path / "flip.txt"
    f.write_text("y" * 64)
    _set_attrs(f, ph.FILE_ATTRIBUTE_OFFLINE)
    assert ph.wait_for_placeholder(f, timeout=1.0), ph.describe(f)

    _set_attrs(f, 0x00000080)  # FILE_ATTRIBUTE_NORMAL

    assert ph.wait_for_hydrated(f, timeout=1.0), ph.describe(f)
    assert not ph.is_placeholder(f), ph.describe(f)


def test_an_absent_file_is_never_reported_hydrated(tmp_path):
    """The dangerous confusion, pinned: ABSENT must not read as "no placeholder
    bits set" — i.e. as hydrated. A deleted file trivially has no bits; if the
    reader shrugged at that, the journey test would go green on a file that was
    never delivered at all. `file_attributes` raises instead."""
    missing = tmp_path / "not-there.txt"

    with pytest.raises(FileNotFoundError):
        ph.file_attributes(missing)
    with pytest.raises(FileNotFoundError):
        ph.is_placeholder(missing)

    # And the bounded waiters degrade to False, never True, never a crash.
    assert not ph.wait_for_placeholder(missing, timeout=0.5)
    assert not ph.wait_for_hydrated(missing, timeout=0.5)
    assert "ABSENT" in ph.describe(missing)


def test_hydrate_via_shell_reads_from_another_process(tmp_path):
    """The driving side: the read is performed by a process that is not us and
    not the provider (cfapi fires nothing for the provider's own I/O). On an
    ordinary file there is no provider, so this just proves the mechanism runs
    and reports the child's outcome rather than swallowing it."""
    f = tmp_path / "readable.txt"
    f.write_text("hello from another process")

    result = ph.hydrate_via_shell(f)

    assert result.returncode == 0, f"cmd /c type failed: {result.stderr!r}"
