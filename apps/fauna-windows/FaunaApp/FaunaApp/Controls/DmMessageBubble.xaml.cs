using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using FaunaApp.Core.Services;
using FaunaApp.Helpers;
using uniffi.fauna_conversations;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using FaunaApp.UiIds;

namespace FaunaApp.Controls;

/// <summary>
/// The fields <see cref="DmMessageBubble"/> renders, projected from the shared
/// <c>MessageSnapshot</c> (UniFFI). A dedicated view record — not the named-pipe
/// IPC wire DTO — so the rendering surface carries no CBOR-codec baggage.
/// <c>From</c> is the resolved display name; <c>Id</c> is the reply target. The body
/// AND its attachments both ride the structured <c>MessageSnapshot.document</c> (set via
/// the internal <c>Document</c> property), so neither leaks a UniFFI snapshot type through
/// this public record (render-model.md § D1/§ D2).
/// </summary>
public record DmMessageView(
    string Id,
    string From,
    bool SignatureValid,
    bool Encrypted,
    string Timestamp,
    bool IsOwn,
    bool Deleted,
    IReadOnlyList<ReactionGroupVm> Reactions,
    // Set (non-null) iff the nest took this message down under a legal obligation
    // (MessageSnapshot.legalTakedownRef; moderation.md § Categories & enforcement item 1).
    // The bubble collapses to the shared tombstone in place of the withheld body.
    string? LegalTakedownRef = null,
    // True iff the decrypted body matched the client-cached muted-keyword list and
    // hasn't been revealed this session (moderation.md § Muted keywords;
    // content-moderation-and-ranking.md § Q3) — computed by
    // ConversationsPage.ToMessageView via MutedKeywordsCache.IsMuted (client-only;
    // the nest never sees the list or this flag). A UI-projection bool, not raw
    // body text, per render-model.md § D1's "the bubble surface is projection-
    // only" rule. The bubble collapses to a session-local reveal placeholder.
    bool Muted = false,
    // The shared content-policy render verdict for this message's post-decrypt
    // MessageSnapshot.labels — "show" | "badge" | "collapse" | "block", composed
    // strictest-wins in shared Rust (family-safety.md § Content policy). Computed by
    // ConversationsPage.ToMessageView via ContentPolicyCache.VerdictFor, exactly like
    // Muted above: a UI-projection primitive, never the raw labels (ContentLabelEntry
    // is UniFFI-internal and this record is public). "show" = unsupervised viewer /
    // nothing hydrated yet, which is also the wire-safe default.
    string ContentVerdict = "show",
    // Whether the viewer tapped "show anyway" on this message's content-policy
    // COLLAPSE this session (ContentPolicyCache.IsRevealed). Consulted only for the
    // collapse arm — a "block" is checked ahead of it and is never revealable.
    bool ContentRevealed = false,
    // The region placeholder when the REGION content policy drove ContentVerdict
    // (region-blocking.md § The blocked render) — from the same one
    // ContentPolicyCache.RenderFor call, painted ahead of every family arm. A public
    // Core record, so this public view carries no UniFFI-internal type.
    RegionPlaceholderModel? Region = null,
    // The `SearchNav.Mail` deep-link marker (conversations.md § The selected
    // message — built 2026-08-10, tui lead; windows closes the set): read-time-resolved on
    // `ThreadDetail.selectedMessageId`, painted as the shared `selected`
    // attribute on `dm-message-timestamp` (the one child every render arm
    // paints, including the withheld ones) rather than a new element id.
    bool Selected = false,
    // The viewer's OWN report hid this message (moderation.md § Corollary — block
    // also hides): the content-policy block arm then paints "You reported this"
    // instead of the family-policy sentence. From the same one
    // ContentPolicyCache.RenderFor call as ContentVerdict.
    bool Reported = false,
    // Whether the ⋯ menu offers `dm-message-report-button`: a RECEIVED message with a
    // plane identity to report it against (mail and bridged messages have none).
    bool CanReport = false);

/// <summary>
/// One reaction group projected from the shared <c>MessageSnapshot.reactions</c>
/// (UniFFI <c>ReactionGroup</c>): an emoji, its aggregate <c>Count</c>, and
/// whether the local actor is in the group (<c>Highlighted</c> ⇐ <c>reactedByMe</c>,
/// drives the pill's highlight + a re-tap toggling it off). A PUBLIC record so the
/// bubble's public surface carries no <c>internal</c> UniFFI type (the same reason
/// <c>Document</c> is <c>internal</c>). conversations.md § Reactions &amp; message delete.
/// </summary>
public record ReactionGroupVm(string Emoji, uint Count, bool Highlighted);

/// <summary>
/// Displays a single DM message bubble with sender info, a structured body, indexed
/// image/file attachments, and reply button — body and attachments both walked from the
/// shared <c>MessageSnapshot.document</c> (render-model.md § D1/§ D2), never a sibling field.
/// </summary>
public sealed partial class DmMessageBubble : UserControl
{
    private DmMessageView? _message;

    /// <summary>The structured render document currently painted, kept so the
    /// load-remote-content click can re-render it in fetch mode. Set from the shared
    /// <c>MessageSnapshot.document</c> (render-model.md § D1) — the bubble walks it,
    /// never re-parsing the body.</summary>
    private RenderDocument _document = new(System.Array.Empty<RenderBlock>());

    /// <summary>Raised when user clicks the reply button.</summary>
    public Action<DmMessageView>? ReplyRequested;

    /// <summary>Raised when user clicks the reply-all button (mail only).</summary>
    public Action<DmMessageView>? ReplyAllRequested;

    /// <summary>Raised when user clicks the <c>load-remote-content-button</c>. The page
    /// dispatches this to <c>ConversationsManager.RevealRemoteImages(messageId)</c> —
    /// the manager flips the in-memory reveal set + re-emits; the next
    /// <c>ThreadDetail</c> projects <c>RemoteImage.revealed=true</c> for this message
    /// and Refresh re-binds (render-model.md § D3). Mirrors <see cref="ReplyRequested"/>.</summary>
    public Action<DmMessageView>? RemoteRevealRequested;

    /// <summary>Raised when user clicks the <c>dm-message-muted-reveal-button</c>
    /// (moderation.md § Muted keywords). Unlike <see cref="RemoteRevealRequested"/>
    /// (server/manager-delegated), the muted-keyword reveal is purely client-local
    /// — the page marks <c>MutedKeywordsCache.Reveal(messageId)</c> and re-renders
    /// directly, no dispatch to shared Rust (the mute list + reveal set are never
    /// shared foundation state).</summary>
    public Action<DmMessageView>? MutedRevealRequested;

    /// <summary>Raised when the user clicks the content-policy collapse's "show
    /// anyway" button (family-safety.md § Content policy). The page marks
    /// <c>ContentPolicyCache.Reveal(messageId)</c> and re-renders — client-local,
    /// like <see cref="MutedRevealRequested"/>, never a shared-Rust dispatch (the
    /// reveal set is session state; the floor itself persists until the guardian
    /// relaxes it). A <c>block</c> raises this never: the gate resolves block ahead
    /// of the reveal set, so the button is not painted for one.</summary>
    public Action<DmMessageView>? ContentRevealRequested;

    /// <summary>
    /// Whether to show the <c>dm-reply-all-button</c> — set by the page to the
    /// thread's <c>supports_recipient_selection</c> capability (mail = true).
    /// Collapsed → no UIA peer, so the e2e counts 0 on a FaunaMls thread. Never
    /// branches on rail directly (conversations.md § Architectural rules #5).
    /// </summary>
    public bool ShowReplyAll
    {
        get => ReplyAllButton.Visibility == Visibility.Visible;
        set => ReplyAllButton.Visibility = value ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// Whether this thread supports emoji reactions — set by the page to the
    /// thread's <c>supports_reactions</c> capability (FaunaMls = true). Gates the
    /// ⋯ flyout's quick-set <c>dm-reaction-option</c> items + the
    /// <c>dm-reaction-more-button</c> + the under-bubble pills. NEVER branches on
    /// the rail enum (conversations.md § Architectural rules #5).
    /// </summary>
    public bool SupportsReactions { get; set; }

    /// <summary>
    /// Whether this thread supports sender-only message delete — set by the page
    /// to the thread's <c>supports_message_delete</c> capability (FaunaMls = true).
    /// Combined with <see cref="DmMessageView.IsOwn"/>, gates the
    /// <c>dm-message-delete-button</c> + its confirm. NEVER branches on the rail
    /// enum (conversations.md § Architectural rules #5).
    /// </summary>
    public bool SupportsMessageDelete { get; set; }

    /// <summary>Raised when the user picks an emoji to toggle on this message —
    /// from a quick-set <c>dm-reaction-option</c>, the more-grid, or a tap on an
    /// existing <c>dm-reaction-pill</c>. The page dispatches to
    /// <c>ConversationsManager.toggle_reaction</c>; the manager applies the
    /// optimistic Add/Remove + re-emits, and the next Refresh re-binds this bubble
    /// with the updated pills (NO local optimistic flip). Mirrors
    /// <see cref="ReplyRequested"/>. conversations.md § Reactions &amp; message
    /// delete.</summary>
    public Action<(DmMessageView msg, string emoji)>? ReactionToggleRequested;

    /// <summary>Raised when the user confirms deleting this (own) message
    /// (<c>dm-message-delete-confirm-button</c>). The page dispatches to
    /// <c>ConversationsManager.delete_message</c> (sender-only; the manager
    /// rejects a non-own target); the manager tombstones + re-emits and Refresh
    /// re-binds this bubble showing the deleted placeholder. Mirrors
    /// <see cref="ReplyRequested"/>.</summary>
    public Action<DmMessageView>? DeleteRequested;

    /// <summary>Raised when the user picks <c>dm-message-mark-as-spam-button</c> on a
    /// RECEIVED message (mail-spam.md § Wire shapes — the live <c>Insert</c> consumer).
    /// The page dispatches to
    /// <c>ConversationsViewModel.MarkMessageSpamAsync</c> with the retained decrypted
    /// body + subject line (not carried on this view — the page reads them off the
    /// source <c>MessageSnapshot</c> it already holds), which trains the caller's
    /// sealed spam model and writes a sealed <c>spam_training_history</c> row via the
    /// shared <c>MailSettingsMachine::train_spam_model_client_mail</c>. Mirrors
    /// <see cref="DeleteRequested"/>; unlike delete this button is offered on a
    /// received message (<c>!IsOwn</c>), never an own one.</summary>
    public Action<DmMessageView>? MarkAsSpamRequested;

    /// <summary>Raised when the user picks <c>dm-message-report-button</c> on a received
    /// message that can be reported (<see cref="DmMessageView.CanReport"/>;
    /// moderation.md § User-initiated reporting). The page builds the report target
    /// off the source <c>MessageSnapshot</c> it holds (its plane ref and sender are
    /// not carried on this UI-projection view) and opens the shared report sheet.
    /// Reaches a human with authority, where mark-as-spam only trains the reporter's
    /// own model.</summary>
    public Action<DmMessageView>? ReportRequested;

    /// <summary>Raised when user taps an attachment image.</summary>
    public Action<BitmapImage>? ImageClicked;

    /// <summary>The message's structured body document
    /// (<c>MessageSnapshot.document</c>, produced once by the manager — render-model.md
    /// § D1/§ D2). The page sets this before <see cref="Message"/> so it is ready when
    /// <see cref="Bind"/> runs. The bubble walks it for BOTH the body text and the
    /// first-class <c>Attachment</c> blocks the manager folded in after the body — there is
    /// no sibling attachments field on this bubble. <c>internal</c> because the
    /// UniFFI-generated <see cref="RenderDocument"/> is emitted <c>internal</c>.</summary>
    internal RenderDocument? Document { get; set; }

    /// <summary>The message's content-label entries (<c>MessageSnapshot.labels</c>,
    /// populated by the manager's post-decrypt classify pass — moderation.md § Per-row
    /// badge data path). The page sets this before <see cref="Message"/> like
    /// <see cref="Document"/>. <c>internal</c> because the UniFFI-generated
    /// <see cref="ContentLabelEntry"/> is emitted <c>internal</c>.</summary>
    internal ContentLabelEntry[]? Labels { get; set; }

    /// <summary>Resolves an attachment's <c>blob_hash</c> to its plaintext bytes via
    /// the shared <c>ConversationsManager.attachment_bytes</c> (the in-memory store
    /// the inbound parse / send echo populated). Image attachments decode these into
    /// a <see cref="BitmapImage"/>; <c>null</c> for a not-yet-fetched hash (e.g. a
    /// FaunaMls nest blob — the GET+decrypt rail is a follow-on). Set by the page
    /// alongside <see cref="Attachments"/>.</summary>
    public Func<string, byte[]?>? AttachmentBytesResolver { get; set; }

    /// <summary>The blob loader for the link-preview og:image (render-model.md § D4) — the
    /// SAME authenticated <c>/api/v1/blob/&lt;hash&gt;</c> nest GET the feed card uses
    /// (<see cref="BlobImageLoader"/>). Set by the page before <see cref="Message"/>. The
    /// og:image obeys the message's D3 reveal (its hash is withheld until revealed), so this
    /// only ever loads a revealed, nest-served content-addressed blob — never a third-party
    /// fetch. <c>null</c> in E2E / when no nest client exists (the element stays realized via
    /// <c>ManageVisibility=False</c>).</summary>
    public BlobImageLoader? LinkPreviewImageLoader { get; set; }

    public static readonly DependencyProperty MessageProperty =
        DependencyProperty.Register(nameof(Message), typeof(DmMessageView),
            typeof(DmMessageBubble), new PropertyMetadata(null, OnMessageChanged));

    public DmMessageView? Message
    {
        get => (DmMessageView?)GetValue(MessageProperty);
        set => SetValue(MessageProperty, value);
    }

    private static void OnMessageChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is DmMessageBubble bubble && bubble.Message is not null)
            bubble.Bind(bubble.Message);
    }

    public DmMessageBubble()
    {
        this.InitializeComponent();
        // Convention 17's walk counts on-screen bubbles only: drop this bubble's
        // registrations when it leaves the tree; a bubble that returns re-binds.
        Unloaded += (_, _) =>
        {
            if (_witnessed is { } key) RegionPlaneHost.Forget(key);
            _witnessed = null;
        };
        Loaded += (_, _) =>
        {
            if (_witnessed is null && _message is { } m) Bind(m);
        };
    }

    private static string WitnessKey(DmMessageView msg) => "dm:" + msg.Id;

    /// The key this bubble's convention-17 registrations are held under.
    private string? _witnessed;

    /// <summary>Convention 17's "a region Block never renders silent" walk for this
    /// bubble (<c>RegionPlaneHost.BlockRenderState</c>): the verdict side from the
    /// message's composed decision, registered before any arm paints; the painted
    /// side only by the region arm itself, when it shows a block. A bubble is reused
    /// across re-binds, so the previous message's registrations go first.</summary>
    private void WitnessRegion(DmMessageView msg)
    {
        var key = WitnessKey(msg);
        if (_witnessed is { } prev && prev != key) RegionPlaneHost.Forget(prev);
        _witnessed = key;
        RegionPlaneHost.WitnessBlocked(key, msg.Region is { IsBlock: true });
        RegionPlaneHost.WitnessPainted(key, false);
    }

    /// <summary>
    /// Binds a message view to this bubble, setting text/badges then walking the structured
    /// <c>MessageSnapshot.document</c> (<see cref="Document"/>) for both the body and its
    /// first-class <c>Attachment</c> blocks (render-model.md § D1/§ D2) — NOT the legacy
    /// [attachment:HASH:mime] body-marker or a sibling attachments field.
    /// </summary>
    public void Bind(DmMessageView msg)
    {
        _message = msg;

        SenderText.Text = msg.From;
        MessageTimestampText.Text = msg.Timestamp;
        // The selected-message marker — set here (before the withheld-arm
        // early-returns below) because dm-message-timestamp is the one child
        // every arm paints. Mirrors RecipientPicker.UpdateResolveState's
        // HelpText idiom (get_attr(id, "state") — UIA has no first-class Tag
        // surface, and this is the shared "always-present, never a new
        // element id" contract, not the "disabled" attribute FlaUI derives
        // natively).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            MessageTimestampText, msg.Selected ? "true" : "false");
        // The visual half of the same mark — an amber ring, cross-app
        // consistent with linux/android. An outline (BorderThickness), not a
        // fill, so it doesn't compete with the bubble's own content.
        SelectionRing.BorderBrush = msg.Selected
            ? new Microsoft.UI.Xaml.Media.SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xF5, 0x9E, 0x0B))
            : null;
        SelectionRing.BorderThickness = new Thickness(msg.Selected ? 2 : 0);

        // WHICH arm withholds this bubble is ONE decision, shared with the feed
        // post-card (FaunaApp.Core SocialRenderGate — unit-tested there, so the
        // ordering can't silently regress behind a running WinUI tree). Order:
        // deleted → legal takedown → content-policy BLOCK → muted → content-policy
        // COLLAPSE → live. The block sits ahead of the muted arm and ahead of every
        // reveal set, so a message that is both blocked and muted can never be
        // revealed past the guardian's floor (family-safety.md § Content policy).
        var arm = SocialRenderGate.Decide(
            msg.ContentVerdict, msg.ContentRevealed, msg.Muted, msg.Deleted, msg.LegalTakedownRef,
            regionVerb: msg.Region?.Verb);
        WitnessRegion(msg);

        // Deleted tombstone (conversations.md § Reactions & message delete): show the
        // placeholder, COLLAPSE every live surface (header/body/remote-content/
        // attachments/reaction-pills/actions row) so none carries a UIA peer, and
        // return early — skipping RenderAttachments/RenderBody/the actions flyout.
        if (arm == SocialRenderArm.Deleted)
        {
            CollapseAllArms();
            DeletedPlaceholder.Visibility = Visibility.Visible;
            return;
        }
        // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1): the
        // nest withheld this message's sealed envelope under a legal obligation, so collapse
        // the bubble exactly like the deleted tombstone and paint the shared localized
        // tombstone in place of the (withheld, empty) body — never a blank/failed-decrypt
        // bubble. Mirrors web `+page.svelte`'s legal_takedown_ref arm + linux
        // message_bubble.rs's early-return; the reference resolves through the shared
        // FaunaFfiMethods.LegalTakedownTombstone face (no per-app string, priority #1/#2).
        // Outranks the content policy: a takedown is a legal obligation on the nest, not a
        // family preference the guardian could relax.
        if (arm == SocialRenderArm.LegalTakedown)
        {
            CollapseAllArms();
            LegalTakedownPlaceholder.Text =
                Strings.Resolve(FaunaFfiMethods.LegalTakedownTombstone(msg.LegalTakedownRef!));
            LegalTakedownPlaceholder.Visibility = Visibility.Visible;
            return;
        }
        // REGION placeholder (region-blocking.md § The blocked render): the region
        // content policy withholds this message — frame, authority, reason verbatim,
        // in place of the body; a `collapse` adds the reveal. Checked ahead of the
        // family block: same verb, better attributed (SocialRenderGate).
        if (arm == SocialRenderArm.RegionWithheld && msg.Region is { } region)
        {
            CollapseAllArms();
            RegionNoticeText.Text = region.NoticeText;
            RegionAuthorityText.Text = region.AuthorityName;
            RegionReasonText.Text = region.Reason;
            // Name beside the AutomationId, or FlaUI does not realize the peer
            // (reference_winui_flaui_datatemplate_name) — set per bind, never static.
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(RegionNoticeText, region.NoticeText);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(RegionAuthorityText, region.AuthorityName);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(RegionReasonText, region.Reason);
            RegionRevealButton.Visibility = region.IsBlock ? Visibility.Collapsed : Visibility.Visible;
            RegionPlaceholderPanel.Visibility = Visibility.Visible;
            RegionPlaneHost.WitnessPainted(WitnessKey(msg), region.IsBlock);
            return;
        }
        // Content-policy BLOCK (family-safety.md § Content policy): the guardian's
        // per-category floor blocks this message's labels. Collapse like the tombstones
        // above and paint the policy-NAMING notice — a blocked message never silently
        // disappears — with NO reveal button, unlike the two collapse arms below.
        // Mirrors linux message_bubble.rs's content_verdict == Block early-return.
        if (arm == SocialRenderArm.ContentBlocked)
        {
            CollapseAllArms();
            // The words follow the cause: the viewer's own report ("You reported
            // this") or the guardian's floor. Set per bind, Name beside Text, or
            // FlaUI reads the stale static sentence.
            var blockedText = Strings.Get(msg.Reported
                ? "moderation/report/hidden_placeholder"
                : "family/content_blocked_notice");
            ContentBlockedPlaceholder.Text = blockedText;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ContentBlockedPlaceholder, blockedText);
            ContentBlockedPlaceholder.Visibility = Visibility.Visible;
            return;
        }
        // Muted-word collapse (moderation.md § Muted keywords;
        // content-moderation-and-ranking.md § Q3): the decrypted body matched the
        // client-cached muted-keyword list and hasn't been revealed this session.
        // Collapse exactly like the tombstones above and show the reveal button;
        // unlike Deleted/LegalTakedown/ContentBlocked this is NOT permanent —
        // MutedRevealButton_Click marks the message revealed for the session and
        // the next Refresh re-binds it live.
        if (arm == SocialRenderArm.Muted)
        {
            CollapseAllArms();
            MutedPlaceholder.Visibility = Visibility.Visible;
            MutedRevealButton.Visibility = Visibility.Visible;
            return;
        }
        // Content-policy COLLAPSE: the same revealable shape as the muted arm but its
        // OWN session set (ContentPolicyCache, keyed on the message id). Presentation
        // only — no ui.yaml id, matching the linux leg (v1 e2e drives the block case).
        if (arm == SocialRenderArm.ContentCollapsed)
        {
            CollapseAllArms();
            ContentCollapsedPlaceholder.Visibility = Visibility.Visible;
            ContentRevealButton.Visibility = Visibility.Visible;
            return;
        }
        // A live message: ensure the surfaces a prior withheld bind collapsed are
        // restored (bubbles are reused across Refresh re-binds).
        HidePlaceholders();
        HeaderRow.Visibility = Visibility.Visible;
        BodyView.Visibility = Visibility.Visible;
        ActionsRow.Visibility = Visibility.Visible;

        // Signature badges
        if (msg.Encrypted)
            EncryptedBadge.Visibility = Visibility.Visible;
        if (msg.SignatureValid)
        {
            VerifiedBadge.Visibility = Visibility.Visible;
            SignatureText.Text = Strings.Get("common/verified");
        }
        else
        {
            SignedBadge.Visibility = Visibility.Visible;
            SignatureText.Text = msg.SignatureValid.ToString();
        }

        // Set the document first: both the attachment walk (RenderAttachments) and the body
        // walk (RenderBody) source from it (render-model.md § D1/§ D2).
        _document = Document ?? new RenderDocument(System.Array.Empty<RenderBlock>());
        RenderAttachments();
        RenderBody();
        RenderRemoteImages();
        RenderQuote();
        RenderLinkPreview();
        RenderContentLabel();
        RenderReactionPills(msg);
        BuildActionsFlyout(msg);
    }

    /// <summary>Collapse EVERY placeholder and every live surface — the shared prelude
    /// of all five withholding arms, so the arm that won only has to make its own
    /// placeholder visible. Bubbles are reused across Refresh re-binds, so this must
    /// leave nothing from a previous bind realized: a stale visible surface would leak a
    /// UIA peer (and, worse, a body) onto a message that is now withheld.</summary>
    private void CollapseAllArms()
    {
        HidePlaceholders();
        HeaderRow.Visibility = Visibility.Collapsed;
        BodyView.Visibility = Visibility.Collapsed;
        QuoteCard.Visibility = Visibility.Collapsed;
        LinkPreviewCards.Children.Clear();
        ContentLabelBadgeBorder.Visibility = Visibility.Collapsed;
        LoadRemoteContentButton.Visibility = Visibility.Collapsed;
        AttachmentsList.Children.Clear();
        ReactionPillsList.Children.Clear();
        ActionsRow.Visibility = Visibility.Collapsed;
    }

    /// <summary>Collapse the five withholding placeholders (+ their two reveal
    /// buttons). Every arm calls this so exactly one placeholder is ever visible.</summary>
    private void HidePlaceholders()
    {
        RegionPlaceholderPanel.Visibility = Visibility.Collapsed;
        RegionRevealButton.Visibility = Visibility.Collapsed;
        DeletedPlaceholder.Visibility = Visibility.Collapsed;
        LegalTakedownPlaceholder.Visibility = Visibility.Collapsed;
        ContentBlockedPlaceholder.Visibility = Visibility.Collapsed;
        MutedPlaceholder.Visibility = Visibility.Collapsed;
        MutedRevealButton.Visibility = Visibility.Collapsed;
        ContentCollapsedPlaceholder.Visibility = Visibility.Collapsed;
        ContentRevealButton.Visibility = Visibility.Collapsed;
    }

    /// <summary>Content label badge (spam/phishing/NSFW) — the highest-confidence entry
    /// of <see cref="Labels"/>, styled entirely from the shared content_label_style map
    /// (moderation.md § Per-row badge data path). Mirrors the feed post-card's x:Bind
    /// wiring (FeedViewModel.cs's FeedPostItem ctor) and the moderation queue's
    /// ModerationViewModel.MapRow — the SAME two shared-Rust calls, so a message carries
    /// an identical badge wherever it renders.</summary>
    private void RenderContentLabel()
    {
        var primary = FaunaFfiMethods.PrimaryContentLabel(Labels ?? System.Array.Empty<ContentLabelEntry>());
        if (primary is null)
        {
            ContentLabelBadgeBorder.Visibility = Visibility.Collapsed;
            return;
        }
        var style = FaunaFfiMethods.ContentLabelStyle(primary.category);
        var text = Strings.Resolve(style.label);
        ContentLabelIconText.Text = style.icon;
        ContentLabelText.Text = text;
        ContentLabelText.Foreground = HexColorToBrushConverter.FromHex(style.accent);
        ContentLabelBadgeBorder.Background = HexColorToBrushConverter.FromHex(style.tint, 38);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ContentLabelBadgeBorder, text);
        ContentLabelBadgeBorder.Visibility = Visibility.Visible;
    }

    // ── Reactions & message delete (conversations.md § Reactions & message delete) ──

    private static IReadOnlyList<string>? _quickSetEmoji;

    /// <summary>The fixed quick-set emoji, in the canonical order shared with every
    /// app (the e2e indexes <c>dm-reaction-option</c> in THIS order) — the shared
    /// <c>fauna_conversations::QUICKSET_EMOJIS</c> face (<c>FaunaConversationsMethods
    /// .QuicksetEmojis()</c>), not a hand-kept literal; windows was the last of the seven
    /// apps carrying its own copy (conversations.md § Where logic lives). Lazily read on
    /// first use rather than a static-initializer field: android hit the same FFI-load-order
    /// question and answered it with `by lazy` — the native library may not be up yet when
    /// this type's static initializer would otherwise run.</summary>
    private static IReadOnlyList<string> QuickSetEmoji =>
        _quickSetEmoji ??= FaunaConversationsMethods.QuicksetEmojis();

    private static IReadOnlyList<string>? _moreGridEmoji;

    /// <summary>The twenty-emoji shortcut grid shown beneath the fuller picker's free-entry
    /// field — the shared <c>fauna_conversations::MORE_GRID_EMOJIS</c> face
    /// (<c>FaunaConversationsMethods.MoreGridEmojis()</c>), read lazily for the same
    /// FFI-load-order reason as <see cref="QuickSetEmoji"/>. Its cells are plain buttons
    /// WITHOUT the <c>dm-reaction-option</c> id (they never inflate the fixed-6 quick-set
    /// count the e2e relies on), routing the same toggle path.</summary>
    private static IReadOnlyList<string> MoreGridEmoji =>
        _moreGridEmoji ??= FaunaConversationsMethods.MoreGridEmojis();

    /// <summary>(Re)build the <c>dm-reaction-pill</c> row from the message's reaction
    /// groups (the manager-folded <c>MessageSnapshot.reactions</c>). Each pill is a
    /// small always-enabled Button carrying the indexed id + an
    /// <c>AutomationProperties.Name</c> (<c>"{emoji} {count}"</c>) so FlaUI counts it
    /// (reference_winui_flaui_datatemplate_name), highlighted when the local actor is in
    /// the group, tap → <see cref="ReactionToggleRequested"/> (re-toggle off). Built into
    /// the non-virtualizing <c>ReactionPillsList</c> StackPanel so the e2e count-all is
    /// bounded. Cleared + rebuilt each Bind. Pills are gated on <see cref="SupportsReactions"/>
    /// (a no-reaction rail folds no groups anyway, but an empty panel keeps it crisp).</summary>
    private void RenderReactionPills(DmMessageView msg)
    {
        ReactionPillsList.Children.Clear();
        if (!SupportsReactions) return;
        foreach (var g in msg.Reactions)
            ReactionPillsList.Children.Add(BuildReactionPill(g));
    }

    private Button BuildReactionPill(ReactionGroupVm g)
    {
        var pill = new Button
        {
            // Always-enabled (no IsEnabled bind — reference_windows_disabled_button_no_invoke);
            // a re-tap toggles my reaction off via the same manager path.
            Padding = new Thickness(8, 2, 8, 2),
            FontSize = 12,
            Content = $"{g.Emoji} {g.Count}",
            Background = g.Highlighted
                ? new Microsoft.UI.Xaml.Media.SolidColorBrush(
                    Windows.UI.Color.FromArgb(0xFF, 0x25, 0x63, 0xEB))
                : ThemeBrush("CardBackgroundFillColorDefaultBrush"),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(pill, Ids.DmReactionPill);
        // FlaUI counts 0 on a row without a Name — give every pill one.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(pill, $"{g.Emoji} {g.Count}");
        pill.Click += (_, _) =>
        {
            if (_message is not null) ReactionToggleRequested?.Invoke((_message, g.Emoji));
        };
        return pill;
    }

    /// <summary>Build the ⋯ <c>dm-message-actions-button</c>'s <c>MenuFlyout</c>
    /// (<c>dm-message-actions-menu</c>) for this message: the fixed-6 quick-set
    /// <c>dm-reaction-option</c> items + the <c>dm-reaction-more-button</c> (both gated on
    /// <see cref="SupportsReactions"/>), then EITHER the <c>dm-message-delete-button</c>
    /// (gated on <see cref="SupportsMessageDelete"/> &amp; <c>msg.IsOwn</c>) OR the
    /// <c>dm-message-mark-as-spam-button</c> (gated <c>!msg.IsOwn</c> — a received
    /// message; mail-spam.md § Wire shapes, no capability gate — mutually exclusive
    /// with delete by construction). The ⋯ button itself is shown iff ≥1 action is
    /// available; capability-gated off → Collapsed (no UIA peer). Rebuilt each Bind so
    /// a capability/own change re-gates it. Items are <c>MenuFlyoutItem</c>s with the
    /// indexed ids set on each (UIA-realized when the flyout opens).</summary>
    private void BuildActionsFlyout(DmMessageView msg)
    {
        var showDelete = SupportsMessageDelete && msg.IsOwn;
        var showMarkAsSpam = !msg.IsOwn;
        var showReport = !msg.IsOwn && msg.CanReport;
        var anyAction = SupportsReactions || showDelete || showMarkAsSpam || showReport;
        MessageActionsButton.Visibility = anyAction ? Visibility.Visible : Visibility.Collapsed;
        MessageActionsButton.Flyout = null;
        if (!anyAction) return;

        var menu = new MenuFlyout();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(menu, Ids.DmMessageActionsMenu);

        if (SupportsReactions)
        {
            foreach (var emoji in QuickSetEmoji)
            {
                var item = new MenuFlyoutItem { Text = emoji };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(item, Ids.DmReactionOption);
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(item, emoji);
                var picked = emoji;
                item.Click += (_, _) =>
                {
                    if (_message is not null) ReactionToggleRequested?.Invoke((_message, picked));
                };
                menu.Items.Add(item);
            }

            var more = new MenuFlyoutItem
            {
                Text = Strings.Get("conversations/detail/more_reactions"),
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(more, Ids.DmReactionMoreButton);
            more.Click += (_, _) => ShowMoreReactionsPicker(more);
            menu.Items.Add(more);
        }

        if (showDelete || showMarkAsSpam || showReport)
        {
            if (SupportsReactions) menu.Items.Add(new MenuFlyoutSeparator());
            if (showDelete)
            {
                var del = new MenuFlyoutItem
                {
                    Text = Strings.Get("conversations/detail/delete_message"),
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(del, Ids.DmMessageDeleteButton);
                del.Click += (_, _) => ShowDeleteConfirm();
                menu.Items.Add(del);
            }
            if (showMarkAsSpam)
            {
                var spam = new MenuFlyoutItem
                {
                    Text = Strings.Get("conversations/detail/mark_as_spam"),
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(spam, Ids.DmMessageMarkAsSpamButton);
                spam.Click += (_, _) =>
                {
                    if (_message is not null) MarkAsSpamRequested?.Invoke(_message);
                };
                menu.Items.Add(spam);
            }
            if (showReport)
            {
                var report = new MenuFlyoutItem
                {
                    Text = Strings.Get("conversations/detail/report_message"),
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(report, Ids.DmMessageReportButton);
                report.Click += (_, _) =>
                {
                    if (_message is not null) ReportRequested?.Invoke(_message);
                };
                menu.Items.Add(report);
            }
        }

        MessageActionsButton.Flyout = menu;
    }

    /// <summary>Open the fuller picker the <c>dm-reaction-more-button</c> raises — a flyout
    /// anchored to the ⋯ button holding a single-emoji free-entry <c>TextBox</c> with the
    /// shortcut grid beneath it (conversations.md § Reactions &amp; message delete →
    /// <i>Rendering / picker glue</i>). The field reaches ANY emoji: Win+. into the focused
    /// field, typing, or pasting. While it is open the field carries
    /// <c>dm-reaction-more-button</c> itself (<i>entry mode</i>, as on tui and apple) and
    /// the menu item that raised it gives the id up until the flyout closes, so one id is
    /// on screen at a time. The field commits on Enter and, as soon as it holds one
    /// complete emoji, on <c>TextChanged</c> — so an OS panel pick (and the e2e's
    /// <c>ValuePattern.SetValue</c>) commits in one gesture. Grid buttons have NO
    /// <c>dm-reaction-option</c> id (the e2e's fixed-6 count stays exact), each routing the
    /// SAME <see cref="ReactionToggleRequested"/> toggle; rows are a non-virtualizing
    /// StackPanel so every button is realized for FlaUI.</summary>
    private void ShowMoreReactionsPicker(MenuFlyoutItem moreItem)
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 4 };

        var field = new TextBox
        {
            PlaceholderText = Strings.Get("conversations/detail/more_reactions"),
            AcceptsReturn = false,
            FontSize = 16,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(field, Ids.DmReactionMoreButton);
        // FlaUI counts nothing without a Name.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(field, Strings.Get("conversations/detail/more_reactions"));
        var committed = false;
        void Commit(string emoji)
        {
            if (committed || emoji.Length == 0) return;
            committed = true;
            if (_message is not null) ReactionToggleRequested?.Invoke((_message, emoji));
            _moreReactionsFlyout?.Hide();
        }
        field.TextChanged += (_, _) =>
        {
            var first = FirstCompleteEmoji(field.Text);
            if (first is not null) Commit(first);
        };
        field.KeyDown += (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter) return;
            e.Handled = true;
            var first = FirstTextElement(field.Text);
            if (first is not null) Commit(first);
        };
        panel.Children.Add(field);

        const int perRow = 5;
        StackPanel? row = null;
        for (var i = 0; i < MoreGridEmoji.Count; i++)
        {
            if (i % perRow == 0)
            {
                row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
                panel.Children.Add(row);
            }
            var emoji = MoreGridEmoji[i];
            var btn = new Button
            {
                Content = emoji,
                FontSize = 16,
                Padding = new Thickness(6, 2, 6, 2),
                MinWidth = 40,
            };
            // Deliberately NO dm-reaction-option id (grid buttons don't inflate the
            // quick-set count); a Name keeps them UIA-addressable for a direct e2e.
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(btn, emoji);
            btn.Click += (_, _) => Commit(emoji);
            row!.Children.Add(btn);
        }

        // Entry mode: the field holds the id while open; the menu item gets it back on close.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(moreItem, string.Empty);
        _moreReactionsFlyout = new Flyout { Content = panel };
        _moreReactionsFlyout.Opened += (_, _) => field.Focus(FocusState.Programmatic);
        _moreReactionsFlyout.Closed += (_, _) =>
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(moreItem, Ids.DmReactionMoreButton);
        _moreReactionsFlyout.ShowAt(MessageActionsButton);
    }

    /// <summary>The first user-perceived character (text element) of <paramref name="text"/>
    /// — never a split ZWJ sequence or a lone half of a surrogate pair — or null when empty.</summary>
    private static string? FirstTextElement(string text)
    {
        var trimmed = text.Trim();
        if (trimmed.Length == 0) return null;
        var e = System.Globalization.StringInfo.GetTextElementEnumerator(trimmed);
        return e.MoveNext() ? (string)e.Current : null;
    }

    /// <summary>The field's first text element once it is a complete emoji — a lone high
    /// surrogate (half a pair, mid-keystroke) or a plain letter is not, so typing a word
    /// does not commit its first letter; Enter commits whatever element is there.</summary>
    private static string? FirstCompleteEmoji(string text)
    {
        var first = FirstTextElement(text);
        if (first is null) return null;
        if (first.Length == 1 && char.IsSurrogate(first[0])) return null;
        var rune = System.Text.Rune.GetRuneAt(first, 0);
        var cat = System.Text.Rune.GetUnicodeCategory(rune);
        // Emoji are OtherSymbol (So) — 😀 ✅ ❤ ⭐ — plus keycaps/flags whose first rune is
        // a digit/#/* or a regional indicator (also So); letters, digits alone and
        // punctuation wait for Enter.
        if (cat == System.Globalization.UnicodeCategory.OtherSymbol) return first;
        return first.Length > 1 && first.Contains('⃣') ? first : null;
    }

    private Flyout? _moreReactionsFlyout;
    private Flyout? _deleteConfirmFlyout;

    /// <summary>Open the delete-confirm step (<c>dm-message-delete-confirm-button</c>) as a
    /// sub-flyout anchored to the ⋯ button — title + a single always-enabled confirm Button
    /// that fires <see cref="DeleteRequested"/>. A flyout (not a ContentDialog) keeps the
    /// confirm button reliably UIA-realized for a FlaUI Invoke without an extra XamlRoot
    /// dance. conversations.md § Reactions &amp; message delete.</summary>
    private void ShowDeleteConfirm()
    {
        var panel = new StackPanel { Orientation = Orientation.Vertical, Spacing = 8, MinWidth = 180 };
        panel.Children.Add(new TextBlock
        {
            Text = Strings.Get("conversations/detail/delete_message_confirm_title"),
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        var confirm = new Button
        {
            Content = Strings.Get("conversations/detail/delete_message_confirm"),
            HorizontalAlignment = HorizontalAlignment.Stretch,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(confirm, Ids.DmMessageDeleteConfirmButton);
        confirm.Click += (_, _) =>
        {
            if (_message is not null) DeleteRequested?.Invoke(_message);
            _deleteConfirmFlyout?.Hide();
        };
        panel.Children.Add(confirm);

        _deleteConfirmFlyout = new Flyout { Content = panel };
        _deleteConfirmFlyout.ShowAt(MessageActionsButton);
    }

    /// <summary>Render one indexed <c>dm-attachment-image</c> / <c>dm-attachment-file</c>
    /// per first-class <c>Attachment</c> block the manager folded into
    /// <c>MessageSnapshot.document</c> after the body (render-model.md § D2;
    /// <see cref="FaunaApp.Core.Helpers.DocumentRenderer.Attachments"/>) — no sibling field.
    /// Image bytes load through <see cref="AttachmentBytesResolver"/> (the shared attachment
    /// store); non-images render a filename + size affordance. The <c>c2pa-badge</c> is
    /// driven off the per-attachment <c>c2pa</c> flag — never re-derived client-side
    /// (the receiver's shared Rust probes the bytes; conversations.md § Attachments). Built
    /// imperatively into the non-virtualizing AttachmentsList so each child carries
    /// the indexed id for the e2e count-all (reference_winui_flaui_datatemplate_name).</summary>
    private void RenderAttachments()
    {
        AttachmentsList.Children.Clear();
        foreach (var att in FaunaApp.Core.Helpers.DocumentRenderer.Attachments(_document))
        {
            AttachmentsList.Children.Add(
                att.isImage ? BuildImageAttachment(att) : BuildFileAttachment(att));
        }
    }

    /// <summary>An <c>dm-attachment-image</c> (real bytes via the resolver) with an
    /// optional <c>c2pa-badge</c> overlay. A picture without decodable bytes — not yet
    /// fetched, or evicted with nowhere to refill from — degrades to its DECLARED
    /// placeholder under the same id: filename and size, never a bare frame
    /// (conversations.md § Attachments → <i>Retention</i>). The element answers
    /// <c>get_attr(.., "state")</c> (its HelpText) with <c>painted</c> once the bitmap is
    /// set, or <c>placeholder</c>. Residency needs no separate change key here: a reused
    /// bubble re-binds on every observer tick and this panel is cleared and rebuilt on
    /// each bind, so an evict's or a refill's <c>notify</c> repaints it. Mirrors linux's
    /// <c>document.rs::build_attachment</c>.</summary>
    private FrameworkElement BuildImageAttachment(RenderBlock.Attachment att)
    {
        var grid = new Grid { HorizontalAlignment = HorizontalAlignment.Left };
        var declared = DeclaredAttachmentLabel(att);
        var bytes = AttachmentBytesResolver?.Invoke(att.blobHash);
        if (bytes is null || bytes.Length == 0)
        {
            grid.Children.Add(BuildImagePlaceholder(declared));
        }
        else
        {
            var image = new Image
            {
                MaxHeight = 200,
                MinHeight = 48,
                MinWidth = 48,
                Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(image, Ids.DmAttachmentImage);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(image, declared);
            image.Tapped += (_, _) =>
            {
                if (image.Source is BitmapImage bmp) ImageClicked?.Invoke(bmp);
            };
            grid.Children.Add(image);
            _ = DecodeImageAsync(grid, image, bytes, declared);
        }

        if (att.c2pa)
        {
            grid.Children.Add(BuildC2paBadge());
        }
        return grid;
    }

    /// <summary>Decode <paramref name="bytes"/> into the image's source and mark it
    /// <c>painted</c>; bytes that do not decode swap the image for its declared
    /// placeholder, the same as bytes that are absent.</summary>
    private static async System.Threading.Tasks.Task DecodeImageAsync(
        Grid grid, Image image, byte[] bytes, string declared)
    {
        var bmp = await BlobImageLoader.BitmapFromBytesAsync(bytes);
        if (bmp is not null)
        {
            image.Source = bmp;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(image, "painted");
            return;
        }
        var at = grid.Children.IndexOf(image);
        if (at < 0) return;
        grid.Children.RemoveAt(at);
        grid.Children.Insert(at, BuildImagePlaceholder(declared));
    }

    /// <summary>The declared placeholder a picture shows without its bytes: its
    /// filename and size as text, under the <c>dm-attachment-image</c> id, answering
    /// <c>state</c> = <c>placeholder</c>. AutomationId only — a TextBlock's UIA Name is
    /// its text.</summary>
    private static TextBlock BuildImagePlaceholder(string declared)
    {
        var placeholder = new TextBlock
        {
            Text = declared,
            FontSize = 12,
            Opacity = 0.7,
            TextWrapping = TextWrapping.Wrap,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(placeholder, Ids.DmAttachmentImage);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(placeholder, "placeholder");
        return placeholder;
    }

    /// <summary>"{filename} ({size})" — what every attachment declares whether or not its
    /// bytes are here; the filename falls back to the blob hash.</summary>
    private static string DeclaredAttachmentLabel(RenderBlock.Attachment att)
    {
        var label = string.IsNullOrWhiteSpace(att.filename) ? att.blobHash : att.filename;
        return $"{label} ({ValueFormat.ByteSize(att.sizeBytes)})";
    }

    /// <summary>The per-attachment <c>c2pa-badge</c>. Label and tooltip are the shared i18n
    /// strings the feed's list-card badge uses (<c>FeedViewModel.C2paLabel</c> /
    /// <c>C2paTooltip</c>), never a per-app phrasing. A layout <c>Border</c> with only an
    /// AutomationId is pruned from the UIA tree — <c>count("c2pa-badge")</c> reads 0 though
    /// the badge is painted — so it carries the label as its <c>AutomationProperties.Name</c>,
    /// exactly as the feed badge does (reference_winui_flaui_datatemplate_name).</summary>
    private static Border BuildC2paBadge()
    {
        var label = Strings.Get("c2pa/badge_label");
        var badge = new Border
        {
            CornerRadius = new CornerRadius(12),
            Padding = new Thickness(6, 2, 6, 2),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Bottom,
            Margin = new Thickness(0, 0, 8, 8),
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(
                Windows.UI.Color.FromArgb(0xFF, 0x25, 0x63, 0xEB)),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(badge, Ids.C2paBadge);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(badge, label);
        ToolTipService.SetToolTip(badge, Strings.Get("conversations/detail/badge_c2pa"));
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        row.Children.Add(new TextBlock
        {
            Text = "", // Segoe MDL2 "Completed" shield
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
            FontSize = 10,
            Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.White),
            VerticalAlignment = VerticalAlignment.Center,
        });
        row.Children.Add(new TextBlock
        {
            Text = label,
            FontSize = 10,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.White),
            VerticalAlignment = VerticalAlignment.Center,
        });
        badge.Child = row;
        return badge;
    }

    /// <summary>A <c>dm-attachment-file</c> affordance: paperclip glyph + filename +
    /// human size. Tap is reserved for the download/open follow-on.</summary>
    private Border BuildFileAttachment(RenderBlock.Attachment att)
    {
        var border = new Border
        {
            Background = ThemeBrush("CardBackgroundFillColorDefaultBrush"),
            CornerRadius = new CornerRadius(6),
            Padding = new Thickness(8, 6, 8, 6),
            HorizontalAlignment = HorizontalAlignment.Left,
        };
        var declared = DeclaredAttachmentLabel(att);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(border, Ids.DmAttachmentFile);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(border, declared);
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        row.Children.Add(new TextBlock
        {
            Text = "", // Segoe MDL2 "Page" glyph
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
            FontSize = 14,
            VerticalAlignment = VerticalAlignment.Center,
        });
        row.Children.Add(new TextBlock
        {
            Text = declared,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
        });
        border.Child = row;
        return border;
    }

    private static Microsoft.UI.Xaml.Media.Brush? ThemeBrush(string key) =>
        Application.Current.Resources.TryGetValue(key, out var value)
            ? value as Microsoft.UI.Xaml.Media.Brush
            : null;

    /// <summary>(Re)render the body at the manager-projected reveal state and show the
    /// per-message <c>load-remote-content-button</c> iff the body has ≥1 blocked
    /// remote image (render-model.md § D3 — the manager projects <c>RemoteImage.revealed</c>
    /// onto the document; the button is hidden once ALL remote images are revealed).
    /// The paint itself — one <c>TextBlock</c> or a virtualized run of them — is
    /// <see cref="DocumentBodyView"/>'s (render-model.md § Implementation status today).</summary>
    private void RenderBody()
    {
        BodyView.Document = _document;
        LoadRemoteContentButton.Visibility =
            FaunaApp.Core.Helpers.DocumentRenderer.HasBlockedRemoteImage(_document)
                ? Visibility.Visible
                : Visibility.Collapsed;
    }

    /// <summary>Paint one D3 <c>doc-remote-image</c> per body <c>RemoteImage</c> block, in body
    /// order (render-model.md § D3 + § Implementation status) — the SAME panel the feed
    /// post-card paints, derived from the shared
    /// <see cref="FaunaApp.Core.Helpers.RemoteImageCardModel"/> so the two surfaces can't drift
    /// (priority #2/#4). Bubbles are reused across Refresh re-binds, so this ALWAYS rebuilds the
    /// panel from scratch (clear, then append) — the same clear-and-rebuild contract
    /// <see cref="RenderAttachments"/> / <see cref="RenderLinkPreview"/> honour.</summary>
    private void RenderRemoteImages()
    {
        DocumentPainter.ApplyRemoteImages(RemoteImagesList, FaunaApp.Core.Helpers.RemoteImageCardModel.All(_document));
    }

    /// <summary>Paint the in-bubble reply-quote card (<c>dm-message-quote</c>) iff the manager
    /// folded a <c>RenderBlock.QuotedMessage</c> into this (reply) message's document — the
    /// parent's author + a ≤2-line snippet (render-model.md § D2;
    /// <see cref="FaunaApp.Core.Helpers.DocumentRenderer.QuotedMessage"/>). The bubble walks the
    /// document, never the sibling <c>reply_to</c> field. Bubbles are reused across Refresh
    /// re-binds, so the live path ALWAYS sets <c>QuoteCard.Visibility</c> explicitly (the
    /// else-branch collapses it) — a stale visible card from a prior bind never leaks a
    /// <c>dm-message-quote</c> UIA peer onto a non-reply message.</summary>
    private void RenderQuote()
    {
        var quote = FaunaApp.Core.Helpers.DocumentRenderer.QuotedMessage(_document);
        if (quote is not null)
        {
            QuoteAuthorText.Text = quote.authorDisplay;
            QuoteSnippetText.Text = quote.snippet;
            // A bare decorative Border carrying only an AutomationId doesn't realize a UIA
            // peer (FlaUI then counts 0 — reference_winui_flaui_datatemplate_name); a Name
            // forces the peer AND carries the quote text so get_text("dm-message-quote") sees
            // the author + snippet (e.g. the parent-body snippet the e2e asserts on).
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
                QuoteCard, $"{quote.authorDisplay} {quote.snippet}");
            QuoteCard.Visibility = Visibility.Visible;
        }
        else
        {
            QuoteCard.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Paint one D4 <c>link-preview-card</c> per <c>Resolved</c> folded
    /// <c>LinkPreview</c> in the message body, in body order (render-model.md § D4) — the SAME
    /// card the feed post paints, derived from the shared
    /// <see cref="FaunaApp.Core.Helpers.LinkPreviewCardModel"/> so the two surfaces can't drift
    /// (priority #2/#4). The producer folds a preview after EACH standalone bare-URL paragraph,
    /// so a two-URL message paints two cards; the previous single fixed slot fed by a
    /// <c>FirstOrDefault</c> twin dropped every card but the first (render-model.md
    /// § Implementation status). Resolving/Failed/absent contribute nothing — the shared
    /// <c>resolved_link_previews</c> face pre-filters, so there is no state match here — and the
    /// kept inline body link shows the URL (no skeleton, matching web).
    ///
    /// Bubbles are reused across Refresh re-binds, so this ALWAYS rebuilds the panel from
    /// scratch (clear, then append): a stale card from a previous bind can never leak a
    /// <c>link-preview-card</c> UIA peer onto a message that now has none — the same
    /// clear-and-rebuild contract <see cref="RenderAttachments"/> honours.
    ///
    /// The og:image is reveal-gated: this method owns each image's Visibility (the
    /// <c>ManageVisibility=False</c> opt-out) and the model withholds the blob hash until
    /// revealed, so the loader never fetches before reveal. The bubble's existing
    /// <c>load-remote-content-button</c> reveals them together with the body images —
    /// <see cref="RenderBody"/>'s <c>HasBlockedRemoteImage</c> gate already counts every
    /// un-revealed og:image (the shared <c>has_blocked_remote_images</c> LinkPreview arm, which
    /// recurses over ALL previews), so no extra wiring is needed here.</summary>
    private void RenderLinkPreview()
    {
        LinkPreviewCards.Children.Clear();
        foreach (var card in FaunaApp.Core.Helpers.LinkPreviewCardModel.All(_document))
            LinkPreviewCards.Children.Add(BuildLinkPreviewCard(card));
    }

    /// <summary>Build one <c>link-preview-card</c> Border for <paramref name="card"/> — the
    /// imperative twin of the feed's <c>ItemsControl</c> DataTemplate (same ids, same reveal
    /// gate), since this bubble builds its rich widgets in code like
    /// <see cref="RenderAttachments"/> rather than through a template.</summary>
    private Border BuildLinkPreviewCard(FaunaApp.Core.Helpers.LinkPreviewCardModel card)
    {
        // og:image reveal gate: this method owns Visibility (ManageVisibility=False), the hash
        // is withheld ("") until revealed so the loader never fetches before reveal (D3 posture).
        var image = new Image
        {
            MaxHeight = 200,
            Stretch = Microsoft.UI.Xaml.Media.Stretch.UniformToFill,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            Margin = new Thickness(0, 0, 0, 2),
            Visibility = card.ShowImage ? Visibility.Visible : Visibility.Collapsed,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(image, Ids.LinkPreviewImage);
        ImageHashBind.SetManageVisibility(image, false);
        ImageHashBind.SetLoader(image, LinkPreviewImageLoader);
        ImageHashBind.SetHash(image, card.ImageHash);

        var title = new TextBlock
        {
            Text = card.Title,
            FontSize = 13,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
            MaxLines = 2,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(title, Ids.LinkPreviewTitle);

        var description = new TextBlock
        {
            Text = card.Description,
            FontSize = 12,
            Opacity = 0.8,
            TextWrapping = TextWrapping.Wrap,
            MaxLines = 3,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(description, Ids.LinkPreviewDescription);

        var domain = new TextBlock { Text = card.Domain, FontSize = 11, Opacity = 0.6 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(domain, Ids.LinkPreviewDomain);

        var stack = new StackPanel { Spacing = 2 };
        stack.Children.Add(image);
        stack.Children.Add(title);
        stack.Children.Add(description);
        stack.Children.Add(domain);

        var border = new Border
        {
            BorderThickness = new Thickness(1),
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["DividerStrokeColorDefaultBrush"],
            Padding = new Thickness(8),
            Margin = new Thickness(0, 4, 0, 0),
            CornerRadius = new CornerRadius(4),
            HorizontalAlignment = HorizontalAlignment.Left,
            Child = stack,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(border, Ids.LinkPreviewCard);
        // A bare AutomationId-only Border is UIA-pruned (FlaUI counts 0 even when realized —
        // reference_winui_flaui_datatemplate_name); a Name forces the peer, like QuoteCard.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(border, card.Title);
        return border;
    }

    private void LoadRemoteContentButton_Click(object sender, RoutedEventArgs e)
    {
        // Dispatch to the manager via the page callback — the manager flips the
        // in-memory reveal set and re-emits; the next Refresh re-binds this bubble
        // with the updated document (render-model.md § D3; html-mail.md § Security).
        // NO local flip — the manager is the authority for reveal state.
        if (_message is not null)
            RemoteRevealRequested?.Invoke(_message);
    }

    private void MutedRevealButton_Click(object sender, RoutedEventArgs e)
    {
        // Client-local reveal (moderation.md § Muted keywords) — the page marks
        // MutedKeywordsCache.Reveal(messageId) and re-renders directly; unlike
        // LoadRemoteContentButton_Click there is no shared-Rust dispatch (the mute
        // list + reveal set are session-local client state, never server state).
        if (_message is not null)
            MutedRevealRequested?.Invoke(_message);
    }

    private void RegionRevealButton_Click(object sender, RoutedEventArgs e)
    {
        // A region `collapse` is one reveal away (region-blocking.md § The blocked
        // render) and shares the content-policy reveal set, so it rides the same
        // page callback: the page marks ContentPolicyCache.Reveal(messageId) and
        // re-renders. Never reachable on a region `block` (no button is painted).
        if (_message is not null)
            ContentRevealRequested?.Invoke(_message);
    }

    private void ContentRevealButton_Click(object sender, RoutedEventArgs e)
    {
        // Client-local reveal of a content-policy COLLAPSE (family-safety.md
        // § Content policy) — the page marks ContentPolicyCache.Reveal(messageId)
        // and re-renders, exactly like MutedRevealButton_Click. No shared-Rust
        // dispatch: the floor lives in the guardian's policy, the reveal is session
        // state. Never reachable on a `block` (that arm paints no button).
        if (_message is not null)
            ContentRevealRequested?.Invoke(_message);
    }

    private void ReplyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_message is not null)
            ReplyRequested?.Invoke(_message);
    }

    private void ReplyAllButton_Click(object sender, RoutedEventArgs e)
    {
        if (_message is not null)
            ReplyAllRequested?.Invoke(_message);
    }

}
