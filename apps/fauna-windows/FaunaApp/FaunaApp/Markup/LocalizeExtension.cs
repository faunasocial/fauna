using Microsoft.UI.Xaml.Markup;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Markup;

/// <summary>
/// XAML markup extension that resolves an i18n key to its localized string at
/// load time: <c>Text="{loc:Localize Key=onboarding/claim_code/title}"</c>.
/// Replaces per-view <c>ApplyLocalization()</c> code-behind. A missing key
/// falls back to the raw key (same as <see cref="S.Get"/>). One-shot — there
/// is no in-app locale switch, so the value need not update after load.
/// </summary>
// WinUI's MarkupExtensionReturnTypeAttribute is parameterless (unlike WPF's,
// which takes the return Type); the return type is inferred from ProvideValue.
public sealed class LocalizeExtension : MarkupExtension
{
    public string Key { get; set; } = "";

    protected override object ProvideValue() => S.Get(Key);
}
