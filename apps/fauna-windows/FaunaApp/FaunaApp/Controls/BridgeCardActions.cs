using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// What a <see cref="BridgeCard"/>'s action button does: the link / unlink ceremony.
///
/// <para>Lifted here 2026-07-31 out of <c>BridgesPage.xaml.cs</c> when the Bluesky
/// page's Linked-account panel became a second host of the same card. Two pages
/// hand-rolling one provider's link dialog would be exactly the per-app-style
/// divergence priority #1 exists to prevent — in miniature, inside one app.</para>
///
/// <para>There is no per-bridge hard-coded field set anywhere in it: the form is built
/// from the provider's own declared <see cref="BridgeLinkMode"/> metadata
/// (<c>bridges.md</c> § Link modes / § Element IDs), mirroring linux
/// <c>views/bridges/detail.rs</c> and web <c>bridges/+page.svelte</c>.</para>
/// </summary>
internal static class BridgeCardActions
{
    /// <summary>Run the action the card's button offers for <paramref name="bridge"/>:
    /// confirm-then-unlink when it is linked, else the metadata-driven link dialog.
    /// Mutations go through <paramref name="vm"/>, which re-loads itself on success —
    /// so the card repaints from what the nest persisted, never from the tap.
    ///
    /// <para>Both dialogs build their own Primary/Cancel <see cref="Button"/>s inside
    /// <c>Content</c> rather than using <c>PrimaryButtonText</c>/<c>CloseButtonText</c>
    /// — a <see cref="ContentDialog"/>'s template-generated buttons carry no
    /// <see cref="AutomationProperties.AutomationIdProperty"/> and aren't automatable
    /// (established precedent: <c>FeedPage.xaml.cs</c>'s create-feed dialog).</para></summary>
    public static async Task RunAsync(XamlRoot xamlRoot, BridgesViewModel vm, BridgeInfo bridge)
    {
        if (bridge.Linked)
        {
            ContentDialog? unlinkDialog = null;
            bool shouldUnlink = false;

            // ui.yaml bridges § platform_elements.windows (user-approved 2026-07-31):
            // windows is the only client that confirms unlink through a modal — the
            // other 6 unlink directly from bridge-action-button, no confirm step.
            var panel = new StackPanel { Spacing = 8 };
            AutomationProperties.SetAutomationId(panel, Ids.BridgeUnlinkConfirmModal);
            // A bare layout panel carrying only an AutomationId is UIA-pruned — Name is
            // what materializes the peer (reference_winui_flaui_datatemplate_name; this
            // is the C#-code-behind twin of the XAML trap windows-name-lint guards).
            AutomationProperties.SetName(panel, "bridge-unlink-confirm-modal");
            panel.Children.Add(new TextBlock
            {
                Text = S.Get("bridges/unlink_confirm_msg"),
                TextWrapping = TextWrapping.Wrap,
            });

            var unlinkButtonRow = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 8,
                HorizontalAlignment = HorizontalAlignment.Right,
            };
            var unlinkCancelBtn = new Button { Content = S.Get("common/cancel") };
            AutomationProperties.SetAutomationId(unlinkCancelBtn, Ids.BridgeUnlinkCancelButton);
            unlinkCancelBtn.Click += (_, _) => unlinkDialog?.Hide();
            var unlinkConfirmBtn = new Button
            {
                Content = S.Get("bridges/unlink_action"),
                Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            };
            AutomationProperties.SetAutomationId(unlinkConfirmBtn, Ids.BridgeUnlinkConfirmButton);
            unlinkConfirmBtn.Click += (_, _) =>
            {
                shouldUnlink = true;
                unlinkDialog?.Hide();
            };
            unlinkButtonRow.Children.Add(unlinkCancelBtn);
            unlinkButtonRow.Children.Add(unlinkConfirmBtn);
            panel.Children.Add(unlinkButtonRow);

            var dialog = new ContentDialog
            {
                Title = $"{S.Get("bridges/unlink_action")} {bridge.DisplayName}?",
                Content = panel,
                XamlRoot = xamlRoot,
            };
            unlinkDialog = dialog;

            E2eTrace.Write($"[bridge-unlink] dialog open ({bridge.Id})");
            await Dialogs.ShowAsync(dialog);
            // `ShowAsync` returns on ANY close (confirm, cancel, ESC, external
            // dismissal); `shouldUnlink` is the only witness of which — trace it,
            // or a silently-cancelled dialog is indistinguishable from a lost RPC.
            E2eTrace.Write($"[bridge-unlink] dialog closed, confirmed={shouldUnlink} ({bridge.Id})");

            if (shouldUnlink)
            {
                await vm.UnlinkBridgeCommand.ExecuteAsync(bridge.Id);
            }
        }
        else
        {
            var modes = BridgeLinkForm.ModesForPlatform(bridge);
            var (fieldsContent, collect) = BuildLinkDialogContent(modes, bridge);

            ContentDialog? linkDialog = null;
            bool shouldLink = false;

            var linkButtonRow = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 8,
                HorizontalAlignment = HorizontalAlignment.Right,
            };
            var linkCancelBtn = new Button { Content = S.Get("common/cancel") };
            linkCancelBtn.Click += (_, _) => linkDialog?.Hide();
            var linkConfirmBtn = new Button
            {
                Content = S.Get("bridges/link_action"),
                Style = (Style)Application.Current.Resources["AccentButtonStyle"],
                IsEnabled = modes.Count > 0,
            };
            // Reuses bridge-action-button (bridges.md § User actions: "the SAME id for
            // both the link and unlink action" — this is that same action, just
            // realized as the dialog's submit step), scoped by bridge-link-form to
            // disambiguate it from the card's trigger button, which stays in the tree
            // behind the modal — mirrors linux's views/bridges/detail.rs exactly.
            AutomationProperties.SetAutomationId(linkConfirmBtn, Ids.BridgeActionButton);
            linkConfirmBtn.Click += (_, _) =>
            {
                shouldLink = true;
                linkDialog?.Hide();
            };
            linkButtonRow.Children.Add(linkCancelBtn);
            linkButtonRow.Children.Add(linkConfirmBtn);

            var wrapper = new StackPanel { Spacing = 8 };
            AutomationProperties.SetAutomationId(wrapper, Ids.BridgeLinkForm);
            // Same bare-layout-panel trap as bridge-unlink-confirm-modal above — Name
            // materializes the peer UIA would otherwise prune.
            AutomationProperties.SetName(wrapper, "bridge-link-form");
            wrapper.Children.Add(fieldsContent);
            wrapper.Children.Add(linkButtonRow);

            var dialog = new ContentDialog
            {
                Title = $"{S.Get("bridges/link_action")} {bridge.DisplayName}",
                Content = wrapper,
                XamlRoot = xamlRoot,
            };
            linkDialog = dialog;

            E2eTrace.Write($"[bridge-link] dialog open ({bridge.Id}, modes={modes.Count})");
            await Dialogs.ShowAsync(dialog);
            // Same witness as the unlink half: `confirmed=False` here means the
            // dialog closed WITHOUT the submit handler running (ESC / cancel /
            // external dismissal) and no RPC follows by design.
            E2eTrace.Write($"[bridge-link] dialog closed, confirmed={shouldLink} ({bridge.Id})");

            if (shouldLink)
            {
                var result = collect();
                if (result.HasValue)
                    await vm.LinkBridgeAsync(bridge.Id, result.Value.mode, result.Value.fields);
            }
        }
    }

    /// <summary>
    /// Build the metadata-driven link form for <paramref name="modes"/> (the
    /// platform-applicable <see cref="BridgeLinkMode"/>s; bridges.md § Link modes +
    /// § Element IDs). Each provider-declared field renders as a labelled
    /// <c>bridge-link-field-{key}</c> input — a <see cref="PasswordBox"/> for a
    /// <c>secret</c> field, a <see cref="TextBox"/> otherwise — and when a provider
    /// offers more than one applicable mode a selector switches between them. The
    /// returned collector yields the selected mode + its non-empty field values (the
    /// <c>fauna.bridges.link</c> params), or <c>null</c> when the bridge declares no
    /// linkable mode.
    /// </summary>
    private static (UIElement content, Func<(string mode, Dictionary<string, string> fields)?> collect)
        BuildLinkDialogContent(IReadOnlyList<BridgeLinkMode> modes, BridgeInfo bridge)
    {
        var panel = new StackPanel { Spacing = 8 };

        // Why linking is unavailable, from the shared Rust rule (null = it IS)
        // so windows cannot drift from the other six apps. This used to read
        // "No configuration needed." — the exact inverse of the truth: it told
        // the user everything was fine while the submit button sat disabled and
        // the nest's own explanation was thrown away at decode.
        var block = uniffi.fauna_ffi.FaunaFfiMethods.BridgeLinkBlock(
            bridge.Linked, bridge.Error, (uint)modes.Count);
        if (block is not null)
        {
            var reason = new TextBlock
            {
                Text = block switch
                {
                    uniffi.fauna_ffi.FfiLinkBlock.ProviderError pe => pe.@message,
                    _ => S.Get("bridges/no_link_method"),
                },
                TextWrapping = TextWrapping.Wrap,
            };
            // No element id here on purpose (it carried `bridge-link-blocked-reason`
            // until 2026-08-16). The spec'd home for that id is the CARD — bridges.md
            // § Errors & edge cases wants the reason beside the disabled action
            // button, where a user actually meets it; a copy inside this dialog is
            // unreachable once the button is correctly disabled, and a second element
            // carrying the same id would be a duplicate in the UIA tree (e2e
            // convention 1). The sentence stays as a belt-and-braces fallback for any
            // path that opens the dialog with no applicable mode.
            panel.Children.Add(reason);
            return (panel, () => null);
        }

        // Inputs for the currently-selected mode — rebuilt on a mode switch and read
        // by the collector. Each entry pairs the provider field key with a reader.
        var fieldInputs = new List<(string key, Func<string> read)>();
        var activeMode = modes[0];
        var fieldsPanel = new StackPanel { Spacing = 8 };

        void RenderFields(BridgeLinkMode mode)
        {
            fieldsPanel.Children.Clear();
            fieldInputs.Clear();
            foreach (var field in mode.Fields)
            {
                var testId = BridgeLinkForm.FieldTestId(field);
                if (field.IsSecret)
                {
                    var box = new PasswordBox { Header = field.Label, PlaceholderText = field.Placeholder ?? string.Empty };
                    AutomationProperties.SetAutomationId(box, testId);
                    fieldsPanel.Children.Add(box);
                    fieldInputs.Add((field.Key, () => box.Password));
                }
                else
                {
                    var box = new TextBox { Header = field.Label, PlaceholderText = field.Placeholder ?? string.Empty };
                    AutomationProperties.SetAutomationId(box, testId);
                    fieldsPanel.Children.Add(box);
                    fieldInputs.Add((field.Key, () => box.Text));
                }
            }
        }

        // Mode selector — only when the provider offers more than one applicable mode.
        if (modes.Count > 1)
        {
            var selector = new ComboBox
            {
                Header = S.Get("common/mode"),
                ItemsSource = modes.Select(m => m.Label).ToList(),
                SelectedIndex = 0,
            };
            selector.SelectionChanged += (_, _) =>
            {
                if (selector.SelectedIndex >= 0)
                {
                    activeMode = modes[selector.SelectedIndex];
                    RenderFields(activeMode);
                }
            };
            panel.Children.Add(selector);
        }

        panel.Children.Add(fieldsPanel);
        RenderFields(activeMode);

        return (panel, () =>
        {
            var fields = new Dictionary<string, string>();
            foreach (var (key, read) in fieldInputs)
            {
                var val = read();
                if (!string.IsNullOrEmpty(val))
                    fields[key] = val;
            }
            return (activeMode.Mode, fields);
        });
    }
}
