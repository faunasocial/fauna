using System.Collections.Generic;

namespace FaunaApp.Core.Models;

/// <summary>One row of <c>fauna.bridges.list</c>.</summary>
/// <param name="Error">The nest's own explanation when <c>provider.status()</c>
/// errored — the degraded shape's <c>error</c> slot (bridges.md § Errors &amp;
/// edge cases). This member did not exist, so windows dropped the sentence at
/// decode and left the user a dead Link control with no reason.</param>
public record BridgeInfo(
    string Id,
    string DisplayName,
    bool Available,
    bool Linked,
    string? Identity,
    string? Mode,
    IReadOnlyList<BridgeLinkMode> LinkModes,
    IReadOnlyList<BridgeSetting> Settings,
    string? Error = null);

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
    long? NumberValue = null);

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
