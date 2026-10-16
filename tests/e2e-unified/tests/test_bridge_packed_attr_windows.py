"""The FlaUI bridge's packed-HelpText attribute read — one value per key, pinned
with no UIA element, no window and no app.

**What this pins.** UIA gives an element exactly one application-supplied string,
``AutomationProperties.HelpText``, and the bridge's attribute read answers every
non-special attribute name with it (``Actions.cs::AttrOf``). An element that must
publish SEVERAL attributes — the inline player in ``video-thumbnail`` publishes
``state``, ``position`` and ``source`` (``render-model.md`` § D6c → *Inline
playback*; ``drivers/base.py::get_attr``) — packs them into that one string as
``key=value`` pairs joined by ``;``. Without a per-key read every one of the three
answered the whole packed string, which the shared
``test_feed_video_playback.py`` compares against a bare phase, a number and a URL.

``PackedAttr.cs`` holds that read as a pure function so
``SelfTest.PackedAttrChecks()`` (driven by ``--self-test-packed-attr``, mirroring
``--self-test-actuation-gate``) can grade it directly: each key answers its own
value, a missing key answers nothing, the packer's escapes come back out — and,
load-bearing, a HelpText that is NOT a pack (a bare ``painted``, a base64 id
ending in ``=``, free text holding ``=`` and ``;``) keeps answering verbatim.

The app half — ``AutomationAttributePack.Pack`` in ``FaunaApp.Core`` — is pinned
by ``InlineVideoPlaybackTests`` in the C# unit suite.

Goal doc: ``docs/goal/architecture/render-model.md`` § D6c → *Inline playback*.

tier_2: a real harness binary (the bridge) evaluating pure functions, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_reads_a_packed_help_text_one_value_per_key() -> None:
    """A pack answers per key, and nothing that is not a pack is read as one."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-packed-attr"],
        capture_output=True, text=True, timeout=60,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge packed-attr self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
