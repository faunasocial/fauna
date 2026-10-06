using System.Text.Json;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;

namespace FaunaApp.Tests;

/// <summary>
/// Mock INestHttpClient for unit testing ViewModels that use the nest HTTP API.
/// Queues up responses and tracks method calls.
/// </summary>
public class MockNestHttpClient : INestHttpClient
{
    private readonly List<string> _calls = new();
    public IReadOnlyList<string> Calls => _calls;

    // ── Configurable responses ──
    public bool NextAvailable { get; set; } = true;
    // Identity/handle left the HTTP plane: account state → MockNestRpcClient
    // (fauna.account.get); identity generation + handle checks → OnboardingMachine.
    public string? NextAuthToken { get; set; }
    // Snapshot list/detail/create + backup status + the snapshot byte download
    // live on INestRpcClient (see MockNestRpcClient).
    public ServiceStatusInfo? NextServiceStatus { get; set; }
    public string? NextError { get; set; }

    public JsonElement? NextBridgesList { get; set; }

    private void RecordCall(string method)
    {
        _calls.Add(method);
        if (NextError is not null)
        {
            var err = NextError;
            NextError = null;
            throw new HttpRequestException(err);
        }
    }

    public Task<bool> IsAvailableAsync(CancellationToken ct = default)
    {
        _calls.Add("IsAvailable");
        return Task.FromResult(NextAvailable);
    }

    public Task<string> GetAuthTokenAsync(string nestUrl, CancellationToken ct = default)
    {
        RecordCall("GetAuthToken");
        return Task.FromResult(NextAuthToken ?? "token");
    }

    public Task ConfigureAsync(string? nestUrl, CancellationToken ct = default)
    {
        RecordCall("Configure");
        return Task.CompletedTask;
    }

    // GetNodeInfoAsync removed — version moved to fauna.admin.status
    // (MockNestRpcClient.AdminStatusAsync).

    // Direct/group send + conversation list/detail + community-group HTTP mocks
    // removed with the dead INestHttpClient methods (conversations run through
    // the shared-Rust ConversationsManager; nest twins deleted at the
    // conversations T8 cutover).

    public Task<ServiceStatusInfo> GetServiceStatusAsync(CancellationToken ct = default)
    {
        RecordCall("GetServiceStatus");
        return Task.FromResult(NextServiceStatus ?? new ServiceStatusInfo("1.0", 0, ConnectionState.Disconnected, new SyncStatusInfo(false, false, 0, 0, null), null));
    }

    // ── Sync Management ──

    // Folder list/create/delete moved to the WS-RPC plane — see
    // MockNestRpcClient (FoldersListAsync / FoldersCreateAsync).

    // The sync control-plane HTTP twins (files/status/changes/devices/register/
    // conflicts) were all lifted to fauna.sync.* — MockNestRpcClient mirrors them
    // (conflicts → ConflictsListAsync).

    // Bridge Management (`fauna.bridges.*`) moved to the WS-RPC plane —
    // see MockNestRpcClient.

    // The feed/posts HTTP twins moved to WS-RPC (see MockNestRpcClient); the
    // blob-upload sidecar (UploadBlobAsync) + bridge-feed subscription stay HTTP.

    // Events + event-social (list/create/rsvp/get/update/delete/attendees/
    // invite/co-hosts/discussion) migrated to MockNestRpcClient (WS-RPC façade);
    // the HTTP-twin mocks were removed with their INestHttpClient methods.

    // Account get / quota / delete moved to MockNestRpcClient (WS-RPC façade).

    public byte[] NextExportedData { get; set; } = Array.Empty<byte>();

    public Task<byte[]> ExportAccountDataAsync(CancellationToken ct = default)
    {
        RecordCall("ExportAccountData");
        return Task.FromResult(NextExportedData);
    }

    // Search migrated to the WS-RPC plane — see MockNestRpcClient.SearchQueryAsync
    // (the limit-growing synthesizer + NextSearchResults fixture moved there).

    // Spam preferences + moderation stats/actions/train moved off the HTTP plane —
    // spam rides the WS-RPC plane (MockNestRpcClient: SpamGet/SetPreferencesAsync);
    // moderation stats/actions/train are nest-blocked (no client seam yet).


    // MLS Welcomes: the legacy client-side-MLS HTTP welcome poll was removed —
    // welcome receive is now the shared-Rust push-driven loop on
    // ConversationsSession (conversations.md § MLS Welcome at-rest).

    // Event-collaboration twin mocks (get/update/delete/attendees/invite/
    // co-hosts/discussion) removed with their INestHttpClient methods — the
    // events surface lives on MockNestRpcClient now.

    // Bridge feed subscriptions (`fauna.bridges.feeds.*`) moved to the WS-RPC
    // plane — see MockNestRpcClient.

    // Snapshot prune / check / delete moved to INestRpcClient (MockNestRpcClient).

    // ── Admin ──
    // Stats + is-admin gate moved to the WS-RPC plane (MockNestRpcClient:
    // AdminStatsAsync / AmIAdminAsync).

    // ── Blobs ──

    /// <summary>Bytes the two blob GETs below hand back. Empty by default, which is what
    /// every caller predating a byte-level assertion expects.</summary>
    public byte[] NextBlobBytes { get; set; } = Array.Empty<byte>();
    public bool NextBlobHasC2pa { get; set; }

    public Task<byte[]> GetBlobAsync(string blobId, CancellationToken ct = default)
    {
        RecordCall("GetBlob");
        return Task.FromResult(NextBlobBytes);
    }

    public Task<(byte[] Data, bool HasC2pa)> GetBlobWithC2paAsync(string hash, CancellationToken ct = default)
    {
        RecordCall("GetBlobWithC2pa");
        return Task.FromResult((NextBlobBytes, NextBlobHasC2pa));
    }

    public List<(byte[] Data, UploadAudience Audience)> UploadBlobCalls { get; } = new();
    public string NextUploadedBlobHash { get; set; } = "00" + new string('0', 62); // valid 64-char hex

    public Task<string> UploadBlobAsync(byte[] data, UploadAudience audience, CancellationToken ct = default)
    {
        RecordCall("UploadBlob");
        UploadBlobCalls.Add(((byte[])data.Clone(), audience));
        return Task.FromResult(NextUploadedBlobHash);
    }

    public List<(byte[] Sidecar, byte[] Bytes, byte[]? ThumbSidecar, byte[]? ThumbBytes)> UploadPreparedBlobCalls { get; } = new();

    public Task<string> UploadPreparedBlobAsync(
        byte[] sidecarCbor,
        byte[] bytes,
        byte[]? thumbnailSidecarCbor,
        byte[]? thumbnailBytes,
        CancellationToken ct = default)
    {
        RecordCall("UploadPreparedBlob");
        UploadPreparedBlobCalls.Add((
            (byte[])sidecarCbor.Clone(),
            (byte[])bytes.Clone(),
            (byte[]?)thumbnailSidecarCbor?.Clone(),
            (byte[]?)thumbnailBytes?.Clone()));
        return Task.FromResult(NextUploadedBlobHash);
    }

    public List<(byte[] Sidecar, byte[] Sealed)> UploadSealedBlobCalls { get; } = new();

    public Task<string> UploadSealedBlobAsync(byte[] sidecarCbor, byte[] sealedBytes, CancellationToken ct = default)
    {
        RecordCall("UploadSealedBlob");
        UploadSealedBlobCalls.Add(((byte[])sidecarCbor.Clone(), (byte[])sealedBytes.Clone()));
        return Task.FromResult(NextUploadedBlobHash);
    }

    // Email Filters (`fauna.email.filters.*`) moved to the WS-RPC plane —
    // see MockNestRpcClient.

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}

/// <summary>
/// Mock ICryptoService for unit testing ViewModels that require crypto operations.
/// </summary>
public class MockCryptoService : ICryptoService
{
    public string ActorIdHex => new string('0', 64);
    public byte[] ActorIdBytes => new byte[32];
    public byte[] SecretBytes => new byte[32];
    public bool HasKey => true;
    public void LoadFromSecret(string secretHex) { }
    public string GenerateKeypair() => new string('0', 64);
    public byte[] Sign(byte[] message) => new byte[64];
}
