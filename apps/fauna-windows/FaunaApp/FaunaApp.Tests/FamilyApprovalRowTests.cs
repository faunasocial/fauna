using System;
using System.Collections.Generic;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The <c>family-approval-item</c> row's display text (family-safety.md § Reach approvals,
/// firmed 2026-07-09). The queue is typed — <c>kind: contact | mail_hold</c> — and the two
/// kinds name their peer differently:
/// <list type="bullet">
/// <item>a <c>contact</c> carries a human <c>summary</c> and a <c>peer_actor_id</c>;</item>
/// <item>a <c>mail_hold</c> carries the sender's <c>peer_address</c> and a <b>deliberately
/// empty <c>summary</c></b> — a subject line is content, and the message is sealed to the
/// ward, so the nest never sees it (family-safety.md:19, :134).</item>
/// </list>
/// Binding <c>Summary</c> for a mail hold therefore renders a blank row with live
/// Approve/Deny buttons — and, because the row container's
/// <c>AutomationProperties.Name</c> binds the same value, an empty Name makes UIA prune the
/// row so FlaUI counts zero of them.
/// </summary>
/// <summary>
/// Serializes with the other <c>Strings.Initialize</c>-mutating test classes
/// (<c>StringsResolutionTests</c>, <c>ValueFormatTests</c>) under xUnit's default
/// parallel-by-class runner — see <c>StringsGlobalCollection</c>.
/// </summary>
[Collection("StringsGlobal")]
public class FamilyApprovalRowTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    // Routes through the real FamilyApprovalRow.From + the real FFI
    // ApprovalDisplayText call (reference_windows_dotnet_test_loads_native_ffi) —
    // DisplayText is no longer a computed getter, it's populated by From() from
    // the shared fauna_core::format::approval_display_text export, so these tests
    // must exercise that real round-trip rather than assert against a value they
    // set themselves (which would prove nothing about the shared derivation).
    private static FamilyApprovalRow Row(string kind, string summary, string peerAddress) =>
        FamilyApprovalRow.From(new FfiFamilyApprovalEntry(
            supervisedActorId: new byte[32],
            supervisedHandle: "ward",
            kind: kind,
            peerActorId: new byte[32],
            peerAddress: peerAddress,
            messageId: kind == "mail_hold" ? new byte[16] : Array.Empty<byte>(),
            summary: summary,
            peerHandle: "",
            bridgeId: "",
            operation: "",
            target: "",
            createdAt: 0L));

    [Fact]
    public void MailHold_DisplaysThePeerAddress_BecauseItsSummaryIsDeliberatelyEmpty()
    {
        var row = Row("mail_hold", summary: "", peerAddress: "stranger@example.com");
        Assert.Equal("stranger@example.com", row.DisplayText);
    }

    [Fact]
    public void Contact_DisplaysItsSummary_AndIgnoresTheEmptyPeerAddress()
    {
        var row = Row("contact", summary: "alice wants to connect", peerAddress: "");
        Assert.Equal("alice wants to connect", row.DisplayText);
    }

    [Fact]
    public void MailHold_DisplayTextIsNeverEmpty_SoTheUiaRowIsNotPruned()
    {
        // A non-empty AutomationProperties.Name is what keeps FlaUI from counting 0 rows
        // (reference_winui_flaui_datatemplate_name). The row Name binds DisplayText.
        var row = Row("mail_hold", summary: "", peerAddress: "stranger@example.com");
        Assert.False(string.IsNullOrWhiteSpace(row.DisplayText));
    }

    [Fact]
    public void NullPathMailHold_FallsBackToTheNoSenderLabel_RatherThanRenderingBlank()
    {
        // A held `MAIL FROM:<>` message (family-safety.md § The mail gate) truthfully
        // carries an empty PeerAddress too; without this fallback it renders the exact
        // same blank-row + pruned-UIA-row defect the summary-emptiness case above fixed.
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["family/approval_no_sender"] = "No sender (delivery notice)",
        }));
        var row = Row("mail_hold", summary: "", peerAddress: "");
        Assert.Equal("No sender (delivery notice)", row.DisplayText);
    }

    [Fact]
    public void AnUnknownFutureKind_FallsBackToSummary_RatherThanRenderingAnAddress()
    {
        // A kind this build does not know yet is treated like `contact`: summary is the
        // safe default; only the envelope kinds are address-shaped.
        var row = Row("a_future_kind", summary: "a future ask", peerAddress: "");
        Assert.Equal("a future ask", row.DisplayText);
    }

    [Fact]
    public void FeedSource_NamesTheGrantKey_NeverOnlyTheWardsLabel()
    {
        // family-safety.md § Feed-source approvals: the grant matches
        // (bridge_id, operation, target), never the label, so the row names what
        // Approve grants and quotes the ward's label after it.
        var row = FamilyApprovalRow.From(new FfiFamilyApprovalEntry(
            supervisedActorId: new byte[32],
            supervisedHandle: "ward",
            kind: "feed_source",
            peerActorId: new byte[32],
            peerAddress: "",
            messageId: Array.Empty<byte>(),
            summary: "a science feed",
            peerHandle: "",
            bridgeId: "bluesky",
            operation: "follow",
            target: "did:plc:example",
            createdAt: 0L));
        Assert.Equal("bluesky · follow · did:plc:example — “a science feed”", row.DisplayText);
    }
}
