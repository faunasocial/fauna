"""tier_1: the cross-session shared-live-box flock (`helpers/live_box_lock.py`).

In-process, no nest / driver / external process — just `fcntl.flock` + the
filesystem, with the lock dir redirected to a tmp path so we never touch the
real-home lock files.
"""

import os

import pytest

from helpers import live_box_lock

pytestmark = pytest.mark.tier_1


def test_lock_path_keys_on_normalized_url(monkeypatch, tmp_path):
    monkeypatch.setenv("FAUNA_E2E_LIVE_BOX_LOCK_DIR", str(tmp_path))
    a = live_box_lock._lock_path("https://example.com")
    a_slash = live_box_lock._lock_path("https://example.com/")
    b = live_box_lock._lock_path("https://other.example")
    assert a == a_slash, "a trailing slash must not change the lock key"
    assert a != b, "different live boxes must map to different lock files"
    assert str(tmp_path) in a, "the lock dir override must be honored"


def test_acquire_serializes_the_same_box(monkeypatch, tmp_path):
    """While one holder owns the box, a second (independent) exclusive lock on
    the same file must fail non-blocking — i.e. a sibling session would wait."""
    fcntl = pytest.importorskip("fcntl")  # POSIX-only; the lock is a no-op elsewhere
    monkeypatch.setenv("FAUNA_E2E_LIVE_BOX_LOCK_DIR", str(tmp_path))
    url = "https://example.com"

    fd = live_box_lock.acquire(url)
    assert fd is not None
    try:
        probe = os.open(live_box_lock._lock_path(url), os.O_CREAT | os.O_RDWR, 0o644)
        try:
            with pytest.raises(OSError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(probe)
    finally:
        live_box_lock.release(fd)

    # After release the slot is free — the same non-blocking lock now succeeds.
    probe = os.open(live_box_lock._lock_path(url), os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)  # must not raise
        fcntl.flock(probe, fcntl.LOCK_UN)
    finally:
        os.close(probe)


def test_different_boxes_do_not_contend(monkeypatch, tmp_path):
    """Holding box A must not block acquiring box B (only same-box runs serialize);
    if it blocked, `acquire` would hang and this test would time out."""
    pytest.importorskip("fcntl")
    monkeypatch.setenv("FAUNA_E2E_LIVE_BOX_LOCK_DIR", str(tmp_path))
    fd_a = live_box_lock.acquire("https://example.com")
    fd_b = live_box_lock.acquire("https://other.example")
    try:
        assert fd_a is not None and fd_b is not None
    finally:
        live_box_lock.release(fd_a)
        live_box_lock.release(fd_b)
