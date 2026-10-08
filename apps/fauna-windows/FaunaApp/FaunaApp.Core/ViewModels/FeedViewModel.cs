using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Thin observer over the shared <see cref="FfiFeedManager"/> — the Feed page's
/// analogue of <see cref="ConversationsViewModel"/>. All authoritative
/// post-list / search / feed-rule / compose state lives in Rust
/// (<c>libs/fauna-feed::FeedManager</c>); this view-model snapshots it and
/// raises <see cref="INotifyPropertyChanged"/> for the page to re-render
/// (feed.md § State &amp; data shape, ratified 2026-06-14 — no client-side
/// post-list state). The retired ad-hoc VM (its own <c>Posts</c>/<c>Feeds</c>
/// collections, cursor pagination, client-side search filter, and
/// build-sign-create compose) is replaced wholesale.
///
/// <para>Constructor takes the manager and the WinUI-side observer (a
/// <c>FeedNotifyObserver</c> in the app project, passed as the
/// <see cref="FeedSnapshotObserver"/> trait); the VM registers it with the
/// manager and forwards its blanket "everything changed" notification.
/// <see cref="ViewModelBase"/> supplies the reconnect re-hydrate wiring
/// (transport.md § Push events — the feed has no poll backstop, so a post that
/// arrived while disconnected reaches the list only via the reconnect re-fetch).</para>
///
/// <para>Marked <c>internal</c> because the UniFFI-generated types
/// (<see cref="FfiFeedManager"/>, <see cref="FeedSnapshot"/>, the snapshot
/// records) are emitted as <c>internal</c>; <see cref="FeedPostItem"/> /
/// <see cref="PostSourceBadge"/> below stay <c>public</c> so the post-card /
/// feed-item DataTemplates materialize (a WinUI ListView skips rows whose
/// <c>x:DataType</c> is internal).</para>
/// </summary>
internal partial class FeedViewModel : ViewModelBase
{
    /// <summary>Singleton accessor mirroring <c>ConversationsViewModel.Current</c>;
    /// the E2E state bridge reads the live snapshot through it.</summary>
    internal static FeedViewModel? Current { get; private set; }

    private readonly FfiFeedManager _manager;
    // Observer held to keep its callback alive for the manager's lifetime.
    private readonly FeedSnapshotObserver _observer;
    private FeedSnapshot? _cachedSnapshot;

    // `rpc` is no longer FIELDED: the last thing this VM held it for was the raw
    // posts.interact call, which now goes through the manager. It is still used
    // here, for the reconnect re-hydrate wiring below.
    public FeedViewModel(FfiFeedManager manager, FeedSnapshotObserver observer, INestRpcClient rpc)
    {
        _manager = manager;
        _observer = observer;
        Current = this;
        // `manager` is FeedManagerHost's session-lifetime instance, published by
        // the host itself — this VM no longer sets it. It used to, back when the
        // manager was per-page-load and the newest VM's was by definition the
        // live one; with one manager per session that assignment would only be a
        // second writer racing the host for the same slot.

        if (observer is INotifyPropertyChanged inpc)
            inpc.PropertyChanged += (_, _) =>
            {
                _cachedSnapshot = null;
                // A timeline of what each reload actually returned, beside the
                // `feed_reloads` triple the shared re-hydrate barrier reads. This is
                // the "what came back" half that barrier's own failure text tells the
                // next session to hunt ("Hunt upstream — what the fetch asked for and
                // what came back"), and reading it off the live snapshot is the only
                // place that answer exists. Gated on E2eTrace being armed, so it costs
                // nothing unless a human exported FAUNA_E2E_AGENT_LOG, and the whole
                // sink is compiled out of release builds (e2e convention 15).
                if (Logs.E2eTrace.Enabled)
                {
                    try
                    {
                        var snap = _manager.Snapshot();
                        Logs.E2eTrace.Write(
                            $"[feed-snap] mgr={_manager.GetHashCode()} reloads={_manager.FeedReloadsJson()} "
                            + $"status={snap.@status} posts={snap.@posts.Length} hasMore={snap.@hasMore} "
                            + $"selected={snap.@selectedFeed ?? "<local>"} search={snap.@searchQuery ?? "<none>"} "
                            + $"err={(snap.@error is null ? "<none>" : "set")} "
                            + $"bodies=[{string.Join(" | ", snap.@posts.Select(x => x.@body.Length > 40 ? x.@body.Substring(0, 40) : x.@body))}]");
                    }
                    catch (System.Exception ex) { Logs.E2eTrace.Write($"[feed-snap] threw: {ex.Message}"); }
                }
                OnPropertyChanged(string.Empty);
            };

        // The manager now OUTLIVES this view-model (FeedManagerHost — one per
        // session, not per Feed-page load), so a previous page's observer is
        // still registered on it and points at a torn-down page. `add_observer`
        // appends, so registering ours without this would grow the observer list
        // by one on every navigation back to the feed, each dead one still
        // notified on every mutation. Exactly one Feed page is live at a time,
        // and this VM is windows' only feed-observer registrant, so clearing
        // first is precisely "replace the page's observer".
        _manager.ClearObservers();
        _manager.AddObserver(observer);

        // Re-hydrate the feed on every WS reconnect — it has no poll backstop, so a
        // post that arrived while disconnected would otherwise stay invisible until
        // a manual refresh (transport.md § Push events). The shared FeedManager is
        // transport-agnostic, so the re-query is wired here (the deterministic guard
        // for the tier_3 test_nest_flip_feed_rehydrate windows fan-out).
        RefreshOnReconnect(rpc, ReloadCommand);
    }

    /// <summary>The shared manager — exposed for the page's direct mutator calls
    /// (compose update, resolve-media/quoted) that don't need a VM wrapper.</summary>
    public FfiFeedManager Manager => _manager;

    public FeedSnapshot Snapshot => _cachedSnapshot ??= _manager.Snapshot();

    // ── Read projections (page rebuilds its bound collections from these) ──

    public IReadOnlyList<FeedSummaryView> Feeds => Snapshot.feeds;
    public IReadOnlyList<PostSummary> Posts => Snapshot.posts;
    public IReadOnlyList<BridgeFeedView> BridgeFeeds => Snapshot.bridgeFeeds;

    /// <summary>The bridges this nest's build actually serves (`{id,name}`, from the
    /// server-filtered <c>fauna.bridges.list</c> — build + per-bridge runtime gated).
    /// Drives the `bridge-form-bridge-select` options and gates the
    /// `bridge-feed-subscribe-toggle` (hidden when empty), matching web/linux/android/apple
    /// (version-compatibility.md § Dimension 3 — the feed-selector capability gate).</summary>
    public IReadOnlyList<AvailableBridge> AvailableBridges => Snapshot.availableBridges;
    public string? SelectedFeedId => Snapshot.selectedFeed;

    /// <summary>Whether the built-in Trending virtual feed is the current
    /// selection (`trending.md` § The Trending feed) — additive beside
    /// <see cref="SelectedFeedId"/>, never both set (mutually exclusive with a
    /// custom feed / the local feed).</summary>
    public bool TrendingSelected => Snapshot.trendingSelected;
    public string? SearchQuery => Snapshot.searchQuery;
    public FeedComposeState Compose => Snapshot.compose;
    public BridgeFormState BridgeForm => Snapshot.bridgeForm;
    public FeedStatus Status => Snapshot.status;
    public bool HasMore => Snapshot.hasMore;
    public bool IsLoading => Snapshot.status == FeedStatus.Loading;

    /// <summary>Which empty state the page paints — `feed-empty-state` (NoPosts) or
    /// `feed-no-results` (NoMatches), or neither while a read is in flight or posts
    /// are on screen. The answer of the shared <c>FeedSnapshot::empty_state</c> over
    /// its UniFFI free export, never re-derived here from <see cref="Status"/>,
    /// <see cref="Posts"/> and <see cref="SearchQuery"/> (`ui/feed.md` § Errors &amp;
    /// edge cases).</summary>
    public FeedEmptyState? EmptyState => FaunaFeedMethods.FeedEmptyState(Snapshot);

    /// <summary>Page-level error (`error-message`) resolved from the snapshot's
    /// <c>LocalizedText</c> via the windows i18n pipeline (architectural rule 4 —
    /// localized strings come from the i18n table, not English literals).</summary>
    public string? ErrorText => Snapshot.error is { } e ? Strings.Resolve(e) : null;

    /// <summary>Composer error (`compose-error`) ← <c>snapshot.compose.error</c>.</summary>
    public string? ComposeErrorText => Snapshot.compose.error is { } e ? Strings.Resolve(e) : null;

    /// <summary>The local actor's own subscription tiers (`snapshot.own_tiers`) as display
    /// names — the <c>compose-gate-tier-select</c> options that follow the "Public" sentinel
    /// (monetization.md § Pillars 2+3). Refreshed for free by <see cref="ReloadAsync"/>
    /// (shared <c>refresh_feeds</c> → <c>refresh_own_tiers</c>), so a tier minted this session
    /// appears on the next feed nav — windows rebuilds the manager per nav (unlike the web/linux
    /// persisted singletons), so no explicit refresh call is owed.</summary>
    public IReadOnlyList<string> OwnTierNames => Snapshot.ownTiers.Select(t => t.name).ToList();

    /// <summary>The rooms the composer offers as a fourth audience answer
    /// (`snapshot.own_rooms`) — the `compose-gate-tier-select` options that follow the
    /// author's own tiers and precede "Sell this post…" (`ui/feed.md` § Encryption at
    /// rest → Room-restricted — the app half, *The rooms offered*). A projection of the
    /// conversations plane: refreshed for free by <see cref="ReloadAsync"/> (shared
    /// `refresh_feeds` → `refresh_own_rooms`) and again on that plane's own change tick
    /// (<c>RoomsRefreshObserver</c>, registered once on <c>ConversationsManagerHost.Instance</c>
    /// for the process's whole lifetime — not page-scoped), so a room joined or left
    /// reaches the composer with no re-entry into the feed.</summary>
    public IReadOnlyList<GateRoomOption> OwnRooms => Snapshot.ownRooms;

    // ── Actions (delegate to the manager; NO ConfigureAwait(false) — a WinUI VM
    //    must resume on the UI thread, else bound-state mutation throws a silent
    //    COMException). Page-/form-level failures land in the snapshot, not here. ──

    /// <summary>Re-hydrate the page: refresh the feed selector + bridge-feed lists
    /// and re-query the current feed. The initial Page_Loaded fetch and the
    /// reconnect re-fetch both run this. The re-query rides the shared
    /// `RefreshCurrentFeed` seam — same source (Local / Trending / a feed), same
    /// search term, sealed scorers reloaded — never `SelectFeed(SelectedFeedId)`:
    /// that id is `null` both for the local feed AND while Trending is selected
    /// (trending.md § The Trending feed), so re-selecting it would silently drop
    /// an active Trending selection on every refresh/reconnect.</summary>
    [RelayCommand]
    private async Task ReloadAsync()
    {
        await _manager.RefreshFeeds();
        await _manager.RefreshBridgeFeeds();
        await _manager.RefreshAvailableBridges();
        await _manager.RefreshCurrentFeed();
    }

    public async Task SelectFeedAsync(string? feedId) => await _manager.SelectFeed(feedId);

    /// <summary>Select the built-in Trending virtual feed (`feed-trending-item`,
    /// trending.md § The Trending feed) — mirrors <see cref="SelectFeedAsync"/>.</summary>
    public async Task SelectTrendingFeedAsync() => await _manager.SelectTrendingFeed();

    public async Task SetSearchQueryAsync(string? term) => await _manager.SetSearchQuery(term);

    public async Task ClearSearchAsync() => await _manager.ClearSearch();

    public async Task LoadMoreAsync() => await _manager.LoadMore();

    /// <summary>Stage the composer's text/tags/attachment as an explicit GESTURE —
    /// the user's own pick or remove of a file (<c>attachedFile</c> is the new
    /// staged handle, or <c>null</c> for a deliberate remove). This is the ONLY
    /// path allowed to change <c>attached_file</c> (`ui/feed.md` § Persistence →
    /// *Attachments by content address*); a content-only edit (typing) must use
    /// <see cref="UpdateComposeText"/> instead, which carries the already-staged
    /// attachment through untouched.</summary>
    public void UpdateCompose(string text, string tags, AttachedFile? attachedFile)
        => _manager.UpdateCompose(text, tags, attachedFile);

    /// <summary>Update the compose text/tags for a CONTENT-only edit — a
    /// keystroke, the post-restore already-typed sync, or the compose-dialog's
    /// whole-body edit — none of which are the user's attachment gesture.
    /// Carries the manager's OWN currently-staged <c>attached_file</c> through
    /// unchanged, so an edit never drops a restored draft's handle by passing
    /// <c>null</c> in its place (`ui/feed.md` § Persistence → *Attachments by
    /// content address*: staging <c>None</c> is reserved for the user's own
    /// remove gesture, <see cref="UpdateCompose"/>, never a side effect of
    /// typing). Reads the manager's live snapshot directly rather than the
    /// cached <see cref="Snapshot"/>, so the read is correct even before the
    /// notify round-trip has refreshed the cache.</summary>
    public void UpdateComposeText(string text, string tags)
        => _manager.UpdateCompose(text, tags, _manager.Snapshot().compose.attachedFile);

    /// <summary>Stage the composer's gate-to-tier fields (`compose-gate-tier-select` /
    /// `compose-gate-preview-field`) — the gate sibling of <see cref="UpdateCompose"/>.
    /// <paramref name="gateTier"/> <c>null</c> composes a normal (ungated) post; a tier name
    /// gates the post to that tier (feed.md § Encryption at rest).</summary>
    public void UpdateComposeGate(string? gateTier, string gatePreview)
        => _manager.UpdateComposeGate(gateTier, gatePreview);

    /// <summary>Stage the composer's **room** answer (`compose-gate-tier-select`'s fourth
    /// option, a room from <see cref="OwnRooms"/> by its hex channel id) — the room
    /// sibling of <see cref="UpdateComposeGate"/>, sharing its teaser. <c>null</c> leaves
    /// the room answer (back to Public); a hex channel id makes the post room-restricted
    /// and clears any staged tier/sale (`ui/feed.md` § Encryption at rest → Room-restricted
    /// — the app half, *The composer's fourth answer*).</summary>
    public void UpdateComposeRoom(string? gateRoom, string gatePreview)
        => _manager.UpdateComposeRoom(gateRoom, gatePreview);

    /// <summary>Stage the composer's teaser (`compose-gate-preview-field`) ALONE — shared by
    /// every restricted answer and no part of any key, so it touches no answer and drops no
    /// staged seal (the setter the other three would otherwise have to re-read the mode for).</summary>
    public void UpdateComposePreview(string gatePreview)
        => _manager.UpdateComposePreview(gatePreview);

    /// <summary>Process + seal one compose attachment for the composer's CURRENT audience and
    /// return the multipart parts to POST — the native door onto the shared-Rust seal-by-id
    /// helper (media.md § Encryption at rest). Call it at submit, AFTER
    /// <see cref="UpdateComposeGate"/>/<see cref="UpdateComposeSell"/> have staged the audience
    /// and BEFORE the bytes are uploaded: a tier's period key never crosses the FFI boundary, so
    /// <c>FaunaFfiMethods.ProcessAndSealUpload</c> (which <see cref="INestHttpClient.UploadBlobAsync"/>
    /// uses) can only express the two client-key audiences and would publish a plaintext copy of
    /// a restricted post's photo that no blob DELETE exists to remove. The reply's
    /// <c>mediaType</c> is the plaintext's real MIME and is the ONLY correct source for the
    /// <c>MediaItem</c> — a sealed sidecar says <c>application/octet-stream</c>.</summary>
    public async Task<ComposeAttachmentUpload> SealComposeAttachmentAsync(byte[] raw)
        => await _manager.SealComposeAttachment(raw);

    public async Task SubmitPostAsync() => await _manager.SubmitPost();

    /// <summary>Build + seal the staged GATED post (shared <c>build_gated_post</c>) and return its
    /// sealed full-body blob for the page to upload (`POST /api/v1/blob`, PeriodRestrictedPost
    /// sidecar). <c>null</c> ⇒ the composer isn't gated — call <see cref="SubmitPostAsync"/>
    /// instead. On success the signed post is staged for <see cref="SubmitGatedPostAsync"/>.</summary>
    public async Task<byte[]?> PrepareGatedBlobAsync() => await _manager.PrepareGatedBlob();

    /// <summary>The DAG-CBOR <c>UploadSidecar</c> bytes the STAGED gated post's sealed body
    /// uploads under — <c>GroupRestrictedPost</c> for a room post, a tier's/sale's
    /// <c>PeriodRestrictedPost</c> otherwise. Call this after <see cref="PrepareGatedBlobAsync"/>
    /// / <see cref="PrepareSellPostAsync"/>, in place of the free
    /// <c>FaunaFfiMethods.GatedPostSidecar()</c>, which knows only the tier class and would
    /// mis-tag a room post's body (`ui/feed.md` § Encryption at rest → Room-restricted — the
    /// app half, *One post, one key*). The class is decided off the staged post, never here.</summary>
    public byte[] GatedUploadSidecar() => _manager.GatedUploadSidecar();

    /// <summary>Create the gated post staged by <see cref="PrepareGatedBlobAsync"/>, after the
    /// page uploaded the sealed blob. <paramref name="uploadedHash"/> is the upload reply's hex
    /// hash — it must echo the staged post's <c>encrypted_ref</c>.</summary>
    public async Task SubmitGatedPostAsync(string uploadedHash) => await _manager.SubmitGatedPost(uploadedHash);

    /// <summary>Abort a staged gated submit whose blob upload failed: drop the staged post and
    /// surface <paramref name="message"/> on `compose-error`, keeping the composer text for a
    /// manual retry.</summary>
    public void AbortGatedSubmit(string message) => _manager.AbortGatedSubmit(message);

    /// <summary>Stage the composer's sell-this-post fields (`compose-sell-price` /
    /// `compose-sell-asking-price` / `compose-sell-subscribers-free`) — the sell
    /// sibling of <see cref="UpdateComposeGate"/>, carrying the shared teaser field
    /// too (monetization.md § Per-post pay-to-unlock: `compose-gate-preview-field`
    /// is shared by both gated answers). <paramref name="askingPrice"/> is the raw
    /// draft text (whole sats), independent of <paramref name="price"/> — the shared
    /// `SellComposeState.askingPrice` carries it for draft persistence; the parse
    /// happens at submit time in <see cref="PrepareSellPostAsync"/>.</summary>
    public void UpdateComposeSell(string price, string askingPrice, bool subscribersGetItFree, string gatePreview)
        => _manager.UpdateComposeSell(new SellComposeState(price, askingPrice, subscribersGetItFree), gatePreview);

    /// <summary>Phase one of sell-this-post, for a compose that carries an attachment: mint the
    /// unlock tier and persist its period key, creating nothing server-side. Call it between
    /// <see cref="UpdateComposeSell"/> and <see cref="SealComposeAttachmentAsync"/> — a sold
    /// post's photo seals under the tier the sale mints, and that tier does not exist when the
    /// file is picked. With NO attachment, don't call it: <see cref="PrepareSellPostAsync"/>
    /// runs it itself, so the one-call flow is unchanged. Pass the same arguments that call
    /// will: this one decides the tier's rank and refuses an unconvertible asking price while
    /// refusing is still free.</summary>
    public async Task StageSellTierAsync(bool subscribersGetItFree, string? askingPriceText)
        => await _manager.StageSellTier(
            subscribersGetItFree,
            ulong.TryParse(askingPriceText?.Trim(), out var sats) ? sats : (ulong?)null);

    /// <summary>Run the sell-this-post orchestration (mint + persist the period key, build the
    /// birth KeyBlob, build the gated body naming the auto-minted tier) and return its sealed
    /// full-body blob for the page to upload — the sell sibling of
    /// <see cref="PrepareGatedBlobAsync"/>, but never <c>null</c> on success (sell mode always
    /// produces a post; call this only when <c>IsSellSelected()</c>). Empty
    /// <paramref name="priceHint"/> is normalized to <c>null</c> by the caller, matching
    /// linux's exact shape. Finishes through the SAME <see cref="SubmitGatedPostAsync"/> /
    /// <see cref="AbortGatedSubmit"/> pair as an ordinary gated post — no new upload glue.</summary>
    public async Task<byte[]> PrepareSellPostAsync(string? priceHint, string? askingPriceText, bool subscribersGetItFree)
        // Empty or unparseable both mean no machine price — never an error at this
        // layer (mirrors tui's `feed/mod.rs` reference leg:
        // `sell.asking_price.trim().parse::<u64>().ok()` exactly); an unpriced sold
        // post is permanently valid, staying buyable through an explicit-intent
        // mechanism, with a zap on it staying a tip (monetization.md § The asking
        // price). `prepare_sell_post` owns the sats→msat conversion and the overflow
        // refusal — this is a plain text→u64 parse, nothing else.
        => await _manager.PrepareSellPost(
            priceHint,
            subscribersGetItFree,
            ulong.TryParse(askingPriceText?.Trim(), out var sats) ? sats : (ulong?)null);

    /// <summary>Resolve a loaded gated post's sealed-blob hash (hex <c>encrypted_ref</c>) for the
    /// page to fetch (`GET /api/v1/blob/{hash}`), caching the decoded gate info for
    /// <see cref="UnlockGatedPostAsync"/>. <c>null</c> when the post isn't loaded, isn't gated,
    /// or can't be decoded.</summary>
    public async Task<string?> GatedBlobHashAsync(string postId) => await _manager.GatedBlobHash(postId);

    /// <summary>Decrypt a gated post's full body from its fetched sealed blob (<paramref
    /// name="blobBytes"/>) and swap it into the snapshot (<c>body</c> + <c>gated_unlocked</c>),
    /// then notify — author via custody, subscriber via their KeyBlob wrap entry.</summary>
    public async Task UnlockGatedPostAsync(string postId, byte[] blobBytes)
        => await _manager.UnlockGatedPost(postId, blobBytes);

    // `factors` carries the `feed-factor-*` editor's accumulated entries
    // (`FeedPage.CreateFeed_Click`); `FeedManager::create_feed` splits them —
    // `global: false` onto this feed's own `composition`, `global: true`
    // read-merge-written into the caller's global factor set
    // (content-moderation-and-ranking.md § Composition). Stays optional so the
    // other call paths (compose/tests) need not pass an empty list.
    public async Task<string> CreateFeedAsync(
        string name, IReadOnlyList<FilterRuleInput> rules, string combination,
        string? scope = null, IReadOnlyList<string>? contributorSeeds = null,
        IReadOnlyList<FactorWeightInput>? factors = null)
        => await _manager.CreateFeed(
            name, rules.ToArray(), combination, scope, contributorSeeds?.ToArray(),
            (factors ?? Array.Empty<FactorWeightInput>()).ToArray());

    public async Task DeleteFeedAsync(string feedId) => await _manager.DeleteFeed(feedId);

    public async Task<long> SubscribeBridgeAsync(string kind, string uri, string name)
        => await _manager.SubscribeBridge(kind, uri, name);

    public async Task UnsubscribeBridgeAsync(long id) => await _manager.UnsubscribeBridge(id);

    /// <summary>Trigger resolution of a post's embedded quoted-post — from the loaded set
    /// with no fetch when possible, else one <c>fauna.posts.get</c>. The manager folds a
    /// <c>QuotedPost</c> block into the quoting <c>PostSummary.document</c> and re-emits
    /// **idempotently** (a no-op with no re-emit if the block is already folded), so the
    /// next snapshot render rebuilds the row and paints the quoted-post card from the
    /// document (render-model.md § D6). The returned <c>QuotedPostView</c> is the same shared
    /// projection (the fold's source); windows no longer reads it — it walks the document.</summary>
    public async Task<QuotedPostView?> ResolveQuotedPostAsync(string quotedPostId)
        => await _manager.ResolveQuotedPost(quotedPostId);

    /// <summary>Lazily resolve a <c>has_media</c> post's first blob hash; the
    /// manager writes it into the matching <c>PostSummary.media_hash</c> and
    /// notifies, so the next snapshot render paints the image.</summary>
    public async Task ResolveMediaAsync(string postId) => await _manager.ResolveMedia(postId);

    /// <summary>Trigger resolution of a post's folded <c>LinkPreview</c> embed (render-model.md
    /// § D4). For a block still in <c>Resolving</c>, the shared
    /// <c>FeedManager::resolve_link_preview</c> resolves the url once via
    /// <c>fauna.linkpreview.resolve</c> (cached), maps the reply onto
    /// <c>PreviewState::{Resolved,Failed}</c> and re-emits, so the next snapshot render paints
    /// the <c>link-preview-card</c> from the <c>Resolved</c> state (the text leg landed).
    /// A <c>Resolving</c>/<c>Failed</c> block paints no card — the kept inline body link shows
    /// (matches the web reference; no skeleton). Fire-once via state change: once terminal the
    /// block is no longer <c>Resolving</c>, so <see cref="FeedPage"/>'s gate won't re-fire (the
    /// cached, idempotent resolve is the in-flight backstop — same shape as
    /// <see cref="ResolveMediaAsync"/> / <see cref="ResolveQuotedPostAsync"/>).</summary>
    public async Task ResolveLinkPreviewAsync(string url) => await _manager.ResolveLinkPreview(url);

    /// <summary>Trigger resolution of a sold post's self-serve teaser purchase fields (gap
    /// (2c), monetization.md § Per-post pay-to-unlock → the buyer's price read is
    /// post-addressed). The manager folds a resolved offer into the matching
    /// <c>PostSummary.unlock_offer</c> and re-emits, so the next snapshot render paints
    /// <c>gated-post-price</c>/<c>gated-post-payment-link</c>/<c>gated-post-buy-button</c>.
    /// Fire-once via <see cref="FeedPostItem.HasUnlockOffer"/> false (mirrors
    /// <see cref="ResolveMediaAsync"/>) — an unresolved OR no-offer answer both leave
    /// <c>unlock_offer</c> null, but the manager caches the read either way so re-firing costs
    /// nothing (matches linux's identical trigger).</summary>
    public async Task ResolvePostUnlockOfferAsync(string postId) => await _manager.ResolvePostUnlockOffer(postId);

#if PAYMENTS
    /// <summary>Trigger resolution of a post's tip attribution (<c>monetization.md</c>
    /// § Tips). The manager folds a <c>TipView</c> into the matching
    /// <c>PostSummary.tips</c> and re-emits, so the next snapshot render paints
    /// <c>post-tip-total</c>/<c>post-tip-count</c>/<c>post-tip-list-button</c>.
    /// Fire-once via <see cref="FeedPostItem.HasTips"/>'s underlying <c>tips == null</c>
    /// check (mirrors <see cref="ResolveMediaAsync"/>) — there is no "does this post have
    /// tips" signal to gate on instead, so the resolve is called for every post and writes
    /// a view on every outcome (including "no tips" and a transport
    /// error), which is what makes the guard close.
    /// <see cref="FfiFeedManager.ResolvePostTips"/> itself is <c>#[cfg(feature = "payments")]</c>
    /// (the wire kind is excised entirely in a store-safe deployment), so the call site
    /// is gated too — the field it fills (<c>PostSummary.tips</c>) and the display it
    /// feeds are NOT gated (dynamic-features.md's "prose included" ruling covers only the
    /// element-id doc comments), so this is the one call this ViewModel needs to guard.</summary>
    public async Task ResolvePostTipsAsync(string postId) => await _manager.ResolvePostTips(postId);
#endif   // PAYMENTS

    /// <summary>Buy the sold post's unlock tier off the resolved teaser offer
    /// (<c>gated-post-buy-button</c>) — the existing subscribe flow against the resolved
    /// offer's tier, queued pending the author's own §2 approve (a client-minted unlock tier is
    /// never auto_approve). Returns <c>null</c> when the offer never resolved (nothing to buy);
    /// callers surface a thrown exception via <c>feed/error_buy_unlock</c>.</summary>
    public async Task<bool?> BuyUnlockOfferAsync(string postId) => await _manager.BuyUnlockOffer(postId);

    /// <summary>Dispatch <c>load-remote-content-button</c> to the manager (D3): flips the
    /// in-memory reveal set for <paramref name="postId"/> and re-emits; the next
    /// <c>Snapshot()</c> projects <c>RemoteImage.revealed=true</c> for that post
    /// (covers both the list card and the detail — they read the same snapshot post).
    /// Sync void — no <c>ConfigureAwait</c> (WinUI VM must not leave the UI thread).</summary>
    public void RevealRemoteImages(string postId) => _manager.RevealRemoteImages(postId);

    /// <summary>Like / reply / repost / quote — through the shared manager, not a
    /// raw <c>PostsInteractAsync</c>. The interaction bar's four counts render from
    /// the manager snapshot (<c>PostSummary.{like,reply,repost,quote}_count</c>,
    /// feed.md § Interaction bar) and the manager is what writes the nest's
    /// post-act counters back into it, so the tapped count moves at once.
    /// <para>This used to say it "mirrors linux <c>interact_with_post</c>" — which
    /// by then it did not: linux re-queried the whole feed afterwards to pick the
    /// new number up. A comment naming a sibling app's behaviour is a claim with no
    /// test behind it.</para></summary>
    public async Task InteractAsync(string postId, string action, string? body = null)
        => await _manager.Interact(postId, action, body);

    /// <summary>Like / un-like toggle (<c>feed-like-button</c>) — through
    /// <c>FfiFeedManager::like</c>, NOT <c>InteractAsync(id, "like")</c>: the nest's like
    /// arm is idempotent per (actor, post), so the one-way interact call can record a like
    /// but never take it back. Both directions ride the same door on the same post id and
    /// fold the nest's post-act counters into the snapshot either way (feed.md § Interaction
    /// bar → Repost, ratified 2026-08-10).</summary>
    public async Task LikeAsync(string postId) => await _manager.Like(postId);

    /// <summary>Repost / un-repost toggle (<c>feed-repost-button</c>) — through
    /// <c>FfiFeedManager::repost</c>, NOT <c>InteractAsync(id, "repost")</c>: absent
    /// <c>viewer_repost_id</c> composes the caller's empty-body <c>Reference::Repost</c>
    /// post; present un-reposts it. The raw one-way interact call creates nothing on a
    /// native post and can never be reversed (feed.md § Interaction bar → Repost, ratified
    /// 2026-08-10).</summary>
    public async Task RepostAsync(string postId) => await _manager.Repost(postId);

    /// <summary>Reply — composes a real reply post (<c>feed-reply-button</c> →
    /// <c>feed-reply-dialog</c>) through <c>FfiFeedManager::reply</c>, NOT
    /// <c>InteractAsync(id, "reply", body)</c>: the nest's native arm never composes a post
    /// from that door and discards <c>body</c> outright (feed.md § Interaction bar; §
    /// Implementation status today).</summary>
    public async Task ReplyAsync(string postId, string body) => await _manager.Reply(postId, body);

    /// <summary>Quote-repost with commentary (<c>feed-quote-button</c>) through
    /// <c>FfiFeedManager::quote</c>, NOT <c>InteractAsync(id, "quote", body)</c> — same
    /// discarding door as reply above (feed.md § Interaction bar; § Implementation status
    /// today). Fired with an empty body — the ratified direct-tap
    /// shape (matches tui/linux/web/android/macos/ios); a commentary composer is a separate
    /// fleet-wide follow-on.</summary>
    public async Task QuoteAsync(string postId, string body) => await _manager.Quote(postId, body);

    /// <summary>A rejected feed verb (like / reply / repost / quote) as <c>error-message</c>
    /// paints it. A <b>stated refusal</b> — words under a restricted post (<c>ui/feed.md</c>
    /// § Encryption at rest → <i>A reply, quote or repost of a restricted post</i>, ruling 6)
    /// — reads in the user's language, recognized by the one shared
    /// <c>feed_refusal_i18n_key</c>; every other failure keeps its own text. The twin of
    /// tui's <c>refusal_copy</c>, web's <c>verbErrorCopy</c> and linux's refused-reference
    /// arm.</summary>
    internal static string VerbErrorCopy(System.Exception ex)
    {
        var text = ex is FfiException.General general ? general.@msg : ex.Message;
        return FaunaFfiMethods.FeedRefusalI18nKey(text) is { } key
            ? Strings.Get(key.Replace('.', '/'))
            : text;
    }
}

/// <summary>
/// Public XAML-binding wrapper over the internal <see cref="PostSummary"/>
/// snapshot record — exposes only public-typed members so the <c>post-card</c>
/// DataTemplate's <c>x:DataType</c> resolves at runtime (an internal type makes
/// the WinUI ListView container generator skip row materialization, even though
/// x:Bind codegen succeeds — the same constraint as <c>ThreadRow</c>). Rebuilt
/// from the snapshot on every observer tick; the quoted-post fields fill
/// asynchronously (the page calls <c>resolve_quoted_post</c> per quoting row),
/// so this is an <see cref="ObservableObject"/> with OneWay-bound quote fields.
/// </summary>
public partial class FeedPostItem : ObservableObject
{
    public string PostId { get; }
    public string AuthorHex { get; }
    public string BodyText { get; }
    /// <summary>
    /// Set iff THIS post — never the quoted embed (see
    /// <see cref="QuotedPostIsLegalTakedown"/>) — was taken down under a legal
    /// obligation (moderation.md § Categories &amp; enforcement item 1;
    /// <c>PostSummary.legal_takedown_ref</c>). Only ever set on a post reached
    /// through the `resolve_post` deep-link door — `query_feed`'s feed-index
    /// projection omits a taken-down post from the timeline outright, so this
    /// is always <c>null</c> for an ordinary loaded list row. The exact twin of
    /// the quoted embed's and the DM bubble's own field — one concept, one
    /// shape, one shared string, on all three surfaces (priority #1/#3).
    /// </summary>
    public bool IsLegalTakedown { get; }
    /// <summary>The shared tombstone text — see <see cref="IsLegalTakedown"/>.
    /// Empty when not taken down.</summary>
    public string LegalTakedownDisplayBody { get; }
    /// <summary>Epoch-millis at the client boundary (the manager already divided
    /// the wire's micros by 1000 — do NOT divide again).</summary>
    public long Timestamp { get; }
    public bool HasMedia { get; }
    public bool IsReplyPost { get; }
    public string MediaHashHex { get; }

    /// <summary>The folded <c>Video</c> embed's content hash, or <c>""</c> when the post
    /// carries no video (render-model.md § Implementation status today — the D6b typed
    /// sibling of <see cref="MediaHashHex"/>). Drives the <c>video-thumbnail</c> play-glyph +
    /// hash text render — see <see cref="HasVideo"/>.</summary>
    public string VideoHashHex { get; }

    /// <summary>The nest-relative path of a bridged post's picture (its first folded
    /// <c>ProxiedImage</c>), or <c>""</c> when the post carries none or has a blob image —
    /// <see cref="MediaHashHex"/> wins the one <c>post-image</c> slot (render-model.md
    /// § D6c). The card fetches it from the user's own nest with the session bearer and
    /// shows a placeholder addressed by this path until the bytes land.</summary>
    public string MediaProxiedPath { get; }

    /// <summary>The nest-relative path of a bridged post's video (its first folded
    /// <c>ProxiedVideo</c>), or <c>""</c> when the post carries none or has a blob video
    /// (render-model.md § D6c → Proxied video). Painted as text only — never
    /// byte-loaded.</summary>
    public string VideoProxiedPath { get; }

    /// <summary>What <c>video-thumbnail</c> paints beside the play glyph: the content hash
    /// of a blob <c>Video</c>, else the path of a bridged <c>ProxiedVideo</c>, else
    /// <c>""</c>.</summary>
    public string VideoThumbnailText => VideoHashHex.Length > 0 ? VideoHashHex : VideoProxiedPath;

    /// <summary>Gate for <c>video-thumbnail</c>'s visibility — true iff the document folded a
    /// <c>Video</c> or a <c>ProxiedVideo</c> embed.</summary>
    public bool HasVideo => VideoThumbnailText.Length > 0;

    /// <summary>The post body as the shared semantic <c>RenderDocument</c>
    /// (<c>PostSummary.document</c>, produced once by the feed manager via
    /// <c>markdown_to_document</c> — render-model.md § D6). The list card + the detail
    /// dialog walk it through the SAME <see cref="FaunaApp.Helpers.DocumentPainter"/> the
    /// conversations bubble uses, instead of rendering the raw markdown string, so the
    /// body renders structurally (bold/italic/headings/links/blockquote) identically on
    /// every app (priority #1/#4). <c>internal</c> because the UniFFI-generated
    /// <c>RenderDocument</c> is emitted <c>internal</c>.</summary>
    internal uniffi.fauna_core.RenderDocument Document { get; }

    // ── Blocked-remote-image reveal (body markdown only — no persistence:
    //    html-mail.md § Rendering / Slice 3). The per-card load-remote-content-button is
    //    shown only while the body has ≥1 blocked remote image (manager-projected reveal
    //    state via RenderBlock.RemoteImage.revealed — render-model.md § D3). ──

    /// <summary>Gate for the per-card <c>load-remote-content-button</c>: the body carries
    /// ≥1 blocked remote image (manager projects revealed state onto each RemoteImage block
    /// — render-model.md § D3; read directly from the document at construction, immutable
    /// per snapshot since items are rebuilt on every observer tick).</summary>
    public bool HasBlockedRemoteImage { get; }

    public ObservableCollection<string> Tags { get; } = new();

    // Interaction-bar counts (icon + count, hidden at 0) read straight from the shared
    // snapshot's PostSummary.{like,reply,repost,quote}_count — real as of the nest
    // engagement-count backend (feed.md § Interaction bar, ratified 2026-06-27; the nest
    // increments via the live social-action paths). The buttons fire the action through
    // FeedViewModel.InteractAsync → FfiFeedManager.Interact, which folds the nest's
    // post-act counters back into that same snapshot, so a tapped count moves at once
    // (feed.md § User actions — interact IS a FeedManager method as of 2026-08-10). The count
    // TextBlock is hidden when 0 (FeedPage.CountToVisibility) — a clean icon-only button
    // until the post has activity, uniform across all seven apps.
    public long LikeCount { get; }
    public long ReplyCount { get; }
    public long RepostCount { get; }
    public long QuoteCount { get; }

    // Each interaction button's accessible Name: what the button paints, said in words —
    // the verb (the glyph itself carries no text) and the count only while it is shown,
    // so a post with no activity reads as a verb and no number (feed.md § Interaction
    // bar). The glyph and a one-digit count are single-character TextBlocks, which the
    // automation read passes over as icons, so without a Name the button reads back
    // empty. linux declares the same thing with `testid::set_test_text`.
    public string LikeButtonName => InteractionButtonName("feed/like_tooltip", LikeCount);
    public string ReplyButtonName => InteractionButtonName("common/reply", ReplyCount);
    public string RepostButtonName => InteractionButtonName("feed/post/repost", RepostCount);
    public string QuoteButtonName => InteractionButtonName("feed/quote", QuoteCount);

    /// <summary>An interaction button's Name — the localized verb, plus the count when
    /// the button shows one (<c>FeedPage.CountToVisibility</c>'s rule: above zero).</summary>
    internal static string InteractionButtonName(string verbKey, long count)
    {
        var verb = Strings.Get(verbKey);
        return count > 0 ? $"{verb} {count}" : verb;
    }

    /// <summary>The like button's toggle state (<c>PostSummary.viewer_liked</c>) — a lit
    /// <c>feed-like-button</c> means the next tap un-likes (feed.md § Interaction bar →
    /// Repost, ratified 2026-08-10). Drives <c>FeedPage.LikedToButtonStyle</c>.</summary>
    public bool ViewerLiked { get; }

    /// <summary>The <c>Reference::Repost</c> target this post reposts (hex id) — <c>Some</c>
    /// marks THIS card a REPOST ROW (<c>PostSummary.reposted_post_id</c>; feed.md
    /// § Interaction bar → Repost, ratified 2026-08-10). Drives <see cref="IsRepostRow"/>,
    /// which suppresses this card's own interaction bar and redirects
    /// <c>PostCard_Click</c> to the original's detail — a repost's own counters are
    /// structurally dark and its body is empty by construction.</summary>
    public string? RepostedPostId { get; }

    /// <summary>Gate for <c>repost-attribution</c> and for hiding the interaction bar
    /// (<see cref="RepostedPostId"/> is <c>Some</c>) — mirrors tui/linux/web's
    /// <c>is_repost_row</c>.</summary>
    public bool IsRepostRow => RepostedPostId is { Length: > 0 };

    /// <summary>The connection actor's own live repost of THIS row's post
    /// (<c>PostSummary.viewer_repost_id</c>) — presence = "reposted by me", and the next
    /// <c>feed-repost-button</c> tap un-reposts it (it is exactly <c>unrepost</c>'s
    /// argument). Drives <c>FeedPage.RepostedToButtonStyle</c>, the repost-toggle twin of
    /// <see cref="ViewerLiked"/>/<c>LikedToButtonStyle</c>, matching linux's <c>.reposted</c>
    /// CSS class and web's <c>viewer_reposted</c> lit state.</summary>
    public string? ViewerRepostId { get; }

    // ── Gate-to-tier (feed.md § Encryption at rest; monetization.md § Pillars 2+3).
    //    GatedTier is the tier name of an audience-restricted post (content_meta.gated_tier) —
    //    drives the gated-post-badge on the card; null = a public post. GatedUnlocked flips the
    //    detail from the public teaser (the list `body`) to the unsealed full body once the
    //    reader decrypts it (author custody / subscriber KeyBlob). Both are immutable
    //    projections read once at construction and both feed ContentEquals, so a tier badge
    //    appearing or an unlock landing repaints the row. ──
    public string? GatedTier { get; }
    public bool GatedUnlocked { get; }

    /// <summary>The reader's own label for the post's room (`PostSummary.room_label`) —
    /// `Some` exactly when this device still holds a seat on the room, derived fresh on
    /// every snapshot read against `own_rooms` (never taken from anything the author
    /// sent). `null` for a non-room gated post, or for a room post this reader is not on
    /// the floor of (ui/feed.md § Encryption at rest → Room-restricted — the card, (b)).</summary>
    public string? RoomLabel { get; }

    /// <summary>True iff this post is gated to a tier — shows the gated-post-badge.</summary>
    public bool HasGatedBadge => GatedTier is { Length: > 0 };

    /// <summary>The publish-state twin of <see cref="GatedTier"/>
    /// (<c>FeedPostItem.web_slug</c> ← the <c>content_links</c> web-publish row;
    /// <c>web-content-hosting.md</c> § Published-post management). <c>null</c> =
    /// not published. Drives the
    /// own-post web-publishing verbs on <c>feed-post-actions-menu</c>
    /// (<c>ui/feed.md</c> § User actions): <c>null</c> offers <i>Publish to
    /// web</i>, non-null offers <i>Unpublish</i> + <i>Copy web link</i>, and
    /// non-null together with <see cref="GatedTier"/> additionally offers
    /// <i>Copy paywall link</i>.</summary>
    public string? WebSlug { get; }

    /// <summary>The gated-post-badge's TEXT: the room's label for a member
    /// (<see cref="RoomLabel"/>) — the composer's own <c>feed/post/gate_room</c>
    /// ("Room: ‹label›") string — else the tier name, which for a room post any other
    /// reader sees is the reserved constant <c>room</c> (ruling 3's honest degrade).
    /// The badge TEXT is the only thing that differs between a member and everyone else
    /// (ui/feed.md § Encryption at rest → Room-restricted — the card, (c)); mirrors
    /// linux's <c>post_list.rs</c> badge/tooltip match on <c>room_label</c>.</summary>
    public string? GatedBadgeText => RoomLabel is { Length: > 0 } room
        ? Strings.Get("feed/post/gate_room").Replace("{room}", room)
        : GatedTier;

    /// <summary>Tooltip for the gated-post-badge — a member's own room label substituted
    /// into <c>feed/post/gated_badge_room_tooltip</c> ("Room members only: {room}"), else
    /// the tier substituted into the shared <c>feed/post/gated_badge_tooltip</c>
    /// ("Subscribers only: {tier}"), client-side (windows resw is flat).</summary>
    public string GatedBadgeTooltip => RoomLabel is { Length: > 0 } room
        ? Strings.Get("feed/post/gated_badge_room_tooltip").Replace("{room}", room)
        : GatedTier is { Length: > 0 } tier
            ? Strings.Get("feed/post/gated_badge_tooltip").Replace("{tier}", tier)
            : "";

    // ── Sold-post buyer teaser (gap (2c), monetization.md § Per-post pay-to-unlock → the
    //    buyer's price read is post-addressed) — the public purchase fields of a resolved
    //    fauna.subscriptions.post_unlock.get offer, folded by the shared
    //    FeedManager::resolve_post_unlock_offer into PostSummary.unlock_offer. null covers both
    //    "not yet resolved" and "the nest answered no offer" — both leave the priceless teaser,
    //    claim-code redemption (§5) staying the fallback purchase path. Immutable projections
    //    read once at construction, feeding ContentEquals so a resolved offer repaints the row. ──
    public bool HasUnlockOffer { get; }
    public string UnlockOfferPriceText { get; }
    public string? UnlockOfferPaymentUrl { get; }
    public bool HasUnlockPaymentLink => !string.IsNullOrEmpty(UnlockOfferPaymentUrl);

    // ── Tip display surface (monetization.md § Tips) — the post's tip attribution,
    //    folded by the shared FeedManager::resolve_post_tips into PostSummary.tips and
    //    re-emitted; null covers both "not yet resolved" and "no tips" (fire-once via
    //    HasTips false, mirrors HasUnlockOffer/tips == null). The two counters are
    //    guarded INDEPENDENTLY: HasTipTotal only when total_msats != 0 (tip_count can be
    //    non-zero with no summable amount — an unparseable receipt still counts but
    //    contributes no total; monetization.md § Tips forbids coercing that to "0 sats"),
    //    HasTips (drives post-tip-count + the list button) whenever tip_count != 0.
    //    TipSenders is the immutable attribution window for the post-tip-list dialog —
    //    every row the nest sent, unfiltered (trust is settled at ingest, never at read;
    //    § Zap receipts). Immutable projections read once at construction, feeding
    //    ContentEquals so a resolved tip view repaints the row (mirrors the unlock-offer
    //    fields above). ──
    public bool HasTips { get; }
    public bool HasTipTotal { get; }
    public string TipTotalText { get; }
    public string TipCountText { get; }
    public string TipListTitle { get; }
    public IReadOnlyList<TipSenderRow> TipSenders { get; }

    // ── Quoted-post embed — derived from the folded `QuotedPost` block in Document
    //    (render-model.md § D6), NOT the sibling quoted_post_id. The feed manager folds the
    //    block when resolve_quoted_post resolves the quote, then re-emits; SyncPosts rebuilds
    //    this item from the now-folded document, so these are immutable getters set once at
    //    construction (Document is the snapshot's immutable projection). The author is the
    //    shared ShortId form, matching the quoted-post card on every other app. ──
    public bool HasQuotedPost { get; }
    public string QuotedPostAuthor { get; }
    public string QuotedPostBody { get; }

    /// <summary>True iff the <b>quoted</b> post's own envelope verification failed
    /// (<c>RenderBlock.QuotedPost.verification == Failed</c>) — drives the muted
    /// <c>unverified-source-badge</c> on the quoted-post embed card (security.md
    /// § App display of unverified content, Slice 2b; the quoted block carries the
    /// quoted post's <c>QuotedPostView::verification</c>). Same uniform-but-decode-only
    /// firing as <see cref="IsUnverifiedSource"/> is for the post itself.</summary>
    public bool QuotedPostIsUnverified { get; }

    /// <summary>True iff the quoted post has been taken down under a legal obligation
    /// (<c>RenderBlock.QuotedPost.legalTakedownRef</c> set; moderation.md § Categories &amp;
    /// enforcement item 1) — the shared <c>resolve_quoted_post</c> withholds the body and
    /// projects a tombstone <c>QuotedPostView</c>. When set, the quoted-post card paints the
    /// shared tombstone (<see cref="QuotedPostDisplayBody"/>) in place of the (empty) body and
    /// omits the author + unverified-source header (<see cref="QuotedPostShowHeaderRow"/>).</summary>
    public bool QuotedPostIsLegalTakedown { get; }

    /// <summary>True iff the quoted post is gone — its author deleted it
    /// (<c>RenderBlock.QuotedPost.notFound</c>; <c>ui/feed.md</c> § Post deletion: a
    /// reference to a deleted post dangles by design, and the embed says so). Paints
    /// <c>feed.post.post_not_found</c> exactly where <see cref="QuotedPostIsLegalTakedown"/>
    /// paints its tombstone, with the header omitted the same way (no envelope).</summary>
    public bool QuotedPostIsNotFound { get; }

    /// <summary>The text the quoted-post card paints as its body: the shared localized
    /// legal-takedown tombstone (<c>legalTakedownTombstone(reference)</c> via the FFI face,
    /// resolved through <c>Strings</c> — never a hand-rolled string, priority #1/#2) when
    /// <see cref="QuotedPostIsLegalTakedown"/>, <c>feed.post.post_not_found</c> when
    /// <see cref="QuotedPostIsNotFound"/>, else the quoted body. One derivation so the
    /// list-card XAML and the detail-dialog paint the same text (mirrors web
    /// <c>QuotedPost.svelte</c>'s <c>legal_takedown_ref</c> arm + linux <c>document.rs</c>).</summary>
    public string QuotedPostDisplayBody { get; }

    /// <summary>Whether the quoted-post card shows its author + unverified-source header row:
    /// true for a normal quote, false for a legal-takedown tombstone or a not-found quote
    /// (there is no envelope —
    /// author is withheld/empty, so the header is omitted, matching every other app).</summary>
    public bool QuotedPostShowHeaderRow { get; }

    /// <summary>The quoted-post card's accessible <c>Name</c> — keeps the <c>quoted-post</c>
    /// Border UIA-discoverable (a bare AutomationId-only Border is pruned —
    /// reference_winui_flaui_datatemplate_name). The quoted author for a normal quote, the
    /// placeholder text for a takedown or a not-found quote (author is empty then, which
    /// would otherwise prune it).</summary>
    public string QuotedPostAccessibleName { get; }

    // ── Link-preview card — derived from the folded top-level `LinkPreview` block in
    //    Document (render-model.md § D4). The producer emits a `LinkPreview { Resolving }`
    //    for a standalone bare-URL paragraph (the inline link stays); the card's text is
    //    painted from the Resolved state. The Resolving→Resolved trigger is now wired
    //    (FeedPage.SyncPosts fires FeedViewModel.ResolveLinkPreviewAsync for a Resolving
    //    block → the shared FeedManager::resolve_link_preview flips it).
    //    Resolving/Failed still paint no card — the kept inline link shows the URL, matching
    //    the web reference (no skeleton). The og:image (`link-preview-image`) stays deferred:
    //    a cross-app gate-vs-direct divergence (tracked internally).
    //    PreviewState is UniFFI-internal, so these are public primitives. ──

    /// <summary>Every resolved <c>link-preview-card</c> this post paints, in body order
    /// (render-model.md § D4) — empty when the post folds no <c>LinkPreview</c>, or none has
    /// resolved yet (<c>Resolving</c>/<c>Failed</c> paint no card; the kept inline link already
    /// shows the URL, matching the web reference — no skeleton).
    /// <para>A LIST, not a single card: the producer emits a preview after **each** standalone
    /// bare-URL paragraph, so a two-URL body carries two cards. windows painted only the first
    /// until 2026-08-02 (the <c>FirstOrDefault</c> gap in render-model.md § Implementation
    /// status); the other six apps always iterated the full list.</para>
    /// Derived through the shared <see cref="Helpers.LinkPreviewCardModel"/> — the SAME
    /// derivation the conversations DM bubble uses (priority #2/#4), which carries the og:image
    /// D3 reveal gate: a card's <c>ImageHash</c> stays <c>""</c> until <c>ImageRevealed</c>, so
    /// the blob is never fetched before the post's <c>load-remote-content-button</c> is tapped
    /// (user-ratified 2026-06-27). The XAML <c>ItemsControl</c> binds this directly, so the card
    /// fields need no flat per-post mirrors on the item.</summary>
    public IReadOnlyList<Helpers.LinkPreviewCardModel> LinkPreviewCards { get; }

    /// <summary>Every link preview in the body with its state, in body order — cards and
    /// non-cards alike (<see cref="LinkPreviewStateRow"/>). Render-silent (a
    /// <c>resolving</c> and a <c>failed</c> preview both paint only the inline link) but
    /// part of <see cref="ContentEquals"/>, so the row — and the e2e dump that reads it —
    /// follows the preview to its terminal state.</summary>
    public IReadOnlyList<LinkPreviewStateRow> LinkPreviewStates { get; }

    /// <summary>Every <c>doc-remote-image</c> this post paints, in body order (render-model.md
    /// § D3 + § Implementation status; apps/tui.md § Rendering) — empty when the body folds no
    /// <c>RemoteImage</c> block. Derived through the shared <see cref="Helpers.RemoteImageCardModel"/>
    /// — the SAME derivation the conversations DM bubble uses (priority #2/#4) — via the shared
    /// <c>RenderDocument::remote_images</c> face rather than a local block-tree walk. The body
    /// <c>TextBlock</c>'s walker arm is inert for <c>RemoteImage</c> (<c>DocumentRenderer.Flatten</c>);
    /// the page paints this list as its own elements instead, just under the body. Reveal-gated
    /// by the SAME <c>load-remote-content-button</c> as <see cref="HasBlockedRemoteImage"/> and
    /// the link-preview og:image (one per-post reveal set).</summary>
    public IReadOnlyList<Helpers.RemoteImageCardModel> RemoteImages { get; }

    /// <summary>True iff this client's own envelope verification of the post
    /// <b>failed</b> (<c>VerificationStatus.Failed</c>) — drives the muted
    /// <c>unverified-source-badge</c> (security.md § Client display of unverified
    /// content). A feed-list card is the home nest's trusted index projection,
    /// which carries no signed envelope, so it is <c>Unchecked</c> (no badge) until
    /// the post is actually decoded — the badge wiring is uniform across surfaces
    /// but only fires where a raw envelope was verified and failed. The post still
    /// renders in full (a key-rotation-lag false-negative must not vanish a
    /// legitimate post).</summary>
    public bool IsUnverifiedSource { get; }

    /// <summary>True iff this post was written by an external app under the account's
    /// D10 authoring delegation (<c>AuthoringOriginStatus.Delegated</c>) — drives the
    /// <c>delegated-origin-badge</c> (atproto-pds-full.md &#167; Problem 1 &#8594; D10
    /// &#8594; <i>Audit</i>).
    ///
    /// <para><b>Badged iff <c>Delegated</c>, and that is a security rule, not a
    /// style.</b> <c>Unknown</c> deliberately covers verification-FAILED as well as
    /// never-decoded, and a "helpful" fallback that read origin off a failed envelope
    /// would invert the whole audit surface: it would let a forgery paint itself as
    /// "merely delegated". The bit is trustworthy in exactly the direction that
    /// matters — <c>signer_auth</c> rides OUTSIDE the signed bytes, so stripping it
    /// from a delegated value makes verification FAIL rather than read as direct.</para></summary>
    public bool IsDelegatedOrigin { get; }

    /// <summary>The same D10 audit marker for the QUOTED embed — its own origin, not
    /// the quoting post's, exactly as <see cref="QuotedPostIsUnverified"/> is its own
    /// verification.</summary>
    public bool QuotedPostIsDelegatedOrigin { get; }

    /// <summary>Does this post match one of the user's muted words
    /// (<c>FeedManager::is_muted</c>, topic-factors.md § Scoring)? A per-post
    /// manager query read once at construction (the sealed scorers are loaded on
    /// each real fetch) — drives the collapse-to-placeholder render treatment,
    /// which applies everywhere (chronological feeds included; a mute cannot
    /// SINK a post outside score order, but it always collapses).</summary>
    public bool IsMuted { get; }

    private bool _revealed;

    /// <summary>Session-local one-tap reveal for a muted post (mirrors
    /// <c>dm-message-muted</c>'s reveal). Mutated in place on THIS instance by
    /// <see cref="FeedPage"/>'s reveal handler — <b>not</b> part of
    /// <see cref="ContentEquals"/>, so an unrelated observer-tick rebuild (a
    /// sibling post's media resolving, say) can never reset an already-revealed
    /// post back to collapsed.</summary>
    public bool Revealed
    {
        get => _revealed;
        set
        {
            if (_revealed == value) return;
            _revealed = value;
            OnPropertyChanged();
            NotifyRenderArmChanged();
        }
    }

    // ── Content-policy render enforcement (family-safety.md § Content policy) ──
    //
    // The feed read model is one of the TWO surfaces the policy binds at (the
    // conversation bubble is the other); both resolve through the SAME
    // ContentPolicyCache verdict and the SAME SocialRenderGate ordering, so a
    // floor can never be enforced differently on a post than on a message.

    /// <summary>The shared content-policy verdict for this post's
    /// <c>PostSummary.labels</c> — <c>"show" | "badge" | "collapse" | "block"</c>,
    /// composed entirely in shared Rust (<c>ContentRenderVerdict</c>) from the
    /// guardian floor ∪ the viewer's own thresholds. Read ONCE at construction,
    /// exactly like <see cref="IsMuted"/>: items are rebuilt from the snapshot on
    /// every observer tick, and the verdict is part of <see cref="ContentEquals"/>,
    /// so a policy that hydrates after the first paint rebuilds the affected rows
    /// on the next tick.</summary>
    public string ContentVerdict { get; } = "show";

    /// <summary>Whether this post's content-policy <c>collapse</c> has been
    /// revealed ("show anyway") this session. Keyed on
    /// <see cref="ContentPolicyCache"/>'s session set by <see cref="PostId"/>
    /// rather than a per-instance bool like <see cref="Revealed"/>: an observer
    /// tick rebuilds a changed post into a BRAND-NEW <c>FeedPostItem</c>, which
    /// would silently reset a per-instance flag back to collapsed — the
    /// cache-keyed set survives the rebuild, which is strictly better (the muted
    /// arm gets away with a per-instance flag only because <see cref="Revealed"/>
    /// is excluded from <see cref="ContentEquals"/>, so its instance usually
    /// survives).</summary>
    public bool ContentRevealed => ContentPolicyCache.IsRevealed(PostId);

    /// <summary>Which arm this card paints — the ONE ordering decision, shared
    /// with the conversation bubble (<see cref="SocialRenderGate.Decide"/>): a
    /// <c>block</c> wins over both the muted collapse and the content collapse,
    /// and is never revealable.</summary>
    public SocialRenderArm RenderArm =>
        SocialRenderGate.Decide(ContentVerdict, ContentRevealed, IsMuted && !Revealed,
            regionVerb: Region?.Verb);

    /// <summary>The region placeholder when the region content policy drove this
    /// post's verdict (region-blocking.md § The blocked render), else <c>null</c>.
    /// Part of <see cref="ContentEquals"/>, like <see cref="ContentVerdict"/>.</summary>
    public RegionPlaceholderModel? Region { get; }

    /// <summary>Gate for the region placeholder (<c>region-blocked-notice</c> +
    /// authority + reason) painted in place of the body.</summary>
    public bool ShowRegionPlaceholder => RenderArm == SocialRenderArm.RegionWithheld;

    /// <summary>Gate for the <c>region-collapsed-reveal-button</c>: a region
    /// <c>collapse</c> is one reveal away; a <c>block</c> has none.</summary>
    public bool ShowRegionReveal => ShowRegionPlaceholder && Region is { IsBlock: false };

    /// <summary>Convention 17's verdict side: the region BLOCKS this post.</summary>
    public bool IsRegionBlocked => Region is { IsBlock: true };

    public string RegionNoticeText => Region?.NoticeText ?? "";

    /// The region frame while the region arm withholds the body, else empty — the
    /// post-card's accessible Name must never carry the body it hides.
    public string RegionAccessibleNotice => ShowRegionPlaceholder ? RegionNoticeText : "";
    public string RegionAuthority => Region?.AuthorityName ?? "";
    public string RegionReason => Region?.Reason ?? "";

    /// <summary>Gate for the <c>content-policy-blocked-notice</c> painted in place
    /// of the body (family-safety.md § Content policy — a blocked item names the
    /// policy, it never silently disappears). No reveal affordance accompanies it.</summary>
    public bool ShowContentBlockedNotice => RenderArm == SocialRenderArm.ContentBlocked;

    /// <summary>Gate for the muted-keyword placeholder + its reveal button.</summary>
    public bool ShowMutedCollapse => RenderArm == SocialRenderArm.Muted;

    /// <summary>Gate for the content-policy collapsed placeholder + its reveal
    /// button (presentation-only — no ui.yaml id, matching the linux leg).</summary>
    public bool ShowContentCollapse => RenderArm == SocialRenderArm.ContentCollapsed;

    /// <summary>Gate for the full card body — true iff no arm withholds it.</summary>
    public bool ShowNormalContent => RenderArm == SocialRenderArm.Live;

    /// <summary>Reveal this post's content-policy <c>collapse</c> for the rest of
    /// the session (the floor itself persists — only the guardian relaxing it
    /// stops future collapse). A no-op for a <c>block</c>: the gate checks block
    /// ahead of the reveal set, so the button never exists on a blocked card.</summary>
    public void RevealContent()
    {
        ContentPolicyCache.Reveal(PostId);
        NotifyRenderArmChanged();
    }

    /// <summary>Re-raise the derived render gates so x:Bind repaints this row in
    /// place after a session-local reveal (either kind).</summary>
    private void NotifyRenderArmChanged()
    {
        OnPropertyChanged(nameof(ContentRevealed));
        OnPropertyChanged(nameof(RenderArm));
        OnPropertyChanged(nameof(ShowRegionPlaceholder));
        OnPropertyChanged(nameof(ShowRegionReveal));
        OnPropertyChanged(nameof(ShowContentBlockedNotice));
        OnPropertyChanged(nameof(ShowMutedCollapse));
        OnPropertyChanged(nameof(ShowContentCollapse));
        OnPropertyChanged(nameof(ShowNormalContent));
    }

    /// <summary>Gate for the <c>content-label-badge</c>: this post carries at least one
    /// content-label entry (moderation.md § Per-row badge data path). The highest-
    /// confidence entry is picked by the shared <c>primary_content_label</c> — one
    /// decision so the feed post-card, the DM bubble, and the moderation queue all agree
    /// on which of several <c>PostSummary.labels</c> wins the one visible badge (no
    /// client re-derives the "which category wins" reduce).</summary>
    public bool HasContentLabel { get; }

    /// <summary>The badge icon glyph (<c>content_label_style(category).icon</c>);
    /// <c>""</c> when <see cref="HasContentLabel"/> is false.</summary>
    public string ContentLabelIcon { get; } = "";

    /// <summary>The localized category label text; <c>""</c> when
    /// <see cref="HasContentLabel"/> is false.</summary>
    public string ContentLabelText { get; } = "";

    /// <summary>The badge background-tint hex (<c>#RRGGBB</c>) — windows applies its own
    /// low-alpha treatment at render time (<c>HexColorToBrushConverter</c> ConverterParameter),
    /// mirroring web's <c>rgba(.., 0.15)</c> / android's <c>copy(alpha=.15)</c> per-app
    /// convention (<c>fauna_core::content_category</c> § tint doc comment). <c>""</c> when
    /// <see cref="HasContentLabel"/> is false.</summary>
    public string ContentLabelTint { get; } = "";

    /// <summary>The higher-contrast accent hex (<c>#RRGGBB</c>) for the badge text/icon;
    /// <c>""</c> when <see cref="HasContentLabel"/> is false.</summary>
    public string ContentLabelAccent { get; } = "";

    /// <summary>Localized label for the unverified-source badge — one shared i18n
    /// key (<c>feed.unverified_source</c>) across all apps.</summary>
    public string UnverifiedSourceLabel => Strings.Get("feed/unverified_source");

    /// <summary>Localized tooltip explaining the unverified-source caveat — so the
    /// user sees both the content and why it is flagged (security.md § Client
    /// display of unverified content).</summary>
    public string UnverifiedSourceTooltip => Strings.Get("feed/unverified_source_tooltip");

    /// <summary>The <c>delegated-origin-badge</c>'s label ("Via connected app") — the
    /// shared i18n string every app paints, never a per-app phrasing.</summary>
    public string DelegatedOriginLabel => Strings.Get("feed/delegated_origin");

    /// <summary>The badge's tooltip: what it means, and where the access is managed.</summary>
    public string DelegatedOriginTooltip => Strings.Get("feed/delegated_origin_tooltip");

    /// <summary>
    /// C2PA content-provenance badge (media.md § C2PA provenance). Unlike every other
    /// field on this class, this is NOT read from the snapshot at construction — there
    /// is no manager-side signal for it (media.md's own per-app-render-glue pattern:
    /// windows checks it the same way android/apple do, a client-side header read, not
    /// a shared-Rust door). Starts <c>false</c>; <c>FeedPage.SyncPosts</c> flips it once
    /// the list-card image's own authenticated blob GET resolves the `x-c2pa` header
    /// (<c>BlobImageLoader.LoadWithC2paAsync</c>, cache-shared with the plain
    /// <c>LoadAsync</c> the image binding already calls — no extra request). Deliberately
    /// EXCLUDED from <see cref="ContentEquals"/>: it is page-side async state, not
    /// snapshot content, and <see cref="ObservableCollectionReconcile"/> keeping the
    /// existing instance across an unrelated content-equal tick is exactly what lets the
    /// fire-once check below survive un-repeated.
    /// </summary>
    public bool HasC2pa { get; private set; }

    /// <summary>Fire-once guard for the <see cref="HasC2pa"/> check — there is no
    /// snapshot-side "resolved" signal to gate on (unlike <c>mediaHash</c>/`quotedPostId`
    /// above), so the page tracks it here instead.</summary>
    internal bool C2paChecked { get; private set; }

    /// <summary>Records the result of the async C2PA check (see <see cref="HasC2pa"/>).
    /// Idempotent: a repeat call with the same value marks checked without re-raising
    /// <see cref="INotifyPropertyChanged"/>.</summary>
    internal void SetHasC2pa(bool value)
    {
        C2paChecked = true;
        if (HasC2pa == value) return;
        HasC2pa = value;
        OnPropertyChanged(nameof(HasC2pa));
    }

    /// <summary>The <c>c2pa-badge</c> label — the SAME shared i18n string linux's list-card
    /// badge and windows' own conversations <c>DmMessageBubble</c> badge use, never a
    /// per-app phrasing.</summary>
    public string C2paLabel => Strings.Get("c2pa/badge_label");

    /// <summary>The badge's tooltip. Reuses the conversations badge's tooltip string —
    /// same concept (uploader-asserted C2PA content credentials present), same text,
    /// mirroring linux's identical reuse for its own list-card badge.</summary>
    public string C2paTooltip => Strings.Get("conversations/detail/badge_c2pa");

    internal FeedPostItem(PostSummary p, FfiFeedManager? manager = null)
    {
        // Read once at construction — a manager query over the loaded sealed
        // scorers (topic-factors.md § Scoring), not a PostSummary field. `null`
        // (the badge-only ForSourceTest helper) means never muted.
        IsMuted = manager?.IsMuted(p.postId) ?? false;
        PostId = p.postId;
        AuthorHex = p.author;
        BodyText = p.body;
        // Twin of the quoted embed's own legal-takedown paint below (see
        // QuotedPostIsLegalTakedown) — the shared reference resolves through
        // the SAME FaunaFfiMethods.LegalTakedownTombstone i18n face.
        IsLegalTakedown = p.legalTakedownRef is { Length: > 0 };
        LegalTakedownDisplayBody = IsLegalTakedown
            ? Strings.Resolve(FaunaFfiMethods.LegalTakedownTombstone(p.legalTakedownRef!))
            : "";
        Timestamp = p.timestamp;
        HasMedia = p.hasMedia;
        IsReplyPost = p.isReply;
        LikeCount = p.likeCount;
        ReplyCount = p.replyCount;
        RepostCount = p.repostCount;
        QuoteCount = p.quoteCount;
        ViewerLiked = p.viewerLiked;
        RepostedPostId = p.repostedPostId;
        ViewerRepostId = p.viewerRepostId;
        GatedTier = p.gatedTier;
        RoomLabel = p.roomLabel;
        WebSlug = p.webSlug;
        GatedUnlocked = p.gatedUnlocked;
        HasUnlockOffer = p.unlockOffer is not null;
        UnlockOfferPriceText = p.unlockOffer?.priceHint ?? "";
        UnlockOfferPaymentUrl = p.unlockOffer?.paymentUrl;
        // Tip display surface (monetization.md § Tips): guarded independently, per the
        // class-level doc above. HasTips gates on tip_count != 0 (never on total_msats,
        // which can legitimately be 0 while real tips exist), matching tui's
        // tip_elements gate. senders is a bounded window; the title carries the "and N
        // more" tail from the nest's own has_more flag, never a length comparison
        // inferred here (mirrors tui's tip_list_elements).
        var tips = p.tips;
        HasTips = tips is not null && tips.tipCount != 0;
        HasTipTotal = HasTips && tips!.totalMsats != 0;
        TipTotalText = HasTipTotal ? Strings.Resolve(FaunaFfiMethods.TipAmount(tips!.totalMsats)) : "";
        TipCountText = HasTips ? Strings.Resolve(FaunaFfiMethods.TipCount(tips!.tipCount)) : "";
        TipListTitle = HasTips && tips!.hasMore
            ? $"{Strings.Get("tips/list_title")} — {Strings.Resolve(FaunaFfiMethods.TipMore(tips.tipCount - tips.senders.Length))}"
            : Strings.Get("tips/list_title");
        TipSenders = HasTips
            ? tips!.senders.Select(s =>
              {
                  // Who: the local actor when the mechanism identity resolved to one,
                  // else the mechanism-native id it published, else the localized
                  // stand-in — an outside tip still counts and still displays.
                  var who = s.sender is { Length: > 0 } sender ? sender
                      : s.senderRef is { Length: > 0 } senderRef ? senderRef
                      : Strings.Get("tips/sender_unknown");
                  // How much, or the honest absence. NEVER "0 sats".
                  var amount = s.amountMsats is { } msats
                      ? Strings.Resolve(FaunaFfiMethods.TipAmount(msats))
                      : Strings.Get("tips/amount_unknown");
                  return new TipSenderRow($"{who} — {amount}");
              })
              .ToList()
            : Array.Empty<TipSenderRow>();
        Document = p.document;
        // Quoted-post + media are derived from the folded `QuotedPost` / `Image` blocks the
        // feed manager folds into the document (render-model.md § D6 embed-fold), NOT the
        // sibling quoted_post_id / media_hash fields, so windows paints from the same shared
        // projection as linux, web, and android (priority #1/#4). Both extractors now delegate
        // to the shared UniFFI faces rather than re-walking the block tree here.
        MediaHashHex = FaunaApp.Core.Helpers.DocumentRenderer.MediaImageHash(p.document) ?? "";
        VideoHashHex = FaunaApp.Core.Helpers.DocumentRenderer.MediaVideoHash(p.document) ?? "";
        // A bridged post's media folds in as ProxiedImage / ProxiedVideo — a nest-relative
        // path instead of a content hash (render-model.md § D6c).
        MediaProxiedPath = FaunaApp.Core.Helpers.DocumentRenderer.MediaProxiedPath(p.document) ?? "";
        VideoProxiedPath = FaunaApp.Core.Helpers.DocumentRenderer.MediaProxiedVideoPath(p.document) ?? "";
        var quote = FaunaApp.Core.Helpers.DocumentRenderer.QuotedPost(p.document);
        HasQuotedPost = quote is not null;
        QuotedPostAuthor = quote is not null ? FaunaFfiMethods.ShortId(quote.author) : "";
        QuotedPostBody = quote?.body ?? "";
        // Slice 2b: the quoted embed's own verification, folded from the quoted post's
        // QuotedPostView::verification into the RenderBlock.QuotedPost carrier — drives the
        // unverified-source-badge on the quoted card (security.md § Client display of
        // unverified content). Failed only ever arrives on a decode path, never the list.
        QuotedPostIsUnverified = quote is not null
            && quote.verification == uniffi.fauna_core.VerificationStatus.Failed;
        // The quoted embed's OWN D10 audit marker. `Delegated` only — `Unknown`
        // covers verification-FAILED, and reading origin off an envelope that did not
        // verify is exactly the inversion the audit surface must not make.
        QuotedPostIsDelegatedOrigin = quote is not null
            && quote.authoringOrigin == uniffi.fauna_core.AuthoringOriginStatus.Delegated;
        // Legal-takedown quoted post (moderation.md § Categories & enforcement item 1): the
        // shared resolve_quoted_post withheld the quoted body under a legal obligation and
        // projected a tombstone QuotedPostView (empty body/author) carrying legalTakedownRef.
        // Paint the shared localized tombstone in place of the (empty) body + omit the
        // author/unverified header — mirroring web QuotedPost.svelte's `legal_takedown_ref`
        // arm + linux document.rs's build_quoted_post_card branch. The reference resolves
        // through the shared FaunaFfiMethods.LegalTakedownTombstone i18n face, never a
        // per-app string (priority #1/#2).
        QuotedPostIsLegalTakedown = quote?.legalTakedownRef is { Length: > 0 };
        // A quote of a post its author deleted (ui/feed.md § Post deletion) — the
        // shared fold says `not_found`, and the card paints one line in place of
        // author + body, as it does for the tombstone above (linux document.rs's
        // placeholder, web QuotedPost.svelte's not_found arm).
        QuotedPostIsNotFound = !QuotedPostIsLegalTakedown && quote is { notFound: true };
        QuotedPostDisplayBody = QuotedPostIsLegalTakedown
            ? Strings.Resolve(FaunaFfiMethods.LegalTakedownTombstone(quote!.legalTakedownRef!))
            : QuotedPostIsNotFound
                ? Strings.Get("feed/post/post_not_found")
                : QuotedPostBody;
        var quotedPlaceholder = QuotedPostIsLegalTakedown || QuotedPostIsNotFound;
        QuotedPostShowHeaderRow = HasQuotedPost && !quotedPlaceholder;
        QuotedPostAccessibleName = quotedPlaceholder ? QuotedPostDisplayBody : QuotedPostAuthor;
        // Link-preview card text + reveal-gated og:image, single-sourced through the shared
        // LinkPreviewCardModel so the feed post card and the conversations DM bubble paint the
        // SAME card from ONE derivation (render-model.md § D4; priority #2/#4 — no per-surface
        // copy that can drift). Only Resolved paints a card; Resolving/Failed leave the kept
        // inline link (no skeleton, matching web). Domain via the shared url_host face; the
        // og:image hash is withheld until the post's remote content is revealed (the D3 posture).
        LinkPreviewCards = FaunaApp.Core.Helpers.LinkPreviewCardModel.All(p.document);
        // Every preview's state, card or not (the shared RenderDocument::link_previews).
        LinkPreviewStates = FaunaFfiMethods.RenderDocumentLinkPreviews(p.document)
            .Select(lp => new LinkPreviewStateRow(lp.@url, lp.@state))
            .ToList();
        // Walked once at construction (the document is immutable on the snapshot) so the
        // load-remote-content-button gate is a cheap property read on every Refresh tick.
        // D3: reads the manager-projected reveal state from each RemoteImage.revealed field.
        HasBlockedRemoteImage = FaunaApp.Core.Helpers.DocumentRenderer.HasBlockedRemoteImage(p.document);
        // Body remote images (render-model.md § D3 + § Implementation status): the SAME
        // shared-face derivation as LinkPreviewCards above, single-sourced so the feed and the
        // DM bubble paint identically (priority #2/#4).
        RemoteImages = FaunaApp.Core.Helpers.RemoteImageCardModel.All(p.document);
        foreach (var tag in p.tags)
            Tags.Add(tag);
        SourceBadges = FaunaFfiMethods.ClassifySources(p.source)
            .Select(b => new PostSourceBadge(
                b.id, b.label, FaunaApp.Core.Helpers.SourceGlyphAsset.Emoji(b.glyph)))
            .ToList();
        // Walked once at construction (the snapshot is immutable). Drives the muted
        // unverified-source-badge; Failed only ever arrives on a decode path, never
        // the feed-list projection (security.md § App display of unverified content).
        IsUnverifiedSource = p.verification == uniffi.fauna_core.VerificationStatus.Failed;
        // The D10 audit marker: an external app wrote this post as the account.
        // Trails the unverified badge in the same badge row, the order every app
        // paints these two in. Strictly `Delegated` — see IsDelegatedOrigin's own doc
        // for why a fallback here would be a security defect rather than a nicety.
        IsDelegatedOrigin = p.authoringOrigin == uniffi.fauna_core.AuthoringOriginStatus.Delegated;
        // Content-label badge (moderation.md § Per-row badge data path): pick the
        // highest-confidence entry via the shared primary_content_label, then resolve its
        // presentation via the shared content_label_style — the SAME two calls the
        // moderation queue already makes (ModerationViewModel.MapRow), so a post carries
        // an identical badge wherever it renders.
        if (FaunaFfiMethods.PrimaryContentLabel(p.labels) is { } primaryLabel)
        {
            var style = FaunaFfiMethods.ContentLabelStyle(primaryLabel.category);
            HasContentLabel = true;
            ContentLabelIcon = style.icon;
            ContentLabelText = Strings.Resolve(style.label);
            ContentLabelTint = style.tint;
            ContentLabelAccent = style.accent;
        }
        // Content-policy render enforcement (family-safety.md § Content policy) over
        // the SAME labels the badge above is styled from: the guardian's per-category
        // floor composed with the viewer's own spam/phishing thresholds, resolved
        // strictest-wins ENTIRELY in shared Rust (no rule assembly in C#). Read once
        // at construction like IsMuted; an all-absent cache short-circuits to "show"
        // without touching the FFI, so an unsupervised viewer pays nothing.
        //
        // The region content policy composes in as the third source
        // (region-blocking.md § Where it composes): one call, one verdict, and — when
        // the region drove a block/collapse — its placeholder, painted ahead of the
        // family arm.
        var decision = ContentPolicyCache.RenderFor(p.labels, RegionSubject.Post(p));
        ContentVerdict = decision.Verdict;
        Region = decision.Placeholder;
        // Guardian Notify (family-safety.md § Guardian Notify): count any
        // GUARDIAN-floor enforcement on this item — never the own-threshold
        // collapse above, which is a different lens. A no-op unless the ward's
        // content_notify knob is on (GuardianNotifyCache's own gate).
        var guardianEnforcedCategories = FaunaFfiMethods.GuardianEnforcedCategories(
            p.labels, ContentPolicyCache.Current.ContentPolicy);
        if (guardianEnforcedCategories.Length > 0)
            GuardianNotifyCache.Record(p.postId, guardianEnforcedCategories);
    }

    /// <summary>Test-only convenience: build a wrapper from just a source string to
    /// lock the shared <c>classify_sources</c> badge ids + labels without minting a
    /// full <see cref="PostSummary"/> by hand — every other field is a
    /// <see cref="PostSummaryFixture"/> inert default (badge-only, no body/media/
    /// gating/label data).</summary>
    internal static FeedPostItem ForSourceTest(string source) =>
        new FeedPostItem(PostSummaryFixture.Make(postId: "", author: "", source: source));

    /// <summary>
    /// Whether <paramref name="other"/> would render this card identically — the
    /// <see cref="FaunaApp.Core.Helpers.ObservableCollectionReconcile"/> key that lets
    /// <c>FeedPage.SyncPosts</c> keep an <b>unchanged</b> post's row (and its in-flight
    /// image-blob load) in place across observer ticks, rebuilding only a post whose rendered
    /// content actually changed (media/quote/link-preview folded into the document, counts
    /// bumped, body edited, a remote image revealed). Compares every field the post-card binds;
    /// the folded-document mutations all surface here as scalar fields (render-model.md § D6),
    /// so a scalar comparison is exact without deep-walking the <c>RenderDocument</c> tree. This
    /// MUST list every render-bound field — FeedPostItemTests guards the load-bearing cases.
    /// </summary>
    public bool ContentEquals(FeedPostItem other)
    {
        if (other is null) return false;
        return PostId == other.PostId
            && AuthorHex == other.AuthorHex
            && BodyText == other.BodyText
            && Timestamp == other.Timestamp
            && HasMedia == other.HasMedia
            && IsReplyPost == other.IsReplyPost
            && MediaHashHex == other.MediaHashHex
            && VideoHashHex == other.VideoHashHex
            && MediaProxiedPath == other.MediaProxiedPath
            && VideoProxiedPath == other.VideoProxiedPath
            && HasBlockedRemoteImage == other.HasBlockedRemoteImage
            && LikeCount == other.LikeCount
            && ReplyCount == other.ReplyCount
            && RepostCount == other.RepostCount
            && QuoteCount == other.QuoteCount
            && ViewerLiked == other.ViewerLiked
            && RepostedPostId == other.RepostedPostId
            && ViewerRepostId == other.ViewerRepostId
            && GatedTier == other.GatedTier
            && RoomLabel == other.RoomLabel
            && WebSlug == other.WebSlug
            && GatedUnlocked == other.GatedUnlocked
            && HasUnlockOffer == other.HasUnlockOffer
            && UnlockOfferPriceText == other.UnlockOfferPriceText
            && UnlockOfferPaymentUrl == other.UnlockOfferPaymentUrl
            && HasTips == other.HasTips
            && HasTipTotal == other.HasTipTotal
            && TipTotalText == other.TipTotalText
            && TipCountText == other.TipCountText
            && TipListTitle == other.TipListTitle
            && TipSenders.SequenceEqual(other.TipSenders)
            && HasQuotedPost == other.HasQuotedPost
            && QuotedPostAuthor == other.QuotedPostAuthor
            && QuotedPostBody == other.QuotedPostBody
            && QuotedPostIsUnverified == other.QuotedPostIsUnverified
            && QuotedPostIsDelegatedOrigin == other.QuotedPostIsDelegatedOrigin
            && QuotedPostIsLegalTakedown == other.QuotedPostIsLegalTakedown
            && QuotedPostIsNotFound == other.QuotedPostIsNotFound
            && QuotedPostDisplayBody == other.QuotedPostDisplayBody
            // Every link-preview card, in order: LinkPreviewCardModel is a record, so
            // SequenceEqual compares all seven painted fields structurally — which keeps this
            // check complete as cards are added/resolved/revealed, and (unlike the eight flat
            // comparisons it replaces) notices a change in the SECOND card of a two-URL post.
            && LinkPreviewCards.SequenceEqual(other.LinkPreviewCards)
            // ...and every preview's STATE: `resolving` and `failed` paint the same (no
            // card), so the cards alone would keep a stale row — and the e2e dump, which
            // reads the row, would say `resolving` for ever after the preview failed.
            && LinkPreviewStates.SequenceEqual(other.LinkPreviewStates)
            // Every doc-remote-image, in order: RemoteImageCardModel is a record, so
            // SequenceEqual compares all three painted fields structurally — a reveal (Revealed
            // flips true) must repaint the row, exactly like LinkPreviewCards above.
            && RemoteImages.SequenceEqual(other.RemoteImages)
            && IsUnverifiedSource == other.IsUnverifiedSource
            && IsDelegatedOrigin == other.IsDelegatedOrigin
            && IsMuted == other.IsMuted
            // The content-policy verdict IS render-bound content: it changes when the
            // post's labels change AND when the policy itself hydrates after the first
            // paint (the two ContentPolicyCache reads land off MainPage.Page_Loaded,
            // possibly after the feed's first tick). Including it here is what makes the
            // next observer tick rebuild — and actually enforce on — an already-rendered
            // row. ContentRevealed is NOT here, for the same reason Revealed isn't: it's
            // session-local reveal state, not snapshot content.
            && ContentVerdict == other.ContentVerdict
            // ...and the region placeholder beside it (a record: verb, region,
            // authority, reason) — a newer document can keep the verb and change
            // the reason, which is render-bound content too.
            && Region == other.Region
            // Revealed is deliberately EXCLUDED: it's session-local UI state the
            // reveal handler mutates in place on the surviving instance, not
            // render-bound snapshot content — including it here would make every
            // reveal look like a content change and defeat the point of Reconcile
            // keeping the same instance.
            && Tags.SequenceEqual(other.Tags)
            && SourceBadges.SequenceEqual(other.SourceBadges);
    }

    // Shared short-id form (first 12 + …); never the old ASCII "...".
    // (docs/goal/behavior/value-formatting.md § Short id; priority #1/#4).
    public string ShortAuthor => FaunaFfiMethods.ShortId(AuthorHex);

    // The timestamp is already epoch-MILLIS (the manager scaled the wire micros);
    // the bucket decision + localized strings live in shared Rust via
    // ValueFormat.RelativeTime.
    public string TimeAgo => ValueFormat.RelativeTime(
        System.DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), Timestamp);

    // Post-source badges, classified by the shared fauna-feed classifier via the
    // fauna-ffi `classify_sources` façade — one badge per origin for a
    // comma-separated `source` field, deduplicated, in first-appearance order,
    // with canonical labels (activitypub → "Fediverse"). Windows keeps ONLY its
    // platform-specific maps: the SourceToColorConverter (color, keyed off the
    // stable Id) and the SourceGlyphAsset emoji map (keyed off the shared
    // SourceGlyph `glyph` concept — render-model.md § D5; the SAME map the
    // conversations rail uses, so badge and rail can't drift). The label text is
    // the shared canonical label (feed.md § Where logic lives).
    public IReadOnlyList<PostSourceBadge> SourceBadges { get; }
}

/// One classified post-source badge for binding: a stable lowercase id
/// (<c>"fauna" | "bluesky" | "nostr" | "activitypub" | "email" | "other"</c>)
/// keying windows's platform color map, the canonical user-facing label, and the
/// resolved <c>Glyph</c> emoji — all from the shared <c>fauna_feed</c> classifier
/// (the fauna-ffi <c>FfiSourceBadge { id, label, glyph }</c> façade; <c>Glyph</c>
/// is <c>SourceGlyphAsset.Emoji(badge.glyph)</c>, the SAME <c>SourceGlyph → emoji</c>
/// map the conversations rail uses — render-model.md § D5). A windows-local public
/// type so the XAML can <c>x:Bind</c> it (the generated <c>FfiSourceBadge</c> is
/// <c>internal</c> to FaunaApp.Core).
public sealed record PostSourceBadge(string Id, string Label, string Glyph);

/// <summary>One link preview in a post body with its state (<c>resolving</c> /
/// <c>resolved</c> / <c>failed</c>) — the shared <c>RenderDocument::link_previews</c>
/// pair (render-model.md § D4), which the e2e state dump publishes as
/// <c>link_previews</c>. A windows-local public record for the same reason as
/// <see cref="PostSourceBadge"/>.</summary>
public sealed record LinkPreviewStateRow(string Url, string State);

/// <summary>One row in the <c>post-tip-list</c> attribution window
/// (monetization.md § Tips) — the tipper (or the localized "someone"/mechanism-id
/// stand-in) and the amount (or the localized "amount not reported" — never "0
/// sats"), pre-joined at construction since the row paints one text run, not a
/// name/amount pair (mirrors tui's <c>tip_list_elements</c> row shape). A
/// windows-local public type so the XAML can <c>x:Bind</c> it.</summary>
public sealed record TipSenderRow(string DisplayText);
