using System;
using System.Collections.Generic;
using System.Linq;
using Fauna.Generated;
using FaunaApp.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.System;
using UpdateChannel = uniffi.fauna_provisioning.UpdateChannel;
using S = FaunaApp.Core.Services.Strings;
using FieldMetaPlain = uniffi.fauna_onboarding_machine.FieldMetaPlain;
using FieldTypePlain = uniffi.fauna_onboarding_machine.FieldTypePlain;
using CapabilityPlain = uniffi.fauna_onboarding_machine.CapabilityPlain;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 6 of handle-first onboarding: VPS configuration. Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/vps_config.rs</c> and
/// <see cref="DnsConfigView"/>.
///
/// Provider-row generation lives here (not in
/// <see cref="OnboardingViewModel"/>) because the generated
/// <see cref="Fauna.Generated.Providers"/> registry is in the FaunaApp
/// assembly, not FaunaApp.Core. The VM exposes the underlying machine
/// methods (<c>SelectVpsProvider</c>, <c>VpsServerTypes</c>, etc.) and
/// this view stitches them together with the registry.
///
/// Visible-fields filtering happens in the shared <c>OnboardingMachine</c>
/// (<c>visible_vps_fields()</c>); this view just binds the result.
/// </summary>
public sealed partial class VpsConfigView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    /// <summary>
    /// One row entry per VPS-eligible provider (capabilities includes
    /// <see cref="Capability.Vps"/> AND <c>CuratedOffers</c> non-empty).
    /// Cached so future per-tick refreshes can mutate buttons without
    /// rebuilding the row.
    /// </summary>
    private readonly List<(ProviderMeta Provider, Button Button)> _vpsProviderEntries = new();

    /// <summary>
    /// The provider id <see cref="CredsForm"/> was last populated for.
    /// Used to skip rebuilds when the selected provider hasn't changed
    /// — avoids destroying half-typed credentials on every observer tick
    /// (matches the Linux <c>last_pid</c> guard).
    /// </summary>
    private string? _credsBoundForProviderId;

    /// <summary>
    /// Suppresses the <c>MailModeCheck</c> Checked/Unchecked handlers while
    /// <see cref="SyncMailModeCheck"/> programmatically syncs IsChecked from
    /// the snapshot — otherwise the sync would re-fire
    /// <c>SetProvisionMailMode</c>.
    /// </summary>
    private bool _syncingMailMode;

    public VpsConfigView()
    {
        InitializeComponent();
    }

    internal VpsConfigView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        BuildVpsProviderRow();
        BuildUpdateChannelRows();

        // Same x:Bind refresh pattern as DnsConfigView: the VM raises
        // PropertyChanged with empty PropertyName on every observer tick,
        // and Bindings.Update() re-runs all OneWay sources. Without this,
        // IsEnabled / IsChecked / Visibility / VpsLocations / VpsServerTypes
        // would freeze at their initial values.
        ViewModel.PropertyChanged += (_, _) =>
        {
            Bindings.Update();
            UpdateProviderSectionContent();
            SyncMailModeCheck();
            SyncUpdateChannelRadios();
            // hosted-auth field button's label/pressability (onboarding.md
            // § 4) — CredsForm owns no tick of its own, matches DnsConfigView.
            CredsForm.RefreshHostedAuthButtons();
        };

        UpdateProviderSectionContent();
        SyncMailModeCheck();
        SyncUpdateChannelRadios();
    }

    /// <summary>
    /// Populate <c>VpsProviderRow</c> with one Button per VPS-eligible
    /// provider. AutomationId pattern <c>vps-provider-row[&lt;provider_id&gt;]</c>
    /// matches the indexed lookup in
    /// <c>tests/e2e-unified/tests/test_vps_config.py</c>.
    ///
    /// The eligibility filter mirrors <c>ui.yaml</c>'s
    /// <c>generated_filter: "capabilities includes 'vps' and
    /// len(curated_offers) &gt; 0"</c>: providers without a curated server
    /// list (e.g. OVH) can't surface a fixed radio group, so they're
    /// excluded from the row entirely.
    /// </summary>
    private void BuildVpsProviderRow()
    {
        VpsProviderRow.Children.Clear();
        _vpsProviderEntries.Clear();
        foreach (var p in Providers.All
                     .Where(p => p.Capabilities.Contains(Capability.Vps)
                                 && p.CuratedOffers.Length > 0))
        {
            // DisplayNameKey is dotted (e.g. "provisioning.hetzner.name");
            // resw stores slashes. Fall back to the raw key on miss
            // (mirrors DnsConfigView).
            var slashed = p.DisplayNameKey.Replace('.', '/');
            var resolved = S.Get(slashed);
            var label = resolved == slashed ? p.DisplayNameKey : resolved;

            var btn = new Button { Content = label };
            btn.SetValue(AutomationProperties.AutomationIdProperty, $"vps-provider-row[{p.Id}]");
            var pid = p.Id;
            btn.Click += (_, _) => ViewModel.SelectVpsProvider(pid);
            VpsProviderRow.Children.Add(btn);
            _vpsProviderEntries.Add((p, btn));
        }
    }

    /// <summary>
    /// Update the per-provider link / open-browser / help-text content
    /// from the registry, and rebuild the credentials form's field list
    /// when the selected provider changes. The rest of the section
    /// (verify button, location picker, server-type radios) reads
    /// directly from the VM via x:Bind.
    /// </summary>
    private void UpdateProviderSectionContent()
    {
        var pid = ViewModel?.SelectedVpsProviderId;
        if (pid is null)
        {
            // Clearing the bound provider id ensures a future re-selection
            // forces a creds-form rebuild even if the user picked the same
            // provider twice (rare, but matches the Linux behavior).
            _credsBoundForProviderId = null;
            return;
        }
        var p = Providers.All.FirstOrDefault(x => x.Id == pid);
        if (p is null) return;

        ProviderLink.Content              = p.SignupUrl;
        ProviderOpenBrowserBtn.Tag        = p.SignupUrl;
        ProviderLink.Tag                  = p.SignupUrl;
        // HelpKey is the dotted-yaml key; resw uses slashes — convert,
        // and fall back to the bare key if the localizer doesn't know it
        // (matches GenericProviderForm.ResolveLabel).
        var helpSlashed = p.HelpKey.Replace('.', '/');
        var helpResolved = S.Get(helpSlashed);
        ProviderHelpText.Text = helpResolved == helpSlashed ? p.HelpKey : helpResolved;

        // Rebuild the credentials form on provider change. The shared
        // OnboardingMachine filters fields by Capability::Vps and returns
        // a FieldMetaPlain[] ready to feed to GenericProviderForm.
        if (_credsBoundForProviderId != pid)
        {
            // ViewModel is non-null here — UpdateProviderSectionContent is
            // only ever called from BuildVpsProviderRow, which runs from the
            // ViewModel-bound constructor.
            CredsForm.Fields = ViewModel!.VisibleVpsFields;
            _credsBoundForProviderId = pid;
        }
    }

    /// <summary>
    /// Both the HyperlinkButton and the Open-Browser button route here.
    /// Routed through <see cref="Services.UrlOpener"/> — the shell's one opener
    /// seam — so the system handler picks up the user's default browser, and a
    /// harness launch records the address instead of reaching the OS.
    /// </summary>
    private void OnProviderLinkClick(object sender, RoutedEventArgs e)
    {
        var pid = ViewModel?.SelectedVpsProviderId;
        if (pid is null) return;
        var p = Providers.All.FirstOrDefault(x => x.Id == pid);
        if (p is null) return;
        FaunaApp.Services.UrlOpener.Open(p.SignupUrl, "Onboarding");
    }

    /// <summary>
    /// Forward radio-button selection to the machine. We read the bound
    /// <see cref="ServerTypeItemViewModel"/> from the RadioButton's
    /// DataContext rather than relying on x:Bind two-way (RadioButton
    /// IsChecked TwoWay across DataTemplate items can fire the wrong
    /// item's setter mid-rebuild).
    /// </summary>
    private void OnVpsServerTypeChecked(object sender, RoutedEventArgs e)
    {
        if (sender is not RadioButton rb) return;
        if (rb.DataContext is not ServerTypeItemViewModel item) return;
        ViewModel?.SelectVpsServerType(item.Id);
    }

    /// <summary>
    /// Re-sync the mail-mode CheckBox from the snapshot, guarded so the
    /// programmatic update doesn't re-fire <c>SetProvisionMailMode</c>. The
    /// default is handle-derived in the machine (ON for a real domain, OFF for
    /// localhost / IP), so the first render reflects ON/OFF without a click.
    /// The RAM-gated server-type radio re-renders via
    /// the OneWay x:Bind on <c>ViewModel.VpsServerTypes</c> (refreshed by
    /// <c>Bindings.Update()</c>), so this method only owns the checkbox state.
    /// Per onboarding.md §5.
    /// </summary>
    private void SyncMailModeCheck()
    {
        _syncingMailMode = true;
        try
        {
            if (MailModeCheck.IsChecked != ViewModel.ProvisionMailModeEnabled)
                MailModeCheck.IsChecked = ViewModel.ProvisionMailModeEnabled;
        }
        finally { _syncingMailMode = false; }
    }

    /// <summary>
    /// CheckBox toggle handler → forward the mail-vs-social choice to the
    /// machine (which clears a now-too-small server-type selection on mail-ON).
    /// Guarded against the programmatic re-sync in
    /// <see cref="SyncMailModeCheck"/>.
    /// </summary>
    private void MailModeCheck_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncingMailMode) return;
        ViewModel.SetProvisionMailMode(MailModeCheck.IsChecked == true);
    }

    /// <summary>
    /// The update channels in the shared <c>UpdateChannel::ALL</c> order, with
    /// the id each radio is keyed by and the resw key of its label.
    /// </summary>
    private static readonly (UpdateChannel Channel, string Id, string LabelKey)[] UpdateChannels =
    {
        (UpdateChannel.Stable, "stable", "onboarding/vps_config/update_channel_stable_label"),
        (UpdateChannel.Test,   "test",   "onboarding/vps_config/update_channel_test_label"),
        (UpdateChannel.Dev,    "dev",    "onboarding/vps_config/update_channel_dev_label"),
    };

    private readonly List<(UpdateChannel Channel, RadioButton Radio)> _updateChannelRadios = new();

    /// <summary>
    /// Suppresses <c>OnUpdateChannelChecked</c> while
    /// <see cref="SyncUpdateChannelRadios"/> programmatically moves the
    /// selection — otherwise the sync would re-fire <c>SetProvisionUpdateChannel</c>.
    /// </summary>
    private bool _syncingUpdateChannel;

    /// <summary>
    /// Populate <c>UpdateChannelRow</c> with one RadioButton per channel,
    /// <c>vps-config-update-channel-row[&lt;id&gt;]</c>, each showing its label
    /// and one-line description. The description is looked up by the key the
    /// shared <c>UpdateChannelDescriptionKey</c> hands back (the compat-line
    /// wording while the product major is 0) — never a per-channel constant.
    /// Mirrors <c>apps/fauna-linux/src/views/onboarding/vps_config.rs</c>.
    /// </summary>
    private void BuildUpdateChannelRows()
    {
        foreach (var (channel, id, labelKey) in UpdateChannels)
        {
            var label = S.Get(labelKey);
            var descDotted = OnboardingViewModel.UpdateChannelDescriptionKey(channel);
            var descSlashed = descDotted.Replace('.', '/');
            var descResolved = S.Get(descSlashed);
            var desc = descResolved == descSlashed ? descDotted : descResolved;

            var content = new StackPanel { Spacing = 2 };
            content.Children.Add(new TextBlock { Text = label });
            content.Children.Add(new TextBlock
            {
                Text = desc,
                TextWrapping = TextWrapping.Wrap,
                Opacity = 0.7,
                FontSize = 12,
            });

            var radio = new RadioButton { Content = content, GroupName = "VpsUpdateChannel" };
            radio.SetValue(AutomationProperties.AutomationIdProperty, $"vps-config-update-channel-row[{id}]");
            radio.SetValue(AutomationProperties.NameProperty, label);
            var c = channel;
            radio.Checked += (_, _) => OnUpdateChannelChecked(c);
            UpdateChannelRow.Children.Add(radio);
            _updateChannelRadios.Add((channel, radio));
        }
    }

    /// <summary>Radio selection → the shared setter (guarded against the tick re-sync).</summary>
    private void OnUpdateChannelChecked(UpdateChannel channel)
    {
        if (_syncingUpdateChannel) return;
        ViewModel.SetProvisionUpdateChannel(channel);
    }

    /// <summary>
    /// Repaint the radios from the shared getter (the only source; nothing is
    /// cached here) and publish each row's on/off word in HelpText, which the
    /// e2e bridge reads as the <c>state</c> attribute.
    /// </summary>
    private void SyncUpdateChannelRadios()
    {
        var selected = ViewModel.ProvisionUpdateChannel;
        _syncingUpdateChannel = true;
        try
        {
            foreach (var (channel, radio) in _updateChannelRadios)
            {
                var on = channel == selected;
                if (radio.IsChecked != on) radio.IsChecked = on;
                radio.SetValue(AutomationProperties.HelpTextProperty, on ? "on" : "off");
            }
        }
        finally { _syncingUpdateChannel = false; }
    }

    /// <summary>
    /// x:Bind helper bridging a plain <see cref="bool"/> on the
    /// cross-platform VM to <see cref="Visibility"/>. Mirrors the
    /// pattern used in <see cref="DnsConfigView.BoolToVisibility"/>.
    /// </summary>
    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Hide the Continue-blocked reason when there's nothing to explain
    /// (rule-5 render lift — the reason text is "" iff the button is enabled).</summary>
    public static Visibility TextToVisibility(string value)
        => value.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
}
