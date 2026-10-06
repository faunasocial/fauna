using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_conversations;

namespace FaunaApp.Controls;

public sealed partial class ThreadHeader : UserControl
{
    public event EventHandler? RenameRequested;
    public event EventHandler? AddParticipantRequested;

    /// <summary><c>thread-room-settings-button</c> was pressed — open the room
    /// policy editor (the <c>room_settings</c> sub-page).</summary>
    public event EventHandler? RoomSettingsRequested;

    /// <summary>A member chip's <c>thread-member-keep-button</c> was pressed —
    /// the pressed person's raw id as lowercase hex (the chip's own
    /// <c>PersonHex</c>, never re-derived).
    /// succession-aftermath.md § Propagation → *Removing a flagged member*.</summary>
    public event EventHandler<string>? MemberKeepRequested;

    /// <summary>A <c>thread-member-chip</c> was pressed on a
    /// membership-change-capable thread — the chip's own bound
    /// <c>TypedAddress</c> (never re-derived by index at tap time, so a
    /// roster that shifts between paint and tap still removes the person the
    /// chip named — <c>TypedAddress::same_participant</c> keys on actor id).
    /// conversations.md § the <c>thread-member-chip[i]</c> row.</summary>
    internal event EventHandler<TypedAddress>? MemberRemoveRequested;

    private bool _supportsMembershipChange;
    private bool _canRemoveMembers = true;

    public ThreadHeader() { InitializeComponent(); }

    public void SetLabel(string label) => ThreadLabel.Text = label;

    public void SetProtocolIcon(string glyph) => ProtocolIcon.Text = glyph;

    /// <summary>
    /// Gate the header's affordances. <paramref name="canInvite"/> and
    /// <paramref name="canRemoveMembers"/> are the ROLE-gated halves — the
    /// roles table applied once in shared Rust (<c>RoomSnapshot::gate</c>),
    /// never re-derived here (<c>conversation-rooms.md</c> § Roles and
    /// authorization). They are <c>true</c> on every thread that is not a
    /// governed room, so a mail or policy-less thread behaves exactly as before.
    /// </summary>
    public void SetCapabilities(
        bool supportsRename,
        bool supportsMembershipChange,
        bool canInvite,
        bool canRemoveMembers)
    {
        RenameButton.Visibility = supportsRename ? Visibility.Visible : Visibility.Collapsed;
        AddParticipantButton.Visibility = supportsMembershipChange ? Visibility.Visible : Visibility.Collapsed;
        // ...and GREY it — never hide it — when the viewer's role in a governed
        // room may not invite (`ui/conversations.md` § Architectural rules 5).
        AddParticipantButton.IsEnabled = canInvite;
        // Chips always render (they show who's in the thread), so unlike
        // AddParticipantButton this can't gate via Visibility — a mail chip
        // must keep looking like a plain pill, never a disabled-looking
        // control (reference_windows_disabled_button_no_invoke.md: keep
        // always-enabled, guard in the handler instead). The one case that DOES
        // grey a chip is the governed room's plain member — mutable membership,
        // but no role to remove with — and that greying rides each chip's own
        // `RemoveEnabled`, set where the chips are built.
        _supportsMembershipChange = supportsMembershipChange;
        _canRemoveMembers = canRemoveMembers;
    }

    /// <summary>
    /// The room's class statement and the editor's door — read off the
    /// projected room, never computed here (<c>conversation-rooms.md</c> § The
    /// three classes). Both stay collapsed where the rail models no room; where
    /// it does, the button is painted AND LIVE for every member
    /// (<c>ui/conversations.md</c> § Element IDs, widened 2026-09-20): the
    /// walk-out lives inside the editor and is a plain member's verb, so gating
    /// the door on <c>can_set_policy</c> put the one control a member needs behind
    /// the one capability a member never has. The greying is per control INSIDE
    /// the editor (<c>ConversationsPage.PaintRoomEditorGates</c>), never here.
    /// </summary>
    /// <param name="classLabel">The localized class name, or null for no room.</param>
    /// <param name="classToken">The driver-facing `class` attribute token.</param>
    public void SetRoom(string? classLabel, string? classToken)
    {
        if (classLabel is null)
        {
            RoomClassLabel.Visibility = Visibility.Collapsed;
            RoomSettingsButton.Visibility = Visibility.Collapsed;
            return;
        }
        RoomClassLabel.Text = classLabel;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            RoomClassLabel, classToken ?? string.Empty);
        RoomClassLabel.Visibility = Visibility.Visible;
        RoomSettingsButton.Visibility = Visibility.Visible;
        RoomSettingsButton.IsEnabled = true;
    }

    private void OnRenameClicked(object sender, RoutedEventArgs e)
    {
        RenameRequested?.Invoke(this, EventArgs.Empty);
    }

    private void OnAddParticipantClicked(object sender, RoutedEventArgs e)
    {
        AddParticipantRequested?.Invoke(this, EventArgs.Empty);
    }

    private void OnRoomSettingsClicked(object sender, RoutedEventArgs e)
    {
        RoomSettingsRequested?.Invoke(this, EventArgs.Empty);
    }

    private void OnMemberKeepClicked(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string personHex })
            MemberKeepRequested?.Invoke(this, personHex);
    }

    private void OnMemberChipClicked(object sender, RoutedEventArgs e)
    {
        // Gate on supports_membership_change exactly as AddParticipantButton's
        // own visibility does — a mail chip removes nothing (mail membership
        // is immutable, conversations.md § Participants vs. reply recipients) —
        // AND on the viewer's role, the same conjunction the chip's own
        // IsEnabled paints. The greyed chip cannot raise this event at all, so
        // this half is the belt behind that gate, not the gate.
        if (!_supportsMembershipChange || !_canRemoveMembers) return;
        if (sender is Button { Tag: TypedAddress addr })
            MemberRemoveRequested?.Invoke(this, addr);
    }
}
