"""tier_4 (live-remote, OPT-IN): a believable mail RECEIVE round-trip against a
live nest (example.com), where **the test uses NO nest APIs** — every nest-side
step is driven through the linux app UI. The only non-UI component is a plain
**IMAP client acting as the external MUA** (the stand-in for macOS Mail /
Thunderbird), which is exactly the second mail party a real inbound needs.

Difference from the two siblings:
  - `test_mail_client_full_roundtrip.py` (local nest) fakes DNS discovery, injects
    the admin session via `set_state`, and provisions cert/DKIM/spam over WS-RPC.
  - `test_mail_enable_live_nest.py` (example.com) is cleaner but still calls THREE
    nest APIs: a bootstrap `fauna.admin.factory_reset`, `fauna.setup.status`
    polling, and admin WS-RPC.

This test calls NONE of those. The flow:

  1. **Reach the admin shell, deciding like a user would.** Import the admin
     secret, type `test@example.com`, run the REAL DoH handle check, then branch on
     the wizard's own outcome:
       - unclaimed → claim with `FAUNA_LIVE_CLAIM_CODE` (UI),
       - already claimed (we're the admin) → sign in (UI).
  2. **Factory-reset the nest through the admin Danger-zone button**, then
     re-onboard through the wizard (the new claim code is carried PRE-FILLED in
     the reply — never typed). The complete operator wipe→re-claim loop, entirely
     client-driven; normalizes to a known-fresh nest so the test is self-contained
     from any starting state.
  3. **Enable mail** (PLAIN password) through the mail-settings UI.
  4. **Approve the co-located mail bridge** through the admin UI (no-op if already
     approved / auto-approved).
  5. **Deliver a real inbound mail via a plain IMAP client** — `LOGIN` over the
     box's real publicly-trusted cert, then `APPEND`. The MDA seals the appended
     body to the actor's MLS pubkey, so it becomes a normal sealed mail record.
     This is the ONLY non-UI step, and it's the MUA, not a nest API. macOS Mail
     replaces it later.
  6. **See that same mail in the client's conversations view** (UI).

Navigation note: the linux e2e driver implements every "go to page X" via the
client's nav-state hook (`set_state({"nav": ...})`) — the platform's standard
equivalent of clicking a tab, used by ALL linux e2e tests. The test does NOT
inject a session, fake auth, or inject the inbound message; it really signs
in/claims, really resets, and the inbound is a real IMAP delivery.

Preconditions (env):
  --nest live:URL --live-box disposable
                            THE OPT-IN — this test is DESTRUCTIVE (it factory-resets the box through the UI), so
                            it runs only on a run that declares its box
                            disposable, which only a staging box can be
                            (testing.md § The shared-box rule → The
                            disposable-box declaration). The box is that run's
                            (``live_box_door.destructive_live_box``) — an
                            exported FAUNA_LIVE_NEST_URL never picks it alone.
  admin seed                resolved per box (``live_box_door.admin_seed``:
                            FAUNA_LIVE_SECRET_HEX > the box's
                            ``~/.config/fauna/staging-box/<host>.json`` >
                            ``~/.fauna-id``); ambient, so never the opt-in.
  mailbox                   resolved per box (``live_box_door.mailbox``):
                            FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD
                            > the box's staging-box file ``handle`` /
                            ``mail_password`` (the identity it was provisioned
                            under); each var overrides its own field.
                            The address is the re-claim handle; the
                            password is the PLAIN mail password = IMAP login.
  FAUNA_LIVE_CLAIM_CODE     required only if the nest is UNCLAIMED at start
Optional:
  FAUNA_CONV_POLL_SECS=5 (export) so the client polls the mailbox quickly

Test taxonomy: tier_4 (live-remote — the deployed production image under real supervision; ruling 2026-07-22). The test contains no
nest API calls; the IMAP client is the external MUA.
"""

import imaplib
import os
import ssl
import time

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.live_admin import (
    approve_pending_bridges,
    enable_mail,
    reach_admin_shell,
    wait_connected,
    wait_logged_in,
    wait_port_open,
)
from helpers import live_box_door

# DESTRUCTIVE (see the module docstring): the box is the run's
# declared-disposable one and the admin seed is resolved for THAT box
# (`live_box_door.destructive_live_box` / `admin_seed`). The seed is ambient —
# a staging-box file or `~/.fauna-id` — so it is never the opt-in; the run's
# `--live-box disposable` declaration is (testing.md § The shared-box rule).
URL, _NOT_DISPOSABLE = live_box_door.destructive_live_box()
SECRET, SECRET_SOURCE = live_box_door.admin_seed(URL)

ADDRESS, PASSWORD = live_box_door.mailbox(URL)

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_box,
    pytest.mark.cd_suite,
    pytest.mark.skipif(_NOT_DISPOSABLE is not None, reason=_NOT_DISPOSABLE or ""),
    pytest.mark.skipif(
        not (SECRET and ADDRESS and PASSWORD),
        reason="live-nest UI-only mail receive test: provide the box's admin seed "
        + f"({live_box_door.SEED_SOURCES}) and its mailbox ({live_box_door.MAILBOX_SOURCES})"
        + " to run (hits a live external nest; opt-in)",
    ),
]

CLAIM_CODE = os.environ.get("FAUNA_LIVE_CLAIM_CODE", "")
DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS
MAIL_HOST = f"mail.{DOMAIN}"
IMAP_PORT = 993


# ── step 2: factory-reset the nest + re-onboard, entirely through the UI ────────


def _reclaim_after_ui_reset(app) -> None:
    """After `app.admin.factory_reset_via_ui()` fired the reset through the
    Danger-zone button, the nest is restarting into the wipe and the client
    re-seeded onboarding on the claim-code page with the new code PRE-FILLED
    (carried in the reply — never typed, never read off the box). Re-claim by
    submitting that pre-filled code and re-confirming nat_mode_choice. We wait
    for the nest to come back purely by watching the wizard advance — no API
    poll."""
    app.driver.wait_for("claim-code-submit-button", timeout=60)
    deadline = time.monotonic() + 180.0
    while time.monotonic() < deadline:
        if app.driver.is_visible("nat-mode-confirm-button"):
            break
        if app.driver.is_visible("claim-code-submit-button"):
            try:
                app.driver.click("claim-code-submit-button")  # code already filled
            except Exception:
                pass
        time.sleep(3.0)
    # NAT-mode choice — the terminal admin-path step (§ 3b-bis). A factory
    # reset clears the nest_nat_mode row, so the page is re-asked on reclaim.
    app.driver.wait_for("nat-mode-confirm-button", timeout=60)
    app.onboarding.finish_nat_mode()
    wait_logged_in(app)


# ── the test ────────────────────────────────────────────────────────────────────


@pytest.mark.feature("email-in-conversations")
def test_live_mail_receive_ui_only(app):
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the live mail receive-and-render-in-conversations drive",
            detail="drives the linux Fauna app UI + conversations; the "
            "other apps are the remaining cross-app follow-on",
            tracked="mail-settings.md",
        )

    # 0) Preflight (skipped with a claim code: an unclaimed box is then the
    #    expected start): a box that does not know the resolved seed skips as
    #    environment, naming the seed's source.
    if not CLAIM_CODE:
        live_box_door.preflight_admin(URL, SECRET, SECRET_SOURCE)

    # 1) Reach the admin shell purely through the wizard (claim or sign in).
    state = reach_admin_shell(app, nest_url=URL, secret_hex=SECRET, claim_code=CLAIM_CODE)
    print(f"\n[zero-cheat] reached admin shell via: {state}")

    # Wait for the WS-RPC to actually connect before driving any nest-backed UI
    # (the shell renders before connect() completes).
    wait_connected(app)

    # 2) Factory-reset the nest through the admin Danger-zone button and
    #    re-onboard through the wizard (pre-filled claim code) — the full
    #    operator loop, entirely client-driven. Normalizes to a known-fresh nest
    #    so the test is self-contained from any starting state.
    app.admin.factory_reset_via_ui()
    _reclaim_after_ui_reset(app)
    wait_connected(app)

    # 3) Enable mail (PLAIN password) via the mail-settings UI.
    enable_mail(app, PASSWORD)

    # 4) Approve the co-located mail bridge via the admin UI, wait for IMAPS.
    approve_pending_bridges(app)
    wait_port_open(MAIL_HOST, IMAP_PORT)

    # 5) Deliver a real inbound via a plain IMAP client (the external MUA — the
    #    macOS Mail stand-in). LOGIN over the real cert, then APPEND.
    nonce = f"uionly{int(time.time())}qx"
    raw = (
        "\r\n".join([
            f"From: Sender <{ADDRESS}>",
            f"To: {ADDRESS}",
            f"Subject: {nonce}",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"IMAP-APPEND body carrying {nonce}.",
        ]) + "\r\n"
    ).encode()

    ctx = ssl.create_default_context()  # full verification — real ACME cert
    imap = imaplib.IMAP4_SSL(MAIL_HOST, IMAP_PORT, ssl_context=ctx)
    try:
        imap.login(ADDRESS, PASSWORD)
        typ, data = imap.append("INBOX", "", imaplib.Time2Internaldate(time.time()), raw)
        assert typ == "OK", f"IMAP APPEND failed: {typ} {data!r}"
    finally:
        try:
            imap.logout()
        except Exception:
            pass

    # 6) See the same mail in the client's conversations view (UI).
    app.conversations.navigate()
    deadline = time.monotonic() + 120.0
    found = None
    while time.monotonic() < deadline and found is None:
        for t in app.conversations.list_threads():
            if nonce in (t.label or "") or nonce in (t.snippet or ""):
                found = t
                break
        if found is None:
            time.sleep(2.0)

    assert found is not None, (
        f"the IMAP-appended message {nonce!r} never surfaced in the client's "
        f"conversations — the receive path (poll/push → open_inbound_record → "
        f"render) did not complete.\n  threads: "
        f"{[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
        f"  conversations error: {app.error_text()!r}"
    )
    assert found.rail == "Smtp", f"mail must land on the Smtp rail; got {found.rail!r}"
    print(f"[zero-cheat] RECEIVED in conversations: label={found.label!r} rail={found.rail!r}")
