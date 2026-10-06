using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;
using uniffi.fauna_provisioning;
// `LocalizedText` and `NodeMode` are defined in the fauna-core crate's
// bindings (uniffi.fauna_core); the onboarding-machine bindings only
// reference them. Targeted aliases keep this consumer building without a
// broad namespace import.
using LocalizedText = uniffi.fauna_core.LocalizedText;
using NodeMode = uniffi.fauna_core.NodeMode;
// The onboarding launch glue (LoggedIn arm) seals the captured DNS credential
// via the shared DnsManagementMachine. Targeted aliases (not a broad
// `using uniffi.fauna_client_dns;`) so the wizard's many provisioning /
// onboarding types don't collide with same-named DNS types.
using DnsAction = uniffi.fauna_client_dns.DnsAction;
using DnsCredentialField = uniffi.fauna_client_dns.DnsCredentialField;
// Same reasoning as the DNS aliases above: the trust_prompt launch glue
// (LoggedIn arm) mints the one-tap default grant set via the shared
// LinkedNestsMachine — a targeted alias, not a broad `using`.
using LinkedNestsAction = uniffi.fauna_client_pair.LinkedNestsAction;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Cross-platform proxy over the shared UniFFI <see cref="OnboardingMachine"/>.
/// Lives in FaunaApp.Core (no Windows-specific types) so it can be shared
/// or unit-tested against. Per-stage views are constructed and selected
/// by the platform-specific OnboardingPage shell, which subscribes to
/// <see cref="INotifyPropertyChanged.PropertyChanged"/> and reads
/// <see cref="CurrentStep"/> on each notification.
///
/// All real wizard state lives in the Rust state machine; this class just
/// forwards snapshot getters and command invocations. Identity-confirm
/// wrappers commit the returned secret hex through the registry's shared
/// confirm-identity moment immediately, so a force-quit before the user
/// reaches nest_login still leaves a recoverable identity. Wizard scratchpad
/// persistence has been removed entirely (Rust state machine is in-memory).
///
/// Marked <c>internal</c> because the UniFFI-generated <see cref="OnboardingStep"/>,
/// <see cref="OnboardingMachine"/>, and <see cref="OnboardingObserver"/> types
/// are emitted as <c>internal</c>. FaunaApp.Core exposes its internals to the
/// FaunaApp WinUI assembly via <c>[InternalsVisibleTo("FaunaApp")]</c>, so the
/// shell can still consume this VM.
/// </summary>
internal partial class OnboardingViewModel : ObservableObject
{
    /// <summary>
    /// Singleton accessor used by the test agent to route
    /// <c>call_machine_method</c> bridge commands to the live
    /// <see cref="OnboardingMachine"/> instance. Set in the constructor;
    /// cleared on dispose-equivalent paths is a future cleanup. Per the
    /// target-state doc the bridge only needs to reach the machine while
    /// the wizard is on screen, so the lifetime matches the page's.
    /// </summary>
    internal static OnboardingViewModel? Current { get; private set; }

    /// <summary>
    /// Clears <see cref="Current"/> once this instance is no longer the active wizard
    /// (called from <c>OnboardingPage.OnNavigatedFrom</c> — every exit path, LoggedIn/
    /// append-switch/abandon-cancel alike, navigates the page away). Self-checked so an
    /// already-superseded instance (a fresh wizard constructed before the old page's
    /// navigated-from fires) can't clobber the new one. Without this, `Current` dangles
    /// at the torn-down VM after an append — latent today (nothing re-reads it post-exit)
    /// but exactly the shape that would make a future dropped test-agent command
    /// (routed via `Current`) look like a product bug instead of a stale reference.
    /// </summary>
    internal void ClearIfCurrent()
    {
        if (ReferenceEquals(Current, this)) Current = null;
    }

    private readonly OnboardingMachine _m;
    // The observer is held to keep its callback alive for the machine's lifetime;
    // not used directly here.
    private readonly OnboardingObserver _observer;
    // The multi-account registry — the ONLY long-term store
    // (long-term-store.md § Downgrade mirror + abandoned-append recovery, the
    // 2026-09-24 retirement of the single slot). Every wizard write is one of
    // its shared moments: ConfirmIdentity (moment 1, both modes),
    // PersistPendingInvite (the submit return), PersistAwaitingDns (the
    // deferred-DNS exit) and PersistLoggedIn (the LoggedIn terminal) —
    // onboarding.md § Long-term store contract. The identity the wizard is
    // acting as is read from the MACHINE (`_m.EffectiveSecret()`), never back
    // from the store.
    private readonly IFfiAccountRegistry _registry;
    // The install-scoped secret store `DeviceIdForActor` derives the named
    // sync-device-row id from (sync-agent-credentials.md § Credential model,
    // the RULED 2026-09-20 block). Windows has exactly one credential store —
    // `CredentialStore.Logical`, whose sign-out (`ClearAll()`) never names
    // `install/device_secret` and has no second wholesale sweep — so the shell
    // passes the SAME store it builds `_registry` over; no second store is
    // needed the way android's wholesale `SecureStorage.clear()` requires one.
    private readonly FfiSecretStore _installStore;
    // Previous tick's _m.Step(), used only to gate FetchRecoveryBoxes to
    // "once per entry into nest_recovery" (see the constructor bridge).
    private OnboardingStep? _lastObservedStep;

    /// <summary>
    /// True when this wizard instance is append-mode ("Add account" over a live
    /// session — long-term-store.md § Multi-account evolution) rather than fresh
    /// single-identity onboarding. Threaded in from <c>App.IsAppendingAccount</c>
    /// (FaunaApp.Core has no reference to the WinUI project, so the flag can't be
    /// read directly — same injection shape as the Stage-2 <c>ConfirmReauthHandler</c>
    /// seam). Only <c>identity_choice</c> renders a cancel affordance from it: every
    /// other stage's existing Back-button chain already retreats to identity_choice,
    /// so no other stage needs one.
    /// </summary>
    public bool IsAppendMode { get; }

    public OnboardingViewModel(
        OnboardingObserver observer, IFfiAccountRegistry registry,
        bool isAppendMode = false, FfiSecretStore? installStore = null)
    {
        _observer = observer;
        _registry = registry;
        // Defaulted (never null in production — OnboardingPage always passes
        // CredentialStore.Logical) so the many existing test constructions that
        // never reach WizardOutcome.LoggedIn don't all need a store fake.
        _installStore = installStore ?? new NullFfiSecretStore();
        IsAppendMode = isAppendMode;
        Current = this;
        // The constructor carries no provider base-URL override — E2E tests that
        // redirect VPS / DNS / nest-health calls to the Python fake_cloud go
        // through the call_machine_method bridge, which exists only in a
        // test-helpers build (testing.md convention 15).
        //
        // Constructed BEFORE the PropertyChanged bridge below (which now reads
        // _m.Step() for the box-list-fetch gate) is subscribed — OnboardingMachine::new
        // never fires the observer synchronously (it only stores it), so this
        // ordering is purely to keep _m definitely-assigned for the compiler's
        // nullable analysis of the closure, not a functional requirement.
        // `NewWithPersistence`, never the bare constructor: the wizard writes the
        // pending-provision slot BEFORE it calls `create_server`, so a quit or crash
        // between minting the claim code and claiming the box resumes from the app
        // alone instead of orphaning a box nobody can claim and a bill nobody can
        // stop (`onboarding.md` § 6 *The pending-provision slot*; custody precedes
        // dispatch). The bare form still compiles and still provisions — it just
        // silently loses that resume, which is why `provision_slot_wiring.rs` reads
        // this exact call site as a structural pin.
        //
        // The store comes off the registry the page already handed us
        // (`OnboardingPage.xaml.cs`), never a second `FfiAccountRegistry` minted
        // here: a registry built past the sanctioned accessor carries the wrong
        // mutation lock. One slot, one writer — the same shared
        // `fauna_client_accounts` implementation all seven apps use.
        _m = OnboardingMachine.NewWithPersistence(observer, registry.PendingProvisionStore());
        // onboarding.md § 3b-ter (windows leg): declare the capability so the machine routes into
        // trust_prompt after nat_mode_choice instead of exiting straight to
        // Done. Same capability-flag shape recovery_kit uses; unconditional, mirroring
        // tui's Wizard::new / linux's machine_glue::make_machine.
        _m.SetRendersTrustPrompt(true);
        // onboarding.md § 1 Identity (windows leg; tui led, linux + web built
        // 2026-09-26): windows renders the
        // recovery_kit offer and the recovery_entry restore, so a created identity
        // routes through the kit screen. Same pairing rule as the trust prompt:
        // this declaration and OnboardingPage's RecoveryKit arm land together, or
        // the wizard strands on a step with no view.
        _m.SetRendersRecoveryKit(true);

        // The observer impl is expected to be INotifyPropertyChanged; cast and
        // forward. (The Windows impl, NotifyObserver, satisfies this.) If a
        // future non-Windows platform supplies an observer that doesn't
        // implement INPC, the cast simply skips the bridge — that platform
        // will need its own change-notification wiring.
        if (observer is INotifyPropertyChanged inpc)
            inpc.PropertyChanged += (_, _) =>
            {
                // Any machine transition supersedes a client-side import-parse
                // error: the user advanced (or went Back), so a stale parse
                // message must not outlive it (the machine owns the error
                // surface again). Mirrors apple OnboardingVM.onMachineChanged.
                _importError = null;
                _cachedSnapshot = null;
                // Box-list fetch fires once per ENTRY into nest_recovery, not
                // once per tick — mirrors linux handle_change's `entered` gate
                // (views/onboarding/mod.rs). Comparing against the previously
                // observed step (rather than a page-visibility flag, which
                // windows has no equivalent of) means SetRecoveryBoxes's own
                // notification below can't re-enter this.
                var step = _m.Step();
                if (step == OnboardingStep.NestRecovery && _lastObservedStep != OnboardingStep.NestRecovery)
                {
                    FetchRecoveryBoxes();
                }
                _lastObservedStep = step;
                OnPropertyChanged(string.Empty);
            };

        // Cached delegates for GenericProviderForm — captured once so
        // {x:Bind ...} on the property reads back the same instance on
        // every observer tick (otherwise the form would rebuild and the
        // user would lose focus mid-typing).
        SetDnsCred = (id, v) => _m.SetDnsCred(id, v);
        GetDnsCred = id =>
        {
            var creds = _m.DnsConfig().@creds;
            return creds.TryGetValue(id, out var v) ? v : "";
        };

        // VPS counterparts — same caching rationale as the DNS pair above.
        SetVpsCred = (id, v) => _m.SetVpsCred(id, v);
        GetVpsCred = id =>
        {
            var creds = _m.VpsConfig().@creds;
            return creds.TryGetValue(id, out var v) ? v : "";
        };

        // Hosted-auth (bundled provider's device flow, onboarding.md § 4) —
        // same caching rationale as the pairs above. Label is resolved via
        // this app's own resw keys (provisioning/hosted_auth/*), not
        // HostedAuthButtonText's pre-localized Rust string, matching how
        // every other windows label goes through S.Get/S.Format. Begin
        // returns the verification_url to open (or null on failure — the
        // machine's own HostedAuthState.Failed already reflects it, nothing
        // to re-derive here) rather than opening the browser itself:
        // Windows.System.Launcher is a WinUI/UWP API this net10.0 Core
        // project can't reference, so the caller (FaunaApp.Controls.
        // GenericProviderForm, which can) does the open.
        HostedAuthLabelDns = id => ResolveHostedAuthLabel(CredentialForm.Dns, id);
        HostedAuthEnabledDns = id => _m.HostedAuthCanBegin(CredentialForm.Dns, id);
        HostedAuthRunDns = (id, openUrl) => RunHostedAuth(CredentialForm.Dns, id, openUrl);

        HostedAuthLabelVps = id => ResolveHostedAuthLabel(CredentialForm.Vps, id);
        HostedAuthEnabledVps = id => _m.HostedAuthCanBegin(CredentialForm.Vps, id);
        HostedAuthRunVps = (id, openUrl) => RunHostedAuth(CredentialForm.Vps, id, openUrl);
    }

    /// <summary>
    /// Map the machine's <see cref="HostedAuthState"/> to this app's own
    /// resw keys — Idle/Pending/Connected/Failed, mirroring tui's
    /// <c>hosted_auth_button_text</c> match one-to-one (onboarding.md § 4).
    /// </summary>
    private static string ResolveHostedAuthLabel(CredentialForm form, string fieldId, OnboardingMachine m)
        => m.HostedAuthState(form, fieldId) switch
        {
            uniffi.fauna_onboarding_machine.HostedAuthState.Pending p =>
                Strings.Format("provisioning/hosted_auth/pending", p.@userCode),
            uniffi.fauna_onboarding_machine.HostedAuthState.Connected =>
                Strings.Get("provisioning/hosted_auth/connected"),
            uniffi.fauna_onboarding_machine.HostedAuthState.Failed f =>
                Strings.Format("provisioning/hosted_auth/failed", f.@message),
            _ => Strings.Get("provisioning/hosted_auth/connect"),
        };

    private string ResolveHostedAuthLabel(CredentialForm form, string fieldId)
        => ResolveHostedAuthLabel(form, fieldId, _m);

    /// <summary>
    /// Begin the device flow; returns the URL to open, or null on failure
    /// (the machine's own <see cref="HostedAuthState.Failed"/> already
    /// reflects it via the next observer tick — nothing to surface here).
    /// </summary>
    /// <summary>
    /// The WHOLE `hosted-auth` device flow as one call (onboarding.md § 4):
    /// begin, hand the verification URL to <paramref name="openUrl"/> — the
    /// app's own open-URL affordance, since <c>Windows.System.Launcher</c> is a
    /// WinUI API this net10.0 Core project cannot reference — then poll until
    /// the token lands (or the attempt fails, which the machine's own
    /// <c>HostedAuthState.Failed</c> already carries, so there is nothing to
    /// re-derive or rethrow here).
    ///
    /// <para>⚠ Both phases are sequenced HERE, not in the view. They used to be
    /// two separate <c>DependencyProperty</c> delegates that
    /// <c>GenericProviderForm</c>'s Click handler called in order, which put
    /// the ordering, the null-checks and the second delegate's very binding on
    /// the far side of a UI-thread continuation and a foreground-stealing
    /// browser launch — and `test_bundled_provider.py --app windows` sat on
    /// `Pending` having issued NO token poll at all in four runs of five
    /// (2026-09-03), non-deterministically, while the identical journey passed
    /// on macOS/iOS. This method is reachable from a plain unit test
    /// (<c>OnboardingHostedAuthTests</c>) where the whole WinUI dispatcher and
    /// binding layer is out of the picture; the view is left with one delegate
    /// and no decisions, which is the windows pattern everywhere else.</para>
    /// </summary>
    private async Task RunHostedAuth(CredentialForm form, string fieldId, Action<string> openUrl)
    {
        // ⚠ The step markers are not debug leftovers. This flow has twice been
        // diagnosed the expensive way — from the OUTSIDE, by which provider-fake
        // endpoint stopped receiving requests — because nothing said which of its
        // four steps the app had actually reached, and each guess cost an ~18 min
        // e2e run. Every marker below distinguishes a state the outside view
        // cannot: "wait never entered" and "wait entered but issued nothing" look
        // identical at the fake. `ShellLog` tees into the e2e agent trace with
        // thread tags, so these also separate a blocked UI thread from an await
        // that never resumed. Redaction (observability.md): step names only — no
        // user code, no verification URL, no token.
        ShellLog.Info("Onboarding", $"hosted-auth {form}/{fieldId}: begin");
        HostedAuthPrompt prompt;
        try
        {
            prompt = await _m.HostedAuthBegin(form, fieldId);
        }
        catch (OnboardingException e)
        {
            // the field is already Failed; the button repaints from it
            ShellLog.Info("Onboarding", $"hosted-auth {form}/{fieldId}: begin refused ({e.GetType().Name})");
            return;
        }

        ShellLog.Info("Onboarding", $"hosted-auth {form}/{fieldId}: begun, opening browser");
        try { openUrl(prompt.@verificationUrl); }
        catch { /* no browser association is not a reason to abandon the poll */ }

        // ⚠ The pool counters are load-bearing diagnostics, not noise. The poll
        // that follows waits on a tokio timer whose wake resumes on the .NET
        // ThreadPool (the FFI layer awaits with ConfigureAwait(false) — see
        // native-async-execution.md § The rule). So the two ways this step can
        // stall — "the timer never woke us" and "it woke us and no pool thread
        // was free to run the next poll" — are invisible from outside and look
        // identical at the provider fake. A saturated pool shows up here as a
        // high pending count against a pegged thread count.
        ShellLog.Info("Onboarding",
            $"hosted-auth {form}/{fieldId}: polling for the token "
            + $"[pool threads={ThreadPool.ThreadCount} pending={ThreadPool.PendingWorkItemCount} "
            + $"completed={ThreadPool.CompletedWorkItemCount}]");
        try { await _m.HostedAuthWait(form, fieldId); }
        catch (OnboardingException e)
        {
            ShellLog.Info("Onboarding", $"hosted-auth {form}/{fieldId}: poll ended ({e.GetType().Name})");
            return;
        }
        ShellLog.Info("Onboarding",
            $"hosted-auth {form}/{fieldId}: connected "
            + $"[pool threads={ThreadPool.ThreadCount} pending={ThreadPool.PendingWorkItemCount}]");
    }

    // ── Stage routing ──
    /// <summary>
    /// The current Rust stage. Platform shells map this to the right view.
    /// </summary>
    public OnboardingStep CurrentStep => _m.Step();

    // ── Shared snapshot getters ──
    // A client-side import-parse error (set by ConfirmImportedIdentity before
    // the machine is touched) takes precedence over the machine's own error
    // surface, mirroring apple OnboardingVM.errorMessage. Cleared on the next
    // import attempt or any machine transition (see the constructor bridge).
    public string? ErrorMessage  => _importError ?? _m.ErrorMessage();
    public bool    HasError      => ErrorMessage is not null;
    public bool    IsLoading     => _m.IsLoading();

    // ── The sign-out residue surface ──
    private ISignOutResidueSurface? _signOutResidue;
    // One re-sweep at a time; a press that lands during one waits its turn.
    private readonly SemaphoreSlim _signOutResidueRetries = new(1, 1);

    /// <summary>
    /// What a sign-out's erase could not remove, while it still owes work — the
    /// whole state of <c>identity_choice</c>'s <c>sign-out-residue</c> view
    /// (<c>account-scoping.md</c> § Erasure follows scope → <i>the residue
    /// surface</i>). <c>null</c> is the clean outcome and the only one.
    ///
    /// <para>Handed in by whatever built this wizard instance: a sign-out or the
    /// unreadable-index start-over (<c>App.SignOutHandler</c>), or the
    /// signed-out launch's silent re-check
    /// (<c>App.DispatchLaunchSnapshotAsync</c>). <c>null</c> on every other
    /// entry, the "Add account" append included, which erases nothing.</para>
    ///
    /// <para>State the wizard's machine does not own, which is the point: the
    /// line used to ride <see cref="ErrorMessage"/> as a sticky fallback, because
    /// the machine's own error is re-read on every observer tick and wiped
    /// anything painted there once. Never cleared on display — until a re-sweep
    /// says otherwise the statement is still true.</para>
    /// </summary>
    internal ISignOutResidueSurface? SignOutResidue
    {
        get => _signOutResidue;
        set
        {
            _signOutResidue = value;
            OnPropertyChanged(string.Empty);
        }
    }

    /// <summary>The render gate for the whole <c>sign-out-residue</c> view.</summary>
    public bool HasSignOutResidue => _signOutResidue is not null;

    /// <summary><c>sign-out-residue-message</c>, or <c>null</c> when the view is absent.</summary>
    public string? SignOutResidueMessage => _signOutResidue?.Line;

    /// <summary>
    /// Remove Again (<c>sign-out-residue-retry-button</c>): re-sweep what the
    /// painted residue recorded, off the UI thread, and paint what is left —
    /// nothing, when the device is now clean.
    ///
    /// <para>Presses are <b>serialized, never dropped</b>: a press that lands
    /// while a re-sweep is running was made after whatever the user just fixed,
    /// so it waits and then runs over what that re-sweep left. That is why this
    /// is a plain method and the button stays enabled, rather than a
    /// <c>[RelayCommand]</c> whose in-flight guard would swallow it.</para>
    ///
    /// <para>A re-sweep that throws has said nothing about the disk, so the
    /// line on screen stands.</para>
    /// </summary>
    public async Task RetrySignOutResidueAsync()
    {
        await _signOutResidueRetries.WaitAsync();
        try
        {
            if (_signOutResidue is not { } residue) return;
            ISignOutResidueSurface? left;
            try
            {
                left = await Task.Run(residue.Retry);
            }
            catch (Exception e)
            {
                ShellLog.Warn("Onboarding", $"sign-out residue retry failed: {e.Message}");
                return;
            }
            SignOutResidue = left;
        }
        finally
        {
            _signOutResidueRetries.Release();
        }
    }

    /// <summary>
    /// Client-side parse error for the identity-import field, set when the
    /// shared <c>ParseIdentityImport</c> (<c>fauna_core::identity_qr</c>)
    /// rejects the input <em>before</em> the machine is touched (see
    /// <see cref="ConfirmImportedIdentity"/>). Takes precedence over the
    /// machine error in <see cref="ErrorMessage"/>; cleared on the next import
    /// attempt or any machine transition. Mirrors apple
    /// <c>OnboardingVM.importError</c> / linux's page-local error label.
    /// </summary>
    private string? _importError;

    // ── Shared commands ──
    [RelayCommand] private void Back() => _m.Back();
    [RelayCommand] private void ClearError() => _m.ClearError();

    // ── Identity stage ──
    /// <summary>
    /// The hex-encoded secret key generated by the machine's
    /// <c>BeginCreateIdentity</c> step. <c>null</c> until that step has run.
    /// Bound by the IdentityCreatedView's secret-key-display element.
    /// </summary>
    public string? GeneratedSecret => _m.GeneratedSecret();

    /// <summary>
    /// Two-way bound to the paste-secret-field on IdentityImportView. The
    /// machine validates the value when <see cref="ConfirmImportedIdentityCommand"/>
    /// runs; we only hold the raw input here.
    /// </summary>
    public string ImportedSecret
    {
        get => _importedSecret;
        set
        {
            if (_importedSecret == value) return;
            _importedSecret = value;
            OnPropertyChanged();
        }
    }
    private string _importedSecret = "";

    [RelayCommand] private void BeginCreateIdentity() => _m.BeginCreateIdentity();
    [RelayCommand] private void BeginImportIdentity() => _m.BeginImportIdentity();

    [RelayCommand]
    private void ConfirmGeneratedIdentity()
    {
        // Errors raised by the machine are surfaced to the UI via the observer
        // (HasError / ErrorMessage); we just swallow the exception so the
        // command doesn't propagate it as an unhandled WinUI exception.
        // The returned secret hex is committed to the registry immediately so
        // a force-quit before the user reaches nest_login still leaves a
        // recoverable identity (first-run mode; see CommitConfirmedIdentity).
        try
        {
            var secret = _m.ConfirmGeneratedIdentity();
            CommitConfirmedIdentity(secret, "generated");
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Moment 1's commit, through the SHARED confirm-identity moment
    /// (<c>fauna_client_accounts::persist_confirmed_identity</c>, the FFI's
    /// <c>ConfirmIdentity</c>) — <b>the one call every app's confirm arm makes,
    /// in both modes</b>. First run: the per-actor account is created, the
    /// secret READ BACK (a bare store write reports success on a keystore that
    /// kept nothing — at the one write whose silent failure destroys an
    /// account outright), activated, and the previous run's abandoned identity
    /// retracted. Append mode ("Add account" over a live session) writes
    /// NOTHING: the appended identity stays in the machine
    /// (<c>EffectiveSecret()</c>) until its own terminal — <c>App.xaml.cs</c>'s
    /// append completion — registers and switches, so an abandoned append can
    /// neither leave a half-account nor shadow the active one. The mode rule
    /// lives in shared Rust, not in an app-side <c>if (append)</c>
    /// (long-term-store.md § Downgrade mirror + abandoned-append recovery).
    /// Failure surfaces log-only, the cross-app convention for a failed store
    /// write; the wizard has already advanced either way.</summary>
    private void CommitConfirmedIdentity(string secret, string arm)
    {
        try
        {
            _registry.ConfirmIdentity(secret, IsAppendMode);
        }
        catch (Exception ex)
        {
            ShellLog.Error("OnboardingViewModel",
                $"[onboarding] persist_confirmed_identity ({arm}) failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Parse a pasted/scanned identity-import field through the shared
    /// <c>fauna_core::identity_qr</c> parser (UniFFI <c>ParseIdentityImport</c>)
    /// and commit it. Accepts the union of every app form — a bare 64-hex
    /// secret, the <c>fauna://identity?secret=&amp;handle=</c> query form, or the
    /// colon form — so all seven apps parse identical input from one crate
    /// instead of each hand-rolling the grammar (priority #2/#4;
    /// <c>onboarding.md</c> §1.identity_import).
    ///
    /// An empty list signals a parse failure → surface the localized
    /// <c>invalid_secret</c> via <see cref="_importError"/> <b>without touching
    /// the machine</b>, so the user sees the parse string rather than the
    /// machine's <c>errors.secret_key_invalid</c>. Otherwise pre-fill the handle
    /// the payload carried (when any) via <c>SetCurrentHandle</c> before
    /// validating + persisting the secret through the machine (the durable
    /// commit point). Mirrors apple <c>OnboardingVM.importIdentity</c> / linux
    /// <c>identity_import.rs</c> / android <c>IdentityImportVM.importIdentity</c>.
    /// </summary>
    [RelayCommand]
    private void ConfirmImportedIdentity()
    {
        _importError = null;
        var parts = FaunaFfiMethods.ParseIdentityImport(_importedSecret.Trim());
        if (parts.Length != 2)
        {
            _importError = Strings.Get("onboarding/identity_import/invalid_secret");
            OnPropertyChanged(string.Empty);
            return;
        }
        var handle = parts[1];
        if (!string.IsNullOrEmpty(handle))
            _m.SetCurrentHandle(handle);
        try
        {
            var validated = _m.ConfirmImportedIdentity(parts[0]);
            CommitConfirmedIdentity(validated, "imported");
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    // ── Recovery kit offer (onboarding.md § 1 Identity) ──
    // The screen after identity_created on the CREATE path — windows' leg of the
    // page tui led (linux: views/onboarding/recovery_kit.rs). It MINTS AND
    // DISPLAYS ONLY: no nest exists at this position, so registration + escrow run
    // at the LoggedIn terminal (QueueDeferredRecoveryKitRegistration). The machine
    // routes here only because the constructor declares SetRendersRecoveryKit.

    /// The minted RecoveryKey root, bare 64-hex — what a user copies onto paper.
    /// <c>null</c> outside the screen's lifetime.
    public string? RecoveryKitSecretHex => _m.RecoveryKitSecretHex();

    /// The display line: the secret, or the not-minted placeholder.
    public string RecoveryKitSecretText =>
        RecoveryKitSecretHex ?? Strings.Get("onboarding/recovery_kit/not_minted");

    /// The machine's ONE <c>fauna://recovery</c> URI — what the QR and the copy
    /// button both carry, never the bare hex (identity-succession.md § The
    /// RecoveryKey → <i>Which encoding each affordance carries</i>). Read at use
    /// time, never cached: it lives exactly as long as the machine holds the root.
    public string? RecoveryKitUri => _m.RecoveryKitUri();

    /// "I've saved it": keeps the root for the signed-in handoff.
    [RelayCommand] private void ConfirmRecoveryKit() => _m.ConfirmRecoveryKit();

    /// One click, never blocks onboarding; the minted root is dropped, so nothing
    /// registers at the handoff and Settings' never-created warning tells the truth.
    [RelayCommand] private void SkipRecoveryKit() => _m.SkipRecoveryKit();

    // ── Phrase-only identity restore (onboarding.md § 1 Identity) ──
    // Reached from identity_choice's restore-from-recovery-kit-button — distinct
    // from the lost-box NEST recovery beside it. Submit runs the shared
    // SubmitRecoveryEntry (the pre-identity escrow restore); what every refusal
    // SAYS is the shared RecoveryEntryOutcomeMessage table. linux:
    // views/onboarding/recovery_entry.rs.

    [RelayCommand] private void BeginRecoveryEntry() => _m.BeginRecoveryEntry();

    /// Two-way bound to <c>recovery-entry-phrase-field</c>; raw input, the
    /// ceremony parses it.
    public string RecoveryPhrase
    {
        get => _recoveryPhrase;
        set
        {
            if (_recoveryPhrase == value) return;
            _recoveryPhrase = value;
            OnPropertyChanged();
        }
    }
    private string _recoveryPhrase = "";

    /// Two-way bound to <c>recovery-entry-account-field</c> — a handle, needed
    /// only when the phrase names no account.
    public string RecoveryAccount
    {
        get => _recoveryAccount;
        set
        {
            if (_recoveryAccount == value) return;
            _recoveryAccount = value;
            OnPropertyChanged();
        }
    }
    private string _recoveryAccount = "";

    /// Enabled unconditionally (the async command itself refuses a second click
    /// while a restore is in flight): every way the input can be wrong is an
    /// answer the ceremony gives on <c>error-message</c> — an unparseable phrase
    /// and a missing account are refused locally, before anything is sent — so a
    /// disabled submit would make "why can't I click this?" the user's problem.
    [RelayCommand]
    private async Task SubmitRecoveryEntryAsync()
    {
        // The typed account rides on the machine's one account field — the one
        // handle_entry asks for next (tui's stash-then-forward shape).
        var typed = _recoveryAccount.Trim();
        if (typed.Length > 0) _m.SetCurrentHandle(typed);
        // No ConfigureAwait(false): the settle below mutates the machine the UI
        // observes (the WinUI off-thread-COMException rule).
        var outcome = await _m.SubmitRecoveryEntry(_recoveryPhrase);
        SettleRecoveryEntry(outcome);
    }

    /// Fold a restore's outcome back into the wizard. <c>internal</c> for the
    /// unit test, which drives every outcome without a nest.
    internal void SettleRecoveryEntry(RecoveryEntryOutcome outcome)
    {
        ShellLog.Info("Onboarding", $"recovery entry settled: {outcome.GetType().Name} at step {_m.Step()}");
        switch (outcome)
        {
            // Uniform with the launch flow's superseded refusal: the import
            // screen, carrying why.
            case RecoveryEntryOutcome.Superseded:
                _m.BeginImportIdentityWithReason(Strings.Get("onboarding/recovery_entry/superseded"));
                return;
            // The seed is back: commit it exactly as an import does, so a crash
            // before complete-login resumes at handle_entry.
            case RecoveryEntryOutcome.Restored:
            case RecoveryEntryOutcome.RestoredPredecessorsLost:
                if (_m.EffectiveSecret() is { Length: > 0 } secret)
                    CommitConfirmedIdentity(secret, "restored");
                break;
        }
        if (FaunaOnboardingMachineMethods.RecoveryEntryOutcomeMessage(outcome) is { } message)
            _m.SetErrorMessage(Strings.Resolve(message));
    }

    /// <summary>
    /// Pre-seed the wizard with a previously-saved identity so the user
    /// resumes onboarding without re-entering their secret. Called from
    /// the launch-flow code in App.xaml.cs when the long-term store has
    /// a secret but no nest URL (i.e. the user force-quit between
    /// confirm-identity and complete-login on a previous run).
    /// </summary>
    public void SeedIdentity(string secret) => _m.SeedIdentity(secret);

    /// <summary>
    /// The launch arm for a <b>succeeded</b> identity (identity-succession.md
    /// § Propagation → *Own device fleet*): "this identity was succeeded — import
    /// the new identity". The windows twin of apple's <c>SupersededLaunchRoute</c>,
    /// tui's <c>launch.rs</c> and linux's <c>main.rs</c> arms.
    ///
    /// <para><b>No new ui.yaml elements</b> — the affordance IS the existing import
    /// flow (page <c>identity_import</c>), the reason on its <c>error-message</c>.
    /// The reason goes through the machine (<c>BeginImportIdentityWithReason</c>:
    /// step and reason in one mutation), because <see cref="ErrorMessage"/> mirrors
    /// the machine's on every observer tick and would erase a view-local label on
    /// the very tick the transition fires.</para>
    ///
    /// <para><b>The first message deliberately does not name the successor.</b> The
    /// refusal's successor is only <i>claimed</i> until the registration chain proves
    /// it; presenting it as fact would make this client trust the nest as an
    /// authorizer. The claim goes to the log; the anonymous chain walk
    /// (<c>SuccessionResolveVerifiedSuccessor</c>) upgrades the message once it
    /// verifies a successor, and every failure leaves the claim-free message
    /// standing.</para>
    ///
    /// <para><b>Adopt instead when this device already holds the verified successor's
    /// key</b> — the state a lost succession reply leaves behind
    /// (<c>identity-succession.md</c> § Implementation status today, <i>a lost submit
    /// reply no longer destroys the account</i>): the ceremony persisted the successor
    /// without activating it, and its undecidable arm promised that reopening the app
    /// signs in as it. Then there is nothing to import. The shared
    /// <c>AdoptHeldSuccessor</c> decides (and records the succession link); this
    /// records the kit and the group sweep the adoption owes
    /// (<see cref="SuccessionHandoff.RecordRelaunchAdoption"/>) and hands the switch
    /// to <paramref name="adopt"/> — the app's own account switch, the one the
    /// ceremony's persisted arm ends in. Both halves are proofs, never claims: the
    /// successor is the chain's answer, and the key is one this device minted and
    /// kept. apple's <c>SupersededLaunchRoute</c> and tui's
    /// <c>App::adopt_held_successor</c> are the twins.</para>
    ///
    /// <para><paramref name="secretHex"/>/<paramref name="nestUrl"/> are the refused
    /// identity's registry session material — without them the claim-free message is
    /// final. <paramref name="adopt"/> null means no switch is available, so nothing
    /// is adopted. <paramref name="resolve"/> is a test seam; production passes
    /// null.</para>
    /// </summary>
    public async Task RouteSupersededRefusalAsync(
        string claimedSuccessor,
        string? secretHex,
        string? nestUrl,
        Func<string, Task>? adopt = null,
        Func<string, byte[], Task<string?>>? resolve = null)
    {
        // The refused identity — read before anything moves, as apple's route does.
        var predecessor = _registry.Active();
        ShellLog.Error("Onboarding",
            $"[launch-machine] this identity was succeeded (claimed successor {claimedSuccessor}) — routing to the identity-import flow");
        if (!string.IsNullOrEmpty(secretHex)) _m.SeedIdentity(secretHex);
        _m.BeginImportIdentityWithReason(Strings.Get("onboarding/launch/identity_superseded"));

        if (string.IsNullOrEmpty(secretHex) || string.IsNullOrEmpty(nestUrl))
        {
            ShellLog.Warn("Onboarding",
                "[launch] no session material for the refused identity — cannot verify the succession");
            return;
        }

        string? verified;
        try
        {
            // No ConfigureAwait(false): the upgrade below mutates the machine the
            // UI observes, so the continuation stays on the caller's context.
            verified = await (resolve ?? FaunaFfiMethods.SuccessionResolveVerifiedSuccessor)(
                nestUrl, Convert.FromHexString(secretHex));
        }
        catch (Exception e)
        {
            // A malformed secret, or the walk itself failing: the claim-free message stands.
            ShellLog.Warn("Onboarding", $"[launch] could not verify the succession: {e.Message}");
            return;
        }
        if (verified is null) return;
        // The walk can land after the user navigated away; asserting a supersession
        // over whatever they are doing now would show a banner from a flow they have
        // already handled.
        if (_m.Step() != OnboardingStep.IdentityImport) return;
        if (adopt is not null && predecessor is not null
            && _registry.AdoptHeldSuccessor(predecessor, verified))
        {
            ShellLog.Info("Onboarding",
                $"[launch] this device holds the verified successor {verified}; adopting it");
            SuccessionHandoff.RecordRelaunchAdoption(predecessor, verified);
            await adopt(verified);
            return;
        }
        _m.BeginImportIdentityWithReason(
            Strings.Format("onboarding/launch/identity_superseded_verified", verified));
    }

    /// <summary>
    /// Surviving-device recovery entry from <c>launch-recover-button</c>
    /// (box-recovery.md § Recovery UI (step 4)): a superset of
    /// <see cref="SeedIdentity"/> — same imported secret, but flips
    /// <c>recovery_intent</c> and lands the step directly on
    /// <see cref="OnboardingStep.NestRecovery"/> — then pushes the box list
    /// the launch-time reachable-nest read (<c>App.xaml.cs</c>'s
    /// <c>DeploymentSeedCustody.LoadRecoverableBoxesAsync</c>, off the SAME
    /// saved nest) already resolved, so the page renders
    /// <c>recover-box-item</c> rows immediately with no further wait — unlike
    /// <see cref="FetchRecoveryBoxes"/>'s own re-read (which still fires on
    /// entry, since <c>_m.NestUrl()</c> is empty on this fresh
    /// recovery-scoped machine; harmless — an empty read never clobbers a
    /// list already on screen, see its ≥1-box guard).
    /// Mirrors linux's <c>on_recover</c> closure
    /// (<c>seed_identity_for_recovery</c> + <c>set_recovery_boxes</c>).
    /// </summary>
    public void SeedRecoveryFromLaunch(string secret, string[] boxes)
    {
        _m.SeedIdentityForRecovery(secret);
        if (boxes.Length > 0) _m.SetRecoveryBoxes(boxes);
    }

    /// <summary>
    /// Pre-loads a saved pending-invite slot. Call after
    /// <see cref="SeedIdentity"/> when the long-term store has a
    /// pending-invite record. Lands the wizard at
    /// <see cref="OnboardingStep.InviteRequest"/> with the snapshot
    /// hydrated. status_json is opaque on the client side — the wizard
    /// parses it back; on parse failure the wizard falls back to
    /// <c>PendingReview</c>.
    /// </summary>
    public void SeedPendingInvite(string nestUrl, string handle, string requestId, string statusJson)
        => _m.SeedPendingInvite(nestUrl, handle, requestId, statusJson);

    /// <summary>
    /// Land the wizard directly on <c>invite_request</c> for the given
    /// (nest_url, handle). Used by the launch flow when the silent
    /// challenge reports the secret is unregistered on a known nest
    /// (target spec §App-launch routing). Pure state mutation — does
    /// not perform any network IO; the user follows up by submitting
    /// an invite request from the page. Distinct from
    /// <see cref="SeedPendingInvite"/> which restores a previously
    /// submitted request from the long-term store.
    /// </summary>
    public void NavigateToInviteRequestForKnownNest(string nestUrl, string handle)
        => _m.NavigateToInviteRequestForKnownNest(nestUrl, handle);

    /// <summary>
    /// Land the wizard directly on <c>claim_code</c> for the given
    /// (nest_url, handle). Used by the launch flow when the silent
    /// challenge reports the secret is unregistered AND
    /// <c>setup-status.claimed == false</c> (target spec §App-launch
    /// routing — silent-challenge fallback table, unclaimed-nest row).
    /// Pure state mutation — does not perform any network IO; the user
    /// follows up by submitting the claim code from the page.
    /// </summary>
    public void NavigateToClaimCodeForKnownNest(string nestUrl, string handle)
        => _m.NavigateToClaimCodeForKnownNest(nestUrl, handle);

    /// <summary>
    /// Land the wizard directly on <c>claim_code</c> for (nest_url, handle)
    /// with the claim code <b>pre-filled</b>. Used by the factory-reset
    /// affordance: <c>fauna.admin.factory_reset</c> returns the post-reset claim
    /// code to the client (the human never sees it), so the re-onboard path must
    /// carry it into the wizard or the admin is stranded. The machine stashes it
    /// in <c>claim_code_prefill()</c>; <see cref="Views.Onboarding.ClaimCodeView"/>
    /// applies it to the (empty) input. Per
    /// <c>docs/goal/behavior/mail-bridge-lifecycle.md</c> § Factory reset and
    /// <c>onboarding.md</c> §3a.
    /// </summary>
    public void NavigateToClaimCodeForKnownNestWithCode(string nestUrl, string handle, string code)
        => _m.NavigateToClaimCodeForKnownNestWithCode(nestUrl, handle, code);

    /// <summary>The claim code to pre-fill the <c>claim-code-input</c> with on the
    /// factory-reset re-onboard path, or <c>null</c> on the ordinary claim path.
    /// Read reactively by <see cref="Views.Onboarding.ClaimCodeView"/>.</summary>
    public string? ClaimCodePrefill => _m.ClaimCodePrefill();

    /// <summary>
    /// Cross-app E2E bridge entry point. The Windows test agent
    /// receives <c>{action: "call_machine_method", method, json_arg}</c>
    /// from the FlaUI bridge and routes here. The shared
    /// <see cref="OnboardingMachine"/> has a `call_machine_method` of
    /// its own (under the <c>test-helpers</c> feature) that dispatches
    /// to the named setter in Rust. Per the target-state doc
    /// §"E2E bridge contract".
    ///
    /// <para><b>Debug-only</b> (testing.md convention 15): <c>call_machine_method</c>
    /// is a UniFFI seam the production FFI flavor does not export, so this
    /// forwarder cannot compile in Release. Its only caller is the equally
    /// Debug-only <c>Testing.TestAgent</c>.</para>
    /// </summary>
#if DEBUG || FAUNA_E2E_AGENT
    public void CallMachineMethod(string name, string jsonArg)
        => _m.CallMachineMethod(name, jsonArg);

    /// <summary>
    /// Value-returning twin of <see cref="CallMachineMethod"/>, for reader names
    /// (<c>provisioning_snapshot</c>, <c>provider_base_url</c>) that a test needs
    /// to poll live machine state through. Per the target-state doc §"E2E bridge
    /// contract" § Return values — mirrors linux/android/macOS/iOS, which already
    /// route this way and stash the result as <c>machine_method_result</c>.
    /// </summary>
    public string? CallMachineMethodWithResult(string name, string jsonArg)
        => _m.CallMachineMethodWithResult(name, jsonArg);

    /// <summary>
    /// Async twin of <see cref="CallMachineMethodWithResult"/>, and the one the
    /// E2E bridge routes through.
    ///
    /// <para>The sync dispatchers cannot <c>.await</c>, so every <b>async</b>
    /// machine method (<c>verify_dns</c>, <c>wizard_submit_claim_code</c>,
    /// <c>submit_nat_mode_choice</c>, …) falls into their silent <c>_</c> arm and
    /// <b>acks green having done nothing</b> — a convention-11 silent drop in
    /// effect, however much the call site looks like it honours the command.
    /// That is what kept the live Hetzner provisioning drive at
    /// <c>overall: 'Idle'</c> on macOS (measured on <c>mac</c> 2026-08-29:
    /// <c>verify_dns</c> never ran, every step <c>Pending</c> after the full
    /// 1200 s). Windows sat on the identical gap until this forwarder landed.</para>
    ///
    /// <para>Every non-async name delegates to the sync dispatcher inside Rust, so
    /// this is a strict superset of <see cref="CallMachineMethodWithResult"/> —
    /// ONE name table, in shared Rust, rather than a hand-written per-app copy
    /// (the reasoning <c>apps/fauna-tui/src/automation.rs</c> records).</para>
    /// </summary>
    public async System.Threading.Tasks.Task<string?> CallMachineMethodAsync(
        string name, string jsonArg)
        => await _m.CallMachineMethodAsync(name, jsonArg);

    /// <summary>
    /// The one arm the shared dispatcher deliberately does NOT own: provisioning
    /// spawns work that outlives the call (minutes — the driver polls
    /// <c>provisioning_snapshot</c> rather than blocking on the ack), so runtime
    /// ownership is per-app.
    ///
    /// <para>⚠ This routes to <c>RunProvisioning()</c>, NOT the same-named
    /// <c>StartProvisioning()</c>/<c>RetryProvisioning()</c> — see
    /// <see cref="ProvisioningStartAsync"/> for why the latter's bare Rust-side
    /// <c>tokio::spawn</c> is a panic hazard on the WinUI UI thread.
    /// <c>run_provisioning_inner</c> resets the snapshot + cancel flag at entry,
    /// so the same call serves <c>start_provisioning</c> and
    /// <c>retry_provisioning</c> alike.</para>
    /// </summary>
    public System.Threading.Tasks.Task RunProvisioningForTestAsync()
        => ProvisioningStartAsync();
#endif

    /// <summary>
    /// Set by the launch flow (via <c>ServiceClients.OnOnboardingCompleted</c>)
    /// so the wizard can hand off to MainPage when it terminates with
    /// <see cref="WizardOutcome.LoggedIn"/>. Null on seed-only / re-onboard
    /// paths where re-entering onboarding doesn't imply completion.
    /// </summary>
    public Action? OnLoggedIn { get; set; }

    /// <summary>
    /// The app shell's hook for "a pending-invite slot was just written" — the
    /// append-mode ("Add account") adoption seam for the pending-invite journey,
    /// windows twin of apple's <c>OnboardingVM.onPendingInvitePersisted</c>
    /// (<c>d42bb27654</c>). Set by <see cref="Views.OnboardingPage"/> from
    /// <c>ServiceClients.OnPendingInvitePersisted</c>, next to
    /// <see cref="OnLoggedIn"/> — this class has no reach to <c>App</c>.
    ///
    /// Fired by <see cref="PersistPendingInviteSlot"/> with the actor id the
    /// shared writer just registered <b>and activated</b>, unconditionally
    /// (append mode included — see that method's doc). Whether a call is an
    /// adoption is the shell's decision, not this class's: only when
    /// <c>App</c>'s own append latch is still set does the installed handler
    /// leave append mode and switch (<c>onboarding.md</c> § Multi-account:
    /// "the append glue adopts on the submit return … switch to it");
    /// everywhere else — first-run onboarding, a resumed non-append wizard —
    /// the call is a no-op.
    /// </summary>
    public Action<string>? OnPendingInvitePersisted { get; set; }

    /// <summary>
    /// The deferred-DNS twin of <see cref="OnPendingInvitePersisted"/>: fired by
    /// the <see cref="WizardOutcome.AwaitingManualDns"/> exit with the actor id
    /// <see cref="IFfiAccountRegistry.PersistAwaitingDns"/> just registered
    /// <b>and activated</b>, unconditionally, append mode included. It is the
    /// append's adoption seam for that exit (<c>onboarding.md</c> § Multi-account,
    /// *Append-mode deferred/incomplete states*). The shell
    /// installs the same handler as for the pending-invite seam, which switches
    /// only while its own append latch is set.
    /// </summary>
    public Action<string>? OnAwaitingDnsPersisted { get; set; }

    // ── Handle entry stage ──
    /// <summary>
    /// Two-way bound to the handle-input TextBox. Reads/writes through the
    /// machine so validation state (e.g. <see cref="HandleEntryContinueEnabled"/>)
    /// stays consistent with the Rust state. The observer fires
    /// <c>PropertyChanged</c> after <c>SetCurrentHandle</c> updates the
    /// machine, which refreshes derived properties like
    /// <see cref="HandleEntryContinueEnabled"/>.
    /// </summary>
    public string CurrentHandle
    {
        get => _m.CurrentHandle();
        set => _m.SetCurrentHandle(value);
    }

    // ── Handle-check snapshot rendering (Wave 3 / Spec 1) ──
    //
    // The shared OnboardingMachine drives handle_entry through
    // HandleCheckSnapshot { phase, outcome, message, continueEnabled,
    // controlCheckboxVisible, controlCheckboxChecked }. The view binds to
    // the derived properties below; pressing Check kicks off StartHandleCheck
    // and the observer fires PropertyChanged on every phase boundary so the
    // message panel updates as DNS → nest probe → challenge complete.

    /// <summary>
    /// Resolves the snapshot's <see cref="LocalizedText"/> message — looks
    /// up the dotted key in the platform's i18n table and substitutes
    /// <c>{name}</c> placeholders with snapshot args. Empty when the key is
    /// blank (initial Idle phase, etc.). Mirrors the contract in the
    /// target-state doc: "Localized strings come from LocalizedText".
    /// </summary>
    public string HandleCheckMessage
    {
        get
        {
            var snap = _m.HandleCheckSnapshot();
            return Strings.Resolve(snap.@message);
        }
    }

    public bool HandleCheckContinueEnabled => _m.HandleCheckSnapshot().@continueEnabled;
    public bool HandleControlCheckboxVisible => _m.HandleCheckSnapshot().@controlCheckboxVisible;

    public bool HandleControlCheckboxChecked
    {
        get => _m.HandleCheckSnapshot().@controlCheckboxChecked;
        set => _m.SetControlCheckbox(value);
    }

    /// <summary>
    /// Check button is disabled while the input is empty. The machine
    /// itself is cancellation-safe — re-pressing Check while a probe is in
    /// flight cancels and restarts.
    /// </summary>
    public bool HandleCheckEnabled => !string.IsNullOrEmpty(_m.CurrentHandle());

    [RelayCommand]
    private async Task HandleCheckAsync()
    {
        try { await _m.StartHandleCheck(_m.CurrentHandle()); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    [RelayCommand]
    private async Task HandleEntryContinueAsync()
    {
        try
        {
            var step = await _m.SubmitHandleCheckContinue();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>
    /// Routes on <see cref="OnboardingMachine.WizardOutcome"/> per the
    /// target-state doc's exit-routing table. Persists the appropriate
    /// long-term-store fields and triggers <see cref="_onCompleted"/>
    /// (set by the launch flow) so MainPage can take over for
    /// <c>LoggedIn</c>. <c>InviteSubmitted</c> writes the pending-invite
    /// slot; <c>AwaitingManualDns</c> stores the manual-DNS records for
    /// the post-provisioning surface.
    /// </summary>
    internal void HandleWizardOutcome()
    {
        var outcome = _m.WizardOutcome();
        if (outcome is null) return;
        switch (outcome)
        {
            case WizardOutcome.LoggedIn loggedIn:
            {
                // The terminal reads the secret from the MACHINE, never from the
                // store (onboarding.md § Long-term store contract, ratified
                // 2026-08-27): moment 1's write can silently not land, and a
                // machine-only drive (seed_identity) never runs it at all.
                var secretHex = _m.EffectiveSecret();
                // The named sync_devices row this identity registers under —
                // the persisted per-account slot when one exists, else derived
                // from the install-scoped secret (sync-agent-credentials.md §
                // Credential model, the RULED 2026-09-20 block).
                // `_registry.DeviceIdForActor` is the ONE call site every windows
                // device-id mint goes through — here, and the session-patch
                // door's non-injected arm (App.xaml.cs) — mirroring apple's
                // `FaunaAccounts.deviceId(forActorId:)` (priority #1/#2). For an
                // identity the registry does not hold yet (append mode) the id
                // comes back unpersisted and rides into the registration below.
                string? deviceId = null;
                string? actorId = null;
                if (!string.IsNullOrEmpty(secretHex))
                {
                    try
                    {
                        actorId = Convert.ToHexString(FaunaFfiMethods.ActorIdFromSecret(
                            Convert.FromHexString(secretHex))).ToLowerInvariant();
                        deviceId = _registry.DeviceIdForActor(_installStore, actorId);
                    }
                    catch (Exception ex)
                    {
                        // Extremely rare — only when the install secret could not
                        // be minted or read back. Never block onboarding on it:
                        // fall back to a one-off random id, matching this call
                        // site's behaviour before the derivation existed.
                        ShellLog.Error("OnboardingViewModel",
                            $"[onboarding] deviceIdForActor failed: {ex.GetType().Name}: {ex.Message}");
                        deviceId = Convert.ToHexString(FaunaFfiMethods.GenerateDeviceId()).ToLowerInvariant();
                    }
                }
                LoggedInNestUrl = loggedIn.@nestUrl;
                LoggedInDeviceId = deviceId;
                if (IsAppendMode)
                {
                    // Append mode is exempt from moment 4 (onboarding.md § Long-
                    // term store contract → *Append mode is exempt*): the append
                    // terminal (App.xaml.cs's completion) registers the identity
                    // and switches to it, so activating here would move `active`
                    // off the live account first. That terminal does not reach
                    // the shared helper, so the resume rows it would have spent
                    // are cleared explicitly at this same terminal — scoped to
                    // the ACTIVE account, which is the appended identity itself
                    // when its pending-invite submit already adopted it, and
                    // otherwise the live (authenticated, slot-free) account.
                    try
                    {
                        _registry.LaunchPersistence().DeletePendingInvite();
                        _registry.ClearAwaitingDns();
                    }
                    catch (Exception ex)
                    {
                        ShellLog.Error("OnboardingViewModel",
                            $"[onboarding] append-mode resume-slot clear failed: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                else if (!string.IsNullOrEmpty(secretHex))
                {
                    // Moment 4, the shared `persist_logged_in`: register the home
                    // nest PER-ACTOR (the only place it can be recorded — the next
                    // launch's silent-challenge row branches on it), activate, and
                    // spend the pending-invite and awaiting-DNS slots — the ruled
                    // clearing moment for both (ratified 2026-09-21), never
                    // earlier. The reach hint rides along for a box this wizard
                    // provisioned (onboarding-provisioning.md § Reach hint; null
                    // on every other path, which leaves the slot untouched).
                    try
                    {
                        _registry.PersistLoggedIn(
                            secretHex, loggedIn.@nestUrl, deviceId, _m.ProvisionReachIpv4());
                    }
                    catch (Exception ex)
                    {
                        ShellLog.Error("OnboardingViewModel",
                            $"[onboarding] persist_logged_in failed: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                else
                {
                    ShellLog.Error("OnboardingViewModel",
                        "[onboarding] LoggedIn with no effective secret on the machine");
                }
                // The predecessor seeds a phrase-only restore recovered from the
                // escrow blob's additive section — empty on every ordinary
                // onboarding, the only copies left anywhere when not
                // (identity-succession.md § Seed escrow). Linked to the restored
                // identity BY NAME: first-run, moment 4 above has just added and
                // activated it; in append mode the shell adds it after this
                // returns, and a live account already holds `active`, so these
                // rows cannot claim it first. Read off the machine before
                // OnLoggedIn tears it down.
                try
                {
                    _registry.PersistRestoredPredecessors(actorId, _m.RestoredPredecessors());
                }
                catch (Exception ex)
                {
                    ShellLog.Error("OnboardingViewModel",
                        $"[onboarding] persist_restored_predecessors failed: {ex.GetType().Name}: {ex.Message}");
                }
                // The kit the user confirmed on the recovery_kit screen, minted
                // there but deliberately unregistered until a signed-in connection
                // exists (identity-succession.md § The RecoveryKey → Creation UX).
                // Consume-once, read before OnLoggedIn; null if skipped.
                var pendingKit = _m.TakePendingRecoverySecret();
                if (pendingKit is not null && IsAppendMode)
                {
                    // tui's and linux's append handoffs drop it the same way: the
                    // kit registers only on a first-sign-in, and Settings says
                    // never-created.
                    ShellLog.Warn("OnboardingViewModel",
                        "[onboarding] add-account: the confirmed recovery kit is not registered on this path");
                }
                else if (pendingKit is not null && actorId is not null)
                {
                    QueueDeferredRecoveryKitRegistration(actorId, pendingKit);
                }
                // Claim terminal (gap CR-1, nest/common.md § Client-state
                // recoverability). A re-claim after a factory reset has landed, so the
                // pre-dispatch slot is spent. Leaving it set would pin every future
                // launch to the pre-filled claim surface for a box already re-claimed —
                // that row is evaluated before ALL the others. Addressed by the active
                // account, the same way the launch persistence reads it (moment 4
                // above has just activated this identity); a no-op when no reset was
                // pending (the ordinary first-onboarding path). Mirrors linux's
                // `clear_pending_factory_reset_slot`.
                try
                {
                    _registry.LaunchPersistence().DeletePendingFactoryReset();
                }
                catch (Exception ex)
                {
                    ShellLog.Error("OnboardingViewModel",
                        $"[onboarding] pending-factory-reset clear failed: {ex.GetType().Name}: {ex.Message}");
                }
                // Onboarding launch glue: seal the onboarding-captured DNS-provider
                // credential into the admin's fauna.state.dns now that an
                // authenticated identity exists. Fire-and-forget so it never blocks
                // the user's landing; the credential is read synchronously inside
                // (before OnLoggedIn tears the wizard down). Mirrors linux
                // launch_main_app_after_signin -> dns_put_credentials and web
                // sealCapturedDnsCredential. dns-management.md § Where the credential lives.
                if (actorId is not null) QueueCapturedDnsCredentialSeal(actorId);
                // onboarding.md § 3b-ter: the one-tap trust answer latched on the
                // trust_prompt page (windows leg). Same capture-before-adopt / fire-and-forget shape as
                // the DNS credential seal above.
                if (actorId is not null) QueueDefaultTrustSetMint(actorId);
                OnLoggedIn?.Invoke();
                break;
            }
            // ⚠ There is deliberately no InviteSubmitted case (retired
            // 2026-08-12). That exit fell back to the identity-choice page here
            // — one of the five divergent per-app behaviors onboarding.md
            // § Wizard exit handling deletes. The journey now has no exit: the
            // wizard stays on invite_request and polls, and the slot is written
            // at the submit return by PersistPendingInviteSlot().
            case WizardOutcome.AwaitingManualDns dns:
                // Persist the awaiting-manual-DNS slot per target doc
                // §"Wizard exit handling" so the "Almost ready" surface can
                // poll DNS resolution + claim completion across app
                // restarts. Mirrors Linux. Use the machine's
                // own JSON producer for the records — never hand-roll via
                // JsonSerializer over the UniFFI-bound DnsRecordPlain[]: its
                // fields are camelCase (recordType) while the seeder's serde
                // parse expects snake_case (record_type), so that
                // round-trips to an empty list (onboarding.md § "Almost
                // ready" surface, trap 2). handle is not in the outcome
                // payload — the machine is where the wizard put it.
                //
                // Deliberately does NOT SaveNestUrl here (trap 4): the nest
                // is neither claimed nor reachable yet, so caching it would
                // make the *next* launch's silent-challenge fast path
                // (WizardAt(HandleEntry)/Online routing) try a dead host
                // before AwaitingManualDns's own (earlier-checked) row gets
                // a chance — the bug that made web's relaunch silently
                // discard the nest.
                // The per-actor slot (onboarding.md § Long-term store contract)
                // through the shared writer — composes correctly even with an
                // EMPTY handle, which the deferred-DNS exit reaches with no
                // handle stage at all (identity → provisioning →
                // dns_post_instructions). Mirrors apple completeOnboarding's
                // .awaitingManualDns arm.
                //
                // Unconditional — append mode included:
                // this write IS the append's terminal for this exit
                // (long-term-store.md § Downgrade mirror + abandoned-append
                // recovery; tui persists here unconditionally too), so skipping
                // it left the appended identity's secret and the parked box's
                // claim code in process memory for the whole "Almost ready"
                // wait. PersistAwaitingDns registers AND activates, and
                // OnAwaitingDnsPersisted hands the actor to the shell, which
                // leaves append mode and switches — exactly the pending-invite
                // adoption (PersistPendingInviteSlot). The hijack the old skip
                // guarded against (an append parked here, then backed out to
                // identity_choice and cancelled over a moved active pointer)
                // has no path left: the exit is a wizard Done, which the shared
                // machine's back() never leaves, and the switch clears the
                // shell's append latch before the rebuilt wizard renders, so no
                // cancel affordance follows.
                {
                    // The identity the wizard is acting as comes from the
                    // MACHINE, never back from the store (the same rule the
                    // LoggedIn terminal above follows).
                    var secretHex = _m.EffectiveSecret();
                    if (!string.IsNullOrEmpty(secretHex))
                    {
                        try
                        {
                            var actorId = _registry.PersistAwaitingDns(
                                secretHex, dns.@nestUrl, _m.CurrentHandle(),
                                _m.AwaitingDnsRecordsJson(), dns.@claimCode);
                            OnAwaitingDnsPersisted?.Invoke(actorId);
                        }
                        catch (Exception ex)
                        {
                            ShellLog.Error("OnboardingViewModel",
                                $"[onboarding] registry PersistAwaitingDns failed: {ex.GetType().Name}: {ex.Message}");
                        }
                    }
                    else
                    {
                        ShellLog.Error("OnboardingViewModel",
                            "[onboarding] AwaitingManualDns exit with no effective secret on the machine");
                    }
                }
                break;
        }
    }

    /// <summary>
    /// Queue the seal of the onboarding-captured DNS-provider credential
    /// into the admin's tip-sealed <c>fauna.state.dns</c> record, via the
    /// post-onboarding <see cref="DnsAction.PutCredentials"/> path on the shared
    /// <c>DnsManagementMachine</c> — one store, one writer
    /// (<c>docs/goal/behavior/dns-management.md</c> § Where the credential lives;
    /// <c>onboarding.md</c> §4). <see cref="OnboardingMachine.CapturedDnsCredential"/>
    /// is non-null only on the managed-publish path where the DNS step verified a
    /// provider credential (null on the manual / set-up-later / returning-user
    /// paths). The credential is read <b>synchronously</b> here, before the caller
    /// fires <see cref="OnLoggedIn"/> (which tears the wizard down); the seal
    /// itself runs later on the session's own client (<see cref="PostSignInHandoff"/>)
    /// — the machine re-runs <c>verify()</c> against the provider API (seconds) to
    /// (re)derive the covered zones — so it never blocks the user's landing. A
    /// failure is logged, not surfaced: the admin can re-enter the credential on
    /// <c>admin-dns</c>. Mirrors linux
    /// <c>launch_main_app_after_signin -> dns_put_credentials</c> and web
    /// <c>sealCapturedDnsCredential</c>.
    /// </summary>
    private void QueueCapturedDnsCredentialSeal(string actorId)
    {
        // Read before the first await — OnLoggedIn (fired by the caller right
        // after this method yields) tears the wizard machine down.
        var cred = _m.CapturedDnsCredential();
        if (cred is null) return;

        // The only transform is map→array: SecretString is a `string` alias on
        // both the captured shape and DnsCredentialField, so the entered secrets
        // ride verbatim into PutCredentials (the machine re-derives the covered
        // zones via verify(), so the captured zones are not re-surfaced).
        var providerId = cred.@providerId;
        var label = cred.@label;
        var fields = cred.@fields
            .Select(kv => new DnsCredentialField(kv.Key, kv.Value))
            .ToArray();

        // Best-effort, off the landing path: a failure is logged by the hand-off,
        // and the admin re-enters on admin-dns.
        PostSignInHandoff.Enqueue(actorId, "DNS credential seal", async rpc =>
        {
            using var machine = await rpc.BuildDnsManagementMachineWithCredentialsAsync();
            // Dispatch awaits the verify() + the fauna.account.state.put round-trip, so by
            // the time it returns the credential is persisted server-side into the
            // admin's fauna.state.dns (verified by the windows e2e gate
            // test_onboarding_dns_glue_windows.py).
            await machine.Dispatch(new DnsAction.PutCredentials(providerId, fields, label));
        });
    }

    /// <summary>
    /// Queue the mint of the default capability-grant set — the one-tap
    /// <c>trust_prompt</c> answer, consumed at the signed-in handoff
    /// (<c>onboarding.md</c> § 3b-ter). <see cref="OnboardingMachine.TakeTrustPromptGranted"/>
    /// is read <b>synchronously</b> here, before the caller fires
    /// <see cref="OnLoggedIn"/> (which tears the wizard machine down) — the same
    /// capture-before-adopt shape <see cref="QueueCapturedDnsCredentialSeal"/>
    /// uses. The mint itself runs later on the session's own client
    /// (<see cref="PostSignInHandoff"/>), so it never blocks the user's landing. Best-effort and log-only <b>by design</b>: the user has
    /// completed onboarding, and a mint failure must not paint an error over
    /// that — the same trust is grantable any time from Settings → Nests, which
    /// is also where the minted grants and their log entries surface.
    /// <b>Which</b> grants is not decided here: <c>MintDefaultSet</c> mints
    /// exactly what the shared mint catalog derives, so this glue holds no
    /// policy that could drift from the Nests page's own picker (the same
    /// <c>LinkedNestsMachine</c> the Nests page's own control drives). Mirrors
    /// linux's <c>Client::mint_default_trust_set</c> / tui's twin, both thin
    /// wrappers over the shared
    /// <c>fauna_client_pair::dispatch_mint_default_trust_set</c>.
    /// </summary>
    private void QueueDefaultTrustSetMint(string actorId)
    {
        // Read now — OnLoggedIn (fired by the caller right after this returns)
        // tears the wizard machine down.
        if (!_m.TakeTrustPromptGranted()) return;

        // Best-effort, off the landing path: a failure is logged by the hand-off,
        // and the user can grant the same trust any time from Settings → Nests.
        PostSignInHandoff.Enqueue(actorId, "one-tap trust mint", async rpc =>
        {
            using var machine = await rpc.BuildLinkedNestsMachineWithTrustAsync();
            await machine.Dispatch(new LinkedNestsAction.MintDefaultSet());
        });
    }

    /// <summary>
    /// Queue the registration of the kit the <c>recovery_kit</c> screen
    /// minted and the user confirmed — the deferred half of that screen's
    /// ceremony (<c>identity-succession.md</c> § The RecoveryKey → <i>Creation
    /// UX</i>). The shared <c>ceremony::register_deferred_kit</c> behind
    /// <see cref="FaunaFfiMethods.RecoveryRegisterDeferredKit"/> registers THAT
    /// root (never a fresh one — the user has just written this one down), mirrors
    /// the profile head and logs every arm; tui's and linux's handoffs call the
    /// same body. Runs on the session's own client (<see cref="PostSignInHandoff"/>),
    /// like <see cref="QueueDefaultTrustSetMint"/>; every input is captured by the
    /// caller before <see cref="OnLoggedIn"/> tears the wizard down. Log-only by
    /// design: a failure leaves Settings' <c>recovery-kit-status</c> telling the
    /// truth (never-created).
    /// </summary>
    private static void QueueDeferredRecoveryKitRegistration(string actorId, string kitHex) =>
        PostSignInHandoff.Enqueue(actorId, "deferred recovery kit registration",
            rpc => rpc.RecoveryRegisterDeferredKitAsync(kitHex));

    /// <summary>
    /// Read-only access to the current nest URL — kept for the
    /// <see cref="NestLoginAddressText"/>-equivalent surfaces and for
    /// post-Done persistence (<see cref="HandleWizardOutcome"/> reads
    /// <c>WizardOutcome</c> directly, not this).
    /// </summary>
    public string NestUrl => _m.NestUrl();

    // ── DNS config stage ──
    /// <summary>
    /// "Buy domain on Continue" — two-way bound. The Rust state machine
    /// re-derives provider eligibility (registrar capability) when this
    /// flips, so the observer fires PropertyChanged for everything that
    /// depends on it.
    /// </summary>
    public bool BuyDomain
    {
        get => _m.DnsConfig().@buyDomain;
        set => _m.ToggleBuyDomain(value);
    }

    /// <summary>
    /// "Buy VPS with same provider" — two-way bound. When on, providers
    /// without VPS capability are disabled in the row, and the VPS-only
    /// fields of dual-capability providers (e.g. Hetzner) appear in the
    /// credentials form via <see cref="VisibleDnsFields"/>.
    /// </summary>
    public bool SameProviderForVps
    {
        get => _m.DnsConfig().@sameProviderForVps;
        set => _m.ToggleSameProviderForVps(value);
    }

    /// <summary>
    /// The currently selected provider id (from the row), or <c>null</c>
    /// if none selected yet. The whole per-provider section
    /// (link / help / credentials form / verify / status / price) hangs
    /// off this — see <see cref="DnsProviderSectionVisible"/>.
    /// </summary>
    public string? SelectedDnsProviderId => _m.DnsConfig().@selectedProviderId;

    /// <summary>
    /// Whether the per-provider section is shown. False until the user
    /// clicks a provider button; matches the Linux/web pattern of
    /// rebuilding the section reactively when <c>SelectedProviderId</c>
    /// changes.
    /// </summary>
    public bool DnsProviderSectionVisible => SelectedDnsProviderId is not null;

    /// <summary>
    /// The visible credential fields for the selected DNS provider. The
    /// machine filters by capability and by the
    /// <see cref="SameProviderForVps"/> flag (e.g. Hetzner's VPS-only
    /// field is hidden until same-provider is on).
    /// </summary>
    public IReadOnlyList<FieldMetaPlain> VisibleDnsFields => _m.VisibleDnsFields();

    /// <summary>
    /// Setter delegate for <see cref="GenericProviderForm"/>. Captured as
    /// a field once because <c>{x:Bind}</c> on a property re-reads on
    /// every notification — without caching, the form would lose focus
    /// every refresh.
    /// </summary>
    public Action<string, string> SetDnsCred { get; }

    /// <summary>
    /// Getter delegate for <see cref="GenericProviderForm"/>. Reads from
    /// the latest <see cref="OnboardingMachine.DnsConfig"/> snapshot.
    /// </summary>
    public Func<string, string> GetDnsCred { get; }

    /// <summary>Hosted-auth (bundled provider) delegates for the DNS form's
    /// <c>hosted-auth</c> field button — label, pressability, begin (returns
    /// the URL to open, or null on failure) and wait. See the constructor's
    /// caching note above; onboarding.md § 4.</summary>
    public Func<string, string> HostedAuthLabelDns { get; }
    public Func<string, bool> HostedAuthEnabledDns { get; }

    /// <summary>The whole device flow as ONE delegate — see
    /// <see cref="RunHostedAuth"/> for why the view no longer sequences
    /// begin and wait itself.</summary>
    public Func<string, Action<string>, Task> HostedAuthRunDns { get; }

    /// <summary>
    /// Whether the verify button is enabled — the machine returns true
    /// once all required fields for the selected provider are non-empty.
    /// </summary>
    public bool CanVerifyDns => _m.CanVerifyDns();

    /// <summary>
    /// Whether the dns-config-continue-button is enabled — the machine
    /// requires both verified credentials and (if buy_domain is on) the
    /// price-confirm checkbox ticked.
    /// </summary>
    public bool CanContinueDns => _m.CanContinueDns();

    /// <summary>
    /// Status text for `dns-status-text`. The machine returns a
    /// <c>LocalizedText</c> {key, args}; the client resolves the key
    /// through its generated i18n table and substitutes args.
    /// </summary>
    public string DnsStatusText => Strings.Resolve(_m.DnsStatusTextKey());

    /// <summary>
    /// Pre-formatted price for the <c>dns-tld-price-display</c> element.
    /// Sourced from the registrar's per-domain availability call (M2B2
    /// step 1) — the wizard no longer queries TLD-level pricing. Empty
    /// when the registrar didn't return <c>Buyable</c>.
    /// Mirrors `apps/fauna-linux/src/views/onboarding/dns_config.rs`'s
    /// `dns-tld-price-display` branch.
    /// </summary>
    public string TldPriceText
    {
        get
        {
            if (_m.DnsConfig().@currentAvailability is RegistrarAvailability.Buyable b)
                return FaunaOnboardingMachineMethods.FormatPrice(
                    b.@priceCents, b.@currency ?? "USD");
            return "";
        }
    }

    /// <summary>
    /// Whether the <c>dns-tld-price-display</c> TextBlock is shown —
    /// only when the registrar reported <c>Buyable</c> for this domain.
    /// </summary>
    public bool TldPriceVisible
        => _m.DnsConfig().@currentAvailability is RegistrarAvailability.Buyable;

    /// <summary>
    /// Whether the <c>dns-price-confirm-checkbox</c> is shown. Only when
    /// the user opted to buy via the registrar AND availability returned
    /// <c>Buyable</c>.
    /// </summary>
    public bool PriceConfirmVisible
        => _m.DnsConfig().@buyDomain
           && _m.DnsConfig().@currentAvailability is RegistrarAvailability.Buyable;

    /// <summary>
    /// Two-way bound to <c>dns-price-confirm-checkbox</c>. The setter
    /// only forwards the on-toggle (the machine has no
    /// <c>UnconfirmPrice</c>) — matching the Linux behavior.
    /// </summary>
    public bool PriceAgreed
    {
        get => _m.DnsConfig().@priceAgreed;
        set { if (value) _m.ConfirmPrice(); }
    }

    // ── WHOIS contact form ──
    //
    // Per target-state doc §4: visible only when the user is buying the
    // domain, the registrar reports UnregisteredBuyable, AND the registrar
    // requires per-registration contact info (Gandi today; Porkbun = false
    // because it uses account-level contacts). The predicate lives in the
    // shared machine (fauna_onboarding_machine, lifted); this is
    // the queryable form, not a re-derivation.
    public bool ContactFormVisible => _m.ShouldShowContactForm();

    private static readonly ContactInfo _emptyContact =
        new ContactInfo("", "", "", "", "", "", "", "", "");

    /// <summary>
    /// Substitutes an empty record when the machine has no contact yet —
    /// pre-fill from <see cref="_m"/>.<c>DnsConfig().contact</c> happens
    /// automatically once <c>verifyDns</c> populates it via
    /// <c>Registrar::fetch_default_contact()</c>.
    /// </summary>
    private ContactInfo CurrentContact => _m.DnsConfig().@contact ?? _emptyContact;

    /// <summary>
    /// Per-field two-way bound contact properties. Each setter reads the
    /// current contact, applies the per-field mutation via a record
    /// <c>with</c> expression, and pushes the result through
    /// <c>SetContact</c> — matches Apple's per-field update pattern in
    /// <c>MacDnsConfigView.update(_:_:)</c>.
    /// </summary>
    public string ContactFirstName
    {
        get => CurrentContact.@firstName;
        set => _m.SetContact(CurrentContact with { @firstName = value });
    }

    public string ContactLastName
    {
        get => CurrentContact.@lastName;
        set => _m.SetContact(CurrentContact with { @lastName = value });
    }

    public string ContactEmail
    {
        get => CurrentContact.@email;
        set => _m.SetContact(CurrentContact with { @email = value });
    }

    public string ContactPhone
    {
        get => CurrentContact.@phone;
        set => _m.SetContact(CurrentContact with { @phone = value });
    }

    public string ContactAddress1
    {
        get => CurrentContact.@address1;
        set => _m.SetContact(CurrentContact with { @address1 = value });
    }

    public string ContactCity
    {
        get => CurrentContact.@city;
        set => _m.SetContact(CurrentContact with { @city = value });
    }

    public string ContactState
    {
        get => CurrentContact.@state;
        set => _m.SetContact(CurrentContact with { @state = value });
    }

    public string ContactPostalCode
    {
        get => CurrentContact.@postalCode;
        set => _m.SetContact(CurrentContact with { @postalCode = value });
    }

    public string ContactCountry
    {
        get => CurrentContact.@country;
        set => _m.SetContact(CurrentContact with { @country = value });
    }

    [RelayCommand] private void DnsSetUpLater() => _m.DnsSetUpLater();

    [RelayCommand]
    private async Task VerifyDnsAsync()
    {
        try { await _m.VerifyDns(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    [RelayCommand]
    private void DnsConfigContinue()
    {
        try { _m.ContinueFromDns(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>
    /// Forwards a provider-row button click to the machine. Used by the
    /// platform-side <c>ProviderItemViewModel</c> wrapper since the
    /// generated <c>Providers.All</c> list lives in the WinUI assembly,
    /// not Core.
    /// </summary>
    public void SelectDnsProvider(string id) => _m.SelectDnsProvider(id);

    /// <summary>
    /// Whether a DNS provider row is selectable given the current
    /// buy-domain / same-provider-for-VPS choices (registrar / vps capability
    /// rule). Queries the shared machine's canonical predicate
    /// (<c>dns_provider_eligible</c>, lifted) rather than
    /// re-deriving the capability check per client.
    /// </summary>
    public bool DnsProviderEligible(string id) => _m.DnsProviderEligible(id);

    /// <summary>
    /// Why a DNS provider row is disabled — the sibling getter to
    /// <see cref="DnsProviderEligible"/>, the same shared shortfall
    /// computation (<c>dns_provider_ineligible_reason</c>) so the two can
    /// never disagree. "" when the provider IS eligible (nothing to show).
    /// </summary>
    public string DnsProviderIneligibleReason(string id)
    {
        var reason = _m.DnsProviderIneligibleReason(id);
        return reason is null ? "" : Strings.Resolve(reason);
    }

    /// <summary>
    /// Whether the <c>dns-registrar-notes-text</c> block is shown: buy-domain
    /// is on AND the selected provider has registrar notes. The shared
    /// machine's canonical predicate (<c>should_show_registrar_notes</c>,
    /// lifted); the note text itself stays platform-side (it
    /// reads the generated <c>Providers</c> registry for the i18n key).
    /// </summary>
    public bool ShouldShowRegistrarNotes() => _m.ShouldShowRegistrarNotes();

    // ── VPS config stage ──
    /// <summary>
    /// The currently selected VPS provider id (from the row), or
    /// <c>null</c> if none selected. The per-provider section
    /// (link / open-browser / help / credentials form / verify /
    /// location picker / server-type radio) hangs off this — see
    /// <see cref="VpsProviderSectionVisible"/>.
    /// </summary>
    public string? SelectedVpsProviderId => _m.VpsConfig().@selectedProviderId;

    /// <summary>
    /// Whether the per-provider section is shown. False until the user
    /// clicks a provider button; matches the Linux pattern of rebuilding
    /// the section reactively when <c>SelectedProviderId</c> changes.
    /// </summary>
    public bool VpsProviderSectionVisible => SelectedVpsProviderId is not null;

    /// <summary>
    /// The visible credential fields for the selected VPS provider. The
    /// machine filters by capability (<c>Capability::Vps</c>); empty when
    /// no provider is selected.
    /// </summary>
    public IReadOnlyList<FieldMetaPlain> VisibleVpsFields => _m.VisibleVpsFields();

    /// <summary>
    /// Setter delegate for <see cref="GenericProviderForm"/>. Captured
    /// once for the same focus-preservation reason as
    /// <see cref="SetDnsCred"/>.
    /// </summary>
    public Action<string, string> SetVpsCred { get; }

    /// <summary>
    /// Getter delegate for <see cref="GenericProviderForm"/>. Reads from
    /// the latest <see cref="OnboardingMachine.VpsConfig"/> snapshot.
    /// </summary>
    public Func<string, string> GetVpsCred { get; }

    /// <summary>Hosted-auth (bundled provider) delegates for the VPS form's
    /// <c>hosted-auth</c> field button — same shape as the DNS pair above
    /// (<see cref="HostedAuthLabelDns"/>); the sign-in done on the DNS form
    /// carries over here since both forms address the same machine field id
    /// per the machine's own state.</summary>
    public Func<string, string> HostedAuthLabelVps { get; }
    public Func<string, bool> HostedAuthEnabledVps { get; }

    /// <summary>See <see cref="HostedAuthRunDns"/>.</summary>
    public Func<string, Action<string>, Task> HostedAuthRunVps { get; }

    /// <summary>
    /// Whether the VPS verify button is enabled — the machine returns
    /// <c>true</c> once all required VPS-kind fields for the selected
    /// provider are non-empty.
    /// </summary>
    public bool CanVerifyVps => _m.CanVerifyVps();

    /// <summary>
    /// Whether the <c>vps-config-continue-button</c> is enabled — the
    /// machine requires verified credentials, a chosen server type, and
    /// (when locations are returned) a selected location.
    /// </summary>
    public bool CanContinueVps => _m.CanContinueVps();

    /// <summary>
    /// Why <c>vps-config-continue-button</c> is disabled — "" when nothing
    /// blocks it (<c>vps_continue_blocked_reason</c>, the shared machine's
    /// sibling getter to <see cref="CanContinueVps"/> so the two can never
    /// disagree; rule-5 render lift, `README.md` § Copy comprehensibility).
    /// </summary>
    public string VpsContinueBlockedReasonText
    {
        get
        {
            var reason = _m.VpsContinueBlockedReason();
            return reason is null ? "" : Strings.Resolve(reason);
        }
    }

    /// <summary>
    /// Available VPS regions returned by <c>verify_vps</c>. Populates the
    /// <c>vps-location-picker</c> ComboBox; empty until verify succeeds.
    /// </summary>
    /// <summary>The region picker's options. Identity-stable while unchanged, for
    /// the reason <see cref="VpsServerTypes"/> states at length — same binding, same
    /// per-tick <c>Bindings.Update()</c>, same rebuild if the instance churns.</summary>
    private IReadOnlyList<VpsLocation>? _locationsShown;

    public IReadOnlyList<VpsLocation> VpsLocations
    {
        get
        {
            var fresh = _m.VpsConfig().@locations;
            if (_locationsShown is not null && _locationsShown.SequenceEqual(fresh))
                return _locationsShown;
            _locationsShown = fresh;
            return _locationsShown;
        }
    }

    /// <summary>
    /// Two-way bound to <c>vps-location-picker</c>'s SelectedValue. The
    /// setter forwards to the machine so back-and-forth navigation
    /// preserves the user's choice.
    /// </summary>
    public string? SelectedVpsLocationId
    {
        get => _m.VpsConfig().@selectedLocationId;
        // ⚠ The equality guard is load-bearing, not tidiness. This is the page's
        // ONLY TwoWay binding, and the machine's `mutate` notifies observers
        // UNCONDITIONALLY — it does not compare old and new. Without the guard the
        // page sustains its own notification storm: `Bindings.Update()` re-reads
        // this getter, WinUI writes the picker's SelectedValue, the TwoWay binding
        // pushes that same value straight back into this setter, `SelectVpsLocation`
        // mutates, `on_changed` fires, and the next full-page re-evaluation is
        // queued — for ever. Measured at ~300 re-evaluations/second consuming 100%
        // of the UI thread, which is not a performance nuisance but the reason the
        // page could not answer UI Automation at all. It starts only once
        // `verify_vps` populates the locations and something is selected, which is
        // exactly where the symptom appeared. Pinned by
        // `OnboardingSelectionFeedbackTests`.
        set
        {
            if (value is null || value == _m.VpsConfig().@selectedLocationId) return;
            _m.SelectVpsLocation(value);
        }
    }

    /// <summary>
    /// Whether the <c>vps-location-picker</c> is shown. Hidden until
    /// <c>verify_vps</c> populates the regions list.
    /// </summary>
    public bool VpsLocationPickerVisible => _m.VpsConfig().@locations.Length > 0;

    /// <summary>
    /// Snapshot state of <c>vps-config-mail-mode-toggle</c> — whether the
    /// VPS will be provisioned as a <b>mail box</b> (email + calendar, with
    /// the spam/virus scanner sidecars) or a lean <b>social-only</b> box.
    /// <b>Default ON iff the handle targets a real registerable domain</b>
    /// (the same predicate as the §3b enable-email default), OFF for a
    /// <c>localhost</c> / IP-literal target; the machine derives this until
    /// the user toggles it, so the first render reflects the right state
    /// without a click. The decision lives here (not §3b) because cloud-init
    /// must know mail-intent — and the RAM, picked on this same page — before
    /// the box boots. Read by the view's mail-mode re-sync to keep the
    /// CheckBox in sync, mirroring Linux's <c>m.provision_mail_mode_enabled()</c>.
    /// Per onboarding.md §5.
    /// </summary>
    public bool ProvisionMailModeEnabled => _m.ProvisionMailModeEnabled();

    /// <summary>CheckBox toggle handler → record the mail-vs-social choice.
    /// Forwards to the shared machine, which also <b>clears a now-too-small
    /// server-type selection</b> when mail is turned ON (the RAM gate), so the
    /// view need only re-render the filtered <see cref="VpsServerTypes"/>.
    /// Called from the view's codebehind. Per onboarding.md §5.</summary>
    public void SetProvisionMailMode(bool enabled) => _m.SetProvisionMailMode(enabled);

    /// <summary>
    /// Curated server-type options for the selected provider, RAM-gated by
    /// the mail-mode toggle (mail ON ⇒ only <c>mem_gb &gt;= 2</c> plans, via
    /// the shared <c>server_type_allowed_for_mail</c> predicate) and then
    /// capped at 5 to match <c>ui.yaml</c>'s "<c>max 5 options from
    /// curated_offers</c>" rule. Filtering <b>before</b> the index keeps
    /// <c>vps-server-type-radio[i]</c> 0-based over the <i>shown</i> set,
    /// identical across all seven apps (the e2e selects by displayed index).
    /// Wraps each <see cref="ServerTypeInfo"/> in a
    /// <see cref="ServerTypeItemViewModel"/> that carries the
    /// <c>vps-server-type-radio[i]</c> AutomationId.
    /// </summary>
    /// <summary>
    /// ⚠ Both VPS list getters below return the SAME instance while their content
    /// is unchanged, and that identity is load-bearing — it is not a micro-optimisation.
    ///
    /// <para><c>VpsConfigView</c> binds these <c>OneWay</c> and calls
    /// <c>Bindings.Update()</c> on <b>every observer tick</b>, so a getter that
    /// materialises a fresh <c>List</c> per read hands <c>ItemsSource</c> a new
    /// object every tick. WinUI compares by reference, so it tears down and rebuilds
    /// every row, continuously — the same defect the credentials form had (its
    /// <c>Fields</c> DP fired on every tick and destroyed the hosted-auth button
    /// mid-<c>await</c>), one page over.</para>
    ///
    /// <para>Measured cost: with the rows rebuilding under it, UIA could not answer
    /// about them. Every call against the app cost a ~2 s provider timeout — the
    /// bridge's own per-phase trace showed `scroll-into-view vps-server-type-radio[0]`
    /// throwing in 1998/2000/2002/2012/2015 ms over and over — and
    /// <c>Application.GetMainWindow</c>, which every element route re-resolves before
    /// it searches, timed out outright and failed the run.</para>
    ///
    /// <para>The freshness test is exact and costs no FFI call: <c>ServerTypeInfo</c>
    /// and <c>VpsLocation</c> are C# <c>record</c>s, so <c>SequenceEqual</c> is
    /// structural equality over every field. A genuine catalog change still produces
    /// a new list and a real rebuild.</para>
    /// </summary>
    private IReadOnlyList<ServerTypeInfo>? _serverTypesRaw;
    private IReadOnlyList<ServerTypeItemViewModel>? _serverTypesShown;

    public IReadOnlyList<ServerTypeItemViewModel> VpsServerTypes
    {
        get
        {
            var mailOn = ProvisionMailModeEnabled;
            var raw = _m.VpsConfig().@serverTypes
                .Where(st => FaunaOnboardingMachineMethods.ServerTypeAllowedForMail(st, mailOn))
                .Take(5)
                .ToList();
            if (_serverTypesShown is not null && _serverTypesRaw is not null
                && _serverTypesRaw.SequenceEqual(raw))
                return _serverTypesShown;
            _serverTypesRaw = raw;
            _serverTypesShown = raw.Select((st, i) => new ServerTypeItemViewModel(st, i)).ToList();
            return _serverTypesShown;
        }
    }

    /// <summary>
    /// Two-way bound to the radio group. The setter forwards to the
    /// machine; the getter resolves <c>SelectedServerTypeId</c> back to
    /// the matching VM so the radios stay in sync after rebuild.
    /// </summary>
    public ServerTypeItemViewModel? SelectedVpsServerType
    {
        get
        {
            var id = _m.VpsConfig().@selectedServerTypeId;
            if (id is null) return null;
            return VpsServerTypes.FirstOrDefault(s => s.Id == id);
        }
        set { if (value is not null) _m.SelectVpsServerType(value.Id); }
    }

    /// <summary>
    /// Forwards a VPS provider-row button click to the machine. Same
    /// rationale as <see cref="SelectDnsProvider"/> — the generated
    /// providers registry lives in the WinUI assembly.
    /// </summary>
    public void SelectVpsProvider(string id) => _m.SelectVpsProvider(id);

    /// <summary>
    /// Forwards a server-type radio selection to the machine. Used by
    /// the codebehind's <c>Checked</c> handler; XAML two-way binding
    /// also calls <see cref="SelectedVpsServerType"/>'s setter.
    /// </summary>
    public void SelectVpsServerType(string id) => _m.SelectVpsServerType(id);

    [RelayCommand]
    private async Task VerifyVpsAsync()
    {
        try { await _m.VerifyVps(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    [RelayCommand]
    private async Task VpsConfigContinueAsync()
    {
        try { await _m.ContinueFromVps(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    // ── DNS post-instructions stage ──
    /// <summary>
    /// The pre-rendered markdown block (DNS records the user must add at
    /// their registrar). Comes from the machine's
    /// <c>dns_post_instructions()</c> snapshot. Empty until VPS
    /// provisioning has returned a <c>DeferredDnsResult</c> that produced
    /// the records. Bound (read-only) to the
    /// <c>dns-post-instructions-text</c> TextBox.
    /// </summary>
    public string DnsPostInstructions => _m.DnsPostInstructions() ?? "";

    /// <summary>
    /// Acknowledges the DNS instructions and advances to <c>nest_login</c>.
    /// The "copy to clipboard" command lives in the codebehind because it
    /// uses Windows-specific <c>Windows.ApplicationModel.DataTransfer</c>
    /// types that FaunaApp.Core (net10.0) can't reference.
    /// </summary>
    [RelayCommand]
    private void DnsPostInstructionsContinue()
    {
        var step = _m.ContinueFromDnsPostInstructions();
        if (step is OnboardingStep.Done) HandleWizardOutcome();
    }

    // ── Awaiting-manual-DNS ("Almost ready") surface ────────────────────
    //
    // NOT an OnboardingStep — rendered whenever wizard_outcome() ==
    // AwaitingManualDns, true on both the same-session exit from
    // DnsPostInstructionsContinue above and the relaunch-hydration path
    // (SeedAwaitingManualDns below, seeded from App.xaml.cs). See
    // docs/goal/behavior/onboarding.md § "Almost ready" surface.

    /// <summary>
    /// Seeds the wizard straight to the "Almost ready" surface on relaunch,
    /// from the persisted awaiting-manual-DNS slot. Caller must call
    /// <see cref="SeedIdentity"/> first so signing works for the eventual
    /// claim. dnsRecordsJson is passed through verbatim (opaque to this
    /// client) to <c>seed_awaiting_manual_dns_json</c>, which parses it with
    /// serde — never re-encode it client-side. Mirrors
    /// <see cref="SeedPendingInvite"/>.
    /// </summary>
    public void SeedAwaitingManualDns(string nestUrl, string handle, string dnsRecordsJson, string claimCode)
        => _m.SeedAwaitingManualDnsJson(nestUrl, handle, dnsRecordsJson, claimCode);

    /// <summary>True while the wizard is parked at the deferred-DNS exit.</summary>
    public bool IsAwaitingManualDns => _m.WizardOutcome() is WizardOutcome.AwaitingManualDns;

    /// <summary>
    /// True once the wizard has reached <c>Done</c>/<see cref="WizardOutcome.LoggedIn"/>
    /// and <see cref="OnLoggedIn"/> has fired — the app is handing off to
    /// <c>App.StartMainAppAsync</c> (WS-RPC connect, MLS session build) and will
    /// navigate to the main app once that finishes. Lets the platform shell render
    /// a transient "signing in" placeholder for that gap instead of falling back to
    /// <c>IdentityChoice</c> (which briefly reappeared after a completed wizard).
    /// </summary>
    public bool IsSigningIn => _m.WizardOutcome() is WizardOutcome.LoggedIn;

    /// <summary>
    /// The machine's own status wording for the current <see cref="AwaitingDnsState"/>
    /// (Pending/Checking/Claiming/Claimed/Error) — never re-derived client-side, so
    /// every app says the same thing in the same state. Bound to
    /// <c>awaiting-dns-status</c>.
    /// </summary>
    public string AwaitingDnsStatusText => Strings.Resolve(_m.AwaitingManualDnsSnapshot().@message);

    /// <summary>
    /// The DNS records to add at the registrar, pre-formatted — the label and the
    /// copy button read the exact same text, so they can never disagree. Bound
    /// (read-only) to <c>awaiting-dns-records</c>.
    /// </summary>
    public string AwaitingDnsRecordsText => _m.AwaitingDnsRecordsText();

    /// <summary>
    /// Disabled while a probe or claim is already in flight — a second one would
    /// race the first for no benefit. Bound to <c>awaiting-dns-recheck-button</c>.
    /// </summary>
    public bool AwaitingDnsRecheckEnabled =>
        _m.AwaitingManualDnsSnapshot().@state is not (AwaitingDnsState.Checking or AwaitingDnsState.Claiming);

    /// <summary>
    /// False when there are no records to copy — a resumed standard-path run
    /// (`onboarding-provisioning.md` § "Almost ready" surface, *Two modes, one
    /// page*). Disabled, never hidden: <c>awaiting-dns-copy-button</c> is one of
    /// this page's required elements in ui.yaml. Bound (never re-derived
    /// client-side) to the shared machine's own answer.
    /// </summary>
    public bool AwaitingDnsCopyEnabled => _m.AwaitingDnsCopyEnabled();

    /// <summary>
    /// Whether the exit ("Use a different nest") is live — the machine's answer
    /// (off only while a claim is in flight), never re-derived here. Deliberately
    /// NOT <see cref="AwaitingDnsRecheckEnabled"/>: a box that never answers spends
    /// its life in <c>Checking</c>, and the exit exists for exactly that box
    /// (`onboarding-provisioning.md` § "Almost ready" surface → *Exit*). Bound to
    /// <c>awaiting-dns-fallthrough-button</c>.
    /// </summary>
    public bool AwaitingDnsFallthroughEnabled => _m.AwaitingDnsFallthroughEnabled();

    /// <summary>
    /// The exit's one door: the shared machine clears the onboarded identity's
    /// awaiting slot (through the injected pending-provision store) BEFORE it moves
    /// its own state, then lands <c>handle_entry</c> holding the same identity. No
    /// registry call here — the machine's store owns the slot. The observer tick
    /// that follows swaps <see cref="OnboardingPage"/> off the surface. Mirrors
    /// tui's <c>Action::AbandonAwaitingManualDns</c>.
    /// </summary>
    [RelayCommand]
    private void AbandonAwaitingManualDns() => _m.AbandonAwaitingManualDns();

    /// <summary>
    /// The explicit "check now" probe on the "Almost ready" surface — one
    /// single-shot <c>recheck_manual_dns()</c>. Also fired on a 10s timer by
    /// <c>AwaitingManualDnsView</c> while the surface is shown (the machine's
    /// poll cadence is single-shot by contract; the client owns the cadence —
    /// mirrors <c>apps/fauna-linux/src/views/onboarding/awaiting_manual_dns.rs</c>
    /// POLL_INTERVAL). On a successful claim this advances the wizard past
    /// <c>Done</c>, so <see cref="OnboardingPage"/> naturally re-renders the
    /// next step.
    /// </summary>
    [RelayCommand]
    private async Task RecheckManualDnsAsync()
    {
        var step = await _m.RecheckManualDns();
        if (step is OnboardingStep.Done) HandleWizardOutcome();
    }

    // ── Provisioning stage ─────────────────────────────────────────────
    //
    // The machine drives nest_provisioning through ProvisioningSnapshot
    // { overall, steps[4], started_at_ms, finished_at_ms, result,
    // final_error }. The view binds to the derived properties below;
    // commands forward to the snapshot-driving methods on the machine.
    // Mirrors apps/fauna-linux/src/views/onboarding/nest_provisioning.rs.

    /// <summary>Cached provisioning snapshot — refreshed lazily on each
    /// access if the cache is stale. The observer-bridge invalidates the
    /// cache on every machine notification (see constructor). Caching
    /// matters because Bindings.Update() on every view subscriber reads
    /// the snapshot 9 times per tick (once per derived property), and
    /// during a navigation burst the cumulative clones saturate the UI
    /// thread enough to widen click-then-is_visible races in tests that
    /// don't include implicit waits.</summary>
    private ProvisioningSnapshot? _cachedSnapshot;
    public ProvisioningSnapshot ProvisioningSnapshot
        => _cachedSnapshot ??= _m.ProvisioningSnapshot();

    /// <summary>Four <see cref="ProvisioningStepItemViewModel"/> rows
    /// derived from the snapshot. Built fresh on every observer tick
    /// because OnboardingViewModel raises PropertyChanged with empty
    /// string (line 68) — every binding re-evaluates.</summary>
    public IReadOnlyList<ProvisioningStepItemViewModel> ProvisioningSteps
    {
        get
        {
            var snap = ProvisioningSnapshot;
            var list = new List<ProvisioningStepItemViewModel>(4);
            foreach (var step in snap.@steps)
                list.Add(BuildStepItem(step));
            return list;
        }
    }

    /// <summary>
    /// The §6 top-region price summary's one-time (domain) line — the
    /// pre-commit recap of what the run is about to charge. Both the
    /// first-year/renewal split and the plain fallback are chosen once in
    /// shared Rust (<c>OnboardingMachine::bom_domain_line</c>), so this
    /// getter only resolves the text (priority #2/#4 — linux and tui once
    /// carried mirrored, independently-drifting copies of that branch).
    /// <c>ResolveNested</c>, not <c>Resolve</c>: <c>{label}</c> is itself an
    /// i18n key (the step's own <c>onboarding.provision.step.domain</c>), so a
    /// plain resolve would paint the raw key inside the sentence.
    /// </summary>
    public string ProvisioningBomDomainLineText
    {
        get
        {
            var line = _m.BomDomainLine();
            return line is null ? "" : Strings.ResolveNested(line);
        }
    }

    /// <summary>The domain line renders only on the buy-a-new-domain path —
    /// <c>bom_domain_line()</c> is <c>None</c> when nothing one-time is
    /// chargeable (onboarding.md § 6).</summary>
    public bool ProvisioningBomDomainLineVisible
        => ProvisioningBomDomainLineText.Length > 0;

    /// <summary>The §6 recap's recurring (VPS) line. Always present once
    /// <c>vps_config</c>'s Continue has been taken — that page requires a
    /// server-type selection — but still gated on the getter, so a wizard
    /// state with no selection paints nothing rather than a stale price.</summary>
    public string ProvisioningBomVpsLineText
    {
        get
        {
            var line = _m.BomVpsLine();
            return line is null ? "" : Strings.ResolveNested(line);
        }
    }

    /// <summary>See <see cref="ProvisioningBomDomainLineVisible"/>.</summary>
    public bool ProvisioningBomVpsLineVisible
        => ProvisioningBomVpsLineText.Length > 0;

    /// <summary>Visible only when overall == Idle (top-region "Buy and
    /// set up" CTA per target doc §6).</summary>
    public bool ProvisioningStartButtonVisible
        => ProvisioningSnapshot.@overall is OverallStatus.Idle;

    /// <summary>Visible only when overall == Running.</summary>
    public bool ProvisioningCancelVisible
        => ProvisioningSnapshot.@overall is OverallStatus.Running;

    /// <summary>Visible when overall == Failed or Cancelled — Retry resumes a
    /// stopped run from either terminal state (idempotency skips done steps).
    /// Without the Cancelled case a soft-cancel would strand the user with only
    /// Back. Per docs/goal/behavior/onboarding.md §6.</summary>
    public bool ProvisioningRetryVisible
        => ProvisioningSnapshot.@overall is OverallStatus.Failed or OverallStatus.Cancelled;

    /// <summary>Continue is enabled only when overall == Succeeded.</summary>
    public bool ProvisioningContinueEnabled
        => ProvisioningSnapshot.@overall is OverallStatus.Succeeded;

    /// <summary>
    /// Why the wizard-exit Continue button is dead — one message per blocked
    /// <c>OverallStatus</c> (idle/running/failed/cancelled), not just the four
    /// step glyphs (`ui/README.md` rule 5). "" when <see cref="ProvisioningContinueEnabled"/>.
    /// </summary>
    public string ProvisioningContinueBlockedReasonText
    {
        get
        {
            var reason = _m.ProvisioningContinueBlockedReason();
            return reason is null ? "" : Strings.Resolve(reason);
        }
    }

    /// <summary>Elapsed text is rendered once started_at_ms is set.</summary>
    public bool ProvisioningElapsedVisible
        => ProvisioningSnapshot.@startedAtMs is not null;

    /// <summary>Wall-clock elapsed since started_at_ms, frozen on
    /// finished_at_ms once the run terminates. The ms→secs subtraction, the
    /// freeze, and the backwards-clock guard live once in shared Rust
    /// (<c>fauna_provisioning::progress::elapsed_display</c>, surfaced via
    /// <see cref="ValueFormat.ProvisioningElapsed"/>); this getter just supplies
    /// the ticking <c>now</c> (priority #2/#4).</summary>
    public string ProvisioningElapsedText
    {
        get
        {
            var snap = ProvisioningSnapshot;
            return ValueFormat.ProvisioningElapsed(snap.@startedAtMs, snap.@finishedAtMs, _nowMs);
        }
    }

    /// <summary>Page-level error: prefer snapshot.final_error, fall back
    /// to the machine's general ErrorMessage. Mirrors Linux
    /// nest_provisioning.rs:336-349.</summary>
    public string? ProvisioningPageErrorMessage
    {
        get
        {
            var snap = ProvisioningSnapshot;
            if (!string.IsNullOrEmpty(snap.@finalError)) return snap.@finalError;
            return _m.ErrorMessage();
        }
    }

    /// <summary>True when the page-level InfoBar should be shown.</summary>
    public bool ProvisioningHasError
        => !string.IsNullOrEmpty(ProvisioningPageErrorMessage);

    /// <summary>Wall-clock now in ms — bumped by the 1Hz DispatcherTimer
    /// (Task 6) so ProvisioningElapsedText advances between snapshot
    /// updates. Initialized at construction time.</summary>
    private ulong _nowMs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    /// <summary>Called by the View's 1Hz DispatcherTimer to advance the
    /// elapsed counter. The VM lives in FaunaApp.Core which can't reference
    /// Microsoft.UI.Xaml.DispatcherTimer, so the View owns the tick and
    /// pokes the VM via this method. Fires PropertyChanged on the UI
    /// thread (since DispatcherTimer ticks there), so the binding system
    /// re-reads ProvisioningElapsedText safely.</summary>
    public void TickElapsed()
    {
        _nowMs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        OnPropertyChanged(nameof(ProvisioningElapsedText));
    }

    /// <summary>Build one row from a snapshot step. Per-step row visibility is
    /// the shared canonical projection — <c>shows_substep</c>/<c>shows_error</c>/
    /// <c>shows_attempt_suffix</c>, computed once in
    /// <c>fauna_provisioning::progress::StepSnapshot::recompute_display</c> and
    /// carried on the snapshot (linux/web/android read the same booleans). This
    /// renderer reads them rather than re-deriving the rule (priority #2/#4). The
    /// substep <em>text</em> is still composed here: <c>shows_substep</c> is
    /// designed to be true exactly when this resolved text is non-empty.</summary>
    private static ProvisioningStepItemViewModel BuildStepItem(StepSnapshot step)
    {
        string substepText = "";
        if (step.@showsSubstep)
        {
            // Skipped resolves the "already configured" placeholder directly (its
            // substep is StatusSkipped anyway); otherwise the shared substep_label
            // maps the key → its i18n key (StatusRetrying carries {cause}).
            substepText = step.@status is StepStatus.Skipped
                ? Strings.Get("onboarding/provision/substep/status_skipped")
                : (step.@substep is SubstepKey k
                    ? ValueFormat.ProvisioningSubstepLabel(k, step.@lastError)
                    : "");
            if (step.@showsAttemptSuffix)
            {
                // The resw template keeps en.yaml's NAMED placeholders
                // ({attempt}/{max_attempts}) verbatim — the windows resw
                // format is flat, it does not renumber them to {0}/{1}.
                // A bare string.Format here throws FormatException (can't
                // parse "attempt" as a positional index), which aborts this
                // getter and — since it's read from a single
                // OnPropertyChanged(string.Empty) batch alongside every
                // other x:Bind on the page — silently stalls sibling
                // bindings too. Strings.Format maps each positional arg
                // onto the i-th distinct {name} token in textual order,
                // same as every other multi-arg key in this file (e.g.
                // Strings.Error's errors/http_error).
                substepText += Strings.Format(
                    "onboarding/provision/step_attempt_template",
                    step.@attempt, step.@maxAttempts);
            }
        }

        return new ProvisioningStepItemViewModel
        {
            Glyph          = ValueFormat.ProvisioningStatusGlyph(step.@status),
            Label          = ValueFormat.ProvisioningStepLabel(step.@kind),
            Substep        = substepText,
            SubstepVisible = step.@showsSubstep,
            Error          = step.@lastError ?? "",
            ErrorVisible   = step.@showsError,
        };
    }

    // ── Provisioning commands ────────────────────────────────────────

    /// <summary>"Buy and set up" CTA — drives the orchestrator via the
    /// suspend export and returns once it completes. Idempotent per target
    /// doc §6. Uses <c>RunProvisioning()</c> (UniFFI's own managed tokio
    /// runtime), not the sync <c>StartProvisioning()</c> — the latter does a
    /// bare Rust-side <c>tokio::spawn</c> with no ambient runtime guaranteed
    /// on the calling (WinUI UI) thread, the same panic hazard linux/android/
    /// macOS/iOS already found and fixed for this exact call
    /// (`onboarding.md` § E2E bridge contract, "Fire-and-forget
    /// orchestration").</summary>
    [RelayCommand]
    private async Task ProvisioningStartAsync()
    {
        try { await _m.RunProvisioning(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Soft-cancel — observed at the next step boundary.</summary>
    [RelayCommand]
    private void ProvisioningCancel()
    {
        try { _m.CancelProvisioning(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Re-runs from the top; idempotency short-circuits done
    /// steps so this is functionally "resume from failed step." Same
    /// suspend-export rationale as <see cref="ProvisioningStartAsync"/> —
    /// `retry_provisioning` resets the snapshot/cancel flag then calls the
    /// same `run_provisioning` Rust fn.</summary>
    [RelayCommand]
    private async Task ProvisioningRetryAsync()
    {
        try { await _m.RunProvisioning(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Bottom-row Continue. Refuses unless overall == Succeeded
    /// (machine-side guard). On Done, route through HandleWizardOutcome
    /// — covers both LoggedIn (standard) and AwaitingManualDns (deferred
    /// path) per target doc "Wizard exit handling".</summary>
    [RelayCommand]
    private void ProvisioningContinue()
    {
        try
        {
            var step = _m.ContinueFromProvisioning();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    // ── Invite request stage (Wave 3 / Spec 1) ─────────────────────────
    //
    // The machine drives invite_request through InviteRequestSnapshot
    // { state, message, continueEnabled, recheckVisible, outOfBandCodeState }.
    // The view binds to the derived properties below; commands forward to
    // the snapshot-driving methods on the machine.

    /// <summary>Resolved LocalizedText for the top-row status display.</summary>
    public string InviteRequestStatusText
        => Strings.Resolve(_m.InviteRequestSnapshot().@message);

    public bool InviteRequestContinueEnabled => _m.InviteRequestSnapshot().@continueEnabled;
    public bool InviteRequestRecheckVisible  => _m.InviteRequestSnapshot().@recheckVisible;

    /// <summary>
    /// Bottom-row out-of-band invite code. Held locally because the machine
    /// takes the code as a parameter to <see cref="VerifyOobInviteCode"/>
    /// rather than maintaining a setter.
    /// </summary>
    public string InviteCode
    {
        get => _inviteCode;
        set
        {
            if (_inviteCode == value) return;
            _inviteCode = value;
            OnPropertyChanged();
        }
    }
    private string _inviteCode = "";

    /// <summary>
    /// Localized status text for the bottom-row out-of-band code section.
    /// Resolves <see cref="InviteRequestSnapshot.oobMessage"/> — shared Rust
    /// computes the localized key + args from <c>outOfBandCodeState</c> per
    /// <c>docs/goal/behavior/onboarding.md</c> Architectural rule 4.
    /// </summary>
    public string InviteCodeStatusText
        => Strings.Resolve(_m.InviteRequestSnapshot().@oobMessage);

    /// <summary>The guardian's handle if the verified OOB code carries a supervised
    /// designation (family-safety.md § Wire &amp; data shape); <c>null</c> when the
    /// code hasn't verified Valid yet, or is an ordinary code.</summary>
    private string? InviteCodeSupervisedByHandle
        => _m.InviteRequestSnapshot().@outOfBandCodeState is OobCodeState.Valid v ? v.@supervisedBy : null;

    /// <summary>Drives <c>invite-code-supervised-notice</c> — shown before
    /// redemption when the checked code carries a supervised designation.</summary>
    public bool ShowInviteCodeSupervisedNotice => InviteCodeSupervisedByHandle is not null;

    /// <summary>"This account will be supervised by {guardian}" — resolved here
    /// (named-placeholder substitution is per-app, i18n_placeholder_named_not_numeric).</summary>
    public string InviteCodeSupervisedNoticeText
        => InviteCodeSupervisedByHandle is { } handle
            ? Strings.Get("family/supervised_notice_onboarding").Replace("{guardian}", handle)
            : "";

    [RelayCommand]
    private async Task SubmitInviteRequestAsync()
    {
        try
        {
            var step = await _m.WizardSubmitInviteRequest();
            // "The only write moment" (onboarding.md § 3 Persistence callouts).
            // This journey has no wizard exit to hang the write on any more, so
            // the slot is written here and the poll takes over.
            PersistPendingInviteSlot();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    [RelayCommand]
    private async Task RecheckInviteStatusAsync()
    {
        try
        {
            var step = await _m.RecheckInviteStatus();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    [RelayCommand]
    private async Task VerifyOobInviteCodeAsync()
    {
        if (string.IsNullOrEmpty(_inviteCode)) return;
        try { await _m.VerifyOobInviteCode(_inviteCode); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>
    /// Continue is the out-of-band code's redeem and nothing else
    /// (onboarding.md § 3 — the button's row). The Approved and PendingReview
    /// branches retired 2026-08-12 with the continue-exit: no live nest serves
    /// Approved, and the pending-review journey advances by polling, where
    /// <c>continue_enabled</c> is false throughout.
    /// </summary>
    [RelayCommand]
    private async Task InviteRequestContinueAsync()
    {
        try
        {
            var snap = _m.InviteRequestSnapshot();
            if (snap.@outOfBandCodeState is not OobCodeState.Valid) return;
            var step = await _m.RedeemInvite();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>
    /// Write the pending-invite resume slot at the submit return — windows' half
    /// of the 2026-08-12 retirement of <c>WizardOutcome::InviteSubmitted</c>.
    /// The slot itself is assembled by shared Rust
    /// (<c>OnboardingMachine.PendingInviteSlot()</c>), so the nest_url and
    /// status_json rules are not re-derived per app. A no-op unless the machine
    /// is actually in <c>PendingReview</c>.
    ///
    /// <b>The registry write is unconditional — append mode included.</b> It is
    /// the same <see cref="IFfiAccountRegistry.PersistPendingInvite"/> the
    /// first-run journey uses, and moving the active pointer is the ratified
    /// adoption (<c>onboarding.md</c> § Multi-account: "the append glue adopts
    /// on the submit return"), not a defect — the opposite of
    /// <see cref="CommitConfirmedIdentity"/>, which SKIPS the registry in append
    /// mode because moment 1 is not the append's terminal; this write IS the
    /// terminal. What append mode adds is the live session following the
    /// registry: <see cref="OnPendingInvitePersisted"/> hands the actor id to
    /// the app shell, which switches (see its doc). Fired AFTER the write, so
    /// the switch finds the appended identity already registered.
    /// </summary>
    private void PersistPendingInviteSlot()
    {
        var slot = _m.PendingInviteSlot();
        if (slot is null) return;
        // The identity comes from the MACHINE — in append mode it exists
        // nowhere else yet (moment 1 wrote nothing).
        var secretHex = _m.EffectiveSecret();
        if (string.IsNullOrEmpty(secretHex))
        {
            ShellLog.Error("OnboardingViewModel",
                "[onboarding] pending-invite slot with no effective secret on the machine");
            return;
        }
        try
        {
            // The real per-actor slot (onboarding.md § Long-term store contract).
            var actorId = _registry.PersistPendingInvite(
                secretHex, slot.@nestUrl, slot.@handle, slot.@requestId, slot.@statusJson);
            OnPendingInvitePersisted?.Invoke(actorId);
        }
        catch (Exception ex)
        {
            ShellLog.Error("OnboardingViewModel",
                $"[onboarding] registry PersistPendingInvite failed: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Back from invite_request: cancel any in-flight HTTP and route via
    /// the standard <see cref="BackCommand"/>. Per the target-state doc,
    /// the pending-invite slot is NOT deleted by Back.
    /// </summary>
    [RelayCommand]
    private void InviteRequestBack()
    {
        _m.CancelInviteOp();
        _m.Back();
    }

    // ── Claim code stage (target §3a) ──────────────────────────────────
    //
    // Reached when handle-check returns UnregisteredUnclaimedNest (the
    // nest is reachable but the fauna.setup.status WS-RPC kind reports claimed=false).
    // Mutually exclusive with invite_request — there is no admin yet, so
    // the only path forward is to enter the one-time claim code printed
    // by the nest server's bootstrap process and atomically become the
    // admin via POST /api/v1/claim-admin.
    //
    // The machine drives the page through ClaimCodeSnapshot
    // { state, message, submit_enabled }. Submit is terminal: on 2xx the
    // wizard exits with WizardOutcome::LoggedIn (admin); 4xx/5xx leave
    // the wizard on ClaimCode with state ∈ {Invalid, Error}.

    /// <summary>
    /// Two-way bound to <c>claim-code-input</c>. Held locally because the
    /// machine takes the code as a parameter to
    /// <see cref="WizardSubmitClaimCode"/> rather than maintaining a
    /// setter, mirroring the InviteCode pattern.
    /// </summary>
    public string ClaimCode
    {
        get => _claimCode;
        set
        {
            if (_claimCode == value) return;
            _claimCode = value;
            OnPropertyChanged();
        }
    }
    private string _claimCode = "";

    /// <summary>Resolved LocalizedText for <c>claim-code-status</c> —
    /// renders the snapshot's Idle / Submitting / Claimed / Invalid /
    /// Error message. Per-field error feedback for the code input lives
    /// here; the page-level <c>error-message</c> InfoBar is reserved for
    /// orchestrator-pushed banners.</summary>
    public string ClaimCodeStatusText
        => Strings.Resolve(_m.ClaimCodeSnapshot().@message);

    /// <summary>Submit-button enablement comes directly from the
    /// snapshot — the Rust side encodes both "machine is not in flight"
    /// and "input non-empty" per <c>docs/goal/behavior/onboarding.md</c>
    /// §3a.</summary>
    public bool ClaimCodeSubmitEnabled => _m.ClaimCodeSnapshot().@submitEnabled;

    [RelayCommand]
    private async Task SubmitClaimCodeAsync()
    {
        if (string.IsNullOrEmpty(_claimCode)) return;
        try
        {
            // Phase-4 "no-modes" routes a successful wizard_submit_claim_code
            // directly to NatModeChoice (the admin path's terminal step) rather
            // than straight to Done — the observer tick swaps OnboardingPage
            // to that view. Only the explicit defer / submit actions there
            // return Done.
            var step = await _m.WizardSubmitClaimCode(_claimCode);
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Back routes to <c>handle_entry</c> (machine-side: the
    /// ClaimCode arm of <c>back()</c>).</summary>
    [RelayCommand]
    private void ClaimCodeBack() => _m.Back();

    // ── NAT-mode-choice stage (target §3b-bis) ──────────────────────────
    //
    // Reached directly from a successful admin claim (§3a) — the single,
    // terminal admin-path setup step (no-modes, ratified 2026-07-12).
    // Resolves the nest's NAT mode (public/private, the network-reachability
    // axis). selected_mode defaults to the nest's seeded node_mode, refined
    // private-ward for a private-network target, so the common case is
    // confirm-only. Unlike the retired encryption-mode defer there is no
    // resume slot and no unresolved state: both Confirm and Defer route
    // through HandleWizardOutcome — the seed always resolves to LoggedIn.

    private NatModeSnapshot NatModeSnap => _m.NatModeSnapshot();

    /// <summary>True when <c>public-nat-mode-radio</c> should render
    /// checked.</summary>
    public bool NatModePublicSelected => NatModeSnap.@selectedMode == NodeMode.Public;

    /// <summary>True when <c>private-nat-mode-radio</c> should render
    /// checked.</summary>
    public bool NatModePrivateSelected => NatModeSnap.@selectedMode == NodeMode.Private;

    /// <summary>Resolved LocalizedText for <c>nat-mode-status</c> — renders
    /// the snapshot's Choosing / private-ward hint / Submitting / Done /
    /// Error{cause} message.</summary>
    public string NatModeStatusText => Strings.Resolve(NatModeSnap.@message);

    /// <summary><c>nat-mode-confirm-button</c> enablement. The set is
    /// mutable (no write-once conflict, unlike the retired storage-mode
    /// commit), so resubmit after an Error stays enabled; only Submitting
    /// and Done disable it.</summary>
    public bool NatModeConfirmEnabled => NatModeSnap.@submitEnabled;

    /// <summary>True while <c>submit_nat_mode_choice()</c> is in flight —
    /// the view disables both radios on this (mirrors linux's
    /// <c>inflight</c> gate in <c>nat_mode_choice.rs</c>).</summary>
    public bool NatModeInFlight => NatModeSnap.@state is NatModeState.Submitting;

    /// <summary>Negation of <see cref="NatModeInFlight"/> for the radios'
    /// <c>IsEnabled</c> bind (x:Bind has no built-in negation converter).</summary>
    public bool NatModeRadiosEnabled => !NatModeInFlight;

    /// <summary>
    /// Forwards a radio selection to the machine. Exposed as a plain method
    /// (not a <c>[RelayCommand]</c>) because the view calls it from a
    /// shared, re-entrancy-guarded <c>Checked</c> handler on two
    /// RadioButtons — the same shape as
    /// <see cref="Views.Onboarding.VpsConfigView.OnVpsServerTypeChecked"/>.
    /// </summary>
    public void SelectNatMode(NodeMode mode) => _m.SelectNatMode(mode);

    [RelayCommand]
    private async Task SubmitNatModeChoiceAsync()
    {
        try
        {
            // Commits via the mutable fauna.setup.nat_mode kind (upserts
            // nest_nat_mode; live MTA/MDA re-evaluation, ACME/STUN on next
            // restart). On success returns Done with wizard_outcome() ==
            // LoggedIn — the same terminal exit as Defer, just persisted.
            var step = await _m.SubmitNatModeChoice();
            if (step is OnboardingStep.Done) HandleWizardOutcome();
        }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary>Sync: the seeded mode stays in effect (already a working
    /// default) — settable later from the admin-nest page.</summary>
    [RelayCommand]
    private void DeferNatModeChoice()
    {
        var step = _m.DeferNatModeChoice();
        if (step is OnboardingStep.Done) HandleWizardOutcome();
    }

    // ── Trust prompt (3b-ter, `onboarding.md` § 3b-ter) ──
    //
    // The one-tap "trust this box" offer. Both exits are pure local latches
    // that land the machine on Done — same shape as DeferNatModeChoice just
    // above, and the reason both commands may call HandleWizardOutcome
    // straight from the click: HandleWizardOutcome's own work is a
    // synchronous prefix (persistence + the fire-and-forget captures) ending
    // in OnLoggedIn?.Invoke(), whose actual launch work lands on the
    // DispatcherQueue rather than running on the calling (click) thread — see
    // App.xaml.cs's OnOnboardingCompleted wiring. ⚠ The trap this generalizes
    // (measured on linux, `test_trust_prompt.py --app linux`, 2026-08-14):
    // do NOT block the dispatcher thread with a synchronous launch call from
    // inside the click handler itself — latch via the machine, let the
    // existing Done-arm plumbing drive the conclusion, same as every other
    // wizard-exit transition here.

    /// <summary>Grants the one-tap default trust set; the actual mint is
    /// deferred to the signed-in handoff (<see cref="QueueDefaultTrustSetMint"/>)
    /// since it needs an authenticated session and the nest's own roster,
    /// neither of which the wizard holds.</summary>
    [RelayCommand]
    private void GrantDefaultTrust()
    {
        var step = _m.GrantDefaultTrust();
        if (step is OnboardingStep.Done) HandleWizardOutcome();
    }

    /// <summary>Declines the offer; nothing is minted at the handoff.</summary>
    [RelayCommand]
    private void SkipTrustPrompt()
    {
        var step = _m.SkipTrustPrompt();
        if (step is OnboardingStep.Done) HandleWizardOutcome();
    }

    // ── Post-claim machine-derived intents (Phase-4 "no-modes", S8.7) ──
    //
    // The pre-alpha encryption-mode-choice page (and its four enable-{email,
    // caldav,carddav,webdav} checkboxes) is retired without replacement — every
    // nest is sealed at rest, so there's no client choice to make. The intents
    // themselves survive as machine-derived predicates (ON iff the handle
    // targets a real registerable domain, computed from the handle alone, no
    // checkbox); the post-auth launch glue still reads them once at LoggedIn
    // to fire the matching Admin-class enable RPC.

    /// <summary>Post-onboarding read of the enable-email intent. The launch glue
    /// (<c>App.onOnboardingCompleted</c>) calls this once the authenticated Admin
    /// WS connection exists and, if true, fires
    /// <c>fauna.bridges.set_mail_enabled(true)</c> — idempotent with the
    /// mail-settings enable path. Mirrors Linux's
    /// <c>email_enable_requested()</c> capture in
    /// <c>views/onboarding/mod.rs</c>.</summary>
    public bool EmailEnableRequested => _m.EmailEnableRequested();

    /// <summary>Post-onboarding read of the enable-CalDAV intent. The launch glue
    /// (<c>App.onOnboardingCompleted</c>) calls this once the authenticated Admin
    /// WS connection exists and, if true, fires
    /// <c>fauna.bridges.set_caldav_enabled(true)</c> — idempotent with the
    /// mail-settings enable path, and independent of email (a calendar-only
    /// deployment fires only this). Mirrors Linux's
    /// <c>caldav_enable_requested()</c> capture in
    /// <c>views/onboarding/mod.rs</c>.</summary>
    public bool CaldavEnableRequested => _m.CaldavEnableRequested();

    /// <summary>Post-onboarding read of the enable-CardDAV intent. The launch glue
    /// (<c>App.onOnboardingCompleted</c>) calls this once the authenticated Admin
    /// WS connection exists and, if true, fires
    /// <c>fauna.bridges.set_carddav_enabled(true)</c> — idempotent with the
    /// mail-settings enable path, and independent of email/CalDAV (a
    /// contacts-only deployment fires only this). Mirrors the CalDAV
    /// <c>CaldavEnableRequested</c> capture.</summary>
    public bool CarddavEnableRequested => _m.CarddavEnableRequested();

    /// <summary>Post-onboarding read of the enable-WebDAV intent. The launch glue
    /// (<c>App.onOnboardingCompleted</c>) calls this once the authenticated Admin
    /// WS connection exists and, if true, fires
    /// <c>fauna.bridges.set_webdav_enabled(true)</c> — idempotent with the
    /// mail-settings enable path, and independent of email/CalDAV/CardDAV (a
    /// files-only deployment fires only this). Mirrors the CardDAV
    /// <c>CarddavEnableRequested</c> capture.</summary>
    public bool WebdavEnableRequested => _m.WebdavEnableRequested();

    /// <summary>Post-onboarding read of the identity the wizard just
    /// authenticated with, straight from the machine — never the store.
    /// <c>onboarding.md</c>'s 2026-08-27 ruling ("the terminal reads the
    /// secret from the MACHINE, never from the store"): the store holds it
    /// only if moment 1's confirm-arm write landed, which a machine-only drive
    /// (<c>seed_identity</c> — the paid live-provisioning e2e's path) never
    /// runs, and an append-mode run never writes at all. The launch glue
    /// (<c>App.onOnboardingCompleted</c>) reads this synchronously, alongside
    /// the serving intents, before <see cref="OnLoggedIn"/> tears the wizard
    /// down.</summary>
    public string? EffectiveSecret => _m.EffectiveSecret();

    /// <summary>The home nest the <c>LoggedIn</c> outcome carried, latched by
    /// the terminal before <see cref="OnLoggedIn"/> fires; null until then.
    /// Moment 4 records it per-actor for a first-run wizard, but an append-mode
    /// run registers nothing itself, so the append terminal
    /// (<c>App.onOnboardingCompleted</c>) takes it from here.</summary>
    public string? LoggedInNestUrl { get; private set; }

    /// <summary>The sync device id the <c>LoggedIn</c> terminal resolved for
    /// this identity (<c>DeviceIdForActor</c>, or the random fallback), latched
    /// beside <see cref="LoggedInNestUrl"/> for the same append-terminal
    /// reason.</summary>
    public string? LoggedInDeviceId { get; private set; }

    // ── Recovery stage (box-recovery step 4 — Task E) ──────────────────
    //
    // Two pages over the shared machine's recovery branch (C1): nest_recovery
    // (box-selection hub) + recover_selfhosted_instructions. Entry is
    // recover-lost-box-button on identity_choice → BeginRecoverLostBox, which
    // routes through identity_import (recovery intent) to nest_recovery.
    // Mirrors linux apps/fauna-linux/src/views/onboarding/{nest_recovery,
    // recover_selfhosted_instructions}.rs + the web reference. Per
    // box-recovery.md § Recovery UI (step 4). NO shared-Rust change (the
    // machine surface is landed). The box list is empty in production (the
    // reachable-nest fetch is gated) → the view shows
    // recover-box-empty-message; the e2e seeds it via
    // call_machine_method("set_recovery_boxes").

    /// <summary>True once the wizard entered the recovery flow (set by
    /// <see cref="BeginRecoverLostBox"/>); the shared confirm-import routes a
    /// recovery-intent identity to nest_recovery rather than handle_entry.</summary>
    public bool RecoveryIntent => _m.RecoveryIntent();

    /// <summary>
    /// Populate <c>recover-box-list</c> on entering nest_recovery — the native
    /// twin of linux <c>fetch_recovery_boxes</c> (apps/fauna-linux/src/views/onboarding/mod.rs)
    /// and the web reference, both cited by box-recovery.md § Recovery UI
    /// (step 4) as "the two-arm reachable-else-offline split you should
    /// mirror". Two sources, picked by whether the machine already resolved a
    /// nest URL:
    /// <list type="bullet">
    /// <item>A resolved <see cref="NestUrl"/> (the surviving-device launch
    /// entry, or the Q2-A fresh-client path where handle_entry just resolved
    /// another box the admin owns) → the reachable-nest read
    /// (<see cref="FetchRecoveryBoxesReachableAsync"/>), the shared
    /// <c>FaunaFfiMethods.DeploymentSeeds</c> local ⊔ cold join, falling back to
    /// the device's own store when the nest does not answer.</item>
    /// <item>No nest URL (the single-last-box total-loss
    /// case) → the synchronous device-local read,
    /// <c>FaunaFfiMethods.DeploymentSeedsLocal</c> (the device's own account
    /// store).</item>
    /// </list>
    /// Both are best-effort and only push into the machine on ≥1 box, so an
    /// empty or failed read never clobbers a list already on screen (the
    /// launch entry's instant push, or the tier_2 <c>set_recovery_boxes</c>
    /// injection). No identity yet ⇒ nothing to unseal the account plane with ⇒
    /// no read.
    /// </summary>
    private void FetchRecoveryBoxes()
    {
        var secretHex = _m.EffectiveSecret();
        if (string.IsNullOrEmpty(secretHex)) return;
        byte[] ownerSecret;
        try
        {
            ownerSecret = Convert.FromHexString(secretHex);
        }
        catch (FormatException)
        {
            return;
        }

        var nestUrl = _m.NestUrl();
        if (string.IsNullOrEmpty(nestUrl))
        {
            PushRecoveryBoxes(FaunaApp.Core.Helpers.DeploymentSeedCustody.ReadLocalRecoverableBoxes(ownerSecret));
            return;
        }

        _ = FetchRecoveryBoxesReachableAsync(nestUrl, ownerSecret);
    }

    /// <summary>Reachable-nest leg of <see cref="FetchRecoveryBoxes"/>: the shared
    /// <see cref="FaunaApp.Core.Helpers.DeploymentSeedCustody.ReadRecoverableBoxesAsync"/>
    /// read over a throwaway connection to the machine-resolved
    /// <paramref name="nestUrl"/>, which falls back to the device's own store when
    /// that nest does not answer.</summary>
    private async Task FetchRecoveryBoxesReachableAsync(string nestUrl, byte[] ownerSecret)
    {
        PushRecoveryBoxes(await FaunaApp.Core.Helpers.DeploymentSeedCustody.ReadRecoverableBoxesAsync(nestUrl, ownerSecret));
    }

    /// <summary>Push a non-empty box list into the machine (indexed
    /// <c>recover-box-item-{i}</c> rows, via <see cref="RecoveryBoxItems"/>);
    /// an empty read is silently dropped so it can never clobber a list
    /// already on screen.</summary>
    private void PushRecoveryBoxes(FfiDeploymentSeedEntry[] boxes)
    {
        if (boxes.Length == 0) return;
        _m.SetRecoveryBoxes(boxes.Select(b => b.nestActorId).ToArray());
    }

    /// <summary>The custodied boxes offered on nest_recovery, projected into
    /// dash-indexed <c>recover-box-item-{i}</c> item VMs carrying the current
    /// selection flag. Read live so <c>Bindings.Update()</c> on each observer
    /// tick reflects a new selection / a fresh seed.</summary>
    public IReadOnlyList<RecoveryBoxItemViewModel> RecoveryBoxItems
    {
        get
        {
            var selected = _m.RecoverySelectedNestId();
            return _m.RecoveryBoxes()
                .Select((id, i) => new RecoveryBoxItemViewModel(id, i, id == selected))
                .ToList();
        }
    }

    /// <summary>Whether any custodied box is available — gates
    /// <c>recover-box-list</c> vs. <c>recover-box-empty-message</c>.</summary>
    public bool HasRecoveryBoxes => _m.RecoveryBoxes().Length > 0;

    /// <summary>Enablement of <c>recover-method-{cloud,selfhosted}-button</c> — a
    /// box must be selected first (mirrors the machine's
    /// <c>require_selected_recovery_box</c> guard + linux/web
    /// disabled-until-selected gating).</summary>
    public bool RecoveryMethodButtonsEnabled => _m.RecoverySelectedNestId() is not null;

    /// <summary>Text for <c>recover-selfhosted-command</c> — a PLACEHOLDER until
    /// the C2 reachable-nest command render lands (box-recovery.md §
    /// Implementation status of step 4, "self-hosted recovery command … C2
    /// partial"); mirrors linux's <c>SELFHOSTED_COMMAND_PENDING</c> + web's
    /// placeholder fallback. This is the single seam a future C2 leg swaps for
    /// the resolved command.</summary>
    public string RecoverSelfhostedCommand
        => Strings.Get("onboarding/recovery/selfhosted_command_pending");

    /// <summary>Entry from <c>recover-lost-box-button</c> on identity_choice:
    /// routes through identity_import with recovery intent, then to
    /// nest_recovery (<c>begin_recover_lost_box</c>).</summary>
    [RelayCommand] private void BeginRecoverLostBox() => _m.BeginRecoverLostBox();

    /// <summary>Select a custodied box on nest_recovery (enables the method
    /// buttons). Called from NestRecoveryView's row-click handler; a plain
    /// method (not a two-way property) mirrors <see cref="SelectVpsServerType"/>.</summary>
    public void SelectRecoveryBox(string nestActorId) => _m.SelectRecoveryBox(nestActorId);

    /// <summary><c>recover-method-cloud-button</c>: re-provision on a cloud host
    /// → VpsConfig (recovery mode). The machine throws when no box is selected
    /// (the button is gated, but the guard is authoritative); swallow it so a
    /// stray click is a no-op — the observer surfaces any error.</summary>
    [RelayCommand]
    private void RecoverViaCloud()
    {
        try { _m.RecoverViaCloud(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary><c>recover-method-selfhosted-button</c> →
    /// recover_selfhosted_instructions. Same guarded shape as
    /// <see cref="RecoverViaCloud"/>.</summary>
    [RelayCommand]
    private void RecoverViaSelfhosted()
    {
        try { _m.RecoverViaSelfhosted(); }
        catch (OnboardingException) { /* observer surfaces ErrorMessage */ }
    }

    /// <summary><c>recover-restore-cta</c> / <c>recover-selfhosted-continue-button</c>:
    /// leave the recovery flow. Mirrors linux (<c>m.reset()</c>); windows has no
    /// in-wizard Backups deep-link, and the recovery re-provision drive is
    /// C2/gated.</summary>
    [RelayCommand] private void RecoverReset() => _m.Reset();

    /// <summary>No-op <see cref="FfiSecretStore"/> — the default <see cref="_installStore"/>
    /// when the caller supplies none. Every read is absent, so
    /// <c>DeviceIdForActor</c> fails closed onto the random-mint fallback rather
    /// than throw at construction — keeps every existing test that never reaches
    /// <c>WizardOutcome.LoggedIn</c> building with no store fake of its own.</summary>
    private sealed class NullFfiSecretStore : FfiSecretStore
    {
        public string? Get(string key) => null;
        public void Set(string key, string value) { }
        public void Delete(string key) { }
    }
}

/// <summary>
/// Wraps a <see cref="ServerTypeInfo"/> for the <c>vps-server-type-radio</c>
/// list. Carries the per-position AutomationId
/// (<c>vps-server-type-radio[i]</c>) and a pre-formatted display string
/// using the shared UniFFI <c>server_type_label</c> helper so all apps
/// render identical text.
///
/// Public because the WinUI XAML compiler can't resolve
/// <c>x:DataType</c> against <c>internal</c> types even with
/// <c>InternalsVisibleTo</c>. The constructor is internal so callers
/// outside the assembly can't pass arbitrary <see cref="ServerTypeInfo"/>
/// values (the type itself is internal-by-UniFFI).
/// </summary>
public sealed class ServerTypeItemViewModel
{
    public string Id { get; }
    public int Index { get; }
    public string RadioItemId => $"vps-server-type-radio[{Index}]";
    public string Display { get; }

    internal ServerTypeItemViewModel(ServerTypeInfo raw, int index)
    {
        Id = raw.@id;
        Index = index;
        // server_type_label is the UniFFI free function shared with Linux
        // (apps/fauna-linux/src/views/onboarding/vps_config.rs). Falling
        // back to a local format would diverge from the Linux/web shape
        // — always use the helper.
        Display = FaunaOnboardingMachineMethods.ServerTypeLabel(raw);
    }
}

/// <summary>
/// Wraps a custodied box's <c>nest_actor_id</c> for the <c>recover-box-list</c>
/// on nest_recovery. Carries the per-position AutomationId
/// (<c>recover-box-item-{i}</c> — DASH form, matching the e2e's
/// <c>recover-box-item-0</c> and web/linux's <c>recover-box-item-{i}</c>, unlike
/// vps's bracket form), a shortened display id, and the selection flag.
///
/// Public because the WinUI XAML compiler can't resolve <c>x:DataType</c>
/// against <c>internal</c> types even with <c>InternalsVisibleTo</c>; the
/// constructor is internal so callers outside the assembly can't fabricate rows.
/// </summary>
public sealed class RecoveryBoxItemViewModel
{
    public string Id { get; }
    public int Index { get; }
    public string BoxItemId => $"recover-box-item-{Index}";
    public string ShortId { get; }
    public bool IsSelected { get; }

    internal RecoveryBoxItemViewModel(string nestActorId, int index, bool isSelected)
    {
        Id = nestActorId;
        Index = index;
        IsSelected = isSelected;
        // head-8 … tail-8 elision of a long hex id, single-sourced via the shared
        // fauna_core::format::short_nest_id (was a per-app hand-roll on linux +
        // windows; value-formatting.md § Short nest id).
        ShortId = FaunaFfiMethods.ShortNestId(nestActorId);
    }
}
