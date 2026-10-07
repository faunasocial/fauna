"""E2E coverage for the user-facing mail-settings page.

Target state: docs/goal/ui/mail-settings.md (the page the user manages
third-party-MUA access from). Feature behavior: docs/goal/behavior/
mail-credentials.md.

Slice 1 (the linux mail-admin follow-up) lands the static page skeleton + every
ui.yaml ID + navigation; the enable-toggle and the add/rotate buttons are
inert until Slice 2 wires the shared MailSettingsMachine. So this slice
asserts only reachability + presence of the always-visible top-of-page
elements. Enable/add/revoke/rotate flows are added as their slices land.

tier_3: navigates a real client driver against a real fauna-nest
(logged_in_app spins one). The page renders no business logic, but the
reachability path runs the full client UI against the live session.
"""

import pytest

from i18n.strings import S

# The mail-settings page is implemented on linux + web + windows (the linux
# mail-admin follow-up / the web MailSettingsSection / the windows mail-settings
# follow-up's Controls/MailSettingsPanel)
# and on apple. macos shares the FaunaKit MailSettingsView and is fully green (8/8).
# iOS renders the page too — the structural top-of-page tests pass — but the
# enable-dependent tests still fail. ⚠ Refined N+38 (fresh iOS build, mtime-verified;
# + a tree() probe): apple's inline-reveal fix (`fix(macos,ios): inline-reveal mail
# add-credential + rotate-keys forms (drop .sheet)`) DID fix the
# N+36/N+37 registration gap — the inline add-credential form now fully registers on
# iOS in-process (probe: `mail-add-credential-name-input` count=1 + the whole 6-id
# form tree). But it EXPOSED the next iOS gap: SUBMITTING the enable-mode form does
# NOT complete the enable — after clicking `mail-add-credential-submit-button` the
# probe saw, for 40s straight: no credential row (`mail-settings-credential-item`
# count=0), no MUA instructions, the status indicator gone (the form never dismissed),
# and no error. So it is NOT a slow-enable/timeout. macOS runs the IDENTICAL shared
# submit→EnableMail flow 8/8 green, so the iOS submit either doesn't dispatch EnableMail
# or the view doesn't observe the updated snapshot (the throwaway-VM class — check
# whether MailSettingsView observes the snapshot the submit mutates). Handed back. So `ios` stays PER-TEST on the 2 structural tests below only.
# android now renders the page (`MailSettingsScreen.kt`, full ID parity incl.
# `mail-settings-enabled-toggle`) and is Compose-content-tested
# (`MailSettingsContentTest.kt`); its tier_3 run stays host-emulator-gated like
# every other android e2e test — marked per-test below
# on the outcomes it's proven for. NB: windows FlaUI e2e flakes
# (reference_windows_e2e_flake); the windows deterministic surface (MSBuild + dotnet
# test + name/xbind/ui-actual lints) is the gate, this is best-effort.
pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.web, pytest.mark.windows, pytest.mark.macos]


@pytest.mark.ios  # structural (no enable/reveal) — iOS green N+36; see module comment
@pytest.mark.tui  # tui M8 slice 1: page + enable + credentials list (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_page_reachable(logged_in_app):
    """Navigate to the mail-settings page and confirm the enable-toggle shows."""
    logged_in_app.mail_settings.navigate()
    assert logged_in_app.mail_settings.is_page_visible(), (
        "mail-settings-enabled-toggle should be visible after navigating to "
        f"the mail-settings page; error: {logged_in_app.error_text()!r}"
    )


@pytest.mark.ios  # structural (no enable/reveal) — iOS green N+36; see module comment
@pytest.mark.tui  # tui M8 slice 1 (tracked internally)
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_top_elements_present(logged_in_app):
    """The always-visible top-of-page elements are present (Slice 1 skeleton).

    page-heading + enabled-toggle + status-indicator + the page-level
    error-message are rendered regardless of enabled state, per
    mail-settings.md § Layout & flow ("Top of page (always visible)").
    """
    logged_in_app.mail_settings.navigate()
    driver = logged_in_app.driver
    # wait_for scrolls each element into the viewport — the embedded mail page
    # sits below the privacy section, so GTK renders it into the AT-SPI tree
    # lazily as it scrolls on-screen.
    for element_id in (
        "mail-settings-enabled-toggle",
        "mail-settings-status-indicator",
    ):
        try:
            driver.wait_for(element_id, timeout=10.0)
        except TimeoutError:
            raise AssertionError(
                f"{element_id} should be visible on the mail-settings page; "
                f"error: {logged_in_app.error_text()!r}"
            )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 2: the MUA-instructions block (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_enable_populates_mua_instructions(logged_in_app):
    """Flow F1 (the linux mail-admin follow-up, Slice 2): enabling mail surfaces the
    MUA connection details.

    Per mail-settings.md § Layout, the MUA-instructions block is visible only
    when mail is enabled. A successful enable provisions the snapshot +
    wrapped-MSEK + submission-token blobs, commits the `fauna.state.mail` plane, and the
    page renders `mail.<domain>` IMAP/SMTP hosts. GREEN since the MLS read-side
    snapshot producer landed (the shared-Rust mail-admin follow-up): this exercises the full
    glue→machine→nest enable path end-to-end. (Was xfail + a sibling stub-error
    test while the producer was a stub; both retired when it landed.)
    """
    app = logged_in_app
    app.mail_settings.navigate()
    # Idempotent: the tier_3 nest + actor are session-scoped, so an earlier mail
    # test (test_mail_client_receive/_send) may already have enabled mail for
    # this actor. enable_mail() clicks the enabled-*toggle* unconditionally, so
    # against an already-enabled actor it would toggle mail OFF and the
    # add-credential form would never appear (a full-suite-only red). The
    # observable contract here — "mail enabled => MUA instructions + mail.<domain>
    # host" — holds either way; the other five tests in this file already coexist
    # with the shared actor via ensure_mail_enabled.
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.mua_instructions_visible(), (
        "MUA-instructions block should be visible after enabling mail; "
        f"error: {app.error_text()!r}"
    )
    host = app.mail_settings.mua_imap_host()
    # The MUA host is "mail.<domain>" for a registrable domain, but the BARE
    # locator (no "mail." prefix) for a local-target nest — bare IP / localhost /
    # .local — where "mail.<IP>" is nonsense. That carve-out is ratified
    # (caldav-server.md § Network exposure / Client endpoint display, "Change B′";
    # shared `MuaInstructions::for_node_url` via `fauna_core::resolve::is_local_host`).
    # The tier_3 nest is reached at 127.0.0.1, so the host here is the bare
    # locator. Assert a populated, placeholder-free connectable host of either
    # shape (the pre-B′ "mail."-only assertion was wrong for this localhost nest).
    assert host and "{" not in host, (
        "IMAP host should be a populated, placeholder-free connectable locator "
        "after enable (bare locator for a local-target nest, else mail.<domain>); "
        f"got {host!r}; error: {app.error_text()!r}"
    )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 1 (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_enable_renders_credential_row(logged_in_app):
    """Slice 3: an enabled actor has its credential(s) rendered as indexed
    `mail-settings-credential-item-*` rows (mail-settings.md § Credentials list).

    Enabling goes Toggle → add-credential form → submit; the committed
    `fauna.state.mail` credentials re-render as rows. `ensure_mail_enabled` is
    idempotent because the tier_3 nest is session-scoped (a prior test may have
    already enabled mail for this actor).
    """
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    assert app.mail_settings.credential_count() >= 1, (
        "at least one credential row should render once mail is enabled; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The row must show the EXACT MUA username (mail-credentials.md
    # § Implementation status — the username-display gap that let a user
    # configure the wrong username). The render substitutes the logged-in
    # handle into the shared snapshot's resolved-except-handle template, so the
    # rendered value must be a concrete `local@domain` with NO leftover `{...}`
    # placeholder. (The bare-vs-`+suffix` rule per credential_id is covered by
    # the shared-Rust unit tests in fauna-client-mail-settings.)
    username = app.mail_settings.credential_username(0)
    assert "@" in username and "{" not in username, (
        "the credential row must show the concrete MUA username with the handle "
        f"substituted (no template placeholder); got {username!r}"
    )

    # The row's kind badge (mail-settings-credential-item-type) renders the
    # human auth-kind label from shared-Rust credential_kind_badge (i18n
    # settings.mail.kind_{password,bearer}; mail-settings.md § Credentials list
    # "kind (Password / Bearer token)"). The default MUA credential is
    # password-auth, so the badge must be a non-empty, placeholder-free label.
    # Closes the mail-settings-credential-item-type coverage gap — no client
    # asserted this shared-Rust render leg before.
    kind = app.mail_settings.credential_type(0)
    assert kind and "{" not in kind, (
        "the credential row must render its kind badge "
        "(mail-settings-credential-item-type) with the shared-Rust auth-kind "
        f"label; got {kind!r} — error: "
        f"{app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 3: the credential row's reveal/copy children (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_reveal_credential_secret(logged_in_app):
    """The credential row reveals its secret on demand (mail-credentials.md
    § Implementation status — the secret re-reveal that lets a user recover the
    exact password/token to reconfigure a MUA without revoke + re-add).

    The secret is NOT in the snapshot — clicking reveal fetches it from the
    client's own `fauna.state.mail` plane via MailSettingsMachine::reveal_credential_secret —
    so a populated secret label after the click proves that on-demand path.
    """
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    secret = app.mail_settings.reveal_credential_secret(0)
    assert secret, (
        "clicking reveal must show the credential's secret (fetched on demand "
        "from the mail plane); got empty — error: "
        f"{app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 3: the row's two-click revoke (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_add_then_revoke_credential(logged_in_app):
    """Slice 3: add-credential appends a row and soft-revoke removes one
    (mail-settings.md § User actions → "Add a credential" / "Revoke a
    credential" → AddCredential / RevokeCredential).

    Asserts *relative* counts (the session-scoped nest means the absolute
    starting count isn't fixed across the run): add one → count + 1; revoke one
    → back to the starting count. Net-zero so the shared nest is left as found.
    """
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    before = app.mail_settings.credential_count()
    app.mail_settings.add_credential("Phone")
    assert app.mail_settings.wait_for_credential_count(before + 1), (
        f"add-credential should append a row (had {before}); "
        f"got {app.mail_settings.credential_count()}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    app.mail_settings.revoke_credential(0)
    assert app.mail_settings.wait_for_credential_count(before), (
        f"revoke should remove one row (back to {before}); "
        f"got {app.mail_settings.credential_count()}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 1 painted keys-info; asserted from slice 3 (tracked internally)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_keys_info_present(logged_in_app):
    """Bundle A: the (i) keys explainer (mail-settings-keys-info) renders beside
    the rotate-keys button once mail is enabled (mail-settings.md § keys-info;
    ui.yaml `mail-settings` page). Static copy = S.mail_settings.keys_info, so we
    assert presence + non-empty text rather than the exact wording.
    """
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    driver = app.driver
    try:
        driver.wait_for("mail-settings-keys-info", timeout=10.0)
    except TimeoutError:
        raise AssertionError(
            "mail-settings-keys-info should render once mail is enabled; "
            f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
        )
    assert driver.get_text("mail-settings-keys-info").strip(), (
        "the keys-info explainer should carry non-empty copy"
    )


@pytest.mark.ios
@pytest.mark.tui  # tui M8 slice 5: the rotate-keys inline confirm form (StartRotation)
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_rotate_keys_completes(logged_in_app):
    """Slice 4: rotating mail keys re-wraps every surviving credential under a
    fresh MSEK and completes without error (mail-settings.md § User actions →
    "Rotate mail keys"; mail-credentials.md § Hard revoke).

    A no-exclusion rotation preserves the credential list (every credential is
    re-wrapped), so the observable contract is: the rotate-keys confirm form
    opens, the multi-RPC StartRotation round-trip surfaces no error, and the
    credential count is unchanged.
    """
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    before = app.mail_settings.credential_count()
    assert before >= 1

    app.mail_settings.rotate_keys()
    # page_error_text waits up to the timeout for an error to surface, returning
    # "" if none — which doubles as the settle window for the rotation loop.
    err = app.mail_settings.page_error_text(timeout=10.0)
    assert not err, f"rotate-keys should complete without error; got {err!r}"
    assert app.mail_settings.credential_count() == before, (
        f"rotation should preserve all {before} surviving credential(s); "
        f"got {app.mail_settings.credential_count()}"
    )


@pytest.mark.tui
@pytest.mark.feature("turn-on-mail")
def test_calendar_written_before_a_key_rotation_still_opens(app, request, nest_instance):
    """A rotation must not hide the user's own calendar (mail-credentials.md
    § Rotation and recovery → *DAV bodies across a rotation*, ruling 1).

    Every calendar's metadata and every event body rests sealed to ONE MSEK
    generation and is never re-sealed, so after "Rotate mail keys" the app opens
    them through the whole ring (current + prior generations). Before the ring,
    every app opened with the current generation alone and the whole calendar
    failed ``unseal DAV body: HPKE open failed`` — the 2026-10-06 linux witness
    this test was missing. The calendar and event are made through the Events
    page BEFORE the rotation; after it (and, on a native app, a cold relaunch
    that re-reads the custody from the account plane) both must still render.

    A dedicated actor, so the rotation cannot shift the shared ``test_user``'s
    keys under any later test.
    """
    from datetime import datetime, timedelta
    import uuid

    from conftest import _login_app_as, _make_user

    user = _make_user(nest_instance)
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    tag = uuid.uuid4().hex[:8]
    cal_name = f"prerot-cal-{tag}"
    summary = f"prerot-event-{tag}"
    start = (datetime.now() + timedelta(days=3)).replace(
        hour=10, minute=0, second=0, microsecond=0
    )
    ev = app.events
    ev.navigate()
    ev.create_calendar(cal_name)
    ev.select_calendar(cal_name)
    ev.create_event(
        summary=summary,
        start=start.strftime("%Y-%m-%dT%H:%M"),
        end=(start + timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M"),
    )
    assert summary in ev.event_summaries(), (
        f"sanity: the event must render before the rotation; error: {app.error_text()!r}"
    )

    app.mail_settings.navigate()
    app.mail_settings.rotate_keys()
    err = app.mail_settings.page_error_text(timeout=10.0)
    assert not err, f"rotate-keys should complete without error; got {err!r}"

    if not app.driver.is_web():
        # A cold relaunch: nothing cached in the process survives, so the ring
        # can only come from the custody the rotation wrote.
        assert app.driver.recover(), "client process relaunch (driver.recover()) failed"
        _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    ev.navigate()
    # Stripped: a row's label can carry trailing padding (tui renders one).
    names = [n.strip() for n in ev.calendar_names()]
    assert cal_name in names, (
        f"the calendar created before the rotation must still be listed by name "
        f"(its metadata opens through a prior generation); listed={names!r}, "
        f"error: {app.error_text()!r}"
    )
    ev.select_calendar(cal_name)
    summaries = ev.event_summaries()
    assert summary in summaries, (
        f"the event created before the rotation must still render; "
        f"events={summaries!r}, error: {app.error_text()!r}"
    )


# ---------------------------------------------------------------------------
# M8 slice 4 (tui): the PLAIN add-credential family + the OAUTHBEARER one-time
# token. These add credentials to an ALREADY-enabled actor (the session-scoped
# test_user has mail on from the enable tests above), so they drive the
# add-credential *button* path — NOT the enable toggle, which on an enabled actor
# opens the destructive disable dialog. Each proves the secret the user SEES
# equals the secret the client STORES — the two apple 2026-07-13 bugs
# (mail-settings.md § Implementation status): auto-generate hid the password so
# submit re-minted a *different* secret; the OAUTHBEARER form closed on success,
# destroying the one-time token at the instant it was minted. Proven cheaply (no
# MUA round-trip) by revealing the new row's stored secret and comparing it to
# the value the form showed. The new credential is the last row (credentials
# append; no revoke runs between add and reveal).
# ---------------------------------------------------------------------------


@pytest.mark.tui  # tui M8 slice 4: the PLAIN manual add-credential path
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_add_plain_credential_manual_stores_typed_secret(logged_in_app):
    """PLAIN manual: the kind selector reveals the password fields, turning
    auto-generate OFF makes the field editable, and the password the user TYPES is
    the one stored — revealing the new row returns exactly it (mail-settings.md
    § Add credential; the manual side of the apple secret-integrity fix)."""
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    chosen = "s3cretMuaPass99XYZ"
    before = app.mail_settings.credential_count()
    app.mail_settings.add_credential_plain("Manualcred", chosen)
    assert app.mail_settings.wait_for_credential_count(before + 1), (
        f"adding a PLAIN credential should append a row (had {before}); "
        f"got {app.mail_settings.credential_count()}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    idx = app.mail_settings.credential_count() - 1
    assert app.mail_settings.credential_type(idx) == S.settings.mail.kind_password, (
        "a PLAIN credential's kind badge should read 'Password'; "
        f"got {app.mail_settings.credential_type(idx)!r}"
    )
    revealed = app.mail_settings.reveal_credential_secret(idx)
    assert revealed == chosen, (
        "the stored secret must equal the password the user typed (submit must "
        f"not re-mint or drop it); typed {chosen!r}, revealed {revealed!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.tui  # tui M8 slice 4: the PLAIN auto-generate add-credential path
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_add_plain_credential_autogen_stores_shown_secret(logged_in_app):
    """PLAIN auto-generate (the default): the client mints a ~143-bit password,
    shows it once in the read-only field, and stores EXACTLY that value —
    revealing the new row returns the same password the field showed. This is the
    apple bug the fix closed: the password field never mounted while auto-generate
    was on, so submit re-minted a *different* secret (mail-settings.md
    § Implementation status, 2026-07-13)."""
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    before = app.mail_settings.credential_count()
    shown = app.mail_settings.add_credential_autogen("Autocred")
    assert shown and len(shown) >= 16 and shown.isalnum(), (
        "auto-generate should show a strong alphanumeric password in the "
        f"read-only field; read back {shown!r}"
    )
    assert app.mail_settings.wait_for_credential_count(before + 1), (
        f"adding a PLAIN autogen credential should append a row (had {before}); "
        f"got {app.mail_settings.credential_count()}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    idx = app.mail_settings.credential_count() - 1
    revealed = app.mail_settings.reveal_credential_secret(idx)
    assert revealed == shown, (
        "the stored secret must equal the auto-generated password the field "
        f"showed (submit must reuse it, not re-mint); shown {shown!r}, revealed "
        f"{revealed!r}; error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


@pytest.mark.tui  # tui M8 slice 4: OAUTHBEARER one-time token stays shown on success
@pytest.mark.android
@pytest.mark.feature("turn-on-mail")
def test_mail_settings_add_oauthbearer_credential_shows_token_once(logged_in_app):
    """OAUTHBEARER: submit reveals the one-time token and KEEPS the form open (the
    apple bug: closing on success destroyed the token at the instant it was
    minted). The revealed token equals the credential's stored secret
    (mail-settings.md § Errors + § Implementation status)."""
    app = logged_in_app
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    before = app.mail_settings.credential_count()
    token = app.mail_settings.add_credential_oauthbearer("Bearercred")
    assert token, (
        "the OAUTHBEARER form must reveal the minted token and stay open; got "
        f"empty; error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    app.mail_settings.close_add_credential_form()
    assert app.mail_settings.wait_for_credential_count(before + 1), (
        f"adding an OAUTHBEARER credential should append a row (had {before}); "
        f"got {app.mail_settings.credential_count()}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    idx = app.mail_settings.credential_count() - 1
    assert app.mail_settings.credential_type(idx) == S.settings.mail.kind_bearer, (
        "an OAUTHBEARER credential's kind badge should read 'Bearer token'; "
        f"got {app.mail_settings.credential_type(idx)!r}"
    )
    revealed = app.mail_settings.reveal_credential_secret(idx)
    assert revealed == token, (
        "the stored secret must equal the one-time token the form showed; shown "
        f"{token!r}, revealed {revealed!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
