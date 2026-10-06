using FaunaApp.Core.Services;
using uniffi.fauna_conversations;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Windows's <c>room concept → localized string</c> map — the room model's one
/// genuinely platform-specific half.
///
/// <para><b>Why this file exists at all, given priority #2.</b> Shared Rust
/// already owns every room <em>decision</em> and every driver-facing
/// <em>token</em>, and this app consumes those directly:
/// <c>RoomSettingsDraft</c> stages the edits, <c>ApplyRoomSettings</c> commits
/// them, and <c>RoomClassAttrToken</c> / <c>RoomRoleAttrToken</c> /
/// <c>RoomJoinRuleToken</c> / <c>RoomHistoryPolicyToken</c> hand over the
/// attribute strings. Nothing about policy, roles or eligibility is retyped
/// here — the whole file is a lookup.</para>
///
/// <para>What it does NOT consume is the shared <em>label</em> helpers
/// (<c>room_class_label</c>, <c>room_member_chip_text</c>, …), and that is
/// deliberate: those return the English text out of
/// <c>fauna_i18n::strings</c> — the table tui, linux and FaunaKit localize
/// against — whereas this app's i18n table is its own <c>.resw</c>, reached by
/// key through <c>Strings.Get</c>. Passing shared English straight into a
/// <c>Text</c> property would be hardcoded English one layer up, which
/// <c>ui/conversations.md</c> § Architectural rules 3 forbids. So the shared
/// enum comes in and a resw key goes out, exactly as
/// <c>AdminCustodyHostingViewModel.ReceiptStateLabel</c> and
/// <c>AtprotoViewModel</c>'s status map already do for their own shared
/// enums.</para>
/// </summary>
internal static class RoomLabels
{
    /// <summary>The separator between a member's display name and their role
    /// mark. The one piece of shared FORMAT this file restates — it is
    /// <c>fauna_conversations::member_chip_text</c>'s
    /// (<c>libs/fauna-conversations/src/room.rs</c>), and the mark either side
    /// of it is localized here rather than there for the reason in the class
    /// doc.</summary>
    private const string ChipMarkSeparator = " · ";

    /// <summary>The class stated on <c>thread-room-class</c>
    /// (<c>conversation-rooms.md</c> § The three classes).</summary>
    internal static string ClassLabel(RoomClass roomClass) => Strings.Get(roomClass switch
    {
        RoomClass.Community => "conversations/unified/room_class_community",
        RoomClass.TransportOnly => "conversations/unified/room_class_transport_only",
        _ => "conversations/unified/room_class_end_to_end",
    });

    /// <summary>The owner/admin mark a chip carries, or <c>""</c> for a plain
    /// member — who is marked by NOT being marked, exactly as
    /// <c>RoomRole::chip_mark</c> returns <c>None</c> there.</summary>
    internal static string RoleMark(RoomRole role) => role switch
    {
        RoomRole.Owner => Strings.Get("conversations/unified/room_role_owner"),
        RoomRole.Admin => Strings.Get("conversations/unified/room_role_admin"),
        _ => string.Empty,
    };

    /// <summary><c>thread-member-chip[i]</c>'s text: the display name, plus the
    /// role mark on a governed room. <paramref name="role"/> is null on a
    /// policy-less room and on every non-room thread.</summary>
    internal static string MemberChipText(string display, RoomRole? role)
    {
        if (role is null) return display;
        var mark = RoleMark(role.Value);
        return mark.Length == 0 ? display : display + ChipMarkSeparator + mark;
    }

    /// <summary>The <c>room-join-rule-select</c> option's human label.
    /// <c>Request</c> is not an editor choice (it is the community class's
    /// rule, refused by <c>RoomPolicy::validate</c> on an end-to-end policy),
    /// so it falls through to the invite label rather than earning a
    /// string.</summary>
    internal static string JoinRuleLabel(JoinRule rule) => Strings.Get(rule switch
    {
        JoinRule.MemberInvite => "conversations/unified/room_join_rule_member_invite",
        _ => "conversations/unified/room_join_rule_invite",
    });

    /// <summary>The <c>room-history-policy-select</c> option's human label
    /// (<c>conversation-rooms.md</c> § History for joiners).</summary>
    internal static string HistoryPolicyLabel(HistoryPolicy policy) => Strings.Get(policy switch
    {
        HistoryPolicy.Full => "conversations/unified/room_history_policy_full",
        _ => "conversations/unified/room_history_policy_none",
    });

    /// <summary><c>room-admin-toggle[i]</c>'s own text, off its staged state.</summary>
    internal static string AdminToggleLabel(bool staged) => Strings.Get(staged
        ? "conversations/unified/room_admin_yes"
        : "conversations/unified/room_admin_no");

    /// <summary><c>room-owner-transfer-button[i]</c>'s own text, off its staged
    /// state — at most one row ever reads as staged
    /// (<c>RoomSettingsDraft::toggle_transfer</c>).</summary>
    internal static string TransferToggleLabel(bool staged) => Strings.Get(staged
        ? "conversations/unified/room_transfer_staged"
        : "conversations/unified/room_transfer_mark");
}
