using System;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Locks the windows add-contact knock composition. <see cref="NestRpcClient.BuildKnockPayload"/>
/// must produce the canonical signed (ContactRequest, Post) tuple the nest's
/// <c>verify_inbox_payload</c> decodes (<c>canonical_decode::&lt;(EmbedAsBytes, EmbedAsBytes)&gt;</c>)
/// — a dag-cbor definite 2-element array. Runs the REAL native FFI <c>build_signed_email</c>
/// (<c>reference_windows_dotnet_test_loads_native_ffi</c>), so a regression in the shared composer
/// OR the windows glue (wrong secret / recipient-hex / nodeUrl wiring) is caught here, not only in
/// the tier_3 e2e. Mirrors linux <c>client.send_knock</c> / apple <c>sendToInbox</c>
/// ("Knock" / "Contact request"); see <c>docs/goal/architecture/federation.md</c> § Federation
/// residue surface.
/// </summary>
public class InboxKnockTests
{
    [Fact]
    public void BuildKnockPayload_ProducesSignedCrPostTuple()
    {
        var crypto = new CryptoService();
        var secret = new byte[32];
        Array.Fill(secret, (byte)0x2A);
        crypto.LoadFromSecret(Convert.ToHexString(secret));
        var recipientHex = new string('a', 64); // 32 bytes of 0xAA

        var payload = NestRpcClient.BuildKnockPayload(crypto, recipientHex, "https://nest.test");

        Assert.NotEmpty(payload);
        // dag-cbor encodes the (ContactRequest, Post) tuple as a definite-length
        // 2-element array → CBOR header byte 0x82 (major type 4 "array", count 2),
        // exactly the shape the nest's verify_inbox_payload decodes.
        Assert.Equal(0x82, payload[0]);
    }
}
