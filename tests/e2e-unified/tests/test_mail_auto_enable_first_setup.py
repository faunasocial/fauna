"""tier_3 (local CI): the **client-driven** non-admin first-setup mail auto-mint,
proven end-to-end through the believable linux onboarding wizard + a real MDA
bridge.

This is the goal's **"Done ="** (`docs/goal/behavior/mail-credentials.md` §
Auto-enable for new users · `docs/goal/behavior/mail-policy-config.md` § Tier-2
*Auto-enable mail for new users*): a freshly-registered **non-admin** user, after
their **first client setup** on a mail-enabled deployment, can **IMAP-login at
`<handle>@<domain>`** with a generated password — with **no** admin call and
**no** manual `mail-settings` enable.

Why no existing harness fits (so this file exists):
  - the invite/handle-entry tests (`test_handle_entry_outcomes.py`) are tier_2
    snapshot-injection — they never fire the real `provision_mail_at_first_setup`
    glue;
  - the believable mail round-trip (`test_mail_enable_live_nest.py`) is
    live-remote-only (it factory-resets example.com, env-gated), and claims the
    nest as **admin**;
  - `test_mail_bridge_mda.py` pre-provisions the recipient's wrapped-MSEK blob
    out-of-band via the `seal-helper` (no client driver), so it can't prove the
    *client* mint produces an IMAP-loginable mailbox.

What this proves that the tier_1 unit tests
(`libs/fauna-client-mail-settings/tests/state_machine.rs`) cannot: the auto-mint
*glue* fires on the real linux first-setup path **and** the wrapped-MSEK blob the
client mints AEAD-unwraps at the bridge's AUTH — the same client-side seal
`seal-helper seal-wrapped-msek` stands in for in `test_mail_bridge_mda.py`, here
performed by the real client.

Flow (everything a real new user does, in order):
  1. A fresh **non-admin** user self-registers a handle over open registration
     (`fauna.account.register` → `register_handled_actor`) — no admin involved.
  2. **First client setup**: the believable linux wizard. The loopback handle
     `<localpart>@127.0.0.1:<port>` resolves to the local nest
     (`fauna_provisioning::probe::resolve_handle_domain` → `is_local` → skip DNS),
     the handle-check silently authenticates the already-registered secret →
     `AlreadyOnNest{handle_matches}` → Continue → `WizardOutcome::LoggedIn` →
     `launch_main_app_after_signin` → `provision_mail_at_first_setup`
     (`am_i_admin == false`) → the non-admin auto-mint.
  3. The auto-minted `default` credential appears in `mail-settings` with **no**
     manual enable; its generated password reveals via the one-time reveal.
  4. **IMAP-login** at `<localpart>@fauna.test` with that password against the
     real MDA bridge (AUTH PLAIN + SELECT INBOX) — the full client→bridge proof.

Desktop apps (linux + macOS + tui): the linux app wires
`FaunaClient::provision_mail_at_first_setup`; macOS wires the **same** first-setup
auto-mint through shared FaunaKit `MailEnableGlue.provisionMailAtFirstSetup` (fired
from `completeAuthenticatedLaunch`, latch-gated on `pendingFirstSetupMail`); tui
wires the ported `mail_glue::provision_mail_at_first_setup` from
`wizard::handle_wizard_done`'s `WizardOutcome::LoggedIn` arm. All three drive the
believable first-setup wizard end-to-end — on macOS via the in-process driver (no
XCUITest). The remaining clients' lift is tracked elsewhere.
"""

from __future__ import annotations

import secrets
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

# Mirror the sys.path bootstrap of the other local-onboarding e2e files so the
# shared `common` package (tests/common/) and the e2e-unified helpers resolve.
_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from common.auth import register_handled_actor  # noqa: E402
from conftest import (  # noqa: E402
    MAIL_PRIMARY_DOMAIN,
    _spawn_mda_bridge,
    get_available_apps,
)
from helpers.app_surface import skip_unbuilt  # noqa: E402
from helpers.authenticated_shell import SHELL_MARKERS  # noqa: E402
from helpers.mail_wire import (  # noqa: E402
    _imap_auth_plain,
    _imap_read_tagged,
    _imaps_connect,
)

_avail_clients = get_available_apps()
if not any(c in _avail_clients for c in ("linux", "macos", "ios", "tui")):
    pytest.skip(
        "drives the linux/macOS/iOS/tui client first-setup onboarding UI",
        allow_module_level=True,
    )

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


# The authed app shell has mounted once any main-view landmark is visible: this
# test only needs "onboarding is done" before it navigates to mail-settings, and
# apps legitimately differ on where they land. The set is shared (which app lands
# where is documented there) — a private copy is what traced two 120s windows hangs to.
_LOGGED_IN_MARKERS = SHELL_MARKERS


# ── Fixtures: a mail-enabled handled nest + an MDA bridge, recipient-less ──────
#
# The recipient is deliberately NOT pre-provisioned (unlike `mail_bridge_mda`):
# the believable client mint must create the credential the bridge unwraps at
# AUTH — that is the integration under test.


@pytest.fixture(scope="module")
def autoenable_nest(request, nest_mode, tmp_path_factory):
    """A nest with ``handle_domain == primary mail domain == MAIL_PRIMARY_DOMAIN``,
    open self-service registration, committed plaintext storage, and the mail
    subsystem **enabled** — so a fresh handled actor's canonical
    ``<handle>@MAIL_PRIMARY_DOMAIN`` is the routable mailbox and the client's
    first-setup auto-mint gates pass (``email_enabled`` true; the
    ``auto_enable_mail_for_new_users`` policy defaults ON).
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _start_dedicated_nest

    # Serves REAL self-signed HTTPS (`serve_tls=True`): after Pillar C (uniform
    # https) the loopback `…@127.0.0.1:{port}` first-setup handle resolves to
    # `https://…`, so the production-faithful posture is a nest that actually
    # serves TLS there. The native app trusts the self-signed floor via
    # channel-binding (probe + post-LoggedIn SPKI pin); the Python admin/anon
    # WS-RPC clients trust via `CERT_NONE`; and the MDA bridge — a LOOPBACK nest
    # endpoint — skips TLS verification via `nestHTTPClient` (the in-container
    # production path). No `set_provider_base_urls` override is needed.
    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "autoenable-nest",
        # The domained claim, and this nest's ONLY registration of
        # MAIL_PRIMARY_DOMAIN — the `add_local_domain` that used to sit a few
        # lines below is gone, because two doors onto one domain means the
        # loser's arguments are silently discarded (`add_local_domain` is
        # idempotent by domain NAME). The claim's cert mode is `expand_primary`
        # where that call asked for `per_host`, which is inert here: nothing
        # this test reads derives from `mta_sts_cert_mode`. Both doors reach the
        # same `apply_primary_identity`, so `serve_tls=True` arms no issuance
        # the later call was not already arming (`testing.md` § Default app and
        # nest mode, ruling (3)).
        claim_domain=MAIL_PRIMARY_DOMAIN, serve_tls=True,
    )
    from common.auth import open_registration
    open_registration(nest)
    admin_sk = nest["admin"]["signing_key"]

    with WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    ) as ws:
        # The primary local domain (which routes inbound
        # `<...>@MAIL_PRIMARY_DOMAIN`) is already registered — the claim carried
        # it as its `mail_domain`, the way production registers it.
        # Enable the mail subsystem so `fauna.setup.status.email_enabled` is true
        # (auto-mint gate #1). `auto_enable_mail_for_new_users` (gate #2) is the
        # default-ON deployment policy — left unset here, so it reads ON.
        assert ws.call("fauna.bridges.set_mail_enabled", {"enabled": True}).get("ok") is True

    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def autoenable_mda(autoenable_nest, mail_bridge_binary, tmp_path_factory):
    """An MDA-role `fauna-mail-bridge` serving IMAPS for ``MAIL_PRIMARY_DOMAIN``
    against ``autoenable_nest``. No recipient is pre-provisioned — the believable
    client mint creates the wrapped-MSEK credential the bridge AEAD-unwraps at AUTH.

    Exposes ``imaps_port`` + ``domain`` so the `mail_wire` IMAP helpers
    (`_imaps_connect`) duck-type on it.
    """
    tmp = tmp_path_factory.mktemp("autoenable-mda")
    spawned = _spawn_mda_bridge(
        mail_bridge_binary=mail_bridge_binary,
        nest_instance=autoenable_nest,
        tmp=tmp,
        bridge_id="mda-autoenable-1",
        domain=MAIL_PRIMARY_DOMAIN,
    )
    try:
        yield SimpleNamespace(
            imaps_port=spawned.imaps_port,
            domain=MAIL_PRIMARY_DOMAIN,
            spawned=spawned,
        )
    finally:
        spawned.cleanup()


# ── Helpers ───────────────────────────────────────────────────────────────────


def _drive_first_setup_to_logged_in(app, secret_hex: str, typed_handle: str) -> None:
    """Drive the believable first-setup wizard (linux or macOS in-process) for an
    already-registered non-admin user from identity import to the logged-in feed.

    Platform-agnostic: `app.onboarding.*` drives the shared ui.yaml onboarding IDs
    on both apps, and the welcome-back consent buttons are the same IDs on linux
    and macOS. The logged-in landmark differs only by default landing view
    (`_LOGGED_IN_MARKERS`).

    The user is registered, so the handle-check resolves the loopback nest →
    `AlreadyOnNest{handle_matches}` → Continue → `WizardOutcome::LoggedIn`. The
    welcome-back path may surface the terminal nat_mode_choice step or a
    launch screen — click through whatever appears until the authed app
    renders.
    """
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)  # lands on handle_entry
    ob.fill_handle(typed_handle)
    # Pillar C (uniform https): the loopback `…@127.0.0.1:{port}` handle resolves to
    # `https://127.0.0.1:{port}` and the nest serves real self-signed HTTPS there
    # (`serve_tls=True`), so NO `set_provider_base_urls` override is needed — the
    # probe AND the post-LoggedIn authed connection (which provisions the mailbox)
    # reach the nest directly and trust the self-signed floor via the graduated SPKI pin.
    ob.run_handle_check(timeout=45)  # local nest probe → AlreadyOnNest
    ob.submit_handle()

    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        if any(app.driver.is_visible(m) for m in _LOGGED_IN_MARKERS):
            return
        # Drive through any consent / launch screens the AlreadyOnNest welcome-back
        # path surfaces (the terminal NAT-mode confirm per onboarding.md §
        # 3b-bis, launch retry).
        for btn in (
            "nat-mode-confirm-button",
            "launch-retry-button",
        ):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        time.sleep(2.0)

    # Diagnostic failure — dump what's on screen.
    err = ""
    for eid in ("launch-transient-error", "error-message"):
        try:
            if app.driver.is_visible(eid):
                err = app.driver.get_text(eid)
                break
        except Exception:
            pass
    try:
        tree = app.driver.tree()
    except Exception as e:  # pragma: no cover - diagnostic only
        tree = f"(tree dump failed: {e})"
    pytest.fail(
        "client never reached the authed app after the non-admin first-setup "
        f"onboarding. launch error: {err!r}\n--- accessibility tree ---\n{tree}"
    )


def _wait_for_auto_minted_credential(app, timeout: float = 60.0) -> bool:
    """Wait for the auto-minted `default` credential to surface on the
    mail-settings page, re-opening the page each iteration.

    The auto-mint is a fire-and-forget background task that completes ~1-2s
    *after* the feed renders (it runs on a separate `MailSettingsMachine`). The
    linux settings sub-stack is built once at app launch and re-hydrates the mail
    page only when it is *shown* (`connect_map`), so a single show right after
    onboarding can race ahead of the mint. A real user opening mail-settings to
    read their generated password does so well after the mint; this mirrors that
    by re-showing the page (away → back) until the credential appears.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        # Show a different settings sub-page (unmaps mail), then re-show mail —
        # the re-map fires the page's `connect_map` re-hydrate against the now
        # post-mint mail state.
        app.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "account"}]},
        })
        app.mail_settings.navigate()
        if app.mail_settings.wait_for_credential_count_at_least(1, timeout=6.0):
            return True
    return False


# ── The test ──────────────────────────────────────────────────────────────────


@pytest.mark.feature("turn-on-mail")
def test_non_admin_first_setup_auto_mints_and_imap_logs_in(
    app, autoenable_nest, autoenable_mda
):
    """Fresh non-admin user → first client setup → auto-mint → IMAP-login, with
    no admin call and no manual mail-settings enable (the goal's "Done =")."""
    if not (
        app.driver.is_linux()
        or app.driver.is_macos()
        or app.driver.is_ios()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="first-setup mail auto-mint",
            detail="linux wires FaunaClient::provision_mail_at_first_setup; "
            "macOS + iOS wire the same first-setup auto-mint via the shared "
            "FaunaKit MailEnableGlue (iOS confirmed in-process 2026-06-17); "
            "tui wires the ported mail_glue::provision_mail_at_first_setup "
            "(2026-07-19); web/windows/android are the remaining cross-app "
            "follow-on",
            tracked="mail-settings.md",
        )

    nest = autoenable_nest

    # Relaunch trusting `nest` BEFORE the wizard below points the app at it —
    # a relaunch inside `_drive_first_setup_to_logged_in` would be too late,
    # the same reasoning as the recovery-kit journeys.
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    # 1) A fresh NON-ADMIN user self-registers over open registration. No admin
    #    call — the user provisions their own handle (the works-out-of-box
    #    invariant, extended to every user).
    localpart = "newuser" + secrets.token_hex(3)
    actor = register_handled_actor(
        nest["port"], handle=localpart, domain=MAIL_PRIMARY_DOMAIN,
        base_url=nest["url"],  # serve_tls nest → dial the https base for registration
    )
    secret_hex = actor["signing_key"].encode().hex()
    address = f"{localpart}@{MAIL_PRIMARY_DOMAIN}"

    # 2) First client setup through the believable linux wizard. The loopback
    #    handle drives nest discovery; the already-registered secret yields
    #    AlreadyOnNest → LoggedIn → launch_main_app_after_signin →
    #    provision_mail_at_first_setup (non-admin branch) → auto-mint.
    typed_handle = f"{localpart}@127.0.0.1:{nest['port']}"
    _drive_first_setup_to_logged_in(app, secret_hex, typed_handle)

    # 3) The auto-mint fired: a `default` mail credential is present in
    #    mail-settings WITHOUT any manual enable (the goal's "no manual step").
    assert _wait_for_auto_minted_credential(app, timeout=60.0), (
        "non-admin first-setup did NOT auto-mint a mail credential — the "
        "provision_mail_at_first_setup glue did not fire or the gates rejected. "
        f"mail-page error: {app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )
    password = app.mail_settings.reveal_credential_secret(0)
    assert password, "the auto-minted credential's generated password did not reveal"

    # 4) IMAP-login at <handle>@<domain> with the auto-generated password. The
    #    client-minted wrapped-MSEK blob must AEAD-unwrap at the bridge's AUTH
    #    (UnwrapMsekBlob) — the full client→bridge integration the tier_1 unit
    #    tests cannot reach.
    deadline = time.monotonic() + 45.0
    sock, buf = _imaps_connect(autoenable_mda, deadline)
    with sock:
        status = _imap_auth_plain(sock, buf, "a1", address, password, deadline)
        assert status == "OK", (
            f"AUTH PLAIN at {address} with the auto-minted password must succeed "
            f"(the client mint must produce a bridge-unwrappable wrapped-MSEK "
            f"blob); got {status}"
        )
        sock.sendall(b"a2 SELECT INBOX\r\n")
        status, untagged = _imap_read_tagged(sock, buf, "a2", deadline)
        assert status == "OK", f"SELECT INBOX must succeed; got {status}: {untagged!r}"
        assert any(ln.upper().endswith("EXISTS") for ln in untagged), (
            f"SELECT must report an EXISTS line; got {untagged!r}"
        )
        sock.sendall(b"a3 LOGOUT\r\n")
