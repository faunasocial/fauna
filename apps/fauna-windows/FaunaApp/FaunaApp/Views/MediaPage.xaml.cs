using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Media;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Helpers;
using FaunaApp.Sync;
using uniffi.fauna_ffi;
using uniffi.fauna_media_machine;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Media content browser — the READ plane over folders (media.md), rendered
/// ENTIRELY off the shared <see cref="MediaMachine"/> (<c>libs/fauna-media-machine</c>
/// via UniFFI <c>BuildMediaMachine</c>), the same observer-driven pattern as
/// <see cref="DevicesPage"/>. media.md rule 2 — observer-driven rendering off the
/// cross-set <c>fauna.media.list</c> snapshot; all sort/filter logic runs in shared
/// Rust. Folder CREATION and CONFIGURATION live in Settings → Folders
/// (<see cref="FoldersPage"/>).
/// <para>
/// The default view is the cross-set all-media aggregate (<c>media-folder-filter</c>
/// = <c>"__all__"</c> ⇒ <c>SetFilter(null)</c>); a per-set filter scopes to one set.
/// Upload (<c>file-upload</c> / <c>upload-button</c>) reads the picked file and uploads
/// it into the selected set via the shared <c>MediaMachine::upload_selected</c> gesture
/// (seal under the owner BackupKey → POST the blob → record the manifest member →
/// refresh; media.md § Where logic lives / § User actions). The "which set" policy
/// lives in shared Rust, so every app targets the same set (priority #1/#2 — no
/// app-side seal/POST); reading the file + deriving the owner key and this device's
/// sync id is the only client glue. Mirrors linux
/// (<c>apps/fauna-linux/src/views/media/mod.rs</c>).
/// </para>
/// </summary>
public sealed partial class MediaPage : Page
{
    private INestRpcClient? _rpc;
    private ICryptoService? _crypto;
    private ISessionAccount? _account;
    // search.md § User actions (SearchNav.File) — set when SearchResultsPage
    // routed a File search hit here; consumed once in Page_Loaded, after the
    // machine's own list has loaded (LocateFile answers from ITS snapshot,
    // not a fresh read).
    private (long FolderId, string PathHash)? _deepLinkFile;
    private MediaMachine? _machine;
    private MediaNotifyObserver? _observer;

    // ── media-item-detail state (media.md § Element IDs) ────────────────────
    // The item whose detail sheet is open, or null when it is closed. Doubles as
    // the stale-response guard: every await in the versions/restore flow re-checks
    // ReferenceEquals(_detailItem, item) afterwards, so a close — or an open of a
    // DIFFERENT item — while a fetch is in flight can never retarget the sheet
    // (mirrors web's `if (detailItem !== item) return`).
    private MediaItem? _detailItem;
    // The versions backing the file-version-item rows, oldest→newest, exactly as
    // shared Rust returned them. Kept so a restore click resolves its row's
    // version_num back to the full FileVersionSummary the machine needs.
    private IReadOnlyList<FileVersionSummary> _versions = Array.Empty<FileVersionSummary>();
    private FileVersionSummary? _restoreTarget;
    // Recovery browse (file-versions.md § Retention (3)): ON
    // re-lists with include_pruned so soft-pruned rows appear with their badge +
    // undelete button; OFF returns to the live-only listing. Reset per opened item
    // (OpenDetail), mirroring linux's per-open Cell<bool>.
    private bool _includePruned;
    // media-item-detail-download-button (media.md § Element IDs, approved
    // 2026-09-25). The newest version row — the current file, whose manifest the
    // shared download_file walk is keyed by; null until the versions read lands
    // (the button paints only once it exists). And the followed scope's opaque
    // value when the detail opened inside one — such an item downloads through the
    // keyless download_followed instead (media.md rule 6), never the owner-key walk.
    private FileVersionSummary? _latest;
    private string? _detailFollowedScope;

    // True while RenderPage is mutating bound controls (combo selections, view
    // toggle). The chrome SelectionChanged / Click handlers early-return so a
    // programmatic update never re-enters the machine (mirrors linux's `updating`
    // guard).
    private bool _rendering;

    // The followed browse scopes' opaque select values, as of the last snapshot
    // (media.md § Followed public folders). The filter handler asks THIS which
    // values route to the on-demand followed browse rather than to the own-set
    // filter — the value's shape is the machine's business and is never parsed
    // here.
    private HashSet<string> _followedValues = new();

    // Static accessor for the DataTemplate x:Bind (called by ImageHashBind in each
    // media-thumbnail Image). Built in Page_Loaded once the MediaMachine + owner
    // BackupKey exist; a row realized before then binds a null loader (the
    // placeholder) — harmless, since rows are only created after the machine builds.
    private static MediaThumbnailLoader? _thumbLoader;
    // Returns the public interface (not the internal MediaThumbnailLoader) so the
    // public getter is XAML-bindable to ImageHashBind.Loader without a CS0050.
    public static IHashImageLoader? GetThumbLoader() => _thumbLoader;

    public MediaPage()
    {
        this.InitializeComponent();

        MediaTitle.Text = S.Get("media/title");
        EmptyText.Text = S.Get("media/no_media_yet");
        ImageLightboxDialog.CloseButtonText = S.Get("common/close");

        // media-item-detail / file-version-restore-confirm-modal static labels.
        VersionsTitle.Text = S.Get("media/versions_title");
        RestoreConfirmTitle.Text = S.Get("media/restore_confirm_title");
        RestoreConfirmBody.Text = S.Get("media/restore_confirm_body");
        DeleteConfirmTitle.Text = S.Get("media/file_detail/delete_confirm_title");

        // Static toolbar labels.
        BrowseBtn.Content = S.Get("devices/sync_locations/browse");
        UploadBtn.Content = S.Get("media/upload");
        UploadPathBox.PlaceholderText = S.Get("media/choose_file");

        // Default view-toggle label: the button offers to switch TO grid view.
        ViewToggleBtn.Content = S.Get("media/view_grid");

        // Sort ComboBox — name / size / date, label sourced from shared Rust
        // (fauna-core::format::media_sort_label, media.md § Layout & flow).
        // AutomationProperties.Name = tag value so FlaUI Select-by-value works;
        // the machine's SetSort takes the same tag.
        foreach (var tag in new[] { "name", "size", "date" })
        {
            var item = new ComboBoxItem { Content = S.Resolve(FaunaFfiMethods.MediaSortLabel(tag)), Tag = tag };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, tag);
            SortCombo.Items.Add(item);
        }
        SortCombo.SelectedIndex = 0;

        // The two share gestures the offline gate can desensitize — create and
        // revoke are OnlineOnly (share-links.md § Create). The list button's
        // `fauna.share.list` is a Read, which a windows gate may not declare
        // (check-offline-gate-kinds: only class 3 desensitizes).
        ShareCreateBtn.FaunaGate("fauna.share.create");
        ShareRevokeConfirmBtn.FaunaGate("fauna.share.revoke");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _crypto = clients.Crypto;
            _account = clients.Account;
            _deepLinkFile = clients.DeepLinkFile;
            // Live refresh while the page is open (media.md § State & data shape →
            // *Staying live while the page is open*; file-sync.md § Remote-change
            // nudge owns the push mechanism itself). This page has no
            // NavigationCacheMode (a fresh instance per navigation, mirrors
            // FoldersPage), so subscribing here and unsubscribing in
            // OnNavigatedFrom below IS the "Media is the page on screen" gate —
            // fauna.media.list is a cross-set aggregate and the nest emits one
            // nudge per recorded file, so an unscoped subscriber would re-read it
            // once per file during an initial folder sync nobody is watching.
            //
            // ALSO subscribe to Reconnected (closed 2026-09-06, transport.md §
            // Which surfaces a push invalidates — the windows-leg audit): Media
            // used to sit entirely outside the reconnect sweep, so a dropped
            // fauna.sync.changed push across a socket gap stayed unrecovered
            // until the user navigated away and back.
            if (_rpc is not null)
            {
                _rpc.FolderChangedPushed += OnFolderChangedPushed;
                _rpc.Reconnected += OnReconnected;
            }
        }
    }

    /// <summary>Stop live-refreshing once this page is navigated away from — the
    /// subscription would otherwise leak onto <c>_rpc</c>, which outlives the page
    /// (mirrors FoldersPage.OnNavigatedFrom).</summary>
    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        if (_rpc is not null)
        {
            _rpc.FolderChangedPushed -= OnFolderChangedPushed;
            _rpc.Reconnected -= OnReconnected;
        }
        base.OnNavigatedFrom(e);
    }

    /// <summary>A record landed in a folder this device participates in
    /// (fauna.sync.changed) while Media is the page on screen — re-read the
    /// cross-set aggregate so a file from this device's own sync engine, a
    /// second device, or a collaborator appears with no manual refresh. Unlike
    /// FoldersPage's per-set device-activity roster, Media has no per-row scope
    /// to match against: any push while this page is open means SOME readable
    /// set changed, and the aggregate itself is what needs re-reading (mirrors
    /// linux's `media_page_is_visible` gate / tui's `StaleSurfaces::media`).</summary>
    private void OnFolderChangedPushed(string folder) => RefreshMedia();

    /// <summary>Reconnect-sweep arm (transport.md § Which surfaces a push
    /// invalidates, windows-leg audit): a reconnect or ResyncRequired may have
    /// dropped a fauna.sync.changed push while this page was open, so re-read
    /// the aggregate unconditionally rather than waiting for the next push.</summary>
    private void OnReconnected() => RefreshMedia();

    private void RefreshMedia()
    {
        if (_machine is null) return;
        byte[]? backupKey = _crypto is not null
            ? uniffi.fauna_ffi.FaunaFfiMethods.BackupKeyDerive(_crypto.SecretBytes)
            : null;
        _ = _machine.Refresh(backupKey);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            // Build the page machine bound to the session WS-RPC requester; the
            // observer ticks the UI thread (MediaNotifyObserver) → RenderPage.
            _observer = new MediaNotifyObserver(RenderPage);
            _machine = await _rpc.BuildMediaMachineAsync(_observer);
            // The followed-public-folder source, injected right after the build
            // (media.md § Followed public folders). Without it snapshot.followed
            // is permanently empty and the filter offers no followed scope — a
            // compiles-and-is-unreachable omission, which is why it sits on the
            // line after the machine exists rather than anywhere later.
            await _rpc.WireMediaFollowedFoldersAsync(_machine);
            // Build the media-thumbnail loader now that the machine + owner BackupKey
            // exist: each media-thumbnail Image fetches its blob through the shared
            // MediaMachine::fetch_thumbnail (direct-by-hash + BackupKey decrypt;
            // media.md § Thumbnails), which can render owner-sealed thumbnails a plain
            // blob GET cannot. The owner BackupKey is derived from the identity seed
            // via the FFI (the same derivation the upload path uses).
            byte[]? backupKey = _crypto is not null
                ? uniffi.fauna_ffi.FaunaFfiMethods.BackupKeyDerive(_crypto.SecretBytes)
                : null;
            if (backupKey is not null)
            {
                _thumbLoader = new MediaThumbnailLoader(_machine, backupKey);
                // Write-side label custody for the delete/restore gestures (S8 D2,
                // file-sync.md § Sealed names & paths → Implementation status): the
                // same per-actor owner key the upload gesture seals with, injected
                // once so those records seal instead of resting plaintext-only.
                // Mirrors linux's build_media_machine_with_folder_keys. No second
                // derivation — same backupKey the read path above already computed.
                _machine.SetOwnerBackupKey(backupKey);
            }
            // Pass the owner backup key so a sealed row renders from its seal too
            // (file-sync.md § Sealed names & paths — the S3 per-app swap).
            await _machine.Refresh(backupKey);
        }
        catch (Exception ex)
        {
            ErrorBar.Message = S.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
        }
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RenderPage();

        // search.md § User actions (SearchNav.File): resolve the durable
        // (folder_id, path_hash) pair through the now-loaded machine's own
        // LocateFile — never a client-side scan or a name-based match, since
        // the page's rendered rows key on Folder/Path (names), a DIFFERENT
        // id space from this pair (content-index.md, the id-space trap this
        // row's diff repeats). A None (deleted, renamed, or in a set this
        // actor cannot see) degrades to the navigate-with-nothing-open
        // posture tui/linux take for the same case — never an error.
        if (_deepLinkFile is { } link && _machine is not null)
        {
            _deepLinkFile = null;
            if (_machine.LocateFile(link.FolderId, link.PathHash) is { } located)
            {
                OpenDetail(MapItem(located));
            }
        }

        // The Explorer Share leaf's hand-off (windows.md § Shell Extension → The
        // Share hand-off, step 4): open the file's detail and the machine's create
        // surface over it. Located by (set id, path) through the machine's own
        // LocatePath — never a name match; the machine refuses an ineligible file
        // itself (open_share_create is then a no-op). Nothing found → the page
        // with nothing open, never an error.
        if (App.PendingShareLink is { } share && _machine is not null)
        {
            App.PendingShareLink = null;
            if (_machine.LocatePath(share.FolderId, share.Path) is { } located)
            {
                OpenDetail(MapItem(located));
                _machine.OpenShareCreate(located.folder, located.path);
                RenderPage();
            }
        }
    }

    // ── Observer-driven render (media.md rule 2) ────────────────────────────

    private void RenderPage()
    {
        if (_machine is null) return;
        _rendering = true;
        try
        {
            var snap = _machine.Snapshot();

            // Items — the cross-set aggregate for the active filter, already sorted
            // in shared Rust. Bind both views to the same list; visibility below
            // picks the active one.
            var rows = snap.items.Select(MapItem).ToList();
            FileList.ItemsSource = rows;
            FileGrid.ItemsSource = rows;

            // View toggle (list ↔ grid).
            var grid = snap.viewGrid;
            FileList.Visibility = grid ? Visibility.Collapsed : Visibility.Visible;
            FileGrid.Visibility = grid ? Visibility.Visible : Visibility.Collapsed;
            // The button offers the OTHER mode.
            ViewToggleBtn.Content = grid ? S.Get("media/view_list") : S.Get("media/view_grid");

            // Sort + filter selects reflect the snapshot's active state.
            SelectComboByTag(SortCombo, snap.sort);
            // The followed browse scopes ride AFTER the own-set options, and the
            // ACTIVE filter is either an own set (snap.filter) or a followed
            // scope (snap.followedScope) — never both, so one nullable read
            // picks the selection (media.md § Followed public folders).
            _followedValues = snap.followed.Select(f => f.value).ToHashSet();
            RebuildFilterCombo(snap.folders, snap.followed, snap.followedScope?.value ?? snap.filter);

            // A followed browse scope is READ-ONLY, structurally: the upload
            // affordance is removed from the tree rather than disabled, because a
            // follow never enters `known_folders` and so can never be an upload
            // target. Collapsed ⇒ uncounted by UIA, which is the contract
            // (`count("upload-button") == 0`).
            UploadRow.Visibility = snap.followedScope is null
                ? Visibility.Visible
                : Visibility.Collapsed;

            // Empty state: the page has finished loading (snapshot.loaded) AND no
            // readable set has any media for the active filter. Gating on `loaded`
            // stops "No media yet" from painting over a read still in flight — the
            // three-state rule (media.md § Default view: cross-set all-media).
            EmptyText.Visibility = snap.loaded && rows.Count == 0
                ? Visibility.Visible
                : Visibility.Collapsed;

            // Page-level error: the machine localizes the last read failure into
            // snapshot.error; a subsequent successful refresh clears it.
            if (snap.error is { } err)
            {
                var msg = S.Resolve(err);
                ErrorBar.Message = msg;
                ErrorBar.IsOpen = true;
                App.CurrentErrorMessage = msg;
            }
            else
            {
                ErrorBar.IsOpen = false;
                App.CurrentErrorMessage = null;
            }

            RenderShareSurfaces(snap);
        }
        finally
        {
            _rendering = false;
        }
    }

    private static MediaItem MapItem(MediaItemSummary s) => new(
        Name: s.name,
        Path: s.path,
        Folder: s.folder,
        SizeBytes: (ulong)s.sizeBytes,
        UpdatedAt: (ulong)s.updatedAt,
        ThumbnailHash: s.thumbnailHash,
        SourceStatus: s.sourceOnline
            ? S.Get("media/source_online")
            : S.Get("media/source_offline"),
        ShareLinkEligible: s.shareLinkEligible);

    private static void SelectComboByTag(ComboBox combo, string tag)
    {
        for (var i = 0; i < combo.Items.Count; i++)
        {
            if (combo.Items[i] is ComboBoxItem { Tag: string t } && t == tag)
            {
                combo.SelectedIndex = i;
                return;
            }
        }
    }

    private void RebuildFilterCombo(
        IReadOnlyList<string> folders,
        IReadOnlyList<FollowedScopeOption> followed,
        string? activeFilter)
    {
        // "__all__" (cross-set all-media default) + one entry per browsable set the
        // client knows, empty ones included (snapshot.folders — media.md § Layout &
        // flow), then the followed browse scopes AFTER the own sets. Mirror linux:
        // rebuild only when the tag set
        // changed, so a newly-populated set appears without clobbering an in-flight
        // selection. "__all__" is the canonical sentinel (actions/media.py
        // MEDIA_FILTER_ALL; reserved __*-prefixed so it can't collide with a set
        // name). media.md § O-4.
        var desired = new List<string> { "__all__" };
        desired.AddRange(folders);
        // Both halves of a followed option are shared-Rust minted: the value is
        // opaque and disjoint from every set name by construction, the label
        // already carries the owner disambiguator. Never parse the value — the
        // MACHINE says which values are followed, and the routing below asks it.
        desired.AddRange(followed.Select(f => f.value));

        var current = FilterCombo.Items
            .OfType<ComboBoxItem>()
            .Select(it => it.Tag as string)
            .ToList();

        if (!current.SequenceEqual(desired))
        {
            // A followed scope's value is an OPAQUE address; painting it would
            // put the raw address on screen. A WinUI ComboBoxItem carries the
            // display and the drive-key on separate properties — Content is the
            // minted label, the UIA Name is the value the cross-app
            // select(id, value) contract addresses — so windows needs no
            // value→label side map of the sort GTK and Compose keep.
            var labels = followed.ToDictionary(f => f.value, f => f.label);
            FilterCombo.Items.Clear();
            foreach (var tag in desired)
            {
                var label = tag == "__all__"
                    ? S.Get("media/filter_all")
                    : labels.TryGetValue(tag, out var minted) ? minted : tag;
                var item = new ComboBoxItem { Content = label, Tag = tag };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, tag);
                FilterCombo.Items.Add(item);
            }
        }

        SelectComboByTag(FilterCombo, activeFilter ?? "__all__");
    }

    // ── Explorer chrome event handlers (gestures → machine) ─────────────────

    private void ViewToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || _rendering) return;
        // Flip relative to the snapshot's current state; the observer repaints.
        _machine.SetViewGrid(!_machine.Snapshot().viewGrid);
    }

    private void SortCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_machine is null || _rendering) return;
        if (SortCombo.SelectedItem is ComboBoxItem { Tag: string tag })
            _machine.SetSort(tag);
    }

    private async void FilterCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_machine is null || _rendering) return;
        if (FilterCombo.SelectedItem is not ComboBoxItem { Tag: string tag }) return;
        // A followed scope's minted value routes to the async on-demand browse
        // instead of the own-set filter. Which values are followed is ASKED of
        // the snapshot, never parsed out of the value — the shape of that
        // address is the machine's business (media.md § Followed public
        // folders).
        if (_followedValues.Contains(tag))
        {
            await _machine.SelectFollowedScope(tag);
            return;
        }
        // "__all__" ⇒ the cross-set all-media view (null filter).
        _machine.SetFilter(tag == "__all__" ? null : tag);
    }

    // ── media-item-detail + file-version-history ─────────────────────────────
    //
    // media.md § Element IDs (approved 2026-07-09) + file-sync.md § File Versions /
    // § Restore. The whole surface is a thin render over two shared-Rust gestures:
    //
    //   MediaMachine::file_versions(folder, path)  → Vec<FileVersionSummary>
    //   MediaMachine::restore_version(folder, device_id, path, version)
    //
    // The path→path_hash BLAKE3 derivation, the projection semantics, and the
    // restore re-point (an ordinary `modify` carrying the historical manifest_hash /
    // size_bytes / content_key_version — no byte re-upload) all live in shared Rust.
    // Never reimplement them here (priority #2). Mirrors linux
    // (apps/fauna-linux/src/views/media/detail.rs) and web (routes/media/+page.svelte).

    private void MediaItem_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || _rendering) return;
        if (sender is not Button { Tag: MediaItem item }) return;
        OpenDetail(item);
    }

    private async void OpenDetail(MediaItem item)
    {
        _detailItem = item;
        _versions = Array.Empty<FileVersionSummary>();
        _includePruned = false;
        _latest = null;
        ShowPrunedToggle.IsOn = false;
        VersionsList.ItemsSource = null;
        VersionsStatus.Visibility = Visibility.Collapsed;
        DetailName.Text = item.Name;
        DetailSheet.Visibility = Visibility.Visible;

        // A followed item's detail offers DOWNLOAD ALONE (media.md § Followed
        // public folders): the public read plane is head-only, so there is no
        // version history to list or recover, nothing to restore, and nothing this
        // reader may delete. All of it is removed from the tree rather than
        // disabled, the same absence rule the upload row follows — and the versions
        // read is skipped outright rather than issued and discarded. The download
        // paints at once: a followed item needs no manifest from a version row (the
        // machine retained the head at scope entry).
        _detailFollowedScope = _machine?.Snapshot().followedScope?.@value;
        var followed = _detailFollowedScope is not null;
        VersionsTitle.Visibility = followed ? Visibility.Collapsed : Visibility.Visible;
        ShowPrunedToggle.Visibility = followed ? Visibility.Collapsed : Visibility.Visible;
        VersionsBorder.Visibility = followed ? Visibility.Collapsed : Visibility.Visible;
        DeleteBtn.Visibility = followed ? Visibility.Collapsed : Visibility.Visible;
        DownloadBtn.Visibility = followed ? Visibility.Visible : Visibility.Collapsed;
        // share-link-button: absent (Collapsed), never inert, on an ineligible file —
        // the shared verdict, already false in a followed scope.
        ShareLinkBtn.Visibility = item.ShareLinkEligible ? Visibility.Visible : Visibility.Collapsed;
        if (followed) return;

        await LoadVersions(item);
    }

    private void DetailClose_Click(object sender, RoutedEventArgs e) => CloseDetail();

    private void CloseDetail()
    {
        // Clearing _detailItem BEFORE collapsing is what makes the stale-response
        // guard fire for any fetch still in flight.
        _detailItem = null;
        _restoreTarget = null;
        RestoreConfirmSheet.Visibility = Visibility.Collapsed;
        DeleteConfirmSheet.Visibility = Visibility.Collapsed;
        DetailSheet.Visibility = Visibility.Collapsed;
    }

    /// <summary>
    /// Load (or reload) the open item's version rows. The read is a per-item QUERY:
    /// <c>MediaMachine::file_versions</c> returns its error to the caller rather than
    /// setting <c>snapshot.error</c>, so a failure surfaces on this sheet's own status
    /// line and never on the page <c>error-message</c> banner (linux + web both do
    /// exactly this).
    /// </summary>
    private async System.Threading.Tasks.Task LoadVersions(MediaItem item)
    {
        if (_machine is null) return;

        VersionsStatus.Text = S.Get("media/versions_loading");
        VersionsStatus.Visibility = Visibility.Visible;

        FileVersionSummary[] versions;
        try
        {
            versions = await _machine.FileVersions(item.Folder, item.Path, _includePruned);
        }
        catch (Exception ex)
        {
            // The sheet may have been closed (or retargeted) while the read was in
            // flight — writing a stale error into it would be a lie about the item
            // now on screen.
            if (!ReferenceEquals(_detailItem, item)) return;
            VersionsStatus.Text = S.Get("media/versions_error").Replace("{message}", S.Error(ex));
            VersionsStatus.Visibility = Visibility.Visible;
            return;
        }

        if (!ReferenceEquals(_detailItem, item)) return;

        _versions = versions;
        // Rows are oldest→newest: the last one is the current file, and the
        // download paints once it exists (linux's render_versions).
        _latest = versions.LastOrDefault();
        DownloadBtn.Visibility = _latest is null ? Visibility.Collapsed : Visibility.Visible;
        var nowMs = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        // FileVersionFormat owns the row projection — and with it the millis-vs-seconds
        // unit contract — so FaunaApp.Tests can pin it; this code-behind is unreachable
        // from that assembly (the NestTrustFormat precedent).
        VersionsList.ItemsSource = versions
            .Select(v => FileVersionFormat.MapRow(v, nowMs))
            .ToList();
        VersionsStatus.Visibility = Visibility.Collapsed;
    }

    private void RestoreVersion_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { Tag: long versionNum }) return;
        var target = _versions.FirstOrDefault(v => v.@versionNum == versionNum);
        if (target is null) return;
        _restoreTarget = target;
        RestoreConfirmSheet.Visibility = Visibility.Visible;
    }

    /// <summary>Recovery browse (file-versions.md § Retention (3)): ON re-lists with <c>include_pruned</c> so soft-pruned rows appear with
    /// their badge + undelete button; OFF returns to the live-only listing. Mirrors
    /// linux's <c>show_pruned_switch.connect_active_notify</c>.</summary>
    private async void ShowPrunedToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_detailItem is not { } item) return;
        _includePruned = ShowPrunedToggle.IsOn;
        await LoadVersions(item);
    }

    /// <summary>Recover a soft-pruned version back into the live listable population
    /// (<c>fauna.files.versions.undelete</c>, the version-plane twin of
    /// <c>snapshot-undelete-button</c>) — present only on pruned rows, keyed by the
    /// row's own <c>VersionNum</c>, never a painted index (mirrors
    /// <see cref="RestoreVersion_Click"/>'s own lookup).</summary>
    private async void UndeleteVersion_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        if (sender is not Button { Tag: long versionNum }) return;
        if (_detailItem is not { } item) return;

        try
        {
            await _machine.UndeleteVersion(item.Path, versionNum);
        }
        catch (Exception ex)
        {
            if (!ReferenceEquals(_detailItem, item)) return;
            VersionsStatus.Text = S.Get("media/error_undelete").Replace("{message}", S.Error(ex));
            VersionsStatus.Visibility = Visibility.Visible;
            return;
        }

        if (!ReferenceEquals(_detailItem, item)) return;
        await LoadVersions(item);
    }

    private void RestoreCancel_Click(object sender, RoutedEventArgs e)
    {
        _restoreTarget = null;
        RestoreConfirmSheet.Visibility = Visibility.Collapsed;
    }

    private async void RestoreConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        if (_detailItem is not { } item || _restoreTarget is not { } version) return;

        RestoreConfirmSheet.Visibility = Visibility.Collapsed;
        _restoreTarget = null;

        var deviceId = _account?.DeviceId;
        if (string.IsNullOrEmpty(deviceId))
        {
            // Client-glue failure (no sync device id) — surface it on the sheet, the
            // same place linux puts its device-id derivation failure.
            VersionsStatus.Text = S.Get("media/error_restore").Replace("{message}", "no sync device id");
            VersionsStatus.Visibility = Visibility.Visible;
            return;
        }

        // restore_version never throws for an RPC failure: it folds the error into
        // snapshot.error (the page banner) and returns. On success it refreshes, so
        // the observer repaints the list with the re-pointed head.
        await _machine.RestoreVersion(item.Folder, deviceId!, item.Path, version);

        if (!ReferenceEquals(_detailItem, item)) return;

        // Repaint here rather than waiting for the observer's dispatcher hop, then
        // reload the sheet's rows — the restore appended a NEW version (it is
        // reversible; file-sync.md § Restore), so the history grows by one.
        RenderPage();
        await LoadVersions(item);
    }

    // ── media-item-detail-download-button (media.md § Element IDs, approved
    //    2026-09-25). Two shared-Rust per-item QUERIES — a followed scope's
    //    keyless MediaMachine::download_followed, anything else the
    //    MediaMachine::download_file walk keyed by the latest version row (a
    //    shared set opens under the member's content keys through the
    //    NestFolderKeyResolver fauna-ffi's build_media_machine wires). The only
    //    client glue is the save: the ISnapshotFileSaver seam the backups
    //    single-file download uses. Mirrors linux (detail.rs::start_download /
    //    download_to). ─────────────────────────────────────────────────────────

    private async void DownloadBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || _detailItem is not { } item) return;
        var followedScope = _detailFollowedScope;
        var latest = _latest;
        if (followedScope is null && latest is null) return; // paints only once a manifest is known

        DownloadBtn.IsEnabled = false;
        try
        {
            byte[] bytes;
            if (followedScope is not null)
            {
                bytes = await _machine.DownloadFollowed(followedScope, item.Path);
            }
            else
            {
                // The owner BackupKey the walk takes for an owner-only set (the
                // same derivation the upload and thumbnail paths use); a shared or
                // served set resolves its content keys machine-side instead.
                var backupKey = _crypto is not null
                    ? FaunaFfiMethods.BackupKeyDerive(_crypto.SecretBytes)
                    : Array.Empty<byte>();
                bytes = await _machine.DownloadFile(
                    latest!.@manifestHash, latest.@contentKeyVersion, item.Folder, item.Path, backupKey);
            }
            // Named after the file's basename, so a name carrying a separator can
            // never steer the write outside the chosen directory.
            await Services.SnapshotFileSavers.ForSession().SaveAsync(SaveFileName(item.Name), bytes);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("MediaPage", $"media download failed: {ex.Message}");
            // A per-item query: the failure lands on the detail's own status line —
            // the page banner sits behind this sheet, where the user who asked could
            // not read it (the delete failure's reasoning).
            if (!ReferenceEquals(_detailItem, item)) return;
            VersionsStatus.Text = S.Get("media/error_download").Replace("{message}", S.Error(ex));
            VersionsStatus.Visibility = Visibility.Visible;
        }
        finally
        {
            DownloadBtn.IsEnabled = true;
        }
    }

    /// <summary>The save file name for <paramref name="name"/>: its last path
    /// component, else <c>download</c> (linux's <c>save_file_name</c>).</summary>
    private static string SaveFileName(string name)
    {
        var baseName = System.IO.Path.GetFileName(name.Replace('/', System.IO.Path.DirectorySeparatorChar));
        return string.IsNullOrEmpty(baseName) ? "download" : baseName;
    }

    // ── media-delete-button → media-delete-confirm-modal (media.md § Element
    //    IDs, user-approved 2026-07-16). The delete itself is the shared
    //    MediaMachine::delete gesture (→ fauna.sync.delete_member — records a
    //    tombstone; historical version rows survive, file-sync.md § File
    //    Versions). Mirrors linux (detail.rs::open_delete_confirm). ────────────

    private void DeleteBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_detailItem is not { } item) return;
        // The body names the file — the shared {name}-arg string, never a
        // hand-assembled sentence.
        DeleteConfirmBody.Text = S.Get("media/file_detail/delete_confirm")
            .Replace("{name}", item.Name);
        DeleteConfirmSheet.Visibility = Visibility.Visible;
    }

    // Cancel is a PURE no-op — close the modal, touch nothing (the contract the
    // sibling restore/re-auth confirm modals hold; pinned by
    // test_media_delete_cancel_is_a_no_op).
    private void DeleteCancel_Click(object sender, RoutedEventArgs e)
        => DeleteConfirmSheet.Visibility = Visibility.Collapsed;

    private async void DeleteConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || _detailItem is not { } item) return;

        DeleteConfirmSheet.Visibility = Visibility.Collapsed;

        var deviceId = _account?.DeviceId;
        if (string.IsNullOrEmpty(deviceId))
        {
            // Client-glue failure (no sync device id) — surface it on the sheet's
            // own status line, where linux puts its device-id derivation failure.
            VersionsStatus.Text = S.Get("media/error_delete").Replace("{message}", "no sync device id");
            VersionsStatus.Visibility = Visibility.Visible;
            return;
        }

        // delete never throws for an RPC failure: it folds the error into
        // snapshot.error and returns; on success it refreshes (the observer
        // repaints the explorer without the tombstoned row). The snapshot is
        // therefore the success signal, exactly like linux.
        await _machine.Delete(item.Folder, deviceId!, item.Path);

        if (!ReferenceEquals(_detailItem, item)) return;

        if (_machine.Snapshot().error is { } err)
        {
            // Failed: the file still exists, so KEEP the surface open and show the
            // error on its own status line — the page banner also carries it, but a
            // banner behind this sheet is a banner the user cannot read (linux's
            // exact rationale).
            VersionsStatus.Text = S.Resolve(err);
            VersionsStatus.Visibility = Visibility.Visible;
            return;
        }

        // Deleted: close the surface — its subject is gone, so a version history
        // for it would be a dangling view. (Restore instead reloads: its file
        // still exists.)
        CloseDetail();
        RenderPage();
    }

    // ── Share links (share-links.md § Flows; the share-link-* ids, approved
    //    2026-09-25). Every piece of state — the create step, the URL revealed
    //    only after registration, the list's `loaded` bit and row states, the
    //    armed revoke — lives in the shared MediaMachine; this section paints it
    //    and forwards gestures (§ Where logic lives). The three sheets are
    //    render-driven: each is Visible exactly while its half of the snapshot is
    //    open, so a close always goes through the machine's own gesture. Mirrors
    //    linux (apps/fauna-linux/src/views/media/share.rs). ─────────────────────

    private void RenderShareSurfaces(MediaPageSnapshot snap)
    {
        RenderShareCreate(snap);
        RenderShareList(snap);

        // The confirm sits over the list: armed AND the list still open.
        var armed = snap.shareLinks.open ? snap.shareLinks.revokeConfirm : null;
        ShareRevokeSheet.Visibility = armed is null ? Visibility.Collapsed : Visibility.Visible;
        if (armed is not null)
        {
            var name = snap.shareLinks.rows.FirstOrDefault(r => r.tokenId == armed)?.name ?? string.Empty;
            ShareRevokeBody.Text = S.Format("share_link/revoke_confirm_body", name);
        }
    }

    private void RenderShareCreate(MediaPageSnapshot snap)
    {
        if (snap.shareCreate is not { } create)
        {
            ShareCreateSheet.Visibility = Visibility.Collapsed;
            return;
        }
        ShareCreateSheet.Visibility = Visibility.Visible;
        ShareCreateTitle.Text = S.Format("share_link/create_title", create.name);
        RebuildExpiryCombo(snap.shareExpiryOptions, create.expiry);

        // Step 4 of the create flow: the URL (and its Copy) exist only once the
        // registration succeeded; until then the expiry and Create show. A busy
        // create relabels rather than disables — the machine itself refuses a
        // second create while one is in flight, and the offline gate owns this
        // control's enablement.
        var revealed = create.url;
        var pending = revealed is null ? Visibility.Visible : Visibility.Collapsed;
        ShareExpiryRow.Visibility = pending;
        ShareCreateBtn.Visibility = pending;
        ShareCreateBtn.Content = S.Get(create.busy ? "share_link/creating" : "share_link/create");
        ShareUrlText.Text = revealed ?? string.Empty;
        ShareUrlText.Visibility = revealed is null ? Visibility.Collapsed : Visibility.Visible;
        ShareCopyBtn.Visibility = ShareUrlText.Visibility;
        ShareCancelBtn.Content = S.Get(revealed is null ? "share_link/cancel" : "share_link/close");
        PaintStatus(ShareCreateStatus, ShareLinkFormat.OwnError(snap.error, ShareLinkFormat.CreateErrorKeys));
    }

    private void RenderShareList(MediaPageSnapshot snap)
    {
        var list = snap.shareLinks;
        ShareListSheet.Visibility = list.open ? Visibility.Visible : Visibility.Collapsed;
        if (!list.open) return;

        // Three states off one `loaded` bit (ui/README.md § List pages: loading is
        // not empty): rows, the empty state, or the loading line.
        ShareListLoading.Visibility = list.loaded ? Visibility.Collapsed : Visibility.Visible;
        ShareListEmpty.Visibility = list.loaded && list.rows.Length == 0
            ? Visibility.Visible
            : Visibility.Collapsed;
        ShareLinksList.ItemsSource = list.rows.Select(ShareLinkFormat.MapRow).ToList();
        PaintStatus(ShareListStatus, ShareLinkFormat.OwnError(snap.error, ShareLinkFormat.ListErrorKeys));
    }

    private static void PaintStatus(TextBlock status, string? text)
    {
        status.Text = text ?? string.Empty;
        status.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>The <c>share-link-expiry-select</c> options, rebuilt only when the
    /// machine's option list changed; each item's UIA Name is the option VALUE (the
    /// cross-app <c>select(id, "7d")</c> contract) and its Content the shared label.
    /// Called under <see cref="_rendering"/>, so the selection it makes never
    /// re-enters the machine.</summary>
    private void RebuildExpiryCombo(IReadOnlyList<string> options, string selected)
    {
        var current = ShareExpiryCombo.Items.OfType<ComboBoxItem>().Select(it => it.Tag as string);
        if (!current.SequenceEqual(options))
        {
            ShareExpiryCombo.Items.Clear();
            foreach (var value in options)
            {
                var item = new ComboBoxItem { Content = ShareLinkFormat.ExpiryLabel(value), Tag = value };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, value);
                ShareExpiryCombo.Items.Add(item);
            }
        }
        SelectComboByTag(ShareExpiryCombo, selected);
    }

    private void ShareLinkBtn_Click(object sender, RoutedEventArgs e)
    {
        // The machine refuses an ineligible or unknown file (a no-op), so no
        // surface follows for one.
        if (_machine is null || _detailItem is not { } item) return;
        _machine.OpenShareCreate(item.Folder, item.Path);
        RenderPage();
    }

    private void ShareExpiryCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_machine is null || _rendering) return;
        if (ShareExpiryCombo.SelectedItem is ComboBoxItem { Tag: string value })
            _machine.SetShareExpiry(value);
    }

    private async void ShareCreateBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        // mint → sign → seal the name → register; the URL is revealed only once the
        // registration succeeded, and a failure lands in snapshot.error (repeated
        // on this sheet's own status line). Never throws for an RPC failure.
        await _machine.CreateShareLink();
        RenderPage();
    }

    private void ShareCancelBtn_Click(object sender, RoutedEventArgs e)
    {
        _machine?.CloseShareCreate();
        RenderPage();
    }

    private void ShareCopyBtn_Click(object sender, RoutedEventArgs e) =>
        CopyShareUrl(_machine?.Snapshot().shareCreate?.url);

    private async void ShareLinkListBtn_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        // Opens the surface at once (loading) and reads fauna.share.list; the
        // observer repaints as the read lands.
        await _machine.OpenShareLinks();
        RenderPage();
    }

    private void ShareListClose_Click(object sender, RoutedEventArgs e)
    {
        _machine?.CloseShareLinks();
        RenderPage();
    }

    // Copy only where the shared re-derivation verified the URL — the button is
    // absent otherwise (ShareLinkRow.CanCopy), so the Tag is never a wrong link.
    private void ShareRowCopy_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string url }) CopyShareUrl(url);
    }

    private void ShareRowRevoke_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || sender is not Button { Tag: string tokenId }) return;
        _machine.ArmShareRevoke(tokenId);
        RenderPage();
    }

    private void ShareRevokeCancel_Click(object sender, RoutedEventArgs e)
    {
        _machine?.CancelShareRevoke();
        RenderPage();
    }

    private async void ShareRevokeConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        // Takes the armed token first thing; the render then closes the confirm
        // and the row repaints Revoked (or the error lands on the list's line).
        await _machine.ConfirmShareRevoke();
        RenderPage();
    }

    private void CopyShareUrl(string? url)
    {
        try
        {
            ClipboardHelper.CopyText(url);
        }
        catch (Exception ex)
        {
            // Clipboard contention is the OS's, not a share failure: log it and
            // leave the URL on screen, where it stays selectable.
            ShellLog.Warn("MediaPage", $"share link copy failed: {ex.Message}");
        }
    }

    // ── Upload: Browse + submit (present but inert — see class summary) ──────

    private async void BrowseBtn_Click(object sender, RoutedEventArgs e)
    {
        if (App.MainWindow is null) return;
        try
        {
            var picker = new Windows.Storage.Pickers.FileOpenPicker();
            picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.Desktop;
            picker.FileTypeFilter.Add("*");

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSingleFileAsync();
            if (file is not null)
                UploadPathBox.Text = file.Path;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("MediaPage", $"file pick failed: {ex.Message}");
        }
    }

    private async void UploadBtn_Click(object sender, RoutedEventArgs e)
    {
        // Upload the picked file into the selected set via the shared
        // MediaMachine::upload_selected gesture (seal under the owner BackupKey →
        // POST the blob → record the manifest member → refresh; media.md § Where
        // logic lives / § User actions). The "which set" target policy (the
        // filter-selected set, else the first set with media, else the no-set page
        // error) lives in shared Rust — identical on every app (priority #1).
        // Reading the file + deriving the owner key and this device's sync id is the
        // only client glue (priority #2 — no app-side seal/POST). Mirrors linux
        // (apps/fauna-linux/src/views/media/mod.rs).
        if (_machine is null) return;

        // Nothing chosen: SAY so on `error-message`. This used to `return`
        // silently on the theory that the "Choose file…" placeholder guides,
        // which predates field evidence — a live user pressed tui's Upload with
        // an empty box, saw nothing happen, and reported the upload feature as
        // missing entirely (media.md § Client glue; e2e convention 2 — a pressed
        // button must always answer). The precondition itself lives in
        // FaunaApp.Core so it can be pinned without a window.
        var picked = UploadPathGuard.UploadPathOf(UploadPathBox.Text);
        if (picked is null)
        {
            ShowPageError(S.Get(UploadPathGuard.FileRequiredKey));
            return;
        }

        byte[] rawBytes;
        string memberPath;
        byte[] backupKey;
        var deviceId = _account?.DeviceId;
        try
        {
            rawBytes = System.IO.File.ReadAllBytes(picked);
            // The member path within the set is the picked file's name — a new
            // basename records a NEW member, not an update (matches linux).
            memberPath = System.IO.Path.GetFileName(picked);
            if (string.IsNullOrEmpty(deviceId))
                throw new InvalidOperationException("no sync device id");
            // The owner BackupKey (Library audience at rest; media.md § Encryption
            // at rest), derived from the identity seed via the FFI — the seed never
            // leaves the derivation (mirrors CapabilityProvisioner).
            backupKey = uniffi.fauna_ffi.FaunaFfiMethods.BackupKeyDerive(_crypto!.SecretBytes);
        }
        catch (Exception ex)
        {
            ShowUploadGlueError(ex.Message);
            return;
        }

        // Hand the bytes to the shared gesture; it seals + POSTs + records into the
        // selected set, then refreshes (the observer repaints the list, clearing
        // this error). A no-set / upload failure surfaces via snapshot.error → the
        // RenderPage error bar; the recording device self-heals its write
        // registration on the nest's device-unregistered rejection.
        try
        {
            await _machine.UploadSelected(deviceId!, memberPath, rawBytes, backupKey);
            // Surface the gesture's outcome synchronously (the same pattern as
            // Page_Loaded's post-Refresh render): the observer also enqueues a
            // render, but rendering here in the await-continuation avoids the
            // dispatcher-hop lag before the new item — or the page error
            // (snapshot.error, e.g. the no-set banner) — becomes visible.
            RenderPage();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = S.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
        }
    }

    /// <summary>
    /// Surface a client-side upload-glue failure (file read / missing sync device id
    /// / key derivation) on the page error, formatted as the localized
    /// <c>media/error_upload</c> banner so it reads like a machine-surfaced upload
    /// error. A subsequent successful upload's refresh clears it (RenderPage sets the
    /// bar from <c>snapshot.error</c>). Mirrors linux's <c>show_upload_glue_error</c>.
    /// </summary>
    private void ShowUploadGlueError(string detail)
    {
        var msg = S.Get("media/error_upload").Replace("{message}", detail);
        ErrorBar.Message = msg;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = msg;
    }

    /// <summary>
    /// Surface an already-complete message on the page <c>error-message</c>, verbatim.
    /// </summary>
    /// <remarks>
    /// Deliberately NOT <see cref="ShowUploadGlueError"/>: that one wraps its argument
    /// in the <c>media/error_upload</c> "Failed to upload: {message}" banner, which is
    /// right for a failed attempt but wrong for a precondition that stopped the upload
    /// before it began — "Failed to upload: Choose a file first" reports a failure that
    /// never happened. media.md § Client glue asks for <c>media.file_required</c>
    /// itself on <c>error-message</c>, which is also what tui shows.
    /// <para>
    /// <c>App.CurrentErrorMessage</c> is set alongside the bar because that field — not
    /// the bar's own text — is what the e2e state provider reads for
    /// <c>error-message</c>.
    /// </para>
    /// </remarks>
    private void ShowPageError(string message)
    {
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = message;
    }

    // ── x:Bind surface (static; called from DataTemplate) ───────────────────

    /// <summary>Format byte count using the shared Rust formatter.</summary>
    public static string FormatBytes(ulong bytes) => ValueFormat.ByteSize(bytes);

    /// <summary>Map a bool to Visible/Collapsed for the DataTemplate flags
    /// (<c>Pruned</c>, <c>CanCopy</c>, <c>CanRevoke</c>).</summary>
    public static Visibility BoolToVisibility(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Format a Unix-seconds timestamp as the shared fixed local
    /// date + time string (value-formatting.md § Absolute local timestamp display).</summary>
    public static string FormatTimestamp(ulong unixSecs) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocal((long)unixSecs);

    /// <summary>Open the image lightbox for a blob URL. Unwired: a media-item tap now
    /// opens <c>media-item-detail</c>; an in-sheet preview is a sanctioned later
    /// extension of that surface (media.md § Element IDs — additions go through
    /// ui.yaml approval).</summary>
    public async void ShowImageLightbox(string imageUrl)
    {
        ImageLightboxDialog.XamlRoot = this.XamlRoot;
        await Controls.Dialogs.ShowAsync(ImageLightboxDialog, prepare: () =>
            LightboxImage.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(imageUrl)));
    }
}
