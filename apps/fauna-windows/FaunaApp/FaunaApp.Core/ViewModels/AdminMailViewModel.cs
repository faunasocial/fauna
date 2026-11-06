using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

// ── Public mirror records ────────────────────────────────────────────────────
//
// The UniFFI-generated *View records + MailPolicyAction + MailPolicySnapshot are
// `internal` to FaunaApp.Core, so the admin-mail XAML page (which lives in the
// separate FaunaApp app project and CANNOT see internal types) needs a public
// editable-value carrier per group. These mirror the internal *View field shapes
// 1:1 with PascalCase properties, exposing string lists as IReadOnlyList and the
// retry-schedule as IReadOnlyList<ulong>. Each carries `From` (snapshot → mirror)
// and `ToView` (mirror → full-PUT view) round-trip helpers. Same pattern as
// MailSpamViewModel's MailSpamTrainingRow / AdminAliasesViewModel's ForwarderRowVm.

/// <summary>Editable Spam / inbound-perimeter policy values (<c>admin-mail</c> spam
/// section → <c>SaveSpam</c> full PUT). Mirrors the internal <c>SpamPolicyView</c>.</summary>
public sealed record SpamPolicyValues(
    uint MaxScoreBeforeSpamFolder,
    uint MaxScoreBeforeReject,
    IReadOnlyList<string> DnsblServers,
    bool RejectNoRdns,
    bool GreylistEnabled,
    uint GreylistDelaySecs,
    uint MaxConnPerMin,
    string FcrdnsMode,
    bool HeloIdentityRequired,
    bool RejectFcrdnsFail,
    uint MaxMessageBytes,
    // Per-user training (Tier-2 combined-score knobs; same put_spam_policy).
    uint BayesianWeightMilli,
    uint BayesianMinSamples,
    uint BayesianFullConfidenceSamples,
    uint TrainingHistoryRetentionDays,
    // Recipient-whitelist unlisted-recipient penalty in points (0 = off, default);
    // added to a catch-all recipient's combined score at the Go MTA loop
    // (mail-spam.md § Unlisted-recipient penalty). Same put_spam_policy full-PUT.
    uint UnlistedRecipientPenalty,
    // Standing publish of the deployment spam baseline (default off). Not
    // rendered yet: carried from the hydrated value so a save never turns it
    // off, which would withdraw the baseline (mail-spam.md § Cold start Path 2).
    bool BaselineStandingPublish)
{
    internal static SpamPolicyValues From(SpamPolicyView v) => new(
        v.@maxScoreBeforeSpamFolder,
        v.@maxScoreBeforeReject,
        v.@dnsblServers,
        v.@rejectNoRdns,
        v.@greylistEnabled,
        v.@greylistDelaySecs,
        v.@maxConnPerMin,
        v.@fcrdnsMode,
        v.@heloIdentityRequired,
        v.@rejectFcrdnsFail,
        v.@maxMessageBytes,
        v.@bayesianWeightMilli,
        v.@bayesianMinSamples,
        v.@bayesianFullConfidenceSamples,
        v.@trainingHistoryRetentionDays,
        v.@unlistedRecipientPenalty,
        v.@baselineStandingPublish);

    internal SpamPolicyView ToView() => new(
        @maxScoreBeforeSpamFolder: MaxScoreBeforeSpamFolder,
        @maxScoreBeforeReject: MaxScoreBeforeReject,
        @dnsblServers: DnsblServers.ToArray(),
        @rejectNoRdns: RejectNoRdns,
        @greylistEnabled: GreylistEnabled,
        @greylistDelaySecs: GreylistDelaySecs,
        @maxConnPerMin: MaxConnPerMin,
        @fcrdnsMode: FcrdnsMode,
        @heloIdentityRequired: HeloIdentityRequired,
        @rejectFcrdnsFail: RejectFcrdnsFail,
        @maxMessageBytes: MaxMessageBytes,
        @bayesianWeightMilli: BayesianWeightMilli,
        @bayesianMinSamples: BayesianMinSamples,
        @bayesianFullConfidenceSamples: BayesianFullConfidenceSamples,
        @trainingHistoryRetentionDays: TrainingHistoryRetentionDays,
        @unlistedRecipientPenalty: UnlistedRecipientPenalty,
        @baselineStandingPublish: BaselineStandingPublish);
}

/// <summary>Editable Auth / enforcement policy values (→ <c>SaveAuth</c> full PUT).
/// Mirrors the internal <c>AuthPolicyView</c>.</summary>
public sealed record AuthPolicyValues(
    bool EnforceDmarc,
    bool EnforceDmarcQuarantine,
    bool EnforceSpfHardfail,
    bool EnforceDkim,
    bool LogOnly,
    uint MaxAuthFailuresPerMinute,
    // Carried through the load→save round-trip so a full-PUT auth save never resets
    // the nest's configured per-IP connection cap. Not yet user-editable — the
    // per-IP-cap widget is a separate (design-gated) entrusted mail-admin track.
    uint MaxConnPerIp)
{
    internal static AuthPolicyValues From(AuthPolicyView v) => new(
        v.@enforceDmarc,
        v.@enforceDmarcQuarantine,
        v.@enforceSpfHardfail,
        v.@enforceDkim,
        v.@logOnly,
        v.@maxAuthFailuresPerMinute,
        v.@maxConnPerIp);

    internal AuthPolicyView ToView() => new(
        @enforceDmarc: EnforceDmarc,
        @enforceDmarcQuarantine: EnforceDmarcQuarantine,
        @enforceSpfHardfail: EnforceSpfHardfail,
        @enforceDkim: EnforceDkim,
        @logOnly: LogOnly,
        @maxAuthFailuresPerMinute: MaxAuthFailuresPerMinute,
        @maxConnPerIp: MaxConnPerIp);
}

/// <summary>Editable Submission-quota policy values (→ <c>SaveSubmission</c> full PUT).
/// Mirrors the internal <c>SubmissionPolicyView</c>.</summary>
public sealed record SubmissionPolicyValues(
    uint MaxPerDay,
    uint MaxRecipientsPerMessage)
{
    internal static SubmissionPolicyValues From(SubmissionPolicyView v) => new(
        v.@maxPerDay,
        v.@maxRecipientsPerMessage);

    internal SubmissionPolicyView ToView() => new(
        @maxPerDay: MaxPerDay,
        @maxRecipientsPerMessage: MaxRecipientsPerMessage);
}

/// <summary>Editable IMAP-server policy values (→ <c>SaveImap</c> full PUT).
/// Mirrors the internal <c>ImapPolicyView</c>.</summary>
public sealed record ImapPolicyValues(
    uint IdleTimeoutSecs,
    uint TombstoneRetentionDays,
    string DeleteNonempty,
    uint BodystructureCacheMax,
    ulong StorageBytesDefault,
    uint MessageCountDefault)
{
    internal static ImapPolicyValues From(ImapPolicyView v) => new(
        v.@idleTimeoutSecs,
        v.@tombstoneRetentionDays,
        v.@deleteNonempty,
        v.@bodystructureCacheMax,
        v.@storageBytesDefault,
        v.@messageCountDefault);

    internal ImapPolicyView ToView() => new(
        @idleTimeoutSecs: IdleTimeoutSecs,
        @tombstoneRetentionDays: TombstoneRetentionDays,
        @deleteNonempty: DeleteNonempty,
        @bodystructureCacheMax: BodystructureCacheMax,
        @storageBytesDefault: StorageBytesDefault,
        @messageCountDefault: MessageCountDefault);
}

/// <summary>Editable Outbound-delivery policy values (→ <c>SaveOutbound</c> full PUT).
/// Mirrors the internal <c>OutboundPolicyView</c>.</summary>
public sealed record OutboundPolicyValues(
    IReadOnlyList<ulong> RetrySchedule,
    uint PermanentFailureTimeoutHours,
    uint DelayWarningAtHours,
    uint NdrRateLimitDays,
    bool SuppressNdrSpfHardfail,
    bool SuppressNdrDmarcReject,
    bool PostmasterCcBounces,
    bool TlsrptSendReports,
    bool Ipv6Enabled,
    IReadOnlyList<string> Treat5xxAsTransient)
{
    internal static OutboundPolicyValues From(OutboundPolicyView v) => new(
        v.@retryScheduleSeconds,
        v.@permanentFailureTimeoutHours,
        v.@delayWarningAtHours,
        v.@ndrRateLimitDays,
        v.@suppressNdrSpfHardfail,
        v.@suppressNdrDmarcReject,
        v.@postmasterCcBounces,
        v.@tlsrptSendReports,
        v.@ipv6Enabled,
        v.@treat5xxAsTransient);

    internal OutboundPolicyView ToView() => new(
        @retryScheduleSeconds: RetrySchedule.ToArray(),
        @permanentFailureTimeoutHours: PermanentFailureTimeoutHours,
        @delayWarningAtHours: DelayWarningAtHours,
        @ndrRateLimitDays: NdrRateLimitDays,
        @suppressNdrSpfHardfail: SuppressNdrSpfHardfail,
        @suppressNdrDmarcReject: SuppressNdrDmarcReject,
        @postmasterCcBounces: PostmasterCcBounces,
        @tlsrptSendReports: TlsrptSendReports,
        @ipv6Enabled: Ipv6Enabled,
        @treat5xxAsTransient: Treat5xxAsTransient.ToArray());
}

/// <summary>Editable nest-side alias policy values (→ <c>SaveAlias</c> full PUT).
/// Mirrors the internal <c>AliasPolicyView</c>.</summary>
public sealed record AliasPolicyValues(
    uint ExactAliasesMax,
    IReadOnlyList<string> ReservedLocalParts,
    bool SubaddressingEnabled,
    bool WildcardPrefixEnabled)
{
    internal static AliasPolicyValues From(AliasPolicyView v) => new(
        v.@exactAliasesMax,
        v.@reservedLocalParts,
        v.@subaddressingEnabled,
        v.@wildcardPrefixEnabled);

    internal AliasPolicyView ToView() => new(
        @exactAliasesMax: ExactAliasesMax,
        @reservedLocalParts: ReservedLocalParts.ToArray(),
        @subaddressingEnabled: SubaddressingEnabled,
        @wildcardPrefixEnabled: WildcardPrefixEnabled);
}

/// <summary>The deployment's own Microsoft app registration for the Outlook import
/// (<c>admin-mail</c> Microsoft section → <c>SetMicrosoftRegistration</c>): the two stored
/// strings the fields seed from (empty when none is set) and whether the project's
/// registration is the one in force. Mirrors the internal <c>MicrosoftRegistrationView</c>.</summary>
public sealed record MicrosoftRegistrationValues(string ClientId, string Tenant, bool IsDefault)
{
    internal static MicrosoftRegistrationValues From(MicrosoftRegistrationView v) => new(
        v.@clientId,
        v.@tenant,
        v.@isDefault);
}

/// <summary>Editable deployment-wide DMARC policy values (→ <c>SaveDmarc</c> full PUT).
/// Mirrors the internal <c>DmarcPolicyView</c>. Only the four report fields are edited on
/// the page (forensic opt-in, <c>fo=</c> mode, the two retention windows); every other
/// record tag, and the outbound aggregate-report switch, is carried through
/// <see cref="ToView"/> unchanged so a save never resets it.</summary>
public sealed record DmarcPolicyValues(
    string PolicyMode,
    string SubdomainPolicyMode,
    byte Pct,
    string AdkimMode,
    string AspfMode,
    string? RuaDestination,
    bool RufPublish,
    string? RufDestination,
    string FoMode,
    uint RiSeconds,
    uint AggregateRetentionDays,
    uint ForensicRetentionDays,
    bool OutboundAggregateEnabled)
{
    internal static DmarcPolicyValues From(DmarcPolicyView v) => new(
        v.@policyMode,
        v.@subdomainPolicyMode,
        v.@pct,
        v.@adkimMode,
        v.@aspfMode,
        v.@ruaDestination,
        v.@rufPublish,
        v.@rufDestination,
        v.@foMode,
        v.@riSeconds,
        v.@aggregateRetentionDays,
        v.@forensicRetentionDays,
        v.@outboundAggregateEnabled);

    internal DmarcPolicyView ToView() => new(
        @policyMode: PolicyMode,
        @subdomainPolicyMode: SubdomainPolicyMode,
        @pct: Pct,
        @adkimMode: AdkimMode,
        @aspfMode: AspfMode,
        @ruaDestination: RuaDestination,
        @rufPublish: RufPublish,
        @rufDestination: RufDestination,
        @foMode: FoMode,
        @riSeconds: RiSeconds,
        @aggregateRetentionDays: AggregateRetentionDays,
        @forensicRetentionDays: ForensicRetentionDays,
        @outboundAggregateEnabled: OutboundAggregateEnabled);
}

/// <summary>Read-only outcome of the last <c>PublishSpamBaseline</c> action
/// (<c>admin-mail-publish-spam-baseline-button</c>). Aggregate-only — never a
/// contributor identity. Mirrors the internal <c>BaselinePublishView</c>;
/// <c>Published</c> is the wire flag as the nest sent it.</summary>
public sealed record BaselinePublishValues(
    bool Published,
    uint Contributors,
    uint SampleCount,
    uint SkippedContributors,
    bool Deferred)
{
    internal static BaselinePublishValues From(BaselinePublishView v) => new(
        v.@published,
        v.@contributors,
        v.@sampleCount,
        v.@skippedContributors,
        v.@deferred);
}

/// <summary>Read-only state of the served spam baseline
/// (<c>admin-mail-spam-baseline-state</c>; mail-spam.md § Cold start Path 2 →
/// <i>Standing publish</i>). <c>null</c> on the VM until the state read answers (and on
/// a nest that refuses it). Mirrors the internal <c>BaselineStateView</c>.</summary>
public sealed record BaselineStateValues(
    bool Published,
    uint Contributors,
    uint SampleCount,
    long? PublishedAtMs,
    uint SkippedContributors,
    bool Deferred,
    bool Standing)
{
    internal static BaselineStateValues From(BaselineStateView v) => new(
        v.@published,
        v.@contributors,
        v.@sampleCount,
        v.@publishedAtMs,
        v.@skippedContributors,
        v.@deferred,
        v.@standing);
}

/// <summary>One painted row of the mail health readout (<c>admin-mail-health-check</c>):
/// the resolved label, the state word and the detail text, every sentence already
/// finished by shared Rust or the nest (<c>mail-deliverability.md</c> § The mail health
/// readout). Rows 5–6 are the heartbeats: the fold leaves their detail empty and the
/// stamp is painted there.</summary>
public sealed record MailHealthRow(string Label, string State, string Detail);

/// <summary>The painted mail health readout (<c>admin-mail-health-section</c>):
/// the <c>admin-mail-health-status</c> line, the check rows, and the delist URL
/// (present only while the outbound IP is listed → <c>admin-mail-health-delist-link</c>).
/// Projected from the internal <c>MailHealthView</c>.</summary>
public sealed record MailHealthValues(
    string StatusText,
    IReadOnlyList<MailHealthRow> Checks,
    string? DelistUrl);

/// <summary>
/// The admin <c>admin-mail</c> policy page (admin.md § Mail policy) — the admin's
/// single deployment-wide mail-policy console: flip the mail-enable toggle and persist
/// each of the seven full-PUT policy groups (spam/inbound-perimeter, auth/enforcement,
/// submission quotas, IMAP server, outbound delivery, and nest-side aliases). A dumb
/// projection over the shared <c>fauna_client_mail_settings::MailPolicyMachine</c>,
/// consumed through its UniFFI-generated <see cref="IMailPolicyMachine"/> seam
/// (machine-as-seam — no hand-written seam; the page builds the real machine, the unit
/// test fakes the interface). All projection / validation / WS-RPC sequencing lives in
/// shared Rust (priority #2: <c>get_mail_config</c> hydrate + the seven <c>put_*</c>
/// twins, the out-of-order spam-threshold rejection, the alias read/write twins); this
/// VM forwards the gestures and re-projects the snapshot after each.
///
/// Assembly-boundary rule: the internal <c>*View</c> / <c>MailPolicyAction</c> /
/// <c>MailPolicySnapshot</c> types stay inside FaunaApp.Core, so the public surface
/// uses primitive scalars + the public <c>*PolicyValues</c> mirror records only —
/// exactly as MailSpamViewModel exposes MailSpamTrainingRow + primitives. Mirrors
/// MailSpamViewModel / AdminAliasesViewModel's load + dispatch-then-reproject
/// conventions; lifts the flat linux admin-mail reference.
/// </summary>
public partial class AdminMailViewModel : ObservableObject
{
    private readonly IMailPolicyMachine _machine;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary><c>admin-mail-enabled-toggle</c> — the deployment-wide mail-enable
    /// flag, reflected from the snapshot after each load / action.</summary>
    [ObservableProperty] private bool _mailEnabled;

    /// <summary>The box cannot enable mail (a public box with no content scanners) —
    /// <c>MailPolicySnapshot.mailUnavailableHere</c>, computed once in shared Rust from
    /// <c>fauna.setup.status</c> and read here, never re-derived (admin.md § 6 Mail).</summary>
    [ObservableProperty] private bool _mailUnavailableHere;

    /// <summary>The enable toggle is refused: disabled, under
    /// <c>admin-mail-unavailable-notice</c>. Only while mail is off — where mail is
    /// already on the toggle stays live so the admin can still turn it off.</summary>
    public bool MailEnableRefused => !MailEnabled && MailUnavailableHere;

    /// <summary><c>admin-mail-enabled-toggle</c>'s <c>state</c> attr:
    /// <c>on</c> / <c>unavailable</c> / <c>off</c> (ui.yaml).</summary>
    public string MailEnabledToggleState =>
        MailEnabled ? "on" : MailEnableRefused ? "unavailable" : "off";

    /// <summary><c>admin-mail-auto-enable-new-users-toggle</c> — the deployment-wide
    /// "auto-enable mail for new users" policy (default-on; when on, a freshly-registered
    /// user's client auto-provisions its own mailbox on first setup). Reflected from the
    /// snapshot after each load / action. Read back from <c>fauna.setup.status</c>, not
    /// <c>get_mail_config</c> — the bridge never reads it (mail-policy-config.md § Tier-2).</summary>
    [ObservableProperty] private bool _autoEnableMailForNewUsers;

    /// <summary>Current persisted spam / inbound-perimeter policy. Seeded in the ctor
    /// from the machine's pre-hydrate snapshot (the shared catalog defaults), so it is
    /// never null pre-load and never duplicates the Rust catalog defaults in C#.</summary>
    public SpamPolicyValues Spam { get; private set; } = null!;

    /// <summary>Current persisted auth / enforcement policy.</summary>
    public AuthPolicyValues Auth { get; private set; } = null!;

    /// <summary>Current persisted submission-quota policy.</summary>
    public SubmissionPolicyValues Submission { get; private set; } = null!;

    /// <summary>Current persisted IMAP-server policy.</summary>
    public ImapPolicyValues Imap { get; private set; } = null!;

    /// <summary>Current persisted outbound-delivery policy.</summary>
    public OutboundPolicyValues Outbound { get; private set; } = null!;

    /// <summary>Current persisted nest-side alias policy.</summary>
    public AliasPolicyValues Alias { get; private set; } = null!;

    /// <summary>Current persisted deployment-wide DMARC policy.</summary>
    public DmarcPolicyValues Dmarc { get; private set; } = null!;

    /// <summary>The deployment's own Microsoft registration for the Outlook import
    /// (read back via <c>fauna.setup.status</c>).</summary>
    public MicrosoftRegistrationValues Microsoft { get; private set; } = null!;

    /// <summary>Outcome of the last publish-baseline action, or <c>null</c> until the
    /// admin clicks <c>admin-mail-publish-spam-baseline-button</c>. A transient action
    /// outcome (not persisted policy) — a later re-read clears it.</summary>
    public BaselinePublishValues? BaselinePublish { get; private set; }

    /// <summary>The served baseline's current state, or <c>null</c> until the state
    /// read answers (<c>admin-mail-spam-baseline-state</c>).</summary>
    public BaselineStateValues? BaselineState { get; private set; }

    /// <summary>The mail health readout, or <c>null</c> while the machine holds none
    /// (before the first hydrate) — the page then hides the whole section.</summary>
    public MailHealthValues? Health { get; private set; }

    /// <summary>The confirm button a first press armed (the shared
    /// <c>snapshot.armed</c>), or <c>null</c>: that button now reads its confirm sentence
    /// and the next press of it dispatches. Any other action or re-read disarms it in the
    /// shared machine, so every <see cref="Apply"/> re-projects it.</summary>
    internal MailPolicyConfirm? Armed { get; private set; }

    /// <summary>True while <c>admin-mail-health-warmup-reset-button</c> is the armed one.</summary>
    public bool WarmupResetArmed => Armed == MailPolicyConfirm.ResetWarmup;

    /// <summary>True while <c>admin-mail-publish-spam-baseline-button</c> is the armed one.</summary>
    public bool PublishSpamBaselineArmed => Armed == MailPolicyConfirm.PublishSpamBaseline;

    /// <summary>True while <c>admin-mail-list-unsubscribe-secret-rotate-button</c> is the
    /// armed one.</summary>
    public bool ListUnsubscribeSecretRotateArmed => Armed == MailPolicyConfirm.RotateListUnsubscribeSecret;

    /// <summary>True while <c>admin-mail-forward-srs-secret-rotate-button</c> is the armed one.</summary>
    public bool ForwardSrsSecretRotateArmed => Armed == MailPolicyConfirm.RotateSrsSecret;

    /// <summary>The last secret rotation's outcome line under the Secrets group
    /// (<c>mail-mass-mailing.md</c> § Secret rotation, <c>mail-forwarding.md</c> § SRS
    /// scheme), worded by the shared <c>secret_rotation_text</c>; <c>null</c> until a
    /// rotation ran.</summary>
    public string? SecretRotationLine { get; private set; }

    private readonly Func<long> _nowMs;

    internal AdminMailViewModel(IMailPolicyMachine machine, Func<long>? nowMs = null)
    {
        _machine = machine;
        _nowMs = nowMs ?? (() => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        // Seed the bound state from the machine's pre-hydrate snapshot — the shared
        // `MailPolicyMachine` starts on `MailPolicySnapshot::defaults()` (the catalog
        // defaults, no I/O), so this is the single source of truth for pre-load values
        // and avoids re-encoding the Rust catalog defaults in C# (priority #2).
        Apply(_machine.Snapshot());
    }

    /// <summary>Initial page load: hydrate from <c>get_mail_config</c>, then project the
    /// snapshot. The transport already tolerates the post-login connect race for a single
    /// RPC (transport.md § Request lifecycle step 3) — no app-level retry needed here.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            await _machine.Hydrate();
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Flip the deployment-wide mail-enable toggle
    /// (<c>admin-mail-enabled-toggle</c> → <c>set_mail_enabled</c>).</summary>
    public Task SetMailEnabledAsync(bool enabled)
        => DispatchAsync(new MailPolicyAction.SetMailEnabled(enabled));

    /// <summary>Flip the deployment-wide "auto-enable mail for new users" policy
    /// (<c>admin-mail-auto-enable-new-users-toggle</c> →
    /// <c>set_auto_enable_mail_for_new_users</c>; persisted via <c>fauna.bridges</c>,
    /// re-read via <c>fauna.setup.status</c>).</summary>
    public Task SetAutoEnableMailForNewUsersAsync(bool enabled)
        => DispatchAsync(new MailPolicyAction.SetAutoEnableMailForNewUsers(enabled));

    /// <summary>Persist the whole spam / inbound-perimeter sub-struct (full PUT).</summary>
    public Task SaveSpamAsync(SpamPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveSpam(values.ToView()));

    /// <summary>Persist the whole auth / enforcement sub-struct (full PUT).</summary>
    public Task SaveAuthAsync(AuthPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveAuth(values.ToView()));

    /// <summary>Persist the whole submission-quota sub-struct (full PUT).</summary>
    public Task SaveSubmissionAsync(SubmissionPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveSubmission(values.ToView()));

    /// <summary>Persist the whole IMAP-server sub-struct (full PUT).</summary>
    public Task SaveImapAsync(ImapPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveImap(values.ToView()));

    /// <summary>Persist the whole outbound-delivery sub-struct (full PUT).</summary>
    public Task SaveOutboundAsync(OutboundPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveOutbound(values.ToView()));

    /// <summary>Persist the whole nest-side alias sub-struct (full PUT).</summary>
    public Task SaveAliasAsync(AliasPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveAlias(values.ToView()));

    /// <summary>Persist the whole deployment-wide DMARC catalog (full PUT).</summary>
    public Task SaveDmarcAsync(DmarcPolicyValues values)
        => DispatchAsync(new MailPolicyAction.SaveDmarc(values.ToView()));

    /// <summary>Save the deployment's own Microsoft registration from the two fields'
    /// drafts (<c>admin-mail-microsoft-save-button</c> → <c>SetMicrosoftRegistration</c>);
    /// both empty clears it. The shared machine checks the pair first — a refused draft
    /// sends nothing and its sentence lands on <see cref="Error"/>.</summary>
    public Task SetMicrosoftRegistrationAsync(string clientId, string tenant)
        => DispatchAsync(new MailPolicyAction.SetMicrosoftRegistration(clientId, tenant));

    /// <summary>One press of the same-button two-press
    /// (<c>admin-mail-publish-spam-baseline-button</c>): the first press arms it (the page
    /// relabels the button to the confirm sentence) and publishes nothing; the second
    /// dispatches <c>publish_spam_baseline</c>, and the outcome lands in
    /// <see cref="BaselinePublish"/>. Returns <c>true</c> when this press dispatched.</summary>
    public Task<bool> PressPublishSpamBaselineAsync()
        => PressAsync(MailPolicyConfirm.PublishSpamBaseline);

    /// <summary>Flip the standing baseline publish
    /// (<c>admin-mail-spam-baseline-standing-toggle</c> →
    /// <c>SetBaselineStandingPublish</c>) — its own gesture off the persisted group,
    /// never a Spam-group draft Save.</summary>
    public Task SetBaselineStandingPublishAsync(bool enabled)
        => DispatchAsync(new MailPolicyAction.SetBaselineStandingPublish(enabled));

    /// <summary><c>admin-mail-health-recheck-button</c> → <c>RecheckHealth</c> (fresh
    /// blocklist self-check + deliverability diagnostics, then a health re-read).</summary>
    public Task RecheckHealthAsync()
        => DispatchAsync(new MailPolicyAction.RecheckHealth());

    /// <summary><c>admin-mail-health-warmup-reset-button</c>: the same-button two-press.
    /// The first press arms (the page relabels the button to the confirm sentence) and
    /// dispatches nothing; the second dispatches <c>ResetWarmup</c>.
    /// Returns <c>true</c> when this press dispatched.</summary>
    public Task<bool> PressWarmupResetAsync()
        => PressAsync(MailPolicyConfirm.ResetWarmup);

    /// <summary><c>admin-mail-list-unsubscribe-secret-rotate-button</c>: the same-button
    /// two-press. The second press dispatches <c>rotate_list_unsubscribe_secret</c> and the
    /// outcome lands in <see cref="SecretRotationLine"/>. Returns <c>true</c> when this
    /// press dispatched.</summary>
    public Task<bool> PressListUnsubscribeSecretRotateAsync()
        => PressAsync(MailPolicyConfirm.RotateListUnsubscribeSecret);

    /// <summary><c>admin-mail-forward-srs-secret-rotate-button</c>: the same-button
    /// two-press. The second press dispatches <c>rotate_srs_secret</c> and the outcome
    /// lands in <see cref="SecretRotationLine"/>. Returns <c>true</c> when this press
    /// dispatched.</summary>
    public Task<bool> PressForwardSrsSecretRotateAsync()
        => PressAsync(MailPolicyConfirm.RotateSrsSecret);

    /// <summary>One press of a shared confirm button (<c>MailPolicyAction.Press</c>): the
    /// machine arms it when it is not the armed one, else disarms and runs the act. The
    /// press dispatched exactly when <paramref name="button"/> was already armed.</summary>
    private async Task<bool> PressAsync(MailPolicyConfirm button)
    {
        var dispatches = Armed == button;
        await DispatchAsync(new MailPolicyAction.Press(button));
        return dispatches;
    }

    /// <summary>Dispatch an action then re-project the snapshot. The machine captures
    /// any user-facing error into <c>snapshot.error</c> (and also throws), so the throw
    /// is swallowed and the error read from the snapshot — matching MailSpamViewModel /
    /// linux; the exception is a fallback only if the snapshot carried no error.</summary>
    private async Task DispatchAsync(MailPolicyAction action)
    {
        try
        {
            await _machine.Dispatch(action);
            Apply(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            Apply(_machine.Snapshot());
            if (string.IsNullOrEmpty(Error)) Error = Strings.Error(ex);
        }
    }

    /// <summary>Paint the shared health fold into finished sentences: the status line
    /// (<c>admin.mail_page.health_status_line</c> filled from the state label and the
    /// two heartbeat facts), each row's label/state resolved, and the heartbeat rows 5–6
    /// carrying the stamp. Every sentence is shared Rust (UniFFI twins of
    /// <c>mail_health_state_label</c> / <c>_check_state_label</c> / <c>_heartbeat_label</c>)
    /// or the nest's own detail.</summary>
    private MailHealthValues ProjectHealth(MailHealthView health)
    {
        var now = _nowMs();
        string Heartbeat(long? at) => ValueFormat.ResolveRelativeTimeDisplay(
            uniffi.fauna_ffi.FaunaFfiMethods.MailHealthHeartbeatLabel(at, now),
            (at ?? 0) * 1000);

        var status = Strings.Get("admin/mail_page/health_status_line")
            .Replace("{state}", Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.MailHealthStateLabel(health.state)))
            .Replace("{delivered}", Heartbeat(health.lastOutboundDeliveredAt))
            .Replace("{received}", Heartbeat(health.lastInboundAcceptedAt));

        var stamps = new[] { health.lastOutboundDeliveredAt, health.lastInboundAcceptedAt };
        var rows = new List<MailHealthRow>(health.checks.Length);
        for (var i = 0; i < health.checks.Length; i++)
        {
            var check = health.checks[i];
            var detail = i >= 5 && i - 5 < stamps.Length ? Heartbeat(stamps[i - 5]) : check.detail;
            var slashed = check.labelKey.Replace('.', '/');
            var label = Strings.Get(slashed);
            rows.Add(new MailHealthRow(
                label == slashed ? check.labelKey : label,
                Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.MailHealthCheckStateLabel(check.state)),
                detail));
        }
        return new MailHealthValues(status, rows, health.delistUrl);
    }

    /// <summary>Project the machine snapshot onto the bound state: the mail-enable flag,
    /// each group's persisted values, and the last action error (null when empty).</summary>
    private void Apply(MailPolicySnapshot snap)
    {
        Error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        MailEnabled = snap.mailEnabled;
        MailUnavailableHere = snap.mailUnavailableHere;
        OnPropertyChanged(nameof(MailEnableRefused));
        OnPropertyChanged(nameof(MailEnabledToggleState));
        AutoEnableMailForNewUsers = snap.autoEnableMailForNewUsers;
        Spam = SpamPolicyValues.From(snap.spam);
        Auth = AuthPolicyValues.From(snap.auth);
        Submission = SubmissionPolicyValues.From(snap.submission);
        Imap = ImapPolicyValues.From(snap.imap);
        Outbound = OutboundPolicyValues.From(snap.outbound);
        Alias = AliasPolicyValues.From(snap.alias);
        Dmarc = DmarcPolicyValues.From(snap.dmarc);
        Microsoft = MicrosoftRegistrationValues.From(snap.microsoftRegistration);
        BaselinePublish = snap.baselinePublishResult is null
            ? null
            : BaselinePublishValues.From(snap.baselinePublishResult);
        BaselineState = snap.baselineState is null
            ? null
            : BaselineStateValues.From(snap.baselineState);
        Armed = snap.armed;
        SecretRotationLine = snap.secretRotation is null
            ? null
            : Strings.Resolve(FaunaClientMailSettingsMethods.SecretRotationText(snap.secretRotation));
        Health = snap.health is null ? null : ProjectHealth(snap.health);
        OnPropertyChanged(nameof(Health));
        OnPropertyChanged(nameof(Armed));
        OnPropertyChanged(nameof(WarmupResetArmed));
        OnPropertyChanged(nameof(PublishSpamBaselineArmed));
        OnPropertyChanged(nameof(ListUnsubscribeSecretRotateArmed));
        OnPropertyChanged(nameof(ForwardSrsSecretRotateArmed));
        OnPropertyChanged(nameof(SecretRotationLine));
        OnPropertyChanged(nameof(BaselineState));
        OnPropertyChanged(nameof(Spam));
        OnPropertyChanged(nameof(Auth));
        OnPropertyChanged(nameof(Submission));
        OnPropertyChanged(nameof(Imap));
        OnPropertyChanged(nameof(Outbound));
        OnPropertyChanged(nameof(Alias));
        OnPropertyChanged(nameof(Dmarc));
        OnPropertyChanged(nameof(Microsoft));
        OnPropertyChanged(nameof(BaselinePublish));
    }
}
