using System;
using System.Collections.Generic;
using System.Linq;
using Fauna.Generated;
using FaunaApp.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.System;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 5 of handle-first onboarding: DNS configuration. Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/dns_config.rs</c>.
///
/// Provider-row generation lives here (not in
/// <see cref="OnboardingViewModel"/>) because the generated
/// <see cref="Fauna.Generated.Providers"/> registry is in the FaunaApp
/// assembly, not FaunaApp.Core. The VM exposes the underlying machine
/// methods (<c>SelectDnsProvider</c>, <c>VisibleDnsFields</c>, etc.) and
/// this view stitches them together with the registry.
/// </summary>
public sealed partial class DnsConfigView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    /// <summary>
    /// One row entry per DNS-capable provider. Each entry caches the
    /// generated <see cref="ProviderMeta"/> alongside the Button that
    /// represents it in <c>DnsProviderRow</c> so the per-tick refresh
    /// can flip <see cref="Button.IsEnabled"/> without rebuilding, and the
    /// reason TextBlock painted beside it when ineligible (linux parity —
    /// <c>dns_provider_ineligible_reason</c>, the sibling getter to the
    /// eligibility predicate so the two can never disagree).
    /// </summary>
    private readonly List<(ProviderMeta Provider, Button Button, TextBlock Reason)> _dnsProviderEntries = new();

    public DnsConfigView()
    {
        InitializeComponent();
    }

    internal DnsConfigView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        BuildDnsProviderRow();

        // x:Bind generates one-time evaluations for OneWay-on-non-INPC
        // sources; the VM raises PropertyChanged with empty PropertyName
        // on every observer tick, and Bindings.Update() re-runs them all.
        // Without this, IsEnabled / IsChecked / Visibility / VisibleDnsFields
        // would freeze at their initial values.
        ViewModel.PropertyChanged += (_, _) =>
        {
            Bindings.Update();
            // Per-button enable recomputed against current VM state.
            RefreshDnsProviderEnablement();
            // Refresh the help / link / browser button content for the
            // currently selected provider — they live outside the VM.
            UpdateProviderSectionContent();
            // hosted-auth field button's label/pressability (onboarding.md
            // § 4) — CredsForm owns no tick of its own, mirrors
            // RefreshDnsProviderEnablement above.
            CredsForm.RefreshHostedAuthButtons();
        };

        UpdateProviderSectionContent();
    }

    /// <summary>
    /// Populate <c>DnsProviderRow</c> with one Button per DNS-capable
    /// provider. AutomationId pattern <c>dns-provider-row[&lt;provider_id&gt;]</c>
    /// matches the indexed lookup in
    /// <c>tests/e2e-unified/tests/test_dns_config.py</c>. Each Button's
    /// click forwards to <see cref="OnboardingViewModel.SelectDnsProvider"/>;
    /// the machine then fires PropertyChanged so the per-provider section
    /// rebuilds via <see cref="UpdateProviderSectionContent"/>.
    /// </summary>
    private void BuildDnsProviderRow()
    {
        DnsProviderRow.Children.Clear();
        _dnsProviderEntries.Clear();
        foreach (var p in Providers.All.Where(p => p.Capabilities.Contains(Capability.Dns)))
        {
            // DisplayNameKey is dotted (e.g. "provisioning.cloudflare.name");
            // resw stores slashes. Fall back to the raw key on miss.
            var slashed = p.DisplayNameKey.Replace('.', '/');
            var resolved = S.Get(slashed);
            var label = resolved == slashed ? p.DisplayNameKey : resolved;

            var btn = new Button { Content = label };
            btn.SetValue(AutomationProperties.AutomationIdProperty, $"dns-provider-row[{p.Id}]");
            var pid = p.Id;
            btn.Click += (_, _) => ViewModel.SelectDnsProvider(pid);

            // Ineligibility reason, painted beside the button (linux parity —
            // dns_config.rs's provider_widgets (btn, reason) pair). Hidden
            // when the provider IS eligible; nothing to explain.
            var reason = new TextBlock
            {
                FontSize = 11,
                Opacity = 0.7,
                TextWrapping = TextWrapping.Wrap,
                Visibility = Visibility.Collapsed,
            };
            var stack = new StackPanel { Orientation = Orientation.Vertical, Spacing = 2 };
            stack.Children.Add(btn);
            stack.Children.Add(reason);
            DnsProviderRow.Children.Add(stack);
            _dnsProviderEntries.Add((p, btn, reason));
        }
        RefreshDnsProviderEnablement();
    }

    /// <summary>
    /// Update each provider button's IsEnabled to reflect the current
    /// checkbox state, and its reason text when disabled. Queries the shared
    /// machine's canonical eligibility predicate (<c>dns_provider_eligible</c>,
    /// lifted) and its sibling getter (<c>dns_provider_ineligible_
    /// reason</c>) rather than re-deriving the registrar/vps capability rule here.
    /// </summary>
    private void RefreshDnsProviderEnablement()
    {
        if (ViewModel is null) return;
        foreach (var (p, btn, reason) in _dnsProviderEntries)
        {
            btn.IsEnabled = ViewModel.DnsProviderEligible(p.Id);
            var text = ViewModel.DnsProviderIneligibleReason(p.Id);
            reason.Text = text;
            reason.Visibility = text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    /// <summary>
    /// Update the per-provider link / open-browser / help-text content
    /// from the registry whenever the selected provider id changes. The
    /// rest of the section (credentials form, verify, status, price)
    /// reads from the VM via x:Bind.
    /// </summary>
    private void UpdateProviderSectionContent()
    {
        var pid = ViewModel?.SelectedDnsProviderId;
        if (pid is null) return;
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
    }

    /// <summary>
    /// Both the HyperlinkButton and the Open-Browser button route here.
    /// Routed through <see cref="Services.UrlOpener"/> — the shell's one opener
    /// seam — so the system handler picks up the user's default browser and a
    /// harness launch records the address instead of reaching the OS. The
    /// HyperlinkButton's built-in
    /// NavigateUri does the same thing for valid URIs, but the explicit
    /// path matches the Open-Browser button and survives invalid URLs.
    /// </summary>
    private void OnProviderLinkClick(object sender, RoutedEventArgs e)
    {
        var pid = ViewModel?.SelectedDnsProviderId;
        if (pid is null) return;
        var p = Providers.All.FirstOrDefault(x => x.Id == pid);
        if (p is null) return;
        FaunaApp.Services.UrlOpener.Open(p.SignupUrl, "Onboarding");
    }

    /// <summary>
    /// x:Bind helper bridging a plain <see cref="bool"/> on the
    /// cross-platform VM to <see cref="Visibility"/>. Mirrors the
    /// <c>NestSelectView.BoolToVisibility</c> pattern; FaunaApp.Core
    /// (net10.0) can't reference WinUI types directly so the VM exposes
    /// booleans and each platform converts them.
    /// </summary>
    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>
    /// Resolved localized text for <c>dns-registrar-notes-text</c>. Empty
    /// when the selected provider has no <c>RegistrarNotesKey</c> or no
    /// provider is selected. Lives on the codebehind (not the VM) because
    /// <see cref="Fauna.Generated.Providers"/> is in the FaunaApp assembly,
    /// not Core. Mirrors Linux dns_config.rs:382-388.
    /// </summary>
    public string RegistrarNotesText
    {
        get
        {
            var key = SelectedRegistrarNotesKey();
            if (key is null) return "";
            // Dotted i18n keys → slashed resw resource ids. Fall back to
            // the bare key (so a future-key surfaces obviously rather than
            // rendering empty), matching ProviderHelpText's resolution.
            var slashed = key.Replace('.', '/');
            var resolved = S.Get(slashed);
            return resolved == slashed ? key : resolved;
        }
    }

    /// <summary>
    /// Visibility gate for <c>dns-registrar-notes-text</c>. Queries the shared
    /// machine's canonical predicate (<c>should_show_registrar_notes</c> —
    /// buy_domain AND the selected provider has registrar notes, lifted) rather than re-deriving it. The note text is still resolved
    /// platform-side (<see cref="RegistrarNotesText"/>) from the generated
    /// registry's i18n key.
    /// </summary>
    public bool RegistrarNotesVisible => ViewModel?.ShouldShowRegistrarNotes() == true;

    private string? SelectedRegistrarNotesKey()
    {
        var pid = ViewModel?.SelectedDnsProviderId;
        if (string.IsNullOrEmpty(pid)) return null;
        return Providers.All.FirstOrDefault(x => x.Id == pid)?.RegistrarNotesKey;
    }
}

