using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailPolicyMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="FakeMailSpamMachine"/> /
/// <see cref="FakeMailAliasesMachine"/>. Records dispatched actions and returns a
/// configurable <see cref="NextSnapshot"/>; set <see cref="NextError"/> to make
/// Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeMailPolicyMachine : MailPolicyMachineFakeBase
{
    public MailPolicySnapshot NextSnapshot { get; set; } = Empty();
    public List<MailPolicyAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailPolicyAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override MailPolicySnapshot Snapshot() => NextSnapshot;

    /// <summary>A catalog-plausible default snapshot (all defaults; mail disabled).</summary>
    public static MailPolicySnapshot Empty() => Snap();

    /// <summary>Build a full snapshot from per-group overrides, defaulting any unset
    /// group to its catalog-plausible default view. <paramref name="autoEnableMailForNewUsers"/>
    /// defaults <c>true</c> (the works-out-of-box deployment default; unset ⇒ ON).</summary>
    public static MailPolicySnapshot Snap(
        bool mailEnabled = false,
        bool autoEnableMailForNewUsers = true,
        SpamPolicyView? spam = null,
        AuthPolicyView? auth = null,
        SubmissionPolicyView? submission = null,
        ImapPolicyView? imap = null,
        OutboundPolicyView? outbound = null,
        AliasPolicyView? alias = null,
        BaselinePublishView? baselinePublishResult = null,
        BaselineStateView? baselineState = null,
        MailHealthView? mailHealth = null,
        string? error = null) =>
        new(
            mailEnabled,
            autoEnableMailForNewUsers,
            spam ?? DefaultSpam(),
            auth ?? DefaultAuth(),
            submission ?? DefaultSubmission(),
            imap ?? DefaultImap(),
            outbound ?? DefaultOutbound(),
            alias ?? DefaultAlias(),
            MailPolicyStatus.Idle,
            baselinePublishResult,
            baselineState,
            mailHealth,
            error);

    public static SpamPolicyView DefaultSpam() => new(
        @maxScoreBeforeSpamFolder: 5,
        @maxScoreBeforeReject: 0,
        @dnsblServers: new[] { "zen.spamhaus.org" },
        @rejectNoRdns: false,
        @greylistEnabled: false,
        @greylistDelaySecs: 300,
        @maxConnPerMin: 30,
        @fcrdnsMode: "score_signal",
        @heloIdentityRequired: false,
        @rejectFcrdnsFail: false,
        @maxMessageBytes: 26_214_400,
        @bayesianWeightMilli: 700,
        @bayesianMinSamples: 50,
        @bayesianFullConfidenceSamples: 200,
        @trainingHistoryRetentionDays: 30,
        @unlistedRecipientPenalty: 0,
        @baselineStandingPublish: false);

    public static AuthPolicyView DefaultAuth() => new(
        @enforceDmarc: true,
        @enforceDmarcQuarantine: false,
        @enforceSpfHardfail: false,
        @enforceDkim: false,
        @logOnly: false,
        @maxAuthFailuresPerMinute: 5,
        @maxConnPerIp: 0);

    public static SubmissionPolicyView DefaultSubmission() => new(
        @maxPerDay: 500,
        @maxRecipientsPerMessage: 100);

    public static ImapPolicyView DefaultImap() => new(
        @idleTimeoutSecs: 1800,
        @tombstoneRetentionDays: 30,
        @deleteNonempty: "forbidden",
        @bodystructureCacheMax: 1000,
        @storageBytesDefault: 5_368_709_120UL,
        @messageCountDefault: 50_000);

    public static OutboundPolicyView DefaultOutbound() => new(
        @retryScheduleSeconds: new ulong[] { 300, 900, 3600 },
        @permanentFailureTimeoutHours: 120,
        @delayWarningAtHours: 4,
        @ndrRateLimitDays: 1,
        @suppressNdrSpfHardfail: true,
        @suppressNdrDmarcReject: true,
        @postmasterCcBounces: false,
        @tlsrptSendReports: false,
        @ipv6Enabled: false,
        @treat5xxAsTransient: new[] { "4.2.2" });

    public static AliasPolicyView DefaultAlias() => new(
        @exactAliasesMax: 20,
        @reservedLocalParts: new[] { "postmaster", "abuse" },
        @subaddressingEnabled: true,
        @wildcardPrefixEnabled: true);
}

/// <summary>
/// Deterministic unit tests for the admin <c>admin-mail</c> policy page VM, over the
/// <see cref="FakeMailPolicyMachine"/> (the UniFFI <c>IMailPolicyMachine</c> seam —
/// the e2e flow made deterministic; no live nest / FlaUI, which flakes on windows).
/// Covers the snapshot projection across all seven policy groups + the mail-enable
/// flag, and that each save / toggle gesture dispatches the right full-PUT
/// <c>MailPolicyAction</c> with the edited fields carried through the public
/// mirror-record → internal view round-trip.
/// </summary>
public class AdminMailViewModelTests
{
    [Fact]
    public async Task Load_ProjectsAllSevenGroups()
    {
        var fake = new FakeMailPolicyMachine
        {
            NextSnapshot = FakeMailPolicyMachine.Snap(
                mailEnabled: true,
                spam: FakeMailPolicyMachine.DefaultSpam() with
                {
                    @maxScoreBeforeSpamFolder = 7,
                    @maxScoreBeforeReject = 20,
                    @fcrdnsMode = "enforce",
                    @dnsblServers = new[] { "bl.example.org", "zen.spamhaus.org" },
                },
                auth: FakeMailPolicyMachine.DefaultAuth() with { @enforceDmarc = true, @enforceDkim = true },
                submission: new SubmissionPolicyView(@maxPerDay: 777, @maxRecipientsPerMessage: 33),
                imap: FakeMailPolicyMachine.DefaultImap() with
                {
                    @deleteNonempty = "allowed",
                    @storageBytesDefault = 9_999_999_999UL,
                },
                outbound: FakeMailPolicyMachine.DefaultOutbound() with
                {
                    @retryScheduleSeconds = new ulong[] { 60, 120, 240 },
                    @treat5xxAsTransient = new[] { "5.7.1", "4.4.2" },
                },
                alias: FakeMailPolicyMachine.DefaultAlias() with
                {
                    @exactAliasesMax = 42,
                    @reservedLocalParts = new[] { "root", "security" },
                }),
        };
        var vm = new AdminMailViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Null(vm.Error);
        Assert.True(vm.MailEnabled);

        // Spam group.
        Assert.Equal(7u, vm.Spam.MaxScoreBeforeSpamFolder);
        Assert.Equal(20u, vm.Spam.MaxScoreBeforeReject);
        Assert.Equal("enforce", vm.Spam.FcrdnsMode);
        Assert.Equal(new[] { "bl.example.org", "zen.spamhaus.org" }, vm.Spam.DnsblServers);

        // Auth group.
        Assert.True(vm.Auth.EnforceDmarc);
        Assert.True(vm.Auth.EnforceDkim);

        // Submission group.
        Assert.Equal(777u, vm.Submission.MaxPerDay);
        Assert.Equal(33u, vm.Submission.MaxRecipientsPerMessage);

        // IMAP group (string + ulong fields).
        Assert.Equal("allowed", vm.Imap.DeleteNonempty);
        Assert.Equal(9_999_999_999UL, vm.Imap.StorageBytesDefault);

        // Outbound group (ulong[] + string[] fields).
        Assert.Equal(new ulong[] { 60, 120, 240 }, vm.Outbound.RetrySchedule);
        Assert.Equal(new[] { "5.7.1", "4.4.2" }, vm.Outbound.Treat5xxAsTransient);

        // Alias group.
        Assert.Equal(42u, vm.Alias.ExactAliasesMax);
        Assert.Equal(new[] { "root", "security" }, vm.Alias.ReservedLocalParts);
    }

    [Fact]
    public async Task SetMailEnabled_DispatchesFlag()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetMailEnabledAsync(false);

        Assert.Contains(fake.Dispatched, a => a is MailPolicyAction.SetMailEnabled { enabled: false });
    }

    [Fact]
    public async Task Load_ProjectsAutoEnableForNewUsers()
    {
        // The deployment-wide "auto-enable mail for new users" flag (default-on,
        // read back from fauna.setup.status) projects onto the bound VM property —
        // here the nest reports it OFF. mail-policy-config.md § Tier-2.
        var fake = new FakeMailPolicyMachine
        {
            NextSnapshot = FakeMailPolicyMachine.Snap(autoEnableMailForNewUsers: false),
        };
        var vm = new AdminMailViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.AutoEnableMailForNewUsers);
    }

    [Fact]
    public async Task SetAutoEnableMailForNewUsers_DispatchesFlag()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetAutoEnableMailForNewUsersAsync(false);

        Assert.Contains(fake.Dispatched,
            a => a is MailPolicyAction.SetAutoEnableMailForNewUsers { enabled: false });
    }

    [Fact]
    public async Task SaveSpam_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        var edited = new SpamPolicyValues(
            MaxScoreBeforeSpamFolder: 4,
            MaxScoreBeforeReject: 15,
            DnsblServers: new[] { "custom.dnsbl.example" },
            RejectNoRdns: true,
            GreylistEnabled: true,
            GreylistDelaySecs: 600,
            MaxConnPerMin: 60,
            FcrdnsMode: "enforce",
            HeloIdentityRequired: true,
            RejectFcrdnsFail: true,
            MaxMessageBytes: 10_485_760,
            BayesianWeightMilli: 500,
            BayesianMinSamples: 40,
            BayesianFullConfidenceSamples: 150,
            TrainingHistoryRetentionDays: 14,
            UnlistedRecipientPenalty: 250,
            BaselineStandingPublish: false);

        await vm.SaveSpamAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveSpam { policy: var p }
            && p.@maxScoreBeforeReject == 15
            && p.@fcrdnsMode == "enforce"
            && p.@rejectNoRdns
            && p.@dnsblServers.Length == 1
            && p.@dnsblServers[0] == "custom.dnsbl.example"
            && p.@bayesianWeightMilli == 500
            && p.@bayesianFullConfidenceSamples == 150
            && p.@trainingHistoryRetentionDays == 14
            && p.@unlistedRecipientPenalty == 250);
    }

    [Fact]
    public async Task SaveSpam_UnlistedRecipientPenalty_RoundTripsWithoutClobberingOtherFields()
    {
        // A save that only edits the unlisted-recipient-penalty knob still full-PUTs the
        // whole sub-struct (mail-policy-config.md § Policy catalog Tier 2) — starting from
        // the persisted snapshot (mirrors AdminMailPage's Save-button gather, which seeds
        // every field from `_vm.Spam` and only overrides the edited widget) proves the
        // untouched fields survive unchanged, not silently zeroed.
        var fake = new FakeMailPolicyMachine
        {
            NextSnapshot = FakeMailPolicyMachine.Snap(
                spam: FakeMailPolicyMachine.DefaultSpam() with
                {
                    @maxScoreBeforeReject = 20,
                    @fcrdnsMode = "enforce",
                    @unlistedRecipientPenalty = 0,
                }),
        };
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // Gather starting from the persisted snapshot (the page's Save-button pattern),
        // editing only UnlistedRecipientPenalty.
        var edited = vm.Spam with { UnlistedRecipientPenalty = 1000 };

        await vm.SaveSpamAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveSpam { policy: var p }
            && p.@unlistedRecipientPenalty == 1000
            && p.@maxScoreBeforeReject == 20   // untouched field survives, not clobbered
            && p.@fcrdnsMode == "enforce");    // untouched field survives, not clobbered
    }

    [Fact]
    public async Task PublishSpamBaseline_DispatchesAndProjectsAggregate()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine re-projects the aggregate outcome into the snapshot.
        fake.NextSnapshot = FakeMailPolicyMachine.Snap(
            baselinePublishResult: new BaselinePublishView(
                @published: true, @contributors: 3, @sampleCount: 42, @skippedContributors: 0, @deferred: false));

        await vm.PublishSpamBaselineAsync();

        Assert.Contains(fake.Dispatched, a => a is MailPolicyAction.PublishSpamBaseline);
        Assert.NotNull(vm.BaselinePublish);
        Assert.True(vm.BaselinePublish!.Published);
        Assert.Equal(3u, vm.BaselinePublish.Contributors);
        Assert.Equal(42u, vm.BaselinePublish.SampleCount);
    }

    [Fact]
    public async Task PublishSpamBaseline_WithSkippedContributors_ProjectsTheErosionCount()
    {
        // mail-spam.md § Encrypted-mode interaction: opt-in contributors whose sealed
        // model couldn't be merged this run (holder unavailable / no reaching grant /
        // undecodable copy) are surfaced beside the published outcome, not silently
        // dropped from the contributor count.
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextSnapshot = FakeMailPolicyMachine.Snap(
            baselinePublishResult: new BaselinePublishView(
                @published: true, @contributors: 3, @sampleCount: 42, @skippedContributors: 2, @deferred: false));

        await vm.PublishSpamBaselineAsync();

        Assert.NotNull(vm.BaselinePublish);
        Assert.Equal(2u, vm.BaselinePublish!.SkippedContributors);
    }

    [Fact]
    public async Task PublishSpamBaseline_WithheldBelowFloor_ProjectsNotPublished()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextSnapshot = FakeMailPolicyMachine.Snap(
            baselinePublishResult: new BaselinePublishView(
                @published: false, @contributors: 2, @sampleCount: 0, @skippedContributors: 0, @deferred: false));

        await vm.PublishSpamBaselineAsync();

        Assert.NotNull(vm.BaselinePublish);
        Assert.False(vm.BaselinePublish!.Published);
        Assert.Equal(2u, vm.BaselinePublish.Contributors);
    }

    [Fact]
    public async Task SaveAuth_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        var edited = new AuthPolicyValues(
            EnforceDmarc: true,
            EnforceDmarcQuarantine: true,
            EnforceSpfHardfail: true,
            EnforceDkim: true,
            LogOnly: false,
            MaxAuthFailuresPerMinute: 9,
            MaxConnPerIp: 25);

        await vm.SaveAuthAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveAuth { policy: var p }
            && p.@enforceDmarcQuarantine
            && p.@enforceSpfHardfail
            && p.@maxAuthFailuresPerMinute == 9
            && p.@maxConnPerIp == 25); // full-PUT carries the preserved per-IP cap
    }

    [Fact]
    public async Task SaveSubmission_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SaveSubmissionAsync(new SubmissionPolicyValues(MaxPerDay: 250, MaxRecipientsPerMessage: 12));

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveSubmission { policy: var p }
            && p.@maxPerDay == 250
            && p.@maxRecipientsPerMessage == 12);
    }

    [Fact]
    public async Task SaveImap_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        var edited = new ImapPolicyValues(
            IdleTimeoutSecs: 900,
            TombstoneRetentionDays: 14,
            DeleteNonempty: "allowed",
            BodystructureCacheMax: 2000,
            StorageBytesDefault: 12_884_901_888UL,
            MessageCountDefault: 100_000);

        await vm.SaveImapAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveImap { policy: var p }
            && p.@deleteNonempty == "allowed"
            && p.@storageBytesDefault == 12_884_901_888UL
            && p.@idleTimeoutSecs == 900);
    }

    [Fact]
    public async Task SaveOutbound_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        var edited = new OutboundPolicyValues(
            RetrySchedule: new ulong[] { 30, 90, 300 },
            PermanentFailureTimeoutHours: 72,
            DelayWarningAtHours: 2,
            NdrRateLimitDays: 3,
            SuppressNdrSpfHardfail: false,
            SuppressNdrDmarcReject: false,
            PostmasterCcBounces: false,
            TlsrptSendReports: true,
            Ipv6Enabled: true,
            Treat5xxAsTransient: new[] { "5.7.1" });

        await vm.SaveOutboundAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveOutbound { policy: var p }
            && p.@retryScheduleSeconds.SequenceEqual(new ulong[] { 30, 90, 300 })
            && p.@tlsrptSendReports
            && p.@ipv6Enabled
            && p.@treat5xxAsTransient.Length == 1
            && p.@treat5xxAsTransient[0] == "5.7.1");
    }

    [Fact]
    public async Task SaveAlias_DispatchesFullPut()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        var edited = new AliasPolicyValues(
            ExactAliasesMax: 50,
            ReservedLocalParts: new[] { "root", "postmaster", "abuse" },
            SubaddressingEnabled: false,
            WildcardPrefixEnabled: false);

        await vm.SaveAliasAsync(edited);

        Assert.Contains(fake.Dispatched, a =>
            a is MailPolicyAction.SaveAlias { policy: var p }
            && p.@exactAliasesMax == 50
            && !p.@subaddressingEnabled
            && p.@reservedLocalParts.Length == 3
            && p.@reservedLocalParts[0] == "root");
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailPolicyMachine();
        var vm = new AdminMailViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The
        // VM swallows the throw and surfaces an error rather than letting it escape.
        fake.NextError = "boom";
        await vm.SaveSubmissionAsync(new SubmissionPolicyValues(MaxPerDay: 1, MaxRecipientsPerMessage: 1));

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
