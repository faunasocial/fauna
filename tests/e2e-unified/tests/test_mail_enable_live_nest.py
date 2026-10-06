"""tier_4 (live-remote, OPT-IN): the **believable, 100%-client-driven** mail
round-trip against a LIVE nest (e.g. example.com), using ONLY the linux Fauna
app UI + a Python IMAP client — no nest-API *config* calls, no `set_state`
session shortcut.

Flow (everything a real operator/user would do, in order):
  0. **Bootstrap factory-reset** the live nest back to fresh/unclaimed (the one
     bootstrap WS-RPC call — a *reset*, not config — via `fauna.admin.factory_reset`,
     authenticated with the admin secret). Guarantees a clean claimable state
     regardless of any prior run; the reply carries the new claim code, so the
     test drives the re-claim with it. The box restarts into a wipe that
     preserves the host identity + the real ACME cert (so it stays reachable
     over TLS). See `docs/goal/architecture/nest/common.md` § Factory reset.
  1. **Onboard from scratch through the linux UI**: import the identity, type the
     handle `test@example.com` (the client resolves the domain to the nest via
     real DoH — `resolve_handle_domain("example.com") → https://example.com`), run the
     handle check (discovers the unclaimed nest), submit, enter the claim code on
     the claim-code page, and confirm the terminal nat_mode_choice step — all UI
     (no-modes retirement, ratified 2026-07-12: claim lands directly on
     nat_mode_choice, no intervening storage-mode question). The handle binds
     the admin's `test@example.com` recipient identity (`mail_domain` auto-registered
     at claim). Now logged in as admin.
  1b. **T3 — factory reset through the UI button.** Click "Factory reset this
     nest" on the admin-settings Danger zone (`admin-factory-reset-button`),
     confirm the destructive dialog, and let the client re-seed onboarding at the
     claim-code page with the returned code **pre-filled** (the human never sees
     it). Re-claim by submitting that pre-filled code and re-confirming
     nat_mode_choice — proving the fully-UI factory-reset → re-onboard loop, the
     human-facing equivalent of step 0. See `docs/goal/behavior/mail-bridge-lifecycle.md` §
     Factory reset.
  2. **Enable mail** (PLAIN password) through the mail-settings UI.
  3. **Approve the co-located mail bridge** through the admin UI. With the real
     ACME cert on disk, the bridge serves the publicly-trusted cert on
     `mail.example.com:993` (seal-on-read `fetch_tls_cert_blob`), so the Python
     IMAP client verifies it normally — no insecure-TLS escape hatch.
  4. **Receive a mail via Python IMAP** (`LOGIN` over TLS — the command the MDA
     now supports — then `APPEND`). The MDA seals the appended body to the
     actor's MLS pubkey, so it becomes a normal sealed mail record.
  5. **See that same mail in the client's conversations view** (UI).

**Skipped unless these are set** (it hits a live external box and is
destructive — it factory-resets the nest):
  --nest live:URL --live-box disposable
                            THE OPT-IN — this test is DESTRUCTIVE (it factory-resets the box over WS-RPC), so
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
                            The seed MUST currently be the box's admin (the
                            reset is admin-gated); the test re-onboards as
                            this same identity, so it stays admin across runs.
  mailbox                   resolved per box (``live_box_door.mailbox``):
                            FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD
                            > the box's staging-box file ``handle`` /
                            ``mail_password`` (the identity it was provisioned
                            under); each var overrides its own field.
                            The address is the re-claim handle; the
                            password is the PLAIN mail password = IMAP login.
Optional:
  FAUNA_CONV_POLL_SECS=5 (export) so the client polls the mailbox quickly.

End goal: once this passes, a real MUA (macOS Mail / Thunderbird) connects with
the SAME address + password over the box's real publicly-trusted cert.
"""

import imaplib
import ssl
import time

import pytest

from helpers.app_surface import skip_unbuilt
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
        reason="believable live-nest mail test: provide the box's admin seed "
        + f"({live_box_door.SEED_SOURCES}) and its mailbox ({live_box_door.MAILBOX_SOURCES})"
        + " to run (hits + factory-resets a live external nest; opt-in)",
    ),
]

DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS
MAIL_HOST = f"mail.{DOMAIN}"
IMAP_PORT = 993


# ── Phase 0: factory reset (the one bootstrap call — a reset, not config) ──────


def _factory_reset_and_wait() -> str:
    """Factory-reset the live nest as admin; return the new claim code.

    Drives `fauna.admin.factory_reset` over the authenticated WS-RPC connection
    (admin-gated). The reply carries the post-reset claim code. The nest then
    exits + restarts into the wipe, so we poll `fauna.setup.status` until the box
    is back AND fresh (`claimed == False`, `admin_exists == False`).
    """
    from nacl.signing import SigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = SigningKey(bytes.fromhex(SECRET))
    actor = bytes(sk.verify_key)

    with WsRpcAdminClient(URL, actor_id=actor, signing_key=bytes(sk)) as adm:
        reply = adm.call("fauna.admin.factory_reset", {})
    claim_code = reply["claim_code"]
    assert claim_code, f"factory_reset returned no claim code: {reply!r}"

    # Wait for the restart + wipe to land a fresh, unclaimed nest.
    _wait_nest_fresh()
    return claim_code


def _wait_nest_fresh(timeout: float = 120.0) -> None:
    """Poll `fauna.setup.status` until the nest is back AND fresh (`claimed` and
    `admin_exists` both False), tolerating the mid-restart window. Asserts the
    TLS cert survived the wipe. Shared by the WS-RPC bootstrap and the T3
    UI-driven reset (which also exits + restarts the nest)."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        time.sleep(4.0)
        try:
            with WsRpcAnonClient(URL) as anon:
                st = anon.call("fauna.setup.status", {})
            last = st
            if st.get("claimed") is False and st.get("admin_exists") is False:
                assert st.get("tls_active") is True, (
                    f"factory reset must preserve the TLS cert: {st!r}"
                )
                return
        except Exception:
            pass  # nest mid-restart — keep polling
    pytest.fail(
        f"nest did not return to fresh/unclaimed within {timeout:.0f}s after "
        f"factory_reset (last setup.status: {last!r})"
    )


# ── Phase 1: real onboarding through the linux UI ─────────────────────────────


def _onboard_and_claim(app, claim_code: str) -> None:
    """Drive the linux onboarding wizard from identity import to logged-in.

    identity import → handle entry (`test@example.com`, resolved to the nest by
    real DoH) → handle check → submit → claim-code page → nat_mode_choice
    → logged in. All UI; the handle binds the recipient identity.
    """
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(SECRET)  # lands on handle_entry

    ob.fill_handle(ADDRESS)
    ob.run_handle_check(timeout=40)  # real DoH probe of the handle's domain
    ob.submit_handle()  # → claim_code page (UnregisteredUnclaimedNest outcome)

    # Claim-code page: enter the code from factory_reset, submit.
    app.driver.wait_for("claim-code-input", timeout=40)
    app.driver.clear_and_type("claim-code-input", claim_code)
    app.driver.click("claim-code-submit-button")

    # NAT-mode choice — the terminal admin-path step (onboarding.md § 3b-bis),
    # reached directly on claim completion (no-modes retirement, ratified
    # 2026-07-12). Confirm the seeded mode; the wizard only reaches Done after
    # this.
    app.driver.wait_for("nat-mode-confirm-button", timeout=40)
    app.onboarding.finish_nat_mode()

    # Logged in → the feed view renders. A cold launch against a *remote* nest
    # (WS connect + initial sync over the internet) is slower than a local one,
    # and the launch screen may surface a transient connect error that needs a
    # retry click, so poll generously and drive the retry button if it appears.
    _wait_logged_in(app, timeout=150.0)


def _wait_logged_in(app, timeout: float = 150.0) -> None:
    """Wait for the feed view after claim, driving the launch screen's retry on a
    transient connect error. Fails with the launch error text for diagnosis."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if app.driver.is_visible("feed-view") or app.driver.is_visible("feed-tab"):
            return
        if app.driver.is_visible("launch-retry-button"):
            try:
                app.driver.click("launch-retry-button")
            except Exception:
                pass
        time.sleep(2.0)
    err = ""
    for eid in ("launch-transient-error", "error-message"):
        try:
            if app.driver.is_visible(eid):
                err = app.driver.get_text(eid)
                break
        except Exception:
            pass
    # Dump the accessibility tree so the stuck screen is identifiable.
    tree = ""
    try:
        tree = app.driver.tree()
    except Exception as e:
        tree = f"(tree dump failed: {e})"
    visible = {
        eid: app.driver.is_visible(eid)
        for eid in (
            "nat-mode-confirm-button",
            "launch-retry-button",
            "feed-tab",
            "feed-view",
        )
    }
    pytest.fail(
        f"client never reached the feed view within {timeout:.0f}s after the "
        f"claim (the claim itself succeeded on the nest). "
        f"launch error: {err!r}; candidate visibility: {visible}\n"
        f"--- accessibility tree ---\n{tree}"
    )


# ── Phase 1b: T3 — factory reset through the UI button, then re-claim ──────────


def _reclaim_after_ui_reset(app) -> None:
    """After the T3 UI factory-reset (`app.admin.factory_reset_via_ui`), the
    client has torn down the session and re-seeded onboarding on the claim-code
    page with the returned code pre-filled. Wait for the nest to come back
    fresh, submit the pre-filled code (NOT typed — the human never sees it),
    confirm the terminal nat_mode_choice step, and wait for the feed. The
    human-facing equivalent of `_onboard_and_claim`'s claim → mode →
    logged-in tail."""
    # The button's handler fired `fauna.admin.factory_reset`; the nest is now
    # restarting into the wipe. Wait for it to come back unclaimed.
    _wait_nest_fresh()

    # Submit the pre-filled code. The nest may still be settling, so the
    # claim-code page can surface a transient error and stay put; retry submit
    # until nat_mode_choice appears.
    app.driver.wait_for("claim-code-submit-button", timeout=40)
    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        if app.driver.is_visible("nat-mode-confirm-button"):
            break
        if app.driver.is_visible("claim-code-submit-button"):
            try:
                app.driver.click("claim-code-submit-button")
            except Exception:
                pass
        time.sleep(3.0)

    # NAT-mode choice — the terminal admin-path step (onboarding.md § 3b-bis).
    # A factory reset clears the nest_nat_mode row, so the page is re-asked.
    app.driver.wait_for("nat-mode-confirm-button", timeout=40)
    app.onboarding.finish_nat_mode()
    _wait_logged_in(app)


# ── Phase 2/3: enable mail + approve bridge (existing UI surfaces) ─────────────


def _ensure_mail_enabled(app) -> None:
    """Enable mail (PLAIN, the known password) through the mail-settings UI."""
    app.mail_settings.navigate()
    if app.mail_settings.wait_for_credential_count_at_least(1, timeout=12.0):
        return  # already enabled (idempotent across re-runs of phase 2)
    app.mail_settings.enable_mail_plain(PASSWORD, display_name="Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=30.0), (
        "enabling mail did not mint a credential — the WS-RPC enable round-trip "
        f"failed. mail-page error: {app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )


def _ensure_bridge_serving(app) -> None:
    """Approve any pending mail bridge through the admin UI. No-op if none pending
    (already approved, or email-on-by-default auto-approved it)."""
    app.admin.navigate_bridges_pending()
    deadline = time.monotonic() + 60.0
    while time.monotonic() < deadline:
        n = app.driver.count("admin-bridges-pending-pubkey-hex")
        if n == 0:
            break
        for i in range(n):
            try:
                app.driver.click("admin-bridges-pending-approve-button", index=i)
            except Exception:
                pass
        time.sleep(3.0)


def _wait_imap_serving(host: str, port: int, timeout: float = 150.0) -> None:
    import socket

    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=4.0):
                return
        except OSError as e:
            last = str(e)
            time.sleep(3.0)
    pytest.fail(
        f"{host}:{port} (IMAPS) never accepted a connection within {timeout:.0f}s "
        f"(last: {last}). The bridge isn't serving and/or the firewall doesn't "
        f"allow {port}."
    )


# ── The test ──────────────────────────────────────────────────────────────────


@pytest.mark.feature("email-in-conversations")
def test_believable_live_mail_roundtrip(app):
    """Factory-reset → onboard → enable mail → IMAP receive → see in conversations,
    all client-driven (+ Python IMAP as the external MUA)."""
    if not app.driver.is_linux():
        skip_unbuilt(
            app.driver,
            surface="the believable live-nest mail round-trip drive",
            detail="drives the linux Fauna app UI + conversations; the "
            "other apps are the remaining cross-app follow-on",
            tracked="mail-settings.md",
        )

    # -1) Preflight: the reset below is admin-gated, so a box that does not know
    #     the resolved seed (the wrong box's seed, or one already reset) skips as
    #     environment, naming the seed's source, instead of failing the reset.
    live_box_door.preflight_admin(URL, SECRET, SECRET_SOURCE)

    # 0) Bootstrap to a clean claimable state. One WS-RPC factory_reset (a
    #    reset, not config) guarantees the box is fresh regardless of any prior
    #    run, so the UI onboard below has a code to claim with.
    claim_code = _factory_reset_and_wait()

    # 1) Onboard from scratch through the UI and claim test@example.com → admin.
    _onboard_and_claim(app, claim_code)

    # 1b) T3 — exercise the human-facing "Factory reset this nest" button on the
    #     admin-settings Danger zone, then re-claim through the wizard's
    #     pre-filled claim code. This proves the fully-UI factory-reset →
    #     re-onboard loop end to end (the claim code is never typed; the client
    #     carries it from the reply into the input).
    app.admin.factory_reset_via_ui()
    _reclaim_after_ui_reset(app)

    # 2) Enable mail (PLAIN password) via the UI.
    _ensure_mail_enabled(app)

    # 3) Approve the co-located bridge + wait for IMAPS to serve.
    _ensure_bridge_serving(app)
    _wait_imap_serving(MAIL_HOST, IMAP_PORT)

    # 4) Receive a mail via Python IMAP (LOGIN over TLS + APPEND).
    nonce = f"liveimap{int(time.time())}qx"
    raw = (
        "\r\n".join([
            f"From: Self <{ADDRESS}>",
            f"To: {ADDRESS}",
            f"Subject: {nonce}",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"IMAP-APPEND body carrying {nonce}.",
        ])
        + "\r\n"
    ).encode()

    ctx = ssl.create_default_context()  # full verification — real ACME cert
    imap = imaplib.IMAP4_SSL(MAIL_HOST, IMAP_PORT, ssl_context=ctx)
    try:
        imap.login(ADDRESS, PASSWORD)  # exercises the IMAP LOGIN command (T2)
        typ, data = imap.append(
            "INBOX", "", imaplib.Time2Internaldate(time.time()), raw
        )
        assert typ == "OK", f"IMAP APPEND failed: {typ} {data!r}"
    finally:
        try:
            imap.logout()
        except Exception:
            pass

    # 5) See the same mail in the client's conversations view (UI navigation).
    app.conversations.navigate()
    deadline = time.monotonic() + 90.0
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
