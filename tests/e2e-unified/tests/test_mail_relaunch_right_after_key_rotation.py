"""tier_3: an app relaunched the moment a mail-key rotation's form closes
leaves IMAP working — the relaunched app never re-seals the MDA snapshot under
a key the credentials are not wrapped under.

The witness for ``docs/goal/behavior/mail-credentials.md`` § Hard revoke →
*Published before the nest moves*, under the recoverability invariant
(``docs/goal/architecture/nest/common.md`` § Client-state recoverability: a
client dying the instant after any write leaves a box a client can recover).

**The finding it pins (2026-10-07).** ``test_mail_survives_four_key_rotations``
first relaunched right after its last rotation form closed, without waiting
for "All up to date"; a normal MUA's IMAPS ``AUTH PLAIN`` then failed at the
MDA with ``decrypt mls-snapshot: AEAD verify failed``. The rotation's nest
writes (snapshot, re-wrapped blobs, pubkey) are direct RPCs, but its custody
writes (the sentinel, the MSEK swap) were local rows the account runtime
published later; the relaunch hydrated a fresh store from the nest, read the
pre-rotation MSEK, and its connect-time refresh re-sealed the snapshot under
it while every credential's blob named MSEK′. That journey now waits for the
page to settle, which hides the window — so this one relaunches without
waiting, by design.

  mail on (PLAIN) → one pump pass ships the enable rows → **relaunch under
  the publish hold** → Rotate mail keys → confirm → the form closes →
  **relaunch at once** → the relaunched app's connect-time refresh re-seals
  the snapshot (observed: the nest's snapshot row changes) → IMAPS AUTH with
  the surviving credential succeeds → the page reads up to date (no resume
  offered for a rotation that finished) → **relaunch again** → its refresh
  re-seals once more → AUTH still succeeds (nothing drifts across connects).

**Held, so the window is open every run.** The window is the account
runtime's publish latency after a local write, and an unheld pre-fix run of
this journey passed (2026-10-10): the runtime's local-write wake usually
publishes before the relaunch kills the process. So the rotating seat runs
under the runtime's e2e publish hold (``helpers/account_runtime_holds.py``):
while it owes the nest a mail-custody row, no unprompted publish ships one,
and only the rotation's own awaited ``publish`` barrier — rules (1) and (3)
— carries the custody to the nest. On a tree with those barriers reverted the
relaunch hydrates the pre-rotation custody beside the moved nest, every run:
its refresh then either re-seals under the pre-rotation key (AUTH fails — the
2026-10-07 finding) or, rule (4) seeing a key it never walked and no walk to
take, moves nothing (the reseal barrier fails — what the 2026-10-11
demonstration measured, the co-located sync agent holding the engine role).
Red either way. The enable rows are shipped by a pump pass BEFORE the hold
arms, so the relaunched replica reads the pre-rotation custody the real
failure read, not an empty one. The tier_1 trio
in ``libs/fauna-client-mail-settings/tests/rotation_resume.rs`` pins the same
rules on a fake custody that publishes only when asked.

**Non-vacuous.** Each AUTH runs only after the relaunched app's refresh has
re-provisioned the snapshot (the ``bridge_mls_snapshot_blobs`` row's bytes
change — every seal draws a fresh nonce), so the assertion reads the snapshot
the relaunched custody sealed, never the one the rotation left.

tier_3: every binary real, the real IMAPS wire, real process restarts
(``hard_reload()``, replaying the login over a fresh client store).
"""

from __future__ import annotations

import pytest

from helpers.account_runtime_holds import (
    KIND_MAIL,
    PUBLISH_HOLD_ENV,
    relaunch_session_app_with,
)
from helpers.budgets import ORCHESTRATION_STEP_S
from helpers.fleet import poked_pass
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_reseal import imap_auth_and_select, snapshot_blob, wait_for_reseal

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
]

_MUA_PASSWORD = "rotate-plain-pw-1"


@pytest.mark.feature("turn-on-mail")
def test_a_relaunch_right_after_a_key_rotation_keeps_imap_working(
    app, dedicated_mail_nest, request
):
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # ── 1. Mail on (PLAIN), an address routed to this account, the gates
    # rebound (the binaries e2e has no supervisor — see the MUA round-trip).
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    recipient = alias_admin_to_address(nest, domain)
    app.mail_settings.navigate()
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "enabling mail must flip the page to enabled; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    handle.rebind_after_enable()
    imap_auth_and_select(handle, recipient, _MUA_PASSWORD, "before the rotation")

    # ── 2. Ship the enable rows (a full pump pass publishes the whole own
    # log), then relaunch under the publish hold: from here no unprompted
    # publish carries a mail-custody row, so only the rotation's own barriers
    # can put its custody on the nest.
    poked_pass(app.driver, what="mail was enabled")
    relaunch_session_app_with(app, request, {PUBLISH_HOLD_ENV: KIND_MAIL})
    app.mail_settings.navigate()
    assert app.mail_settings.wait_for_enabled_status(timeout=ORCHESTRATION_STEP_S), (
        "relaunched under the publish hold, the page must read mail enabled "
        "— its fresh store hydrated the custody the pump pass shipped; "
        f"status={app.mail_settings.status_text()!r}"
    )
    # ── 3. Rotate; the form closing is the only barrier — no wait for the
    # page to settle, which is the window this journey exists for.
    app.mail_settings.rotate_keys()
    assert app.mail_settings.wait_for_rotation_to_finish(ORCHESTRATION_STEP_S), (
        "the rotation never finished: the rotate form is still open with "
        f"progress {app.driver.get_text('mail-rotate-keys-progress-indicator')!r}"
    )
    rotated = snapshot_blob(nest)

    # ── 4. Relaunch at once; AUTH reads the snapshot the relaunch re-sealed.
    app.driver.hard_reload()
    resealed = wait_for_reseal(nest, rotated, "the first relaunch")
    imap_auth_and_select(
        handle, recipient, _MUA_PASSWORD, "after a relaunch right after the rotation"
    )
    app.mail_settings.navigate()
    assert app.mail_settings.wait_for_enabled_status(timeout=ORCHESTRATION_STEP_S), (
        "the relaunched page must read up to date — the rotation finished before "
        "its form closed, so no resume may be offered; "
        f"status={app.mail_settings.status_text()!r}"
    )

    # ── 5. Another connect-time refresh keeps it working.
    app.driver.hard_reload()
    wait_for_reseal(nest, resealed, "the second relaunch")
    imap_auth_and_select(handle, recipient, _MUA_PASSWORD, "after a second relaunch")

