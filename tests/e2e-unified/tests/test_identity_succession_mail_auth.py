"""tier_3: after a succession, the predecessor's mail password must stop
**authenticating** at the bridge.

**Why this file exists — row 71 proved a proxy, not the property.** The mail
burn's client-side journey (`test_identity_succession_aftermath.py`
::`test_the_successors_mail_passwords_are_burned`) asserts three *client-side*
statements: the rows render revoked, the stored secret is emptied, the progress
line names the done arm. All three are proxies for the only thing the user
actually cares about — that the password sitting in the thief's Thunderbird
stops working. A burn that marked every row and emptied every secret while
leaving the credential reachable at the bridge would pass that journey
completely and leave the mailbox readable.

`mail-credentials.md` § Rotation and recovery → *Succession* makes the claim
this file tests, in its third bullet: *"The thief's fetch path is already cut
before this leg runs, by the ceremony itself"* — bridge AUTH resolves the
address to an actor at AUTH time, and the succession transaction moves the
handle, so the predecessor's address no longer reaches any blob they can open.
That is a claim about **bridge AUTH**, and until this file no test on the
succession track touched bridge AUTH at all.

**The pre-ceremony IMAP login is the whole test.** Without it a post-ceremony
`NO` could mean the login never worked in the first place — a wrong domain, a
missing alias, an unprovisioned recipient — and the assertion would pass
vacuously while proving nothing. It is the same discipline as the
pre-ceremony reveal, one layer further out: there, bytes that must stop
revealing; here, a password that must stop authenticating.

**Why a dedicated nest, when the client-side journey needed none.** An IMAP
assertion needs a real MDA bridge and a routable `<handle>@<domain>` mailbox.
`dedicated_mail_nest_handle_domain` is the fixture that gives both: a claimed
`fauna.test` local domain, an MDA serving IMAPS for it, and — the piece the
default `dedicated_mail_nest` lacks — a nest whose **handle domain IS the mail
domain**, so a freshly registered actor's canonical `<handle>@fauna.test` is a
real mailbox with no admin aliasing. Do NOT push this shape back onto the
journey: it rides the shared session nest deliberately, it is green, and the
two files answer different questions at very different costs.

tier_3 (mocking depth): real nest + real MDA bridge + the real client UI minting
the credential + real IMAP wire AUTH. Nothing below this tier can see the
composition — a crate-level fake cannot tell you which actor an address resolves
to on a nest that has claimed a mail domain.
"""

from __future__ import annotations

import time

import pytest

from helpers.mail_wire import _imap_auth_plain, _imaps_connect
from helpers.succession_ceremony import require_stolen_gate
from helpers.waiting import wait_until

from .test_identity_succession_aftermath import (
    _MAIL_BURN_S,
    _require_recovery_section,
    _run_succession,
)

pytestmark = pytest.mark.tier_3

# Chosen (typed) password, auto-generate OFF, so the MUA authenticates with a
# known value — the same reason `test_mail_bare_username_auth.py` types one.
# a-zA-Z0-9 only: pastes cleanly through SASL PLAIN.
_PASSWORD = "SuccessionBurnPlainPw0007Kx"  # gitleaks:allow

# The mailbox must exist before the ceremony can take it away: enable-mail mints
# the credential, provisions the recipient pubkey, and (nest-side) writes the
# canonical `<handle>@<domain>` alias. One client round trip on a loaded box.
_ENABLE_S = 30.0


def _require_mail_settings(app) -> None:
    """Skip, declaring the class, on an app with no mail-settings page."""
    if app.mail_settings.is_page_visible():
        return
    from helpers.app_surface import skip_unbuilt

    skip_unbuilt(
        app.driver,
        surface="mail-settings-enabled-toggle",
        detail=(
            "mail-settings.md § User actions — the mailbox enable + credential "
            "list; tui leads and the other six follow in batched trickle-down"
        ),
        tracked=(
            "docs/goal/behavior/mail-credentials.md "
            "§ Rotation and recovery → Succession"
        ),
    )


def _imap_auth(handle, address: str, password: str) -> str:
    """One full IMAPS connect + `AUTHENTICATE PLAIN`, returning the tagged status.

    A **fresh connection every time**, which is what makes the after-assertion
    mean anything: the bridge holds the unwrapped capability for the life of an
    authenticated session (`imap/auth.go` § finishAuth), so reusing the
    pre-ceremony socket would be asserting against a cache, not against the
    nest's current answer. Each call re-runs `validate_recipient` +
    `fetch_wrapped_mls_blob` from scratch.
    """
    deadline = time.monotonic() + 60.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        return _imap_auth_plain(sock, buf, "a1", address, password, deadline)


@pytest.mark.feature("take-your-account-back")
def test_a_succession_stops_the_predecessors_mail_password_authenticating(
    app, request, dedicated_mail_nest_handle_domain
):
    """enable mail with a known password → IMAP-login with it → succeed → the
    same password must no longer authenticate.

    ⚠ **The assertion ORDER is load-bearing and it is not the order of
    interest.** The pre-ceremony login comes first because it is the only thing
    that makes the post-ceremony refusal non-vacuous; a file that asserted only
    the refusal would stay green if the mailbox had never worked at all.

    ⚠ **A post-ceremony refusal does not by itself prove the burn deleted
    anything.** `mail-credentials.md`'s third bullet says the *ceremony* cuts the
    fetch path (address→actor resolution at AUTH time), independently of leg 6's
    blob deletes. That is why this assertion is a companion to the
    client-side journey rather than a replacement for it: together they say the
    material is gone AND unreachable; either alone leaves the other half open.
    """
    handle = dedicated_mail_nest_handle_domain
    nest = handle.nest
    domain = handle.domain

    # Web has no mail-settings enable path yet; the gate lives in the action
    # layer (convention 7) and declares its class rather than skipping silently.
    app.mail_settings.require_scripted_mua_seed_supported()

    # ── A fresh NON-ADMIN handled actor on the dedicated mail nest ───────────
    # Non-admin for the reason `succeedable_app` states: the ceremony re-points
    # the whole account. Its handle carries no domain, and the nest's handle
    # domain IS the mail domain, so `canonical_address_for_actor` resolves the
    # mailbox to `<handle>@fauna.test` — the address a real user would type into
    # their MUA, minted by the enable below with no admin aliasing at all.
    from conftest import _login_app_as, _make_user

    user = _make_user(nest)
    address = f"{user['handle']}@{domain}"
    app.driver.reset()
    _login_app_as(app, request, nest, user)

    # ── Precondition 1: a real mailbox with a known password ─────────────────
    app.mail_settings.navigate()
    _require_mail_settings(app)
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=_ENABLE_S), (
        "enabling mail must mint the 'default' credential — the one the bridge's "
        "AUTH PLAIN path looks up — and the ceremony has nothing to take away "
        "without it; page error: "
        f"{app.mail_settings.page_error_text(timeout=2.0)!r}, "
        f"app error: {app.error_text()!r}"
    )
    credentials_before = app.mail_settings.credential_count()

    # ⚠ The enable above did NOT open the deployment gates, because the actor
    # driving it is deliberately a NON-admin (see the block above). The
    # deployment-wide `mail_enabled` toggle is Admin-class on the nest, and
    # `MailSettingsMachine::enable_mail` calls it best-effort and swallows the
    # rejection — "the non-admin no-op"
    # (`libs/fauna-client-mail-settings/src/machine.rs`). The user's mailbox is
    # provisioned; the subsystem is still off. So the admin opens the gate here,
    # exactly as it happens in production, where the admin owns the deployment
    # toggle and each user enables only their own mailbox
    # (mail-bridge-lifecycle.md § Default-off on first claim).
    #
    # Without this, `mta.Bindable` never opens, the idling bridge correctly never
    # exits, and the `rebind_after_enable()` below times out after 60s against a
    # perfectly healthy bridge — the failure this test showed 3/3 before the fix.
    handle.admin_opens_mail_gate()

    # NOW the deployment gates are open, so each idling bridge has exited 0 for
    # the supervisor to restart it bound (mail-bridge-lifecycle.md § Running →
    # *An idling bridge stays subscribed*; internal/wsrpc/idle_gate_watch.go).
    # The binaries e2e has no s6, so play supervisor here — Precondition 2 below
    # AUTHs against the IMAP listener this brings up.
    handle.rebind_after_enable()

    # ── Precondition 2: THAT password authenticates at the bridge ────────────
    # The non-vacuity anchor. Everything below is a statement about what the
    # ceremony took away, and it can only be read as that if this line passes.
    status_before = _imap_auth(handle, address, _PASSWORD)
    assert status_before == "OK", (
        f"AUTH PLAIN at {address} with the enable password must SUCCEED before "
        "the ceremony — this is the exact credential a pre-succession seed thief "
        "would be holding, and without a working login here the post-ceremony "
        f"refusal below proves nothing at all; got {status_before!r}. "
        f"({handle.bridge_log_hint('mda')})"
    )

    # ── The ceremony ────────────────────────────────────────────────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    _require_recovery_section(app)
    require_stolen_gate(app)
    _run_succession(app)

    # ── Barrier: leg 6 has run ──────────────────────────────────────────────
    # A causal barrier, not a settle-sleep (convention 14): the burn marks every
    # row revoked, so `revoked == total` is the observable that says the leg
    # completed. Asserting the AUTH refusal ahead of it would be asserting
    # against a race — and would go green for the wrong reason on a run where
    # the successor's post-auth task had not started yet.
    app.mail_settings.navigate()
    wait_until(
        lambda: (
            app.mail_settings.credential_count() >= 1
            and app.mail_settings.revoked_credential_count()
            == app.mail_settings.credential_count()
        ),
        _MAIL_BURN_S,
        diagnose=lambda: (
            "leg 6 (the mail burn) must mark every pre-succession credential "
            "revoked before the AUTH assertion below is meaningful, but "
            f"{app.mail_settings.revoked_credential_count()} of "
            f"{app.mail_settings.credential_count()} rows are marked (was "
            f"{credentials_before} before the ceremony); "
            f"error={app.error_text()!r}"
        ),
    )

    # ── The property: the same password no longer authenticates ─────────────
    status_after = _imap_auth(handle, address, _PASSWORD)
    assert status_after != "OK", (
        f"AUTH PLAIN at {address} with the PREDECESSOR's password must be "
        f"REFUSED after the succession, but the bridge answered {status_after!r} "
        "— the identity was replaced, every credential row renders *Compromised "
        "— access revoked*, and the thief can still read the mailbox. "
        "mail-credentials.md § Rotation and recovery → Succession claims two "
        "independent things make this impossible: the ceremony re-points the "
        "address away from the retired actor, and leg 6 deletes that actor's "
        "resting blobs. A success here means NEITHER held. "
        f"({handle.bridge_log_hint('mda')})"
    )
