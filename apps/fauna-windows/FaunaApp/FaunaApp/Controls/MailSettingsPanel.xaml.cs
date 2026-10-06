using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_client_mail_settings;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "Mail" settings surface (docs/goal/ui/mail-settings.md): manage
/// third-party-MUA access — enable/disable mail, see/revoke credentials, rotate
/// the MSEK, read the MUA connection details. Dumb renderer of the shared
/// <c>fauna_client_mail_settings::MailSettingsMachine</c> (exposed over UniFFI by
/// libs/fauna-ffi/src/mail_admin.rs) — no business logic here (priority #2).
/// Lifts the linux reference (apps/fauna-linux/src/settings/mail.rs) and mirrors
/// the sibling <see cref="FaunaApp.Controls.NestsPanel"/>: hosted by its dedicated Settings
/// shell sub-page <c>SettingsMailPage</c>, which supplies <c>ServiceClients</c> via
/// <c>OnNavigatedTo</c> and surfaces this panel's <see cref="ErrorChanged"/> on its
/// own page-level <c>error-message</c>. Builds the machine over the session's
/// shared, auto-reconnecting WS-RPC connection (the INestRpcClient seam), which
/// supplies the actor secret + node_url the machine needs (unlike the admin
/// machines). The machine is request/response (no observer), so every dispatch is
/// followed by a snapshot re-render.
/// </summary>
public sealed partial class MailSettingsPanel : UserControl
{
    /// <summary>Which dispatch the add-credential form's submit maps to.</summary>
    private enum FormMode { Enable, Add }

    private ServiceClients? _clients;
    private MailSettingsMachine? _machine;
    private bool _suppressToggle;
    // Guards the programmatic ServeHereToggle.IsOn set in RenderSnapshot from
    // re-firing Toggled (same idiom as _suppressToggle for the enabled switch).
    private bool _suppressServeHere;
    // Guards the programmatic AutogenerateToggle.IsOn set from re-firing Toggled.
    private bool _suppressAutogen;
    // Every nest stores at rest encrypted (no-modes, storage-modes.md), so the
    // manual-password warning's nest_encrypted input is unconditionally true —
    // the same constant tui and apple pass to warn_manual_password.
    private const bool _nestEncrypted = true;
    private FormMode _formMode = FormMode.Enable;
    // Rotate-keys exclude checkboxes, rebuilt from the snapshot when the form opens.
    private readonly List<(string CredentialId, CheckBox Box)> _rotateChecks = new();
    // The rotate form's hold-open decision (FaunaApp.Core, unit-tested): progress text +
    // confirm/cancel enablement, and the confirm → await → collapse order.
    private readonly MailRotateForm _rotateForm = new();

    /// <summary>Raised with the machine's error message (or null to clear) so the host
    /// page surfaces it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public MailSettingsPanel()
    {
        this.InitializeComponent();
    }

    /// <summary>Supplied by the host page (SettingsMailPage.OnNavigatedTo) before Loaded
    /// fires — carries the secrets used to build the WS client.</summary>
    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureMachineAsync()
    {
        if (_machine is not null || _clients?.Rpc is null) return;
        // Build the machine over the session's shared, auto-reconnecting WS-RPC
        // connection (the INestRpcClient seam) rather than a per-panel one-shot
        // FfiNestClient.Connect() that would surface a transient os-error-10061 as a
        // panel error while shared-client pages recover. The seam supplies the actor
        // secret (sign submission tokens + key the account plane) + node_url (derive the MUA
        // connection details) the machine needs — see mail_admin.rs
        // build_mail_settings_machine.
        _machine = await _clients.Rpc.BuildMailSettingsMachineAsync();
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            await EnsureMachineAsync();
            // The transport already tolerates the post-login connect race for a
            // single RPC (transport.md § Request lifecycle step 3) — no app-level
            // retry needed here.
            await _machine!.Hydrate();
            RenderSnapshot(_machine!.Snapshot());
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(Strings.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Dispatch an action, then render the resulting snapshot. The machine
    /// captures any user-facing error into <c>snapshot.error</c> (and also throws), so
    /// the throw is swallowed and the error read from the snapshot — matching linux,
    /// which discards the dispatch Result and renders the snapshot.</summary>
    private async Task DispatchAsync(MailSettingsAction action)
    {
        if (_machine is null) return;
        try { await _machine.Dispatch(action); }
        catch (Exception) { /* surfaced via snapshot.error below */ }
        RenderSnapshot(_machine.Snapshot());
    }

    private void RenderSnapshot(MailSettingsSnapshot snap)
    {
        // Guard suppresses the Toggled re-fire from the programmatic IsOn set;
        // the on/off is mirrored to HelpText so the e2e harness's
        // `ensure_mail_enabled` phase-1 gate reads it via get_attr(id, "state")
        // ("on"/"off") instead of the derived status label (mirrors
        // ServeHereToggle below).
        _suppressToggle = true;
        EnabledToggle.IsOn = snap.enabled;
        _suppressToggle = false;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            EnabledToggle, snap.enabled ? "on" : "off");

        // Status-indicator text single-sourced in shared Rust
        // (fauna_client_mail_settings::settings_status_label; mail-settings.md § the
        // status indicator). The Idle arm gates on `enabled` (a disabled mailbox reads
        // status_disabled), and RotationInProgress carries its remaining-count via the
        // LocalizedText {count} arg the resolver substitutes.
        StatusIndicator.Text =
            S.Resolve(FaunaClientMailSettingsMethods.SettingsStatusLabel(snap.status, snap.enabled));

        PendingRotationBanner.Visibility =
            snap.pendingRotation is not null ? Visibility.Visible : Visibility.Collapsed;

        // Through the seam, not straight off the snapshot: while this panel's own
        // rotation is in flight the snapshot is still the pre-rotation one, and a
        // re-render must not wipe the progress line or re-enable the controls.
        ApplyRotatePaint(_rotateForm.Paint(snap.status));

        ErrorChanged?.Invoke(string.IsNullOrEmpty(snap.error) ? null : snap.error);

        // The shared credential-management section (add / rotate / keys-info),
        // the serve-here toggle, and the MUA-instructions block all render whenever the
        // actor has a provisioned mailbox — email OR CalDAV OR serving ≥1 folder over
        // WebDAV — so a DAV-only deployment (email off) can still obtain + rotate its
        // one shared bridge password (mail-settings.md § CalDAV-only mailbox;
        // caldav-server.md / webdav-server.md § Independent enablement — the same
        // `(actor, default)` credential AUTHs IMAP+SMTP+CalDAV+WebDAV). The predicate is
        // computed once in shared Rust (priority #2/#4:
        // `MailSettingsSnapshot::credential_management_reachable`) rather than
        // re-derived per client. The enabled-toggle + status stay email-specific (gated
        // on snap.enabled).
        var mailbox = snap.credentialManagementReachable;

        // The list itself is the Connected apps roster's (connected-apps.md); this page
        // keeps the one pointer line while a mailbox exists.
        CredentialsMovedText.Visibility = mailbox ? Visibility.Visible : Visibility.Collapsed;

        // Manage buttons: add visible when a mailbox exists; rotate when ≥ 1 credential.
        ManageGroup.Visibility = mailbox ? Visibility.Visible : Visibility.Collapsed;
        RotateKeysButton.Visibility =
            snap.credentials.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        // keys-info explainer: visible alongside the manage group (mailbox provisioned).
        KeysInfoText.Visibility = mailbox ? Visibility.Visible : Visibility.Collapsed;

        // MUA instructions render whenever a mailbox is provisioned (email OR CalDAV OR
        // WebDAV). Within the one block, the IMAP/SMTP host/port rows describe the
        // *email* protocol → gated on `enabled`; the CalDAV host/port rows describe the
        // *calendar* protocol → gated on `caldavEnabled` (mail.<domain>:443, served
        // independent of email — caldav-server.md § Network exposure); the WebDAV URL
        // row describes the *files* protocol → gated on `servesWebdavSet` (a single full
        // collection-root URL, no host/port split — no SRV autodiscovery exists for
        // WebDAV; mail-settings.md § WebDAV files); the username-format + auth rows are
        // shared by all (one credential AUTHs IMAP+SMTP+CalDAV+WebDAV) → shown whenever
        // the block is. Values are set unconditionally; per-row visibility decides what
        // the user sees (mirrors linux settings/mail.rs).
        MuaGroup.Visibility = mailbox ? Visibility.Visible : Visibility.Collapsed;
        MuaEmailRows.Visibility = snap.enabled ? Visibility.Visible : Visibility.Collapsed;
        MuaCaldavRows.Visibility = snap.caldavEnabled ? Visibility.Visible : Visibility.Collapsed;
        MuaImapHost.Text = snap.mua.imapHost;
        MuaImapPort.Text = snap.mua.imapPort.ToString();
        MuaSmtpHost.Text = snap.mua.smtpHost;
        MuaSmtpPort.Text = snap.mua.smtpPort.ToString();
        MuaCaldavHost.Text = snap.mua.caldavHost;
        MuaCaldavPort.Text = snap.mua.caldavPort.ToString();
        MuaWebdavRows.Visibility = snap.servesWebdavSet ? Visibility.Visible : Visibility.Collapsed;
        MuaWebdavUrl.Text = snap.mua.webdavUrl;
        MuaUsername.Text = snap.mua.usernameFormat;
        MuaAuth.Text = snap.mua.authMechanism;

        // Local IMAP/CalDAV-serving toggle: dumb render of snap.serving_enabled
        // (mail-settings.md § Local IMAP/CalDAV-serving toggle). Visible whenever a
        // mailbox is provisioned (email OR CalDAV) — the per-actor "serve my mailbox
        // over IMAP/CalDAV here" flip applies to a CalDAV-only mailbox just as much as
        // an email one. The guard suppresses the Toggled re-fire from the programmatic
        // IsOn set; the on/off is mirrored to HelpText so the e2e reads it via
        // get_attr(id, "state") ("on"/"off").
        _suppressServeHere = true;
        ServeHereToggle.IsOn = snap.servingEnabled;
        _suppressServeHere = false;
        ServeHereGroup.Visibility = mailbox ? Visibility.Visible : Visibility.Collapsed;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ServeHereToggle, snap.servingEnabled ? "on" : "off");
    }

    // ── Enable toggle ──

    private void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        // NB: do NOT bail on `_machine is null` here. The WS connect + hydrate runs
        // async in LoadAsync (up to 8×500ms retry), but the toggle element renders
        // immediately and the e2e only waits for *it* before clicking — so a click
        // can land before the machine is ready. Bailing then silently dropped the
        // open-form (and LoadAsync's later RenderSnapshot flipped the toggle back
        // off under the suppress guard), so the add-credential form never appeared
        // — an intermittent enable failure. Open the form regardless; Submit_Click
        // guards `_machine is null`, and by submit time (wait-for + type) the load
        // has settled.
        if (_suppressToggle) return;
        if (EnabledToggle.IsOn)
        {
            // ON from disabled → open the add-credential form in Enable mode (the
            // first credential is the enable path, mail-settings.md § User actions).
            OpenForm(FormMode.Enable);
        }
        else
        {
            // OFF → open the destructive disable-confirm overlay. Snap the toggle back
            // ON immediately (under the guard) so it never shows a lying "off" before
            // the user confirms — the overlay is the real decision point (mirrors linux
            // open_disable_confirm). On confirm DisableMail's re-render lands the toggle
            // off; on cancel it stays on. Open regardless of machine-readiness so an
            // early click can't strand the toggle off (DisableConfirm_Click guards null).
            _suppressToggle = true;
            EnabledToggle.IsOn = true;
            _suppressToggle = false;
            ErrorChanged?.Invoke(null);
            DisableConfirmForm.Visibility = Visibility.Visible;
        }
    }

    // ── Disable-mail confirm (destructive) ──

    private async void DisableConfirm_Click(object sender, RoutedEventArgs e)
    {
        DisableConfirmForm.Visibility = Visibility.Collapsed;
        if (_machine is null) return;
        // Bulk soft-revoke every credential (one RevokeCredential per row) + clear
        // the `fauna.state.mail` MSEK; the snapshot returns enabled:false, landing the toggle
        // off (mail-settings.md § Disable mail). Deliberately NOT the Admin-scoped,
        // deployment-wide set_mail_enabled(false) — that boots/tears the box-level s6
        // mail subsystem shared by every mailbox; the shared MailSettingsMachine
        // enforces the asymmetry, this is just the dispatch.
        StatusIndicator.Text = S.Get("settings/mail/status_syncing");
        await DispatchAsync(new MailSettingsAction.DisableMail());
    }

    private void DisableCancel_Click(object sender, RoutedEventArgs e)
    {
        // No-op: the toggle was already restored to on; just close the overlay and
        // re-sync to the machine's actual state (ui.yaml mail-settings-disable-confirm
        // § cancel = no-op).
        DisableConfirmForm.Visibility = Visibility.Collapsed;
        if (_machine is not null) RenderSnapshot(_machine.Snapshot());
    }

    // ── Local IMAP/CalDAV-serving toggle ──

    private async void ServeHereToggle_Toggled(object sender, RoutedEventArgs e)
    {
        // User flip → SetServingEnabled for this actor. Non-optimistic: the machine
        // fires the caller-scoped set_mail_serving_enabled WS-RPC and only then sets
        // snapshot.serving_enabled, so DispatchAsync's re-render reflects the
        // nest-confirmed value (and reverts the toggle + surfaces snapshot.error on
        // failure). Guarded against the programmatic IsOn set in RenderSnapshot.
        if (_suppressServeHere) return;
        await DispatchAsync(new MailSettingsAction.SetServingEnabled(ServeHereToggle.IsOn));
    }

    // ── Add-credential form ──

    private void AddCredential_Click(object sender, RoutedEventArgs e) => OpenForm(FormMode.Add);

    private void OpenForm(FormMode mode)
    {
        _formMode = mode;
        NameInput.Text = mode == FormMode.Enable ? "Default" : string.Empty;
        TypeSelector.IsChecked = false;
        PasswordInput.Password = string.Empty;
        PasswordInput.PasswordRevealMode = PasswordRevealMode.Hidden;
        PasswordShowToggle.IsChecked = false;
        PasswordShowToggle.Content = S.Get("settings/mail/show");
        PasswordStrength.Text = string.Empty;
        PasswordInput.IsEnabled = true;
        // Auto-generate defaults ON; the actual password is minted when PLAIN is
        // revealed (TypeSelector_Toggled → ApplyAutogenerateState).
        _suppressAutogen = true;
        AutogenerateToggle.IsOn = true;
        _suppressAutogen = false;
        WeakPasswordWarning.Visibility = Visibility.Collapsed;
        PlainBox.Visibility = Visibility.Collapsed;
        TokenBox.Visibility = Visibility.Collapsed;
        TokenDisplay.Text = string.Empty;
        AddCredentialInputBox.Visibility = Visibility.Visible;
        SubmitButton.Content = S.Get(mode == FormMode.Enable
            ? "settings/mail/submit_enable"
            : "settings/mail/submit_add");
        CancelButton.Content = S.Get("settings/mail/cancel");
        ErrorChanged?.Invoke(null);
        AddCredentialForm.Visibility = Visibility.Visible;
    }

    private void TypeSelector_Toggled(object sender, RoutedEventArgs e)
    {
        var plain = TypeSelector.IsChecked == true;
        PlainBox.Visibility = plain ? Visibility.Visible : Visibility.Collapsed;
        if (plain)
        {
            // Revealing PLAIN arms auto-generate (default ON) and mints a fresh
            // password into the read-only input, shown-once for the user to copy.
            _suppressAutogen = true;
            AutogenerateToggle.IsOn = true;
            _suppressAutogen = false;
            ApplyAutogenerateState();
        }
    }

    /// <summary>Auto-generate ON (default): shared Rust mints a strong a-zA-Z0-9
    /// password into the read-only, revealed password input, shown-once for the user
    /// to copy. OFF: clear it, allow manual masked entry, show the strength meter, and
    /// — on an ENCRYPTED nest — the manual-password warning (warn_manual_password).
    /// All decision logic lives in shared Rust (priority #2); this is glue.</summary>
    private void ApplyAutogenerateState()
    {
        var auto = AutogenerateToggle.IsOn;
        // The autogenerate-vs-manual decision (PLAIN only; OAUTHBEARER always None)
        // is the shared fauna_client_mail_settings::password_gen::resolve_autogenerated_password
        // — called ONLY here, at the settled toggle edge, never re-derived at submit
        // (mail-credentials.md § Mint-once sequencing; the apple secret-integrity bug
        // class this closes: re-minting between display and submit).
        if (FaunaFfiMethods.ResolveAutogeneratedBridgePassword(CredentialKind.Plain, auto) is { } generated)
        {
            PasswordInput.Password = generated;
            PasswordInput.IsEnabled = false;
            PasswordInput.PasswordRevealMode = PasswordRevealMode.Visible;
        }
        else
        {
            PasswordInput.Password = string.Empty;
            PasswordInput.IsEnabled = true;
            PasswordInput.PasswordRevealMode = PasswordRevealMode.Hidden;
            PasswordShowToggle.IsChecked = false;
            PasswordShowToggle.Content = S.Get("settings/mail/show");
        }
        PasswordShowToggle.Visibility = auto ? Visibility.Collapsed : Visibility.Visible;
        PasswordStrength.Visibility = auto ? Visibility.Collapsed : Visibility.Visible;
        PasswordStrength.Text = auto ? string.Empty : PasswordStrengthLabel(PasswordInput.Password);
        WeakPasswordWarning.Visibility =
            FaunaFfiMethods.WarnManualBridgePassword(auto, _nestEncrypted)
                ? Visibility.Visible
                : Visibility.Collapsed;
    }

    private void AutogenerateToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_suppressAutogen) return;
        ApplyAutogenerateState();
    }

    private void PasswordShowToggle_Toggled(object sender, RoutedEventArgs e)
    {
        var show = PasswordShowToggle.IsChecked == true;
        PasswordInput.PasswordRevealMode = show ? PasswordRevealMode.Visible : PasswordRevealMode.Hidden;
        PasswordShowToggle.Content = S.Get(show ? "settings/mail/hide" : "settings/mail/show");
    }

    private void PasswordInput_Changed(object sender, RoutedEventArgs e) =>
        PasswordStrength.Text = PasswordStrengthLabel(PasswordInput.Password);

    private async void Submit_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;

        var displayName = NameInput.Text.Trim();
        if (string.IsNullOrEmpty(displayName)) displayName = "Default";

        var kind = TypeSelector.IsChecked == true ? CredentialKind.Plain : CredentialKind.OAuthBearer;

        // Build the secret + (OAUTHBEARER) the human-copyable token string.
        byte[] secret;
        string? tokenToShow = null;
        if (kind == CredentialKind.OAuthBearer)
        {
            var token = FaunaFfiMethods.GenerateBridgeToken();
            secret = Encoding.UTF8.GetBytes(token);
            tokenToShow = token;
        }
        else
        {
            var pw = PasswordInput.Password;
            if (string.IsNullOrEmpty(pw))
            {
                ErrorChanged?.Invoke(S.Get("settings/mail/password_required"));
                return;
            }
            secret = Encoding.UTF8.GetBytes(pw);
        }

        MailSettingsAction action = _formMode == FormMode.Enable
            ? new MailSettingsAction.EnableMail(displayName, kind, secret)
            : new MailSettingsAction.AddCredential(displayName, kind, secret);

        // Optimistic in-flight status.
        StatusIndicator.Text = S.Get("settings/mail/status_syncing");

        await DispatchAsync(action);

        if (string.IsNullOrEmpty(_machine.Snapshot().error))
        {
            if (tokenToShow is not null)
            {
                // OAUTHBEARER success: reveal the one-time token; keep the form open
                // so the user can copy it. Relabel cancel → Done.
                TokenDisplay.Text = tokenToShow;
                AddCredentialInputBox.Visibility = Visibility.Collapsed;
                TokenBox.Visibility = Visibility.Visible;
                CancelButton.Content = S.Get("settings/mail/done");
            }
            else
            {
                // PLAIN success: nothing to show once; close the form.
                AddCredentialForm.Visibility = Visibility.Collapsed;
            }
        }
        // On error, DispatchAsync already surfaced snapshot.error; leave the form open.
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        AddCredentialForm.Visibility = Visibility.Collapsed;
        if (_machine is not null) RenderSnapshot(_machine.Snapshot());
    }

    private void TokenCopy_Click(object sender, RoutedEventArgs e)
    {
        var token = TokenDisplay.Text;
        if (string.IsNullOrEmpty(token)) return;
        FaunaApp.Helpers.ClipboardHelper.CopyText(token);
        TokenCopyButton.Content = S.Get("settings/mail/copied");
    }

    // ── Rotate-keys form ──

    private void RotateKeys_Click(object sender, RoutedEventArgs e)
    {
        // A rotation is running: the form is already open, holding its progress.
        if (_machine is null || _rotateForm.InFlight) return;
        // Rebuild the exclude checkbox list from the current credentials. Checking a
        // box marks that credential compromised → dropped from the rotation.
        RotateExcludeList.Children.Clear();
        _rotateChecks.Clear();
        foreach (var cred in _machine.Snapshot().credentials)
        {
            var cb = new CheckBox { Content = cred.displayName };
            RotateExcludeList.Children.Add(cb);
            _rotateChecks.Add((cred.credentialId, cb));
        }
        RotateProgress.Text = string.Empty;
        ErrorChanged?.Invoke(null);
        RotateKeysForm.Visibility = Visibility.Visible;
    }

    /// <summary>Confirm holds the form open until the rotation returns — confirm and
    /// cancel disabled, the progress line painted from the shared
    /// <c>rotation_rewrap_count</c> — and only then collapses it
    /// (<see cref="MailRotateForm.ConfirmAsync"/>; a second confirm while one runs is
    /// ignored). Collapsing first left nothing on screen showing the rotation running,
    /// so <c>wait_for_rotation_to_finish</c> passed at once.</summary>
    private async void RotateConfirm_Click(object sender, RoutedEventArgs e)
    {
        var machine = _machine;
        if (machine is null) return;
        var excluded = _rotateChecks
            .Where(c => c.Box.IsChecked == true)
            .Select(c => c.CredentialId)
            .ToArray();
        var rewrapCount = FaunaClientMailSettingsMethods.RotationRewrapCount(machine.Snapshot(), excluded);
        await _rotateForm.ConfirmAsync(
            rewrapCount,
            status: () => machine.Snapshot().status,
            paint: ApplyRotatePaint,
            dispatch: () => DispatchAsync(new MailSettingsAction.StartRotation(excluded)),
            collapse: () => RotateKeysForm.Visibility = Visibility.Collapsed);
    }

    private void RotateCancel_Click(object sender, RoutedEventArgs e)
    {
        RotateKeysForm.Visibility = Visibility.Collapsed;
        if (_machine is not null) RenderSnapshot(_machine.Snapshot());
    }

    private void ApplyRotatePaint(MailRotateFormPaint paint)
    {
        RotateProgress.Text = paint.ProgressText;
        RotateConfirmButton.IsEnabled = paint.ControlsEnabled;
        RotateCancelButton.IsEnabled = paint.ControlsEnabled;
    }

    private async void Resume_Click(object sender, RoutedEventArgs e) =>
        await DispatchAsync(new MailSettingsAction.ResumeRotation());

    // ── Helpers ──

    /// <summary>Advisory PLAIN-password strength word from the shared Rust meter
    /// (one canonical &lt;8 Weak / &lt;16 Fair / ≥16 Strong threshold across clients —
    /// <c>mail-settings.md</c>). Never gates submission; empty password ⇒ no readout.</summary>
    private static string PasswordStrengthLabel(string pw)
    {
        var label = FaunaClientMailSettingsMethods.PasswordStrengthLabel(pw);
        return label is null ? string.Empty : S.Resolve(label);
    }
}
