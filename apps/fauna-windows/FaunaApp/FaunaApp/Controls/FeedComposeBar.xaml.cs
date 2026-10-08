using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;
using uniffi.fauna_feed;

namespace FaunaApp.Controls;

/// <summary>The composer's audience answer as the controls show it: at most one of
/// <paramref name="GateTier"/> / <paramref name="GateRoom"/> / <paramref name="Sell"/> is set
/// (all three resolve off the one selected index), none = Public.
/// <paramref name="GatePreview"/> is the teaser every restricted answer shares.
/// <c>Sell.AskingPrice</c> is the raw draft text (whole sats). <paramref name="Sell"/> is
/// always null in a store-safe build, which offers no "Sell this post…" answer.</summary>
public readonly record struct ComposeAudience(
    string? GateTier,
    string? GateRoom,
    string GatePreview,
    (string Price, string AskingPrice, bool SubscribersGetItFree)? Sell);

/// <summary>
/// Feed post compose bar with text, tags, submit, and compose-dialog button.
/// </summary>
public sealed partial class FeedComposeBar : UserControl
{
    private byte[]? _attachedBytes;
    private IReadOnlyList<string> _gateTierNames = Array.Empty<string>();

    /// <summary>The author's own rooms (`snapshot.own_rooms`), laid out between the
    /// tiers and "Sell this post…" — <see cref="RebuildGateItems"/>'s room segment.</summary>
    private IReadOnlyList<GateRoomOption> _ownRooms = Array.Empty<GateRoomOption>();

    /// <summary>Which of the four <c>compose-gate-tier-select</c> answers is staged. The
    /// fourth, the sale, is the paywall-designation gesture and exists only where the
    /// payments plane does (dynamic-features.md § Platform-family surface excision → The
    /// price-and-route class): a store-safe build offers three.</summary>
    private enum GateKind
    {
        Public,
        Tier,
        Room,
#if PAYMENTS
        Sell,
#endif
    }

#if PAYMENTS
    /// <summary>The sell composer's fields, dropped into <c>SellFieldsHost</c> — same
    /// removable-item reasoning as <c>ProfilePage._tierMoneyFields</c>.</summary>
    private readonly Views.Payments.SellComposeFields _sellFields;
#endif

    /// <summary>
    /// Raised when user clicks Post. Args: (text, tags, attachBytes, audience). The
    /// attachment's MIME is deliberately NOT carried: the page seals
    /// the bytes for the composer's audience at submit and the shared seal's own reply is the
    /// only correct source of the <c>MediaItem</c>'s type (media.md § Encryption at rest — a
    /// sealed sidecar reads <c>application/octet-stream</c>, and the picker's OS guess is a
    /// second source that can only ever disagree). The audience (<see cref="ComposeAudience"/>)
    /// is the answer as it stood at the press: a tier name gates the post to that tier
    /// (feed.md § Encryption at rest); a room is its hex channel id (`ui/feed.md`
    /// § Encryption at rest → Room-restricted — the app half); the teaser is shown to
    /// non-subscribers on a gated post.
    /// Returns whether the submit actually went out (published/gated/sold) — <c>false</c> on a
    /// refusal or an aborted upload, which keep the composer's staged content untouched
    /// (`ui/feed.md` § Persistence → Attachments by content address); <see cref="PostButton_Click"/>
    /// clears the boxes only on <c>true</c>, else a refused draft would lose the very text/tags/
    /// attachment the refusal exists to preserve.
    /// </summary>
    public event Func<string, string, byte[]?, ComposeAudience, Task<bool>>? PostRequested;

    /// <summary>
    /// Raised by the user's OWN audience gesture — picking an answer in
    /// <c>compose-gate-tier-select</c>, or editing a sale field while "Sell this post…" is
    /// picked — with the whole answer as the controls now show it. The page stages it on the
    /// shared manager AS IT IS PICKED, not only at submit, so a half-written post's audience
    /// rides the posts draft rail across a restart (`ui/feed.md` § Persistence → *Only
    /// user-authored input rests*; linux's <c>AudienceControls::forward</c> and web's
    /// <c>stageAudience</c> are the same shape). Never raised for this control's own option
    /// rebuilds or its paint of a restored answer (<see cref="_suppressAudienceForward"/>).
    /// </summary>
    public event Action<ComposeAudience>? AudienceChanged;

    /// <summary>Raised when the user edits the teaser (<c>compose-gate-preview-field</c>) — the
    /// one audience field that goes through its own setter, since it is shared by every
    /// restricted answer and must not re-stage (or flip) the answer itself.</summary>
    public event Action<string>? GatePreviewChanged;

    /// <summary>Suppresses <see cref="AudienceChanged"/>/<see cref="GatePreviewChanged"/> while
    /// this control writes the audience controls itself: <see cref="RebuildGateItems"/> (whose
    /// <c>Items.Clear()</c> alone raises SelectionChanged), <see cref="PaintAudience"/> and
    /// <see cref="RestoreAudienceFieldsIfUntouched"/>. Without it a rebuild that briefly shows
    /// Public — the restored tier's option not offered yet — would forward Public and erase the
    /// restored answer from the manager before it could ever be shown.</summary>
    private bool _suppressAudienceForward;

    /// <summary>The snapshot answer <see cref="PaintAudience"/> last put on screen — it paints
    /// only when the snapshot's answer CHANGES from this. The page's snapshot is cached until
    /// the manager's notify arrives (a dispatcher hop later), so a Refresh in between reads
    /// the answer from BEFORE the user's own pick; painting that would take the pick back.
    /// Unchanged-since-last-paint is exactly the stale case, so it is left alone.</summary>
    private (GateKind Kind, string? Key)? _paintedAnswer;

    /// <summary>Raised when user clicks the compose-dialog expand button.</summary>
    public event Action? ComposeDialogRequested;

    /// <summary>
    /// Raised whenever the user EDITS <see cref="ComposeText"/> or <see cref="TagsText"/>
    /// — never for this control's own restore-from-draft assignment
    /// (<see cref="RestoreDraftIfEmpty"/> suppresses it). Args: (text, tags). The
    /// feed page live-forwards this into the manager (<c>FeedViewModel.UpdateComposeText</c>,
    /// which carries the already-staged attachment through untouched — ui/feed.md §
    /// Persistence → Attachments by content address) and schedules a debounced draft save —
    /// draft-persistence v2, the feed leg (reserved-folders.md § Drafts Sync).
    /// </summary>
    public event Action<string, string>? ComposeChanged;

    /// <summary>
    /// Raised by the user's OWN attachment gesture — a fresh pick (<see cref="SetAttachment"/>,
    /// from the picker or e2e file injection) or the <c>compose-file-remove</c> click (drop
    /// it, <paramref name="attachedFile"/> arriving <c>null</c>). The feed page forwards this
    /// into the manager via <c>FeedViewModel.UpdateCompose</c> (the CURRENT text/tags plus this
    /// handle) — the ONE path allowed to change <c>attached_file</c> (`ui/feed.md` §
    /// Persistence → Attachments by content address); <see cref="ComposeChanged"/> must never
    /// touch it. Staged as the hash-less handle at pick time (before the submit's upload
    /// resolves its hash), so a draft saved before submit still carries the file.
    /// <c>internal</c> because <see cref="AttachedFile"/> is UniFFI-generated
    /// <c>internal</c> (mirrors <see cref="SetOwnRooms"/> below).
    /// </summary>
    internal event Action<AttachedFile?>? AttachmentStaged;

    /// <summary>
    /// Suppresses <see cref="ComposeChanged"/> during <see cref="RestoreDraftIfEmpty"/>'s
    /// own text assignment — without this, restoring (even into an already-empty
    /// field, where WinUI's TextBox still raises TextChanged for a same-value
    /// set) forwards straight back into <c>UpdateCompose</c> + a scheduled save,
    /// which can WIN the race against the user's own in-flight typing and
    /// persist an empty/stale draft over it. The exact restore→forward→re-save
    /// feedback loop linux's and web's own drafts legs discovered and guarded
    /// against (feed.md § Implementation status today) — windows' control-level
    /// equivalent of linux's <c>compose_bar::ComposeBar::render</c> push guard.
    /// </summary>
    private bool _suppressComposeChanged;

    /// <summary>Gets or sets the compose text. A multi-line WinUI TextBox separates lines
    /// with a bare <c>\r</c>; the post body leaves here with <c>\n</c>, the separator
    /// every other app's composer hands the shared producer (the conversations
    /// composer's <c>MarkdownRichEditBox.GetPlainText</c> does the same).</summary>
    public string ComposeText
    {
        get => ComposeBox.Text.Replace("\r\n", "\n").Replace('\r', '\n');
        set => ComposeBox.Text = value;
    }

    /// <summary>Gets or sets the tags text.</summary>
    public string TagsText
    {
        get => TagsBox.Text;
        set => TagsBox.Text = value;
    }

    public FeedComposeBar()
    {
        this.InitializeComponent();
        ComposeBox.PlaceholderText = S.Get("feed/post/write_post");
        TagsBox.PlaceholderText = S.Get("feed/post/tags_placeholder");
        PostButton.Content = S.Get("common/post");
        ToolTipService.SetToolTip(ComposeDialogButton, S.Get("composer/new_post"));

        // Gate-to-tier composer (feed.md § Encryption at rest): the preview placeholder + a
        // Public-only option set until the feed snapshot supplies the author's own tiers
        // (SetGateTiers, called from FeedPage.Refresh on every observer tick).
        GatePreviewBox.PlaceholderText = S.Get("feed/post/gate_preview_placeholder");
        ToolTipService.SetToolTip(GateTierSelect, S.Get("feed/post/gate_audience"));
#if PAYMENTS
        _sellFields = new Views.Payments.SellComposeFields();
        SellFieldsHost.Content = _sellFields;
        _sellFields.Changed += ForwardSaleField;
#endif
        GatePreviewBox.TextChanged += (_, _) =>
        {
            if (!_suppressAudienceForward) GatePreviewChanged?.Invoke(GatePreviewBox.Text);
        };
        SetGateTiers(Array.Empty<string>());
    }

    /// <summary>The answer the controls show right now — what a submit sends, and what
    /// <see cref="AudienceChanged"/> carries.</summary>
    public ComposeAudience CurrentAudience() => new(
        SelectedGateTier(),
        SelectedGateRoom(),
        GatePreviewBox.Text,
        StagedSale());

    /// <summary>The sale the fields show while "Sell this post…" is the answer, else null —
    /// always null in a store-safe build, which offers no such answer.</summary>
    private (string Price, string AskingPrice, bool SubscribersGetItFree)? StagedSale()
    {
#if PAYMENTS
        if (IsSellSelected())
            return (_sellFields.Price, _sellFields.AskingPrice, _sellFields.SubscribersGetItFree);
#endif
        return null;
    }

#if PAYMENTS
    /// <summary>A sale field edit is part of the answer only while the sale is the answer;
    /// with another answer picked the fields are hidden page-local text the manager does not
    /// hold (it keeps one answer, never "a tier plus a price").</summary>
    private void ForwardSaleField()
    {
        if (_suppressAudienceForward || !IsSellSelected()) return;
        AudienceChanged?.Invoke(CurrentAudience());
    }
#endif

    /// <summary>The kind + key the snapshot's audience names — the snapshot twin of
    /// <see cref="CurrentGateSelection"/>. The shared setters clear each other, so at most
    /// one of <c>sell</c> / <c>gate_room</c> / <c>gate_tier</c> is ever set.</summary>
    private static (GateKind Kind, string? Key) AnswerOf(FeedComposeState compose)
    {
        // A sale staged on a full client and synced in is not a store-safe build's to show
        // (it offers no Sell answer): its select reads as the gated answer the shared state
        // otherwise carries — apple's FeedVM.composeGateSelection, the same ruling.
#if PAYMENTS
        if (compose.@sell is not null) return (GateKind.Sell, null);
#endif
        if (compose.@gateRoom is { } room) return (GateKind.Room, room);
        if (compose.@gateTier is { } tier) return (GateKind.Tier, tier);
        return (GateKind.Public, null);
    }

    /// <summary>
    /// Show the snapshot's audience ANSWER in <c>compose-gate-tier-select</c> — called from
    /// <c>FeedPage.Refresh</c> on every observer tick, after <see cref="SetGateTiers"/> /
    /// <see cref="SetOwnRooms"/> have laid the options out. This is how a restored draft's
    /// tier, room or sale comes back on screen (`ui/feed.md` § Persistence → *Only
    /// user-authored input rests*; linux's <c>AudienceControls::paint</c>).
    ///
    /// <para>Paints only on a CHANGE of the snapshot's answer (<see cref="_paintedAnswer"/>).
    /// A tier or room the options do not offer yet — the reload that fills
    /// <c>own_tiers</c>, or the conversations state that fills <c>own_rooms</c>, still in
    /// flight — is left unpainted and retried on the next tick; the manager keeps the answer
    /// meanwhile, since nothing here forwards.</para>
    /// </summary>
    internal void PaintAudience(FeedComposeState compose)
    {
        var answer = AnswerOf(compose);
        if (_paintedAnswer == answer) return;
        var idx = ResolveGateIndex(answer);
        if (idx == 0 && answer.Kind != GateKind.Public) return;
        _paintedAnswer = answer;
        if (GateTierSelect.SelectedIndex == idx) return;
        _suppressAudienceForward = true;
        try { GateTierSelect.SelectedIndex = idx; }
        finally { _suppressAudienceForward = false; }
    }

    /// <summary>
    /// Put a restored draft's teaser and sale fields back into their boxes — the audience
    /// twin of <see cref="RestoreDraftIfEmpty"/>, and called beside it, with the same
    /// never-clobber rule: the teaser only into an empty box, the sale fields only while all
    /// three still stand at their fresh defaults. The ANSWER is <see cref="PaintAudience"/>'s
    /// (it may have to wait for its option to be offered); these are plain text the boxes can
    /// hold at once, shown or hidden.
    /// </summary>
    internal void RestoreAudienceFieldsIfUntouched(FeedComposeState compose)
    {
        _suppressAudienceForward = true;
        try
        {
            if (string.IsNullOrEmpty(GatePreviewBox.Text)) GatePreviewBox.Text = compose.@gatePreview;
#if PAYMENTS
            if (compose.@sell is { } sell && _sellFields.IsAtDefaults)
            {
                _sellFields.Price = sell.@price;
                _sellFields.AskingPrice = sell.@askingPrice;
                _sellFields.SubscribersGetItFree = sell.@subscribersGetItFree;
            }
#endif
        }
        finally
        {
            _suppressAudienceForward = false;
        }
    }

    // Draft-persistence v2, feed leg (reserved-folders.md § Drafts Sync): the
    // page — not this control — owns restore (it reads the manager's snapshot
    // after FeedDraftsService.RestoreOnLaunchAsync) and the debounced save (it
    // owns the FfiFeedManager this control has no reference to). This control's
    // only job is to report every USER text/tags edit via ComposeChanged.

    /// <summary>
    /// Apply a restored draft — but ONLY into an empty field (never clobber text
    /// the user is mid-typing; <c>FeedPage.Page_Loaded</c> can fire more than
    /// once per login, since a repeat "feed" nav command rebuilds the manager
    /// each time) — with <see cref="ComposeChanged"/> suppressed for the
    /// assignment itself, so the restore never re-forwards into a save.
    /// </summary>
    public void RestoreDraftIfEmpty(string text, string tags)
    {
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreDraftIfEmpty ENTER: current text len={ComposeText.Length}, tags len={TagsText.Length}, incoming text len={text.Length}, tags len={tags.Length}");
        _suppressComposeChanged = true;
        try
        {
            if (string.IsNullOrEmpty(ComposeText)) ComposeText = text;
            if (string.IsNullOrEmpty(TagsText)) TagsText = tags;
        }
        finally
        {
            _suppressComposeChanged = false;
        }
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreDraftIfEmpty EXIT: text len={ComposeText.Length}, tags len={TagsText.Length}");
    }

    private void ComposeOrTags_TextChanged(object sender, TextChangedEventArgs e)
    {
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] TextChanged fired: suppress={_suppressComposeChanged}, text len={ComposeText.Length}, tags len={TagsText.Length}");
        if (_suppressComposeChanged) return;
        ComposeChanged?.Invoke(ComposeText, TagsText);
    }

    /// <summary>
    /// Sets attachment data (called externally, e.g. from the picker or file injection —
    /// <c>ComposeFile_Click</c> and <c>App.xaml.cs</c>'s test-agent path alike). Holds the
    /// bytes locally for the submit's own seal + upload, and stages the pick on the manager
    /// AT ONCE as its hash-less handle (<see cref="AttachmentStaged"/>) — a null
    /// <c>mediaType</c> at pick, resolved only by the submit's own seal reply (media.md §
    /// Encryption at rest) — so a draft saved before the submit still carries the file by
    /// name (`ui/feed.md` § Persistence → Attachments by content address; mirrors tui's
    /// stage-at-pick shape).
    /// </summary>
    public void SetAttachment(byte[] bytes, string fileName)
    {
        _attachedBytes = bytes;
        AttachmentStaged?.Invoke(new AttachedFile(fileName, (ulong)bytes.Length, null, null));
    }

    /// <summary>Clears the LOCAL held bytes and hides the bar immediately — used right
    /// after a successful submit (<see cref="PostButton_Click"/>) while the staged file is
    /// still the one that was sent, alongside the text/tags boxes it clears beside: the
    /// manager's own compose already dropped
    /// <c>attached_file</c> via <c>clear_sent</c>, and the next observer tick's
    /// <see cref="RenderAttachedFile"/> would repaint the same thing, but clearing here too
    /// keeps the bar in step with the boxes rather than racing the next notify. NOT the
    /// user's remove gesture — that's <see cref="ComposeFileRemove_Click"/>, which also
    /// tells the manager.</summary>
    public void ClearAttachment()
    {
        _attachedBytes = null;
        RenderAttachedFile(null);
    }

    /// <summary>Render the <c>compose-file-ready</c> bar + its <c>compose-file-remove</c>
    /// control off the snapshot's staged attachment (`ui/feed.md` § Persistence →
    /// Attachments by content address) — a fresh local pick OR a restored draft's
    /// hash-less handle alike, never only a local pick (the pick-only painting this
    /// replaces). Called from <c>FeedPage.Refresh</c> on every observer tick.</summary>
    internal void RenderAttachedFile(AttachedFile? file)
    {
        if (file is null)
        {
            FileReadyBar.IsOpen = false;
            FileReadyBar.Visibility = Visibility.Collapsed;
            ComposeFileRemoveButton.Visibility = Visibility.Collapsed;
            return;
        }
        FileReadyBar.Message = $"{file.name} ({ValueFormat.ByteSize(file.size)})";
        FileReadyBar.IsOpen = true;
        FileReadyBar.Visibility = Visibility.Visible;
        ComposeFileRemoveButton.Visibility = Visibility.Visible;
    }

    /// <summary>The user's remove gesture (`compose-file-remove`) — drops the LOCAL bytes
    /// (a fresh pick has nothing left to upload) and stages <c>None</c> on the manager via
    /// <see cref="AttachmentStaged"/>, keeping the text/tags exactly as they stand (`ui/feed.md`
    /// § Persistence → Attachments by content address; mirrors android's
    /// <c>FeedVM.clearComposeAttachment</c>).</summary>
    private void ComposeFileRemove_Click(object sender, RoutedEventArgs e)
    {
        _attachedBytes = null;
        AttachmentStaged?.Invoke(null);
    }

    // ── Gate-to-tier (feed.md § Encryption at rest; monetization.md § Pillars 2+3) ──
    // Fixed layout: Public (index 0) → the author's own tiers → the author's own rooms
    // (`ui/feed.md` § Encryption at rest → Room-restricted — the app half, *The composer's
    // fourth answer*) → always-last "Sell this post…", which only a payments build offers
    // (dynamic-features.md § Platform-family surface excision → The price-and-route
    // class). Every answer resolves by the
    // SELECTED INDEX'S POSITION in that layout, never by the item's text: a tier can be
    // named exactly like a room option's "Room: ‹label›" text, and a name must never be
    // able to hijack another answer (mirrors linux's `GateOptions`, pinned by its own
    // `gate_options_keep_a_tier_named_like_a_room_option_a_tier` test).

    /// <summary>Rebuild the <c>compose-gate-tier-select</c> options from the CURRENT
    /// <see cref="_gateTierNames"/> / <see cref="_ownRooms"/>. Called by
    /// <see cref="SetGateTiers"/> and <see cref="SetOwnRooms"/>, each only on an actual
    /// change to its own list (mirrors linux post_list.rs) — clearing Items mid-interaction
    /// would disrupt an in-progress selection. The prior selection is preserved by KIND +
    /// KEY (tier name / room id), never by index or text, so a list that grew or shrank on
    /// the OTHER axis (a room joined while a tier list is unchanged, or vice versa) cannot
    /// silently move the selection onto a different answer. Each item carries its label in
    /// BOTH Content (the visible label) and AutomationProperties.Name — <c>driver.select</c>
    /// matches Name EXACTLY (reference_windows_flaui_select_exact_name).</summary>
    private void RebuildGateItems()
    {
        var current = CurrentGateSelection();
        _suppressAudienceForward = true;
        try { RebuildGateItemsCore(current); }
        finally { _suppressAudienceForward = false; }
    }

    private void RebuildGateItemsCore((GateKind Kind, string? Key) current)
    {
        GateTierSelect.Items.Clear();

        var publicItem = new ComboBoxItem { Content = S.Get("feed/post/gate_public") };
        AutomationProperties.SetName(publicItem, S.Get("feed/post/gate_public"));
        GateTierSelect.Items.Add(publicItem);

        foreach (var name in _gateTierNames)
        {
            var item = new ComboBoxItem { Content = name };
            AutomationProperties.SetName(item, name);
            GateTierSelect.Items.Add(item);
        }

        foreach (var room in _ownRooms)
        {
            var label = S.Get("feed/post/gate_room").Replace("{room}", room.@label);
            var item = new ComboBoxItem { Content = label };
            AutomationProperties.SetName(item, label);
            GateTierSelect.Items.Add(item);
        }

#if PAYMENTS
        var sellLabel = S.Get("feed/post/gate_sell");
        var sellItem = new ComboBoxItem { Content = sellLabel };
        AutomationProperties.SetName(sellItem, sellLabel);
        GateTierSelect.Items.Add(sellItem);
#endif

        GateTierSelect.SelectedIndex = ResolveGateIndex(current);
    }

    /// <summary>The currently-selected answer, read BEFORE <see cref="RebuildGateItems"/>
    /// clears <c>GateTierSelect.Items</c> — against the layout <see cref="_gateTierNames"/> /
    /// <see cref="_ownRooms"/> describe right now (i.e. call this before reassigning either
    /// backing field for the rebuild it precedes).</summary>
    private (GateKind Kind, string? Key) CurrentGateSelection()
    {
#if PAYMENTS
        if (IsSellSelected()) return (GateKind.Sell, null);
#endif
        if (SelectedGateRoom() is { } room) return (GateKind.Room, room);
        if (SelectedGateTier() is { } tier) return (GateKind.Tier, tier);
        return (GateKind.Public, null);
    }

    /// <summary>The item index <paramref name="selection"/> maps to under the layout
    /// <see cref="_gateTierNames"/> / <see cref="_ownRooms"/> describe NOW (i.e. call this
    /// AFTER updating whichever backing field changed) — 0 (Public) when the prior answer
    /// no longer exists (its tier was removed, or its room was left).</summary>
    private int ResolveGateIndex((GateKind Kind, string? Key) selection)
    {
        switch (selection.Kind)
        {
#if PAYMENTS
            case GateKind.Sell:
                return GateTierSelect.Items.Count - 1;
#endif
            case GateKind.Tier:
                for (var i = 0; i < _gateTierNames.Count; i++)
                {
                    if (_gateTierNames[i] == selection.Key) return i + 1;
                }
                return 0;
            case GateKind.Room:
                for (var i = 0; i < _ownRooms.Count; i++)
                {
                    if (_ownRooms[i].@room == selection.Key) return 1 + _gateTierNames.Count + i;
                }
                return 0;
            default:
                return 0;
        }
    }

    /// <summary>Rebuild the tier segment of <c>compose-gate-tier-select</c> from the
    /// author's own tier names (<c>snapshot.own_tiers</c>, which already excludes
    /// designated/sold-post tiers). Called on every feed observer tick; rebuilds ONLY on an
    /// actual tier-list change — but ALWAYS on the first call (Items empty), so the Public
    /// sentinel is seeded even when the initial tier set is empty (the constructor case
    /// where <see cref="_gateTierNames"/> and <paramref name="tierNames"/> are both
    /// empty).</summary>
    public void SetGateTiers(IReadOnlyList<string> tierNames)
    {
        if (GateTierSelect.Items.Count > 0 && _gateTierNames.SequenceEqual(tierNames)) return;
        _gateTierNames = tierNames.ToList();
        RebuildGateItems();
    }

    /// <summary>Rebuild the room segment of <c>compose-gate-tier-select</c> from the
    /// author's own rooms (<c>snapshot.own_rooms</c>) — the room sibling of
    /// <see cref="SetGateTiers"/>, called from the same feed-observer tick AND from the
    /// conversations plane's own change tick (<c>RoomsRefreshObserver</c>). Rebuilds only on
    /// an actual change; <see cref="GateRoomOption"/> is a record, so
    /// <see cref="Enumerable.SequenceEqual{TSource}(IEnumerable{TSource}, IEnumerable{TSource})"/>
    /// compares by value.</summary>
    internal void SetOwnRooms(IReadOnlyList<GateRoomOption> rooms)
    {
        if (GateTierSelect.Items.Count > 0 && _ownRooms.SequenceEqual(rooms)) return;
        _ownRooms = rooms.ToList();
        RebuildGateItems();
    }

    private void GateTierSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        UpdateGatePreviewVisibility();
        UpdateSellFieldsVisibility();
        if (!_suppressAudienceForward && GateTierSelect.SelectedIndex >= 0)
            AudienceChanged?.Invoke(CurrentAudience());
    }

    private void UpdateGatePreviewVisibility()
    {
        // The public teaser is meaningful for either gated answer — a tier OR sell (both are
        // index > 0, the Public sentinel is index 0) — mirrors linux's gate row.
        GatePreviewBox.Visibility = GateTierSelect.SelectedIndex > 0
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    private void UpdateSellFieldsVisibility()
    {
        SellFieldsHost.Visibility = IsSellSelected() ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>True iff "Sell this post…" (always the last item, added by
    /// <see cref="RebuildGateItemsCore"/> under <c>PAYMENTS</c>) is selected — never in a
    /// store-safe build, which offers no such answer.</summary>
    private bool IsSellSelected()
#if PAYMENTS
        => GateTierSelect.Items.Count > 0
           && GateTierSelect.SelectedIndex == GateTierSelect.Items.Count - 1;
#else
        => false;
#endif

    /// <summary>The selected tier name, or <c>null</c> for the "Public" sentinel (index 0), a
    /// room answer, or sell mode. Resolved by the selected index's POSITION against
    /// <see cref="_gateTierNames"/> — never by the item's text, which a room option's "Room:
    /// ‹label›" text could otherwise collide with (see the layout note above
    /// <see cref="RebuildGateItems"/>).</summary>
    private string? SelectedGateTier()
    {
        var idx = GateTierSelect.SelectedIndex;
        return idx >= 1 && idx <= _gateTierNames.Count ? _gateTierNames[idx - 1] : null;
    }

    /// <summary>The selected room's hex channel id, or <c>null</c> unless the selected index
    /// falls in the room segment (after the tiers, before "Sell this post…") — the room
    /// sibling of <see cref="SelectedGateTier"/>, same position-not-text resolution
    /// (`ui/feed.md` § Encryption at rest → Room-restricted — the app half).</summary>
    private string? SelectedGateRoom()
    {
        var idx = GateTierSelect.SelectedIndex;
        var roomStart = _gateTierNames.Count + 1;
        var roomOffset = idx - roomStart;
        return roomOffset >= 0 && roomOffset < _ownRooms.Count ? _ownRooms[roomOffset].@room : null;
    }

    private async void PostButton_Click(object sender, RoutedEventArgs e)
    {
        var text = ComposeText;
        var tags = TagsBox.Text;
        if (string.IsNullOrWhiteSpace(text)) return;

        // Gate state: null tier/room = a normal post; a tier name or a room id gates it, with
        // the preview field's text as the public teaser (empty preview is allowed — the
        // manager validates). Sell mode ignores gateTier/gateRoom entirely (all three are
        // mutually exclusive by construction — the same selected index resolves at most one).
        var audience = CurrentAudience();
        var gateTier = audience.GateTier;
        var gateRoom = audience.GateRoom;
        var gatePreview = audience.GatePreview;
        var sell = audience.Sell;
        // The picked bytes as they stood at the press, held by reference: a pick made after
        // the click is new input even when it carries the same file name.
        var sentBytes = _attachedBytes;

        var succeeded = true;
        if (PostRequested is not null)
        {
            succeeded = await PostRequested.Invoke(text, tags, _attachedBytes, audience);
        }
        if (!succeeded)
        {
            // Refused (empty text / an unresolved attachment / an aborted upload) — the
            // shared manager kept the draft exactly as the author left it (`ui/feed.md`
            // § Persistence → Attachments by content address). Clearing the boxes here
            // would throw away content the refusal explicitly preserved.
            return;
        }

        // Clear what was SENT — not whatever is on screen now. The await above spans an
        // upload and a nest round trip while the composer stays editable (`ui/feed.md` §
        // User actions, `post-submit-button`), so the boxes may already hold the next
        // post. A field still holding its sent value IS the sent value and clears; a field
        // the user changed since the click is new input and stays. The same per-field rule
        // as the shared `FeedComposeState::clear_sent` (the manager applied it to its own
        // compose already) and web's `compose-sent`; blanking every box unconditionally
        // threw the next post's text away — and reset its audience to Public — with no
        // error anywhere.
        //
        // ComposeBox must still be cleared HERE when it holds the sent text: the post text
        // flows ComposeText → PostRequested directly (not through VM.ComposeText), so
        // the VM's `ComposeText = ""` is a no-op (value already "", no PropertyChanged)
        // and never reaches this TextBox. Without this, each successive post accumulates
        // the prior text (AAA, AAABBB, AAABBBCCC).
        var textSent = ComposeText == text;
        var tagsSent = TagsBox.Text == tags;
        var attachmentSent = ReferenceEquals(_attachedBytes, sentBytes);
        if (textSent) ComposeBox.Text = string.Empty;
        if (tagsSent) TagsBox.Text = string.Empty;
        if (attachmentSent) ClearAttachment();
        // The audience group — the tier / room / sale answer, its teaser and its price — is
        // STICKY (owner ruling, mirrored from `clear_sent`): it clears only when the
        // composer is otherwise untouched, so a user who has already started the next post
        // keeps the audience they picked instead of having it silently widened to Public.
        // Each field still clears only if it holds what was sent.
        if (textSent && tagsSent && attachmentSent)
        {
            var answerSent = SelectedGateTier() == gateTier && SelectedGateRoom() == gateRoom
                && IsSellSelected() == (sell is not null);
            if (GatePreviewBox.Text == gatePreview) GatePreviewBox.Text = string.Empty;
#if PAYMENTS
            if (SellFieldsHold(sell)) _sellFields.Reset(); // the ratified rank knob defaults ON
#endif
            if (answerSent) GateTierSelect.SelectedIndex = 0; // back to the Public sentinel
        }
        // Sent → any box cleared above already fired ComposeChanged with the cleared text,
        // live-forwarding it into the manager; flush the composer to the nest NOW
        // (draft-persistence v2) rather than waiting on the debounce, so a later quit
        // doesn't restore stale text. A box the user kept typing in is flushed as it stands.
        _ = App.FeedDrafts?.SaveNowAsync();
    }

#if PAYMENTS
    /// <summary>True iff the sale fields still hold what a submit sent — <c>sell</c> null is
    /// a non-sale submit, which the fields still hold only while no sale is selected. The
    /// sale half of <see cref="PostButton_Click"/>'s clear-only-what-was-sent rule.</summary>
    private bool SellFieldsHold((string Price, string AskingPrice, bool SubscribersGetItFree)? sell)
        => sell is { } s
            ? StagedSale() is { } staged && staged == s
            : !IsSellSelected();
#endif

    private async void ComposeFile_Click(object sender, RoutedEventArgs e)
    {
        if (App.MainWindow is null) return;
        try
        {
            var picker = new Windows.Storage.Pickers.FileOpenPicker();
            picker.SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.PicturesLibrary;
            picker.FileTypeFilter.Add(".jpg");
            picker.FileTypeFilter.Add(".jpeg");
            picker.FileTypeFilter.Add(".png");
            picker.FileTypeFilter.Add(".gif");
            picker.FileTypeFilter.Add(".webp");

            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(App.MainWindow);
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);

            var file = await picker.PickSingleFileAsync();
            if (file is not null)
            {
                var buffer = await Windows.Storage.FileIO.ReadBufferAsync(file);
                var bytes = new byte[buffer.Length];
                using var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer);
                reader.ReadBytes(bytes);
                SetAttachment(bytes, file.Name);
            }
        }
        catch (System.Exception ex)
        {
            ShellLog.Warn("FeedComposeBar",
                $"attachment pick failed: {ex.Message}");
        }
    }

    private void ComposeDialogButton_Click(object sender, RoutedEventArgs e)
    {
        ComposeDialogRequested?.Invoke();
    }
}
