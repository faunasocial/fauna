using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the conversation legal-takedown tombstone
/// (moderation.md § Categories & enforcement item 1). When the nest withholds a
/// message's sealed envelope under a legal obligation, shared Rust
/// (<c>poll_inbound_conv</c>) short-circuits it into a tombstone
/// <c>MessageSnapshot.legalTakedownRef</c> (empty body, no decrypt), and every
/// app paints the SAME shared tombstone in place of the bubble — never a
/// blank/failed-decrypt bubble. This locks the shared FFI face
/// (<c>fauna_core::obligation::legal_takedown_tombstone</c> via
/// <c>FaunaFfiMethods.LegalTakedownTombstone</c>) that the windows
/// <c>DmMessageBubble</c> collapse routes through — the same face the post
/// quoted-post tombstone uses — so no client hand-rolls the string (priority #2/#4).
/// Calls the REAL export (native dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) and resolves through the
/// production <see cref="Strings"/> pipeline with a fake localizer mirroring the
/// generated windows resw. The bubble collapse render itself (like the sibling
/// <c>dm-message-deleted</c> collapse) is the tier_3 e2e's job.
/// </summary>
[Collection("StringsGlobal")]
public class ConversationsLegalTakedownTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // Mirrors the generated windows resw entry for the shared tombstone key
        // (i18n/strings/en.yaml `moderation.legal_takedown.tombstone`); the
        // {reference} placeholder is substituted by Strings.Resolve.
        private static readonly Dictionary<string, string> Map = new()
        {
            ["moderation/legal_takedown/tombstone"] = "Removed under legal obligation ({reference})",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ConversationsLegalTakedownTests() => Strings.Initialize(new FakeLocalizer());

    [Fact]
    public void LegalTakedownTombstone_ResolvesSharedTombstoneWithReference()
    {
        const string reference = "EU-DSA-2024/12345";

        var text = Strings.Resolve(FaunaFfiMethods.LegalTakedownTombstone(reference));

        // The shared face carries the reference through its {reference} arg — the
        // exact string the DmMessageBubble paints in place of the withheld body.
        Assert.Equal("Removed under legal obligation (EU-DSA-2024/12345)", text);
    }
}
