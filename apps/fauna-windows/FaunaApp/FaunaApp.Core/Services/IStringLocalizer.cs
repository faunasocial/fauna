using System.Net.Http;
using System.Text.RegularExpressions;

namespace FaunaApp.Core.Services;

/// <summary>
/// Provides localized strings from the i18n resource system.
/// Implemented in the app layer using ResourceLoader; consumed by ViewModels.
/// </summary>
public interface IStringLocalizer
{
    /// <summary>
    /// Returns the localized string for the given resource key (slash notation, e.g. "common/cancel").
    /// Returns the key itself if not found.
    /// </summary>
    string Get(string key);
}

/// <summary>
/// Static accessor so ViewModels can use localized strings without constructor injection.
/// Must be initialized at app startup before any ViewModel is created.
/// </summary>
public static class Strings
{
    private static IStringLocalizer? _localizer;

    /// <summary>
    /// Initialize with the platform-specific localizer. Call once at app startup.
    /// </summary>
    public static void Initialize(IStringLocalizer localizer)
    {
        _localizer = localizer;
    }

    /// <summary>
    /// Get a localized string by key. Returns the key if no localizer is set or key is not found.
    /// </summary>
    public static string Get(string key)
    {
        return _localizer?.Get(key) ?? key;
    }

    /// <summary>
    /// Get a localized string with positional format parameters, e.g.
    /// <c>Strings.Format("errors/http_error", ex.Message)</c>. The generated resw
    /// uses named <c>{placeholder}</c> tokens (the shared i18n convention); each
    /// positional arg is mapped onto the i-th <b>distinct</b> placeholder in
    /// textual order — reproducing the <c>{0}/{1}</c> indexing the windows
    /// generator used to emit, but consistent with <see cref="Resolve"/>'s
    /// by-name substitution. Unfilled placeholders (more tokens than args) are
    /// left intact.
    /// </summary>
    public static string Format(string key, params object[] args)
    {
        var template = Get(key);
        var order = new Dictionary<string, int>();
        return Regex.Replace(template, @"\{(\w+)\}", m =>
        {
            var name = m.Groups[1].Value;
            if (!order.TryGetValue(name, out var idx))
            {
                idx = order.Count;
                order[name] = idx;
            }
            return idx < args.Length ? (args[idx]?.ToString() ?? string.Empty) : m.Value;
        });
    }

    /// <summary>
    /// Format an exception as an i18n error: "i18n prefix: exception detail".
    /// Uses errors/http_error ("HTTP error: {0}") as the default wrapper.
    /// <para>A shared-Rust refusal (<c>FfiException.General</c>) is the exception to the
    /// wrapper: its <c>msg</c> is already the user-facing sentence (the nest's reason,
    /// resolved by shared code), so it is shown as that sentence — not as the exception's
    /// aggregated <c>@msg=…</c> text behind "HTTP error:", which is what every VM that
    /// reached <c>ShowError</c> painted until this arm (each refusal read as a transport
    /// failure carrying a field-name artifact, and no other app shows it that way).</para>
    /// </summary>
    public static string Error(Exception ex)
    {
        return ex switch
        {
            uniffi.fauna_ffi.FfiException.General general => general.@msg,
            // The guardian gate's refusal is also the nest's own sentence — typed
            // only so the ward's ask can be offered beside it.
            uniffi.fauna_ffi.FfiException.GuardianApprovalRequired guardian => guardian.@msg,
            HttpRequestException => Format("errors/http_error", ex.Message),
            TaskCanceledException => Get("errors/nest_timeout"),
            UriFormatException => Get("errors/nest_unreachable"),
            _ => Format("errors/http_error", ex.Message),
        };
    }

    /// <summary>
    /// Resolve a shared-Rust <see cref="uniffi.fauna_core.LocalizedText"/>
    /// (i18n key + named args) into a display string. The key is dotted in Rust;
    /// the generated resw uses slash notation, so dots → slashes for lookup.
    /// Args are substituted purely <b>by name</b> — each <c>{name}</c> in the
    /// template is replaced with <c>args[name]</c> — matching how every other
    /// app localizes <c>LocalizedText</c> (web <c>L()</c>, Apple <c>Bundle</c>,
    /// Android <c>getString</c>, Linux <c>fauna_i18n</c>). This is order-robust:
    /// <c>LocalizedText.args</c> is a Rust <c>HashMap</c> with no iteration-order
    /// guarantee, so the previous positional <c>string.Format</c> pass misordered
    /// multi-arg keys (e.g. <c>time.uptime_dhm</c> → "0d 0h 1m"); by-name
    /// substitution sidesteps that entirely. A missing key falls back to the raw
    /// key so untranslated strings surface obviously.
    /// <para><c>internal</c> because <c>LocalizedText</c> is UniFFI-<c>internal</c>;
    /// the test + WinUI assemblies see it via <c>[InternalsVisibleTo]</c>. This is
    /// the single canonical resolver (the former OnboardingViewModel copy was
    /// folded into it).</para>
    /// </summary>
    internal static string Resolve(uniffi.fauna_core.LocalizedText msg)
    {
        if (string.IsNullOrEmpty(msg.@key)) return string.Empty;
        var slashed = msg.@key.Replace('.', '/');
        var resolved = Get(slashed);
        var template = resolved == slashed ? msg.@key : resolved;
        if (msg.@args is null || msg.@args.Count == 0) return template;

        var sb = new System.Text.StringBuilder(template);
        foreach (var (name, value) in msg.@args) sb.Replace("{" + name + "}", value);
        return sb.ToString();
    }

    /// <summary>
    /// <see cref="Resolve"/>, but each ARGUMENT is first put through the same
    /// key lookup too — for templates whose substitution is itself a
    /// translatable term rather than raw data (dynamic-features.md's
    /// exhaustion sentence and quota-cell label both carry a nested key, e.g.
    /// <c>window = "features.window_day"</c>; a plain <see cref="Resolve"/>
    /// would leave the raw key inside the rendered sentence). The C# twin of
    /// shared Rust <c>LocalizedText::resolve_nested</c> — mirrors Android
    /// <c>resolveLocalizedNested</c> / web's TS twin / linux's
    /// <c>resolve_nested</c> call. An argument that is not itself a resource
    /// key simply misses the lookup (<see cref="Get"/> falls back to its
    /// input) and substitutes verbatim.
    /// </summary>
    internal static string ResolveNested(uniffi.fauna_core.LocalizedText msg)
    {
        if (string.IsNullOrEmpty(msg.@key)) return string.Empty;
        var slashed = msg.@key.Replace('.', '/');
        var resolved = Get(slashed);
        var template = resolved == slashed ? msg.@key : resolved;
        if (msg.@args is null || msg.@args.Count == 0) return template;

        var sb = new System.Text.StringBuilder(template);
        foreach (var (name, value) in msg.@args)
        {
            var argSlashed = value.Replace('.', '/');
            var argResolved = Get(argSlashed);
            var argValue = argResolved == argSlashed ? value : argResolved;
            sb.Replace("{" + name + "}", argValue);
        }
        return sb.ToString();
    }

    /// <summary>
    /// A gated-feature quota cell's headroom sentence, fully composed — the C#
    /// twin of shared Rust <c>fauna_client_features::row::cell_value_text</c>
    /// (mirrors Android <c>cellValueText</c> / web's TS twin). When
    /// <c>cell.magnitudes</c> is set (<c>volume</c> cells), its two localized
    /// magnitudes are resolved FIRST and substituted into <c>value</c>'s
    /// <c>{remaining}</c>/<c>{limit}</c> holes — a <c>LocalizedText</c>
    /// argument is a flat string, so a magnitude that is itself localized
    /// ("931.3 GB") has to be composed before the outer template resolves
    /// (the <c>BackupLastUploadDisplay</c> two-level shape). Without
    /// magnitudes, <c>{remaining}</c>/<c>{limit}</c> are already finished
    /// numbers, so plain <see cref="Resolve"/> is enough.
    /// </summary>
    internal static string CellValueText(uniffi.fauna_ffi.FfiLimitCell cell)
    {
        if (cell.@magnitudes is not { } magnitudes) return Resolve(cell.@value);

        var composedArgs = new Dictionary<string, string>(cell.@value.@args)
        {
            ["remaining"] = Resolve(magnitudes.@remaining),
            ["limit"] = Resolve(magnitudes.@limit),
        };
        return Resolve(cell.@value with { @args = composedArgs });
    }

    /// <summary>
    /// The `custody-holder-receipt-status` line — the C# twin of apple
    /// <c>ValueFormat.custodyReceiptStatusText</c> / android
    /// <c>Localized.kt::custodyReceiptStatusText</c> / linux
    /// <c>i18n::custody_receipt_status</c>. Substitutes the resolved local
    /// timestamp into <c>{when}</c> when the receipt carries one — epoch
    /// SECONDS ride the wire on purpose (<c>format_unix_local</c> needs the OS
    /// tz database), so formatting happens here, never on the shared side.
    /// </summary>
    internal static string CustodyReceiptStatusText(uniffi.fauna_client_capabilities.CustodyReceiptRowView receipt)
    {
        var composed = receipt.@statusLabel;
        if (receipt.@attestedAtSecs is { } secs)
        {
            var args = new Dictionary<string, string>(composed.@args)
            {
                ["when"] = uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocal(secs),
            };
            composed = composed with { @args = args };
        }
        return Resolve(composed);
    }

    /// <summary>
    /// The `custody-holder-held-bytes` line — held bytes against the budget in
    /// force, with the degraded badge appended (never substituted for the
    /// line: `degraded` is orthogonal to freshness, a fresh receipt can
    /// honestly report dropped payload). C# twin of apple
    /// <c>ValueFormat.custodyHeldBytesText</c> / android
    /// <c>Localized.kt::custodyHeldBytesText</c> / linux
    /// <c>i18n::custody_held_bytes</c>.
    /// </summary>
    internal static string CustodyHeldBytesText(uniffi.fauna_client_capabilities.CustodyReceiptRowView receipt)
    {
        var args = new Dictionary<string, string>(receipt.@heldBytesLabel.@args)
        {
            ["held"] = Resolve(receipt.@held),
            ["cap"] = Resolve(receipt.@cap),
        };
        var line = Resolve(receipt.@heldBytesLabel with { @args = args });
        if (!receipt.@degraded) return line;
        var badge = Resolve(new uniffi.fauna_core.LocalizedText(
            uniffi.fauna_ffi.FaunaFfiMethods.CustodyDegradedBadgeKey(), new Dictionary<string, string>()));
        return $"{line} — {badge}";
    }
}
