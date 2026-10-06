using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
using System.Text.Json;
using System.Text.Json.Serialization;
using FaunaApp.Core.Models;
using uniffi.fauna_ffi;
// fauna-launch-machine FFI types, aliased one by one: pull in only the launch
// types the bearer path needs.
using LaunchMachine = uniffi.fauna_launch_machine.LaunchMachine;
using TokenStatus = uniffi.fauna_launch_machine.TokenStatus;

namespace FaunaApp.Core.Services;

/// <summary>
/// HTTP client that calls the standard nest API directly (same endpoints as web/Android/iOS/Linux).
/// Uses Ed25519 signature-based auth via ICryptoService. Does not require a local proxy.
/// </summary>
public sealed class DirectNestClient : INestHttpClient
{
    private HttpClient _http;
    private readonly ICryptoService _crypto;
    private string _nestUrl;
    /// <summary>
    /// The <c>host[:port]</c> authority the nest's TLS identity is pinned under
    /// (shared-Rust <c>FaunaFfiMethods.AuthorityOf</c> of <see cref="_nestUrl"/>) —
    /// the pin key the cert-validation callback looks up. Kept in lockstep with
    /// <see cref="_http"/> by <see cref="CreateHttpClient"/>.
    /// </summary>
    private string _authority = string.Empty;
    /// <summary>
    /// The configured nest's URL as a <see cref="Uri"/> — what the cert callback's
    /// <see cref="NestCertTrust.IsForNest"/> guard compares a request's host + port
    /// against (numerically; it never re-derives <see cref="_authority"/>, the
    /// port-as-written pin key). Kept in lockstep with <see cref="_http"/> and
    /// <see cref="_authority"/> by <see cref="CreateHttpClient"/>.
    /// </summary>
    private Uri? _nestUri;
    private string? _bearerToken;
    private DateTimeOffset _tokenExpiry = DateTimeOffset.MinValue;
    /// <summary>
    /// The fauna-launch-machine that drove this client's launch, when one was
    /// wired (the <c>MainPage</c> path — see <c>App.xaml.cs.StartMainAppAsync</c>).
    /// When non-null, <see cref="EnsureAuthAsync"/> sources the bearer from the
    /// machine (<see cref="LaunchMachine.CurrentBearer"/> / <see cref="LaunchMachine.RefreshToken"/>)
    /// and only falls back to the self-acquire FFI-mint path (<c>fauna.auth.handshake</c>)
    /// if the machine can't supply one. Null on the onboarding / test-agent / e2e
    /// bridge-<c>session</c> paths, where the self-acquire path is the only one.
    /// </summary>
    private readonly LaunchMachine? _launch;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    /// <summary>
    /// Creates a new DirectNestClient that self-acquires its bearer via the
    /// shared FFI <c>mint_bearer</c> (<c>fauna.auth.handshake</c>) (the onboarding /
    /// test-agent / e2e bridge-<c>session</c> paths — no <see cref="LaunchMachine"/> wired).
    /// </summary>
    /// <param name="nestUrl">Base URL of the nest server (e.g. https://nest.fauna.example).</param>
    /// <param name="crypto">CryptoService for Ed25519 signing.</param>
    public DirectNestClient(string nestUrl, ICryptoService crypto) : this(nestUrl, crypto, null) { }

    /// <summary>
    /// Creates a new DirectNestClient wired to the <see cref="LaunchMachine"/>
    /// that authenticated this session (the <c>MainPage</c> path —
    /// <c>App.xaml.cs.StartMainAppAsync</c>). Bearer acquisition delegates to
    /// the machine; the self-acquire FFI-mint path (<c>fauna.auth.handshake</c>) is
    /// only a last-resort fallback (machine wedged in a non-refreshable <c>Offline</c>).
    /// <para>This ctor is <c>internal</c> because <paramref name="launch"/>'s
    /// type is UniFFI-emitted <c>internal</c>; the <c>FaunaApp</c> WinUI
    /// assembly sees it via <c>[InternalsVisibleTo]</c>.</para>
    /// </summary>
    /// <param name="nestUrl">Base URL of the nest server.</param>
    /// <param name="crypto">CryptoService for Ed25519 signing.</param>
    /// <param name="launch">The launch machine driving this session's auth.</param>
    internal DirectNestClient(string nestUrl, ICryptoService crypto, LaunchMachine? launch)
    {
        _nestUrl = nestUrl.TrimEnd('/');
        _crypto = crypto;
        _launch = launch;
        _http = CreateHttpClient(_nestUrl);
    }

    /// <summary>
    /// Builds the residual-HTTP client (health / blob / snapshot) with a
    /// <c>ServerCertificateCustomValidationCallback</c> that trusts a self-signed
    /// nest the same way the rest of the Rust stack does — the shared pinned SPKI —
    /// so a same-box install (<c>https://127.0.0.1:443</c>) and a remote
    /// <c>test@&lt;ip&gt;</c> self-signed nest both work without a public CA. Without
    /// it, .NET validates these direct HTTPS calls against the OS trust store and
    /// rejects the floor cert (media / health / snapshot silently break). Also
    /// captures <see cref="_authority"/> (the pin key) and <see cref="_nestUri"/> (the
    /// host + port the carve-outs are granted to) for the URL it serves. See
    /// <c>docs/goal/architecture/security.md</c> § Transport trust.
    /// </summary>
    private HttpClient CreateHttpClient(string nestUrl)
    {
        _authority = FaunaFfiMethods.AuthorityOf(nestUrl);
        _nestUri = new Uri(nestUrl);
        var handler = new HttpClientHandler
        {
            ServerCertificateCustomValidationCallback = ValidateServerCertificate,
        };
        return new HttpClient(handler)
        {
            BaseAddress = _nestUri,
            Timeout = TimeSpan.FromSeconds(30),
        };
    }

    /// <summary>
    /// The platform-side trust policy for the nest's TLS cert: accept iff the chain
    /// is WebPKI-valid, OR the host is loopback (same-box install — sanctioned prior
    /// art; also covers the unauthenticated health check that can precede the WS
    /// handshake that graduates a pin), OR the served cert's SPKI matches the pin.
    /// The SPKI + pin are computed in shared Rust (<see cref="NestCertTrust"/> only
    /// decides). Fail-closed otherwise.
    /// <para>The loopback and pin carve-outs are the CONFIGURED nest's, so they are
    /// granted only to a request for that nest's own host and port
    /// (<see cref="NestCertTrust.IsForNest"/>): <c>HttpClientHandler</c> follows a 30x
    /// by default and calls back for the redirected request, and a redirect anywhere
    /// else — or a request naming no host — falls to strict WebPKI alone. Apple's
    /// <c>APIClient</c> leg grants the same; two legs of one policy must not differ.</para>
    /// <para><c>internal</c> so <c>FaunaApp.Tests</c> can drive the callback directly
    /// (<c>[InternalsVisibleTo]</c>) with a hand-built request — no socket needed to
    /// pin what a request for another host is granted.</para>
    /// </summary>
    internal bool ValidateServerCertificate(
        HttpRequestMessage request, X509Certificate2? cert, X509Chain? chain, SslPolicyErrors errors)
    {
        var webPkiValid = errors == SslPolicyErrors.None;
        if (!IsRequestForNest(request))
            return webPkiValid;
        var isLoopback = NestCertTrust.IsLoopbackAuthority(_authority);
        var certSpki = cert is not null ? FaunaFfiMethods.SpkiSha256OfCertDer(cert.RawData) : null;
        var pinnedSpki = FaunaFfiMethods.PinnedSpkiForHost(_authority);
        return NestCertTrust.ShouldTrust(certSpki, pinnedSpki, webPkiValid, isLoopback);
    }

    /// <summary>
    /// Whether the request's host + port are the configured nest's. A request with
    /// no absolute URI names no host, so it is not the nest's (fail-closed).
    /// </summary>
    private bool IsRequestForNest(HttpRequestMessage request)
        => request.RequestUri is { IsAbsoluteUri: true } uri
           && _nestUri is not null
           && NestCertTrust.IsForNest(uri.Host, uri.Port, _nestUri);

    // ── Auth token lifecycle ──

    /// <summary>
    /// Mirrors the shared-Rust <c>fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS</c>
    /// — the one owner of this policy (<c>docs/goal/behavior/login.md</c> § the
    /// token-TTL paragraph): a cached bearer within this many seconds of
    /// <c>expires_at</c> is treated as spent. Not exported over UniFFI (a getter
    /// for one <c>u64</c> would cost a binding regen across Kotlin/Swift/C#/Go for
    /// no other consumer); this named const is windows' one mirror instead of
    /// three bare <c>60</c> literals (the two below, plus
    /// <c>App.xaml.cs.RunTtlRefreshLoopAsync</c>'s wake calculation).
    /// <para>⚠ Must stay STRICTLY LESS than the nest's token TTL — the Rust side
    /// pins that inequality at compile time (<c>auth_core.rs</c>, beside
    /// <c>TOKEN_TTL_SECS</c>); a buffer at or above the TTL makes every minted
    /// token born spent, since no cache would ever serve one before re-minting.</para>
    /// </summary>
    public const int BearerRefreshBufferSecs = 60;

    // TODO: route every call through one
    // 401-retrying send helper that, when `_launch is not null`, calls
    // `_launch.Notify401()` + `_launch.RefreshToken()` and retries once on a
    // fresh bearer. v1 relies on this method's pre-expiry refresh + the C#-side
    // TTL refresh loop (App.xaml.cs.RunTtlRefreshLoopAsync) to keep 401s from
    // happening at all — many public methods here do their own _http.*Async
    // after EnsureAuthAsync, so a partial fix at just the JSON chokepoints would
    // be a half-cleanup. Tracked internally.

    /// <summary>
    /// Ensures a valid bearer token is set on <see cref="_http"/>.
    /// <para>When a <see cref="LaunchMachine"/> is wired (<c>_launch is not null</c> —
    /// the <c>MainPage</c> path): the bearer comes from the machine. If the
    /// machine's snapshot has a <see cref="TokenStatus.Valid"/> token more than
    /// <see cref="BearerRefreshBufferSecs"/> from expiry, use
    /// <see cref="LaunchMachine.CurrentBearer"/> directly; otherwise ask the
    /// machine to <see cref="LaunchMachine.RefreshToken"/> and re-read. If the
    /// machine still can't supply a bearer (wedged in a non-refreshable
    /// <c>Offline</c>), fall through to the self-acquire path below as a last
    /// resort.</para>
    /// <para>When <c>_launch is null</c> (onboarding / test-agent / e2e
    /// bridge-<c>session</c> path): the self-acquire logic mints a bearer via the
    /// shared FFI <c>mint_bearer</c> over <c>fauna.auth.handshake</c> — refreshed
    /// <see cref="BearerRefreshBufferSecs"/> before expiry. This fallback is
    /// load-bearing for the <c>logged_in_app</c> e2e fixture, which never
    /// constructs a machine.</para>
    /// </summary>
    private async Task EnsureAuthAsync(CancellationToken ct)
    {
        if (_launch is not null)
        {
            var snap = _launch.Snapshot();
            var nowSecs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            if (snap.@token is TokenStatus.Valid v && v.@expiresAtSecs > nowSecs + BearerRefreshBufferSecs)
            {
                var bearer = _launch.CurrentBearer();
                if (bearer is not null)
                {
                    _http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", bearer);
                    return;
                }
            }

            // Token expired/refreshing, or CurrentBearer() came back null —
            // ask the machine to refresh, then re-read.
            try { await _launch.RefreshToken().ConfigureAwait(false); }
            catch { /* the failed refresh moved state to Offline; fall through to self-acquire */ }

            var freshBearer = _launch.CurrentBearer();
            if (freshBearer is not null)
            {
                _http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", freshBearer);
                return;
            }
            // Machine couldn't supply a bearer (wedged Offline / non-refreshable)
            // — fall through to the self-acquire path as a last resort.
        }

        if (_bearerToken != null && DateTimeOffset.UtcNow < _tokenExpiry.AddSeconds(-BearerRefreshBufferSecs))
            return;

        if (!_crypto.HasKey)
            throw new InvalidOperationException("CryptoService has no key loaded. Generate a keypair or load a key first.");

        // Mint the bearer over the pre-identity WS-RPC `fauna.auth.handshake` kind
        // via the shared-Rust `mint_bearer` — the faithful 1:1 replacement for the
        // retired HTTP `POST /api/v1/auth/token` twin (api-layers.md § auth;
        // transport.md § Pre-identity). `ExpiresAt` is an absolute unix timestamp
        // in *seconds*; the cache check above refreshes BearerRefreshBufferSecs
        // before it. On a fault MintBearer throws and the cached bearer/expiry
        // are left untouched.
        var minted = await FaunaFfiMethods.MintBearer(_nestUrl, _crypto.SecretBytes).ConfigureAwait(false);
        _bearerToken = minted.@token;
        _tokenExpiry = DateTimeOffset.FromUnixTimeSeconds((long)minted.@expiresAt);

        _http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", _bearerToken);
    }

    /// <summary>
    /// Returns a valid bearer token, acquiring one if needed.
    /// </summary>
    public async Task<string> GetBearerTokenAsync(CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        // When a machine is wired, EnsureAuthAsync set the header from the
        // machine's bearer (or the self-acquire fallback wrote _bearerToken);
        // either way the Authorization header is the source of truth here.
        if (_launch is not null)
        {
            var bearer = _launch.CurrentBearer();
            if (bearer is not null)
                return bearer;
            // Machine had no bearer → EnsureAuthAsync fell through to
            // self-acquire, which populated _bearerToken.
        }
        return _bearerToken!;
    }

    /// <summary>
    /// <see cref="GetBearerTokenAsync"/> plus the bearer's expiry — unix seconds on this
    /// device's clock, anchored at receipt (login.md § Token lifetime on the client's
    /// clock): the machine's own deadline when a machine is wired, else the self-acquired
    /// mint's. What the sync-agent provisioning loop pushes, so the agent plans its
    /// renewal on a real deadline.
    /// </summary>
    public async Task<(string Token, ulong ExpiresAt)> GetBearerWithExpiryAsync(CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        if (_launch is not null
            && _launch.CurrentBearer() is string bearer
            && _launch.Snapshot().@token is TokenStatus.Valid v)
            return (bearer, v.@expiresAtSecs);
        return (_bearerToken!, (ulong)Math.Max(0L, _tokenExpiry.ToUnixTimeSeconds()));
    }

    // The silent-sign-in (challenge + /auth/verify) flow used to live here as
    // SilentSignInAsync / SilentSignInResult; it was consumed only by the
    // pre-launch-machine App.xaml.cs launch logic. The launch flow now runs
    // entirely inside fauna-launch-machine (the machine does the silent
    // challenge and writes handle/domain/tier to the long-term store via the
    // shared RegistryLaunchPersistence), so the C#-side copy was removed.

    // ── HTTP helpers ──

    private async Task<JsonElement> GetJsonAsync(string url, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.GetAsync(url, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
    }

    private async Task<JsonElement> PostJsonAsync(string url, object? payload, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var content = payload != null
            ? JsonContent.Create(payload, options: JsonOptions)
            : null;
        var resp = await _http.PostAsync(url, content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
    }

    private async Task<JsonElement> DeleteJsonAsync(string url, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.DeleteAsync(url, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
    }

    private async Task PostNoResponseAsync(string url, object? payload, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var content = payload != null
            ? JsonContent.Create(payload, options: JsonOptions)
            : null;
        var resp = await _http.PostAsync(url, content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
    }

    private async Task DeleteNoResponseAsync(string url, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.DeleteAsync(url, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
    }

    private async Task<JsonElement> PostBytesAsync(string url, byte[] data, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var content = new ByteArrayContent(data);
        content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
        var resp = await _http.PostAsync(url, content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
    }

    private async Task<byte[]> GetBytesAsync(string url, CancellationToken ct)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.GetAsync(url, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
    }

    // ── Health ──

    public async Task<bool> IsAvailableAsync(CancellationToken ct = default)
    {
        try
        {
            // Health check does not require auth
            var resp = await _http.GetAsync("/api/v1/health", ct).ConfigureAwait(false);
            return resp.IsSuccessStatusCode;
        }
        catch
        {
            return false;
        }
    }

    // ── Identity / auth bootstrap ──

    // Account state (`GetIdentityAsync`), client-side identity generation, and
    // handle availability all left this plane — account state →
    // `fauna.account.get` (NestRpcClient); identity generation + handle checks
    // are driven by the shared OnboardingMachine. Only the bearer-token
    // self-acquire bootstrap stays here.

    public async Task<string> GetAuthTokenAsync(string nestUrl, CancellationToken ct = default)
    {
        // Update nest URL if different
        if (!string.Equals(_nestUrl, nestUrl.TrimEnd('/'), StringComparison.OrdinalIgnoreCase))
        {
            _nestUrl = nestUrl.TrimEnd('/');
            _http = CreateHttpClient(_nestUrl); // re-capture authority + cert callback
            _bearerToken = null; // Force re-auth
        }

        await EnsureAuthAsync(ct).ConfigureAwait(false);
        return _bearerToken!;
    }

    // `RegisterAsync` (HTTP `POST /api/v1/register`) was removed: account
    // creation rides the anonymous WS-RPC `fauna.account.register` kind via the
    // shared OnboardingMachine, and Settings handle changes now ride the
    // authenticated `fauna.profile.handle.change` kind (NestRpcClient /
    // INestRpcClient.ChangeHandleAsync). This was the last caller of the
    // `/api/v1/register` HTTP twin, which the nest has since dropped.

    // ── Configuration ──

    public Task ConfigureAsync(string? nestUrl, CancellationToken ct = default)
    {
        // In direct mode, configure updates the stored nest URL.
        // HttpClient.BaseAddress cannot be changed after the first request,
        // so we create a new HttpClient instance.
        if (nestUrl != null)
        {
            _nestUrl = nestUrl.TrimEnd('/');
            _http = CreateHttpClient(_nestUrl);
            _bearerToken = null; // Force re-auth with new URL
        }
        return Task.CompletedTask;
    }

    // GetNodeInfoAsync removed — the GET /api/v1/node-info twin was deleted
    // nest-side; the admin dashboard reads the version from fauna.admin.status
    // (NestRpcClient.AdminStatusAsync).

    // ── Messaging ──
    // The whole messaging surface left this plane — direct + group send and
    // conversation list/detail (List/Get conversation, List/Create/
    // SendGroupMessage) were removed as dead code; the Conversations page runs
    // through the shared-Rust `ConversationsManager` (UniFFI) and the nest HTTP
    // twins were deleted in the conversations T8 cutover.
    // See docs/goal/ui/conversations.md.

    // ── Contacts ──
    // The roster, knock actions, AND add-contact knock-send all migrated to
    // WS-RPC (fauna.{contacts,knocks}.* + fauna.inbox.send, NestRpcClient). The
    // old unauthenticated `POST /api/v1/inbox/{actor}` knock twin — which sent an
    // off-spec JSON `{from,handle,type:"knock"}` the nest's verify_inbox_payload
    // rejects — is gone; the canonical signed (ContactRequest, Post) tuple now
    // rides `fauna.inbox.send` (federation.md § Federation residue surface).

    // ── Backups / Snapshots ──
    // Entirely on the WS-RPC façade (`fauna.filesync.snapshot.*`,
    // `fauna.sync.backup_status`, and the client-side byte walk
    // DownloadSnapshotFileBytesAsync — INestRpcClient); no snapshot route is HTTP.

    // ── Service Status ──

    public async Task<ServiceStatusInfo> GetServiceStatusAsync(CancellationToken ct = default)
    {
        // Combine health check + sync device info for service status
        var healthOk = await IsAvailableAsync(ct).ConfigureAwait(false);
        var connection = healthOk ? ConnectionState.Connected : ConnectionState.Disconnected;

        var sync = new SyncStatusInfo(healthOk, false, 0, 0, null);
        IdentityInfo? identity = null;

        if (_crypto.HasKey)
        {
            identity = new IdentityInfo(_crypto.ActorIdHex, null, _nestUrl);
        }

        return new ServiceStatusInfo("direct", 0, connection, sync, identity);
    }

    // Folder list/create/delete moved to the WS-RPC façade
    // (`fauna.folders.{list,create,delete}`, NestRpcClient.Folders*) — the
    // `/api/v1/file-sets*` HTTP twins were deleted nest-side.

    // The sync control-plane HTTP twins (/api/v1/sync/{files,status,changes,
    // devices,register,conflicts}) were all lifted to fauna.sync.* via
    // INestRpcClient or the shared machines (files → the media machine;
    // devices → DevicesMachine; backup-status → BackupStatusAsync;
    // conflicts → ConflictsList/ResolveAsync).
    // The nest deleted these routes, so this HTTP client carries none of them.

    // Bridge Management (`fauna.bridges.*`, NestRpcClient) — the HTTP twins were
    // deleted nest-side. list/link/unlink/follows ride the WS-RPC seam.

    // The feed/posts HTTP twins (/api/v1/feeds*, /api/v1/posts*) were deleted in
    // the ws-rpc-everywhere cutover; the surface rides WS-RPC (fauna.feed.* /
    // fauna.posts.*) via INestRpcClient. Only the blob byte plane stays on HTTP
    // (UploadBlobAsync / GetBlobAsync below) — permanent byte-bulk residue.
    // Bridge-feed subscription rides `fauna.bridges.feeds.*`, not HTTP.

    // Search migrated to WS-RPC (fauna.search.query, NestRpcClient.SearchQueryAsync);
    // the GET /api/v1/search twin is deleted nest-side.

    // Inbox mode migrated to WS-RPC (fauna.inbox.mode.{get,set}, NestRpcClient).

    // Events + event-social migrated to WS-RPC (`fauna.events.*` /
    // `fauna.calendars.*`, NestRpcClient.Events*); the HTTP twins were dead
    // client-side after the events migration and have been removed.

    // Spam preferences moved to the WS-RPC façade (`fauna.spam.{get,set}_preferences`,
    // NestRpcClient.Spam*) — the `GET|PUT /api/v1/spam/preferences` HTTP twin was
    // deleted nest-side. Moderation stats / actions / train likewise lost their HTTP
    // twins; their WS-RPC client seam isn't built yet.


    // ── Account Management ──
    // Account get / quota / delete moved to the WS-RPC façade
    // (NestRpcClient) — the HTTP `/api/v1/account` twins were deleted nest-side.

    /// <summary>
    /// `include_blobs=true` is what makes this the whole archive rather than
    /// an index of it — the nest defaults the flag off. Not a user choice:
    /// account-data-plane.md § Nest-side requirements item 1, Payload stores
    /// decision (5). A plain relative-URI GetAsync, so the `?` is parsed as a query
    /// delimiter rather than percent-encoded (the bug apple's
    /// `appendingPathComponent` hit).
    /// </summary>
    public async Task<byte[]> ExportAccountDataAsync(CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.GetAsync("/api/v1/export?include_blobs=true", ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
    }

    // Community-group members/messages HTTP methods (GetGroupMembers,
    // InviteToGroup, RemoveFromGroup, GetGroupMessages) removed as dead code —
    // the nest HTTP twins were deleted in the conversations T8 cutover and
    // these had no callers. See docs/goal/ui/conversations.md.

    // MLS Welcomes: the legacy client-side-MLS HTTP welcome poll
    // (GET /api/v1/welcome/{actor}) was removed — welcome receive is now the
    // shared-Rust push-driven loop on ConversationsSession (welcomes WS-pushed per
    // docs/goal/ui/conversations.md § MLS Welcome at-rest). The keypackage POOL
    // (count + upload) already rides the WS-RPC FfiConversationsClient seam
    // (NestRpcClient.Keypackage{Count,Upload}Async; rule #2 "no client-side MLS").

    // Notifications migrated to WS-RPC (fauna.notifications.{list,mark_read,
    // count}, NestRpcClient).

    // Event-social (invite / co-hosts / discussion) migrated to WS-RPC with the
    // rest of the events surface — twins removed (see the Events note above).

    // Bridge Feed Subscriptions (`fauna.bridges.feeds.*`, NestRpcClient) — the
    // HTTP twins were deleted nest-side.

    // Snapshot prune / check / delete moved to the WS-RPC façade
    // (`fauna.filesync.snapshot.{prune,check,delete}`, INestRpcClient) — the
    // deprecated HTTP twins were removed with the rest of the backups migration.

    // ── Blobs ──

    public async Task<string> UploadBlobAsync(byte[] data, UploadAudience audience, CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);

        // Seal + sidecar in shared Rust (process_and_seal) — every app encodes
        // the wire bytes through this one packer so they are byte-identical
        // (priority #1/#2). The nest's `parse_multipart_upload` reads exactly two
        // parts named `sidecar` (DAG-CBOR UploadSidecar) and `bytes` (the sealed
        // primary). See docs/goal/architecture/encryption-at-rest.md § Media row.
        var payload = FaunaFfiMethods.ProcessAndSealUpload(data, MapAudience(audience));

        // Best-effort thumbnail upload (the nest does not gate the primary on it).
        // The on-device `process_media` is now real on every native app
        // (landed 2026-06-30), so a >300px
        // image yields a thumbnail here; the nest serves it via `?thumb=1` off the
        // primary's stored sidecar hash.
        if (payload.@thumbnail is { } thumb)
        {
            try
            {
                using var thumbForm = BuildBlobMultipart(thumb.@sidecarCbor, thumb.@bytes);
                using var thumbResp = await _http.PostAsync("/api/v1/blob", thumbForm, ct).ConfigureAwait(false);
            }
            catch
            {
                // Non-fatal: the primary upload below still proceeds.
            }
        }

        using var content = BuildBlobMultipart(payload.@primary.@sidecarCbor, payload.@primary.@bytes);
        var resp = await _http.PostAsync("/api/v1/blob", content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        var json = await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
        return json.GetProperty("hash").GetString() ?? throw new InvalidOperationException("Blob upload missing hash");
    }

    public async Task<string> UploadPreparedBlobAsync(
        byte[] sidecarCbor,
        byte[] bytes,
        byte[]? thumbnailSidecarCbor,
        byte[]? thumbnailBytes,
        CancellationToken ct = default)
    {
        // The compose-attachment path: shared Rust already resolved the composer's
        // audience and sealed (or passed through, for a public compose) the bytes —
        // FeedManager::seal_compose_attachment, the seal-by-id helper media.md §
        // Encryption at rest names. So this is UploadBlobAsync MINUS process_and_seal,
        // with the same best-effort thumbnail-then-primary order.
        await EnsureAuthAsync(ct).ConfigureAwait(false);

        if (thumbnailSidecarCbor is not null && thumbnailBytes is not null)
        {
            try
            {
                using var thumbForm = BuildBlobMultipart(thumbnailSidecarCbor, thumbnailBytes);
                using var thumbResp = await _http.PostAsync("/api/v1/blob", thumbForm, ct).ConfigureAwait(false);
            }
            catch
            {
                // Non-fatal: the primary upload below still proceeds.
            }
        }

        using var content = BuildBlobMultipart(sidecarCbor, bytes);
        var resp = await _http.PostAsync("/api/v1/blob", content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        var json = await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
        return json.GetProperty("hash").GetString() ?? throw new InvalidOperationException("Blob upload missing hash");
    }

    public async Task<string> UploadSealedBlobAsync(byte[] sidecarCbor, byte[] sealedBytes, CancellationToken ct = default)
    {
        // The gated-post path: the sealed full-body blob was produced by the shared
        // FeedManager::prepare_gated_blob (already ciphertext) and the sidecar is the
        // PeriodRestrictedPost class from FaunaFfiMethods.GatedPostSidecar(). This is
        // UploadBlobAsync MINUS process_and_seal — we must NEVER re-seal opaque bytes nor
        // swap the sidecar. Mirrors linux fauna_client::upload_gated_post_blob.
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        using var content = BuildBlobMultipart(sidecarCbor, sealedBytes);
        var resp = await _http.PostAsync("/api/v1/blob", content, ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        var json = await resp.Content.ReadFromJsonAsync<JsonElement>(JsonOptions, ct).ConfigureAwait(false);
        return json.GetProperty("hash").GetString() ?? throw new InvalidOperationException("Gated blob upload missing hash");
    }

    /// <summary>
    /// Maps the public <see cref="UploadAudience"/> selector to the UniFFI-internal
    /// <c>FfiUploadAudience</c> the shared-Rust packer consumes. Only the two
    /// client-key audiences exist (see <see cref="UploadAudience"/>).
    /// </summary>
    internal static FfiUploadAudience MapAudience(UploadAudience audience) => audience switch
    {
        UploadAudience.PublicPost => new FfiUploadAudience.PublicPost(),
        UploadAudience.Library lib => new FfiUploadAudience.Library(lib.BackupKey),
        _ => throw new ArgumentOutOfRangeException(nameof(audience), audience, "unknown upload audience"),
    };

    /// <summary>
    /// Builds the <c>multipart/form-data</c> body for <c>POST /api/v1/blob</c>:
    /// exactly two parts named <c>sidecar</c> (<c>application/cbor</c>, the canonical
    /// DAG-CBOR <c>UploadSidecar</c>) and <c>bytes</c> (<c>application/octet-stream</c>,
    /// the sealed primary). Matches the nest's <c>parse_multipart_upload</c> contract
    /// and the linux <c>post_multipart_blob</c> reference.
    /// </summary>
    internal static MultipartFormDataContent BuildBlobMultipart(byte[] sidecarCbor, byte[] sealedBytes)
    {
        var form = new MultipartFormDataContent();
        var sidecar = new ByteArrayContent(sidecarCbor);
        sidecar.Headers.ContentType = new MediaTypeHeaderValue("application/cbor");
        form.Add(sidecar, "sidecar");
        var bytes = new ByteArrayContent(sealedBytes);
        bytes.Headers.ContentType = new MediaTypeHeaderValue("application/octet-stream");
        form.Add(bytes, "bytes");
        return form;
    }

    public async Task<byte[]> GetBlobAsync(string hash, CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.GetAsync($"/api/v1/blob/{hash}", ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        return await resp.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
    }

    public async Task<(byte[] Data, bool HasC2pa)> GetBlobWithC2paAsync(string hash, CancellationToken ct = default)
    {
        await EnsureAuthAsync(ct).ConfigureAwait(false);
        var resp = await _http.GetAsync($"/api/v1/blob/{hash}", ct).ConfigureAwait(false);
        resp.EnsureSuccessStatusCode();
        var data = await resp.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
        var hasC2pa = resp.Headers.TryGetValues("X-C2PA", out var values)
            && string.Equals(values.FirstOrDefault(), "true", StringComparison.OrdinalIgnoreCase);
        return (data, hasC2pa);
    }

    // Email Filters (`fauna.email.filters.*`, NestRpcClient) — the HTTP twins
    // were deleted nest-side.

    // ── Admin ──
    // Stats (fauna.admin.stats) and the is-admin gate (fauna.account.am_i_admin)
    // moved to WS-RPC (INestRpcClient.AdminStatsAsync / AmIAdminAsync); the
    // /admin/api/stats twin was deleted nest-side, so nothing remains here.

    // User administration (users / invite codes / invite requests) moved to the
    // admin-users hub over the fauna.admin.* WS-RPC kinds (FfiAdminClient) — the
    // /admin/api/users, /invite-codes, /invite-requests twins are no longer called.

    // ── Dispose ──

    public ValueTask DisposeAsync()
    {
        _http.Dispose();
        return ValueTask.CompletedTask;
    }
}
