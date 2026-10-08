using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → Account <b>Recovery Kit</b> section's state
/// (<c>docs/goal/ui/settings.md</c> § Recovery kit). The windows twin of apple's
/// <c>FaunaKit/ViewModels/RecoveryKitVM.swift</c>.
///
/// <para><b>What this deliberately does NOT do.</b> It decides nothing. The status
/// line's state, which of the actions each state enables, and what the succession
/// ceremony did all arrive already decided from
/// <c>libs/fauna-ffi/src/recovery.rs</c> — which in turn composes the shared
/// <c>fauna_client_recovery</c> ceremonies every other app runs. <b>Never derive
/// enablement from <see cref="FfiRecoveryKitStatus.kind"/> here.</b> Two of the
/// predicates do not follow from the state the way a renderer would guess:</para>
/// <list type="bullet">
///   <item><description><c>allowsStolen</c> is <b>unconditionally true</b>,
///   including with no kit ever created — a thief who took the seed before a kit
///   existed is precisely the case the ceremony is for; and</description></item>
///   <item><description><c>allowsReplace</c> stays true <b>during</b> a pending
///   replacement window, because replacing with a kit you hold is how an owner
///   ends that window at once.</description></item>
/// </list>
///
/// <para><b>The kit is held only while the screen shows it.</b>
/// <see cref="MintedSecretHex"/> is the single copy of a freshly minted recovery
/// root in this process. There is no path that shows it again and there can never
/// be one (<c>identity-succession.md</c> § The RecoveryKey — <i>Custody</i>), so it
/// is dropped on leaving the page and never written anywhere.</para>
/// </summary>
/// <remarks><c>internal</c> because its surface carries UniFFI-<c>internal</c>
/// records (<see cref="FfiRecoveryKitStatus"/>, <see cref="FfiLandedSuccession"/>),
/// exactly as <c>ConversationsViewModel</c> and <c>FeedViewModel</c> are; the
/// FaunaApp shell and the test project reach it through
/// <c>[InternalsVisibleTo]</c>.</remarks>
internal partial class RecoveryKitViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;

    /// <summary>
    /// The type-to-confirm token. <b>Never localized</b> — only its prompt is
    /// (<c>settings/recovery_kit/stolen_confirm_placeholder</c>), or the gate would
    /// differ per locale. Every app spells the same literal at its own call site,
    /// exactly as account deletion spells <c>"DELETE"</c>.
    /// </summary>
    internal const string StolenConfirmWord = "SUCCEED";

    /// <summary>
    /// The section's state, or null until the chain read resolves. An un-hydrated
    /// section must not <i>claim</i> a state — <see cref="StatusLine"/> paints
    /// <c>status_loading</c> in that window, which is not an answer.
    /// </summary>
    [ObservableProperty] private FfiRecoveryKitStatus? _status;

    /// <summary>True while the chain read is in flight.</summary>
    [ObservableProperty] private bool _loading;

    /// <summary>
    /// The kit-in-hand buffer behind <c>recovery-entry-phrase-field</c> — the
    /// onboarding screen's own id, reused inline (<c>settings.md</c> § Recovery kit
    /// → <i>Kit-in-hand entry</i>). Feeds replace, stolen, veto and the escrow
    /// re-seal.
    /// </summary>
    [ObservableProperty] private string _phraseInput = string.Empty;

    /// <summary>The type-to-confirm buffer behind
    /// <c>identity-stolen-confirm-field</c>.</summary>
    [ObservableProperty] private string _stolenConfirmInput = string.Empty;

    /// <summary>A secret a ceremony just minted, shown once. Cleared on leaving the
    /// page.</summary>
    [ObservableProperty] private string? _mintedSecretHex;

    /// <summary>
    /// Whether the escrow blob landed with the registration that minted
    /// <see cref="MintedSecretHex"/>. ⚠ <c>false</c> is <b>not</b> an error to
    /// render: the registration has already landed, so the shown secret is live and
    /// is the only copy in existence. Say the kit works <i>and</i> that phrase
    /// recovery is not yet armed.
    /// </summary>
    [ObservableProperty] private bool _mintedEscrowStored = true;

    /// <summary>Unix seconds a seed-alone replacement lands, when the mint came from
    /// the <c>lost</c> ceremony. Null for create and replace, which land
    /// immediately.</summary>
    [ObservableProperty] private long? _mintedLandsAt;

    /// <summary>True while any ceremony is in flight — every button reads it, so a
    /// second click cannot start a second irreversible ceremony.</summary>
    [ObservableProperty] private bool _busy;

    /// <summary>
    /// The succession's outcome, held <b>only</b> so the page can render the
    /// persist-failure arm. ⚠ When <c>persisted</c> is false the caller must NOT
    /// tear the session down: that takes the only copy of the successor seed with
    /// it.
    /// </summary>
    [ObservableProperty] private FfiLandedSuccession? _landedSuccession;

    /// <summary>
    /// The post-succession sweep's own view, mirrored from
    /// <see cref="SuccessionHandoff.Sweep"/> at hydrate — carried across the
    /// account switch, replaced by <see cref="RetrySweepAsync"/> on a
    /// <c>swept</c> answer, and read by the page to select <c>sweep_copy</c> at
    /// paint time (<c>settings.md</c> § Recovery kit → <i>The sweep's own
    /// lines</i>). <c>null</c> on every ordinary sign-in that did not just run a
    /// ceremony.
    /// </summary>
    [ObservableProperty] private FfiSweepView? _sweepView;

    /// <summary>
    /// True while a stolen-identity ceremony's persist-failure message
    /// (<see cref="SucceedWithHeldKitAsync"/>'s <c>!landed.persisted</c> arm, or the
    /// undecided outcome whose record <c>carriesTheOnlySeed</c> — both parked by
    /// <see cref="ParkStolenMessage"/>) sits in
    /// <see cref="ViewModelBase.ErrorMessage"/>, not yet acknowledged
    /// (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The persist-failure
    /// message survives the page</i>). It is the ONLY surviving copy of the
    /// successor's new identity secret, so every other writer of the error slot —
    /// in this view model AND on the page's shared <c>ErrorBar</c> render funnel,
    /// which also takes writes from <see cref="SettingsViewModel"/> and the page
    /// itself — must leave it alone while this is true.
    ///
    /// <para>Mirrors apple's <c>stolenFailedMessagePending</c>
    /// (<c>RecoveryKitVM.swift</c>) and linux's
    /// <c>PENDING_STOLEN_FAILED_MESSAGE</c>. Internal, not private, so a test can
    /// seed the parked state directly rather than driving the whole ceremony
    /// through a fake RPC client.</para>
    /// </summary>
    internal bool StolenPersistFailurePending { get; set; }

    /// <summary>Where the ceremony reports its start and end, so the supersession it
    /// causes is held back while it owns the Account page
    /// (<see cref="StolenCeremonyHold"/>).</summary>
    private readonly StolenCeremonyHold _ceremonyHold;

    internal RecoveryKitViewModel(INestRpcClient nest, StolenCeremonyHold? ceremonyHold = null)
    {
        _nest = nest;
        _ceremonyHold = ceremonyHold ?? StolenCeremonyHold.Shared;
    }

    /// <summary>
    /// Mirror <see cref="SuccessionHandoff.Sweep"/> into <see cref="SweepView"/> —
    /// call once per hydrate, before <see cref="LoadStatusAsync"/>. Idempotent:
    /// the handoff's sweep only ever changes via <c>Record</c> or a successful
    /// <see cref="RetrySweepAsync"/>, both of which this mirror must reflect, and
    /// it outlives every hydrate (cleared only on a credential-namespace wipe).
    /// </summary>
    public void HydrateSweep() => SweepView = SuccessionHandoff.Sweep;

    /// <summary>
    /// The sweep's outcome line — what it did, or silent for a succession over
    /// zero groups (<c>settings.md</c> § Recovery kit → <i>The sweep's own
    /// lines</i>). Selected off the carried view by the SHARED
    /// <c>sweep_copy</c> projection, never matched on <c>Kind</c> here —
    /// which arm says what is that projection's alone, not this view model's.
    /// <c>rendersRetry: true</c> unconditionally: whether THIS device can retry
    /// is <see cref="FfiSweepView.owesWork"/>'s question, independent of whether
    /// the platform HAS a retry affordance at all — windows does, as of this
    /// leg.
    /// </summary>
    public string? SweepOutcomeLine =>
        SweepView is { } view
            ? FaunaFfiMethods.SweepCopy(view, rendersRetry: true).outcome is { } outcome
                ? S.Resolve(outcome)
                : null
            : null;

    /// <summary>The roster the sweep cannot vouch for, as its OWN line — never a
    /// qualifier folded into <see cref="SweepOutcomeLine"/>. Same selection rule
    /// as that property.</summary>
    public string? SweepUnattestedLine =>
        SweepView is { } view
            ? FaunaFfiMethods.SweepCopy(view, rendersRetry: true).unattested is { } unattested
                ? S.Resolve(unattested)
                : null
            : null;

    /// <summary><c>recovery-kit-sweep-retry-button</c>'s render gate — unfinished
    /// work, deliberately NEVER "this device can retry": a device that cannot
    /// still renders the button and must answer in words when pressed
    /// (<c>settings.md</c> § Recovery kit → <i>Finishing an unfinished group
    /// sweep</i>).</summary>
    public bool SweepOwesWork => SweepView?.owesWork == true;

    /// <summary>
    /// The one status line. An un-hydrated section paints <c>status_loading</c>
    /// rather than nothing — it still owes the user a reason for the dead buttons
    /// (<c>ui/README.md</c> rule 5) — and the driver reads that as "" so it stays
    /// usable as the causal barrier for "the chain read completed".
    /// </summary>
    public string StatusLine => Status?.kind switch
    {
        "never-created" => S.Get("settings/recovery_kit/status_never_created"),
        "registered" => S.Get("settings/recovery_kit/status_registered"),
        "registered-no-escrow" => S.Get("settings/recovery_kit/status_registered_no_escrow"),
        "replacement-pending" =>
            S.Format("settings/recovery_kit/status_replacement_pending", PendingDays),
        // A kind this build does not know is a NEWER nest/app pairing, not a corrupt
        // read. Say the honest thing rather than inventing a state.
        _ => S.Get("settings/recovery_kit/status_loading"),
    };

    /// <summary>
    /// Whole days left in a pending window — the shared rounding rule
    /// (<c>recovery_pending_days_remaining</c> over
    /// <c>fauna_client_recovery::replacement::days_remaining_from</c>), not
    /// re-derived here: only <c>now</c> is this view model's to own, matching
    /// <c>pending_lands_at</c>'s own contract of a caller-supplied clock.
    /// </summary>
    public string PendingDays =>
        Status?.pendingLandsAt is { } landsAt
            ? FaunaFfiMethods
                .RecoveryPendingDaysRemaining(landsAt, DateTimeOffset.UtcNow.ToUnixTimeSeconds())
                .ToString()
            : "0";

    /// <summary>
    /// The phrase field renders while a ceremony that consumes one is reachable —
    /// replace, stolen, veto or the no-escrow re-seal. Apple's gate, widened by the
    /// same reasoning as <see cref="StolenVisible"/>: an unread status must not take
    /// the field away from the ceremony that needs it most.
    /// </summary>
    public bool PhraseFieldVisible =>
        Status is not { } s
        || s.allowsReplace || s.allowsStolen || s.allowsEscrowReseal
        || s.pendingLandsAt is not null;

    /// <summary>
    /// Whether the succession trigger and its confirm gate render. Per
    /// <c>settings.md</c> § Recovery kit the stolen action belongs to "stolen (any)"
    /// — <b>every</b> state — and the shared predicate makes that literal:
    /// <c>allows_stolen</c> is unconditionally true.
    ///
    /// <para>⚠ Deliberately also true while the status is UNREAD, which is where this
    /// diverges from apple's section (which paints nothing until its chain read
    /// lands). The divergence is on the merits, not incidental: this ceremony exists
    /// for an owner a thief has locked out, the authenticated chain read is exactly
    /// what fails for such an owner, and the ceremony's authorization is the KIT, not
    /// the status — <c>succession_succeed_with_held_kit</c> deliberately does no
    /// status re-read of its own ("the kit is the whole authorization, and a chain
    /// read here would only add a round trip a locked-out owner can fail on"). Hiding
    /// the trigger behind a read that may never succeed would withhold the affordance
    /// precisely from the person it is for. It also removes a real race: the section
    /// paints as soon as it mounts, and the journey's own visibility gate is a
    /// point-in-time read. Only a status that POSITIVELY says stolen is disallowed
    /// hides it.</para>
    /// </summary>
    public bool StolenVisible => Status is not { } s || s.allowsStolen;

    /// <summary>
    /// Whether <c>identity-stolen-button</c> is armed. The confirm gate is a
    /// <b>second</b> condition, not a replacement for the status one —
    /// <c>allowsStolen</c> is unconditionally true, so this gate is the only thing
    /// between a stray click and a re-pointed account.
    /// </summary>
    public bool StolenArmed =>
        string.Equals(StolenConfirmInput, StolenConfirmWord, StringComparison.Ordinal) && !Busy;

    partial void OnStatusChanged(FfiRecoveryKitStatus? value)
    {
        OnPropertyChanged(nameof(StatusLine));
        OnPropertyChanged(nameof(PendingDays));
        OnPropertyChanged(nameof(PhraseFieldVisible));
        OnPropertyChanged(nameof(StolenVisible));
    }

    partial void OnSweepViewChanged(FfiSweepView? value)
    {
        OnPropertyChanged(nameof(SweepOutcomeLine));
        OnPropertyChanged(nameof(SweepUnattestedLine));
        OnPropertyChanged(nameof(SweepOwesWork));
    }

    partial void OnStolenConfirmInputChanged(string value) => OnPropertyChanged(nameof(StolenArmed));

    partial void OnBusyChanged(bool value) => OnPropertyChanged(nameof(StolenArmed));

    /// <summary>
    /// Read the section's state from the <b>registration chain</b>, never a local
    /// flag — so a kit created on another device is reflected here.
    /// </summary>
    public async Task LoadStatusAsync()
    {
        Loading = true;
        try
        {
            Status = await _nest.RecoveryKitStatusAsync();
        }
        catch (Exception ex)
        {
            // A section that cannot read its chain paints no state and says why; it
            // must not fall back to a state it did not observe, because every
            // action's enablement hangs off that read.
            Status = null;
            ShowGuardedError(ex);
        }
        finally
        {
            Loading = false;
        }
    }

    /// <summary>
    /// Write <see cref="ViewModelBase.ErrorMessage"/> — UNLESS a stolen-ceremony
    /// persist-failure message is still pending acknowledgment
    /// (<see cref="StolenPersistFailurePending"/>), in which case the write is
    /// dropped rather than clobbering the only surviving copy of the successor's
    /// key. Every writer of the error slot in this view model, other than the
    /// park write itself inside <see cref="SucceedWithHeldKitAsync"/>'s
    /// persist-failure arm, MUST call this — never <c>SetError</c>/<c>ShowError</c>
    /// directly. Mirrors apple's <c>setErrorText</c> guard. Internal, not
    /// private, for the same testability reason as
    /// <see cref="StolenPersistFailurePending"/>: every other writer that reaches
    /// this guard needs a live <c>_nest</c> call, so a probe exercises the guard
    /// itself directly instead of driving a ceremony through a fake RPC client.
    /// </summary>
    internal void SetGuardedError(string? message)
    {
        if (StolenPersistFailurePending) return;
        SetError(message);
    }

    private void ShowGuardedError(Exception ex) => SetGuardedError(Strings.Error(ex));

    /// <summary>
    /// Discharge a still-pending persist-failure message: the user left the
    /// Account sub-page (<c>OnNavigatedFrom</c>, via <see cref="ClearHeldSecrets"/>)
    /// or the signed-in identity changed (<see cref="ResetForIdentityChange"/>,
    /// which calls <see cref="ClearHeldSecrets"/> first) — either way they had the
    /// whole visit to read or copy the successor's key. A no-op when nothing is
    /// pending. Folded into <see cref="ClearHeldSecrets"/> rather than called
    /// separately at each of its call sites, mirroring apple's
    /// <c>acknowledgeStolenFailedMessage()</c>/<c>clearHeldSecrets()</c> pairing —
    /// which is also what keeps <see cref="ResetForIdentityChange"/>'s own
    /// <c>SetGuardedError(null)</c> from being dropped by its own guard, since the
    /// discharge always runs first. Internal, not private, for the same
    /// testability reason as <see cref="StolenPersistFailurePending"/>.
    /// </summary>
    internal void AcknowledgeStolenPersistFailure()
    {
        StolenPersistFailurePending = false;
    }

    /// <summary>
    /// Drop everything a <b>different identity</b> put here — held secrets, and the
    /// status those secrets were read against.
    ///
    /// <para>The extra thing this does over <see cref="ClearHeldSecrets"/> is drop
    /// <see cref="Status"/>, and that is the load-bearing half: every action's
    /// enablement hangs off it, so carrying the previous account's status into this
    /// one does not render a <i>weaker</i> answer, it renders the <b>wrong</b> one —
    /// the section offers ceremonies the signed-in account cannot run and withholds
    /// ones it can.</para>
    /// </summary>
    public void ResetForIdentityChange()
    {
        ClearHeldSecrets();
        Status = null;
        SetGuardedError(null);
    }

    /// <summary>
    /// Drop everything the screen was holding. Called when the page goes away: the
    /// minted secret must not survive the view that displayed it, and the two typed
    /// buffers are a recovery phrase and a confirm token.
    /// </summary>
    public void ClearHeldSecrets()
    {
        MintedSecretHex = null;
        MintedLandsAt = null;
        MintedEscrowStored = true;
        PhraseInput = string.Empty;
        StolenConfirmInput = string.Empty;
        LandedSuccession = null;
        AcknowledgeStolenPersistFailure();
    }

    /// <summary><c>recovery-kit-create-button</c> (no phrase) and
    /// <c>recovery-kit-replace-button</c> (the phrase in hand) — one ceremony, two
    /// authorization arms.</summary>
    public async Task CreateOrReplaceKitAsync(bool usingHeldPhrase)
    {
        if (Busy) return;
        if (usingHeldPhrase && string.IsNullOrEmpty(PhraseInput))
        {
            SetGuardedError(S.Get("settings/recovery_kit/kit_phrase_required"));
            return;
        }
        await RunMintAsync(() => _nest.RecoveryCreateKitAsync(usingHeldPhrase ? PhraseInput : null));
    }

    /// <summary><c>recovery-kit-lost-button</c> — opens the 30-day window rather than
    /// taking effect now. Still mints and shows a secret immediately.</summary>
    public async Task RequestSeedAloneReplacementAsync()
    {
        if (Busy) return;
        await RunMintAsync(() => _nest.RecoveryRequestSeedAloneReplacementAsync());
    }

    /// <summary><c>recovery-pending-veto-button</c> — contest a pending
    /// replacement.</summary>
    public async Task VetoPendingReplacementAsync()
    {
        if (Busy) return;
        if (string.IsNullOrEmpty(PhraseInput))
        {
            SetGuardedError(S.Get("settings/recovery_kit/kit_phrase_required"));
            return;
        }
        Busy = true;
        try
        {
            await _nest.RecoveryVetoPendingReplacementAsync(PhraseInput);
            PhraseInput = string.Empty;
            SetGuardedError(null);
            await LoadStatusAsync();
        }
        catch (Exception ex)
        {
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary><c>recovery-kit-escrow-reseal-button</c> — the no-escrow repair.
    /// Restores phrase recovery <b>without</b> retiring the kit in hand, which is why
    /// it is not a create.</summary>
    public async Task ResealEscrowWithHeldKitAsync()
    {
        if (Busy) return;
        if (string.IsNullOrEmpty(PhraseInput))
        {
            SetGuardedError(S.Get("settings/recovery_kit/kit_phrase_required"));
            return;
        }
        Busy = true;
        try
        {
            await _nest.RecoveryResealEscrowWithHeldKitAsync(PhraseInput);
            PhraseInput = string.Empty;
            SetGuardedError(null);
            await LoadStatusAsync();
        }
        catch (Exception ex)
        {
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary>
    /// <c>identity-stolen-button</c> — the irreversible succession ceremony.
    ///
    /// <para>Both gates are re-checked <b>here</b>, not only in the render: a
    /// disabled control emits no gesture, but a test agent driving the id reaches
    /// this handler, and an irreversible ceremony must refuse out loud rather than
    /// run (<c>settings.md</c> § Recovery kit).</para>
    ///
    /// <para>On success the account belongs to a new identity and this session's
    /// bearers were revoked inside the nest's own transaction — so the caller
    /// switches to the successor (<paramref name="onSucceeded"/> carries its actor
    /// id). ⚠ <b>Except</b> when the device failed to save the successor seed
    /// (<c>persisted == false</c>): tearing the session down then takes the only copy
    /// of the key with it, so the secret goes on screen and the session stays up.
    /// <paramref name="onSucceeded"/> is called only on the arm where the tear-down
    /// is safe.</para>
    ///
    /// <para>⚠ <see cref="SuccessionHandoff.Record"/> happens on BOTH arms and before
    /// either — the succession landed either way, so the owed kit, the sweep report
    /// and the predecessor id are owed either way.</para>
    ///
    /// <para>Every ceremony that ran answers with its typed
    /// <see cref="FfiStolenOutcome"/>, folded here by arm (<c>settings.md</c>
    /// § Recovery kit → <i>The ceremony's outcome is headlined by its arm</i>): the
    /// two landed halves above, and every other arm's shared sentence on
    /// <c>error-message</c> verbatim, wrapping nothing — parked when the record says
    /// it carries the only copy of the successor seed. apple's
    /// <c>applyStolenOutcome</c> and linux's <c>dispatch_unlanded</c> are the
    /// twins.</para>
    /// </summary>
    public async Task SucceedWithHeldKitAsync(
        string? predecessorActorIdHex, Func<string, Task>? onSucceeded)
    {
        if (Busy) return;
        if (!string.Equals(StolenConfirmInput, StolenConfirmWord, StringComparison.Ordinal))
        {
            SetGuardedError(S.Get("settings/recovery_kit/stolen_confirm_placeholder"));
            return;
        }
        if (string.IsNullOrEmpty(PhraseInput))
        {
            SetGuardedError(S.Get("settings/recovery_kit/kit_phrase_required"));
            return;
        }
        Busy = true;
        // From here the nest may commit at any moment, and this session's next
        // re-mint is then refused as superseded — held back until the result below
        // is handled (StolenCeremonyHold).
        _ceremonyHold.CeremonyStarted();
        var adopted = false;
        try
        {
            var outcome = await _nest.SuccessionSucceedWithHeldKitAsync(PhraseInput);
            if (outcome.landed is not { } landed)
            {
                // Nothing moved, landed for another, undecided: the shared sentence
                // already carries its own headline (and only nothing-moved's says
                // "failed"), so a wrapper here would be a second one — false on two
                // of the three. The undecided arm whose persist was not verified
                // carries the only copy of the successor seed: parked, decided by
                // the record's flag and never by its kind or key, and no switch.
                var sentence = outcome.message is { } message ? S.Resolve(message) : outcome.kind;
                if (outcome.carriesTheOnlySeed) ParkStolenMessage(sentence);
                else SetGuardedError(sentence);
            }
            else
            {
                PhraseInput = string.Empty;
                StolenConfirmInput = string.Empty;
                LandedSuccession = landed;
                // Hand the ceremony's survivors over BEFORE the switch below:
                // `onSucceeded` tears this session (and this view model) down
                // moments from now, and everything recorded here is declared to
                // outlive that.
                SuccessionHandoff.Record(landed, predecessorActorIdHex);
                if (landed.persisted)
                {
                    adopted = true;
                    SetGuardedError(null);
                    if (onSucceeded is not null) await onSucceeded(landed.newActorIdHex);
                }
                else
                {
                    // The one landed arm where the secret must go on screen. Never
                    // phrased as "nothing happened" — the account DID move.
                    MintedSecretHex = landed.successorSecretHex;
                    MintedEscrowStored = true;
                    ParkStolenMessage(S.Format(
                        "settings/recovery_kit/stolen_persist_failed", landed.successorSecretHex));
                }
            }
        }
        catch (Exception ex)
        {
            // No ceremony outcome reaches here: every ceremony that ran is an
            // outcome above, never an exception. Only a failure BEFORE it could
            // start (no connection, unparseable secret bytes) does — or a throw
            // from the caller's own switch.
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
        await _ceremonyHold.CeremonyEndedAsync(adopted, messageParked: StolenPersistFailurePending);
    }

    /// <summary>
    /// Park a sentence that carries the only copy of the successor's seed — the
    /// persist-failure message and the undecided-unsaved outcome alike — on
    /// <c>error-message</c>, where every other writer then leaves it alone
    /// (<c>settings.md</c> § Recovery kit → <i>The persist-failure message survives
    /// the page</i>). apple's <c>parkStolenMessage</c> and linux's
    /// <c>park_stolen_failed_message</c> are the twins.
    ///
    /// <para>The write is deliberately unguarded (<see cref="SetGuardedError"/>
    /// would refuse its own display), and <see cref="StolenPersistFailurePending"/>
    /// flips only AFTER it: the write reaches the page's <c>RenderError</c> funnel
    /// synchronously (via the <c>ErrorMessage</c> property change), so setting the
    /// flag first would have the guard there refuse the very message it exists to
    /// protect.</para>
    ///
    /// <para>⚠ Never through <see cref="ViewModelBase.SetError"/>, which logs its
    /// whole argument: the seed would land in the shared log ring and its on-disk
    /// file, against <c>ShellLog</c>'s redaction rule (never secrets or keys). The
    /// log gets a seed-free line instead.</para>
    /// </summary>
    private void ParkStolenMessage(string sentence)
    {
        ErrorMessage = sentence;
        ShellLog.Error(GetType().Name,
            "the stolen ceremony parked the successor's only secret on error-message");
        StolenPersistFailurePending = true;
    }

    /// <summary>
    /// <c>recovery-kit-sweep-retry-button</c> — finish a sweep the ceremony left
    /// unfinished (<c>settings.md</c> § Recovery kit → <i>Finishing an unfinished
    /// group sweep</i>).
    ///
    /// <para><b>The answer is the product here, not a side effect.</b> The button
    /// renders on unfinished work and deliberately NOT on whether this device can
    /// retry, so a press that cannot sweep must still say something. Only the
    /// <c>swept</c> arm says nothing on the error surface: its outcome renders
    /// through <see cref="SweepOutcomeLine"/>/<see cref="SweepUnattestedLine"/>
    /// instead, off the FRESH view this replaces <see cref="SweepView"/> with —
    /// <see cref="SuccessionHandoff.ReplaceSweep"/> carries the same replacement
    /// into <c>data.succession_sweep</c>, so a journey asserting either witness
    /// sees the finished pass.</para>
    /// </summary>
    public async Task RetrySweepAsync()
    {
        if (Busy) return;
        Busy = true;
        try
        {
            var answer = await _nest.SuccessionRetryGroupSweepAsync();
            if (answer.sweep is { } fresh && answer.sweepStateJson is { } json)
            {
                SweepView = fresh;
                SuccessionHandoff.ReplaceSweep(fresh, json);
            }
            SetGuardedError(answer.message is { } message ? S.Resolve(message) : null);
        }
        catch (Exception ex)
        {
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary>
    /// Discharge the group sweep a <b>relaunch adoption</b> owes — an unbidden press
    /// of <c>recovery-kit-sweep-retry-button</c>, run by the successor's first
    /// authenticated session BEFORE the kit's mint (the ceremony's own order: sweep,
    /// then kit) — <c>succession-propagation.md</c> § Propagation → <i>Own device
    /// fleet</i>, the relaunch-adoption clause. apple's <c>dischargeOwedSweep</c> is
    /// the twin.
    ///
    /// <para>Nothing happens unless <see cref="SuccessionHandoff.SweepOwedTo"/> names
    /// this session, so an ordinary visit pays one comparison. The answer folds where
    /// a press would: its sentence (if any) on <c>error-message</c>, and a report
    /// parked in <see cref="SweepView"/> and <c>data.succession_sweep</c> — the fresh
    /// one when it swept, else an arm that still owes work so the retry button
    /// renders (shared Rust picks which, never this file).</para>
    ///
    /// <para>The busy guard comes BEFORE the claim, as in
    /// <see cref="DischargeOwedSuccessionKitAsync"/>: a busy view model leaves the
    /// obligation standing for the next hydrate rather than spending it on a pass
    /// that cannot run.</para>
    /// </summary>
    public async Task DischargeOwedSweepAsync(string? sessionActorIdHex)
    {
        if (sessionActorIdHex is null
            || !string.Equals(SuccessionHandoff.SweepOwedTo, sessionActorIdHex, StringComparison.Ordinal))
            return;
        if (Busy)
        {
            FaunaApp.Core.Logs.ShellLog.Info("RecoveryKitViewModel",
                "[succession-sweep] discharge deferred (busy) — obligation kept for the next pass");
            return;
        }
        if (!SuccessionHandoff.ClaimOwedSweep(sessionActorIdHex)) return;
        Busy = true;
        try
        {
            var owed = await _nest.SuccessionDischargeOwedSweepAsync();
            SweepView = owed.parked;
            SuccessionHandoff.ReplaceSweep(owed.parked, owed.parkedStateJson);
            FaunaApp.Core.Logs.ShellLog.Info("RecoveryKitViewModel",
                $"[succession-sweep] owed sweep answered {owed.answer.kind}");
            SetGuardedError(owed.answer.message is { } message ? S.Resolve(message) : null);
        }
        catch (Exception ex)
        {
            // Only reaching the nest (or a session with no secret) throws, before
            // anything ran — put the obligation back rather than spend it.
            SuccessionHandoff.RearmOwedSweep(sessionActorIdHex);
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
    }

    /// <summary>
    /// Mint and show the successor's recovery kit — the <b>closing act</b> of an
    /// identity succession, run on the successor's own first authenticated session
    /// (<c>identity-succession.md</c> § The RecoveryKey → <i>At succession</i>).
    ///
    /// <para>Nothing happens unless <see cref="SuccessionHandoff.KitOwed"/> is set,
    /// so an ordinary visit to this section pays one boolean check. A <i>silent</i>
    /// background mint is not an option: the old kit retired with the old identity
    /// and the nest deleted its escrow row in the same transaction, so between the
    /// succession and this mint the account has no route back but the 30-day
    /// seed-alone window — and a mint nobody was <i>shown</i> leaves a kit nobody
    /// holds, which is strictly worse than never-created.</para>
    ///
    /// <para>⚠ <paramref name="sessionActorIdHex"/> is not decoration — it is what
    /// keeps the OUTGOING session from taking the obligation. This section is live
    /// while the ceremony runs and stays live through the teardown, so it
    /// re-hydrates as the departing identity and would otherwise mint against a nest
    /// that has just revoked its bearers.</para>
    /// </summary>
    public async Task DischargeOwedSuccessionKitAsync(string? sessionActorIdHex)
    {
        // The busy guard comes BEFORE the claim, and it is the load-bearing one:
        // `CreateOrReplaceKitAsync` returns early while another ceremony is in
        // flight, so claiming first would consume the obligation without minting
        // anything — the one way this step can be lost silently rather than retried.
        if (Busy)
        {
            FaunaApp.Core.Logs.ShellLog.Debug("RecoveryKitViewModel",
                "[discharge] skipped: already Busy");
            return;
        }
        if (!SuccessionHandoff.ClaimOwedKit(sessionActorIdHex))
        {
            FaunaApp.Core.Logs.ShellLog.Debug("RecoveryKitViewModel",
                "[discharge] ClaimOwedKit refused (not owed, or actor mismatch)");
            return;
        }
        FaunaApp.Core.Logs.ShellLog.Info("RecoveryKitViewModel",
            "[discharge] claimed — minting the successor's kit");
        // `RecoveryCreateKitAsync` reads the chain head and picks its own arm, so
        // this stays correct even though the successor is never-created by
        // construction. The secret lands in `MintedSecretHex` — i.e. on screen —
        // through the same mint every other ceremony uses.
        await CreateOrReplaceKitAsync(usingHeldPhrase: false);
        FaunaApp.Core.Logs.ShellLog.Info("RecoveryKitViewModel",
            $"[discharge] mint returned, MintedSecretHex={(MintedSecretHex is null ? "<null>" : "set")}");
    }

    /// <summary>Run a kit-minting ceremony and surface its secret once.</summary>
    private async Task RunMintAsync(Func<Task<FfiMintedKit>> ceremony)
    {
        Busy = true;
        try
        {
            FaunaApp.Core.Logs.ShellLog.Debug("RecoveryKitViewModel", "[mint] ceremony() calling");
            var kit = await ceremony();
            FaunaApp.Core.Logs.ShellLog.Debug("RecoveryKitViewModel", "[mint] ceremony() returned");
            // Display BEFORE anything else can fail: at this instant the secret
            // exists nowhere else in the world, and a ceremony whose kit is never
            // shown leaves a kit nobody holds.
            MintedSecretHex = kit.secretHex;
            MintedEscrowStored = kit.escrowStored;
            MintedLandsAt = kit.landsAt;
            PhraseInput = string.Empty;
            SetGuardedError(null);
            await LoadStatusAsync();
        }
        catch (Exception ex)
        {
            ShowGuardedError(ex);
        }
        finally
        {
            Busy = false;
        }
    }
}
