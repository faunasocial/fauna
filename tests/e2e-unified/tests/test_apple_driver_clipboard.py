"""Unit tests for the apple in-process drivers' ``get_clipboard_text`` — the leg
that lets a journey carry what a copy button REALLY put on the OS pasteboard
(``test_recovery_kit_restore.py``'s copied-kit journey: the base default raises
``NotImplementedError``, which turns that journey into a
``skip_environment`` skip on any driver that does not implement it).

One implementation on the shared ``InProcessAgentDriver`` base serves both apple
drivers (macOS: ``NSPasteboard``; iOS Simulator: ``UIPasteboard``), through the
app's own ``GET /clipboard/text`` (``InProcessAutomationServer.swift``). The
contract is the windows / linux drivers': the text, or ``None`` when the
pasteboard holds none; an agent refusal RAISES rather than reading empty (an
empty read would let a "copy put nothing new" wait pass for the wrong reason).

tier_1: pure Python, no nest binary, no client driver, no app process.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.base import PlatformDriver
from drivers.inprocess_agent import InProcessAgentDriver
from drivers.ios import IosInProcessDriver
from drivers.macos import MacosInProcessDriver

APPLE_DRIVERS = [MacosInProcessDriver, IosInProcessDriver]


class _Agent:
    """Stand-in for ``HttpBridgeDriver._get``: records every route asked and
    answers ``/clipboard/text`` with a fixed reply."""

    def __init__(self, reply):
        self.reply = reply
        self.routes: list[str] = []

    def __call__(self, path, params=None):
        self.routes.append(path)
        if path != "/clipboard/text":
            raise AssertionError(f"get_clipboard_text must not touch {path}")
        return self.reply


def _bare(cls, agent):
    """A real driver class with its transport replaced — built without
    ``__init__`` so no app process, port or simulator is involved."""
    d = object.__new__(cls)
    d._get = agent
    return d


@pytest.mark.parametrize("cls", APPLE_DRIVERS, ids=lambda c: c.__name__)
def test_both_apple_drivers_read_the_pasteboard_instead_of_raising(cls):
    """The point of the leg: neither apple driver falls through to the base
    default's ``NotImplementedError``, so the copied-kit journey runs on macOS
    and iOS rather than skipping as ``skip_environment``."""
    agent = _Agent({"text": "fauna://recovery?secret=ab"})
    assert _bare(cls, agent).get_clipboard_text() == "fauna://recovery?secret=ab"
    assert agent.routes == ["/clipboard/text"]


def test_the_implementation_is_the_shared_base_not_a_per_driver_copy():
    """Priority #1/#2: one reader for both apps. A per-driver override is where
    macOS and iOS would drift apart."""
    assert "get_clipboard_text" in vars(InProcessAgentDriver)
    for cls in APPLE_DRIVERS:
        assert "get_clipboard_text" not in vars(cls), (
            f"{cls.__name__} re-implements get_clipboard_text; it must inherit the "
            "InProcessAgentDriver one"
        )
        assert cls.get_clipboard_text is InProcessAgentDriver.get_clipboard_text
        assert cls.get_clipboard_text is not PlatformDriver.get_clipboard_text


@pytest.mark.parametrize("cls", APPLE_DRIVERS, ids=lambda c: c.__name__)
def test_a_pasteboard_holding_no_text_reads_as_none(cls):
    """The agent answers JSON ``null`` for an empty pasteboard — ``None``, the
    windows / linux contract, so ``get_clipboard_text() not in (None, before)``
    keeps its meaning."""
    assert _bare(cls, _Agent({"text": None})).get_clipboard_text() is None
    assert _bare(cls, _Agent({})).get_clipboard_text() is None


@pytest.mark.parametrize("cls", APPLE_DRIVERS, ids=lambda c: c.__name__)
def test_an_agent_refusal_raises_instead_of_reading_as_empty(cls):
    with pytest.raises(RuntimeError, match="pasteboard read refused: nope"):
        _bare(cls, _Agent({"error": "nope"})).get_clipboard_text()
