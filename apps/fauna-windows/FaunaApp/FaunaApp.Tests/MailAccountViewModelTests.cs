using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The mail-settings account controls' view-model (mail-policy-config.md § Tier 3;
/// email-filters.md § The auto-respond setting; imap-server.md § Composition with
/// submission quota): every field is what the nest answered, never the gesture, and
/// the shared draft check and quota wording decide first.
///
/// <para>[Collection("StringsGlobal")] because the readouts resolve shared
/// <c>LocalizedText</c> through <see cref="Strings"/>, whose localizer is
/// process-global. This class installs its own <see cref="FakeLocalizer"/> mapping
/// the real templates, so the two-level byte sizes are read as they render.</para>
/// </summary>
[Collection("StringsGlobal")]
public class MailAccountViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["mail_settings/mailbox_quota"] = "{used} of {limit} · {messages} of {message_limit} messages",
            ["mail_settings/submission_quota"] = "{used} of {limit} recipients today",
            ["mail_settings/over_quota_mailbox"] = "Your mailbox is full.",
            ["mail_settings/over_quota_sending"] = "Today's limit of {limit} is spent.",
            ["mail_settings/autorespond_message_empty"] = "Write the message first.",
            ["size/bytes"] = "{value} B",
            ["size/kb"] = "{value} KB",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public MailAccountViewModelTests() => Strings.Initialize(new FakeLocalizer());

    private static MailQuotas Quotas(
        ulong usedBytes = 0, ulong limitBytes = 0, uint messages = 0, uint messageLimit = 0,
        uint sent = 0, uint dailyLimit = 0) =>
        new(usedBytes, limitBytes, messages, messageLimit, sent, dailyLimit);

    [Fact]
    public void BeforeTheFirstRead_NothingIsKnown()
    {
        var vm = new MailAccountViewModel(new MockNestRpcClient());

        Assert.Null(vm.ReceiveEnabled);
        Assert.Null(vm.SendEnabled);
        Assert.Null(vm.AutorespondEnabled);
        Assert.Null(vm.MailboxQuotaText);
        Assert.Null(vm.SubmissionQuotaText);
        Assert.Null(vm.OverQuotaNotice);
    }

    [Fact]
    public async Task Load_ProjectsTheSwitchesTheReplyAndTheReadouts()
    {
        var rpc = new MockNestRpcClient
        {
            NextMailSwitches = new(true, false, true),
            NextAutorespond = new(true, "Away."),
            NextMailQuotas = Quotas(2048, 1024 * 1024 * 1024, 3, 100, 4, 50),
        };
        var vm = new MailAccountViewModel(rpc);

        await vm.LoadAsync();

        Assert.True(vm.ReceiveEnabled);
        Assert.False(vm.SendEnabled);
        Assert.True(vm.AutorespondEnabled);
        Assert.Equal("Away.", vm.AutorespondMessage);
        Assert.Equal("4 of 50 recipients today", vm.SubmissionQuotaText);
        Assert.Null(vm.OverQuotaNotice);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task MailboxQuota_ResolvesBothSizesBeforeTheSentence()
    {
        var rpc = new MockNestRpcClient { NextMailQuotas = Quotas(2048, 1, 3, 100) };
        var vm = new MailAccountViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("2 KB of 1 B · 3 of 100 messages", vm.MailboxQuotaText);
    }

    [Fact]
    public async Task ALimitOfZeroReadsAsNoLimit_NoNotice()
    {
        var vm = new MailAccountViewModel(new MockNestRpcClient { NextMailQuotas = Quotas(5, 0, 5, 0, 5, 0) });

        await vm.LoadAsync();

        Assert.Null(vm.OverQuotaNotice);
    }

    [Fact]
    public async Task AFullMailbox_NamesTheMailboxBeforeTheSendingLimit()
    {
        var rpc = new MockNestRpcClient { NextMailQuotas = Quotas(2, 1, 0, 0, 5, 5) };
        var vm = new MailAccountViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("Your mailbox is full.", vm.OverQuotaNotice);
    }

    [Fact]
    public async Task ASpentSendingAllowance_NamesItsLimit()
    {
        var rpc = new MockNestRpcClient { NextMailQuotas = Quotas(0, 10, 0, 10, 5, 5) };
        var vm = new MailAccountViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("Today's limit of 5 is spent.", vm.OverQuotaNotice);
    }

    [Fact]
    public async Task ALoadFailure_NamesItAndLeavesEveryControlAbsent()
    {
        var rpc = new MockNestRpcClient { NextError = "down" };
        var vm = new MailAccountViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.Null(vm.ReceiveEnabled);
        Assert.Null(vm.AutorespondEnabled);
        Assert.Null(vm.MailboxQuotaText);
    }

    [Fact]
    public async Task SetReceive_WritesOnlyThatSwitch()
    {
        var rpc = new MockNestRpcClient { NextMailSwitches = new(true, true, true) };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetReceiveAsync(false);

        Assert.Equal((false, (bool?)null), rpc.LastMailSwitchesSet);
        Assert.False(vm.ReceiveEnabled);
        Assert.True(vm.SendEnabled);
    }

    [Fact]
    public async Task SetSend_WritesOnlyThatSwitch()
    {
        var rpc = new MockNestRpcClient { NextMailSwitches = new(true, true, true) };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetSendAsync(false);

        Assert.Equal(((bool?)null, false), rpc.LastMailSwitchesSet);
        Assert.True(vm.ReceiveEnabled);
        Assert.False(vm.SendEnabled);
    }

    [Fact]
    public async Task ASwitchPaintsWhatTheNestKept_NotTheGesture()
    {
        var rpc = new MockNestRpcClient
        {
            NextMailSwitches = new(true, true, true),
            MailSwitchesReread = new(true, true, true),
        };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetReceiveAsync(false);

        Assert.True(vm.ReceiveEnabled);
    }

    [Fact]
    public async Task ARefusedSwitchWrite_KeepsTheStoredValueAndNamesTheError()
    {
        var rpc = new MockNestRpcClient { NextMailSwitches = new(true, true, true) };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();
        rpc.NextError = "refused";

        await vm.SetReceiveAsync(false);

        Assert.True(vm.ReceiveEnabled);
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task ABlankReply_IsRefusedWithItsReasonAndNothingIsSent()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitAutorespondMessageAsync("   ");

        Assert.Equal("Write the message first.", vm.Error);
        Assert.Equal(0, rpc.AutorespondSetCalls);
        Assert.False(vm.AutorespondEnabled);
    }

    [Fact]
    public async Task CommittingAReply_SendsTheTrimmedWordsAndTurnsItOn()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitAutorespondMessageAsync("  Away until Monday. ");

        Assert.Equal("Away until Monday.", rpc.LastAutorespondSet);
        Assert.True(vm.AutorespondEnabled);
        Assert.Equal("Away until Monday.", vm.AutorespondMessage);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task TurningTheReplyOff_ClearsItAndKeepsTheWords()
    {
        var rpc = new MockNestRpcClient { NextAutorespond = new(true, "Away.") };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetAutorespondAsync(false, "Away.");

        Assert.Equal(1, rpc.AutorespondClearCalls);
        Assert.False(vm.AutorespondEnabled);
        Assert.Equal("Away.", vm.AutorespondMessage);
    }

    [Fact]
    public async Task TurningTheReplyBackOn_SendsTheSameWords()
    {
        var rpc = new MockNestRpcClient { NextAutorespond = new(true, "Away.") };
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();
        await vm.SetAutorespondAsync(false, "Away.");

        await vm.SetAutorespondAsync(true, "Away.");

        Assert.Equal("Away.", rpc.LastAutorespondSet);
        Assert.True(vm.AutorespondEnabled);
    }

    [Fact]
    public async Task TurningTheReplyOnWithABlankDraft_IsRefusedAndStaysOff()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();

        await vm.SetAutorespondAsync(true, "");

        Assert.Equal("Write the message first.", vm.Error);
        Assert.Equal(0, rpc.AutorespondSetCalls);
        Assert.False(vm.AutorespondEnabled);
    }

    [Fact]
    public async Task TheReplyToggle_IsInertUntilTheFirstRead()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);

        await vm.SetAutorespondAsync(true, "Away.");

        Assert.Equal(0, rpc.AutorespondSetCalls);
        Assert.Equal(0, rpc.AutorespondClearCalls);
        Assert.Null(vm.AutorespondEnabled);
    }

    [Fact]
    public async Task TheSwitches_AreInertUntilTheFirstRead()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);

        await vm.SetReceiveAsync(false);
        await vm.SetSendAsync(false);

        Assert.Null(rpc.LastMailSwitchesSet);
        Assert.Null(vm.ReceiveEnabled);
    }

    [Fact]
    public async Task ACommitAfterARefusal_ClearsTheError()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailAccountViewModel(rpc);
        await vm.LoadAsync();
        await vm.CommitAutorespondMessageAsync(" ");

        await vm.CommitAutorespondMessageAsync("Away.");

        Assert.Null(vm.Error);
    }
}
