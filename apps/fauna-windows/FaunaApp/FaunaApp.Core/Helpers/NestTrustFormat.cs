using FaunaApp.Core.Services;
using uniffi.fauna_client_pair;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Shell-side label mapping for the Nests-page trust facet
/// (docs/goal/ui/nests.md § Where logic lives — "scope + status + history
/// labels are shell-side"; priority #2, one scope shape, per-app localized
/// labels). <see cref="ScopeLabel"/>/<see cref="StatusLabel"/>/
/// <see cref="MintOptionLabel"/> are thin wrappers over the shared
/// <c>fauna_client_pair::{scope_label,status_label,mint_option_label}</c> FFI
/// doors + <see cref="Strings.Resolve"/> (mirrors how
/// <c>MailListMembersViewModel</c> consumes
/// <c>FaunaClientMailSettingsMethods.MemberStatusLabel</c>) — the match arms
/// themselves no longer live per-app (tui/linux/android/web/apple
/// already consume the same shared functions). No nest round-trip;
/// <see cref="HistoryLine"/> delegates absolute timestamp formatting to the
/// shared <c>FaunaFfiMethods.FormatUnixLocal</c> (value-formatting.md §
/// Absolute local timestamp display) rather than a local reimplementation.
/// </summary>
internal static class NestTrustFormat
{
    /// <summary>
    /// Map one declared scope tuple to its localized content-kind label via
    /// the shared <c>fauna_client_pair::scope_label</c> door.
    /// </summary>
    internal static string ScopeLabel(TrustScope scope) =>
        Strings.Resolve(FaunaClientPairMethods.ScopeLabel(scope));

    /// <summary>"Mail, Calendar" — the grant scope's rendered line.</summary>
    internal static string ScopeLine(IEnumerable<TrustScope> scope) =>
        string.Join(", ", scope.Select(ScopeLabel));

    /// <summary>
    /// A grant's liveness status (<c>nest-trust-grant-status</c>) via the
    /// shared <c>fauna_client_pair::status_label</c> door.
    /// </summary>
    internal static string StatusLabel(TrustLiveness liveness) =>
        Strings.Resolve(FaunaClientPairMethods.StatusLabel(liveness));

    /// <summary>
    /// One History-lens row's self-describing line ("Trusted to read ‹scope›
    /// · ‹when›" etc.). The <c>history_*</c> i18n strings carry
    /// <c>{scope}</c>/<c>{when}</c> named placeholders substituted in textual
    /// order via <see cref="Strings.Format"/>.
    /// </summary>
    internal static string HistoryLine(TrustHistoryRow h)
    {
        var scope = ScopeLine(h.@scope);
        var when = FaunaFfiMethods.FormatUnixLocal(h.@at);
        return h.@kind switch
        {
            TrustEventKind.Mint => Strings.Format("nests/history_minted", scope, when),
            TrustEventKind.Renew => Strings.Format("nests/history_renewed", scope, when),
            TrustEventKind.Revoke => Strings.Format("nests/history_revoked", scope, when),
            _ => Strings.Format("nests/history_minted", scope, when),
        };
    }

    /// <summary>
    /// Map a shared mint-picker option (<c>LinkedNestRow.mint_options</c>) to its
    /// localized use-case label (<c>nests/mint_option_*</c>; the paywalled option
    /// carries its tier via the <c>{tier}</c> named placeholder) via the shared
    /// <c>fauna_client_pair::mint_option_label</c> door (nests.md § Mint,
    /// scope-first design ratified 2026-07-13).
    /// </summary>
    internal static string MintOptionLabel(TrustMintOption option) =>
        Strings.Resolve(FaunaClientPairMethods.MintOptionLabel(option));
}
