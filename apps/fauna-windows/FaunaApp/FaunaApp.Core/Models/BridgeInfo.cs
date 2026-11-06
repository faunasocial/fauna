using System.Collections.Generic;

namespace FaunaApp.Core.Models;

/// <summary>One row of <c>fauna.bridges.list</c>.</summary>
/// <param name="Error">The nest's own explanation when <c>provider.status()</c>
/// errored — the degraded shape's <c>error</c> slot (bridges.md § Errors &amp;
/// edge cases). This member did not exist, so windows dropped the sentence at
/// decode and left the user a dead Link control with no reason.</param>
/// <param name="SupportsFollowRequests">The provider's declared capability
/// (<c>BridgeStatus.supports_follow_requests</c>, bridges.md § Follow requests):
/// <c>false</c> renders no follow-requests section.</param>
/// <param name="FollowRequests">The requests waiting on the account, read by
/// <c>BridgesViewModel</c> after the list for a linked bridge that declares the
/// capability; <c>null</c> when nothing was read (the card paints none).</param>
/// <param name="IdentityLabel">What the provider calls the identity
/// (<c>BridgeStatus.identity.label</c>, e.g. "Handle"); <see cref="Identity"/> is its
/// <c>display</c>.</param>
/// <param name="SupportsFollows">The provider exposes a follows list
/// (<c>BridgeStatus.supports_follows</c>, bridges.md § Follows).</param>
/// <param name="Follows">The account's follows on this bridge, read with the list for a
/// linked bridge that supports them; <c>null</c> when nothing was read.</param>
public record BridgeInfo(
    string Id,
    string DisplayName,
    bool Available,
    bool Linked,
    string? Identity,
    string? Mode,
    IReadOnlyList<BridgeLinkMode> LinkModes,
    IReadOnlyList<BridgeSetting> Settings,
    string? Error = null,
    bool SupportsFollowRequests = false,
    IReadOnlyList<BridgeFollowRequest>? FollowRequests = null,
    string? IdentityLabel = null,
    bool SupportsFollows = false,
    IReadOnlyList<BridgeFollow>? Follows = null)
{
    /// <summary>The <c>bridge-identity</c> line, <c>&lt;label&gt;: &lt;display&gt;</c>
    /// (bridges.md § User actions; tui's and apple's cards compose the same line);
    /// <c>null</c> when the nest reports no identity.</summary>
    public string? IdentityLine => Identity is null
        ? null
        : string.IsNullOrEmpty(IdentityLabel) ? Identity : $"{IdentityLabel}: {Identity}";
}

/// <summary>One follow request waiting on the account (bridges.md § Follow requests):
/// <see cref="Id"/> is what Approve / Refuse forward, <see cref="Display"/> the one line
/// the row paints — the shared row model's <c>FollowRequestRow::display</c>, never
/// composed here.</summary>
public record BridgeFollowRequest(string Id, string Display);

/// <summary>One provider-declared setting from <c>fauna.bridges.list</c>
/// (bridges.md § State &amp; data shape → <c>BridgeSetting</c>). The wire value is
/// CBOR; the three projections below cover every type a client reads today —
/// the 5 Nostr <c>bool</c> flags + its <c>relay_list</c> JSON-array-in-a-string,
/// plus the content-bridge search-policy <c>number</c> cap
/// (<c>limit_posts_in_search</c>). A type this shape
/// can't carry projects to every field null rather than throwing — an unknown
/// setting is inert, never fatal (the card's own fallback then renders it
/// read-only, matching every sibling app).</summary>
public record BridgeSetting(
    string Key,
    string Label,
    string SettingType,
    bool? BoolValue,
    string? TextValue,
    long? NumberValue = null)
{
    /// <summary>One wire setting row projected onto this shape — the one projection a
    /// bridge card's settings and a plugin card's settings share (both arrive as the
    /// UniFFI <c>FfiBridgeSetting</c>; <c>third-party.md</c> § The principal model →
    /// <i>Hosted principals</i>: a plugin's settings are the bridge cards' own rows).</summary>
    internal static BridgeSetting FromFfi(uniffi.fauna_ffi.FfiBridgeSetting s) => new(
        s.key, s.label, s.settingType,
        s.value is uniffi.fauna_ffi.FfiCborValue.Bool bo ? bo.@v : null,
        s.value is uniffi.fauna_ffi.FfiCborValue.Text tx ? tx.@v : null,
        s.value is uniffi.fauna_ffi.FfiCborValue.Integer iv ? iv.@v : null);
}

/// <summary>One setting value written back over <c>fauna.bridges.set_settings</c>.
/// Exactly one of <see cref="BoolValue"/> / <see cref="TextValue"/> /
/// <see cref="NumberValue"/> is non-null. The nest's <c>update_settings</c>
/// deserializes a PARTIAL map (every field of the provider's wire type is
/// optional), so a caller sends only the keys it is changing — never a full
/// read-modify-write of the unrelated flags.</summary>
public record BridgeSettingValue(string Key, bool? BoolValue, string? TextValue, long? NumberValue = null)
{
    public static BridgeSettingValue Bool(string key, bool value) => new(key, value, null);

    public static BridgeSettingValue Text(string key, string value) => new(key, null, value);

    public static BridgeSettingValue Number(string key, long value) => new(key, null, null, value);

    /// <summary>The value as the CBOR it crosses the wire as — shared by a bridge's
    /// <c>fauna.bridges.set_settings</c> map and a plugin's
    /// <c>fauna.plugins.set_settings</c> entry.</summary>
    internal uniffi.fauna_ffi.FfiCborValue ToCbor() =>
        BoolValue is bool b ? new uniffi.fauna_ffi.FfiCborValue.Bool(b)
        : NumberValue is long n ? new uniffi.fauna_ffi.FfiCborValue.Integer(n)
        : new uniffi.fauna_ffi.FfiCborValue.Text(TextValue ?? string.Empty);
}

/// <summary>One provider-declared link mode (bridges.md § State &amp; data shape →
/// <c>BridgeLinkMode</c>). The metadata-driven link form renders the active mode's
/// <see cref="Fields"/>; <see cref="Mode"/> is the wire value passed to
/// <c>fauna.bridges.link</c>.</summary>
public record BridgeLinkMode(
    string Mode,
    string Label,
    string? ClientAction,
    string? Platform,
    IReadOnlyList<BridgeLinkField> Fields);

/// <summary>One declarative field in a <see cref="BridgeLinkMode"/> (bridges.md →
/// <c>BridgeLinkField</c>). The form emits one input per field tagged
/// <c>bridge-link-field-{Key}</c>; a <c>secret</c> field renders as a password
/// input.</summary>
public record BridgeLinkField(
    string Key,
    string Label,
    string FieldType,
    string? Placeholder)
{
    public bool IsSecret => FieldType == "secret";
}

public record BridgeFollow(string Id, string? Petname);

public record BridgeFeedSubscription(long Id, string Bridge, string FeedUri, string Name);
