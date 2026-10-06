"""The FlaUI bridge's actuation gate — convention 11's decision, pinned with no
UIA element, no window and no app.

**What this pins.** windows' automation server is the out-of-process FlaUI bridge,
so every `/element/{click,double_click,type,clear,select}` call drove whatever the
finder resolved without ever consulting the control's enabled state
(`Actions.cs`). That is `e2e-conventions.md` § convention 11 **one layer down** —
not a *dropped* command but an **illegal one silently honoured** — and it presents
as a product bug: the call "succeeds", the app does nothing, and the test dies on
a downstream read. apple's first gated sweep caught a real one this way (a feed
form whose create button no user could reach, driven directly by a test that had
passed for months), and linux's caught a product deadlock.

`ActuationGate.cs` factors that decision out of `Actions.cs` — mirroring, not
calling, `libs/fauna-e2e-agent::gate_actuation`, which the Rust hosts (tui, linux)
share and a C# bridge cannot link — so `SelfTest.ActuationGateChecks()` (driven by
`--self-test-actuation-gate`, mirroring `--self-test-scroll-policy`) can grade it
directly: the verdict's four corners, the flag precedence (**permissive wins**, so
an enumerating run can never be the run that turns red), the marker line
byte-for-byte as the shared cross-app format, the refusal wording the Python
driver discriminates on, and the side-effecting entry point's mark-vs-throw split.

**The polarity check is why this file is not optional.** `ShouldRefuse` takes
`strict` as an argument, so it is correct whichever way the host's default points
and *cannot* catch a default that silently flips — the same reason apple pins its
own polarity (`apps/apple-e2e-automation.md` § The actuation gate). windows
refuses by default since 2026-09-14, so the check asserts
`WindowsRefusesDisabledActuationByDefault == true`, reading the production
constant rather than a duplicated literal. It flipped in the same change as the
constant, so a default that ever moves back reds here instead of passing
unnoticed.

Goal doc: `docs/goal/architecture/e2e-conventions.md` § convention 11 (*an illegal
command is the same failure one layer down*).

tier_2: a real harness binary (the bridge) evaluating pure functions, no app.
"""

from __future__ import annotations

import subprocess

import pytest

from drivers.windows import _BRIDGE_EXE, _ensure_bridge_built

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]


def test_bridge_actuation_gate_verdict_flags_and_marker_are_pinned() -> None:
    """The gate refuses only a disabled control in strict mode, marks it in
    permissive mode, and spells both in the cross-app shared wording."""
    _ensure_bridge_built()
    proc = subprocess.run(
        [str(_BRIDGE_EXE), "--self-test-actuation-gate"],
        capture_output=True, text=True, timeout=60,
    )
    # The self-test names each failure on its own line; surface the whole
    # transcript so the failure diagnoses itself (convention 6).
    assert proc.returncode == 0, (
        f"bridge actuation-gate self-test reported {proc.returncode} failure(s).\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}"
    )
