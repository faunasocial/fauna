using System;
using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The settings region section (<c>settings-region-*</c>, region-blocking.md § The
/// blocked render and the transparency surface) as display strings — a paint of the
/// shared <c>FfiRegionPlane.view()</c>, never an app-side fold: the declared region
/// and its source (the change path named, no in-app override), each policy on the
/// chain (authority, sequence, issued-at, and the inert/malformed notice), when the
/// relay was last asked, and the staleness warning. linux's
/// <c>region::paint_settings</c>, apple's <c>RegionViews.swift</c>; the page paints
/// these lines one element per id. Pure over its inputs, so it is unit-tested
/// without a WinUI tree.
/// </summary>
public sealed record RegionSettingsModel(
    string DeclaredText,
    string? SourceText,
    string? NoPolicyText,
    IReadOnlyList<RegionPolicyLines> Policies,
    string? LastCheckedText,
    string? StaleWarningText)
{
    /// <summary>Project the plane's view. <paramref name="formatTime"/> turns unix
    /// seconds into the local display time — <c>FaunaFfiMethods.FormatUnixLocal</c> in
    /// the app, the one door every app formats an absolute local time through.</summary>
    internal static RegionSettingsModel From(FfiRegionView view, Func<long, string> formatTime)
    {
        if (view.@declared is not { } declared)
            return new(Strings.Get("region/none_declared"), null, null,
                Array.Empty<RegionPolicyLines>(), null, null);

        var policies = view.@policies.Select(p => new RegionPolicyLines(
            Fill("region.policy_authority", ("region", p.@region), ("authority", p.@authorityName)),
            Fill("region.policy_version",
                ("sequence", p.@sequence.ToString(System.Globalization.CultureInfo.InvariantCulture)),
                ("issued", formatTime((long)p.@issuedAt))),
            p.@state switch
            {
                "inert" => Fill("region.inert_notice",
                    ("version", p.@inertVersion?.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? "")),
                "malformed" => Strings.Get("region/malformed_notice"),
                _ => null,
            })).ToList();

        return new(
            Fill("region.declared", ("region", declared.@code)),
            // The source's i18n key comes from shared Rust (`region.source_*`), so a
            // source this build never names still paints its own label.
            Strings.Get(declared.@sourceLabelKey.Replace('.', '/')),
            policies.Count == 0 ? Strings.Get("region/no_policy") : null,
            policies,
            view.@lastCheckedAt is { } checkedAt
                ? Fill("region.last_checked", ("time", formatTime((long)checkedAt)))
                : null,
            view.@stale ? Strings.Get("region/stale_warning") : null);
    }

    private static string Fill(string key, params (string Name, string Value)[] args) =>
        Strings.Resolve(new uniffi.fauna_core.LocalizedText(
            key, args.ToDictionary(a => a.Name, a => a.Value)));
}

/// <summary>One policy on the declared chain (<c>settings-region-policy-item</c>): its
/// authority line, its version line, and — for a document this app cannot apply — the
/// <c>settings-region-inert-notice</c> saying so.</summary>
public sealed record RegionPolicyLines(string AuthorityText, string VersionText, string? NoticeText);
