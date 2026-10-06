using System.Formats.Cbor;
using Xunit;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

// Upload-sidecar wire-up (encrypted-mode blob upload).
//
// Goal: docs/goal/architecture/encryption-at-rest.md § Per-content-kind
// conformance → Media row. The client seals + sidecars every blob upload and
// POSTs `multipart/form-data` (sidecar + bytes parts) to /api/v1/blob.
//
// Two surfaces are tested deterministically (no live nest):
//  1. The windows-side multipart wire shape (DirectNestClient.BuildBlobMultipart).
//  2. The public UploadAudience → UniFFI FfiUploadAudience mapping.
//  3. Cross-language conformance: the real shared-Rust FFI packer
//     (FaunaFfiMethods.ProcessAndSealUpload) — the dll loads in the dotnet test
//     host (memory: reference_windows_dotnet_test_loads_native_ffi), so this is a
//     genuine build→wire check, not a C#-internal round-trip.
public class MediaUploadTests
{
    [Fact]
    public void BuildBlobMultipart_HasSidecarAndBytesParts()
    {
        var sidecarCbor = new byte[] { 1, 2, 3, 4 };
        var sealedBytes = new byte[] { 9, 8, 7, 6, 5 };

        using var form = DirectNestClient.BuildBlobMultipart(sidecarCbor, sealedBytes);

        var parts = form.ToList();
        Assert.Equal(2, parts.Count);

        var sidecar = parts.Single(p => PartName(p) == "sidecar");
        Assert.Equal("application/cbor", sidecar.Headers.ContentType!.MediaType);
        Assert.Equal(sidecarCbor, sidecar.ReadAsByteArrayAsync().Result);

        var bytes = parts.Single(p => PartName(p) == "bytes");
        Assert.Equal("application/octet-stream", bytes.Headers.ContentType!.MediaType);
        Assert.Equal(sealedBytes, bytes.ReadAsByteArrayAsync().Result);
    }

    [Fact]
    public void MapAudience_PublicPost_MapsToFfiPublicPost()
    {
        var ffi = DirectNestClient.MapAudience(new UploadAudience.PublicPost());
        Assert.IsType<FfiUploadAudience.PublicPost>(ffi);
    }

    [Fact]
    public void MapAudience_Library_CarriesBackupKey()
    {
        var key = new byte[32];
        for (var i = 0; i < key.Length; i++) key[i] = (byte)i;

        var ffi = DirectNestClient.MapAudience(new UploadAudience.Library(key));

        var lib = Assert.IsType<FfiUploadAudience.Library>(ffi);
        Assert.Equal(key, lib.@backupKey);
    }

    [Fact]
    public void ProcessAndSealUpload_PublicPost_PassesBytesThroughWithDecodableSidecar()
    {
        var raw = System.Text.Encoding.UTF8.GetBytes("public post attachment bytes");

        var payload = FaunaFfiMethods.ProcessAndSealUpload(raw, new FfiUploadAudience.PublicPost());

        // PublicPost: signed plaintext, byte-identical (no seal); no thumbnail
        // from the stub process_media.
        Assert.Equal(raw, payload.@primary.@bytes);
        Assert.Null(payload.@thumbnail);

        var (cls, mime, hasC2pa, thumbNull) = DecodeSidecar(payload.@primary.@sidecarCbor);
        Assert.Equal("PublicPost", cls);
        Assert.Equal("application/octet-stream", mime);
        Assert.False(hasC2pa);
        Assert.True(thumbNull);
    }

    [Fact]
    public void ProcessAndSealUpload_Library_SealsUnderBackupKey()
    {
        var raw = System.Text.Encoding.UTF8.GetBytes("owner-only library media bytes");
        var key = new byte[32];
        Array.Fill(key, (byte)7);

        var payload = FaunaFfiMethods.ProcessAndSealUpload(raw, new FfiUploadAudience.Library(key));

        // Sealed: an AEAD envelope, never byte-identical, carrying at least the
        // 12-byte nonce + 16-byte tag floor over the plaintext.
        Assert.NotEqual(raw, payload.@primary.@bytes);
        Assert.True(payload.@primary.@bytes.Length >= raw.Length + 28);

        var (cls, mime, _, _) = DecodeSidecar(payload.@primary.@sidecarCbor);
        Assert.Equal("Library", cls);
        Assert.Equal("application/octet-stream", mime);
    }

    private static string PartName(System.Net.Http.HttpContent part)
        => part.Headers.ContentDisposition!.Name!.Trim('"');

    // Decode the canonical DAG-CBOR UploadSidecar map (libs/fauna-media/src/sidecar.rs):
    // { class: text, mime: text, has_c2pa: bool, thumbnail_hash: bytes|null }.
    private static (string Class, string Mime, bool HasC2pa, bool ThumbnailNull) DecodeSidecar(byte[] cbor)
    {
        var reader = new CborReader(cbor);
        var count = reader.ReadStartMap();
        string? cls = null, mime = null;
        bool? c2pa = null;
        var thumbNull = false;
        for (var i = 0; i < count; i++)
        {
            var key = reader.ReadTextString();
            switch (key)
            {
                case "class": cls = reader.ReadTextString(); break;
                case "mime": mime = reader.ReadTextString(); break;
                case "has_c2pa": c2pa = reader.ReadBoolean(); break;
                case "thumbnail_hash":
                    if (reader.PeekState() == CborReaderState.Null) { reader.ReadNull(); thumbNull = true; }
                    else { reader.ReadByteString(); }
                    break;
                default: reader.SkipValue(); break;
            }
        }
        reader.ReadEndMap();
        return (cls!, mime!, c2pa!.Value, thumbNull);
    }
}
