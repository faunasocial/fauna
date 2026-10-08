using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Account sub-page (settings.md § Navigation model). Pure actions:
/// handle / nest-url display + change-handle + the Danger Zone delete-account.
/// The live nest data (actor-id + quota) lives on the Status sub-page, not here.
/// Split out of the former single-scroll <see cref="SettingsPage"/>; constructs
/// its own <see cref="SettingsViewModel"/> from <see cref="ServiceClients"/> in
/// OnNavigatedTo, mirroring the admin sub-pages.
/// </summary>
public sealed partial class SettingsAccountPage : Page
{
    private SettingsViewModel? _viewModel;

    /// <summary>The identity secret + cached handle for the identity-export QR
    /// (settings.md § Identity export). Read from <see cref="ISessionAccount"/> — never
    /// from a settings snapshot (architecture/apps/common.md § Credential storage).
    /// Null in E2E / when no secret is available; the show button then no-ops.</summary>
    private string? _identitySecretHex;
    private string? _identityHandle;

    /// <summary>The signed-in identity, 64-hex, read from the session's crypto
    /// service (never a settings snapshot) — the same source the MLS store scoping
    /// and the account switcher use. Null before a key is loaded.</summary>
    private string? _sessionActorIdHex;

    /// <summary>The session's RPC seam, kept for the inherited-filter re-read on
    /// every Account entry. Null in E2E-without-clients; the line then stays
    /// on whatever the aftermath hook last cached.</summary>
    private INestRpcClient? _rpc;

    /// <summary>The Recovery Kit section's VM (settings.md § Recovery kit). Built in
    /// OnNavigatedTo beside <see cref="_viewModel"/>, off the same RPC seam; null in
    /// E2E / when no ServiceClients arrived, and the section then stays on its
    /// <c>status_loading</c> line with every action dead — which is the honest render
    /// (ui/README.md rule 5), not an omission.</summary>
    private RecoveryKitViewModel? _recoveryKit;

    /// <summary>The signed-in identity at the time the section hydrated — one value,
    /// two roles. At ceremony time it is the identity the account is moving <i>away</i>
    /// from, so the succession records which registry row the retired identity is
    /// before the switch makes it unnameable. At hydrate time it is whoever is signed
    /// in <i>now</i>, which is what lets the owed successor kit be claimed by the
    /// successor's session and refused to the departing one
    /// (<see cref="FaunaApp.Core.Services.SuccessionHandoff"/>).</summary>
    private string? _recoveryKitHydratedActor;

    /// <summary>The multi-account switcher's registry view + VM
    /// (long-term-store.md § Multi-account evolution). The registry is a disposable
    /// view over the process-wide store, so it is built per visit and released in
    /// <see cref="OnNavigatedFrom"/>.</summary>
    private FfiAccountRegistry? _switcherRegistry;
    private AccountSwitcherViewModel? _switcherViewModel;

    /// <summary>The Push notifications section's VM (settings.md § Push notifications),
    /// built per visit like the switcher: the registration it drives is the signed-in
    /// actor's, so a build-once section would toggle the previous account's row.</summary>
    private PushNotificationsViewModel? _push;

    /// <summary>Set while <see cref="RenderPush"/> writes the toggle, so the write is not
    /// read back as the user's click.</summary>
    private bool _renderingPush;

    public SettingsAccountPage()
    {
        this.InitializeComponent();
        Unloaded += Page_Unloaded;
    }

    /// <summary>Collapsed when false — the qualified-static form x:Bind requires.
    /// An UNqualified static call in a binding expression fails MSBuild with CS0176,
    /// and `just windows-xbind-lint` does not catch that shape.</summary>
    public static Visibility BoolToVisibility(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        EnterAccountMount();
        if (e.Parameter is ServiceClients clients)
        {
            // Export My Data's save step (settings.md § Data export) — the SAME
            // ISnapshotFileSaver seam single-file restore uses: dialog-less under
            // e2e (BackupsPage's own precedent), the native FileSavePicker in
            // production (SnapshotFileSavers.ForSession, convention 15).
            ISnapshotFileSaver exportFileSaver = Services.SnapshotFileSavers.ForSession();
            _viewModel = new SettingsViewModel(clients.Nest, clients.Rpc!, fileSaver: exportFileSaver);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            _identitySecretHex = clients.Account.SecretHex;
            _identityHandle = clients.Account.Handle;
            _sessionActorIdHex = clients.Crypto.HasKey ? clients.Crypto.ActorIdHex : null;
            _rpc = clients.Rpc;

            // Built fresh per visit, exactly like the switcher below and for the same
            // reason: this page is frame-navigated, so a build-once section would
            // render the PREVIOUS account's status after a switch — and every action's
            // enablement hangs off that read, so a stale one is the wrong answer, not
            // a weaker one.
            if (clients.Rpc is not null)
            {
                _recoveryKit = new RecoveryKitViewModel(clients.Rpc);
                _recoveryKit.PropertyChanged += RecoveryKit_PropertyChanged;
            }
        }

        // Push notifications: the registration is built on demand, over this visit's
        // session — the signed-in actor and the install's derived device id for it
        // (PushSession) — and is null with no session, which the VM answers with its
        // failure line (and, for a disable, by still clearing the bit).
        var pushRpc = _rpc;
        var pushActor = _sessionActorIdHex;
        var pushDevice = (e.Parameter as ServiceClients)?.Account.DeviceId;
        _push = new PushNotificationsViewModel(
            buildRegistration: async () =>
                pushRpc is null || string.IsNullOrEmpty(pushActor) || string.IsNullOrEmpty(pushDevice)
                    ? null
                    : await pushRpc.BuildPushRegistrationAsync(PushSession.IntentPath, pushActor, pushDevice),
            agent: new AgentStatusProbe(() => App.CurrentSyncAgent?.Channel));

        // Build the switcher fresh on every visit. This page is frame-navigated, so
        // OnNavigatedTo re-runs per visit — which is exactly what the goal doc
        // requires: the require-confirm flag MUST be re-read at render, never cached.
        // A build-once account page renders a stale OFF over a flag the admin
        // auto-default has since set, and the user's tap on that OFF-looking toggle
        // then writes ON — making the flag impossible to turn off. Linux hit this and
        // had to add a page-visible re-read; windows gets it structurally.
        _switcherRegistry = Services.CredentialStore.Registry();
        _switcherViewModel = new AccountSwitcherViewModel(_switcherRegistry)
        {
            // The VM never tears the session down itself — switching disposes the
            // nest/RPC clients, the backup driver and the conversations manager, all
            // of which live on App. It just asks.
            OnSwitchRequested = (actorId, confirmed) =>
                App.SwitchAccountHandler?.Invoke(actorId, confirmed)
                    ?? System.Threading.Tasks.Task.CompletedTask,
            // Stage-2 re-auth gate: the VM consults this before activating a flagged
            // account. Windows Hello lives on App (WinRT is unreachable from Core). A
            // null handler fails closed — the VM declines the flagged switch.
            ConfirmReauth = () =>
                App.ConfirmReauthHandler?.Invoke()
                    ?? System.Threading.Tasks.Task.FromResult(false),
        };
        AccountSwitcherList.ItemsSource = _switcherViewModel.Accounts;
        // The switcher's refusals (an unlaunchable switch, a remove another window
        // blocks) are this page's to paint: through the same RenderError choke point
        // as every other writer of `error-message`.
        _switcherViewModel.PropertyChanged += SwitcherViewModel_PropertyChanged;
    }

    private void SwitcherViewModel_PropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (sender is AccountSwitcherViewModel vm
            && ReferenceEquals(vm, _switcherViewModel)
            && e.PropertyName == nameof(AccountSwitcherViewModel.ErrorMessage))
        {
            RenderError(vm.ErrorMessage);
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        FaunaApp.Core.Helpers.AftermathProgress.Changed -= AftermathProgress_Changed;
        FaunaApp.Core.Helpers.InheritedFilterMarks.Changed -= AftermathProgress_Changed;
        if (_switcherViewModel is not null)
        {
            _switcherViewModel.PropertyChanged -= SwitcherViewModel_PropertyChanged;
        }
        _switcherViewModel = null;
        _switcherRegistry?.Dispose();
        _switcherRegistry = null;
        _push = null;

        // The minted secret must not survive the view that displayed it, and the two
        // typed buffers are a recovery phrase and a confirm token
        // (identity-succession.md § The RecoveryKey — Custody).
        if (_recoveryKit is not null)
        {
            _recoveryKit.PropertyChanged -= RecoveryKit_PropertyChanged;
            _recoveryKit.ClearHeldSecrets();
            _recoveryKit = null;
        }
        _recoveryKitHydratedActor = null;
        RecoveryPhraseBox.Text = string.Empty;
        StolenConfirmBox.Text = string.Empty;
        FaunaApp.Helpers.QrPainter.Clear(MintedKitQrCanvas);

        // AFTER the discharge above: leaving Account is the persist-failure
        // message's acknowledgment, and the held-back supersession follows it.
        LeaveAccountMount();
    }

    // --- The stolen ceremony's hold (StolenCeremonyHold) ---
    //
    // The hold counts Account mounts: while one is on screen a non-adopting outcome
    // keeps the supersession the ceremony caused held back, and the last one leaving
    // performs it (settings.md § Recovery kit → *The persist-failure message
    // survives the page*). Counted off BOTH edges each way — the navigation edge and
    // the visual-tree edge, once per page instance — because neither alone is
    // complete: this page lives in the Settings shell's inner frame, so an outer
    // navigation (Settings re-selected) unloads it with no OnNavigatedFrom, which
    // leaked a mount and stranded the escalation on the outcome-17 journey
    // (measured 2026-09-27).

    private bool _accountMounted;

    private void EnterAccountMount()
    {
        if (_accountMounted) return;
        _accountMounted = true;
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] mount enter");
        StolenCeremonyHold.Shared.AccountAppeared();
    }

    private void Page_Unloaded(object sender, RoutedEventArgs e) => LeaveAccountMount();

    /// <summary>Queued, not awaited: the escalation re-roots the root frame, which
    /// must not run inside this navigation's own callback.</summary>
    private void LeaveAccountMount()
    {
        if (!_accountMounted) return;
        _accountMounted = false;
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] mount leave");
        DispatcherQueue?.TryEnqueue(async () =>
        {
            try
            {
                await StolenCeremonyHold.Shared.AccountLeftAsync();
            }
            catch (Exception ex)
            {
                FaunaApp.Core.Logs.ShellLog.Error(
                    "SettingsAccountPage", $"held-back supersession escalation failed: {ex.Message}");
            }
        });
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] loaded enter");
        EnterAccountMount();
        // The aftermath's progress lines paint from the process-wide store the
        // sink feeds — independent of the nest-backed load below, and live: a
        // leg that reports while the page is open repaints it. -= first so a
        // re-Loaded page never subscribes twice.
        FaunaApp.Core.Helpers.AftermathProgress.Changed -= AftermathProgress_Changed;
        FaunaApp.Core.Helpers.AftermathProgress.Changed += AftermathProgress_Changed;
        FaunaApp.Core.Helpers.InheritedFilterMarks.Changed -= AftermathProgress_Changed;
        FaunaApp.Core.Helpers.InheritedFilterMarks.Changed += AftermathProgress_Changed;
        RenderAftermathProgress();
        // Re-read the inherited-filter marks on every Account entry (linux's
        // Privacy nav-edge re-read, web's tick): a Keep answered on the Email
        // Filters list — or on another device — must clear the line here.
        // Fire-and-forget; Changed repaints when it lands.
        if (_rpc is { } rpc) _ = FaunaApp.Core.Helpers.InheritedFilterMarks.RefreshAsync(rpc);
        // Populate the switcher first and independently of _viewModel: the account
        // list is local (the credential registry), so it must render even when the
        // nest-backed settings load fails or the session is offline.
        _switcherViewModel?.Refresh();

        // The push toggle paints from the install's stored bit (local, like the
        // switcher), and its line from the agent's current answer.
        if (_push is { } push)
        {
            push.Load();
            RenderPush();
            await push.RefreshLineAsync();
            RenderPush();
        }

        if (_viewModel is null) return;

        await _viewModel.LoadCommand.ExecuteAsync(null);
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] after LoadCommand");

        HandleText.Text = _viewModel.Handle ?? "--";
        NestUrlText.Text = _viewModel.NestUrl ?? "--";

        await HydrateRecoveryKitAsync();
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] loaded exit");

        // Hydrate on nav to the Account page (settings.md § Pending actions).
        // Fire-and-forget, matching web's own `void loadPendingActions()` —
        // the section's un-hydrated bare-title state is a deliberately
        // tolerated honest reading while this is still in flight, so
        // Page_Loaded must not block returning (and stalling the rest of
        // this page's readiness) on it.
        _ = _viewModel.LoadPendingActionsCommand.ExecuteAsync(null);
    }

    // --- Push notifications (settings.md § Push notifications) ---

    private async void PushOptInToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_renderingPush || _push is not { } push) return;
        if (push.Busy || PushOptInToggle.IsOn == push.OptedIn)
        {
            // A click while a toggle is still in flight, or one that changes nothing:
            // the stored bit is what the toggle shows.
            RenderPush();
            return;
        }
        await push.SetOptInAsync(PushOptInToggle.IsOn);
        RenderPush();
    }

    /// <summary>Paint the toggle from the stored bit — its HelpText carries the same
    /// answer for the e2e <c>state</c> read — and the line only when it has text.</summary>
    private void RenderPush()
    {
        if (_push is not { } push) return;
        _renderingPush = true;
        try
        {
            PushOptInToggle.IsOn = push.OptedIn;
        }
        finally
        {
            _renderingPush = false;
        }
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            PushOptInToggle, push.OptedIn ? "on" : "off");
        PushNotificationsErrorText.Text = push.LineText ?? string.Empty;
        PushNotificationsErrorText.Visibility = string.IsNullOrEmpty(push.LineText)
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    // --- Recovery kit (settings.md § Recovery kit) ---
    //
    // The RecoveryKey's Settings home. Every judgment this surface needs is already
    // shared: the status line's state and each action's enablement come from
    // `fauna_client_recovery::status` through libs/fauna-ffi's FfiRecoveryKitStatus,
    // and the succession's whole outcome from ONE export. Nothing here decides
    // anything — the code below paints and gates, and that is all.
    //
    // Reference impls: apple's FaunaKit/Views/RecoveryKitSection.swift (the same
    // FFI-mediated shape windows takes) and its RecoveryKitVM; tui's
    // settings/mod.rs and web's fauna-wasm/src/succession.rs drive a wider seam and
    // are NOT the model to copy here.

    /// <summary>
    /// Read the chain, paint the section, then discharge an owed successor kit if
    /// this session is the one that owes it.
    ///
    /// <para><b>Order is load-bearing.</b> The status read first, so an ordinary
    /// visit paints its state before anything else; the discharge second, because it
    /// mints — and the mint re-reads the status itself, so the section ends on the
    /// successor's <i>new</i> state rather than the never-created one it opened on.
    /// </para>
    /// </summary>
    private async System.Threading.Tasks.Task HydrateRecoveryKitAsync()
    {
        if (_recoveryKit is null) return;

        // An identity change first, before anything reads or paints: a kit minted for
        // the account we just left must not linger on screen for the one we just
        // entered (the shown-once custody rule does not stop at a switch), and the
        // previous actor's status is not a weaker answer for this one — it is the
        // wrong answer, and every action's enablement hangs off it.
        var signedIn = _sessionActorIdHex;
        if (_recoveryKitHydratedActor is { } previous && previous != (signedIn ?? "-"))
        {
            _recoveryKit.ResetForIdentityChange();
        }
        _recoveryKitHydratedActor = signedIn ?? "-";

        // Mirrored, not consumed: SuccessionHandoff.Sweep outlives every hydrate
        // (cleared only on a credential-namespace wipe), so re-reading it here on
        // an ordinary re-hydrate is idempotent — it only ever changes via Record
        // or a successful sweep retry, both of which this mirror must reflect.
        _recoveryKit.HydrateSweep();
        FaunaApp.Core.Logs.E2eTrace.Write(
            $"[account-page] before LoadStatusAsync signedIn={(signedIn is null ? "<null>" : "set")}");
        await _recoveryKit.LoadStatusAsync();
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] before DischargeOwedSuccessionKitAsync");
        RenderRecoveryKit();
        // A relaunch adoption's owed sweep first — the ceremony's own order (sweep,
        // then kit). A no-op on every other hydrate.
        await _recoveryKit.DischargeOwedSweepAsync(signedIn);
        await _recoveryKit.DischargeOwedSuccessionKitAsync(signedIn);
        FaunaApp.Core.Logs.E2eTrace.Write("[account-page] after DischargeOwedSuccessionKitAsync");
    }

    /// <summary>The sink fires on the FFI's runtime thread — marshal to ours.</summary>
    private void AftermathProgress_Changed() =>
        DispatcherQueue?.TryEnqueue(RenderAftermathProgress);

    /// <summary>
    /// Paint the post-succession aftermath's progress lines (settings.md § Recovery
    /// kit → <i>The post-succession aftermath's progress lines</i>): each line is
    /// the shared projection's already-resolved text, Collapsed while its leg has
    /// nothing to say — never a C# match on the leg (priorities #1/#3).
    /// </summary>
    private void RenderAftermathProgress()
    {
        Paint(AftermathBackupRegrantText, FfiAftermathLeg.BackupRegrant);
        Paint(AftermathMlsResealText, FfiAftermathLeg.MlsReseal);
        Paint(AftermathGrantRemintText, FfiAftermathLeg.GrantRemint);
        Paint(AftermathDraftsResealText, FfiAftermathLeg.DraftsReseal);
        Paint(AftermathMailBurnText, FfiAftermathLeg.MailBurn);

        var inherited = FaunaApp.Core.Helpers.InheritedFilterMarks.Count;
        InheritedFiltersText.Text = inherited == 0
            ? string.Empty
            : S.Format("settings/recovery_kit/inherited_filters", inherited);
        InheritedFiltersText.Visibility = inherited == 0 ? Visibility.Collapsed : Visibility.Visible;

        static void Paint(TextBlock target, FfiAftermathLeg leg)
        {
            var line = FaunaApp.Core.Helpers.AftermathProgress.Line(leg);
            target.Text = line is null ? string.Empty : S.Resolve(line);
            target.Visibility = line is null ? Visibility.Collapsed : Visibility.Visible;
        }
    }

    private void RecoveryKit_PropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_recoveryKit is null) return;
        if (e.PropertyName == nameof(RecoveryKitViewModel.ErrorMessage))
        {
            RenderError(_recoveryKit.ErrorMessage);
            return;
        }
        RenderRecoveryKit();
    }

    /// <summary>
    /// Paint the section from the VM's state. Enablement comes from the shared
    /// predicates ONLY — never from <c>Status.kind</c>, which would get two of them
    /// wrong (<c>allowsStolen</c> is unconditionally true; <c>allowsReplace</c> stays
    /// true during a pending window).
    /// </summary>
    private void RenderRecoveryKit()
    {
        if (_recoveryKit is null) return;

        RenderSweepChrome();

        // A parked persist-failure message is the ONLY surviving copy of the
        // successor's new key — closing it via the InfoBar's own X would hide it
        // for the rest of the visit with no way back (RenderError's guard refuses
        // every later write while pending, so nothing could reopen it), so the
        // close affordance is withheld while a message is parked.
        ErrorBar.IsClosable = !_recoveryKit.StolenPersistFailurePending;

        RecoveryKitStatusText.Text = _recoveryKit.StatusLine;

        var status = _recoveryKit.Status;
        var busy = _recoveryKit.Busy;
        RecoveryCreateButton.IsEnabled = status is { allowsCreate: true } && !busy;
        RecoveryReplaceButton.IsEnabled = status is { allowsReplace: true } && !busy;
        RecoveryLostButton.IsEnabled = status is { allowsLost: true } && !busy;

        // Both are affordances FOR A STATE — Collapsed elsewhere, which is what
        // `optional_elements` means for these two ids (FlaUI does not count a
        // Collapsed element, so "not applicable" reads as absent, not as disabled).
        RecoveryEscrowResealButton.Visibility =
            BoolToVisibility(status is { allowsEscrowReseal: true });
        RecoveryEscrowResealButton.IsEnabled = !busy;
        RecoveryPendingVetoButton.Visibility =
            BoolToVisibility(status?.pendingLandsAt is not null);
        RecoveryPendingVetoButton.IsEnabled = !busy;

        RecoveryPhraseBox.Visibility = BoolToVisibility(_recoveryKit.PhraseFieldVisible);

        // The trigger renders in EVERY state, and also while the status is still
        // unread — see RecoveryKitViewModel.StolenVisible for why that is on the
        // merits rather than a shortcut. Its ONE gate is the confirm field beside it.
        StolenActionPanel.Visibility = BoolToVisibility(_recoveryKit.StolenVisible);
        IdentityStolenButton.IsEnabled = _recoveryKit.StolenArmed;

        RenderMintedKit();
    }

    /// <summary>
    /// Paint the post-succession sweep's own lines + retry button, at the top of
    /// the section (<c>settings.md</c> § Recovery kit → <i>The sweep's own
    /// lines</i>). Selected off the carried view by the view model's
    /// <c>SweepOutcomeLine</c>/<c>SweepUnattestedLine</c> — never matched on
    /// <c>Kind</c> here.
    /// </summary>
    private void RenderSweepChrome()
    {
        if (_recoveryKit is null) return;

        var hasSweep = _recoveryKit.SweepView is not null;
        SweepChromePanel.Visibility = BoolToVisibility(hasSweep);
        if (!hasSweep) return;

        var outcome = _recoveryKit.SweepOutcomeLine;
        SweepOutcomeText.Text = outcome ?? string.Empty;
        SweepOutcomeText.Visibility = BoolToVisibility(outcome is not null);

        var unattested = _recoveryKit.SweepUnattestedLine;
        SweepUnattestedText.Text = unattested ?? string.Empty;
        SweepUnattestedText.Visibility = BoolToVisibility(unattested is not null);

        RecoveryKitSweepRetryButton.Visibility = BoolToVisibility(_recoveryKit.SweepOwesWork);
        RecoveryKitSweepRetryButton.IsEnabled = !_recoveryKit.Busy;
    }

    private async void RecoveryKitSweepRetryButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        await vm.RetrySweepAsync();
    }

    /// <summary>Show a freshly minted secret — once. Painted the moment it exists,
    /// because at that instant it exists nowhere else in the world and a ceremony
    /// whose kit is never shown leaves a kit nobody holds.</summary>
    private void RenderMintedKit()
    {
        var secret = _recoveryKit?.MintedSecretHex;
        if (string.IsNullOrEmpty(secret))
        {
            MintedKitPanel.Visibility = Visibility.Collapsed;
            MintedSecretText.Text = string.Empty;
            FaunaApp.Helpers.QrPainter.Clear(MintedKitQrCanvas);
            return;
        }

        MintedNoEscrowWarning.Visibility =
            BoolToVisibility(_recoveryKit?.MintedEscrowStored == false);
        MintedKitPanel.Visibility = Visibility.Visible;

        // Already painted — re-drawing the QR on every property change would redraw
        // ~1000 Rectangles per keystroke elsewhere on the page.
        if (MintedSecretText.Text == secret) return;

        MintedSecretText.Text = secret;
        try
        {
            // The secret itself, over the SHARED fauna_core qr grid — never a
            // platform/NuGet QR library (priorities #1/#2), and the same artifact
            // apple's section encodes (`qrMatrix(data: secret)`). The richer
            // `fauna://recovery` URI form is not reachable at this boundary:
            // FfiMintedKit carries the secret alone.
            FaunaApp.Helpers.QrPainter.Paint(MintedKitQrCanvas, secret);
        }
        catch (Exception ex)
        {
            // The QR is the convenience; the 64-hex above it is the kit. A QR that
            // could not be drawn must never take the secret off screen with it.
            FaunaApp.Core.Logs.ShellLog.Error(
                "SettingsAccountPage", $"recovery kit QR paint failed: {ex.Message}");
            FaunaApp.Helpers.QrPainter.Clear(MintedKitQrCanvas);
        }
        // A bare StartBringIntoView() in the same pass as MintedKitPanel's
        // Visibility flip above is a silent no-op — the panel has not been
        // measured yet, so it has no size or position to scroll to.
        // UpdateLayout() forces that pass first (mirrors PersonalizationPage's
        // own BringIntoViewAfterLayout / AtprotoPage's identical fix;
        // reference_windows_e2e_is_visible_offscreen). The deferred low-priority
        // retry is the same belt-and-suspenders those two use — this section
        // sits below the sweep chrome + status/description text, so on a
        // small e2e window the panel can start genuinely offscreen even after
        // the immediate scroll (measured — the
        // successor's owed-kit render read as "no kit on screen" though the
        // mint itself had already succeeded).
        MintedKitPanel.UpdateLayout();
        BringMintOutcomeIntoView();
        DispatcherQueue?.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            BringMintOutcomeIntoView);
    }

    /// <summary>
    /// Scroll a ceremony's outcome on screen — BOTH halves of it: the kit to write
    /// down, and the status line saying what the ceremony did to the account (a kit
    /// live now, or a 30-day window that is not).
    ///
    /// <para>Anchored on the status line, top-aligned, never on the secret alone:
    /// the status sits ABOVE the minted panel with only the description between
    /// them, so any scroll that stops at the secret's top edge leaves the status
    /// just outside the viewport. Measured
    /// after "I lost my kit": <c>recovery-kit-status</c> painted with the pending
    /// text but offscreen (empty frame), the secret flush with the viewport top.
    /// Status + description + secret is a few hundred DIPs, so all three land
    /// inside even the 600-DIP e2e window.</para>
    /// </summary>
    private void BringMintOutcomeIntoView() =>
        RecoveryKitStatusText.StartBringIntoView(
            new BringIntoViewOptions { VerticalAlignmentRatio = 0.0 });

    private void MintedSecretCopyButton_Click(object sender, RoutedEventArgs e)
    {
        var secret = _recoveryKit?.MintedSecretHex;
        if (string.IsNullOrEmpty(secret)) return;
        var package = new Windows.ApplicationModel.DataTransfer.DataPackage();
        package.SetText(secret);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(package);
    }

    private async void RecoveryCreateButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: false);
    }

    private async void RecoveryReplaceButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        vm.PhraseInput = RecoveryPhraseBox.Text;
        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: true);
        RecoveryPhraseBox.Text = vm.PhraseInput;
    }

    private async void RecoveryLostButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        await vm.RequestSeedAloneReplacementAsync();
    }

    private async void RecoveryEscrowResealButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        vm.PhraseInput = RecoveryPhraseBox.Text;
        await vm.ResealEscrowWithHeldKitAsync();
        RecoveryPhraseBox.Text = vm.PhraseInput;
    }

    private async void RecoveryPendingVetoButton_Click(object sender, RoutedEventArgs e)
    {
        if (_recoveryKit is not { } vm) return;
        vm.PhraseInput = RecoveryPhraseBox.Text;
        await vm.VetoPendingReplacementAsync();
        RecoveryPhraseBox.Text = vm.PhraseInput;
    }

    /// <summary>The type-to-confirm gate, the twin of
    /// <see cref="DeleteConfirmBox_TextChanged"/>: <c>identity-stolen-button</c> stays
    /// disabled until this reads exactly <c>SUCCEED</c>. The token is NEVER localized
    /// — only its prompt is — so this comparison is the same in every language.</summary>
    private void StolenConfirmBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_recoveryKit is null) return;
        _recoveryKit.StolenConfirmInput = StolenConfirmBox.Text;
        IdentityStolenButton.IsEnabled = _recoveryKit.StolenArmed;
    }

    /// <summary>
    /// The irreversible ceremony. On the persisted arm the account now belongs to a
    /// new identity and this session's bearers were revoked inside the nest's own
    /// transaction, so the app switches to the successor. ⚠ On the persist-failure
    /// arm it does NOT: tearing the session down there takes the only copy of the
    /// successor seed with it, so the VM puts the secret on screen instead.
    /// </summary>
    private async void IdentityStolenButton_Click(object sender, RoutedEventArgs e)
    {
        // Held in a LOCAL across the await, never re-read off the field: on the
        // persisted arm the switch below re-roots the frame, so OnNavigatedFrom has
        // already run — and nulled `_recoveryKit` — by the time this resumes.
        if (_recoveryKit is not { } vm) return;
        vm.PhraseInput = RecoveryPhraseBox.Text;
        vm.StolenConfirmInput = StolenConfirmBox.Text;
        await vm.SucceedWithHeldKitAsync(
            _sessionActorIdHex,
            successorActorIdHex =>
                App.SwitchAccountHandler?.Invoke(successorActorIdHex, false)
                ?? System.Threading.Tasks.Task.CompletedTask);
        RecoveryPhraseBox.Text = vm.PhraseInput;
        StolenConfirmBox.Text = vm.StolenConfirmInput;
    }

    /// <summary>Tapping a row switches to that identity (live in-session reconnect).
    /// The VM guards the already-active case, so tapping the active row is a no-op.</summary>
    private async void AccountRow_Click(object sender, RoutedEventArgs e)
    {
        if (_switcherViewModel is null) return;
        if (sender is FrameworkElement { Tag: string actorId })
        {
            await _switcherViewModel.SwitchToAsync(actorId);
        }
    }

    /// <summary>The per-row <c>account-require-confirm-toggle</c> (Stage 2). Writes the flag
    /// through the VM; setting it never prompts (only activating a flagged account does).
    ///
    /// <para><b>Render-echo guard.</b> Binding <c>IsOn</c> from the (immutable) row record on
    /// every reconcile re-raises <c>Toggled</c> with no user input — and on the initial render
    /// of an already-flagged row. Only write when the switch state actually differs from the
    /// row's stored flag, so an echo is a no-op and there is no write→refresh→echo loop
    /// (reference_windows_flaui_state_attr_helptext — "guard checkbox render-echo").</para>
    /// </summary>
    private void RequireConfirm_Toggled(object sender, RoutedEventArgs e)
    {
        if (_switcherViewModel is null) return;
        if (sender is not ToggleSwitch { Tag: string actorId } sw) return;

        var row = _switcherViewModel.Accounts.FirstOrDefault(r => r.ActorId == actorId);
        if (row is null || row.RequireConfirmToActivate == sw.IsOn) return;

        _switcherViewModel.SetRequireConfirm(actorId, sw.IsOn);
    }

    /// <summary>Remove a non-active account from this install. Does NOT switch, so the
    /// list must refresh in place — the ObservableCollection is what delivers that.</summary>
    private void AccountRemove_Click(object sender, RoutedEventArgs e)
    {
        if (_switcherViewModel is null) return;
        if (sender is FrameworkElement { Tag: string actorId })
        {
            try
            {
                _switcherViewModel.Remove(actorId);
            }
            catch (System.Exception ex)
            {
                RenderError(ex.Message);
            }
        }
    }

    /// <summary>"Add account" → append-mode onboarding over the LIVE session.</summary>
    private void AccountAdd_Click(object sender, RoutedEventArgs e)
    {
        App.AddAccountHandler?.Invoke();
    }

    /// <summary>
    /// "Open in new window" → spawn a second app process BOUND to this row's account
    /// (account-scoping.md § Concurrent instances → the running instance's surface).
    /// This window stays on its own account throughout.
    ///
    /// <para>Nothing is pre-checked: the child decides its own binding outcome over
    /// <c>bind_account</c> + the instance-lock acquire, so there is one gate rather
    /// than two that can drift. Fire-and-forget, matching apple's
    /// <c>onOpenNewInstance</c> and linux's <c>spawn_bound_instance</c> — a spawn
    /// that cannot start the executable at all is an OS-level anomaly the spawner
    /// logs; anything the user can act on is reported by the child, in the child.</para>
    /// </summary>
    private void AccountOpenNewInstance_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string actorId })
        {
            Services.InstanceSpawner.Spawn(actorId);
        }
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(SettingsViewModel.Handle):
                HandleText.Text = _viewModel.Handle ?? "--";
                break;
            case nameof(SettingsViewModel.NestUrl):
                NestUrlText.Text = _viewModel.NestUrl ?? "--";
                break;
            case nameof(SettingsViewModel.ErrorMessage):
                RenderError(_viewModel.ErrorMessage);
                break;
            case nameof(SettingsViewModel.IsDeleting):
                UpdateDeleteButtonEnabled();
                DeleteAccountButton.Content = _viewModel.IsDeleting ? S.Get("events/deleting") : S.Get("settings/account_page/delete_account");
                break;
            case nameof(SettingsViewModel.PendingActions):
                RenderPendingActions();
                break;
        }
    }

    // --- Pending actions (settings.md § Pending actions) ---
    //
    // A STANDING section: the VM's PendingActions is null while un-hydrated
    // (bare title), an empty list once loaded with nothing scheduled, or the
    // counted rows — never a settled "nothing scheduled" claim before the
    // first list read lands.

    private void RenderPendingActions()
    {
        var rows = _viewModel?.PendingActions;
        PendingActionsTitleText.Text = rows switch
        {
            null => S.Get("settings/pending_actions/title"),
            { Count: 0 } => S.Get("settings/pending_actions/none_scheduled"),
            _ => S.Format("settings/pending_actions/title_count", rows.Count),
        };
        PendingActionsList.ItemsSource = rows;
    }

    /// One click, no confirm — cancelling is the safe direction. The VM
    /// always re-lists rather than splicing the row out locally.
    private async void CancelPendingAction_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is not FrameworkElement { Tag: long id }) return;
        await _viewModel.CancelPendingActionCommand.ExecuteAsync(id);
    }

    private async void ChangeHandleButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        var newHandle = NewHandleBox.Text.Trim();
        if (string.IsNullOrEmpty(newHandle)) return;

        // Handle changes ride `fauna.profile.handle.change` (the authenticated
        // bearer kind) via the VM command — a queued, cancellable pending action.
        // A format-invalid handle is rejected client-side by the VM's shared-
        // validator pre-check (no RPC) and surfaces in `error-message`; a *taken*
        // handle stays server-authoritative (settings.md § Where logic lives →
        // Handle change). HandleText is NOT updated here; it refreshes on the next
        // account load once the pending action executes (matching linux / apple).
        _viewModel.NewHandle = newHandle;
        await _viewModel.ChangeHandleCommand.ExecuteAsync(null);
        RenderError(_viewModel.ErrorMessage);
        if (_viewModel.ErrorMessage is null)
        {
            NewHandleBox.Text = string.Empty;
        }
    }

    // --- Data export (settings.md § Data export) ---
    //
    // The click dispatches the fetch+save into a task and returns immediately —
    // a failed export lands in error-message, never in the click itself
    // (e2e convention 6).

    private async void ExportDataButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.ExportAccountDataCommand.ExecuteAsync(null);
        RenderError(_viewModel.ErrorMessage);
    }

    /// <summary>Render the VM's <see cref="SettingsViewModel.ErrorMessage"/> onto
    /// the page-level <c>error-message</c> InfoBar + the always-realized FlaUI /
    /// state-protocol mirror TextBlock — the element <c>test_handle_change_validation</c>
    /// reads directly (mirrors <see cref="SettingsSubscriptionsPage"/> /
    /// AdminCalendarPage RenderError).
    ///
    /// <para>⚠ This is the ONE choke point for every writer of this shared
    /// widget — both view models' error properties AND the page's own direct
    /// calls (<see cref="AccountRemove_Click"/>,
    /// <see cref="IdentityExportShowQrButton_Click"/>) — which is exactly why the
    /// guard against clobbering a parked stolen-identity persist-failure message
    /// lives HERE rather than in <c>RecoveryKitViewModel.SetGuardedError</c>: a
    /// guard there alone would miss <see cref="SettingsViewModel"/>'s direct
    /// <c>ErrorMessage =</c> writes and these page-only calls, none of which goes
    /// through any view model's guarded setter (<c>docs/goal/ui/settings.md</c> §
    /// Recovery kit → <i>The persist-failure message survives the page</i>).
    /// </para>
    /// </summary>
    private void RenderError(string? msg)
    {
        if (!StolenCeremonyHold.Shared.Admits(msg)) return;
        var empty = string.IsNullOrEmpty(msg);
        ErrorBar.Message = msg ?? string.Empty;
        ErrorBar.IsOpen = !empty;
        ErrorTextMirror.Text = msg ?? " ";
        App.CurrentErrorMessage = empty ? null : msg;
    }

    /// <summary>Inline type-to-confirm (mirrors web/tui): the button stays disabled
    /// until the field reads exactly "DELETE" AND no delete is already in flight —
    /// no separate confirm dialog, matching every other app's single-click-after-
    /// typing shape (priority #1).</summary>
    private async void DeleteAccountButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.DeleteAccountCommand.ExecuteAsync(null);
    }

    private void DeleteConfirmBox_TextChanged(object sender, TextChangedEventArgs e) =>
        UpdateDeleteButtonEnabled();

    private void UpdateDeleteButtonEnabled()
    {
        var confirmed = DeleteConfirmBox.Text == "DELETE";
        DeleteAccountButton.IsEnabled = confirmed && _viewModel is { IsDeleting: false };
    }

    // --- Sign out ---
    //
    // Sign-out clears the local credentials (keyring) + state and re-roots
    // onboarding at identity_choice (settings.md § Account). The teardown +
    // navigation are App-level (rootFrame + SecretStore), so the confirm
    // invokes the static App.SignOutHandler registered in OnLaunched — the
    // same seam shape as FactoryResetReonboardHandler. The confirm itself is an
    // inline Collapsed panel (the windows destructive-confirm idiom) so the
    // sign-out-confirm-button id surfaces in UIA only while open.

    private void SignOutButton_Click(object sender, RoutedEventArgs e)
    {
        SignOutConfirmPanel.Visibility = Visibility.Visible;
        // Bring the just-revealed confirm into view — a control below the fold
        // fails is_visible (IsOffscreen) even when realized
        // (reference_windows_is_visible_offscreen). UpdateLayout() must run
        // BEFORE StartBringIntoView() — called in the same pass as the
        // Visibility flip above it is a silent no-op, since the panel has not
        // been measured yet (mirrors PersonalizationPage.BringIntoViewAfterLayout
        // / AtprotoPage's identical fix; same latent shape closed for
        // MintedSecretText below).
        SignOutConfirmPanel.UpdateLayout();
        SignOutConfirmButton.StartBringIntoView();
        DispatcherQueue?.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => SignOutConfirmButton.StartBringIntoView());
    }

    private void SignOutCancelButton_Click(object sender, RoutedEventArgs e)
    {
        SignOutConfirmPanel.Visibility = Visibility.Collapsed;
    }

    private void SignOutConfirmButton_Click(object sender, RoutedEventArgs e)
    {
        // Ask BEFORE anything tears down (account-scoping.md § Concurrent instances
        // → *An erase refuses while a sibling serves the account*): App.SignOutHandler's
        // own first statements already drop actor-scoped state and unprovision the
        // sync agent ahead of its erase, so the gate has to sit here, at the click,
        // never inside the handler. `_switcherRegistry` is built fresh in
        // OnNavigatedTo and is null only if this page was never navigated to — a
        // click could not have fired, so this degrades open rather than no-op.
        if (_switcherRegistry is { } registry)
        {
            var blocked = FaunaFfiMethods.SignOutBlocked(registry, AccountStateDir.Base, null, null);
            if (blocked is not null)
            {
                RenderError(S.Resolve(blocked.line));
                return;
            }
        }
        App.SignOutHandler?.Invoke();
    }

    // --- Identity export (settings.md § Identity export) ---
    //
    // Show/Hide toggle over the shared fauna_core::qr_matrix grid (never a
    // platform/NuGet QR library, priorities #1/#2) — the counterpart of
    // onboarding's identity_import step. Nothing persisted, no server call; pure
    // view state. Reference impls: apps/fauna-linux/src/settings/identity_export.rs
    // (draw_qr), apps/fauna-web's +page.svelte (inline SVG),
    // apps/fauna-android's IdentityExportSection.kt (Compose drawRect).

    private void IdentityExportShowQrButton_Click(object sender, RoutedEventArgs e)
    {
        if (IdentityExportQrCanvas.Visibility == Visibility.Visible)
        {
            // Hide: drop the drawn matrix so the secret's encoding isn't retained
            // on screen (or in the visual tree) any longer than it is shown.
            FaunaApp.Helpers.QrPainter.Clear(IdentityExportQrCanvas);
            IdentityExportQrCanvas.Visibility = Visibility.Collapsed;
            IdentityExportWarning.Visibility = Visibility.Collapsed;
            IdentityExportShowQrButton.Content = S.Get("settings/identity_export/show_qr");
            return;
        }

        try
        {
            if (string.IsNullOrEmpty(_identitySecretHex))
                throw new InvalidOperationException("no identity secret available to export");

            var uri = FaunaFfiMethods.IdentityQrEncode(_identitySecretHex, _identityHandle);
            FaunaApp.Helpers.QrPainter.Paint(IdentityExportQrCanvas, uri);
            IdentityExportQrCanvas.Visibility = Visibility.Visible;
            IdentityExportWarning.Visibility = Visibility.Visible;
            IdentityExportShowQrButton.Content = S.Get("settings/identity_export/hide_qr");
        }
        catch (Exception ex)
        {
            // Surfaced (never silently swallowed): a no-op click with zero feedback
            // is undebuggable both in production and in e2e — RenderError is this
            // page's existing error-message surface (ChangeHandleButton_Click's).
            RenderError(S.Error(ex));
        }
    }

}
