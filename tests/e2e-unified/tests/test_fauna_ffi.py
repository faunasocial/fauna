"""Smoke test for the fauna_ffi ctypes wrapper."""
import pytest

from fauna_ffi import build_post

pytestmark = pytest.mark.tier_1


def test_build_post_produces_nonempty_bytes():
    secret = b"\x04" * 32
    out = build_post(secret, "hello world")
    assert isinstance(out, bytes)
    assert len(out) > 64


def test_build_post_with_tags():
    secret = b"\x04" * 32
    out = build_post(secret, "tagged post", tags=["test", "foo"])
    assert isinstance(out, bytes)
    assert len(out) > 64


def test_build_post_rejects_bad_secret_length():
    with pytest.raises(ValueError):
        build_post(b"\x01" * 16, "hi")


def test_build_post_rejects_bad_reply_to_length():
    with pytest.raises(ValueError):
        build_post(b"\x01" * 32, "hi", reply_to=b"\x00" * 16)
