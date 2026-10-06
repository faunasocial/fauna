from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailSettingsActions:
    """Drive the user-facing mail-settings page (docs/goal/ui/mail-settings.md).

    The page is reached via the settings/status view on every app. Slice 1
    (the linux mail-admin follow-up) lands a static skeleton with every ui.yaml ID;
    the toggle/buttons are inert until Slice 2 wires the shared
    MailSettingsMachine. Action methods grow per slice (enable/add/revoke/
    rotate) — kept uniform across all 7 apps.
    """

    #: The ceiling for "enabling mail has taken effect" — the default credential
    #: shows in the list, and the status reads enabled. Enabling provisions the
    #: credential, the MSEK and the MLS snapshot before either settles, so on a
    #: loaded box it outlasts a page render: a 15 s ceiling was the intermittent
    #: red in three windows runs of the DAV witnesses (2026-09-25 — each passed
    #: on re-run, and the error surface read '' every time). A ceiling only a
    #: wedge can reach, never a timing assertion: a green run still returns the
    #: moment the state is true.
    ENABLE_SETTLE_S = 60.0

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def require_scripted_mua_seed_supported(self) -> None:
        """Skip unless this app can enable mail through its own UI to mint
        the recipient/seal key material a scripted CardDAV/CalDAV MUA test
        needs (e2e convention 7 — the platform check lives in the action
        layer, not the test body).

        linux/windows/macos/ios/tui enable mail via `enable_mail_plain`; web
        joined too — its own `enable_mail_plain` path
        is proven directly by `test_mail_credentials.py`'s web marker and by
        the two tests this very gate unblocked
        (`test_mail_multi_credential_auth.py`, `test_mail_bare_username_
        auth.py`, both green `--app web`). android joined last: its form paints
        every element `enable_mail_plain` drives, its kind selector defaults to
        OAUTHBEARER like the others', and the dedicated nest reaches the device
        through `_relaunch_trusting_nest`'s `adb reverse` — so no app is excluded today, and the gate stays only as the
        one place a new app would declare the gap."""
        if not (
            self.driver.is_linux()
            or self.driver.is_windows()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_tui()
            or self.driver.is_web()
            or self.driver.is_android()
        ):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the enable_mail_plain scripted-MUA-seed path",
                detail="every app with a mail-settings page drives it",
                tracked="mail-settings.md",
            )

    def navigate(self) -> None:
        """Navigate to the mail-settings page by the two-element nav patch
        ``{"view":"settings"},{"view":"settings","id":"mail-settings"}`` —
        the Settings shell's cross-app sub-page nav (``settings.md`` §
        Navigation model), the same shape the admin actions use.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-settings"}]},
        })

    def open_by_click(self, timeout: float = 15.0) -> None:
        """Reach mail-settings the way a human does: ``settings-tab``, then its
        rail row ``settings-nav-row[mail-settings]`` (ui.yaml
        ``navigation.sub_page_nav_rows``), then wait for the enable toggle.

        The tab is clicked only when the row is not already on screen. This is
        the path a test bound by the gesture-only rule uses (the live tier_4
        tests, convention 8); ``navigate`` stays the nav-patch path.
        """
        row = "settings-nav-row[mail-settings]"
        if not self.driver.is_visible(row):
            self.driver.click("settings-tab")
            self.driver.wait_for(row, timeout=timeout)
        self.driver.click(row)
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=timeout)

    def is_page_visible(self) -> bool:
        """True when the mail-settings page is reachable (enable-toggle present).

        Uses wait_for so the driver scrolls the toggle into the viewport —
        the mail page sits below the privacy section in the embedded settings
        view, so GTK may not have rendered it into the AT-SPI tree until it's
        scrolled on-screen.
        """
        try:
            self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
            return True
        except TimeoutError:
            return False

    def mua_imap_host(self) -> str:
        return self.driver.get_text("mail-settings-mua-imap-host")

    def mua_imap_port(self) -> str:
        return self.driver.get_text("mail-settings-mua-imap-port")

    def mua_smtp_port(self) -> str:
        return self.driver.get_text("mail-settings-mua-smtp-port")

    def mua_webdav_url(self) -> str:
        """The WebDAV collection-root URL row — one full URL, not a host+port
        pair (no SRV autodiscovery exists for WebDAV). Rendered only once the
        actor serves >=1 folder over WebDAV."""
        return self.driver.get_text("mail-settings-mua-webdav-url")

    def mua_webdav_url_visible(self, timeout: float = 6.0) -> bool:
        """True once the (serve-gated) WebDAV URL row is on screen. Returns False
        on timeout rather than raising, so the caller decides how to assert."""
        try:
            self.driver.wait_for("mail-settings-mua-webdav-url", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def enable_mail(self, display_name: str = "Default") -> None:
        """Enable mail by minting the first credential (Flow F1).

        Per mail-settings.md § User actions, "Enable mail (first credential)"
        is Toggle → add-credential dialog → submit. On linux the dialog is an
        inline reveal embedded in the same status-view page (the state protocol
        can't open the separate PreferencesWindow modal — see
        the linux mail-admin follow-up, Slice 2b), so toggling on reveals the
        mail-add-credential form; we fill the name and submit, which dispatches
        MailSettingsAction::EnableMail with the default (OAUTHBEARER) credential
        kind through the shared machine.
        """
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
        self.driver.click("mail-settings-enabled-toggle")
        self._submit_add_credential_form(display_name)

    def enable_mail_plain(self, password: str, display_name: str = "Default") -> None:
        """Enable mail with a PLAIN (password) credential — the credential a
        Thunderbird-style MUA authenticates with over IMAP/SMTP AUTH PLAIN.

        Toggle on → the add-credential form reveals. The kind selector
        (`mail-add-credential-type-selector`) is a single toggle button: inactive
        is the OAUTHBEARER default, active is PLAIN, so one click from the default
        selects PLAIN and reveals the password field (the GtkDropDown it used to
        be can't be driven coordinate-free in a headless Linux session). Fill the
        name + password and
        submit, dispatching MailSettingsAction::EnableMail with
        CredentialKind::Plain. The credential_id derives from `display_name`, so
        "Default" → "default" — the credential_id the bridge's AUTH PLAIN path
        looks up (`plainCredentialID`).
        """
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
        self.driver.click("mail-settings-enabled-toggle")
        self.driver.wait_for("mail-add-credential-type-selector", timeout=10.0)
        self.driver.click("mail-add-credential-type-selector")  # OAUTHBEARER → PLAIN
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        # The password entry lives in the PLAIN-only box, revealed by the toggle
        # above; wait for it to render into the tree before typing.
        self.driver.wait_for("mail-add-credential-password-input", timeout=10.0)
        # Auto-generate defaults ON (mail-credentials.md § Auto-generated bridge
        # password): the password input then holds a client-minted secret and is
        # read-only. To use the test's chosen password — the one the MUA will
        # authenticate with — turn auto-generate OFF so manual entry is enabled.
        # Tolerant of clients that don't render the toggle yet (linux uses a local
        # strength label): only click it when present. The toggle renders in the
        # same PLAIN-only block as the password input we just waited for.
        if self.driver.is_visible("mail-add-credential-autogenerate-toggle"):
            self.driver.click("mail-add-credential-autogenerate-toggle")
        self.driver.clear_and_type("mail-add-credential-password-input", password)
        self.driver.click("mail-add-credential-submit-button")

    def provision_default_credential_autogen(self, display_name: str = "Default") -> str:
        """Provision the actor's PLAIN credential with auto-generate ON and return
        the *generated* password read back from the UI — the realistic flow
        (mail-credentials.md § Auto-generated bridge password: default ON, the
        client mints a ~143-bit secret shown-once to copy into the MUA). Unlike
        `enable_mail_plain`, this keeps auto-generate ON and reads the secret
        rather than typing a chosen one. The credential a CalDAV/IMAP MUA
        authenticates with (AUTH PLAIN looks up credential_id `default`, derived
        from display_name "Default").

        Uses the same proven enable path as `enable_mail_plain` — the enable
        toggle's first-credential `EnableMail` dispatch, which reliably brings the
        mail subsystem up (the add-credential-button `AddCredential` path does
        not boot the bridge the same way) — only it keeps auto-generate ON and
        reads the secret instead of typing one. credential_id derives from
        display_name "Default" → `default`.
        """
        import time

        self.navigate()
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
        self.driver.click("mail-settings-enabled-toggle")
        self.driver.wait_for("mail-add-credential-type-selector", timeout=15.0)
        self.driver.click("mail-add-credential-type-selector")  # OAUTHBEARER → PLAIN
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        self.driver.wait_for("mail-add-credential-password-input", timeout=10.0)
        # Auto-generate defaults ON → the password field holds the client-minted
        # secret (read-only). Read it back: get_text returns a gtk::Entry's
        # content even while masked (linux automation find.rs::text_of).
        password = ""
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            password = self.driver.get_text("mail-add-credential-password-input").strip()
            if password:
                break
            time.sleep(0.3)
        self.driver.click("mail-add-credential-submit-button")
        return password

    def enable_mail_oauthbearer(self, display_name: str = "Default") -> str:
        """Enable mail with the default OAUTHBEARER credential and return the
        one-time bearer token the client mints — the token a SASL OAUTHBEARER
        MUA authenticates with over IMAP/SMTP.

        Toggle on → the add-credential form reveals with OAUTHBEARER selected by
        default (the `mail-add-credential-type-selector` toggle stays inactive, so
        — unlike `enable_mail_plain` — no kind-toggle click and no password field).
        Fill the name + submit, dispatching MailSettingsAction::EnableMail with
        CredentialKind::OAuthBearer. On success the client reveals the freshly
        minted token in `mail-add-credential-token-display` (shown once); we read
        it back and return it. The credential_id derives from `display_name`, so
        "Default" → "default" — the credential_id the bridge's OAUTHBEARER AUTH
        path looks up (`plainCredentialID`, shared with PLAIN today).
        """
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
        self.driver.click("mail-settings-enabled-toggle")
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        self.driver.click("mail-add-credential-submit-button")
        # On an OAUTHBEARER success the form swaps the input box for the token
        # reveal; wait for the token label to render, then read it back.
        self.driver.wait_for("mail-add-credential-token-display", timeout=15.0)
        return self._wait_for_nonempty_token(timeout=15.0)

    def _wait_for_nonempty_token(self, timeout: float = 15.0) -> str:
        """Poll `mail-add-credential-token-display` until it holds the minted
        token (the reveal sets the label only after the async EnableMail
        dispatch returns success)."""
        import time

        deadline = time.monotonic() + timeout
        token = ""
        while time.monotonic() < deadline:
            token = self.driver.get_text("mail-add-credential-token-display").strip()
            if token:
                return token
            time.sleep(0.3)
        return token

    def add_credential(self, display_name: str) -> None:
        """Add a credential to an already-enabled actor (mail-settings.md
        § User actions → "Add a credential").

        Opens the add-credential inline form via the add-credential button,
        fills the name, and submits — dispatching MailSettingsAction::AddCredential
        (default OAUTHBEARER kind). The type-selector is a GtkDropDown that can't
        be driven coordinate-free in a headless Linux session, so the e2e
        exercises the default OAUTHBEARER path only; the PLAIN-only fields are
        present but not driven.
        """
        self.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
        self.driver.click("mail-settings-add-credential-button")
        self._submit_add_credential_form(display_name)

    def start_add_credential(self, display_name: str) -> None:
        """Submit a new (OAUTHBEARER) credential WITHOUT waiting for the mint to
        land — for a caller standing inside the dispatch (``helpers.rpc_hold``
        holding one of its writes) and polling what the page shows meanwhile.

        The ``feed.start_submit`` shape: **tui** presses Enter on the submit
        button, the keyboard's activation, which runs the op off the render loop
        exactly as a user's keypress does — its agent ``click`` awaits the whole
        dispatch before replying, so a held write would time the click out.
        **Every other app** clicks; an app whose click likewise waits for the
        whole dispatch then fails at the caller's arrival wait, naming that
        app's gap instead of passing without it.
        """
        self.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
        self.driver.click("mail-settings-add-credential-button")
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        if self.driver.is_tui():
            self.driver.press_key("mail-add-credential-submit-button", "Enter")
        else:
            self.driver.click("mail-add-credential-submit-button")

    def add_credential_plain(self, display_name: str, password: str) -> None:
        """Add a SECOND PLAIN credential to an already-enabled actor with a CHOSEN
        password — the "I added another mail password in the client" gesture.

        The add-credential twin of `enable_mail_plain` (which uses the enable
        toggle for the FIRST credential): enters through the add-credential
        *button*, flips the kind selector (a single CheckButton toggle —
        apps/fauna-linux/src/settings/mail.rs § type_selector — so one click
        selects PLAIN and reveals the password field), turns auto-generate OFF, and
        types the chosen password (a deterministic password the MUA authenticates
        with — avoiding the masked auto-generate read-back's timing flakiness).
        Dispatches MailSettingsAction::AddCredential with CredentialKind::Plain.
        The credential_id derives from `display_name` (kebab-case via
        `derive_credential_id`), so e.g. "Phone" → "phone" — the MUA authenticates
        as `<handle>+phone@<domain>` per mail-credentials.md § MUA-username (RFC
        5233 sub-addressing).
        """
        self.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
        self.driver.click("mail-settings-add-credential-button")
        self.driver.wait_for("mail-add-credential-type-selector", timeout=10.0)
        self.driver.click("mail-add-credential-type-selector")  # OAUTHBEARER → PLAIN
        self.driver.wait_for("mail-add-credential-password-input", timeout=10.0)
        # Auto-generate defaults ON (field read-only, holds a minted secret). Turn
        # it OFF so the field becomes editable and we can type the chosen password.
        if self.driver.is_visible("mail-add-credential-autogenerate-toggle"):
            self.driver.click("mail-add-credential-autogenerate-toggle")
        self.driver.clear_and_type("mail-add-credential-password-input", password)
        # Set the name LAST and VERIFY it stuck. The Add form opens with an EMPTY
        # name (unlike the enable form's "Default" pre-fill), so a dropped keystroke
        # would silently fall back to display_name "Default" → credential_id
        # "default-2" (derive_credential_id collision suffix), not the intended id —
        # and the MUA's sub-addressed username would then point at a credential that
        # doesn't exist. The name entry is not masked, so the read-back is reliable.
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        for _ in range(5):
            self.driver.clear_and_type("mail-add-credential-name-input", display_name)
            if self.driver.get_text("mail-add-credential-name-input").strip() == display_name:
                break
        self.driver.click("mail-add-credential-submit-button")

    def add_credential_autogen(self, display_name: str) -> str:
        """Add a PLAIN credential with auto-generate ON (the default) to an
        already-enabled actor, returning the *generated* password read back from
        the read-only password field.

        The add-path twin of `provision_default_credential_autogen` (which uses
        the enable toggle for the FIRST credential): enters through the
        add-credential *button*, flips the kind selector to PLAIN, keeps
        auto-generate ON, and reads the client-minted ~143-bit secret shown-once
        in `mail-add-credential-password-input`. Dispatches
        MailSettingsAction::AddCredential with CredentialKind::Plain. The
        read-back value is the SAME secret submit stores (the client mints it
        once and shows it — never re-mints at submit, mail-settings.md
        § Implementation status, the apple 2026-07-13 fix).
        """
        import time

        self.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
        self.driver.click("mail-settings-add-credential-button")
        self.driver.wait_for("mail-add-credential-type-selector", timeout=10.0)
        self.driver.click("mail-add-credential-type-selector")  # OAUTHBEARER → PLAIN
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        self.driver.wait_for("mail-add-credential-password-input", timeout=10.0)
        # Auto-generate defaults ON → the field holds the client-minted secret
        # (read-only). Read it back; get_text returns the field's content.
        password = ""
        deadline = time.monotonic() + 10.0
        while time.monotonic() < deadline:
            password = self.driver.get_text("mail-add-credential-password-input").strip()
            if password:
                break
            time.sleep(0.3)
        self.driver.click("mail-add-credential-submit-button")
        return password

    def add_credential_oauthbearer(self, display_name: str) -> str:
        """Add an OAUTHBEARER credential to an already-enabled actor and return
        the one-time bearer token the client mints.

        The add-path twin of `enable_mail_oauthbearer`: enters through the
        add-credential *button*, leaves the kind selector inactive (OAUTHBEARER
        is the default — no password field), fills the name, submits, and reads
        the token the form reveals in `mail-add-credential-token-display`. The
        form stays open on OAUTHBEARER success (the token is shown once — closing
        would destroy it, mail-settings.md § Errors + § Implementation status).
        """
        self.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
        self.driver.click("mail-settings-add-credential-button")
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        self.driver.click("mail-add-credential-submit-button")
        # On an OAUTHBEARER success the form reveals the token; wait for it.
        self.driver.wait_for("mail-add-credential-token-display", timeout=15.0)
        return self._wait_for_nonempty_token(timeout=15.0)

    def close_add_credential_form(self) -> None:
        """Close the add-credential form (the cancel/Done button). After an
        OAUTHBEARER add the form stays open showing the one-time token; the user
        closes it once they've copied the token."""
        self.driver.wait_for("mail-add-credential-cancel-button", timeout=10.0)
        self.driver.click("mail-add-credential-cancel-button")

    def _submit_add_credential_form(self, display_name: str) -> None:
        """Fill the mail-add-credential inline form's name and submit it."""
        self.driver.wait_for("mail-add-credential-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-add-credential-name-input", display_name)
        self.driver.click("mail-add-credential-submit-button")

    # ── The app-password rows ─────────────────────────────────────────────
    # On an app whose Connected apps page is built, the rows are rows of that
    # page's roster and no longer render here (docs/goal/ui/mail-settings.md
    # § Credentials list) — the move is a lift, never a duplication. Every
    # helper below therefore reads the rows where the app under test shows
    # them: this page, or one visit to Connected apps and back. Callers stay
    # written against "the user's app passwords" and never branch on the app.
    # A row is addressed by `index` among the mail app passwords only — the
    # roster is mixed-class, and the Connected apps actions pick the mail rows
    # out by the leaves only they carry.

    def _roster(self):
        """The Connected apps actions when this app lists the app passwords
        there, else None."""
        from actions.connected_apps import ConnectedAppsActions, mail_rows_on_connected_apps

        if not mail_rows_on_connected_apps(self.driver):
            return None
        return ConnectedAppsActions(self.driver)

    @contextlib.contextmanager
    def _on_roster(self, *, must_load: bool = True):
        """One visit to Connected apps, then back to this page — so a caller
        written against the mail page carries on where it was. Coming back is
        a fresh visit to this page: an open add-password form is closed and a
        shown-once token is gone, exactly as for a user who left and returned.

        `must_load` fails the visit when its read never returned: an instant
        read off an unread roster would answer 0 for "how many passwords"."""
        roster = self._roster()
        loaded = roster.visit()
        try:
            assert loaded or not must_load, (
                "the Connected apps page never finished its read; "
                f"error: {roster.current_error_text()!r}"
            )
            yield roster
        finally:
            self.navigate()

    def credential_count(self) -> int:
        """How many app-password rows the user's list currently holds."""
        if self._roster() is None:
            return self.driver.count("mail-settings-credential-item-name")
        with self._on_roster() as roster:
            return roster.mail_row_count()

    def credential_name(self, index: int = 0) -> str:
        """The display name on row `index`."""
        if self._roster() is None:
            return self.driver.get_text("mail-settings-credential-item-name", index).strip()
        with self._on_roster() as roster:
            return roster.mail_name(index)

    def credential_created_at(self, index: int = 0) -> str:
        """When row `index` says its password was made (`YYYY-MM-DD HH:MM`)."""
        if self._roster() is None:
            return self.driver.get_text(
                "mail-settings-credential-item-created-at", index
            ).strip()
        with self._on_roster() as roster:
            return roster.mail_created(index)

    def credential_username(self, index: int = 0) -> str:
        """The exact MUA username rendered on row `index`:
        `<handle>+<credential_id>@<domain>`, or bare `<handle>@<domain>` for
        the `default` credential."""
        if self._roster() is None:
            return self.driver.get_text(
                "mail-settings-credential-item-username", index
            ).strip()
        with self._on_roster() as roster:
            return roster.mail_login(index)

    def copy_credential_username(self, index: int = 0) -> str:
        """Click row `index`'s copy-login button and return what it copied (a
        real clipboard read where the driver can do one, else the button's
        `copied` attr)."""
        import types

        from helpers.copy_button import read_clipboard_or_copied

        app = types.SimpleNamespace(driver=self.driver)
        if self._roster() is None:
            button = "mail-settings-credential-item-copy-username"
            self.driver.click(button, index)
            return read_clipboard_or_copied(app, button, index)
        with self._on_roster():
            button = "connected-apps-item-copy-username"
            self.driver.click(button, index)
            return read_clipboard_or_copied(app, button, index)

    def credential_type(self, index: int = 0) -> str:
        """The kind badge rendered on row `index`: the human auth-kind label
        from shared-Rust `credential_kind_badge` — "Password" / "Bearer token"
        (i18n `settings.mail.kind_{password,bearer}`)."""
        if self._roster() is None:
            return self.driver.get_text(
                "mail-settings-credential-item-type", index
            ).strip()
        with self._on_roster() as roster:
            return roster.mail_kind(index)

    def revoked_credential_count(self) -> int:
        """How many credential rows render the succession burn's *Compromised —
        access revoked* state (`mail-settings-credential-item-revoked`).

        A **count**, not a per-row predicate, deliberately: the burn is total by
        construction (`mail-credentials.md` § Rotation and recovery →
        *Succession* — "exclusion is total and not user-selectable"), so the
        assertion worth making is `revoked_credential_count() ==
        credential_count()`. A per-index reader would invite callers to assume
        the revoked markers align positionally with the rows, which holds only
        while every row is revoked — i.e. exactly when the count already answers
        the question.

        ⚠ The rows themselves keep rendering when burned — they are the user's
        list of which mail apps to set up again — so `credential_count()` says
        nothing about whether any of those passwords still work. Only this does.
        """
        if self._roster() is None:
            return self.driver.count("mail-settings-credential-item-revoked")
        with self._on_roster() as roster:
            return roster.mail_revoked_count()

    def reveal_credential_secret(self, index: int = 0, timeout: float = 8.0) -> str:
        """Click the reveal toggle on credential row `index` and return the
        secret once `mail-settings-credential-item-secret` populates. The reveal
        is an async mail-state read (MailSettingsMachine::reveal_credential_secret),
        so poll until the hidden secret label shows a value.

        Every app keeps the secret element on every row, EMPTY until the reveal
        lands (web, tui, windows, and linux since 2026-09-15), so `index` names
        the same row for the reveal button and the secret. linux used to build
        the label `.visible(false)`, which its automation walk prunes, so the
        secret list held only the revealed rows and `index=i` 404'd for every
        row past the first revealed one. The `LookupError` tolerance stays for
        a read that lands mid re-render.
        """
        import time

        if self._roster() is not None:
            with self._on_roster() as roster:
                return roster.reveal_mail_secret(index, timeout)
        self.driver.click("mail-settings-credential-item-reveal-secret", index)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                s = self.driver.get_text(
                    "mail-settings-credential-item-secret", index
                ).strip()
            except LookupError:
                s = ""
            if s:
                return s
            time.sleep(0.25)
        return self.driver.get_text(
            "mail-settings-credential-item-secret", index
        ).strip()

    def wait_for_credential_count(self, expected: int, timeout: float = 10.0) -> bool:
        """Poll until the credentials list holds exactly `expected` rows. Returns
        whether it reached the count before the timeout (the dispatch → snapshot
        → re-render round-trip is async)."""
        import time

        if self._roster() is not None:
            with self._on_roster(must_load=False) as roster:
                return roster.wait_for_mail_rows(lambda n: n == expected, timeout)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.credential_count() == expected:
                return True
            time.sleep(0.3)
        return self.credential_count() == expected

    def wait_for_credential_count_at_least(self, n: int, timeout: float = 10.0) -> bool:
        """Poll until the credentials list holds at least `n` rows."""
        import time

        if self._roster() is not None:
            with self._on_roster(must_load=False) as roster:
                return roster.wait_for_mail_rows(lambda have: have >= n, timeout)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.credential_count() >= n:
                return True
            time.sleep(0.3)
        return self.credential_count() >= n

    # `mail-settings-enabled-toggle`'s own on/off, carried via the `state` attr
    # — same idiom as `SERVE_HERE_ID` above (`driver.get_attr(id, "state")`).
    ENABLED_TOGGLE_ID = "mail-settings-enabled-toggle"

    def enabled_toggle_state(self) -> str:
        """The enabled toggle's on/off, read via the `state` attr ("on"/"off")."""
        return self.driver.get_attr(self.ENABLED_TOGGLE_ID, "state") or ""

    def wait_for_enabled_toggle_state(self, expected: str, timeout: float = 15.0) -> bool:
        """Poll until the enabled toggle itself reports `expected` ("on"/"off")."""
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.enabled_toggle_state() == expected:
                return True
            time.sleep(0.3)
        return self.enabled_toggle_state() == expected

    def ensure_mail_enabled(self) -> None:
        """Make sure mail is enabled, idempotently.

        tier_3 tests share a session-scoped `nest_instance` (and a cached
        driver), so a prior test may have already enabled mail for this actor —
        in which case mail is on and the page should report enabled.

        Two-phase, because the two stale proxies fail in opposite directions:

        1. **Is mail on?** — the *toggle's own* on/off signal
           (`enabled_toggle_state()` == "on"), NOT the derived status-indicator
           text and NOT the credential-row count. `wait_for_enabled_status`
           (`STATUS_ENABLED` = "All up to date") is **not** a proxy for "is mail
           on" — shared Rust `settings_status_label` (`libs/fauna-client-mail-
           settings/src/state.rs`) requires `enabled && status == Idle`; a
           reconnected mail machine can sit in `Syncing`/`RotationInProgress`
           past this method's poll window under load even though `snap.enabled`
           is already true. That previously mis-fired the enable gesture against
           an *already*-enabled actor: it clicks `mail-settings-enabled-toggle`
           OFF, opening the destructive disable-confirm dialog instead of the
           add-credential form (timeout on `mail-add-credential-name-input` — a
           real production-macOS-baseline red this class of bug caused). The
           toggle's own `state` attr answers only the question this gate needs,
           independent of sync status; a genuinely-off actor never reports "on",
           so the wait times out and we drive enable.

        2. **Settle to a fully-hydrated page** — `snap.enabled` (and so the
           toggle state) flips a beat *before* the snapshot carrying
           `credentials` / the resolved `mua` host finalizes on the web re-mount
           (the machine applies snapshots in stages). So after confirming
           enabled, wait for the credential row(s) to render, so callers that
           read the credentials list or MUA details immediately after see the
           complete page, not the intermediate snapshot (whose `mua.imap_host`
           is still the placeholder and whose `credentials` is still empty).
           This wait never touches the toggle — it's a pure settle — so it
           can't re-introduce the phase-1 mis-fire. An enabled mailbox always
           has ≥1 credential.
        """
        # Phase 1 is a POLL, not a one-shot read. The rationale above already
        # describes it as one ("a genuinely-off actor never reports 'on', so the
        # WAIT times out and we drive enable") — but the code read the attr once,
        # which re-opened the very mis-fire the rationale exists to prevent, just
        # through a different door. The toggle's `state` reads "off" both when
        # mail is off AND while the page is still hydrating (phase 2 below says
        # so outright: `snap.enabled` "flips a beat before" the rest of the
        # snapshot), and nothing distinguishes the two from outside. A one-shot
        # read that lands in that beat clicks the toggle on an ALREADY-ENABLED
        # actor, opening the destructive disable-confirm dialog instead of the
        # add-credential form — the `mail-add-credential-name-input` timeout
        # named above. Measured 2026-07-30: 4 occurrences across 3
        # `test_events.py` runs, on BOTH apple apps, every one of them after
        # an earlier test in the same session had already enabled mail for the
        # shared actor.
        #
        # Convention 14: the budget is not a timing assertion. An already-on
        # actor returns on the first poll and pays nothing; only a genuinely-off
        # actor pays the ceiling, once per client per session (mail is enabled
        # once for the session-scoped nest, and every later call sees "on").
        self.driver.wait_for(self.ENABLED_TOGGLE_ID, timeout=10.0)
        if not self.wait_for_enabled_toggle_state("on", timeout=15.0):
            self.enable_mail("Default")
            assert self.wait_for_enabled_toggle_state("on", timeout=15.0), (
                "enabling mail should flip the enabled toggle to on; "
                f"status={self.status_text()!r}, "
                f"error: {self.page_error_text(timeout=2.0)!r}"
            )
        self.wait_for_credential_count_at_least(1, timeout=10.0)

    def rotate_keys(self, exclude: tuple[str, ...] = ()) -> None:
        """Rotate the MSEK (hard revoke) via the rotate-keys confirm form
        (mail-settings.md § User actions → "Rotate mail keys" → StartRotation).

        Opens the inline rotate-keys form, ticks the
        `mail-rotate-keys-exclude-item` checkbox of each app password named in
        `exclude` (by display name — the "this one is compromised, don't
        re-wrap it" case, mail-credentials.md § Hard revoke), and confirms.
        The checkboxes are flat-indexed, one per credential, each reading its
        credential's display name. An empty `exclude` is a no-exclusion
        rotation: every surviving credential is re-wrapped under a fresh MSEK.
        """
        self.driver.wait_for("mail-settings-rotate-keys-button", timeout=10.0)
        self.driver.click("mail-settings-rotate-keys-button")
        self.driver.wait_for("mail-rotate-keys-confirm-button", timeout=10.0)
        if exclude:
            names = [
                self.driver.get_text("mail-rotate-keys-exclude-item", i).strip()
                for i in range(self.driver.count("mail-rotate-keys-exclude-item"))
            ]
            for name in exclude:
                assert name in names, (
                    f"no exclude checkbox for {name!r} on the rotate form; listed: {names!r}"
                )
                self.driver.click("mail-rotate-keys-exclude-item", names.index(name))
        self.driver.click("mail-rotate-keys-confirm-button")

    def wait_for_rotation_to_finish(self, timeout: float) -> bool:
        """After :meth:`rotate_keys`, wait until the rotation has finished —
        the confirm form closes when the rotation's dispatch returns, having
        shown its progress line while it ran (mail-settings.md § Element
        visibility: the progress indicator shows "during multi-step
        rotation"). Returns whether it closed within ``timeout``.

        A real barrier on every app: the form stays open for the whole rotation
        (tui, web, linux, macOS, iOS, android, windows). An app that closed the
        form on dispatch instead would pass it at once, so a caller whose next
        step must not interrupt the rotation (a relaunch) could not rely on it.
        """
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not self.driver.is_visible("mail-rotate-keys-confirm-button"):
                return True
            time.sleep(0.3)
        return not self.driver.is_visible("mail-rotate-keys-confirm-button")

    def revoke_credential(self, index: int = 0) -> None:
        """Soft-revoke the credential at `index` (mail-settings.md § User actions
        → "Revoke a credential").

        Per-app the revoke gesture differs, so this is **adaptive** rather than
        a fixed two-click:

        - **linux** uses a two-click inline confirm (no modal; no ui.yaml ID for a
          separate confirm button — `apps/fauna-linux/src/settings/mail.rs`): the
          first click ARMS (label → "Confirm?", count unchanged, auto-disarms
          after 4 s), the second dispatches `RevokeCredential`.
        - **web** revokes on a SINGLE click (`MailSettingsSection.svelte`'s revoke
          button dispatches `RevokeCredential` directly — no arm step).

        A blind second click is wrong on web: once the list re-renders (the row
        gone, count dropped), the button at `index` now binds the *next*
        credential, so a second click revokes a SECOND credential — the bug that
        left the session actor with zero credentials and broke a later test's
        `ensure_mail_enabled`. So: click once, then click again ONLY if the count
        hasn't dropped within a short window (< linux's 4 s auto-disarm). On web
        the count drops and we return with no second click; on linux it stays put
        (armed) and the second click confirms. If web is slow and the count hasn't
        dropped yet, the row hasn't re-rendered either, so the second click
        re-targets the SAME credential — an idempotent no-op, never an over-revoke.
        """
        import time

        # On the Connected apps roster the revoke is that page's own gesture:
        # Revoke opens an inline confirm with its own button.
        if self._roster() is not None:
            with self._on_roster() as roster:
                roster.revoke_mail_row(index)
            return
        before = self.credential_count()
        self.driver.click("mail-settings-credential-item-revoke-button", index)
        deadline = time.monotonic() + 2.0
        while time.monotonic() < deadline:  # deadline-ok: documented below — the timeout branches to the confirm click, it isn't a failure to signal
            if self.credential_count() < before:
                return  # single-click revoke landed (web) — no confirm click
            time.sleep(0.2)
        # Count unchanged within the window → linux's armed inline-confirm; the
        # second click (still within the 4 s arm) dispatches RevokeCredential.
        self.driver.click("mail-settings-credential-item-revoke-button", index)

    def disable_mail(self) -> None:
        """Disable mail entirely via the destructive confirmation dialog
        (mail-settings.md § Disable mail).

        Flipping `mail-settings-enabled-toggle` off snaps it back on (the dialog
        is the real decision point) and opens the `mail-settings-disable-confirm`
        dialog; its destructive `mail-settings-disable-confirm-button` dispatches
        MailSettingsAction::DisableMail = bulk soft-revoke every credential (one
        RevokeCredential per row) + clear the `msek` in the `fauna.state.mail` plane (snapshot
        `enabled` → false). The dialog is a presented adw::MessageDialog whose
        response button is tagged via `tag_response_button` — the same shape the
        thread-rename / add-participant / folder-delete confirms use, so the
        agent drives it directly (unlike the single-credential revoke, which is a
        two-click inline confirm with no dialog).
        """
        self.driver.wait_for("mail-settings-enabled-toggle", timeout=10.0)
        self.driver.click("mail-settings-enabled-toggle")
        self.driver.wait_for("mail-settings-disable-confirm-button", timeout=10.0)
        self.driver.click("mail-settings-disable-confirm-button")

    # Status-indicator text the linux app renders (settings/mail.rs::render):
    # "Mail is disabled" while off, "Syncing mail credentials…" in-flight, and
    # "All up to date" once mail is enabled and the snapshot has settled. The
    # marker label carries this text verbatim for the e2e driver.
    STATUS_DISABLED = "Mail is disabled"
    STATUS_ENABLED = "All up to date"

    def status_text(self) -> str:
        """The current `mail-settings-status-indicator` text, or "" if absent.

        Brings the indicator into view before reading. On windows `is_visible`
        checks `!IsOffscreen`, and an OAUTHBEARER enable leaves the one-time-token
        reveal open (`enable_mail_oauthbearer` keeps the add-credential form open
        showing the token), which scrolls the top-of-page status indicator
        offscreen — so a bare `is_visible` read returns "" even though the status
        correctly reports enabled (the PLAIN enable closes the form, so its read
        never hit this). `wait_for`'s targeted scroll-into-view brings it back; it
        is a no-op on linux/web, where the indicator is already on-screen. Returns
        "" only if the indicator never renders.
        """
        try:
            self.driver.wait_for("mail-settings-status-indicator", timeout=2.0)
        except TimeoutError:
            return ""
        return self.driver.get_text("mail-settings-status-indicator")

    def wait_for_enabled_status(self, timeout: float = 15.0) -> bool:
        """Poll until the status indicator reports mail as enabled.

        After toggling mail on, `EnableMail` dispatches over WS-RPC and the page
        re-renders from the resulting snapshot: the status flips from "Mail is
        disabled" through a transient "Syncing…" to "All up to date" once
        `snap.enabled` is true and `snap.status` is Idle. Returns whether it
        reached the enabled status before the timeout.

        This is the regression guard for the user-visible freeze symptom: if the
        EnableMail dispatch fails (e.g. RpcDisconnected against a remote nest),
        `snap.enabled` stays false, so the status never leaves "Mail is disabled"
        — this returns False and the caller can surface `page_error_text()`.
        """
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.status_text() == self.STATUS_ENABLED:
                return True
            time.sleep(0.3)
        return self.status_text() == self.STATUS_ENABLED

    def page_error_text(self, timeout: float = 10.0) -> str:
        """Return the mail page's `error-message` text once it appears.

        Reads the `error-message` element directly (waiting for it to render)
        rather than via `app.error_text()`. On clients whose state protocol
        surfaces a window-global error message — linux's authenticated status
        view does — `app.error_text()` reflects that global label, not the
        embedded mail page's, so it would shadow a mail-page error. The element
        is the page's ground truth. Returns "" if no error appears in time.
        """
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("error-message"):
                return self.driver.get_text("error-message")
            time.sleep(0.3)
        return ""

    # --- Local IMAP/CalDAV-serving toggle (mail-settings.md § Local IMAP/CalDAV-
    # serving toggle). User-set, default on; visible when mail is enabled. The
    # SwitchRow's on/off is carried via the `state` attr (on/off) — the uniform
    # cross-app read idiom (driver.get_attr(id, "state")). ---
    SERVE_HERE_ID = "mail-settings-serve-here-toggle"

    def serve_here_visible(self, timeout: float = 6.0) -> bool:
        """True once the (enabled-gated) serve-here toggle is on screen."""
        try:
            self.driver.wait_for(self.SERVE_HERE_ID, timeout=timeout)
            return True
        except TimeoutError:
            return False

    def serve_here_state(self) -> str:
        """The serve-here toggle's on/off, read via the `state` attr ("on"/"off")."""
        return self.driver.get_attr(self.SERVE_HERE_ID, "state") or ""

    def wait_for_serve_here_state(self, expected: str, timeout: float = 12.0) -> bool:
        """Poll until the serve-here toggle reports `expected` ("on"/"off").

        The flip is async (dispatch → set_mail_serving_enabled WS-RPC → snapshot
        → re-render writes the `state` attr); the value only flips once the nest
        confirms the write (the dispatch is non-optimistic), so this doubles as
        the regression guard that the toggle reflects the *nest's* state, not an
        optimistic local one.
        """
        import time

        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.serve_here_state() == expected:
                return True
            time.sleep(0.3)
        return self.serve_here_state() == expected

    def set_serve_here(self, enabled: bool) -> None:
        """Drive the serve-here toggle to `enabled`, idempotently.

        The SwitchRow click flips its state (agent set_active), dispatching
        MailSettingsAction::SetServingEnabled with the new value; we click only
        when the current `state` attr differs from the target so the call is
        idempotent against the session-scoped shared actor.
        """
        self.driver.wait_for(self.SERVE_HERE_ID, timeout=10.0)
        want = "on" if enabled else "off"
        if self.serve_here_state() != want:
            self.driver.click(self.SERVE_HERE_ID)

    def mua_instructions_visible(self, timeout: float = 6.0) -> bool:
        """True once the (enabled-gated) MUA-instructions block is on screen.

        Gives the async EnableMail round-trip (mail-state load → snapshot
        mint → blob provision → render) time to settle. Returns False on
        timeout rather than raising, so the caller decides how to assert.
        """
        try:
            self.driver.wait_for("mail-settings-mua-instructions", timeout=timeout)
            return True
        except TimeoutError:
            return False

    # --- Forwarding (mail-forwarding.md § Per-account "forward all",
    # § Per-account forward rate-limit). Two inputs committed on Enter — the
    # `mail-spam-threshold-override-input` shape: no save button, and the field
    # repaints from the value the nest persisted. Shown while mail is enabled. ---
    FORWARD_ALL_TO_ID = "mail-settings-forward-all-to-input"
    FORWARD_PER_HOUR_ID = "mail-settings-forward-per-hour-input"

    def set_forward_all_to(self, address: str) -> None:
        """Type the forward-all address (blank stops forwarding) and commit it
        with Enter → `fauna.bridges.set_forward_all_to`."""
        self.driver.wait_for(self.FORWARD_ALL_TO_ID, timeout=10.0)
        self.driver.clear_and_type(self.FORWARD_ALL_TO_ID, address)
        self.driver.press_key(self.FORWARD_ALL_TO_ID, "Enter")

    def forward_all_to(self) -> str:
        """The forward-all field's current text."""
        self.driver.wait_for(self.FORWARD_ALL_TO_ID, timeout=10.0)
        return (self.driver.get_text(self.FORWARD_ALL_TO_ID) or "").strip()

    def set_forward_per_hour(self, value: str) -> None:
        """Type the hourly forwarding limit and commit it with Enter →
        `fauna.bridges.set_forward_per_hour`."""
        self.driver.wait_for(self.FORWARD_PER_HOUR_ID, timeout=10.0)
        self.driver.clear_and_type(self.FORWARD_PER_HOUR_ID, value)
        self.driver.press_key(self.FORWARD_PER_HOUR_ID, "Enter")

    def forward_per_hour(self) -> str:
        """The hourly-limit field's current text."""
        self.driver.wait_for(self.FORWARD_PER_HOUR_ID, timeout=10.0)
        return (self.driver.get_text(self.FORWARD_PER_HOUR_ID) or "").strip()
