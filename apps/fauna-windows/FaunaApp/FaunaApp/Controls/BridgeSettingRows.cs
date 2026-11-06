using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.System;
using FaunaApp.Core.Models;

namespace FaunaApp.Controls;

/// <summary>
/// The one settings renderer (bridges.md § Bridge settings): one
/// row per <see cref="BridgeSetting"/>, built imperatively since the set and shape vary per
/// bridge and per nest version (a newer nest can add a <c>setting_type</c> this build
/// predates). Mirrors linux's <c>build_settings_group</c> / tui's <c>setting_elements</c> /
/// web's <c>BridgeCard.svelte</c> / android's <c>SettingRow</c> arm-for-arm (priority #1).
///
/// <para>Two hosts paint through it: a bridge's <see cref="BridgeCard"/>, and a hosted
/// plugin's settings card on the Bridges page, whose settings are the bridge cards' own
/// rows (third-party.md § The principal model → <i>Hosted principals</i>). The row decides
/// nothing: it hands the committed value to its host, which writes it
/// (<c>fauna.bridges.set_settings</c> or <c>fauna.plugins.set_settings</c>) and re-reads.</para>
/// </summary>
internal static class BridgeSettingRows
{
    /// <summary><c>bridge-setting[&lt;key&gt;]</c>: KEYED by the setting itself, on the
    /// row's control whatever its type — the literal id string carries the key, the
    /// <c>settings-nav-row[&lt;key&gt;]</c> idiom (bridges.md § User actions; tui's
    /// <c>setting_id</c>).</summary>
    private static string SettingId(BridgeSetting setting) =>
        $"{FaunaApp.UiIds.Ids.BridgeSetting}[{setting.Key}]";

    /// <summary>The row for <paramref name="setting"/>; <paramref name="commit"/> receives
    /// each value the user commits. <paramref name="editor"/>, when given, is handed the
    /// row's editing control (the toggle or the text box) — where a host declares the wire
    /// kind that control actuates, since the kind is the host's (a bridge's and a plugin's
    /// settings write through different kinds).</summary>
    public static FrameworkElement Build(
        BridgeSetting setting, Action<BridgeSettingValue> commit, Action<Control>? editor = null)
    {
        // Which control a setting paints is the shared classifier's call
        // (fauna_client_bridges::setting_control) — no spelling is matched here.
        switch (uniffi.fauna_ffi.FaunaFfiMethods.SettingControl(setting.SettingType))
        {
            case uniffi.fauna_ffi.FfiSettingControl.Toggle:
                return BuildToggleRow(setting, commit, editor);
            case uniffi.fauna_ffi.FfiSettingControl.Number:
                return BuildTextRow(setting, commit, editor, isNumber: true);
            case uniffi.fauna_ffi.FfiSettingControl.Text:
                return BuildTextRow(setting, commit, editor, isNumber: false);
            default:
                // Unknown (a setting_type this build predates — a newer nest)
                // and Select (windows has no picker row yet): a read-only
                // row, never a dropped setting.
                // Mirrors tui's Element::chrome fallback / web's/android's
                // read-only "{label}: {value}" line.
                var value = setting.BoolValue?.ToString()
                    ?? setting.NumberValue?.ToString()
                    ?? setting.TextValue
                    ?? string.Empty;
                var line = new TextBlock { Text = $"{setting.Label}: {value}", Opacity = 0.6 };
                AutomationProperties.SetAutomationId(line, SettingId(setting));
                return line;
        }
    }

    private static ToggleSwitch BuildToggleRow(
        BridgeSetting setting, Action<BridgeSettingValue> commit, Action<Control>? editor)
    {
        var toggle = new ToggleSwitch
        {
            Header = setting.Label,
            OnContent = string.Empty,
            OffContent = string.Empty,
            IsOn = setting.BoolValue ?? false,
        };
        AutomationProperties.SetAutomationId(toggle, SettingId(setting));
        toggle.Toggled += (_, _) => commit(BridgeSettingValue.Bool(setting.Key, toggle.IsOn));
        editor?.Invoke(toggle);
        return toggle;
    }

    /// <summary>A text/number setting row. Commits on BOTH blur and Enter (the
    /// mail-spam-threshold-override-input / FoldersPage capInput idiom) — Enter
    /// alone doesn't move focus off a single-line TextBox, so the e2e's
    /// click-to-commit gesture needs the LostFocus arm too. An unparseable
    /// number commits nothing (never a client error — nest-side clamping is the
    /// guard, this is a UX nicety per the row's own scope note).</summary>
    private static Grid BuildTextRow(
        BridgeSetting setting, Action<BridgeSettingValue> commit, Action<Control>? editor, bool isNumber)
    {
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var label = new TextBlock
        {
            Text = setting.Label,
            VerticalAlignment = VerticalAlignment.Center,
            Opacity = 0.7,
        };
        Grid.SetColumn(label, 0);

        var textBox = new TextBox
        {
            Width = 120,
            Text = isNumber ? setting.NumberValue?.ToString() ?? string.Empty : setting.TextValue ?? string.Empty,
        };
        AutomationProperties.SetAutomationId(textBox, SettingId(setting));
        Grid.SetColumn(textBox, 1);

        void Commit()
        {
            if (isNumber)
            {
                if (uniffi.fauna_ffi.FaunaFfiMethods.ParseCountI64(textBox.Text) is long n)
                {
                    commit(BridgeSettingValue.Number(setting.Key, n));
                }
            }
            else
            {
                commit(BridgeSettingValue.Text(setting.Key, textBox.Text));
            }
        }

        textBox.LostFocus += (_, _) => Commit();
        textBox.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter) Commit();
        };

        editor?.Invoke(textBox);
        row.Children.Add(label);
        row.Children.Add(textBox);
        return row;
    }
}
