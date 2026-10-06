using FaunaApp.Core.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Helpers;

/// <summary>
/// The WinUI half of the offline-affordance gate — W4 (account-data-plane.md § Workstreams) phase 4 on windows
/// (<c>docs/goal/architecture/account-data-plane.md</c> § The offline-mutation
/// contract → <i>How a surface asks</i>).
///
/// <para>Everything <i>decided</i> lives in <see cref="OfflineGate"/> in
/// <c>FaunaApp.Core</c>, over <see cref="IGatedControl"/>, so the registry's four
/// traps are unit-testable without XAML (<c>OfflineGateTests</c>). This file is
/// only the toolkit adapter: reading a <see cref="Control"/>'s own enablement,
/// mapping <c>IsEnabledChanged</c>, and putting the reason on screen as the
/// inline caption windows' own idiom calls for.</para>
///
/// <para>⚠ This file is EXCLUDED from the dev-fleet offline-gate-kinds checker's
/// scan, for android's reason #1: C# spells a method definition the same way it
/// spells a call, so <c>FaunaGate(this Control …)</c> below would read as a
/// declaration site with no kind literal and red rule 1. Never put a real
/// declaration in here.</para>
/// </summary>
internal static class OfflineGateExtensions
{
    /// <summary>
    /// Declare that this control actuates <paramref name="kind"/>, and gate it
    /// from now on.
    ///
    /// <para>Call it once, at construction, beside the control's other setup —
    /// the only contribution a page author makes. Spell the kind as a literal at
    /// the call site: a computed kind cannot be checked, and the dev-fleet
    /// offline-gate-kinds checker refuses one.</para>
    ///
    /// <para>A control that issues nothing over the wire (dismissing a modal,
    /// revealing an inline form) simply does not call this.</para>
    /// </summary>
    internal static void FaunaGate(this Control control, string kind) =>
        OfflineGate.Shared.Declare(new ControlGate(control), kind);
}

/// <summary>
/// One WinUI <see cref="Control"/> as an <see cref="IGatedControl"/>.
///
/// <para>The control is held WEAKLY: the gate's registry outlives any one page, and
/// a navigated-away page's controls must be collectable rather than pinned for the
/// process's life. <see cref="IsAlive"/> answering <c>false</c> is what lets the
/// registry prune them.</para>
/// </summary>
internal sealed class ControlGate : IGatedControl
{
    /// <summary>
    /// The <see cref="AutomationProperties.ItemStatus"/> value <see cref="ControlGate"/>
    /// stamps on every control it wraps — a UIA-visible "a gate declared this
    /// control's enablement" marker, the windows twin of web's
    /// <c>data-offline-gate-declared</c> attribute (<c>web-bridge/server.py</c>'s
    /// registry-snapshot JS). UIA's raw <c>IsEnabled</c> cannot distinguish a
    /// DECLARED enablement from one that merely DEFAULTED to <c>true</c> — that
    /// is exactly the distinction a whole-frame checker needs (e2e convention 17)
    /// to tell a meaningful enablement delta from noise, so it needs its own
    /// channel rather than being inferred from a tag or a value. <c>ItemStatus</c>
    /// carries no other meaning on any gated control today, unlike
    /// <c>AutomationProperties.HelpText</c>, which several pages already use for
    /// their own per-control state (the FlaUI bridge's <c>/registry</c> route
    /// reads this same constant).
    /// </summary>
    internal const string GateDeclaredMarker = "fauna-gate-declared";

    private readonly WeakReference<Control> _control;

    /// <summary>
    /// The caption <see cref="TextBlock"/> this gate owns, created lazily beside
    /// the control the first time a reason is shown. Held strongly — it is ours,
    /// and it is reachable from the visual tree anyway.
    /// </summary>
    private TextBlock? _caption;

    internal ControlGate(Control control)
    {
        _control = new WeakReference<Control>(control);
        control.IsEnabledChanged += OnIsEnabledChanged;
        AutomationProperties.SetItemStatus(control, GateDeclaredMarker);
    }

    public event Action<bool>? EnabledChanged;

    public bool IsAlive => _control.TryGetTarget(out _);

    /// <summary>
    /// The <see cref="Control"/> itself — the identity the registry compares
    /// declarations by, since each <c>FaunaGate()</c> call makes a fresh wrapper.
    ///
    /// <para>Resolved through the weak reference rather than held, so this never
    /// pins the control: a strongly-held identity would keep the control alive,
    /// which would keep <see cref="IsAlive"/> answering <c>true</c>, which would
    /// stop the entry from ever being pruned — the weak reference's whole purpose,
    /// defeated. Once the control is gone the identity falls back to this wrapper,
    /// which matches nothing, and the entry is pruned on the next pass anyway.</para>
    /// </summary>
    public object Identity => _control.TryGetTarget(out var control) ? control : this;

    public void Detach()
    {
        if (_control.TryGetTarget(out var control)) control.IsEnabledChanged -= OnIsEnabledChanged;
    }

    /// <summary>
    /// The control's own <c>IsEnabled</c>.
    ///
    /// <para>Trap 1 asks for the control's OWN value rather than an
    /// ancestor-folded one. WinUI's <c>Control.IsEnabled</c> is the property the
    /// page writes and the one <c>IsEnabledChanged</c> reports on, so the two
    /// agree by construction here. ⚠ A control nested inside another
    /// <i>disabled <c>Control</c></i> is the case where that could stop being
    /// true; the surfaces declared so far sit directly in <c>Panel</c>s (which
    /// have no <c>IsEnabled</c>), so it does not arise yet. A sweep that reaches
    /// a control nested in a disabled <c>ContentControl</c> should pin the
    /// behaviour with a test before trusting this.</para>
    /// </summary>
    public bool IsEnabled
    {
        get => _control.TryGetTarget(out var control) && control.IsEnabled;
        set
        {
            if (_control.TryGetTarget(out var control)) control.IsEnabled = value;
        }
    }

    /// <summary>
    /// The inline reason caption — windows' own disabled-with-a-reason idiom, not
    /// a tooltip.
    ///
    /// <para><c>FoldersPage</c> already ruled a tooltip insufficient here in so
    /// many words ("a tooltip alone is not discoverable on a control the user
    /// cannot focus"), and the reason must show per affordance, never as a global
    /// banner (§ R11). So this appends a dimmed caption directly after the
    /// control in its parent panel, matching that idiom's shape exactly, and
    /// collapses it again when the gate withdraws — the element stays in the tree
    /// so the sibling indices around it never shift.</para>
    /// </summary>
    public string? GateReason
    {
        get => _caption is { Visibility: Visibility.Visible } caption ? caption.Text : null;
        set
        {
            if (value is null)
            {
                if (_caption is null) return;
                _caption.Text = string.Empty;
                _caption.Visibility = Visibility.Collapsed;
                return;
            }

            EnsureCaption();
            if (_caption is null) return; // no panel parent to host one
            _caption.Text = value;
            _caption.Visibility = Visibility.Visible;
        }
    }

    private void EnsureCaption()
    {
        if (_caption is not null) return;
        if (!_control.TryGetTarget(out var control)) return;
        if (control.Parent is not Panel panel) return;
        var index = panel.Children.IndexOf(control);
        if (index < 0) return;

        // The FoldersPage idiom, field for field.
        _caption = new TextBlock
        {
            Opacity = 0.6,
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Visibility = Visibility.Collapsed,
        };
        panel.Children.Insert(index + 1, _caption);
    }

    private void OnIsEnabledChanged(object sender, DependencyPropertyChangedEventArgs e) =>
        EnabledChanged?.Invoke(sender is Control control && control.IsEnabled);
}
