"""Restore from a custodian nest — the two outcomes that need no custody.

`docs/goal/ui/nests.md` § Trust facet — restore from a custodian nest: the
opener renders on every signed-in visit, custody rows or none; an address the
dial policy refuses is told why, never silently dropped; a nest that does not
admit the device answers one opaque *not admitted*; the form stays open on
every failure; and the notice's copy is shared Rust's
(`CustodianRestoreOutcome::notice_parts`), never the shell's.

**Why a file of its own.** The full journey
(`test_custody_cold_device_restore.py`) needs the custody ceremony first, and
the ceremony (mint, accept) is a declared absence on web (`ui/devices.md`
§ Where logic lives). These two arms dial no custodian that holds anything, so
they run on every app that paints the form — web included — from one signed-in
seat: the shell's dispatch, the outcome's copy and the form decision, end to
end through the real runtime command.
"""

from __future__ import annotations

import pytest

from drivers.port_util import find_free_port
from helpers.waiting import await_account_runtime_assembled, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_RESTORE_S = 120.0


def _settled_notice(app) -> str:
    """Wait for the notice to leave *running*; returns its text (convention 6:
    the error surface rides any failure)."""
    settled: dict = {}

    def done() -> bool:
        text = app.nest_trust.custody_restore_notice()
        settled["text"] = text
        return bool(text) and text != S.nests.custody_restore_running

    wait_until(
        done,
        _RESTORE_S,
        interval=0.5,
        diagnose=lambda: (
            f"the restore never settled; notice {settled.get('text')!r}, "
            f"error surface {app.error_text()!r}"
        ),
    )
    return settled["text"]


def test_a_refused_address_and_an_unadmitting_nest_each_say_so(logged_in_app):
    app = logged_in_app
    app.nest_trust.require_custody_restore_supported()
    await_account_runtime_assembled(app.driver)
    app.linked_nests.navigate()
    app.driver.wait_for("nests-custody-restore-button", timeout=30.0)
    assert app.driver.is_absent("nests-custody-restore-notice"), (
        "the notice is absent until a restore ran this session"
    )

    # The policy refuses a plaintext non-loopback address before any dial.
    app.nest_trust.restore_from_custodian("http://custodian.example")
    refused = _settled_notice(app)
    refused_prefix = S.nests.custody_restore_refused(reason="\0").split("\0")[0]
    assert refused.startswith(refused_prefix), refused
    assert len(refused) > len(refused_prefix), f"the refusal names no reason: {refused!r}"
    assert app.driver.is_visible("nests-custody-restore-input"), (
        "the form stays open on a refusal"
    )
    assert not app.error_text(), f"a refusal is a notice, not an error: {app.error_text()!r}"

    # Nothing listens at this loopback port: the dial fails, and the device
    # cannot tell that from a nest holding nothing for it — one answer.
    nobody = f"http://127.0.0.1:{find_free_port()}"
    app.driver.click("nests-custody-restore-cancel-button")
    app.nest_trust.restore_from_custodian(nobody)
    not_admitted = _settled_notice(app)
    assert not_admitted == S.nests.custody_restore_not_admitted, not_admitted
    assert app.driver.is_visible("nests-custody-restore-input"), (
        "the form stays open when the nest does not admit the device"
    )
    assert not app.error_text(), app.error_text()
